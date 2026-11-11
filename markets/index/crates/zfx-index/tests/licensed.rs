//! Dataset index variant: tokens from the documented daily dataset, listed prices,
//! frozen mix, statuses, snapshot bases, corrections, and the 25% LIC-7D clamp.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use zfx_index::engine::{Market, Signer};
use zfx_index::ledger::{FetchRecord, Ledger};
use zfx_index::licensed::{self, frozen_params, value_micro, Listed};
use zfx_index::time::{fmt_http_date, UtcDate};
use zfx_index::weekly::{self, Spec7d};
use zfx_oracle_print::OraclePrint;

// a model that has frozen shares in LIC-v1.params.json
const SLUG: &str = "anthropic/claude-opus-5.5-20260921";
const ID: &str = "anthropic/claude-opus-5.5";
const H: i64 = 3_600_000;

fn d(s: &str) -> UtcDate {
    UtcDate::parse(s).unwrap()
}

struct T {
    l: Ledger,
    root: PathBuf,
    signer: Signer,
}
impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn t(name: &str, slug: &str, id: &str) -> T {
    let root = std::env::temp_dir().join(format!("zfx-lic-{name}-{}-{}", std::process::id(), zfx_index::time::now_ms()));
    let l = Ledger::open_write(&root).unwrap();
    let m = Market {
        permaslug: slug.into(),
        model_id: id.into(),
        name: "x".into(),
        author: "x".into(),
        hugging_face_id: None,
        modality: None,
        created_s: None,
        listed_at_ms: 0,
        listing_day: "2026-11-01".into(),
        listing_rank: 1,
        listing_tokens: "1".into(),
    };
    l.append_json("markets.jsonl", &mut json!({"event": "LIST", "market": m})).unwrap();
    T { l, root, signer: Signer { seed: [3; 32] } }
}

impl T {
    fn put(&self, kind: &str, subject: &str, body: &str, fetched: i64, lm: Option<i64>, documented: bool) {
        let sha = self.l.put_blob(body.as_bytes()).unwrap();
        let mut h = BTreeMap::new();
        if let Some(lm) = lm {
            h.insert("last-modified".to_string(), fmt_http_date(lm));
        }
        let mut r = FetchRecord {
            seq: self.l.next_fetch_seq().unwrap(),
            kind: kind.into(),
            subject: subject.into(),
            documented,
            method: "GET".into(),
            url: format!("https://test/{kind}"),
            status: 200,
            fetched_at_ms: fetched,
            headers: h,
            sha256: sha,
            len: body.len() as u64,
            client: "t".into(),
            error: None,
            chain: String::new(),
        };
        self.l.append_fetch(&mut r).unwrap();
    }
    /// dataset response with rows (date, slug, tokens), as_of
    fn dataset(&self, start: &str, end: &str, rows: &[(&str, &str, u64)], as_of: i64) {
        let data: Vec<Value> = rows.iter().map(|(dt, s, t)| json!({"date": dt, "model_permaslug": s, "total_tokens": t.to_string()})).collect();
        let body = json!({"data": data, "meta": {"as_of": zfx_index::time::fmt_ms(as_of), "start_date": start, "end_date": end, "version": "v1"}}).to_string();
        self.put("rankings_daily_api", "q", &body, as_of + 1000, None, true);
    }
    fn endpoints(&self, id: &str, at: i64) {
        let e = |p: &str, c: &str, cr: &str| json!({"provider_name": "p", "tag": "p", "pricing": {"prompt": p, "completion": c, "input_cache_read": cr}});
        let body = json!({"data": {"endpoints": [e("0.000004", "0.00002", "0.0000002"), e("0.000005", "0.000025", "0.0000005"), e("0.000003", "0.00002", "0.0000003")]}}).to_string();
        self.put("model_endpoints", id, &body, at, Some(at), true);
    }
    fn run(&self) {
        licensed::run(&self.l, &self.signer, 1_800_000_000_000).unwrap();
    }
    fn rev(&self, date: &str) -> Value {
        self.l.read_json(licensed::REVISIONS_LIC).unwrap().into_iter().filter(|r| r["utc_date"] == date).last().unwrap_or(Value::Null)
    }
}

fn verify(p: &Value) -> OraclePrint {
    let b = hex::decode(p["canonicalHex"].as_str().unwrap()).unwrap();
    let parts: Vec<&str> = p["reporterSignatures"][0].as_str().unwrap().split(':').collect();
    let pk: [u8; 32] = hex::decode(parts[1]).unwrap().try_into().unwrap();
    let sg: [u8; 64] = hex::decode(parts[2]).unwrap().try_into().unwrap();
    assert!(zfx_oracle_print::verify(&b, &pk, &sg));
    OraclePrint::decode(&b).unwrap()
}

#[test]
fn frozen_params_are_published_and_hashed() {
    let f = frozen_params();
    assert!(f.len() >= 50);
    assert!(f.contains_key(SLUG));
    let mut b = licensed::FORMULA_TEXT_LIC.to_vec();
    b.extend_from_slice(licensed::PARAMS_LIC);
    assert_eq!(licensed::formula_hash_lic(), zfx_oracle_print::sha256(&b));
}

