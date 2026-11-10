//! Authenticated admin API at /api/admin/v1.

use crate::auth;
use crate::models::SpanSelection;
use crate::state::{self, AppState, OnboardingState, StoredEntity};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use pv_audit::{EventQuery, JsonTable};
use pv_core::category;
use pv_core::*;
use pv_detectors::exact::ExactKind;
use pv_detectors::normalize::NormOpts;
use pv_detectors::span::{normalize_label, CustomEntity};
use pv_detectors::DetectorSettings;
use pv_proxy::notify::NotificationSettings;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub struct ApiErr(StatusCode, String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": {"message": self.1, "code": "PV_ADMIN_ERROR"}}))).into_response()
    }
}

impl<E: std::fmt::Display> From<E> for ApiErr {
    fn from(e: E) -> Self {
        ApiErr(StatusCode::BAD_REQUEST, e.to_string())
    }
}

fn bad(m: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, m.into())
}

type R = Result<Json<Value>, ApiErr>;

fn owner_event(st: &AppState, kind: &str, action: &str, summary: &str, cats: Vec<String>, severity: Sensitivity) {
    let ev = AuditEvent {
        schema_version: AUDIT_SCHEMA_VERSION,
        event_id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        kind: kind.into(),
        app_id: "dashboard".into(),
        agent_id: None,
        user_id: "local-owner".into(),
        destination_id: None,
        endpoint: None,
        categories: cats,
        action: action.into(),
        policy_ids: vec![],
        policy_hash: Some(st.engine.policy_hash()),
        recognizer_versions: vec![],
        latency_ms: 0,
        finding_count: 0,
        approval_id: None,
        approval_status: None,
        error_code: None,
        severity,
        direction: Direction::Inbound,
        summary: summary.into(),
        context_ref: None,
    };
    st.engine.log(&ev);
}

pub fn router(st: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/session", get(session))
        .route("/session/ticket", post(ticket))
        .route("/session/logout", post(logout))
        .route("/overview", get(overview))
        .route("/events", get(events).delete(delete_events))
        .route("/events/export", get(export_events))
        .route("/events/verify", post(verify_events))
        .route("/events/{id}", get(event))
        .route("/policies", get(policies).post(put_policy))
        .route("/policies/validate", post(validate_policies))
        .route("/policies/test", post(test_policy))
        .route("/policies/export", get(export_policies))
        .route("/policies/import", post(import_policies))
        .route("/policies/{id}", delete(delete_policy))
        .route("/policies/{id}/history", get(policy_history))
        .route("/categories", get(categories))
        .route("/categories/{id}", put(set_category))
        .route("/secrets", get(secrets).post(add_secret))
        .route("/secrets/{id}", delete(remove_secret))
        .route("/entities", get(list_entities).post(add_entity))
        .route("/entities/{id}", delete(remove_entity))
        .route("/apps", get(apps).post(add_app))
        .route("/apps/{id}", delete(revoke_app).patch(patch_app))
        .route("/providers", get(providers).post(put_provider))
        .route("/providers/{id}", delete(delete_provider))
        .route("/providers/{id}/test", post(test_provider))
        .route("/near/models", get(near_models))
        .route("/near/verify", post(near_verify))
        .route("/models", get(models))
        .route("/models/select", post(select_model))
        .route("/models/install", post(install_model))
        .route("/models/test", post(test_model))
        .route("/approvals", get(approvals))
        .route("/approvals/{id}", get(approval))
        .route("/approvals/{id}/{action}", post(approval_action))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/onboarding", get(onboarding))
        .route("/onboarding/complete", post(complete_onboarding))
        .route("/emergency-stop", post(emergency_stop))
        .route("/health", get(health))
        .route("/vault/rotate", post(rotate))
        .route("/vault/recovery", post(recovery))
        .route("/integrations", get(integrations).post(add_integration))
        .route("/notifications/recent", get(recent_notifications))
        .route("/schemas", get(schemas))
        .route("/shutdown", post(shutdown))
        .layer(axum::middleware::from_fn_with_state(st, auth::admin_guard))
}

// ------------------------------------------------------------------ session

async fn session(State(st): St, headers: HeaderMap) -> R {
    let csrf = auth::session_from(&st, &headers).map(|(_, s)| s.csrf);
    Ok(Json(json!({"authenticated": true, "csrf": csrf, "version": state::VERSION})))
}

async fn ticket(State(st): St) -> R {
    let t = st.sessions.new_ticket();
    let port = st.port.load(std::sync::atomic::Ordering::Relaxed);
    Ok(Json(json!({"ticket": t, "url": format!("http://127.0.0.1:{port}/dashboard/login?ticket={t}")})))
}

async fn logout(State(st): St, headers: HeaderMap) -> R {
    if let Some((id, _)) = auth::session_from(&st, &headers) {
        st.sessions.revoke(&id);
    }
    Ok(Json(json!({"ok": true})))
}

// ------------------------------------------------------------------ overview & events

async fn overview(State(st): St) -> R {
    let stats = st.db().event_stats()?;
    let recent = st.db().events(&EventQuery { limit: Some(8), ..Default::default() })?;
    let pending = st.db().list_pending(false)?.into_iter().filter(|p| p.expires_at > Utc::now()).count();
    let s = st.engine.settings();
    Ok(Json(json!({
        "stats": stats,
        "recent": recent,
        "pending_approvals": pending,
        "protected": !s.emergency_stop && s.default_destination.is_some(),
        "emergency_stop": s.emergency_stop,
        "base_url": st.base_url(),
    })))
}

#[derive(Deserialize)]
struct EvQ {
    action: Option<String>,
    severity: Option<String>,
    app_id: Option<String>,
    kind: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn events(State(st): St, Query(q): Query<EvQ>) -> R {
    let ev = st.db().events(&EventQuery { action: q.action, severity: q.severity, app_id: q.app_id, kind: q.kind, since: None, limit: q.limit, offset: q.offset })?;
    Ok(Json(json!({"events": ev})))
}

async fn event(State(st): St, Path(id): Path<String>) -> R {
    let e = st.db().event(&id)?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "event not found".into()))?;
    Ok(Json(json!(e)))
}

async fn export_events(State(st): St) -> Result<Response, ApiErr> {
    let ev = st.db().events(&EventQuery { limit: Some(10_000), ..Default::default() })?;
    let mut out = String::new();
    for e in ev.iter().rev() {
        out.push_str(&serde_json::to_string(e)?);
        out.push('\n');
    }
    Ok(([("content-type", "application/x-ndjson"), ("content-disposition", "attachment; filename=promptvpn-events.jsonl")], out).into_response())
}

async fn verify_events(State(st): St) -> R {
    let v = st.vault().clone();
    match st.db().verify_chain(&|k| v.audit_mac_key(k)) {
        Ok(n) => Ok(Json(json!({"ok": true, "verified_events": n}))),
        Err(e) => Ok(Json(json!({"ok": false, "error": e.to_string()}))),
    }
}

