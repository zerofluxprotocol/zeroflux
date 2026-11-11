use super::*;
use near_sdk::test_utils::{accounts, VMContextBuilder};
use near_sdk::{testing_env, NearToken};
use zfx_oracle_print::{sign, DataStatus, Finality, OracleStatus};

const DAY: i64 = 86_400_000;
const D0: i64 = 20_400 * DAY; // 2025-11-08
const IDX: &str = "or-model-spend-lic-7d/acme/model-x-20261001";
const VER: &str = "ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1";
const FH: [u8; 32] = [0x77; 32];

fn seed(b: u8) -> [u8; 32] {
    [b; 32]
}
fn pkhex(b: u8) -> String {
    hex(&zfx_oracle_print::public_key(&seed(b)))
}

fn ctx(pred: usize, now_ms: i64) {
    testing_env!(VMContextBuilder::new()
        .predecessor_account_id(accounts(pred))
        .current_account_id(accounts(5))
        .block_timestamp((now_ms as u64) * 1_000_000)
        .account_balance(NearToken::from_near(100))
        .build());
}

fn setup() -> Oracle {
    ctx(0, D0 + 30 * DAY);
    let mut o = Oracle::new(accounts(0), vec![pkhex(1), pkhex(2)], 1, U128(0));
    o.set_indices(vec![IndexSpec { index_id: IDX.into(), version: VER.into(), formula_hash: hex(&FH), value_scale: 6 }]);
    o
}

fn print(day: i64, rev: u32, prev: [u8; 32], value: u64) -> OraclePrint {
    OraclePrint {
        index_id: IDX.into(),
        version: VER.into(),
        value_scale: 6,
        value,
        source_start_ms: D0 + (day - 6) * DAY,
        source_end_ms: D0 + (day + 1) * DAY,
        observed_at_ms: D0 + (day + 1) * DAY + 7 * 3_600_000,
        published_at_ms: D0 + (day + 1) * DAY + 8 * 3_600_000,
        valid_until_ms: D0 + (day + 3) * DAY,
        source_hash: [1; 32],
        formula_hash: FH,
        status: OracleStatus::Fresh,
        data_status: DataStatus::Ok,
        finality: if rev > 1 { Finality::Corrected } else { Finality::Final },
        revision: rev,
        prev_print_hash: prev,
    }
}

fn signed(p: &OraclePrint, seeds: &[u8]) -> PushPrint {
    let b = p.encode().unwrap();
    PushPrint {
        canonical_hex: hex(&b),
        reporter_signatures: seeds.iter().map(|s| {
            let (pk, sig) = sign(&b, &seed(*s));
            format!("ed25519:{}:{}", hex(&pk), hex(&sig))
        }).collect(),
    }
}

fn h(p: &OraclePrint) -> [u8; 32] {
    p.hash().unwrap()
}

#[test]
fn accepts_and_views_reconstruct_exact_bytes() {
    let mut o = setup();
    let p1 = print(1, 1, [0; 32], 42_500_000);
    let p2 = print(2, 1, [0; 32], 43_000_000);
    let r = o.push(vec![signed(&p1, &[1]), signed(&p2, &[2])]);
    assert_eq!((r.accepted, r.rejected.len()), (2, 0));
    let l = o.latest(IDX.into()).unwrap();
    assert_eq!(l["value"], "43.000000");
    assert_eq!(l["utc_date"], fmt_date(20_402));
    assert_eq!(l["canonical_hex"], hex(&p2.encode().unwrap()), "rebuilt canonical bytes are exact");
    assert_eq!(l["print_hash"], hex(&h(&p2)));
    let sig = l["reporter_signatures"][0].as_str().unwrap();
    let parts: Vec<&str> = sig.split(':').collect();
    assert!(zfx_oracle_print::verify(&p2.encode().unwrap(), &hex32(parts[1]).unwrap(), &unhex(parts[2]).unwrap().try_into().unwrap()));
    let hist = o.history(IDX.into(), fmt_date(20_401), Some(10));
    assert_eq!(hist.len(), 2);
    assert_eq!(o.history(IDX.into(), fmt_date(20_402), None).len(), 1);
    assert_eq!(o.print(IDX.into(), fmt_date(20_401), 1).unwrap()["value"], "42.500000");
    assert!(o.print(IDX.into(), fmt_date(20_401), 2).is_none());
    assert_eq!(o.index_info(IDX.into()).unwrap()["prints"], 2);
}

