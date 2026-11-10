//! Encrypted local vault.
//!
//! * A key set (one or more 256-bit data keys with IDs) lives in the OS
//!   credential store (macOS Keychain / Linux Secret Service). A file backend
//!   exists for headless machines and tests and is reported as degraded.
//! * Records are sealed with XChaCha20-Poly1305, a fresh 192-bit nonce each,
//!   and associated data binding the ciphertext to its table and row.
//! * Keyed fingerprints (HMAC-SHA256 under an HKDF subkey) index values without
//!   storing them; bare hashes are never used.

pub mod keys;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use keys::{KeyBackend, KeySet};
use pv_audit::rusqlite::{params, OptionalExtension};
use pv_audit::Db;
use pv_detectors::exact::{validate_entry, ExactEntry, ExactKind};
use pv_detectors::normalize::{normalize, NormOpts};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::{Arc, RwLock};
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("vault locked: {0}")]
    Locked(String),
    #[error("decryption failed (wrong key or tampered record)")]
    Decrypt,
    #[error("store: {0}")]
    Store(#[from] pv_audit::StoreError),
    #[error("sql: {0}")]
    Sql(#[from] pv_audit::rusqlite::Error),
    #[error("invalid entry: {0}")]
    Invalid(String),
    #[error("key backend: {0}")]
    Backend(String),
    #[error("not found")]
    NotFound,
}

pub type Result<T> = std::result::Result<T, VaultError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretMeta {
    pub secret_id: String,
    pub kind: ExactKind,
    pub label: String,
    pub norm: NormOpts,
    pub allow: bool,
    /// Policy action applied to matches of this value (`BLOCK`, `TOKENIZE`, ...).
    pub action: String,
    pub created_at: String,
}

pub struct Vault {
    db: Db,
    backend: Arc<dyn KeyBackend>,
    keys: RwLock<KeySet>,
}

impl Vault {
    /// Open the vault, creating a key set on first use. If encrypted records
    /// exist but no key can be loaded, the vault is locked (fail closed).
    pub fn open(db: Db, backend: Arc<dyn KeyBackend>) -> Result<Vault> {
        let loaded = backend.load().map_err(VaultError::Backend)?;
        let keys = match loaded {
            Some(ks) => ks,
            None => {
                let has_records: i64 = db.conn().query_row(
                    "SELECT (SELECT COUNT(*) FROM secrets)+(SELECT COUNT(*) FROM mappings)+(SELECT COUNT(*) FROM sealed)+(SELECT COUNT(*) FROM pending_payloads)+(SELECT COUNT(*) FROM events)",
                    [],
                    |r| r.get(0),
                )?;
                if has_records > 0 {
                    return Err(VaultError::Locked(format!("no data key in {} but encrypted records exist; restore with `promptvpn vault recover`", backend.name())));
                }
                let ks = KeySet::generate("k1");
                backend.store(&ks).map_err(VaultError::Backend)?;
                ks
            }
        };
        db.meta_set("vault_key_backend", backend.name())?;
        Ok(Vault { db, backend, keys: RwLock::new(keys) })
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn backend_name(&self) -> String {
        self.backend.name().to_string()
    }

    pub fn backend_secure(&self) -> bool {
        self.backend.secure()
    }

    pub fn active_key_id(&self) -> String {
        self.keys.read().unwrap().active.clone()
    }

    fn cipher(&self, key_id: &str) -> Result<XChaCha20Poly1305> {
        let ks = self.keys.read().unwrap();
        let k = ks.key(key_id).ok_or_else(|| VaultError::Locked(format!("key {key_id} unavailable")))?;
        Ok(XChaCha20Poly1305::new_from_slice(&k[..]).expect("32-byte key"))
    }

    /// Encrypt with the active key. Returns (key_id, nonce, ciphertext).
    pub fn seal(&self, aad: &str, plaintext: &[u8]) -> Result<(String, Vec<u8>, Vec<u8>)> {
        let kid = self.active_key_id();
        let c = self.cipher(&kid)?;
        let mut nonce = [0u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ct = c.encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad: aad.as_bytes() }).map_err(|_| VaultError::Decrypt)?;
        Ok((kid, nonce.to_vec(), ct))
    }

    pub fn unseal(&self, aad: &str, key_id: &str, nonce: &[u8], ct: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        if nonce.len() != 24 {
            return Err(VaultError::Decrypt);
        }
        let c = self.cipher(key_id)?;
        let pt = c.decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: aad.as_bytes() }).map_err(|_| VaultError::Decrypt)?;
        Ok(Zeroizing::new(pt))
    }

    fn subkey(&self, key_id: &str, info: &str) -> Result<Zeroizing<Vec<u8>>> {
        let ks = self.keys.read().unwrap();
        let k = ks.key(key_id).ok_or_else(|| VaultError::Locked(format!("key {key_id} unavailable")))?;
        let hk = hkdf::Hkdf::<Sha256>::new(Some(b"promptvpn-v1"), &k[..]);
        let mut out = Zeroizing::new(vec![0u8; 32]);
        hk.expand(info.as_bytes(), &mut out).expect("hkdf len");
        Ok(out)
    }

    /// MAC key for the audit chain, by key ID.
    pub fn audit_mac_key(&self, key_id: &str) -> Option<Vec<u8>> {
        self.subkey(key_id, "audit-mac").ok().map(|k| k.to_vec())
    }

    /// Keyed fingerprint (HMAC under an HKDF subkey of the active key).
    pub fn fingerprint(&self, scope: &str, value: &str) -> Result<String> {
        let k = self.subkey(&self.active_key_id(), "fingerprint")?;
        let mut m = <HmacSha256 as Mac>::new_from_slice(&k).expect("hmac");
        m.update(scope.as_bytes());
        m.update(&[0]);
        m.update(value.as_bytes());
        Ok(hex::encode(m.finalize().into_bytes()))
    }

    // -------------------------------------------------------------- exact secrets

    pub fn add_secret(&self, kind: ExactKind, label: &str, value: &str, norm: NormOpts, allow: bool, action: &str) -> Result<String> {
        validate_entry(kind, value, norm).map_err(|e| VaultError::Invalid(e.to_string()))?;
        if label.chars().count() > 200 {
            return Err(VaultError::Invalid("label too long".into()));
        }
        let id = format!("sec_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let normalized = Zeroizing::new(normalize(value, norm));
        let fp = self.fingerprint("secret", &normalized)?;
        let dup: Option<String> = self.db.conn().query_row("SELECT secret_id FROM secrets WHERE fingerprint=?1", [&fp], |r| r.get(0)).optional()?;
        if let Some(d) = dup {
            return Err(VaultError::Invalid(format!("value already registered as {d}")));
        }
        let (kid, nonce, ct) = self.seal(&format!("secrets:{id}"), value.as_bytes())?;
        self.db.conn().execute(
            "INSERT INTO secrets(secret_id,kind,label,norm,allow,action,fingerprint,key_id,nonce,ct,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![id, serde_json::to_string(&kind).unwrap(), label, serde_json::to_string(&norm).unwrap(), allow as i32, action, fp, kid, nonce, ct, Utc::now().to_rfc3339()],
        )?;
        Ok(id)
    }

    pub fn list_secrets(&self) -> Result<Vec<SecretMeta>> {
        let c = self.db.conn();
        let mut st = c.prepare("SELECT secret_id,kind,label,norm,allow,action,created_at FROM secrets ORDER BY created_at")?;
        let rows = st.query_map([], |r| {
            Ok(SecretMeta {
                secret_id: r.get(0)?,
                kind: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(ExactKind::Value),
                label: r.get(2)?,
                norm: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                allow: r.get::<_, i32>(4)? != 0,
                action: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn remove_secret(&self, id: &str) -> Result<bool> {
        Ok(self.db.conn().execute("DELETE FROM secrets WHERE secret_id=?1", [id])? > 0)
    }

    /// Decrypt all entries for the in-process matcher.
    pub fn load_exact_entries(&self) -> Result<Vec<ExactEntry>> {
        let rows: Vec<(String, String, String, i32, String, Vec<u8>, Vec<u8>)> = {
            let c = self.db.conn();
            let mut st = c.prepare("SELECT secret_id,kind,norm,allow,key_id,nonce,ct FROM secrets")?;
            let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        let mut out = Vec::with_capacity(rows.len());
        for (id, kind, norm, allow, kid, nonce, ct) in rows {
            let pt = self.unseal(&format!("secrets:{id}"), &kid, &nonce, &ct)?;
            out.push(ExactEntry {
                secret_id: id,
                kind: serde_json::from_str(&kind).unwrap_or(ExactKind::Value),
                value: Zeroizing::new(String::from_utf8_lossy(&pt).into_owned()),
                norm: serde_json::from_str(&norm).unwrap_or_default(),
                allow: allow != 0,
            });
        }
        Ok(out)
    }

    // -------------------------------------------------------------- reversible mappings

    /// Return the existing token for `value` in `scope`, or mint a new one.
    pub fn token_for(&self, scope: &str, category: &str, value: &str, ttl: Duration) -> Result<String> {
        let fp = self.fingerprint(&format!("map:{scope}"), value)?;
        let now = Utc::now();
        let existing: Option<String> = self
            .db
            .conn()
            .query_row("SELECT token FROM mappings WHERE scope_id=?1 AND fingerprint=?2 AND expires_at>?3", params![scope, fp, now.to_rfc3339()], |r| r.get(0))
            .optional()?;
        if let Some(t) = existing {
            self.db.conn().execute("UPDATE mappings SET expires_at=?2 WHERE token=?1", params![t, (now + ttl).to_rfc3339()])?;
            return Ok(t);
        }
        let label = token_label(category);
        for _ in 0..8 {
            let mut b = [0u8; 4];
            rand::rngs::OsRng.fill_bytes(&mut b);
            let token = format!("[[PV_{label}_{}]]", hex::encode(b));
            let (kid, nonce, ct) = self.seal(&format!("mappings:{token}:{scope}"), value.as_bytes())?;
            let r = self.db.conn().execute(
                "INSERT OR IGNORE INTO mappings(token,scope_id,category,fingerprint,key_id,nonce,ct,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![token, scope, category, fp, kid, nonce, ct, now.to_rfc3339(), (now + ttl).to_rfc3339()],
            )?;
            if r == 1 {
                return Ok(token);
            }
        }
        Err(VaultError::Invalid("could not allocate token".into()))
    }

    /// Resolve a token only within its own scope. Unknown, expired or
    /// cross-scope tokens resolve to None.
    pub fn resolve_token(&self, scope: &str, token: &str) -> Result<Option<Zeroizing<String>>> {
        let row: Option<(String, Vec<u8>, Vec<u8>)> = self
            .db
            .conn()
            .query_row(
                "SELECT key_id,nonce,ct FROM mappings WHERE token=?1 AND scope_id=?2 AND expires_at>?3",
                params![token, scope, Utc::now().to_rfc3339()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match row {
            Some((kid, nonce, ct)) => {
                let pt = self.unseal(&format!("mappings:{token}:{scope}"), &kid, &nonce, &ct)?;
                Ok(Some(Zeroizing::new(String::from_utf8_lossy(&pt).into_owned())))
            }
            None => Ok(None),
        }
    }

    pub fn mapping_count(&self) -> Result<u64> {
        Ok(self.db.conn().query_row("SELECT COUNT(*) FROM mappings", [], |r| r.get::<_, i64>(0))? as u64)
    }

    // -------------------------------------------------------------- pending payloads

    pub fn put_pending_payload(&self, approval_id: &str, body: &[u8], expires_at: DateTime<Utc>) -> Result<()> {
        let (kid, nonce, ct) = self.seal(&format!("pending:{approval_id}"), body)?;
        self.db.conn().execute(
            "INSERT OR REPLACE INTO pending_payloads(approval_id,key_id,nonce,ct,expires_at) VALUES(?1,?2,?3,?4,?5)",
            params![approval_id, kid, nonce, ct, expires_at.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Returns None if the payload expired or was destroyed.
    pub fn get_pending_payload(&self, approval_id: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let row: Option<(String, Vec<u8>, Vec<u8>)> = self
            .db
            .conn()
            .query_row(
                "SELECT key_id,nonce,ct FROM pending_payloads WHERE approval_id=?1 AND expires_at>?2",
                params![approval_id, Utc::now().to_rfc3339()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match row {
            Some((kid, nonce, ct)) => Ok(Some(self.unseal(&format!("pending:{approval_id}"), &kid, &nonce, &ct)?)),
            None => Ok(None),
        }
    }

    pub fn delete_pending_payload(&self, approval_id: &str) -> Result<()> {
        self.db.conn().execute("DELETE FROM pending_payloads WHERE approval_id=?1", [approval_id])?;
        Ok(())
    }

    // -------------------------------------------------------------- generic sealed records

    pub fn put_sealed(&self, kind: &str, id: &str, data: &[u8]) -> Result<()> {
        let (kid, nonce, ct) = self.seal(&format!("sealed:{kind}:{id}"), data)?;
        self.db.conn().execute(
            "INSERT OR REPLACE INTO sealed(kind,id,key_id,nonce,ct,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![kind, id, kid, nonce, ct, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_sealed(&self, kind: &str, id: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let row: Option<(String, Vec<u8>, Vec<u8>)> = self
            .db
            .conn()
            .query_row("SELECT key_id,nonce,ct FROM sealed WHERE kind=?1 AND id=?2", params![kind, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?;
        match row {
            Some((kid, nonce, ct)) => Ok(Some(self.unseal(&format!("sealed:{kind}:{id}"), &kid, &nonce, &ct)?)),
            None => Ok(None),
        }
    }

    pub fn list_sealed(&self, kind: &str) -> Result<Vec<(String, Zeroizing<Vec<u8>>)>> {
        let rows: Vec<(String, String, Vec<u8>, Vec<u8>)> = {
            let c = self.db.conn();
            let mut st = c.prepare("SELECT id,key_id,nonce,ct FROM sealed WHERE kind=?1 ORDER BY created_at, id")?;
            let it = st.query_map([kind], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        let mut out = Vec::new();
        for (id, kid, nonce, ct) in rows {
            let pt = self.unseal(&format!("sealed:{kind}:{id}"), &kid, &nonce, &ct)?;
            out.push((id, pt));
        }
        Ok(out)
    }

    pub fn delete_sealed(&self, kind: &str, id: &str) -> Result<bool> {
        Ok(self.db.conn().execute("DELETE FROM sealed WHERE kind=?1 AND id=?2", params![kind, id])? > 0)
    }

    // -------------------------------------------------------------- rotation and recovery

    /// Rotate to a fresh data key: re-encrypt every record, recompute
    /// fingerprints, re-MAC the audit chain, then retire the old key.
    pub fn rotate(&self) -> Result<String> {
        let old_id = self.active_key_id();
        let n: u32 = old_id.trim_start_matches('k').parse().unwrap_or(1);
        let new_id = format!("k{}", n + 1);
        {
            let mut ks = self.keys.write().unwrap();
            ks.add_new_active(&new_id);
            self.backend.store(&ks).map_err(VaultError::Backend)?;
        }
        // secrets
        let secrets = self.load_exact_entries()?;
        for e in &secrets {
            let (kid, nonce, ct) = self.seal(&format!("secrets:{}", e.secret_id), e.value.as_bytes())?;
            let fp = self.fingerprint("secret", &normalize(&e.value, e.norm))?;
            self.db.conn().execute("UPDATE secrets SET key_id=?2, nonce=?3, ct=?4, fingerprint=?5 WHERE secret_id=?1", params![e.secret_id, kid, nonce, ct, fp])?;
        }
        // mappings
        let maps: Vec<(String, String, String, Vec<u8>, Vec<u8>)> = {
            let c = self.db.conn();
            let mut st = c.prepare("SELECT token,scope_id,key_id,nonce,ct FROM mappings")?;
            let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        for (token, scope, kid, nonce, ct) in maps {
            let aad = format!("mappings:{token}:{scope}");
            let pt = self.unseal(&aad, &kid, &nonce, &ct)?;
            let (k2, n2, c2) = self.seal(&aad, &pt)?;
            let fp = self.fingerprint(&format!("map:{scope}"), &String::from_utf8_lossy(&pt))?;
            self.db.conn().execute("UPDATE mappings SET key_id=?2, nonce=?3, ct=?4, fingerprint=?5 WHERE token=?1", params![token, k2, n2, c2, fp])?;
        }
        // pending payloads
        let pend: Vec<(String, String, Vec<u8>, Vec<u8>)> = {
            let c = self.db.conn();
            let mut st = c.prepare("SELECT approval_id,key_id,nonce,ct FROM pending_payloads")?;
            let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        for (id, kid, nonce, ct) in pend {
            let aad = format!("pending:{id}");
            let pt = self.unseal(&aad, &kid, &nonce, &ct)?;
            let (k2, n2, c2) = self.seal(&aad, &pt)?;
            self.db.conn().execute("UPDATE pending_payloads SET key_id=?2, nonce=?3, ct=?4 WHERE approval_id=?1", params![id, k2, n2, c2])?;
        }
        // sealed
        let sealed: Vec<(String, String, String, Vec<u8>, Vec<u8>)> = {
            let c = self.db.conn();
            let mut st = c.prepare("SELECT kind,id,key_id,nonce,ct FROM sealed")?;
            let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        for (kind, id, kid, nonce, ct) in sealed {
            let aad = format!("sealed:{kind}:{id}");
            let pt = self.unseal(&aad, &kid, &nonce, &ct)?;
            let (k2, n2, c2) = self.seal(&aad, &pt)?;
            self.db.conn().execute("UPDATE sealed SET key_id=?3, nonce=?4, ct=?5 WHERE kind=?1 AND id=?2", params![kind, id, k2, n2, c2])?;
        }
        // audit chain
        let new_mac = self.audit_mac_key(&new_id).expect("new key");
        self.db.rekey_chain(&|k| self.audit_mac_key(k), &new_mac, &new_id)?;
        {
            let mut ks = self.keys.write().unwrap();
            ks.retire(&old_id);
            self.backend.store(&ks).map_err(VaultError::Backend)?;
        }
        Ok(new_id)
    }

    /// Passphrase-wrapped copy of the key set (Argon2id + XChaCha20-Poly1305).
    pub fn export_recovery(&self, passphrase: &str) -> Result<String> {
        if passphrase.chars().count() < 12 {
            return Err(VaultError::Invalid("recovery passphrase must be at least 12 characters".into()));
        }
        keys::wrap_keyset(&self.keys.read().unwrap(), passphrase).map_err(VaultError::Invalid)
    }

    /// Restore a key set from a recovery bundle into the backend.
    pub fn recover(backend: &dyn KeyBackend, bundle: &str, passphrase: &str) -> Result<()> {
        let ks = keys::unwrap_keyset(bundle, passphrase).map_err(|_| VaultError::Decrypt)?;
        backend.store(&ks).map_err(VaultError::Backend)
    }
}

/// Token label for a category: `[[PV_EMAIL_xxxxxxxx]]`, `[[PV_CUSTOM_xxxxxxxx]]`.
pub fn token_label(category: &str) -> String {
    let base = category.split(':').next().unwrap_or("DATA");
    let l: String = base.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>().to_ascii_uppercase();
    if l.is_empty() {
        "DATA".into()
    } else {
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keys::MemoryBackend;

    fn vault() -> Vault {
        Vault::open(Db::open_memory().unwrap(), Arc::new(MemoryBackend::default())).unwrap()
    }

    #[test]
    fn seal_roundtrip_and_aad_binding() {
        let v = vault();
        let (k, n, c) = v.seal("a", b"hello").unwrap();
        assert_eq!(&v.unseal("a", &k, &n, &c).unwrap()[..], b"hello");
        assert!(v.unseal("b", &k, &n, &c).is_err());
        let mut c2 = c.clone();
        c2[0] ^= 1;
        assert!(v.unseal("a", &k, &n, &c2).is_err());
    }

    #[test]
    fn secrets_never_stored_plaintext() {
        let v = vault();
        let id = v.add_secret(ExactKind::Value, "codename", "Project Nightjar", NormOpts { casefold: true, ..Default::default() }, false, "BLOCK").unwrap();
        assert!(v.add_secret(ExactKind::Value, "dup", "project nightjar", NormOpts { casefold: true, ..Default::default() }, false, "BLOCK").is_err());
        let dump: Vec<u8> = v.db().conn().query_row("SELECT ct FROM secrets WHERE secret_id=?1", [&id], |r| r.get(0)).unwrap();
        assert!(!String::from_utf8_lossy(&dump).contains("Nightjar"));
        let fp: String = v.db().conn().query_row("SELECT fingerprint FROM secrets", [], |r| r.get(0)).unwrap();
        use sha2::Digest;
        assert_ne!(fp, hex::encode(Sha256::digest(b"project nightjar")));
        let e = v.load_exact_entries().unwrap();
        assert_eq!(e[0].value.as_str(), "Project Nightjar");
        assert_eq!(v.list_secrets().unwrap()[0].label, "codename");
        assert!(v.remove_secret(&id).unwrap());
    }

    #[test]
    fn tokens_scoped_and_stable() {
        let v = vault();
        let t1 = v.token_for("conv-a", "EMAIL", "jane@example.com", Duration::hours(1)).unwrap();
        let t2 = v.token_for("conv-a", "EMAIL", "jane@example.com", Duration::hours(1)).unwrap();
        let t3 = v.token_for("conv-b", "EMAIL", "jane@example.com", Duration::hours(1)).unwrap();
        assert_eq!(t1, t2);
        assert_ne!(t1, t3);
        assert!(t1.starts_with("[[PV_EMAIL_") && t1.ends_with("]]"));
        assert_eq!(v.resolve_token("conv-a", &t1).unwrap().unwrap().as_str(), "jane@example.com");
        assert!(v.resolve_token("conv-b", &t1).unwrap().is_none(), "cross-scope restoration must fail");
        assert!(v.resolve_token("conv-a", "[[PV_EMAIL_00000000]]").unwrap().is_none());
    }

    #[test]
    fn expired_tokens_do_not_resolve() {
        let v = vault();
        let t = v.token_for("s", "EMAIL", "a@b.co", Duration::seconds(-1)).unwrap();
        assert!(v.resolve_token("s", &t).unwrap().is_none());
    }

    #[test]
    fn rotation_preserves_data_and_chain() {
        let v = vault();
        v.add_secret(ExactKind::Value, "x", "Nightjar-42", NormOpts::default(), false, "BLOCK").unwrap();
        let t = v.token_for("s", "EMAIL", "a@b.co", Duration::hours(1)).unwrap();
        v.put_sealed("entity", "c1", b"{\"x\":1}").unwrap();
        v.put_pending_payload("ap1", b"body", Utc::now() + Duration::minutes(15)).unwrap();
        let ev = pv_core::AuditEvent {
            schema_version: 1, event_id: "e".into(), timestamp: Utc::now(), kind: "request".into(), app_id: "a".into(), agent_id: None, user_id: "u".into(),
            destination_id: None, endpoint: None, categories: vec![], action: "ALLOW".into(), policy_ids: vec![], policy_hash: None, recognizer_versions: vec![],
            latency_ms: 0, finding_count: 0, approval_id: None, approval_status: None, error_code: None, severity: pv_core::Sensitivity::Low,
            direction: pv_core::Direction::Inbound, summary: String::new(), context_ref: None,
        };
        let k1 = v.active_key_id();
        v.db().append_event(&ev, 30, &v.audit_mac_key(&k1).unwrap(), &k1).unwrap();
        let new = v.rotate().unwrap();
        assert_eq!(new, "k2");
        assert_eq!(v.load_exact_entries().unwrap()[0].value.as_str(), "Nightjar-42");
        assert_eq!(v.resolve_token("s", &t).unwrap().unwrap().as_str(), "a@b.co");
        assert_eq!(v.token_for("s", "EMAIL", "a@b.co", Duration::hours(1)).unwrap(), t, "fingerprints recomputed");
        assert_eq!(&v.get_sealed("entity", "c1").unwrap().unwrap()[..], b"{\"x\":1}");
        assert_eq!(&v.get_pending_payload("ap1").unwrap().unwrap()[..], b"body");
        assert_eq!(v.db().verify_chain(&|k| v.audit_mac_key(k)).unwrap(), 1);
        assert!(v.audit_mac_key("k1").is_none(), "old key retired");
    }

    #[test]
    fn locked_when_key_missing() {
        let db = Db::open_memory().unwrap();
        let b1: Arc<dyn KeyBackend> = Arc::new(MemoryBackend::default());
        let v = Vault::open(db.clone(), b1).unwrap();
        v.add_secret(ExactKind::Value, "x", "abcdefgh", NormOpts::default(), false, "BLOCK").unwrap();
        let empty: Arc<dyn KeyBackend> = Arc::new(MemoryBackend::default());
        assert!(matches!(Vault::open(db, empty), Err(VaultError::Locked(_))));
    }

    #[test]
    fn recovery_roundtrip() {
        let db = Db::open_memory().unwrap();
        let v = Vault::open(db.clone(), Arc::new(MemoryBackend::default())).unwrap();
        v.add_secret(ExactKind::Value, "x", "abcdefgh", NormOpts::default(), false, "BLOCK").unwrap();
        assert!(v.export_recovery("short").is_err());
        let bundle = v.export_recovery("correct horse battery staple").unwrap();
        let fresh = Arc::new(MemoryBackend::default());
        assert!(Vault::recover(fresh.as_ref(), &bundle, "wrong passphrase!!").is_err());
        Vault::recover(fresh.as_ref(), &bundle, "correct horse battery staple").unwrap();
        let v2 = Vault::open(db, fresh).unwrap();
        assert_eq!(v2.load_exact_entries().unwrap()[0].value.as_str(), "abcdefgh");
    }

    #[test]
    fn pending_payload_expiry() {
        let v = vault();
        v.put_pending_payload("x", b"b", Utc::now() - Duration::seconds(1)).unwrap();
        assert!(v.get_pending_payload("x").unwrap().is_none());
    }
}
