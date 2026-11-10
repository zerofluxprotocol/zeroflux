//! Owner actions on held requests. Approvals never replay the original HTTP
//! request: the client must retry, or the owner resubmits explicitly.

use crate::engine::{Caller, Engine, ProxyOutput, Source};
use crate::fields;
use chrono::{Duration, Utc};
use pv_core::*;
use pv_detectors::exact::ExactKind;
use pv_detectors::normalize::NormOpts;
use pv_provider::Endpoint;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("approval not found")]
    NotFound,
    #[error("approval is {0:?}, not pending")]
    NotPending(PendingStatus),
    #[error("approval expired; its payload has been destroyed")]
    Expired,
    #[error("{0}")]
    Other(String),
}

impl Engine {
    fn pending_open(&self, id: &str) -> Result<PendingApproval, ApprovalError> {
        let p = self.db.get_pending(id).map_err(|e| ApprovalError::Other(e.to_string()))?.ok_or(ApprovalError::NotFound)?;
        if p.expires_at <= Utc::now() {
            return Err(ApprovalError::Expired);
        }
        if p.status != PendingStatus::Pending {
            return Err(ApprovalError::NotPending(p.status));
        }
        Ok(p)
    }

    fn owner_event(&self, p: &PendingApproval, action: &str, status: &str, summary: &str) {
        let ev = AuditEvent {
            schema_version: AUDIT_SCHEMA_VERSION,
            event_id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            kind: "approval".into(),
            app_id: p.app_id.clone(),
            agent_id: None,
            user_id: self.settings().owner_id,
            destination_id: Some(p.destination_id.clone()),
            endpoint: Some(p.endpoint.clone()),
            categories: p.categories.clone(),
            action: action.into(),
            policy_ids: vec![],
            policy_hash: Some(p.policy_hash.clone()),
            recognizer_versions: vec![],
            latency_ms: 0,
            finding_count: 0,
            approval_id: Some(p.approval_id.clone()),
            approval_status: Some(status.into()),
            error_code: None,
            severity: Sensitivity::Moderate,
            direction: Direction::Inbound,
            summary: summary.into(),
            context_ref: None,
        };
        self.log(&ev);
    }

    /// Approve once. The approval is valid for the configured TTL and binds to
    /// request hash, destination, policy hash and exact transformed payload.
    pub fn approve(&self, id: &str, actor: &str) -> Result<PendingApproval, ApprovalError> {
        let mut p = self.pending_open(id)?;
        if p.approved_payload_hash == "unapprovable" {
            return Err(ApprovalError::Other("this request cannot be approved: a non-overridable rule still blocks it".into()));
        }
        p.status = PendingStatus::Approved;
        p.decided_at = Some(Utc::now());
        p.decided_by = Some(actor.into());
        p.expires_at = Utc::now() + Duration::minutes(self.settings().pending_ttl_minutes);
        self.db.put_pending(&p).map_err(|e| ApprovalError::Other(e.to_string()))?;
        self.owner_event(&p, "APPROVE_ONCE", "APPROVED", "Owner approved a held request once; the client must retry");
        Ok(p)
    }

    pub fn deny(&self, id: &str, actor: &str) -> Result<PendingApproval, ApprovalError> {
        let mut p = self.pending_open(id)?;
        p.status = PendingStatus::Denied;
        p.decided_at = Some(Utc::now());
        p.decided_by = Some(actor.into());
        self.vault.delete_pending_payload(id).map_err(|e| ApprovalError::Other(e.to_string()))?;
        self.db.put_pending(&p).map_err(|e| ApprovalError::Other(e.to_string()))?;
        self.owner_event(&p, "DENY", "DENIED", "Owner denied a held request");
        Ok(p)
    }

    pub fn cancel(&self, id: &str, actor: &str) -> Result<PendingApproval, ApprovalError> {
        let mut p = self.db.get_pending(id).map_err(|e| ApprovalError::Other(e.to_string()))?.ok_or(ApprovalError::NotFound)?;
        if !matches!(p.status, PendingStatus::Pending | PendingStatus::Approved) {
            return Err(ApprovalError::NotPending(p.status));
        }
        p.status = PendingStatus::Cancelled;
        p.decided_at = Some(Utc::now());
        p.decided_by = Some(actor.into());
        self.vault.delete_pending_payload(id).map_err(|e| ApprovalError::Other(e.to_string()))?;
        self.db.put_pending(&p).map_err(|e| ApprovalError::Other(e.to_string()))?;
        self.owner_event(&p, "CANCEL", "CANCELLED", "Held request cancelled");
        Ok(p)
    }

    /// Owner-only reveal of the encrypted original request. Audited.
    pub fn reveal(&self, id: &str, actor: &str) -> Result<Value, ApprovalError> {
        let p = self.db.get_pending(id).map_err(|e| ApprovalError::Other(e.to_string()))?.ok_or(ApprovalError::NotFound)?;
        let body = self.vault.get_pending_payload(id).map_err(|e| ApprovalError::Other(e.to_string()))?.ok_or(ApprovalError::Expired)?;
        let v: Value = serde_json::from_slice(&body).map_err(|e| ApprovalError::Other(e.to_string()))?;
        self.owner_event(&p, "REVEAL", &format!("{:?}", p.status).to_uppercase(), &format!("Device owner ({actor}) revealed the held prompt"));
        Ok(v)
    }

