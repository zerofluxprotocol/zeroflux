//! Node integration tests: a real node (HTTP + WS + journal on disk) driven by the Rust client.
//! Chain events are injected through /internal/chain_events the same way the NEAR adapter does.

use zfx_perps_client::{Ack, Admin, BookFeed, Client, Error, Feed, Order, Signer};
use zfx_perps_node::config::{AssetCfg, Config, MarketCfg};
use serde_json::{Value, json};
use std::path::Path;

const TOKEN: &str = "admin-token-for-tests-only";
const NEAR: &str = "wrap.test.near";
const USDC: &str = "usdc.test.near";
const M: &str = "NEAR-USDC";
const LOT: u128 = 10_000_000_000_000_000_000_000;

fn cfg(dir: &Path) -> Config {
    Config {
        listen: "127.0.0.1:0".into(),
        data_dir: dir.to_path_buf(),
        fsync: true,
        settlement_contract: "ob.test.near".into(),
        fee_account: "fees.test.near".into(),
        admin_token: TOKEN.into(),
        batch_interval_ms: 3_600_000,
        snapshot_every: 50,
        max_batch_entries: 48,
        max_batch_withdrawals: 4,
        rate_limit_per_sec: 10_000,
        insurance_account: None,
        funding_interval_ms: 3_600_000,
        oracle: None,
        assets: vec![
            AssetCfg {
                symbol: "NEAR".into(),
                token: NEAR.into(),
                decimals: 24,
            },
            AssetCfg {
                symbol: "USDC".into(),
                token: USDC.into(),
                decimals: 6,
            },
        ],
        markets: vec![MarketCfg {
            symbol: M.into(),
            base: "NEAR".into(),
            quote: "USDC".into(),
            base_lot: LOT.to_string(),
            quote_tick: "1".into(),
            min_qty: 1,
            max_qty: 100_000_000,
            max_price: 100_000_000,
            maker_fee_bps: 2,
            taker_fee_bps: 5,
            max_open_orders: 100,
            perp: None,
        }],
    }
}

struct Node {
    url: String,
    admin: Admin,
    running: zfx_perps_node::Running,
}

async fn start(dir: &Path) -> Node {
    let running = zfx_perps_node::run(cfg(dir)).await.unwrap();
    let url = format!("http://{}", running.addr);
    Node {
        admin: Admin::new(&url, TOKEN),
        url,
        running,
    }
}

impl Node {
    async fn stop(self) {
        self.running.shutdown().await;
    }
}

/// Onboard accounts through chain events: key registration plus deposits.
async fn onboard(admin: &Admin, start_seq: u64, users: &[(&str, &Signer)]) -> u64 {
    let mut seq = start_seq;
    let mut evs: Vec<Value> = vec![];
    for (name, s) in users {
        for e in [
            json!({"type": "key_added", "account": name, "public_key": s.public_key()}),
            json!({"type": "deposit", "account": name, "token": NEAR, "amount": (100_000 * LOT).to_string()}),
            json!({"type": "deposit", "account": name, "token": USDC, "amount": "100000000000"}),
        ] {
            seq += 1;
            let mut e = e;
            e["seq"] = json!(seq.to_string());
            evs.push(e);
        }
    }
    let r = admin.chain_events(evs).await.unwrap();
    for x in r["results"].as_array().unwrap() {
        assert_eq!(x["result"]["type"], "ok", "{x}");
    }
    seq
}

