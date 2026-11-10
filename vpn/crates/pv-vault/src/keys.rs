//! Data key sets and where they live.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use zeroize::Zeroizing;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub struct KeySet {
    pub active: String,
    keys: BTreeMap<String, Zeroizing<[u8; 32]>>,
}

#[derive(Serialize, Deserialize)]
struct KeySetWire {
    v: u32,
    active: String,
    keys: BTreeMap<String, String>,
}

impl KeySet {
    pub fn generate(id: &str) -> KeySet {
        let mut k = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(&mut k[..]);
        let mut keys = BTreeMap::new();
        keys.insert(id.to_string(), k);
        KeySet { active: id.to_string(), keys }
    }

    pub fn key(&self, id: &str) -> Option<&Zeroizing<[u8; 32]>> {
        self.keys.get(id)
    }

    pub fn ids(&self) -> Vec<String> {
        self.keys.keys().cloned().collect()
    }

    pub fn add_new_active(&mut self, id: &str) {
        let mut k = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(&mut k[..]);
        self.keys.insert(id.to_string(), k);
        self.active = id.to_string();
    }

    pub fn retire(&mut self, id: &str) {
        if id != self.active {
            self.keys.remove(id);
        }
    }

    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let w = KeySetWire { v: 1, active: self.active.clone(), keys: self.keys.iter().map(|(k, v)| (k.clone(), B64.encode(&v[..]))).collect() };
        Zeroizing::new(serde_json::to_vec(&w).expect("serialize"))
    }

    pub fn from_bytes(b: &[u8]) -> Result<KeySet, String> {
        let w: KeySetWire = serde_json::from_slice(b).map_err(|e| e.to_string())?;
        let mut keys = BTreeMap::new();
        for (id, s) in w.keys {
            let raw = Zeroizing::new(B64.decode(s).map_err(|e| e.to_string())?);
            if raw.len() != 32 {
                return Err("bad key length".into());
            }
            let mut k = Zeroizing::new([0u8; 32]);
            k.copy_from_slice(&raw);
            keys.insert(id, k);
        }
        if !keys.contains_key(&w.active) {
            return Err("active key missing".into());
        }
        Ok(KeySet { active: w.active, keys })
    }
}

pub trait KeyBackend: Send + Sync {
    fn name(&self) -> &str;
    /// True for OS-protected stores (Keychain / Secret Service).
    fn secure(&self) -> bool;
    fn load(&self) -> Result<Option<KeySet>, String>;
    fn store(&self, ks: &KeySet) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;
}

/// OS credential store via the `keyring` crate (macOS Keychain, Linux Secret Service/keyutils).
pub struct KeychainBackend {
    entry: keyring::Entry,
    label: String,
}

impl KeychainBackend {
    pub fn new(service: &str, account: &str) -> Result<Self, String> {
        let entry = keyring::Entry::new(service, account).map_err(|e| e.to_string())?;
        Ok(KeychainBackend { entry, label: format!("os-keychain({service}/{account})") })
    }
}

impl KeyBackend for KeychainBackend {
    fn name(&self) -> &str {
        &self.label
    }
    fn secure(&self) -> bool {
        true
    }
    fn load(&self) -> Result<Option<KeySet>, String> {
        match self.entry.get_password() {
            Ok(s) => KeySet::from_bytes(s.as_bytes()).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("keychain read failed: {e}")),
        }
    }
    fn store(&self, ks: &KeySet) -> Result<(), String> {
        let b = ks.to_bytes();
        let s = Zeroizing::new(String::from_utf8_lossy(&b).into_owned());
        self.entry.set_password(&s).map_err(|e| format!("keychain write failed: {e}"))?;
        // Read back: some Linux backends accept writes they cannot persist.
        match self.load()? {
            Some(back) if back.active == ks.active => Ok(()),
            _ => Err("keychain write did not persist; secure storage unavailable".into()),
        }
    }
    fn delete(&self) -> Result<(), String> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Key file with 0600 permissions. Degraded: protects only against other users.
pub struct FileBackend {
    pub path: PathBuf,
}

