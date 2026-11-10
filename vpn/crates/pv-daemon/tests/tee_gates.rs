//! Gate 4 (spec 3.7): a route that requires a TEE rejects an unverified NEAR
//! proxy (gateway-only evidence), an expired measurement, unavailable E2EE,
//! route substitution and credential crossover, and never downgrades to a
//! public model when that happens.
//!
//! These run against `MockNear`, whose evidence is synthetic and labelled as
//! such; the decision code under test (`pv_provider::tee::evaluate`, the
//! registry and the engine) is the same code that judges real SDK evidence.
//! Replays of recorded live NEAR reports are in pv-provider/tests/near_replay.rs.

mod common;
use common::*;
use pv_core::*;
use pv_daemon::mock::{self, Recorded};
use pv_provider::near::MockNear;
use pv_provider::tee::{MeasurementPin, TeeEvidence};
use serde_json::{json, Value};
use std::sync::Arc;

const MODEL: &str = "deepseek-ai/DeepSeek-V4.1-Flash-1017";

struct Near {
    rec: Arc<Recorded>,
    backend: Arc<MockNear>,
}

async fn setup(h: &H) -> Near {
    let rec = Arc::new(Recorded::default());
    let (base, _) = mock::serve(0, rec.clone()).await.unwrap();
    let mut cfg = provider("near", pv_provider::near::DEFAULT_NEAR_BASE, MODEL, DestinationTier::AttestedTee, false, true);
    cfg.kind = ProviderKind::NearAi;
    cfg.credential_ref = Some("provider:near".into());
    pv_daemon::admin::put_provider_inner(&h.st, cfg, Some("near-api-key-synthetic-0001".into())).map_err(|_| ()).unwrap();
    let backend = Arc::new(MockNear::new(&base, true));
    h.st.engine.registry.set_near_backend("near", backend.clone());
    let mut s = h.st.engine.settings();
    s.fallback.tee_consent = true;
    s.fallback.mode = FallbackMode::TeePrivateRoute;
    s.private_destination = Some("near".into());
    s.private_model = Some(MODEL.into());
    pv_daemon::state::save_settings(&h.st, &s, "test").unwrap();
    Near { rec, backend }
}

fn mutate(h: &H, n: &Near, f: impl FnOnce(&mut TeeEvidence)) {
    let mut e = MockNear::synthetic_evidence(MODEL);
    f(&mut e);
    *n.backend.evidence.lock().unwrap() = Some(e);
    h.st.engine.registry.invalidate("near", MODEL);
}

fn confidential(text: &str) -> Value {
    json!({"model": "gpt-test", "messages": [{"role": "user", "content": text}], "zfx_privacy": {"mode": "confidential", "on_unavailable": "block"}})
}

async fn send(h: &H, body: &Value) -> (u16, Value, reqwest::header::HeaderMap) {
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).json(body).send().await.unwrap();
    let s = r.status().as_u16();
    let hd = r.headers().clone();
    (s, r.json().await.unwrap_or(Value::Null), hd)
}

async fn assert_rejected(h: &H, n: &Near, why: &str) {
    let (s, v, _) = send(h, &confidential("Patient Jane Roe, MRN 4471, needs a summary")).await;
    assert_eq!(s, 403, "{why}: {v}");
    assert_eq!(v["error"]["code"], "ZFX_TEE_UNVERIFIED", "{why}: {v}");
    assert_eq!(n.rec.count(), 0, "{why}: nothing reached the TEE upstream");
    assert_eq!(h.rec.count(), 0, "{why}: never downgraded to the public model");
}

