//! Local SQLite store: migrations, MAC-chained audit events with retention,
//! versioned policies, settings history, consent records, apps and pending
//! approval metadata. Secret material lives only in encrypted columns written
//! by `pv-vault`.

use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use pv_core::{AuditEvent, PendingApproval, PendingStatus, PolicyRule};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

pub use rusqlite;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("schema version {found} is newer than supported {supported}")]
    TooNew { found: i64, supported: i64 },
    #[error("integrity check failed: {0}")]
    Integrity(String),
    #[error("not found")]
    NotFound,
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Ordered migrations. Index i upgrades user_version i -> i+1. Never edit a
/// shipped migration; append a new one.
pub const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE events(
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT UNIQUE NOT NULL,
        ts TEXT NOT NULL,
        kind TEXT NOT NULL,
        action TEXT NOT NULL,
        severity TEXT NOT NULL,
        app_id TEXT NOT NULL,
        json TEXT NOT NULL,
        retention_days INTEGER NOT NULL,
        prev_mac TEXT NOT NULL,
        mac TEXT NOT NULL,
        key_id TEXT NOT NULL
    );
    CREATE INDEX events_ts ON events(ts);
    CREATE TABLE policy_versions(
        id TEXT NOT NULL, version INTEGER NOT NULL, json TEXT NOT NULL,
        created_at TEXT NOT NULL, actor TEXT NOT NULL, active INTEGER NOT NULL,
        PRIMARY KEY(id, version)
    );
    CREATE TABLE settings(key TEXT PRIMARY KEY, json TEXT NOT NULL, version INTEGER NOT NULL, updated_at TEXT NOT NULL);
    CREATE TABLE settings_history(id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL, json TEXT NOT NULL, version INTEGER NOT NULL, updated_at TEXT NOT NULL, actor TEXT NOT NULL);
    CREATE TABLE consents(id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, granted INTEGER NOT NULL, details TEXT NOT NULL, policy_hash TEXT, created_at TEXT NOT NULL);
    CREATE TABLE apps(app_id TEXT PRIMARY KEY, name TEXT NOT NULL, key_hash TEXT UNIQUE NOT NULL, agent_id TEXT, destination_id TEXT, created_at TEXT NOT NULL, revoked_at TEXT);
    CREATE TABLE pending(approval_id TEXT PRIMARY KEY, json TEXT NOT NULL, status TEXT NOT NULL, request_hash TEXT NOT NULL, expires_at TEXT NOT NULL);
    CREATE INDEX pending_hash ON pending(request_hash);
    CREATE TABLE secrets(secret_id TEXT PRIMARY KEY, kind TEXT NOT NULL, label TEXT NOT NULL, norm TEXT NOT NULL, allow INTEGER NOT NULL, action TEXT NOT NULL, fingerprint TEXT NOT NULL, key_id TEXT NOT NULL, nonce BLOB NOT NULL, ct BLOB NOT NULL, created_at TEXT NOT NULL);
    CREATE TABLE mappings(token TEXT PRIMARY KEY, scope_id TEXT NOT NULL, category TEXT NOT NULL, fingerprint TEXT NOT NULL, key_id TEXT NOT NULL, nonce BLOB NOT NULL, ct BLOB NOT NULL, created_at TEXT NOT NULL, expires_at TEXT NOT NULL);
    CREATE INDEX mappings_scope_fp ON mappings(scope_id, fingerprint);
    CREATE TABLE pending_payloads(approval_id TEXT PRIMARY KEY, key_id TEXT NOT NULL, nonce BLOB NOT NULL, ct BLOB NOT NULL, expires_at TEXT NOT NULL);
    CREATE TABLE sealed(kind TEXT NOT NULL, id TEXT NOT NULL, key_id TEXT NOT NULL, nonce BLOB NOT NULL, ct BLOB NOT NULL, created_at TEXT NOT NULL, PRIMARY KEY(kind, id));
    CREATE TABLE providers(id TEXT PRIMARY KEY, json TEXT NOT NULL, updated_at TEXT NOT NULL);
    CREATE TABLE integrations(id TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE attestations(destination_id TEXT NOT NULL, model TEXT NOT NULL, json TEXT NOT NULL, verified_at TEXT NOT NULL, PRIMARY KEY(destination_id, model));
    "#,
];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let conn = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Self::init(conn)
    }

    pub fn open_memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)).unwrap_or_default();
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;")?;
        let db = Db { conn: Arc::new(Mutex::new(conn)) };
        db.migrate()?;
        Ok(db)
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn().query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    fn migrate(&self) -> Result<()> {
        let mut c = self.conn();
        let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(StoreError::TooNew { found: v, supported: SCHEMA_VERSION });
        }
        for (i, m) in MIGRATIONS.iter().enumerate().skip(v as usize) {
            let tx = c.transaction()?;
            tx.execute_batch(m)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn integrity_ok(&self) -> bool {
        self.conn().query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).map(|s| s == "ok").unwrap_or(false)
    }

    // ------------------------------------------------------------ meta

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn().query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
        Ok(())
    }

    // ------------------------------------------------------------ settings

    pub fn setting<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        let s: Option<String> = self.conn().query_row("SELECT json FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?;
        Ok(match s {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    /// Versioned settings write; history is kept for audit and rollback.
    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T, actor: &str) -> Result<i64> {
        let json = serde_json::to_string(value)?;
        let now = Utc::now().to_rfc3339();
        let mut c = self.conn();
        let tx = c.transaction()?;
        let v: i64 = tx.query_row("SELECT version FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?.unwrap_or(0) + 1;
        tx.execute("INSERT INTO settings(key,json,version,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(key) DO UPDATE SET json=excluded.json, version=excluded.version, updated_at=excluded.updated_at", params![key, json, v, now])?;
        tx.execute("INSERT INTO settings_history(key,json,version,updated_at,actor) VALUES(?1,?2,?3,?4,?5)", params![key, json, v, now, actor])?;
        tx.commit()?;
        Ok(v)
    }

    pub fn record_consent(&self, kind: &str, granted: bool, details: &serde_json::Value, policy_hash: Option<&str>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO consents(kind,granted,details,policy_hash,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![kind, granted as i32, details.to_string(), policy_hash, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn consents(&self) -> Result<Vec<ConsentRecord>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT kind,granted,details,policy_hash,created_at FROM consents ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok(ConsentRecord {
                kind: r.get(0)?,
                granted: r.get::<_, i32>(1)? != 0,
                details: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                policy_hash: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ------------------------------------------------------------ policies

    /// Store a new immutable version of a rule and make it the active one.
    pub fn put_policy(&self, rule: &PolicyRule, actor: &str) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let exists: Option<i64> = tx.query_row("SELECT 1 FROM policy_versions WHERE id=?1 AND version=?2", params![rule.id, rule.version], |r| r.get(0)).optional()?;
        if exists.is_some() {
            return Err(StoreError::Integrity(format!("policy {} v{} already exists; versions are immutable", rule.id, rule.version)));
        }
        tx.execute("UPDATE policy_versions SET active=0 WHERE id=?1", [&rule.id])?;
        tx.execute(
            "INSERT INTO policy_versions(id,version,json,created_at,actor,active) VALUES(?1,?2,?3,?4,?5,1)",
            params![rule.id, rule.version, serde_json::to_string(rule)?, Utc::now().to_rfc3339(), actor],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Deactivate a rule (deletion keeps history).
    pub fn deactivate_policy(&self, id: &str) -> Result<bool> {
        Ok(self.conn().execute("UPDATE policy_versions SET active=0 WHERE id=?1 AND active=1", [id])? > 0)
    }

    pub fn active_policies(&self) -> Result<Vec<PolicyRule>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT json FROM policy_versions WHERE active=1 ORDER BY id")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(serde_json::from_str(&r?)?);
        }
        Ok(out)
    }

    pub fn policy_history(&self, id: &str) -> Result<Vec<(PolicyRule, String, String, bool)>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT json,created_at,actor,active FROM policy_versions WHERE id=?1 ORDER BY version")?;
        let rows = st.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i32>(3)? != 0)))?;
        let mut out = Vec::new();
        for r in rows {
            let (j, a, b, act) = r?;
            out.push((serde_json::from_str(&j)?, a, b, act));
        }
        Ok(out)
    }

    pub fn latest_policy_version(&self, id: &str) -> Result<u32> {
        Ok(self.conn().query_row("SELECT COALESCE(MAX(version),0) FROM policy_versions WHERE id=?1", [id], |r| r.get::<_, i64>(0))? as u32)
    }

    // ------------------------------------------------------------ apps

    pub fn put_app(&self, app: &AppRecord) -> Result<()> {
        self.conn().execute(
            "INSERT INTO apps(app_id,name,key_hash,agent_id,destination_id,created_at,revoked_at) VALUES(?1,?2,?3,?4,?5,?6,NULL)
             ON CONFLICT(app_id) DO UPDATE SET name=excluded.name, key_hash=excluded.key_hash, agent_id=excluded.agent_id, destination_id=excluded.destination_id, revoked_at=NULL",
            params![app.app_id, app.name, app.key_hash, app.agent_id, app.destination_id, app.created_at],
        )?;
        Ok(())
    }

    pub fn app_by_key_hash(&self, key_hash: &str) -> Result<Option<AppRecord>> {
        Ok(self
            .conn()
            .query_row("SELECT app_id,name,key_hash,agent_id,destination_id,created_at,revoked_at FROM apps WHERE key_hash=?1 AND revoked_at IS NULL", [key_hash], app_row)
            .optional()?)
    }

    pub fn apps(&self) -> Result<Vec<AppRecord>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT app_id,name,key_hash,agent_id,destination_id,created_at,revoked_at FROM apps ORDER BY created_at")?;
        let rows = st.query_map([], app_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn revoke_app(&self, app_id: &str) -> Result<bool> {
        Ok(self.conn().execute("UPDATE apps SET revoked_at=?2 WHERE app_id=?1 AND revoked_at IS NULL", params![app_id, Utc::now().to_rfc3339()])? > 0)
    }

    pub fn update_app_routing(&self, app_id: &str, agent_id: Option<&str>, destination_id: Option<&str>) -> Result<bool> {
        Ok(self.conn().execute("UPDATE apps SET agent_id=?2, destination_id=?3 WHERE app_id=?1", params![app_id, agent_id, destination_id])? > 0)
    }

    // ------------------------------------------------------------ providers / integrations / attestations

    pub fn put_json(&self, table: JsonTable, id: &str, value: &impl Serialize) -> Result<()> {
        let json = serde_json::to_string(value)?;
        let c = self.conn();
        match table {
            JsonTable::Providers => c.execute("INSERT INTO providers(id,json,updated_at) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET json=excluded.json, updated_at=excluded.updated_at", params![id, json, Utc::now().to_rfc3339()])?,
            JsonTable::Integrations => c.execute("INSERT INTO integrations(id,json) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET json=excluded.json", params![id, json])?,
        };
        Ok(())
    }

    pub fn list_json<T: for<'de> Deserialize<'de>>(&self, table: JsonTable) -> Result<Vec<T>> {
        let c = self.conn();
        let sql = match table {
            JsonTable::Providers => "SELECT json FROM providers ORDER BY id",
            JsonTable::Integrations => "SELECT json FROM integrations ORDER BY id",
        };
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(serde_json::from_str(&r?)?);
        }
        Ok(out)
    }

    pub fn delete_json(&self, table: JsonTable, id: &str) -> Result<bool> {
        let sql = match table {
            JsonTable::Providers => "DELETE FROM providers WHERE id=?1",
            JsonTable::Integrations => "DELETE FROM integrations WHERE id=?1",
        };
        Ok(self.conn().execute(sql, [id])? > 0)
    }

    pub fn put_attestation(&self, destination_id: &str, model: &str, json: &serde_json::Value) -> Result<()> {
        self.conn().execute(
            "INSERT INTO attestations(destination_id,model,json,verified_at) VALUES(?1,?2,?3,?4) ON CONFLICT(destination_id,model) DO UPDATE SET json=excluded.json, verified_at=excluded.verified_at",
            params![destination_id, model, json.to_string(), Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn attestations(&self) -> Result<Vec<serde_json::Value>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT json FROM attestations ORDER BY destination_id, model")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(serde_json::from_str(&r?)?);
        }
        Ok(out)
    }

    // ------------------------------------------------------------ pending approvals (metadata)

    pub fn put_pending(&self, p: &PendingApproval) -> Result<()> {
        self.conn().execute(
            "INSERT INTO pending(approval_id,json,status,request_hash,expires_at) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(approval_id) DO UPDATE SET json=excluded.json, status=excluded.status, expires_at=excluded.expires_at",
            params![p.approval_id, serde_json::to_string(p)?, status_str(p.status), p.request_hash, p.expires_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_pending(&self, id: &str) -> Result<Option<PendingApproval>> {
        let s: Option<String> = self.conn().query_row("SELECT json FROM pending WHERE approval_id=?1", [id], |r| r.get(0)).optional()?;
        Ok(match s {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    pub fn pending_by_hash(&self, request_hash: &str) -> Result<Vec<PendingApproval>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT json FROM pending WHERE request_hash=?1 ORDER BY expires_at DESC")?;
        let rows = st.query_map([request_hash], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(serde_json::from_str(&r?)?);
        }
        Ok(out)
    }

    pub fn list_pending(&self, include_closed: bool) -> Result<Vec<PendingApproval>> {
        let c = self.conn();
        let sql = if include_closed { "SELECT json FROM pending ORDER BY expires_at DESC LIMIT 500" } else { "SELECT json FROM pending WHERE status IN ('PENDING','APPROVED') ORDER BY expires_at DESC" };
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(serde_json::from_str(&r?)?);
        }
        Ok(out)
    }

    /// Mark expired pending/approved records and delete their encrypted payloads.
    /// Returns IDs that expired. Safe to run at startup (crash recovery).
    pub fn expire_pending(&self, now: DateTime<Utc>) -> Result<Vec<String>> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let ids: Vec<String> = {
            let mut st = tx.prepare("SELECT approval_id FROM pending WHERE status IN ('PENDING','APPROVED') AND expires_at <= ?1")?;
            let rows = st.query_map([now.to_rfc3339()], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for id in &ids {
            let json: String = tx.query_row("SELECT json FROM pending WHERE approval_id=?1", [id], |r| r.get(0))?;
            let mut p: PendingApproval = serde_json::from_str(&json)?;
            p.status = PendingStatus::Expired;
            tx.execute("UPDATE pending SET status='EXPIRED', json=?2 WHERE approval_id=?1", params![id, serde_json::to_string(&p)?])?;
        }
        // Payloads are destroyed at expiry regardless of status bookkeeping.
        tx.execute("DELETE FROM pending_payloads WHERE expires_at <= ?1", [now.to_rfc3339()])?;
        tx.commit()?;
        Ok(ids)
    }

    /// Atomically mark an approval as consumed. Returns false if already used.
    pub fn consume_approval(&self, approval_id: &str) -> Result<bool> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let json: Option<String> = tx.query_row("SELECT json FROM pending WHERE approval_id=?1 AND status='APPROVED'", [approval_id], |r| r.get(0)).optional()?;
        let Some(json) = json else {
            return Ok(false);
        };
        let mut p: PendingApproval = serde_json::from_str(&json)?;
        p.status = PendingStatus::Consumed;
        tx.execute("UPDATE pending SET status='CONSUMED', json=?2 WHERE approval_id=?1 AND status='APPROVED'", params![approval_id, serde_json::to_string(&p)?])?;
        tx.execute("DELETE FROM pending_payloads WHERE approval_id=?1", [approval_id])?;
        tx.commit()?;
        Ok(true)
    }

    // ------------------------------------------------------------ audit events

    /// Append an event to the MAC chain. `mac_key` comes from the vault key set.
    pub fn append_event(&self, ev: &AuditEvent, retention_days: u32, mac_key: &[u8], key_id: &str) -> Result<()> {
        let json = serde_json::to_string(ev)?;
        let mut c = self.conn();
        let tx = c.transaction()?;
        let prev: String = match tx.query_row("SELECT mac FROM events ORDER BY seq DESC LIMIT 1", [], |r| r.get::<_, String>(0)).optional()? {
            Some(m) => m,
            None => tx.query_row("SELECT value FROM meta WHERE key='audit_anchor'", [], |r| r.get::<_, String>(0)).optional()?.unwrap_or_else(|| "genesis".into()),
        };
        let mac = chain_mac(mac_key, &prev, &json);
        tx.execute(
            "INSERT INTO events(event_id,ts,kind,action,severity,app_id,json,retention_days,prev_mac,mac,key_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![ev.event_id, ev.timestamp.to_rfc3339(), ev.kind, ev.action, format!("{:?}", ev.severity).to_lowercase(), ev.app_id, json, retention_days, prev, mac, key_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn events(&self, q: &EventQuery) -> Result<Vec<AuditEvent>> {
        let c = self.conn();
        let mut sql = String::from("SELECT json FROM events WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(a) = &q.action {
            sql.push_str(" AND action=?");
            args.push(Box::new(a.clone()));
        }
        if let Some(s) = &q.severity {
            sql.push_str(" AND severity=?");
            args.push(Box::new(s.clone()));
        }
        if let Some(a) = &q.app_id {
            sql.push_str(" AND app_id=?");
            args.push(Box::new(a.clone()));
        }
        if let Some(k) = &q.kind {
            sql.push_str(" AND kind=?");
            args.push(Box::new(k.clone()));
        }
        if let Some(since) = &q.since {
            sql.push_str(" AND ts>=?");
            args.push(Box::new(since.to_rfc3339()));
        }
        sql.push_str(" ORDER BY seq DESC LIMIT ? OFFSET ?");
        args.push(Box::new(q.limit.unwrap_or(100).min(10_000) as i64));
        args.push(Box::new(q.offset.unwrap_or(0) as i64));
        let mut st = c.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = st.query_map(refs.as_slice(), |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(serde_json::from_str(&r?)?);
        }
        Ok(out)
    }

    pub fn event(&self, id: &str) -> Result<Option<AuditEvent>> {
        let s: Option<String> = self.conn().query_row("SELECT json FROM events WHERE event_id=?1", [id], |r| r.get(0)).optional()?;
        Ok(match s {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    pub fn event_stats(&self) -> Result<EventStats> {
        let c = self.conn();
        let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64;
        Ok(EventStats {
            total_requests: count("SELECT COUNT(*) FROM events WHERE kind='request'"),
            transformed: count("SELECT COUNT(*) FROM events WHERE kind='request' AND action IN ('TRANSFORM','TOKENIZE','REDACT','GENERALIZE')"),
            blocked: count("SELECT COUNT(*) FROM events WHERE action IN ('BLOCK','PV_POLICY_BLOCKED','PV_ROUTE_FORBIDDEN')"),
            review: count("SELECT COUNT(*) FROM events WHERE action='REQUIRE_APPROVAL'"),
            private_routed: count("SELECT COUNT(*) FROM events WHERE action='PRIVATE_ROUTE'"),
            errors: count("SELECT COUNT(*) FROM events WHERE kind='error'"),
        })
    }

    /// Verify the MAC chain. `keys` maps key_id -> MAC key.
    pub fn verify_chain(&self, keys: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<u64> {
        let c = self.conn();
        let anchor: String = c.query_row("SELECT value FROM meta WHERE key='audit_anchor'", [], |r| r.get(0)).optional()?.unwrap_or_else(|| "genesis".into());
        let mut st = c.prepare("SELECT seq,json,prev_mac,mac,key_id FROM events ORDER BY seq")?;
        let mut rows = st.query([])?;
        let mut prev = anchor;
        let mut n = 0u64;
        while let Some(r) = rows.next()? {
            let seq: i64 = r.get(0)?;
            let json: String = r.get(1)?;
            let prev_mac: String = r.get(2)?;
            let mac: String = r.get(3)?;
            let kid: String = r.get(4)?;
            if prev_mac != prev {
                return Err(StoreError::Integrity(format!("chain break at seq {seq}")));
            }
            let key = keys(&kid).ok_or_else(|| StoreError::Integrity(format!("unknown key {kid} at seq {seq}")))?;
            if chain_mac(&key, &prev_mac, &json) != mac {
                return Err(StoreError::Integrity(format!("MAC mismatch at seq {seq}")));
            }
            prev = mac;
            n += 1;
        }
        Ok(n)
    }

    /// Re-MAC every event with a new key (key rotation). Verifies first.
    pub fn rekey_chain(&self, old: &dyn Fn(&str) -> Option<Vec<u8>>, new_key: &[u8], new_key_id: &str) -> Result<u64> {
        self.verify_chain(old)?;
        let mut c = self.conn();
        let tx = c.transaction()?;
        let anchor: String = tx.query_row("SELECT value FROM meta WHERE key='audit_anchor'", [], |r| r.get(0)).optional()?.unwrap_or_else(|| "genesis".into());
        let rows: Vec<(i64, String)> = {
            let mut st = tx.prepare("SELECT seq,json FROM events ORDER BY seq")?;
            let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        let mut prev = anchor;
        for (seq, json) in &rows {
            let mac = chain_mac(new_key, &prev, json);
            tx.execute("UPDATE events SET prev_mac=?2, mac=?3, key_id=?4 WHERE seq=?1", params![seq, prev, mac, new_key_id])?;
            prev = mac;
        }
        tx.commit()?;
        Ok(rows.len() as u64)
    }

    /// Delete events past their retention. The MAC of the newest deleted event
    /// becomes the chain anchor so the remaining chain still verifies.
    pub fn enforce_retention(&self, now: DateTime<Utc>) -> Result<u64> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let rows: Vec<(i64, String, i64)> = {
            let mut st = tx.prepare("SELECT seq,ts,retention_days FROM events ORDER BY seq")?;
            let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            it.collect::<std::result::Result<_, _>>()?
        };
        // Delete only a contiguous expired prefix so the chain stays verifiable.
        let mut last_seq = None;
        for (seq, ts, days) in rows {
            let t = DateTime::parse_from_rfc3339(&ts).map(|d| d.with_timezone(&Utc)).unwrap_or(now);
            if t + Duration::days(days) <= now {
                last_seq = Some(seq);
            } else {
                break;
            }
        }
        let Some(last) = last_seq else {
            return Ok(0);
        };
        let anchor: String = tx.query_row("SELECT mac FROM events WHERE seq=?1", [last], |r| r.get(0))?;
        let n = tx.execute("DELETE FROM events WHERE seq<=?1", [last])?;
        tx.execute("INSERT INTO meta(key,value) VALUES('audit_anchor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [anchor])?;
        tx.commit()?;
        Ok(n as u64)
    }

    /// Delete all events (owner request). Records a new anchor.
    pub fn delete_all_events(&self) -> Result<u64> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let n = tx.execute("DELETE FROM events", [])?;
        tx.execute("INSERT INTO meta(key,value) VALUES('audit_anchor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [format!("reset-{}", uuid::Uuid::new_v4())])?;
        tx.commit()?;
        Ok(n as u64)
    }

    /// Delete expired reversible mappings.
    pub fn expire_mappings(&self, now: DateTime<Utc>) -> Result<u64> {
        Ok(self.conn().execute("DELETE FROM mappings WHERE expires_at <= ?1", [now.to_rfc3339()])? as u64)
    }
}

pub fn chain_mac(key: &[u8], prev: &str, json: &str) -> String {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac key");
    m.update(prev.as_bytes());
    m.update(b"\n");
    m.update(json.as_bytes());
    hex::encode(m.finalize().into_bytes())
}

fn status_str(s: PendingStatus) -> &'static str {
    match s {
        PendingStatus::Pending => "PENDING",
        PendingStatus::Approved => "APPROVED",
        PendingStatus::Denied => "DENIED",
        PendingStatus::Cancelled => "CANCELLED",
        PendingStatus::Expired => "EXPIRED",
        PendingStatus::Consumed => "CONSUMED",
    }
}

fn app_row(r: &rusqlite::Row) -> rusqlite::Result<AppRecord> {
    Ok(AppRecord {
        app_id: r.get(0)?,
        name: r.get(1)?,
        key_hash: r.get(2)?,
        agent_id: r.get(3)?,
        destination_id: r.get(4)?,
        created_at: r.get(5)?,
        revoked_at: r.get(6)?,
    })
}

#[derive(Debug, Clone, Copy)]
pub enum JsonTable {
    Providers,
    Integrations,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppRecord {
    pub app_id: String,
    pub name: String,
    /// SHA-256 of the local caller key; the key itself is shown once at creation.
    pub key_hash: String,
    pub agent_id: Option<String>,
    pub destination_id: Option<String>,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub kind: String,
    pub granted: bool,
    pub details: serde_json::Value,
    pub policy_hash: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventQuery {
    pub action: Option<String>,
    pub severity: Option<String>,
    pub app_id: Option<String>,
    pub kind: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventStats {
    pub total_requests: u64,
    pub transformed: u64,
    pub blocked: u64,
    pub review: u64,
    pub private_routed: u64,
    pub errors: u64,
}
