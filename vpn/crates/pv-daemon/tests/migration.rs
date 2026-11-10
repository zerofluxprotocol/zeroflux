//! Gate 1 (spec 3.7): a Prompt VPN 0.1 install survives upgrade to ZFX VPN and
//! rollback. Uses `tests/fixtures/legacy-promptvpn-0.1.0`, a data directory
//! written by the original promptvpn binary (see its README and generate.sh).
//!
//! The end-to-end variant that runs the old binary again after rollback is
//! `tests/migration/upgrade_rollback.sh`.

mod common;

use pv_daemon::migrate::{self, Env, MemorySecretStore, Plan, SecretStore};
use pv_daemon::state::{self, Paths, StoreKeyBackend};
use pv_vault::keys::{FileBackend, KeyBackend};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

static STORE: LazyLock<Arc<MemorySecretStore>> = LazyLock::new(|| Arc::new(MemorySecretStore::default()));

fn store() -> Arc<dyn SecretStore> {
    state::use_secret_store(STORE.clone())
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/legacy-promptvpn-0.1.0")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// Every encrypted or MAC'd byte the old code wrote, for no-re-encryption checks.
fn crypto_snapshot(db: &Path) -> Vec<String> {
    let c = pv_audit::rusqlite::Connection::open(db).unwrap();
    let mut out = vec![];
    for (q, n) in [
        ("SELECT secret_id||'|'||key_id||'|'||hex(nonce)||'|'||hex(ct)||'|'||fingerprint FROM secrets ORDER BY secret_id", "secrets"),
        ("SELECT token||'|'||scope_id||'|'||key_id||'|'||hex(nonce)||'|'||hex(ct)||'|'||fingerprint FROM mappings ORDER BY token", "mappings"),
        ("SELECT kind||'|'||id||'|'||key_id||'|'||hex(nonce)||'|'||hex(ct) FROM sealed ORDER BY kind,id", "sealed"),
        ("SELECT seq||'|'||event_id||'|'||prev_mac||'|'||mac||'|'||json FROM events ORDER BY seq", "events"),
        ("SELECT app_id||'|'||key_hash FROM apps ORDER BY app_id", "apps"),
        ("SELECT id||'|'||version||'|'||json FROM policy_versions ORDER BY id,version", "policies"),
    ] {
        let mut st = c.prepare(q).unwrap();
        let rows: Vec<String> = st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| format!("{n}:{}", r.unwrap())).collect();
        assert!(!rows.is_empty() || n == "x", "fixture has {n}");
        out.extend(rows);
    }
    out
}

fn schema(db: &Path) -> i64 {
    pv_audit::rusqlite::Connection::open(db).unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
}

fn env(tmp: &Path, pv_home: Option<PathBuf>, backend: &str) -> Env {
    Env { zfx_home: None, pv_home, default_dir: tmp.join("new-default/zfx-vpn"), legacy_default_dir: tmp.join("old-default/promptvpn"), key_backend: backend.into() }
}

/// Checks that need only the vault and database (no housekeeping, so the
/// audit chain is verified exactly as the old code wrote it).
fn verify_material(db_path: &Path, backend: Arc<dyn KeyBackend>) {
    let db = pv_audit::Db::open(db_path).unwrap();
    let vault = pv_vault::Vault::open(db.clone(), backend).unwrap();
    // Audit chain: all 9 events, MACs intact.
    assert_eq!(db.verify_chain(&|k| vault.audit_mac_key(k)).unwrap(), 9);
    // Registered secrets decrypt to the original values.
    let mut vals: Vec<String> = vault.load_exact_entries().unwrap().into_iter().map(|e| e.value.to_string()).collect();
    vals.sort();
    assert_eq!(vals, vec!["Nightjar-Cedar-77".to_string(), "zz-legacy-blocked-value-991".to_string()]);
    // Sealed custom entity type decrypts.
    let ents = vault.list_sealed("entity").unwrap();
    assert_eq!(ents.len(), 1);
    assert!(String::from_utf8_lossy(&ents[0].1).contains("insurance policy number"));
    // Legacy placeholders still resolve in their own scope (expiry extended:
    // the fixture's 24 h mapping TTL is not what is under test).
    db.conn().execute("UPDATE mappings SET expires_at='2999-01-01T00:00:00+00:00'", []).unwrap();
    let scope = "app_16d7a9e750cf:fixture-conv-1";
    assert_eq!(vault.resolve_token(scope, "[[PV_EMAIL_bb25f99e]]").unwrap().unwrap().as_str(), "alice.fixture@example.test");
    assert_eq!(vault.resolve_token(scope, "[[PV_EXACT_SECRET_caf08ead]]").unwrap().unwrap().as_str(), "Nightjar-Cedar-77");
    assert!(vault.resolve_token("other:scope", "[[PV_EMAIL_bb25f99e]]").unwrap().is_none());
}

