use chrono::{Duration, Utc};
use pv_audit::*;
use pv_core::*;

fn ev(id: &str, ts: chrono::DateTime<Utc>) -> AuditEvent {
    AuditEvent {
        schema_version: 1,
        event_id: id.into(),
        timestamp: ts,
        kind: "request".into(),
        app_id: "app".into(),
        agent_id: None,
        user_id: "local-owner".into(),
        destination_id: Some("near".into()),
        endpoint: Some("/v1/chat/completions".into()),
        categories: vec!["EMAIL".into()],
        action: "TRANSFORM".into(),
        policy_ids: vec!["builtin-email".into()],
        policy_hash: Some("sha256:x".into()),
        recognizer_versions: vec!["builtin-v1".into()],
        latency_ms: 3,
        finding_count: 1,
        approval_id: None,
        approval_status: None,
        error_code: None,
        severity: Sensitivity::Moderate,
        direction: Direction::Inbound,
        summary: "Email detected and tokenized locally".into(),
        context_ref: None,
    }
}

const K: &[u8] = b"0123456789abcdef0123456789abcdef";

#[test]
fn migrations_apply_and_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("pv.db");
    let db = Db::open(&p).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    drop(db);
    let db = Db::open(&p).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(db.integrity_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn newer_schema_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("pv.db");
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch("PRAGMA user_version = 999").unwrap();
    }
    assert!(matches!(Db::open(&p), Err(StoreError::TooNew { .. })));
}

#[test]
fn chain_verifies_and_detects_tamper() {
    let db = Db::open_memory().unwrap();
    for i in 0..5 {
        db.append_event(&ev(&format!("e{i}"), Utc::now()), 30, K, "k1").unwrap();
    }
    let keys = |_: &str| Some(K.to_vec());
    assert_eq!(db.verify_chain(&keys).unwrap(), 5);
    db.conn().execute("UPDATE events SET json=replace(json,'TRANSFORM','ALLOW') WHERE event_id='e2'", []).unwrap();
    assert!(db.verify_chain(&keys).is_err());
}

#[test]
fn deletion_in_middle_detected() {
    let db = Db::open_memory().unwrap();
    for i in 0..4 {
        db.append_event(&ev(&format!("e{i}"), Utc::now()), 30, K, "k1").unwrap();
    }
    db.conn().execute("DELETE FROM events WHERE event_id='e1'", []).unwrap();
    assert!(db.verify_chain(&|_| Some(K.to_vec())).is_err());
}

#[test]
fn retention_keeps_chain_valid() {
    let db = Db::open_memory().unwrap();
    let old = Utc::now() - Duration::days(40);
    db.append_event(&ev("old1", old), 30, K, "k1").unwrap();
    db.append_event(&ev("old2", old), 30, K, "k1").unwrap();
    db.append_event(&ev("new", Utc::now()), 30, K, "k1").unwrap();
    assert_eq!(db.enforce_retention(Utc::now()).unwrap(), 2);
    assert_eq!(db.verify_chain(&|_| Some(K.to_vec())).unwrap(), 1);
    db.append_event(&ev("new2", Utc::now()), 30, K, "k1").unwrap();
    assert_eq!(db.verify_chain(&|_| Some(K.to_vec())).unwrap(), 2);
    assert!(db.event("old1").unwrap().is_none());
}

#[test]
fn rekey_chain() {
    let db = Db::open_memory().unwrap();
    for i in 0..3 {
        db.append_event(&ev(&format!("e{i}"), Utc::now()), 30, K, "k1").unwrap();
    }
    let k2 = b"ffffffffffffffffffffffffffffffff";
    db.rekey_chain(&|_| Some(K.to_vec()), k2, "k2").unwrap();
    assert!(db.verify_chain(&|_| Some(K.to_vec())).is_err());
    assert_eq!(db.verify_chain(&|id| (id == "k2").then(|| k2.to_vec())).unwrap(), 3);
}

