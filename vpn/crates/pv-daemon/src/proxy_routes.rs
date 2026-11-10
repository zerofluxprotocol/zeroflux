//! OpenAI-compatible routes: /v1/chat/completions, /v1/responses, /v1/models,
//! plus the ZFX metadata routes /v1/zfx/requests/{id}/privacy and
//! /v1/zfx/privacy-policy.

use crate::auth::authenticate_caller;
use crate::state::AppState;
use axum::body::{Body as AxBody, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pv_core::{ErrorCode, PrivacyTier, PvError};
use pv_provider::Endpoint;
use pv_proxy::{Body, Caller, ProxyOutput, Source};
use std::sync::Arc;

/// Header a caller sends to receive Prompt VPN 0.1 error codes (`PV_*`).
pub const ERROR_COMPAT_HEADER: &str = "x-zfx-error-codes";

pub fn wants_legacy_errors(h: &HeaderMap) -> bool {
    h.get(ERROR_COMPAT_HEADER).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("pv1"))
}

pub fn error_response(e: &PvError) -> Response {
    error_response_compat(e, false)
}

pub fn error_response_compat(e: &PvError, legacy: bool) -> Response {
    let status = StatusCode::from_u16(e.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(e.envelope_json(legacy))).into_response()
}

/// First present header among the current name and its legacy alias.
pub fn header(h: &HeaderMap, names: &[&str]) -> Option<String> {
    names.iter().find_map(|k| h.get(*k).and_then(|v| v.to_str().ok()).map(|s| s.chars().filter(|c| c.is_ascii_graphic()).take(128).collect()))
}

/// Apply route metadata headers (non-sensitive) to a proxied response.
pub fn decorate(r: &mut Response, out_event: &str, route: Option<&pv_core::PrivacyRoute>, notices: &[&'static str]) {
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(out_event) {
        h.insert("x-zfx-request-id", v.clone());
        h.insert("x-promptvpn-event-id", v);
    }
    if let Some(route) = route {
        if let Ok(v) = HeaderValue::from_str(&route.header_value()) {
            h.insert("x-zfx-privacy-route", v);
        }
        if let Ok(v) = HeaderValue::from_str(route.privacy_tier.as_str()) {
            h.insert("x-zfx-privacy-tier", v);
        }
    }
    if !notices.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&notices.join(", ")) {
            h.insert("x-zfx-notice", v);
        }
    }
}

pub fn proxy_response(out: ProxyOutput) -> Response {
    let ProxyOutput { status, body, event_id, route, notices } = out;
    let mut r = match body {
        Body::Json(v) => (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), Json(v)).into_response(),
        Body::Stream(s) => {
            let stream = futures::StreamExt::map(s, Ok::<_, std::io::Error>);
            let mut r = Response::new(AxBody::from_stream(stream));
            r.headers_mut().insert("content-type", "text/event-stream".parse().unwrap());
            r
        }
    };
    decorate(&mut r, &event_id, route.as_ref(), &notices);
    r
}

async fn handle(st: Arc<AppState>, headers: HeaderMap, body: Bytes, endpoint: Endpoint) -> Response {
    let legacy = wants_legacy_errors(&headers);
    let app = match authenticate_caller(&st, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json");
    if !ct.starts_with("application/json") {
        return error_response_compat(&PvError::new(ErrorCode::UnsupportedContent, "Only application/json request bodies are supported"), legacy);
    }
    let caller = Caller {
        agent_id: header(&headers, &["x-zfx-agent", "x-promptvpn-agent"]).or(app.agent_id.clone()),
        destination_override: app.destination_id.clone(),
        conversation: header(&headers, &["x-zfx-conversation", "x-promptvpn-conversation"]),
        source: Source::Client,
        ..Caller::client(app.app_id.clone())
    };
    match st.engine.handle(caller, endpoint, &body).await {
        Ok(out) => proxy_response(out),
        Err(e) => error_response_compat(&e, legacy),
    }
}

pub async fn chat(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    handle(st, headers, body, Endpoint::Chat).await
}

pub async fn responses(State(st): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    handle(st, headers, body, Endpoint::Responses).await
}

/// Configured destinations' models as OpenAI model objects, with the privacy
/// tier the destination provides right now (NEAR models are verified live).
pub async fn models(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(r) = authenticate_caller(&st, &headers) {
        return r;
    }
    let mut data = Vec::new();
    for p in st.engine.registry.providers().into_iter().filter(|p| p.enabled) {
        let resolved = st.engine.registry.resolve(&p, None).await;
        let tier = PrivacyTier::of(p.kind, resolved.tier);
        data.push(serde_json::json!({
            "id": format!("{}/{}", p.id, p.default_model),
            "object": "model",
            "created": 0,
            "owned_by": format!("zfx-vpn:{}", p.id),
            "privacy_tier": tier.as_str(),
            "zfx_hosted_transport": p.zfx_hosted_transport,
        }));
    }
    let s = st.engine.settings();
    if let (Some(pd), Some(m)) = (&s.private_destination, &s.private_model) {
        if let Some(p) = st.engine.registry.get(pd) {
            let r = st.engine.registry.resolve(&p, Some(m)).await;
            data.push(serde_json::json!({
                "id": format!("{}/{}", p.id, m),
                "object": "model",
                "created": 0,
                "owned_by": format!("zfx-vpn:{}", p.id),
                "privacy_tier": PrivacyTier::of(p.kind, r.tier).as_str(),
                "zfx_role": "confidential_fallback",
            }));
        }
    }
    Json(serde_json::json!({"object": "list", "data": data})).into_response()
}

/// Route metadata for one of the caller's own requests. Other apps' request
/// IDs return 404 (indistinguishable from unknown IDs).
pub async fn request_privacy(State(st): State<Arc<AppState>>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let legacy = wants_legacy_errors(&headers);
    let app = match authenticate_caller(&st, &headers) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match st.engine.db.route(&id, Some(&app.app_id)) {
        Ok(Some(r)) => Json(r).into_response(),
        _ => error_response_compat(&PvError::new(ErrorCode::NotFound, "No route record for this request id"), legacy),
    }
}

/// Privacy disclosure for local mode (no authentication needed; no config details).
pub async fn privacy_policy() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "service": "zfx-vpn-local",
        "mode": "local",
        "operator_sees_raw_prompts": false,
        "disclosure": "Local ZFX VPN runs on this device. Detection, transformation, the placeholder map and the audit log stay on this machine. Destinations receive only what the policy allows for their privacy tier: public and unverified models receive transformed content; verified NEAR AI TEE models can receive raw content only with your consent.",
        "privacy_tiers": ["LOCAL_ONLY", "TRANSFORMED_PUBLIC", "ATTESTED_CONFIDENTIAL", "UNVERIFIED_EXTERNAL"],
        "raw_retention": "none (held requests: encrypted, max 15 minutes)",
        "metadata_retention_days": 30,
        "error_codes_version": pv_core::errors::ERROR_CODES_VERSION,
    }))
}

pub async fn unsupported(headers: HeaderMap) -> Response {
    error_response_compat(&PvError::new(ErrorCode::UnsupportedContent, "This OpenAI endpoint is not proxied by ZFX VPN; only /v1/chat/completions and /v1/responses are inspected"), wants_legacy_errors(&headers))
}
