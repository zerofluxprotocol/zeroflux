//! Approval queue: immediate PV_APPROVAL_REQUIRED, explicit retry, single use,
//! binding to request/app/policy, deny, reveal, edit-and-retry, expiry.

mod common;
use common::*;
use serde_json::json;

async fn review_phone(h: &H) {
    let rule = json!({"id": "review-phone", "version": 1, "scope": {"kind": "user", "id": "local-owner"},
        "selector": {"categories": ["PHONE"], "destinations": ["external"]}, "action": "REQUIRE_APPROVAL", "severity": "moderate", "override": "APPROVAL_ONCE"});
    let (s, v) = h.admin_post("/policies", rule).await;
    assert_eq!(s, 200, "{v}");
}

#[tokio::test]
async fn approval_required_then_explicit_retry_single_use() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let body = user("call me at (555) 010-1234 tomorrow");
    let (s, v) = h.chat(body.clone()).await;
    assert_eq!(s, 428, "{v}");
    assert_eq!(v["error"]["code"], "PV_APPROVAL_REQUIRED");
    assert_eq!(v["error"]["retryable"], true);
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    assert_eq!(h.rec.count(), 0, "held request is not forwarded");

    // Preview is redacted; payload exists encrypted.
    let (_, a) = h.admin_get(&format!("/approvals/{id}")).await;
    let preview = a["approval"]["redacted_preview"].as_str().unwrap();
    assert!(preview.contains("[REDACTED_PHONE]") && !preview.contains("010-1234"), "{preview}");
    assert_eq!(a["payload_available"], true);
    assert!(!h.disk_text().contains("010-1234"), "pending payload is encrypted at rest");

    // Approving does not replay: upstream still untouched until the client retries.
    let (s, _) = h.admin_post(&format!("/approvals/{id}/approve"), json!({})).await;
    assert_eq!(s, 200);
    assert_eq!(h.rec.count(), 0);

    let (s, v) = h.chat(body.clone()).await;
    assert_eq!(s, 200, "{v}");
    assert!(h.upstream_text().contains("(555) 010-1234"), "approved payload sent as-is");

    // Single use: the same request needs a new approval.
    let (s, v) = h.chat(body.clone()).await;
    assert_eq!(s, 428, "{v}");
    assert_ne!(v["error"]["approval_id"].as_str().unwrap(), id);
}

#[tokio::test]
async fn approval_bound_to_exact_request_and_app() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let body = user("phone (555) 010-1234");
    let (_, v) = h.chat(body.clone()).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    h.admin_post(&format!("/approvals/{id}/approve"), json!({})).await;
    // Edited request is a different request.
    let (s, _) = h.chat(user("phone (555) 010-1234 and more")).await;
    assert_eq!(s, 428);
    // Another app cannot use this approval.
    let (_, other) = pv_daemon::state::create_app(&h.st, "other", None, None).unwrap();
    let (s, _) = h.chat_key(&other, body.clone()).await;
    assert_eq!(s, 428);
    // Original app still can (approval unused so far).
    let (s, _) = h.chat(body).await;
    assert_eq!(s, 200);
}

#[tokio::test]
async fn approval_invalidated_by_policy_change() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let body = user("phone (555) 010-1234");
    let (_, v) = h.chat(body.clone()).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    h.admin_post(&format!("/approvals/{id}/approve"), json!({})).await;
    // Owner changes an unrelated rule: effective policy hash changes.
    h.admin_put("/categories/EMAIL", json!({"action": "REDACT"})).await;
    let (s, _) = h.chat(body).await;
    assert_eq!(s, 428, "approval bound to old policy hash must not apply");
}

#[tokio::test]
async fn forged_or_reused_approval_ids_do_nothing() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let (s, _) = h.admin_post("/approvals/apr_forged/approve", json!({})).await;
    assert_eq!(s, 409);
    let body = user("phone (555) 010-1234");
    let (_, v) = h.chat(body.clone()).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    // Client cannot self-approve through the proxy key.
    let r = h.http.post(format!("{}/api/admin/v1/approvals/{id}/approve", h.base)).bearer_auth(&h.key).send().await.unwrap();
    assert_eq!(r.status(), 401);
    h.admin_post(&format!("/approvals/{id}/approve"), json!({})).await;
    let (s, _) = h.admin_post(&format!("/approvals/{id}/approve"), json!({})).await;
    assert_eq!(s, 409, "already decided");
}

#[tokio::test]
async fn critical_findings_never_approvable() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    // Phone needs review, but the SSN is a FORBIDDEN block: blocked outright.
    let (s, v) = h.chat(user("phone (555) 010-1234 ssn 123-45-6789")).await;
    assert_eq!(s, 403, "{v}");
    assert_eq!(h.rec.count(), 0);
}

