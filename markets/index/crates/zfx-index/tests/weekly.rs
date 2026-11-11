//! 7D index: validity rule, never-zero, corrections, clamp, cost metrics, prints.

use serde_json::{json, Value};
use std::path::PathBuf;
use zfx_index::engine::Signer;
use zfx_index::ledger::Ledger;
use zfx_index::publish::{day_lifecycle_7d, lifecycle_traded};
use zfx_index::weekly::{self, Params7d};
use zfx_oracle_print::{DataStatus, OraclePrint, OracleStatus};

const M: &str = "acme/model-x-20261001";

struct T {
    l: Ledger,
    root: PathBuf,
    signer: Signer,
    p: Params7d,
}
impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn t(name: &str) -> T {
    let root = std::env::temp_dir().join(format!("zfx-7d-{name}-{}-{}", std::process::id(), zfx_index::time::now_ms()));
    T { l: Ledger::open_write(&root).unwrap(), root, signer: Signer { seed: [5; 32] }, p: Params7d::default() }
}

/// Appends a daily revision record with the fields the 7D engine reads.
fn daily(t: &T, date: &str, rev: u32, status: &str, usd: Option<u64>, blocked: bool) {
    let mut r = json!({
        "market": M, "utc_date": date, "revision": rev, "revision_id": format!("{date}.r{rev}"),
        "data_status": status, "blocked": blocked,
        "value_micro_usd": usd.map(|v| (v as u128 * 1_000_000).to_string()),
        "value_usd": usd.map(|v| format!("{v}.000000")),
        "observed_at": format!("{date}T07:00:00.000Z"),
        "attribution": "Source: OpenRouter (openrouter.ai/rankings), as of test",
        "print": if blocked { Value::Null } else { json!({"printHash": format!("{:064x}", rev as u64 * 1000 + date[8..].parse::<u64>().unwrap())}) },
    });
    t.l.append_json("revisions.jsonl", &mut r).unwrap();
}

fn run(t: &T) -> weekly::Report7d {
    weekly::run(&t.l, &t.signer, &t.p, 1_800_000_000_000).unwrap()
}

fn rev7(t: &T, date: &str) -> Value {
    t.l.read_json(weekly::REVISIONS_7D).unwrap().into_iter().filter(|r| r["utc_date"] == date).last().unwrap_or(Value::Null)
}

fn print_of(r: &Value) -> OraclePrint {
    let p = &r["print"];
    let b = hex::decode(p["canonicalHex"].as_str().unwrap()).unwrap();
    let parts: Vec<&str> = p["reporterSignatures"][0].as_str().unwrap().split(':').collect();
    let pk: [u8; 32] = hex::decode(parts[1]).unwrap().try_into().unwrap();
    let sg: [u8; 64] = hex::decode(parts[2]).unwrap().try_into().unwrap();
    assert!(zfx_oracle_print::verify(&b, &pk, &sg));
    OraclePrint::decode(&b).unwrap()
}

fn days(n: u32) -> Vec<String> {
    (1..=n).map(|i| format!("2026-11-{i:02}")).collect()
}

#[test]
fn full_window_mean_and_cost_metrics() {
    let t = t("full");
    for (i, d) in days(7).iter().enumerate() {
        daily(&t, d, 1, "OK", Some(100 + i as u64 * 10), false); // 100..160, sum 910
    }
    run(&t);
    let r = rev7(&t, "2026-11-07");
    assert_eq!(r["data_status"], "OK");
    assert_eq!(r["valid_days"], 7);
    assert_eq!(r["value_usd"], "130.000000");
    assert_eq!(r["window_sum_usd"], "910.000000");
    assert_eq!(r["manipulation_cost_usd_per_pct"], "9.100000");
    assert_eq!(r["max_open_interest_usd"], "227.500000");
    assert_eq!(r["finality"], "FINAL");
    let p = print_of(&r);
    assert_eq!(p.index_id, format!("or-model-spend-7d/{M}"));
    assert_eq!(p.version, weekly::FORMULA_VERSION_7D);
    assert_eq!(p.value, 130_000_000);
    assert_eq!(zfx_index::time::fmt_ms(p.source_start_ms), "2026-11-01T00:00:00.000Z");
    assert_eq!(zfx_index::time::fmt_ms(p.source_end_ms), "2026-11-08T00:00:00.000Z");
    assert_eq!(p.formula_hash, weekly::formula_hash_7d());
    // windows with fewer than 6 days of history are NOT_REPORTED, not partial means
    assert_eq!(rev7(&t, "2026-11-05")["data_status"], "NOT_REPORTED");
    assert_eq!(rev7(&t, "2026-11-06")["data_status"], "OK"); // 6 of 7 valid (11-00 is missing)
    // idempotent
    let r2 = run(&t);
    assert_eq!(r2.new_final + r2.new_corrected, 0);
}

