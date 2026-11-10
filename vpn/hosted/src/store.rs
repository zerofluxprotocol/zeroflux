//! Hosted control-plane storage: tenants, API keys (hashed), usage accounting.
//! Contains no prompt content, no model output and no placeholder maps.

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use pv_audit::rusqlite::{params, Connection, OptionalExtension};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub const KEY_PREFIX: &str = "zfx_sk_";

/// Scopes a tenant API key can hold.
pub const SCOPES: &[&str] = &["inference", "models:read", "privacy:read"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Quota {
    /// Billable requests per calendar month (UTC). 0 = unlimited.
    #[serde(default)]
    pub requests_per_month: u64,
    /// Upstream tokens (prompt + completion, as reported by the upstream) per month. 0 = unlimited.
    #[serde(default)]
    pub tokens_per_month: u64,
    /// Requests per minute per API key (token bucket). 0 = unlimited.
    #[serde(default = "default_rpm")]
    pub requests_per_minute: u32,
}

fn default_rpm() -> u32 {
    60
}

impl Default for Quota {
    fn default() -> Self {
        Quota { requests_per_month: 0, tokens_per_month: 0, requests_per_minute: default_rpm() }
    }
}

/// Tenant configuration. Upstream credentials are not part of this struct:
/// they are sealed in the tenant's own vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantConfig {
    pub name: String,
    /// Upstream destinations (OpenAI-compatible endpoints, NEAR AI).
    pub upstreams: Vec<pv_core::ProviderConfig>,
    pub default_destination: Option<String>,
    /// Per-category action choices for noncritical categories (critical ones always block).
    #[serde(default)]
    pub categories: pv_core::CategoryChoices,
    /// Confidential fallback: NEAR AI destination + model, and the tenant
    /// owner's consent to send content there when transformation is not enough.
    #[serde(default)]
    pub private_destination: Option<String>,
    #[serde(default)]
    pub private_model: Option<String>,
    #[serde(default)]
    pub tee_consent: bool,
    #[serde(default)]
    pub quota: Quota,
    /// Days of metadata retention (no content is ever retained).
    #[serde(default = "default_retention")]
    pub metadata_retention_days: u32,
}

