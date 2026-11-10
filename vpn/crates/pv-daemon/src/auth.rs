//! Local authentication and browser-attack defenses.
//!
//! * Host header must name a loopback origin on our port (DNS-rebinding defense).
//! * Admin API: `Authorization: Bearer <admin token>` (CLI) or a session cookie
//!   from a one-time login ticket; cookie-authenticated mutations also need the
//!   per-session CSRF header and a same-origin `Origin`.
//! * No CORS headers are ever sent.
//! * Proxy API: local caller key (`pvk_...`); requests carrying a foreign
//!   `Origin` are refused so web pages cannot drive the proxy.

use crate::state::{random_hex, AppState};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

pub const SESSION_COOKIE: &str = "pv_session";
pub const CSRF_HEADER: &str = "x-pv-csrf";
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
const TICKET_TTL: Duration = Duration::from_secs(120);

pub fn key_hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

#[derive(Clone)]
pub struct Session {
    pub csrf: String,
    pub expires: Instant,
}

#[derive(Default)]
pub struct Sessions {
    sessions: Mutex<HashMap<String, Session>>,
    tickets: Mutex<HashMap<String, Instant>>,
}

impl Sessions {
    pub fn new_ticket(&self) -> String {
        let t = random_hex(24);
        let mut m = self.tickets.lock().unwrap();
        m.retain(|_, exp| *exp > Instant::now());
        m.insert(t.clone(), Instant::now() + TICKET_TTL);
        t
    }

    /// Single-use ticket exchange.
    pub fn redeem(&self, ticket: &str) -> Option<(String, Session)> {
        let ok = self.tickets.lock().unwrap().remove(ticket).is_some_and(|exp| exp > Instant::now());
        if !ok {
            return None;
        }
        let id = random_hex(32);
        let s = Session { csrf: random_hex(16), expires: Instant::now() + SESSION_TTL };
        self.sessions.lock().unwrap().insert(id.clone(), s.clone());
        Some((id, s))
    }

    pub fn get(&self, id: &str) -> Option<Session> {
        let mut m = self.sessions.lock().unwrap();
        m.retain(|_, s| s.expires > Instant::now());
        m.get(id).cloned()
    }

    pub fn revoke(&self, id: &str) {
        self.sessions.lock().unwrap().remove(id);
    }
}

pub fn allowed_hosts(port: u16) -> [String; 3] {
    [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")]
}

pub fn origin_ok(origin: &str, port: u16) -> bool {
    allowed_hosts(port).iter().any(|h| origin == format!("http://{h}"))
}

fn deny(status: StatusCode, msg: &str) -> Response {
    let body = serde_json::json!({"error": {"type": "promptvpn_unauthorized", "code": "PV_UNAUTHORIZED", "message": msg, "event_id": null, "approval_id": null, "retryable": false}});
    (status, axum::Json(body)).into_response()
}

/// Applied to every route.
pub async fn host_guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let port = st.port.load(std::sync::atomic::Ordering::Relaxed);
    let host = req.headers().get("host").and_then(|h| h.to_str().ok()).unwrap_or("");
    if !allowed_hosts(port).iter().any(|h| h == host) {
        return deny(StatusCode::MISDIRECTED_REQUEST, "Host header must be a loopback address for this port");
    }
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert("x-content-type-options", "nosniff".parse().unwrap());
    h.insert("x-frame-options", "DENY".parse().unwrap());
    h.insert("referrer-policy", "no-referrer".parse().unwrap());
    h.insert("cache-control", "no-store".parse().unwrap());
    resp
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all("cookie").iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer "))).map(|s| s.trim().to_string())
}

/// Admin-route middleware.
pub async fn admin_guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let port = st.port.load(std::sync::atomic::Ordering::Relaxed);
    let h = req.headers();
    if let Some(o) = h.get("origin").and_then(|v| v.to_str().ok()) {
        if !origin_ok(o, port) {
            return deny(StatusCode::FORBIDDEN, "Cross-origin admin requests are refused");
        }
    }
    if let Some(sfs) = h.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if sfs == "cross-site" {
            return deny(StatusCode::FORBIDDEN, "Cross-site admin requests are refused");
        }
    }
    if let Some(tok) = bearer(h) {
        if bool::from(tok.as_bytes().ct_eq(st.admin_token.as_bytes())) {
            return next.run(req).await;
        }
        return deny(StatusCode::UNAUTHORIZED, "Invalid admin token");
    }
    if let Some(sid) = cookie(h, SESSION_COOKIE) {
        if let Some(s) = st.sessions.get(&sid) {
            let mutating = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
            if mutating {
                let csrf = h.get(CSRF_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
                if !bool::from(csrf.as_bytes().ct_eq(s.csrf.as_bytes())) {
                    return deny(StatusCode::FORBIDDEN, "Missing or invalid CSRF token");
                }
                if h.get("origin").is_none() && h.get("sec-fetch-site").and_then(|v| v.to_str().ok()) != Some("same-origin") {
                    return deny(StatusCode::FORBIDDEN, "Mutations from the dashboard must be same-origin");
                }
            }
            return next.run(req).await;
        }
    }
    deny(StatusCode::UNAUTHORIZED, "Authentication required")
}

/// Identify the local caller app from its key. Returns (app record) or error response.
pub fn authenticate_caller(st: &AppState, headers: &HeaderMap) -> Result<pv_audit::AppRecord, Response> {
    let port = st.port.load(std::sync::atomic::Ordering::Relaxed);
    if let Some(o) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !origin_ok(o, port) {
            return Err(deny(StatusCode::FORBIDDEN, "Browser cross-origin calls to the local proxy are refused"));
        }
    }
    let key = bearer(headers).or_else(|| headers.get("api-key").and_then(|v| v.to_str().ok()).map(String::from)).ok_or_else(|| deny(StatusCode::UNAUTHORIZED, "Missing local Prompt VPN API key (use the pvk_... key from `promptvpn connect`)"))?;
    match st.db().app_by_key_hash(&key_hash(&key)) {
        Ok(Some(app)) => Ok(app),
        _ => Err(deny(StatusCode::UNAUTHORIZED, "Unknown or revoked local API key")),
    }
}

pub fn session_cookie(id: &str) -> String {
    format!("{SESSION_COOKIE}={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}", SESSION_TTL.as_secs())
}

pub fn session_from(st: &AppState, headers: &HeaderMap) -> Option<(String, Session)> {
    let sid = cookie(headers, SESSION_COOKIE)?;
    st.sessions.get(&sid).map(|s| (sid, s))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origins() {
        assert!(origin_ok("http://127.0.0.1:8787", 8787));
        assert!(origin_ok("http://localhost:8787", 8787));
        assert!(!origin_ok("http://localhost:8788", 8787));
        assert!(!origin_ok("https://evil.example", 8787));
        assert!(!origin_ok("http://127.0.0.1.evil.example:8787", 8787));
    }
    #[test]
    fn tickets_single_use() {
        let s = Sessions::default();
        let t = s.new_ticket();
        assert!(s.redeem(&t).is_some());
        assert!(s.redeem(&t).is_none());
        assert!(s.redeem("nope").is_none());
    }
}
