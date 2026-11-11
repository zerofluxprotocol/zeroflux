//! Sandbox test of the deployed zfx-oracle wasm (needs the near-sandbox binary, see
//! `sandbox.rs`, and `contracts/target/near/zfx_oracle/zfx_oracle.wasm` from
//! `cargo near build non-reproducible-wasm --no-abi`). Writes measurements to
//! `target/oracle-sandbox-metrics.json`.
//!
//! cargo test -p zfx-oracle-near --test sandbox -- --nocapture

use near_api::NearToken;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use zfx_oracle_near::sandbox::Sandbox;
use zfx_oracle_near::*;
use zfx_oracle_print::{sign, DataStatus, Finality, OraclePrint, OracleStatus};

const DAY: i64 = 86_400_000;
const IDX: &str = "or-model-spend-lic-7d/acme/model-x-20261001";
const VER: &str = "ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1";
const FH: [u8; 32] = [0x77; 32];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}
fn pk(seed: u8) -> String {
    hex::encode(zfx_oracle_print::public_key(&[seed; 32]))
}

fn print(base_day: i64, day: i64, rev: u32, prev: [u8; 32], value: u64) -> OraclePrint {
    let end = (base_day + day + 1) * DAY;
    OraclePrint {
        index_id: IDX.into(),
        version: VER.into(),
        value_scale: 6,
        value,
        source_start_ms: end - 7 * DAY,
        source_end_ms: end,
        observed_at_ms: end + 1000,
        published_at_ms: end + 2000,
        valid_until_ms: end + 2 * DAY,
        source_hash: [3; 32],
        formula_hash: FH,
        status: OracleStatus::Fresh,
        data_status: DataStatus::Ok,
        finality: if rev > 1 { Finality::Corrected } else { Finality::Final },
        revision: rev,
        prev_print_hash: prev,
    }
}

fn signed(p: &OraclePrint, seeds: &[u8]) -> Value {
    let b = p.encode().unwrap();
    let sigs: Vec<String> = seeds.iter().map(|s| {
        let (k, sg) = sign(&b, &[*s; 32]);
        format!("ed25519:{}:{}", hex::encode(k), hex::encode(sg))
    }).collect();
    json!({"canonicalHex": hex::encode(&b), "reporterSignatures": sigs})
}

async fn usage(near: &Near, c: &str) -> u64 {
    near.view(c, "storage_status", json!({})).await.unwrap()["storage_usage_bytes"].as_str().unwrap().parse().unwrap()
}

async fn push(near: &Near, who: &Actor, c: &str, prints: Vec<Value>) -> (Outcome, Value) {
    let o = near.call(who, c, "push", json!({"prints": prints}), NearToken::from_yoctonear(0), 300).await.unwrap();
    let v = o.value.clone();
    (o, v)
}

