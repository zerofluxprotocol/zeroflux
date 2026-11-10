//! ZFX VPN hosted gateway.
//!
//! A multi-tenant, OpenAI-compatible endpoint that runs the same detection,
//! policy, transformation, routing and output pipeline as the local daemon,
//! with these differences:
//!
//! * Raw prompts reach this service before they are transformed. The operator
//!   can technically see them. Every response says so (`X-ZFX-Trust`), as does
//!   `GET /v1/zfx/privacy-policy`. It is not zero-knowledge and not an enclave.
//! * Placeholder maps live in a fresh in-memory store per request and are
//!   dropped when the response (or stream) ends. There is no global map and no
//!   cross-request restoration.
//! * No raw request or response content is written anywhere. Stored: tenant
//!   config, hashed API keys, sealed upstream credentials, usage counters and
//!   metadata-only audit/route records (30 days by default).
//! * No review queue: requests that local mode would hold for the owner fail
//!   with ZFX_APPROVAL_REQUIRED and nothing is stored.
//! * Preprocessed requests (`X-ZFX-Preprocessed: v1`, sent by a local ZFX VPN
//!   daemon in protected-transport mode) were transformed on the client, which
//!   keeps the map. The gateway never mints or restores placeholders for them.
//!
//! Tenant API keys carry scopes, per-key rate limits and per-tenant monthly
//! quotas; `Idempotency-Key` makes retries safe (no second upstream call, no
//! second charge).

pub mod store;

use axum::body::{Body as AxBody, Bytes};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use futures::StreamExt;
use pv_core::{ErrorCode, PrivacyTier, PvError};
use pv_daemon::proxy_routes::{decorate, error_response_compat, wants_legacy_errors};
use pv_proxy::{Body, Caller, Engine, Source};
use pv_vault::keys::{FileBackend, KeyBackend, MemoryBackend};
use pv_vault::Vault;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use store::{Store, Tenant, TenantConfig};
use zeroize::Zeroizing;

pub const DISCLOSURE: &str = "Hosted ZFX VPN: your raw prompt reaches the ZFX hosted service before sensitive values are transformed, so the service operator can technically see it. This mode is not confidential from ZFX and is not zero-knowledge. Nothing is retained: placeholder maps live in memory for one request; only metadata (categories, actions, privacy tier, token counts) is stored, 30 days by default. For prompts ZFX must not see, run ZFX VPN locally, or use hosted transport with local preprocessing.";
pub const PREPROCESSED_DISCLOSURE: &str = "Hosted transport with local preprocessing: your local ZFX VPN transformed this request and keeps the placeholder map; the hosted service received only the transformed payload and returns transformed output for local restoration.";

#[derive(Clone, Debug)]
pub struct Config {
    /// Data directory; `None` keeps everything in memory (tests, demos).
    pub data_dir: Option<PathBuf>,
    pub admin_token: Zeroizing<String>,
    /// Seconds a completed non-streaming response is kept IN MEMORY for
    /// idempotent replay. Default 0: results are not retained, and a retry of
    /// a completed request gets ZFX_IDEMPOTENCY_CONFLICT instead of a second
    /// upstream call. Raising it keeps model output (with restored values) in
    /// process memory for that long; it is never written to disk.
    pub idempotency_response_ttl_secs: u64,
    /// How long an idempotency key is remembered (body hash and status only).
    pub idempotency_key_ttl_secs: u64,
    pub max_request_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { data_dir: None, admin_token: Zeroizing::new(String::new()), idempotency_response_ttl_secs: 0, idempotency_key_ttl_secs: 24 * 3600, max_request_bytes: 4 << 20 }
    }
}