    /// Edit-and-retry: the owner submits an edited body through the dashboard.
    /// The original is cancelled and the edit is fully reclassified.
    pub async fn resubmit_edited(self: &Arc<Self>, id: &str, edited: &Value) -> Result<Result<ProxyOutput, PvError>, ApprovalError> {
        let p = self.db.get_pending(id).map_err(|e| ApprovalError::Other(e.to_string()))?.ok_or(ApprovalError::NotFound)?;
        if !matches!(p.status, PendingStatus::Pending | PendingStatus::Approved) {
            return Err(ApprovalError::NotPending(p.status));
        }
        self.cancel(id, "owner-edit")?;
        let endpoint = if p.endpoint.ends_with("responses") { Endpoint::Responses } else { Endpoint::Chat };
        let caller = Caller { app_id: p.app_id.clone(), agent_id: None, destination_override: Some(p.destination_id.clone()), conversation: None, source: Source::Dashboard };
        let raw = serde_json::to_vec(edited).map_err(|e| ApprovalError::Other(e.to_string()))?;
        Ok(self.handle(caller, endpoint, &raw).await)
    }

    /// Distinct localized sensitive values in a held request (for whitelist/blacklist).
    async fn flagged_values(&self, id: &str) -> Result<(PendingApproval, Vec<(String, String)>), ApprovalError> {
        let p = self.db.get_pending(id).map_err(|e| ApprovalError::Other(e.to_string()))?.ok_or(ApprovalError::NotFound)?;
        let body = self.vault.get_pending_payload(id).map_err(|e| ApprovalError::Other(e.to_string()))?.ok_or(ApprovalError::Expired)?;
        let v: Value = serde_json::from_slice(&body).map_err(|e| ApprovalError::Other(e.to_string()))?;
        let roots = if p.endpoint.ends_with("responses") { fields::RESPONSES_REQUEST_ROOTS } else { fields::CHAT_REQUEST_ROOTS };
        let ex = fields::extract(&v, roots);
        let texts = fields::texts(&ex.fields);
        let rep = self.detector.detect(&texts, true).await;
        let mut out: Vec<(String, String)> = Vec::new();
        for f in rep.findings.iter().filter(|f| !f.whole_field) {
            let val = texts[f.field_index][f.start_byte..f.end_byte].to_string();
            if val.chars().count() >= pv_detectors::exact::MIN_VALUE_LEN && !out.iter().any(|(v, _)| *v == val) {
                out.push((val, f.category.clone()));
            }
        }
        Ok((p, out))
    }

    /// Whitelist: allowlist the flagged noncritical values, then approve once.
    pub async fn whitelist(&self, id: &str, actor: &str) -> Result<(PendingApproval, usize), ApprovalError> {
        let (_, vals) = self.flagged_values(id).await?;
        let mut n = 0;
        for (v, cat) in vals {
            if category::is_critical(&cat) || cat == category::EXACT_SECRET {
                continue; // critical values cannot be allowlisted
            }
            if self.vault.add_secret(ExactKind::Value, &format!("allowlisted {cat} (from review)"), &v, NormOpts::default(), true, "ALLOW").is_ok() {
                n += 1;
            }
        }
        self.reload_exact().map_err(ApprovalError::Other)?;
        let p = self.approve(id, actor)?;
        self.owner_event(&p, "WHITELIST", "APPROVED", &format!("Owner allowlisted {n} value(s) from a held request"));
        Ok((p, n))
    }

    /// Blacklist ("Always block"): register flagged values as blocked exact secrets, then deny.
    pub async fn blacklist(&self, id: &str, actor: &str) -> Result<(PendingApproval, usize), ApprovalError> {
        let (_, vals) = self.flagged_values(id).await?;
        let mut n = 0;
        for (v, cat) in vals {
            if self.vault.add_secret(ExactKind::Value, &format!("always block {cat} (from review)"), &v, NormOpts { casefold: true, unicode: true, ..Default::default() }, false, "BLOCK").is_ok() {
                n += 1;
            }
        }
        self.reload_exact().map_err(ApprovalError::Other)?;
        let p = self.deny(id, actor)?;
        self.owner_event(&p, "BLACKLIST", "DENIED", &format!("Owner registered {n} value(s) from a held request as always-blocked"));
        Ok((p, n))
    }

    /// Rebuild the in-process exact matcher from the vault.
    pub fn reload_exact(&self) -> Result<(), String> {
        let entries = self.vault.load_exact_entries().map_err(|e| e.to_string())?;
        self.detector.set_exact(pv_detectors::exact::ExactMatcher::new(entries));
        Ok(())
    }

    /// Expire held requests and mappings; enforce audit retention.
    pub fn housekeeping(&self) {
        let now = Utc::now();
        if let Ok(ids) = self.db.expire_pending(now) {
            for id in ids {
                let _ = self.vault.delete_pending_payload(&id);
            }
        }
        let _ = self.db.expire_mappings(now);
        let _ = self.db.enforce_retention(now);
    }
}
