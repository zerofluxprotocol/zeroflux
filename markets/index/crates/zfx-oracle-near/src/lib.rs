//! NEAR client for the `zfx-oracle` contract (`markets/index/contracts/zfx-oracle`).
//!
//! - [`Near`]: a small wrapper over `near-api` with RPC failover and a tx log.
//! - [`local_prints`]: signed prints from the index ledger's revision files.
//! - [`ensure_indices`]: registers index ids the ledger has prints for (owner only).
//! - [`push_new`]: sends prints newer than each index's on-chain latest, batched under the gas
//!   limit. Idempotent: anything already on chain is skipped locally and rejected by the contract.
//! - [`sandbox`]: a local nearcore sandbox for tests.

pub mod sandbox;

use anyhow::{anyhow, bail, Context, Result};
use near_api::{RPCEndpoint, RetryMethod};
use near_api::{AccountId, Contract, NearGas, NearToken, NetworkConfig, Signer};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Daily revision files whose prints the oracle archives by default (dataset index, daily and 7D).
pub const DEFAULT_FILES: &[&str] = &["revisions_lic.jsonl", "revisions_lic_7d.jsonl"];
pub const PUSH_GAS_TGAS: u64 = 300;
/// Gas budget per batch, leaving headroom under the 300 Tgas transaction limit.
pub const BATCH_BUDGET_TGAS: u64 = 240;
pub const MAX_BATCH: usize = 100;

#[derive(Clone)]
pub struct Actor {
    pub id: AccountId,
    pub signer: Arc<Signer>,
}

impl std::fmt::Debug for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Actor({})", self.id)
    }
}

impl Actor {
    pub fn new(id: &str, secret_key: &str) -> Result<Self> {
        let sk = secret_key.parse().map_err(|e| anyhow!("secret key for {id}: {e:?}"))?;
        Ok(Actor { id: id.parse().with_context(|| format!("account id {id}"))?, signer: Signer::from_secret_key(sk)? })
    }

    /// Reads a NEAR credentials JSON (`private_key` or `secret_key`, optional `account_id`).
    /// The key is never printed.
    pub fn from_key_file(account: Option<&str>, path: &Path) -> Result<Self> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?)?;
        let sk = v["private_key"].as_str().or(v["secret_key"].as_str()).context("key file has no private_key")?;
        let id = account.map(String::from).or_else(|| v["account_id"].as_str().map(String::from)).context("account id not given and not in key file")?;
        Actor::new(&id, sk)
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub gas_burnt: u64,
    pub tx_hash: String,
    pub value: Value,
    pub logs: Vec<String>,
}

#[derive(Clone)]
pub struct Near {
    pub network: NetworkConfig,
    pub tx_log: Arc<Mutex<Vec<(String, String)>>>,
}

impl Near {
    /// One or more RPC URLs, tried in order with failover.
    pub fn rpcs(urls: &[String]) -> Result<Self> {
        let first = urls.first().context("at least one RPC url")?;
        let mut network = NetworkConfig::from_rpc_url("custom", first.parse().context("rpc url")?);
        network.rpc_endpoints = urls
            .iter()
            .map(|u| Ok(RPCEndpoint { url: u.parse().context("rpc url")?, bearer_header: None, retries: 3, retry_method: RetryMethod::ExponentialBackoff { initial_sleep: std::time::Duration::from_millis(500), factor: 2 } }))
            .collect::<Result<_>>()?;
        Ok(Near { network, tx_log: Default::default() })
    }

    pub fn record(&self, what: impl Into<String>, hash: &str) {
        self.tx_log.lock().unwrap().push((what.into(), hash.to_string()));
    }