#[tokio::test]
async fn deny_blocks_retries_and_destroys_payload() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let body = user("phone (555) 010-1234");
    let (_, v) = h.chat(body.clone()).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    let (s, _) = h.admin_post(&format!("/approvals/{id}/deny"), json!({})).await;
    assert_eq!(s, 200);
    let (s, v) = h.chat(body).await;
    assert_eq!(s, 403, "{v}");
    let (_, a) = h.admin_get(&format!("/approvals/{id}")).await;
    assert_eq!(a["payload_available"], false);
    assert_eq!(h.rec.count(), 0);
}

#[tokio::test]
async fn reveal_requires_owner_confirmation_and_is_audited() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let (_, v) = h.chat(user("phone (555) 010-1234")).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    let (s, _) = h.admin_post(&format!("/approvals/{id}/reveal"), json!({})).await;
    assert_eq!(s, 428);
    let (s, v) = h.admin_post(&format!("/approvals/{id}/reveal"), json!({"confirm": true})).await;
    assert_eq!(s, 200);
    assert!(v["body"].to_string().contains("010-1234"));
    let ev = h.events_text().await;
    assert!(ev.contains("\"REVEAL\""));
    assert!(!ev.contains("010-1234"));
}

#[tokio::test]
async fn edit_and_retry_reclassifies() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let (_, v) = h.chat(user("phone (555) 010-1234")).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    // Edit that still contains a phone → held again (no silent forward).
    let (s, v) = h.admin_post(&format!("/approvals/{id}/resubmit"), json!({"body": user("phone (555) 010-9999")})).await;
    assert_eq!(s, 200);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "PV_APPROVAL_REQUIRED");
    let id2 = v["error"]["approval_id"].as_str().unwrap().to_string();
    // Edit that removes the phone → sent, response returned to the dashboard.
    let (_, v) = h.admin_post(&format!("/approvals/{id2}/resubmit"), json!({"body": user("please call me tomorrow")})).await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(h.rec.count(), 1);
    assert!(!h.upstream_text().contains("010-"));
}

#[tokio::test]
async fn whitelist_and_blacklist_persist_as_rules() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let (_, v) = h.chat(user("phone (555) 010-1234")).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    let (s, v) = h.admin_post(&format!("/approvals/{id}/whitelist"), json!({})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["allowlisted"], 1);
    // Allowlisted value no longer needs review in a new request.
    let (s, _) = h.chat(user("new msg (555) 010-1234")).await;
    assert_eq!(s, 200);
    // Blacklist: a different phone becomes an always-blocked exact value.
    let (_, v) = h.chat(user("other (555) 010-7777")).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    let (_, v) = h.admin_post(&format!("/approvals/{id}/blacklist"), json!({})).await;
    assert_eq!(v["registered"], 1);
    let (s, v) = h.chat(user("ping (555) 010-7777 again")).await;
    assert_eq!(s, 403, "{v}");
    assert!(v["error"]["categories"].to_string().contains("EXACT_SECRET"));
}

#[tokio::test]
async fn expired_payloads_destroyed() {
    let h = harness("mock-echo").await;
    review_phone(&h).await;
    let (_, v) = h.chat(user("phone (555) 010-1234")).await;
    let id = v["error"]["approval_id"].as_str().unwrap().to_string();
    // Simulate 16 minutes passing (restart-time cleanup uses the same path).
    let later = chrono::Utc::now() + chrono::Duration::minutes(16);
    let expired = h.st.db().expire_pending(later).unwrap();
    assert_eq!(expired, vec![id.clone()]);
    let n: i64 = h.st.db().conn().query_row("SELECT COUNT(*) FROM pending_payloads", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    let (s, _) = h.admin_post(&format!("/approvals/{id}/approve"), json!({})).await;
    assert_eq!(s, 409);
}

#[tokio::test]
async fn block_and_review_fallback_for_classifier_failure() {
    let h = harness("mock-echo").await;
    // Detection model required but missing → CLASSIFIER_FAILURE → review.
    h.st.engine.detector.set_settings(pv_detectors::DetectorSettings { span_enabled: true, span_required: true, span_timeout_ms: 500 });
    let (s, v) = h.chat(user("ordinary text")).await;
    assert_eq!(s, 428, "{v}");
    assert!(v["error"]["categories"].to_string().contains("CLASSIFIER_FAILURE"));
    assert_eq!(h.rec.count(), 0, "model failure never silently allows");
}
