//! Static JSON publisher and source-health report. The output tree mirrors the
//! read-only API paths so a CDN or object store can serve it unchanged:
//!
//! ```text
//! <out>/v1/markets/models.json
//! <out>/v1/indices/source-health.json
//! <out>/v1/indices/prints/latest.json
//! <out>/v1/oracle/reporter.json
//! <out>/v1/indices/or-model-spend/<permaslug>/series.json
//! <out>/v1/indices/or-model-spend/<permaslug>/latest.json
//! <out>/v1/indices/or-model-spend/<permaslug>/provenance/<revision_id>.json
//! ```

use crate::engine::{self, Loaded, Market, Params, FORMULA_VERSION, MODE, NOT_SETTLEMENT_GRADE};
use crate::fetch::RateBudget;
use crate::fixed::{self, fmt_scaled};
use crate::ledger::{Ledger, LedgerError};
use crate::source::Kind;
use crate::time::{fmt_ms, UtcDate};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn write_json(path: &Path, v: &Value) -> std::io::Result<()> {
    write_bytes(path, &serde_json::to_vec_pretty(v).unwrap())
}

/// Compact form for the many per-revision provenance files.
fn write_json_compact(path: &Path, v: &Value) -> std::io::Result<()> {
    write_bytes(path, &serde_json::to_vec(v).unwrap())
}

fn write_bytes(path: &Path, b: &[u8]) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    if fs::read(path).ok().as_deref() == Some(b) {
        return Ok(()); // unchanged: keep mtime so CDN sync only uploads changes
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, b)?;
    fs::rename(tmp, path)
}

/// Lifecycle of the traded market (the 7D index), per spec 5.4. Daily anomaly blocking
/// still halts; a clamped or not-reported 7D day is CLOSE_ONLY.
pub fn lifecycle_traded(daily: Option<&Value>, d7: Option<&Value>, now: i64) -> (&'static str, String) {
    if let Some(r) = daily {
        if r["blocked"].as_bool() == Some(true) {
            return ("HALTED", "latest daily value failed a blocking integrity check".into());
        }
    }
    let Some(w) = d7 else { return ("PROPOSED", "no 7D value yet".into()) };
    if w["data_status"] != "OK" {
        return ("CLOSE_ONLY", format!("latest 7D day {} is {}: {}", w["utc_date"].as_str().unwrap_or(""), w["data_status"].as_str().unwrap_or(""), w["reason"].as_str().unwrap_or("")));
    }
    if w["clamped"] == json!(true) {
        return ("CLOSE_ONLY", format!("7D moved {} vs the prior day, beyond the {} clamp bound", w["change_vs_prev"].as_str().unwrap_or("?"), w["clamp_bound"].as_str().unwrap_or("?")));
    }
    if let Some(r) = daily {
        if r["data_status"] != "OK" {
            return ("CLOSE_ONLY", format!("latest daily value is {}", r["data_status"].as_str().unwrap_or("")));
        }
    }
    match w["print"]["validUntil"].as_str().and_then(crate::time::parse_rfc3339_ms) {
        Some(v) if now > v => ("HALTED", format!("latest 7D print expired at {}", fmt_ms(v))),
        _ => ("ACTIVE", "paper trading only".into()),
    }
}

/// Lifecycle a perps engine should apply for one 7D day.
pub fn day_lifecycle_7d(w: &Value) -> &'static str {
    if w["data_status"] != "OK" || w["clamped"] == json!(true) { "CLOSE_ONLY" } else { "ACTIVE" }
}

pub fn lifecycle(latest: Option<&Value>, now: i64) -> (&'static str, String) {
    let Some(r) = latest else { return ("PROPOSED", "no revision yet".into()) };
    let ds = r["data_status"].as_str().unwrap_or("");
    let valid_until = r["print"]["validUntil"].as_str().and_then(crate::time::parse_rfc3339_ms);
    if r["blocked"].as_bool() == Some(true) {
        return ("HALTED", "latest day failed a blocking integrity check".into());
    }
    if ds != "OK" {
        return ("CLOSE_ONLY", format!("latest day is {ds}"));
    }
    match valid_until {
        Some(v) if now > v => ("HALTED", format!("latest print expired at {}", fmt_ms(v))),
        _ => ("ACTIVE", "paper trading only".into()),
    }
}

