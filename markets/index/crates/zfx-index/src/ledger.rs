//! Append-only ledger.
//!
//! ```text
//! <root>/blobs/<aa>/<sha256>.bin   raw response bytes, content-addressed
//! <root>/fetches.jsonl             one record per HTTP fetch (success or failure)
//! <root>/revisions.jsonl           one record per index revision (FINAL / CORRECTED)
//! <root>/markets.jsonl             one record per market listing event
//! <root>/LOCK                      held by the single writer
//! ```
//!
//! Every JSONL record carries `chain = SHA-256(prev_chain || record-without-chain)`
//! so deleting or editing a line breaks verification (`zfx-index verify`).
//! Records are never rewritten. Corrections are new revisions.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FetchRecord {
    pub seq: u64,
    pub kind: String,
    /// Logical subject, e.g. the model permaslug, or "" for market-wide responses.
    pub subject: String,
    pub documented: bool,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub fetched_at_ms: i64,
    /// Selected response headers, lower-cased names.
    pub headers: BTreeMap<String, String>,
    /// SHA-256 of the raw body, or "" if no body was received.
    pub sha256: String,
    pub len: u64,
    pub client: String,
    pub error: Option<String>,
    #[serde(default)]
    pub chain: String,
}

pub struct Ledger {
    pub root: PathBuf,
    _lock: Option<File>,
}

#[derive(Debug)]
pub enum LedgerError {
    Io(std::io::Error),
    Locked(PathBuf),
    Corrupt(String),
}
impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        LedgerError::Io(e)
    }
}
impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Io(e) => write!(f, "io: {e}"),
            LedgerError::Locked(p) => write!(f, "ledger is locked by another writer ({})", p.display()),
            LedgerError::Corrupt(s) => write!(f, "ledger corrupt: {s}"),
        }
    }
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn chain_next(prev: &str, record_json: &str) -> String {
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(record_json.as_bytes());
    hex::encode(h.finalize())
}

impl Ledger {
    /// Opens for reading only. No lock.
    pub fn open_read(root: &Path) -> Ledger {
        Ledger { root: root.to_path_buf(), _lock: None }
    }