struct TenantRuntime {
    engine: Arc<Engine>,
    vault: Arc<Vault>,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

/// A replayable response: body and the non-sensitive x-zfx headers.
type Replay = (Value, Vec<(String, String)>);

enum Idem {
    InProgress { body_hash: String, since: Instant },
    Done { body_hash: String, status: u16, response: Option<Replay>, at: Instant, response_until: Option<Instant> },
}

pub struct Hosted {
    pub store: Store,
    cfg: Config,
    runtimes: RwLock<HashMap<String, Arc<TenantRuntime>>>,
    span_template: Option<Arc<pv_detectors::Detector>>,
    buckets: Mutex<HashMap<String, Bucket>>,
    idem: Mutex<HashMap<(String, String), Idem>>,
}

fn err(code: ErrorCode, msg: &str, legacy: bool) -> Response {
    error_response_compat(&PvError::new(code, msg), legacy)
}

fn bearer(h: &HeaderMap) -> Option<String> {
    h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer "))).map(|s| s.trim().to_string())
}

fn header(h: &HeaderMap, k: &str) -> Option<String> {
    h.get(k).and_then(|v| v.to_str().ok()).map(|s| s.chars().filter(|c| c.is_ascii_graphic()).take(128).collect())
}

impl Hosted {
    pub fn new(cfg: Config, span_template: Option<Arc<pv_detectors::Detector>>) -> anyhow::Result<Arc<Hosted>> {
        let store = match &cfg.data_dir {
            Some(d) => Store::open(&d.join("control.db"))?,
            None => Store::open_memory()?,
        };
        Ok(Arc::new(Hosted { store, cfg, runtimes: RwLock::new(HashMap::new()), span_template, buckets: Mutex::new(HashMap::new()), idem: Mutex::new(HashMap::new()) }))
    }

    /// Engine for a tenant (built on first use, rebuilt on config change).
    fn runtime(&self, t: &Tenant) -> anyhow::Result<Arc<TenantRuntime>> {
        if let Some(r) = self.runtimes.read().unwrap().get(&t.tenant_id) {
            return Ok(r.clone());
        }
        let r = Arc::new(self.build_runtime(t)?);
        self.runtimes.write().unwrap().insert(t.tenant_id.clone(), r.clone());
        Ok(r)
    }

    fn build_runtime(&self, t: &Tenant) -> anyhow::Result<TenantRuntime> {
        let (db, backend): (pv_audit::Db, Arc<dyn KeyBackend>) = match &self.cfg.data_dir {
            Some(d) => {
                let dir = d.join("tenants").join(&t.tenant_id);
                (pv_audit::Db::open(&dir.join("metadata.db"))?, Arc::new(FileBackend { path: dir.join("keys.json") }))
            }
            None => (pv_audit::Db::open_memory()?, Arc::new(MemoryBackend::default())),
        };
        let vault = Arc::new(Vault::open(db.clone(), backend).map_err(|e| anyhow::anyhow!("{e}"))?);
        let v2 = vault.clone();
        // Each tenant's registry can only read that tenant's sealed credentials.
        let creds: pv_provider::CredentialFn = Arc::new(move |r: &str| v2.get_sealed("credential", r).ok().flatten().map(|b| Zeroizing::new(String::from_utf8_lossy(&b).into_owned())));
        let registry = Arc::new(pv_provider::Registry::new(creds.clone()));
        registry.set_providers(t.config.upstreams.clone());
        for u in t.config.upstreams.iter().filter(|u| u.kind == pv_core::ProviderKind::NearAi) {
            if std::env::var("ZFX_NEAR_MOCK").is_ok() {
                continue; // tests install mock backends
            }
            let key = u.credential_ref.as_deref().and_then(|r| creds(r)).map(|k| k.to_string());
            if let Ok(b) = pv_provider::near::NearSdkBackend::new(&u.base_url, key) {
                registry.set_near_backend(&u.id, Arc::new(b));
            }
        }
        let mut choices = pv_core::default_category_choices();
        for (k, v) in &t.config.categories {
            choices.insert(k.clone(), *v);
        }
        let policy = pv_core::default_policy(&choices);
        let d = pv_proxy::Settings::default();
        let settings = pv_proxy::Settings {
            owner_id: t.tenant_id.clone(),
            default_destination: t.config.default_destination.clone(),
            private_destination: t.config.private_destination.clone(),
            private_model: t.config.private_model.clone(),
            fallback: pv_core::FallbackSettings {
                tee_consent: t.config.tee_consent,
                mode: if t.config.tee_consent { pv_core::FallbackMode::TeePrivateRoute } else { pv_core::FallbackMode::BlockAndReview },
                local_private_handling: false,
                ..d.fallback.clone()
            },
            retention_days: t.config.metadata_retention_days,
            max_request_bytes: self.cfg.max_request_bytes,
            ..d
        };
        let detector = Arc::new(pv_detectors::Detector::default());
        if let Some(tpl) = &self.span_template {
            *detector.span.write().unwrap() = tpl.span.read().unwrap().clone();
            detector.set_settings(tpl.settings());
        }
        let notifier: Arc<dyn pv_proxy::notify::Notifier> = Arc::new(pv_proxy::notify::Recorder::default());
        let engine = Arc::new(Engine::new(db, vault.clone(), detector, registry, notifier, policy, settings));
        Ok(TenantRuntime { engine, vault })
    }

