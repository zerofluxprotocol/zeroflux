//! Engine behaviour on synthetic ledgers: statuses, corrections, checks, prints.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use zfx_index::engine::{self, Market, Params, Signer};
use zfx_index::ledger::{FetchRecord, Ledger};
use zfx_index::source;
use zfx_index::time::{fmt_http_date, UtcDate};
use zfx_oracle_print::OraclePrint;

const SLUG: &str = "acme/model-x-20261001";
const H: i64 = 3_600_000;

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("zfx-eng-{name}-{}-{}", std::process::id(), zfx_index::time::now_ms()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn d(s: &str) -> UtcDate {
    UtcDate::parse(s).unwrap()
}

struct T {
    l: Ledger,
    root: PathBuf,
    signer: Signer,
    params: Params,
}

impl T {
    fn new(name: &str) -> T {
        let root = tmp(name);
        let l = Ledger::open_write(&root).unwrap();
        let m = Market {
            permaslug: SLUG.into(),
            model_id: "acme/model-x".into(),
            name: "Acme: Model X".into(),
            author: "acme".into(),
            hugging_face_id: None,
            modality: Some("text->text".into()),
            created_s: None,
            listed_at_ms: 0,
            listing_day: "2026-11-01".into(),
            listing_rank: 1,
            listing_tokens: "1".into(),
        };
        l.append_json("markets.jsonl", &mut json!({"event": "LIST", "market": m})).unwrap();
        T { l, root, signer: Signer { seed: [9u8; 32] }, params: Params::default() }
    }

    fn put(&self, kind: &str, subject: &str, body: &str, fetched_at: i64, lm: Option<i64>) {
        let sha = self.l.put_blob(body.as_bytes()).unwrap();
        let mut headers = BTreeMap::new();
        if let Some(lm) = lm {
            headers.insert("last-modified".to_string(), fmt_http_date(lm));
        }
        let mut r = FetchRecord {
            seq: self.l.next_fetch_seq().unwrap(),
            kind: kind.into(),
            subject: subject.into(),
            documented: false,
            method: "GET".into(),
            url: format!("https://test/{kind}"),
            status: 200,
            fetched_at_ms: fetched_at,
            headers,
            sha256: sha,
            len: body.len() as u64,
            client: "test".into(),
            error: None,
            chain: String::new(),
        };
        self.l.append_fetch(&mut r).unwrap();
    }

    /// rows: (date, prompt, completion)
    fn activity(&self, rows: &[(&str, u64, u64)], cached_at: i64) {
        let rs: Vec<Value> = rows
            .iter()
            .map(|(dt, p, c)| json!({"date": format!("{dt} 00:00:00"), "model_permaslug": SLUG, "variant": "standard", "total_prompt_tokens": p, "total_completion_tokens": c, "total_native_tokens_reasoning": 0, "count": 10, "total_native_tokens_cached": 0, "total_byok_prompt_tokens": 0, "total_byok_completion_tokens": 0}))
            .collect();
        let body = json!({"data": {"analytics": rs, "cachedAt": cached_at}}).to_string();
        self.put("model_activity", SLUG, &body, cached_at + 60_000, None);
    }

    /// Effective pricing with a summary (`w_in`, `w_out` as JSON number text) and a daily chart.
    fn pricing(&self, w_in: &str, w_out: &str, chart: &[(&str, &[(&str, &str, &str)])], providers: &[(&str, u64)], lm: i64) {
        let ps: Vec<Value> = providers.iter().map(|(e, t)| json!({"endpointId": e, "providerName": e, "providerSlug": e, "effectiveInputPrice": null, "effectiveOutputPrice": null, "totalTokens": t})).collect();
        let mut inc = Vec::new();
        let mut outc = Vec::new();
        for (dt, eps) in chart {
            let mut yi = serde_json::Map::new();
            let mut yo = serde_json::Map::new();
            for (e, pi, po) in eps.iter() {
                yi.insert(e.to_string(), serde_json::from_str(pi).unwrap());
                yo.insert(e.to_string(), serde_json::from_str(po).unwrap());
            }
            inc.push(json!({"x": format!("{dt} 00:00:00"), "y": yi}));
            outc.push(json!({"x": format!("{dt} 00:00:00"), "y": yo}));
        }
        let body = format!(
            r#"{{"data":{{"weightedInputPrice":{w_in},"weightedOutputPrice":{w_out},"weightedCacheHitRate":0,"providerSummaries":{},"inputChartData":{},"outputChartData":{}}}}}"#,
            Value::Array(ps),
            Value::Array(inc),
            Value::Array(outc)
        );
        self.put("effective_pricing", SLUG, &body, lm + 60_000, Some(lm));
    }

    fn run(&self, now: i64) -> engine::EngineReport {
        engine::run(&self.l, &self.signer, &self.params, now).unwrap()
    }

    fn revs(&self) -> Vec<Value> {
        self.l.read_json("revisions.jsonl").unwrap()
    }

    fn rev(&self, date: &str) -> Value {
        self.revs().into_iter().filter(|r| r["utc_date"] == date).last().unwrap_or(Value::Null)
    }
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn verify_print(p: &Value) -> OraclePrint {
    let bytes = hex::decode(p["canonicalHex"].as_str().unwrap()).unwrap();
    let sig = p["reporterSignatures"][0].as_str().unwrap();
    let parts: Vec<&str> = sig.split(':').collect();
    let pk: [u8; 32] = hex::decode(parts[1]).unwrap().try_into().unwrap();
    let sg: [u8; 64] = hex::decode(parts[2]).unwrap().try_into().unwrap();
    assert!(zfx_oracle_print::verify(&bytes, &pk, &sg));
    assert_eq!(hex::encode(zfx_oracle_print::sha256(&bytes)), p["printHash"].as_str().unwrap());
    OraclePrint::decode(&bytes).unwrap()
}

#[test]
fn close_capture_observed_mix_final_print() {
    let t = T::new("close");
    // day 2026-11-09: 90M prompt, 10M completion
    t.activity(&[("2026-11-08", 80_000_000, 10_000_000), ("2026-11-09", 90_000_000, 10_000_000), ("2026-11-10", 5, 5)], d("2026-11-10").start_ms() + 7 * H);
    // close observation at 23:40Z on 2026-11-09: $0.25 in, $2.00 out
    t.pricing("0.25", "2.00", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 20 * 60_000);
    let rep = t.run(d("2026-11-10").start_ms() + 8 * H);
    assert!(rep.new_final >= 1);
    let r = t.rev("2026-11-09");
    assert_eq!(r["data_status"], "OK");
    assert_eq!(r["finality"], "FINAL");
    // 90M * 0.25/M + 10M * 2/M = 22.5 + 20 = 42.50
    assert_eq!(r["value_usd"], "42.500000");
    assert_eq!(r["mix"]["w_in"], "0.900000");
    assert_eq!(r["price"]["basis"], "CLOSE_TRAILING_24H");
    assert_eq!(r["confidence"], "MEDIUM");
    let p = verify_print(&r["print"]);
    assert_eq!(p.value, 42_500_000);
    assert_eq!(p.source_start_ms, d("2026-11-09").start_ms());
    assert_eq!(p.source_end_ms, d("2026-11-10").start_ms());
    assert_eq!(p.formula_hash, engine::formula_hash());
    // the partial current day (2026-11-10) is never evaluated
    assert_eq!(t.rev("2026-11-10"), Value::Null);
    // idempotent: no new revisions on rerun
    let rep2 = t.run(d("2026-11-10").start_ms() + 9 * H);
    assert_eq!(rep2.new_final + rep2.new_corrected, 0);
}

#[test]
fn close_rule_is_at_or_before_end_of_day() {
    let t = T::new("closerule");
    t.activity(&[("2026-11-09", 1_000_000, 0)], d("2026-11-10").start_ms() + 7 * H);
    // observation exactly at 00:00:00Z of the next day belongs to the next day: not a close
    t.pricing("1", "1", &[], &[("e1", 1)], d("2026-11-10").start_ms());
    t.run(d("2026-11-10").start_ms() + 8 * H);
    let r = t.rev("2026-11-09");
    assert_eq!(r["data_status"], "UNPRICED");
    // and an observation older than the 60 minute window is not a close either
    let t2 = T::new("closerule2");
    t2.activity(&[("2026-11-09", 1_000_000, 0)], d("2026-11-10").start_ms() + 7 * H);
    t2.pricing("1", "1", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 61 * 60_000);
    t2.run(d("2026-11-10").start_ms() + 8 * H);
    assert_eq!(t2.rev("2026-11-09")["data_status"], "UNPRICED");
}

#[test]
fn reweighted_daily_fallback() {
    let t = T::new("reweight");
    t.activity(&[("2026-11-05", 1_000_000_000, 0)], d("2026-11-10").start_ms() + 7 * H);
    // endpoints e1 (weight 3) at $1.00/M and e2 (weight 1) at $2.00/M on 11-05 -> $1.25/M
    t.pricing("9", "9", &[("2026-11-05", &[("e1", "1.0", "4"), ("e2", "2.0", "4")])], &[("e1", 3), ("e2", 1), ("e3", 4)], d("2026-11-10").start_ms() + 7 * H);
    t.run(d("2026-11-10").start_ms() + 8 * H);
    let r = t.rev("2026-11-05");
    assert_eq!(r["price"]["basis"], "DAILY_ENDPOINT_REWEIGHTED");
    assert_eq!(r["price"]["p_in_usd_per_m"], "1.250000000");
    assert_eq!(r["value_usd"], "1250.000000");
    assert_eq!(r["confidence"], "LOW");
    // e1+e2 carry 4 of 8 weight units
    assert_eq!(r["price"]["weight_coverage"], "0.500000");
}

#[test]
fn gap_day_is_not_reported_not_zero() {
    let t = T::new("gap");
    t.activity(&[("2026-11-07", 100, 0), ("2026-11-09", 100, 0)], d("2026-11-10").start_ms() + 7 * H);
    t.pricing("1", "1", &[("2026-11-07", &[("e1", "1", "1")]), ("2026-11-08", &[("e1", "1", "1")]), ("2026-11-09", &[("e1", "1", "1")])], &[("e1", 1)], d("2026-11-10").start_ms() + 7 * H);
    t.run(d("2026-11-10").start_ms() + 8 * H);
    let r = t.rev("2026-11-08");
    assert_eq!(r["data_status"], "NOT_REPORTED");
    assert_eq!(r["value_usd"], Value::Null);
    let p = verify_print(&r["print"]);
    assert_eq!(p.status, zfx_oracle_print::OracleStatus::Unavailable);
    assert_eq!(p.data_status, zfx_oracle_print::DataStatus::NotReported);
    assert_eq!(p.value, 0);
    assert_eq!(t.rev("2026-11-09")["data_status"], "OK");
}

#[test]
fn unpriced_when_no_price_or_zero_price() {
    let t = T::new("unpriced");
    t.activity(&[("2026-11-09", 100, 10)], d("2026-11-10").start_ms() + 7 * H);
    t.run(d("2026-11-10").start_ms() + 8 * H);
    let r = t.rev("2026-11-09");
    assert_eq!(r["data_status"], "UNPRICED");
    assert_eq!(r["value_usd"], Value::Null);
    let t2 = T::new("zero");
    t2.activity(&[("2026-11-09", 100, 10)], d("2026-11-10").start_ms() + 7 * H);
    t2.pricing("0", "0", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 10 * 60_000);
    t2.run(d("2026-11-10").start_ms() + 8 * H);
    assert_eq!(t2.rev("2026-11-09")["data_status"], "UNPRICED");
}

#[test]
fn grace_window_delays_finality() {
    let t = T::new("grace");
    // cachedAt only 2 h after day end: inside the 6 h grace, so 11-09 is not evaluable yet
    t.activity(&[("2026-11-09", 100, 10)], d("2026-11-10").start_ms() + 2 * H);
    t.pricing("1", "1", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 10 * 60_000);
    t.run(d("2026-11-10").start_ms() + 3 * H);
    assert_eq!(t.rev("2026-11-09"), Value::Null);
    t.activity(&[("2026-11-09", 100, 10)], d("2026-11-10").start_ms() + 6 * H);
    t.run(d("2026-11-10").start_ms() + 7 * H);
    assert_eq!(t.rev("2026-11-09")["finality"], "FINAL");
}

#[test]
fn correction_creates_new_immutable_revision() {
    let t = T::new("correct");
    t.activity(&[("2026-11-09", 90_000_000, 10_000_000)], d("2026-11-10").start_ms() + 7 * H);
    t.pricing("0.25", "2.00", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 10 * 60_000);
    t.run(d("2026-11-10").start_ms() + 8 * H);
    let r1 = t.rev("2026-11-09");
    // a day later the source reports more tokens for 11-09
    t.activity(&[("2026-11-09", 92_000_000, 10_000_000), ("2026-11-10", 1, 1)], d("2026-11-11").start_ms() + 7 * H);
    let rep = t.run(d("2026-11-11").start_ms() + 8 * H);
    assert_eq!(rep.new_corrected, 1);
    let all: Vec<Value> = t.revs().into_iter().filter(|r| r["utc_date"] == "2026-11-09").collect();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0], r1, "first revision unchanged");
    let r2 = &all[1];
    assert_eq!(r2["finality"], "CORRECTED");
    assert_eq!(r2["revision"], 2);
    assert_eq!(r2["revision_id"], "2026-11-09.r2");
    assert_eq!(r2["prev_revision_id"], "2026-11-09.r1");
    assert_eq!(r2["value_usd"], "43.000000");
    let p2 = verify_print(&r2["print"]);
    assert_eq!(p2.finality, zfx_oracle_print::Finality::Corrected);
    assert_eq!(hex::encode(p2.prev_print_hash), r1["print"]["printHash"].as_str().unwrap());
    assert!(r2["correction"]["from"].is_object());
    // ledger chain still verifies
    t.l.verify().unwrap();
}