    pub async fn call(&self, who: &Actor, contract: &str, method: &str, args: Value, deposit: NearToken, gas_tgas: u64) -> Result<Outcome> {
        let c: AccountId = contract.parse()?;
        let r = Contract(c.clone())
            .call_function(method, args)
            .transaction()
            .gas(NearGas::from_tgas(gas_tgas))
            .deposit(deposit)
            .with_signer(who.id.clone(), who.signer.clone())
            .send_to(&self.network)
            .await
            .with_context(|| format!("{method} on {c}"))?;
        let full = r.into_full().ok_or_else(|| anyhow!("{method}: transaction still pending"))?;
        let gas_burnt = full.total_gas_burnt.as_gas();
        let tx_hash = full.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
        self.record(format!("{method} on {c} by {}", who.id), &tx_hash);
        if full.clone().into_result().is_err() {
            let s = format!("{:?}", full.clone().into_result().err());
            let msg = s.find("ExecutionError(").map(|i| s[i..].chars().take(400).collect()).unwrap_or_else(|| s.chars().take(600).collect::<String>());
            bail!("{method} on {c} failed (tx {tx_hash}): {msg}");
        }
        let logs = full.logs().into_iter().map(String::from).collect();
        let value = full.into_result().map_err(|e| anyhow!("{method}: {e:?}"))?.json::<Value>().unwrap_or(Value::Null);
        Ok(Outcome { gas_burnt, tx_hash, value, logs })
    }

    pub async fn view(&self, contract: &str, method: &str, args: Value) -> Result<Value> {
        let c: AccountId = contract.parse()?;
        let d = Contract(c).call_function(method, args).read_only::<Value>().fetch_from(&self.network).await.with_context(|| format!("view {method}"))?;
        Ok(d.data)
    }

    pub async fn balance(&self, account: &str) -> Result<u128> {
        let a: AccountId = account.parse()?;
        let v = near_api::Tokens::account(a).near_balance().fetch_from(&self.network).await?;
        Ok(v.total.as_yoctonear())
    }

    /// Creates `<name>.<parent>` funded with `near`, with a fresh full-access key. Returns the
    /// actor and the secret key (for the caller to store; never logged here).
    pub async fn create_subaccount(&self, parent: &Actor, name: &str, near: NearToken) -> Result<(Actor, String, String)> {
        let id: AccountId = format!("{name}.{}", parent.id).parse()?;
        let sk = near_api::signer::generate_secret_key()?;
        let r = near_api::Account::create_account(id.clone())
            .fund_myself(parent.id.clone(), near)
            .with_public_key(sk.public_key())
            .with_signer(parent.signer.clone())
            .send_to(&self.network)
            .await
            .with_context(|| format!("create {id}"))?
            .into_full()
            .ok_or_else(|| anyhow!("create {id}: pending"))?;
        let h = r.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
        r.into_result().map_err(|e| anyhow!("create {id}: {e:?}"))?;
        self.record(format!("create account {id} funded {near} by {}", parent.id), &h);
        let secret = sk.to_string();
        let public = sk.public_key().to_string();
        Ok((Actor::new(id.as_str(), &secret)?, secret, public))
    }

    /// Sends NEAR from `from` to `to` (gas top-ups for the relayer). Returns the tx hash.
    pub async fn transfer(&self, from: &Actor, to: &str, amount: NearToken) -> Result<String> {
        let to_id: AccountId = to.parse()?;
        let r = near_api::Tokens::account(from.id.clone())
            .send_to(to_id.clone())
            .near(amount)
            .with_signer(from.signer.clone())
            .send_to(&self.network)
            .await
            .with_context(|| format!("transfer to {to_id}"))?
            .into_full()
            .ok_or_else(|| anyhow!("transfer to {to_id}: pending"))?;
        let h = r.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
        r.into_result().map_err(|e| anyhow!("transfer to {to_id}: {e:?}"))?;
        self.record(format!("transfer {amount} {} -> {to_id}", from.id), &h);
        Ok(h)
    }

