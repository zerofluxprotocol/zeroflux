//! Perps end to end on a local nearcore sandbox. A long and a short trade NEAR-PERP, the mark
//! moves, batches settle PnL on-chain, the winner withdraws more USDC than they deposited (paid
//! out of the loser's settled balance), and an under-margined account is liquidated with the
//! chain staying in lockstep with the engine.

use anyhow::Result;
use near_api::NearToken;
use zfx_perps_client::{Admin, Client, Order, Signer};
use zfx_perps_near::adapter::Adapter;
use zfx_perps_near::sandbox::Sandbox;
use zfx_perps_near::stack::{self, StackOptions};
use zfx_perps_near::{Actor, Ft, Near, Settlement};
use zfx_perps_node::config::{AssetCfg, Config, MarketCfg, PerpCfg};
use std::time::{Duration, Instant};

const ADMIN: &str = "e2e-perp-admin-token-xyz";
const P: &str = "NEAR-PERP";
const USD: u128 = 1_000_000;

async fn wait<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn settled(url: &str) -> bool {
    Admin::new(url, ADMIN)
        .status()
        .await
        .is_ok_and(|s| s["unconfirmed"] == 0 && s["pending"] == false)
}

struct User {
    actor: Actor,
    client: Client,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn perps_settle_real_tokens_on_sandbox() -> Result<()> {
    let _ = tracing_subscriber::fmt().with_env_filter("zfx_perps_near=info").try_init();
    let sb = Sandbox::start().await?;
    let near = Near::rpc(&sb.rpc_url)?;
    let stk = stack::deploy(&near, &sb.rpc_url, &sb.root, &StackOptions::default()).await?;
    let set = Settlement::new(near.clone(), &stk.contract);
    let usdc = stk.tokens[1].token.clone();
    let ft = Ft {
        near: near.clone(),
        token: usdc.clone(),
    };
    let insurance = stk.insurance.clone().unwrap();

    let dir = tempfile::tempdir()?;
    let cfg = Config {
        listen: "127.0.0.1:0".into(),
        data_dir: dir.path().into(),
        fsync: true,
        settlement_contract: stk.contract.clone(),
        fee_account: stk.fees.account.clone(),
        admin_token: ADMIN.into(),
        batch_interval_ms: 300,
        snapshot_every: 1_000,
        max_batch_entries: 48,
        max_batch_withdrawals: 4,
        rate_limit_per_sec: 1_000,
        insurance_account: Some(insurance.account.clone()),
        funding_interval_ms: 3_600_000,
        oracle: None,
        cors_allow: vec!["*".into()],
        clearing: None,
        assets: stk
            .tokens
            .iter()
            .map(|t| AssetCfg {
                symbol: t.symbol.clone(),
                token: t.token.clone(),
                decimals: t.decimals,
            })
            .collect(),
        markets: vec![MarketCfg {
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
        }],
    };
    let node = zfx_perps_node::run(cfg).await.map_err(anyhow::Error::msg)?;
    let url = format!("http://{}", node.addr);
    let admin = Admin::new(&url, ADMIN);
    let mut adapter = Adapter::new(set.clone(), stk.operator.actor()?, Admin::new(&url, ADMIN));
    adapter.poll = Duration::from_millis(200);
    let adapter_task = tokio::spawn(async move { adapter.run().await });

    let mut users = vec![];
    for (name, usd) in [("alice", 1_000), ("bob", 1_000), ("carol", 60)] {
        let actor = near.create_subaccount(&sb.root, name, NearToken::from_near(5)).await?;
        let signer = Signer::generate();
        set.register_key(&actor, &signer.public_key()).await?;
        ft.mint(&actor, usd * USD).await?;
        set.deposit(&actor, &usdc, usd * USD, None).await?;
        users.push(User {
            client: Client::new(&url, actor.id.as_str(), signer),
            actor,
        });
    }
    // Seed the insurance fund with $500 (deposit on its behalf).
    ft.mint(&sb.root, 500 * USD).await?;
    set.deposit(&sb.root, &usdc, 500 * USD, Some(&insurance.account)).await?;
    let (alice, bob, carol) = (&users[0], &users[1], &users[2]);
    wait("deposits", 60, || async {
        carol.client.account().await.is_ok_and(|v| v.total(&usdc) == 60 * USD)
    })
    .await;

    // Alice long $400 against Bob at $4.00; the mark goes to $4.40 (+$40); she closes there.
    admin.set_mark(P, 40_000).await?;
    bob.client.place(&Order::limit(P, "sell", 40_000, 10_000)).await?;
    alice.client.place(&Order::ioc(P, "buy", 40_000, 10_000)).await?;
    admin.set_mark(P, 44_000).await?;
    bob.client.place(&Order::limit(P, "buy", 44_000, 10_000)).await?;
    alice.client.place(&Order::ioc(P, "sell", 44_000, 10_000)).await?;
    wait("perp PnL settled on-chain", 60, || settled(&url)).await;
    // Taker fees 5 bps on $400 and $440, both paid by Alice; Bob paid 2 bps maker on each.
    let alice_fee = (400 * USD * 5).div_ceil(10_000) + (440 * USD * 5).div_ceil(10_000);
    let bob_fee = (400 * USD * 2).div_ceil(10_000) + (440 * USD * 2).div_ceil(10_000);
    assert_eq!(set.balance(alice.actor.id.as_str(), &usdc).await?, 1_040 * USD - alice_fee);
    assert_eq!(set.balance(bob.actor.id.as_str(), &usdc).await?, 960 * USD - bob_fee);
    assert_eq!(set.balance(&stk.fees.account, &usdc).await?, alice_fee + bob_fee);
    // Chain equals engine for each user (collateral + settled perp value).
    for u in &users {
        let v = u.client.account().await?;
        let engine = v.total(&usdc) as i128 + v.margin.settled_perp_value.parse::<i128>()?;
        assert_eq!(set.balance(u.actor.id.as_str(), &usdc).await? as i128, engine, "{}", u.actor.id);
    }

    // Alice withdraws everything: more than she deposited.
    let free: u128 = alice.client.account().await?.margin.free().try_into()?;
    assert_eq!(free, 1_040 * USD - alice_fee);
    let before = ft.balance(alice.actor.id.as_str()).await?;
    alice.client.withdraw_next(&usdc, free).await?;
    wait("profit paid out on-chain", 60, || async {
        ft.balance(alice.actor.id.as_str()).await.is_ok_and(|b| b == before + free)
    })
    .await;
    assert!(free > 1_000 * USD, "withdrew {free}, deposited {}", 1_000 * USD);
    assert_eq!(set.balance(alice.actor.id.as_str(), &usdc).await?, 0);

    // Carol: $60 of collateral, long 100 NEAR ($440) at $4.40; Bob bids $4.30 below.
    // Equity = 60 - (4.40 - mark) * 100 - 0.22 fee; maintenance = 5% of mark * 100.
    bob.client.place(&Order::limit(P, "sell", 44_000, 10_000)).await?;
    carol.client.place(&Order::ioc(P, "buy", 44_000, 10_000)).await?;
    bob.client.place(&Order::limit(P, "buy", 43_000, 20_000)).await?;
    admin.set_mark(P, 42_200).await?; // equity 41.78 vs 21.10: alive
    assert_eq!(carol.client.account().await?.position(P), 10_000);
    admin.set_mark(P, 40_000).await?; // equity 19.78 vs 20.00: liquidated
    // The liquidation sells at Bob's $4.30 bid (inside the 3% slippage bound) and pays a 1% fee
    // on $400 to insurance.
    wait("carol liquidated", 10, || async {
        carol.client.account().await.is_ok_and(|v| v.position(P) == 0)
    })
    .await;
    wait("liquidation settled", 60, || settled(&url)).await;
    let cv = carol.client.account().await?;
    let engine = cv.total(&usdc) as i128 + cv.margin.settled_perp_value.parse::<i128>()?;
    assert!(engine >= 0);
    assert_eq!(set.balance(carol.actor.id.as_str(), &usdc).await? as i128, engine);
    let ins_onchain = set.balance(&insurance.account, &usdc).await?;
    assert!(ins_onchain >= 500 * USD, "insurance gained the liquidation fee: {ins_onchain}");

    // Conservation on-chain: everything still in the contract plus Alice's payout equals deposits.
    let mut onchain = set.balance(&stk.fees.account, &usdc).await? + ins_onchain;
    for u in &users {
        onchain += set.balance(u.actor.id.as_str(), &usdc).await?;
    }
    assert_eq!(onchain + free, 2_060 * USD + 500 * USD);
    println!(
        "alice deposited $1000, withdrew ${:.2}; insurance ${:.2}",
        free as f64 / 1e6,
        ins_onchain as f64 / 1e6
    );
    adapter_task.abort();
    node.shutdown().await;
    sb.stop().await;
    Ok(())
}
