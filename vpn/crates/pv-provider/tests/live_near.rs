//! Live NEAR checks. Run with: cargo test -p pv-provider --test live_near -- --ignored
use pv_provider::near::*;

#[tokio::test]
#[ignore]
async fn live_verification_gates() {
    let b = NearSdkBackend::new(DEFAULT_NEAR_BASE, std::env::var("NEARAI_API_KEY").ok()).unwrap();
    let ok = b.verify("deepseek-ai/DeepSeek-V4.1-Flash-1017").await;
    assert!(ok.eligible, "{ok:?}");
    assert!(ok.gpu_verified && ok.e2ee_key && ok.gateway_tls_spki.is_some());
    let alias = b.verify(SPEC_PREFERRED_MODEL).await;
    assert!(!alias.eligible);
    assert!(alias.error.unwrap().contains("alias"));
    let incognito = b.verify("deepseek/deepseek-v4.1-flash").await;
    assert!(!incognito.eligible);
    let third_party = b.verify("deepseek/deepseek-v3.2").await;
    assert!(!third_party.eligible);
}