    pub async fn deploy(&self, who: &Actor, wasm: Vec<u8>) -> Result<String> {
        let n = wasm.len();
        let r = Contract::deploy(who.id.clone())
            .use_code(wasm)
            .without_init_call()
            .with_signer(who.signer.clone())
            .send_to(&self.network)
            .await?
            .into_full()
            .ok_or_else(|| anyhow!("deploy {}: pending", who.id))?;
        let h = r.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
        r.into_result().map_err(|e| anyhow!("deploy {}: {e:?}", who.id))?;
        self.record(format!("deploy {n}-byte wasm to {}", who.id), &h);
        Ok(h)
    }
}

// ---------------------------------------------------------------------------------------------
// Local prints

#[derive(Clone, Debug)]
pub struct LocalPrint {
    pub index_id: String,
    pub utc_date: String,
    pub source_end_ms: i64,
    pub revision: u32,
    pub hash: [u8; 32],
    pub prev_hash: [u8; 32],
    /// `{canonicalHex, reporterSignatures}` as the contract's `push` expects.
    pub push: Value,
}

fn utc_date(source_end_ms: i64) -> String {
    let day = (source_end_ms - 1).div_euclid(86_400_000) + 719_468;
    let era = if day >= 0 { day } else { day - 146_096 } / 146_097;
    let doe = day - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + if m <= 2 { 1 } else { 0 })
}

/// All signed prints in the given revision files, grouped by index_id and sorted by
/// (source_end, revision). Unsigned (blocked) revisions are skipped.
pub fn local_prints(ledger: &Path, files: &[&str]) -> Result<BTreeMap<String, Vec<LocalPrint>>> {
    let mut out: BTreeMap<String, Vec<LocalPrint>> = BTreeMap::new();
    for f in files {
        let p = ledger.join(f);
        if !p.exists() {
            continue;
        }
        for line in std::fs::read_to_string(&p)?.lines().filter(|l| !l.trim().is_empty()) {
            let r: Value = serde_json::from_str(line)?;
            let pr = &r["print"];
            let Some(hexs) = pr["canonicalHex"].as_str() else { continue };
            let bytes = hex::decode(hexs)?;
            let d = zfx_oracle_print::OraclePrint::decode(&bytes).map_err(|e| anyhow!("{}: {e:?}", p.display()))?;
            out.entry(d.index_id.clone()).or_default().push(LocalPrint {
                utc_date: utc_date(d.source_end_ms),
                source_end_ms: d.source_end_ms,
                revision: d.revision,
                hash: zfx_oracle_print::sha256(&bytes),
                prev_hash: d.prev_print_hash,
                push: json!({"canonicalHex": hexs, "reporterSignatures": pr["reporterSignatures"]}),
                index_id: d.index_id,
            });
        }
    }
    for v in out.values_mut() {
        v.sort_by_key(|p| (p.source_end_ms, p.revision));
    }
    Ok(out)
}

/// Prints that the contract would accept after `onchain` (latest end ms, revision, hash),
/// following the forward rule: a later window, or a chained correction of the same window.
/// Unchainable corrections of the current window are dropped.
pub fn pending_after(local: &[LocalPrint], onchain: Option<(i64, u32, [u8; 32])>, from_date: Option<&str>) -> Vec<LocalPrint> {
    let mut cur = onchain;
    let mut out = Vec::new();
    for p in local {
        if let Some(f) = from_date {
            if p.utc_date.as_str() < f {
                continue;
            }
        }
        let ok = match cur {
            None => true,
            Some((end, rev, h)) => p.source_end_ms > end || (p.source_end_ms == end && p.revision > rev && p.prev_hash == h),
        };
        if ok {
            cur = Some((p.source_end_ms, p.revision, p.hash));
            out.push(p.clone());
        }
    }
    out
}

