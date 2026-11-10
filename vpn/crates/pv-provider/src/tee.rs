//! ZFX VPN's confidentiality decision for a NEAR AI model route.
//!
//! The NEAR AI inference SDK verifies cryptographic evidence (TDX quotes
//! through Intel DCAP, report-data nonce and signer binding, RTMR3 replay,
//! MRCONFIGID/compose binding, NVIDIA GPU evidence through NRAS). This module
//! decides whether that verified evidence is enough to send a request that
//! needs confidential execution. It requires provider-level (model TEE) proof:
//! a verified gateway attestation alone never qualifies, because NEAR documents
//! that "gateway_attestation is evidence for the NEAR AI Cloud gateway. It is
//! not evidence for a model environment."
//!
//! What a pass means, and what it does not: the selected model's serving CVM
//! booted the measured configuration on genuine TDX + GPU hardware, with a
//! fresh nonce, and holds the key requests are end-to-end encrypted to. It
//! does not prove the serving software inside the CVM is free of
//! vulnerabilities (see NEAR's 9 Nov 2026 SGLang incident report), and NEAR
//! publishes no measurement allowlist, so measurements are only compared when
//! the owner pins them.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Normalized result of a verification run, independent of the SDK types so
/// recorded and synthetic evidence go through the same decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeeEvidence {
    pub provider_id: String,
    /// Base URL the evidence was fetched from; verification is bound to it.
    pub base_url: String,
    /// Model the caller asked for.
    pub requested_model: String,
    /// Catalog says the requested id is an alias of another model.
    pub alias_of: Option<String>,
    /// `model_name` values in the verified model reports.
    pub attested_models: Vec<String>,
    /// Gateway quote verified (necessary, never sufficient).
    pub gateway_verified: bool,
    /// Number of model (provider-level) reports that verified. Zero means only
    /// gateway evidence exists.
    pub provider_reports: usize,
    /// Every verified model report carried GPU evidence that NRAS accepted.
    pub gpu_verified: bool,
    /// Every verified model report advertised a public key for E2EE.
    pub e2ee_keys: bool,
    pub tcb_statuses: Vec<String>,
    pub debug_enabled: bool,
    pub compose_hashes: Vec<String>,
    pub os_image_hashes: Vec<String>,
    /// Model signing identities established by the verified reports.
    pub signers: Vec<String>,
    pub nonce: String,
    pub verified_at: DateTime<Utc>,
    /// What actually checked the quotes, e.g. "nearai-inference-sdk 0.3.0
    /// (Intel DCAP via PCCS, NVIDIA NRAS)". Fixture replays say so here.
    pub verifier: String,
}

/// Owner-pinned measurements for a model, with an expiry. NEAR does not
/// publish an allowlist, so pins are captured by the owner (`zfx-vpn near pin`)
/// after reviewing a verified report, and must be renewed when they expire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasurementPin {
    pub model: String,
    #[serde(default)]
    pub compose_hash: Option<String>,
    #[serde(default)]
    pub os_image_hash: Option<String>,
    pub not_after: DateTime<Utc>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeeRequirements {
    pub require_gpu: bool,
    pub require_e2ee: bool,
    /// Accepted Intel TCB statuses. The SDK default also accepts OutOfDate and
    /// TDRelaunchAdvised; ZFX VPN's default for confidential routes is stricter.
    pub accepted_tcb: Vec<String>,
    /// Evidence older than this is never used (freshness comes only from the nonce).
    pub max_age_secs: i64,
    pub pins: Vec<MeasurementPin>,
}