fn wasm(name: &str) -> Option<Vec<u8>> {
    std::fs::read(root().join("contracts/target/near/zfx_oracle").join(name)).ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn oracle_on_sandbox() {
    let Some(code) = wasm("zfx_oracle.wasm") else {
        panic!("build the contract first: (cd markets/index/contracts/zfx-oracle && cargo near build non-reproducible-wasm --no-abi)");
    };
    let sb = Sandbox::start().await.expect("sandbox");
    let near = Near::rpcs(&[sb.rpc_url.clone()]).unwrap();
    let (oracle, _, _) = near.create_subaccount(&sb.root, "oracle", NearToken::from_near(20)).await.unwrap();
    let (relayer, _, _) = near.create_subaccount(&sb.root, "relayer", NearToken::from_near(10)).await.unwrap();
    let c = oracle.id.to_string();
    near.deploy(&oracle, code.clone()).await.unwrap();
    near.call(&oracle, &c, "new", json!({"owner": c, "reporters": [pk(1), pk(2)], "quorum": 1, "min_free_yocto": "0"}), NearToken::from_yoctonear(0), 30).await.unwrap();
    near.call(&oracle, &c, "set_indices", json!({"specs": [{"index_id": IDX, "version": VER, "formula_hash": hex::encode(FH), "value_scale": 6}]}), NearToken::from_yoctonear(0), 30).await.unwrap();
    let base = now_ms() / DAY - 200;

    // ensure_indices: registers what is missing (owner only), then is a no-op
    let mut wanted = std::collections::BTreeMap::new();
    for id in [IDX, "or-model-spend-7d/acme/new-model-20261101"] {
        wanted.insert(id.to_string(), json!({"index_id": id, "version": VER, "formula_hash": hex::encode(FH), "value_scale": 6}));
    }
    assert!(zfx_oracle_near::ensure_indices(&near, &relayer, &c, &wanted).await.is_err(), "only the owner registers");
    assert_eq!(zfx_oracle_near::ensure_indices(&near, &oracle, &c, &wanted).await.unwrap(), 1);
    assert_eq!(zfx_oracle_near::ensure_indices(&near, &oracle, &c, &wanted).await.unwrap(), 0);
    assert!(near.view(&c, "index_info", json!({"index_id": "or-model-spend-7d/acme/new-model-20261101"})).await.unwrap().is_object());

    // single print: gas and storage
    let u0 = usage(&near, &c).await;
    let p1 = print(base, 1, 1, [0; 32], 42_500_000);
    let (o1, v1) = push(&near, &relayer, &c, vec![signed(&p1, &[1])]).await;
    assert_eq!(v1["accepted"], 1, "{v1}");
    let u1 = usage(&near, &c).await;
    let gas1 = o1.gas_burnt;

    // batch of 10 and of 50
    let mut prev = p1.hash().unwrap();
    let mut day = 2;
    let batch = |n: usize, day: &mut i64, prev: &mut [u8; 32]| -> Vec<Value> {
        (0..n).map(|_| {
            let p = print(base, *day, 1, [0; 32], 40_000_000 + *day as u64);
            *prev = p.hash().unwrap();
            *day += 1;
            signed(&p, &[1])
        }).collect()
    };
    let b10 = batch(10, &mut day, &mut prev);
    let (o10, v10) = push(&near, &relayer, &c, b10).await;
    assert_eq!(v10["accepted"], 10);
    let u10 = usage(&near, &c).await;
    let b50 = batch(50, &mut day, &mut prev);
    let (o50, v50) = push(&near, &relayer, &c, b50).await;
    assert_eq!(v50["accepted"], 50);
    let u50 = usage(&near, &c).await;
    // a full 100-print batch (event log split under the 16 KiB per-line limit)
    let b100 = batch(100, &mut day, &mut prev);
    let (o100, v100) = push(&near, &relayer, &c, b100).await;
    assert_eq!(v100["accepted"], 100);
    assert!(o100.logs.iter().map(|l| l.len()).sum::<usize>() <= 16_384, "total log length under the NEAR limit");
    println!("gas: 100 prints {:.2} Tgas ({:.2}/print)", o100.gas_burnt as f64 / 1e12, o100.gas_burnt as f64 / 1e14);
    println!("gas: 1 print {:.2} Tgas; 10 prints {:.2} Tgas ({:.2}/print); 50 prints {:.2} Tgas ({:.2}/print)", gas1 as f64 / 1e12, o10.gas_burnt as f64 / 1e12, o10.gas_burnt as f64 / 1e13, o50.gas_burnt as f64 / 1e12, o50.gas_burnt as f64 / 5e13);
    println!("storage: first print {} bytes, then {} bytes/print", u1 - u0, (u50 - u10) / 50);
    assert!(o50.gas_burnt < 300_000_000_000_000);

    // attacks
    let last = day - 1;
    let newp = print(base, day, 1, [0; 32], 1);
    let mut forged = signed(&newp, &[1]);
    let s = forged["reporterSignatures"][0].as_str().unwrap().to_string();
    let flipped = format!("{}{}", &s[..s.len() - 1], if s.ends_with('0') { '1' } else { '0' });
    forged["reporterSignatures"] = json!([flipped]);
    let outsider = signed(&newp, &[9]);
    let stale = signed(&print(base, last - 3, 1, [0; 32], 1), &[1]);
    let replay = signed(&print(base, last, 1, [0; 32], 40_000_000 + last as u64), &[1]);
    let unchained = signed(&print(base, last, 2, [0xee; 32], 2), &[1]);
    let (_, va) = push(&near, &relayer, &c, vec![forged, outsider, stale, replay, unchained]).await;
    let reasons: Vec<String> = va["rejected"].as_array().unwrap().iter().map(|r| r[1].as_str().unwrap().to_string()).collect();
    println!("attack rejections: {reasons:?}");
    assert_eq!(va["accepted"], 0);
    assert!(reasons[0].starts_with("quorum not met") && reasons[1].starts_with("quorum not met"));
    assert!(reasons[2].starts_with("not newer"));
    assert_eq!(reasons[3], "duplicate of the latest print");
    assert!(reasons[4].starts_with("not newer"));

    // correction chaining on the latest window
    let r2 = print(base, last, 2, prev, 99_000_000);
    let r3 = print(base, last, 3, r2.hash().unwrap(), 98_000_000);
    let (_, vc) = push(&near, &relayer, &c, vec![signed(&r2, &[2]), signed(&r3, &[1])]).await;
    assert_eq!(vc["accepted"], 2, "{vc}");

    // views and off-chain re-verification
    let l = near.view(&c, "latest", json!({"index_id": IDX})).await.unwrap();
    assert_eq!(l["value"], "98.000000");
    assert_eq!(l["revision"], 3);
    let bytes = hex::decode(l["canonical_hex"].as_str().unwrap()).unwrap();
    assert_eq!(hex::encode(zfx_oracle_print::sha256(&bytes)), l["print_hash"].as_str().unwrap());
    let parts: Vec<&str> = l["reporter_signatures"][0].as_str().unwrap().split(':').collect();
    assert!(zfx_oracle_print::verify(&bytes, &hex::decode(parts[1]).unwrap().try_into().unwrap(), &hex::decode(parts[2]).unwrap().try_into().unwrap()));
    let hist = near.view(&c, "history", json!({"index_id": IDX, "from_date": l["utc_date"], "limit": 10})).await.unwrap();
    assert_eq!(hist.as_array().unwrap().len(), 3, "r1, r2, r3 of the last day");
    let one = near.view(&c, "print", json!({"index_id": IDX, "utc_date": l["utc_date"], "revision": 2})).await.unwrap();
    assert_eq!(one["value"], "99.000000");

    // owner-only and storage refusal
    assert!(near.call(&relayer, &c, "set_quorum", json!({"quorum": 1}), NearToken::from_yoctonear(0), 30).await.is_err());
    near.call(&oracle, &c, "set_min_free", json!({"min_free_yocto": NearToken::from_near(1000).as_yoctonear().to_string()}), NearToken::from_yoctonear(0), 30).await.unwrap();
    let e = near.call(&relayer, &c, "push", json!({"prints": [signed(&print(base, day + 5, 1, [0; 32], 1), &[1])]}), NearToken::from_yoctonear(0), 300).await.unwrap_err().to_string();
    assert!(e.contains("storage balance short"), "{e}");
    near.call(&oracle, &c, "set_min_free", json!({"min_free_yocto": "0"}), NearToken::from_yoctonear(0), 30).await.unwrap();

    // pusher end to end with real dataset 7D prints from the committed snapshot
    let snap = root().join("ledger");
    let local = local_prints(&snap, &["revisions_lic_7d.jsonl"]).unwrap();
    let (real_id, real) = local.iter().max_by_key(|(_, v)| v.len()).unwrap();
    let rp: Value = serde_json::from_str(&std::fs::read_to_string(root().join("keys/oracle-reporter.pub.json")).unwrap()).unwrap();
    let fh = hex::encode(zfx_oracle_print::sha256(&std::fs::read(root().join("formula/ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1.txt")).unwrap()));
    near.call(&oracle, &c, "set_reporters", json!({"add": [rp["pubkey_hex"]], "deactivate": []}), NearToken::from_yoctonear(0), 30).await.unwrap();
    near.call(&oracle, &c, "set_indices", json!({"specs": [{"index_id": real_id, "version": "ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1", "formula_hash": fh, "value_scale": 6}]}), NearToken::from_yoctonear(0), 30).await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::copy(snap.join("revisions_lic_7d.jsonl"), tmp.path().join("revisions_lic_7d.jsonl")).unwrap();
    let only = Some([real_id.clone()].into_iter().collect());
    let ur0 = usage(&near, &c).await;
    let r1 = push_new(&near, &relayer, &c, tmp.path(), &["revisions_lic_7d.jsonl"], &PushOpts { from_date: None, only: only.clone(), est_tgas_per_print: 8, dry_run: false, max_prints: None }).await.unwrap();
    let ur1 = usage(&near, &c).await;
    println!("real push: {}", serde_json::to_string(&r1).unwrap());
    assert_eq!(r1["accepted"].as_u64().unwrap() as usize, real.len());
    let r2 = push_new(&near, &relayer, &c, tmp.path(), &["revisions_lic_7d.jsonl"], &PushOpts { from_date: None, only, est_tgas_per_print: 8, dry_run: false, max_prints: None }).await.unwrap();
    assert_eq!(r2["queued"], 0, "idempotent: nothing to send the second time");
    let real_gas: f64 = r1["txs"].as_array().unwrap().iter().map(|t| t["gas_burnt_tgas"].as_str().unwrap().parse::<f64>().unwrap()).sum();
    let real_per_print_gas = real_gas / real.len() as f64;
    let real_per_print_bytes = (ur1 - ur0) as f64 / real.len() as f64;
    let rl = near.view(&c, "latest", json!({"index_id": real_id})).await.unwrap();
    let real_last = real.last().unwrap();
    assert_eq!(rl["print_hash"].as_str().unwrap(), hex::encode(real_last.hash));
    println!("real prints: {} for {real_id}, {real_per_print_gas:.2} Tgas/print, {real_per_print_bytes:.0} bytes/print", real.len());

    // optional: in-wasm dalek verification for comparison
    let mut dalek = Value::Null;
    if let Some(dcode) = wasm("zfx_oracle_dalek.wasm") {
        let (o2, _, _) = near.create_subaccount(&sb.root, "oracle2", NearToken::from_near(10)).await.unwrap();
        let c2 = o2.id.to_string();
        near.deploy(&o2, dcode).await.unwrap();
        near.call(&o2, &c2, "new", json!({"owner": c2, "reporters": [pk(1)], "quorum": 1, "min_free_yocto": "0"}), NearToken::from_yoctonear(0), 30).await.unwrap();
        near.call(&o2, &c2, "set_indices", json!({"specs": [{"index_id": IDX, "version": VER, "formula_hash": hex::encode(FH), "value_scale": 6}]}), NearToken::from_yoctonear(0), 30).await.unwrap();
        let (od, vd) = push(&near, &relayer, &c2, (1..=10).map(|d| signed(&print(base, d, 1, [0; 32], 1), &[1])).collect()).await;
        assert_eq!(vd["accepted"], 10);
        println!("dalek in-wasm verify: 10 prints {:.2} Tgas ({:.2}/print)", od.gas_burnt as f64 / 1e12, od.gas_burnt as f64 / 1e13);
        dalek = json!({"prints": 10, "gas_tgas": od.gas_burnt as f64 / 1e12, "wasm_bytes": std::fs::metadata(root().join("contracts/target/near/zfx_oracle/zfx_oracle_dalek.wasm")).unwrap().len()});
    }

    let metrics = json!({
        "wasm_bytes": code.len(),
        "gas_tgas": {"one_print": gas1 as f64 / 1e12, "batch_10": o10.gas_burnt as f64 / 1e12, "batch_50": o50.gas_burnt as f64 / 1e12, "batch_100": o100.gas_burnt as f64 / 1e12, "per_print_in_batch_100": o100.gas_burnt as f64 / 1e14, "per_print_in_batch_50": o50.gas_burnt as f64 / 5e13, "real_lic_7d_per_print": real_per_print_gas},
        "storage_bytes": {"first_print": u1 - u0, "per_print_synthetic_43_char_id": (u50 - u10) as f64 / 50.0, "per_print_real": real_per_print_bytes},
        "real": {"index_id": real_id, "prints": real.len(), "txs": r1["txs"]},
        "attack_rejections": reasons,
        "dalek_verify": dalek,
    });
    let out = root().join("target/oracle-sandbox-metrics.json");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(&out, serde_json::to_vec_pretty(&metrics).unwrap()).unwrap();
    println!("metrics written to {}", out.display());
    let _ = Path::new("");
    sb.stop().await;
}
