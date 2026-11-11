//! Index calculator, integrity checks and revision ledger.
//! The formula is frozen in `formula/ZFX-OR-MODEL-SPEND-USD-v1.txt`; this file
//! implements it. Constants here must match that text (tested).

use crate::fixed::{self, fmt_scaled, PRICE_SCALE, USD_SCALE};
use crate::ledger::{FetchRecord, Ledger, LedgerError};
use crate::source::{self, ActivityDoc, CatalogModel, EffectivePricingDoc, Kind, RankingsDayDoc, UsageRow};
use crate::time::{fmt_ms, UtcDate, DAY_MS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use zfx_oracle_print::{DataStatus, Finality, OraclePrint, OracleStatus};

pub const FORMULA_VERSION: &str = "ZFX-OR-MODEL-SPEND-USD-v1";
pub const FORMULA_TEXT: &[u8] = include_bytes!("../../../formula/ZFX-OR-MODEL-SPEND-USD-v1.txt");
pub const MODE: &str = "ESTIMATED_OBSERVED_MIX";
pub const ATTRIBUTION_PREFIX: &str = "Source: OpenRouter (openrouter.ai/rankings), as of";

#[derive(Clone, Debug)]
pub struct Params {
    pub grace_ms: i64,
    pub close_window_ms: i64,
    pub valid_for_ms: i64,
    pub activity_window_days: i64,
    pub top_n: usize,
}
impl Default for Params {
    fn default() -> Self {
        Params {
            grace_ms: 6 * 3_600_000,
            close_window_ms: 60 * 60_000,
            valid_for_ms: 48 * 3_600_000,
            activity_window_days: 31,
            top_n: 50,
        }
    }
}

pub fn formula_hash() -> [u8; 32] {
    zfx_oracle_print::sha256(FORMULA_TEXT)
}

pub fn index_id(permaslug: &str) -> String {
    format!("or-model-spend/{permaslug}")
}

pub fn attribution(as_of_ms: i64) -> String {
    format!("{ATTRIBUTION_PREFIX} {}", fmt_ms(as_of_ms))
}

// ---------------------------------------------------------------------------
// Markets

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Market {
    pub permaslug: String,
    pub model_id: String,
    pub name: String,
    pub author: String,
    pub hugging_face_id: Option<String>,
    pub modality: Option<String>,
    pub created_s: Option<i64>,
    pub listed_at_ms: i64,
    pub listing_day: String,
    pub listing_rank: u32,
    pub listing_tokens: String,
}

impl Market {
    /// "open_weights" is only claimed when the catalog links a Hugging Face repo.
    pub fn weights(&self) -> &'static str {
        if self.hugging_face_id.is_some() {
            "open (Hugging Face id listed in OpenRouter catalog)"
        } else {
            "not listed as open (no Hugging Face id in OpenRouter catalog)"
        }
    }
}

pub fn load_markets(l: &Ledger) -> Result<Vec<Market>, LedgerError> {
    let mut out: Vec<Market> = Vec::new();
    for v in l.read_json("markets.jsonl")? {
        if v.get("event").and_then(|e| e.as_str()) == Some("LIST") {
            if let Ok(m) = serde_json::from_value::<Market>(v["market"].clone()) {
                if !out.iter().any(|x| x.permaslug == m.permaslug) {
                    out.push(m);
                }
            }
        }
    }
    Ok(out)
}

/// Markets still tracked: listed, and not retired since their last listing.
/// `markets.jsonl` holds LIST and RETIRE events; the last event per model decides.
pub fn load_active_markets(l: &Ledger) -> Result<Vec<Market>, LedgerError> {
    let mut active: BTreeMap<String, bool> = BTreeMap::new();
    for v in l.read_json("markets.jsonl")? {
        match v.get("event").and_then(|e| e.as_str()) {
            Some("LIST") => {
                if let Some(p) = v["market"]["permaslug"].as_str() {
                    active.insert(p.to_string(), true);
                }
            }
            Some("RETIRE") => {
                if let Some(p) = v["permaslug"].as_str() {
                    active.insert(p.to_string(), false);
                }
            }
            _ => {}
        }
    }
    Ok(load_markets(l)?.into_iter().filter(|m| active.get(&m.permaslug).copied().unwrap_or(true)).collect())
}

/// Catalog entries by canonical slug. Several ids can share one (e.g. "x" and "x:batch"); the plain id wins.
pub fn catalog_by_canon(catalog: &[CatalogModel]) -> BTreeMap<&str, &CatalogModel> {
    let mut by_canon: BTreeMap<&str, &CatalogModel> = BTreeMap::new();
    for c in catalog {
        let replace = match by_canon.get(c.canonical_slug.as_str()) {
            None => true,
            Some(prev) => prev.id.contains(':') && !c.id.contains(':'),
        };
        if replace {
            by_canon.insert(c.canonical_slug.as_str(), c);
        }
    }
    by_canon
}

