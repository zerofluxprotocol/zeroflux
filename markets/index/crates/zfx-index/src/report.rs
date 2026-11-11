//! Statistics published in source-health: documented dataset coverage and latency,
//! documented-vs-web token cross-check, licensed-vs-web tracking, and the 7D clamp and
//! min-valid-days calibration from the documented history. Integer math; ratios are
//! reported as fixed-point strings (6 decimals).

use crate::engine::{self, Loaded};
use crate::fetch::DATASET_DAILY_CAP;
use crate::fixed::{div_round, fmt_scaled};
use crate::ledger::{Ledger, LedgerError};
use crate::licensed::{self, REVISIONS_LIC};
use crate::time::{fmt_ms, UtcDate};
use crate::weekly::{latest_in, REVISIONS_7D, REVISIONS_LIC_7D};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const PPM: u128 = 1_000_000;

fn pct_of(sorted: &[u128], p: usize) -> Option<u128> {
    if sorted.is_empty() {
        return None;
    }
    Some(sorted[((sorted.len() - 1) * p / 1000).min(sorted.len() - 1)])
}

fn ppm_str(v: Option<u128>) -> Value {
    v.map(|x| json!(fmt_scaled(x, 6))).unwrap_or(Value::Null)
}

fn rel_ppm(a: u128, b: u128) -> u128 {
    if b == 0 {
        return if a == 0 { 0 } else { u128::MAX / 4 };
    }
    div_round(a.abs_diff(b) * PPM, b)
}

pub fn dataset_summary(s: &Loaded, now: i64) -> Value {
    let reqs = s.fetches.iter().filter(|f| f.kind == "rankings_daily_api").count();
    let req24 = s.fetches.iter().filter(|f| f.kind == "rankings_daily_api" && f.fetched_at_ms > now - 86_400_000).count();
    let fails = s.fetches.iter().filter(|f| f.kind == "rankings_daily_api" && f.status != 200).count();
    let view = licensed::dataset_view(s);
    let models: BTreeSet<&String> = view.rows.keys().map(|(_, m)| m).filter(|m| *m != "other").collect();
    // first time each end_date was seen, by meta.as_of
    let mut first: BTreeMap<UtcDate, (i64, i64)> = BTreeMap::new();
    for p in &s.dataset {
        first.entry(p.doc.end_date).or_insert((p.as_of_ms, p.rec.fetched_at_ms));
    }
    let lat: Vec<Value> = first
        .iter()
        .rev()
        .take(30)
        .map(|(d, (asof, f))| json!({"end_date": d.to_string(), "first_seen_as_of": fmt_ms(*asof), "first_seen_fetch_at": fmt_ms(*f), "minutes_after_day_end": (asof - d.end_ms()) / 60_000}))
        .collect();
    json!({
        "endpoint": "GET /api/v1/datasets/rankings-daily (documented, API key, CC BY 4.0)",
        "requests_total": reqs,
        "requests_failed": fails,
        "requests_24h": req24,
        "daily_cap_used_by_this_program": DATASET_DAILY_CAP,
        "account_limits": "30 requests/min per key, 500/day per account (OpenRouter docs)",
        "max_range_days": 366,
        "covered_first_day": view.covering.keys().next().map(|d| d.to_string()),
        "covered_last_day": view.covering.keys().last().map(|d| d.to_string()),
        "covered_days": view.covering.len(),
        "rows": view.rows.len(),
        "distinct_models": models.len(),
        "first_seen_by_end_date": lat,
        "note": "A day is covered once a response with meta.as_of >= day end + 6 h includes it. first_seen_by_end_date lists when each end_date first appeared; it is a live latency measure only for days first fetched after they ended.",
    })
}

/// Documented total_tokens vs web-endpoint prompt+completion for the same market-day.
pub fn crosscheck(l: &Ledger, s: &Loaded) -> Result<Value, LedgerError> {
    let view = licensed::dataset_view(s);
    let web = engine::latest_revisions(l)?;
    let mut rels: Vec<u128> = Vec::new();
    let mut exact = 0usize;
    let mut missing = 0usize;
    let mut worst: Option<(u128, String, String, u128, u128)> = None;
    let mut first: Option<String> = None;
    let mut last: Option<String> = None;
    for ((m, d), r) in &web {
        let Some(w) = r["tokens"]["total"].as_str().and_then(|x| x.parse::<u128>().ok()) else { continue };
        let Some(dd) = UtcDate::parse(d) else { continue };
        if !view.covering.contains_key(&dd) {
            continue;
        }
        match view.rows.get(&(dd, m.clone())) {
            None => missing += 1,
            Some(&t) => {
                let rp = rel_ppm(t, w);
                if t == w {
                    exact += 1;
                }
                if worst.as_ref().map(|x| rp > x.0).unwrap_or(true) {
                    worst = Some((rp, m.clone(), d.clone(), t, w));
                }
                rels.push(rp);
                if first.as_ref().map(|f| d < f).unwrap_or(true) {
                    first = Some(d.clone());
                }
                if last.as_ref().map(|f| d > f).unwrap_or(true) {
                    last = Some(d.clone());
                }
            }
        }
    }
    rels.sort_unstable();
    let n = rels.len();
    Ok(json!({
        "what": "documented datasets/rankings-daily total_tokens vs web endpoint model-activity prompt+completion, same model and UTC day",
        "days": [first, last],
        "pairs_compared": n,
        "exact_matches": exact,
        "exact_match_rate": if n > 0 { json!(fmt_scaled(div_round(exact as u128 * PPM, n as u128), 6)) } else { Value::Null },
        "max_relative_difference": ppm_str(rels.last().copied()),
        "median_relative_difference": ppm_str(pct_of(&rels, 500)),
        "worst_pair": worst.map(|w| json!({"market": w.1, "utc_date": w.2, "documented": w.3.to_string(), "web": w.4.to_string()})),
        "market_days_missing_from_documented": missing,
        "missing_note": "Missing market-days are days the model was outside the documented top 50; the web endpoint still reports them.",
    }))
}