    /// Opens for writing; takes an exclusive lock file. A stale lock (dead pid)
    /// must be removed by hand after checking no writer is running.
    pub fn open_write(root: &Path) -> Result<Ledger, LedgerError> {
        fs::create_dir_all(root.join("blobs"))?;
        let lock = root.join("LOCK");
        match OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(mut f) => {
                writeln!(f, "{}", std::process::id())?;
                Ok(Ledger { root: root.to_path_buf(), _lock: Some(f) })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // reclaim if the recorded pid is gone
                let pid: Option<i32> = fs::read_to_string(&lock).ok().and_then(|s| s.trim().parse().ok());
                if let Some(pid) = pid {
                    if !pid_alive(pid) {
                        fs::remove_file(&lock)?;
                        return Ledger::open_write(root);
                    }
                }
                Err(LedgerError::Locked(lock))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn blob_path(&self, sha: &str) -> PathBuf {
        self.root.join("blobs").join(&sha[..2]).join(format!("{sha}.bin"))
    }

    pub fn put_blob(&self, bytes: &[u8]) -> Result<String, LedgerError> {
        let sha = sha256_hex(bytes);
        let p = self.blob_path(&sha);
        if !p.exists() {
            fs::create_dir_all(p.parent().unwrap())?;
            let tmp = p.with_extension("tmp");
            fs::write(&tmp, bytes)?;
            fs::rename(&tmp, &p)?;
        }
        Ok(sha)
    }

    pub fn get_blob(&self, sha: &str) -> Result<Vec<u8>, LedgerError> {
        let b = fs::read(self.blob_path(sha))?;
        if sha256_hex(&b) != sha {
            return Err(LedgerError::Corrupt(format!("blob {sha} hash mismatch")));
        }
        Ok(b)
    }

    fn append<T: Serialize + DeserializeOwned>(&self, file: &str, rec: &mut T, set_chain: impl Fn(&mut T, String)) -> Result<(), LedgerError>
    where
        T: Clone,
    {
        if self._lock.is_none() {
            return Err(LedgerError::Corrupt("ledger opened read-only".into()));
        }
        let path = self.root.join(file);
        let prev = last_chain(&path)?;
        set_chain(rec, String::new());
        let body = serde_json::to_string(rec).map_err(|e| LedgerError::Corrupt(e.to_string()))?;
        let chain = chain_next(&prev, &body);
        set_chain(rec, chain);
        let line = serde_json::to_string(rec).map_err(|e| LedgerError::Corrupt(e.to_string()))?;
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_data()?;
        Ok(())
    }

    pub fn next_fetch_seq(&self) -> Result<u64, LedgerError> {
        Ok(self.fetches()?.last().map(|r| r.seq + 1).unwrap_or(1))
    }

    pub fn append_fetch(&self, rec: &mut FetchRecord) -> Result<(), LedgerError> {
        self.append("fetches.jsonl", rec, |r, c| r.chain = c)
    }

    pub fn fetches(&self) -> Result<Vec<FetchRecord>, LedgerError> {
        read_jsonl(&self.root.join("fetches.jsonl"))
    }

    pub fn append_json(&self, file: &str, rec: &mut serde_json::Value) -> Result<(), LedgerError> {
        self.append(file, rec, |r, c| {
            if let Some(o) = r.as_object_mut() {
                o.insert("chain".into(), serde_json::Value::String(c));
            }
        })
    }

    pub fn read_json(&self, file: &str) -> Result<Vec<serde_json::Value>, LedgerError> {
        read_jsonl(&self.root.join(file))
    }

    /// Verifies hash chains of all JSONL files and every referenced blob.
    pub fn verify(&self) -> Result<VerifyReport, LedgerError> {
        let mut rep = VerifyReport::default();
        for file in ["fetches.jsonl", "revisions.jsonl", "markets.jsonl", "revisions_7d.jsonl"] {
            let path = self.root.join(file);
            if !path.exists() {
                continue;
            }
            let mut prev = GENESIS.to_string();
            for (i, line) in BufReader::new(File::open(&path)?).lines().enumerate() {
                let line = line?;
                let mut v: serde_json::Value = serde_json::from_str(&line)
                    .map_err(|e| LedgerError::Corrupt(format!("{file}:{}: {e}", i + 1)))?;
                let chain = v.get("chain").and_then(|c| c.as_str()).unwrap_or("").to_string();
                v.as_object_mut().unwrap().insert("chain".into(), serde_json::Value::String(String::new()));
                let body = serde_json::to_string(&v).unwrap();
                let want = chain_next(&prev, &body);
                if want != chain {
                    return Err(LedgerError::Corrupt(format!("{file}:{}: chain mismatch", i + 1)));
                }
                prev = chain;
                *rep.records.entry(file.to_string()).or_default() += 1;
            }
        }
        for f in self.fetches()? {
            if !f.sha256.is_empty() {
                if !self.blob_path(&f.sha256).exists() {
                    rep.blobs_missing += 1;
                    continue;
                }
                self.get_blob(&f.sha256)?;
                rep.blobs_checked += 1;
            }
        }
        Ok(rep)
    }
}

#[derive(Debug, Default, Serialize)]
pub struct VerifyReport {
    pub records: BTreeMap<String, u64>,
    pub blobs_checked: u64,
    /// Referenced by fetches.jsonl but not present (e.g. a published snapshot without raw responses).
    pub blobs_missing: u64,
}

fn last_chain(path: &Path) -> Result<String, LedgerError> {
    if !path.exists() {
        return Ok(GENESIS.to_string());
    }
    let mut last = None;
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if !line.trim().is_empty() {
            last = Some(line);
        }
    }
    match last {
        None => Ok(GENESIS.to_string()),
        Some(l) => {
            let v: serde_json::Value = serde_json::from_str(&l).map_err(|e| LedgerError::Corrupt(e.to_string()))?;
            Ok(v.get("chain").and_then(|c| c.as_str()).unwrap_or(GENESIS).to_string())
        }
    }
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, LedgerError> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for (i, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line).map_err(|e| LedgerError::Corrupt(format!("{}:{}: {e}", path.display(), i + 1)))?);
    }
    Ok(out)
}

fn pid_alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

impl Drop for Ledger {
    fn drop(&mut self) {
        if self._lock.is_some() {
            let _ = fs::remove_file(self.root.join("LOCK"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn tmpdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("zfx-ledger-{name}-{}-{}", std::process::id(), crate::time::now_ms()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn rec(seq: u64) -> FetchRecord {
        FetchRecord {
            seq,
            kind: "model_activity".into(),
            subject: "a/b".into(),
            documented: false,
            method: "GET".into(),
            url: "https://example/x".into(),
            status: 200,
            fetched_at_ms: 1,
            headers: BTreeMap::new(),
            sha256: String::new(),
            len: 0,
            client: "test".into(),
            error: None,
            chain: String::new(),
        }
    }

    #[test]
    fn append_only_chain_detects_edits() {
        let root = tmpdir("chain");
        {
            let l = Ledger::open_write(&root).unwrap();
            let sha = l.put_blob(b"hello").unwrap();
            let mut r = rec(1);
            r.sha256 = sha;
            l.append_fetch(&mut r).unwrap();
            l.append_fetch(&mut rec(2)).unwrap();
            assert_eq!(l.verify().unwrap().records["fetches.jsonl"], 2);
            // second writer is refused while the first holds the lock
            assert!(matches!(Ledger::open_write(&root), Err(LedgerError::Locked(_))));
        }
        // tamper with the first record
        let p = root.join("fetches.jsonl");
        let s = fs::read_to_string(&p).unwrap().replacen("\"status\":200", "\"status\":201", 1);
        fs::write(&p, s).unwrap();
        assert!(matches!(Ledger::open_read(&root).verify(), Err(LedgerError::Corrupt(_))));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn blob_tamper_detected() {
        let root = tmpdir("blob");
        let l = Ledger::open_write(&root).unwrap();
        let sha = l.put_blob(b"abc").unwrap();
        fs::write(l.blob_path(&sha), b"abd").unwrap();
        assert!(l.get_blob(&sha).is_err());
        drop(l);
        let _ = fs::remove_dir_all(&root);
    }
}