#[test]
fn event_queries_and_stats() {
    let db = Db::open_memory().unwrap();
    db.append_event(&ev("a", Utc::now()), 30, K, "k1").unwrap();
    let mut b = ev("b", Utc::now());
    b.action = "BLOCK".into();
    b.severity = Sensitivity::Critical;
    db.append_event(&b, 30, K, "k1").unwrap();
    let q = EventQuery { action: Some("BLOCK".into()), ..Default::default() };
    assert_eq!(db.events(&q).unwrap().len(), 1);
    let q = EventQuery { severity: Some("critical".into()), ..Default::default() };
    assert_eq!(db.events(&q).unwrap()[0].event_id, "b");
    let s = db.event_stats().unwrap();
    assert_eq!(s.total_requests, 2);
    assert_eq!(s.blocked, 1);
    assert_eq!(s.transformed, 1);
}

#[test]
fn policies_are_immutable_versions() {
    let db = Db::open_memory().unwrap();
    let set = default_policy(&default_category_choices());
    for r in &set.rules {
        db.put_policy(r, "test").unwrap();
    }
    assert_eq!(db.active_policies().unwrap().len(), set.rules.len());
    let mut r = set.rules[0].clone();
    assert!(db.put_policy(&r, "test").is_err());
    r.version = 2;
    r.action = Action::Allow;
    db.put_policy(&r, "owner").unwrap();
    let active = db.active_policies().unwrap();
    assert_eq!(active.iter().find(|x| x.id == r.id).unwrap().version, 2);
    assert_eq!(db.policy_history(&r.id).unwrap().len(), 2);
    assert!(db.deactivate_policy(&r.id).unwrap());
    assert_eq!(db.active_policies().unwrap().len(), set.rules.len() - 1);
}

#[test]
fn settings_versioned() {
    let db = Db::open_memory().unwrap();
    assert_eq!(db.set_setting("fallback", &FallbackSettings::default(), "owner").unwrap(), 1);
    let fb = FallbackSettings { tee_consent: true, ..Default::default() };
    assert_eq!(db.set_setting("fallback", &fb, "owner").unwrap(), 2);
    let got: FallbackSettings = db.setting("fallback").unwrap().unwrap();
    assert!(got.tee_consent);
}

fn pending(id: &str, exp: chrono::DateTime<Utc>, status: PendingStatus) -> PendingApproval {
    PendingApproval {
        schema_version: 1,
        approval_id: id.into(),
        event_id: "e".into(),
        app_id: "a".into(),
        destination_id: "near".into(),
        endpoint: "/v1/chat/completions".into(),
        request_hash: "h".into(),
        policy_hash: "p".into(),
        approved_payload_hash: "x".into(),
        categories: vec![],
        redacted_preview: "[REDACTED]".into(),
        reason: "r".into(),
        status,
        created_at: Utc::now(),
        expires_at: exp,
        decided_at: None,
        decided_by: None,
    }
}

#[test]
fn pending_expiry_and_single_use() {
    let db = Db::open_memory().unwrap();
    db.put_pending(&pending("p1", Utc::now() - Duration::minutes(1), PendingStatus::Pending)).unwrap();
    db.put_pending(&pending("p2", Utc::now() + Duration::minutes(10), PendingStatus::Approved)).unwrap();
    db.conn().execute("INSERT INTO pending_payloads(approval_id,key_id,nonce,ct,expires_at) VALUES('p1','k1',x'00',x'00',?1)", [(Utc::now() - Duration::minutes(1)).to_rfc3339()]).unwrap();
    let expired = db.expire_pending(Utc::now()).unwrap();
    assert_eq!(expired, vec!["p1".to_string()]);
    let n: i64 = db.conn().query_row("SELECT COUNT(*) FROM pending_payloads", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    assert!(db.consume_approval("p2").unwrap());
    assert!(!db.consume_approval("p2").unwrap());
    assert!(!db.consume_approval("p1").unwrap());
}
