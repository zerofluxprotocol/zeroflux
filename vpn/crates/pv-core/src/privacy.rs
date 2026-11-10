//! ZFX VPN privacy tiers, the `zfx_privacy` request extension and the
//! per-request route record returned to callers.
//!
//! The extension can only make a request stricter than the device or tenant
//! policy. It never enables a destination, a consent or an action that the
//! owner's policy does not already allow.

use crate::errors::{ErrorCode, PvError};
use crate::types::{DestinationTier, ProviderKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a caller can rely on for one request's destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrivacyTier {
    /// Generation ran on this device (loopback Ollama or a declared local endpoint).
    LocalOnly,
    /// A public or unverified model received only content the policy allows
    /// off-device, after detection and transformation.
    TransformedPublic,
    /// A NEAR AI model whose provider-level TEE attestation verified for this
    /// exact model before the request was sent.
    AttestedConfidential,
    /// A destination that claims confidential execution (a NEAR AI model) but
    /// whose provider-level attestation did not verify. Treated like a public
    /// model: it only receives transformed content.
    UnverifiedExternal,
}

impl PrivacyTier {
    pub fn of(kind: ProviderKind, tier: DestinationTier) -> PrivacyTier {
        match (tier, kind) {
            (DestinationTier::Local, _) => PrivacyTier::LocalOnly,
            (DestinationTier::AttestedTee, _) => PrivacyTier::AttestedConfidential,
            (DestinationTier::ExternalUnverified, ProviderKind::NearAi) => PrivacyTier::UnverifiedExternal,
            (DestinationTier::ExternalUnverified, _) => PrivacyTier::TransformedPublic,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PrivacyTier::LocalOnly => "LOCAL_ONLY",
            PrivacyTier::TransformedPublic => "TRANSFORMED_PUBLIC",
            PrivacyTier::AttestedConfidential => "ATTESTED_CONFIDENTIAL",
            PrivacyTier::UnverifiedExternal => "UNVERIFIED_EXTERNAL",
        }
    }
}

/// Requested privacy mode. Ordered from least to most restrictive in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    /// Follow the owner's policy: transform for the selected model, private
    /// route or review when transformation is not enough.
    #[default]
    Auto,
    /// Only transformed sends to the selected model. Anything that would need a
    /// private route fails closed instead.
    TransformOnly,
    /// The whole request must run on a verified confidential (TEE) model or on
    /// this device. Never sent to a public model, even transformed.
    Confidential,
    /// Never leave the device: only LOCAL_ONLY destinations.
    LocalOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnUnavailable {
    /// Fail closed with an error (default).
    #[default]
    Block,
    /// Hold for owner review (local mode only; hosted rejects it).
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreamBuffering {
    /// Buffer output when the policy needs it, and say so in headers.
    #[default]
    Allow,
    /// Fail with ZFX_STREAM_SAFETY_BUFFERED instead of buffering.
    Reject,
}

/// The `zfx_privacy` request extension. Strict: unknown fields are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivacyRequest {
    #[serde(default)]
    pub mode: PrivacyMode,
    /// NEAR AI model for confidential fallback. Must pass provider-level
    /// attestation; the owner's TEE consent is still required.
    #[serde(default)]
    pub fallback_model: Option<String>,
    #[serde(default)]
    pub on_unavailable: OnUnavailable,
    #[serde(default)]
    pub stream_buffering: StreamBuffering,
    /// Require a verified provider-TEE response signature before any output is
    /// released. Forces buffering on confidential routes; fails closed when the
    /// signature is unavailable.
    #[serde(default)]
    pub require_receipt: bool,
    /// Conversation scope for placeholder restoration (alternative to the
    /// `X-ZFX-Conversation` header).
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Set by the protected-transport client: the payload was transformed on
    /// the client, which keeps the mapping. The gateway must not mint or
    /// restore placeholders.
    #[serde(default)]
    pub preprocessed: bool,
}

pub const EXTENSION_KEY: &str = "zfx_privacy";

impl PrivacyRequest {
    /// Remove and validate the extension from an OpenAI request body. The body
    /// left behind is what standard upstream adapters receive.
    pub fn take_from(body: &mut Value) -> Result<PrivacyRequest, PvError> {
        let Some(obj) = body.as_object_mut() else { return Ok(PrivacyRequest::default()) };
        let Some(v) = obj.remove(EXTENSION_KEY) else { return Ok(PrivacyRequest::default()) };
        if v.is_null() {
            return Ok(PrivacyRequest::default());
        }
        // Never echo the field contents: they may contain anything.
        let p: PrivacyRequest = serde_json::from_value(v).map_err(|e| PvError::new(ErrorCode::BadRequest, format!("invalid zfx_privacy extension: {}", sanitize_serde(&e.to_string()))))?;
        if let Some(m) = &p.fallback_model {
            if m.is_empty() || m.len() > 200 || !m.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:".contains(c)) {
                return Err(PvError::new(ErrorCode::BadRequest, "invalid zfx_privacy.fallback_model"));
            }
        }
        if let Some(c) = &p.conversation_id {
            if c.is_empty() || c.len() > 128 || !c.chars().all(|c| c.is_ascii_graphic()) {
                return Err(PvError::new(ErrorCode::BadRequest, "invalid zfx_privacy.conversation_id"));
            }
        }
        Ok(p)
    }
}

