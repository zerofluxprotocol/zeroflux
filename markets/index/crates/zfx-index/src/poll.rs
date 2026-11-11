//! Backfill and the bounded poller.
//!
//! Schedule (all UTC):
//! - documented catalog `/api/v1/models`: once per 24 h
//! - rankings day view: every 10 min while yesterday is not yet visible, else hourly;
//!   spend view once per newly seen day. First-seen times give publication latency.
//! - close window [d+1 00:00Z - 50 min, d+1 00:00Z - 3 min): one effective-pricing
//!   fetch per market with range=3d (the CLOSE_TRAILING_24H observation for day d)
//! - from d+1 00:00Z + grace: one model-activity fetch per market; one effective-pricing
//!   fetch per market only if no close observation exists
//! - documented `/api/v1/models/{id}/endpoints`: once per market per day, after the grace window
//!   (price snapshot for the dataset index)
//! - documented `/api/v1/datasets/rankings-daily` (API key): one 30-day request hourly until
//!   yesterday appears, then once after the grace window. Separate cap of 450/day.
//!
//! Universe: new models enter from the dataset's newest day (its top 50); a tracked model keeps
//! being polled through the web endpoints after it leaves the dataset, and is retired when it
//! leaves the catalog or shows no tokens for `RETIRE_ZERO_DAYS` days (`retire_reason`).
//!
//! Every request goes through `RateBudget`. Rough volume with 50 markets: ~180/day.

use crate::engine::{self, Loaded, Market, Params, Signer};
use crate::fetch::{path_for, pricing_path, FetchError, Fetcher, RateBudget};
use crate::ledger::{Ledger, LedgerError};
use crate::publish;
use crate::source::{self, Kind};
use crate::time::{fmt_ms, now_ms, UtcDate};
use serde_json::json;
use std::path::Path;

#[derive(Debug)]
pub enum PollError {
    Fetch(FetchError),
    Ledger(LedgerError),
    Other(String),
}
impl From<FetchError> for PollError {
    fn from(e: FetchError) -> Self {
        PollError::Fetch(e)
    }
}
impl From<LedgerError> for PollError {
    fn from(e: LedgerError) -> Self {
        PollError::Ledger(e)
    }
}
impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PollError::Fetch(e) => write!(f, "{e}"),
            PollError::Ledger(e) => write!(f, "{e}"),
            PollError::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Fetch, but treat HTTP errors as recorded-and-continue. Budget/ledger errors propagate.
