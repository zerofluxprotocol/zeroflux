//! Plain OpenAI-compatible upstream (Ollama `/v1`, custom endpoints, mock).

use crate::registry::{Endpoint, ProviderError, UpstreamResponse};
use futures::StreamExt;
use pv_core::ProviderConfig;
use serde_json::Value;
use std::time::Duration;

pub async fn send(http: &reqwest::Client, cfg: &ProviderConfig, endpoint: Endpoint, body: &Value, token: Option<&str>) -> Result<UpstreamResponse, ProviderError> {
    let url = format!("{}/{}", cfg.base_url.trim_end_matches('/'), endpoint.path());
    let bytes = serde_json::to_vec(body).map_err(|e| ProviderError::Protocol(e.to_string()))?;
    let mut req = http
        .post(&url)
        .header("content-type", "application/json")
        .header("accept-encoding", "identity")
        .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
        .body(bytes);
    // Only this destination's own credential is ever attached.
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| if e.is_timeout() { ProviderError::Timeout } else { ProviderError::Unreachable })?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(ProviderError::Auth);
    }
    if status >= 400 {
        return Err(ProviderError::Status(status));
    }
    let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_string();
    let body = resp.bytes_stream().map(|r| r.map_err(|e| if e.is_timeout() { ProviderError::Timeout } else { ProviderError::Unreachable }));
    Ok(UpstreamResponse { status, content_type, body: Box::pin(body), receipt_provider: None })
}
