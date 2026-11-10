//! Perps through the node: manual marks, trading, positions/margin, mark-to-market batches,
//! liquidation over the account stream, funding, and restart.

use obkit_client::{Admin, Client, Feed, Order, Signer};
use obkit_node::config::{Config, MarketCfg, PerpCfg};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const TOKEN: &str = "perp-test-admin-token-xx";
const USDC: &str = "usdc.test.near";
const P: &str = "NEAR-PERP";
const USD: u128 = 1_000_000;

fn cfg(dir: &Path) -> Config {
    let mut c: Config = serde_json::from_value(json!({
        "listen": "127.0.0.1:0", "data_dir": dir, "fsync": true,
        "settlement_contract": "ob.test.near", "fee_account": "fees.test.near", "admin_token": TOKEN,
        "insurance_account": "insurance.test.near", "batch_interval_ms": 3600000, "funding_interval_ms": 3600000,
        "assets": [{"symbol": "NEAR", "token": "wrap.test.near", "decimals": 24}, {"symbol": "USDC", "token": USDC, "decimals": 6}],
        "markets": []
    }))
    .unwrap();
    c.markets.push(MarketCfg {
        symbol: P.into(),
        base: "NEAR".into(),
        quote: "USDC".into(),
        base_lot: "1".into(),
        quote_tick: "1".into(),
        min_qty: 1,
        max_qty: 100_000_000,
        max_price: 100_000_000,
        maker_fee_bps: 2,
        taker_fee_bps: 5,
        max_open_orders: 100,
        perp: Some(PerpCfg::default()),
    });
    c.validate().unwrap();
    c
}

async fn onboard(admin: &Admin, users: &[(&str, Option<&Signer>, u128)]) {
    let mut seq = admin.status().await.unwrap()["chain_cursor"].as_u64().unwrap();
    let mut evs: Vec<Value> = vec![];
    for (name, s, usd) in users {
        if let Some(s) = s {
            seq += 1;
            evs.push(json!({"type": "key_added", "seq": seq.to_string(), "account": name, "public_key": s.public_key()}));
        }
        seq += 1;
        evs.push(json!({"type": "deposit", "seq": seq.to_string(), "account": name, "token": USDC, "amount": (usd * USD).to_string()}));
    }
    let r = admin.chain_events(evs).await.unwrap();
    for x in r["results"].as_array().unwrap() {
        assert_eq!(x["result"]["type"], "ok", "{x}");
    }
}