fn soft(r: Result<(crate::ledger::FetchRecord, Vec<u8>), FetchError>) -> Result<Option<Vec<u8>>, PollError> {
    match r {
        Ok((_, b)) => Ok(Some(b)),
        Err(FetchError::Http(e, _)) => {
            eprintln!("[poll] request failed (recorded): {e}");
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// Adds models to the tracked universe. With the documented daily dataset available, new
/// models enter from its newest day (the top 50); without it, from the rankings day view.
/// Models already tracked are skipped; a retired model that reappears is listed again.
pub fn list_markets(l: &Ledger, top_n: usize, now: i64) -> Result<Vec<Market>, PollError> {
    let s = engine::load(l)?;
    let active = engine::load_active_markets(l)?;
    let (added, basis) = listing_candidates(&s, &active, top_n, now)?;
    for m in &added {
        let mut ev = json!({"event": "LIST", "at": fmt_ms(now), "market": m, "basis": basis});
        l.append_json("markets.jsonl", &mut ev)?;
    }
    Ok(added)
}

/// Models `list_markets` would add, and the fetches that justify them. Writes nothing.
pub fn listing_candidates(s: &Loaded, active: &[Market], top_n: usize, now: i64) -> Result<(Vec<Market>, serde_json::Value), PollError> {
    let Some(c) = s.catalog.as_ref() else {
        return Err(PollError::Other("need a catalog response first".into()));
    };
    let ds = s.dataset.iter().max_by_key(|p| (p.doc.end_date, p.rec.seq));
    let (cands, basis) = match (ds, s.rankings.last()) {
        (Some(d), _) => (engine::select_from_dataset(&d.doc, &c.doc, now), json!({"dataset_fetch_seq": d.rec.seq, "catalog_fetch_seq": c.rec.seq})),
        (None, Some(r)) => (engine::select_universe(&r.doc, &c.doc, top_n, now), json!({"rankings_fetch_seq": r.rec.seq, "catalog_fetch_seq": c.rec.seq})),
        (None, None) => return Err(PollError::Other("need a dataset or rankings day response first".into())),
    };
    Ok((cands.into_iter().filter(|m| !active.iter().any(|e| e.permaslug == m.permaslug)).collect(), basis))
}

/// Days of zero web-reported tokens after which a model stops being tracked.
pub const RETIRE_ZERO_DAYS: i64 = 7;

/// Why a tracked model should stop being polled, if it should.
/// - it is gone from the newest catalog, or
/// - a model-activity response fetched after `yesterday` closed shows no tokens on any of the
///   last RETIRE_ZERO_DAYS days, and the dataset does not list it on any of them.
///
/// Models listed in the last RETIRE_ZERO_DAYS days are never retired.
pub fn retire_reason(s: &Loaded, m: &Market, yesterday: UtcDate) -> Option<String> {
    let lo = UtcDate(yesterday.0 - (RETIRE_ZERO_DAYS - 1));
    if UtcDate::parse(&m.listing_day).map(|d| d >= lo).unwrap_or(true) {
        return None;
    }
    if let Some(c) = s.catalog.as_ref() {
        if !c.doc.is_empty() && !c.doc.iter().any(|x| x.canonical_slug == m.permaslug) {
            return Some(format!("not in the OpenRouter catalog (fetch #{})", c.rec.seq));
        }
    }
    let act = s.activity.get(&m.permaslug)?.iter().filter(|p| p.rec.fetched_at_ms >= yesterday.end_ms()).max_by_key(|p| p.rec.seq)?;
    let tokens: u128 = act.doc.rows.iter().filter(|r| r.date >= lo && r.date <= yesterday).map(|r| r.total_tokens()).sum();
    if tokens > 0 {
        return None;
    }
    if s.dataset.iter().flat_map(|p| p.doc.rows.iter()).any(|r| r.permaslug == m.permaslug && r.date >= lo && r.date <= yesterday && r.total_tokens > 0) {
        return None;
    }
    Some(format!("no tokens from {lo} to {yesterday} (model-activity fetch #{})", act.rec.seq))
}

/// Appends a RETIRE event for every tracked model that meets `retire_reason`.
pub fn retire_markets(l: &Ledger, now: i64) -> Result<Vec<(String, String)>, PollError> {
    let s = engine::load(l)?;
    let yesterday = UtcDate::of_ms(now).pred();
    let mut out = Vec::new();
    for m in engine::load_active_markets(l)? {
        if let Some(reason) = retire_reason(&s, &m, yesterday) {
            let mut ev = json!({"event": "RETIRE", "at": fmt_ms(now), "permaslug": m.permaslug, "reason": reason});
            l.append_json("markets.jsonl", &mut ev)?;
            out.push((m.permaslug.clone(), reason));
        }
    }
    Ok(out)
}

pub fn backfill(l: &Ledger, budget: RateBudget, top_n: usize) -> Result<(), PollError> {
    let f = Fetcher::new(l, budget);
    soft(f.get(Kind::CatalogModels, "", &path_for(Kind::CatalogModels, "")))?;
    soft(f.get(Kind::RankingsDay, "", &path_for(Kind::RankingsDay, "")))?;
    soft(f.get(Kind::RankingsDaySpend, "", &path_for(Kind::RankingsDaySpend, "")))?;
    let added = list_markets(l, top_n, now_ms())?;
    eprintln!("[backfill] listed {} new markets", added.len());
    for m in engine::load_active_markets(l)? {
        soft(f.get(Kind::ModelActivity, &m.permaslug, &path_for(Kind::ModelActivity, &m.permaslug)))?;
        soft(f.get(Kind::EffectivePricing, &m.permaslug, &path_for(Kind::EffectivePricing, &m.permaslug)))?;
        soft(f.get(Kind::ModelEndpoints, &m.model_id, &path_for(Kind::ModelEndpoints, &m.model_id)))?;
    }
    Ok(())
}

fn last_fetch_ms(s: &Loaded, kind: Kind, subject: &str) -> Option<i64> {
    s.fetches.iter().rev().find(|f| f.kind == kind.name() && f.subject == subject).map(|f| f.fetched_at_ms)
}

/// One scheduler pass. Returns the number of requests sent.
pub fn tick(l: &Ledger, f: &Fetcher, params: &Params, now: i64) -> Result<usize, PollError> {
    let mut sent = 0usize;
    let s = engine::load(l)?;
    let today = UtcDate::of_ms(now);
    let yesterday = today.pred();

    if last_fetch_ms(&s, Kind::CatalogModels, "").map(|t| now - t >= 86_400_000).unwrap_or(true) {
        soft(f.get(Kind::CatalogModels, "", &path_for(Kind::CatalogModels, "")))?;
        sent += 1;
    }

    let newest = s.rankings.last().map(|r| r.doc.date);
    let interval = if newest.map(|d| d < yesterday).unwrap_or(true) { 10 * 60_000 } else { 60 * 60_000 };
    if last_fetch_ms(&s, Kind::RankingsDay, "").map(|t| now - t >= interval).unwrap_or(true) {
        if let Some(b) = soft(f.get(Kind::RankingsDay, "", &path_for(Kind::RankingsDay, "")))? {
            sent += 1;
            if let Ok(doc) = source::parse_rankings_day(&b) {
                if newest.map(|d| doc.date > d).unwrap_or(true) {
                    eprintln!("[poll] rankings day {} first seen at {}", doc.date, fmt_ms(now_ms()));
                    soft(f.get(Kind::RankingsDaySpend, "", &path_for(Kind::RankingsDaySpend, "")))?;
                    sent += 1;
                    let l2 = list_markets(l, params.top_n, now_ms())?;
                    if !l2.is_empty() {
                        eprintln!("[poll] listed {} new markets", l2.len());
                    }
                }
            }
        } else {
            sent += 1;
        }
    }

    // documented dataset: one request covers the last 30 days (corrections included).
    // Hourly until yesterday appears (latency), then once more after the grace window (finality).
    if f.api_key.is_some() {
        let s = engine::load(l)?;
        let newest_end = s.dataset.iter().map(|p| p.doc.end_date).max();
        let final_ok = s.dataset.iter().any(|p| p.doc.end_date >= yesterday && p.as_of_ms >= yesterday.end_ms() + params.grace_ms);
        let last = last_fetch_ms(&s, Kind::RankingsDailyApi, &format!("start_date={}&end_date={}", UtcDate(yesterday.0 - 29), yesterday))
            .or_else(|| s.fetches.iter().rev().find(|x| x.kind == Kind::RankingsDailyApi.name()).map(|x| x.fetched_at_ms));
        let since = last.map(|t| now - t).unwrap_or(i64::MAX);
        let want_latency = newest_end.map(|e| e < yesterday).unwrap_or(true) && since >= 30 * 60_000;
        let want_final = !final_ok && now >= yesterday.end_ms() + params.grace_ms && since >= 60 * 60_000;
        if want_latency || want_final {
            let q = format!("start_date={}&end_date={}", UtcDate(yesterday.0 - 29), yesterday);
            soft(f.get(Kind::RankingsDailyApi, &q, &path_for(Kind::RankingsDailyApi, &q)))?;
            sent += 1;
            // the dataset's newest day introduces new models
            match list_markets(l, params.top_n, now_ms()) {
                Ok(a) if !a.is_empty() => eprintln!("[poll] listed {} new markets from the dataset", a.len()),
                Ok(_) => {}
                Err(e) => eprintln!("[poll] listing from the dataset failed: {e}"),
            }
        }
    }

    // models that left OpenRouter or have had no usage for RETIRE_ZERO_DAYS days stop being polled
    if now >= yesterday.end_ms() + params.grace_ms {
        for (m, why) in retire_markets(l, now)? {
            eprintln!("[poll] retired {m}: {why}");
        }
    }

    let markets = engine::load_active_markets(l)?;
    let s = engine::load(l)?;
    // close window for today
    let close_lo = today.end_ms() - 50 * 60_000;
    let close_hi = today.end_ms() - 3 * 60_000;
    if now >= close_lo && now < close_hi {
        for m in &markets {
            if now_ms() >= close_hi {
                break;
            }
            let have = s.pricing.get(&m.permaslug).map(|v| v.iter().any(|p| p.as_of_ms >= today.end_ms() - params.close_window_ms && p.as_of_ms < today.end_ms() && p.rec.fetched_at_ms >= close_lo)).unwrap_or(false);
            if !have {
                soft(f.get(Kind::EffectivePricing, &m.permaslug, &pricing_path(&m.permaslug, "3d")))?;
                sent += 1;
            }
        }
    }
    // post-grace inputs for yesterday
    if now >= yesterday.end_ms() + params.grace_ms {
        for m in &markets {
            let have_act = s.activity.get(&m.permaslug).map(|v| v.iter().any(|p| p.as_of_ms >= yesterday.end_ms() + params.grace_ms)).unwrap_or(false);
            let recent_try = last_fetch_ms(&s, Kind::ModelActivity, &m.permaslug).map(|t| now - t < 3 * 3_600_000).unwrap_or(false);
            if !have_act && !recent_try {
                soft(f.get(Kind::ModelActivity, &m.permaslug, &path_for(Kind::ModelActivity, &m.permaslug)))?;
                sent += 1;
            }
            let pricing = s.pricing.get(&m.permaslug);
            let have_close = pricing.map(|v| v.iter().any(|p| p.as_of_ms >= yesterday.end_ms() - params.close_window_ms && p.as_of_ms < yesterday.end_ms())).unwrap_or(false);
            let have_post = pricing.map(|v| v.iter().any(|p| p.as_of_ms >= yesterday.end_ms() + params.grace_ms)).unwrap_or(false);
            let recent_price_try = last_fetch_ms(&s, Kind::EffectivePricing, &m.permaslug).map(|t| now - t < 3 * 3_600_000 && t >= yesterday.end_ms() + params.grace_ms).unwrap_or(false);
            if !have_close && !have_post && !recent_price_try {
                // a newly listed market has no history yet: fetch the full chart once
                let range = if pricing.map(|v| v.is_empty()).unwrap_or(true) { "all" } else { "1w" };
                soft(f.get(Kind::EffectivePricing, &m.permaslug, &pricing_path(&m.permaslug, range)))?;
                sent += 1;
            }
            // daily documented price snapshot: the dataset index price input
            if last_fetch_ms(&s, Kind::ModelEndpoints, &m.model_id).map(|t| t < yesterday.end_ms() + params.grace_ms).unwrap_or(true) {
                soft(f.get(Kind::ModelEndpoints, &m.model_id, &path_for(Kind::ModelEndpoints, &m.model_id)))?;
                sent += 1;
            }
        }
    }
    Ok(sent)
}

pub fn run_loop(ledger_dir: &Path, out: &Path, signer: &Signer, params: &Params, budget: RateBudget, once: bool) -> Result<(), PollError> {
    let mut last_publish = 0i64;
    let mut last_push = 0i64;
    let mut pending_push = true;
    loop {
        let res = (|| -> Result<(), PollError> {
            let l = Ledger::open_write(ledger_dir)?;
            let f = Fetcher::new(&l, budget.clone());
            let n = tick(&l, &f, params, now_ms())?;
            if n > 0 || now_ms() - last_publish > 3_600_000 {
                let rep = engine::run(&l, signer, params, now_ms())?;
                let rep7 = crate::weekly::run(&l, signer, &Default::default(), now_ms())?;
                let lic = crate::licensed::run(&l, signer, now_ms())?;
                let lic7 = crate::weekly::run_spec(&l, signer, &crate::weekly::Spec7d::lic(), now_ms())?;
                eprintln!("[poll] 7d {:?}; lic {:?}; lic 7d {:?}", rep7, lic, lic7);
                if lic.new_final + lic.new_corrected + lic7.new_final + lic7.new_corrected > 0 {
                    pending_push = true;
                }
                let files = publish::publish(&l, out, &signer.pubkey(), params, &budget, now_ms())?;
                eprintln!("[poll] {} requests; engine {:?}; published {} files", n, rep, files);
                last_publish = now_ms();
            }
            Ok(())
        })();
        // on-chain push after publishing new revisions (or every 6 h), when configured
        if let Some(cfg) = load_push_config() {
            if pending_push || now_ms() - last_push > 6 * 3_600_000 {
                match push_from_config(ledger_dir, &cfg) {
                    Ok(r) => {
                        eprintln!("[poll] push-near: queued {} accepted {} rejected {}", r["queued"], r["accepted"], r["rejected"]);
                        let log = ledger_dir.join("..").join("near-push.jsonl");
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                            use std::io::Write;
                            let _ = writeln!(f, "{}", json!({"at": fmt_ms(now_ms()), "result": r}));
                        }
                        pending_push = false;
                    }
                    Err(e) => eprintln!("[poll] push-near failed (will retry): {e}"),
                }
                last_push = now_ms();
            }
        }
        match res {
            Ok(()) => {}
            Err(PollError::Ledger(LedgerError::Locked(p))) => eprintln!("[poll] ledger busy ({}), retrying next tick", p.display()),
            Err(PollError::Fetch(FetchError::Budget(t))) => {
                eprintln!("[poll] rate budget exhausted until {}", fmt_ms(t));
            }
            Err(e) => eprintln!("[poll] error: {e}"),
        }
        if once {
            return Ok(());
        }
        let sleep = 60_000 + crate::fetch::random_u32_below(30_000) as u64;
        std::thread::sleep(std::time::Duration::from_millis(sleep));
    }
}

/// Backfills the documented `datasets/rankings-daily` from `from` to `to` with as few
/// requests as the API allows. It starts with the whole range; on HTTP 400 it halves the
/// span; if the server resolves a shorter window than requested (meta.end_date), it learns
/// that span. Every response, including errors, is recorded in the ledger.
pub fn dataset_backfill(l: &Ledger, budget: RateBudget, from: UtcDate, to: UtcDate) -> Result<serde_json::Value, PollError> {
    let f = Fetcher::new(l, budget);
    if f.api_key.is_none() {
        return Err(PollError::Other("no OpenRouter API key (env OPENROUTER_API_KEY or ~/.config/zeroflux/keys.env)".into()));
    }
    let mut start = from;
    let mut span = to.0 - from.0 + 1;
    let mut log = Vec::new();
    let mut errors = 0;
    while start <= to {
        let end = UtcDate((start.0 + span - 1).min(to.0));
        let q = format!("start_date={start}&end_date={end}");
        match f.get(Kind::RankingsDailyApi, &q, &path_for(Kind::RankingsDailyApi, &q)) {
            Ok((rec, body)) => {
                let doc = source::parse_rankings_daily_api(&body).map_err(|e| PollError::Other(format!("fetch #{}: {e}", rec.seq)))?;
                let days = doc.rows.iter().map(|r| r.date).collect::<std::collections::BTreeSet<_>>().len();
                eprintln!("[dataset] #{} requested {start}..{end} resolved {:?}..{} rows {} days {}", rec.seq, doc.start_date.map(|d| d.to_string()), doc.end_date, doc.rows.len(), days);
                log.push(json!({"seq": rec.seq, "requested": [start.to_string(), end.to_string()], "resolved": [doc.start_date.map(|d| d.to_string()), doc.end_date.to_string()], "rows": doc.rows.len(), "days": days, "bytes": rec.len}));
                if doc.end_date < start {
                    break; // nothing newer is available
                }
                if doc.end_date < end {
                    span = (doc.end_date.0 - start.0 + 1).max(1);
                }
                start = doc.end_date.succ();
            }
            Err(FetchError::Http(e, rec)) => {
                errors += 1;
                let status = rec.as_ref().map(|r| r.status).unwrap_or(0);
                eprintln!("[dataset] {start}..{end} failed: {e} (status {status})");
                log.push(json!({"seq": rec.as_ref().map(|r| r.seq), "requested": [start.to_string(), end.to_string()], "status": status, "error": e}));
                if status == 400 && span > 1 && errors < 12 {
                    span = (span + 1) / 2;
                    continue;
                }
                return Err(PollError::Other(format!("dataset backfill stopped at {start}: {e}")));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(json!({"requests": log.len(), "log": log}))
}


/// On-chain push configuration, read from a file outside the repository
/// (default `~/.config/zeroflux/oracle-push.json`):
/// `{"enabled": true, "contract": "oracle.zeroflux.testnet", "signer": "...", "key_file": "...", "rpc": ["..."], "files": ["revisions_lic_7d.jsonl", "revisions_7d.jsonl"], "owner": "...", "owner_key_file": "..."}`.
/// `owner`/`owner_key_file` are optional: with them, new tracked models' indices are registered before each push.
pub fn push_config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".config/zeroflux/oracle-push.json")
}

pub fn load_push_config() -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(push_config_path()).ok()?).ok()?;
    if v["enabled"] == json!(true) { Some(v) } else { None }
}

/// Pushes new prints to the NEAR oracle. Blocking; builds its own runtime.
pub fn push_near(ledger: &Path, contract: &str, signer: &str, key_file: &Path, rpcs: &[String], files: &[String], from_date: Option<&str>, dry_run: bool) -> Result<serde_json::Value, PollError> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| PollError::Other(e.to_string()))?;
    let near = zfx_oracle_near::Near::rpcs(rpcs).map_err(|e| PollError::Other(e.to_string()))?;
    let who = zfx_oracle_near::Actor::from_key_file(Some(signer), key_file).map_err(|e| PollError::Other(e.to_string()))?;
    let fl: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
    let opts = zfx_oracle_near::PushOpts { from_date, only: None, est_tgas_per_print: 1, dry_run, max_prints: None };
    rt.block_on(zfx_oracle_near::push_new(&near, &who, contract, ledger, &fl, &opts)).map_err(|e| PollError::Other(format!("{e:#}")))
}