#[tokio::test]
async fn verified_tee_route_is_used_and_labelled() {
    let h = harness("mock-echo").await;
    let n = setup(&h).await;
    let (s, v, hd) = send(&h, &confidential("summarize: internal memo about Project Cedar")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(n.rec.count(), 1);
    assert_eq!(h.rec.count(), 0);
    let route = hd.get("x-zfx-privacy-route").unwrap().to_str().unwrap().to_string();
    assert!(route.contains("tier=ATTESTED_CONFIDENTIAL") && route.contains("verification=PENDING_VERIFICATION"), "{route}");
    assert!(!route.contains("Cedar"));
    // The zfx_privacy extension is stripped before forwarding.
    assert!(!n.rec.all_text().contains("zfx_privacy"));
    // Receipt verification completes in the background.
    let id = hd.get("x-zfx-request-id").unwrap().to_str().unwrap().to_string();
    let mut status = Value::Null;
    for _ in 0..50 {
        let r: Value = h.http.get(format!("{}/v1/zfx/requests/{id}/privacy", h.base)).bearer_auth(&h.key).send().await.unwrap().json().await.unwrap();
        status = r["verification"].clone();
        if status == "RECEIPT_VERIFIED" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(status, "RECEIPT_VERIFIED");
}

#[tokio::test]
async fn rejects_unverified_proxy_gateway_only_evidence() {
    let h = harness("mock-echo").await;
    let n = setup(&h).await;
    mutate(&h, &n, |e| e.provider_reports = 0);
    assert_rejected(&h, &n, "gateway-only").await;
    // Not even a gateway: the backend reports no evidence at all.
    n.backend.eligible.store(false, std::sync::atomic::Ordering::SeqCst);
    h.st.engine.registry.invalidate("near", MODEL);
    assert_rejected(&h, &n, "no evidence").await;
}

#[tokio::test]
async fn rejects_expired_or_mismatched_measurement() {
    let h = harness("mock-echo").await;
    let n = setup(&h).await;
    h.st.engine.registry.tee_requirements.write().unwrap().pins = vec![MeasurementPin { model: MODEL.into(), compose_hash: Some("synthetic-compose".into()), os_image_hash: None, not_after: chrono::Utc::now() - chrono::Duration::hours(1), note: "expired".into() }];
    h.st.engine.registry.invalidate("near", MODEL);
    assert_rejected(&h, &n, "expired pin").await;
    h.st.engine.registry.tee_requirements.write().unwrap().pins[0].not_after = chrono::Utc::now() + chrono::Duration::days(7);
    mutate(&h, &n, |e| e.compose_hashes = vec!["a-different-deployment".into()]);
    assert_rejected(&h, &n, "measurement mismatch").await;
    mutate(&h, &n, |e| e.tcb_statuses = vec!["OutOfDate".into()]);
    assert_rejected(&h, &n, "out-of-date TCB").await;
    mutate(&h, &n, |e| e.verified_at = chrono::Utc::now() - chrono::Duration::hours(2));
    assert_rejected(&h, &n, "stale evidence").await;
    // Back to good evidence with a valid pin: accepted.
    mutate(&h, &n, |_| {});
    let (s, v, _) = send(&h, &confidential("ok now")).await;
    assert_eq!(s, 200, "{v}");
}

#[tokio::test]
async fn rejects_unavailable_e2ee() {
    let h = harness("mock-echo").await;
    let n = setup(&h).await;
    mutate(&h, &n, |e| e.e2ee_keys = false);
    assert_rejected(&h, &n, "no attested E2EE key").await;
    // Key advertised but the encrypted send fails: no plaintext retry.
    mutate(&h, &n, |_| {});
    n.backend.e2ee.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_rejected(&h, &n, "E2EE send failure").await;
}

#[tokio::test]
async fn rejects_route_substitution() {
    let h = harness("mock-echo").await;
    let n = setup(&h).await;
    // Evidence for a different model (e.g. the gateway resolved an alias).
    mutate(&h, &n, |e| e.attested_models = vec!["z-ai/glm-5.3-flash".into()]);
    assert_rejected(&h, &n, "other model attested").await;
    mutate(&h, &n, |e| e.alias_of = Some("z-ai/glm-5.3-flash".into()));
    assert_rejected(&h, &n, "alias").await;
    // Evidence fetched from another endpoint.
    mutate(&h, &n, |e| e.base_url = "https://proxy.example/v1".into());
    assert_rejected(&h, &n, "evidence from another endpoint").await;
    // The answer is signed by a key that was not verified before sending:
    // with require_receipt nothing is released.
    mutate(&h, &n, |_| {});
    *n.backend.receipt_signer.lock().unwrap() = "0xsomeone-else".into();
    let mut b = confidential("hello");
    b["zfx_privacy"]["require_receipt"] = json!(true);
    let (s, v, _) = send(&h, &b).await;
    assert_eq!(s, 403, "{v}");
    assert_eq!(v["error"]["code"], "ZFX_TEE_UNVERIFIED");
    assert!(v["error"]["message"].as_str().unwrap().contains("withheld"), "{v}");
    assert!(!v.to_string().contains("You said"), "no model output released: {v}");
    assert_eq!(h.rec.count(), 0);
    // Without require_receipt the reply is released but the route is marked FAILED.
    let (s, _, hd) = send(&h, &confidential("hello again")).await;
    assert_eq!(s, 200);
    let id = hd.get("x-zfx-request-id").unwrap().to_str().unwrap().to_string();
    let mut status = Value::Null;
    for _ in 0..50 {
        let r: Value = h.http.get(format!("{}/v1/zfx/requests/{id}/privacy", h.base)).bearer_auth(&h.key).send().await.unwrap().json().await.unwrap();
        status = r["verification"].clone();
        if status != "PENDING_VERIFICATION" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(status, "FAILED");
}

#[tokio::test]
async fn gateway_signature_is_not_provider_proof() {
    let h = harness("mock-echo").await;
    let n = setup(&h).await;
    *n.backend.receipt_kind.lock().unwrap() = pv_provider::GATEWAY_SIGNATURE.into();
    let mut b = confidential("hello");
    b["zfx_privacy"]["require_receipt"] = json!(true);
    let (s, v, _) = send(&h, &b).await;
    assert_eq!(s, 403, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("gateway signature"), "{v}");
    // Streaming with require_receipt is buffered and also withheld.
    b["stream"] = json!(true);
    let r = h.http.post(format!("{}/v1/chat/completions", h.base)).bearer_auth(&h.key).json(&b).send().await.unwrap();
    assert_eq!(r.headers().get("x-zfx-notice").unwrap(), "ZFX_STREAM_SAFETY_BUFFERED");
    let sse = r.text().await.unwrap();
    assert!(sse.contains("ZFX_TEE_UNVERIFIED") && !sse.contains("You said"), "{sse}");
    // Unavailable signature (e.g. stream disconnect upstream): also withheld.
    *n.backend.receipt_kind.lock().unwrap() = "none".into();
    b["stream"] = json!(false);
    let (s, _, _) = send(&h, &b).await;
    assert_eq!(s, 403);
    // A caller that cannot accept buffering gets an error before anything is sent.
    let before = n.rec.count();
    b["stream"] = json!(true);
    b["zfx_privacy"]["stream_buffering"] = json!("reject");
    let (s, v, _) = send(&h, &b).await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["error"]["code"], "ZFX_STREAM_SAFETY_BUFFERED");
    assert_eq!(n.rec.count(), before);
}

#[tokio::test]
async fn credentials_and_verification_never_cross_destinations() {
    let h = harness("mock-echo").await;
    let n = setup(&h).await;
    // A second NEAR-kind destination at another URL, made the private
    // destination. The mock backend's evidence is for cloud-api.near.ai, so the
    // verification must not transfer to it.
    let rec2 = Arc::new(Recorded::default());
    let (base2, _) = mock::serve(0, rec2.clone()).await.unwrap();
    let mut cfg = provider("near-proxy", "https://near-proxy.example/v1", MODEL, DestinationTier::AttestedTee, false, true);
    cfg.kind = ProviderKind::NearAi;
    pv_daemon::admin::put_provider_inner(&h.st, cfg, None).map_err(|_| ()).unwrap();
    h.st.engine.registry.set_near_backend("near-proxy", Arc::new(MockNear::new(&base2, true)));
    let mut s = h.st.engine.settings();
    s.private_destination = Some("near-proxy".into());
    pv_daemon::state::save_settings(&h.st, &s, "test").unwrap();
    let (st, v, _) = send(&h, &confidential("hello")).await;
    assert_eq!(st, 403, "{v}");
    assert_eq!(v["error"]["code"], "ZFX_TEE_UNVERIFIED");
    assert_eq!(rec2.count() + n.rec.count() + h.rec.count(), 0);
    // Back on the real destination, an ordinary (public) request carries the
    // public provider's own credential only; the NEAR key never leaves for it,
    // and the caller's local key is never forwarded.
    let mut s = h.st.engine.settings();
    s.private_destination = Some("near".into());
    pv_daemon::state::save_settings(&h.st, &s, "test").unwrap();
    let (st, _, _) = send(&h, &user("plain public request")).await;
    assert_eq!(st, 200);
    let all = h.rec.all_raw();
    assert!(!all.contains("near-api-key-synthetic-0001"), "NEAR credential leaked to the public upstream");
    assert!(!all.contains(&h.key), "client key forwarded");
}

#[tokio::test]
async fn confidential_mode_without_consent_or_destination_fails_closed() {
    let h = harness("mock-echo").await;
    let n = setup(&h).await;
    let mut s = h.st.engine.settings();
    s.fallback.tee_consent = false;
    pv_daemon::state::save_settings(&h.st, &s, "test").unwrap();
    let (st, v, _) = send(&h, &confidential("hello")).await;
    assert_eq!(st, 403, "{v}");
    assert_eq!(v["error"]["code"], "ZFX_FALLBACK_UNAVAILABLE");
    assert_eq!(v["error"]["legacy_code"], "PV_ROUTE_FORBIDDEN");
    assert_eq!(n.rec.count() + h.rec.count(), 0);
    // A request may not loosen anything: fallback_model naming an unverified
    // model with consent restored still has to verify.
    let mut s = h.st.engine.settings();
    s.fallback.tee_consent = true;
    pv_daemon::state::save_settings(&h.st, &s, "test").unwrap();
    n.backend.eligible.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut b = confidential("hello");
    b["zfx_privacy"]["fallback_model"] = json!("some/other-model");
    let (st, v, _) = send(&h, &b).await;
    assert_eq!(st, 403, "{v}");
    assert_eq!(n.rec.count() + h.rec.count(), 0);
}
