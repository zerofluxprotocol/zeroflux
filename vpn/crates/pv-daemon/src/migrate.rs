//! Upgrade of a Prompt VPN 0.1 install to ZFX VPN, and rollback.
//!
//! What changes on upgrade, and nothing else:
//! * data directory: the old default (`dev.promptvpn.promptvpn`) is renamed to
//!   the new default (`network.zeroflux.zfx-vpn`) and a symlink is left at the
//!   old path. With `PV_HOME`/`ZFX_VPN_HOME` the directory stays where it is.
//! * database file `promptvpn.db` -> `zfx-vpn.db`, schema 1 -> current
//!   (additive tables only), after a byte copy backup in `backups/`.
//! * OS keychain: the key set and provider credentials are COPIED from service
//!   `promptvpn` to service `zfx-vpn` (account derived from the new path) and
//!   read back byte-for-byte. The old entries are kept for rollback.
//! * Nothing is decrypted or re-encrypted: vault rows, mappings, sealed
//!   records, pending payloads and the audit MAC chain keep their bytes.
//!
//! Rollback reverses each step: downgrade the schema, write the current key set
//! back to the old keychain entry, rename the database and directory back,
//! remove the new keychain entries and the migration record.

use crate::state::{key_account, PRODUCT_DIR, LEGACY_PRODUCT_DIR};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zeroize::Zeroizing;

pub const SERVICE: &str = "zfx-vpn";
pub const LEGACY_SERVICE: &str = "promptvpn";
pub const DB_FILE: &str = "zfx-vpn.db";
pub const LEGACY_DB_FILE: &str = "promptvpn.db";
pub const RECORD_FILE: &str = "migration.json";
pub const ROLLBACK_RECORD_FILE: &str = "rollback-from-zfx-vpn.json";
pub const RECORD_FORMAT: &str = "zfx-vpn-migration-v1";

/// OS credential store abstraction (Keychain / Secret Service), so migration
/// logic is testable without touching the real keychain.
pub trait SecretStore: Send + Sync {
    fn get(&self, service: &str, account: &str) -> Result<Option<Zeroizing<String>>, String>;
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, service: &str, account: &str) -> Result<(), String>;
}

/// The real OS store via the `keyring` crate.
pub struct OsSecretStore;