#[tokio::test]
async fn in_place_upgrade_file_backend_keeps_all_material_and_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("pvhome");
    copy_dir(&fixture().join("home"), &home);
    let before = crypto_snapshot(&home.join("promptvpn.db"));
    let keys_before = std::fs::read(home.join("keys.insecure.json")).unwrap();
    let e = env(tmp.path(), Some(home.clone()), "file");
    assert_eq!(migrate::plan(&e), Plan::Migrate { from: home.clone(), to: home.clone(), in_place: true });

    let (data, rec) = migrate::ensure(&e, store().as_ref()).unwrap();
    let rec = rec.expect("migrated");
    assert_eq!(data, home);
    assert_eq!((rec.schema_from, rec.schema_to), (1, 2));
    assert!(!home.join("promptvpn.db").exists());
    assert!(home.join("zfx-vpn.db").exists());
    assert_eq!(schema(&home.join("zfx-vpn.db")), 2);
    // Backup is the untouched original database.
    assert_eq!(std::fs::read(home.join(&rec.backup_file)).unwrap(), std::fs::read(fixture().join("home/promptvpn.db")).unwrap());
    // No re-encryption: every encrypted/MAC'd row and the key file are byte-identical.
    assert_eq!(crypto_snapshot(&home.join("zfx-vpn.db")), before);
    assert_eq!(std::fs::read(home.join("keys.insecure.json")).unwrap(), keys_before);
    // Idempotent: a second run finds nothing to do.
    assert_eq!(migrate::plan(&e), Plan::Ready { data: home.clone() });

    verify_material(&home.join("zfx-vpn.db"), Arc::new(FileBackend { path: home.join("keys.insecure.json") }));

    // The daemon boots on the migrated data with the same policy and app keys.
    let st = state::bootstrap_with_backend(Paths::at(home.clone()), Some(Arc::new(pv_proxy::notify::Recorder::default())), Arc::new(FileBackend { path: home.join("keys.insecure.json") })).await.unwrap();
    let exported: Value = serde_json::from_slice(&std::fs::read(fixture().join("policy-export.json")).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&st.engine.policy().rules).unwrap(), exported["rules"]);
    for f in ["default.key", "app_637ad207aed3.key"] {
        let k = std::fs::read_to_string(home.join("apps").join(f)).unwrap();
        assert!(st.db().app_by_key_hash(&pv_daemon::auth::key_hash(k.trim())).unwrap().is_some(), "{f} still authenticates");
    }
    let secrets: Value = serde_json::from_slice(&std::fs::read(fixture().join("secrets-list.json")).unwrap()).unwrap();
    let now: Vec<String> = st.vault().list_secrets().unwrap().into_iter().map(|m| format!("{}:{}:{}", m.secret_id, m.label, m.action)).collect();
    let was: Vec<String> = secrets["secrets"].as_array().unwrap().iter().map(|m| format!("{}:{}:{}", m["secret_id"].as_str().unwrap(), m["label"].as_str().unwrap(), m["action"].as_str().unwrap())).collect();
    assert_eq!(now.len(), 2);
    for w in was {
        assert!(now.contains(&w), "{w} in {now:?}");
    }
    drop(st);

    // Rollback: schema 1, old file name, same material.
    let r = migrate::rollback(&home, store().as_ref()).unwrap();
    assert_eq!(r.schema, 1);
    assert!(home.join("promptvpn.db").exists() && !home.join("zfx-vpn.db").exists());
    assert!(!home.join("migration.json").exists());
    assert_eq!(schema(&home.join("promptvpn.db")), 1);
    let after = crypto_snapshot(&home.join("promptvpn.db"));
    // Rows written by the old code are unchanged (the boot may have appended events).
    for row in before.iter().filter(|r| !r.starts_with("events:")) {
        assert!(after.contains(row), "row survived rollback: {row}");
    }
    let tables: Vec<String> = {
        let c = pv_audit::rusqlite::Connection::open(home.join("promptvpn.db")).unwrap();
        let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type='table'").unwrap();
        let v: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        v
    };
    assert!(!tables.contains(&"privacy_routes".to_string()));
    // Upgrading again works (the rollback record does not block it).
    assert!(matches!(migrate::plan(&e), Plan::Migrate { .. }));
}

