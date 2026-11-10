use pv_core::category::*;
use pv_core::*;

fn finding(cat: &str, conf: f32) -> Finding {
    Finding::new(ClassifierType::Builtin, cat, 0, 5, conf, "test", "pattern", category::sensitivity_of(cat))
}

fn ctx(tier: DestinationTier) -> EvalContext<'static> {
    EvalContext {
        user_id: "local-owner",
        agent_id: Some("codex".into()),
        app_id: Some("app1".into()),
        request_id: None,
        destination: DestinationRef { id: "near".into(), tier },
        direction: Direction::Inbound,
        fallback: FallbackSettings { mode: FallbackMode::BlockAndReview, tee_consent: false, local_private_handling: false, uncertain_threshold: 0.5 },
        approval: None,
    }
}

fn defaults() -> PolicySet {
    default_policy(&default_category_choices())
}

#[test]
fn default_policy_is_valid() {
    defaults().validate().unwrap();
}

#[test]
fn critical_blocked_for_every_remote_tier() {
    for tier in [DestinationTier::AttestedTee, DestinationTier::ExternalUnverified] {
        for cat in [GOVERNMENT_ID, PASSPORT, PAYMENT_CARD, API_KEY, PRIVATE_KEY, AUTH_SECRET, DRIVER_LICENSE] {
            let d = evaluate(&defaults(), &[finding(cat, 0.99)], &ctx(tier));
            assert_eq!(d.disposition, Disposition::Block, "{cat} {tier:?}");
        }
    }
}

#[test]
fn critical_allowed_to_local_destination() {
    let d = evaluate(&defaults(), &[finding(GOVERNMENT_ID, 0.99)], &ctx(DestinationTier::Local));
    assert_eq!(d.disposition, Disposition::Allow);
}

#[test]
fn low_confidence_critical_still_blocks() {
    let d = evaluate(&defaults(), &[finding(GOVERNMENT_ID, 0.3)], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.disposition, Disposition::Block);
}

#[test]
fn email_tokenized_by_default() {
    let d = evaluate(&defaults(), &[finding(EMAIL, 0.99)], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.disposition, Disposition::Transform);
    assert_eq!(d.findings[0].action, Action::Tokenize);
}

#[test]
fn names_allowed_by_default() {
    let d = evaluate(&defaults(), &[finding(PERSON, 0.9)], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.disposition, Disposition::Allow);
}

fn custom(id: &str, kind: ScopeKind, scope_id: &str, cat: &str, action: Action, ov: OverrideMode) -> PolicyRule {
    PolicyRule {
        schema_version: 1,
        id: id.into(),
        version: 1,
        scope: Scope { kind, id: scope_id.into() },
        selector: Selector { categories: vec![cat.into()], destinations: vec!["any".into()], direction: Direction::Inbound, min_confidence: None },
        action,
        severity: Sensitivity::Moderate,
        notify: vec![],
        override_mode: ov,
        retention_days: 30,
        enabled: true,
        description: String::new(),
        origin: "owner".into(),
        critical_exception: false,
    }
}

#[test]
fn more_specific_scope_wins_when_overridable() {
    let mut set = defaults();
    set.rules.push(custom("app-email-allow", ScopeKind::Application, "app1", EMAIL, Action::Allow, OverrideMode::Allowed));
    let d = evaluate(&set, &[finding(EMAIL, 0.99)], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.findings[0].action, Action::Allow);
    assert_eq!(d.findings[0].policy_id.as_deref(), Some("app-email-allow"));
}

#[test]
fn lower_scope_cannot_weaken_forbidden_rule() {
    let mut set = defaults();
    set.rules.push(custom("agent-ssn-allow", ScopeKind::Agent, "codex", GOVERNMENT_ID, Action::Allow, OverrideMode::Allowed));
    set.rules.push(custom("req-ssn-allow", ScopeKind::Application, "app1", GOVERNMENT_ID, Action::Tokenize, OverrideMode::Allowed));
    let d = evaluate(&set, &[finding(GOVERNMENT_ID, 0.99)], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.disposition, Disposition::Block);
    assert!(d.findings[0].explanation.contains("non-overridable"));
}

#[test]
fn lower_scope_may_strengthen() {
    let mut set = defaults();
    set.rules.push(custom("app-email-block", ScopeKind::Application, "app1", EMAIL, Action::Block, OverrideMode::Allowed));
    let d = evaluate(&set, &[finding(EMAIL, 0.99)], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.disposition, Disposition::Block);
}

#[test]
fn rule_for_other_app_does_not_apply() {
    let mut set = defaults();
    set.rules.push(custom("other-app", ScopeKind::Application, "other", EMAIL, Action::Block, OverrideMode::Allowed));
    let d = evaluate(&set, &[finding(EMAIL, 0.99)], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.disposition, Disposition::Transform);
}

