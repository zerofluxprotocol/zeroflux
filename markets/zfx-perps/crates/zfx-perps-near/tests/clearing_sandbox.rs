//! The clearing contract on a real local nearcore sandbox: custody attacks against the deployed
//! wasm, and gas per verified fill. Writes `target/clearing_gas.json` (`research/data/clearing_gas.json`
//! with ZFX_RECORD=1).
//!
//!   cargo test -p zfx-perps-near --test clearing_sandbox -- --nocapture

mod common;

use anyhow::Result;
use common::*;
use near_api::NearToken;
use serde_json::json;
use zfx_perps_clearing::testkit::{FORMULA, INDEX, model_params};
use zfx_perps_clearing::wire::Side;
use zfx_perps_clearing::*;
use zfx_perps_near::clearing::{Clearing, ClearingOptions, deploy_clearing};
use zfx_perps_near::sandbox::Sandbox;
use zfx_perps_near::{Ft, Near};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clearing_contract_on_sandbox() -> Result<()> {
    let sb = Sandbox::start().await?;
    let near = Near::rpc(&sb.rpc_url)?;
    let rep = reporter();
    let opts = ClearingOptions {
        reporters: vec![pk_str(&rep.key)],
        force_timeout_ns: 3_600_000_000_000,
        ..Default::default()
    };
    let stk = deploy_clearing(&near, &sb.rpc_url, &sb.root, &opts).await?;
    let c = Clearing::new(near.clone(), &stk.contract);
    let owner = stk.owner.actor()?;
    let operator = stk.operator.actor()?;
    // The daily close-only window is covered by unit tests; off here so the test runs at any hour.
    let mut params = model_params();
    params.print_close_only_from_sec = 0;
    params.mark_limit_bps = 600;
    params.max_open_interest = 10_000_000;
    params.max_position = 1_000_000;
    let m = params.market_id.clone();
    c.create_market(&owner, &params).await?;
    let pid = c.set_market_status(&owner, &m, "ACTIVE").await?.expect("activation is timelocked");
    c.execute_proposal(&owner, pid).await?;
    let mut x = Ctx { near: near.clone(), stk: stk.clone(), c: c.clone(), operator, owner, policy: [0; 32], nonce: 1, batch: 0, market: m.clone() };
    x.refresh_policy().await?;
    let p = print(INDEX, FORMULA, 1_000_000_000, 1, [0; 32]);
    let (_, _, rej) = x.settle(vec![], vec![oracle_op(&m, &p, &rep)]).await?;
    assert!(rej.is_empty(), "{rej:?}");
    assert_eq!(c.market(&m).await?["mark"], "1000");

    let mut users = vec![];
    for i in 0..8 {
        users.push(x.user(&sb.root, &format!("u{i}"), 10_000_000_000).await?);
    }
    let (ua, a) = users[0].clone();
    let (_ub, b) = users[1].clone();

    // ---- custody attacks against the deployed contract ----
    // Wrong token: a second mock token is refunded by ft_on_transfer.
    let other = near.create_subaccount(&sb.root, "fake", NearToken::from_near(3)).await?;
    near.deploy(&other, std::fs::read(zfx_perps_near::stack::out_dir().join("mock_ft.wasm"))?).await?;
    near.call(&other, other.id.as_str(), "new", json!({"owner": sb.root.id, "name": "Fake", "symbol": "FAKE", "decimals": 6, "faucet_limit": "1000000000"}), NearToken::from_yoctonear(0), 30).await?;
    let fake = Ft { near: near.clone(), token: other.id.to_string() };
    near.call(&ua, other.id.as_str(), "mint", json!({"account_id": ua.id, "amount": "1000"}), NearToken::from_millinear(10), 30).await?;
    fake.storage_deposit(&sb.root, &stk.contract).await?;
    assert!(c.deposit(&ua, other.id.as_str(), 1_000, None).await.is_err(), "wrong token must be refunded");
    assert_eq!(fake.balance(ua.id.as_str()).await?, 1_000);

    let sell = x.order(&b, Side::Sell, 1_000, 100, |_| {});
    let buy = x.order(&a, Side::Buy, 1_000, 100, |_| {});
    let mut forged = buy.clone();
    forged.qty = 10_000;
    let wrong_chain = x.order(&a, Side::Buy, 1_000, 100, |o| o.chain_id = "near:mainnet".into());
    let wrong_contract = x.order(&a, Side::Buy, 1_000, 100, |o| o.clearing_contract = "other.test.near".into());
    let (_, tx, rej) = x
        .settle(
            vec![sell.clone(), buy.clone(), forged, wrong_chain, wrong_contract],
            vec![
                Op::Fill { maker: 0, taker: 2, qty: 100, price: 1_000 },
                Op::Fill { maker: 0, taker: 3, qty: 100, price: 1_000 },
                Op::Fill { maker: 0, taker: 4, qty: 100, price: 1_000 },
                Op::Fill { maker: 0, taker: 1, qty: 100, price: 1_000 },
                Op::Fill { maker: 0, taker: 1, qty: 1, price: 1_000 },
            ],
        )
        .await?;
    println!("attack batch tx {tx}");
    assert_eq!(
        rej,
        vec![(0, "bad_signature".into()), (1, "wrong_chain".into()), (2, "wrong_contract".into()), (4, "overfill".into())]
    );
    assert_eq!((x.size(a.name.as_str()).await?, x.size(b.name.as_str()).await?), (100, -100));
    // Fees exactly once: 2 + 5 bps of 100 lots × 1000 ticks × 1000.
    assert_eq!(c.state().await?["fee_balance"], "70000");
    // Double settlement: an already-used batch id fails the transaction.
    x.batch -= 1;
    assert!(x.settle(vec![], vec![]).await.is_err());
    // A non-operator can't settle.
    let mut y = Ctx { near: near.clone(), stk: stk.clone(), c: c.clone(), operator: ua.clone(), owner: ua.clone(), policy: x.policy, nonce: 0, batch: x.batch, market: m.clone() };
    assert!(y.settle(vec![], vec![]).await.is_err());

    // Withdraw pays the owner exactly, once.
    let ft = Ft { near: near.clone(), token: stk.collateral.clone() };
    let before = ft.balance(ua.id.as_str()).await?;
    let msg = wire::withdraw_message(&stk.chain_id, &stk.contract, ua.id.as_str(), &stk.collateral, 123_456, 1);
    let w = Op::Withdraw { account: ua.id.to_string(), amount: 123_456, nonce: 1, public_key: a.key.public(), signature: wire::Sig64(a.key.sign(&msg)) };
    let (wg, _, rej) = x.settle(vec![], vec![w.clone(), w]).await?;
    assert_eq!(rej, vec![(1, "nonce_used".into())]);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(ft.balance(ua.id.as_str()).await? - before, 123_456);

    // ---- gas per verified fill ----
    let mut rows = vec![];
    rows.push(json!({"case": "withdrawal (1 payout)", "ops": 1, "tgas": wg}));
    let traders: Vec<_> = users.iter().map(|(_, t)| t.clone()).collect();
    // k independent fills: every fill brings two new orders (two signature checks).
    for k in [1usize, 10, 25, 40] {
        let mut orders = vec![];
        let mut ops = vec![];
        for i in 0..k {
            let (mi, ti) = (2 + (i % 3) * 2, 3 + (i % 3) * 2);
            let s = x.order(&traders[mi], Side::Sell, 1_000, 10, |_| {});
            let t = x.order(&traders[ti], Side::Buy, 1_000, 10, |_| {});
            ops.push(Op::Fill { maker: orders.len() as u32, taker: orders.len() as u32 + 1, qty: 10, price: 1_000 });
            orders.push(s);
            orders.push(t);
        }
        let (g, tx, rej) = x.settle(orders, ops).await?;
        assert!(rej.is_empty(), "{rej:?}");
        println!("{k:>3} fills, 2 new orders each: {g:.2} Tgas ({:.2}/fill) tx {tx}", g / k as f64);
        rows.push(json!({"case": "independent fills", "fills": k, "new_orders": 2 * k, "tgas": g, "tgas_per_fill": g / k as f64}));
    }
    // One resting maker swept by k takers: k + 1 signature checks.
    for k in [10usize, 40] {
        let maker = x.order(&traders[6], Side::Sell, 1_000, 10 * k as u64, |_| {});
        let mut orders = vec![maker];
        let mut ops = vec![];
        for i in 0..k {
            orders.push(x.order(&traders[[2, 3, 4, 5, 7][i % 5]], Side::Buy, 1_000, 10, |_| {}));
            ops.push(Op::Fill { maker: 0, taker: i as u32 + 1, qty: 10, price: 1_000 });
        }
        let (g, tx, rej) = x.settle(orders, ops).await?;
        assert!(rej.is_empty(), "{rej:?}");
        println!("{k:>3} fills against one maker: {g:.2} Tgas ({:.2}/fill) tx {tx}", g / k as f64);
        rows.push(json!({"case": "one maker swept", "fills": k, "new_orders": k + 1, "tgas": g, "tgas_per_fill": g / k as f64}));
    }
    // An oracle print alone, and a liquidation.
    let cur = c.market(&m).await?["print"]["hash"].as_str().unwrap().to_string();
    let p2 = print(INDEX, FORMULA, 1_010_000_000, 2, wire::parse_hex32(&cur).unwrap());
    let (g, _, rej) = x.settle(vec![], vec![oracle_op(&m, &p2, &rep)]).await?;
    assert!(rej.is_empty(), "{rej:?}");
    rows.push(json!({"case": "oracle print (1 reporter signature)", "ops": 1, "tgas": g}));
    println!("oracle print: {g:.2} Tgas");

    let out = json!({"measured_at_unix": now_ns() / 1_000_000_000, "chain": "local nearcore sandbox", "wasm_bytes":
        std::fs::metadata(zfx_perps_near::stack::out_dir().join("zfx_perps_clearing.wasm"))?.len(), "rows": rows});
    // The committed measurement is only replaced on request (ZFX_RECORD=1); otherwise the
    // numbers go to target/ so a test run leaves the checkout clean.
    let path = if std::env::var("ZFX_RECORD").as_deref() == Ok("1") {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../research/data/clearing_gas.json").to_string()
    } else {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/clearing_gas.json").to_string()
    };
    std::fs::write(&path, serde_json::to_string_pretty(&out)? + "\n")?;
    println!("gas table: {path}");
    sb.stop().await;
    Ok(())
}
