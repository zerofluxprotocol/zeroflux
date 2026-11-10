//! End-to-end load test: HTTP + signature check + sequencing + journal fsync + reply.
//!
//! Starts its own node in a temp directory, onboards accounts through the internal chain-event
//! API, then runs concurrent clients sending a mixed flow (55% passive limits, 35% cancels, 10%
//! IOC takers) for a fixed time. Orders are signed before the timer starts, so latency is the
//! server round trip a client sees.
//!
//! cargo run --release -p zfx-perps-node --bin zfx-perps-loadgen -- [--clients 64] [--seconds 10] [--no-fsync]

use hdrhistogram::Histogram;
use zfx_perps_client::{Admin, Client, Order, Signer, now_ms};
use zfx_perps_node::config::{AssetCfg, Config, MarketCfg};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const M: &str = "NEAR-USDC";

fn arg(name: &str, default: u64) -> u64 {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let clients = arg("--clients", 64) as usize;
    let seconds = arg("--seconds", 10);
    let fsync = !std::env::args().any(|a| a == "--no-fsync");
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        listen: "127.0.0.1:0".into(),
        data_dir: dir.path().into(),
        fsync,
        settlement_contract: "ob.bench.near".into(),
        fee_account: "fees.bench.near".into(),
        admin_token: "loadgen-admin-token-0000".into(),
        batch_interval_ms: 1_000,
        snapshot_every: 1_000_000,
        max_batch_entries: 48,
        max_batch_withdrawals: 4,
        rate_limit_per_sec: 1_000_000,
        insurance_account: None,
        funding_interval_ms: 3_600_000,
        oracle: None,
        clearing: None,
        assets: vec![
            AssetCfg {
                symbol: "NEAR".into(),
                token: "wrap.bench.near".into(),
                decimals: 24,
            },
            AssetCfg {
                symbol: "USDC".into(),
                token: "usdc.bench.near".into(),
                decimals: 6,
            },
        ],
        markets: vec![MarketCfg {
            symbol: M.into(),
            base: "NEAR".into(),
            quote: "USDC".into(),
            base_lot: "10000000000000000000000".into(),
            quote_tick: "1".into(),
            min_qty: 1,
            max_qty: 100_000_000,
            max_price: 100_000_000,
            maker_fee_bps: 2,
            taker_fee_bps: 5,
            max_open_orders: 1_000_000,
            perp: None,
        }],
    };
    let node = zfx_perps_node::run(cfg).await.unwrap();
    let url = format!("http://{}", node.addr);
    let admin = Admin::new(&url, "loadgen-admin-token-0000");
    let signers: Vec<Signer> = (0..clients).map(|i| Signer::from_seed([i as u8 + 1; 32])).collect();
    let mut evs = vec![];
    let mut seq = 0u64;
    for (i, s) in signers.iter().enumerate() {
        let name = format!("u{i}.bench.near");
        for e in [
            json!({"type": "key_added", "account": name, "public_key": s.public_key()}),
            json!({"type": "deposit", "account": name, "token": "wrap.bench.near", "amount": "1000000000000000000000000000000000"}),
            json!({"type": "deposit", "account": name, "token": "usdc.bench.near", "amount": "1000000000000000000"}),
        ] {
            seq += 1;
            let mut e = e;
            e["seq"] = json!(seq.to_string());
            evs.push(e);
        }
    }
    admin.chain_events(evs).await.unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let mut tasks = vec![];
    for (i, s) in signers.into_iter().enumerate() {
        let c = Client::new(&url, &format!("u{i}.bench.near"), s);
        let stop = stop.clone();
        let url = url.clone();
        tasks.push(tokio::spawn(async move {
            let http = reqwest::Client::builder().pool_max_idle_per_host(4).build().unwrap();
            let mut rng = SmallRng::seed_from_u64(i as u64);
            let mut h = [Histogram::<u64>::new(3).unwrap(), Histogram::<u64>::new(3).unwrap(), Histogram::<u64>::new(3).unwrap()];
            let mut live: Vec<u64> = vec![];
            let mut errors = 0u64;
            // Unique client ids make every signed message unique (identical orders signed in the
            // same millisecond would otherwise share a signature and trip the replay guard).
            let mut cid = 0u64;
            let mid = 42_500u64;
            while !stop.load(Ordering::Relaxed) {
                let r: f64 = rng.r#gen();
                let side = if rng.gen_bool(0.5) { "buy" } else { "sell" };
                let (kind, path, body) = if r < 0.55 || live.is_empty() {
                    let off = rng.gen_range(1..200u64);
                    let px = if side == "buy" { mid - off } else { mid + off };
                    {
                        cid += 1;
                        (0, "/v1/orders", c.signed_order(&Order::limit(M, side, px, rng.gen_range(1..500)).client_id(cid), now_ms()))
                    }
                } else if r < 0.90 {
                    let id = live.swap_remove(rng.gen_range(0..live.len()));
                    let ts = now_ms();
                    let sig = c.signer.sign_b64(&zfx_perps_wire::cancel_msg(&c.account, M, id, 0, ts));
                    (1, "/v1/orders/cancel", json!({"account": c.account, "market": M, "order_id": id, "ts": ts, "public_key": c.signer.public_key(), "signature": sig}))
                } else {
                    let px = if side == "buy" { mid + 200 } else { mid - 200 };
                    {
                        cid += 1;
                        (2, "/v1/orders", c.signed_order(&Order::ioc(M, side, px, rng.gen_range(200..1500)).client_id(cid), now_ms()))
                    }
                };
                let t = Instant::now();
                let resp = http.post(format!("{url}{path}")).json(&body).send().await;
                let ok = match resp {
                    Ok(r) => {
                        let status = r.status();
                        let v: serde_json::Value = r.json().await.unwrap_or_default();
                        if kind == 0 && v["status"] == "resting" {
                            live.push(v["order_id"].as_u64().unwrap());
                        }
                        status.is_success() || status.as_u16() == 409
                    }
                    Err(_) => false,
                };
                h[kind].record(t.elapsed().as_micros() as u64).unwrap();
                if !ok {
                    errors += 1;
                }
            }
            (h, errors)
        }));
    }
    let t0 = Instant::now();
    let s0 = node.app.seq.stats.durable_seq.load(Ordering::Relaxed);
    let f0 = node.app.seq.stats.fsyncs.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    stop.store(true, Ordering::Relaxed);
    let mut all = [
        Histogram::<u64>::new(3).unwrap(),
        Histogram::<u64>::new(3).unwrap(),
        Histogram::<u64>::new(3).unwrap(),
    ];
    let mut errors = 0;
    for t in tasks {
        let (h, e) = t.await.unwrap();
        for k in 0..3 {
            all[k].add(&h[k]).unwrap();
        }
        errors += e;
    }
    let secs = t0.elapsed().as_secs_f64();
    let cmds = node.app.seq.stats.durable_seq.load(Ordering::Relaxed) - s0;
    let fsyncs = node.app.seq.stats.fsyncs.load(Ordering::Relaxed) - f0;
    let total: u64 = all.iter().map(|h| h.len()).sum();
    println!(
        "zfx-perps-node end-to-end: {clients} clients, {secs:.1}s, fsync={fsync}: {total} requests = {:.0} req/s, {cmds} journaled commands, {fsyncs} fsyncs ({:.1} commands per fsync), {errors} errors",
        total as f64 / secs,
        cmds as f64 / fsyncs.max(1) as f64
    );
    for (name, h) in ["place (passive)", "cancel", "IOC taker"].iter().zip(&all) {
        println!(
            "  {name:16} n={:>8}  p50={:>6}us  p90={:>6}us  p99={:>6}us  p99.9={:>6}us  max={:>7}us",
            h.len(),
            h.value_at_quantile(0.5),
            h.value_at_quantile(0.9),
            h.value_at_quantile(0.99),
            h.value_at_quantile(0.999),
            h.max()
        );
    }
    node.shutdown().await;
}
