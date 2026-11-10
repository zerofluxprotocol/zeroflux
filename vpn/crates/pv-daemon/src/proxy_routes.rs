//! OpenAI-compatible routes: /v1/chat/completions, /v1/responses, /v1/models.

use crate::auth::authenticate_caller;
use crate::state::AppState;
use axum::body::{Body as AxBody, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pv_core::PvError;
use pv_provider::Endpoint;
use pv_proxy::{Body, Caller, Source};
use std::sync::Arc;

pub fn error_response(e: &PvError) -> Response {
    let status = StatusCode::from_u16(e.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(e.envelope())).into_response()
}

fn header(h: &HeaderMap, k: &str) -> Option<String> {
    h.get(k).and_then(|v| v.to_str().ok()).map(|s| s.chars().filter(|c| c.is_ascii_graphic()).take(128).collect())
}

async fn handle(st: Arc<AppState>, headers: HeaderMap, body: Bytes, endpoint: Endpoint) -> Response {
    let app = match authenticate_caller(&st, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json");
    if !ct.starts_with("application/json") {
        return error_response(&PvError::new(pv_core::ErrorCode::UnsupportedContent, "Only application/json request bodies are supported"));
    }
    let caller = Caller {
        app_id: app.app_id.clone(),
        agent_id: header(&headers, "x-promptvpn-agent").or(app.agent_id.clone()),
        destination_override: app.destination_id.clone(),
        conversation: header(&headers, "x-promptvpn-conversation"),
        source: Source::Client,
    };
    match st.engine.handle(caller, endpoint, &body).await {
        Ok(out) => match out.body {
            Body::Json(v) => {
                let mut r = (StatusCode::from_u16(out.status).unwrap_or(StatusCode::OK), Json(v)).into_response();
                r.headers_mut().insert("x-promptvpn-event-id", out.event_id.parse().unwrap());
                r
            }
            Body::Stream(s) => {
                let stream = futures::StreamExt::map(s, Ok::<_, std::io::Error>);
                let mut r = Response::new(AxBody::from_stream(stream));
                r.headers_mut().insert("content-type", "text/event-stream".parse().unwrap());
                r.headers_mut().insert("x-promptvpn-event-id", out.event_id.parse().unwrap());
                r
            }
        },
        Err(e) => error_response(&e),
    }
}

pub async fn chat(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    handle(st, headers, body, Endpoint::Chat).await
}

pub async fn responses(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    handle(st, headers, body, Endpoint::Responses).await
}

/// Lists configured destinations' models as OpenAI model objects.
pub async fn models(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = authenticate_caller(&st, &headers) {
        return r;
    }
    let data: Vec<serde_json::Value> = st
        .engine
        .registry
        .providers()
        .into_iter()
        .filter(|p| p.enabled)
        .map(|p| serde_json::json!({"id": format!("{}/{}", p.id, p.default_model), "object": "model", "created": 0, "owned_by": format!("promptvpn:{}", p.id)}))
        .collect();
    Json(serde_json::json!({"object": "list", "data": data})).into_response()
}

pub async fn unsupported() -> Response {
    error_response(&PvError::new(pv_core::ErrorCode::UnsupportedContent, "This OpenAI endpoint is not proxied by Prompt VPN; only /v1/chat/completions and /v1/responses are inspected"))
}
