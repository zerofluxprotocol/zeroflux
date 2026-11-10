//! End-to-end on a local nearcore sandbox: settlement contract + operator node + adapter + users.
//!
//! Covers: deposits reaching the engine, trading, netted batches settling on-chain with on-chain
//! balances equal to engine balances, signed withdrawals paying out, a withdrawal that can't pay
//! out (receiver not registered with the token) being re-credited, a forced withdrawal honored by
//! the operator, a node restart mid-stream, and finally the operator going dark: a forced
//! withdrawal times out, anyone freezes the contract, users escape with their settled balances.
//!
//! Needs out/*.wasm (scripts/build-contracts.sh) and a near-sandbox binary (see sandbox.rs).

use anyhow::Result;
use near_api::NearToken;
use obkit_client::{Ack, Admin, Client, Order, Signer};
use obkit_near::adapter::Adapter;
use obkit_near::sandbox::Sandbox;
use obkit_near::stack::{self, StackOptions};
use obkit_near::{Actor, Ft, Near, Settlement};
use obkit_node::config::{AssetCfg, Config, MarketCfg};
use std::path::Path;
use std::time::{Duration, Instant};

const ADMIN: &str = "e2e-admin-token-not-secret";
const M: &str = "NEAR-USDC";
const LOT: u128 = 10_000_000_000_000_000_000_000; // 0.01 NEAR
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

