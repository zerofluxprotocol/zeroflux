//! Daemon state: paths, bootstrap from SQLite + vault, live reconfiguration.

use crate::models::{ModelManager, SpanSelection};
use anyhow::{anyhow, Context, Result};
use pv_audit::{Db, JsonTable};
use pv_core::*;
use pv_detectors::span::CustomEntity;
use pv_detectors::{Detector, DetectorSettings};
use pv_provider::{CredentialFn, Registry};
use pv_proxy::notify::{Desktop, NotificationSettings, Notifier, RateLimited, Recorder};
use pv_proxy::{Engine, Settings};
use pv_vault::keys::{FileBackend, KeyBackend, MemoryBackend};
use pv_vault::Vault;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroizing;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_PORT: u16 = 8787;

/// `directories::ProjectDirs` triple for the data directory.
pub const PRODUCT_DIR: (&str, &str, &str) = ("network", "zeroflux", "zfx-vpn");
/// Prompt VPN 0.1 data directory triple (migrated on first run).
pub const LEGACY_PRODUCT_DIR: (&str, &str, &str) = ("dev", "promptvpn", "promptvpn");

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    /// Database file name (`zfx-vpn.db`; `promptvpn.db` only while a Prompt VPN
    /// 0.1 daemon is still running from an unmigrated install).
    pub db_name: String,
    /// An unmigrated Prompt VPN install whose daemon is running: commands talk
    /// to it over HTTP; migration happens on the next daemon start.
    pub legacy_running: bool,
}

impl Paths {
    /// Data directory for this process, without side effects. `ZFX_VPN_HOME`
    /// overrides, then the legacy `PV_HOME`, then the OS data dir.
    pub fn resolve() -> Paths {
        let env = crate::migrate::Env::from_process();
        match crate::migrate::plan(&env) {
            crate::migrate::Plan::Ready { data } => Paths::at(data),
            crate::migrate::Plan::Migrate { from, to, .. } => {
                if crate::migrate::daemon_running(&from) {
                    Paths { data: from, db_name: crate::migrate::LEGACY_DB_FILE.into(), legacy_running: true }
                } else {
                    Paths::at(to)
                }
            }
        }
    }

    pub fn at(data: impl Into<PathBuf>) -> Paths {
        Paths { data: data.into(), db_name: crate::migrate::DB_FILE.into(), legacy_running: false }
    }

    /// Resolve and, if a Prompt VPN 0.1 install is found (and its daemon is
    /// stopped), migrate it now.
    pub fn resolve_and_migrate() -> Result<(Paths, Option<crate::migrate::MigrationRecord>)> {
        let p = Paths::resolve();
        if p.legacy_running {
            return Ok((p, None));
        }
        let env = crate::migrate::Env::from_process();
        let (data, rec) = crate::migrate::ensure(&env, secret_store().as_ref())?;
        Ok((Paths::at(data), rec))
    }

    pub fn db(&self) -> PathBuf {
        self.data.join(&self.db_name)
    }
    pub fn admin_token(&self) -> PathBuf {
        self.data.join("admin.token")
    }
    pub fn key_file(&self) -> PathBuf {
        self.data.join("keys.insecure.json")
    }
    pub fn pid(&self) -> PathBuf {
        self.data.join("daemon.pid")
    }
    pub fn port_file(&self) -> PathBuf {
        self.data.join("daemon.port")
    }
    pub fn log(&self) -> PathBuf {
        self.data.join("daemon.log")
    }
    pub fn models(&self) -> PathBuf {
        self.data.join("models")
    }
    pub fn app_key(&self, app: &str) -> PathBuf {
        self.data.join("apps").join(format!("{app}.key"))
    }
    pub fn backups(&self) -> PathBuf {
        self.data.join("client-backups")
    }
}

pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(data)?;
    f.sync_all()
}

pub fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

static SECRET_STORE: std::sync::OnceLock<Arc<dyn crate::migrate::SecretStore>> = std::sync::OnceLock::new();

/// OS credential store used for the key set and provider credentials.
pub fn secret_store() -> Arc<dyn crate::migrate::SecretStore> {
    SECRET_STORE.get_or_init(|| Arc::new(crate::migrate::OsSecretStore)).clone()
}

