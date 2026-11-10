//! The request pipeline: authenticate → extract → classify → decide → route →
//! transform → send → inspect output → restore → audit.

use crate::fields::{self, Field};
use crate::notify::{Notification, Notifier};
use crate::output::{OutCtx, OutStats};
use crate::protocol;
use crate::sse::{data_line, ChatStream, ResponsesStream, SseParser};
use bytes::Bytes;
use chrono::{Duration, Utc};
use futures::stream::BoxStream;
use futures::StreamExt;
use pv_audit::Db;
use pv_core::category::{self, UNSUPPORTED_CONTENT};
use pv_core::*;
use pv_detectors::{DetectionReport, Detector};
use pv_provider::{Endpoint, ProviderError, Registry, Resolved};
use pv_transform::{TokenStore, TransformError};
use pv_vault::Vault;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub owner_id: String,
    pub fallback: FallbackSettings,
    /// Provider used for normal requests (NEAR by default when configured).
    pub default_destination: Option<String>,
    /// NEAR provider and explicitly selected verified model for private routes.
    pub private_destination: Option<String>,
    pub private_model: Option<String>,
    /// Ordered alternates tried on provider failure (each must allow failover).
    pub failover: Vec<String>,
    pub mapping_ttl_minutes: i64,
    pub pending_ttl_minutes: i64,
    pub retention_days: u32,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    /// Also run the span model on model output (forces full buffering of streams).
    pub outbound_span_model: bool,
    pub restore_placeholders: bool,
    /// Owner kill switch: block all proxy traffic.
    pub emergency_stop: bool,
    /// Abort a stream when the upstream sends nothing for this long.
    #[serde(default = "default_idle_timeout")]
    pub stream_idle_timeout_secs: u64,
}

fn default_idle_timeout() -> u64 {
    120
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            owner_id: "local-owner".into(),
            fallback: FallbackSettings { mode: FallbackMode::BlockAndReview, tee_consent: false, local_private_handling: false, uncertain_threshold: 0.5 },
            default_destination: None,
            private_destination: None,
            private_model: None,
            failover: vec![],
            mapping_ttl_minutes: 24 * 60,
            pending_ttl_minutes: 15,
            retention_days: 30,
            max_request_bytes: 4 << 20,
            max_response_bytes: 8 << 20,
            outbound_span_model: false,
            restore_placeholders: true,
            emergency_stop: false,
            stream_idle_timeout_secs: default_idle_timeout(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// An OpenAI-compatible client calling /v1/*.
    Client,
    /// The authenticated owner resubmitting through the dashboard.
    Dashboard,
}

#[derive(Clone)]
pub struct Caller {
    pub app_id: String,
    pub agent_id: Option<String>,
    pub destination_override: Option<String>,
    pub conversation: Option<String>,
    pub source: Source,
    /// Mapping store for this request. `None` uses the local vault. The hosted
    /// gateway passes a fresh per-request store that is dropped afterwards.
    pub token_store: Option<Arc<dyn TokenStore>>,
    /// Multi-tenant hosted gateway: no review queue, raw payloads are never stored.
    pub hosted: bool,
}

impl Caller {
    pub fn client(app_id: impl Into<String>) -> Caller {
        Caller { app_id: app_id.into(), agent_id: None, destination_override: None, conversation: None, source: Source::Client, token_store: None, hosted: false }
    }
}

/// Store used for preprocessed (client-transformed) requests: the gateway must
/// never mint or resolve placeholders for them.
struct RefuseStore;

impl TokenStore for RefuseStore {
    fn token_for(&self, _: &str, _: &str, _: &str, _: Duration) -> Result<String, TransformError> {
        Err(TransformError::Store("preprocessed request still contains values that need transformation; the client must transform them".into()))
    }
    fn resolve_token(&self, _: &str, _: &str) -> Result<Option<zeroize::Zeroizing<String>>, TransformError> {
        Ok(None)
    }
}

pub enum Body {
    Json(Value),
    Stream(BoxStream<'static, Bytes>),
}

pub struct ProxyOutput {
    pub status: u16,
    pub body: Body,
    pub event_id: String,
    /// Route record, finalized before the first byte is released.
    pub route: Option<PrivacyRoute>,
    /// Non-error notices for headers, e.g. `ZFX_STREAM_SAFETY_BUFFERED`.
    pub notices: Vec<&'static str>,
}

pub struct Engine {
    pub db: Db,
    pub vault: Arc<Vault>,
    pub detector: Arc<Detector>,
    pub registry: Arc<Registry>,
    pub notifier: Arc<dyn Notifier>,
    policy: RwLock<Arc<PolicySet>>,
    settings: RwLock<Settings>,
}

fn sha(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(b)))
}