pub fn source_health(l: &Ledger, s: &Loaded, params: &Params, budget: &RateBudget, now: i64) -> Result<Value, LedgerError> {
    let revs = l.read_json("revisions.jsonl")?;
    let latest = engine::latest_revisions(l)?;
    // per-kind fetch stats
    let mut kinds: BTreeMap<String, Value> = BTreeMap::new();
    for f in &s.fetches {
        let e = kinds.entry(f.kind.clone()).or_insert_with(|| json!({"documented": f.documented, "fetches": 0, "fetches_24h": 0, "failures": 0}));
        e["fetches"] = json!(e["fetches"].as_u64().unwrap() + 1);
        if f.fetched_at_ms > now - 86_400_000 {
            e["fetches_24h"] = json!(e["fetches_24h"].as_u64().unwrap() + 1);
        }
        if f.status != 200 {
            e["failures"] = json!(e["failures"].as_u64().unwrap() + 1);
        }
        e["last_fetch"] = json!({"at": fmt_ms(f.fetched_at_ms), "status": f.status, "url": f.url, "seq": f.seq});
        if f.status == 200 {
            e["last_success"] = json!(fmt_ms(f.fetched_at_ms));
        }
    }
    let mut failing_kinds = Vec::new();
    for ((kind, subject), (rec, err)) in &s.newest_failed {
        failing_kinds.push(json!({"kind": kind, "subject": subject, "seq": rec.seq, "error": err}));
    }

    // publication latency: first fetch at which rankings day view showed day d
    let mut first_seen: BTreeMap<UtcDate, Value> = BTreeMap::new();
    let mut last_before: BTreeMap<UtcDate, i64> = BTreeMap::new();
    for r in &s.rankings {
        let d = r.doc.date;
        if !first_seen.contains_key(&d) {
            let prev_seen = last_before.get(&d.pred()).copied();
            let lat = r.rec.fetched_at_ms - d.end_ms();
            first_seen.insert(d, json!({
                "utc_date": d.to_string(),
                "first_seen_fetch_at": fmt_ms(r.rec.fetched_at_ms),
                "last_modified_at_first_seen": r.rec.headers.get("last-modified"),
                "latency_upper_bound_minutes": lat / 60_000,
                "previous_fetch_still_showing_prior_day": prev_seen.map(fmt_ms),
                "latency_lower_bound_minutes": prev_seen.filter(|t| *t >= d.end_ms()).map(|t| (t - d.end_ms()) / 60_000),
                "measured_live": prev_seen.map(|t| t >= d.end_ms()).unwrap_or(false),
            }));
        }
        last_before.insert(d, r.rec.fetched_at_ms);
    }
    let lat_rows: Vec<Value> = first_seen.into_values().collect();
    let live: Vec<i64> = lat_rows.iter().filter(|r| r["measured_live"] == json!(true)).filter_map(|r| r["latency_upper_bound_minutes"].as_i64()).collect();

    // top-N coverage and spend cross-check on the newest rankings day
    let mut coverage = Value::Null;
    if let Some(r) = s.rankings.last() {
        let mut tot: Vec<u128> = r.doc.rows.iter().map(|x| x.usage.total_tokens()).collect();
        tot.sort_unstable_by(|a, b| b.cmp(a));
        let all: u128 = tot.iter().sum();
        let top: u128 = tot.iter().take(params.top_n).sum();
        coverage = json!({
            "utc_date": r.doc.date.to_string(),
            "rows": r.doc.rows.len(),
            "total_tokens_all_rows": all.to_string(),
            "top_n": params.top_n,
            "top_n_tokens": top.to_string(),
            "top_n_share": if all > 0 { fixed::fmt_ppm(top * 1_000_000 / all) } else { "0".into() },
            "other_tokens": (all - top).to_string(),
            "note": "The web rankings day view lists every model, so `other` is reconstructed as all rows outside the top N.",
        });
    }
    let mut spend_check = Vec::new();
    for sp in &s.spend {
        let d = sp.doc.date;
        let mut ratios: Vec<(u128, String, u128, u128, u128, u128)> = Vec::new();
        for row in &sp.doc.rows {
            if row.usage.variant != "standard" {
                continue;
            }
            let Some(rev) = latest.get(&(row.usage.permaslug.clone(), d.to_string())) else { continue };
            let Some(v) = rev["value_micro_usd"].as_str().and_then(|x| x.parse::<u128>().ok()) else { continue };
            let Ok(share) = fixed::parse_decimal(&row.metric_text, 12) else { continue };
            if share == 0 {
                continue;
            }
            // diagnostic only (not signed, not the index): the same proxy including BYOK tokens
            let nb = rev["info_value_usd_incl_byok"].as_str().and_then(|x| fixed::parse_decimal(x, 6).ok()).unwrap_or(0);
            ratios.push((v * 1_000_000_000_000 / share, row.usage.permaslug.clone(), v, share, nb * 1_000_000_000_000 / share, nb));
        }
        if ratios.is_empty() {
            continue;
        }
        ratios.sort();
        let n = ratios.len();
        let q = |p: usize| fmt_scaled(ratios[(n - 1) * p / 100].0, 6);
        let mut nb: Vec<u128> = ratios.iter().map(|r| r.4).collect();
        nb.sort_unstable();
        let qn = |p: usize| fmt_scaled(nb[(n - 1) * p / 100], 6);
        spend_check.push(json!({
            "utc_date": d.to_string(),
            "markets_compared": n,
            "implied_market_total_usd": {"p10": q(10), "median": q(50), "p90": q(90)},
            "implied_market_total_usd_incl_byok_diagnostic": {"p10": qn(10), "median": qn(50), "p90": qn(90)},
            "note": "For each market: implied_usd / OpenRouter spend share. If the proxy tracked OpenRouter's own spend ranking, every ratio would equal the same market total. The spread measures proxy error (price weighting, cache accounting, tokenizer and promotional effects). The incl_byok diagnostic shows the much wider spread when BYOK tokens, which OpenRouter does not bill, are priced in.",
            "per_market": ratios.iter().map(|(t, slug, v, sh, tn, vn)| json!({"permaslug": slug, "implied_usd": fmt_scaled(*v, 6), "or_spend_share": fmt_scaled(*sh, 12), "implied_market_total_usd": fmt_scaled(*t, 6), "implied_usd_incl_byok_diagnostic": fmt_scaled(*vn, 6), "implied_market_total_usd_incl_byok_diagnostic": fmt_scaled(*tn, 6)})).collect::<Vec<_>>(),
        }));
    }

    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    let mut corrections = 0u64;
    let mut blocked = 0u64;
    for r in &revs {
        *by_status.entry(r["data_status"].as_str().unwrap_or("?").to_string()).or_default() += 1;
        if r["finality"] == "CORRECTED" {
            corrections += 1;
        }
    }
    // markets whose most recent day is blocked (historical blocked days do not degrade health)
    let mut newest: BTreeMap<&str, &Value> = BTreeMap::new();
    for ((m, _), r) in &latest {
        let e = newest.entry(m.as_str()).or_insert(r);
        if r["utc_date"].as_str() > e["utc_date"].as_str() {
            *e = r;
        }
    }
    for r in newest.values() {
        if r["blocked"] == json!(true) {
            blocked += 1;
        }
    }
    let blocked_days_total = latest.values().filter(|r| r["blocked"] == json!(true)).count();
    let req_24h = s.fetches.iter().filter(|f| f.fetched_at_ms > now - 86_400_000).count();
    let newest_complete = UtcDate::of_ms(now).pred();
    let newest_seen = s.rankings.last().map(|r| r.doc.date);
    let late = newest_seen.map(|d| d < newest_complete && now > newest_complete.end_ms() + 6 * 3_600_000).unwrap_or(true);
    let status = if !failing_kinds.is_empty() {
        "FAILING"
    } else if late || blocked > 0 {
        "DEGRADED"
    } else {
        "OK"
    };
    Ok(json!({
        "generated_at": fmt_ms(now),
        "status": status,
        "notice": NOT_SETTLEMENT_GRADE,
        "attribution": s.rankings.last().map(|r| engine::attribution(r.as_of_ms)),
        "license": "OpenRouter dataset content is published under CC BY 4.0 per OpenRouter's Datasets API documentation.",
        "formula": {"version": FORMULA_VERSION, "hash": hex::encode(engine::formula_hash()), "mode": MODE},
        "newest_complete_utc_day": newest_complete.to_string(),
        "newest_rankings_day_seen": newest_seen.map(|d| d.to_string()),
        "late": late,
        "failing_inputs": failing_kinds,
        "parse_errors_total": s.parse_errors.len(),
        "kinds": kinds,
        "rate_budget": {"requests_24h": req_24h, "daily_cap": budget.daily_cap, "min_gap_ms": budget.min_gap_ms, "jitter_ms": budget.jitter_ms},
        "publication_latency": {
            "method": "first fetch of /rankings/models?view=day that shows day d, minus d+1 00:00Z. Upper bound; the lower bound is the previous fetch that still showed d-1. Rows with measured_live=false were first seen during backfill and say nothing about latency.",
            "days_measured_live": live.len(),
            "target_days": 30,
            "rows": lat_rows,
        },
        "coverage": coverage,
        "spend_share_crosscheck": spend_check,
        "revisions_7d": revisions_7d_summary(l)?,
        "documented_dataset": crate::report::dataset_summary(s, now),
        "documented_vs_web_tokens": crate::report::crosscheck(l, s)?,
        "licensed_vs_web_tracking": crate::report::tracking(l)?,
        "clamp_calibration": crate::report::clamp_calibration(s),
        "revisions": {"total": revs.len(), "by_data_status": by_status, "corrections": corrections, "markets_blocked_latest": blocked, "blocked_days_total": blocked_days_total},
    }))
}