fn code(e: Error) -> String {
    match e {
        Error::Api { code, .. } => code,
        other => panic!("expected API error, got {other}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trade_flow_feeds_batches_and_crash_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let node = start(dir.path()).await;
    let (sa, sb) = (Signer::generate(), Signer::generate());
    let alice = Client::new(&node.url, "alice.test.near", sa.clone());
    let bob = Client::new(&node.url, "bob.test.near", sb.clone());
    onboard(&node.admin, 0, &[("alice.test.near", &sa), ("bob.test.near", &sb)]).await;

    let info = alice.info().await.unwrap();
    assert_eq!(info.markets[0].symbol, M);
    assert_eq!(info.settlement_contract, "ob.test.near");

    // Subscribe before trading so we see every delta.
    let mut feed = BookFeed::connect(&node.url, M).await.unwrap();
    let mut acct_feed = Feed::connect(&node.url).await.unwrap();
    acct_feed.send(bob.ws_auth()).await.unwrap();
    assert_eq!(acct_feed.next().await.unwrap()["ok"], true);

    // Alice posts asks, Bob lifts part of them.
    for (px, q) in [(42_500, 100), (42_510, 50), (42_520, 25)] {
        let a = alice.place(&Order::limit(M, "sell", px, q)).await.unwrap();
        assert!(matches!(a, Ack::Placed { ref status, .. } if status == "resting"));
    }
    let a = bob.place(&Order::ioc(M, "buy", 42_510, 120).client_id(9)).await.unwrap();
    assert_eq!(
        a,
        Ack::Placed {
            order_id: 4,
            status: "filled".into(),
            filled: 120,
            remaining: 0
        }
    );

    // REST book, WS book and recent trades agree.
    let snap = alice.book(M, 10).await.unwrap();
    assert_eq!(snap.asks, vec![(42_510, 30), (42_520, 25)]);
    feed.until_seq(snap.seq).await.unwrap();
    assert_eq!(feed.state.asks.iter().map(|(p, q)| (*p, *q)).collect::<Vec<_>>(), snap.asks);
    let tr = alice.trades(M, 10).await.unwrap();
    assert_eq!(
        tr.iter().map(|t| (t.price, t.qty)).collect::<Vec<_>>(),
        vec![(42_510, 20), (42_500, 100)]
    );

    // Bob's private stream: accepted, then two fills.
    let mut kinds = vec![];
    while kinds.len() < 3 {
        kinds.push(acct_feed.next().await.unwrap()["type"].as_str().unwrap().to_string());
    }
    assert_eq!(kinds, vec!["accepted", "fill", "fill"]);

    // Balances: Bob paid 120 lots * price + 5 bps taker fee.
    let notional: u128 = 100 * 42_500 + 20 * 42_510;
    let fee = (100 * 42_500u128 * 5).div_ceil(10_000) + (20 * 42_510u128 * 5).div_ceil(10_000);
    let bv = bob.account().await.unwrap();
    assert_eq!(bv.total(USDC), 100_000_000_000 - notional - fee);
    assert_eq!(bv.total(NEAR), 100_000 * LOT + 120 * LOT);
    let av = alice.account().await.unwrap();
    assert_eq!(av.open_orders.len(), 2);
    assert_eq!(av.available(NEAR), 100_000 * LOT - 175 * LOT);

    // Auth failures.
    let mallory = Client::new(&node.url, "alice.test.near", Signer::generate());
    assert_eq!(
        code(mallory.place(&Order::limit(M, "buy", 1, 1)).await.unwrap_err()),
        "key_not_registered"
    );
    let body = alice.signed_order(&Order::limit(M, "buy", 40_000, 1), zfx_perps_client::now_ms());
    let http = reqwest::Client::new();
    assert!(
        http.post(format!("{}/v1/orders", node.url))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let replay = http.post(format!("{}/v1/orders", node.url)).json(&body).send().await.unwrap();
    assert_eq!(replay.status(), 401, "replayed signature rejected");
    let stale = alice.signed_order(&Order::limit(M, "buy", 40_000, 1), zfx_perps_client::now_ms() - 120_000);
    assert_eq!(
        http.post(format!("{}/v1/orders", node.url))
            .json(&stale)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut tampered = alice.signed_order(&Order::limit(M, "buy", 40_000, 1), zfx_perps_client::now_ms());
    tampered["qty"] = json!(1000);
    assert_eq!(
        http.post(format!("{}/v1/orders", node.url))
            .json(&tampered)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    // Engine reject surfaces as 409 with a code.
    assert_eq!(
        code(bob.place(&Order::limit(M, "buy", 42_520, 1).tif("post_only")).await.unwrap_err()),
        "post_only_would_cross"
    );
    assert_eq!(
        code(bob.place(&Order::limit(M, "buy", 100_000_000, 100_000_000)).await.unwrap_err()),
        "insufficient_balance"
    );
    assert_eq!(
        code(bob.place(&Order::limit("BTC-USDC", "buy", 1, 1)).await.unwrap_err()),
        "unknown_market"
    );

    // Withdrawal + batch cut: the batch nets everyone's trades and carries the signed withdrawal.
    assert_eq!(bob.withdraw_next(USDC, 1_000_000).await.unwrap(), Ack::WithdrawQueued { nonce: 1 });
    node.admin.cut().await.unwrap();
    let batches = loop {
        let b = node.admin.batches().await.unwrap();
        if !b["batches"].as_array().unwrap().is_empty() {
            break b;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let b0 = &batches["batches"][0];
    assert_eq!(b0["id"], "1");
    assert_eq!(b0["seal"], true);
    assert_eq!(b0["withdrawals"][0]["amount"], "1000000");
    let sum: i128 = b0["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["token"] == USDC)
        .map(|e| e["delta"].as_str().unwrap().parse::<i128>().unwrap())
        .sum();
    assert_eq!(sum, 0, "trade deltas net to zero");

    // Crash recovery: stop, restart from the same directory, state is identical.
    let before_alice = alice.account().await.unwrap();
    let before_bob = bob.account().await.unwrap();
    let before_book = alice.book(M, 50).await.unwrap();
    node.stop().await;
    let node = start(dir.path()).await;
    let alice = Client::new(&node.url, "alice.test.near", sa.clone());
    let bob = Client::new(&node.url, "bob.test.near", sb.clone());
    assert_eq!(alice.account().await.unwrap(), before_alice);
    assert_eq!(bob.account().await.unwrap(), before_bob);
    let after = alice.book(M, 50).await.unwrap();
    assert_eq!((after.bids.clone(), after.asks.clone()), (before_book.bids, before_book.asks));
    assert_eq!(
        node.admin.batches().await.unwrap()["batches"][0],
        *b0,
        "unconfirmed batch survives restart"
    );
    // The chain confirms batch 1.
    node.admin
        .chain_events(vec![json!({"type": "batch_settled", "seq": "7", "batch_id": "1"})])
        .await
        .unwrap();
    let st = node.admin.status().await.unwrap();
    assert_eq!((st["last_settled"].as_u64(), st["unconfirmed"].as_u64()), (Some(1), Some(0)));
    // Chain event gaps are refused and duplicates are idempotent.
    let r = node
        .admin
        .chain_events(vec![json!({"type": "batch_settled", "seq": "9", "batch_id": "1"})])
        .await
        .unwrap();
    assert_eq!(r["results"][0]["result"]["error"], "chain_event_gap");
    let r = node
        .admin
        .chain_events(vec![json!({"type": "batch_settled", "seq": "7", "batch_id": "1"})])
        .await
        .unwrap();
    assert_eq!(r["results"][0]["result"]["type"], "duplicate");
    // Trading continues after restart with fresh order ids.
    let a = alice.place(&Order::limit(M, "sell", 43_000, 1)).await.unwrap();
    assert!(matches!(a, Ack::Placed { order_id, .. } if order_id > 4));
    node.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_clients_and_ws_book_consistency() {
    let dir = tempfile::tempdir().unwrap();
    let node = start(dir.path()).await;
    let signers: Vec<Signer> = (0..8).map(|_| Signer::generate()).collect();
    let names: Vec<String> = (0..8).map(|i| format!("t{i}.test.near")).collect();
    let users: Vec<(&str, &Signer)> = names.iter().map(|n| n.as_str()).zip(signers.iter()).collect();
    onboard(&node.admin, 0, &users).await;
    let mut feed = BookFeed::connect(&node.url, M).await.unwrap();
    let mut tasks = vec![];
    for (i, s) in signers.iter().enumerate() {
        let c = Client::new(&node.url, &names[i], s.clone());
        tasks.push(tokio::spawn(async move {
            let mut ok = 0;
            for k in 0..150u64 {
                let side = if (i as u64 + k).is_multiple_of(2) { "buy" } else { "sell" };
                let px = 42_480 + ((k * 7 + i as u64 * 3) % 40);
                let o = if k % 5 == 0 {
                    Order::ioc(M, side, px, 1 + k % 9)
                } else {
                    Order::limit(M, side, px, 1 + k % 9)
                };
                if c.place(&o).await.is_ok() {
                    ok += 1;
                }
                if k % 10 == 9 {
                    c.cancel_all(Some(M)).await.unwrap();
                }
            }
            ok
        }));
    }
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap();
    }
    assert!(total > 1000, "accepted {total}");
    let snap = Client::new(&node.url, &names[0], signers[0].clone()).book(M, 5_000).await.unwrap();
    feed.until_seq(snap.seq).await.unwrap();
    let ws_bids: Vec<_> = feed.state.bids.iter().rev().map(|(p, q)| (*p, *q)).collect();
    let ws_asks: Vec<_> = feed.state.asks.iter().map(|(p, q)| (*p, *q)).collect();
    assert_eq!(
        (ws_bids, ws_asks),
        (snap.bids.clone(), snap.asks.clone()),
        "WS-maintained book equals REST snapshot"
    );
    if let (Some(b), Some(a)) = (snap.bids.first(), snap.asks.first()) {
        assert!(b.0 < a.0, "book not crossed");
    }
    assert_eq!(feed.resyncs, 0);
    node.stop().await;
}