async fn delete_events(State(st): St) -> R {
    let n = st.db().delete_all_events()?;
    owner_event(&st, "admin", "DELETE_EVENTS", &format!("Owner deleted {n} audit events"), vec![], Sensitivity::Moderate);
    Ok(Json(json!({"deleted": n})))
}

// ------------------------------------------------------------------ policies

async fn policies(State(st): St) -> R {
    let p = st.engine.policy();
    Ok(Json(json!({"rules": p.rules, "effective_policy_hash": st.engine.policy_hash()})))
}

fn store_rule(st: &AppState, mut rule: PolicyRule, actor: &str) -> Result<PolicyRule, ApiErr> {
    rule.version = st.db().latest_policy_version(&rule.id)? + 1;
    if rule.origin.is_empty() {
        rule.origin = "owner".into();
    }
    rule.validate()?;
    // Validate the whole resulting set before committing.
    let mut set = st.engine.policy().as_ref().clone();
    set.rules.retain(|r| r.id != rule.id);
    set.rules.push(rule.clone());
    set.validate()?;
    st.db().put_policy(&rule, actor)?;
    state::reload_policy(st)?;
    Ok(rule)
}

async fn put_policy(State(st): St, Json(rule): Json<PolicyRule>) -> R {
    // Critical built-in rules change only through the category exception flow.
    if rule.id.starts_with("builtin-critical-") {
        return Err(bad("critical protections are changed under Categories with an explicit confirmation"));
    }
    let r = store_rule(&st, rule, "owner")?;
    owner_event(&st, "policy", "POLICY_EDIT", &format!("Policy {} saved as v{}", r.id, r.version), r.selector.categories.clone(), Sensitivity::Moderate);
    Ok(Json(json!(r)))
}

async fn delete_policy(State(st): St, Path(id): Path<String>) -> R {
    if id.starts_with("builtin-critical-") {
        return Err(bad("critical protections cannot be deleted; disable a category under Categories with confirmation"));
    }
    if !st.db().deactivate_policy(&id)? {
        return Err(ApiErr(StatusCode::NOT_FOUND, "no active policy with that id".into()));
    }
    state::reload_policy(&st)?;
    owner_event(&st, "policy", "POLICY_DELETE", &format!("Policy {id} deactivated"), vec![], Sensitivity::Moderate);
    Ok(Json(json!({"ok": true})))
}

async fn policy_history(State(st): St, Path(id): Path<String>) -> R {
    let h: Vec<Value> = st.db().policy_history(&id)?.into_iter().map(|(r, at, actor, active)| json!({"rule": r, "created_at": at, "actor": actor, "active": active})).collect();
    Ok(Json(json!({"history": h})))
}

async fn validate_policies(Json(v): Json<Value>) -> R {
    let rules: Vec<PolicyRule> = serde_json::from_value(v.get("rules").cloned().unwrap_or(v.clone())).map_err(|e| bad(format!("PV_POLICY_INVALID: {e}")))?;
    let set = PolicySet { rules };
    match set.validate() {
        Ok(()) => Ok(Json(json!({"valid": true, "rules": set.rules.len()}))),
        Err(e) => Ok(Json(json!({"valid": false, "error": e.to_string()}))),
    }
}

async fn export_policies(State(st): St) -> R {
    Ok(Json(json!({"format": "promptvpn-policy-v1", "rules": st.engine.policy().rules})))
}

async fn import_policies(State(st): St, Json(v): Json<Value>) -> R {
    let rules: Vec<PolicyRule> = serde_json::from_value(v.get("rules").cloned().unwrap_or_default()).map_err(|e| bad(format!("PV_POLICY_INVALID: {e}")))?;
    let mut n = 0;
    for r in rules {
        if r.id.starts_with("builtin-critical-") {
            continue; // never weaken critical defaults through import
        }
        store_rule(&st, r, "owner-import")?;
        n += 1;
    }
    owner_event(&st, "policy", "POLICY_IMPORT", &format!("Imported {n} policy rules"), vec![], Sensitivity::Moderate);
    Ok(Json(json!({"imported": n})))
}

#[derive(Deserialize)]
struct TestReq {
    text: String,
    destination_tier: Option<DestinationTier>,
    destination_id: Option<String>,
    #[serde(default)]
    direction: Direction,
    #[serde(default = "default_true")]
    use_models: bool,
}

/// Playground: run detection and policy on owner-typed text (local only).
async fn test_policy(State(st): St, Json(t): Json<TestReq>) -> R {
    if t.text.len() > 200_000 {
        return Err(bad("text too long"));
    }
    let rep = st.engine.detector.detect(std::slice::from_ref(&t.text), t.use_models).await;
    let s = st.engine.settings();
    let dest = DestinationRef { id: t.destination_id.unwrap_or_else(|| "playground".into()), tier: t.destination_tier.unwrap_or(DestinationTier::ExternalUnverified) };
    let ctx = EvalContext { user_id: "local-owner", agent_id: None, app_id: None, request_id: None, destination: dest, direction: t.direction, fallback: s.fallback, approval: None };
    let d = evaluate(&st.engine.policy(), &rep.findings, &ctx);
    let findings: Vec<Value> = rep
        .findings
        .iter()
        .map(|f| json!({"finding": f, "text": t.text.get(f.start_byte..f.end_byte).unwrap_or(""), "action": d.action_for(&f.finding_id)}))
        .collect();
    Ok(Json(json!({"findings": findings, "decision": d, "failures": rep.failures, "recognizers": rep.recognizer_versions})))
}

// ------------------------------------------------------------------ categories

async fn categories(State(st): St) -> R {
    let p = st.engine.policy();
    let cats: Vec<Value> = category::BUILTIN
        .iter()
        .map(|c| {
            let id = if c.critical { format!("builtin-critical-{}", c.id.to_lowercase().replace('_', "-")) } else { format!("builtin-{}", c.id.to_lowercase().replace('_', "-")) };
            let rule = p.rules.iter().find(|r| r.id == id);
            json!({
                "id": c.id, "label": c.label, "description": c.description, "critical": c.critical, "sensitivity": c.sensitivity, "examples": c.examples,
                "action": rule.map(|r| r.action), "protected": rule.map(|r| r.action != Action::Allow).unwrap_or(c.critical),
                "exception": rule.map(|r| r.critical_exception).unwrap_or(false), "rule_id": id, "rule_version": rule.map(|r| r.version)
            })
        })
        .collect();
    Ok(Json(json!({"categories": cats})))
}

#[derive(Deserialize)]
struct SetCategory {
    action: Action,
    /// Required to relax a critical category: must equal "DISABLE <CATEGORY_ID>".
    confirm: Option<String>,
}

