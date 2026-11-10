//! Structured, non-sensitive error contract returned to local callers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Error codes. Version 2 (ZFX VPN) names are `ZFX_*`. Prompt VPN 0.1 names
/// (`PV_*`) are accepted on input as aliases and reported in `legacy_code`, and
/// callers that still match on them can request them with the
/// `X-ZFX-Error-Codes: pv1` header for the migration period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ErrorCode {
    #[serde(rename = "ZFX_POLICY_BLOCKED", alias = "PV_POLICY_BLOCKED")]
    PolicyBlocked,
    #[serde(rename = "ZFX_APPROVAL_REQUIRED", alias = "PV_APPROVAL_REQUIRED")]
    ApprovalRequired,
    /// Detection could not run (model failed, missing, or invalid offsets).
    #[serde(rename = "ZFX_DETECTION_UNAVAILABLE", alias = "PV_CLASSIFIER_UNAVAILABLE")]
    ClassifierUnavailable,
    #[serde(rename = "ZFX_UNSUPPORTED_CONTENT", alias = "PV_UNSUPPORTED_CONTENT")]
    UnsupportedContent,
    /// No destination is permitted for this request (none configured, or policy forbids all).
    #[serde(rename = "ZFX_ROUTE_FORBIDDEN", alias = "PV_ROUTE_FORBIDDEN")]
    RouteForbidden,
    /// Confidential execution was required but the TEE could not be verified.
    #[serde(rename = "ZFX_TEE_UNVERIFIED")]
    TeeUnverified,
    /// Confidential fallback required but none is configured or consented.
    #[serde(rename = "ZFX_FALLBACK_UNAVAILABLE")]
    FallbackUnavailable,
    /// The caller forbade buffering but the privacy policy needs to buffer output.
    #[serde(rename = "ZFX_STREAM_SAFETY_BUFFERED")]
    StreamSafetyBuffered,
    #[serde(rename = "ZFX_POLICY_INVALID", alias = "PV_POLICY_INVALID")]
    PolicyInvalid,
    #[serde(rename = "ZFX_VAULT_LOCKED", alias = "PV_VAULT_LOCKED")]
    VaultLocked,
    #[serde(rename = "ZFX_UPSTREAM_FAILURE", alias = "PV_PROVIDER_ERROR")]
    ProviderError,
    #[serde(rename = "ZFX_UNAUTHORIZED", alias = "PV_UNAUTHORIZED")]
    Unauthorized,
    #[serde(rename = "ZFX_FORBIDDEN_SCOPE")]
    ForbiddenScope,
    #[serde(rename = "ZFX_BAD_REQUEST", alias = "PV_BAD_REQUEST")]
    BadRequest,
    #[serde(rename = "ZFX_PAYLOAD_TOO_LARGE", alias = "PV_PAYLOAD_TOO_LARGE")]
    PayloadTooLarge,
    #[serde(rename = "ZFX_OUTPUT_BLOCKED", alias = "PV_OUTPUT_BLOCKED")]
    OutputBlocked,
    #[serde(rename = "ZFX_RATE_LIMITED")]
    RateLimited,
    #[serde(rename = "ZFX_QUOTA_EXCEEDED")]
    QuotaExceeded,
    #[serde(rename = "ZFX_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[serde(rename = "ZFX_NOT_FOUND")]
    NotFound,
}

pub const ERROR_CODES_VERSION: u32 = 2;

