//! Tracked universe: the dataset introduces models, the web data keeps tracking them, and a
//! model that leaves the catalog or has no usage for RETIRE_ZERO_DAYS days is retired.

use serde_json::{json, Value};
use std::path::PathBuf;
use zfx_index::engine::{self, Market};
use zfx_index::ledger::{FetchRecord, Ledger};
use zfx_index::poll;
use zfx_index::time::{fmt_ms, UtcDate};

const H: i64 = 3_600_000;
const A: &str = "acme/alpha-20260901";
const B: &str = "acme/beta-20261101";
const C: &str = "acme/free-20261101";

fn d(s: &str) -> UtcDate {
    UtcDate::parse(s).unwrap()
}

struct T {
    l: Ledger,
    root: PathBuf,
}
impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn t(name: &str) -> T {
    let root = std::env::temp_dir().join(format!("zfx-uni-{name}-{}-{}", std::process::id(), zfx_index::time::now_ms()));
    let _ = std::fs::remove_dir_all(&root);
    T { l: Ledger::open_write(&root).unwrap(), root }
}

impl T {
    fn put(&self, kind: &str, subject: &str, body: &str, fetched: i64, documented: bool) {
        let sha = self.l.put_blob(body.as_bytes()).unwrap();
        let mut r = FetchRecord {
            seq: self.l.next_fetch_seq().unwrap(),
            kind: kind.into(),
            subject: subject.into(),
            documented,
            method: "GET".into(),
            url: format!("https://test/{kind}"),
            status: 200,
            fetched_at_ms: fetched,
            headers: Default::default(),
            sha256: sha,
            len: body.len() as u64,
            client: "t".into(),
            error: None,
            chain: String::new(),
        };
        self.l.append_fetch(&mut r).unwrap();
    }
    /// catalog entries: (canonical slug, id, prompt price per token)
    fn catalog(&self, models: &[(&str, &str, &str)], at: i64) {
        let data: Vec<Value> = models.iter().map(|(s, id, p)| json!({"id": id, "canonical_slug": s, "name": format!("Acme: {id}"), "pricing": {"prompt": p, "completion": p}, "architecture": {"modality": "text->text"}})).collect();
        self.put("catalog_models", "", &json!({"data": data}).to_string(), at, true);
    }
    fn dataset(&self, start: &str, end: &str, rows: &[(&str, &str, u64)], as_of: i64) {
        let data: Vec<Value> = rows.iter().map(|(dt, s, t)| json!({"date": dt, "model_permaslug": s, "total_tokens": t.to_string()})).collect();
        let body = json!({"data": data, "meta": {"as_of": fmt_ms(as_of), "start_date": start, "end_date": end, "version": "v1"}}).to_string();
        self.put("rankings_daily_api", "q", &body, as_of + 1000, true);
    }
    /// model-activity for `slug`: (date, total tokens), fetched at `at`
    fn activity(&self, slug: &str, rows: &[(&str, u64)], at: i64) {
        let rs: Vec<Value> = rows
            .iter()
            .map(|(dt, n)| json!({"date": format!("{dt} 00:00:00"), "model_permaslug": slug, "variant": "standard", "total_prompt_tokens": n, "total_completion_tokens": 0, "total_native_tokens_reasoning": 0, "count": 1, "total_native_tokens_cached": 0, "total_byok_prompt_tokens": 0, "total_byok_completion_tokens": 0}))
            .collect();
        self.put("model_activity", slug, &json!({"data": {"analytics": rs, "cachedAt": at}}).to_string(), at, false);
    }
    fn list(&self, slug: &str, day: &str) {
        let m = Market {
            permaslug: slug.into(),
            model_id: slug.split('-').next().unwrap().into(),
            name: "x".into(),
            author: "acme".into(),
            hugging_face_id: None,
            modality: Some("text->text".into()),
            created_s: None,
            listed_at_ms: 0,
            listing_day: day.into(),
            listing_rank: 1,
            listing_tokens: "1".into(),
        };
        self.l.append_json("markets.jsonl", &mut json!({"event": "LIST", "market": m})).unwrap();
    }
    fn active(&self) -> Vec<String> {
        engine::load_active_markets(&self.l).unwrap().into_iter().map(|m| m.permaslug).collect()
    }
}

fn days(from: &str, to: &str, n: u64) -> Vec<(String, u64)> {
    (d(from).0..=d(to).0).map(|x| (UtcDate(x).to_string(), n)).collect()
}