#[test]
fn forged_and_wrong_reporter_signatures_rejected() {
    let mut o = setup();
    let p = print(1, 1, [0; 32], 1);
    let mut forged = signed(&p, &[1]);
    let last = forged.reporter_signatures[0].pop().unwrap();
    forged.reporter_signatures[0].push(if last == '0' { '1' } else { '0' });
    let outsider = signed(&p, &[9]); // valid signature, key not in the allowlist
    let mut tampered = signed(&p, &[1]);
    let mut b = unhex(&tampered.canonical_hex).unwrap();
    b[prefix_len(IDX, VER) + 1] ^= 1; // change the value after signing
    tampered.canonical_hex = hex(&b);
    let r = o.push(vec![forged, outsider, tampered]);
    assert_eq!(r.accepted, 0);
    assert!(r.rejected.iter().all(|(_, e)| e.starts_with("quorum not met") || e.starts_with("decode")), "{:?}", r.rejected);
}

#[test]
fn quorum_two_requires_distinct_reporters() {
    let mut o = setup();
    o.set_quorum(2);
    let p = print(1, 1, [0; 32], 1);
    let mut dup = signed(&p, &[1]);
    dup.reporter_signatures.push(dup.reporter_signatures[0].clone());
    let r = o.push(vec![dup]);
    assert_eq!(r.accepted, 0);
    let r = o.push(vec![signed(&p, &[1, 2])]);
    assert_eq!(r.accepted, 1);
}

#[test]
fn replay_stale_and_backdated_rejected() {
    let mut o = setup();
    let p5 = print(5, 1, [0; 32], 5);
    assert_eq!(o.push(vec![signed(&p5, &[1])]).accepted, 1);
    let r = o.push(vec![signed(&p5, &[1]), signed(&print(4, 1, [0; 32], 4), &[1]), signed(&print(5, 2, [9; 32], 6), &[1])]);
    assert_eq!(r.accepted, 0);
    assert_eq!(r.rejected[0].1, "duplicate of the latest print");
    assert!(r.rejected[1].1.starts_with("not newer"));
    assert!(r.rejected[2].1.starts_with("not newer"), "correction not chained to the latest");
    // window that ends after publication
    let mut bad = print(6, 1, [0; 32], 6);
    bad.published_at_ms = bad.source_end_ms - 1;
    // published in the future
    let mut fut = print(7, 1, [0; 32], 7);
    fut.published_at_ms = D0 + 40 * DAY;
    let r = o.push(vec![signed(&bad, &[1]), signed(&fut, &[1])]);
    assert_eq!(r.rejected[0].1, "window not complete before publication");
    assert_eq!(r.rejected[1].1, "published in the future");
}

#[test]
fn correction_chaining() {
    let mut o = setup();
    let p1 = print(3, 1, [0; 32], 100_000_000);
    let p2 = print(3, 2, h(&p1), 110_000_000);
    let p3 = print(3, 3, h(&p2), 120_000_000);
    let r = o.push(vec![signed(&p1, &[1]), signed(&p2, &[1]), signed(&p3, &[1])]);
    assert_eq!(r.accepted, 3);
    assert_eq!(o.latest(IDX.into()).unwrap()["value"], "120.000000");
    assert_eq!(o.latest(IDX.into()).unwrap()["finality"], "CORRECTED");
    assert_eq!(o.print(IDX.into(), fmt_date(20_403), 2).unwrap()["value"], "110.000000");
    assert_eq!(o.history(IDX.into(), fmt_date(20_403), None).len(), 3, "all revisions kept, append-only");
}