#[test]
fn six_of_seven_is_mean_of_six_never_zero() {
    let t = t("six");
    for d in days(7) {
        if d == "2026-11-04" {
            daily(&t, &d, 1, "NOT_REPORTED", None, false);
        } else {
            daily(&t, &d, 1, "OK", Some(120), false);
        }
    }
    run(&t);
    let r = rev7(&t, "2026-11-07");
    assert_eq!(r["valid_days"], 6);
    assert_eq!(r["value_usd"], "120.000000", "a missing day is excluded, not counted as zero (which would give 102.86)");
    assert_eq!(r["window_sum_usd"], "720.000000");
    assert_eq!(r["window"][3]["counted"], false);
}

#[test]
fn five_valid_is_not_reported_and_unavailable() {
    let t = t("five");
    for d in days(7) {
        match d.as_str() {
            "2026-11-02" => daily(&t, &d, 1, "UNPRICED", None, false),
            "2026-11-05" => daily(&t, &d, 1, "OK", Some(9999), true), // blocked: excluded
            _ => daily(&t, &d, 1, "OK", Some(100), false),
        }
    }
    run(&t);
    let r = rev7(&t, "2026-11-07");
    assert_eq!(r["data_status"], "NOT_REPORTED");
    assert_eq!(r["valid_days"], 5);
    assert_eq!(r["value_usd"], Value::Null);
    assert_eq!(r["manipulation_cost_usd_per_pct"], Value::Null);
    let p = print_of(&r);
    assert_eq!((p.status, p.data_status, p.value), (OracleStatus::Unavailable, DataStatus::NotReported, 0));
    assert_eq!(day_lifecycle_7d(&r), "CLOSE_ONLY");
}

#[test]
fn daily_correction_revises_every_window_containing_it() {
    let t = t("corr");
    for d in days(9) {
        daily(&t, &d, 1, "OK", Some(100), false);
    }
    run(&t);
    let before = t.l.read_json(weekly::REVISIONS_7D).unwrap().len();
    daily(&t, "2026-11-07", 2, "OK", Some(170), false); // CORRECTED daily day
    let rep = run(&t);
    // windows ending 11-07, 11-08, 11-09 contain 11-07
    assert_eq!(rep.new_corrected, 3);
    assert_eq!(t.l.read_json(weekly::REVISIONS_7D).unwrap().len(), before + 3);
    let r = rev7(&t, "2026-11-07");
    assert_eq!(r["finality"], "CORRECTED");
    assert_eq!(r["revision_id"], "2026-11-07.r2");
    assert_eq!(r["value_usd"], "110.000000");
    let p = print_of(&r);
    assert_eq!(p.revision, 2);
    let first = t.l.read_json(weekly::REVISIONS_7D).unwrap().into_iter().find(|x| x["utc_date"] == "2026-11-07").unwrap();
    assert_eq!(hex::encode(p.prev_print_hash), first["print"]["printHash"].as_str().unwrap());
    assert_eq!(rev7(&t, "2026-11-06")["revision"], 1, "windows not containing the day are untouched");
    t.l.verify().unwrap();
}

#[test]
fn jump_clamp_flags_close_only_but_print_reflects_data() {
    let t = t("clamp");
    for d in days(7) {
        daily(&t, &d, 1, "OK", Some(100), false);
    }
    daily(&t, "2026-11-08", 1, "OK", Some(300), false); // mean 100 -> 128.57 (+28.6%)
    daily(&t, "2026-11-09", 1, "OK", Some(110), false); // 128.57 -> 130.0 (+1.1%)
    let rep = run(&t);
    assert_eq!(rep.clamped, 1);
    let r = rev7(&t, "2026-11-08");
    assert_eq!(r["clamped"], true);
    assert_eq!(r["value_usd"], "128.571429");
    assert_eq!(r["change_vs_prev"], "0.285714");
    assert_eq!(print_of(&r).value, 128_571_429);
    assert_eq!(day_lifecycle_7d(&r), "CLOSE_ONLY");
    let (life, _) = lifecycle_traded(None, Some(&r), 0);
    assert_eq!(life, "CLOSE_ONLY");
    let next = rev7(&t, "2026-11-09");
    assert_eq!(next["clamped"], false);
    assert_eq!(day_lifecycle_7d(&next), "ACTIVE");
    // a 15% bound is inclusive: exactly +15% is not clamped (unit-tested in weekly::tests)
}

#[test]
fn blocked_latest_daily_halts_traded_market() {
    let w = json!({"data_status": "OK", "clamped": false, "print": {"validUntil": "2099-01-01T00:00:00.000Z"}});
    let blocked = json!({"blocked": true, "data_status": "OK"});
    assert_eq!(lifecycle_traded(Some(&blocked), Some(&w), 0).0, "HALTED");
    let ok = json!({"blocked": false, "data_status": "OK"});
    assert_eq!(lifecycle_traded(Some(&ok), Some(&w), 0).0, "ACTIVE");
    assert_eq!(lifecycle_traded(Some(&ok), None, 0).0, "PROPOSED");
}