impl SecretStore for OsSecretStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<Zeroizing<String>>, String> {
        let e = keyring::Entry::new(service, account).map_err(|e| e.to_string())?;
        match e.get_password() {
            Ok(s) => Ok(Some(Zeroizing::new(s))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String> {
        keyring::Entry::new(service, account).map_err(|e| e.to_string())?.set_password(value).map_err(|e| e.to_string())
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        match keyring::Entry::new(service, account).map_err(|e| e.to_string())?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemorySecretStore {
    pub items: Mutex<HashMap<(String, String), String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<Zeroizing<String>>, String> {
        Ok(self.items.lock().unwrap().get(&(service.to_string(), account.to_string())).map(|s| Zeroizing::new(s.clone())))
    }
    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), String> {
        self.items.lock().unwrap().insert((service.to_string(), account.to_string()), value.to_string());
        Ok(())
    }
    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        self.items.lock().unwrap().remove(&(service.to_string(), account.to_string()));
        Ok(())
    }
}

/// Environment inputs for locating data (injectable for tests).
#[derive(Debug, Clone)]
pub struct Env {
    /// `ZFX_VPN_HOME`
    pub zfx_home: Option<PathBuf>,
    /// `PV_HOME` (legacy name, still honored)
    pub pv_home: Option<PathBuf>,
    pub default_dir: PathBuf,
    pub legacy_default_dir: PathBuf,
    /// `keychain` (default), `file`, `memory` from ZFX_KEY_BACKEND / PV_KEY_BACKEND.
    pub key_backend: String,
}

impl Env {
    pub fn from_process() -> Env {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let pd = |q: &str, o: &str, a: &str, fallback: &str| directories::ProjectDirs::from(q, o, a).map(|p| p.data_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from(fallback));
        Env {
            zfx_home: var("ZFX_VPN_HOME"),
            pv_home: var("PV_HOME"),
            default_dir: pd(PRODUCT_DIR.0, PRODUCT_DIR.1, PRODUCT_DIR.2, ".zfx-vpn"),
            legacy_default_dir: pd(LEGACY_PRODUCT_DIR.0, LEGACY_PRODUCT_DIR.1, LEGACY_PRODUCT_DIR.2, ".promptvpn"),
            key_backend: std::env::var("ZFX_KEY_BACKEND").or_else(|_| std::env::var("PV_KEY_BACKEND")).unwrap_or_else(|_| "keychain".into()),
        }
    }
    pub fn explicit_home(&self) -> Option<&PathBuf> {
        self.zfx_home.as_ref().or(self.pv_home.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// Use `data` as is (already ZFX VPN, or a fresh install).
    Ready { data: PathBuf },
    /// A Prompt VPN 0.1 install was found.
    Migrate { from: PathBuf, to: PathBuf, in_place: bool },
}

pub fn plan(env: &Env) -> Plan {
    let target = env.explicit_home().cloned().unwrap_or_else(|| env.default_dir.clone());
    if target.join(DB_FILE).exists() {
        return Plan::Ready { data: target };
    }
    if target.join(LEGACY_DB_FILE).exists() {
        return Plan::Migrate { from: target.clone(), to: target, in_place: true };
    }
    if env.explicit_home().is_none() {
        let l = &env.legacy_default_dir;
        let is_link = std::fs::symlink_metadata(l).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if !is_link && l.join(LEGACY_DB_FILE).exists() {
            return Plan::Migrate { from: l.clone(), to: target, in_place: false };
        }
        // Migrated in place at the old default path (e.g. by a Prompt VPN
        // service unit that set PV_HOME): keep using it.
        if !is_link && l.join(DB_FILE).exists() {
            return Plan::Ready { data: l.clone() };
        }
    }
    Plan::Ready { data: target }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MigrationRecord {
    pub format: String,
    pub from_product: String,
    pub from_dir: PathBuf,
    pub to_dir: PathBuf,
    pub in_place: bool,
    pub key_backend: String,
    pub legacy_keychain: Option<(String, String)>,
    pub keychain: Option<(String, String)>,
    pub credential_refs: Vec<String>,
    pub schema_from: i64,
    pub schema_to: i64,
    pub backup_file: PathBuf,
    pub migrated_at: String,
}

/// Whether a Prompt VPN / ZFX VPN daemon is serving from `dir` right now.
pub fn daemon_running(dir: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(dir.join("daemon.pid")) else { return false };
    let Ok(pid) = pid.trim().parse::<u32>() else { return false };
    std::process::Command::new("kill").arg("-0").arg(pid.to_string()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// Absolute path for `p` even if it does not exist yet (canonical parent).
pub fn canonical_future(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => canonical_future(parent).join(name),
        _ => p.to_path_buf(),
    }
}

/// Keychain account Prompt VPN 0.1 used: hash of the data path as given.
pub fn legacy_key_account(dir_as_given: &Path) -> String {
    key_account(&dir_as_given.to_string_lossy())
}

fn credential_refs(db_path: &Path) -> Result<Vec<String>> {
    let c = pv_audit::rusqlite::Connection::open(db_path)?;
    let mut st = c.prepare("SELECT json FROM providers")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for j in rows {
        let v: serde_json::Value = serde_json::from_str(&j?)?;
        if let Some(r) = v.get("credential_ref").and_then(|r| r.as_str()) {
            if !out.contains(&r.to_string()) {
                out.push(r.to_string());
            }
        }
    }
    Ok(out)
}

fn copy_verified(store: &dyn SecretStore, from: (&str, &str), to: (&str, &str)) -> Result<bool> {
    let Some(v) = store.get(from.0, from.1).map_err(|e| anyhow!("keychain read {}/{}: {e}", from.0, from.1))? else { return Ok(false) };
    store.set(to.0, to.1, &v).map_err(|e| anyhow!("keychain write {}/{}: {e}", to.0, to.1))?;
    let back = store.get(to.0, to.1).map_err(|e| anyhow!("keychain read-back: {e}"))?;
    if back.as_deref().map(|s| s.as_str()) != Some(v.as_str()) {
        bail!("keychain entry {}/{} did not read back identically; migration stopped (old entry untouched)", to.0, to.1);
    }
    Ok(true)
}

fn private_copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::copy(from, to)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Run the upgrade described by `Plan::Migrate`.
pub fn migrate(from: &Path, to: &Path, in_place: bool, key_backend: &str, store: &dyn SecretStore) -> Result<MigrationRecord> {
    if daemon_running(from) {
        bail!("a Prompt VPN daemon is running from {}; stop it first (`zfx-vpn stop` or `promptvpn stop`), then run any zfx-vpn command again", from.display());
    }
    let legacy_db = from.join(LEGACY_DB_FILE);
    // 1. Settle the WAL so the file alone is the whole database; read the version.
    let schema_from: i64 = {
        let c = pv_audit::rusqlite::Connection::open(&legacy_db).context("open legacy database")?;
        let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let _ = c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        v
    };
    if schema_from > pv_audit::SCHEMA_VERSION {
        bail!("database schema {schema_from} is newer than this zfx-vpn supports ({})", pv_audit::SCHEMA_VERSION);
    }
    // 2. Byte copy backup inside the data dir (moves with it).
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let backup_rel = PathBuf::from("backups").join(format!("promptvpn.db.pre-zfx-vpn-{ts}"));
    private_copy(&legacy_db, &from.join(&backup_rel)).context("back up legacy database")?;

    // 3. Keychain: copy, never move. Old entries stay for rollback.
    let to_abs = canonical_future(to);
    let refs = credential_refs(&legacy_db).unwrap_or_default();
    let (legacy_kc, new_kc) = if key_backend == "keychain" {
        if cfg!(target_os = "macos") {
            eprintln!("Copying the Prompt VPN data key to the ZFX VPN keychain entry. macOS may ask whether zfx-vpn may read the item \"promptvpn\" (it was created by the old binary); choose Always Allow. Nothing changes if you deny.");
        }
        let old = (LEGACY_SERVICE.to_string(), legacy_key_account(from));
        let new = (SERVICE.to_string(), key_account(&to_abs.to_string_lossy()));
        let copied = copy_verified(store, (&old.0, &old.1), (&new.0, &new.1))?;
        if !copied {
            let has_records: i64 = pv_audit::rusqlite::Connection::open(&legacy_db)?.query_row(
                "SELECT (SELECT COUNT(*) FROM secrets)+(SELECT COUNT(*) FROM mappings)+(SELECT COUNT(*) FROM sealed)+(SELECT COUNT(*) FROM pending_payloads)+(SELECT COUNT(*) FROM events)",
                [],
                |r| r.get(0),
            )?;
            if has_records > 0 {
                bail!("the Prompt VPN data key was not found in the OS keychain ({}/{}) but encrypted records exist; nothing was changed. Unlock the keychain or restore with `zfx-vpn vault recover`", old.0, old.1);
            }
        }
        for r in &refs {
            let a = format!("cred:{r}");
            copy_verified(store, (LEGACY_SERVICE, &a), (SERVICE, &a))?;
        }
        (Some(old), Some(new))
    } else {
        (None, None)
    };

    // 4. Directory and database file.
    if !in_place {
        if to.exists() {
            let empty = std::fs::read_dir(to).map(|mut d| d.next().is_none()).unwrap_or(false);
            if !empty {
                bail!("{} already exists and is not empty; refusing to merge installs", to.display());
            }
            std::fs::remove_dir(to)?;
        }
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::rename(from, to).with_context(|| format!("move {} to {}", from.display(), to.display()))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(to, from).with_context(|| format!("leave a link at {}", from.display()))?;
    }
    std::fs::rename(to.join(LEGACY_DB_FILE), to.join(DB_FILE)).context("rename database")?;
    for ext in ["-wal", "-shm"] {
        let a = to.join(format!("{LEGACY_DB_FILE}{ext}"));
        if a.exists() {
            std::fs::rename(&a, to.join(format!("{DB_FILE}{ext}")))?;
        }
    }

    // 5. Schema upgrade (additive) and product marker.
    let db = pv_audit::Db::open(&to.join(DB_FILE)).context("open migrated database")?;
    db.meta_set("product", "zfx-vpn")?;
    db.meta_set("migrated_from", &format!("promptvpn schema {schema_from}"))?;
    let schema_to = db.schema_version()?;
    drop(db);

    let rec = MigrationRecord {
        format: RECORD_FORMAT.into(),
        from_product: "promptvpn".into(),
        from_dir: from.to_path_buf(),
        to_dir: to.to_path_buf(),
        in_place,
        key_backend: key_backend.to_string(),
        legacy_keychain: legacy_kc,
        keychain: new_kc,
        credential_refs: refs,
        schema_from,
        schema_to,
        backup_file: backup_rel,
        migrated_at: chrono::Utc::now().to_rfc3339(),
    };
    crate::state::write_private(&to.join(RECORD_FILE), serde_json::to_string_pretty(&rec)?.as_bytes())?;
    Ok(rec)
}

/// Locate data, migrating a Prompt VPN install first if one is found.
/// Concurrent invocations (e.g. two CLI commands started together) serialize
/// on a lock file next to the legacy directory; the loser re-plans and finds
/// the migration done.
pub fn ensure(env: &Env, store: &dyn SecretStore) -> Result<(PathBuf, Option<MigrationRecord>)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let (from, to, in_place) = match plan(env) {
            Plan::Ready { data } => return Ok((data, None)),
            Plan::Migrate { from, to, in_place } => (from, to, in_place),
        };
        let lock = from.parent().unwrap_or(Path::new(".")).join(format!(".{}.zfx-vpn-migration.lock", from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(_) => {
                let r = match plan(env) {
                    Plan::Migrate { .. } => migrate(&from, &to, in_place, &env.key_backend, store).map(|rec| (to.clone(), Some(rec))),
                    Plan::Ready { data } => Ok((data, None)),
                };
                let _ = std::fs::remove_file(&lock);
                return r;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Stale lock from a crashed run: older than 2 minutes.
                let stale = std::fs::metadata(&lock).and_then(|m| m.modified()).map(|t| t.elapsed().unwrap_or_default() > std::time::Duration::from_secs(120)).unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(&lock);
                    continue;
                }
                if std::time::Instant::now() > deadline {
                    bail!("another zfx-vpn process is migrating {} (lock {}); try again", from.display(), lock.display());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(anyhow!("create migration lock {}: {e}", lock.display())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackReport {
    pub restored_dir: PathBuf,
    pub schema: i64,
    pub keychain_restored: bool,
    pub credentials_restored: usize,
}

/// Hand a migrated install back to Prompt VPN 0.1.
pub fn rollback(data: &Path, store: &dyn SecretStore) -> Result<RollbackReport> {
    let rec_path = data.join(RECORD_FILE);
    let rec: MigrationRecord = serde_json::from_slice(&std::fs::read(&rec_path).with_context(|| format!("{} not found: this install was not migrated from Prompt VPN", rec_path.display()))?)?;
    if rec.format != RECORD_FORMAT {
        bail!("unknown migration record format {}", rec.format);
    }
    if daemon_running(data) {
        bail!("the daemon is running; stop it first (`zfx-vpn stop`)");
    }
    // 1. Schema back to Prompt VPN 0.1.
    let schema = pv_audit::Db::downgrade_file(&data.join(DB_FILE), pv_audit::LEGACY_SCHEMA_VERSION)?;

    // 2. Keychain: current key set (may include keys rotated after the upgrade)
    //    goes back to the old entry first; new entries are deleted last.
    let mut keychain_restored = false;
    let mut creds = 0;
    if let (Some(old), Some(new)) = (&rec.legacy_keychain, &rec.keychain) {
        if copy_verified(store, (&new.0, &new.1), (&old.0, &old.1))? {
            keychain_restored = true;
        }
        for r in &rec.credential_refs {
            let a = format!("cred:{r}");
            if copy_verified(store, (SERVICE, &a), (LEGACY_SERVICE, &a))? {
                creds += 1;
            }
        }
    }

    // 3. Files and directory.
    std::fs::rename(data.join(DB_FILE), data.join(LEGACY_DB_FILE)).context("rename database back")?;
    for ext in ["-wal", "-shm"] {
        let a = data.join(format!("{DB_FILE}{ext}"));
        if a.exists() {
            std::fs::rename(&a, data.join(format!("{LEGACY_DB_FILE}{ext}")))?;
        }
    }
    let _ = std::fs::remove_file(&rec_path);
    let mut restored_dir = data.to_path_buf();
    if !rec.in_place {
        let from = &rec.from_dir;
        let is_link = std::fs::symlink_metadata(from).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if is_link {
            std::fs::remove_file(from)?;
        } else if from.exists() {
            bail!("{} exists and is not the link left by the upgrade; refusing to overwrite", from.display());
        }
        std::fs::rename(&rec.to_dir, from)?;
        restored_dir = from.clone();
    }
    let note = serde_json::json!({"rolled_back_at": chrono::Utc::now().to_rfc3339(), "from": rec});
    let _ = crate::state::write_private(&restored_dir.join(ROLLBACK_RECORD_FILE), serde_json::to_string_pretty(&note)?.as_bytes());

    // 4. Remove the ZFX VPN keychain entries now that the old ones are verified.
    if let Some(new) = &rec.keychain {
        let _ = store.delete(&new.0, &new.1);
        for r in &rec.credential_refs {
            let _ = store.delete(SERVICE, &format!("cred:{r}"));
        }
    }
    Ok(RollbackReport { restored_dir, schema, keychain_restored, credentials_restored: creds })
}