fn set_category_inner(st: &AppState, id: &str, action: Action, confirm: Option<&str>, actor: &str) -> Result<PolicyRule, ApiErr> {
    let info = category::info(id).ok_or_else(|| bad("unknown built-in category"))?;
    let rid = if info.critical { format!("builtin-critical-{}", id.to_lowercase().replace('_', "-")) } else { format!("builtin-{}", id.to_lowercase().replace('_', "-")) };
    let current = st.engine.policy().rules.iter().find(|r| r.id == rid).cloned().ok_or_else(|| bad("rule missing"))?;
    let mut r = current.clone();
    r.action = action;
    if info.critical {
        let relaxing = action.strictness() < Action::Block.strictness();
        if relaxing {
            let want = format!("DISABLE {id}");
            if confirm != Some(want.as_str()) {
                return Err(ApiErr(StatusCode::PRECONDITION_REQUIRED, format!("Relaxing a critical protection requires confirm = \"{want}\"")));
            }
            r.critical_exception = true;
        } else {
            r.critical_exception = false;
        }
    }
    let r = store_rule(st, r, actor)?;
    if info.critical {
        owner_event(st, "policy", if r.critical_exception { "CRITICAL_EXCEPTION" } else { "CRITICAL_RESTORED" }, &format!("{} protection set to {} (v{})", info.label, r.action.as_str(), r.version), vec![id.to_string()], Sensitivity::Critical);
    } else {
        owner_event(st, "policy", "CATEGORY_ACTION", &format!("{} set to {}", info.label, r.action.as_str()), vec![id.to_string()], Sensitivity::Moderate);
    }
    Ok(r)
}

async fn set_category(State(st): St, Path(id): Path<String>, Json(b): Json<SetCategory>) -> R {
    Ok(Json(json!(set_category_inner(&st, &id, b.action, b.confirm.as_deref(), "owner")?)))
}

// ------------------------------------------------------------------ secrets

async fn secrets(State(st): St) -> R {
    Ok(Json(json!({"secrets": st.vault().list_secrets()?})))
}

#[derive(Deserialize)]
struct AddSecret {
    #[serde(default = "default_kind")]
    kind: ExactKind,
    label: String,
    value: String,
    #[serde(default = "default_norm")]
    norm: NormOpts,
    #[serde(default = "default_block")]
    action: Action,
    #[serde(default)]
    allow: bool,
}
fn default_true() -> bool {
    true
}
fn default_kind() -> ExactKind {
    ExactKind::Value
}
fn default_norm() -> NormOpts {
    NormOpts { unicode: true, casefold: true, whitespace: true, separators: false }
}
fn default_block() -> Action {
    Action::Block
}

pub fn add_secret_inner(st: &AppState, s: AddSecretInput) -> Result<String, ApiErr> {
    let id = st.vault().add_secret(s.kind, &s.label, &s.value, s.norm, s.allow, s.action.as_str())?;
    if !s.allow && s.action != Action::Block {
        let rule = PolicyRule {
            schema_version: 1,
            id: format!("secret-{id}"),
            version: 1,
            scope: Scope { kind: ScopeKind::User, id: "local-owner".into() },
            selector: Selector { categories: vec![format!("EXACT_SECRET:{id}")], destinations: vec!["external".into()], direction: Direction::Inbound, min_confidence: None },
            action: s.action,
            severity: Sensitivity::Critical,
            notify: vec!["local_user".into()],
            override_mode: OverrideMode::PolicyOwnerOnly,
            retention_days: 30,
            enabled: true,
            description: format!("Action for registered value '{}'", s.label),
            origin: "owner".into(),
            critical_exception: false,
        };
        store_rule(st, rule, "owner")?;
    }
    st.engine.reload_exact().map_err(bad)?;
    owner_event(st, "secret", if s.allow { "ALLOWLIST_ADD" } else { "SECRET_ADD" }, &format!("Registered {} '{}' ({})", if s.allow { "allowlisted value" } else { "exact value" }, s.label, s.action.as_str()), vec![category::EXACT_SECRET.into()], Sensitivity::Moderate);
    Ok(id)
}

pub struct AddSecretInput {
    pub kind: ExactKind,
    pub label: String,
    pub value: String,
    pub norm: NormOpts,
    pub action: Action,
    pub allow: bool,
}

async fn add_secret(State(st): St, Json(s): Json<AddSecret>) -> R {
    let id = add_secret_inner(&st, AddSecretInput { kind: s.kind, label: s.label, value: s.value, norm: s.norm, action: s.action, allow: s.allow })?;
    Ok(Json(json!({"secret_id": id})))
}

async fn remove_secret(State(st): St, Path(id): Path<String>) -> R {
    if !st.vault().remove_secret(&id)? {
        return Err(ApiErr(StatusCode::NOT_FOUND, "no such secret".into()));
    }
    let _ = st.db().deactivate_policy(&format!("secret-{id}"));
    state::reload_policy(&st)?;
    st.engine.reload_exact().map_err(bad)?;
    owner_event(&st, "secret", "SECRET_REMOVE", &format!("Removed registered value {id}"), vec![], Sensitivity::Moderate);
    Ok(Json(json!({"ok": true})))
}

// ------------------------------------------------------------------ custom entity types

async fn list_entities(State(st): St) -> R {
    Ok(Json(json!({"entities": state::entities(&st)?, "span_model_loaded": st.engine.detector.span_loaded(), "builtin_labels": builtin_labels()})))
}

#[derive(Deserialize)]
pub struct AddEntity {
    /// What to look for, in plain words, e.g. "insurance policy number".
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_tokenize")]
    pub action: Action,
}

fn default_tokenize() -> Action {
    Action::Tokenize
}

fn builtin_labels() -> Vec<&'static str> {
    pv_detectors::span::GLINER_LABELS.iter().map(|(l, _)| *l).collect()
}

/// Validate a label without writing anything. Returns the normalized label.
pub fn check_entity_label(st: &AppState, raw: &str, pending: &[String]) -> Result<String, ApiErr> {
    let label = normalize_label(raw).ok_or_else(|| bad("label must be 2..60 letters, digits or spaces"))?;
    if builtin_labels().contains(&label.as_str()) {
        return Err(bad(format!("'{label}' is already a built-in category")));
    }
    if state::entities(st)?.iter().any(|x| x.entity.label == label) || pending.contains(&label) {
        return Err(bad(format!("'{label}' already exists")));
    }
    Ok(label)
}