#[test]
fn dataset_introduces_new_priced_text_models() {
    let t = t("intro");
    t.list(A, "2026-10-01");
    t.catalog(&[(A, "acme/alpha", "0.000001"), (B, "acme/beta", "0.000002"), (C, "acme/free", "0")], d("2026-11-10").start_ms());
    t.dataset("2026-11-08", "2026-11-09", &[("2026-11-08", A, 900), ("2026-11-09", A, 1000), ("2026-11-09", B, 500), ("2026-11-09", C, 400)], d("2026-11-10").start_ms() + 2 * H);
    let added = poll::list_markets(&t.l, 50, d("2026-11-10").start_ms() + 3 * H).unwrap();
    // A is already tracked; C has no listed price
    assert_eq!(added.iter().map(|m| m.permaslug.as_str()).collect::<Vec<_>>(), vec![B]);
    assert_eq!(added[0].listing_day, "2026-11-09");
    assert_eq!(added[0].listing_rank, 2);
    let ev = t.l.read_json("markets.jsonl").unwrap();
    assert!(ev.last().unwrap()["basis"]["dataset_fetch_seq"].is_u64());
    assert_eq!(t.active(), vec![A, B]);
    // idempotent
    assert!(poll::list_markets(&t.l, 50, d("2026-11-10").start_ms() + 4 * H).unwrap().is_empty());
}

#[test]
fn model_that_falls_out_of_the_dataset_keeps_being_tracked_while_it_has_usage() {
    let t = t("tail");
    t.list(A, "2026-10-01");
    t.catalog(&[(A, "acme/alpha", "0.000001")], d("2026-11-10").start_ms());
    // not in the dataset at all in the window, but the web data still shows usage
    let rows = days("2026-10-20", "2026-11-09", 5);
    let r: Vec<(&str, u64)> = rows.iter().map(|(a, b)| (a.as_str(), *b)).collect();
    t.activity(A, &r, d("2026-11-10").start_ms() + 2 * H);
    assert!(poll::retire_markets(&t.l, d("2026-11-10").start_ms() + 6 * H).unwrap().is_empty());
    assert_eq!(t.active(), vec![A]);
}

#[test]
fn seven_days_without_usage_retires_and_the_dataset_can_bring_it_back() {
    let t = t("zero");
    t.list(A, "2026-10-01");
    t.catalog(&[(A, "acme/alpha", "0.000001")], d("2026-11-10").start_ms());
    let mut rows = days("2026-10-20", "2026-11-02", 5);
    rows.extend(days("2026-11-03", "2026-11-09", 0));
    let r: Vec<(&str, u64)> = rows.iter().map(|(a, b)| (a.as_str(), *b)).collect();
    // a response fetched before 2026-11-09 closed does not count
    t.activity(A, &r, d("2026-11-09").start_ms() + 2 * H);
    assert!(poll::retire_markets(&t.l, d("2026-11-10").start_ms() + 6 * H).unwrap().is_empty());
    t.activity(A, &r, d("2026-11-10").start_ms() + 2 * H);
    let out = poll::retire_markets(&t.l, d("2026-11-10").start_ms() + 6 * H).unwrap();
    assert_eq!(out.len(), 1);
    assert!(out[0].1.contains("no tokens from 2026-11-03 to 2026-11-09"), "{}", out[0].1);
    assert!(t.active().is_empty());
    // all-time history stays: load_markets still returns it
    assert_eq!(engine::load_markets(&t.l).unwrap().len(), 1);
    // once retired, not retired twice
    assert!(poll::retire_markets(&t.l, d("2026-11-10").start_ms() + 7 * H).unwrap().is_empty());
    // back in the dataset: listed again
    t.dataset("2026-11-10", "2026-11-10", &[("2026-11-10", A, 1000)], d("2026-11-11").start_ms() + 2 * H);
    let added = poll::list_markets(&t.l, 50, d("2026-11-11").start_ms() + 3 * H).unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(t.active(), vec![A]);
}

#[test]
fn one_dataset_day_in_the_window_prevents_retirement() {
    let t = t("dsday");
    t.list(A, "2026-10-01");
    t.catalog(&[(A, "acme/alpha", "0.000001")], d("2026-11-10").start_ms());
    let rows = days("2026-11-03", "2026-11-09", 0);
    let r: Vec<(&str, u64)> = rows.iter().map(|(a, b)| (a.as_str(), *b)).collect();
    t.activity(A, &r, d("2026-11-10").start_ms() + 2 * H);
    t.dataset("2026-11-05", "2026-11-05", &[("2026-11-05", A, 10)], d("2026-11-06").start_ms() + 2 * H);
    assert!(poll::retire_markets(&t.l, d("2026-11-10").start_ms() + 6 * H).unwrap().is_empty());
}

#[test]
fn removed_from_catalog_retires_but_new_listings_get_a_grace_period() {
    let t = t("gone");
    t.list(A, "2026-10-01");
    t.list(B, "2026-11-08");
    t.catalog(&[(C, "acme/free", "0.000001")], d("2026-11-10").start_ms());
    let out = poll::retire_markets(&t.l, d("2026-11-10").start_ms() + 6 * H).unwrap();
    assert_eq!(out.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec![A]);
    assert!(out[0].1.contains("not in the OpenRouter catalog"));
    assert_eq!(t.active(), vec![B]);
}