pub fn publish(l: &Ledger, out: &Path, pubkey: &[u8; 32], params: &Params, budget: &RateBudget, now: i64) -> Result<usize, LedgerError> {
    let s = engine::load(l)?;
    let markets = engine::load_markets(l)?;
    let revs = l.read_json("revisions.jsonl")?;
    let latest = engine::latest_revisions(l)?;
    let mut files = 0usize;
    let base = out.join("v1");
    let idx = base.join("indices").join("or-model-spend");
    let idx7 = base.join("indices").join("or-model-spend-7d");
    let revs7 = l.read_json(crate::weekly::REVISIONS_7D)?;
    let latest7 = crate::weekly::latest_7d(l)?;
    let mut per_market7: BTreeMap<String, usize> = BTreeMap::new();
    for r in &revs7 {
        let m = r["market"].as_str().unwrap_or("").to_string();
        let rid = r["revision_id"].as_str().unwrap_or("");
        write_json_compact(&idx7.join(&m).join("provenance").join(format!("{rid}.json")), r)?;
        files += 1;
        *per_market7.entry(m).or_default() += 1;
    }

    // provenance for every revision
    let mut per_market: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for r in &revs {
        let m = r["market"].as_str().unwrap_or("").to_string();
        let rid = r["revision_id"].as_str().unwrap_or("");
        write_json_compact(&idx.join(&m).join("provenance").join(format!("{rid}.json")), r)?;
        files += 1;
        per_market.entry(m).or_default().push(r);
    }
    let mut prints = Vec::new();
    let mut models = Vec::new();
    for m in &markets {
        let mut days: Vec<&Value> = latest.iter().filter(|((mk, _), _)| *mk == m.permaslug).map(|(_, v)| v).collect();
        days.sort_by(|a, b| a["utc_date"].as_str().cmp(&b["utc_date"].as_str()));
        let mut days7: Vec<&Value> = latest7.iter().filter(|((mk, _), _)| *mk == m.permaslug).map(|(_, v)| v).collect();
        days7.sort_by(|a, b| a["utc_date"].as_str().cmp(&b["utc_date"].as_str()));
        let by_date7: BTreeMap<&str, &Value> = days7.iter().map(|r| (r["utc_date"].as_str().unwrap_or(""), *r)).collect();
        let points: Vec<Value> = days
            .iter()
            .map(|r| {
                let mut p = point(r);
                p["index_7d"] = by_date7.get(r["utc_date"].as_str().unwrap_or("")).map(|w| point7_short(w)).unwrap_or(Value::Null);
                p
            })
            .collect();
        let latest_rev = days.last().copied();
        let latest7_rev = days7.last().copied();
        let (life, why) = lifecycle_traded(latest_rev, latest7_rev, now);
        let points7: Vec<Value> = days7.iter().map(|w| point7(w)).collect();
        let series7 = json!({
            "market": m.permaslug,
            "index_id": crate::weekly::index_id_7d(&m.permaslug),
            "label": format!("{}: OpenRouter-implied billings proxy, trailing 7-day mean, USD/day", m.name),
            "formula": {"version": crate::weekly::FORMULA_VERSION_7D, "hash": hex::encode(crate::weekly::formula_hash_7d()), "derived_from": FORMULA_VERSION},
            "notice": NOT_SETTLEMENT_GRADE,
            "attribution": latest7_rev.and_then(|r| r["attribution"].as_str()),
            "revisions_total": per_market7.get(&m.permaslug).copied().unwrap_or(0),
            "points": points7,
        });
        write_json(&idx7.join(&m.permaslug).join("series.json"), &series7)?;
        files += 1;
        if let Some(w) = latest7_rev {
            write_json(&idx7.join(&m.permaslug).join("latest.json"), w)?;
            files += 1;
            prints.push(w["print"].clone());
        }
        let series = json!({
            "market": m.permaslug,
            "index_id": engine::index_id(&m.permaslug),
            "label": format!("{}: OpenRouter-implied daily billings proxy, USD/day", m.name),
            "formula": {"version": FORMULA_VERSION, "hash": hex::encode(engine::formula_hash()), "mode": MODE},
            "notice": NOT_SETTLEMENT_GRADE,
            "attribution": latest_rev.and_then(|r| r["attribution"].as_str()),
            "revisions_total": per_market.get(&m.permaslug).map(|v| v.len()).unwrap_or(0),
            "points": points,
        });
        write_json(&idx.join(&m.permaslug).join("series.json"), &series)?;
        files += 1;
        if let Some(r) = latest_rev {
            write_json(&idx.join(&m.permaslug).join("latest.json"), r)?;
            files += 1;
            if !r["print"].is_null() {
                prints.push(r["print"].clone());
            }
        }
        models.push(json!({
            "market_id": format!("ZFX-OR-{}", m.permaslug),
            "permaslug": m.permaslug,
            "index_id": engine::index_id(&m.permaslug),
            "label": format!("{}: OpenRouter-implied daily billings proxy, USD/day", m.name),
            "model_id": m.model_id,
            "name": m.name,
            "author": m.author,
            "weights": m.weights(),
            "hugging_face_id": m.hugging_face_id,
            "modality": m.modality,
            "created": m.created_s.map(|s| fmt_ms(s * 1000)),
            "listing": {"listed_at": fmt_ms(m.listed_at_ms), "basis_day": m.listing_day, "basis_rank": m.listing_rank, "basis_tokens": m.listing_tokens},
            "lifecycle": {"status": life, "reason": why, "trading": "PAPER_ONLY"},
            "traded_index": "7D",
            "latest": latest_rev.map(point),
            "series_path": format!("/v1/indices/or-model-spend/{}/series.json", m.permaslug),
            "index_7d": {
                "index_id": crate::weekly::index_id_7d(&m.permaslug),
                "latest": latest7_rev.map(point7),
                "series_path": format!("/v1/indices/or-model-spend-7d/{}/series.json", m.permaslug),
            },
            "manipulation_cost_usd_per_pct": latest7_rev.and_then(|w| w["manipulation_cost_usd_per_pct"].as_str()),
            "max_open_interest_usd": latest7_rev.and_then(|w| w["max_open_interest_usd"].as_str()),
        }));
    }
    // licensed (documented-sources-only) variant, daily and 7D
    let lic_dir = base.join("indices").join("or-model-spend-lic");
    let lic7_dir = base.join("indices").join("or-model-spend-lic-7d");
    let lic_latest = crate::weekly::latest_in(l, crate::licensed::REVISIONS_LIC)?;
    let lic7_latest = crate::weekly::latest_in(l, crate::weekly::REVISIONS_LIC_7D)?;
    for (file, dir) in [(crate::licensed::REVISIONS_LIC, &lic_dir), (crate::weekly::REVISIONS_LIC_7D, &lic7_dir)] {
        for r in l.read_json(file)? {
            let m = r["market"].as_str().unwrap_or("").to_string();
            let rid = r["revision_id"].as_str().unwrap_or("");
            write_json_compact(&dir.join(&m).join("provenance").join(format!("{rid}.json")), &r)?;
            files += 1;
        }
    }
    let spec_lic7 = crate::weekly::Spec7d::lic();
    let token_hist = crate::licensed::dataset_view(&s);
    for mv in models.iter_mut() {
        let slug = mv["permaslug"].as_str().unwrap_or("").to_string();
        let mut d1: Vec<&Value> = lic_latest.iter().filter(|((mk, _), _)| *mk == slug).map(|(_, v)| v).collect();
        d1.sort_by(|a, b| a["utc_date"].as_str().cmp(&b["utc_date"].as_str()));
        let mut d7: Vec<&Value> = lic7_latest.iter().filter(|((mk, _), _)| *mk == slug).map(|(_, v)| v).collect();
        d7.sort_by(|a, b| a["utc_date"].as_str().cmp(&b["utc_date"].as_str()));
        let history: Vec<Value> = token_hist.rows.iter().filter(|((_, m), _)| *m == slug).map(|((d, _), t)| json!([d.to_string(), t.to_string()])).collect();
        let series = json!({
            "market": slug,
            "index_id": crate::licensed::index_id_lic(&slug),
            "formula": {"version": crate::licensed::FORMULA_VERSION_LIC, "hash": hex::encode(crate::licensed::formula_hash_lic()), "mode": crate::licensed::MODE_LIC},
            "sources": "documented only: datasets/rankings-daily (CC BY 4.0), /api/v1/models, /api/v1/models/{id}/endpoints",
            "attribution": d1.last().and_then(|r| r["attribution"].as_str()),
            "points": d1.iter().map(|r| point_lic(r)).collect::<Vec<_>>(),
            "token_history": {"note": "documented total_tokens per UTC day (top-50 days only), from 2025-01-01; no USD value before price_backfill_from", "rows": history},
        });
        write_json(&lic_dir.join(&slug).join("series.json"), &series)?;
        let series7 = json!({
            "market": slug,
            "index_id": spec_lic7.index_id(&slug),
            "formula": {"version": spec_lic7.version, "hash": hex::encode(spec_lic7.formula_hash()), "derived_from": crate::licensed::FORMULA_VERSION_LIC},
            "points": d7.iter().map(|w| point7(w)).collect::<Vec<_>>(),
        });
        write_json(&lic7_dir.join(&slug).join("series.json"), &series7)?;
        files += 2;
        if let Some(r) = d1.last() {
            write_json(&lic_dir.join(&slug).join("latest.json"), r)?;
            files += 1;
            if !r["print"].is_null() {
                prints.push(r["print"].clone());
            }
        }
        if let Some(w) = d7.last() {
            write_json(&lic7_dir.join(&slug).join("latest.json"), w)?;
            files += 1;
            prints.push(w["print"].clone());
        }
        let (life, why) = lifecycle_traded(d1.last().copied(), d7.last().copied(), now);
        mv["licensed"] = json!({
            "index_id": crate::licensed::index_id_lic(&slug),
            "latest": d1.last().map(|r| point_lic(r)),
            "series_path": format!("/v1/indices/or-model-spend-lic/{slug}/series.json"),
            "index_7d": {"index_id": spec_lic7.index_id(&slug), "latest": d7.last().map(|w| point7(w)), "series_path": format!("/v1/indices/or-model-spend-lic-7d/{slug}/series.json")},
            "manipulation_cost_usd_per_pct": d7.last().and_then(|w| w["manipulation_cost_usd_per_pct"].as_str()),
            "max_open_interest_usd": d7.last().and_then(|w| w["max_open_interest_usd"].as_str()),
            "lifecycle": {"status": life, "reason": why, "trading": "PAPER_ONLY"},
        });
    }
    let header = json!({
        "generated_at": fmt_ms(now),
        "notice": NOT_SETTLEMENT_GRADE,
        "what_this_is_not": "Not AI-provider revenue, not model-owner royalties, not equity or any claim on a model's cash flows. One venue (OpenRouter), one declared formula.",
        "formula": {"version": FORMULA_VERSION, "hash": hex::encode(engine::formula_hash()), "mode": MODE},
        "formula_7d": {"version": crate::weekly::FORMULA_VERSION_7D, "hash": hex::encode(crate::weekly::formula_hash_7d())},
        "formula_licensed": {"version": crate::licensed::FORMULA_VERSION_LIC, "hash": hex::encode(crate::licensed::formula_hash_lic()), "version_7d": crate::weekly::FORMULA_VERSION_LIC_7D, "hash_7d": hex::encode(crate::weekly::Spec7d::lic().formula_hash())},
        "recommended_reference": "licensed.index_7d (ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1): documented sources only. The web-endpoint variant (index_7d) is kept for comparison and for models outside the documented top 50. See docs/DATA_METHOD.md section 2.",
        "manipulation_note": "manipulation_cost_usd_per_pct is 1% of the 7-day window's counted spend: the OpenRouter-billed spend a third party must add to move the 7D index by 1%. max_open_interest_usd is 25% of that window spend. This bounds the cost of a push; it does not make manipulation impossible. See docs/DATA_METHOD.md, manipulation resistance.",
        "reporter_pubkey_ed25519": hex::encode(pubkey),
    });
    let mut mm = header.clone();
    mm["markets"] = json!(models);
    write_json(&base.join("markets").join("models.json"), &mm)?;
    let mut pp = header.clone();
    pp["prints"] = json!(prints);
    write_json(&base.join("indices").join("prints").join("latest.json"), &pp)?;
    write_json(
        &base.join("oracle").join("reporter.json"),
        &json!({
            "scheme": "ed25519 (RFC 8032) over canonical OraclePrint v1 bytes",
            "domain": "ZFX-oracle-print-v1",
            "pubkey_hex": hex::encode(pubkey),
            "spec": "markets/index/docs/ORACLE_PRINT_V1.md",
            "vectors": "markets/index/vectors/oracle_print_v1.json",
            "trust_note": "A valid signature proves which reporter key signed a print. It does not make the OpenRouter input true, complete or manipulation-free.",
        }),
    )?;
    let h = source_health(l, &s, params, budget, now)?;
    write_json(&base.join("indices").join("source-health.json"), &h)?;
    files += 4;
    Ok(files)
}

