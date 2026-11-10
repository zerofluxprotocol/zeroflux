//! Deterministic policy engine.
//!
//! Decisions depend only on findings, the immutable policy set, the request
//! context and owner settings. No model output or prompt text can change a rule.

use crate::category;
use crate::types::*;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Action {
    Allow,
    Tokenize,
    Redact,
    Generalize,
    PrivateRoute,
    RequireApproval,
    Block,
    /// Resolves to `PRIVATE_ROUTE` (verified TEE, with consent) or to the
    /// block-and-review queue, according to the owner's fallback setting.
    Fallback,
}

impl Action {
    /// Higher means less data exposure. Used to stop lower scopes weakening rules.
    pub fn strictness(self) -> u8 {
        match self {
            Action::Allow => 0,
            Action::PrivateRoute => 1,
            Action::Fallback => 1,
            Action::Generalize => 2,
            Action::Tokenize => 3,
            Action::Redact => 4,
            Action::RequireApproval => 5,
            Action::Block => 6,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Action::Allow => "ALLOW",
            Action::Tokenize => "TOKENIZE",
            Action::Redact => "REDACT",
            Action::Generalize => "GENERALIZE",
            Action::PrivateRoute => "PRIVATE_ROUTE",
            Action::RequireApproval => "REQUIRE_APPROVAL",
            Action::Block => "BLOCK",
            Action::Fallback => "FALLBACK",
        }
    }