fn parse_ms(s: &str) -> Option<i64> {
    // YYYY-MM-DDTHH:MM:SS.mmmZ
    let (y, m, d) = (s.get(0..4)?.parse::<i64>().ok()?, s.get(5..7)?.parse::<i64>().ok()?, s.get(8..10)?.parse::<i64>().ok()?);
    let (hh, mi, ss, ms) = (s.get(11..13)?.parse::<i64>().ok()?, s.get(14..16)?.parse::<i64>().ok()?, s.get(17..19)?.parse::<i64>().ok()?, s.get(20..23)?.parse::<i64>().ok()?);
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400_000 + hh * 3_600_000 + mi * 60_000 + ss * 1000 + ms)
}

pub struct PushOpts<'a> {
    pub from_date: Option<&'a str>,
    /// Only these index ids (default: every index enabled on chain).
    pub only: Option<BTreeSet<String>>,
    pub est_tgas_per_print: u64,
    pub dry_run: bool,
    /// Send at most this many prints (oldest first within each index).
    pub max_prints: Option<usize>,
}


/// Index specs to register so that the latest prints in `files` for `markets` can be pushed:
/// one per index id, with the formula version and hash of its newest revision. Pure.
pub fn wanted_specs(ledger: &Path, files: &[&str], markets: &BTreeSet<String>) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for f in files {
        let p = ledger.join(f);
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let r: Value = serde_json::from_str(line).with_context(|| format!("{}", p.display()))?;
            let (Some(id), Some(m), Some(v), Some(h)) = (r["index_id"].as_str(), r["market"].as_str(), r["formula_version"].as_str(), r["formula_hash"].as_str()) else { continue };
            if markets.contains(m) {
                out.insert(id.to_string(), json!({"index_id": id, "version": v, "formula_hash": h, "value_scale": 6}));
            }
        }
    }
    Ok(out)
}

/// Registers (as the contract owner) every index in `wanted` that is not on chain yet, 20 per
/// transaction. Returns how many were registered. Indices already on chain are left alone.
pub async fn ensure_indices(near: &Near, owner: &Actor, contract: &str, wanted: &BTreeMap<String, Value>) -> Result<usize> {
    let mut have = BTreeSet::new();
    let mut from = 0u32;
    loop {
        let page = near.view(contract, "indices", json!({"from": from, "limit": 100})).await?;
        let a = page.as_array().cloned().unwrap_or_default();
        have.extend(a.iter().filter_map(|x| x["index_id"].as_str().map(String::from)));
        if a.len() < 100 {
            break;
        }
        from += 100;
    }
    let missing: Vec<Value> = wanted.iter().filter(|(k, _)| !have.contains(*k)).map(|(_, v)| v.clone()).collect();
    for ch in missing.chunks(20) {
        near.call(owner, contract, "set_indices", json!({"specs": ch}), NearToken::from_yoctonear(0), 100).await?;
    }
    Ok(missing.len())
}

