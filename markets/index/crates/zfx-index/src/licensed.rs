//! `ZFX-OR-MODEL-SPEND-USD-LIC-v1`: the daily index from documented sources only.
//! Tokens: documented `datasets/rankings-daily` (CC BY 4.0). Prices: documented
//! `/api/v1/models/{id}/endpoints` (catalog fallback). Mix: frozen per model in
//! `formula/ZFX-OR-MODEL-SPEND-USD-LIC-v1.params.json`. Spec: `formula/ZFX-OR-MODEL-SPEND-USD-LIC-v1.txt`.

use crate::engine::{self, hex32, print_json, Check, Loaded, Market, Signer};
use crate::fixed::{div_round, fmt_ppm, fmt_scaled, PRICE_SCALE, USD_SCALE};
use crate::ledger::{FetchRecord, Ledger, LedgerError};
use crate::source::{CatalogModel, ListedEndpoint};
use crate::time::{fmt_ms, UtcDate};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use zfx_oracle_print::{DataStatus, Finality, OraclePrint, OracleStatus};

pub const FORMULA_VERSION_LIC: &str = "ZFX-OR-MODEL-SPEND-USD-LIC-v1";
pub const FORMULA_TEXT_LIC: &[u8] = include_bytes!("../../../formula/ZFX-OR-MODEL-SPEND-USD-LIC-v1.txt");
pub const PARAMS_LIC: &[u8] = include_bytes!("../../../formula/ZFX-OR-MODEL-SPEND-USD-LIC-v1.params.json");
pub const REVISIONS_LIC: &str = "revisions_lic.jsonl";
pub const MODE_LIC: &str = "ESTIMATED_FIXED_MIX";
pub const GRACE_MS: i64 = 6 * 3_600_000;
pub const VALID_FOR_MS: i64 = 48 * 3_600_000;
pub const PRICE_BACKFILL_FROM: &str = "2026-10-11";
pub const OVERLAP: (&str, &str) = ("2026-10-11", "2026-11-09");

pub fn formula_hash_lic() -> [u8; 32] {
    let mut b = FORMULA_TEXT_LIC.to_vec();
    b.extend_from_slice(PARAMS_LIC);
    zfx_oracle_print::sha256(&b)
}

pub fn index_id_lic(permaslug: &str) -> String {
    format!("or-model-spend-lic/{permaslug}")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Frozen {
    pub w_in_ppm: u128,
    pub billed_ppm: u128,
    pub cache_ppm: u128,
    pub days: u32,
}

pub fn frozen_params() -> BTreeMap<String, Frozen> {
    let v: Value = serde_json::from_slice(PARAMS_LIC).unwrap_or(Value::Null);
    let mut out = BTreeMap::new();
    if let Some(m) = v["models"].as_object() {
        for (k, x) in m {
            let g = |f: &str| x[f].as_str().and_then(|s| s.parse::<u128>().ok());
            if let (Some(w), Some(b), Some(c)) = (g("w_in_ppm"), g("billed_ppm"), g("cache_ppm")) {
                out.insert(k.clone(), Frozen { w_in_ppm: w, billed_ppm: b, cache_ppm: c, days: x["days"].as_u64().unwrap_or(0) as u32 });
            }
        }
    }
    out
}

/// Derives the frozen shares from the web-endpoint daily revisions over OVERLAP (counted days only).
pub fn derive_params(l: &Ledger) -> Result<Value, LedgerError> {
    let latest = engine::latest_revisions(l)?;
    let (a, b) = (UtcDate::parse(OVERLAP.0).unwrap(), UtcDate::parse(OVERLAP.1).unwrap());
    let mut acc: BTreeMap<String, [u128; 6]> = BTreeMap::new();
    let mut days: BTreeMap<String, u32> = BTreeMap::new();
    for ((m, d), r) in &latest {
        let Some(dd) = UtcDate::parse(d) else { continue };
        if dd < a || dd > b || r["data_status"] != "OK" || r["blocked"] == json!(true) {
            continue;
        }
        let t = &r["tokens"];
        let g = |k: &str| t[k].as_str().and_then(|x| x.parse::<u128>().ok()).unwrap_or(0);
        let e = acc.entry(m.clone()).or_default();
        for (i, k) in ["billed_prompt", "billed_completion", "total", "prompt", "cached_prompt"].iter().enumerate() {
            e[i] += g(k);
        }
        *days.entry(m.clone()).or_default() += 1;
    }
    let mut models = serde_json::Map::new();
    for (m, e) in acc {
        let billed = e[0] + e[1];
        if billed == 0 || e[2] == 0 || e[3] == 0 {
            continue;
        }
        models.insert(
            m.clone(),
            json!({
                "w_in_ppm": div_round(e[0] * 1_000_000, billed).to_string(),
                "billed_ppm": div_round(billed * 1_000_000, e[2]).to_string(),
                "cache_ppm": div_round(e[4].min(e[3]) * 1_000_000, e[3]).to_string(),
                "days": days[&m],
            }),
        );
    }
    Ok(json!({
        "formula": FORMULA_VERSION_LIC,
        "derived_from": format!("{} web-endpoint daily revisions, counted days {}..{}", engine::FORMULA_VERSION, OVERLAP.0, OVERLAP.1),
        "note": "Frozen composition shares, parts per million. Never updated within this formula version.",
        "models": models,
    }))
}

// ---------------------------------------------------------------------------
// Documented token rows

pub struct DatasetView<'a> {
    /// day -> index into Loaded.dataset of the response that decides it
    pub covering: BTreeMap<UtcDate, usize>,
    pub rows: BTreeMap<(UtcDate, String), u128>,
    pub s: &'a Loaded,
}

