//! Authenticated client for the local admin API (loopback HTTP + install token).

use anyhow::{anyhow, bail, Context, Result};
use pv_daemon::state::Paths;
use serde_json::Value;

pub struct Api {
    pub base: String,
    token: String,
    http: reqwest::Client,
}

pub fn port(paths: &Paths, flag: Option<u16>) -> u16 {
    flag.or_else(|| std::env::var("PV_PORT").ok().and_then(|p| p.parse().ok()))
        .or_else(|| std::fs::read_to_string(paths.port_file()).ok().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(pv_daemon::state::DEFAULT_PORT)
}

impl Api {
    pub fn new(paths: &Paths, port: u16) -> Result<Api> {
        let token = std::fs::read_to_string(paths.admin_token()).with_context(|| format!("admin token not found in {}; run `promptvpn init` first", paths.data.display()))?.trim().to_string();
        Ok(Api { base: format!("http://127.0.0.1:{port}"), token, http: reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(600)).build()? })
    }

    pub async fn healthy(&self) -> bool {
        matches!(self.http.get(format!("{}/healthz", self.base)).timeout(std::time::Duration::from_secs(2)).send().await, Ok(r) if r.status().is_success())
    }

    async fn handle(r: reqwest::Response) -> Result<Value> {
        let status = r.status();
        let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if ct.contains("ndjson") {
            return Ok(Value::String(r.text().await?));
        }
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let msg = v["error"]["message"].as_str().map(String::from).unwrap_or_else(|| v.to_string());
            bail!("{} ({})", msg, status.as_u16());
        }
        Ok(v)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        let r = self.http.get(format!("{}/api/admin/v1{path}", self.base)).bearer_auth(&self.token).send().await.map_err(|_| anyhow!("Prompt VPN daemon is not running; start it with `promptvpn start`"))?;
        Self::handle(r).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let r = self.http.post(format!("{}/api/admin/v1{path}", self.base)).bearer_auth(&self.token).json(&body).send().await.map_err(|_| anyhow!("Prompt VPN daemon is not running; start it with `promptvpn start`"))?;
        Self::handle(r).await
    }

    pub async fn put(&self, path: &str, body: Value) -> Result<Value> {
        let r = self.http.put(format!("{}/api/admin/v1{path}", self.base)).bearer_auth(&self.token).json(&body).send().await.map_err(|_| anyhow!("Prompt VPN daemon is not running"))?;
        Self::handle(r).await
    }

    pub async fn delete(&self, path: &str) -> Result<Value> {
        let r = self.http.delete(format!("{}/api/admin/v1{path}", self.base)).bearer_auth(&self.token).send().await.map_err(|_| anyhow!("Prompt VPN daemon is not running"))?;
        Self::handle(r).await
    }
}
