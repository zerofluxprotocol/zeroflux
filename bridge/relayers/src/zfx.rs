//! ZeroFlux node JSON-RPC client.

use serde_json::{json, Value};

#[derive(Clone)]
pub struct ZfxClient {
    pub url: String,
}

impl ZfxClient {
    pub fn new(url: &str) -> ZfxClient {
        ZfxClient { url: url.trim_end_matches('/').to_string() }
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let r = crate::agent()
            .post(&format!("{}/rpc", self.url))
            .timeout(std::time::Duration::from_secs(30))
            .send_json(json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .map_err(|e| format!("zfx node {}: {}", method, e))?;
        let v: Value = r.into_json().map_err(|e| e.to_string())?;
        if let Some(e) = v.get("error") {
            return Err(format!("{} (code {})", e["message"].as_str().unwrap_or("error"), e["code"]));
        }
        Ok(v["result"].clone())
    }
}