pub fn add_entity_inner(st: &AppState, e: AddEntity) -> Result<String, ApiErr> {
    let label = check_entity_label(st, &e.label, &[])?;
    let id = format!("ent_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
    let se = StoredEntity { entity: CustomEntity { id: id.clone(), label: label.clone(), description: e.description }, action: e.action, created_at: Utc::now().to_rfc3339() };
    st.vault().put_sealed("entity", &id, &serde_json::to_vec(&se)?)?;
    let rule = PolicyRule {
        schema_version: 1,
        id: format!("entity-{id}"),
        version: 1,
        scope: Scope { kind: ScopeKind::User, id: "local-owner".into() },
        selector: Selector { categories: vec![format!("CUSTOM:{id}")], destinations: vec!["external".into()], direction: Direction::Inbound, min_confidence: None },
        action: e.action,
        severity: Sensitivity::High,
        notify: vec!["local_user".into()],
        override_mode: OverrideMode::PolicyOwnerOnly,
        retention_days: 30,
        enabled: true,
        description: format!("Custom entity type '{label}'"),
        origin: "owner".into(),
        critical_exception: false,
    };
    store_rule(st, rule, "owner")?;
    state::reload_entities(st)?;
    owner_event(st, "entity", "ENTITY_ADD", &format!("Added custom entity type '{label}' ({})", e.action.as_str()), vec![format!("CUSTOM:{id}")], Sensitivity::Moderate);
    Ok(id)
}

async fn add_entity(State(st): St, Json(e): Json<AddEntity>) -> R {
    Ok(Json(json!({"entity_id": add_entity_inner(&st, e)?, "span_model_loaded": st.engine.detector.span_loaded()})))
}

async fn remove_entity(State(st): St, Path(id): Path<String>) -> R {
    if !st.vault().delete_sealed("entity", &id)? {
        return Err(ApiErr(StatusCode::NOT_FOUND, "no such entity type".into()));
    }
    let _ = st.db().deactivate_policy(&format!("entity-{id}"));
    state::reload_policy(&st)?;
    state::reload_entities(&st)?;
    owner_event(&st, "entity", "ENTITY_REMOVE", &format!("Removed custom entity type {id}"), vec![], Sensitivity::Moderate);
    Ok(Json(json!({"ok": true})))
}

// ------------------------------------------------------------------ apps

async fn apps(State(st): St) -> R {
    Ok(Json(json!({"apps": st.db().apps()?, "base_url": st.base_url()})))
}

#[derive(Deserialize)]
struct AddApp {
    name: String,
    agent_id: Option<String>,
    destination_id: Option<String>,
}

async fn add_app(State(st): St, Json(a): Json<AddApp>) -> R {
    let (rec, key) = state::create_app(&st, &a.name, a.agent_id, a.destination_id)?;
    let _ = state::write_private(&st.paths.app_key(&rec.app_id), key.as_bytes());
    owner_event(&st, "app", "APP_ADD", &format!("Created local API key for '{}'", rec.name), vec![], Sensitivity::Low);
    Ok(Json(json!({"app": rec, "api_key": key, "base_url": st.base_url()})))
}

async fn revoke_app(State(st): St, Path(id): Path<String>) -> R {
    st.db().revoke_app(&id)?;
    let _ = std::fs::remove_file(st.paths.app_key(&id));
    owner_event(&st, "app", "APP_REVOKE", &format!("Revoked app {id}"), vec![], Sensitivity::Low);
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct PatchApp {
    agent_id: Option<String>,
    destination_id: Option<String>,
}

async fn patch_app(State(st): St, Path(id): Path<String>, Json(p): Json<PatchApp>) -> R {
    st.db().update_app_routing(&id, p.agent_id.as_deref(), p.destination_id.as_deref())?;
    Ok(Json(json!({"ok": true})))
}

// ------------------------------------------------------------------ providers & NEAR

async fn providers(State(st): St) -> R {
    let list: Vec<Value> = st
        .engine
        .registry
        .providers()
        .into_iter()
        .map(|p| {
            let has_cred = p.credential_ref.as_deref().and_then(|r| st.creds().get(r)).is_some();
            json!({"config": p, "has_credential": has_cred})
        })
        .collect();
    Ok(Json(json!({"providers": list, "verifications": st.engine.registry.cached_verifications(), "attestations": st.db().attestations()?})))
}

#[derive(Deserialize)]
struct PutProvider {
    #[serde(flatten)]
    config: ProviderConfig,
    api_key: Option<String>,
}

pub fn put_provider_inner(st: &AppState, mut cfg: ProviderConfig, api_key: Option<String>) -> Result<ProviderConfig, ApiErr> {
    if !cfg.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || cfg.id.is_empty() {
        return Err(bad("provider id must be [A-Za-z0-9_-]+"));
    }
    url_ok(&cfg.base_url)?;
    if cfg.kind == ProviderKind::Ollama && !pv_provider::is_loopback(&cfg.base_url) {
        return Err(bad("Ollama generation endpoints must be on loopback"));
    }
    if cfg.declared_tier == DestinationTier::AttestedTee && cfg.kind != ProviderKind::NearAi {
        return Err(bad("only NEAR AI destinations can be attested TEEs"));
    }
    if let Some(k) = api_key.filter(|k| !k.is_empty()) {
        let r = cfg.credential_ref.clone().unwrap_or_else(|| format!("provider:{}", cfg.id));
        st.creds().set(&r, &k)?;
        cfg.credential_ref = Some(r);
    }
    st.db().put_json(JsonTable::Providers, &cfg.id, &cfg)?;
    state::reload_providers(st)?;
    owner_event(st, "provider", "PROVIDER_SAVE", &format!("Saved destination {} ({:?}, {:?})", cfg.id, cfg.kind, cfg.declared_tier), vec![], Sensitivity::Low);
    Ok(cfg)
}

fn url_ok(u: &str) -> Result<(), ApiErr> {
    let p = url_parse(u).ok_or_else(|| bad("invalid base_url"))?;
    if p != "http" && p != "https" {
        return Err(bad("base_url must be http(s)"));
    }
    if p == "http" && !pv_provider::is_loopback(u) {
        return Err(bad("plain http is only allowed for loopback endpoints"));
    }
    Ok(())
}

fn url_parse(u: &str) -> Option<String> {
    let (scheme, rest) = u.split_once("://")?;
    if rest.is_empty() {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

async fn put_provider(State(st): St, Json(p): Json<PutProvider>) -> R {
    Ok(Json(json!(put_provider_inner(&st, p.config, p.api_key)?)))
}

async fn delete_provider(State(st): St, Path(id): Path<String>) -> R {
    if let Some(p) = st.engine.registry.get(&id) {
        if let Some(r) = &p.credential_ref {
            st.creds().delete(r);
        }
    }
    st.db().delete_json(JsonTable::Providers, &id)?;
    state::reload_providers(&st)?;
    owner_event(&st, "provider", "PROVIDER_DELETE", &format!("Deleted destination {id}"), vec![], Sensitivity::Low);
    Ok(Json(json!({"ok": true})))
}

/// Connectivity + tier test with a synthetic, non-sensitive prompt.
async fn test_provider(State(st): St, Path(id): Path<String>) -> R {
    let cfg = st.engine.registry.get(&id).ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "no such provider".into()))?;
    let r = st.engine.registry.resolve(&cfg, None).await;
    let body = json!({"messages": [{"role": "user", "content": "Reply with the single word: ok"}], "max_tokens": 8});
    let t = std::time::Instant::now();
    let send = st.engine.registry.send(&r, pv_provider::Endpoint::Chat, body, r.tier == DestinationTier::AttestedTee).await;
    let (ok, detail) = match send {
        Ok(up) => {
            use futures::StreamExt;
            let mut b = up.body;
            let mut buf = Vec::new();
            while let Some(Ok(c)) = b.next().await {
                buf.extend_from_slice(&c);
                if buf.len() > 1 << 20 {
                    break;
                }
            }
            let v: Value = serde_json::from_slice(&buf).unwrap_or(Value::Null);
            let id = v["id"].as_str().map(String::from);
            let receipt = match (cfg.kind, id) {
                (ProviderKind::NearAi, Some(id)) => Some(st.engine.registry.verify_receipt(&cfg.id, &id).await),
                _ => None,
            };
            (true, json!({"reply": v["choices"][0]["message"]["content"], "receipt": receipt}))
        }
        Err(e) => (false, json!({"error": e.to_string()})),
    };
    Ok(Json(json!({"ok": ok, "tier": r.tier, "model": r.model, "verification": r.verification, "latency_ms": t.elapsed().as_millis() as u64, "detail": detail})))
}

#[derive(Deserialize)]
struct ProvQ {
    provider_id: Option<String>,
}

async fn near_models(State(st): St, Query(q): Query<ProvQ>) -> R {
    let pid = q.provider_id.unwrap_or_else(|| "near".into());
    let models = match st.engine.registry.near_models(&pid).await {
        Ok(m) => m,
        Err(_) => {
            // Discovery is public; allow it before NEAR is configured.
            let b = pv_provider::near::NearSdkBackend::new(pv_provider::near::DEFAULT_NEAR_BASE, None)?;
            use pv_provider::near::NearBackend;
            b.list_models().await?
        }
    };
    Ok(Json(json!({"models": models, "spec_preferred": pv_provider::near::SPEC_PREFERRED_MODEL})))
}

#[derive(Deserialize)]
struct VerifyReq {
    provider_id: Option<String>,
    model: String,
}

async fn near_verify(State(st): St, Json(v): Json<VerifyReq>) -> R {
    let pid = v.provider_id.unwrap_or_else(|| "near".into());
    let r = if st.engine.registry.near_backend(&pid).is_some() {
        st.engine.registry.verify_near(&pid, &v.model, true).await
    } else {
        use pv_provider::near::NearBackend;
        pv_provider::near::NearSdkBackend::new(pv_provider::near::DEFAULT_NEAR_BASE, None)?.verify(&v.model).await
    };
    st.db().put_attestation(&pid, &v.model, &json!(r))?;
    owner_event(&st, "attestation", if r.eligible { "TEE_VERIFIED" } else { "TEE_NOT_ELIGIBLE" }, &format!("NEAR model {}: {}", v.model, if r.eligible { "attestation verified".to_string() } else { r.error.clone().unwrap_or_default() }), vec![], if r.eligible { Sensitivity::Low } else { Sensitivity::Moderate });
    Ok(Json(json!(r)))
}

// ------------------------------------------------------------------ local models

async fn models(State(st): St) -> R {
    let sel: SpanSelection = st.db().setting("span_model")?.unwrap_or_default();
    let mut v = st.models.status();
    v["selection"] = json!(sel);
    v["detector"] = json!(st.engine.detector.settings());
    v["ollama"] = ollama_status().await;
    Ok(Json(v))
}

/// Ollama is only an optional local generation destination now.
async fn ollama_status() -> Value {
    match pv_detectors::ollama::Ollama::new("http://127.0.0.1:11434") {
        Ok(o) => match o.version().await {
            Ok(v) => json!({"reachable": true, "version": v, "models": o.tags().await.unwrap_or_default().into_iter().map(|t| t.name).collect::<Vec<_>>()}),
            Err(_) => json!({"reachable": false, "install": pv_detectors::ollama::install_instructions(crate::models::os_name())}),
        },
        Err(e) => json!({"reachable": false, "error": e.to_string()}),
    }
}

#[derive(Deserialize)]
struct SelectModel {
    model: String,
}

/// Select (and load) a span model size. Install it first.
pub fn select_model_inner(st: &AppState, model: &str) -> Result<Value, ApiErr> {
    let m = crate::models::resolve_name(model).ok_or_else(|| bad("model must be edge, small or base"))?;
    st.models.provisioned(&m).map_err(|e| bad(format!("{m} is not installed: {e}")))?;
    st.db().set_setting("span_model", &SpanSelection { model: m.clone() }, "owner")?;
    let loaded = state::apply_span(st, &m);
    owner_event(st, "model", "SPAN_MODEL_SELECT", &format!("Detection model set to {m}"), vec![], Sensitivity::Low);
    Ok(json!({"model": m, "loaded": loaded, "status": st.models.status()["span"]}))
}

async fn select_model(State(st): St, Json(s): Json<SelectModel>) -> R {
    Ok(Json(select_model_inner(&st, &s.model)?))
}

#[derive(Deserialize, Default)]
struct InstallReq {
    model: Option<String>,
}

/// Download (pinned, sha256-verified), select and load a span model.
pub async fn install_model_inner(st: &AppState, model: Option<&str>) -> Result<Value, ApiErr> {
    let sel: SpanSelection = st.db().setting("span_model")?.unwrap_or_default();
    let m = match model {
        Some(x) => crate::models::resolve_name(x).ok_or_else(|| bad("model must be edge, small or base"))?,
        None => sel.model,
    };
    st.models.provision(&m).await.map_err(bad)?;
    select_model_inner(st, &m)
}

async fn install_model(State(st): St, body: Option<Json<InstallReq>>) -> R {
    let r = body.map(|b| b.0).unwrap_or_default();
    Ok(Json(install_model_inner(&st, r.model.as_deref()).await?))
}

#[derive(Deserialize)]
struct ModelTest {
    text: Option<String>,
}

async fn test_model(State(st): St, Json(t): Json<ModelTest>) -> R {
    let text = t.text.unwrap_or_else(|| "Synthetic test: remind maria gonzalez (test.user@example.com) about her insulin refill; SSN 123-45-6789".into());
    let t0 = std::time::Instant::now();
    let rep = st.engine.detector.detect(std::slice::from_ref(&text), true).await;
    let found: Vec<Value> = rep.findings.iter().map(|f| json!({"category": f.category, "text": text.get(f.start_byte..f.end_byte), "confidence": f.confidence, "recognizer": f.recognizer_id})).collect();
    Ok(Json(json!({"latency_ms": t0.elapsed().as_millis() as u64, "findings": found, "failures": rep.failures, "recognizers": rep.recognizer_versions, "span_latency_ms": rep.span_latency_ms})))
}

// ------------------------------------------------------------------ approvals

#[derive(Deserialize)]
struct ApQ {
    all: Option<bool>,
}

async fn approvals(State(st): St, Query(q): Query<ApQ>) -> R {
    st.engine.housekeeping();
    Ok(Json(json!({"approvals": st.db().list_pending(q.all.unwrap_or(false))?})))
}

async fn approval(State(st): St, Path(id): Path<String>) -> R {
    let p = st.db().get_pending(&id)?.ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "no such approval".into()))?;
    let ev = st.db().event(&p.event_id)?;
    Ok(Json(json!({"approval": p, "event": ev, "payload_available": st.vault().get_pending_payload(&id)?.is_some()})))
}