/// Human summary for the activity list; names categories, never values.
pub fn summarize(categories: &[String], action: &str) -> String {
    let names: Vec<String> = categories
        .iter()
        .filter(|c| c.as_str() != category::PUBLIC_INFORMATION)
        .map(|c| match category::info(c) {
            Some(i) => i.label.to_string(),
            None if c.starts_with(category::CUSTOM_PREFIX) => "Custom entity".into(),
            None if c == category::EXACT_SECRET => "Registered secret".into(),
            None if c == category::UNCERTAIN => "Possible sensitive data".into(),
            None if c == category::CLASSIFIER_FAILURE => "Classifier failure".into(),
            None if c == UNSUPPORTED_CONTENT => "Uninspectable content".into(),
            None => c.clone(),
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let what = if names.is_empty() { "No sensitive content".to_string() } else { names.join(", ") };
    match action {
        "ALLOW" if names.is_empty() => "No sensitive content detected".into(),
        "ALLOW" => format!("{what} detected and allowed by policy"),
        "TRANSFORM" => format!("{what} detected and transformed locally"),
        "BLOCK" => format!("{what} detected; request blocked locally"),
        "REQUIRE_APPROVAL" => format!("{what} detected; held for your review"),
        "PRIVATE_ROUTE" => format!("{what} detected; sent only to a verified private model"),
        "OUTPUT_TRANSFORM" => format!("{what} in model output redacted"),
        "OUTPUT_BLOCK" => format!("{what} in model output; response blocked"),
        _ => format!("{what}: {action}"),
    }
}

struct Ctx {
    caller: Caller,
    privacy: PrivacyRequest,
    store: Arc<dyn TokenStore>,
    endpoint: Endpoint,
    started: Instant,
    event_id: String,
    request_hash: String,
    scope: String,
    stream: bool,
    report: DetectionReport,
    fields: Vec<Field>,
    findings: Vec<Finding>,
    body: Value,
}

impl Engine {
    pub fn new(db: Db, vault: Arc<Vault>, detector: Arc<Detector>, registry: Arc<Registry>, notifier: Arc<dyn Notifier>, policy: PolicySet, settings: Settings) -> Self {
        Engine { db, vault, detector, registry, notifier, policy: RwLock::new(Arc::new(policy)), settings: RwLock::new(settings) }
    }

    pub fn policy(&self) -> Arc<PolicySet> {
        self.policy.read().unwrap().clone()
    }
    pub fn set_policy(&self, p: PolicySet) {
        *self.policy.write().unwrap() = Arc::new(p);
    }
    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }
    pub fn set_settings(&self, s: Settings) {
        *self.settings.write().unwrap() = s;
    }
    pub fn policy_hash(&self) -> String {
        self.policy().effective_hash(&self.settings().fallback)
    }

    // ------------------------------------------------------------------ audit

    pub fn log(&self, ev: &AuditEvent) {
        let kid = self.vault.active_key_id();
        let Some(k) = self.vault.audit_mac_key(&kid) else {
            tracing::error!("audit key unavailable; event not recorded");
            return;
        };
        if let Err(e) = self.db.append_event(ev, self.settings().retention_days, &k, &kid) {
            tracing::error!("audit append failed: {e}");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn event(&self, c: &Ctx, kind: &str, action: &str, dest: Option<&str>, decision: Option<&Decision>, error: Option<ErrorCode>, approval: Option<(&str, &str)>, extra_cats: &[String]) -> AuditEvent {
        let mut cats: Vec<String> = decision.map(|d| d.findings.iter().filter(|f| f.action != Action::Allow || f.approved).map(|f| f.category.clone()).collect()).unwrap_or_default();
        if decision.is_none() || cats.is_empty() {
            cats.extend(c.findings.iter().map(|f| f.category.clone()));
        }
        cats.extend(extra_cats.iter().cloned());
        cats.sort();
        cats.dedup();
        let severity = c.findings.iter().map(|f| f.sensitivity).max().unwrap_or(Sensitivity::Low);
        AuditEvent {
            schema_version: AUDIT_SCHEMA_VERSION,
            event_id: c.event_id.clone(),
            timestamp: Utc::now(),
            kind: kind.into(),
            app_id: c.caller.app_id.clone(),
            agent_id: c.caller.agent_id.clone(),
            user_id: self.settings().owner_id,
            destination_id: dest.map(String::from),
            endpoint: Some(match c.endpoint {
                Endpoint::Chat => "/v1/chat/completions".into(),
                Endpoint::Responses => "/v1/responses".into(),
            }),
            summary: summarize(&cats, action),
            categories: cats,
            action: action.into(),
            policy_ids: decision.map(|d| d.policy_ids()).unwrap_or_default(),
            policy_hash: decision.map(|d| d.effective_policy_hash.clone()),
            recognizer_versions: c.report.recognizer_versions.clone(),
            latency_ms: c.started.elapsed().as_millis() as u64,
            finding_count: c.findings.len() as u32,
            approval_id: approval.map(|a| a.0.to_string()),
            approval_status: approval.map(|a| a.1.to_string()),
            error_code: error.map(|e| e.as_str().to_string()),
            severity,
            direction: Direction::Inbound,
            context_ref: None,
        }
    }

    fn notify(&self, kind: &str, title: &str, cats: &[String], app: &str) {
        let names = summarize(cats, "");
        let names = names.trim_end_matches(": ").to_string();
        self.notifier.notify(Notification { kind: kind.into(), title: title.into(), body: format!("{names} · app {app}"), categories: cats.to_vec(), at: Utc::now().to_rfc3339() });
    }

    // ------------------------------------------------------------------ entry

    pub async fn handle(self: &Arc<Self>, caller: Caller, endpoint: Endpoint, raw: &[u8]) -> Result<ProxyOutput, PvError> {
        let settings = self.settings();
        let event_id = uuid::Uuid::new_v4().to_string();
        let started = Instant::now();
        if raw.len() > settings.max_request_bytes {
            return Err(PvError::new(ErrorCode::PayloadTooLarge, "Request body exceeds size limit").with_event(event_id));
        }
        let mut body: Value = serde_json::from_slice(raw).map_err(|_| PvError::new(ErrorCode::BadRequest, "Request body is not valid JSON").with_event(event_id.clone()))?;
        if !body.is_object() {
            return Err(PvError::new(ErrorCode::BadRequest, "Request body must be a JSON object").with_event(event_id));
        }
        // Strict extension; removed before anything is forwarded upstream.
        let privacy = PrivacyRequest::take_from(&mut body).map_err(|e| e.with_event(event_id.clone()))?;
        if privacy.on_unavailable == OnUnavailable::Review && caller.hosted {
            return Err(PvError::new(ErrorCode::BadRequest, "zfx_privacy.on_unavailable=review needs the local review queue; the hosted gateway only supports block").with_event(event_id));
        }
        if caller.source == Source::Dashboard {
            body["stream"] = Value::Bool(false);
        }
        let stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
        let request_hash = sha(format!("{}\n{:?}\n{}\n{}", caller.app_id, endpoint, serde_json::to_string(&body).unwrap(), serde_json::to_string(&privacy).unwrap()).as_bytes());

        let roots = match endpoint {
            Endpoint::Chat => fields::CHAT_REQUEST_ROOTS,
            Endpoint::Responses => fields::RESPONSES_REQUEST_ROOTS,
        };
        let ex = fields::extract(&body, roots);
        let texts = fields::texts(&ex.fields);
        let conv = caller.conversation.clone().or(privacy.conversation_id.clone()).unwrap_or_else(|| {
            let first: String = texts.iter().take(2).cloned().collect::<Vec<_>>().join("\n");
            sha(first.as_bytes())[7..23].to_string()
        });
        let scope = format!("{}:{}", caller.app_id, conv);
        let store: Arc<dyn TokenStore> = if privacy.preprocessed {
            Arc::new(RefuseStore)
        } else {
            caller.token_store.clone().unwrap_or_else(|| self.vault.clone() as Arc<dyn TokenStore>)
        };

        let mut c = Ctx { caller, privacy, store, endpoint, started, event_id, request_hash, scope, stream, report: DetectionReport::default(), fields: ex.fields, findings: vec![], body };

        if settings.emergency_stop {
            let ev = self.event(&c, "request", "BLOCK", None, None, Some(ErrorCode::PolicyBlocked), None, &["EMERGENCY_STOP".into()]);
            self.log(&ev);
            return Err(PvError::new(ErrorCode::PolicyBlocked, "ZFX VPN emergency stop is engaged; all model traffic is blocked").with_event(c.event_id));
        }

        // Classify.
        c.report = self.detector.detect(&texts, true).await;
        c.findings = c.report.findings.clone();
        c.findings.extend(fields::unsupported_findings(texts.len(), &ex.unsupported));

        // Resolve primary destination.
        let primary_id = c.caller.destination_override.clone().or(settings.default_destination.clone());
        let Some(primary_cfg) = primary_id.as_deref().and_then(|id| self.registry.get(id)).filter(|p| p.enabled) else {
            let ev = self.event(&c, "error", "ERROR", None, None, Some(ErrorCode::RouteForbidden), None, &[]);
            self.log(&ev);
            return Err(PvError::new(ErrorCode::RouteForbidden, "No generation destination is configured; run `zfx-vpn init` or set one in Settings").with_event(c.event_id));
        };
        let requested_model = c.body.get("model").and_then(|m| m.as_str()).and_then(|m| m.strip_prefix(&format!("{}/", primary_cfg.id))).map(String::from);
        let primary = self.registry.resolve(&primary_cfg, requested_model.as_deref()).await;

        let policy = self.policy();
        let phash = policy.effective_hash(&settings.fallback);

        // Owner decisions recorded for this exact request.
        let prior = if c.caller.hosted { vec![] } else { self.db.pending_by_hash(&c.request_hash).unwrap_or_default() };
        if prior.iter().any(|p| p.status == PendingStatus::Denied && p.app_id == c.caller.app_id && p.expires_at > Utc::now()) {
            let ev = self.event(&c, "request", "BLOCK", Some(&primary.config.id), None, Some(ErrorCode::PolicyBlocked), None, &[]);
            self.log(&ev);
            return Err(PvError::new(ErrorCode::PolicyBlocked, "This request was denied by the device owner").with_event(c.event_id));
        }
        let approved = prior.into_iter().find(|p| p.status == PendingStatus::Approved && p.app_id == c.caller.app_id && p.expires_at > Utc::now() && p.destination_id == primary.config.id && p.policy_hash == phash);

        let base_ctx = |dest: &Resolved, grant: Option<ApprovalGrant>| EvalContext {
            user_id: "local-owner",
            agent_id: c.caller.agent_id.clone(),
            app_id: Some(c.caller.app_id.clone()),
            request_id: None,
            destination: DestinationRef { id: dest.config.id.clone(), tier: dest.tier },
            direction: Direction::Inbound,
            fallback: settings.fallback.clone(),
            approval: grant,
        };

        let mut decision = evaluate(&policy, &c.findings, &base_ctx(&primary, None));
        let mut used_approval: Option<String> = None;
        if let Some(p) = &approved {
            let grant = ApprovalGrant { approval_id: p.approval_id.clone(), request_hash: p.request_hash.clone(), destination_id: p.destination_id.clone(), policy_hash: p.policy_hash.clone(), expires_at: p.expires_at };
            let d2 = evaluate(&policy, &c.findings, &base_ctx(&primary, Some(grant)));
            if d2.used_approval {
                // Approval binds to the exact transformed payload.
                match self.build_payload(&c, &d2, &primary, false) {
                    Ok((payload, _)) if sha(&serde_json::to_vec(&payload).unwrap()) == p.approved_payload_hash
                        && self.db.consume_approval(&p.approval_id).unwrap_or(false) => {
                            decision = d2;
                            used_approval = Some(p.approval_id.clone());
                        }
                    _ => {}
                }
            }
        }

        match decision.disposition {
            Disposition::Block => {
                let ev = self.event(&c, "request", "BLOCK", Some(&primary.config.id), Some(&decision), Some(ErrorCode::PolicyBlocked), None, &[]);
                self.log(&ev);
                self.notify("block", "Request blocked", &ev.categories, &c.caller.app_id);
                return Err(PvError::new(ErrorCode::PolicyBlocked, "Request blocked by policy").with_event(c.event_id).with_categories(ev.categories));
            }
            Disposition::RequireApproval => {
                if c.caller.hosted {
                    // Hosted mode has no review queue and never stores raw payloads.
                    let ev = self.event(&c, "request", "BLOCK", Some(&primary.config.id), Some(&decision), Some(ErrorCode::ApprovalRequired), None, &[]);
                    self.log(&ev);
                    return Err(PvError::new(ErrorCode::ApprovalRequired, "Request needs owner review, which the hosted gateway does not offer; nothing was stored or sent. Use local ZFX VPN or change the tenant policy").with_event(c.event_id).with_categories(ev.categories));
                }
                return Err(self.queue_for_review(&c, &decision, &primary, raw, &phash, &policy, &settings).await);
            }
            _ => {}
        }

        // The caller's extension can only tighten the route.
        let mode = c.privacy.mode;
        if mode == PrivacyMode::TransformOnly && decision.disposition == Disposition::PrivateRoute {
            let ev = self.event(&c, "request", "BLOCK", Some(&primary.config.id), Some(&decision), Some(ErrorCode::PolicyBlocked), None, &[]);
            self.log(&ev);
            return Err(PvError::new(ErrorCode::PolicyBlocked, "Request needs a confidential route but zfx_privacy.mode is transform_only; nothing was sent").with_event(c.event_id).with_categories(ev.categories));
        }
        let private = decision.disposition == Disposition::PrivateRoute || mode == PrivacyMode::Confidential;
        let local_only = mode == PrivacyMode::LocalOnly;
        let private_model = c.privacy.fallback_model.clone().or(settings.private_model.clone());

        // Candidate destinations. Route choice is final once a destination
        // accepts the request: nothing is ever replayed elsewhere afterwards.
        let mut candidates: Vec<pv_core::ProviderConfig> = Vec::new();
        let mut unavailable = ErrorCode::FallbackUnavailable;
        if local_only {
            for p in self.registry.providers() {
                if p.enabled && self.registry.resolve(&p, None).await.tier == DestinationTier::Local {
                    candidates.push(p);
                }
            }
        } else if private {
            let tee_allowed = settings.fallback.tee_consent && (mode == PrivacyMode::Confidential || settings.fallback.mode == FallbackMode::TeePrivateRoute);
            if tee_allowed {
                if let (Some(pd), Some(_)) = (&settings.private_destination, &private_model) {
                    if let Some(cfg) = self.registry.get(pd).filter(|p| p.enabled && p.kind == ProviderKind::NearAi) {
                        candidates.push(cfg);
                    }
                }
            }
            if settings.fallback.local_private_handling {
                for p in self.registry.providers() {
                    if p.enabled && p.kind == ProviderKind::Ollama && pv_provider::is_loopback(&p.base_url) && !candidates.iter().any(|x| x.id == p.id) {
                        candidates.push(p);
                    }
                }
            }
        } else {
            candidates.push(primary.config.clone());
            for id in &settings.failover {
                if let Some(p) = self.registry.get(id).filter(|p| p.enabled && p.allow_failover && p.id != primary.config.id) {
                    candidates.push(p);
                }
            }
            unavailable = ErrorCode::RouteForbidden;
        }
        if candidates.is_empty() {
            if c.privacy.on_unavailable == OnUnavailable::Review && !c.caller.hosted {
                return Err(self.queue_for_review(&c, &decision, &primary, raw, &phash, &policy, &settings).await);
            }
            let ev = self.event(&c, "request", "BLOCK", Some(&primary.config.id), Some(&decision), Some(unavailable), None, &[]);
            self.log(&ev);
            self.notify("block", "No verified private route", &ev.categories, &c.caller.app_id);
            let msg = if local_only {
                "zfx_privacy.mode is local_only but no on-device destination is configured; nothing was sent"
            } else {
                "Request needs a confidential destination but no consented, verified NEAR AI TEE model (or approved local model) is configured; nothing was sent"
            };
            return Err(PvError::new(unavailable, msg).with_event(c.event_id).with_categories(ev.categories));
        }

        let mut last_err: Option<ProviderError> = None;
        let mut tried = Vec::new();
        let route_private = private || local_only;
        for (i, cfg) in candidates.iter().enumerate() {
            let model = if private && cfg.kind == ProviderKind::NearAi { private_model.clone() } else if i == 0 && !route_private { requested_model.clone() } else { None };
            let dest = if !route_private && i == 0 { primary.clone() } else { self.registry.resolve(cfg, model.as_deref()).await };
            tried.push(dest.config.id.clone());
            if route_private {
                let ok = if local_only { dest.tier == DestinationTier::Local } else { dest.tier == DestinationTier::AttestedTee || (dest.tier == DestinationTier::Local && settings.fallback.local_private_handling) };
                if !ok {
                    last_err = Some(ProviderError::Verification(dest.verification.as_ref().and_then(|v| v.error.clone()).unwrap_or_else(|| "not verified".into())));
                    continue;
                }
            }
            // Receipt-before-release needs a provider-attested route.
            if c.privacy.require_receipt && !(dest.tier == DestinationTier::AttestedTee && dest.config.kind == ProviderKind::NearAi) {
                last_err = Some(ProviderError::Verification("zfx_privacy.require_receipt needs a verified NEAR AI TEE route".into()));
                continue;
            }
            // Re-evaluate for this destination; never downgrade.
            let d = if i == 0 && !route_private { decision.clone() } else { evaluate(&policy, &c.findings, &base_ctx(&dest, None)) };
            if !matches!(d.disposition, Disposition::Allow | Disposition::Transform) {
                last_err = Some(ProviderError::Verification(format!("policy does not permit {} for this request", dest.config.id)));
                continue;
            }
            let (payload, translated) = match self.build_payload(&c, &d, &dest, private) {
                Ok(x) => x,
                Err(e) => {
                    let ev = self.event(&c, "request", "BLOCK", Some(&dest.config.id), Some(&d), Some(e.code), None, &[]);
                    self.log(&ev);
                    self.notify("block", "Request blocked", &ev.categories, &c.caller.app_id);
                    return Err(e.with_event(c.event_id.clone()).with_categories(ev.categories));
                }
            };
            // Output handling is decided before anything is sent.
            let buffered = c.stream && (settings.outbound_span_model || c.privacy.require_receipt);
            if buffered && c.privacy.stream_buffering == StreamBuffering::Reject {
                let ev = self.event(&c, "request", "BLOCK", Some(&dest.config.id), Some(&d), Some(ErrorCode::StreamSafetyBuffered), None, &[]);
                self.log(&ev);
                return Err(PvError::new(ErrorCode::StreamSafetyBuffered, "The privacy policy must buffer this stream until it is checked, and zfx_privacy.stream_buffering is reject; nothing was sent").with_event(c.event_id));
            }
            let send_endpoint = if translated { Endpoint::Chat } else { c.endpoint };
            let send_result = if dest.config.zfx_hosted_transport {
                self.registry.send_preprocessed(&dest, send_endpoint, payload, c.privacy.mode).await
            } else {
                self.registry.send(&dest, send_endpoint, payload, private).await
            };
            match send_result {
                Ok(up) => {
                    let action = if private { "PRIVATE_ROUTE" } else if d.disposition == Disposition::Transform { "TRANSFORM" } else { "ALLOW" };
                    let mut ev = self.event(&c, "request", action, Some(&dest.config.id), Some(&d), None, used_approval.as_deref().map(|a| (a, "CONSUMED")), &[]);
                    if i > 0 {
                        ev.summary = format!("{} (failover from {})", ev.summary, tried[..i].join(", "));
                    }
                    if private {
                        self.notify("private_route", "Sent to verified private model", &ev.categories, &c.caller.app_id);
                    } else if d.disposition == Disposition::Transform {
                        self.notify("redaction", "Sensitive data transformed", &ev.categories, &c.caller.app_id);
                    }
                    let tier = PrivacyTier::of(dest.config.kind, dest.tier);
                    let attested = tier == PrivacyTier::AttestedConfidential;
                    let route = PrivacyRoute {
                        request_id: c.event_id.clone(),
                        privacy_tier: tier,
                        destination_id: dest.config.id.clone(),
                        model: dest.model.clone(),
                        mode: c.privacy.mode,
                        action: action.into(),
                        transformed_values: d.findings.iter().filter(|f| f.action.is_transform()).count() as u32,
                        verification: if attested { VerificationStatus::PendingVerification } else { VerificationStatus::NotApplicable },
                        verifier: dest.verification.as_ref().filter(|_| attested).map(|v| v.verifier.clone()),
                        stream_mode: if !c.stream { "none".into() } else if buffered { "buffered".into() } else { "incremental".into() },
                        preprocessed: c.privacy.preprocessed || dest.config.zfx_hosted_transport,
                        created_at: Utc::now().to_rfc3339(),
                    };
                    let _ = self.db.put_route(&c.caller.app_id, &route);
                    return self.respond(c, dest, d, up, translated, ev, route, buffered).await;
                }
                Err(e) => {
                    tracing::warn!("destination {} failed: {e}", dest.config.id);
                    if matches!(e, ProviderError::Verification(_)) {
                        self.registry.invalidate(&dest.config.id, &dest.model);
                    }
                    let fo = e.failover_eligible();
                    last_err = Some(e);
                    if !fo {
                        break;
                    }
                }
            }
        }
        let e = last_err.unwrap_or(ProviderError::Unreachable);
        let code = match (&e, route_private) {
            (ProviderError::Verification(_), true) if !local_only => ErrorCode::TeeUnverified,
            (ProviderError::Verification(_), true) => ErrorCode::FallbackUnavailable,
            (ProviderError::Verification(_), false) => ErrorCode::RouteForbidden,
            _ => ErrorCode::ProviderError,
        };
        let ev = self.event(&c, "error", "ERROR", tried.first().map(|s| s.as_str()), Some(&decision), Some(code), None, &[]);
        self.log(&ev);
        self.notify("error", "Model request failed", &ev.categories, &c.caller.app_id);
        Err(PvError::new(code, format!("No permitted destination completed the request ({e}); nothing was sent to a destination that policy forbids")).with_event(c.event_id))
    }

    /// Transform fields per decision and build the upstream body.
    fn build_payload(&self, c: &Ctx, d: &Decision, dest: &Resolved, private: bool) -> Result<(Value, bool), PvError> {
        let mut texts: Vec<String> = c.fields.iter().map(|f| f.text.clone()).collect();
        let ttl = Duration::minutes(self.settings().mapping_ttl_minutes);
        let rep = pv_transform::apply(&mut texts, &c.findings, d, c.store.as_ref(), &c.scope, ttl).map_err(|e| match e {
            pv_transform::TransformError::NotIsolatable(_) => PvError::new(ErrorCode::PolicyBlocked, "Sensitive content could not be isolated for transformation; blocked locally"),
            pv_transform::TransformError::Store(_) if c.privacy.preprocessed => PvError::new(ErrorCode::PolicyBlocked, "Preprocessed request still contains values that need transformation; the client must transform them. Nothing was sent"),
            pv_transform::TransformError::Vault(_) | pv_transform::TransformError::Store(_) => PvError::new(ErrorCode::VaultLocked, "Mapping store unavailable; failing closed"),
            pv_transform::TransformError::BadOffsets => PvError::new(ErrorCode::ClassifierUnavailable, "Detector produced invalid offsets; failing closed"),
        })?;
        // GENERALIZE output is reclassified; anything still flagged fails closed.
        if !rep.generalized_fields.is_empty() {
            let exact = self.detector.exact.read().unwrap().clone();
            for &fi in &rep.generalized_fields {
                let again = pv_detectors::detect_deterministic(&texts[fi], &exact);
                let ctx = EvalContext {
                    user_id: "local-owner",
                    agent_id: c.caller.agent_id.clone(),
                    app_id: Some(c.caller.app_id.clone()),
                    request_id: None,
                    destination: DestinationRef { id: dest.config.id.clone(), tier: dest.tier },
                    direction: Direction::Inbound,
                    fallback: self.settings().fallback,
                    approval: None,
                };
                let d2 = evaluate(&self.policy(), &again, &ctx);
                if d2.disposition != Disposition::Allow {
                    return Err(PvError::new(ErrorCode::PolicyBlocked, "Generalized text still violates policy; blocked locally"));
                }
            }
        }
        let mut fields_out = c.fields.clone();
        for (f, t) in fields_out.iter_mut().zip(texts) {
            f.text = t;
        }
        let mut body = c.body.clone();
        fields::write_back(&mut body, &fields_out);
        // Media parts that policy allowed (local destinations) stay; nothing else changes.
        let translate = c.endpoint == Endpoint::Responses && (private && dest.config.kind == ProviderKind::NearAi || !dest.config.supports_responses);
        if translate {
            body = protocol::responses_to_chat(&body);
        }
        if let Some(o) = body.as_object_mut() {
            o.remove("store");
        }
        Ok((body, translate))
    }

    #[allow(clippy::too_many_arguments)]
    async fn queue_for_review(&self, c: &Ctx, decision: &Decision, primary: &Resolved, raw: &[u8], phash: &str, policy: &PolicySet, settings: &Settings) -> PvError {
        let approval_id = format!("apr_{}", uuid::Uuid::new_v4().simple());
        // Payload that would be sent if the owner approves (binding target).
        let grant = ApprovalGrant { approval_id: approval_id.clone(), request_hash: c.request_hash.clone(), destination_id: primary.config.id.clone(), policy_hash: phash.to_string(), expires_at: Utc::now() };
        let ctx = EvalContext {
            user_id: "local-owner",
            agent_id: c.caller.agent_id.clone(),
            app_id: Some(c.caller.app_id.clone()),
            request_id: None,
            destination: DestinationRef { id: primary.config.id.clone(), tier: primary.tier },
            direction: Direction::Inbound,
            fallback: settings.fallback.clone(),
            approval: Some(grant),
        };
        let d2 = evaluate(policy, &c.findings, &ctx);
        let payload_hash = match self.build_payload(c, &d2, primary, false) {
            Ok((p, _)) => sha(&serde_json::to_vec(&p).unwrap()),
            Err(_) => "unapprovable".into(),
        };
        let preview = redacted_preview(&c.fields, &c.findings);
        let expires = Utc::now() + Duration::minutes(settings.pending_ttl_minutes);
        let cats = decision.categories();
        let p = PendingApproval {
            schema_version: PENDING_SCHEMA_VERSION,
            approval_id: approval_id.clone(),
            event_id: c.event_id.clone(),
            app_id: c.caller.app_id.clone(),
            destination_id: primary.config.id.clone(),
            endpoint: match c.endpoint {
                Endpoint::Chat => "/v1/chat/completions".into(),
                Endpoint::Responses => "/v1/responses".into(),
            },
            request_hash: c.request_hash.clone(),
            policy_hash: phash.to_string(),
            approved_payload_hash: payload_hash,
            categories: cats.clone(),
            redacted_preview: preview,
            reason: decision.explanations.join("; "),
            status: PendingStatus::Pending,
            created_at: Utc::now(),
            expires_at: expires,
            decided_at: None,
            decided_by: None,
        };
        let stored = self.vault.put_pending_payload(&approval_id, raw, expires).is_ok() && self.db.put_pending(&p).is_ok();
        if !stored {
            let ev = self.event(c, "request", "BLOCK", Some(&primary.config.id), Some(decision), Some(ErrorCode::VaultLocked), None, &[]);
            self.log(&ev);
            return PvError::new(ErrorCode::VaultLocked, "Could not queue request for review; blocked locally").with_event(c.event_id.clone());
        }
        let ev = self.event(c, "request", "REQUIRE_APPROVAL", Some(&primary.config.id), Some(decision), Some(ErrorCode::ApprovalRequired), Some((&approval_id, "PENDING")), &[]);
        self.log(&ev);
        self.notify("review", "Review required", &ev.categories, &c.caller.app_id);
        PvError::new(ErrorCode::ApprovalRequired, "Request held for owner review in the ZFX VPN dashboard; retry after approval").with_event(c.event_id.clone()).with_approval(approval_id).with_categories(cats)
    }

    fn out_ctx(&self, c: &Ctx, dest: &Resolved) -> Arc<OutCtx> {
        let s = self.settings();
        Arc::new(OutCtx {
            policy: self.policy(),
            fallback: s.fallback.clone(),
            user_id: s.owner_id.clone(),
            agent_id: c.caller.agent_id.clone(),
            app_id: Some(c.caller.app_id.clone()),
            destination: DestinationRef { id: dest.config.id.clone(), tier: dest.tier },
            vault: c.store.clone(),
            detector: self.detector.clone(),
            exact: self.detector.exact.read().unwrap().clone(),
            scope: c.scope.clone(),
            // Preprocessed requests are restored by the client that holds the map.
            restore: s.restore_placeholders && !c.privacy.preprocessed,
            span_model: s.outbound_span_model,
        })
    }

    /// Require a verified provider-TEE signature for this exact completion.
    async fn require_provider_receipt(&self, dest: &Resolved, completion_id: Option<&str>) -> Result<String, PvError> {
        let Some(id) = completion_id else {
            return Err(PvError::new(ErrorCode::TeeUnverified, "Upstream response has no completion id to verify; output withheld"));
        };
        let r = self.registry.verify_receipt(&dest.config.id, id).await;
        if r.verified && r.signature_kind == pv_provider::PROVIDER_TEE_SIGNATURE {
            Ok(format!("provider_tee signature by {}", r.signer.unwrap_or_default()))
        } else if r.verified {
            Err(PvError::new(ErrorCode::TeeUnverified, "Only a gateway signature is available for this response, not a provider-TEE signature; output withheld"))
        } else {
            Err(PvError::new(ErrorCode::TeeUnverified, "Provider-TEE response signature could not be verified; output withheld"))
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn respond(self: &Arc<Self>, c: Ctx, dest: Resolved, _d: Decision, up: pv_provider::UpstreamResponse, translated: bool, mut ev: AuditEvent, mut route: PrivacyRoute, buffered: bool) -> Result<ProxyOutput, PvError> {
        let settings = self.settings();
        let octx = self.out_ctx(&c, &dest);
        let event_id = c.event_id.clone();
        let is_sse = up.content_type.contains("text/event-stream");
        let mut notices: Vec<&'static str> = vec![];
        if buffered {
            notices.push("ZFX_STREAM_SAFETY_BUFFERED");
        }
        if !c.stream || !is_sse {
            // Non-streaming: read fully (bounded).
            let mut body = up.body;
            let mut buf = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|e| PvError::new(ErrorCode::ProviderError, format!("upstream read failed: {e}")).with_event(event_id.clone()))?;
                buf.extend_from_slice(&chunk);
                if buf.len() > settings.max_response_bytes {
                    return Err(PvError::new(ErrorCode::ProviderError, "Upstream response exceeds size limit").with_event(event_id));
                }
            }
            let mut v: Value = serde_json::from_slice(&buf).map_err(|_| PvError::new(ErrorCode::ProviderError, "Upstream returned non-JSON").with_event(event_id.clone()))?;
            let mut stats = OutStats::default();
            let roots = if translated || c.endpoint == Endpoint::Chat { fields::CHAT_RESPONSE_ROOTS } else { fields::RESPONSES_RESPONSE_ROOTS };
            let ex = fields::extract(&v, roots);
            let texts = fields::texts(&ex.fields);
            match octx.process_full(&texts, &mut stats).await {
                Ok(out) => {
                    let mut fs = ex.fields;
                    for (f, t) in fs.iter_mut().zip(out) {
                        f.text = t;
                    }
                    fields::write_back(&mut v, &fs);
                }
                Err(b) => {
                    ev.action = "OUTPUT_BLOCK".into();
                    ev.error_code = Some(ErrorCode::OutputBlocked.as_str().into());
                    ev.categories.extend(b.categories.clone());
                    ev.summary = summarize(&b.categories, "OUTPUT_BLOCK");
                    self.log(&ev);
                    self.notify("block", "Model output blocked", &b.categories, &c.caller.app_id);
                    return Err(PvError::new(ErrorCode::OutputBlocked, "Model output blocked by policy").with_event(event_id).with_categories(b.categories));
                }
            }
            if c.privacy.require_receipt {
                match self.require_provider_receipt(&dest, v.get("id").and_then(|i| i.as_str())).await {
                    Ok(by) => {
                        route.verification = VerificationStatus::ReceiptVerified;
                        route.verifier = Some(by);
                        let _ = self.db.put_route(&c.caller.app_id, &route);
                    }
                    Err(e) => {
                        route.verification = VerificationStatus::Failed;
                        let _ = self.db.put_route(&c.caller.app_id, &route);
                        ev.action = "OUTPUT_BLOCK".into();
                        ev.error_code = Some(e.code.as_str().into());
                        self.log(&ev);
                        return Err(e.with_event(event_id));
                    }
                }
            }
            if translated && c.endpoint == Endpoint::Responses {
                v = protocol::chat_to_responses(&v, &dest.model);
            }
            if stats.redacted > 0 {
                ev.summary = format!("{}; {}", ev.summary, summarize(&stats.categories, "OUTPUT_TRANSFORM"));
            }
            self.log(&ev);
            if !c.privacy.require_receipt {
                self.after_receipt(&dest, &v, &c);
            }
            return Ok(ProxyOutput { status: 200, body: Body::Json(v), event_id, route: Some(route), notices });
        }

        // Streaming. The route is final: errors after this point end the
        // stream with an error event; nothing is replayed to another destination.
        let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(64);
        let me = self.clone();
        let max = settings.max_response_bytes;
        let idle = std::time::Duration::from_secs(settings.stream_idle_timeout_secs.max(1));
        let endpoint = c.endpoint;
        let app_id = c.caller.app_id.clone();
        let dest2 = dest.clone();
        let require_receipt = c.privacy.require_receipt;
        let route2 = route.clone();
        tokio::spawn(async move {
            let mut route = route2;
            let mut upstream = up.body;
            let mut parser = SseParser::default();
            let mut total = 0usize;
            let use_chat = endpoint == Endpoint::Chat || translated;
            let mut chat = ChatStream::new(octx.clone(), max);
            let mut resp = ResponsesStream::new(octx.clone(), max);
            let mut tr = if translated { Some(protocol::ChatToResponsesStream::new(&dest2.model)) } else { None };
            let mut blocked: Option<Vec<String>> = None;
            let mut upstream_error: Option<&'static str> = None;
            let mut cancelled = false;
            // Buffered streams hold every inspected byte until the end.
            let mut held: Vec<Bytes> = Vec::new();
            let emit_chat = |v: Value, tr: &mut Option<protocol::ChatToResponsesStream>| -> String {
                match tr {
                    Some(t) => t.push(&v),
                    None => data_line(&v),
                }
            };
            'outer: loop {
                let next = match tokio::time::timeout(idle, upstream.next()).await {
                    Ok(n) => n,
                    Err(_) => {
                        upstream_error = Some("upstream stream idle timeout");
                        break;
                    }
                };
                let Some(chunk) = next else { break };
                let Ok(chunk) = chunk else {
                    upstream_error = Some("upstream stream interrupted");
                    break;
                };
                total += chunk.len();
                if total > max * 2 {
                    blocked = Some(vec!["OUTPUT_TOO_LARGE".into()]);
                    break;
                }
                let events = match parser.push(&chunk) {
                    Ok(e) => e,
                    Err(_) => {
                        upstream_error = Some("malformed upstream stream");
                        break;
                    }
                };
                for e in events {
                    let out = if use_chat {
                        if e.data == "[DONE]" {
                            break 'outer;
                        }
                        let Ok(v) = serde_json::from_str::<Value>(&e.data) else {
                            upstream_error = Some("malformed upstream chunk");
                            break 'outer;
                        };
                        if v.get("error").is_some_and(|x| !x.is_null()) {
                            // Upstream error object: never forwarded verbatim (it may echo input).
                            upstream_error = Some("upstream reported an error mid-stream");
                            break 'outer;
                        }
                        match chat.on_chunk(&v) {
                            Ok(vs) => vs.into_iter().map(|v| emit_chat(v, &mut tr)).collect::<String>(),
                            Err(b) => {
                                blocked = Some(b.categories);
                                break 'outer;
                            }
                        }
                    } else {
                        match resp.on_event(&e).await {
                            Ok(s) => s,
                            Err(b) => {
                                blocked = Some(b.categories);
                                break 'outer;
                            }
                        }
                    };
                    if out.is_empty() {
                        continue;
                    }
                    if buffered {
                        held.push(Bytes::from(out));
                    } else if tx.send(Bytes::from(out)).await.is_err() {
                        cancelled = true;
                        break 'outer;
                    }
                }
            }
            drop(upstream); // stop reading from the provider now
            // Flush.
            if blocked.is_none() && upstream_error.is_none() && !cancelled {
                let tail = if use_chat {
                    match chat.finish().await {
                        Ok(vs) => {
                            let mut s: String = vs.into_iter().map(|v| emit_chat(v, &mut tr)).collect();
                            if let Some(t) = tr.as_mut() {
                                s += &t.finish();
                            } else {
                                s += "data: [DONE]\n\n";
                            }
                            Ok(s)
                        }
                        Err(b) => Err(b),
                    }
                } else {
                    resp.finish().await
                };
                match tail {
                    Ok(s) => held.push(Bytes::from(s)),
                    Err(b) => blocked = Some(b.categories),
                }
            }
            let completion_id = chat.completion_id.clone().or(resp.response_id.clone());
            let mut receipt_error: Option<PvError> = None;
            if blocked.is_none() && upstream_error.is_none() && !cancelled && require_receipt {
                match me.require_provider_receipt(&dest2, completion_id.as_deref()).await {
                    Ok(by) => {
                        route.verification = VerificationStatus::ReceiptVerified;
                        route.verifier = Some(by);
                    }
                    Err(e) => {
                        route.verification = VerificationStatus::Failed;
                        receipt_error = Some(e);
                    }
                }
                let _ = me.db.put_route(&app_id, &route);
            }
            let stats = if use_chat { chat.stats() } else { resp.stats() };
            let error_event = |err: PvError| -> String {
                let env = err.envelope_json(false);
                if use_chat && tr.is_none() {
                    format!("data: {env}\n\ndata: [DONE]\n\n")
                } else {
                    format!("event: error\ndata: {}\n\n", json!({"type": "error", "code": err.code.as_str(), "message": err.message, "event_id": err.event_id}))
                }
            };
            if let Some(cats) = blocked {
                let err = PvError::new(ErrorCode::OutputBlocked, "Model output blocked by policy").with_event(ev.event_id.clone()).with_categories(cats.clone());
                let msg = error_event(err);
                if !buffered {
                    let _ = tx.send(Bytes::from(msg)).await;
                } else {
                    let _ = tx.send(Bytes::from(msg)).await;
                }
                ev.action = "OUTPUT_BLOCK".into();
                ev.error_code = Some(ErrorCode::OutputBlocked.as_str().into());
                ev.summary = summarize(&cats, "OUTPUT_BLOCK");
                ev.categories.extend(cats.clone());
                me.notify("block", "Model output blocked", &cats, &app_id);
            } else if let Some(why) = upstream_error {
                // Buffered output is discarded; incremental output already released stays released.
                let _ = tx.send(Bytes::from(error_event(PvError::new(ErrorCode::ProviderError, why).with_event(ev.event_id.clone())))).await;
                ev.error_code = Some(ErrorCode::ProviderError.as_str().into());
                ev.summary = format!("{}; {why}", ev.summary);
            } else if let Some(err) = receipt_error {
                let _ = tx.send(Bytes::from(error_event(err.with_event(ev.event_id.clone())))).await;
                ev.action = "OUTPUT_BLOCK".into();
                ev.error_code = Some(ErrorCode::TeeUnverified.as_str().into());
                ev.summary = format!("{}; output withheld: provider-TEE receipt not verified", ev.summary);
            } else if cancelled {
                ev.summary = format!("{}; client cancelled the stream", ev.summary);
            } else {
                for b in held {
                    if tx.send(b).await.is_err() {
                        ev.summary = format!("{}; client cancelled the stream", ev.summary);
                        break;
                    }
                }
                if stats.redacted > 0 {
                    ev.summary = format!("{}; {}", ev.summary, summarize(&stats.categories, "OUTPUT_TRANSFORM"));
                }
            }
            ev.latency_ms = c.started.elapsed().as_millis() as u64;
            me.log(&ev);
            if !require_receipt {
                if let Some(id) = completion_id {
                    me.after_receipt(&dest2, &json!({"id": id}), &c);
                }
            }
        });
        let s = tokio_stream::wrappers::ReceiverStream::new(rx).boxed();
        Ok(ProxyOutput { status: 200, body: Body::Stream(s), event_id, route: Some(route), notices })
    }

    /// Verify a NEAR completion receipt in the background, update the route
    /// record (PENDING_VERIFICATION until then) and audit the result.
    fn after_receipt(self: &Arc<Self>, dest: &Resolved, resp: &Value, c: &Ctx) {
        if dest.config.kind != ProviderKind::NearAi {
            return;
        }
        let Some(id) = resp.get("id").and_then(|i| i.as_str()).map(String::from) else { return };
        let me = self.clone();
        let pid = dest.config.id.clone();
        let app = c.caller.app_id.clone();
        let request_id = c.event_id.clone();
        let attested = dest.tier == DestinationTier::AttestedTee;
        tokio::spawn(async move {
            let r = me.registry.verify_receipt(&pid, &id).await;
            if attested {
                let status = if r.verified && r.signature_kind == pv_provider::PROVIDER_TEE_SIGNATURE {
                    VerificationStatus::ReceiptVerified
                } else if r.verified {
                    VerificationStatus::GatewayOnly
                } else {
                    VerificationStatus::Failed
                };
                let _ = me.db.set_route_verification(&request_id, status, Some(&r.signature_kind));
            }
            let ev = AuditEvent {
                schema_version: AUDIT_SCHEMA_VERSION,
                event_id: uuid::Uuid::new_v4().to_string(),
                timestamp: Utc::now(),
                kind: "receipt".into(),
                app_id: app,
                agent_id: None,
                user_id: "local-owner".into(),
                destination_id: Some(pid),
                endpoint: None,
                categories: vec![],
                action: if r.verified { "RECEIPT_VERIFIED".into() } else { "RECEIPT_UNVERIFIED".into() },
                policy_ids: vec![],
                policy_hash: None,
                recognizer_versions: vec![],
                latency_ms: 0,
                finding_count: 0,
                approval_id: None,
                approval_status: None,
                error_code: if r.verified { None } else { Some(ErrorCode::TeeUnverified.as_str().into()) },
                severity: if r.verified { Sensitivity::Low } else { Sensitivity::High },
                direction: Direction::Outbound,
                summary: if r.verified { format!("NEAR response signature verified ({})", r.signature_kind) } else { format!("NEAR response signature not verified: {}", r.error.unwrap_or_default().chars().take(160).collect::<String>()) },
                context_ref: None,
            };
            me.log(&ev);
        });
    }
}

/// Preview with every finding replaced by a category marker. Unsupported media
/// is shown as a marker too. Truncated for display.
pub fn redacted_preview(fields: &[Field], findings: &[Finding]) -> String {
    let mut parts = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let mut spans: Vec<(usize, usize, String)> = findings.iter().filter(|x| x.field_index == i && !x.whole_field).map(|x| (x.start_byte, x.end_byte, x.category.clone())).collect();
        let whole: Vec<&Finding> = findings.iter().filter(|x| x.field_index == i && x.whole_field).collect();
        // A whole-field judgement with nothing localized: mask the field entirely.
        if spans.iter().any(|s| s.0 == 0 && s.1 >= f.text.len()) || (!whole.is_empty() && spans.is_empty()) {
            let cat = whole.first().map(|w| w.category.clone()).or_else(|| spans.first().map(|s| s.2.clone())).unwrap_or_else(|| "CONTENT".into());
            parts.push(format!("[REDACTED_{}]", pv_vault::token_label(&cat)));
            continue;
        }
        let flag = if whole.is_empty() { String::new() } else { format!("\n[flagged: {} (whole message)]", whole.iter().map(|w| w.category.clone()).collect::<Vec<_>>().join(", ")) };
        spans.sort_by_key(|s| (s.0, std::cmp::Reverse(s.1)));
        let mut out = String::new();
        let mut pos = 0;
        for (s, e, cat) in spans {
            if s < pos || e > f.text.len() || !f.text.is_char_boundary(s) || !f.text.is_char_boundary(e) {
                if e > pos && e <= f.text.len() && f.text.is_char_boundary(e) {
                    pos = e;
                }
                continue;
            }
            out.push_str(&f.text[pos..s]);
            out.push_str(&format!("[REDACTED_{}]", pv_vault::token_label(&cat)));
            pos = e;
        }
        out.push_str(&f.text[pos..]);
        out.push_str(&flag);
        parts.push(out);
    }
    let all = parts.join("\n\n");
    if all.chars().count() > 4000 {
        let t: String = all.chars().take(4000).collect();
        format!("{t}\n[…truncated]")
    } else {
        all
    }
}