fn ratio_stats(pairs: &[(u128, u128)]) -> Value {
    // ratio = lic / web in ppm
    let mut r: Vec<u128> = pairs.iter().filter(|(_, w)| *w > 0).map(|(a, w)| div_round(a * PPM, *w)).collect();
    r.sort_unstable();
    let mut dev: Vec<u128> = r.iter().map(|x| x.abs_diff(PPM)).collect();
    dev.sort_unstable();
    json!({
        "n": r.len(),
        "ratio_p10": ppm_str(pct_of(&r, 100)), "ratio_median": ppm_str(pct_of(&r, 500)), "ratio_p90": ppm_str(pct_of(&r, 900)),
        "abs_deviation_median": ppm_str(pct_of(&dev, 500)), "abs_deviation_p90": ppm_str(pct_of(&dev, 900)),
    })
}

/// How the licensed variant tracks the web variant over days both are OK.
pub fn tracking(l: &Ledger) -> Result<Value, LedgerError> {
    let pairs_for = |a: &BTreeMap<(String, String), Value>, b: &BTreeMap<(String, String), Value>| -> BTreeMap<String, Vec<(String, u128, u128)>> {
        let mut out: BTreeMap<String, Vec<(String, u128, u128)>> = BTreeMap::new();
        for (k, x) in a {
            let Some(y) = b.get(k) else { continue };
            let ok = |r: &Value| r["data_status"] == "OK" && r["blocked"] != json!(true);
            if !ok(x) || !ok(y) {
                continue;
            }
            let v = |r: &Value| r["value_micro_usd"].as_str().and_then(|s| s.parse::<u128>().ok());
            if let (Some(p), Some(q)) = (v(x), v(y)) {
                out.entry(k.0.clone()).or_default().push((k.1.clone(), p, q));
            }
        }
        out
    };
    let mut res = serde_json::Map::new();
    for (name, lf, wf) in [("daily", REVISIONS_LIC, "revisions.jsonl"), ("7d", REVISIONS_LIC_7D, REVISIONS_7D)] {
        let lic = latest_in(l, lf)?;
        let web = latest_in(l, wf)?;
        let per = pairs_for(&lic, &web);
        let all: Vec<(u128, u128)> = per.values().flatten().map(|(_, a, b)| (*a, *b)).collect();
        // per-market median ratio
        let med: Vec<(u128, u128)> = per
            .values()
            .filter_map(|v| {
                let mut r: Vec<u128> = v.iter().filter(|x| x.2 > 0).map(|x| div_round(x.1 * PPM, x.2)).collect();
                r.sort_unstable();
                pct_of(&r, 500).map(|m| (m, PPM))
            })
            .collect();
        // day-over-day change agreement
        let (mut agree, mut total) = (0u64, 0u64);
        let mut chg_dev: Vec<u128> = Vec::new();
        for v in per.values() {
            for w in v.windows(2) {
                let (d0, d1) = (UtcDate::parse(&w[0].0), UtcDate::parse(&w[1].0));
                if d1.map(|x| x.0) != d0.map(|x| x.0 + 1) || w[0].1 == 0 || w[0].2 == 0 {
                    continue;
                }
                let up_l = w[1].1 >= w[0].1;
                let up_w = w[1].2 >= w[0].2;
                total += 1;
                if up_l == up_w {
                    agree += 1;
                }
                let cl = div_round(w[1].1 * PPM, w[0].1) as i128 - PPM as i128;
                let cw = div_round(w[1].2 * PPM, w[0].2) as i128 - PPM as i128;
                chg_dev.push(cl.abs_diff(cw));
            }
        }
        chg_dev.sort_unstable();
        res.insert(name.into(), json!({
            "market_days": ratio_stats(&all),
            "per_market_median_ratio": ratio_stats(&med),
            "markets": per.len(),
            "day_over_day_direction_agreement": if total > 0 { json!(fmt_scaled(div_round(agree as u128 * PPM, total as u128), 6)) } else { Value::Null },
            "day_over_day_change_abs_difference_median": ppm_str(pct_of(&chg_dev, 500)),
            "day_over_day_change_abs_difference_p90": ppm_str(pct_of(&chg_dev, 900)),
            "pairs_for_change": total,
        }));
    }
    res.insert("caveat".into(), json!(format!("In-sample until {}: the frozen mix, BYOK and cache shares were derived from the same web data over {}..{}, and LISTED_BACKFILL applies current listed prices to past days. Out-of-sample tracking starts {}.", licensed::OVERLAP.1, licensed::OVERLAP.0, licensed::OVERLAP.1, UtcDate::parse(licensed::OVERLAP.1).unwrap().succ())));
    Ok(Value::Object(res))
}