async fn approval_action(State(st): St, Path((id, action)): Path<(String, String)>, body: Option<Json<Value>>) -> R {
    let body = body.map(|b| b.0).unwrap_or(Value::Null);
    let map = |e: pv_proxy::approvals::ApprovalError| ApiErr(StatusCode::CONFLICT, e.to_string());
    let v = match action.as_str() {
        "approve" => json!(st.engine.approve(&id, "local-owner").map_err(map)?),
        "deny" => json!(st.engine.deny(&id, "local-owner").map_err(map)?),
        "cancel" => json!(st.engine.cancel(&id, "local-owner").map_err(map)?),
        "whitelist" => {
            let (p, n) = st.engine.whitelist(&id, "local-owner").await.map_err(map)?;
            json!({"approval": p, "allowlisted": n})
        }
        "blacklist" => {
            let (p, n) = st.engine.blacklist(&id, "local-owner").await.map_err(map)?;
            json!({"approval": p, "registered": n})
        }
        "reveal" => {
            if body.get("confirm").and_then(|c| c.as_bool()) != Some(true) {
                return Err(ApiErr(StatusCode::PRECONDITION_REQUIRED, "reveal requires {\"confirm\": true}".into()));
            }
            json!({"body": st.engine.reveal(&id, "local-owner").map_err(map)?})
        }
        "resubmit" => {
            let edited = body.get("body").cloned().ok_or_else(|| bad("missing edited body"))?;
            match st.engine.resubmit_edited(&id, &edited).await.map_err(map)? {
                Ok(out) => match out.body {
                    pv_proxy::Body::Json(v) => json!({"ok": true, "response": v, "event_id": out.event_id}),
                    pv_proxy::Body::Stream(_) => json!({"ok": true}),
                },
                Err(e) => json!({"ok": false, "error": e.envelope().error}),
            }
        }
        _ => return Err(ApiErr(StatusCode::NOT_FOUND, "unknown action".into())),
    };
    Ok(Json(v))
}

