//! Serialized contracts shared by the daemon, CLI and dashboard.
//!
//! Every top-level record carries a `schema_version`. Rust types are the
//! source of truth; `promptvpn schema export` writes JSON Schema files to
//! `schemas/`, from which the dashboard TypeScript types are generated.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const FINDING_SCHEMA_VERSION: u32 = 1;
pub const POLICY_SCHEMA_VERSION: u32 = 1;
pub const DECISION_SCHEMA_VERSION: u32 = 1;
pub const AUDIT_SCHEMA_VERSION: u32 = 1;
pub const PENDING_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_SCHEMA_VERSION: u32 = 1;
pub const CLIENT_INTEGRATION_SCHEMA_VERSION: u32 = 1;
pub const TRANSFORM_MAP_SCHEMA_VERSION: u32 = 1;

/// Which family of classifier produced a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierType {
    /// Built-in concepts: regex/checksum recognizers and the GLiNER span model.
    Builtin,
    /// User-registered exact sensitive values.
    Exact,
    /// Owner-defined entity types found by the span model (GLiNER zero-shot labels).
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Low,
    Moderate,
    High,
    Critical,
}

/// Whether content flows to the model (inbound to the provider) or back to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Request content leaving the device toward a destination.
    #[default]
    Inbound,
    /// Model-generated content returning to the local caller.
    Outbound,
    Both,
}

/// One detected sensitive span (or a whole-field marker such as a classifier failure).
///
/// Offsets are UTF-8 byte offsets into the exact local buffer of `field_path`.
/// A finding never carries the matched text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    #[serde(default = "finding_schema_version")]
    pub schema_version: u32,
    pub finding_id: String,
    pub classifier_type: ClassifierType,
    /// Built-in category (e.g. `GOVERNMENT_ID`), `EXACT_SECRET`, `CUSTOM:<id>`,
    /// `UNCERTAIN` or `CLASSIFIER_FAILURE`.
    pub category: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub confidence: f32,
    pub recognizer_id: String,
    pub evidence: String,
    pub sensitivity: Sensitivity,
    /// Index into the request's extracted content fields.
    #[serde(default)]
    pub field_index: usize,
    /// Opaque vault ID for exact-secret findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    /// Classifier/model version that produced the finding.
    #[serde(default)]
    pub classifier_version: String,
    /// True when the finding covers the whole field rather than a localized span.
    #[serde(default)]
    pub whole_field: bool,
}

fn finding_schema_version() -> u32 {
    FINDING_SCHEMA_VERSION
}

impl Finding {
    pub fn new(
        classifier_type: ClassifierType,
        category: impl Into<String>,
        start: usize,
        end: usize,
        confidence: f32,
        recognizer_id: impl Into<String>,
        evidence: impl Into<String>,
        sensitivity: Sensitivity,
    ) -> Self {
        Finding {
            schema_version: FINDING_SCHEMA_VERSION,
            finding_id: uuid::Uuid::new_v4().to_string(),
            classifier_type,
            category: category.into(),
            start_byte: start,
            end_byte: end,
            confidence,
            recognizer_id: recognizer_id.into(),
            evidence: evidence.into(),
            sensitivity,
            field_index: 0,
            secret_id: None,
            entity_id: None,
            classifier_version: String::new(),
            whole_field: false,
        }
    }

    pub fn len(&self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn overlaps(&self, other: &Finding) -> bool {
        self.field_index == other.field_index
            && self.start_byte < other.end_byte
            && other.start_byte < self.end_byte
    }

    /// Validates that offsets are in bounds and on UTF-8 boundaries of `text`.
    pub fn validate_against(&self, text: &str) -> Result<(), String> {
        if self.start_byte > self.end_byte {
            return Err("start after end".into());
        }
        if self.end_byte > text.len() {
            return Err("end out of bounds".into());
        }
        if !text.is_char_boundary(self.start_byte) || !text.is_char_boundary(self.end_byte) {
            return Err("offset not on UTF-8 boundary".into());
        }
        if !(0.0..=1.0).contains(&self.confidence) || self.confidence.is_nan() {
            return Err("confidence out of range".into());
        }
        Ok(())
    }
}

/// Privacy tier of a generation destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DestinationTier {
    /// Runs on this device (loopback only).
    Local,
    /// Remote, with attestation verified for the exact model and service.
    AttestedTee,
    /// Any other remote endpoint, including NEAR models without verified TEE.
    ExternalUnverified,
}

