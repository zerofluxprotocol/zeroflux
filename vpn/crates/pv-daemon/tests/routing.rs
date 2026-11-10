//! Destination routing: NEAR preferred, verified-TEE private route with consent,
//! no downgrade to unverified endpoints, local Ollama fallback, approved custom
//! failover, credential isolation.

mod common;
use common::*;
use pv_core::*;
use pv_daemon::mock::{self, Recorded};
use pv_provider::near::MockNear;
use serde_json::json;
use std::sync::Arc;

struct Near {
    rec: Arc<Recorded>,
    backend: Arc<MockNear>,
}

async fn add_near(h: &H, model: &str, eligible: bool) -> Near {
    let rec = Arc::new(Recorded::default());
    let (base, _) = mock::serve(0, rec.clone()).await.unwrap();
    let mut cfg = provider("near", "https://cloud-api.near.ai/v1", model, DestinationTier::AttestedTee, true, true);
    cfg.kind = ProviderKind::NearAi;
    pv_daemon::admin::put_provider_inner(&h.st, cfg, None).map_err(|_| ()).unwrap();
    let backend = Arc::new(MockNear::new(&base, eligible));
    h.st.engine.registry.set_near_backend("near", backend.clone());
    Near { rec, backend }
}

fn require_semantic(h: &H) {
    // Required span model missing → CLASSIFIER_FAILURE (an uncertain, noncritical finding).
    h.st.engine.detector.set_settings(pv_detectors::DetectorSettings { span_enabled: true, span_required: true, span_timeout_ms: 300 });
}

fn set(h: &H, f: impl FnOnce(&mut pv_proxy::Settings)) {
    let mut s = h.st.engine.settings();
    f(&mut s);
    pv_daemon::state::save_settings(&h.st, &s, "test").unwrap();
}

