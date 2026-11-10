//! Live NEAR checks against the public attestation endpoint (no API key needed).
//! Run with: cargo test -p pv-provider --test live_near -- --ignored
//! This is the only test that verifies attestation live (Intel DCAP via PCCS,
//! NVIDIA NRAS). It needs network access, so it is ignored by default.
use pv_core::*;
use pv_provider::near::*;
use pv_provider::Registry;
use std::sync::Arc;

fn registry() -> Registry {
    let r = Registry::new(Arc::new(|_: &str| None));
    r.set_providers(vec![ProviderConfig {
        schema_version: 1,
        id: "near".into(),
        kind: ProviderKind::NearAi,
        display_name: "NEAR AI".into(),
        base_url: DEFAULT_NEAR_BASE.into(),
        default_model: DEFAULT_NEAR_MODEL.into(),
        declared_tier: DestinationTier::AttestedTee,
        credential_ref: None,
        enabled: true,
        allow_failover: false,
        supports_responses: true,
        timeout_secs: 60,
        zfx_hosted_transport: false,
    }]);
    r.set_near_backend("near", Arc::new(NearSdkBackend::new(DEFAULT_NEAR_BASE, std::env::var("NEARAI_API_KEY").ok()).unwrap()));
    r
}

#[tokio::test]
#[ignore]
async fn live_verification_gates() {
    let r = registry();
    let ok = r.verify_near("near", DEFAULT_NEAR_MODEL, true).await;
    assert!(ok.eligible, "{ok:?}");
    assert!(ok.gpu_verified && ok.e2ee_key && ok.gateway_tls_spki.is_some());
    let ev = ok.evidence.as_ref().unwrap();
    assert_eq!(ev.provider_reports, 1);
    assert!(!ev.compose_hashes.is_empty());
    eprintln!("live: {} tcb {:?} compose {:?} os {:?} signers {:?}", DEFAULT_NEAR_MODEL, ev.tcb_statuses, ev.compose_hashes, ev.os_image_hashes, ev.signers);
    let alias = r.verify_near("near", SPEC_PREFERRED_MODEL, true).await;
    assert!(!alias.eligible);
    assert!(alias.error.unwrap().contains("alias"));
    let incognito = r.verify_near("near", "deepseek/deepseek-v4.1-flash", true).await;
    assert!(!incognito.eligible);
    let third_party = r.verify_near("near", "deepseek/deepseek-v3.2", true).await;
    assert!(!third_party.eligible);
}