// ------------------------------------------------------------------ settings & onboarding

async fn get_settings(State(st): St) -> R {
    let ns: NotificationSettings = st.notify_settings.settings.lock().unwrap().clone();
    let onboarding: OnboardingState = st.db().setting("onboarding")?.unwrap_or_default();
    Ok(Json(json!({
        "engine": st.engine.settings(),
        "detector": st.engine.detector.settings(),
        "notifications": ns,
        "span_model": st.db().setting::<SpanSelection>("span_model")?.unwrap_or_default(),
        "onboarding": onboarding,
        "consents": st.db().consents()?,
    })))
}

async fn put_settings(State(st): St, Json(v): Json<Value>) -> R {
    if let Some(e) = v.get("engine") {
        let mut cur = serde_json::to_value(st.engine.settings())?;
        merge(&mut cur, e);
        let s: pv_proxy::Settings = serde_json::from_value(cur)?;
        let before = st.engine.settings();
        if s.fallback.tee_consent && !before.fallback.tee_consent {
            st.db().record_consent("tee_private_route", true, &json!({"source": "settings"}), Some(&st.engine.policy_hash()))?;
        }
        if !s.fallback.tee_consent && before.fallback.tee_consent {
            st.db().record_consent("tee_private_route", false, &json!({"source": "settings"}), None)?;
        }
        if s.retention_days == 0 || s.pending_ttl_minutes <= 0 {
            return Err(bad("retention and pending TTL must be positive"));
        }
        state::save_settings(&st, &s, "owner")?;
    }
    if let Some(d) = v.get("detector") {
        let mut cur = serde_json::to_value(st.engine.detector.settings())?;
        merge(&mut cur, d);
        let s: DetectorSettings = serde_json::from_value(cur)?;
        st.db().set_setting("detector", &s, "owner")?;
        st.engine.detector.set_settings(s);
    }
    if let Some(n) = v.get("notifications") {
        let mut cur = serde_json::to_value(st.notify_settings.settings.lock().unwrap().clone())?;
        merge(&mut cur, n);
        let s: NotificationSettings = serde_json::from_value(cur)?;
        st.db().set_setting("notifications", &s, "owner")?;
        *st.notify_settings.settings.lock().unwrap() = s;
    }
    owner_event(&st, "settings", "SETTINGS_EDIT", "Settings updated", vec![], Sensitivity::Low);
    get_settings(State(st)).await
}

fn merge(a: &mut Value, b: &Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (a, b) => *a = b.clone(),
    }
}

async fn onboarding(State(st): St) -> R {
    let s: OnboardingState = st.db().setting("onboarding")?.unwrap_or_default();
    let sel: SpanSelection = st.db().setting("span_model")?.unwrap_or_default();
    Ok(Json(json!({
        "state": s,
        "categories": category::BUILTIN,
        "defaults": default_category_choices(),
        "span_model": sel,
        "span_options": st.models.status()["options"],
        "builtin_labels": builtin_labels(),
        "ollama": ollama_status().await,
        "near_default_base": pv_provider::near::DEFAULT_NEAR_BASE,
        "spec_preferred_model": pv_provider::near::SPEC_PREFERRED_MODEL,
        "near_default_model": pv_provider::near::DEFAULT_NEAR_MODEL,
        "base_url": st.base_url(),
        "platform": crate::models::os_name(),
    })))
}

#[derive(Deserialize, Default)]
pub struct OnboardingPayload {
    #[serde(default)]
    pub categories: std::collections::BTreeMap<String, Action>,
    #[serde(default)]
    pub critical_exceptions: Vec<CriticalException>,
    pub fallback_mode: FallbackMode,
    #[serde(default)]
    pub tee_consent: bool,
    #[serde(default)]
    pub local_private_handling: bool,
    /// Span model size to install: edge | small | base. None = skip (deterministic only).
    pub span_model: Option<String>,
    #[serde(default)]
    pub near: NearSetup,
    pub ollama_generation: Option<OllamaGen>,
    pub custom: Option<CustomEndpoint>,
    pub default_destination: Option<String>,
    pub notifications: Option<bool>,
    #[serde(default)]
    pub secrets: Vec<SecretSetup>,
    #[serde(default)]
    pub entities: Vec<AddEntity>,
}

#[derive(Deserialize)]
pub struct CriticalException {
    pub category: String,
    pub action: Action,
    pub confirm: String,
}

#[derive(Deserialize, Default)]
pub struct NearSetup {
    #[serde(default)]
    pub skip: bool,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Deserialize)]
pub struct OllamaGen {
    pub model: String,
    pub url: Option<String>,
}

#[derive(Deserialize)]
pub struct CustomEndpoint {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub allow_failover: bool,
    pub display_name: Option<String>,
    /// The owner acknowledged this endpoint is EXTERNAL_UNVERIFIED.
    #[serde(default)]
    pub acknowledged_privacy_tier: bool,
}

#[derive(Deserialize)]
pub struct SecretSetup {
    pub label: String,
    pub value: String,
    #[serde(default = "default_block")]
    pub action: Action,
}