/// Tests: replace the OS keychain with an in-memory store (first call wins).
pub fn use_secret_store(s: Arc<dyn crate::migrate::SecretStore>) -> Arc<dyn crate::migrate::SecretStore> {
    SECRET_STORE.get_or_init(|| s).clone()
}

fn sha2_digest(b: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(b).to_vec()
}

/// Keychain account for a data path: `keyset:<first 16 hex of sha256(path)>`.
pub fn key_account(path: &str) -> String {
    format!("keyset:{}", &hex::encode(sha2_digest(path.as_bytes()))[..16])
}

/// Key set stored in the OS credential store.
pub struct StoreKeyBackend {
    store: Arc<dyn crate::migrate::SecretStore>,
    service: String,
    account: String,
    label: String,
}

impl StoreKeyBackend {
    pub fn new(store: Arc<dyn crate::migrate::SecretStore>, service: &str, account: &str) -> Self {
        StoreKeyBackend { store, service: service.into(), account: account.into(), label: format!("os-keychain({service}/{account})") }
    }
}

impl KeyBackend for StoreKeyBackend {
    fn name(&self) -> &str {
        &self.label
    }
    fn secure(&self) -> bool {
        true
    }
    fn load(&self) -> std::result::Result<Option<pv_vault::keys::KeySet>, String> {
        match self.store.get(&self.service, &self.account)? {
            Some(s) => pv_vault::keys::KeySet::from_bytes(s.as_bytes()).map(Some),
            None => Ok(None),
        }
    }
    fn store(&self, ks: &pv_vault::keys::KeySet) -> std::result::Result<(), String> {
        let b = ks.to_bytes();
        let s = Zeroizing::new(String::from_utf8_lossy(&b).into_owned());
        self.store.set(&self.service, &self.account, &s)?;
        // Read back: some Linux backends accept writes they cannot persist.
        match self.load()? {
            Some(back) if back.active == ks.active => Ok(()),
            _ => Err("keychain write did not persist; secure storage unavailable".into()),
        }
    }
    fn delete(&self) -> std::result::Result<(), String> {
        self.store.delete(&self.service, &self.account)
    }
}

/// Which key backend to use (`ZFX_KEY_BACKEND`, legacy `PV_KEY_BACKEND`):
/// `keychain` (default), `file` (degraded), `memory` (tests).
pub fn key_backend(paths: &Paths) -> Result<Arc<dyn KeyBackend>> {
    let kind = std::env::var("ZFX_KEY_BACKEND").or_else(|_| std::env::var("PV_KEY_BACKEND"));
    match kind.as_deref() {
        Ok("file") => Ok(Arc::new(FileBackend { path: paths.key_file() })),
        Ok("memory") => Ok(Arc::new(MemoryBackend::default())),
        _ => {
            let abs = crate::migrate::canonical_future(&paths.data);
            Ok(Arc::new(StoreKeyBackend::new(secret_store(), crate::migrate::SERVICE, &key_account(&abs.to_string_lossy()))))
        }
    }
}

/// Owner-defined entity type stored in the vault (sealed) with its policy action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEntity {
    pub entity: CustomEntity,
    pub action: Action,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OnboardingState {
    pub completed: bool,
    pub completed_at: Option<String>,
    pub version: u32,
    pub near_skipped: bool,
}

pub struct AppState {
    pub paths: Paths,
    pub engine: Arc<Engine>,
    pub models: Arc<ModelManager>,
    pub admin_token: Zeroizing<String>,
    pub sessions: crate::auth::Sessions,
    pub port: std::sync::atomic::AtomicU16,
    pub started: chrono::DateTime<chrono::Utc>,
    pub key_backend: Arc<dyn KeyBackend>,
    pub recorder: Arc<Recorder>,
    pub notify_settings: Arc<RateLimited<Arc<dyn Notifier>>>,
    pub shutdown: tokio::sync::Notify,
    pub creds: Arc<Credentials>,
}

/// Credential store: OS keychain, or the vault when running on the file backend.
pub struct Credentials {
    vault: Arc<Vault>,
    use_keychain: bool,
    store: Arc<dyn crate::migrate::SecretStore>,
}

