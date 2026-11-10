//! Test harness: a real daemon on an ephemeral loopback port, a recording mock
//! upstream, an in-memory key backend and a recording notifier.
#![allow(dead_code)]

use pv_core::*;
use pv_daemon::mock::{self, Recorded};
use pv_daemon::state::{self, AppState, Paths};
use pv_proxy::notify::{Notifier, Recorder};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct H {
    pub st: Arc<AppState>,
    pub base: String,
    pub key: String,
    pub admin: String,
    pub rec: Arc<Recorded>,
    pub mock_base: String,
    pub notes: Arc<Recorder>,
    pub http: reqwest::Client,
    pub dir: tempfile::TempDir,
}

pub fn provider(id: &str, base: &str, model: &str, tier: DestinationTier, failover: bool, responses: bool) -> ProviderConfig {
    ProviderConfig {
        schema_version: 1,
        id: id.into(),
        kind: ProviderKind::OpenaiCompatible,
        display_name: id.into(),
        base_url: base.into(),
        default_model: model.into(),
        declared_tier: tier,
        credential_ref: None,
        enabled: true,
        allow_failover: failover,
        supports_responses: responses,
        timeout_secs: 10,
    }
}

pub async fn harness(model: &str) -> H {
    std::env::set_var("PV_KEY_BACKEND", "memory");
    std::env::set_var("PV_NEAR_MOCK", "1");
    let dir = tempfile::tempdir().unwrap();
    let rec = Arc::new(Recorded::default());
    let (mock_base, _) = mock::serve(0, rec.clone()).await.unwrap();
    let notes = Arc::new(Recorder::default());
    let n: Arc<dyn Notifier> = notes.clone();
    let st = state::bootstrap(Paths { data: dir.path().to_path_buf() }, Some(n)).await.unwrap();
    // Deterministic detectors only unless a test opts in.
    st.engine.detector.set_settings(pv_detectors::DetectorSettings { span_enabled: false, span_required: false, span_timeout_ms: 2000 });
    pv_daemon::admin::put_provider_inner(&st, provider("mock", &mock_base, model, DestinationTier::ExternalUnverified, false, true), None).map_err(|_| ()).unwrap();
    let mut s = st.engine.settings();
    s.default_destination = Some("mock".into());
    state::save_settings(&st, &s, "test").unwrap();
    let (_, key) = state::create_app(&st, "test", None, None).unwrap();
    let l = pv_daemon::bind(0).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let st2 = st.clone();
    tokio::spawn(async move {
        let _ = pv_daemon::serve(st2, l).await;
    });
    // wait for port to be published
    for _ in 0..50 {
        if st.port.load(std::sync::atomic::Ordering::Relaxed) == port {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let admin = st.admin_token.to_string();
    H { st, base: format!("http://127.0.0.1:{port}"), key, admin, rec, mock_base, notes, http: reqwest::Client::builder().no_proxy().build().unwrap(), dir }
}

impl H {
    pub async fn chat(&self, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}/v1/chat/completions", self.base)).bearer_auth(&self.key).json(&body).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn chat_key(&self, key: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}/v1/chat/completions", self.base)).bearer_auth(key).json(&body).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn responses(&self, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}/v1/responses", self.base)).bearer_auth(&self.key).json(&body).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    /// Returns (status, raw SSE text).
    pub async fn stream(&self, path: &str, body: Value) -> (u16, String) {
        let r = self.http.post(format!("{}{}", self.base, path)).bearer_auth(&self.key).json(&body).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.text().await.unwrap())
    }

    pub async fn admin_get(&self, path: &str) -> (u16, Value) {
        let r = self.http.get(format!("{}/api/admin/v1{}", self.base, path)).bearer_auth(&self.admin).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn admin_post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}/api/admin/v1{}", self.base, path)).bearer_auth(&self.admin).json(&body).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn admin_put(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.put(format!("{}/api/admin/v1{}", self.base, path)).bearer_auth(&self.admin).json(&body).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub fn upstream_text(&self) -> String {
        self.rec.all_text()
    }

    /// Every byte persisted locally (DB + WAL + keys dir) as lossy text.
    pub fn disk_text(&self) -> String {
        let mut s = String::new();
        for e in walk(self.dir.path()) {
            if let Ok(b) = std::fs::read(&e) {
                s.push_str(&String::from_utf8_lossy(&b));
            }
        }
        s
    }

    pub async fn events_text(&self) -> String {
        let r = self.http.get(format!("{}/api/admin/v1/events/export", self.base)).bearer_auth(&self.admin).send().await.unwrap();
        r.text().await.unwrap()
    }
}

fn walk(p: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

pub fn user(text: &str) -> Value {
    json!({"model": "gpt-test", "messages": [{"role": "user", "content": text}]})
}

/// Concatenate chat SSE `delta.content` strings.
pub fn sse_content(sse: &str) -> String {
    let mut out = String::new();
    for line in sse.lines() {
        if let Some(d) = line.strip_prefix("data: ") {
            if let Ok(v) = serde_json::from_str::<Value>(d) {
                if let Some(c) = v["choices"][0]["delta"]["content"].as_str() {
                    out.push_str(c);
                }
            }
        }
    }
    out
}

/// Concatenate Responses SSE `response.output_text.delta` deltas.
pub fn sse_resp_text(sse: &str) -> String {
    let mut out = String::new();
    let mut ev = "";
    for line in sse.lines() {
        if let Some(e) = line.strip_prefix("event: ") {
            ev = e;
        } else if let Some(d) = line.strip_prefix("data: ") {
            if ev == "response.output_text.delta" {
                if let Ok(v) = serde_json::from_str::<Value>(d) {
                    out.push_str(v["delta"].as_str().unwrap_or(""));
                }
            }
        }
    }
    out
}