pub async fn complete_onboarding_inner(st: &AppState, p: OnboardingPayload) -> Result<Value, ApiErr> {
    // 0. Validate everything that can be rejected before writing anything.
    let mut seen = Vec::new();
    for e in &p.entities {
        seen.push(check_entity_label(st, &e.label, &seen)?);
    }
    for sec in &p.secrets {
        pv_detectors::exact::validate_entry(ExactKind::Value, &sec.value, default_norm()).map_err(|e| bad(format!("secret '{}': {e}", sec.label)))?;
    }
    for e in &p.critical_exceptions {
        if e.confirm != format!("DISABLE {}", e.category) {
            return Err(ApiErr(StatusCode::PRECONDITION_REQUIRED, format!("critical exception for {} needs confirm = \"DISABLE {}\"", e.category, e.category)));
        }
    }
    if let Some(c) = &p.custom {
        if !c.acknowledged_privacy_tier {
            return Err(bad("custom endpoints are EXTERNAL_UNVERIFIED; acknowledge the privacy tier to add one"));
        }
        url_ok(&c.base_url)?;
    }
    if let Some(m) = &p.span_model {
        crate::models::resolve_name(m).ok_or_else(|| bad("span_model must be edge, small or base"))?;
    }
    // 1. Category choices (noncritical) and deliberate critical exceptions.
    for (cat, action) in &p.categories {
        if category::is_critical(cat) {
            continue;
        }
        let cur = st.engine.policy().rules.iter().find(|r| r.selector.categories == vec![cat.clone()] && r.origin == "builtin").map(|r| r.action);
        if cur != Some(*action) {
            set_category_inner(st, cat, *action, None, "onboarding")?;
        }
    }
    for e in &p.critical_exceptions {
        set_category_inner(st, &e.category, e.action, Some(&e.confirm), "onboarding")?;
    }
    // 2. Destinations.
    let mut providers_made = vec![];
    if !p.near.skip {
        if let Some(model) = &p.near.model {
            let cfg = ProviderConfig {
                schema_version: 1,
                id: "near".into(),
                kind: ProviderKind::NearAi,
                display_name: "NEAR AI".into(),
                base_url: p.near.base_url.clone().unwrap_or_else(|| pv_provider::near::DEFAULT_NEAR_BASE.into()),
                default_model: model.clone(),
                declared_tier: DestinationTier::AttestedTee,
                credential_ref: Some("provider:near".into()),
                enabled: true,
                allow_failover: true,
                supports_responses: true,
                timeout_secs: 180,
            };
            put_provider_inner(st, cfg, p.near.api_key.clone())?;
            providers_made.push("near");
            st.db().record_consent("near_inference", true, &json!({"model": model}), None)?;
        }
    }
    if let Some(o) = &p.ollama_generation {
        let cfg = ProviderConfig {
            schema_version: 1,
            id: "ollama".into(),
            kind: ProviderKind::Ollama,
            display_name: format!("Ollama ({})", o.model),
            base_url: o.url.clone().unwrap_or_else(|| "http://127.0.0.1:11434/v1".into()),
            default_model: o.model.clone(),
            declared_tier: DestinationTier::Local,
            credential_ref: None,
            enabled: true,
            allow_failover: true,
            supports_responses: false,
            timeout_secs: 300,
        };
        put_provider_inner(st, cfg, None)?;
        providers_made.push("ollama");
    }
    if let Some(c) = &p.custom {
        if !c.acknowledged_privacy_tier {
            return Err(bad("custom endpoints are EXTERNAL_UNVERIFIED; acknowledge the privacy tier to add one"));
        }
        let cfg = ProviderConfig {
            schema_version: 1,
            id: "custom".into(),
            kind: ProviderKind::OpenaiCompatible,
            display_name: c.display_name.clone().unwrap_or_else(|| "Custom endpoint".into()),
            base_url: c.base_url.clone(),
            default_model: c.model.clone(),
            declared_tier: DestinationTier::ExternalUnverified,
            credential_ref: None,
            enabled: true,
            allow_failover: c.allow_failover,
            supports_responses: false,
            timeout_secs: 180,
        };
        put_provider_inner(st, cfg, c.api_key.clone())?;
        providers_made.push("custom");
        st.db().record_consent("custom_endpoint_external_unverified", true, &json!({"base_url": c.base_url}), None)?;
    }
    // 3. Engine settings: fallback (consent never assumed), routing.
    let mut s = st.engine.settings();
    s.fallback.mode = p.fallback_mode;
    s.fallback.tee_consent = p.fallback_mode == FallbackMode::TeePrivateRoute && p.tee_consent;
    s.fallback.local_private_handling = p.local_private_handling;
    if s.fallback.tee_consent && providers_made.contains(&"near") {
        s.private_destination = Some("near".into());
        s.private_model = p.near.model.clone();
    }
    // Re-running onboarding keeps unrelated configuration: the existing default
    // and failover entries survive unless replaced.
    s.default_destination = p.default_destination.clone().or_else(|| providers_made.first().map(|x| x.to_string())).or(s.default_destination.clone());
    let mut fo: Vec<String> = s.failover.clone();
    for x in ["near", "ollama", "custom"] {
        if providers_made.contains(&x) && !fo.iter().any(|f| f == x) {
            fo.push(x.to_string());
        }
    }
    let known: Vec<String> = st.engine.registry.providers().into_iter().map(|p| p.id).collect();
    fo.retain(|f| Some(f.clone()) != s.default_destination && known.contains(f));
    s.failover = fo;
    state::save_settings(st, &s, "onboarding")?;
    st.db().record_consent("tee_private_route", s.fallback.tee_consent, &json!({"mode": format!("{:?}", s.fallback.mode)}), Some(&st.engine.policy_hash()))?;
    // 4. Span model (download + verify + load). Failure is reported, not fatal.
    let mut span_status = json!(null);
    if let Some(m) = &p.span_model {
        span_status = match install_model_inner(st, Some(m)).await {
            Ok(v) => v,
            Err(e) => json!({"error": e.1}),
        };
    }
    // 5. Notifications.
    if let Some(on) = p.notifications {
        let mut ns = st.notify_settings.settings.lock().unwrap().clone();
        ns.enabled = on;
        st.db().set_setting("notifications", &ns, "onboarding")?;
        *st.notify_settings.settings.lock().unwrap() = ns;
    }
    // 6. Optional exact values and custom entity types.
    for sec in p.secrets {
        add_secret_inner(st, AddSecretInput { kind: ExactKind::Value, label: sec.label, value: sec.value, norm: default_norm(), action: sec.action, allow: false })?;
    }
    for e in p.entities {
        add_entity_inner(st, e)?;
    }
    // 7. Ensure a default app key exists.
    let mut new_key = None;
    if st.db().apps()?.iter().all(|a| a.revoked_at.is_some()) {
        let (rec, key) = state::create_app(st, "default", None, None)?;
        let _ = state::write_private(&st.paths.app_key("default"), key.as_bytes());
        let _ = state::write_private(&st.paths.app_key(&rec.app_id), key.as_bytes());
        new_key = Some(key);
    }
    let ob = OnboardingState { completed: true, completed_at: Some(Utc::now().to_rfc3339()), version: 1, near_skipped: p.near.skip || !providers_made.contains(&"near") };
    st.db().set_setting("onboarding", &ob, "onboarding")?;
    owner_event(st, "settings", "ONBOARDING_COMPLETE", &format!("Onboarding saved (policy {})", &st.engine.policy_hash()[..19]), vec![], Sensitivity::Low);
    Ok(json!({"ok": true, "policy_hash": st.engine.policy_hash(), "base_url": st.base_url(), "api_key": new_key, "near_status": if ob.near_skipped { "NEAR_NOT_CONFIGURED" } else { "CONFIGURED" }, "span_model": span_status}))
}