/// For each day, the most recently fetched usable dataset response (formula section 3).
pub fn dataset_view(s: &Loaded) -> DatasetView<'_> {
    let mut covering = BTreeMap::new();
    for (i, p) in s.dataset.iter().enumerate() {
        let Some(start) = p.doc.start_date.or_else(|| p.doc.rows.iter().map(|r| r.date).min()) else { continue };
        for d in start.0..=p.doc.end_date.0 {
            let d = UtcDate(d);
            if p.as_of_ms >= d.end_ms() + GRACE_MS {
                covering.insert(d, i);
            }
        }
    }
    let mut rows = BTreeMap::new();
    for (d, i) in &covering {
        for r in s.dataset[*i].doc.rows.iter().filter(|r| r.date == *d) {
            rows.insert((*d, r.permaslug.clone()), r.total_tokens);
        }
    }
    DatasetView { covering, rows, s }
}

// ---------------------------------------------------------------------------
// Documented prices

#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub l_in: u128,
    pub l_out: u128,
    pub l_cr: u128,
    pub providers: usize,
    pub source: &'static str,
}

fn lower_median(mut v: Vec<u128>) -> Option<u128> {
    v.sort_unstable();
    if v.is_empty() {
        None
    } else {
        Some(v[(v.len() - 1) / 2])
    }
}

pub fn listed_from_endpoints(e: &[ListedEndpoint]) -> Option<Listed> {
    let l_in = lower_median(e.iter().filter_map(|x| x.listed_in).filter(|x| *x > 0).collect())?;
    let l_out = lower_median(e.iter().filter_map(|x| x.listed_out).filter(|x| *x > 0).collect())?;
    let l_cr = lower_median(e.iter().filter_map(|x| x.listed_cache_read).filter(|x| *x > 0).collect()).unwrap_or(l_in);
    Some(Listed { l_in, l_out, l_cr, providers: e.len(), source: "documented /api/v1/models/{id}/endpoints" })
}

pub fn listed_from_catalog(c: &[CatalogModel], permaslug: &str) -> Option<Listed> {
    let m = c.iter().find(|c| c.canonical_slug == permaslug && !c.id.contains(':'))?;
    let (i, o) = (m.listed_in.filter(|x| *x > 0)?, m.listed_out.filter(|x| *x > 0)?);
    Some(Listed { l_in: i, l_out: o, l_cr: i, providers: 1, source: "documented /api/v1/models catalog" })
}