#[test]
fn token_outlier_blocks_signing() {
    let t = T::new("outlier");
    t.activity(
        &[("2026-11-04", 100, 0), ("2026-11-05", 110, 0), ("2026-11-06", 90, 0), ("2026-11-07", 100, 0), ("2026-11-08", 5000, 0)],
        d("2026-11-10").start_ms() + 7 * H,
    );
    let days: Vec<(&str, &[(&str, &str, &str)])> = ["2026-11-04", "2026-11-05", "2026-11-06", "2026-11-07", "2026-11-08"].iter().map(|x| (*x, &[("e1", "1", "1")][..])).collect();
    t.pricing("1", "1", &days, &[("e1", 1)], d("2026-11-10").start_ms() + 7 * H);
    let rep = t.run(d("2026-11-10").start_ms() + 8 * H);
    assert_eq!(rep.blocked, 1);
    let r = t.rev("2026-11-08");
    assert_eq!(r["blocked"], true);
    assert!(r["print"].is_null(), "blocked days are not signed");
    assert_eq!(r["checks"][0]["code"], "TOKENS_OUTLIER");
    assert!(!t.rev("2026-11-07")["print"].is_null());
}

#[test]
fn price_outlier_against_documented_catalog() {
    let t = T::new("priceout");
    t.activity(&[("2026-11-09", 100, 10)], d("2026-11-10").start_ms() + 7 * H);
    t.pricing("1", "200", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 10 * 60_000);
    let cat = json!({"data": [{"id": "acme/model-x", "canonical_slug": SLUG, "name": "Acme: Model X", "pricing": {"prompt": "0.000001", "completion": "0.000002"}}]}).to_string();
    t.put("catalog_models", "", &cat, d("2026-11-10").start_ms(), None);
    t.run(d("2026-11-10").start_ms() + 8 * H);
    let r = t.rev("2026-11-09");
    assert_eq!(r["blocked"], true);
    assert!(r["checks"].as_array().unwrap().iter().any(|c| c["code"] == "PRICE_OUTLIER"));
}