impl KeyBackend for FileBackend {
    fn name(&self) -> &str {
        "file(insecure-degraded)"
    }
    fn secure(&self) -> bool {
        false
    }
    fn load(&self) -> Result<Option<KeySet>, String> {
        match std::fs::read(&self.path) {
            Ok(b) => {
                let b = Zeroizing::new(b);
                KeySet::from_bytes(&b).map(Some)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
    fn store(&self, ks: &KeySet) -> Result<(), String> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("tmp");
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp).map_err(|e| e.to_string())?;
            f.write_all(&ks.to_bytes()).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
    fn delete(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// In-memory backend for tests.
#[derive(Default)]
pub struct MemoryBackend {
    data: Mutex<Option<Zeroizing<Vec<u8>>>>,
}

impl KeyBackend for MemoryBackend {
    fn name(&self) -> &str {
        "memory"
    }
    fn secure(&self) -> bool {
        false
    }
    fn load(&self) -> Result<Option<KeySet>, String> {
        match &*self.data.lock().unwrap() {
            Some(b) => KeySet::from_bytes(b).map(Some),
            None => Ok(None),
        }
    }
    fn store(&self, ks: &KeySet) -> Result<(), String> {
        *self.data.lock().unwrap() = Some(ks.to_bytes());
        Ok(())
    }
    fn delete(&self) -> Result<(), String> {
        *self.data.lock().unwrap() = None;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct RecoveryBundle {
    format: String,
    kdf: String,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    salt: String,
    nonce: String,
    ct: String,
}

fn kdf(pass: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<Zeroizing<[u8; 32]>, String> {
    let params = Params::new(m, t, p, Some(32)).map_err(|e| e.to_string())?;
    let a = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    a.hash_password_into(pass.as_bytes(), salt, &mut out[..]).map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn wrap_keyset(ks: &KeySet, pass: &str) -> Result<String, String> {
    let (m, t, p) = (64 * 1024, 3, 1);
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let k = kdf(pass, &salt, m, t, p)?;
    let c = XChaCha20Poly1305::new_from_slice(&k[..]).unwrap();
    let ct = c.encrypt(XNonce::from_slice(&nonce), ks.to_bytes().as_slice()).map_err(|_| "encrypt".to_string())?;
    let b = RecoveryBundle { format: "promptvpn-recovery-v1".into(), kdf: "argon2id".into(), m_cost: m, t_cost: t, p_cost: p, salt: B64.encode(salt), nonce: B64.encode(nonce), ct: B64.encode(ct) };
    serde_json::to_string_pretty(&b).map_err(|e| e.to_string())
}

pub fn unwrap_keyset(bundle: &str, pass: &str) -> Result<KeySet, String> {
    let b: RecoveryBundle = serde_json::from_str(bundle).map_err(|e| e.to_string())?;
    if b.format != "promptvpn-recovery-v1" || b.kdf != "argon2id" {
        return Err("unsupported bundle".into());
    }
    let salt = B64.decode(&b.salt).map_err(|e| e.to_string())?;
    let nonce = B64.decode(&b.nonce).map_err(|e| e.to_string())?;
    let ct = B64.decode(&b.ct).map_err(|e| e.to_string())?;
    if nonce.len() != 24 {
        return Err("bad nonce".into());
    }
    let k = kdf(pass, &salt, b.m_cost, b.t_cost, b.p_cost)?;
    let c = XChaCha20Poly1305::new_from_slice(&k[..]).unwrap();
    let pt = Zeroizing::new(c.decrypt(XNonce::from_slice(&nonce), ct.as_slice()).map_err(|_| "wrong passphrase or corrupt bundle".to_string())?);
    KeySet::from_bytes(&pt)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_backend_perms() {
        let d = tempfile::tempdir().unwrap();
        let b = FileBackend { path: d.path().join("keys") };
        assert!(b.load().unwrap().is_none());
        b.store(&KeySet::generate("k1")).unwrap();
        assert_eq!(b.load().unwrap().unwrap().active, "k1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&b.path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