/// serde messages can quote input values; keep only the structural part.
fn sanitize_serde(s: &str) -> String {
    let s = s.split(" at line").next().unwrap_or(s);
    if let Some(i) = s.find("unknown field `") {
        let rest = &s[i + 15..];
        let name: String = rest.chars().take_while(|c| *c != '`').filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(40).collect();
        return format!("unknown field {name}");
    }
    if s.contains("unknown variant") {
        return "unknown enum value".into();
    }
    "wrong type or shape".into()
}

/// Verification state of the route's confidentiality claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VerificationStatus {
    /// No confidentiality claim is made for this route.
    NotApplicable,
    /// Provider-level attestation verified before sending.
    Verified,
    /// Sent after provider attestation verified; the per-response provider
    /// signature has not been verified yet (or only a gateway signature exists).
    PendingVerification,
    /// The per-response provider-TEE signature verified.
    ReceiptVerified,
    /// Only a gateway signature is available for the response. It proves the
    /// gateway relayed these bytes, not that a model TEE produced them.
    GatewayOnly,
    /// Verification failed after the response was produced.
    Failed,
}

/// Non-sensitive record of how one request was routed. Returned in headers and
/// from `GET /v1/zfx/requests/{id}/privacy`. Never contains content or values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PrivacyRoute {
    pub request_id: String,
    pub privacy_tier: PrivacyTier,
    pub destination_id: String,
    pub model: String,
    pub mode: PrivacyMode,
    /// `ALLOW`, `TRANSFORM`, `PRIVATE_ROUTE`.
    pub action: String,
    pub transformed_values: u32,
    pub verification: VerificationStatus,
    pub verifier: Option<String>,
    /// `incremental` or `buffered`.
    pub stream_mode: String,
    pub preprocessed: bool,
    pub created_at: String,
}

impl PrivacyRoute {
    /// Compact header value: no values, no user text.
    pub fn header_value(&self) -> String {
        format!("tier={}; action={}; dest={}; verification={}; stream={}", self.privacy_tier.as_str(), self.action, self.destination_id, serde_json::to_value(self.verification).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(), self.stream_mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extension_stripped_and_strict() {
        let mut b = json!({"model": "m", "messages": [], "zfx_privacy": {"mode": "confidential", "fallback_model": "deepseek-ai/DeepSeek-V4.1-Flash-1017", "on_unavailable": "block"}});
        let p = PrivacyRequest::take_from(&mut b).unwrap();
        assert_eq!(p.mode, PrivacyMode::Confidential);
        assert!(b.get("zfx_privacy").is_none());
        let mut b = json!({"zfx_privacy": {"mode": "auto", "secret_field": "alice@example.com"}});
        let e = PrivacyRequest::take_from(&mut b).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        assert!(!e.message.contains("alice"));
        let mut b = json!({"zfx_privacy": {"mode": "alice@example.com"}});
        let e = PrivacyRequest::take_from(&mut b).unwrap_err();
        assert!(!e.message.contains("alice"), "{}", e.message);
        let mut b = json!({"zfx_privacy": {"fallback_model": "x y"}});
        assert!(PrivacyRequest::take_from(&mut b).is_err());
        let mut b = json!({"model": "m"});
        assert_eq!(PrivacyRequest::take_from(&mut b).unwrap(), PrivacyRequest::default());
    }

    #[test]
    fn tiers() {
        assert_eq!(PrivacyTier::of(ProviderKind::NearAi, DestinationTier::ExternalUnverified), PrivacyTier::UnverifiedExternal);
        assert_eq!(PrivacyTier::of(ProviderKind::OpenaiCompatible, DestinationTier::ExternalUnverified), PrivacyTier::TransformedPublic);
        assert_eq!(PrivacyTier::of(ProviderKind::NearAi, DestinationTier::AttestedTee), PrivacyTier::AttestedConfidential);
        assert_eq!(PrivacyTier::of(ProviderKind::Ollama, DestinationTier::Local), PrivacyTier::LocalOnly);
    }
}