fn point_lic(r: &Value) -> Value {
    json!({
        "utc_date": r["utc_date"],
        "value_usd": r["value_usd"],
        "data_status": r["data_status"],
        "reason": r["reason"],
        "finality": r["finality"],
        "revision": r["revision"],
        "revision_id": r["revision_id"],
        "confidence": r["confidence"],
        "blocked": r["blocked"],
        "total_tokens": r["total_tokens"],
        "frozen": r["frozen"],
        "price": r["price"],
        "source_as_of": r["source_as_of"],
        "print_hash": r["print"]["printHash"],
    })
}

fn point7(w: &Value) -> Value {
    json!({
        "utc_date": w["utc_date"],
        "window": [w["window_start"], w["window_end"]],
        "value_usd": w["value_usd"],
        "data_status": w["data_status"],
        "reason": w["reason"],
        "valid_days": w["valid_days"],
        "window_sum_usd": w["window_sum_usd"],
        "manipulation_cost_usd_per_pct": w["manipulation_cost_usd_per_pct"],
        "max_open_interest_usd": w["max_open_interest_usd"],
        "change_vs_prev": w["change_vs_prev"],
        "clamped": w["clamped"],
        "lifecycle": day_lifecycle_7d(w),
        "finality": w["finality"],
        "revision": w["revision"],
        "revision_id": w["revision_id"],
        "observed_at": w["observed_at"],
        "print_hash": w["print"]["printHash"],
    })
}