#[test]
fn snapshot_and_backfill_bases_values_and_prints() {
    let t = t("ok", SLUG, ID);
    t.dataset("2026-11-07", "2026-11-09", &[("2026-11-08", SLUG, 900_000_000_000), ("2026-11-09", SLUG, 1_000_000_000_000), ("2026-11-09", "other", 5)], d("2026-11-10").start_ms() + 7 * H);
    t.endpoints(ID, d("2026-11-10").start_ms() + H); // during the day after 11-09
    t.run();
    let r = t.rev("2026-11-09");
    assert_eq!(r["data_status"], "OK");
    assert_eq!(r["price"]["basis"], "LISTED_SNAPSHOT");
    assert_eq!(r["confidence"], "MEDIUM");
    // lower median of listed prices: in $4, out $20, cache read $0.30
    assert_eq!(r["price"]["listed_in_usd_per_m"], "4.000000000");
    assert_eq!(r["price"]["listed_out_usd_per_m"], "20.000000000");
    assert_eq!(r["price"]["listed_cache_read_usd_per_m"], "0.300000000");
    let f = &frozen_params()[SLUG];
    let (v, _, _) = value_micro(1_000_000_000_000, f, &Listed { l_in: 4_000_000_000, l_out: 20_000_000_000, l_cr: 300_000_000, providers: 3, source: "" });
    assert_eq!(r["value_micro_usd"], v.to_string());
    let p = verify(&r["print"]);
    assert_eq!(p.index_id, format!("or-model-spend-lic/{SLUG}"));
    assert_eq!(p.version, licensed::FORMULA_VERSION_LIC);
    assert_eq!(p.formula_hash, licensed::formula_hash_lic());
    assert_eq!(p.value as u128, v);
    // 11-08 precedes the first snapshot: current prices backfilled, LOW confidence
    let r8 = t.rev("2026-11-08");
    assert_eq!(r8["price"]["basis"], "LISTED_BACKFILL");
    assert_eq!(r8["confidence"], "LOW");
    // 11-07 is covered but has no row: outside the top 50
    let r7 = t.rev("2026-11-07");
    assert_eq!(r7["data_status"], "NOT_REPORTED");
    assert_eq!(verify(&r7["print"]).value, 0);
}

#[test]
fn grace_and_unfrozen_model() {
    let t1 = t("grace", SLUG, ID);
    t1.dataset("2026-11-09", "2026-11-09", &[("2026-11-09", SLUG, 10)], d("2026-11-10").start_ms() + 2 * H);
    t1.endpoints(ID, d("2026-11-10").start_ms() + H);
    t1.run();
    assert_eq!(t1.rev("2026-11-09"), Value::Null, "as_of inside the 6 h grace: not evaluated");
    let t2 = t("unfrozen", "acme/new-model-20261110", "acme/new-model");
    t2.dataset("2026-11-09", "2026-11-09", &[("2026-11-09", "acme/new-model-20261110", 10)], d("2026-11-10").start_ms() + 7 * H);
    t2.endpoints("acme/new-model", d("2026-11-10").start_ms() + H);
    t2.run();
    let r = t2.rev("2026-11-09");
    assert_eq!(r["data_status"], "UNPRICED");
    assert!(r["reason"].as_str().unwrap().contains("frozen"));
}

#[test]
fn dataset_correction_creates_corrected_revision() {
    let t = t("corr", SLUG, ID);
    t.dataset("2026-11-09", "2026-11-09", &[("2026-11-09", SLUG, 1_000_000)], d("2026-11-10").start_ms() + 7 * H);
    t.endpoints(ID, d("2026-11-10").start_ms() + H);
    t.run();
    t.dataset("2026-11-09", "2026-11-09", &[("2026-11-09", SLUG, 1_100_000)], d("2026-11-11").start_ms() + 7 * H);
    t.run();
    let all: Vec<Value> = t.l.read_json(licensed::REVISIONS_LIC).unwrap().into_iter().filter(|r| r["utc_date"] == "2026-11-09").collect();
    assert_eq!(all.len(), 2);
    assert_eq!(all[1]["finality"], "CORRECTED");
    assert_eq!(all[1]["total_tokens"], "1100000");
    assert_eq!(hex::encode(verify(&all[1]["print"]).prev_print_hash), all[0]["print"]["printHash"].as_str().unwrap());
    t.l.verify().unwrap();
}

#[test]
fn lic_7d_uses_the_derived_25pct_clamp() {
    let t = t("lic7", SLUG, ID);
    let day = |date: &str, usd: u64| {
        let mut r = json!({"market": SLUG, "utc_date": date, "revision": 1, "revision_id": format!("{date}.r1"), "data_status": "OK", "blocked": false,
            "value_micro_usd": (usd as u128 * 1_000_000).to_string(), "observed_at": format!("{date}T07:00:00.000Z"), "print": {"printHash": format!("{:064x}", usd)}});
        t.l.append_json(licensed::REVISIONS_LIC, &mut r).unwrap();
    };
    for i in 1..=7 {
        day(&format!("2026-11-{i:02}"), 100);
    }
    day("2026-11-08", 240); // 7D 100 -> 120 (+20%): clamped at 15%, not at 25%
    weekly::run_spec(&t.l, &t.signer, &Spec7d::lic(), 1_800_000_000_000).unwrap();
    let r = t.l.read_json(weekly::REVISIONS_LIC_7D).unwrap().into_iter().find(|r| r["utc_date"] == "2026-11-08").unwrap();
    assert_eq!(r["value_usd"], "120.000000");
    assert_eq!(r["clamped"], false);
    assert_eq!(r["clamp_bound"], "0.250000");
    let p = verify(&r["print"]);
    assert_eq!(p.index_id, format!("or-model-spend-lic-7d/{SLUG}"));
    assert_eq!(p.version, weekly::FORMULA_VERSION_LIC_7D);
    assert!(weekly::clamp(120, Some(100), &weekly::Params7d::default()).1, "the web 7D (15%) would clamp the same move");
}