impl DestinationTier {
    pub fn is_remote(self) -> bool {
        !matches!(self, DestinationTier::Local)
    }
}

/// A resolved generation destination used for policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DestinationRef {
    pub id: String,
    pub tier: DestinationTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProviderKind {
    NearAi,
    Ollama,
    OpenaiCompatible,
}

/// Configuration of a generation destination. Credentials are never stored here;
/// `credential_ref` names an entry in the OS credential store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderConfig {
    #[serde(default = "provider_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub kind: ProviderKind,
    pub display_name: String,
    pub base_url: String,
    /// Default model sent upstream when the client's model is not served here.
    pub default_model: String,
    /// Tier the owner declared. The runtime tier may be lower (e.g. a NEAR model
    /// whose attestation is not verified is treated as EXTERNAL_UNVERIFIED).
    pub declared_tier: DestinationTier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether this destination may be used automatically when the preferred one fails.
    #[serde(default)]
    pub allow_failover: bool,
    /// Whether the upstream natively supports `/v1/responses`. If false, Prompt VPN
    /// translates Responses requests into Chat Completions.
    #[serde(default)]
    pub supports_responses: bool,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn provider_schema_version() -> u32 {
    PROVIDER_SCHEMA_VERSION
}
fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    120
}

/// Record of an approved, reversible edit to a third-party client's configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientIntegration {
    #[serde(default = "client_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub client: String,
    pub config_path: String,
    pub backup_path: Option<String>,
    pub applied_at: DateTime<Utc>,
    pub base_url: String,
    pub app_id: String,
    pub rolled_back_at: Option<DateTime<Utc>>,
}

fn client_schema_version() -> u32 {
    CLIENT_INTEGRATION_SCHEMA_VERSION
}

/// Non-secret metadata about one reversible placeholder. The value itself lives
/// only in the encrypted vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransformMapEntry {
    #[serde(default = "transform_schema_version")]
    pub schema_version: u32,
    pub token: String,
    pub category: String,
    pub scope_id: String,
    pub key_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

fn transform_schema_version() -> u32 {
    TRANSFORM_MAP_SCHEMA_VERSION
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PendingStatus {
    Pending,
    Approved,
    Denied,
    Cancelled,
    Expired,
    Consumed,
}

/// A request held for owner review. The body is encrypted in the vault; this
/// record holds only metadata and a redacted preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingApproval {
    #[serde(default = "pending_schema_version")]
    pub schema_version: u32,
    pub approval_id: String,
    pub event_id: String,
    pub app_id: String,
    pub destination_id: String,
    pub endpoint: String,
    pub request_hash: String,
    pub policy_hash: String,
    /// Hash of the exact payload that would be sent if approved.
    pub approved_payload_hash: String,
    pub categories: Vec<String>,
    pub redacted_preview: String,
    pub reason: String,
    pub status: PendingStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
}

fn pending_schema_version() -> u32 {
    PENDING_SCHEMA_VERSION
}

/// Metadata-only audit event. Never contains prompt text or secret values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEvent {
    #[serde(default = "audit_schema_version")]
    pub schema_version: u32,
    pub event_id: String,
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub app_id: String,
    pub agent_id: Option<String>,
    pub user_id: String,
    pub destination_id: Option<String>,
    pub endpoint: Option<String>,
    pub categories: Vec<String>,
    pub action: String,
    pub policy_ids: Vec<String>,
    pub policy_hash: Option<String>,
    pub recognizer_versions: Vec<String>,
    pub latency_ms: u64,
    pub finding_count: u32,
    pub approval_id: Option<String>,
    pub approval_status: Option<String>,
    pub error_code: Option<String>,
    pub severity: Sensitivity,
    pub direction: Direction,
    pub summary: String,
    pub context_ref: Option<String>,
}

fn audit_schema_version() -> u32 {
    AUDIT_SCHEMA_VERSION
}