#[test]
fn wrong_formula_version_scale_unknown_and_disabled() {
    let mut o = setup();
    let mut a = print(1, 1, [0; 32], 1);
    a.formula_hash = [0x78; 32];
    let mut b = print(1, 1, [0; 32], 1);
    b.version = "ZFX-OR-MODEL-SPEND-USD-7D-v1".into();
    let mut c = print(1, 1, [0; 32], 1);
    c.value_scale = 2;
    let mut d = print(1, 1, [0; 32], 1);
    d.index_id = "or-model-spend-lic-7d/other/model".into();
    let r = o.push(vec![signed(&a, &[1]), signed(&b, &[1]), signed(&c, &[1]), signed(&d, &[1])]);
    let errs: Vec<&str> = r.rejected.iter().map(|x| x.1.as_str()).collect();
    assert_eq!(errs, vec!["formula_hash mismatch", "version mismatch", "value_scale mismatch", "unknown index_id"]);
    o.set_enabled(IDX.into(), false);
    assert_eq!(o.push(vec![signed(&print(1, 1, [0; 32], 1), &[1])]).rejected[0].1, "index_id disabled");
}

#[test]
fn not_reported_prints_are_recorded_as_unavailable() {
    let mut o = setup();
    let mut p = print(1, 1, [0; 32], 0);
    p.status = OracleStatus::Unavailable;
    p.data_status = DataStatus::NotReported;
    assert_eq!(o.push(vec![signed(&p, &[1])]).accepted, 1);
    let l = o.latest(IDX.into()).unwrap();
    assert_eq!((l["status"].as_str(), l["data_status"].as_str()), (Some("UNAVAILABLE"), Some("NOT_REPORTED")));
}

#[test]
fn deactivated_reporter_cannot_sign_but_old_signatures_still_render() {
    let mut o = setup();
    let p1 = print(1, 1, [0; 32], 1);
    assert_eq!(o.push(vec![signed(&p1, &[2])]).accepted, 1);
    o.set_reporters(vec![], vec![pkhex(2)]);
    assert_eq!(o.push(vec![signed(&print(2, 1, [0; 32], 2), &[2])]).accepted, 0);
    let first = o.print(IDX.into(), fmt_date(20_401), 1).unwrap();
    assert!(first["reporter_signatures"][0].as_str().unwrap().contains(&pkhex(2)));
}

#[test]
#[should_panic(expected = "owner only")]
fn owner_only() {
    let mut o = setup();
    ctx(1, D0 + 30 * DAY);
    o.set_quorum(1);
}

#[test]
#[should_panic(expected = "storage balance short")]
fn refuses_push_when_storage_balance_short() {
    let mut o = setup();
    ctx(0, D0 + 30 * DAY);
    o.set_min_free(U128(NearToken::from_near(100).as_yoctonear()));
    o.push(vec![signed(&print(1, 1, [0; 32], 1), &[1])]);
}

#[test]
fn dates() {
    assert_eq!(parse_date("2026-11-09"), Some(20_766));
    assert_eq!(fmt_date(20_766), "2026-11-09");
    assert_eq!(parse_date("2028-02-29").map(fmt_date).as_deref(), Some("2028-02-29"));
    assert_eq!(utc_day(20_767 * DAY), 20_766);
    assert_eq!(parse_date("2026-13-01"), None);
}

#[test]
fn hundred_print_batch_logs_fit_near_limit() {
    let mut o = setup();
    ctx(0, D0 + 200 * DAY);
    let prints: Vec<PushPrint> = (1..=100).map(|d| signed(&print(d, 1, [0; 32], d as u64), &[1])).collect();
    let r = o.push(prints);
    assert_eq!(r.accepted, 100);
    let logs = near_sdk::test_utils::get_logs();
    println!("{} log lines, sizes {:?}", logs.len(), logs.iter().map(|l| l.len()).collect::<Vec<_>>());
    assert!(logs.iter().map(|l| l.len()).sum::<usize>() <= 16_384, "NEAR total log limit per execution");
}