impl Credentials {
    pub fn new(vault: Arc<Vault>, use_keychain: bool) -> Self {
        Credentials { vault, use_keychain, store: secret_store() }
    }
    pub fn set(&self, r: &str, secret: &str) -> Result<()> {
        if self.use_keychain {
            self.store.set(crate::migrate::SERVICE, &format!("cred:{r}"), secret).map_err(|e| anyhow!(e))?;
        } else {
            self.vault.put_sealed("credential", r, secret.as_bytes())?;
        }
        Ok(())
    }
    pub fn get(&self, r: &str) -> Option<Zeroizing<String>> {
        if self.use_keychain {
            self.store.get(crate::migrate::SERVICE, &format!("cred:{r}")).ok().flatten()
        } else {
            self.vault.get_sealed("credential", r).ok().flatten().map(|b| Zeroizing::new(String::from_utf8_lossy(&b).into_owned()))
        }
    }
    pub fn delete(&self, r: &str) {
        if self.use_keychain {
            let _ = self.store.delete(crate::migrate::SERVICE, &format!("cred:{r}"));
        } else {
            let _ = self.vault.delete_sealed("credential", r);
        }
    }
}

pub struct Boot {
    pub state: Arc<AppState>,
}

pub fn load_or_create_admin_token(paths: &Paths) -> Result<Zeroizing<String>> {
    match std::fs::read_to_string(paths.admin_token()) {
        Ok(s) if s.trim().len() >= 32 => Ok(Zeroizing::new(s.trim().to_string())),
        _ => {
            let t = format!("zfxa_{}", random_hex(32));
            write_private(&paths.admin_token(), t.as_bytes()).context("write admin token")?;
            Ok(Zeroizing::new(t))
        }
    }
}

/// Open everything. `notifier` override is used by tests.
pub async fn bootstrap(paths: Paths, notifier: Option<Arc<dyn Notifier>>) -> Result<Arc<AppState>> {
    let backend = key_backend(&paths)?;
    bootstrap_with_backend(paths, notifier, backend).await
}

/// `bootstrap` with an explicit key backend (tests, migration checks).
pub async fn bootstrap_with_backend(paths: Paths, notifier: Option<Arc<dyn Notifier>>, backend: Arc<dyn KeyBackend>) -> Result<Arc<AppState>> {
    std::fs::create_dir_all(&paths.data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&paths.data, std::fs::Permissions::from_mode(0o700));
    }
    let db = Db::open(&paths.db()).context("open database")?;
    let vault = Arc::new(Vault::open(db.clone(), backend.clone()).map_err(|e| anyhow!("{e}"))?);
    let creds = Arc::new(Credentials::new(vault.clone(), backend.secure()));
    let cf = creds.clone();
    let cred_fn: CredentialFn = Arc::new(move |r: &str| cf.get(r));
    let registry = Arc::new(Registry::new(cred_fn));

    // Policies: install defaults on first run.
    if db.active_policies()?.is_empty() {
        for r in default_policy(&default_category_choices()).rules {
            db.put_policy(&r, "builtin")?;
        }
    }
    let policy = PolicySet { rules: db.active_policies()? };
    policy.validate().map_err(|e| anyhow!("stored policy invalid: {e}"))?;
    let settings: Settings = db.setting("engine")?.unwrap_or_default();

    let detector = Arc::new(Detector::default());
    detector.set_settings(db.setting::<DetectorSettings>("detector")?.unwrap_or_default());

    let recorder = Arc::new(Recorder::default());
    let base: Arc<dyn Notifier> = notifier.unwrap_or_else(|| Arc::new(Desktop));
    let fanout: Arc<dyn Notifier> = Arc::new(Fanout(vec![base, recorder.clone()]));
    let nsettings: NotificationSettings = db.setting("notifications")?.unwrap_or_default();
    let rl = Arc::new(RateLimited::new(fanout, nsettings));

    let engine = Arc::new(Engine::new(db.clone(), vault.clone(), detector.clone(), registry.clone(), rl.clone(), policy, settings));
    engine.reload_exact().map_err(|e| anyhow!(e))?;

    let models = Arc::new(ModelManager::new(paths.clone(), detector.clone()));
    let state = Arc::new(AppState {
        admin_token: load_or_create_admin_token(&paths)?,
        paths,
        engine,
        models,
        sessions: crate::auth::Sessions::default(),
        port: std::sync::atomic::AtomicU16::new(DEFAULT_PORT),
        started: chrono::Utc::now(),
        key_backend: backend,
        recorder,
        notify_settings: rl,
        shutdown: tokio::sync::Notify::new(),
        creds,
    });
    reload_entities(&state)?;
    reload_providers(&state)?;
    // Load the detection model off the startup path. Until it is loaded, an
    // installed model counts as required, so requests fail closed to review.
    let sel: SpanSelection = state.engine.db.setting("span_model")?.unwrap_or_default();
    if state.models.provisioned(&sel.model).is_ok() {
        let mut ds = state.engine.detector.settings();
        ds.span_required = true;
        state.engine.detector.set_settings(ds);
    }
    let st2 = state.clone();
    let model = sel.model.clone();
    tokio::task::spawn_blocking(move || {
        apply_span(&st2, &model);
    });
    state.engine.housekeeping();
    Ok(state)
}