async fn next_of(feed: &mut Feed, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let m = feed.next().await.unwrap();
            if m["type"] == kind {
                return m;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} message"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn perps_end_to_end_through_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let node = obkit_node::run(cfg(dir.path())).await.unwrap();
    let url = format!("http://{}", node.addr);
    let admin = Admin::new(&url, TOKEN);
    let (sa, sb, sc) = (Signer::generate(), Signer::generate(), Signer::generate());
    onboard(
        &admin,
        &[
            ("alice.test.near", Some(&sa), 1_000),
            ("bob.test.near", Some(&sb), 1_000),
            ("carol.test.near", Some(&sc), 60),
            ("insurance.test.near", None, 500),
        ],
    )
    .await;
    let alice = Client::new(&url, "alice.test.near", sa);
    let bob = Client::new(&url, "bob.test.near", sb);
    let carol = Client::new(&url, "carol.test.near", sc);

    let info = alice.info().await.unwrap();
    let pm = info.markets.iter().find(|m| m.symbol == P).unwrap();
    assert_eq!(pm.kind, "perp");
    // No mark yet.
    let e = alice.place(&Order::limit(P, "buy", 40_000, 1)).await.unwrap_err();
    assert!(e.to_string().contains("mark_unavailable"), "{e}");
    admin.set_mark(P, 40_000).await.unwrap();

    let mut perp_feed = Feed::connect(&url).await.unwrap();
    perp_feed.subscribe_perp(P).await.unwrap();
    let mut carol_feed = Feed::connect(&url).await.unwrap();
    carol_feed.send(carol.ws_auth()).await.unwrap();

    // Alice long 10,000 lots ($400) against Bob.
    bob.place(&Order::limit(P, "sell", 40_000, 10_000)).await.unwrap();
    alice.place(&Order::ioc(P, "buy", 40_000, 10_000)).await.unwrap();
    let av = alice.account().await.unwrap();
    assert_eq!(av.position(P), 10_000);
    assert_eq!(av.positions[0].entry_price, 40_000);
    assert_eq!(bob.account().await.unwrap().position(P), -10_000);

    // Mark to $4.20 over the perp channel; Alice is up $20.
    admin.set_mark(P, 42_000).await.unwrap();
    let m = next_of(&mut perp_feed, "mark").await;
    assert_eq!(m["mark"], 42_000);
    let av = alice.account().await.unwrap();
    assert_eq!(av.positions[0].unrealized_pnl, (20 * USD).to_string());
    let fee = (400 * USD * 5).div_ceil(10_000) as i128;
    assert_eq!(av.margin.equity(), (1_020 * USD) as i128 - fee);

    // The batch marks to market: deltas for Alice (+20 - fee), Bob (-20 - maker fee), fees.
    admin.cut().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let b = admin.batches().await.unwrap();
    let entries = b["batches"][0]["entries"].as_array().unwrap().clone();
    let d = |acct: &str| {
        entries
            .iter()
            .filter(|e| e["account"] == acct)
            .map(|e| e["delta"].as_str().unwrap().parse::<i128>().unwrap())
            .sum::<i128>()
    };
    assert_eq!(d("alice.test.near"), (20 * USD) as i128 - fee);
    assert_eq!(d("alice.test.near") + d("bob.test.near") + d("fees.test.near"), 0);

    // Carol: $60 at ~6.6x long, the book has a bid to absorb her liquidation.
    bob.place(&Order::limit(P, "sell", 42_000, 10_000)).await.unwrap();
    carol.place(&Order::ioc(P, "buy", 42_000, 10_000)).await.unwrap();
    bob.place(&Order::limit(P, "buy", 39_000, 20_000)).await.unwrap();
    // Equity = 60 - (4.20 - mark) * 100 - 0.21 fee; maintenance = 5% of mark * 100.
    admin.set_mark(P, 39_500).await.unwrap(); // equity 32.79 vs 19.75: alive
    admin.set_mark(P, 38_000).await.unwrap(); // equity 19.79 vs 19.00: alive
    assert_eq!(carol.account().await.unwrap().position(P), 10_000);
    admin.set_mark(P, 37_900).await.unwrap(); // equity 18.79 vs 18.95: liquidated
    // (The bid at $3.90 is within 3% of the mark, so it absorbs the close.)
    let liq = next_of(&mut carol_feed, "liquidated").await;
    assert!(liq["equity"].as_str().unwrap().parse::<i128>().unwrap() < liq["maintenance"].as_str().unwrap().parse::<i128>().unwrap());
    let cv = carol.account().await.unwrap();
    assert_eq!(cv.position(P), 0);
    assert!(cv.margin.equity() >= 0);

    // Funding: book mid vs index premium, perp channel announces it.
    admin.funding(P).await.unwrap();
    let f = next_of(&mut perp_feed, "funding").await;
    assert!(f["rate_ppm"].is_i64());
    let pi = alice.perp(P).await.unwrap();
    assert_eq!(pi.mark, 37_900);
    assert!(pi.open_interest >= 10_000);

    // Restart keeps positions and margin.
    let before = alice.account().await.unwrap();
    let addr = node.addr;
    node.shutdown().await;
    let mut c = cfg(dir.path());
    c.listen = addr.to_string();
    let node = obkit_node::run(c).await.unwrap();
    assert_eq!(alice.account().await.unwrap(), before);
    node.shutdown().await;
}