fn trackable(c: &CatalogModel) -> bool {
    let priced = c.listed_in.unwrap_or(0) > 0 || c.listed_out.unwrap_or(0) > 0;
    let text_out = c.modality.as_deref().map(|m| m.contains("->text")).unwrap_or(true);
    priced && text_out
}

/// Models to track from the documented daily dataset: every priced text model in its newest
/// day (that day's top 50), ranked by tokens. This is how new models enter the universe.
pub fn select_from_dataset(doc: &source::RankingsDailyDoc, catalog: &[CatalogModel], now: i64) -> Vec<Market> {
    let Some(day) = doc.rows.iter().map(|r| r.date).max() else { return Vec::new() };
    let by_canon = catalog_by_canon(catalog);
    let mut rows: Vec<&source::DailyRow> = doc.rows.iter().filter(|r| r.date == day).collect();
    rows.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens).then(a.permaslug.cmp(&b.permaslug)));
    let mut out = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let Some(c) = by_canon.get(r.permaslug.as_str()) else { continue };
        if !trackable(c) {
            continue;
        }
        out.push(Market {
            permaslug: r.permaslug.clone(),
            model_id: c.id.clone(),
            name: c.name.clone(),
            author: c.author.clone(),
            hugging_face_id: c.hugging_face_id.clone(),
            modality: c.modality.clone(),
            created_s: c.created_s,
            listed_at_ms: now,
            listing_day: day.to_string(),
            listing_rank: i as u32 + 1,
            listing_tokens: r.total_tokens.to_string(),
        });
    }
    out
}

/// Ranks rows by total tokens, descending, across all variants (1-based).
pub fn ranks(doc: &RankingsDayDoc) -> BTreeMap<(String, String), u32> {
    let mut v: Vec<&UsageRow> = doc.rows.iter().map(|r| &r.usage).collect();
    v.sort_by(|a, b| b.total_tokens().cmp(&a.total_tokens()).then(a.permaslug.cmp(&b.permaslug)));
    v.iter()
        .enumerate()
        .map(|(i, r)| ((r.permaslug.clone(), r.variant.clone()), i as u32 + 1))
        .collect()
}