fn node_cfg(dir: &Path, s: &stack::Stack) -> Config {
    Config {
        listen: "127.0.0.1:0".into(),
        data_dir: dir.to_path_buf(),
        fsync: true,
        settlement_contract: s.contract.clone(),
        fee_account: s.fees.account.clone(),
        admin_token: ADMIN.into(),
        batch_interval_ms: 300,
        snapshot_every: 1_000,
        max_batch_entries: 48,
        max_batch_withdrawals: 4,
        rate_limit_per_sec: 1_000,
        insurance_account: None,
        funding_interval_ms: 3_600_000,
        oracle: None,
        assets: s
            .tokens
            .iter()
            .map(|t| AssetCfg {
                symbol: t.symbol.clone(),
                token: t.token.clone(),
                decimals: t.decimals,
            })
            .collect(),
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

struct User {
    actor: Actor,
    client: Client,
}

async fn user(near: &Near, root: &Actor, name: &str, url: &str, set: &Settlement) -> Result<User> {
    let actor = near.create_subaccount(root, name, NearToken::from_near(5)).await?;
    let signer = Signer::generate();
    set.register_key(&actor, &signer.public_key()).await?;
    Ok(User {
        client: Client::new(url, actor.id.as_str(), signer),
        actor,
    })
}

/// Engine totals equal on-chain settled balances once nothing is pending.
async fn assert_reconciled(set: &Settlement, users: &[&User], fees: &Client, tokens: &[&str]) {
    for u in users {
        let v = u.client.account().await.unwrap();
        for t in tokens {
            assert_eq!(
                v.total(t),
                set.balance(u.actor.id.as_str(), t).await.unwrap(),
                "{} {t}: engine vs chain",
                u.actor.id
            );
        }
    }
    let _ = fees;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_lifecycle_on_sandbox() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("obkit_near=info,obkit_node=warn")
        .try_init();
    let sb = Sandbox::start().await?;
    let near = Near::rpc(&sb.rpc_url)?;
    let stk = stack::deploy(
        &near,
        &sb.rpc_url,
        &sb.root,
        &StackOptions {
            force_timeout_ns: 25_000_000_000,
            ..Default::default()
        },
    )
    .await?;
    let set = Settlement::new(near.clone(), &stk.contract);
    let (wnear, usdc) = (stk.tokens[0].token.clone(), stk.tokens[1].token.clone());
    let (ft_near, ft_usdc) = (
        Ft {
            near: near.clone(),
            token: wnear.clone(),
        },
        Ft {
            near: near.clone(),
            token: usdc.clone(),
        },
    );

    // Node + adapter.
    let dir = tempfile::tempdir()?;
    let node = obkit_node::run(node_cfg(dir.path(), &stk)).await.map_err(anyhow::Error::msg)?;
    let url = format!("http://{}", node.addr);
    let mut adapter = Adapter::new(set.clone(), stk.operator.actor()?, Admin::new(&url, ADMIN));
    adapter.poll = Duration::from_millis(200);
    let adapter_task = tokio::spawn(async move { adapter.run().await });

    // Users: keys registered on-chain, then deposits.
    let alice = user(&near, &sb.root, "alice", &url, &set).await?;
    let bob = user(&near, &sb.root, "bob", &url, &set).await?;
    for (u, t, amt) in [
        (&alice, &ft_near, 1_000 * LOT),
        (&alice, &ft_usdc, 100 * USD),
        (&bob, &ft_near, 10 * LOT),
        (&bob, &ft_usdc, 5_000 * USD),
    ] {
        t.mint(&u.actor, amt).await?;
        set.deposit(&u.actor, &t.token, amt, None).await?;
    }
    wait("deposits in the engine", 60, || async {
        bob.client.account().await.is_ok_and(|v| v.total(&usdc) == 5_000 * USD)
    })
    .await;
    assert_eq!(alice.client.account().await?.total(&wnear), 1_000 * LOT);

    // Trade: alice sells 300 lots (3 NEAR) at $4.25 and $4.26; bob takes 250.
    alice.client.place(&Order::limit(M, "sell", 42_500, 200)).await?;
    alice.client.place(&Order::limit(M, "sell", 42_600, 100)).await?;
    let t0 = Instant::now();
    let ack = bob.client.place(&Order::ioc(M, "buy", 42_600, 250)).await?;
    assert_eq!(
        ack,
        Ack::Placed {
            order_id: 3,
            status: "filled".into(),
            filled: 250,
            remaining: 0
        }
    );
    let ack_ms = t0.elapsed().as_secs_f64() * 1e3;
    wait("first batch settled on-chain", 60, || async {
        set.state().await.is_ok_and(|s| s["last_batch_id"] != "0")
    })
    .await;
    let settle_s = t0.elapsed().as_secs_f64();
    println!("order ack {ack_ms:.1} ms over HTTP; settled on-chain after {settle_s:.2} s");
    wait("node sees confirmation", 30, || async {
        Admin::new(&url, ADMIN)
            .status()
            .await
            .is_ok_and(|s| s["unconfirmed"] == 0 && s["pending"] == false)
    })
    .await;
    let fees_client = Client::new(&url, &stk.fees.account, Signer::generate());
    assert_reconciled(&set, &[&alice, &bob], &fees_client, &[&wnear, &usdc]).await;
    let notional = 200 * 42_500u128 + 50 * 42_600;
    let fee_total = (200 * 42_500u128 * 5).div_ceil(10_000)
        + (50 * 42_600u128 * 5).div_ceil(10_000)
        + (200 * 42_500u128 * 2).div_ceil(10_000)
        + (50 * 42_600u128 * 2).div_ceil(10_000);
    assert_eq!(set.balance(&stk.fees.account, &usdc).await?, fee_total);
    assert_eq!(set.balance(bob.actor.id.as_str(), &wnear).await?, 10 * LOT + 250 * LOT);
    assert!(set.balance(bob.actor.id.as_str(), &usdc).await? < 5_000 * USD - notional + 1);

    // Signed withdrawal pays out to bob's own account.
    let before = ft_usdc.balance(bob.actor.id.as_str()).await?;
    assert_eq!(
        bob.client.withdraw_next(&usdc, 1_234 * USD).await?,
        Ack::WithdrawQueued { nonce: 1 }
    );
    wait("bob's withdrawal paid", 60, || async {
        ft_usdc
            .balance(bob.actor.id.as_str())
            .await
            .is_ok_and(|b| b == before + 1_234 * USD)
    })
    .await;
    assert_eq!(set.withdraw_nonce(bob.actor.id.as_str()).await?, 1);

    // A withdrawal that can't pay out is re-credited: dave was credited by deposit-for-beneficiary
    // but never registered with the token contract, so ft_transfer to him fails.
    let dave = user(&near, &sb.root, "dave", &url, &set).await?;
    ft_usdc.mint(&alice.actor, 50 * USD).await?;
    set.deposit(&alice.actor, &usdc, 50 * USD, Some(dave.actor.id.as_str())).await?;
    wait("dave's deposit", 60, || async {
        dave.client.account().await.is_ok_and(|v| v.total(&usdc) == 50 * USD)
    })
    .await;
    dave.client.withdraw_next(&usdc, 20 * USD).await?;
    wait("dave's failed payout re-credited", 90, || async {
        let v = dave.client.account().await;
        v.is_ok_and(|v| v.total(&usdc) == 50 * USD) && set.balance(dave.actor.id.as_str(), &usdc).await.is_ok_and(|b| b == 50 * USD)
    })
    .await;
    assert_eq!(ft_usdc.balance(dave.actor.id.as_str()).await?, 0);

    // Forced withdrawal through the chain: the operator must honor it; alice's resting orders are
    // canceled first so the full NEAR balance is available.
    alice.client.place(&Order::limit(M, "sell", 50_000, 100)).await?;
    let a_before = ft_near.balance(alice.actor.id.as_str()).await?;
    let a_engine = alice.client.account().await?.total(&wnear);
    let rid = set.request_forced_withdrawal(&alice.actor, &wnear, u128::MAX / 2).await?;
    wait("forced withdrawal honored", 60, || async {
        ft_near
            .balance(alice.actor.id.as_str())
            .await
            .is_ok_and(|b| b == a_before + a_engine)
    })
    .await;
    let req: serde_json::Value = near
        .view(&stk.contract, "get_forced", serde_json::json!({"request_id": rid.to_string()}))
        .await?;
    assert_eq!(req["done"], true);
    assert!(alice.client.account().await?.open_orders.is_empty());

    // Node restart: stop it, start again from the journal, adapter keeps going.
    adapter_task.abort();
    let addr = node.addr;
    node.shutdown().await;
    let mut cfg = node_cfg(dir.path(), &stk);
    cfg.listen = addr.to_string();
    let node2 = obkit_node::run(cfg).await.map_err(anyhow::Error::msg)?;
    assert!(
        node2.recovered_records > 0 || std::fs::read_dir(dir.path().join("journal"))?.count() > 1,
        "restarted from disk"
    );
    let mut adapter = Adapter::new(set.clone(), stk.operator.actor()?, Admin::new(&url, ADMIN));
    adapter.poll = Duration::from_millis(200);
    let adapter_task = tokio::spawn(async move { adapter.run().await });
    ft_usdc.mint(&bob.actor, 10 * USD).await?;
    set.deposit(&bob.actor, &usdc, 10 * USD, None).await?;
    let want = set.balance(bob.actor.id.as_str(), &usdc).await?;
    wait("deposit after restart", 60, || async {
        bob.client.account().await.is_ok_and(|v| v.total(&usdc) == want)
    })
    .await;
    bob.client.place(&Order::limit(M, "sell", 42_000, 10)).await?;
    alice.client.place(&Order::ioc(M, "buy", 42_000, 10)).await.ok(); // alice has no NEAR left but has USDC
    wait("post-restart batches settled", 60, || async {
        Admin::new(&url, ADMIN)
            .status()
            .await
            .is_ok_and(|s| s["unconfirmed"] == 0 && s["pending"] == false)
    })
    .await;
    assert_reconciled(&set, &[&alice, &bob, &dave], &fees_client, &[&wnear, &usdc]).await;

    // Operator goes dark. Bob files a forced withdrawal nobody processes; after the timeout anyone
    // freezes the contract and every user escapes with their settled balance.
    adapter_task.abort();
    node2.shutdown().await;
    set.request_forced_withdrawal(&bob.actor, &usdc, 1).await?;
    assert!(set.freeze(&dave.actor).await.is_err(), "not overdue yet");
    tokio::time::sleep(Duration::from_secs(27)).await;
    set.freeze(&dave.actor).await?;
    assert_eq!(set.state().await?["frozen"], true);
    for (u, t) in [(&bob, &ft_usdc), (&bob, &ft_near), (&alice, &ft_usdc)] {
        let owed = set.balance(u.actor.id.as_str(), &t.token).await?;
        let before = t.balance(u.actor.id.as_str()).await?;
        set.escape_withdraw(&u.actor, &t.token).await?;
        assert_eq!(
            t.balance(u.actor.id.as_str()).await?,
            before + owed,
            "{} escaped {}",
            u.actor.id,
            t.token
        );
        assert_eq!(set.balance(u.actor.id.as_str(), &t.token).await?, 0);
    }
    assert!(set.settle_batch(&stk.operator.actor()?, &serde_json::json!({"id": "99", "entries": [], "withdrawals": [], "forced": [], "seal": true, "journal_seq": "0", "journal_hash": ""})).await.is_err());
    sb.stop().await;
    Ok(())
}