impl ErrorCode {
    pub const ALL: [ErrorCode; 20] = [
        ErrorCode::PolicyBlocked,
        ErrorCode::ApprovalRequired,
        ErrorCode::ClassifierUnavailable,
        ErrorCode::UnsupportedContent,
        ErrorCode::RouteForbidden,
        ErrorCode::TeeUnverified,
        ErrorCode::FallbackUnavailable,
        ErrorCode::StreamSafetyBuffered,
        ErrorCode::PolicyInvalid,
        ErrorCode::VaultLocked,
        ErrorCode::ProviderError,
        ErrorCode::Unauthorized,
        ErrorCode::ForbiddenScope,
        ErrorCode::BadRequest,
        ErrorCode::PayloadTooLarge,
        ErrorCode::OutputBlocked,
        ErrorCode::RateLimited,
        ErrorCode::QuotaExceeded,
        ErrorCode::IdempotencyConflict,
        ErrorCode::NotFound,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::PolicyBlocked => "ZFX_POLICY_BLOCKED",
            ErrorCode::ApprovalRequired => "ZFX_APPROVAL_REQUIRED",
            ErrorCode::ClassifierUnavailable => "ZFX_DETECTION_UNAVAILABLE",
            ErrorCode::UnsupportedContent => "ZFX_UNSUPPORTED_CONTENT",
            ErrorCode::RouteForbidden => "ZFX_ROUTE_FORBIDDEN",
            ErrorCode::TeeUnverified => "ZFX_TEE_UNVERIFIED",
            ErrorCode::FallbackUnavailable => "ZFX_FALLBACK_UNAVAILABLE",
            ErrorCode::StreamSafetyBuffered => "ZFX_STREAM_SAFETY_BUFFERED",
            ErrorCode::PolicyInvalid => "ZFX_POLICY_INVALID",
            ErrorCode::VaultLocked => "ZFX_VAULT_LOCKED",
            ErrorCode::ProviderError => "ZFX_UPSTREAM_FAILURE",
            ErrorCode::Unauthorized => "ZFX_UNAUTHORIZED",
            ErrorCode::ForbiddenScope => "ZFX_FORBIDDEN_SCOPE",
            ErrorCode::BadRequest => "ZFX_BAD_REQUEST",
            ErrorCode::PayloadTooLarge => "ZFX_PAYLOAD_TOO_LARGE",
            ErrorCode::OutputBlocked => "ZFX_OUTPUT_BLOCKED",
            ErrorCode::RateLimited => "ZFX_RATE_LIMITED",
            ErrorCode::QuotaExceeded => "ZFX_QUOTA_EXCEEDED",
            ErrorCode::IdempotencyConflict => "ZFX_IDEMPOTENCY_CONFLICT",
            ErrorCode::NotFound => "ZFX_NOT_FOUND",
        }
    }

    /// Prompt VPN 0.1 name for this condition, if one existed. New conditions
    /// that 0.1 reported as `PV_ROUTE_FORBIDDEN` map back to it.
    pub fn legacy_str(self) -> Option<&'static str> {
        Some(match self {
            ErrorCode::PolicyBlocked => "PV_POLICY_BLOCKED",
            ErrorCode::ApprovalRequired => "PV_APPROVAL_REQUIRED",
            ErrorCode::ClassifierUnavailable => "PV_CLASSIFIER_UNAVAILABLE",
            ErrorCode::UnsupportedContent => "PV_UNSUPPORTED_CONTENT",
            ErrorCode::RouteForbidden | ErrorCode::TeeUnverified | ErrorCode::FallbackUnavailable => "PV_ROUTE_FORBIDDEN",
            ErrorCode::PolicyInvalid => "PV_POLICY_INVALID",
            ErrorCode::VaultLocked => "PV_VAULT_LOCKED",
            ErrorCode::ProviderError => "PV_PROVIDER_ERROR",
            ErrorCode::Unauthorized => "PV_UNAUTHORIZED",
            ErrorCode::BadRequest => "PV_BAD_REQUEST",
            ErrorCode::PayloadTooLarge => "PV_PAYLOAD_TOO_LARGE",
            ErrorCode::OutputBlocked => "PV_OUTPUT_BLOCKED",
            ErrorCode::StreamSafetyBuffered | ErrorCode::ForbiddenScope | ErrorCode::RateLimited | ErrorCode::QuotaExceeded | ErrorCode::IdempotencyConflict | ErrorCode::NotFound => return None,
        })
    }

    /// Parse a current or legacy code string.
    pub fn parse(s: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.into_iter().find(|c| c.as_str() == s || c.legacy_str() == Some(s) && !matches!(c, ErrorCode::TeeUnverified | ErrorCode::FallbackUnavailable))
    }

    pub fn error_type(self) -> &'static str {
        match self {
            ErrorCode::PolicyBlocked | ErrorCode::RouteForbidden | ErrorCode::OutputBlocked => "zfx_policy_block",
            ErrorCode::TeeUnverified | ErrorCode::FallbackUnavailable => "zfx_route_unavailable",
            ErrorCode::ApprovalRequired => "zfx_approval_required",
            ErrorCode::ClassifierUnavailable | ErrorCode::VaultLocked | ErrorCode::PolicyInvalid => "zfx_fail_closed",
            ErrorCode::UnsupportedContent | ErrorCode::BadRequest | ErrorCode::PayloadTooLarge | ErrorCode::StreamSafetyBuffered | ErrorCode::IdempotencyConflict | ErrorCode::NotFound => "zfx_invalid_request",
            ErrorCode::ProviderError => "zfx_upstream_error",
            ErrorCode::Unauthorized | ErrorCode::ForbiddenScope => "zfx_unauthorized",
            ErrorCode::RateLimited | ErrorCode::QuotaExceeded => "zfx_limit",
        }
    }

    /// `type` value Prompt VPN 0.1 used, for `X-ZFX-Error-Codes: pv1` callers.
    pub fn legacy_error_type(self) -> String {
        self.error_type().replacen("zfx_", "promptvpn_", 1).replace("promptvpn_route_unavailable", "promptvpn_policy_block").replace("promptvpn_upstream_error", "promptvpn_provider_error")
    }

    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::PolicyBlocked | ErrorCode::RouteForbidden | ErrorCode::OutputBlocked | ErrorCode::ForbiddenScope => 403,
            ErrorCode::TeeUnverified | ErrorCode::FallbackUnavailable => 403,
            ErrorCode::ApprovalRequired => 428,
            ErrorCode::ClassifierUnavailable | ErrorCode::VaultLocked | ErrorCode::PolicyInvalid => 503,
            ErrorCode::UnsupportedContent => 415,
            ErrorCode::BadRequest => 400,
            ErrorCode::StreamSafetyBuffered => 409,
            ErrorCode::IdempotencyConflict => 409,
            ErrorCode::NotFound => 404,
            ErrorCode::PayloadTooLarge => 413,
            ErrorCode::ProviderError => 502,
            ErrorCode::Unauthorized => 401,
            ErrorCode::RateLimited | ErrorCode::QuotaExceeded => 429,
        }
    }

    pub fn retryable(self) -> bool {
        matches!(self, ErrorCode::ApprovalRequired | ErrorCode::ClassifierUnavailable | ErrorCode::ProviderError | ErrorCode::VaultLocked | ErrorCode::RateLimited | ErrorCode::TeeUnverified)
    }
}