#[tokio::test]
async fn near_is_default_destination_when_configured() {
    let h = harness("mock-echo").await;
    let near = add_near(&h, "mock-echo", true).await;
    set(&h, |s| s.default_destination = Some("near".into()));
    let (s, v) = h.chat(user("hello there")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(near.rec.count(), 1);
    assert_eq!(h.rec.count(), 0);
}

#[tokio::test]
async fn critical_pii_never_reaches_tee() {
    let h = harness("mock-echo").await;
    let near = add_near(&h, "mock-echo", true).await;
    set(&h, |s| s.default_destination = Some("near".into()));
    let (s, _) = h.chat(user("ssn 123-45-6789")).await;
    assert_eq!(s, 403);
    assert_eq!(near.rec.count(), 0);
}

#[tokio::test]
async fn uncertain_routes_to_verified_tee_only_with_consent() {
    let h = harness("mock-echo").await;
    let near = add_near(&h, "mock-echo", true).await;
    require_semantic(&h);
    // Without consent: block-and-review, nothing sent anywhere.
    set(&h, |s| {
        s.fallback.mode = FallbackMode::TeePrivateRoute;
        s.fallback.tee_consent = false;
        s.private_destination = Some("near".into());
        s.private_model = Some("mock-echo".into());
    });
    let (s, _) = h.chat(user("quarterly plan draft")).await;
    assert_eq!(s, 428);
    assert_eq!(near.rec.count() + h.rec.count(), 0);
    // With consent: private route to the verified TEE, never the default custom endpoint.
    set(&h, |s| s.fallback.tee_consent = true);
    let (s, v) = h.chat(user("quarterly plan draft")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(near.rec.count(), 1);
    assert_eq!(h.rec.count(), 0, "unverified default destination got nothing");
    let ev = h.events_text().await;
    assert!(ev.contains("PRIVATE_ROUTE"));
}

#[tokio::test]
async fn no_downgrade_when_tee_unverified_or_failing() {
    let h = harness("mock-echo").await;
    let near = add_near(&h, "mock-echo", false).await;
    require_semantic(&h);
    // Custom endpoint allowed for failover still must not receive TEE-required content.
    pv_daemon::admin::put_provider_inner(&h.st, provider("mock", &h.mock_base, "mock-echo", DestinationTier::ExternalUnverified, true, true), None).map_err(|_| ()).unwrap();
    set(&h, |s| {
        s.fallback.mode = FallbackMode::TeePrivateRoute;
        s.fallback.tee_consent = true;
        s.private_destination = Some("near".into());
        s.private_model = Some("mock-echo".into());
        s.failover = vec!["mock".into()];
    });
    let (s, v) = h.chat(user("draft text")).await;
    assert_eq!(s, 403, "{v}");
    assert_eq!(v["error"]["code"], "PV_ROUTE_FORBIDDEN");
    assert_eq!(near.rec.count() + h.rec.count(), 0);
    // Verified but the TEE upstream fails: still no downgrade.
    near.backend.eligible.store(true, std::sync::atomic::Ordering::SeqCst);
    h.st.engine.registry.invalidate("near", "mock-echo");
    let mut cfg = h.st.engine.registry.get("near").unwrap();
    cfg.default_model = "mock-fail-500".into();
    pv_daemon::admin::put_provider_inner(&h.st, cfg, None).map_err(|_| ()).unwrap();
    h.st.engine.registry.set_near_backend("near", near.backend.clone());
    set(&h, |s| s.private_model = Some("mock-fail-500".into()));
    let (s, _) = h.chat(user("draft text")).await;
    assert!(s == 502 || s == 403, "{s}");
    assert_eq!(h.rec.count(), 0, "unverified endpoint is never an automatic replacement for a TEE");
}

#[tokio::test]
async fn local_private_handling_uses_ollama_only_when_enabled() {
    let h = harness("mock-echo").await;
    require_semantic(&h);
    let lrec = Arc::new(Recorded::default());
    let (lbase, _) = mock::serve(0, lrec.clone()).await.unwrap();
    let mut cfg = provider("ollama", &lbase, "mock-echo", DestinationTier::Local, true, false);
    cfg.kind = ProviderKind::Ollama;
    pv_daemon::admin::put_provider_inner(&h.st, cfg, None).map_err(|_| ()).unwrap();
    set(&h, |s| s.fallback.mode = FallbackMode::TeePrivateRoute);
    let (s, _) = h.chat(user("text")).await;
    assert_eq!(s, 428, "no consent, no local-private flag → review");
    set(&h, |s| s.fallback.local_private_handling = true);
    let (s, v) = h.chat(user("text")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(lrec.count(), 1);
    assert_eq!(h.rec.count(), 0);
}

#[tokio::test]
async fn provider_failure_fails_over_to_local_then_approved_custom() {
    let h = harness("mock-fail-503").await;
    let lrec = Arc::new(Recorded::default());
    let (lbase, _) = mock::serve(0, lrec.clone()).await.unwrap();
    let mut cfg = provider("ollama", &lbase, "mock-echo", DestinationTier::Local, true, false);
    cfg.kind = ProviderKind::Ollama;
    pv_daemon::admin::put_provider_inner(&h.st, cfg, None).map_err(|_| ()).unwrap();
    set(&h, |s| s.failover = vec!["ollama".into()]);
    let (s, v) = h.chat(user("hello from a@example.com")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(lrec.count(), 1);
    // Local destination: email policy targets external tiers only, so raw text is allowed locally.
    assert!(lrec.all_text().contains("a@example.com"));
    let ev = h.events_text().await;
    assert!(ev.contains("failover from mock"));
}

#[tokio::test]
async fn custom_failover_requires_owner_authorization() {
    let h = harness("mock-fail-500").await;
    let crec = Arc::new(Recorded::default());
    let (cbase, _) = mock::serve(0, crec.clone()).await.unwrap();
    // Not authorized for failover.
    pv_daemon::admin::put_provider_inner(&h.st, provider("custom", &cbase, "mock-echo", DestinationTier::ExternalUnverified, false, true), None).map_err(|_| ()).unwrap();
    set(&h, |s| s.failover = vec!["custom".into()]);
    let (s, v) = h.chat(user("hello")).await;
    assert_eq!(s, 502, "{v}");
    assert_eq!(crec.count(), 0);
    // Authorized: used, with policy re-evaluated (email tokenized for external).
    pv_daemon::admin::put_provider_inner(&h.st, provider("custom", &cbase, "mock-echo", DestinationTier::ExternalUnverified, true, true), None).map_err(|_| ()).unwrap();
    let (s, _) = h.chat(user("hello x@example.com")).await;
    assert_eq!(s, 200);
    assert_eq!(crec.count(), 1);
    assert!(!crec.all_text().contains("x@example.com"));
}

#[tokio::test]
async fn credentials_never_cross_providers_or_leak_client_key() {
    let h = harness("mock-echo").await;
    pv_daemon::admin::put_provider_inner(&h.st, provider("mock", &h.mock_base, "mock-echo", DestinationTier::ExternalUnverified, false, true), Some("sk-upstream-ONE".into())).map_err(|_| ()).unwrap();
    let crec = Arc::new(Recorded::default());
    let (cbase, _) = mock::serve(0, crec.clone()).await.unwrap();
    pv_daemon::admin::put_provider_inner(&h.st, provider("custom", &cbase, "mock-echo", DestinationTier::ExternalUnverified, true, true), None).map_err(|_| ()).unwrap();
    let (s, _) = h.chat(user("hi")).await;
    assert_eq!(s, 200);
    assert_eq!(h.rec.auth.lock().unwrap()[0].as_deref(), Some("Bearer sk-upstream-ONE"));
    set(&h, |s| s.default_destination = Some("custom".into()));
    let (s, _) = h.chat(user("hi")).await;
    assert_eq!(s, 200);
    assert_eq!(crec.auth.lock().unwrap()[0], None, "no credential leaks to another provider");
    let all = format!("{:?}{:?}", h.rec.auth.lock().unwrap(), crec.auth.lock().unwrap());
    assert!(!all.contains("pvk_"), "local caller key never forwarded");
    assert!(!h.disk_text().contains("sk-upstream-ONE"), "provider token not stored in plaintext");
}

#[tokio::test]
async fn per_app_destination_override() {
    let h = harness("mock-echo").await;
    let crec = Arc::new(Recorded::default());
    let (cbase, _) = mock::serve(0, crec.clone()).await.unwrap();
    pv_daemon::admin::put_provider_inner(&h.st, provider("custom", &cbase, "mock-echo", DestinationTier::ExternalUnverified, false, true), None).map_err(|_| ()).unwrap();
    let (_, key) = pv_daemon::state::create_app(&h.st, "agent2", Some("codex".into()), Some("custom".into())).unwrap();
    let (s, _) = h.chat_key(&key, user("hi")).await;
    assert_eq!(s, 200);
    assert_eq!(crec.count(), 1);
    assert_eq!(h.rec.count(), 0);
}

#[tokio::test]
async fn retries_after_stream_failure_do_not_leak() {
    let h = harness("mock-badsse").await;
    let mut b = user("hello b@example.com");
    b["stream"] = json!(true);
    let (_, sse) = h.stream("/v1/chat/completions", b).await;
    assert!(sse.contains("PV_OUTPUT_BLOCKED") || sse.contains("PV_PROVIDER_ERROR"), "{}", &sse[..sse.len().min(300)]);
    assert!(!h.upstream_text().contains("b@example.com"));
}