/// Sends every local print newer than the on-chain latest, per index, in batches.
pub async fn push_new(near: &Near, who: &Actor, contract: &str, ledger: &Path, files: &[&str], o: &PushOpts<'_>) -> Result<Value> {
    let local = local_prints(ledger, files)?;
    // on-chain indices
    let mut onchain: Vec<Value> = Vec::new();
    let mut from = 0u32;
    loop {
        let page = near.view(contract, "indices", json!({"from": from, "limit": 100})).await?;
        let n = page.as_array().map(|a| a.len()).unwrap_or(0);
        onchain.extend(page.as_array().cloned().unwrap_or_default());
        if n < 100 {
            break;
        }
        from += 100;
    }
    let mut queue: Vec<LocalPrint> = Vec::new();
    let mut per_index = serde_json::Map::new();
    for ix in &onchain {
        let id = ix["index_id"].as_str().unwrap_or("").to_string();
        if ix["enabled"] != json!(true) || o.only.as_ref().map(|s| !s.contains(&id)).unwrap_or(false) {
            continue;
        }
        // index_info carries the latest utc_date, revision and print hash: no per-index view needed
        let cur = if ix["prints"].as_u64().unwrap_or(0) > 0 {
            let d = ix["latest_utc_date"].as_str().context("latest_utc_date")?;
            let end = parse_ms(&format!("{d}T00:00:00.000Z")).context("latest_utc_date")? + 86_400_000;
            let h: [u8; 32] = hex::decode(ix["latest_print_hash"].as_str().unwrap_or(""))?.try_into().map_err(|_| anyhow!("hash"))?;
            Some((end, ix["latest_revision"].as_u64().unwrap_or(0) as u32, h))
        } else {
            None
        };
        let pend = pending_after(local.get(&id).map(|v| v.as_slice()).unwrap_or(&[]), cur, o.from_date);
        per_index.insert(id.clone(), json!({"onchain_latest": ix["latest_utc_date"], "pending": pend.len()}));
        queue.extend(pend);
    }
    if let Some(m) = o.max_prints {
        queue.truncate(m);
    }
    let per_batch = ((BATCH_BUDGET_TGAS / o.est_tgas_per_print.max(1)) as usize).clamp(1, MAX_BATCH);
    let mut txs = Vec::new();
    let (mut accepted, mut rejected) = (0u64, 0u64);
    if !o.dry_run {
        for chunk in queue.chunks(per_batch) {
            let args = json!({"prints": chunk.iter().map(|p| p.push.clone()).collect::<Vec<_>>()});
            // prepay only what the batch needs: NEAR reserves prepaid gas at a pessimistic price,
            // so a flat 300 Tgas would need far more balance than the push costs
            let gas = (20 + o.est_tgas_per_print * chunk.len() as u64).min(PUSH_GAS_TGAS);
            let r = near.call(who, contract, "push", args, NearToken::from_yoctonear(0), gas).await?;
            let a = r.value["accepted"].as_u64().unwrap_or(0);
            let rj = r.value["rejected"].as_array().map(|x| x.len()).unwrap_or(0) as u64;
            accepted += a;
            rejected += rj;
            txs.push(json!({"tx": r.tx_hash, "prints": chunk.len(), "accepted": a, "rejected": r.value["rejected"], "gas_burnt_tgas": format!("{:.2}", r.gas_burnt as f64 / 1e12)}));
            eprintln!("[push-near] tx {} prints {} accepted {} gas {:.1} Tgas", r.tx_hash, chunk.len(), a, r.gas_burnt as f64 / 1e12);
        }
    }
    Ok(json!({"contract": contract, "queued": queue.len(), "batch_size": per_batch, "accepted": accepted, "rejected": rejected, "dry_run": o.dry_run, "txs": txs, "indices": per_index}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(end_day: i64, rev: u32, h: u8, prev: u8) -> LocalPrint {
        LocalPrint { index_id: "x".into(), utc_date: utc_date(end_day * 86_400_000), source_end_ms: end_day * 86_400_000, revision: rev, hash: [h; 32], prev_hash: [prev; 32], push: Value::Null }
    }

    #[test]
    fn pending_follows_forward_rule() {
        let v = vec![lp(10, 1, 1, 0), lp(11, 1, 2, 0), lp(11, 2, 3, 2), lp(11, 3, 4, 9), lp(12, 1, 5, 0)];
        assert_eq!(pending_after(&v, None, None).len(), 4, "r3 of day 11 is not chained to r2");
        let p = pending_after(&v, Some((11 * 86_400_000, 1, [2; 32])), None);
        assert_eq!(p.iter().map(|x| (x.source_end_ms / 86_400_000, x.revision)).collect::<Vec<_>>(), vec![(11, 2), (12, 1)]);
        assert!(pending_after(&v, Some((12 * 86_400_000, 1, [5; 32])), None).is_empty(), "idempotent once on chain");
        assert_eq!(pending_after(&v, None, Some(&utc_date(12 * 86_400_000))).len(), 1);
    }

    #[test]
    fn dates() {
        assert_eq!(utc_date(20_767 * 86_400_000), "2026-11-09");
        assert_eq!(parse_ms("2026-11-10T00:00:00.000Z"), Some(20_767 * 86_400_000));
    }
}