/// 7D day-over-day move distribution and missing-day error from the documented history.
/// Tokens only (constant price), so it measures demand moves, not price moves.
pub fn clamp_calibration(s: &Loaded) -> Value {
    let view = licensed::dataset_view(s);
    let mut series: BTreeMap<&str, BTreeMap<i64, u128>> = BTreeMap::new();
    for ((d, m), t) in &view.rows {
        if m == "other" || m.contains(":free") {
            continue;
        }
        series.entry(m.as_str()).or_default().insert(d.0, *t);
    }
    let (Some(lo), Some(hi)) = (view.covering.keys().next(), view.covering.keys().last()) else { return Value::Null };
    let mut moves: Vec<u128> = Vec::new();
    let mut drop1: Vec<u128> = Vec::new();
    let mut drop2: Vec<u128> = Vec::new();
    let mut valid_counts = [0u64; 8];
    for ser in series.values() {
        let first = *ser.keys().next().unwrap();
        let mut prev: Option<u128> = None;
        for d in lo.0..=hi.0 {
            if d < first + 6 {
                prev = None;
                continue;
            }
            let vals: Vec<u128> = (0..7).filter_map(|k| ser.get(&(d - k)).copied()).collect();
            valid_counts[vals.len()] += 1;
            let sum: u128 = vals.iter().sum();
            if vals.len() == 7 && sum > 0 {
                for i in 0..7 {
                    // |(sum - v_i)/6 / (sum/7) - 1| = |7(sum - v_i) - 6 sum| / (6 sum)
                    let a = 7 * (sum - vals[i]);
                    drop1.push(div_round(a.abs_diff(6 * sum) * PPM, 6 * sum));
                    for j in i + 1..7 {
                        let b = 7 * (sum - vals[i] - vals[j]);
                        drop2.push(div_round(b.abs_diff(5 * sum) * PPM, 5 * sum));
                    }
                }
            }
            let cur = if vals.len() >= 6 { Some(div_round(sum, vals.len() as u128)) } else { None };
            if let (Some(c), Some(p)) = (cur, prev) {
                if p > 0 && d >= first + 20 {
                    moves.push(rel_ppm(c, p));
                }
            }
            prev = cur;
        }
    }
    moves.sort_unstable();
    drop1.sort_unstable();
    drop2.sort_unstable();
    let share_above = |b: u128| if moves.is_empty() { Value::Null } else { json!(fmt_scaled(div_round(moves.iter().filter(|x| **x > b).count() as u128 * PPM, moves.len() as u128), 6)) };
    let pcts = |v: &Vec<u128>| json!({"p50": ppm_str(pct_of(v, 500)), "p90": ppm_str(pct_of(v, 900)), "p95": ppm_str(pct_of(v, 950)), "p99": ppm_str(pct_of(v, 990)), "p99_5": ppm_str(pct_of(v, 995)), "p99_9": ppm_str(pct_of(v, 999))});
    json!({
        "data": format!("documented dataset, {} .. {}, {} models (excluding `other` and :free), tokens only", lo, hi, series.len()),
        "established_moves": {"n": moves.len(), "definition": "|7D(d)/7D(d-1) - 1| where both days have >= 6 of 7 days and d is >= 20 days after the model's first row", "percentiles": pcts(&moves),
            "share_above": {"0.10": share_above(100_000), "0.15": share_above(150_000), "0.20": share_above(200_000), "0.25": share_above(250_000), "0.30": share_above(300_000)}},
        "missing_day_error": {"drop_one_of_seven": pcts(&drop1), "drop_two_of_seven": pcts(&drop2), "note": "Error of the mean when 1 or 2 days of a full window are removed at random. Real missing days are dropouts from the top 50, so they are low-volume days, and the remaining mean is biased upward."},
        "valid_day_count_distribution": {"0": valid_counts[0], "1": valid_counts[1], "2": valid_counts[2], "3": valid_counts[3], "4": valid_counts[4], "5": valid_counts[5], "6": valid_counts[6], "7": valid_counts[7]},
        "chosen": {"clamp_bound": "0.250000", "percentile": "p99 of established moves, rounded down to 5%", "min_valid_days": 6,
            "reason": "At 15% the clamp would turn a market close-only on about 3.7% of days (about 13 days a year). 25% is exceeded on about 1.1%. Above it the move is in the top 1% of observed demand moves, where a push or a data problem is a plausible cause. Dropping one day keeps the p99 error well under the bound; dropping two brings p99 error near it, so 5 of 7 is not accepted."},
    })
}