#[tokio::test]
async fn default_location_upgrade_keychain_copy_and_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), None, "keychain");
    let legacy = e.legacy_default_dir.clone();
    copy_dir(&fixture().join("home"), &legacy);
    // Keychain-mode install: the key set lives in the OS store, not a file.
    let keyset_json = std::fs::read_to_string(legacy.join("keys.insecure.json")).unwrap();
    std::fs::remove_file(legacy.join("keys.insecure.json")).unwrap();
    let st0 = store();
    // Exactly what Prompt VPN 0.1 stored: service "promptvpn", account from the path as given.
    let legacy_account = format!("keyset:{}", &hex_sha256(legacy.to_string_lossy().as_bytes())[..16]);
    assert_eq!(migrate::legacy_key_account(&legacy), legacy_account);
    st0.set("promptvpn", &legacy_account, &keyset_json).unwrap();
    // A provider credential, as `promptvpn near login` would have stored it.
    {
        let c = pv_audit::rusqlite::Connection::open(legacy.join("promptvpn.db")).unwrap();
        let j: String = c.query_row("SELECT json FROM providers WHERE id='custom'", [], |r| r.get(0)).unwrap();
        let mut v: Value = serde_json::from_str(&j).unwrap();
        v["credential_ref"] = "provider:custom".into();
        c.execute("UPDATE providers SET json=?1 WHERE id='custom'", [v.to_string()]).unwrap();
    }
    st0.set("promptvpn", "cred:provider:custom", "sk-synthetic-provider-credential").unwrap();
    let before = crypto_snapshot(&legacy.join("promptvpn.db"));

    let (data, rec) = migrate::ensure(&e, st0.as_ref()).unwrap();
    let rec = rec.unwrap();
    assert_eq!(data, e.default_dir);
    assert!(!rec.in_place);
    // Old path is now a link to the new directory.
    assert!(std::fs::symlink_metadata(&legacy).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::canonicalize(&legacy).unwrap(), std::fs::canonicalize(&e.default_dir).unwrap());
    // Keychain: copied byte-for-byte to the new service/account; old entry kept.
    let (svc, acct) = rec.keychain.clone().unwrap();
    assert_eq!(svc, "zfx-vpn");
    assert_eq!(st0.get(&svc, &acct).unwrap().unwrap().as_str(), keyset_json);
    assert_eq!(st0.get("promptvpn", &legacy_account).unwrap().unwrap().as_str(), keyset_json);
    assert_eq!(st0.get("zfx-vpn", "cred:provider:custom").unwrap().unwrap().as_str(), "sk-synthetic-provider-credential");
    assert_eq!(crypto_snapshot(&e.default_dir.join("zfx-vpn.db")), before);
    // Running commands again (e.g. via the old path through the link) does not re-migrate.
    assert_eq!(migrate::plan(&e), Plan::Ready { data: e.default_dir.clone() });

    // The account the daemon derives for the new path is the one migration wrote.
    let backend = Arc::new(StoreKeyBackend::new(st0.clone(), "zfx-vpn", &state::key_account(&std::fs::canonicalize(&e.default_dir).unwrap().to_string_lossy())));
    verify_material(&e.default_dir.join("zfx-vpn.db"), backend.clone());
    let st = state::bootstrap_with_backend(Paths::at(e.default_dir.clone()), Some(Arc::new(pv_proxy::notify::Recorder::default())), backend).await.unwrap();
    assert_eq!(st.creds().get("provider:custom").unwrap().as_str(), "sk-synthetic-provider-credential");
    // Rotate after upgrade: rollback must hand the NEW key set back to 0.1.
    let new_kid = st.vault().rotate().unwrap();
    drop(st);

    let r = migrate::rollback(&e.default_dir, st0.as_ref()).unwrap();
    assert_eq!(r.restored_dir, legacy);
    assert!(r.keychain_restored);
    assert_eq!(r.credentials_restored, 1);
    assert!(!std::fs::symlink_metadata(&legacy).unwrap().file_type().is_symlink());
    assert!(legacy.join("promptvpn.db").exists() && !e.default_dir.exists());
    let restored = st0.get("promptvpn", &legacy_account).unwrap().unwrap();
    assert!(restored.contains(&format!("\"active\":\"{new_kid}\"")), "rotated key set handed back");
    assert!(st0.get(&svc, &acct).unwrap().is_none(), "zfx-vpn entry removed after rollback");
    assert!(st0.get("zfx-vpn", "cred:provider:custom").unwrap().is_none());
    assert_eq!(st0.get("promptvpn", "cred:provider:custom").unwrap().unwrap().as_str(), "sk-synthetic-provider-credential");
    // Prompt VPN 0.1 opens schema 1 with its own keychain entry. (Opening it
    // with this binary's Db re-applies v2, so check the version first.)
    assert_eq!(schema(&legacy.join("promptvpn.db")), 1);
    let old_backend = Arc::new(StoreKeyBackend::new(st0.clone(), "promptvpn", &legacy_account));
    let db = pv_audit::Db::open(&legacy.join("promptvpn.db")).unwrap();
    let vault = pv_vault::Vault::open(db, old_backend).unwrap();
    let mut vals: Vec<String> = vault.load_exact_entries().unwrap().into_iter().map(|e| e.value.to_string()).collect();
    vals.sort();
    assert_eq!(vals, vec!["Nightjar-Cedar-77".to_string(), "zz-legacy-blocked-value-991".to_string()]);
}

