//! Full stack on a local nearcore sandbox: clearing contract, operator node (journal, engine,
//! REST), settlement relayer, real signed orders over HTTP, a reporter-signed print, an open and a
//! close, a node crash and restart in the middle, and a withdrawal paid in real tokens. On-chain
//! balances are compared with independently computed reference math.
//!
//!   cargo test -p zfx-perps-near --test clearing_e2e -- --nocapture

mod common;

use anyhow::Result;
use base64::Engine as _;
use common::*;
use near_api::NearToken;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use zfx_perps_clearing::store::{Crypto, NativeCrypto};
use zfx_perps_clearing::testkit::{FORMULA, INDEX, Trader, model_params};
use zfx_perps_clearing::wire::{self, Side, SignedOrder, TimeInForce};
use zfx_perps_near::clearing::{Clearing, ClearingOptions, deploy_clearing};
use zfx_perps_near::clearing_adapter::ClearingAdapter;
use zfx_perps_near::sandbox::Sandbox;
use zfx_perps_near::{Actor, Ft, Near};
use zfx_perps_node::config::{ClearingCfg, Config};

const ADMIN: &str = "clearing-e2e-admin-token";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn order_json(o: &SignedOrder, t: &Trader) -> Value {
    json!({
        "chainId": o.chain_id, "clearingContract": o.clearing_contract, "accountId": o.account, "marketId": o.market,
        "side": o.side.as_str(), "type": o.order_type.as_str(), "price": o.price.to_string(), "quantity": o.qty.to_string(),
        "reduceOnly": o.reduce_only, "postOnly": o.post_only, "timeInForce": o.tif.as_str(), "nonce": o.nonce.to_string(),
        "expiryNs": o.expiry_ns.to_string(), "clientOrderId": o.client_order_id.to_string(), "maxFeeBps": o.max_fee_bps,
        "policyHash": wire::hex32(&o.policy_hash), "publicKey": pk_str(&t.key), "signature": b64(&o.signature.0),
    })
}

async fn wait<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

struct Api {
    url: String,
    http: reqwest::Client,
}