#[test]
fn uncertain_defaults_to_block_and_review_without_consent() {
    let mut f = finding(UNCERTAIN, 0.6);
    f.sensitivity = Sensitivity::High;
    let d = evaluate(&defaults(), &[f], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.disposition, Disposition::RequireApproval);
}

#[test]
fn uncertain_private_routes_with_consent() {
    let mut c = ctx(DestinationTier::ExternalUnverified);
    c.fallback = FallbackSettings { mode: FallbackMode::TeePrivateRoute, tee_consent: true, local_private_handling: false, uncertain_threshold: 0.5 };
    let d = evaluate(&defaults(), &[finding(UNCERTAIN, 0.6)], &c);
    assert_eq!(d.disposition, Disposition::PrivateRoute);
}

#[test]
fn tee_mode_without_consent_never_private_routes() {
    let mut c = ctx(DestinationTier::ExternalUnverified);
    c.fallback = FallbackSettings { mode: FallbackMode::TeePrivateRoute, tee_consent: false, local_private_handling: false, uncertain_threshold: 0.5 };
    let d = evaluate(&defaults(), &[finding(CLASSIFIER_FAILURE, 1.0)], &c);
    assert_eq!(d.disposition, Disposition::RequireApproval);
}

#[test]
fn private_route_satisfied_by_attested_destination() {
    let mut c = ctx(DestinationTier::AttestedTee);
    c.fallback = FallbackSettings { mode: FallbackMode::TeePrivateRoute, tee_consent: true, local_private_handling: false, uncertain_threshold: 0.5 };
    let d = evaluate(&defaults(), &[finding(UNCERTAIN, 0.6)], &c);
    assert_eq!(d.disposition, Disposition::Allow);
}

#[test]
fn low_confidence_noncritical_becomes_uncertain() {
    let mut c = ctx(DestinationTier::ExternalUnverified);
    c.fallback.mode = FallbackMode::TeePrivateRoute;
    c.fallback.tee_consent = true;
    let d = evaluate(&defaults(), &[finding(EMAIL, 0.3)], &c);
    assert_eq!(d.findings[0].action, Action::PrivateRoute);
}

#[test]
fn classifier_failure_never_allows() {
    let d = evaluate(&defaults(), &[finding(CLASSIFIER_FAILURE, 1.0)], &ctx(DestinationTier::ExternalUnverified));
    assert_ne!(d.disposition, Disposition::Allow);
}

fn grant() -> ApprovalGrant {
    ApprovalGrant { approval_id: "a".into(), request_hash: "h".into(), destination_id: "near".into(), policy_hash: "p".into(), expires_at: chrono::Utc::now() }
}

#[test]
fn approval_allows_review_once() {
    let mut c = ctx(DestinationTier::ExternalUnverified);
    c.approval = Some(grant());
    let d = evaluate(&defaults(), &[finding(UNCERTAIN, 0.6)], &c);
    assert_eq!(d.disposition, Disposition::Allow);
    assert!(d.used_approval);
}

#[test]
fn approval_cannot_bypass_forbidden_block() {
    let mut c = ctx(DestinationTier::ExternalUnverified);
    c.approval = Some(grant());
    let d = evaluate(&defaults(), &[finding(GOVERNMENT_ID, 0.99)], &c);
    assert_eq!(d.disposition, Disposition::Block);
    assert!(!d.used_approval);
}

#[test]
fn approval_once_block_is_approvable() {
    let mut set = defaults();
    set.rules.push(custom("owner-phone", ScopeKind::User, "local-owner", PHONE, Action::Block, OverrideMode::ApprovalOnce));
    let mut c = ctx(DestinationTier::ExternalUnverified);
    let d = evaluate(&set, &[finding(PHONE, 0.9)], &c);
    assert_eq!(d.disposition, Disposition::Block);
    c.approval = Some(grant());
    let d = evaluate(&set, &[finding(PHONE, 0.9)], &c);
    assert_eq!(d.disposition, Disposition::Allow);
}

#[test]
fn specific_secret_rule_beats_generic_within_owner_scope() {
    let mut set = defaults();
    let mut r = custom("secret-x", ScopeKind::User, "local-owner", "EXACT_SECRET:sec_1", Action::Tokenize, OverrideMode::PolicyOwnerOnly);
    r.selector.destinations = vec!["external".into()];
    set.rules.push(r);
    let mut f = finding(EXACT_SECRET, 1.0);
    f.secret_id = Some("sec_1".into());
    let d = evaluate(&set, &[f.clone()], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.findings[0].action, Action::Tokenize);
    f.secret_id = Some("sec_2".into());
    let d = evaluate(&set, &[f], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.findings[0].action, Action::Block);
}