#[test]
fn missing_keychain_entry_with_records_fails_without_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), None, "keychain");
    let legacy = e.legacy_default_dir.clone();
    copy_dir(&fixture().join("home"), &legacy);
    std::fs::remove_file(legacy.join("keys.insecure.json")).unwrap();
    let mem = MemorySecretStore::default();
    let err = migrate::ensure(&e, &mem).unwrap_err().to_string();
    assert!(err.contains("not found in the OS keychain"), "{err}");
    // Nothing moved or renamed.
    assert!(legacy.join("promptvpn.db").exists());
    assert!(!e.default_dir.exists());
}

#[test]
fn running_legacy_daemon_blocks_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("pvhome");
    copy_dir(&fixture().join("home"), &home);
    std::fs::write(home.join("daemon.pid"), std::process::id().to_string()).unwrap();
    let e = env(tmp.path(), Some(home.clone()), "file");
    let err = migrate::ensure(&e, &MemorySecretStore::default()).unwrap_err().to_string();
    assert!(err.contains("stop it first"), "{err}");
    assert!(home.join("promptvpn.db").exists());
}

#[test]
fn legacy_default_migrated_in_place_is_found_without_env() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path(), None, "file");
    std::fs::create_dir_all(&e.legacy_default_dir).unwrap();
    std::fs::write(e.legacy_default_dir.join("zfx-vpn.db"), b"").unwrap();
    assert_eq!(migrate::plan(&e), Plan::Ready { data: e.legacy_default_dir.clone() });
}

fn hex_sha256(b: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(b))
}