/// Selects the market universe: top-N standard-variant text models with a
/// non-zero listed price in the documented catalog.
pub fn select_universe(doc: &RankingsDayDoc, catalog: &[CatalogModel], top_n: usize, now: i64) -> Vec<Market> {
    let rk = ranks(doc);
    let by_canon = catalog_by_canon(catalog);
    let mut rows: Vec<&UsageRow> = doc.rows.iter().map(|r| &r.usage).filter(|u| u.variant == "standard").collect();
    rows.sort_by(|a, b| b.total_tokens().cmp(&a.total_tokens()).then(a.permaslug.cmp(&b.permaslug)));
    let mut out = Vec::new();
    for u in rows {
        if out.len() >= top_n {
            break;
        }
        let Some(c) = by_canon.get(u.permaslug.as_str()) else { continue };
        if !trackable(c) {
            continue;
        }
        out.push(Market {
            permaslug: u.permaslug.clone(),
            model_id: c.id.clone(),
            name: c.name.clone(),
            author: c.author.clone(),
            hugging_face_id: c.hugging_face_id.clone(),
            modality: c.modality.clone(),
            created_s: c.created_s,
            listed_at_ms: now,
            listing_day: doc.date.to_string(),
            listing_rank: rk[&(u.permaslug.clone(), u.variant.clone())],
            listing_tokens: u.total_tokens().to_string(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Loaded source state

pub struct Parsed<T> {
    pub rec: FetchRecord,
    pub doc: T,
    /// Source-side timestamp: cachedAt, else Last-Modified, else fetch time.
    pub as_of_ms: i64,
}

#[derive(Default)]
pub struct Loaded {
    pub activity: BTreeMap<String, Vec<Parsed<ActivityDoc>>>,
    pub pricing: BTreeMap<String, Vec<Parsed<EffectivePricingDoc>>>,
    pub rankings: Vec<Parsed<RankingsDayDoc>>,
    pub spend: Vec<Parsed<RankingsDayDoc>>,
    pub catalog: Option<Parsed<Vec<CatalogModel>>>,
    pub endpoints: BTreeMap<String, Parsed<Vec<source::ListedEndpoint>>>,
    /// Documented `datasets/rankings-daily` responses, fetch order.
    pub dataset: Vec<Parsed<source::RankingsDailyDoc>>,
    /// Every documented endpoints listing per model id, fetch order (price snapshots).
    pub endpoint_history: BTreeMap<String, Vec<Parsed<Vec<source::ListedEndpoint>>>>,
    /// Every documented catalog response, fetch order.
    pub catalog_history: Vec<Parsed<Vec<CatalogModel>>>,
    /// (kind, subject) -> error of the newest response, if the newest failed validation.
    pub newest_failed: BTreeMap<(String, String), (FetchRecord, String)>,
    pub parse_errors: Vec<(FetchRecord, String)>,
    pub fetches: Vec<FetchRecord>,
}

pub fn load(l: &Ledger) -> Result<Loaded, LedgerError> {
    let fetches = l.fetches()?;
    let mut s = Loaded::default();
    for f in &fetches {
        let Some(kind) = Kind::from_name(&f.kind) else { continue };
        if f.status != 200 || f.sha256.is_empty() {
            continue;
        }
        let bytes = l.get_blob(&f.sha256)?;
        let lm = source::last_modified_ms(&f.headers);
        let key = (f.kind.clone(), f.subject.clone());
        let mut fail = |e: String| {
            s.parse_errors.push((f.clone(), e.clone()));
            s.newest_failed.insert(key.clone(), (f.clone(), e));
        };
        macro_rules! ok {
            () => {
                s.newest_failed.remove(&key)
            };
        }
        match kind {
            Kind::ModelActivity => match source::parse_model_activity(&bytes) {
                Ok(doc) => {
                    ok!();
                    let as_of = doc.cached_at_ms.or(lm).unwrap_or(f.fetched_at_ms);
                    s.activity.entry(f.subject.clone()).or_default().push(Parsed { rec: f.clone(), doc, as_of_ms: as_of });
                }
                Err(e) => fail(e.to_string()),
            },
            Kind::EffectivePricing => match source::parse_effective_pricing(&bytes) {
                Ok(doc) => {
                    ok!();
                    s.pricing.entry(f.subject.clone()).or_default().push(Parsed { rec: f.clone(), doc, as_of_ms: lm.unwrap_or(f.fetched_at_ms) });
                }
                Err(e) => fail(e.to_string()),
            },
            Kind::RankingsDay | Kind::RankingsDaySpend => match source::parse_rankings_day(&bytes) {
                Ok(doc) => {
                    ok!();
                    let p = Parsed { rec: f.clone(), doc, as_of_ms: lm.unwrap_or(f.fetched_at_ms) };
                    if kind == Kind::RankingsDay {
                        s.rankings.push(p)
                    } else {
                        s.spend.push(p)
                    }
                }
                Err(e) => fail(e.to_string()),
            },
            Kind::CatalogModels => match source::parse_catalog(&bytes) {
                Ok(doc) => {
                    ok!();
                    let as_of = lm.unwrap_or(f.fetched_at_ms);
                    s.catalog_history.push(Parsed { rec: f.clone(), doc: doc.clone(), as_of_ms: as_of });
                    s.catalog = Some(Parsed { rec: f.clone(), doc, as_of_ms: as_of });
                }
                Err(e) => fail(e.to_string()),
            },
            Kind::ModelEndpoints => match source::parse_model_endpoints(&bytes) {
                Ok(doc) => {
                    ok!();
                    let as_of = lm.unwrap_or(f.fetched_at_ms);
                    s.endpoint_history.entry(f.subject.clone()).or_default().push(Parsed { rec: f.clone(), doc: doc.clone(), as_of_ms: as_of });
                    s.endpoints.insert(f.subject.clone(), Parsed { rec: f.clone(), doc, as_of_ms: as_of });
                }
                Err(e) => fail(e.to_string()),
            },
            Kind::RankingsDailyApi => match source::parse_rankings_daily_api(&bytes) {
                Ok(doc) => {
                    ok!();
                    let as_of = doc.as_of_ms;
                    s.dataset.push(Parsed { rec: f.clone(), doc, as_of_ms: as_of });
                }
                Err(e) => fail(e.to_string()),
            },
        }
    }
    s.fetches = fetches;
    Ok(s)
}

// ---------------------------------------------------------------------------
// Per-day computation

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Check {
    pub code: String,
    pub blocking: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct InputRef {
    pub role: String,
    pub kind: String,
    pub documented: bool,
    pub fetch_seq: u64,
    pub url: String,
    pub sha256: String,
    pub fetched_at: String,
    pub source_as_of: String,
}

fn input_ref<T>(role: &str, p: &Parsed<T>) -> InputRef {
    InputRef {
        role: role.into(),
        kind: p.rec.kind.clone(),
        documented: p.rec.documented,
        fetch_seq: p.rec.seq,
        url: p.rec.url.clone(),
        sha256: p.rec.sha256.clone(),
        fetched_at: fmt_ms(p.rec.fetched_at_ms),
        source_as_of: fmt_ms(p.as_of_ms),
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EndpointUse {
    pub endpoint_id: String,
    pub provider_slug: String,
    pub weight_tokens: String,
    pub p_in: Option<String>,
    pub p_out: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PriceSel {
    pub basis: &'static str,
    pub p_in: u128,
    pub p_out: u128,
    pub as_of_ms: i64,
    pub endpoints: Vec<EndpointUse>,
    pub weight_coverage_ppm: Option<u128>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Computed {
    pub market: String,
    pub date: UtcDate,
    pub data_status: DataStatus,
    pub value_micro: Option<u128>,
    pub tokens: Option<UsageRow>,
    pub price: Option<PriceSel>,
    pub inputs: Vec<InputRef>,
    pub value_inputs: Vec<String>,
    pub checks: Vec<Check>,
    pub reason: Option<String>,
    pub observed_at_ms: i64,
    pub source_as_of_ms: i64,
    pub rank: Option<u32>,
}

impl Computed {
    pub fn blocked(&self) -> bool {
        self.checks.iter().any(|c| c.blocking)
    }
    pub fn confidence(&self) -> &'static str {
        match (&self.data_status, &self.price) {
            (DataStatus::Ok, Some(p)) if p.basis == "CLOSE_TRAILING_24H" => "MEDIUM",
            (DataStatus::Ok, _) => "LOW",
            _ => "NONE",
        }
    }
    /// What must differ for a new revision to be written.
    pub fn fingerprint(&self) -> Value {
        json!({
            "data_status": self.data_status.name(),
            "value_micro": self.value_micro.map(|v| v.to_string()),
            "prompt": self.tokens.as_ref().map(|t| t.prompt_tokens.to_string()),
            "completion": self.tokens.as_ref().map(|t| t.completion_tokens.to_string()),
            "p_in": self.price.as_ref().map(|p| p.p_in.to_string()),
            "p_out": self.price.as_ref().map(|p| p.p_out.to_string()),
            "basis": self.price.as_ref().map(|p| p.basis),
            "blocked": self.blocked(),
        })
    }
}

fn activity_covers(p: &Parsed<ActivityDoc>, d: UtcDate, params: &Params) -> bool {
    let as_of_day = UtcDate::of_ms(p.as_of_ms);
    d < as_of_day && d.0 >= as_of_day.0 - (params.activity_window_days - 1)
}

fn usable_for(as_of_ms: i64, d: UtcDate, params: &Params) -> bool {
    as_of_ms >= d.end_ms() + params.grace_ms
}

/// Price selection for day d (formula section 4).
pub fn select_price(pricing: &[Parsed<EffectivePricingDoc>], d: UtcDate, params: &Params) -> (Option<PriceSel>, Option<usize>, Option<String>) {
    // (A) close capture
    let mut best: Option<usize> = None;
    for (i, p) in pricing.iter().enumerate() {
        let lm = p.as_of_ms;
        if lm >= d.end_ms() - params.close_window_ms && lm < d.end_ms() && p.doc.weighted_in.is_some() && p.doc.weighted_out.is_some() {
            if best.map(|b| pricing[b].as_of_ms <= lm).unwrap_or(true) {
                best = Some(i);
            }
        }
    }
    if let Some(i) = best {
        let p = &pricing[i];
        return (
            Some(PriceSel {
                basis: "CLOSE_TRAILING_24H",
                p_in: p.doc.weighted_in.unwrap(),
                p_out: p.doc.weighted_out.unwrap(),
                as_of_ms: p.as_of_ms,
                endpoints: p
                    .doc
                    .providers
                    .iter()
                    .map(|e| EndpointUse {
                        endpoint_id: e.endpoint_id.clone(),
                        provider_slug: e.provider_slug.clone(),
                        weight_tokens: e.total_tokens.to_string(),
                        p_in: e.eff_in.map(|x| fmt_scaled(x, PRICE_SCALE)),
                        p_out: e.eff_out.map(|x| fmt_scaled(x, PRICE_SCALE)),
                    })
                    .collect(),
                weight_coverage_ppm: None,
            }),
            Some(i),
            None,
        );
    }
    // (B) first usable response containing day d
    for (i, p) in pricing.iter().enumerate() {
        if !usable_for(p.as_of_ms, d, params) {
            continue;
        }
        let (Some(ins), Some(outs)) = (p.doc.input_by_day.get(&d), p.doc.output_by_day.get(&d)) else { continue };
        let weights: BTreeMap<&str, (u128, &str)> =
            p.doc.providers.iter().map(|e| (e.endpoint_id.as_str(), (e.total_tokens, e.provider_slug.as_str()))).collect();
        let total_w: u128 = weights.values().map(|w| w.0).sum();
        let mut pin = Vec::new();
        let mut pout = Vec::new();
        let mut used = Vec::new();
        let mut covered: u128 = 0;
        let eps: BTreeSet<&String> = ins.keys().chain(outs.keys()).collect();
        for ep in eps {
            let w = weights.get(ep.as_str()).map(|w| w.0).unwrap_or(0);
            if w == 0 {
                continue;
            }
            let (a, b) = (ins.get(ep).copied(), outs.get(ep).copied());
            if let Some(a) = a {
                pin.push((w, a));
            }
            if let Some(b) = b {
                pout.push((w, b));
            }
            if a.is_some() || b.is_some() {
                covered += w;
            }
            used.push(EndpointUse {
                endpoint_id: ep.clone(),
                provider_slug: weights.get(ep.as_str()).map(|w| w.1.to_string()).unwrap_or_default(),
                weight_tokens: w.to_string(),
                p_in: a.map(|x| fmt_scaled(x, PRICE_SCALE)),
                p_out: b.map(|x| fmt_scaled(x, PRICE_SCALE)),
            });
        }
        let (Some(p_in), Some(p_out)) = (fixed::weighted_price(&pin), fixed::weighted_price(&pout)) else {
            return (None, Some(i), Some("no weighted endpoint prices for this day".into()));
        };
        return (
            Some(PriceSel {
                basis: "DAILY_ENDPOINT_REWEIGHTED",
                p_in,
                p_out,
                as_of_ms: p.as_of_ms,
                endpoints: used,
                weight_coverage_ppm: if total_w > 0 { Some(covered * 1_000_000 / total_w) } else { None },
            }),
            Some(i),
            None,
        );
    }
    (None, None, Some("no close observation and no usable daily price chart".into()))
}

pub fn compute_day(m: &Market, d: UtcDate, s: &Loaded, params: &Params) -> Option<Computed> {
    let acts = s.activity.get(&m.permaslug)?;
    let tok = acts.iter().rev().find(|p| usable_for(p.as_of_ms, d, params) && activity_covers(p, d, params))?;
    let mut checks = Vec::new();
    let mut inputs = vec![input_ref("tokens", tok)];
    let mut value_inputs = vec![tok.rec.sha256.clone()];
    let row = tok.doc.rows.iter().find(|r| r.date == d && r.variant == "standard" && r.permaslug == m.permaslug).cloned();

    // a row dated on/after the response's own as-of day would be partial; never used (activity_covers)
    let rank = s.rankings.iter().rev().find(|r| r.doc.date == d).map(|r| {
        inputs.push(input_ref("rankings_check", r));
        ranks(&r.doc).get(&(m.permaslug.clone(), "standard".to_string())).copied()
    });
    let rank = rank.flatten();

    let mut out = Computed {
        market: m.permaslug.clone(),
        date: d,
        data_status: DataStatus::NotReported,
        value_micro: None,
        tokens: row.clone(),
        price: None,
        inputs: vec![],
        value_inputs: vec![],
        checks: vec![],
        reason: None,
        observed_at_ms: tok.rec.fetched_at_ms,
        source_as_of_ms: tok.as_of_ms,
        rank,
    };
    let Some(row) = row else {
        out.reason = Some(format!("no row for {d} in model-activity response #{} covering {}..{}", tok.rec.seq, UtcDate(UtcDate::of_ms(tok.as_of_ms).0 - (params.activity_window_days - 1)), UtcDate::of_ms(tok.as_of_ms).pred()));
        out.inputs = inputs;
        out.value_inputs = value_inputs;
        out.checks = checks;
        return Some(out);
    };

    // rankings reconciliation and top-N membership
    if let Some(r) = s.rankings.iter().rev().find(|r| r.doc.date == d) {
        if let Some(rr) = r.doc.rows.iter().find(|x| x.usage.permaslug == m.permaslug && x.usage.variant == "standard") {
            let (a, b) = (rr.usage.total_tokens(), row.total_tokens());
            let diff = a.abs_diff(b);
            if diff * 100 > a.max(b) {
                checks.push(Check { code: "RECONCILE_MISMATCH".into(), blocking: false, detail: format!("rankings day total {a} vs model-activity {b}") });
            }
        }
        match rank {
            Some(k) if k as usize <= params.top_n => {}
            Some(k) => checks.push(Check { code: "TOP_N_DROPOUT".into(), blocking: false, detail: format!("rank {k} > top {}", params.top_n) }),
            None => checks.push(Check { code: "TOP_N_DROPOUT".into(), blocking: false, detail: "absent from rankings day response".into() }),
        }
    }

    // token outlier vs median of up to 7 prior days in the same response
    let mut prior: Vec<u128> = tok
        .doc
        .rows
        .iter()
        .filter(|r| r.variant == "standard" && r.date < d && r.date.0 >= d.0 - 7)
        .map(|r| r.total_tokens())
        .collect();
    prior.sort_unstable();
    if prior.len() >= 3 {
        let med = prior[prior.len() / 2];
        let t = row.total_tokens();
        if med > 0 && (t > med * 10 || t * 10 < med) {
            checks.push(Check { code: "TOKENS_OUTLIER".into(), blocking: true, detail: format!("total {t} vs 7-day median {med}") });
        }
    }

    let empty = Vec::new();
    let pricing = s.pricing.get(&m.permaslug).unwrap_or(&empty);
    let (sel, idx, why) = select_price(pricing, d, params);
    if let Some(i) = idx {
        inputs.push(input_ref("prices", &pricing[i]));
        value_inputs.push(pricing[i].rec.sha256.clone());
    }
    out.inputs = inputs;
    out.value_inputs = value_inputs;
    let Some(sel) = sel else {
        out.data_status = DataStatus::Unpriced;
        out.reason = why;
        out.checks = checks;
        return Some(out);
    };
    if sel.p_in == 0 && sel.p_out == 0 {
        out.data_status = DataStatus::Unpriced;
        out.reason = Some("effective input and output prices are both zero (free or promotional)".into());
        out.price = Some(sel);
        out.checks = checks;
        return Some(out);
    }
    if let Some((lo_in, hi_in, lo_out, hi_out, basis)) = listed_band(m, s) {
        let _ = lo_in;
        if hi_out > 0 && (sel.p_out * 4 < lo_out || sel.p_out > hi_out * 4) {
            checks.push(Check { code: "PRICE_OUTLIER".into(), blocking: true, detail: format!("effective output {} outside [{}/4, {}*4] listed ({basis})", fmt_scaled(sel.p_out, 9), fmt_scaled(lo_out, 9), fmt_scaled(hi_out, 9)) });
        }
        if hi_in > 0 && sel.p_in > hi_in * 4 {
            checks.push(Check { code: "PRICE_OUTLIER".into(), blocking: true, detail: format!("effective input {} above {}*4 listed ({basis})", fmt_scaled(sel.p_in, 9), fmt_scaled(hi_in, 9)) });
        }
    }
    if let Some(cat) = &s.catalog {
        if let Some(c) = cat.doc.iter().find(|c| c.id == m.model_id) {
            if c.canonical_slug != m.permaslug {
                checks.push(Check { code: "MODEL_ID_CHANGED".into(), blocking: false, detail: format!("catalog id {} now points to {}", c.id, c.canonical_slug) });
            }
        }
    }
    if let Some(cov) = sel.weight_coverage_ppm {
        if cov < 500_000 {
            checks.push(Check { code: "LOW_WEIGHT_COVERAGE".into(), blocking: false, detail: format!("endpoints present on day carry {} of current weights", fixed::fmt_ppm(cov)) });
        }
    }
    // BYOK tokens are billed by the upstream provider, not OpenRouter: excluded from the value.
    let (bp, bc) = billed_tokens(&row);
    let v = fixed::implied_spend_observed_mix(bp, bc, sel.p_in, sel.p_out);
    match v {
        Some(v) if v <= u64::MAX as u128 => {
            out.data_status = DataStatus::Ok;
            out.value_micro = Some(v);
        }
        _ => {
            out.data_status = DataStatus::Unpriced;
            out.reason = Some("value overflow".into());
        }
    }
    out.price = Some(sel);
    out.checks = checks;
    Some(out)
}

/// Days to evaluate for a market: from its first reported day to the newest day
/// any response covers, including gaps (gaps become NOT_REPORTED).
pub fn candidate_days(m: &Market, s: &Loaded, params: &Params) -> Vec<UtcDate> {
    let Some(acts) = s.activity.get(&m.permaslug) else { return vec![] };
    let first = acts.iter().flat_map(|p| p.doc.rows.iter().filter(|r| r.variant == "standard").map(|r| r.date)).min();
    let last = acts.iter().map(|p| UtcDate::of_ms(p.as_of_ms).pred()).max();
    let (Some(first), Some(last)) = (first, last) else { return vec![] };
    let _ = params;
    (first.0..=last.0).map(UtcDate).collect()
}

// ---------------------------------------------------------------------------
// Revisions

pub struct Signer {
    pub seed: [u8; 32],
}
impl Signer {
    pub fn pubkey(&self) -> [u8; 32] {
        zfx_oracle_print::public_key(&self.seed)
    }
}

pub fn latest_revisions(l: &Ledger) -> Result<BTreeMap<(String, String), Value>, LedgerError> {
    let mut m = BTreeMap::new();
    for v in l.read_json("revisions.jsonl")? {
        let k = (v["market"].as_str().unwrap_or("").to_string(), v["utc_date"].as_str().unwrap_or("").to_string());
        m.insert(k, v);
    }
    Ok(m)
}

pub fn hex32(s: &str) -> [u8; 32] {
    let mut a = [0u8; 32];
    if let Ok(b) = hex::decode(s) {
        if b.len() == 32 {
            a.copy_from_slice(&b);
        }
    }
    a
}

pub fn build_print(c: &Computed, revision: u32, prev_hash: [u8; 32], finality: Finality, published_at: i64, params: &Params) -> OraclePrint {
    let hashes: Vec<[u8; 32]> = c.value_inputs.iter().map(|h| hex32(h)).collect();
    OraclePrint {
        index_id: index_id(&c.market),
        version: FORMULA_VERSION.into(),
        value_scale: USD_SCALE as u8,
        value: c.value_micro.unwrap_or(0) as u64,
        source_start_ms: c.date.start_ms(),
        source_end_ms: c.date.end_ms(),
        observed_at_ms: c.observed_at_ms,
        published_at_ms: published_at,
        valid_until_ms: c.date.end_ms() + params.valid_for_ms,
        source_hash: zfx_oracle_print::source_set_hash(&hashes),
        formula_hash: formula_hash(),
        status: if c.data_status == DataStatus::Ok { OracleStatus::Fresh } else { OracleStatus::Unavailable },
        data_status: c.data_status,
        finality,
        revision,
        prev_print_hash: prev_hash,
    }
}

pub fn print_json(p: &OraclePrint, sigs: &[([u8; 32], [u8; 64])]) -> Value {
    let bytes = p.encode().expect("bounded strings");
    json!({
        "indexId": p.index_id,
        "version": p.version,
        "value": fmt_scaled(p.value as u128, p.value_scale as u32),
        "valueScale": p.value_scale,
        "valueInt": p.value.to_string(),
        "sourceStartUtc": fmt_ms(p.source_start_ms),
        "sourceEndUtc": fmt_ms(p.source_end_ms),
        "observedAt": fmt_ms(p.observed_at_ms),
        "publishedAt": fmt_ms(p.published_at_ms),
        "validUntil": fmt_ms(p.valid_until_ms),
        "sourceHash": hex::encode(p.source_hash),
        "formulaHash": hex::encode(p.formula_hash),
        "status": p.status.name(),
        "dataStatus": p.data_status.name(),
        "finality": p.finality.name(),
        "revision": p.revision,
        "prevPrintHash": hex::encode(p.prev_print_hash),
        "canonicalHex": hex::encode(&bytes),
        "printHash": hex::encode(zfx_oracle_print::sha256(&bytes)),
        "reporterSignatures": sigs.iter().map(|(pk, s)| format!("ed25519:{}:{}", hex::encode(pk), hex::encode(s))).collect::<Vec<_>>(),
    })
}

pub fn revision_record(c: &Computed, revision: u32, finality: Finality, prev: Option<&Value>, now: i64, signer: &Signer, params: &Params) -> Value {
    let prev_hash = prev
        .and_then(|p| p.get("print").and_then(|x| x.get("printHash")).and_then(|h| h.as_str()))
        .map(hex32)
        .unwrap_or([0u8; 32]);
    let p = build_print(c, revision, prev_hash, finality, now, params);
    let print = if c.blocked() {
        Value::Null
    } else {
        let bytes = p.encode().expect("bounded");
        let (pk, sig) = zfx_oracle_print::sign(&bytes, &signer.seed);
        print_json(&p, &[(pk, sig)])
    };
    let t = c.tokens.as_ref();
    let billed = t.map(billed_tokens);
    let mix_in = billed.and_then(|(p, c)| if p + c > 0 { Some(fixed::div_round(p * 1_000_000, p + c)) } else { None });
    let incl_byok = match (t, &c.price) {
        (Some(t), Some(p)) if c.data_status == DataStatus::Ok => fixed::implied_spend_observed_mix(t.prompt_tokens, t.completion_tokens, p.p_in, p.p_out),
        _ => None,
    };
    json!({
        "market": c.market,
        "index_id": index_id(&c.market),
        "formula_version": FORMULA_VERSION,
        "formula_hash": hex::encode(formula_hash()),
        "mode": MODE,
        "utc_date": c.date.to_string(),
        "source_start": fmt_ms(c.date.start_ms()),
        "source_end": fmt_ms(c.date.end_ms()),
        "revision": revision,
        "revision_id": format!("{}.r{}", c.date, revision),
        "prev_revision_id": prev.and_then(|p| p.get("revision_id").cloned()),
        "finality": finality.name(),
        "data_status": c.data_status.name(),
        "reason": c.reason,
        "value_usd": c.value_micro.map(|v| fmt_scaled(v, USD_SCALE)),
        "value_micro_usd": c.value_micro.map(|v| v.to_string()),
        "confidence": c.confidence(),
        "tokens": t.map(|t| json!({
            "prompt": t.prompt_tokens.to_string(),
            "completion": t.completion_tokens.to_string(),
            "total": t.total_tokens().to_string(),
            "cached_prompt": t.cached_tokens.to_string(),
            "reasoning": t.reasoning_tokens.to_string(),
            "requests": t.requests.to_string(),
            "byok_prompt": t.byok_prompt_tokens.to_string(),
            "byok_completion": t.byok_completion_tokens.to_string(),
            "billed_prompt": billed_tokens(t).0.to_string(),
            "billed_completion": billed_tokens(t).1.to_string(),
        })),
        "info_value_usd_incl_byok": incl_byok.map(|v| fmt_scaled(v, USD_SCALE)),
        "mix": mix_in.map(|w| json!({"basis": "OBSERVED", "tokens": "billed (excl. BYOK)", "w_in": fixed::fmt_ppm(w), "w_out": fixed::fmt_ppm(1_000_000 - w)})),
        "price": c.price.as_ref().map(|p| json!({
            "basis": p.basis,
            "p_in_usd_per_m": fmt_scaled(p.p_in, PRICE_SCALE),
            "p_out_usd_per_m": fmt_scaled(p.p_out, PRICE_SCALE),
            "as_of": fmt_ms(p.as_of_ms),
            "weight_coverage": p.weight_coverage_ppm.map(fixed::fmt_ppm),
            "endpoints": p.endpoints,
        })),
        "rank": c.rank,
        "checks": c.checks,
        "blocked": c.blocked(),
        "inputs": c.inputs,
        "value_input_hashes": c.value_inputs,
        "source_as_of": fmt_ms(c.source_as_of_ms),
        "observed_at": fmt_ms(c.observed_at_ms),
        "created_at": fmt_ms(now),
        "attribution": attribution(c.source_as_of_ms),
        "fingerprint": c.fingerprint(),
        "print": print,
    })
}

#[derive(Default, Debug, Serialize)]
pub struct EngineReport {
    /// Markets skipped because their newest input response failed validation
    /// (SOURCE_SHAPE). Nothing new is signed for them until a valid response arrives.
    pub held_markets: Vec<String>,
    pub evaluated: usize,
    pub new_final: usize,
    pub new_corrected: usize,
    pub blocked: usize,
    pub unchanged: usize,
}

pub fn run(l: &Ledger, signer: &Signer, params: &Params, now: i64) -> Result<EngineReport, LedgerError> {
    let s = load(l)?;
    let markets = load_markets(l)?;
    let mut latest = latest_revisions(l)?;
    let mut rep = EngineReport::default();
    for m in &markets {
        if let Some(why) = shape_hold(m, &s) {
            eprintln!("[engine] hold {}: {why}", m.permaslug);
            rep.held_markets.push(m.permaslug.clone());
            continue;
        }
        for d in candidate_days(m, &s, params) {
            let Some(c) = compute_day(m, d, &s, params) else { continue };
            rep.evaluated += 1;
            let key = (m.permaslug.clone(), d.to_string());
            let prev = latest.get(&key);
            if let Some(p) = prev {
                if p.get("fingerprint") == Some(&c.fingerprint()) {
                    rep.unchanged += 1;
                    continue;
                }
            }
            let (rev, fin) = match prev {
                None => (1, Finality::Final),
                Some(p) => (p["revision"].as_u64().unwrap_or(0) as u32 + 1, Finality::Corrected),
            };
            let mut rec = revision_record(&c, rev, fin, prev, now, signer, params);
            if let (Some(p), Finality::Corrected) = (prev, fin) {
                rec.as_object_mut().unwrap().insert("correction".into(), json!({"from": p["fingerprint"], "to": c.fingerprint()}));
            }
            l.append_json("revisions.jsonl", &mut rec)?;
            if c.blocked() {
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

pub fn day_count_between(a: UtcDate, b: UtcDate) -> i64 {
    b.0 - a.0
}

pub fn day_of(ms: i64) -> UtcDate {
    UtcDate(ms.div_euclid(DAY_MS))
}

/// SOURCE_SHAPE predicate: the newest token or price response for this market
/// failed validation, so the feed may have changed shape. Hold the market.
pub fn shape_hold(m: &Market, s: &Loaded) -> Option<String> {
    for kind in ["model_activity", "effective_pricing"] {
        if let Some((rec, e)) = s.newest_failed.get(&(kind.to_string(), m.permaslug.clone())) {
            return Some(format!("SOURCE_SHAPE: newest {kind} response #{} failed validation: {e}", rec.seq));
        }
    }
    None
}

/// Token lookup in the documented `datasets/rankings-daily` response (top 50 + `other`).
/// A model absent from day d is NOT_REPORTED, never zero. Day d is only accepted
/// when `meta.end_date >= d`.
pub fn daily_api_tokens(doc: &source::RankingsDailyDoc, permaslug: &str, d: UtcDate) -> Result<u128, DataStatus> {
    if doc.end_date < d || doc.start_date.map(|s| s > d).unwrap_or(false) {
        return Err(DataStatus::NotReported);
    }
    if permaslug == "other" {
        return Err(DataStatus::NotReported);
    }
    doc.rows
        .iter()
        .find(|r| r.date == d && r.permaslug == permaslug)
        .map(|r| r.total_tokens)
        .ok_or(DataStatus::NotReported)
}

/// Listed price band (min/max over providers) from the documented
/// `/api/v1/models/{id}/endpoints` response, else the catalog price.
/// Returns (min_in, max_in, min_out, max_out, basis) in nano-USD per M.
pub fn listed_band(m: &Market, s: &Loaded) -> Option<(u128, u128, u128, u128, &'static str)> {
    if let Some(e) = s.endpoints.get(&m.model_id) {
        let ins: Vec<u128> = e.doc.iter().filter_map(|x| x.listed_in).filter(|x| *x > 0).collect();
        let outs: Vec<u128> = e.doc.iter().filter_map(|x| x.listed_out).filter(|x| *x > 0).collect();
        if !outs.is_empty() {
            return Some((
                ins.iter().copied().min().unwrap_or(0),
                ins.iter().copied().max().unwrap_or(0),
                *outs.iter().min().unwrap(),
                *outs.iter().max().unwrap(),
                "documented endpoints listing",
            ));
        }
    }
    let cat = s.catalog.as_ref()?;
    let c = cat.doc.iter().find(|c| c.canonical_slug == m.permaslug && !c.id.contains(':'))?;
    let (i, o) = (c.listed_in.unwrap_or(0), c.listed_out.unwrap_or(0));
    Some((i, i, o, o, "documented catalog"))
}

/// Tokens billed through OpenRouter: total minus BYOK, per side.
pub fn billed_tokens(r: &UsageRow) -> (u128, u128) {
    (r.prompt_tokens.saturating_sub(r.byok_prompt_tokens), r.completion_tokens.saturating_sub(r.byok_completion_tokens))
}