fn point7_short(w: &Value) -> Value {
    json!({
        "value_usd": w["value_usd"],
        "data_status": w["data_status"],
        "manipulation_cost_usd_per_pct": w["manipulation_cost_usd_per_pct"],
        "max_open_interest_usd": w["max_open_interest_usd"],
        "clamped": w["clamped"],
        "lifecycle": day_lifecycle_7d(w),
        "revision_id": w["revision_id"],
    })
}

fn point(r: &Value) -> Value {
    json!({
        "utc_date": r["utc_date"],
        "value_usd": r["value_usd"],
        "data_status": r["data_status"],
        "finality": r["finality"],
        "revision": r["revision"],
        "revision_id": r["revision_id"],
        "confidence": r["confidence"],
        "blocked": r["blocked"],
        "check_codes": r["checks"].as_array().map(|a| a.iter().map(|c| c["code"].clone()).collect::<Vec<_>>()),
        "tokens": r["tokens"],
        "mix": r["mix"],
        "price": r["price"].as_object().map(|p| json!({"basis": p["basis"], "p_in_usd_per_m": p["p_in_usd_per_m"], "p_out_usd_per_m": p["p_out_usd_per_m"], "as_of": p["as_of"]})),
        "rank": r["rank"],
        "reason": r["reason"],
        "source_as_of": r["source_as_of"],
        "observed_at": r["observed_at"],
        "print_hash": r["print"]["printHash"],
    })
}

/// Pairs used by tests and the CLI to sanity check listed kinds.
pub fn kind_table() -> Vec<(&'static str, bool)> {
    [Kind::RankingsDay, Kind::RankingsDaySpend, Kind::ModelActivity, Kind::EffectivePricing, Kind::CatalogModels, Kind::ModelEndpoints, Kind::RankingsDailyApi]
        .iter()
        .map(|k| (k.name(), k.documented()))
        .collect()
}

pub fn market_dir_ok(m: &Market) -> bool {
    crate::server::slug_ok(&m.permaslug)
}

fn revisions_7d_summary(l: &Ledger) -> Result<Value, LedgerError> {
    let revs = l.read_json(crate::weekly::REVISIONS_7D)?;
    let latest = crate::weekly::latest_7d(l)?;
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for r in latest.values() {
        *by.entry(r["data_status"].as_str().unwrap_or("?").to_string()).or_default() += 1;
    }
    Ok(json!({
        "total": revs.len(),
        "corrections": revs.iter().filter(|r| r["finality"] == "CORRECTED").count(),
        "days_by_data_status": by,
        "days_clamped": latest.values().filter(|r| r["clamped"] == json!(true)).count(),
    }))
}
