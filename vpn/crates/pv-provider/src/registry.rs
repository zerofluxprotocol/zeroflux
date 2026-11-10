//! Destination registry: privacy tiers, credentials and upstream sends.

use crate::near::{NearBackend, NearModelInfo, NearVerification};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::Stream;
use pv_core::{DestinationTier, ProviderConfig, ProviderKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use zeroize::Zeroizing;

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Endpoint {
    Chat,
    Responses,
}

impl Endpoint {
    pub fn path(self) -> &'static str {
        match self {
            Endpoint::Chat => "chat/completions",
            Endpoint::Responses => "responses",
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum ProviderError {
    #[error("destination not configured: {0}")]
    NotConfigured(String),
    #[error("destination unreachable")]
    Unreachable,
    #[error("destination timed out")]
    Timeout,
    #[error("destination returned HTTP {0}")]
    Status(u16),
    #[error("destination rejected credentials")]
    Auth,
    #[error("confidential-execution verification failed: {0}")]
    Verification(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl ProviderError {
    /// Whether a different eligible destination may be tried. Never true for
    /// verification failures of the same destination class is handled by the
    /// router; this only says the failure was transient/transport-level.
    pub fn failover_eligible(&self) -> bool {
        match self {
            ProviderError::Unreachable | ProviderError::Timeout | ProviderError::NotConfigured(_) | ProviderError::Verification(_) | ProviderError::Auth => true,
            ProviderError::Status(s) => *s >= 500 || *s == 429,
            ProviderError::Protocol(_) => false,
        }
    }
}

pub struct UpstreamResponse {
    pub status: u16,
    pub content_type: String,
    pub body: ByteStream,
    /// Set when a NEAR completion receipt can be verified after the body is consumed.
    pub receipt_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptSummary {
    pub completion_id: String,
    pub verified: bool,
    pub signature_kind: String,
    pub signer: Option<String>,
    pub error: Option<String>,
}

/// A destination resolved for one request: config, upstream model and runtime tier.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub config: ProviderConfig,
    pub model: String,
    pub tier: DestinationTier,
    pub verification: Option<NearVerification>,
}

pub type CredentialFn = Arc<dyn Fn(&str) -> Option<Zeroizing<String>> + Send + Sync>;

pub struct Registry {
    providers: RwLock<Vec<ProviderConfig>>,
    near: RwLock<HashMap<String, Arc<dyn NearBackend>>>,
    verifications: RwLock<HashMap<(String, String), NearVerification>>,
    pub verification_ttl: chrono::Duration,
    pub http: reqwest::Client,
    pub creds: CredentialFn,
}

impl Registry {
    pub fn new(creds: CredentialFn) -> Self {
        Registry {
            providers: RwLock::new(vec![]),
            near: RwLock::new(HashMap::new()),
            verifications: RwLock::new(HashMap::new()),
            verification_ttl: chrono::Duration::minutes(10),
            http: reqwest::Client::builder().no_proxy().connect_timeout(std::time::Duration::from_secs(5)).build().expect("http"),
            creds,
        }
    }

    pub fn set_providers(&self, p: Vec<ProviderConfig>) {
        *self.providers.write().unwrap() = p;
        self.verifications.write().unwrap().clear();
    }

    pub fn providers(&self) -> Vec<ProviderConfig> {
        self.providers.read().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<ProviderConfig> {
        self.providers.read().unwrap().iter().find(|p| p.id == id).cloned()
    }

    pub fn set_near_backend(&self, provider_id: &str, b: Arc<dyn NearBackend>) {
        self.near.write().unwrap().insert(provider_id.to_string(), b);
        self.verifications.write().unwrap().retain(|(p, _), _| p != provider_id);
    }

    pub fn near_backend(&self, provider_id: &str) -> Option<Arc<dyn NearBackend>> {
        self.near.read().unwrap().get(provider_id).cloned()
    }

    pub fn cached_verifications(&self) -> Vec<NearVerification> {
        self.verifications.read().unwrap().values().cloned().collect()
    }

    pub fn invalidate(&self, provider_id: &str, model: &str) {
        self.verifications.write().unwrap().remove(&(provider_id.to_string(), model.to_string()));
    }

    /// Verify (or reuse a fresh verification of) a NEAR model.
    pub async fn verify_near(&self, provider_id: &str, model: &str, force: bool) -> NearVerification {
        let key = (provider_id.to_string(), model.to_string());
        if !force {
            if let Some(v) = self.verifications.read().unwrap().get(&key) {
                if v.expires_at > Utc::now() {
                    return v.clone();
                }
            }
        }
        let v = match self.near_backend(provider_id) {
            Some(b) => b.verify(model).await,
            None => NearVerification::failed(provider_id, model, "NEAR backend not configured"),
        };
        let mut v = v;
        v.expires_at = v.checked_at + self.verification_ttl;
        self.verifications.write().unwrap().insert(key, v.clone());
        v
    }

    pub async fn near_models(&self, provider_id: &str) -> Result<Vec<NearModelInfo>, ProviderError> {
        let b = self.near_backend(provider_id).ok_or_else(|| ProviderError::NotConfigured(provider_id.into()))?;
        b.list_models().await
    }

    /// Resolve the runtime privacy tier for a destination and model.
    pub async fn resolve(&self, cfg: &ProviderConfig, model: Option<&str>) -> Resolved {
        let model = model.filter(|m| !m.is_empty()).unwrap_or(&cfg.default_model).to_string();
        match cfg.kind {
            ProviderKind::NearAi => {
                let v = self.verify_near(&cfg.id, &model, false).await;
                let tier = if v.eligible { DestinationTier::AttestedTee } else { DestinationTier::ExternalUnverified };
                Resolved { config: cfg.clone(), model, tier, verification: Some(v) }
            }
            ProviderKind::Ollama => {
                let tier = if is_loopback(&cfg.base_url) { DestinationTier::Local } else { DestinationTier::ExternalUnverified };
                Resolved { config: cfg.clone(), model, tier, verification: None }
            }
            ProviderKind::OpenaiCompatible => {
                // A custom endpoint is never treated as an attested TEE.
                let tier = if cfg.declared_tier == DestinationTier::Local && is_loopback(&cfg.base_url) { DestinationTier::Local } else { DestinationTier::ExternalUnverified };
                Resolved { config: cfg.clone(), model, tier, verification: None }
            }
        }
    }

    /// Send a request body to a resolved destination. `private` requires
    /// confidential handling (NEAR: E2EE to the attested model).
    pub async fn send(&self, r: &Resolved, endpoint: Endpoint, mut body: Value, private: bool) -> Result<UpstreamResponse, ProviderError> {
        body["model"] = Value::String(r.model.clone());
        match r.config.kind {
            ProviderKind::NearAi => {
                let b = self.near_backend(&r.config.id).ok_or_else(|| ProviderError::NotConfigured(r.config.id.clone()))?;
                if private {
                    // Re-check freshness immediately before sending.
                    let v = self.verify_near(&r.config.id, &r.model, false).await;
                    if !v.eligible {
                        return Err(ProviderError::Verification(v.error.unwrap_or_else(|| "model not eligible".into())));
                    }
                }
                b.send(endpoint, body, private).await
            }
            ProviderKind::Ollama | ProviderKind::OpenaiCompatible => {
                if private && r.tier != DestinationTier::Local {
                    return Err(ProviderError::Verification("destination cannot provide confidential execution".into()));
                }
                let token = r.config.credential_ref.as_deref().and_then(|c| (self.creds)(c));
                crate::openai::send(&self.http, &r.config, endpoint, &body, token.as_deref().map(|s| s.as_str())).await
            }
        }
    }

    pub async fn verify_receipt(&self, provider_id: &str, completion_id: &str) -> ReceiptSummary {
        match self.near_backend(provider_id) {
            Some(b) => b.verify_receipt(completion_id).await,
            None => ReceiptSummary { completion_id: completion_id.into(), verified: false, signature_kind: "none".into(), signer: None, error: Some("no NEAR backend".into()) },
        }
    }
}

pub fn is_loopback(u: &str) -> bool {
    match url::Url::parse(u) {
        Ok(p) => match p.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            None => false,
        },
        Err(_) => false,
    }
}

/// Timestamp helper used by verification records.
pub fn now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loopback_detection() {
        assert!(is_loopback("http://127.0.0.1:11434/v1"));
        assert!(is_loopback("http://localhost:1/v1"));
        assert!(is_loopback("http://[::1]:8080"));
        assert!(!is_loopback("http://127.0.0.1.nip.io/v1"));
        assert!(!is_loopback("https://api.openai.com/v1"));
        assert!(!is_loopback("http://user@localhost.evil.com/"));
    }
}
