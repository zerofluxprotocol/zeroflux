//! Shared harness: hosted gateway on ephemeral ports, tenants with recording mock upstreams.
#![allow(dead_code)]
use pv_daemon::mock::{self, Recorded};
use serde_json::{json, Value};
use std::sync::Arc;
use zfx_vpn_hosted::{Config, Hosted};

pub struct G {
    pub h: Arc<Hosted>,
    pub base: String,
    pub admin: String,
    pub http: reqwest::Client,
    pub dir: tempfile::TempDir,
}

pub const OP: &str = "zfxop_test_operator_token_0123456789abcdef";

pub async fn gateway(response_ttl: u64) -> G {
    std::env::set_var("ZFX_NEAR_MOCK", "1");
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config { data_dir: Some(dir.path().to_path_buf()), admin_token: zeroize::Zeroizing::new(OP.into()), idempotency_response_ttl_secs: response_ttl, ..Default::default() };
    let h = Hosted::new(cfg, None).unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let admin = format!("http://{}", a.local_addr().unwrap());
    let r1 = zfx_vpn_hosted::router(h.clone());
    let r2 = zfx_vpn_hosted::admin_router(h.clone());
    tokio::spawn(async move { axum::serve(l, r1).await.unwrap() });
    tokio::spawn(async move { axum::serve(a, r2).await.unwrap() });
    G { h, base, admin, http: reqwest::Client::builder().no_proxy().build().unwrap(), dir }
}

pub struct T {
    pub id: String,
    pub key: String,
    pub key_id: String,
    pub rec: Arc<Recorded>,
}

impl G {
    pub async fn admin_post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}{path}", self.admin)).bearer_auth(OP).json(&body).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    pub async fn tenant(&self, name: &str, model: &str, quota: Value, scopes: Value) -> T {
        let rec = Arc::new(Recorded::default());
        let (mbase, _) = mock::serve(0, rec.clone()).await.unwrap();
        let cfg = json!({
            "name": name,
            "upstreams": [{"schema_version": 1, "id": "frontier", "kind": "OPENAI_COMPATIBLE", "display_name": "frontier", "base_url": mbase, "default_model": model, "declared_tier": "EXTERNAL_UNVERIFIED", "credential_ref": "provider:frontier", "enabled": true, "allow_failover": false, "supports_responses": true, "timeout_secs": 10}],
            "default_destination": "frontier",
            "quota": quota,
        });
        let (s, t) = self.admin_post("/admin/v1/tenants", cfg).await;
        assert_eq!(s, 201, "{t}");
        let id = t["tenant_id"].as_str().unwrap().to_string();
        let r = self.http.put(format!("{}/admin/v1/tenants/{id}/credentials/provider:frontier", self.admin)).bearer_auth(OP).json(&json!({"secret": format!("upstream-secret-of-{name}")})).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let (s, k) = self.admin_post(&format!("/admin/v1/tenants/{id}/keys"), json!({"scopes": scopes})).await;
        assert_eq!(s, 201, "{k}");
        T { id, key: k["key"].as_str().unwrap().into(), key_id: k["info"]["key_id"].as_str().unwrap().into(), rec }
    }

    pub async fn chat(&self, key: &str, body: Value, extra: &[(&str, &str)]) -> (u16, Value, reqwest::header::HeaderMap) {
        let mut rq = self.http.post(format!("{}/v1/chat/completions", self.base)).bearer_auth(key).json(&body);
        for (k, v) in extra {
            rq = rq.header(*k, *v);
        }
        let r = rq.send().await.unwrap();
        let s = r.status().as_u16();
        let h = r.headers().clone();
        (s, r.json().await.unwrap_or(Value::Null), h)
    }
}

