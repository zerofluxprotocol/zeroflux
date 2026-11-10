//! Offline replay of a REAL NEAR AI attestation report, recorded on 2026-11-10
//! from the public endpoint by `examples/near_record.rs`, through the official
//! NEAR AI SDK's `verify_model_attestation` and ZFX VPN's `tee::evaluate`.
//!
//! What is verified for real here: Intel DCAP quote signatures and TCB status
//! (dcap-qvl with Intel PCS collateral recorded at capture time, evaluated at
//! the capture timestamp), report-data nonce and signer binding, MRCONFIGID to
//! app_compose binding, RTMR3 event-log replay, compose-manager attestation.
//! What is NOT verified here: the NVIDIA GPU verdict (NRAS was not contacted
//! when recording; the replay only checks the GPU payload's nonce binding).
//! The live test (tests/live_near.rs, `--ignored`) runs NRAS and DCAP live.

use async_trait::async_trait;
use nearai_inference_sdk::*;
use pv_provider::tee::{self, TeeRejection, TeeRequirements};
use serde_json::Value;
use std::collections::HashMap;

const FIXTURE: &str = include_str!("fixtures/near-deepseek-v4.1-flash-1017.json");
const MODEL: &str = "deepseek-ai/DeepSeek-V4.1-Flash-1017";

struct Recorded {
    doc: Value,
    collateral: HashMap<String, dcap_qvl::QuoteCollateralV3>,
    captured_at: u64,
}

fn recorded() -> Recorded {
    let doc: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut collateral = HashMap::new();
    for (k, v) in doc["dcap_collateral_by_quote_prefix"].as_object().unwrap() {
        collateral.insert(k.clone(), serde_json::from_value(v["collateral"].clone()).unwrap());
    }
    let captured_at = doc["captured_at_unix"].as_u64().unwrap();
    Recorded { doc, collateral, captured_at }
}

/// Full Intel DCAP verification with recorded collateral, at capture time.
struct RecordedDcap<'a>(&'a Recorded);

#[async_trait]
impl TdxQuoteVerifier for RecordedDcap<'_> {
    async fn verify(&self, intel_quote: &str) -> Result<TdxQuoteVerificationResult, VerificationError> {
        let bytes = hex::decode(intel_quote).map_err(|_| VerificationError::QuoteVerificationFailed { reason: "invalid_encoding" })?;
        let c = self.0.collateral.get(&intel_quote[..64]).ok_or(VerificationError::QuoteCollateralUnavailable)?;
        let v = dcap_qvl::verify::verify(&bytes, c, self.0.captured_at).map_err(|_| VerificationError::QuoteVerificationFailed { reason: "verifier_error" })?;
        let td = v.report.as_td10().ok_or(VerificationError::QuoteUnsupportedReportType)?;
        let tcb: TcbStatus = serde_json::from_value(Value::String(v.status.clone())).map_err(|_| VerificationError::QuoteInvalidResult { reason: "unsupported_tcb_status".into() })?;
        Ok(TdxQuoteVerificationResult { tcb_status: tcb, advisory_ids: v.advisory_ids, debug_enabled: (td.td_attributes[0] & 1) != 0, report_data: td.report_data.to_vec(), mr_config_id: td.mr_config_id.to_vec(), rt_mr3: td.rt_mr3.to_vec() })
    }
}

/// GPU evidence: nonce binding only (NRAS not contacted in the recording).
struct NonceOnlyGpu(String);

#[async_trait]
impl GpuEvidenceVerifier for NonceOnlyGpu {
    async fn verify(&self, payload: &str) -> Result<(), VerificationError> {
        let v: Value = serde_json::from_str(payload).map_err(|_| VerificationError::QuoteVerificationFailed { reason: "invalid_encoding" })?;
        if v["nonce"].as_str() == Some(self.0.as_str()) && v["evidence_list"].as_array().is_some_and(|a| !a.is_empty()) {
            Ok(())
        } else {
            Err(VerificationError::QuoteVerificationFailed { reason: "gpu_nonce_mismatch" })
        }
    }
}

fn model_attestation(m: &Value) -> ModelAttestation {
    ModelAttestation {
        signing_public_key: m["signing_public_key"].as_str().map(String::from),
        compose_manager_attestation: serde_json::from_value(m["compose_manager_attestation"].clone()).ok(),
        evidence: AttestationEvidence {
            nonce: m["request_nonce"].as_str().unwrap().into(),
            signer: SigningIdentity { signing_algo: serde_json::from_value(m["signing_algo"].clone()).unwrap(), signing_address: m["signing_address"].as_str().unwrap().into() },
            intel_quote: m["intel_quote"].as_str().unwrap().into(),
            event_log: serde_json::from_value(m["event_log"].clone()).unwrap(),
            app_compose: m["info"]["tcb_info"]["app_compose"].as_str().unwrap().into(),
        },
        reported_quote_data: None,
        nvidia_payload: m["nvidia_payload"].as_str().map(String::from),
    }
}

async fn verify(r: &Recorded, att: &ModelAttestation, nonce: &str) -> Result<VerifiedModelAttestation, VerificationError> {
    let dcap = RecordedDcap(r);
    let gpu = NonceOnlyGpu(r.doc["nonce"].as_str().unwrap().into());
    let policy = ModelAttestationPolicy { accepted_tcb_statuses: None, gpu_evidence: GpuEvidenceRequirement::Required };
    verify_model_attestation(att, &ModelClientBinding { nonce: nonce.into() }, Some(&policy), ModelAttestationVerifiers { tdx_quote: Some(&dcap), deployment: None, gpu_evidence: Some(&gpu) }).await
}