struct Fanout(Vec<Arc<dyn Notifier>>);
impl Notifier for Fanout {
    fn notify(&self, n: pv_proxy::notify::Notification) {
        for x in &self.0 {
            x.notify(n.clone());
        }
    }
}

impl AppState {
    pub fn creds(&self) -> Arc<Credentials> {
        self.creds.clone()
    }
    pub fn db(&self) -> &Db {
        &self.engine.db
    }
    pub fn vault(&self) -> &Arc<Vault> {
        &self.engine.vault
    }
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port.load(std::sync::atomic::Ordering::Relaxed))
    }
}

pub fn entities(state: &AppState) -> Result<Vec<StoredEntity>> {
    let mut out = Vec::new();
    for (_, b) in state.vault().list_sealed("entity")? {
        out.push(serde_json::from_slice(&b)?);
    }
    Ok(out)
}

pub fn reload_entities(state: &AppState) -> Result<()> {
    state.engine.detector.set_entities(entities(state)?.into_iter().map(|e| e.entity).collect());
    Ok(())
}

/// Load the span model. Once a model is installed it is required: if it later
/// fails to load or run, requests fail closed (CLASSIFIER_FAILURE) instead of
/// silently losing detection.
pub fn apply_span(state: &AppState, model: &str) -> bool {
    let loaded = state.models.load(model);
    let installed = state.models.provisioned(model).is_ok();
    let mut ds = state.engine.detector.settings();
    ds.span_required = installed;
    state.engine.detector.set_settings(ds);
    loaded
}

pub fn reload_policy(state: &AppState) -> Result<()> {
    let p = PolicySet { rules: state.db().active_policies()? };
    p.validate().map_err(|e| anyhow!("{e}"))?;
    state.engine.set_policy(p);
    Ok(())
}

pub fn reload_providers(state: &AppState) -> Result<()> {
    let providers: Vec<ProviderConfig> = state.db().list_json(JsonTable::Providers)?;
    for p in providers.iter().filter(|p| p.kind == ProviderKind::NearAi) {
        let key = p.credential_ref.as_deref().and_then(|r| state.creds().get(r)).map(|k| k.to_string());
        if std::env::var("ZFX_NEAR_MOCK").is_ok() || std::env::var("PV_NEAR_MOCK").is_ok() {
            continue; // tests install mock backends explicitly
        }
        match pv_provider::near::NearSdkBackend::new(&p.base_url, key) {
            Ok(b) => state.engine.registry.set_near_backend(&p.id, Arc::new(b)),
            Err(e) => tracing::error!("NEAR backend {}: {e}", p.id),
        }
    }
    state.engine.registry.set_providers(providers);
    Ok(())
}

/// Persist engine settings and apply them live.
pub fn save_settings(state: &AppState, s: &Settings, actor: &str) -> Result<()> {
    state.db().set_setting("engine", s, actor)?;
    state.engine.set_settings(s.clone());
    Ok(())
}

/// Create a local caller app; returns the key (shown once).
pub fn create_app(state: &AppState, name: &str, agent_id: Option<String>, destination_id: Option<String>) -> Result<(pv_audit::AppRecord, String)> {
    let key = format!("zfxk_{}", random_hex(24));
    let rec = pv_audit::AppRecord {
        app_id: format!("app_{}", random_hex(6)),
        name: name.to_string(),
        key_hash: crate::auth::key_hash(&key),
        agent_id,
        destination_id,
        created_at: chrono::Utc::now().to_rfc3339(),
        revoked_at: None,
    };
    state.db().put_app(&rec)?;
    Ok((rec, key))
}