/// Wire error. `message` must never contain matched sensitive spans.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PvErrorBody {
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: ErrorCode,
    pub message: String,
    pub event_id: Option<String>,
    pub approval_id: Option<String>,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// Prompt VPN 0.1 code for the same condition (migration aid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PvErrorEnvelope {
    pub error: PvErrorBody,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct PvError {
    pub code: ErrorCode,
    pub message: String,
    pub event_id: Option<String>,
    pub approval_id: Option<String>,
    pub categories: Vec<String>,
}

impl PvError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        PvError { code, message: message.into(), event_id: None, approval_id: None, categories: vec![] }
    }
    pub fn with_event(mut self, id: impl Into<String>) -> Self {
        self.event_id = Some(id.into());
        self
    }
    pub fn with_approval(mut self, id: impl Into<String>) -> Self {
        self.approval_id = Some(id.into());
        self
    }
    pub fn with_categories(mut self, c: Vec<String>) -> Self {
        self.categories = c;
        self
    }
    pub fn envelope(&self) -> PvErrorEnvelope {
        PvErrorEnvelope {
            error: PvErrorBody {
                error_type: self.code.error_type().into(),
                code: self.code,
                message: self.message.clone(),
                event_id: self.event_id.clone(),
                approval_id: self.approval_id.clone(),
                retryable: self.code.retryable(),
                categories: self.categories.clone(),
                legacy_code: self.code.legacy_str().map(String::from),
            },
        }
    }

    /// Envelope as JSON. `legacy` (from `X-ZFX-Error-Codes: pv1`) reports the
    /// Prompt VPN 0.1 `code` and `type` for callers that still match on them.
    pub fn envelope_json(&self, legacy: bool) -> serde_json::Value {
        let mut v = serde_json::to_value(self.envelope()).expect("envelope");
        if legacy {
            if let Some(l) = self.code.legacy_str() {
                v["error"]["code"] = serde_json::Value::String(l.into());
                v["error"]["type"] = serde_json::Value::String(self.code.legacy_error_type());
                v["error"]["zfx_code"] = serde_json::Value::String(self.code.as_str().into());
                if let Some(o) = v["error"].as_object_mut() {
                    o.remove("legacy_code");
                }
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_codes_accepted_and_reported() {
        let c: ErrorCode = serde_json::from_str("\"PV_PROVIDER_ERROR\"").unwrap();
        assert_eq!(c, ErrorCode::ProviderError);
        assert_eq!(serde_json::to_string(&c).unwrap(), "\"ZFX_UPSTREAM_FAILURE\"");
        let c: ErrorCode = serde_json::from_str("\"PV_CLASSIFIER_UNAVAILABLE\"").unwrap();
        assert_eq!(c, ErrorCode::ClassifierUnavailable);
        assert_eq!(ErrorCode::parse("PV_ROUTE_FORBIDDEN"), Some(ErrorCode::RouteForbidden));
        assert_eq!(ErrorCode::parse("ZFX_TEE_UNVERIFIED"), Some(ErrorCode::TeeUnverified));
        for c in ErrorCode::ALL {
            assert_eq!(ErrorCode::parse(c.as_str()), Some(c));
            let back: ErrorCode = serde_json::from_value(serde_json::to_value(c).unwrap()).unwrap();
            assert_eq!(back, c);
        }
    }

    #[test]
    fn spec_codes_present() {
        for s in ["ZFX_POLICY_BLOCKED", "ZFX_APPROVAL_REQUIRED", "ZFX_DETECTION_UNAVAILABLE", "ZFX_TEE_UNVERIFIED", "ZFX_FALLBACK_UNAVAILABLE", "ZFX_UNSUPPORTED_CONTENT", "ZFX_STREAM_SAFETY_BUFFERED", "ZFX_UPSTREAM_FAILURE"] {
            assert!(ErrorCode::parse(s).is_some(), "{s}");
        }
    }

    #[test]
    fn envelope_modes() {
        let e = PvError::new(ErrorCode::ProviderError, "x");
        let v = e.envelope_json(false);
        assert_eq!(v["error"]["code"], "ZFX_UPSTREAM_FAILURE");
        assert_eq!(v["error"]["legacy_code"], "PV_PROVIDER_ERROR");
        assert_eq!(v["error"]["type"], "zfx_upstream_error");
        let v = e.envelope_json(true);
        assert_eq!(v["error"]["code"], "PV_PROVIDER_ERROR");
        assert_eq!(v["error"]["type"], "promptvpn_provider_error");
        assert_eq!(v["error"]["zfx_code"], "ZFX_UPSTREAM_FAILURE");
    }
}