    pub fn invalidate(&self, tenant_id: &str) {
        self.runtimes.write().unwrap().remove(tenant_id);
    }

    /// Test and operator access to a tenant's engine (e.g. to install a NEAR backend).
    pub fn engine(&self, tenant_id: &str) -> Option<Arc<Engine>> {
        let t = self.store.tenant(tenant_id).ok().flatten()?;
        self.runtime(&t).ok().map(|r| r.engine.clone())
    }

    fn rate_ok(&self, key_id: &str, rpm: u32) -> bool {
        if rpm == 0 {
            return true;
        }
        let mut m = self.buckets.lock().unwrap();
        let b = m.entry(key_id.to_string()).or_insert(Bucket { tokens: rpm as f64, at: Instant::now() });
        let now = Instant::now();
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * rpm as f64 / 60.0).min(rpm as f64);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn housekeeping(&self) {
        let _ = self.store.enforce_retention();
        for r in self.runtimes.read().unwrap().values() {
            r.engine.housekeeping();
            let _ = r.engine.db.expire_routes(chrono::Utc::now() - chrono::Duration::days(r.engine.settings().retention_days as i64));
        }
        let key_ttl = Duration::from_secs(self.cfg.idempotency_key_ttl_secs);
        self.idem.lock().unwrap().retain(|_, e| match e {
            Idem::InProgress { since, .. } => since.elapsed() < Duration::from_secs(3600),
            Idem::Done { at, response_until, response, .. } => {
                if response_until.is_some_and(|u| Instant::now() > u) {
                    *response = None;
                }
                at.elapsed() < key_ttl
            }
        });
    }
}

pub fn router(h: Arc<Hosted>) -> Router {
    let limit = h.cfg.max_request_bytes + 1024;
    Router::new()
        .route("/healthz", get(|| async { Json(json!({"status": "ok", "service": "zfx-vpn-hosted", "version": env!("CARGO_PKG_VERSION")})) }))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/responses", post(responses))
        .route("/v1/models", get(models))
        .route("/v1/zfx/requests/{id}/privacy", get(request_privacy))
        .route("/v1/zfx/privacy-policy", get(privacy_policy))
        .route("/v1/zfx/usage", get(usage))
        .route("/v1/{*rest}", axum::routing::any(|hd: HeaderMap| async move { err(ErrorCode::UnsupportedContent, "Only /v1/chat/completions and /v1/responses are proxied", wants_legacy_errors(&hd)) }))
        .layer(DefaultBodyLimit::max(limit))
        .layer(axum::middleware::from_fn(security_headers))
        .with_state(h)
}

async fn security_headers(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert("cache-control", "no-store".parse().unwrap());
    h.insert("x-content-type-options", "nosniff".parse().unwrap());
    r
}

/// Operator API. Serve it on a separate, private listener.
pub fn admin_router(h: Arc<Hosted>) -> Router {
    Router::new()
        .route("/admin/v1/tenants", post(admin_create_tenant).get(admin_list_tenants))
        .route("/admin/v1/tenants/{id}", put(admin_update_tenant))
        .route("/admin/v1/tenants/{id}/disable", post(admin_disable))
        .route("/admin/v1/tenants/{id}/enable", post(admin_enable))
        .route("/admin/v1/tenants/{id}/keys", post(admin_create_key).get(admin_list_keys))
        .route("/admin/v1/tenants/{id}/keys/{key_id}", delete(admin_revoke_key))
        .route("/admin/v1/tenants/{id}/keys/{key_id}/rotate", post(admin_rotate_key))
        .route("/admin/v1/tenants/{id}/credentials/{cref}", put(admin_put_credential))
        .route("/admin/v1/tenants/{id}/usage", get(admin_usage))
        .layer(axum::middleware::from_fn_with_state(h.clone(), admin_guard))
        .with_state(h)
}

async fn admin_guard(State(h): State<Arc<Hosted>>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let ok = bearer(req.headers()).is_some_and(|t| !h.cfg.admin_token.is_empty() && constant_eq(t.as_bytes(), h.cfg.admin_token.as_bytes()));
    if !ok {
        return err(ErrorCode::Unauthorized, "operator token required", false);
    }
    next.run(req).await
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    let ha = Sha256::digest(a);
    let hb = Sha256::digest(b);
    ha.iter().zip(hb.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn admin_err(e: anyhow::Error) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": {"code": "ZFX_BAD_REQUEST", "message": e.to_string()}}))).into_response()
}

async fn admin_create_tenant(State(h): State<Arc<Hosted>>, Json(cfg): Json<TenantConfig>) -> Response {
    match h.store.create_tenant(&cfg) {
        Ok(t) => (StatusCode::CREATED, Json(t)).into_response(),
        Err(e) => admin_err(e),
    }
}

async fn admin_list_tenants(State(h): State<Arc<Hosted>>) -> Response {
    match h.store.tenants() {
        Ok(t) => Json(json!({"tenants": t})).into_response(),
        Err(e) => admin_err(e),
    }
}

async fn admin_update_tenant(State(h): State<Arc<Hosted>>, Path(id): Path<String>, Json(cfg): Json<TenantConfig>) -> Response {
    match h.store.update_tenant(&id, &cfg) {
        Ok(()) => {
            // Rebuild the engine; sealed credentials live in the tenant vault and survive.
            h.invalidate(&id);
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => admin_err(e),
    }
}

async fn admin_disable(State(h): State<Arc<Hosted>>, Path(id): Path<String>) -> Response {
    Json(json!({"ok": h.store.set_disabled(&id, true).unwrap_or(false)})).into_response()
}

async fn admin_enable(State(h): State<Arc<Hosted>>, Path(id): Path<String>) -> Response {
    Json(json!({"ok": h.store.set_disabled(&id, false).unwrap_or(false)})).into_response()
}

async fn admin_create_key(State(h): State<Arc<Hosted>>, Path(id): Path<String>, Json(v): Json<Value>) -> Response {
    let scopes: Vec<String> = v["scopes"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_else(|| store::SCOPES.iter().map(|s| s.to_string()).collect());
    match h.store.create_key(&id, &scopes) {
        Ok((info, key)) => (StatusCode::CREATED, Json(json!({"key": key, "info": info, "note": "shown once; only a hash is stored"}))).into_response(),
        Err(e) => admin_err(e),
    }
}

async fn admin_list_keys(State(h): State<Arc<Hosted>>, Path(id): Path<String>) -> Response {
    match h.store.keys(&id) {
        Ok(k) => Json(json!({"keys": k})).into_response(),
        Err(e) => admin_err(e),
    }
}

async fn admin_revoke_key(State(h): State<Arc<Hosted>>, Path((id, key_id)): Path<(String, String)>) -> Response {
    Json(json!({"revoked": h.store.revoke_key(&id, &key_id).unwrap_or(false)})).into_response()
}

async fn admin_rotate_key(State(h): State<Arc<Hosted>>, Path((id, key_id)): Path<(String, String)>) -> Response {
    match h.store.rotate_key(&id, &key_id) {
        Ok((info, key)) => Json(json!({"key": key, "info": info})).into_response(),
        Err(e) => admin_err(e),
    }
}

async fn admin_put_credential(State(h): State<Arc<Hosted>>, Path((id, cref)): Path<(String, String)>, Json(v): Json<Value>) -> Response {
    let Some(secret) = v["secret"].as_str() else { return admin_err(anyhow::anyhow!("body must be {{\"secret\": \"...\"}}")) };
    let t = match h.store.tenant(&id) {
        Ok(Some(t)) => t,
        _ => return admin_err(anyhow::anyhow!("no such tenant")),
    };
    match h.runtime(&t).and_then(|r| r.vault.put_sealed("credential", &cref, secret.as_bytes()).map_err(|e| anyhow::anyhow!("{e}"))) {
        Ok(()) => {
            h.invalidate(&id); // NEAR backends pick up the key on rebuild
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => admin_err(e),
    }
}

async fn admin_usage(State(h): State<Arc<Hosted>>, Path(id): Path<String>) -> Response {
    match h.store.month_usage(&id, chrono::Utc::now()) {
        Ok(u) => Json(u).into_response(),
        Err(e) => admin_err(e),
    }
}

/// Authenticate a tenant key and check its scope.
#[allow(clippy::result_large_err)]
fn authorize(h: &Hosted, headers: &HeaderMap, scope: &str) -> Result<(Tenant, store::ApiKeyInfo), Response> {
    let legacy = wants_legacy_errors(headers);
    let Some(key) = bearer(headers) else { return Err(err(ErrorCode::Unauthorized, "Missing ZFX API key (Authorization: Bearer zfx_sk_...)", legacy)) };
    match h.store.authenticate(&key) {
        Ok(Some((t, k))) => {
            if !k.scopes.iter().any(|s| s == scope) {
                return Err(err(ErrorCode::ForbiddenScope, &format!("This key lacks the {scope} scope"), legacy));
            }
            Ok((t, k))
        }
        _ => Err(err(ErrorCode::Unauthorized, "Invalid or revoked ZFX API key", legacy)),
    }
}

async fn chat(State(h): State<Arc<Hosted>>, headers: HeaderMap, body: Bytes) -> Response {
    inference(h, headers, body, pv_provider::Endpoint::Chat).await
}

async fn responses(State(h): State<Arc<Hosted>>, headers: HeaderMap, body: Bytes) -> Response {
    inference(h, headers, body, pv_provider::Endpoint::Responses).await
}

fn usage_of(v: &Value) -> (u64, u64) {
    let u = &v["usage"];
    let p = u["prompt_tokens"].as_u64().or(u["input_tokens"].as_u64()).unwrap_or(0);
    let c = u["completion_tokens"].as_u64().or(u["output_tokens"].as_u64()).unwrap_or(0);
    (p, c)
}

fn trust_headers(r: &mut Response, preprocessed: bool) {
    let h = r.headers_mut();
    h.insert("x-zfx-mode", HeaderValue::from_static(if preprocessed { "hosted-preprocessed" } else { "hosted" }));
    h.insert("x-zfx-trust", HeaderValue::from_static(if preprocessed { "client-preprocessed; operator receives transformed payload only" } else { "operator-sees-raw-prompt; not confidential from ZFX" }));
}

async fn inference(h: Arc<Hosted>, headers: HeaderMap, body: Bytes, endpoint: pv_provider::Endpoint) -> Response {
    let legacy = wants_legacy_errors(&headers);
    let ep = match endpoint {
        pv_provider::Endpoint::Chat => "/v1/chat/completions",
        pv_provider::Endpoint::Responses => "/v1/responses",
    };
    let (tenant, key) = match authorize(&h, &headers, "inference") {
        Ok(x) => x,
        Err(r) => return r,
    };
    let quota = tenant.config.quota.clone();
    if !h.rate_ok(&key.key_id, quota.requests_per_minute) {
        let mut r = err(ErrorCode::RateLimited, "Rate limit exceeded for this key", legacy);
        r.headers_mut().insert("retry-after", HeaderValue::from_static("1"));
        return r;
    }
    if let Ok(u) = h.store.month_usage(&tenant.tenant_id, chrono::Utc::now()) {
        if (quota.requests_per_month > 0 && u.billable_requests >= quota.requests_per_month) || (quota.tokens_per_month > 0 && u.prompt_tokens + u.completion_tokens >= quota.tokens_per_month) {
            return err(ErrorCode::QuotaExceeded, "Monthly quota for this tenant is used up", legacy);
        }
    }
    let preprocessed = headers.get(pv_provider::PREPROCESSED_HEADER).and_then(|v| v.to_str().ok()) == Some("v1");
    let mut body = body.to_vec();
    if preprocessed {
        // The protected transport also sets zfx_privacy.preprocessed; enforce it.
        if let Ok(mut v) = serde_json::from_slice::<Value>(&body) {
            if v.is_object() {
                if !v.get("zfx_privacy").is_some_and(|p| p.is_object()) {
                    v["zfx_privacy"] = json!({});
                }
                v["zfx_privacy"]["preprocessed"] = json!(true);
                body = serde_json::to_vec(&v).unwrap_or(body);
            }
        }
    }

    // Idempotency: never a second upstream call (or charge) for one key.
    let idem_key = header(&headers, "idempotency-key").filter(|k| !k.is_empty());
    let body_hash = hex::encode(Sha256::digest(&body));
    if let Some(k) = &idem_key {
        let mut m = h.idem.lock().unwrap();
        let slot = (tenant.tenant_id.clone(), k.clone());
        match m.get(&slot) {
            Some(Idem::InProgress { body_hash: bh, .. }) | Some(Idem::Done { body_hash: bh, .. }) if *bh != body_hash => {
                return err(ErrorCode::IdempotencyConflict, "Idempotency-Key was already used with a different request body", legacy);
            }
            Some(Idem::InProgress { .. }) => return err(ErrorCode::IdempotencyConflict, "A request with this Idempotency-Key is still in progress", legacy),
            Some(Idem::Done { status, response: Some((v, hdrs)), .. }) => {
                let mut r = (StatusCode::from_u16(*status).unwrap_or(StatusCode::OK), Json(v.clone())).into_response();
                for (k, v) in hdrs {
                    if let (Ok(k), Ok(v)) = (axum::http::HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                        r.headers_mut().insert(k, v);
                    }
                }
                r.headers_mut().insert("idempotent-replayed", HeaderValue::from_static("true"));
                return r;
            }
            Some(Idem::Done { .. }) => return err(ErrorCode::IdempotencyConflict, "A request with this Idempotency-Key already completed; its result is not retained (streams never are). It was not run again", legacy),
            None => {
                m.insert(slot, Idem::InProgress { body_hash: body_hash.clone(), since: Instant::now() });
            }
        }
    }

    let runtime = match h.runtime(&tenant) {
        Ok(r) => r,
        Err(_) => return err(ErrorCode::PolicyInvalid, "Tenant configuration could not be loaded; failing closed", legacy),
    };
    let caller = Caller {
        app_id: format!("{}/{}", tenant.tenant_id, key.key_id),
        agent_id: None,
        destination_override: None,
        conversation: header(&headers, "x-zfx-conversation"),
        source: Source::Client,
        // Fresh per-request map; dropped when the response or stream ends.
        token_store: Some(Arc::new(pv_transform::EphemeralStore::new())),
        hosted: true,
    };
    let result = runtime.engine.handle(caller, endpoint, &body).await;
    let finish_idem = |status: u16, response: Option<Replay>| {
        if let Some(k) = &idem_key {
            let ttl = h.cfg.idempotency_response_ttl_secs;
            let keep = if ttl > 0 { response } else { None };
            let until = keep.as_ref().map(|_| Instant::now() + Duration::from_secs(ttl));
            h.idem.lock().unwrap().insert((tenant.tenant_id.clone(), k.clone()), Idem::Done { body_hash: body_hash.clone(), status, response: keep, at: Instant::now(), response_until: until });
        }
    };
    match result {
        Ok(out) => {
            let route = out.route.clone();
            let tier = route.as_ref().map(|r| r.privacy_tier.as_str().to_string());
            let event_id = out.event_id.clone();
            match out.body {
                Body::Json(v) => {
                    let (p, c) = usage_of(&v);
                    let _ = h.store.record_usage(&tenant.tenant_id, &key.key_id, Some(&event_id), ep, out.status, None, tier.as_deref(), p, c);
                    let mut r = (StatusCode::from_u16(out.status).unwrap_or(StatusCode::OK), Json(v.clone())).into_response();
                    decorate(&mut r, &event_id, route.as_ref(), &out.notices);
                    trust_headers(&mut r, preprocessed);
                    let hdrs: Vec<(String, String)> = r.headers().iter().filter(|(k, _)| k.as_str().starts_with("x-zfx")).map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string())).collect();
                    finish_idem(out.status, Some((v, hdrs)));
                    r
                }
                Body::Stream(s) => {
                    finish_idem(200, None);
                    // Count tokens from usage chunks as they pass; record when the stream ends.
                    let hs = h.clone();
                    let (tid, kid) = (tenant.tenant_id.clone(), key.key_id.clone());
                    let acc = Arc::new(Mutex::new((0u64, 0u64)));
                    let acc2 = acc.clone();
                    let counted = s.map(move |b: Bytes| {
                        if let Ok(t) = std::str::from_utf8(&b) {
                            for line in t.lines().filter_map(|l| l.strip_prefix("data: ")) {
                                if let Ok(v) = serde_json::from_str::<Value>(line) {
                                    let u = if v.get("usage").is_some() { usage_of(&v) } else { usage_of(&v["response"]) };
                                    if u != (0, 0) {
                                        *acc2.lock().unwrap() = u;
                                    }
                                }
                            }
                        }
                        Ok::<_, std::io::Error>(b)
                    });
                    let done = futures::stream::once(async move {
                        let (p, c) = *acc.lock().unwrap();
                        let _ = hs.store.record_usage(&tid, &kid, Some(&event_id), ep, 200, None, tier.as_deref(), p, c);
                        Ok::<_, std::io::Error>(Bytes::new())
                    });
                    let mut r = Response::new(AxBody::from_stream(counted.chain(done)));
                    r.headers_mut().insert("content-type", "text/event-stream".parse().unwrap());
                    decorate(&mut r, &out.event_id, route.as_ref(), &out.notices);
                    trust_headers(&mut r, preprocessed);
                    r
                }
            }
        }
        Err(e) => {
            let _ = h.store.record_usage(&tenant.tenant_id, &key.key_id, e.event_id.as_deref(), ep, e.code.http_status(), Some(e.code.as_str()), None, 0, 0);
            let mut r = error_response_compat(&e, legacy);
            trust_headers(&mut r, preprocessed);
            finish_idem(e.code.http_status(), Some((e.envelope_json(legacy), vec![])));
            r
        }
    }
}

async fn models(State(h): State<Arc<Hosted>>, headers: HeaderMap) -> Response {
    let (tenant, _) = match authorize(&h, &headers, "models:read") {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Ok(rt) = h.runtime(&tenant) else { return err(ErrorCode::PolicyInvalid, "tenant unavailable", false) };
    let mut data = vec![];
    for p in rt.engine.registry.providers().into_iter().filter(|p| p.enabled) {
        let r = rt.engine.registry.resolve(&p, None).await;
        data.push(json!({"id": format!("{}/{}", p.id, p.default_model), "object": "model", "created": 0, "owned_by": format!("zfx-vpn-hosted:{}", p.id), "privacy_tier": PrivacyTier::of(p.kind, r.tier).as_str()}));
    }
    if let (Some(pd), Some(m)) = (&tenant.config.private_destination, &tenant.config.private_model) {
        if let Some(p) = rt.engine.registry.get(pd) {
            let r = rt.engine.registry.resolve(&p, Some(m)).await;
            data.push(json!({"id": format!("{}/{}", p.id, m), "object": "model", "created": 0, "owned_by": format!("zfx-vpn-hosted:{}", p.id), "privacy_tier": PrivacyTier::of(p.kind, r.tier).as_str(), "zfx_role": "confidential_fallback"}));
        }
    }
    Json(json!({"object": "list", "data": data})).into_response()
}

async fn request_privacy(State(h): State<Arc<Hosted>>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let legacy = wants_legacy_errors(&headers);
    let (tenant, key) = match authorize(&h, &headers, "privacy:read") {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Ok(rt) = h.runtime(&tenant) else { return err(ErrorCode::NotFound, "not found", legacy) };
    // Route records are per tenant and per key; other tenants' ids are just not found.
    match rt.engine.db.route(&id, Some(&format!("{}/{}", tenant.tenant_id, key.key_id))) {
        Ok(Some(r)) => Json(r).into_response(),
        _ => err(ErrorCode::NotFound, "No route record for this request id", legacy),
    }
}

async fn usage(State(h): State<Arc<Hosted>>, headers: HeaderMap) -> Response {
    let (tenant, _) = match authorize(&h, &headers, "privacy:read") {
        Ok(x) => x,
        Err(r) => return r,
    };
    match h.store.month_usage(&tenant.tenant_id, chrono::Utc::now()) {
        Ok(u) => Json(json!({"usage": u, "quota": tenant.config.quota})).into_response(),
        Err(_) => err(ErrorCode::NotFound, "usage unavailable", false),
    }
}

async fn privacy_policy() -> Json<Value> {
    Json(json!({
        "service": "zfx-vpn-hosted",
        "modes": {
            "hosted": {"operator_sees_raw_prompts": true, "disclosure": DISCLOSURE},
            "hosted-preprocessed": {"operator_sees_raw_prompts": false, "operator_sees_placeholder_map": false, "disclosure": PREPROCESSED_DISCLOSURE},
        },
        "zero_knowledge": false,
        "enclave": false,
        "raw_retention": "none: request and response content is never written to disk; placeholder maps are per request, in memory",
        "metadata_retention_days_default": 30,
        "privacy_tiers": ["LOCAL_ONLY", "TRANSFORMED_PUBLIC", "ATTESTED_CONFIDENTIAL", "UNVERIFIED_EXTERNAL"],
        "error_codes_version": pv_core::errors::ERROR_CODES_VERSION,
    }))
}
