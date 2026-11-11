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
//! - documented `/api/v1/models/{id}/endpoints`: once per market per 7 days
//! - documented `/api/v1/datasets/rankings-daily`: daily, only if OPENROUTER_API_KEY is set
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

pub fn list_markets(l: &Ledger, top_n: usize, now: i64) -> Result<Vec<Market>, PollError> {
    let s = engine::load(l)?;
    let (Some(r), Some(c)) = (s.rankings.last(), s.catalog.as_ref()) else {
        return Err(PollError::Other("need a rankings day response and a catalog response first".into()));
    };
    let existing = engine::load_markets(l)?;
    let mut added = Vec::new();
    for m in engine::select_universe(&r.doc, &c.doc, top_n, now) {
        if existing.iter().any(|e| e.permaslug == m.permaslug) {
            continue;
        }
        let mut ev = json!({"event": "LIST", "at": fmt_ms(now), "market": m, "basis": {"rankings_fetch_seq": r.rec.seq, "catalog_fetch_seq": c.rec.seq}});
        l.append_json("markets.jsonl", &mut ev)?;
        added.push(m);
    }
    Ok(added)
}

pub fn backfill(l: &Ledger, budget: RateBudget, top_n: usize) -> Result<(), PollError> {
    let f = Fetcher::new(l, budget);
    soft(f.get(Kind::CatalogModels, "", &path_for(Kind::CatalogModels, "")))?;
    soft(f.get(Kind::RankingsDay, "", &path_for(Kind::RankingsDay, "")))?;
    soft(f.get(Kind::RankingsDaySpend, "", &path_for(Kind::RankingsDaySpend, "")))?;
    let added = list_markets(l, top_n, now_ms())?;
    eprintln!("[backfill] listed {} new markets", added.len());
    for m in engine::load_markets(l)? {
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
                    if f.api_key.is_some() {
                        let q = format!("start_date={}&end_date={}", doc.date, doc.date);
                        soft(f.get(Kind::RankingsDailyApi, &q, &path_for(Kind::RankingsDailyApi, &q)))?;
                        sent += 1;
                    }
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

    let markets = engine::load_markets(l)?;
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
            if last_fetch_ms(&s, Kind::ModelEndpoints, &m.model_id).map(|t| now - t >= 7 * 86_400_000).unwrap_or(true) {
                soft(f.get(Kind::ModelEndpoints, &m.model_id, &path_for(Kind::ModelEndpoints, &m.model_id)))?;
                sent += 1;
            }
        }
    }
    Ok(sent)
}

pub fn run_loop(ledger_dir: &Path, out: &Path, signer: &Signer, params: &Params, budget: RateBudget, once: bool) -> Result<(), PollError> {
    let mut last_publish = 0i64;
    loop {
        let res = (|| -> Result<(), PollError> {
            let l = Ledger::open_write(ledger_dir)?;
            let f = Fetcher::new(&l, budget.clone());
            let n = tick(&l, &f, params, now_ms())?;
            if n > 0 || now_ms() - last_publish > 3_600_000 {
                let rep = engine::run(&l, signer, params, now_ms())?;
                let rep7 = crate::weekly::run(&l, signer, &Default::default(), now_ms())?;
                eprintln!("[poll] 7d {:?}", rep7);
                let files = publish::publish(&l, out, &signer.pubkey(), params, &budget, now_ms())?;
                eprintln!("[poll] {} requests; engine {:?}; published {} files", n, rep, files);
                last_publish = now_ms();
            }
            Ok(())
        })();
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