fn push_from_config(ledger: &Path, cfg: &serde_json::Value) -> Result<serde_json::Value, PollError> {
    let s = |k: &str| cfg[k].as_str().map(String::from).ok_or_else(|| PollError::Other(format!("oracle-push.json: missing {k}")));
    let rpcs: Vec<String> = cfg["rpc"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_else(|| vec!["https://rpc.testnet.near.org".into(), "https://test.rpc.fastnear.com".into(), "https://rpc.testnet.fastnear.com".into()]);
    let files: Vec<String> = cfg["files"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_else(|| zfx_oracle_near::DEFAULT_FILES.iter().map(|x| x.to_string()).collect());
    // With an owner key configured, register the indices of every tracked model first, so a
    // newly listed model's prints are accepted on its first push.
    let mut registered = 0usize;
    if let (Some(owner), Some(okf)) = (cfg["owner"].as_str(), cfg["owner_key_file"].as_str()) {
        let l = Ledger::open_read(ledger);
        let markets: std::collections::BTreeSet<String> = engine::load_active_markets(&l)?.into_iter().map(|m| m.permaslug).collect();
        let fl: Vec<&str> = files.iter().map(|x| x.as_str()).collect();
        let wanted = zfx_oracle_near::wanted_specs(ledger, &fl, &markets).map_err(|e| PollError::Other(format!("{e:#}")))?;
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| PollError::Other(e.to_string()))?;
        let near = zfx_oracle_near::Near::rpcs(&rpcs).map_err(|e| PollError::Other(e.to_string()))?;
        let who = zfx_oracle_near::Actor::from_key_file(Some(owner), Path::new(okf)).map_err(|e| PollError::Other(e.to_string()))?;
        registered = rt.block_on(zfx_oracle_near::ensure_indices(&near, &who, &s("contract")?, &wanted)).map_err(|e| PollError::Other(format!("{e:#}")))?;
        if registered > 0 {
            eprintln!("[poll] push-near: registered {registered} new indices");
        }
    }
    let mut r = push_near(ledger, &s("contract")?, &s("signer")?, Path::new(&s("key_file")?), &rpcs, &files, cfg["from_date"].as_str(), false)?;
    r["registered"] = json!(registered);
    Ok(r)
}