#[test]
fn critical_without_any_rule_blocks() {
    let set = PolicySet::default();
    let d = evaluate(&set, &[finding(API_KEY, 0.99)], &ctx(DestinationTier::AttestedTee));
    assert_eq!(d.disposition, Disposition::Block);
}

#[test]
fn critical_exception_by_owner_allows() {
    let mut set = defaults();
    let r = set.rules.iter_mut().find(|r| r.id == "builtin-critical-api-key").unwrap();
    r.action = Action::Allow;
    r.version = 2;
    r.critical_exception = true;
    let d = evaluate(&set, &[finding(API_KEY, 0.99)], &ctx(DestinationTier::ExternalUnverified));
    assert_eq!(d.disposition, Disposition::Allow);
}

#[test]
fn outbound_critical_redacted() {
    let mut c = ctx(DestinationTier::ExternalUnverified);
    c.direction = Direction::Outbound;
    let d = evaluate(&defaults(), &[finding(API_KEY, 0.99)], &c);
    assert_eq!(d.findings[0].action, Action::Redact);
}

#[test]
fn hash_changes_with_policy_and_fallback() {
    let a = defaults();
    let fb = FallbackSettings::default();
    let h1 = a.effective_hash(&fb);
    let mut b = a.clone();
    b.rules[0].version = 2;
    assert_ne!(h1, b.effective_hash(&fb));
    let fb2 = FallbackSettings { tee_consent: true, ..fb.clone() };
    assert_ne!(h1, a.effective_hash(&fb2));
    assert_eq!(h1, a.effective_hash(&fb));
}

#[test]
fn invalid_rules_rejected() {
    let mut r = custom("x", ScopeKind::User, "local-owner", EMAIL, Action::Allow, OverrideMode::Allowed);
    r.selector.destinations = vec!["internet".into()];
    assert!(r.validate().is_err());
    let mut r2 = custom("bad id!", ScopeKind::User, "local-owner", EMAIL, Action::Allow, OverrideMode::Allowed);
    assert!(r2.validate().is_err());
    r2.id = "ok".into();
    r2.scope.kind = ScopeKind::Organization;
    assert!(r2.validate().is_err());
}

#[test]
fn rule_json_matches_spec_example() {
    let j = r#"{"id":"no-ssn-egress","version":1,"scope":{"kind":"user","id":"local-owner"},
      "selector":{"categories":["GOVERNMENT_ID"],"destinations":["external"]},"action":"BLOCK","severity":"critical",
      "notify":["local_user","security_admin"],"override":"FORBIDDEN","retention_days":30}"#;
    let r: PolicyRule = serde_json::from_str(j).unwrap();
    r.validate().unwrap();
    assert_eq!(r.override_mode, OverrideMode::Forbidden);
}

#[test]
fn overlap_groups_keep_all_members() {
    let mut a = finding(PERSON, 0.8);
    a.start_byte = 0;
    a.end_byte = 10;
    let mut b = finding(EXACT_SECRET, 1.0);
    b.start_byte = 5;
    b.end_byte = 15;
    let mut c = finding(EMAIL, 0.9);
    c.start_byte = 20;
    c.end_byte = 30;
    let g = overlap_groups(&[a, b, c]);
    assert_eq!(g.len(), 2);
    assert_eq!(g[0].len(), 2);
}

#[test]
fn finding_validation() {
    let t = "héllo";
    let mut f = finding(EMAIL, 0.9);
    f.start_byte = 0;
    f.end_byte = 2; // inside é
    assert!(f.validate_against(t).is_err());
    f.end_byte = 3;
    assert!(f.validate_against(t).is_ok());
    f.end_byte = 99;
    assert!(f.validate_against(t).is_err());
}

#[test]
fn error_envelope_shape() {
    let e = PvError::new(ErrorCode::PolicyBlocked, "Request blocked by local policy").with_event("e1");
    let v = serde_json::to_value(e.envelope()).unwrap();
    assert_eq!(v["error"]["type"], "promptvpn_policy_block");
    assert_eq!(v["error"]["code"], "PV_POLICY_BLOCKED");
    assert_eq!(v["error"]["retryable"], false);
    assert!(v["error"]["approval_id"].is_null());
}

#[test]
fn local_private_handling_resolves_without_tee_consent() {
    let mut c = ctx(DestinationTier::ExternalUnverified);
    c.fallback = FallbackSettings { mode: FallbackMode::TeePrivateRoute, tee_consent: false, local_private_handling: true, uncertain_threshold: 0.5 };
    let d = evaluate(&defaults(), &[finding(UNCERTAIN, 0.6)], &c);
    assert_eq!(d.disposition, Disposition::PrivateRoute);
    c.fallback.mode = FallbackMode::BlockAndReview;
    let d = evaluate(&defaults(), &[finding(UNCERTAIN, 0.6)], &c);
    assert_eq!(d.disposition, Disposition::RequireApproval);
}