fn first_model(r: &Recorded) -> Value {
    r.doc["report"]["model_attestations"][0].clone()
}

#[tokio::test]
async fn recorded_report_verifies_and_qualifies() {
    let r = recorded();
    let m = first_model(&r);
    assert_eq!(m["model_name"], MODEL);
    let att = model_attestation(&m);
    let nonce = r.doc["nonce"].as_str().unwrap();
    let v = verify(&r, &att, nonce).await.expect("recorded NEAR report verifies");
    assert_eq!(v.gpu_evidence, GpuEvidenceStatus::Verified);
    assert!(v.signing_public_key.is_some());
    assert!(v.evidence.deployment.compose_manager.is_some(), "compose-manager attestation verified");
    let mut ev = tee::sdk::evidence("near", pv_provider::near::DEFAULT_NEAR_BASE, MODEL, vec![m["model_name"].as_str().unwrap().into()], None, std::slice::from_ref(&v), nonce, "fixture replay: DCAP with recorded collateral; NRAS not contacted");
    ev.gateway_verified = true;
    assert_eq!(tee::evaluate(&ev, &TeeRequirements::default(), "near", pv_provider::near::DEFAULT_NEAR_BASE, MODEL, chrono::Utc::now()), Ok(()));
    eprintln!("replay: tcb {:?} compose {:?} os {:?}", ev.tcb_statuses, ev.compose_hashes, ev.os_image_hashes);
    // The same evidence does not qualify a different model (route substitution).
    assert!(matches!(tee::evaluate(&ev, &TeeRequirements::default(), "near", pv_provider::near::DEFAULT_NEAR_BASE, "z-ai/glm-5.3-flash", chrono::Utc::now()), Err(TeeRejection::ModelMismatch { .. })));
    // Pinned to these measurements: accepted; expired pin: rejected.
    let mut req = TeeRequirements::default();
    req.pins.push(tee::MeasurementPin { model: MODEL.into(), compose_hash: ev.compose_hashes.first().cloned(), os_image_hash: ev.os_image_hashes.first().cloned(), not_after: chrono::Utc::now() + chrono::Duration::days(30), note: "replay".into() });
    assert_eq!(tee::evaluate(&ev, &req, "near", pv_provider::near::DEFAULT_NEAR_BASE, MODEL, chrono::Utc::now()), Ok(()));
    req.pins[0].not_after = chrono::Utc::now() - chrono::Duration::seconds(1);
    assert!(matches!(tee::evaluate(&ev, &req, "near", pv_provider::near::DEFAULT_NEAR_BASE, MODEL, chrono::Utc::now()), Err(TeeRejection::MeasurementExpired { .. })));
}

#[tokio::test]
async fn tampered_recordings_are_rejected() {
    let r = recorded();
    let nonce = r.doc["nonce"].as_str().unwrap().to_string();
    let base = first_model(&r);

    // Replay against a different client nonce (freshness).
    let att = model_attestation(&base);
    assert!(verify(&r, &att, &"00".repeat(32)).await.is_err(), "wrong nonce accepted");

    // app_compose changed by one byte: MRCONFIGID binding breaks.
    let mut m = base.clone();
    let compose = m["info"]["tcb_info"]["app_compose"].as_str().unwrap().to_string() + " ";
    m["info"]["tcb_info"]["app_compose"] = Value::String(compose);
    assert!(verify(&r, &model_attestation(&m), &nonce).await.is_err(), "tampered compose accepted");

    // An RTMR3 event removed from the event log: replay breaks.
    let mut m = base.clone();
    let log = m["event_log"].as_array_mut().unwrap();
    let i = log.iter().rposition(|e| e["imr"] == 3).unwrap();
    log.remove(i);
    assert!(verify(&r, &model_attestation(&m), &nonce).await.is_err(), "tampered event log accepted");

    // Signer swapped for another identity: report-data binding breaks.
    let mut m = base.clone();
    let addr = m["signing_address"].as_str().unwrap().to_string();
    let swapped: String = addr.chars().rev().collect();
    m["signing_address"] = Value::String(swapped);
    assert!(verify(&r, &model_attestation(&m), &nonce).await.is_err(), "swapped signer accepted");

    // GPU evidence removed while required.
    let mut m = base.clone();
    m.as_object_mut().unwrap().remove("nvidia_payload");
    assert!(verify(&r, &model_attestation(&m), &nonce).await.is_err(), "missing GPU evidence accepted");

    // One byte of MRTD (TD report body, offset 48 + 136) flipped. Nothing but
    // the DCAP signature over the report covers MRTD, so only a real signature
    // check catches this.
    let mut m = base.clone();
    let mut q = hex::decode(m["intel_quote"].as_str().unwrap()).unwrap();
    q[48 + 140] ^= 0x01;
    let qh = hex::encode(&q);
    // Keep the collateral lookup key (first 32 bytes unchanged).
    m["intel_quote"] = Value::String(qh);
    assert!(verify(&r, &model_attestation(&m), &nonce).await.is_err(), "corrupted quote accepted");
}