#[test]
fn malformed_newest_response_holds_market() {
    let t = T::new("shape");
    t.activity(&[("2026-11-09", 100, 10)], d("2026-11-10").start_ms() + 7 * H);
    t.pricing("1", "1", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 10 * 60_000);
    t.run(d("2026-11-10").start_ms() + 8 * H);
    assert_eq!(t.revs().len(), 1);
    // the source renames a field
    t.put("model_activity", SLUG, r#"{"data":{"analytics":[{"date":"2026-11-10 00:00:00","model":"x"}]}}"#, d("2026-11-11").start_ms() + 7 * H, None);
    let rep = t.run(d("2026-11-11").start_ms() + 8 * H);
    assert_eq!(rep.held_markets, vec![SLUG.to_string()]);
    assert_eq!(t.revs().len(), 1, "nothing new signed while the feed shape is unknown");
    let s = engine::load(&t.l).unwrap();
    let h = zfx_index::publish::source_health(&t.l, &s, &t.params, &Default::default(), d("2026-11-11").start_ms() + 8 * H).unwrap();
    assert_eq!(h["status"], "FAILING");
    assert!(h["failing_inputs"][0]["error"].as_str().unwrap().starts_with("SHAPE_CHANGED"));
}

#[test]
fn top_n_dropout_and_reconciliation() {
    let mut t = T::new("topn");
    t.params.top_n = 1;
    t.activity(&[("2026-11-09", 100, 10)], d("2026-11-10").start_ms() + 7 * H);
    t.pricing("1", "1", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 10 * 60_000);
    let row = |slug: &str, p: u64| json!({"date": "2026-11-09 00:00:00", "model_permaslug": slug, "variant": "standard", "total_prompt_tokens": p, "total_completion_tokens": 0, "total_native_tokens_reasoning": 0, "count": 1, "total_native_tokens_cached": 0, "total_byok_prompt_tokens": 0, "total_byok_completion_tokens": 0, "rankingMetricValue": p});
    let rk = json!({"data": [row("big/model", 1000), row(SLUG, 50)]}).to_string();
    t.put("rankings_day", "", &rk, d("2026-11-10").start_ms() + H, None);
    t.run(d("2026-11-10").start_ms() + 8 * H);
    let r = t.rev("2026-11-09");
    assert_eq!(r["data_status"], "OK", "dropping out of the top N is a lifecycle warning, data still reported");
    assert_eq!(r["rank"], 2);
    let codes: Vec<&str> = r["checks"].as_array().unwrap().iter().map(|c| c["code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"TOP_N_DROPOUT"));
    assert!(codes.contains(&"RECONCILE_MISMATCH"), "rankings 50 vs activity 110");
}

#[test]
fn documented_daily_dataset_missing_model_is_not_reported() {
    let b = br#"{"data":[{"date":"2026-11-09","model_permaslug":"a/x","total_tokens":"7"},{"date":"2026-11-09","model_permaslug":"other","total_tokens":"99"}],
      "meta":{"as_of":"2026-11-10T02:00:00.000Z","start_date":"2026-11-09","end_date":"2026-11-09","version":"v1"}}"#;
    let doc = source::parse_rankings_daily_api(b).unwrap();
    assert_eq!(engine::daily_api_tokens(&doc, "a/x", d("2026-11-09")), Ok(7));
    assert_eq!(engine::daily_api_tokens(&doc, "a/y", d("2026-11-09")), Err(zfx_oracle_print::DataStatus::NotReported));
    assert_eq!(engine::daily_api_tokens(&doc, "other", d("2026-11-09")), Err(zfx_oracle_print::DataStatus::NotReported));
    // end_date has not reached the day yet
    assert_eq!(engine::daily_api_tokens(&doc, "a/x", d("2026-11-10")), Err(zfx_oracle_print::DataStatus::NotReported));
}

#[test]
fn formula_text_matches_code_constants() {
    let txt = std::str::from_utf8(engine::FORMULA_TEXT).unwrap();
    let p = Params::default();
    assert!(txt.starts_with(engine::FORMULA_VERSION));
    assert!(txt.contains(&format!("grace_hours = {}", p.grace_ms / H)));
    assert!(txt.contains(&format!("close_window_minutes = {}", p.close_window_ms / 60_000)));
    assert!(txt.contains(&format!("valid_for_hours = {}", p.valid_for_ms / H)));
    assert!(txt.contains(&format!("activity_window_days = {}", p.activity_window_days)));
    assert!(txt.contains(&format!("mode = {}", engine::MODE)));
}

#[test]
fn byok_tokens_are_excluded_from_value_but_published() {
    let t = T::new("byok");
    let body = json!({"data": {"analytics": [{"date": "2026-11-09 00:00:00", "model_permaslug": SLUG, "variant": "standard",
        "total_prompt_tokens": 100_000_000u64, "total_completion_tokens": 10_000_000u64, "total_native_tokens_reasoning": 0, "count": 1,
        "total_native_tokens_cached": 0, "total_byok_prompt_tokens": 10_000_000u64, "total_byok_completion_tokens": 0}],
        "cachedAt": d("2026-11-10").start_ms() + 7 * H}}).to_string();
    t.put("model_activity", SLUG, &body, d("2026-11-10").start_ms() + 7 * H, None);
    t.pricing("0.25", "2.00", &[], &[("e1", 1)], d("2026-11-09").end_ms() - 10 * 60_000);
    t.run(d("2026-11-10").start_ms() + 8 * H);
    let r = t.rev("2026-11-09");
    // billed: 90M prompt, 10M completion -> $42.50; incl. BYOK would be $45.00
    assert_eq!(r["value_usd"], "42.500000");
    assert_eq!(r["info_value_usd_incl_byok"], "45.000000");
    assert_eq!(r["tokens"]["total"], "110000000");
    assert_eq!(r["tokens"]["billed_prompt"], "90000000");
    assert_eq!(r["mix"]["w_in"], "0.900000");
}