impl Api {
    async fn post(&self, path: &str, body: Value) -> Value {
        let r = self.http.post(format!("{}{path}", self.url)).bearer_auth(ADMIN).json(&body).send().await.unwrap();
        r.json().await.unwrap()
    }
    async fn get(&self, path: &str) -> Value {
        self.http.get(format!("{}{path}", self.url)).send().await.unwrap().json().await.unwrap()
    }
    async fn portfolio(&self, t: &Trader, chain: &str, contract: &str) -> Value {
        let ts = now_ns() / 1_000_000;
        let sig = t.key.sign(&wire::auth_message(chain, contract, &t.name, ts));
        self.http
            .get(format!("{}/v1/portfolio/{}", self.url, t.name))
            .header("x-zfx-perps-key", pk_str(&t.key))
            .header("x-zfx-perps-ts", ts.to_string())
            .header("x-zfx-perps-signature", b64(&sig))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

fn node_cfg(dir: &std::path::Path, contract: &str, collateral: &str, insurance: &str) -> Config {
    Config {
        listen: "127.0.0.1:0".into(),
        data_dir: dir.into(),
        fsync: true,
        settlement_contract: contract.into(),
        fee_account: contract.into(),
        admin_token: ADMIN.into(),
        batch_interval_ms: 300,
        snapshot_every: 50,
        max_batch_entries: 48,
        max_batch_withdrawals: 4,
        rate_limit_per_sec: 1_000,
        insurance_account: None,
        funding_interval_ms: 3_600_000,
        oracle: None,
        clearing: Some(ClearingCfg {
            chain_id: "near:sandbox".into(),
            contract: contract.into(),
            collateral: collateral.into(),
            collateral_decimals: 6,
            insurance_account: insurance.into(),
            settle_grace_ns: 300_000_000_000,
            max_order_ttl_ns: 30 * 86_400_000_000_000,
            funding_check_ms: 60_000,
            print_feed: None,
            print_poll_ms: 60_000,
        }),
        assets: vec![],
        markets: vec![],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_and_close_through_node_and_relayer() -> Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter("zfx_perps_near=info,zfx_perps_node=warn").try_init();
    let sb = Sandbox::start().await?;
    let near = Near::rpc(&sb.rpc_url)?;
    let rep = reporter();
    let stk = deploy_clearing(&near, &sb.rpc_url, &sb.root, &ClearingOptions { reporters: vec![pk_str(&rep.key)], ..Default::default() }).await?;
    let c = Clearing::new(near.clone(), &stk.contract);
    let owner = stk.owner.actor()?;
    let mut params = model_params();
    params.print_close_only_from_sec = 0;
    let m = params.market_id.clone();
    c.create_market(&owner, &params).await?;
    let pid = c.set_market_status(&owner, &m, "ACTIVE").await?.unwrap();
    c.execute_proposal(&owner, pid).await?;

    // Node and relayer.
    let dir = tempfile::tempdir()?;
    let cfg = node_cfg(dir.path(), &stk.contract, &stk.collateral, &stk.insurance_account);
    let mut running = zfx_perps_node::run(cfg.clone()).await.map_err(anyhow::Error::msg)?;
    let api = Api { url: format!("http://{}", running.addr), http: reqwest::Client::new() };
    let mut adapter = ClearingAdapter::new(c.clone(), stk.operator.actor()?, &api.url, ADMIN);
    adapter.poll = Duration::from_millis(300);
    let relayer = tokio::spawn(async move { adapter.run().await });
    wait("market listed on the node", 30, || async { api.get("/v1/markets").await["markets"].as_array().is_some_and(|v| !v.is_empty()) }).await;

    // Users: wallet actions on-chain only (account, test USDC, trading key, deposit).
    let ft = Ft { near: near.clone(), token: stk.collateral.clone() };
    let mut users: Vec<(Actor, Trader)> = vec![];
    for name in ["maker", "taker"] {
        let a = near.create_subaccount(&sb.root, name, NearToken::from_near(2)).await?;
        let t = Trader::new(a.id.as_str(), 1);
        near.call(&a, &stk.collateral, "mint", json!({"account_id": a.id, "amount": "1000000000"}), NearToken::from_millinear(10), 30).await?;
        c.register_key(&a, &pk_str(&t.key), None, true).await?;
        c.deposit(&a, &stk.collateral, 1_000_000_000, None).await?;
        users.push((a, t));
    }
    let (maker, taker) = (users[0].1.clone(), users[1].1.clone());
    let (chain, contract) = (stk.chain_id.clone(), stk.contract.clone());
    for t in [&maker, &taker] {
        wait("deposit credited on-chain", 60, || async { api.portfolio(t, &chain, &contract).await["settled"]["free"] == "1000000000" }).await;
    }

    // A reporter-signed print (mark 1000), relayed by the operator.
    let p = print(INDEX, FORMULA, 1_000_000_000, 1, [0; 32]);
    let r = api
        .post("/internal/oracle", json!({"market": m, "print_hex": wire::hex(&p), "signatures": [{"public_key": pk_str(&rep.key), "signature_hex": wire::hex(&rep.key.sign(&p))}]}))
        .await;
    assert_eq!(r["type"], "ok", "{r}");
    let policy = wire::parse_hex32(api.get("/v1/markets").await["markets"][0]["policy_hash"].as_str().unwrap()).unwrap();

    let mut nonce = now_ns();
    let mut mk = |t: &Trader, side: Side, price: u64, qty: u64, f: &dyn Fn(&mut SignedOrder)| {
        nonce += 1;
        let mut o = SignedOrder {
            chain_id: chain.clone(),
            clearing_contract: contract.clone(),
            account: t.name.clone(),
            market: m.clone(),
            side,
            order_type: wire::OrderType::Limit,
            price,
            qty,
            reduce_only: false,
            post_only: false,
            tif: TimeInForce::Gtc,
            nonce,
            expiry_ns: now_ns() + 3_600_000_000_000,
            client_order_id: nonce,
            max_fee_bps: 10,
            policy_hash: policy,
            public_key: t.key.public(),
            signature: wire::Sig64([0; 64]),
        };
        f(&mut o);
        o.signature = wire::Sig64(t.key.sign(&o.message()));
        o
    };

    // Open: maker rests a post-only ask, taker lifts it.
    let ask = mk(&maker, Side::Sell, 1_010, 100, &|o| o.post_only = true);
    let r = api.post("/v1/orders", order_json(&ask, &maker)).await;
    assert_eq!(r["status"], "resting", "{r}");
    assert_eq!(r["order_hash"], wire::hex32(&NativeCrypto.sha256(&ask.message())));
    let bid = mk(&taker, Side::Buy, 1_020, 100, &|o| o.tif = TimeInForce::Ioc);
    let r = api.post("/v1/orders", order_json(&bid, &taker)).await;
    assert_eq!((r["status"].as_str(), r["filled"].as_str()), (Some("filled"), Some("100")), "{r}");
    let size = |a: Value| a["positions"].as_array().and_then(|p| p.first()).map(|p| p["size"].as_str().unwrap().to_string());
    wait("open settled on-chain", 60, || async { size(c.account(&taker.name).await.unwrap()).as_deref() == Some("100") }).await;
    assert_eq!(size(c.account(&maker.name).await?).as_deref(), Some("-100"));

    // Crash the node after the open; restart from its journal; trading continues.
    running.shutdown().await;
    running = zfx_perps_node::run(cfg.clone()).await.map_err(anyhow::Error::msg)?;
    assert!(running.recovered_records > 0 || dir.path().join("journal").exists());
    let api = Api { url: format!("http://{}", running.addr), http: reqwest::Client::new() };
    relayer.abort();
    let mut adapter = ClearingAdapter::new(c.clone(), stk.operator.actor()?, &api.url, ADMIN);
    adapter.poll = Duration::from_millis(300);
    let relayer = tokio::spawn(async move { adapter.run().await });
    // The recovered engine still has the open (from its journal) and catches up with the chain.
    let pf = api.portfolio(&taker, &chain, &contract).await;
    assert_eq!(pf["pending"]["positions"][0]["size"], "100", "{pf}");
    wait("restarted node confirms the open", 60, || async {
        let pf = api.portfolio(&taker, &chain, &contract).await;
        pf["settled"]["positions"][0]["size"] == "100" && pf["unconfirmed_batches"] == 0
    })
    .await;

    // Close: maker bids, taker sells reduce-only.
    let bid = mk(&maker, Side::Buy, 1_040, 100, &|o| o.post_only = true);
    assert_eq!(api.post("/v1/orders", order_json(&bid, &maker)).await["status"], "resting");
    let ask = mk(&taker, Side::Sell, 1_000, 100, &|o| {
        o.tif = TimeInForce::Ioc;
        o.reduce_only = true
    });
    let r = api.post("/v1/orders", order_json(&ask, &taker)).await;
    assert_eq!(r["filled"], "100", "{r}");
    wait("close settled on-chain", 60, || async { c.account(&taker.name).await.unwrap()["positions"].as_array().is_some_and(|p| p.is_empty()) }).await;

    // Reference math, independent of the ledger code: taker bought 100 @1010 and sold 100 @1040,
    // both as taker (5 bps, rounded up); maker the other side as maker (2 bps).
    let (q, tv) = (100i128, 1_000i128);
    let fee = |n: i128, bps: i128| (n * bps + 9_999) / 10_000;
    let (n1, n2) = (q * 1_010 * tv, q * 1_040 * tv);
    let taker_free = 1_000_000_000 + (n2 - n1) - fee(n1, 5) - fee(n2, 5);
    let maker_free = 1_000_000_000 - (n2 - n1) - fee(n1, 2) - fee(n2, 2);
    let free = |v: Value| v["free"].as_str().unwrap().parse::<i128>().unwrap();
    assert_eq!(free(c.account(&taker.name).await?), taker_free);
    assert_eq!(free(c.account(&maker.name).await?), maker_free);
    let st = c.state().await?;
    assert_eq!(st["fee_balance"].as_str().unwrap().parse::<i128>()?, fee(n1, 5) + fee(n2, 5) + fee(n1, 2) + fee(n2, 2));
    assert_eq!(st["total_collateral"], "2000000000");

    // Withdraw the taker's profit in real tokens.
    let amount = taker_free as u128;
    let before = ft.balance(&taker.name).await?;
    let wmsg = wire::withdraw_message(&chain, &contract, &taker.name, &stk.collateral, amount, 1);
    let r = api
        .post("/v1/withdraw", json!({"accountId": taker.name, "amount": amount.to_string(), "nonce": "1", "publicKey": pk_str(&taker.key), "signature": b64(&taker.key.sign(&wmsg))}))
        .await;
    assert_eq!(r["type"], "ok", "{r}");
    wait("withdrawal paid", 60, || async { ft.balance(&taker.name).await.unwrap() == before + amount }).await;
    println!("taker deposited 1000.000000, withdrew {}.{:06}", amount / 1_000_000, amount % 1_000_000);
    relayer.abort();
    running.shutdown().await;
    sb.stop().await;
    Ok(())
}