/// Snapshot selection (formula section 5). Returns (listed, basis, fetch record).
pub fn select_listed(m: &Market, s: &Loaded, d: UtcDate) -> Option<(Listed, &'static str, FetchRecord, i64)> {
    // candidate snapshots in fetch order: endpoints listings, then catalog as fallback
    let mut cands: Vec<(i64, Listed, FetchRecord)> = Vec::new();
    if let Some(h) = s.endpoint_history.get(&m.model_id) {
        for p in h {
            if let Some(x) = listed_from_endpoints(&p.doc) {
                cands.push((p.as_of_ms, x, p.rec.clone()));
            }
        }
    }
    if cands.is_empty() {
        for p in &s.catalog_history {
            if let Some(x) = listed_from_catalog(&p.doc, &m.permaslug) {
                cands.push((p.as_of_ms, x, p.rec.clone()));
            }
        }
    }
    let after = cands.iter().filter(|c| c.0 >= d.end_ms() && c.0 < d.end_ms() + 86_400_000).min_by_key(|c| (c.0, c.2.seq));
    let during = cands.iter().filter(|c| c.0 >= d.start_ms() && c.0 < d.end_ms()).max_by_key(|c| (c.0, c.2.seq));
    if let Some(c) = after.or(during) {
        return Some((c.1.clone(), "LISTED_SNAPSHOT", c.2.clone(), c.0));
    }
    let first = cands.iter().min_by_key(|c| (c.0, c.2.seq))?;
    let from = UtcDate::parse(PRICE_BACKFILL_FROM).unwrap();
    if d >= from && d.end_ms() <= first.0 {
        return Some((first.1.clone(), "LISTED_BACKFILL", first.2.clone(), first.0));
    }
    None
}

/// Formula section 6.
pub fn value_micro(total: u128, f: &Frozen, l: &Listed) -> (u128, u128, u128) {
    let p_in = div_round((1_000_000 - f.cache_ppm) * l.l_in + f.cache_ppm * l.l_cr, 1_000_000);
    let p_blend = div_round(f.w_in_ppm * p_in + (1_000_000 - f.w_in_ppm) * l.l_out, 1_000_000);
    let t_billed = div_round(total * f.billed_ppm, 1_000_000);
    (div_round(t_billed * p_blend, 1_000_000_000), p_in, p_blend)
}

// ---------------------------------------------------------------------------
// Revisions

#[derive(Default, Debug, Serialize)]
pub struct ReportLic {
    pub evaluated: usize,
    pub new_final: usize,
    pub new_corrected: usize,
    pub blocked: usize,
    pub unchanged: usize,
}

fn input_json(role: &str, rec: &FetchRecord, as_of: i64) -> Value {
    json!({"role": role, "kind": rec.kind, "documented": rec.documented, "fetch_seq": rec.seq, "url": rec.url, "sha256": rec.sha256, "fetched_at": fmt_ms(rec.fetched_at_ms), "source_as_of": fmt_ms(as_of)})
}

