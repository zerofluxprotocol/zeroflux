//! NEAR AI Cloud gateway integration.
//!
//! Verification uses NEAR's official Rust SDK (`nearai-inference-sdk`), which
//! verifies the gateway TDX quote with TLS SPKI binding, every model report
//! (TDX quote, report-data nonce binding, signer derivation, compose/RTMR3
//! measurements, NVIDIA GPU evidence via NRAS JWT), and completion receipts.
//! ZFX VPN adds: `x-no-aliasing` on every call, GPU evidence REQUIRED,
//! eligibility only for NEAR-operated (`providerType=vllm`) models, and field
//! E2EE to the attested model key for private routes.

use crate::registry::{ByteStream, Endpoint, ProviderError, ReceiptSummary, UpstreamResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pv_core::DestinationTier;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_NEAR_BASE: &str = "https://cloud-api.near.ai/v1";
/// Spec's preferred model. Live discovery on 2026-11-08 showed this ID is an
/// alias of `z-ai/glm-5.3-flash`, so verification rejects it (see docs/deviations.md).
pub const SPEC_PREFERRED_MODEL: &str = "deepseek-ai/DeepSeek-V4-Flash";
/// Default NEAR model (NEAR-operated GPU TEE; confirmed by the project owner 2026-11-08).
/// Always verified before use; users can pick any eligible model.
pub const DEFAULT_NEAR_MODEL: &str = "deepseek-ai/DeepSeek-V4.1-Flash-1017";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NearModelInfo {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub attestation_supported: bool,
    pub aliases: Vec<String>,
    /// `PRIVATE_TEE` (NEAR-operated, attestable), `PRIVATE_3P_TEE` or `INCOGNITO`.
    pub privacy_tier: String,
}

impl NearModelInfo {
    pub fn tee_candidate(&self) -> bool {
        self.provider_type == "vllm" && self.attestation_supported
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NearVerification {
    pub provider_id: String,
    pub model: String,
    pub eligible: bool,
    pub tier: DestinationTier,
    pub checked_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub gateway_signer: Option<String>,
    pub gateway_tcb: Option<String>,
    pub gateway_tls_spki: Option<String>,
    pub model_signers: Vec<String>,
    pub model_tcb: Vec<String>,
    pub gpu_verified: bool,
    pub e2ee_key: bool,
    pub verifier: String,
    pub error: Option<String>,
    /// Normalized evidence the eligibility decision was made on.
    #[serde(default)]
    pub evidence: Option<crate::tee::TeeEvidence>,
    /// Why ZFX VPN's confidentiality decision rejected the evidence.
    #[serde(default)]
    pub rejection: Option<crate::tee::TeeRejection>,
}

impl NearVerification {
    pub fn failed(provider_id: &str, model: &str, err: &str) -> Self {
        NearVerification {
            provider_id: provider_id.into(),
            model: model.into(),
            eligible: false,
            tier: DestinationTier::ExternalUnverified,
            checked_at: Utc::now(),
            expires_at: Utc::now(),
            gateway_signer: None,
            gateway_tcb: None,
            gateway_tls_spki: None,
            model_signers: vec![],
            model_tcb: vec![],
            gpu_verified: false,
            e2ee_key: false,
            verifier: VERIFIER.into(),
            error: Some(err.into()),
            evidence: None,
            rejection: None,
        }
    }
}

pub const VERIFIER: &str = "nearai-inference-sdk 0.3.0 (Intel DCAP via PCCS, NVIDIA NRAS, gpu=required, x-no-aliasing)";

#[async_trait]
pub trait NearBackend: Send + Sync {
    async fn list_models(&self) -> Result<Vec<NearModelInfo>, ProviderError>;
    async fn verify(&self, model: &str) -> NearVerification;
    /// `private`: E2EE to the attested model. Responses must be translated to
    /// Chat by the caller before a private send.
    async fn send(&self, endpoint: Endpoint, body: Value, private: bool) -> Result<UpstreamResponse, ProviderError>;
    async fn verify_receipt(&self, completion_id: &str) -> ReceiptSummary;
    fn has_api_key(&self) -> bool;
}

pub fn parse_model_list(v: &Value) -> Vec<NearModelInfo> {
    let mut out = Vec::new();
    for m in v["models"].as_array().cloned().unwrap_or_default() {
        let md = &m["metadata"];
        let provider_type = md["providerType"].as_str().unwrap_or("unknown").to_string();
        let attestation_supported = md["attestationSupported"].as_bool().unwrap_or(false);
        let privacy_tier = match (provider_type.as_str(), attestation_supported) {
            ("vllm", true) => "PRIVATE_TEE",
            ("chutes", _) | ("tinfoil", _) => "PRIVATE_3P_TEE",
            _ => "INCOGNITO",
        }
        .to_string();
        out.push(NearModelInfo {
            id: m["modelId"].as_str().unwrap_or_default().to_string(),
            name: md["modelDisplayName"].as_str().unwrap_or_default().to_string(),
            provider_type,
            attestation_supported,
            aliases: md["aliases"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            privacy_tier,
        });
    }
    out.retain(|m| !m.id.is_empty());
    out
}

/// Returns the canonical model if `id` is only an alias.
pub fn alias_target<'a>(models: &'a [NearModelInfo], id: &str) -> Option<&'a NearModelInfo> {
    if models.iter().any(|m| m.id == id) {
        return None;
    }
    models.iter().find(|m| m.aliases.iter().any(|a| a == id))
}

#[cfg(feature = "near-sdk")]
pub use sdk::NearSdkBackend;

#[cfg(feature = "near-sdk")]
mod sdk {
    use super::*;
    use futures::StreamExt;
    use nearai_inference_sdk::{GpuEvidenceRequirement, GpuEvidenceStatus, InferenceClient, InferenceClientOptions, ModelAttestationPolicy, SigningAlgo};
    use std::time::Duration;

    pub struct NearSdkBackend {
        base_url: String,
        /// E2EE client for private routes (GPU evidence required).
        private: InferenceClient,
        /// Gateway-verified client for ordinary external use.
        plain: InferenceClient,
        http: reqwest::Client,
        has_key: bool,
    }

    fn options(base: &str, key: Option<String>, e2ee: bool) -> InferenceClientOptions {
        let mut o = InferenceClientOptions::default();
        o.base_url = base.to_string();
        o.api_key = key;
        o.e2ee = e2ee;
        o.signing_algo = SigningAlgo::Ed25519;
        o.attestation_cache_ttl = Duration::from_secs(600);
        o.model_verification.policy = Some(ModelAttestationPolicy { accepted_tcb_statuses: None, gpu_evidence: GpuEvidenceRequirement::Required });
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-no-aliasing", reqwest::header::HeaderValue::from_static("true"));
        h.insert("accept-encoding", reqwest::header::HeaderValue::from_static("identity"));
        o.headers = h;
        o
    }

    impl NearSdkBackend {
        pub fn new(base_url: &str, api_key: Option<String>) -> Result<Self, ProviderError> {
            let has_key = api_key.is_some();
            let private = InferenceClient::with_options(options(base_url, api_key.clone(), true)).map_err(|e| ProviderError::Protocol(e.to_string()))?;
            let plain = InferenceClient::with_options(options(base_url, api_key, false)).map_err(|e| ProviderError::Protocol(e.to_string()))?;
            let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| ProviderError::Protocol(e.to_string()))?;
            Ok(NearSdkBackend { base_url: base_url.trim_end_matches('/').to_string(), private, plain, http, has_key })
        }
    }

    fn map_err(e: nearai_inference_sdk::InferenceError) -> ProviderError {
        let s = e.to_string();
        if s.contains("401") || s.to_ascii_lowercase().contains("auth") {
            ProviderError::Auth
        } else if s.contains("timed out") || s.contains("timeout") {
            ProviderError::Timeout
        } else if s.contains("connect") || s.contains("dns") {
            ProviderError::Unreachable
        } else if let Some(code) = s.split("HTTP ").nth(1).and_then(|r| r.get(..3)).and_then(|c| c.parse::<u16>().ok()) {
            ProviderError::Status(code)
        } else {
            ProviderError::Verification(s)
        }
    }

    #[async_trait]
    impl NearBackend for NearSdkBackend {
        async fn list_models(&self) -> Result<Vec<NearModelInfo>, ProviderError> {
            let r = self.http.get(format!("{}/model/list?limit=500", self.base_url)).send().await.map_err(|_| ProviderError::Unreachable)?;
            if !r.status().is_success() {
                return Err(ProviderError::Status(r.status().as_u16()));
            }
            let v: Value = r.json().await.map_err(|e| ProviderError::Protocol(e.to_string()))?;
            Ok(parse_model_list(&v))
        }

        async fn verify(&self, model: &str) -> NearVerification {
            // Catalog gate first: only NEAR-operated attestable models, never aliases.
            let catalog = match self.list_models().await {
                Ok(c) => c,
                Err(e) => return NearVerification::failed("near", model, &format!("model catalog unavailable: {e}")),
            };
            if let Some(t) = alias_target(&catalog, model) {
                return NearVerification::failed("near", model, &format!("'{model}' is an alias of '{}'; select the canonical model explicitly", t.id));
            }
            let Some(info) = catalog.iter().find(|m| m.id == model) else {
                return NearVerification::failed("near", model, "model not in NEAR catalog");
            };
            if !info.tee_candidate() {
                return NearVerification::failed("near", model, &format!("model privacy tier is {} (not NEAR-operated attestable TEE)", info.privacy_tier));
            }
            match self.private.verify(model).await {
                Ok(r) => {
                    let gpu_ok = !r.models.is_empty() && r.models.iter().all(|m| m.gpu_evidence == GpuEvidenceStatus::Verified);
                    let e2ee = !r.models.is_empty() && r.models.iter().all(|m| m.signing_public_key.is_some());
                    // The SDK requested exactly this model with provider=near and
                    // x-no-aliasing, and binds each report to the request nonce.
                    let ev = crate::tee::sdk::evidence("near", &self.base_url, model, r.models.iter().map(|_| model.to_string()).collect(), Some(&r.gateway), &r.models, "", VERIFIER);
                    NearVerification {
                        provider_id: "near".into(),
                        model: model.into(),
                        eligible: false, // decided by Registry::verify_near (crate::tee::evaluate)
                        tier: DestinationTier::ExternalUnverified,
                        checked_at: Utc::now(),
                        expires_at: Utc::now(),
                        gateway_signer: Some(r.gateway.evidence.signer.signing_address.clone()),
                        gateway_tcb: Some(format!("{:?}", r.gateway.evidence.tcb_status)),
                        gateway_tls_spki: match &r.gateway.tls_binding {
                            nearai_inference_sdk::GatewayTlsBinding::Attested { spki_fingerprint } => Some(spki_fingerprint.clone()),
                            _ => None,
                        },
                        model_signers: r.models.iter().map(|m| m.evidence.signer.signing_address.clone()).collect(),
                        model_tcb: r.models.iter().map(|m| format!("{:?}", m.evidence.tcb_status)).collect(),
                        gpu_verified: gpu_ok,
                        e2ee_key: e2ee,
                        verifier: VERIFIER.into(),
                        error: None,
                        evidence: Some(ev),
                        rejection: None,
                    }
                }
                Err(e) => NearVerification::failed("near", model, &e.to_string()),
            }
        }

        async fn send(&self, endpoint: Endpoint, body: Value, private: bool) -> Result<UpstreamResponse, ProviderError> {
            if !self.has_key {
                return Err(ProviderError::Auth);
            }
            let client = if private { &self.private } else { &self.plain };
            let req = match endpoint {
                Endpoint::Chat => client.chat_request(body),
                Endpoint::Responses => {
                    if private {
                        return Err(ProviderError::Protocol("private routes use Chat Completions with E2EE".into()));
                    }
                    client.response_request(body)
                }
            }
            .map_err(map_err)?;
            let resp = client.send(req).await.map_err(map_err)?;
            let status = resp.status.as_u16();
            if status == 401 || status == 403 {
                return Err(ProviderError::Auth);
            }
            if status >= 400 {
                return Err(ProviderError::Status(status));
            }
            let content_type = resp.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_string();
            let body: ByteStream = Box::pin(resp.body.map(|r| r.map_err(map_err)));
            Ok(UpstreamResponse { status, content_type, body, receipt_provider: Some(if private { "private".into() } else { "plain".into() }) })
        }

        async fn verify_receipt(&self, completion_id: &str) -> ReceiptSummary {
            let mut last = None;
            for client in [&self.private, &self.plain] {
                match client.verify_response(completion_id).await {
                    Ok(r) => {
                        return ReceiptSummary {
                            completion_id: completion_id.into(),
                            verified: true,
                            signature_kind: format!("{:?}", r.signature_kind()),
                            signer: Some(r.signature.signer.signing_address.clone()),
                            error: None,
                        }
                    }
                    Err(e) => last = Some(e.to_string()),
                }
            }
            ReceiptSummary { completion_id: completion_id.into(), verified: false, signature_kind: "unknown".into(), signer: None, error: last }
        }

        fn has_api_key(&self) -> bool {
            self.has_key
        }
    }
}

/// Test double: a NEAR backend that forwards to an OpenAI-compatible mock and
/// reports a configurable verification outcome.
pub struct MockNear {
    pub upstream: pv_core::ProviderConfig,
    pub eligible: std::sync::atomic::AtomicBool,
    /// Signature kind the mock reports for receipts (`ProviderTee`, `Gateway`, or `none` = unverifiable).
    pub receipt_kind: std::sync::Mutex<String>,
    /// Receipt signer the mock reports (must be in the preflight signer set).
    pub receipt_signer: std::sync::Mutex<String>,
    /// Evidence the mock "verified" (synthetic; mutate it to test rejections).
    pub evidence: std::sync::Mutex<Option<crate::tee::TeeEvidence>>,
    /// Whether E2EE to the model key works for private sends.
    pub e2ee: std::sync::atomic::AtomicBool,
    pub http: reqwest::Client,
    pub models: Vec<NearModelInfo>,
}

impl MockNear {
    /// Synthetic evidence that passes `tee::evaluate` for `model` at the
    /// default NEAR base URL. Never produced by real verification.
    pub fn synthetic_evidence(model: &str) -> crate::tee::TeeEvidence {
        crate::tee::TeeEvidence {
            provider_id: "near".into(),
            base_url: DEFAULT_NEAR_BASE.into(),
            requested_model: model.into(),
            alias_of: None,
            attested_models: vec![model.into()],
            gateway_verified: true,
            provider_reports: 1,
            gpu_verified: true,
            e2ee_keys: true,
            tcb_statuses: vec!["UpToDate".into()],
            debug_enabled: false,
            compose_hashes: vec!["synthetic-compose".into()],
            os_image_hashes: vec!["synthetic-os".into()],
            signers: vec!["0xmock-model-signer".into()],
            nonce: "synthetic".into(),
            verified_at: chrono::Utc::now(),
            verifier: "mock (synthetic evidence, nothing verified)".into(),
        }
    }

    pub fn new(upstream_base: &str, eligible: bool) -> Self {
        MockNear {
            upstream: pv_core::ProviderConfig {
                schema_version: 1,
                id: "near-mock-upstream".into(),
                kind: pv_core::ProviderKind::OpenaiCompatible,
                display_name: "mock".into(),
                base_url: upstream_base.into(),
                default_model: "m".into(),
                declared_tier: DestinationTier::ExternalUnverified,
                credential_ref: None,
                enabled: true,
                allow_failover: false,
                supports_responses: true,
                timeout_secs: 30,
                zfx_hosted_transport: false,
            },
            eligible: std::sync::atomic::AtomicBool::new(eligible),
            receipt_kind: std::sync::Mutex::new(crate::registry::PROVIDER_TEE_SIGNATURE.into()),
            receipt_signer: std::sync::Mutex::new("0xmock-model-signer".into()),
            evidence: std::sync::Mutex::new(None),
            e2ee: std::sync::atomic::AtomicBool::new(true),
            http: reqwest::Client::new(),
            models: vec![],
        }
    }
}

#[async_trait]
impl NearBackend for MockNear {
    async fn list_models(&self) -> Result<Vec<NearModelInfo>, ProviderError> {
        Ok(self.models.clone())
    }
    async fn verify(&self, model: &str) -> NearVerification {
        let ok = self.eligible.load(std::sync::atomic::Ordering::SeqCst);
        let mut v = NearVerification::failed("near", model, "mock: not eligible");
        if ok {
            v.error = None;
            v.gpu_verified = true;
            v.e2ee_key = true;
            v.verifier = "mock (synthetic evidence, nothing verified)".into();
            v.evidence = Some(self.evidence.lock().unwrap().clone().unwrap_or_else(|| MockNear::synthetic_evidence(model)));
        }
        v
    }
    async fn send(&self, endpoint: Endpoint, body: Value, private: bool) -> Result<UpstreamResponse, ProviderError> {
        if private && !self.eligible.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ProviderError::Verification("mock: not eligible".into()));
        }
        if private && !self.e2ee.load(std::sync::atomic::Ordering::SeqCst) {
            // A real E2EE failure: never fall back to a plaintext send.
            return Err(ProviderError::Verification("E2EE to the attested model key is unavailable".into()));
        }
        crate::openai::send(&self.http, &self.upstream, endpoint, &body, None).await
    }
    async fn verify_receipt(&self, completion_id: &str) -> ReceiptSummary {
        let kind = self.receipt_kind.lock().unwrap().clone();
        if kind == "none" {
            return ReceiptSummary { completion_id: completion_id.into(), verified: false, signature_kind: "unknown".into(), signer: None, error: Some("mock: signature unavailable".into()) };
        }
        ReceiptSummary { completion_id: completion_id.into(), verified: true, signature_kind: kind, signer: Some(self.receipt_signer.lock().unwrap().clone()), error: None }
    }
    fn has_api_key(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_list_tiers_and_aliases() {
        let v: Value = serde_json::json!({"models":[
            {"modelId":"deepseek-ai/DeepSeek-V4.1-Flash-1017","metadata":{"providerType":"vllm","attestationSupported":true,"aliases":[]}},
            {"modelId":"z-ai/glm-5.3-flash","metadata":{"providerType":"vllm","attestationSupported":true,"aliases":["deepseek-ai/DeepSeek-V4-Flash"]}},
            {"modelId":"deepseek/deepseek-v4.1-flash","metadata":{"providerType":"external","attestationSupported":false,"aliases":[]}},
            {"modelId":"deepseek/deepseek-v3.2","metadata":{"providerType":"chutes","attestationSupported":true,"aliases":[]}}
        ]});
        let m = parse_model_list(&v);
        assert_eq!(m[0].privacy_tier, "PRIVATE_TEE");
        assert!(m[0].tee_candidate());
        assert_eq!(m[2].privacy_tier, "INCOGNITO");
        assert_eq!(m[3].privacy_tier, "PRIVATE_3P_TEE");
        assert!(!m[3].tee_candidate());
        assert_eq!(alias_target(&m, SPEC_PREFERRED_MODEL).unwrap().id, "z-ai/glm-5.3-flash");
        assert!(alias_target(&m, "z-ai/glm-5.3-flash").is_none());
    }
}
