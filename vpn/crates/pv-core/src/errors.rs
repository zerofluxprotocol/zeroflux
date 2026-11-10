//! Structured, non-sensitive error contract returned to local callers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ErrorCode {
    #[serde(rename = "PV_POLICY_BLOCKED")]
    PolicyBlocked,
    #[serde(rename = "PV_APPROVAL_REQUIRED")]
    ApprovalRequired,
    #[serde(rename = "PV_CLASSIFIER_UNAVAILABLE")]
    ClassifierUnavailable,
    #[serde(rename = "PV_UNSUPPORTED_CONTENT")]
    UnsupportedContent,
    #[serde(rename = "PV_ROUTE_FORBIDDEN")]
    RouteForbidden,
    #[serde(rename = "PV_POLICY_INVALID")]
    PolicyInvalid,
    #[serde(rename = "PV_VAULT_LOCKED")]
    VaultLocked,
    #[serde(rename = "PV_PROVIDER_ERROR")]
    ProviderError,
    #[serde(rename = "PV_UNAUTHORIZED")]
    Unauthorized,
    #[serde(rename = "PV_BAD_REQUEST")]
    BadRequest,
    #[serde(rename = "PV_PAYLOAD_TOO_LARGE")]
    PayloadTooLarge,
    #[serde(rename = "PV_OUTPUT_BLOCKED")]
    OutputBlocked,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::PolicyBlocked => "PV_POLICY_BLOCKED",
            ErrorCode::ApprovalRequired => "PV_APPROVAL_REQUIRED",
            ErrorCode::ClassifierUnavailable => "PV_CLASSIFIER_UNAVAILABLE",
            ErrorCode::UnsupportedContent => "PV_UNSUPPORTED_CONTENT",
            ErrorCode::RouteForbidden => "PV_ROUTE_FORBIDDEN",
            ErrorCode::PolicyInvalid => "PV_POLICY_INVALID",
            ErrorCode::VaultLocked => "PV_VAULT_LOCKED",
            ErrorCode::ProviderError => "PV_PROVIDER_ERROR",
            ErrorCode::Unauthorized => "PV_UNAUTHORIZED",
            ErrorCode::BadRequest => "PV_BAD_REQUEST",
            ErrorCode::PayloadTooLarge => "PV_PAYLOAD_TOO_LARGE",
            ErrorCode::OutputBlocked => "PV_OUTPUT_BLOCKED",
        }
    }

    pub fn error_type(self) -> &'static str {
        match self {
            ErrorCode::PolicyBlocked | ErrorCode::RouteForbidden | ErrorCode::OutputBlocked => "promptvpn_policy_block",
            ErrorCode::ApprovalRequired => "promptvpn_approval_required",
            ErrorCode::ClassifierUnavailable | ErrorCode::VaultLocked | ErrorCode::PolicyInvalid => "promptvpn_fail_closed",
            ErrorCode::UnsupportedContent | ErrorCode::BadRequest | ErrorCode::PayloadTooLarge => "promptvpn_invalid_request",
            ErrorCode::ProviderError => "promptvpn_provider_error",
            ErrorCode::Unauthorized => "promptvpn_unauthorized",
        }
    }

    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::PolicyBlocked | ErrorCode::RouteForbidden | ErrorCode::OutputBlocked => 403,
            ErrorCode::ApprovalRequired => 428,
            ErrorCode::ClassifierUnavailable | ErrorCode::VaultLocked | ErrorCode::PolicyInvalid => 503,
            ErrorCode::UnsupportedContent => 415,
            ErrorCode::BadRequest => 400,
            ErrorCode::PayloadTooLarge => 413,
            ErrorCode::ProviderError => 502,
            ErrorCode::Unauthorized => 401,
        }
    }

    pub fn retryable(self) -> bool {
        matches!(self, ErrorCode::ApprovalRequired | ErrorCode::ClassifierUnavailable | ErrorCode::ProviderError | ErrorCode::VaultLocked)
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
            },
        }
    }
}
