//! zfx-oracle: a public NEAR record of reporter-signed ZFX Markets index prints.
//!
//! Anyone may relay prints. A print is accepted only if
//! - it decodes as canonical OraclePrint v1 (`zfx-oracle-print`, strict),
//! - its `index_id` is enabled by the owner, with matching `version`, `formula_hash`, `value_scale`,
//! - its window is complete before publication (`source_start < source_end <= published_at`)
//!   and it is not published in the future (10 minute skew),
//! - at least `quorum` distinct active reporters signed the canonical bytes (ed25519),
//! - it moves the index forward: a later source window, or for the same window a higher
//!   revision whose `prev_print_hash` is the current print's hash (same rule as the perps
//!   clearing contract). Replays and older prints are rejected.
//!
//! History is append-only per index and ordered by (source_end, revision). To save storage the
//! contract keeps the canonical bytes after the two strings (index_id and version are implied by
//! the index) and rebuilds the exact canonical bytes for views, so consumers can re-hash and
//! re-verify every signature off-chain.
//!
//! The owner prepays storage by transferring NEAR to the contract. A push that would leave less
//! than `min_free_yocto` above the storage requirement panics, so the whole batch is refused.
//!
//! Trust: the contract proves which reporter keys signed a print. It does not make the
//! OpenRouter source data true, complete or free of manipulation.

use near_sdk::json_types::U128;
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::serde_json::{json, Value};
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{env, near, require, AccountId, BorshStorageKey, PanicOnDefault};
use zfx_oracle_print::{OraclePrint, DOMAIN};

pub const FUTURE_SKEW_MS: i64 = 10 * 60_000;
pub const MAX_BATCH: usize = 100;
pub const MAX_SIGS: usize = 8;
pub const MAX_PRINT_BYTES: usize = 700;
pub const MAX_VIEW_LIMIT: u32 = 100;
const DAY_MS: i64 = 86_400_000;

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum K {
    Reporters,
    Indices,
    Names,
    History,
}

#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct Reporter {
    pub pk: [u8; 32],
    pub active: bool,
}

#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct IndexCfg {
    pub id: u16,
    pub version: String,
    pub formula_hash: [u8; 32],
    pub value_scale: u8,
    pub enabled: bool,
    pub count: u32,
    pub latest_hash: [u8; 32],
    pub latest_end_ms: i64,
    pub latest_rev: u32,
}

/// One stored print: canonical bytes after the version string, and (reporter index, signature).
#[near(serializers = [borsh])]
pub struct Rec {
    pub tail: Vec<u8>,
    pub sigs: Vec<(u8, [u8; 64])>,
}

#[near(serializers = [json])]
#[serde(rename_all = "camelCase")]
pub struct PushPrint {
    /// Canonical bytes, hex (the `canonicalHex` field of a published print).
    pub canonical_hex: String,
    /// `"ed25519:<pubkey hex>:<signature hex>"` (the `reporterSignatures` field).
    pub reporter_signatures: Vec<String>,
}