fn default_retention() -> u32 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tenant {
    pub tenant_id: String,
    pub config: TenantConfig,
    pub created_at: String,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiKeyInfo {
    pub key_id: String,
    pub tenant_id: String,
    /// First characters of the key, for identification only.
    pub prefix: String,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UsageSummary {
    pub month: String,
    pub requests: u64,
    pub billable_requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub blocked: u64,
}

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tenants(tenant_id TEXT PRIMARY KEY, json TEXT NOT NULL, created_at TEXT NOT NULL, disabled INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS api_keys(key_id TEXT PRIMARY KEY, tenant_id TEXT NOT NULL, key_hash TEXT UNIQUE NOT NULL, prefix TEXT NOT NULL, scopes TEXT NOT NULL, created_at TEXT NOT NULL, revoked_at TEXT);
CREATE TABLE IF NOT EXISTS usage(id INTEGER PRIMARY KEY AUTOINCREMENT, tenant_id TEXT NOT NULL, key_id TEXT NOT NULL, request_id TEXT, ts TEXT NOT NULL, endpoint TEXT NOT NULL, status INTEGER NOT NULL, error_code TEXT, privacy_tier TEXT, prompt_tokens INTEGER NOT NULL, completion_tokens INTEGER NOT NULL, billable INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS usage_tenant_ts ON usage(tenant_id, ts);
"#;

pub fn key_hash(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let c = Connection::open(path)?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA secure_delete=ON;")?;
        c.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(c) })
    }

    pub fn open_memory() -> Result<Store> {
        let c = Connection::open_in_memory()?;
        c.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(c) })
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn create_tenant(&self, cfg: &TenantConfig) -> Result<Tenant> {
        validate(cfg)?;
        let t = Tenant { tenant_id: format!("tn_{}", rand_hex(8)), config: cfg.clone(), created_at: Utc::now().to_rfc3339(), disabled: false };
        self.c().execute("INSERT INTO tenants(tenant_id,json,created_at,disabled) VALUES(?1,?2,?3,0)", params![t.tenant_id, serde_json::to_string(&t.config)?, t.created_at])?;
        Ok(t)
    }

    pub fn update_tenant(&self, id: &str, cfg: &TenantConfig) -> Result<()> {
        validate(cfg)?;
        let n = self.c().execute("UPDATE tenants SET json=?2 WHERE tenant_id=?1", params![id, serde_json::to_string(cfg)?])?;
        if n == 0 {
            bail!("no such tenant");
        }
        Ok(())
    }

    pub fn set_disabled(&self, id: &str, disabled: bool) -> Result<bool> {
        Ok(self.c().execute("UPDATE tenants SET disabled=?2 WHERE tenant_id=?1", params![id, disabled as i64])? == 1)
    }

    pub fn tenant(&self, id: &str) -> Result<Option<Tenant>> {
        let row: Option<(String, String, String, i64)> = self
            .c()
            .query_row("SELECT tenant_id,json,created_at,disabled FROM tenants WHERE tenant_id=?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        row.map(|(tenant_id, j, created_at, d)| Ok(Tenant { tenant_id, config: serde_json::from_str(&j)?, created_at, disabled: d != 0 })).transpose()
    }

    pub fn tenants(&self) -> Result<Vec<Tenant>> {
        let ids: Vec<String> = {
            let c = self.c();
            let mut st = c.prepare("SELECT tenant_id FROM tenants ORDER BY created_at")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            v
        };
        ids.iter().filter_map(|i| self.tenant(i).transpose()).collect()
    }

    /// Create a key; the plaintext is returned once and only its hash is stored.
    pub fn create_key(&self, tenant_id: &str, scopes: &[String]) -> Result<(ApiKeyInfo, String)> {
        if self.tenant(tenant_id)?.is_none() {
            bail!("no such tenant");
        }
        for s in scopes {
            if !SCOPES.contains(&s.as_str()) {
                bail!("unknown scope {s}; allowed: {}", SCOPES.join(", "));
            }
        }
        if scopes.is_empty() {
            bail!("a key needs at least one scope");
        }
        let key = format!("{KEY_PREFIX}{}", rand_hex(24));
        let info = ApiKeyInfo { key_id: format!("key_{}", rand_hex(6)), tenant_id: tenant_id.into(), prefix: key[..KEY_PREFIX.len() + 6].to_string(), scopes: scopes.to_vec(), created_at: Utc::now().to_rfc3339(), revoked_at: None };
        self.c().execute(
            "INSERT INTO api_keys(key_id,tenant_id,key_hash,prefix,scopes,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![info.key_id, info.tenant_id, key_hash(&key), info.prefix, info.scopes.join(" "), info.created_at],
        )?;
        Ok((info, key))
    }

    pub fn revoke_key(&self, tenant_id: &str, key_id: &str) -> Result<bool> {
        Ok(self.c().execute("UPDATE api_keys SET revoked_at=?3 WHERE key_id=?1 AND tenant_id=?2 AND revoked_at IS NULL", params![key_id, tenant_id, Utc::now().to_rfc3339()])? == 1)
    }

    /// Replace a key: issue a new one with the same scopes, revoke the old one.
    pub fn rotate_key(&self, tenant_id: &str, key_id: &str) -> Result<(ApiKeyInfo, String)> {
        let old = self.keys(tenant_id)?.into_iter().find(|k| k.key_id == key_id && k.revoked_at.is_none()).ok_or_else(|| anyhow!("no such active key"))?;
        let new = self.create_key(tenant_id, &old.scopes)?;
        self.revoke_key(tenant_id, key_id)?;
        Ok(new)
    }

    pub fn keys(&self, tenant_id: &str) -> Result<Vec<ApiKeyInfo>> {
        let c = self.c();
        let mut st = c.prepare("SELECT key_id,tenant_id,prefix,scopes,created_at,revoked_at FROM api_keys WHERE tenant_id=?1 ORDER BY created_at")?;
        let v = st
            .query_map(params![tenant_id], |r| {
                Ok(ApiKeyInfo { key_id: r.get(0)?, tenant_id: r.get(1)?, prefix: r.get(2)?, scopes: r.get::<_, String>(3)?.split(' ').map(String::from).collect(), created_at: r.get(4)?, revoked_at: r.get(5)? })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(v)
    }

    /// Resolve an active key of an enabled tenant.
    pub fn authenticate(&self, key: &str) -> Result<Option<(Tenant, ApiKeyInfo)>> {
        if !key.starts_with(KEY_PREFIX) {
            return Ok(None);
        }
        let row: Option<String> = self.c().query_row("SELECT key_id FROM api_keys WHERE key_hash=?1 AND revoked_at IS NULL", params![key_hash(key)], |r| r.get(0)).optional()?;
        let Some(kid) = row else { return Ok(None) };
        let tid: String = self.c().query_row("SELECT tenant_id FROM api_keys WHERE key_id=?1", params![kid], |r| r.get(0))?;
        let Some(t) = self.tenant(&tid)? else { return Ok(None) };
        if t.disabled {
            return Ok(None);
        }
        let k = self.keys(&tid)?.into_iter().find(|k| k.key_id == kid).ok_or_else(|| anyhow!("key vanished"))?;
        Ok(Some((t, k)))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_usage(&self, tenant_id: &str, key_id: &str, request_id: Option<&str>, endpoint: &str, status: u16, error_code: Option<&str>, tier: Option<&str>, prompt_tokens: u64, completion_tokens: u64) -> Result<()> {
        let billable = status == 200;
        self.c().execute(
            "INSERT INTO usage(tenant_id,key_id,request_id,ts,endpoint,status,error_code,privacy_tier,prompt_tokens,completion_tokens,billable) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![tenant_id, key_id, request_id, Utc::now().to_rfc3339(), endpoint, status as i64, error_code, tier, prompt_tokens as i64, completion_tokens as i64, billable as i64],
        )?;
        Ok(())
    }

    pub fn month_usage(&self, tenant_id: &str, now: DateTime<Utc>) -> Result<UsageSummary> {
        let start = Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0).unwrap();
        let (requests, billable, p, c, blocked): (i64, i64, i64, i64, i64) = self.c().query_row(
            "SELECT COUNT(*), COALESCE(SUM(billable),0), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(CASE WHEN status=403 THEN 1 ELSE 0 END),0) FROM usage WHERE tenant_id=?1 AND ts>=?2",
            params![tenant_id, start.to_rfc3339()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        Ok(UsageSummary { month: start.format("%Y-%m").to_string(), requests: requests as u64, billable_requests: billable as u64, prompt_tokens: p as u64, completion_tokens: c as u64, blocked: blocked as u64 })
    }

    /// Metadata retention: usage rows older than the tenant's retention.
    pub fn enforce_retention(&self) -> Result<u64> {
        let mut n = 0;
        for t in self.tenants()? {
            let before = Utc::now() - chrono::Duration::days(t.config.metadata_retention_days as i64);
            n += self.c().execute("DELETE FROM usage WHERE tenant_id=?1 AND ts<?2", params![t.tenant_id, before.to_rfc3339()])? as u64;
        }
        Ok(n)
    }
}

fn validate(cfg: &TenantConfig) -> Result<()> {
    if cfg.name.trim().is_empty() || cfg.name.len() > 100 {
        bail!("tenant name required (max 100 chars)");
    }
    let mut ids = std::collections::HashSet::new();
    for u in &cfg.upstreams {
        if !ids.insert(u.id.clone()) || u.id.is_empty() || !u.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("upstream ids must be unique [A-Za-z0-9_-]+");
        }
        if u.zfx_hosted_transport {
            bail!("a hosted tenant cannot use another hosted gateway as upstream");
        }
    }
    if let Some(d) = &cfg.default_destination {
        if !ids.contains(d) {
            bail!("default_destination is not one of the upstreams");
        }
    }
    if let Some(p) = &cfg.private_destination {
        match cfg.upstreams.iter().find(|u| &u.id == p) {
            Some(u) if u.kind == pv_core::ProviderKind::NearAi => {}
            _ => bail!("private_destination must be a NEAR AI upstream"),
        }
    }
    for (c, a) in &cfg.categories {
        if pv_core::category::info(c).is_some_and(|i| i.critical) && *a != pv_core::Action::Block {
            bail!("critical category {c} cannot be weakened in hosted mode");
        }
    }
    Ok(())
}