async fn complete_onboarding(State(st): St, Json(p): Json<OnboardingPayload>) -> R {
    Ok(Json(complete_onboarding_inner(&st, p).await?))
}

#[derive(Deserialize)]
struct Stop {
    engaged: bool,
}

async fn emergency_stop(State(st): St, Json(s): Json<Stop>) -> R {
    let mut cur = st.engine.settings();
    cur.emergency_stop = s.engaged;
    state::save_settings(&st, &cur, "owner")?;
    owner_event(&st, "settings", if s.engaged { "EMERGENCY_STOP_ON" } else { "EMERGENCY_STOP_OFF" }, if s.engaged { "Emergency stop engaged: all model traffic blocked" } else { "Emergency stop released" }, vec![], Sensitivity::High);
    Ok(Json(json!({"emergency_stop": s.engaged})))
}

// ------------------------------------------------------------------ health

pub async fn health_report(st: &AppState) -> Value {
    let s = st.engine.settings();
    let mut warnings: Vec<String> = vec![];
    let v = st.vault().clone();
    let chain = st.db().verify_chain(&|k| v.audit_mac_key(k));
    if chain.is_err() {
        warnings.push("audit chain verification failed".into());
    }
    if !st.vault().backend_secure() {
        warnings.push("data key is stored in a file (degraded); OS keychain unavailable or disabled".into());
    }
    let providers = st.engine.registry.providers();
    let near_cfg = providers.iter().find(|p| p.kind == ProviderKind::NearAi);
    let near_status = match near_cfg {
        None => "NEAR_NOT_CONFIGURED".to_string(),
        Some(p) => {
            let has_key = p.credential_ref.as_deref().and_then(|r| st.creds().get(r)).is_some();
            if !has_key {
                "NEAR_NO_API_KEY".into()
            } else {
                "NEAR_CONFIGURED".into()
            }
        }
    };
    if near_status != "NEAR_CONFIGURED" {
        warnings.push(format!("{near_status}: NEAR inference and TEE private routing are unavailable"));
    }
    if s.default_destination.is_none() {
        warnings.push("no default generation destination configured".into());
    }
    if s.emergency_stop {
        warnings.push("emergency stop engaged".into());
    }
    let span = st.models.span.lock().unwrap().clone();
    if !span.loaded {
        if span.provisioned {
            warnings.push(format!("detection model installed but not loaded; requests fail closed to review: {}", span.error.clone().unwrap_or_default()));
        } else {
            warnings.push("detection model (GLiNER) not installed: only deterministic recognizers run; run `promptvpn model install`".into());
        }
    }
    let pending = st.db().list_pending(false).map(|v| v.len()).unwrap_or(0);
    json!({
        "status": if warnings.is_empty() { "ok" } else { "degraded" },
        "version": state::VERSION,
        "uptime_secs": (Utc::now() - st.started).num_seconds(),
        "base_url": st.base_url(),
        "database": {"schema_version": st.db().schema_version().ok(), "integrity_ok": st.db().integrity_ok()},
        "vault": {"backend": st.vault().backend_name(), "secure": st.vault().backend_secure(), "active_key": st.vault().active_key_id()},
        "audit_chain": match chain { Ok(n) => json!({"ok": true, "events": n}), Err(e) => json!({"ok": false, "error": e.to_string()}) },
        "near": {"status": near_status, "verifications": st.engine.registry.cached_verifications()},
        "span": span,
        "destinations": providers.iter().map(|p| json!({"id": p.id, "kind": p.kind, "declared_tier": p.declared_tier, "enabled": p.enabled})).collect::<Vec<_>>(),
        "default_destination": s.default_destination,
        "fallback": s.fallback,
        "pending_approvals": pending,
        "emergency_stop": s.emergency_stop,
        "coverage": "Inspects only requests sent to this OpenAI-compatible API (including tool-call arguments and tool results inside them). It does not intercept separately executed shell, HTTP or browser tool calls.",
        "warnings": warnings,
    })
}

async fn health(State(st): St) -> R {
    Ok(Json(health_report(&st).await))
}

async fn rotate(State(st): St) -> R {
    let k = st.vault().rotate()?;
    owner_event(&st, "vault", "KEY_ROTATE", &format!("Vault data key rotated to {k}"), vec![], Sensitivity::Moderate);
    Ok(Json(json!({"active_key": k})))
}

#[derive(Deserialize)]
struct Recovery {
    passphrase: String,
}

async fn recovery(State(st): St, Json(r): Json<Recovery>) -> R {
    let b = st.vault().export_recovery(&r.passphrase)?;
    owner_event(&st, "vault", "RECOVERY_EXPORT", "Recovery bundle exported", vec![], Sensitivity::High);
    Ok(Json(json!({"bundle": serde_json::from_str::<Value>(&b)?})))
}

async fn integrations(State(st): St) -> R {
    Ok(Json(json!({"integrations": st.db().list_json::<ClientIntegration>(JsonTable::Integrations)?})))
}

async fn add_integration(State(st): St, Json(i): Json<ClientIntegration>) -> R {
    st.db().put_json(JsonTable::Integrations, &i.id, &i)?;
    owner_event(&st, "integration", if i.rolled_back_at.is_some() { "CLIENT_ROLLBACK" } else { "CLIENT_CONFIGURE" }, &format!("Client {} config {}", i.client, if i.rolled_back_at.is_some() { "rolled back" } else { "pointed at Prompt VPN" }), vec![], Sensitivity::Low);
    Ok(Json(json!({"ok": true})))
}

async fn recent_notifications(State(st): St) -> R {
    let v = st.recorder.sent.lock().unwrap();
    let recent: Vec<_> = v.iter().rev().take(50).cloned().collect();
    Ok(Json(json!({"notifications": recent})))
}

async fn schemas() -> R {
    let m: serde_json::Map<String, Value> = pv_core::schema::all_schemas().into_iter().map(|(n, s)| (n.to_string(), serde_json::from_str(&s).unwrap())).collect();
    Ok(Json(Value::Object(m)))
}

async fn shutdown(State(st): St) -> R {
    st.shutdown.notify_waiters();
    Ok(Json(json!({"ok": true})))
}