#[near(serializers = [json])]
pub struct IndexSpec {
    pub index_id: String,
    pub version: String,
    pub formula_hash: String,
    pub value_scale: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct PushResult {
    pub accepted: u32,
    pub rejected: Vec<(u32, String)>,
    pub storage_usage: u64,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Oracle {
    owner: AccountId,
    reporters: Vector<Reporter>,
    quorum: u8,
    indices: LookupMap<String, IndexCfg>,
    names: Vector<String>,
    history: LookupMap<(u16, u32), Rec>,
    min_free_yocto: u128,
    accepted_total: u64,
}

// ---------------------------------------------------------------------------------------------
// helpers

fn unhex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    let v = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    b.chunks(2).map(|p| Some(v(p[0])? << 4 | v(p[1])?)).collect()
}

fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    unhex(s)?.try_into().ok()
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// "YYYY-MM-DD" -> days since epoch.
pub fn parse_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

fn fmt_date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn fmt_ms(ms: i64) -> String {
    let day = ms.div_euclid(DAY_MS);
    let r = ms.rem_euclid(DAY_MS);
    format!("{}T{:02}:{:02}:{:02}.{:03}Z", fmt_date(day), r / 3_600_000, (r / 60_000) % 60, (r / 1000) % 60, r % 1000)
}

/// The UTC day a print is "for": the day containing its last source millisecond.
fn utc_day(source_end_ms: i64) -> i64 {
    (source_end_ms - 1).div_euclid(DAY_MS)
}

fn put_str(o: &mut Vec<u8>, s: &str) {
    o.extend_from_slice(&(s.len() as u32).to_le_bytes());
    o.extend_from_slice(s.as_bytes());
}

fn prefix_len(index_id: &str, version: &str) -> usize {
    DOMAIN.len() + 4 + index_id.len() + 4 + version.len()
}

// tail layout: value_scale 1 | value 8 | start 8 | end 8 | observed 8 | published 8 | valid 8 | ...
fn tail_end_ms(t: &[u8]) -> i64 {
    i64::from_le_bytes(t[17..25].try_into().unwrap())
}
fn tail_rev(t: &[u8]) -> u32 {
    let n = t.len();
    u32::from_le_bytes(t[n - 36..n - 32].try_into().unwrap())
}

fn fmt_scaled(v: u64, scale: u8) -> String {
    if scale == 0 {
        return v.to_string();
    }
    let p = 10u64.pow(scale as u32);
    format!("{}.{:0w$}", v / p, v % p, w = scale as usize)
}

#[cfg(not(feature = "dalek-verify"))]
fn sig_ok(sig: &[u8; 64], msg: &[u8], pk: &[u8; 32]) -> bool {
    env::ed25519_verify(sig, msg, pk)
}
#[cfg(feature = "dalek-verify")]
fn sig_ok(sig: &[u8; 64], msg: &[u8], pk: &[u8; 32]) -> bool {
    zfx_oracle_print::verify(msg, pk, sig)
}

// ---------------------------------------------------------------------------------------------

#[near]
impl Oracle {
    #[init]
    pub fn new(owner: AccountId, reporters: Vec<String>, quorum: u8, min_free_yocto: U128) -> Self {
        let mut s = Oracle {
            owner,
            reporters: Vector::new(K::Reporters),
            quorum,
            indices: LookupMap::new(K::Indices),
            names: Vector::new(K::Names),
            history: LookupMap::new(K::History),
            min_free_yocto: min_free_yocto.0,
            accepted_total: 0,
        };
        for r in reporters {
            s.add_reporter(&r);
        }
        require!(quorum >= 1 && quorum as u32 <= s.reporters.len(), "quorum must be between 1 and the number of reporters");
        s
    }

    fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "owner only");
    }

    fn add_reporter(&mut self, pk_hex: &str) {
        let pk = hex32(pk_hex.trim_start_matches("ed25519:")).unwrap_or_else(|| env::panic_str("reporter key must be 32 bytes hex"));
        for i in 0..self.reporters.len() {
            let r = self.reporters.get(i).unwrap().clone();
            if r.pk == pk {
                self.reporters.replace(i, Reporter { pk, active: true });
                return;
            }
        }
        require!(self.reporters.len() < 255, "reporter registry full");
        self.reporters.push(Reporter { pk, active: true });
    }

    // ---- owner ----

    /// Adds (or reactivates) and deactivates reporters. Entries are never deleted so stored
    /// signatures keep pointing at the right key.
    pub fn set_reporters(&mut self, add: Vec<String>, deactivate: Vec<String>) {
        self.assert_owner();
        for r in add {
            self.add_reporter(&r);
        }
        for r in deactivate {
            let pk = hex32(r.trim_start_matches("ed25519:")).unwrap_or_else(|| env::panic_str("bad key"));
            for i in 0..self.reporters.len() {
                if self.reporters.get(i).unwrap().pk == pk {
                    self.reporters.replace(i, Reporter { pk, active: false });
                }
            }
        }
        let active = (0..self.reporters.len()).filter(|i| self.reporters.get(*i).unwrap().active).count();
        require!(self.quorum as usize <= active, "quorum exceeds active reporters");
    }

    pub fn set_quorum(&mut self, quorum: u8) {
        self.assert_owner();
        let active = (0..self.reporters.len()).filter(|i| self.reporters.get(*i).unwrap().active).count();
        require!(quorum >= 1 && quorum as usize <= active, "quorum must be between 1 and the number of active reporters");
        self.quorum = quorum;
    }

    /// Enables index ids (or updates their version / formula hash / scale).
    pub fn set_indices(&mut self, specs: Vec<IndexSpec>) {
        self.assert_owner();
        for s in specs {
            require!(!s.index_id.is_empty() && s.index_id.len() <= zfx_oracle_print::MAX_STR_LEN, "bad index_id");
            require!(s.version.len() <= zfx_oracle_print::MAX_STR_LEN, "bad version");
            let fh = hex32(&s.formula_hash).unwrap_or_else(|| env::panic_str("formula_hash must be 32 bytes hex"));
            match self.indices.get(&s.index_id).cloned() {
                Some(mut c) => {
                    c.version = s.version;
                    c.formula_hash = fh;
                    c.value_scale = s.value_scale;
                    c.enabled = true;
                    self.indices.insert(s.index_id, c);
                }
                None => {
                    let id = self.names.len();
                    require!(id < u16::MAX as u32, "too many indices");
                    self.names.push(s.index_id.clone());
                    self.indices.insert(
                        s.index_id,
                        IndexCfg { id: id as u16, version: s.version, formula_hash: fh, value_scale: s.value_scale, enabled: true, count: 0, latest_hash: [0; 32], latest_end_ms: i64::MIN, latest_rev: 0 },
                    );
                }
            }
        }
    }

    pub fn set_enabled(&mut self, index_id: String, enabled: bool) {
        self.assert_owner();
        let mut c = self.indices.get(&index_id).cloned().unwrap_or_else(|| env::panic_str("unknown index_id"));
        c.enabled = enabled;
        self.indices.insert(index_id, c);
    }

    pub fn set_min_free(&mut self, min_free_yocto: U128) {
        self.assert_owner();
        self.min_free_yocto = min_free_yocto.0;
    }

    pub fn set_owner(&mut self, owner: AccountId) {
        self.assert_owner();
        self.owner = owner;
    }

    /// Anyone may add NEAR for storage (a plain transfer works too).
    #[payable]
    pub fn prepay(&mut self) {
        env::log_str(&format!("prepaid {} yoctoNEAR", env::attached_deposit().as_yoctonear()));
    }

    // ---- push ----

    /// Relays signed prints. Each print is checked independently; rejected prints are reported,
    /// not fatal, so re-sending a batch is idempotent. Panics (refusing the whole batch) only if
    /// the contract cannot pay for the storage.
    pub fn push(&mut self, prints: Vec<PushPrint>) -> PushResult {
        require!(prints.len() <= MAX_BATCH, "too many prints in one call");
        let now_ms = (env::block_timestamp() / 1_000_000) as i64;
        let mut accepted = 0u32;
        let mut rejected = Vec::new();
        let mut events = Vec::new();
        for (i, pp) in prints.iter().enumerate() {
            match self.accept(pp, now_ms) {
                Ok(ev) => {
                    accepted += 1;
                    events.push(ev);
                }
                Err(e) => rejected.push((i as u32, e)),
            }
        }
        let need = env::storage_usage() as u128 * env::storage_byte_cost().as_yoctonear();
        let bal = env::account_balance().as_yoctonear();
        if bal < need.saturating_add(self.min_free_yocto) {
            env::panic_str(&format!("storage balance short: balance {bal} < storage {need} + reserve {}; top up the contract", self.min_free_yocto));
        }
        self.accepted_total += accepted as u64;
        // NEAR caps the total log length of one execution at 16 KiB, so events are compact:
        // [index number (see index_info.number), utc_date, revision, print_hash]
        if !events.is_empty() {
            env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "zfx-oracle", "version": "1.0.0", "event": "print", "data": events})));
        }
        PushResult { accepted, rejected, storage_usage: env::storage_usage() }
    }

    fn accept(&mut self, pp: &PushPrint, now_ms: i64) -> Result<Value, String> {
        let bytes = unhex(&pp.canonical_hex).ok_or("canonical bytes are not hex")?;
        if bytes.len() > MAX_PRINT_BYTES {
            return Err("print too large".into());
        }
        let p = OraclePrint::decode(&bytes).map_err(|e| format!("decode: {e:?}"))?;
        let mut cfg = self.indices.get(&p.index_id).cloned().ok_or("unknown index_id")?;
        if !cfg.enabled {
            return Err("index_id disabled".into());
        }
        if p.version != cfg.version {
            return Err("version mismatch".into());
        }
        if p.formula_hash != cfg.formula_hash {
            return Err("formula_hash mismatch".into());
        }
        if p.value_scale != cfg.value_scale {
            return Err("value_scale mismatch".into());
        }
        if !(p.source_start_ms < p.source_end_ms && p.source_end_ms <= p.published_at_ms) {
            return Err("window not complete before publication".into());
        }
        if p.published_at_ms > now_ms + FUTURE_SKEW_MS {
            return Err("published in the future".into());
        }
        if pp.reporter_signatures.len() > MAX_SIGS {
            return Err("too many signatures".into());
        }
        let mut sigs: Vec<(u8, [u8; 64])> = Vec::new();
        for s in &pp.reporter_signatures {
            let mut it = s.split(':');
            let (Some("ed25519"), Some(pk), Some(sg), None) = (it.next(), it.next(), it.next(), it.next()) else { continue };
            let (Some(pk), Some(sg)) = (hex32(pk), unhex(sg).and_then(|v| <[u8; 64]>::try_from(v).ok())) else { continue };
            let Some(idx) = (0..self.reporters.len()).find(|i| {
                let r = self.reporters.get(*i).unwrap();
                r.active && r.pk == pk
            }) else {
                continue;
            };
            if sigs.iter().any(|(j, _)| *j as u32 == idx) {
                continue;
            }
            if sig_ok(&sg, &bytes, &pk) {
                sigs.push((idx as u8, sg));
            }
        }
        if self.quorum == 0 || sigs.len() < self.quorum as usize {
            return Err(format!("quorum not met: {} valid signatures from active reporters, {} required", sigs.len(), self.quorum));
        }
        let hash = env::sha256_array(&bytes);
        if cfg.count > 0 {
            let newer = p.source_end_ms > cfg.latest_end_ms || (p.source_end_ms == cfg.latest_end_ms && p.revision > cfg.latest_rev && p.prev_print_hash == cfg.latest_hash);
            if !newer {
                return Err(if hash == cfg.latest_hash { "duplicate of the latest print".into() } else { "not newer than the latest print (older window, or a correction not chained to the latest)".into() });
            }
        }
        let tail = bytes[prefix_len(&p.index_id, &p.version)..].to_vec();
        self.history.insert((cfg.id, cfg.count), Rec { tail, sigs });
        cfg.count += 1;
        cfg.latest_hash = hash;
        cfg.latest_end_ms = p.source_end_ms;
        cfg.latest_rev = p.revision;
        let ev = json!([cfg.id, fmt_date(utc_day(p.source_end_ms)), p.revision, hex(&hash)]);
        self.indices.insert(p.index_id, cfg);
        Ok(ev)
    }

    // ---- views ----

    fn rebuild(&self, index_id: &str, cfg: &IndexCfg, rec: &Rec) -> Vec<u8> {
        let mut b = Vec::with_capacity(prefix_len(index_id, &cfg.version) + rec.tail.len());
        b.extend_from_slice(DOMAIN);
        put_str(&mut b, index_id);
        put_str(&mut b, &cfg.version);
        b.extend_from_slice(&rec.tail);
        b
    }

    fn render(&self, index_id: &str, cfg: &IndexCfg, seq: u32) -> Option<Value> {
        let rec = self.history.get(&(cfg.id, seq))?;
        let bytes = self.rebuild(index_id, cfg, rec);
        let p = OraclePrint::decode(&bytes).ok()?;
        let now_ms = (env::block_timestamp() / 1_000_000) as i64;
        let sigs: Vec<String> = rec
            .sigs
            .iter()
            .map(|(i, s)| format!("ed25519:{}:{}", hex(&self.reporters.get(*i as u32).map(|r| r.pk).unwrap_or([0; 32])), hex(s)))
            .collect();
        Some(json!({
            "index_id": p.index_id,
            "seq": seq,
            "utc_date": fmt_date(utc_day(p.source_end_ms)),
            "revision": p.revision,
            "finality": p.finality.name(),
            "value": fmt_scaled(p.value, p.value_scale),
            "value_int": p.value.to_string(),
            "value_scale": p.value_scale,
            "status": p.status.name(),
            "data_status": p.data_status.name(),
            "stale": now_ms > p.valid_until_ms,
            "source_start": fmt_ms(p.source_start_ms),
            "source_end": fmt_ms(p.source_end_ms),
            "observed_at": fmt_ms(p.observed_at_ms),
            "published_at": fmt_ms(p.published_at_ms),
            "valid_until": fmt_ms(p.valid_until_ms),
            "formula_hash": hex(&p.formula_hash),
            "source_hash": hex(&p.source_hash),
            "prev_print_hash": hex(&p.prev_print_hash),
            "print_hash": hex(&env::sha256_array(&bytes)),
            "canonical_hex": hex(&bytes),
            "reporter_signatures": sigs,
        }))
    }

    /// First seq whose utc_date >= day (history is ordered by source_end).
    fn lower_bound(&self, cfg: &IndexCfg, day: i64) -> u32 {
        let (mut lo, mut hi) = (0u32, cfg.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let d = utc_day(tail_end_ms(&self.history.get(&(cfg.id, mid)).unwrap().tail));
            if d < day {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    pub fn latest(&self, index_id: String) -> Option<Value> {
        let cfg = self.indices.get(&index_id)?;
        if cfg.count == 0 {
            return None;
        }
        self.render(&index_id, cfg, cfg.count - 1)
    }

    pub fn history(&self, index_id: String, from_date: String, limit: Option<u32>) -> Vec<Value> {
        let Some(cfg) = self.indices.get(&index_id) else { return vec![] };
        let day = parse_date(&from_date).unwrap_or_else(|| env::panic_str("from_date must be YYYY-MM-DD"));
        let lim = limit.unwrap_or(MAX_VIEW_LIMIT).min(MAX_VIEW_LIMIT);
        let start = self.lower_bound(cfg, day);
        (start..cfg.count.min(start.saturating_add(lim))).filter_map(|s| self.render(&index_id, cfg, s)).collect()
    }

    pub fn print(&self, index_id: String, utc_date: String, revision: u32) -> Option<Value> {
        let cfg = self.indices.get(&index_id)?;
        let day = parse_date(&utc_date)?;
        let mut s = self.lower_bound(cfg, day);
        while s < cfg.count {
            let rec = self.history.get(&(cfg.id, s)).unwrap();
            if utc_day(tail_end_ms(&rec.tail)) != day {
                break;
            }
            if tail_rev(&rec.tail) == revision {
                return self.render(&index_id, cfg, s);
            }
            s += 1;
        }
        None
    }

    pub fn index_info(&self, index_id: String) -> Option<Value> {
        let c = self.indices.get(&index_id)?;
        Some(json!({"index_id": index_id, "number": c.id, "version": c.version, "formula_hash": hex(&c.formula_hash), "value_scale": c.value_scale, "enabled": c.enabled, "prints": c.count,
            "latest_utc_date": if c.count > 0 { Value::String(fmt_date(utc_day(c.latest_end_ms))) } else { Value::Null }, "latest_revision": c.latest_rev, "latest_print_hash": hex(&c.latest_hash)}))
    }

    pub fn indices(&self, from: Option<u32>, limit: Option<u32>) -> Vec<Value> {
        let f = from.unwrap_or(0);
        let l = limit.unwrap_or(MAX_VIEW_LIMIT).min(MAX_VIEW_LIMIT);
        (f..self.names.len().min(f.saturating_add(l))).filter_map(|i| self.index_info(self.names.get(i)?.clone())).collect()
    }

    pub fn config(&self) -> Value {
        let reps: Vec<Value> = (0..self.reporters.len()).map(|i| {
            let r = self.reporters.get(i).unwrap();
            json!({"index": i, "pubkey_hex": hex(&r.pk), "active": r.active})
        }).collect();
        json!({"owner": self.owner, "quorum": self.quorum, "reporters": reps, "indices": self.names.len(), "accepted_total": self.accepted_total, "min_free_yocto": self.min_free_yocto.to_string(),
            "print_format": "OraclePrint v1, domain ZFX-oracle-print-v1, ed25519 over canonical bytes",
            "trust_note": "Proves which reporter keys signed each print. Does not make the source data true, complete or manipulation-free."})
    }

    pub fn storage_status(&self) -> Value {
        let usage = env::storage_usage() as u128;
        let cost = env::storage_byte_cost().as_yoctonear();
        let bal = env::account_balance().as_yoctonear();
        let locked = usage * cost;
        json!({"storage_usage_bytes": usage.to_string(), "byte_cost_yocto": cost.to_string(), "balance_yocto": bal.to_string(), "storage_locked_yocto": locked.to_string(),
            "available_yocto": bal.saturating_sub(locked).saturating_sub(self.min_free_yocto).to_string(), "accepted_total": self.accepted_total})
    }
}

#[cfg(test)]
mod tests;
