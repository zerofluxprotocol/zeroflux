//! Minimal Ollama probe (version, installed models) for the optional local
//! generation destination and diagnostics. Ollama must be on loopback.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum OllamaError {
    #[error("ollama not reachable at {0}")]
    Unreachable(String),
    #[error("ollama base URL must be loopback (127.0.0.1, ::1 or localhost)")]
    NotLoopback,
    #[error("ollama error: {0}")]
    Api(String),
    #[error("model identity mismatch: {0}")]
    Identity(String),
    #[error("timeout")]
    Timeout,
}

#[derive(Clone)]
pub struct Ollama {
    pub base: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagModel {
    pub name: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub size: u64,
}

pub fn is_loopback_url(u: &str) -> bool {
    match url_host(u) {
        Some(h) => h == "127.0.0.1" || h == "::1" || h == "[::1]" || h == "localhost",
        None => false,
    }
}

fn url_host(u: &str) -> Option<String> {
    let rest = u.split("://").nth(1)?;
    let hostport = rest.split('/').next()?;
    let hostport = hostport.rsplit('@').next()?;
    if let Some(stripped) = hostport.strip_prefix('[') {
        return Some(format!("[{}]", stripped.split(']').next()?));
    }
    Some(hostport.split(':').next()?.to_ascii_lowercase())
}

impl Ollama {
    pub fn new(base: &str) -> Result<Self, OllamaError> {
        if !is_loopback_url(base) {
            return Err(OllamaError::NotLoopback);
        }
        let http = reqwest::Client::builder().no_proxy().connect_timeout(Duration::from_secs(3)).build().expect("client");
        Ok(Ollama { base: base.trim_end_matches('/').to_string(), http })
    }

    pub async fn version(&self) -> Result<String, OllamaError> {
        let r = self
            .http
            .get(format!("{}/api/version", self.base))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|_| OllamaError::Unreachable(self.base.clone()))?;
        let v: Value = r.json().await.map_err(|e| OllamaError::Api(e.to_string()))?;
        Ok(v["version"].as_str().unwrap_or("unknown").to_string())
    }

    pub async fn tags(&self) -> Result<Vec<TagModel>, OllamaError> {
        let r = self
            .http
            .get(format!("{}/api/tags", self.base))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|_| OllamaError::Unreachable(self.base.clone()))?;
        let v: Value = r.json().await.map_err(|e| OllamaError::Api(e.to_string()))?;
        Ok(serde_json::from_value(v["models"].clone()).unwrap_or_default())
    }

    pub async fn has_model(&self, name: &str) -> Result<bool, OllamaError> {
        let want = if name.contains(':') { name.to_string() } else { format!("{name}:latest") };
        Ok(self.tags().await?.iter().any(|m| m.name == want || m.name == name))
    }

}

/// Official install guidance; Prompt VPN never installs Ollama itself.
pub fn install_instructions(os: &str) -> &'static str {
    match os {
        "macos" => "Install Ollama from the official site: https://ollama.com/download/mac (requires macOS 14+). Open the app once so the `ollama` CLI is linked, then re-run `promptvpn ollama status`.",
        "linux" => "Install Ollama from the official site: https://ollama.com/download/linux . Review the script before running it (curl -fsSL https://ollama.com/install.sh -o ollama-install.sh && less ollama-install.sh && sh ollama-install.sh), or use the manual tarball from https://docs.ollama.com/linux . Then start it with `ollama serve`.",
        _ => "See https://ollama.com/download for official installers.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loopback() {
        assert!(is_loopback_url("http://127.0.0.1:11434"));
        assert!(is_loopback_url("http://localhost:11434/"));
        assert!(is_loopback_url("http://[::1]:11434"));
        assert!(!is_loopback_url("http://10.0.0.2:11434"));
        assert!(!is_loopback_url("http://127.0.0.1.evil.com:11434"));
        assert!(!is_loopback_url("http://localhost@evil.com:11434"));
    }
}