impl Default for TeeRequirements {
    fn default() -> Self {
        TeeRequirements { require_gpu: true, require_e2ee: true, accepted_tcb: vec!["UpToDate".into(), "SWHardeningNeeded".into()], max_age_secs: 600, pins: vec![] }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum TeeRejection {
    #[error("requested model is an alias of {target}; confidential routes need the canonical model id")]
    Alias { target: String },
    #[error("only gateway evidence verified; no provider-level (model TEE) report")]
    GatewayOnly,
    #[error("no verified evidence at all")]
    NoEvidence,
    #[error("verified reports are for {attested:?}, not the requested model {requested}")]
    ModelMismatch { requested: String, attested: Vec<String> },
    #[error("GPU evidence missing or not verified")]
    GpuEvidenceMissing,
    #[error("no attested public key for end-to-end encryption")]
    E2eeUnavailable,
    #[error("TCB status {status} is not accepted for confidential routes")]
    TcbNotAccepted { status: String },
    #[error("TDX debug mode is enabled")]
    DebugEnabled,
    #[error("evidence is older than {max_age_secs} s")]
    Stale { max_age_secs: i64 },
    #[error("pinned measurement for {model} expired at {not_after}; review the current report and renew the pin")]
    MeasurementExpired { model: String, not_after: String },
    #[error("{field} {actual} does not match the pinned value")]
    MeasurementMismatch { field: String, actual: String },
    #[error("evidence was verified for {verified_for}, not {requested}")]
    WrongEndpoint { verified_for: String, requested: String },
    #[error("verification failed: {0}")]
    Verification(String),
}

/// Decide whether `e` qualifies `requested_model` at `base_url` for a
/// confidential (ATTESTED_CONFIDENTIAL) route right now.
pub fn evaluate(e: &TeeEvidence, req: &TeeRequirements, provider_id: &str, base_url: &str, requested_model: &str, now: DateTime<Utc>) -> Result<(), TeeRejection> {
    // Route substitution: evidence for another endpoint or provider is never reused.
    if e.provider_id != provider_id || e.base_url.trim_end_matches('/') != base_url.trim_end_matches('/') {
        return Err(TeeRejection::WrongEndpoint { verified_for: format!("{} at {}", e.provider_id, e.base_url), requested: format!("{provider_id} at {base_url}") });
    }
    if let Some(t) = &e.alias_of {
        return Err(TeeRejection::Alias { target: t.clone() });
    }
    if !e.gateway_verified && e.provider_reports == 0 {
        return Err(TeeRejection::NoEvidence);
    }
    if e.provider_reports == 0 {
        return Err(TeeRejection::GatewayOnly);
    }
    if e.requested_model != requested_model || e.attested_models.is_empty() || e.attested_models.iter().any(|m| m != requested_model) {
        return Err(TeeRejection::ModelMismatch { requested: requested_model.into(), attested: e.attested_models.clone() });
    }
    if now - e.verified_at > Duration::seconds(req.max_age_secs) || e.verified_at > now + Duration::seconds(60) {
        return Err(TeeRejection::Stale { max_age_secs: req.max_age_secs });
    }
    if e.debug_enabled {
        return Err(TeeRejection::DebugEnabled);
    }
    if let Some(s) = e.tcb_statuses.iter().find(|s| !req.accepted_tcb.contains(s)) {
        return Err(TeeRejection::TcbNotAccepted { status: s.clone() });
    }
    if req.require_gpu && !e.gpu_verified {
        return Err(TeeRejection::GpuEvidenceMissing);
    }
    if req.require_e2ee && !e.e2ee_keys {
        return Err(TeeRejection::E2eeUnavailable);
    }
    for pin in req.pins.iter().filter(|p| p.model == requested_model) {
        if pin.not_after <= now {
            return Err(TeeRejection::MeasurementExpired { model: pin.model.clone(), not_after: pin.not_after.to_rfc3339() });
        }
        for (field, want, got) in [("compose_hash", &pin.compose_hash, &e.compose_hashes), ("os_image_hash", &pin.os_image_hash, &e.os_image_hashes)] {
            let Some(want) = want else { continue };
            if got.is_empty() {
                return Err(TeeRejection::MeasurementMismatch { field: field.into(), actual: "(missing)".into() });
            }
            if let Some(bad) = got.iter().find(|h| *h != want) {
                return Err(TeeRejection::MeasurementMismatch { field: field.into(), actual: bad.clone() });
            }
        }
    }
    Ok(())
}

/// A response signature only counts for the route if its signer is one of the
/// model identities established before sending (preflight). A signer outside
/// that set means a different deployment answered: route substitution.
pub fn receipt_signer_matches(e: &TeeEvidence, signer: &str) -> bool {
    e.signers.iter().any(|s| s.eq_ignore_ascii_case(signer))
}

#[cfg(feature = "near-sdk")]
pub mod sdk {
    //! Conversion from the NEAR AI SDK's verified types.
    use super::TeeEvidence;
    use nearai_inference_sdk::{GpuEvidenceStatus, VerifiedGatewayAttestation, VerifiedModelAttestation};

    pub fn tcb(s: nearai_inference_sdk::TcbStatus) -> String {
        serde_json::to_value(s).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| format!("{s:?}"))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn evidence(provider_id: &str, base_url: &str, requested_model: &str, attested_models: Vec<String>, gateway: Option<&VerifiedGatewayAttestation>, models: &[VerifiedModelAttestation], nonce: &str, verifier: &str) -> TeeEvidence {
        let rm = |m: &VerifiedModelAttestation| m.evidence.deployment.runtime_measurements.clone();
        TeeEvidence {
            provider_id: provider_id.into(),
            base_url: base_url.into(),
            requested_model: requested_model.into(),
            alias_of: None,
            attested_models,
            gateway_verified: gateway.is_some(),
            provider_reports: models.len(),
            gpu_verified: !models.is_empty() && models.iter().all(|m| m.gpu_evidence == GpuEvidenceStatus::Verified),
            e2ee_keys: !models.is_empty() && models.iter().all(|m| m.signing_public_key.is_some()),
            tcb_statuses: models.iter().map(|m| tcb(m.evidence.tcb_status)).collect(),
            debug_enabled: false, // the SDK rejects debug quotes before returning
            compose_hashes: models.iter().filter_map(|m| rm(m).compose_hash).collect(),
            os_image_hashes: models.iter().filter_map(|m| rm(m).os_image_hash).collect(),
            signers: models.iter().map(|m| m.evidence.signer.signing_address.clone()).collect(),
            nonce: nonce.into(),
            verified_at: chrono::Utc::now(),
            verifier: verifier.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn good() -> TeeEvidence {
        TeeEvidence {
            provider_id: "near".into(),
            base_url: "https://cloud-api.near.ai/v1".into(),
            requested_model: "m".into(),
            alias_of: None,
            attested_models: vec!["m".into()],
            gateway_verified: true,
            provider_reports: 1,
            gpu_verified: true,
            e2ee_keys: true,
            tcb_statuses: vec!["UpToDate".into()],
            debug_enabled: false,
            compose_hashes: vec!["c1".into()],
            os_image_hashes: vec!["o1".into()],
            signers: vec!["0xabc".into()],
            nonce: "n".into(),
            verified_at: Utc::now(),
            verifier: "synthetic".into(),
        }
    }

    fn ev(e: &TeeEvidence, req: &TeeRequirements) -> Result<(), TeeRejection> {
        evaluate(e, req, "near", "https://cloud-api.near.ai/v1/", "m", Utc::now())
    }

    #[test]
    fn rejections() {
        let r = TeeRequirements::default();
        assert_eq!(ev(&good(), &r), Ok(()));
        let mut e = good();
        e.provider_reports = 0;
        assert_eq!(ev(&e, &r), Err(TeeRejection::GatewayOnly));
        let mut e = good();
        e.attested_models = vec!["other".into()];
        assert!(matches!(ev(&e, &r), Err(TeeRejection::ModelMismatch { .. })));
        let mut e = good();
        e.gpu_verified = false;
        assert_eq!(ev(&e, &r), Err(TeeRejection::GpuEvidenceMissing));
        let mut e = good();
        e.e2ee_keys = false;
        assert_eq!(ev(&e, &r), Err(TeeRejection::E2eeUnavailable));
        let mut e = good();
        e.tcb_statuses = vec!["OutOfDate".into()];
        assert!(matches!(ev(&e, &r), Err(TeeRejection::TcbNotAccepted { .. })));
        let mut e = good();
        e.verified_at = Utc::now() - Duration::minutes(11);
        assert!(matches!(ev(&e, &r), Err(TeeRejection::Stale { .. })));
        let mut e = good();
        e.alias_of = Some("z".into());
        assert!(matches!(ev(&e, &r), Err(TeeRejection::Alias { .. })));
        let e = good();
        assert!(matches!(evaluate(&e, &r, "near", "https://evil.example/v1", "m", Utc::now()), Err(TeeRejection::WrongEndpoint { .. })));
        assert!(matches!(evaluate(&e, &r, "near-2", "https://cloud-api.near.ai/v1", "m", Utc::now()), Err(TeeRejection::WrongEndpoint { .. })));
    }

    #[test]
    fn pins() {
        let mut r = TeeRequirements::default();
        r.pins.push(MeasurementPin { model: "m".into(), compose_hash: Some("c1".into()), os_image_hash: Some("o1".into()), not_after: Utc::now() + Duration::days(1), note: String::new() });
        assert_eq!(ev(&good(), &r), Ok(()));
        r.pins[0].not_after = Utc::now() - Duration::seconds(1);
        assert!(matches!(ev(&good(), &r), Err(TeeRejection::MeasurementExpired { .. })));
        r.pins[0].not_after = Utc::now() + Duration::days(1);
        r.pins[0].compose_hash = Some("c2".into());
        assert!(matches!(ev(&good(), &r), Err(TeeRejection::MeasurementMismatch { .. })));
        let mut e = good();
        e.compose_hashes.clear();
        r.pins[0].compose_hash = Some("c1".into());
        assert!(matches!(ev(&e, &r), Err(TeeRejection::MeasurementMismatch { .. })));
    }

    #[test]
    fn receipt_signer() {
        assert!(receipt_signer_matches(&good(), "0xABC"));
        assert!(!receipt_signer_matches(&good(), "0xdef"));
    }
}