    pub fn is_transform(self) -> bool {
        matches!(self, Action::Tokenize | Action::Redact | Action::Generalize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OverrideMode {
    /// Lower scopes may replace this rule with any action.
    #[default]
    Allowed,
    /// Lower scopes may not weaken it; a one-time owner approval may.
    ApprovalOnce,
    /// Only the policy owner may change it (by editing the rule itself).
    PolicyOwnerOnly,
    /// Nothing weakens it: not lower scopes, not approvals.
    Forbidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScopeKind {
    /// Reserved for later releases; not evaluated in v1.
    Organization,
    /// Reserved for later releases; not evaluated in v1.
    Team,
    User,
    Agent,
    Application,
    Request,
}

impl ScopeKind {
    /// Larger is more specific. Organization/team are reserved and unused.
    pub fn level(self) -> u8 {
        match self {
            ScopeKind::Organization => 0,
            ScopeKind::Team => 1,
            ScopeKind::User => 2,
            ScopeKind::Agent => 3,
            ScopeKind::Application => 4,
            ScopeKind::Request => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Scope {
    pub kind: ScopeKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Selector {
    /// Category IDs. `*` matches all; `CUSTOM:*` all custom entity types; `EXACT_SECRET:<id>` one secret.
    pub categories: Vec<String>,
    /// `any`, `external` (any remote), `local`, `attested_tee`, `external_unverified`, or `dest:<id>`.
    #[serde(default = "default_destinations")]
    pub destinations: Vec<String>,
    #[serde(default)]
    pub direction: Direction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f32>,
}

fn default_destinations() -> Vec<String> {
    vec!["any".into()]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyRule {
    #[serde(default = "policy_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub version: u32,
    pub scope: Scope,
    pub selector: Selector,
    pub action: Action,
    pub severity: Sensitivity,
    #[serde(default)]
    pub notify: Vec<String>,
    #[serde(rename = "override", default)]
    pub override_mode: OverrideMode,
    #[serde(default = "default_retention")]
    pub retention_days: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    /// `builtin`, `owner`, or `onboarding`.
    #[serde(default)]
    pub origin: String,
    /// True when the owner deliberately relaxed a critical default.
    #[serde(default)]
    pub critical_exception: bool,
}

fn policy_schema_version() -> u32 {
    POLICY_SCHEMA_VERSION
}
fn default_retention() -> u32 {
    30
}
fn default_true() -> bool {
    true
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PolicyError {
    #[error("rule {0}: {1}")]
    Invalid(String, String),
}

impl PolicyRule {
    pub fn validate(&self) -> Result<(), PolicyError> {
        let bad = |m: &str| Err(PolicyError::Invalid(self.id.clone(), m.to_string()));
        if self.id.is_empty() || self.id.len() > 128 {
            return bad("id must be 1..128 chars");
        }
        if !self.id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c)) {
            return bad("id may contain only [A-Za-z0-9-_.:]");
        }
        if self.version == 0 {
            return bad("version must be >= 1");
        }
        if self.selector.categories.is_empty() {
            return bad("selector.categories must not be empty");
        }
        if matches!(self.scope.kind, ScopeKind::Organization | ScopeKind::Team) {
            return bad("organization/team scopes are reserved and not supported in v1");
        }
        for d in &self.selector.destinations {
            let ok = matches!(d.as_str(), "any" | "*" | "external" | "remote" | "local" | "attested_tee" | "external_unverified")
                || d.starts_with("dest:");
            if !ok {
                return bad(&format!("unknown destination selector {d}"));
            }
        }
        if let Some(c) = self.selector.min_confidence {
            if !(0.0..=1.0).contains(&c) {
                return bad("min_confidence must be within [0,1]");
            }
        }
        Ok(())
    }

    fn category_specificity(&self, finding: &Finding) -> Option<u8> {
        let mut best: Option<u8> = None;
        for c in &self.selector.categories {
            let s = if c == "*" {
                Some(0)
            } else if c == "CUSTOM:*" && finding.category.starts_with(category::CUSTOM_PREFIX) {
                Some(1)
            } else if *c == finding.category {
                Some(2)
            } else if let (Some(id), Some(sid)) = (c.strip_prefix("EXACT_SECRET:"), finding.secret_id.as_deref()) {
                (finding.category == category::EXACT_SECRET && id == sid).then_some(3)
            } else {
                None
            };
            if let Some(s) = s {
                best = Some(best.map_or(s, |b: u8| b.max(s)));
            }
        }
        best
    }

    fn destination_matches(&self, dest: &DestinationRef) -> bool {
        self.selector.destinations.iter().any(|d| match d.as_str() {
            "any" | "*" => true,
            "external" | "remote" => dest.tier.is_remote(),
            "local" => dest.tier == DestinationTier::Local,
            "attested_tee" => dest.tier == DestinationTier::AttestedTee,
            "external_unverified" => dest.tier == DestinationTier::ExternalUnverified,
            other => other.strip_prefix("dest:") == Some(dest.id.as_str()),
        })
    }

    fn direction_matches(&self, dir: Direction) -> bool {
        matches!(
            (self.selector.direction, dir),
            (Direction::Both, _) | (Direction::Inbound, Direction::Inbound) | (Direction::Outbound, Direction::Outbound)
        )
    }

    fn scope_applies(&self, ctx: &EvalContext) -> bool {
        let id = self.scope.id.as_str();
        match self.scope.kind {
            ScopeKind::User => id == ctx.user_id || id == "*",
            ScopeKind::Agent => ctx.agent_id.as_deref() == Some(id),
            ScopeKind::Application => ctx.app_id.as_deref() == Some(id),
            ScopeKind::Request => ctx.request_id.as_deref() == Some(id),
            ScopeKind::Organization | ScopeKind::Team => false,
        }
    }
}

/// An immutable, versioned set of active rules.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PolicySet {
    pub rules: Vec<PolicyRule>,
}

impl PolicySet {
    pub fn validate(&self) -> Result<(), PolicyError> {
        let mut seen = std::collections::HashSet::new();
        for r in &self.rules {
            r.validate()?;
            if !seen.insert(r.id.clone()) {
                return Err(PolicyError::Invalid(r.id.clone(), "duplicate active rule id".into()));
            }
        }
        Ok(())
    }

    /// Stable hash over every active rule (sorted by id) and the fallback settings.
    pub fn effective_hash(&self, fallback: &FallbackSettings) -> String {
        let mut rules: Vec<&PolicyRule> = self.rules.iter().collect();
        rules.sort_by(|a, b| a.id.cmp(&b.id));
        let canon = serde_json::json!({ "rules": rules, "fallback": fallback });
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(&canon).expect("serializable"));
        format!("sha256:{}", hex::encode(h.finalize()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FallbackMode {
    /// Route uncertain content to a verified NEAR TEE model (only with consent).
    TeePrivateRoute,
    /// Stop locally, notify, and queue for owner review.
    #[default]
    BlockAndReview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct FallbackSettings {
    pub mode: FallbackMode,
    /// Informed onboarding consent to send uncertain content to an attested TEE.
    pub tee_consent: bool,
    /// Explicit owner choice allowing a local Ollama generation model to receive
    /// content that otherwise requires TEE confidentiality.
    #[serde(default)]
    pub local_private_handling: bool,
    /// Noncritical findings below this confidence are treated as uncertain.
    #[serde(default = "default_uncertain")]
    pub uncertain_threshold: f32,
}

fn default_uncertain() -> f32 {
    0.5
}

/// A one-time owner approval presented with a resubmitted request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalGrant {
    pub approval_id: String,
    pub request_hash: String,
    pub destination_id: String,
    pub policy_hash: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct EvalContext<'a> {
    pub user_id: &'a str,
    pub agent_id: Option<String>,
    pub app_id: Option<String>,
    pub request_id: Option<String>,
    pub destination: DestinationRef,
    pub direction: Direction,
    pub fallback: FallbackSettings,
    /// Set only after the proxy has matched hash, destination, policy and expiry.
    pub approval: Option<ApprovalGrant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Disposition {
    Allow,
    Transform,
    PrivateRoute,
    RequireApproval,
    Block,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindingDecision {
    pub finding_id: String,
    pub category: String,
    pub action: Action,
    pub policy_id: Option<String>,
    pub policy_version: Option<u32>,
    pub override_mode: OverrideMode,
    pub approved: bool,
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    pub schema_version: u32,
    pub disposition: Disposition,
    pub findings: Vec<FindingDecision>,
    pub effective_policy_hash: String,
    pub destination: DestinationRef,
    pub direction: Direction,
    pub severity: Sensitivity,
    pub notify: bool,
    pub explanations: Vec<String>,
    /// True if an owner approval was consumed to reach this decision.
    pub used_approval: bool,
}

impl Decision {
    pub fn action_for(&self, finding_id: &str) -> Option<Action> {
        self.findings.iter().find(|f| f.finding_id == finding_id).map(|f| f.action)
    }

    pub fn categories(&self) -> Vec<String> {
        let mut c: Vec<String> = self.findings.iter().map(|f| f.category.clone()).collect();
        c.sort();
        c.dedup();
        c
    }

    pub fn policy_ids(&self) -> Vec<String> {
        let mut c: Vec<String> = self.findings.iter().filter_map(|f| f.policy_id.clone()).collect();
        c.sort();
        c.dedup();
        c
    }
}

struct Candidate<'r> {
    rule: &'r PolicyRule,
    level: u8,
    specificity: u8,
}

pub fn evaluate(set: &PolicySet, findings: &[Finding], ctx: &EvalContext) -> Decision {
    let hash = set.effective_hash(&ctx.fallback);
    let mut out = Vec::with_capacity(findings.len());
    let mut explanations = Vec::new();
    let mut used_approval = false;

    for f in findings {
        let fd = decide_finding(set, f, ctx);
        if fd.approved {
            used_approval = true;
        }
        out.push(fd);
    }

    let mut disposition = Disposition::Allow;
    let rank = |d: Disposition| match d {
        Disposition::Allow => 0,
        Disposition::Transform => 1,
        Disposition::PrivateRoute => 2,
        Disposition::RequireApproval => 3,
        Disposition::Block => 4,
    };
    for fd in &out {
        let d = match fd.action {
            Action::Allow => Disposition::Allow,
            Action::Tokenize | Action::Redact | Action::Generalize => Disposition::Transform,
            Action::PrivateRoute => Disposition::PrivateRoute,
            Action::RequireApproval => Disposition::RequireApproval,
            Action::Block => Disposition::Block,
            Action::Fallback => unreachable!("fallback is resolved in decide_finding"),
        };
        if rank(d) > rank(disposition) {
            disposition = d;
        }
        if fd.action != Action::Allow {
            explanations.push(format!("{}: {} ({})", fd.category, fd.action.as_str(), fd.explanation));
        }
    }
    // A private route to a destination that is already private needs no rerouting.
    if disposition == Disposition::PrivateRoute {
        let private_ok = ctx.destination.tier == DestinationTier::AttestedTee
            || (ctx.destination.tier == DestinationTier::Local && ctx.fallback.local_private_handling);
        if private_ok {
            let any_transform = out.iter().any(|f| f.action.is_transform());
            disposition = if any_transform { Disposition::Transform } else { Disposition::Allow };
            explanations.push(format!("destination {} already satisfies private routing", ctx.destination.id));
        }
    }

    let severity = findings
        .iter()
        .zip(out.iter())
        .filter(|(_, d)| d.action != Action::Allow)
        .map(|(f, _)| f.sensitivity)
        .max()
        .unwrap_or(Sensitivity::Low);

    Decision {
        schema_version: DECISION_SCHEMA_VERSION,
        disposition,
        notify: disposition != Disposition::Allow,
        findings: out,
        effective_policy_hash: hash,
        destination: ctx.destination.clone(),
        direction: ctx.direction,
        severity,
        explanations,
        used_approval,
    }
}

fn decide_finding(set: &PolicySet, f: &Finding, ctx: &EvalContext) -> FindingDecision {
    let mut cands: Vec<Candidate> = set
        .rules
        .iter()
        .filter(|r| r.enabled)
        .filter(|r| r.scope_applies(ctx))
        .filter(|r| r.direction_matches(ctx.direction))
        .filter(|r| r.destination_matches(&ctx.destination))
        .filter(|r| r.selector.min_confidence.is_none_or(|m| f.confidence >= m))
        .filter_map(|r| r.category_specificity(f).map(|s| Candidate { rule: r, level: r.scope.kind.level(), specificity: s }))
        .collect();

    // Most specific scope first, then most specific selector, then strictest action, then id.
    cands.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then(b.specificity.cmp(&a.specificity))
            .then(b.rule.action.strictness().cmp(&a.rule.action.strictness()))
            .then(a.rule.id.cmp(&b.rule.id))
    });

    let (mut action, mut governing, mut explanation) = match cands.first() {
        Some(c) => (c.rule.action, Some(c.rule), format!("rule {} v{}", c.rule.id, c.rule.version)),
        None => {
            // No rule: critical data never leaves by accident; everything else passes.
            if f.sensitivity == Sensitivity::Critical && ctx.destination.tier.is_remote() && ctx.direction == Direction::Inbound {
                (Action::Block, None, "no matching rule; critical data blocked by default".to_string())
            } else {
                (Action::Allow, None, "no matching rule".to_string())
            }
        }
    };

    // Inherited non-overridable rules from less specific scopes clamp the action upward.
    if let Some(top) = cands.first() {
        for c in cands.iter().skip(1) {
            if c.level < top.level
                && c.rule.override_mode != OverrideMode::Allowed
                && c.rule.action.strictness() > action.strictness()
            {
                action = c.rule.action;
                governing = Some(c.rule);
                explanation = format!(
                    "inherited non-overridable rule {} v{} ({:?}) prevents weakening",
                    c.rule.id, c.rule.version, c.rule.override_mode
                );
            }
        }
    }

    // Uncertain noncritical findings follow the owner's fallback choice.
    let threshold = ctx.fallback.uncertain_threshold;
    if f.sensitivity != Sensitivity::Critical
        && f.confidence < threshold
        && action != Action::Allow
        && action != Action::Block
        && action != Action::RequireApproval
        && ctx.direction == Direction::Inbound
    {
        action = Action::Fallback;
        explanation = format!("{explanation}; confidence {:.2} below {:.2}, uncertain", f.confidence, threshold);
    }

    if action == Action::Fallback {
        action = resolve_fallback(&ctx.fallback);
        explanation = format!("{explanation}; fallback resolved to {}", action.as_str());
    }

    let override_mode = governing.map(|r| r.override_mode).unwrap_or(OverrideMode::Forbidden);
    let mut approved = false;
    if ctx.approval.is_some() && matches!(action, Action::RequireApproval | Action::Block) {
        let approvable = match action {
            Action::RequireApproval => override_mode != OverrideMode::Forbidden,
            Action::Block => override_mode == OverrideMode::ApprovalOnce,
            _ => false,
        };
        // A FORBIDDEN rule anywhere in the chain is never bypassed by approval.
        let forbidden_in_chain = cands
            .iter()
            .any(|c| c.rule.override_mode == OverrideMode::Forbidden && c.rule.action.strictness() >= Action::RequireApproval.strictness());
        if approvable && !forbidden_in_chain {
            action = Action::Allow;
            approved = true;
            explanation = format!("{explanation}; allowed once by owner approval");
        }
    }

    FindingDecision {
        finding_id: f.finding_id.clone(),
        category: f.category.clone(),
        action,
        policy_id: governing.map(|r| r.id.clone()),
        policy_version: governing.map(|r| r.version),
        override_mode,
        approved,
        explanation,
    }
}

/// Private routing needs either informed TEE consent (data leaves the device to
/// a verified enclave) or explicit local-only handling (data stays on device).
/// Block-and-review always queues.
pub fn resolve_fallback(s: &FallbackSettings) -> Action {
    match s.mode {
        FallbackMode::TeePrivateRoute if s.tee_consent || s.local_private_handling => Action::PrivateRoute,
        _ => Action::RequireApproval,
    }
}

/// Per-category owner choice for noncritical built-in data.
pub type CategoryChoices = BTreeMap<String, Action>;

pub fn default_category_choices() -> CategoryChoices {
    let mut m = BTreeMap::new();
    m.insert(category::EMAIL.into(), Action::Tokenize);
    m.insert(category::PHONE.into(), Action::Tokenize);
    m.insert(category::ADDRESS.into(), Action::Tokenize);
    m.insert(category::DOB.into(), Action::Tokenize);
    m.insert(category::HEALTH_INFORMATION.into(), Action::Tokenize);
    m.insert(category::FINANCIAL_ACCOUNT.into(), Action::Redact);
    m.insert(category::PERSON.into(), Action::Allow);
    m.insert(category::PUBLIC_INFORMATION.into(), Action::Allow);
    m
}

fn rule(id: &str, cats: &[&str], dests: &[&str], dir: Direction, action: Action, sev: Sensitivity, ov: OverrideMode, desc: &str) -> PolicyRule {
    PolicyRule {
        schema_version: POLICY_SCHEMA_VERSION,
        id: id.into(),
        version: 1,
        scope: Scope { kind: ScopeKind::User, id: "local-owner".into() },
        selector: Selector {
            categories: cats.iter().map(|s| s.to_string()).collect(),
            destinations: dests.iter().map(|s| s.to_string()).collect(),
            direction: dir,
            min_confidence: None,
        },
        action,
        severity: sev,
        notify: vec!["local_user".into()],
        override_mode: ov,
        retention_days: 30,
        enabled: true,
        description: desc.into(),
        origin: "builtin".into(),
        critical_exception: false,
    }
}

/// Default built-in policy. Critical categories are blocked for every remote
/// destination (including attested TEEs) and cannot be approved past.
pub fn default_policy(choices: &CategoryChoices) -> PolicySet {
    let mut rules = Vec::new();
    for c in category::critical_ids() {
        rules.push(rule(
            &format!("builtin-critical-{}", c.to_lowercase().replace('_', "-")),
            &[c],
            &["external"],
            Direction::Inbound,
            Action::Block,
            Sensitivity::Critical,
            OverrideMode::Forbidden,
            &format!("Block {} before any remote destination", category::info(c).map(|i| i.label).unwrap_or(c)),
        ));
    }
    for info in category::BUILTIN.iter().filter(|c| !c.critical) {
        let action = choices.get(info.id).copied().unwrap_or(Action::Allow);
        rules.push(rule(
            &format!("builtin-{}", info.id.to_lowercase().replace('_', "-")),
            &[info.id],
            &["external"],
            Direction::Inbound,
            action,
            info.sensitivity,
            OverrideMode::Allowed,
            &format!("{} for {}", action.as_str(), info.label),
        ));
    }
    rules.push(rule("builtin-exact-secrets", &[category::EXACT_SECRET], &["external"], Direction::Inbound, Action::Block, Sensitivity::Critical, OverrideMode::PolicyOwnerOnly, "Block registered exact values before any remote destination"));
    rules.push(rule("builtin-custom-entities", &["CUSTOM:*"], &["external"], Direction::Inbound, Action::Tokenize, Sensitivity::High, OverrideMode::PolicyOwnerOnly, "Owner-defined entity types are replaced with reversible placeholders unless the entity's own rule says otherwise"));
    rules.push(rule("builtin-uncertain", &[category::UNCERTAIN], &["external"], Direction::Inbound, Action::Fallback, Sensitivity::High, OverrideMode::PolicyOwnerOnly, "Uncertain findings follow the fallback choice"));
    rules.push(rule("builtin-classifier-failure", &[category::CLASSIFIER_FAILURE], &["external"], Direction::Inbound, Action::Fallback, Sensitivity::High, OverrideMode::PolicyOwnerOnly, "Classifier failure follows the fallback choice; never silently allowed"));
    rules.push(rule("builtin-unsupported-content", &[category::UNSUPPORTED_CONTENT], &["external"], Direction::Inbound, Action::Block, Sensitivity::High, OverrideMode::PolicyOwnerOnly, "Content the proxy cannot inspect is blocked"));
    // Outbound: model output that contains critical values is redacted before release.
    let mut crit: Vec<&str> = category::critical_ids();
    crit.push(category::EXACT_SECRET);
    rules.push(rule("builtin-outbound-critical", &crit, &["any"], Direction::Outbound, Action::Redact, Sensitivity::Critical, OverrideMode::PolicyOwnerOnly, "Redact critical values generated by the model"));
    PolicySet { rules }
}
