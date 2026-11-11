//! `ZFX-OR-MODEL-SPEND-USD-7D-v1`: trailing 7-day mean of the daily index, with
//! manipulation-cost metrics and a jump clamp. Frozen in
//! `formula/ZFX-OR-MODEL-SPEND-USD-7D-v1.txt`; constants here must match it (tested).

use crate::engine::{hex32, print_json, Signer};
use crate::fixed::{div_round, fmt_scaled, USD_SCALE};
use crate::ledger::{Ledger, LedgerError};
use crate::time::{fmt_ms, parse_rfc3339_ms, UtcDate};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use zfx_oracle_print::{DataStatus, Finality, OraclePrint, OracleStatus};

pub const FORMULA_VERSION_7D: &str = "ZFX-OR-MODEL-SPEND-USD-7D-v1";
pub const FORMULA_TEXT_7D: &[u8] = include_bytes!("../../../formula/ZFX-OR-MODEL-SPEND-USD-7D-v1.txt");
pub const REVISIONS_7D: &str = "revisions_7d.jsonl";

#[derive(Clone, Debug)]
pub struct Params7d {
    pub window_days: i64,
    pub min_valid_days: usize,
    pub oi_fraction_bps: u128,
    pub clamp_bound_bps: u128,
    pub valid_for_ms: i64,
}
impl Default for Params7d {
    fn default() -> Self {
        Params7d { window_days: 7, min_valid_days: 6, oi_fraction_bps: 2500, clamp_bound_bps: 1500, valid_for_ms: 48 * 3_600_000 }
    }
}

pub const FORMULA_VERSION_LIC_7D: &str = "ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1";
pub const FORMULA_TEXT_LIC_7D: &[u8] = include_bytes!("../../../formula/ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1.txt");
pub const REVISIONS_LIC_7D: &str = "revisions_lic_7d.jsonl";

/// One 7D index definition: which daily revisions it averages and how it is named.
#[derive(Clone, Debug)]
pub struct Spec7d {
    pub in_file: &'static str,
    pub out_file: &'static str,
    pub id_prefix: &'static str,
    pub version: &'static str,
    pub formula_text: &'static [u8],
    pub params: Params7d,
}

impl Spec7d {
    /// 7D over the web-endpoint daily index (clamp 15%, as frozen in 7D-v1).
    pub fn web() -> Spec7d {
        Spec7d { in_file: "revisions.jsonl", out_file: REVISIONS_7D, id_prefix: "or-model-spend-7d", version: FORMULA_VERSION_7D, formula_text: FORMULA_TEXT_7D, params: Params7d::default() }
    }
    /// 7D over the licensed daily index (clamp 25%, derived from 2025-01-01..2026-11-09 history).
    pub fn lic() -> Spec7d {
        Spec7d {
            in_file: crate::licensed::REVISIONS_LIC,
            out_file: REVISIONS_LIC_7D,
            id_prefix: "or-model-spend-lic-7d",
            version: FORMULA_VERSION_LIC_7D,
            formula_text: FORMULA_TEXT_LIC_7D,
            params: Params7d { clamp_bound_bps: 2500, ..Params7d::default() },
        }
    }
    pub fn formula_hash(&self) -> [u8; 32] {
        zfx_oracle_print::sha256(self.formula_text)
    }
    pub fn index_id(&self, permaslug: &str) -> String {
        format!("{}/{permaslug}", self.id_prefix)
    }
}

pub fn formula_hash_7d() -> [u8; 32] {
    zfx_oracle_print::sha256(FORMULA_TEXT_7D)
}