pub fn run(l: &Ledger, signer: &Signer, now: i64) -> Result<ReportLic, LedgerError> {
    let s = engine::load(l)?;
    let markets = engine::load_markets(l)?;
    let frozen = frozen_params();
    let view = dataset_view(&s);
    let mut latest = crate::weekly::latest_in(l, REVISIONS_LIC)?;
    let mut rep = ReportLic::default();
    let from = UtcDate::parse(PRICE_BACKFILL_FROM).unwrap();
    let Some(last) = view.covering.keys().max().copied() else { return Ok(rep) };
    for m in &markets {
        for dd in from.0..=last.0 {
            let d = UtcDate(dd);
            let Some(&di) = view.covering.get(&d) else { continue };
            let ds = &s.dataset[di];
            rep.evaluated += 1;
            let tokens = view.rows.get(&(d, m.permaslug.clone())).copied();
            let mut checks: Vec<Check> = Vec::new();
            let mut inputs = vec![input_json("tokens", &ds.rec, ds.as_of_ms)];
            let mut hashes = vec![hex32(&ds.rec.sha256)];
            let mut status = DataStatus::NotReported;
            let mut reason: Option<String> = None;
            let mut value: Option<u128> = None;
            let mut price_json = Value::Null;
            let mut basis: Option<&str> = None;
            let fz = frozen.get(&m.permaslug);
            match tokens {
                None => reason = Some(format!("{} not in the documented top 50 for {d}", m.permaslug)),
                Some(t) => {
                    // token outlier vs up to 7 prior reported days
                    let mut prior: Vec<u128> = (1..=7).filter_map(|k| view.rows.get(&(UtcDate(d.0 - k), m.permaslug.clone())).copied()).collect();
                    prior.sort_unstable();
                    if prior.len() >= 3 {
                        let med = prior[prior.len() / 2];
                        if med > 0 && (t > med * 10 || t * 10 < med) {
                            checks.push(Check { code: "TOKENS_OUTLIER".into(), blocking: true, detail: format!("total {t} vs 7-day median {med}") });
                        }
                    }
                    match (fz, select_listed(m, &s, d)) {
                        (None, _) => {
                            status = DataStatus::Unpriced;
                            reason = Some("no frozen composition shares for this model in this formula version".into());
                        }
                        (_, None) => {
                            status = DataStatus::Unpriced;
                            reason = Some("no documented price snapshot for this day".into());
                        }
                        (Some(f), Some((lp, b, rec, as_of))) => {
                            let (v, p_in, p_blend) = value_micro(t, f, &lp);
                            inputs.push(input_json("prices", &rec, as_of));
                            hashes.push(hex32(&rec.sha256));
                            basis = Some(b);
                            price_json = json!({
                                "basis": b, "source": lp.source, "providers": lp.providers, "as_of": fmt_ms(as_of),
                                "listed_in_usd_per_m": fmt_scaled(lp.l_in, PRICE_SCALE), "listed_out_usd_per_m": fmt_scaled(lp.l_out, PRICE_SCALE),
                                "listed_cache_read_usd_per_m": fmt_scaled(lp.l_cr, PRICE_SCALE),
                                "p_in_usd_per_m": fmt_scaled(p_in, PRICE_SCALE), "p_blend_usd_per_m": fmt_scaled(p_blend, PRICE_SCALE),
                            });
                            if v <= u64::MAX as u128 {
                                status = DataStatus::Ok;
                                value = Some(v);
                            } else {
                                status = DataStatus::Unpriced;
                                reason = Some("value overflow".into());
                            }
                        }
                    }
                }
            }
            let blocked = checks.iter().any(|c| c.blocking);
            let fp = json!({"data_status": status.name(), "value_micro": value.map(|v| v.to_string()), "tokens": tokens.map(|t| t.to_string()), "basis": basis, "price": price_json["p_blend_usd_per_m"], "blocked": blocked});
            let key = (m.permaslug.clone(), d.to_string());
            let prev = latest.get(&key);
            if prev.map(|r| r["fingerprint"] == fp).unwrap_or(false) {
                rep.unchanged += 1;
                continue;
            }
            let (rev, fin) = match prev {
                None => (1u32, Finality::Final),
                Some(r) => (r["revision"].as_u64().unwrap_or(0) as u32 + 1, Finality::Corrected),
            };
            let prev_hash = prev.and_then(|r| r["print"]["printHash"].as_str()).map(hex32).unwrap_or([0; 32]);
            let pr = OraclePrint {
                index_id: index_id_lic(&m.permaslug),
                version: FORMULA_VERSION_LIC.into(),
                value_scale: USD_SCALE as u8,
                value: value.unwrap_or(0) as u64,
                source_start_ms: d.start_ms(),
                source_end_ms: d.end_ms(),
                observed_at_ms: ds.rec.fetched_at_ms,
                published_at_ms: now,
                valid_until_ms: d.end_ms() + VALID_FOR_MS,
                source_hash: zfx_oracle_print::source_set_hash(&hashes),
                formula_hash: formula_hash_lic(),
                status: if status == DataStatus::Ok { OracleStatus::Fresh } else { OracleStatus::Unavailable },
                data_status: status,
                finality: fin,
                revision: rev,
                prev_print_hash: prev_hash,
            };
            let print = if blocked {
                Value::Null
            } else {
                let bytes = pr.encode().expect("bounded");
                let (pk, sig) = zfx_oracle_print::sign(&bytes, &signer.seed);
                print_json(&pr, &[(pk, sig)])
            };
            let mut rec = json!({
                "index": "LIC",
                "market": m.permaslug,
                "index_id": index_id_lic(&m.permaslug),
                "formula_version": FORMULA_VERSION_LIC,
                "formula_hash": hex::encode(formula_hash_lic()),
                "mode": MODE_LIC,
                "license": "tokens: OpenRouter Datasets API, CC BY 4.0; prices: OpenRouter documented models API",
                "utc_date": d.to_string(),
                "source_start": fmt_ms(d.start_ms()),
                "source_end": fmt_ms(d.end_ms()),
                "revision": rev,
                "revision_id": format!("{d}.r{rev}"),
                "prev_revision_id": prev.map(|r| r["revision_id"].clone()),
                "finality": fin.name(),
                "data_status": status.name(),
                "reason": reason,
                "value_usd": value.map(|v| fmt_scaled(v, USD_SCALE)),
                "value_micro_usd": value.map(|v| v.to_string()),
                "confidence": match (status, basis) { (DataStatus::Ok, Some("LISTED_SNAPSHOT")) => "MEDIUM", (DataStatus::Ok, _) => "LOW", _ => "NONE" },
                "total_tokens": tokens.map(|t| t.to_string()),
                "frozen": fz.map(|f| json!({"w_in": fmt_ppm(f.w_in_ppm), "billed_share": fmt_ppm(f.billed_ppm), "cache_share": fmt_ppm(f.cache_ppm), "derived_days": f.days})),
                "price": price_json,
                "checks": checks,
                "blocked": blocked,
                "inputs": inputs,
                "observed_at": fmt_ms(ds.rec.fetched_at_ms),
                "source_as_of": fmt_ms(ds.as_of_ms),
                "created_at": fmt_ms(now),
                "attribution": engine::attribution(ds.as_of_ms),
                "fingerprint": fp,
                "print": print,
            });
            if let (Some(p0), Finality::Corrected) = (prev, fin) {
                let to = rec["fingerprint"].clone();
                rec.as_object_mut().unwrap().insert("correction".into(), json!({"from": p0["fingerprint"], "to": to}));
            }
            l.append_json(REVISIONS_LIC, &mut rec)?;
            if blocked {
                rep.blocked += 1;
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
    fn value_formula_exact() {
        // 100M tokens, 90% billed, w_in 0.9, cache 50%, L_in $1, L_cr $0.10, L_out $2
        let f = Frozen { w_in_ppm: 900_000, billed_ppm: 900_000, cache_ppm: 500_000, days: 30 };
        let l = Listed { l_in: 1_000_000_000, l_out: 2_000_000_000, l_cr: 100_000_000, providers: 1, source: "t" };
        let (v, p_in, p_blend) = value_micro(100_000_000, &f, &l);
        assert_eq!(p_in, 550_000_000); // 0.5*1 + 0.5*0.1
        assert_eq!(p_blend, 695_000_000); // 0.9*0.55 + 0.1*2
        assert_eq!(v, 62_550_000); // 90M * 0.695 / 1M = $62.55
    }

    #[test]
    fn lower_median_is_deterministic() {
        assert_eq!(lower_median(vec![3, 1, 2]), Some(2));
        assert_eq!(lower_median(vec![4, 1, 2, 3]), Some(2));
        assert_eq!(lower_median(vec![]), None);
    }

    #[test]
    fn formula_text_constants() {
        let t = std::str::from_utf8(FORMULA_TEXT_LIC).unwrap();
        assert!(t.starts_with(FORMULA_VERSION_LIC));
        assert!(t.contains(&format!("mode = {MODE_LIC}")));
        assert!(t.contains(&format!("grace_hours = {}", GRACE_MS / 3_600_000)));
        assert!(t.contains(&format!("valid_for_hours = {}", VALID_FOR_MS / 3_600_000)));
        assert!(t.contains(&format!("price_backfill_from = {PRICE_BACKFILL_FROM}")));
        assert!(t.contains(&format!("{} .. {}", OVERLAP.0, OVERLAP.1)));
    }
}