pub fn index_id_7d(permaslug: &str) -> String {
    format!("or-model-spend-7d/{permaslug}")
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct WindowDay {
    pub utc_date: String,
    pub daily_revision_id: Option<String>,
    pub data_status: Option<String>,
    pub blocked: bool,
    pub value_usd: Option<String>,
    pub print_hash: Option<String>,
    pub counted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Computed7d {
    pub market: String,
    pub date: UtcDate,
    pub window: Vec<WindowDay>,
    pub n_valid: usize,
    pub sum_micro: u128,
    pub value_micro: Option<u128>,
    pub data_status: DataStatus,
    pub observed_at_ms: i64,
    pub value_hashes: Vec<[u8; 32]>,
}

/// Window computation for one market and day from the latest daily revisions.
/// `daily` maps utc_date -> latest daily revision for this market.
pub fn compute(market: &str, d: UtcDate, daily: &BTreeMap<String, &Value>, p: &Params7d) -> Option<Computed7d> {
    daily.get(&d.to_string())?;
    let mut window = Vec::new();
    let mut sum: u128 = 0;
    let mut n = 0usize;
    let mut observed = 0i64;
    let mut hashes = Vec::new();
    for k in (0..p.window_days).rev() {
        let day = UtcDate(d.0 - k);
        let r = daily.get(&day.to_string()).copied();
        let ok = r.map(|r| r["data_status"] == "OK" && r["blocked"] != json!(true)).unwrap_or(false);
        let v = r.and_then(|r| r["value_micro_usd"].as_str()).and_then(|x| x.parse::<u128>().ok());
        let counted = ok && v.is_some();
        if counted {
            sum += v.unwrap();
            n += 1;
            if let Some(h) = r.and_then(|r| r["print"]["printHash"].as_str()) {
                hashes.push(hex32(h));
            }
            if let Some(t) = r.and_then(|r| r["observed_at"].as_str()).and_then(parse_rfc3339_ms) {
                observed = observed.max(t);
            }
        }
        window.push(WindowDay {
            utc_date: day.to_string(),
            daily_revision_id: r.and_then(|r| r["revision_id"].as_str().map(String::from)),
            data_status: r.and_then(|r| r["data_status"].as_str().map(String::from)),
            blocked: r.map(|r| r["blocked"] == json!(true)).unwrap_or(false),
            value_usd: r.and_then(|r| r["value_usd"].as_str().map(String::from)),
            print_hash: r.and_then(|r| r["print"]["printHash"].as_str().map(String::from)),
            counted,
        });
    }
    let ok = n >= p.min_valid_days;
    Some(Computed7d {
        market: market.to_string(),
        date: d,
        window,
        n_valid: n,
        sum_micro: sum,
        value_micro: if ok { Some(div_round(sum, n as u128)) } else { None },
        data_status: if ok { DataStatus::Ok } else { DataStatus::NotReported },
        observed_at_ms: observed,
        value_hashes: hashes,
    })
}

/// Cost to move the mean by 1%: S / 100 (micro-USD). See formula section 4.
pub fn manipulation_cost_per_pct(sum_micro: u128) -> u128 {
    div_round(sum_micro, 100)
}

pub fn max_open_interest(sum_micro: u128, p: &Params7d) -> u128 {
    div_round(sum_micro * p.oi_fraction_bps, 10_000)
}

/// (change in ppm, signed as string; clamped flag). None if no previous OK value.
pub fn clamp(value: u128, prev: Option<u128>, p: &Params7d) -> (Option<String>, bool) {
    let Some(prev) = prev.filter(|x| *x > 0) else { return (None, false) };
    let diff = value.abs_diff(prev);
    let ppm = div_round(diff * 1_000_000, prev);
    let sign = if value < prev { "-" } else { "" };
    // |change| > bound  <=>  diff * 10000 > bound_bps * prev
    let clamped = diff * 10_000 > p.clamp_bound_bps * prev;
    (Some(format!("{sign}{}", fmt_scaled(ppm, 6))), clamped)
}

fn build_print(c: &Computed7d, revision: u32, prev_hash: [u8; 32], finality: Finality, now: i64, spec: &Spec7d) -> OraclePrint {
    let p = &spec.params;
    OraclePrint {
        index_id: spec.index_id(&c.market),
        version: spec.version.into(),
        value_scale: USD_SCALE as u8,
        value: c.value_micro.unwrap_or(0) as u64,
        source_start_ms: UtcDate(c.date.0 - (p.window_days - 1)).start_ms(),
        source_end_ms: c.date.end_ms(),
        observed_at_ms: c.observed_at_ms,
        published_at_ms: now,
        valid_until_ms: c.date.end_ms() + p.valid_for_ms,
        source_hash: zfx_oracle_print::source_set_hash(&c.value_hashes),
        formula_hash: spec.formula_hash(),
        status: if c.data_status == DataStatus::Ok { OracleStatus::Fresh } else { OracleStatus::Unavailable },
        data_status: c.data_status,
        finality,
        revision,
        prev_print_hash: prev_hash,
    }
}

pub fn fingerprint(c: &Computed7d, clamped: bool) -> Value {
    json!({
        "data_status": c.data_status.name(),
        "value_micro": c.value_micro.map(|v| v.to_string()),
        "inputs": c.window.iter().map(|w| json!([w.utc_date, w.daily_revision_id, w.counted])).collect::<Vec<_>>(),
        "clamped": clamped,
    })
}

#[derive(Default, Debug, Serialize)]
pub struct Report7d {
    pub evaluated: usize,
    pub new_final: usize,
    pub new_corrected: usize,
    pub clamped: usize,
    pub unchanged: usize,
}

pub fn latest_7d(l: &Ledger) -> Result<BTreeMap<(String, String), Value>, LedgerError> {
    latest_in(l, REVISIONS_7D)
}

pub fn latest_in(l: &Ledger, file: &str) -> Result<BTreeMap<(String, String), Value>, LedgerError> {
    let mut m = BTreeMap::new();
    for v in l.read_json(file)? {
        m.insert((v["market"].as_str().unwrap_or("").to_string(), v["utc_date"].as_str().unwrap_or("").to_string()), v);
    }
    Ok(m)
}

/// 7D over the web-endpoint daily index with the given parameters.
pub fn run(l: &Ledger, signer: &Signer, p: &Params7d, now: i64) -> Result<Report7d, LedgerError> {
    run_spec(l, signer, &Spec7d { params: p.clone(), ..Spec7d::web() }, now)
}

pub fn run_spec(l: &Ledger, signer: &Signer, spec: &Spec7d, now: i64) -> Result<Report7d, LedgerError> {
    let p = &spec.params;
    let daily_all = latest_in(l, spec.in_file)?;
    let mut latest = latest_in(l, spec.out_file)?;
    let mut by_market: BTreeMap<&str, BTreeMap<String, &Value>> = BTreeMap::new();
    for ((m, d), v) in &daily_all {
        by_market.entry(m.as_str()).or_default().insert(d.clone(), v);
    }
    let mut rep = Report7d::default();
    for (market, daily) in &by_market {
        let days: Vec<UtcDate> = daily.keys().filter_map(|d| UtcDate::parse(d)).collect();
        for d in days {
            let Some(c) = compute(market, d, daily, p) else { continue };
            rep.evaluated += 1;
            let prev_val = latest
                .get(&(market.to_string(), d.pred().to_string()))
                .filter(|r| r["data_status"] == "OK")
                .and_then(|r| r["value_micro_usd"].as_str())
                .and_then(|x| x.parse::<u128>().ok());
            let (change, clamped) = match c.value_micro {
                Some(v) => clamp(v, prev_val, p),
                None => (None, false),
            };
            let fp = fingerprint(&c, clamped);
            let key = (market.to_string(), d.to_string());
            let prev = latest.get(&key);
            if prev.map(|r| r["fingerprint"] == fp).unwrap_or(false) {
                rep.unchanged += 1;
                continue;
            }
            let (rev, fin) = match prev {
                None => (1, Finality::Final),
                Some(r) => (r["revision"].as_u64().unwrap_or(0) as u32 + 1, Finality::Corrected),
            };
            let prev_hash = prev.and_then(|r| r["print"]["printHash"].as_str()).map(hex32).unwrap_or([0; 32]);
            let pr = build_print(&c, rev, prev_hash, fin, now, spec);
            let bytes = pr.encode().expect("bounded");
            let (pk, sig) = zfx_oracle_print::sign(&bytes, &signer.seed);
            let ok = c.data_status == DataStatus::Ok;
            let mut rec = json!({
                "index": if spec.out_file == REVISIONS_7D { "7D" } else { "LIC-7D" },
                "market": market,
                "index_id": spec.index_id(market),
                "formula_version": spec.version,
                "formula_hash": hex::encode(spec.formula_hash()),
                "utc_date": d.to_string(),
                "window_start": UtcDate(d.0 - (p.window_days - 1)).to_string(),
                "window_end": d.to_string(),
                "source_start": fmt_ms(pr.source_start_ms),
                "source_end": fmt_ms(pr.source_end_ms),
                "revision": rev,
                "revision_id": format!("{d}.r{rev}"),
                "prev_revision_id": prev.map(|r| r["revision_id"].clone()),
                "finality": fin.name(),
                "data_status": c.data_status.name(),
                "reason": if ok { Value::Null } else { json!(format!("{} of {} window days valid, {} required", c.n_valid, p.window_days, p.min_valid_days)) },
                "valid_days": c.n_valid,
                "value_usd": c.value_micro.map(|v| fmt_scaled(v, USD_SCALE)),
                "value_micro_usd": c.value_micro.map(|v| v.to_string()),
                "window_sum_usd": if ok { json!(fmt_scaled(c.sum_micro, USD_SCALE)) } else { Value::Null },
                "manipulation_cost_usd_per_pct": if ok { json!(fmt_scaled(manipulation_cost_per_pct(c.sum_micro), USD_SCALE)) } else { Value::Null },
                "max_open_interest_usd": if ok { json!(fmt_scaled(max_open_interest(c.sum_micro, p), USD_SCALE)) } else { Value::Null },
                "prev_7d_value_usd": prev_val.map(|v| fmt_scaled(v, USD_SCALE)),
                "change_vs_prev": change,
                "clamped": clamped,
                "clamp_bound": fmt_scaled(p.clamp_bound_bps * 100, 6),
                "window": c.window,
                "observed_at": fmt_ms(c.observed_at_ms),
                "created_at": fmt_ms(now),
                "attribution": daily.get(&d.to_string()).and_then(|r| r["attribution"].as_str()),
                "fingerprint": fp,
                "print": print_json(&pr, &[(pk, sig)]),
            });
            if let (Some(pr0), Finality::Corrected) = (prev, fin) {
                let to = rec["fingerprint"].clone();
                rec.as_object_mut().unwrap().insert("correction".into(), json!({"from": pr0["fingerprint"], "to": to}));
            }
            l.append_json(spec.out_file, &mut rec)?;
            if clamped {
                rep.clamped += 1;
            }
            match fin {
                Finality::Final => rep.new_final += 1,
                _ => rep.new_corrected += 1,
            }
            latest.insert(key, rec);
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_math() {
        let p = Params7d::default();
        assert_eq!(clamp(115, Some(100), &p), (Some("0.150000".into()), false));
        assert_eq!(clamp(116, Some(100), &p), (Some("0.160000".into()), true));
        assert_eq!(clamp(85, Some(100), &p), (Some("-0.150000".into()), false));
        assert_eq!(clamp(84, Some(100), &p), (Some("-0.160000".into()), true));
        assert_eq!(clamp(84, None, &p), (None, false));
    }

    #[test]
    fn cost_metrics() {
        let p = Params7d::default();
        // 7 days x $100 = $700: 1% move costs $7; OI cap $175
        let s = 700_000_000u128;
        assert_eq!(manipulation_cost_per_pct(s), 7_000_000);
        assert_eq!(max_open_interest(s, &p), 175_000_000);
    }

    #[test]
    fn lic_formula_text_matches() {
        let s = Spec7d::lic();
        let t = std::str::from_utf8(s.formula_text).unwrap();
        assert!(t.starts_with(s.version));
        for x in [format!("window_days = {}", s.params.window_days), format!("min_valid_days = {}", s.params.min_valid_days), format!("oi_fraction_bps = {}", s.params.oi_fraction_bps), format!("clamp_bound_bps = {}", s.params.clamp_bound_bps)] {
            assert!(t.contains(&x), "{x}");
        }
    }

    #[test]
    fn formula_text_matches() {
        let t = std::str::from_utf8(FORMULA_TEXT_7D).unwrap();
        let p = Params7d::default();
        assert!(t.starts_with(FORMULA_VERSION_7D));
        for s in [
            format!("window_days = {}", p.window_days),
            format!("min_valid_days = {}", p.min_valid_days),
            format!("oi_fraction_bps = {}", p.oi_fraction_bps),
            format!("clamp_bound_bps = {}", p.clamp_bound_bps),
        ] {
            assert!(t.contains(&s), "{s}");
        }
    }
}
