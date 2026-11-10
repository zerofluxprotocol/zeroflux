//! Gas benchmark on a local nearcore sandbox: a fully on-chain CLOB (Tonic, the exact wasm
//! deployed at v1.orderbook.near, code hash 2HL5pnkfUhyeAjmjAK9XPjNxH3WaGxSnbwaYVn6kNiY9) against
//! obkit's netted settlement.
//!
//! - Tonic: gas per resting order as the book grows, per taker order sweeping k makers, per cancel.
//! - obkit: gas per settle_batch by entry count, and per signed withdrawal.
//!
//! TONIC_WASM=~/obkit-research/tonic-core/res/tonic-dex.wasm cargo run --release -p obkit-near --example onchain_bench
//! Writes research/data/onchain_bench.json.

use anyhow::{Result, bail};
use futures::future::join_all;
use near_api::NearToken;
use obkit_client::Signer;
use obkit_near::sandbox::Sandbox;
use obkit_near::{Actor, Ft, Near, Settlement};
use serde_json::{Value, json};

const ONE_BASE: u128 = 10u128.pow(16);
const ONE_QUOTE: u128 = 10u128.pow(18);

fn tg(g: u64) -> f64 {
    g as f64 / 1e12
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

async fn token(near: &Near, root: &Actor, name: &str, decimals: u8) -> Result<String> {
    let t = near.create_subaccount(root, name, NearToken::from_near(20)).await?;
    near.deploy(&t, std::fs::read(obkit_near::stack::out_dir().join("mock_ft.wasm"))?)
        .await?;
    near.call(
        &t,
        t.id.as_str(),
        "new",
        json!({"owner": root.id, "name": name, "symbol": name.to_uppercase(), "decimals": decimals, "faucet_limit": u128::MAX.to_string()}),
        NearToken::from_yoctonear(0),
        30,
    )
    .await?;
    Ok(t.id.to_string())
}

async fn fund(near: &Near, a: &Actor, token: &str, amount: u128, to: &str) -> Result<()> {
    let ft = Ft {
        near: near.clone(),
        token: token.into(),
    };
    ft.mint(a, amount).await?;
    near.call(
        a,
        token,
        "ft_transfer_call",
        json!({"receiver_id": to, "amount": amount.to_string(), "msg": ""}),
        NearToken::from_yoctonear(1),
        150,
    )
    .await?;
    Ok(())
}

async fn tonic(near: &Near, root: &Actor, base: &str, quote: &str) -> Result<Value> {
    let wasm_path = std::env::var("TONIC_WASM")
        .unwrap_or_else(|_| format!("{}/obkit-research/tonic-core/res/tonic-dex.wasm", std::env::var("HOME").unwrap()));
    let dex = near.create_subaccount(root, "tonic", NearToken::from_near(200)).await?;
    near.deploy(&dex, std::fs::read(&wasm_path)?).await?;
    let d = dex.id.to_string();
    near.call(&dex, &d, "new", json!({"owner_id": root.id}), NearToken::from_yoctonear(0), 30)
        .await?;
    for t in [base, quote] {
        Ft {
            near: near.clone(),
            token: t.into(),
        }
        .storage_deposit(root, &d)
        .await?;
    }
    let mk = near
        .call(root, &d, "create_market", json!({"args": {"base_token": format!("ft:{base}"), "base_token_lot_size": "1000000000", "quote_token": format!("ft:{quote}"), "quote_token_lot_size": "100000", "taker_fee_base_rate": 20, "maker_rebate_base_rate": 2}}), NearToken::from_near(1), 300)
        .await?;
    let market_id = mk.value.as_str().unwrap().to_string();
    near.call(
        root,
        &d,
        "set_market_state",
        json!({"market_id": market_id, "new_state": "Active"}),
        NearToken::from_yoctonear(0),
        30,
    )
    .await?;
    const MAKERS: usize = 16;
    const PER: usize = 20; // Tonic allows 20 open orders per account per market
    let mut makers = vec![];
    for i in 0..MAKERS {
        makers.push(near.create_subaccount(root, &format!("m{i}"), NearToken::from_near(30)).await?);
    }
    let taker = near.create_subaccount(root, "taker", NearToken::from_near(50)).await?;
    let all: Vec<&Actor> = makers.iter().chain(std::iter::once(&taker)).collect();
    for a in &all {
        near.call(a, &d, "storage_deposit", json!({}), NearToken::from_near(1), 30).await?;
    }
    join_all(all.iter().map(|a| async {
        fund(near, a, base, 100 * ONE_BASE, &d).await?;
        fund(near, a, quote, 100_000 * ONE_QUOTE, &d).await
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;

    let mut resting = vec![];
    let mut order_ids: Vec<(usize, String)> = vec![];
    for round in 0..PER {
        let rs = join_all(makers.iter().enumerate().map(|(i, m)| {
            let price = 5 * ONE_QUOTE + (round * MAKERS + i) as u128 * (ONE_QUOTE / 1000);
            let market_id = market_id.clone();
            let d = d.clone();
            async move {
                let o = near.call(m, &d, "new_order", json!({"market_id": market_id, "order": {"limit_price": price.to_string(), "quantity": (ONE_BASE / 100).to_string(), "side": "Sell", "order_type": "Limit"}}), NearToken::from_yoctonear(0), 300).await?;
                Ok::<_, anyhow::Error>((i, o.value["id"].as_str().unwrap_or_default().to_string(), tg(o.gas_burnt)))
            }
        }))
        .await;
        let mut g = vec![];
        for r in rs {
            let (i, id, gas) = r?;
            order_ids.push((i, id));
            g.push(gas);
            resting.push(json!({"book": round * MAKERS, "gas": gas}));
        }
        println!("tonic resting ask, book ~{:>3}: median {:.2} Tgas", round * MAKERS, median(g));
    }
    let mut sweeps = vec![];
    for k in [1u128, 2, 4, 8, 16, 24, 32, 40, 48, 56, 64] {
        let r = near.call(&taker, &d, "new_order", json!({"market_id": market_id, "order": {"limit_price": (10 * ONE_QUOTE).to_string(), "quantity": (ONE_BASE / 100 * k).to_string(), "side": "Buy", "order_type": "ImmediateOrCancel"}}), NearToken::from_yoctonear(0), 300).await;
        match r {
            Ok(o) => {
                let filled: u128 = o.value["base_fill_quantity"].as_str().unwrap_or("0").parse().unwrap_or(0);
                println!(
                    "tonic taker sweep k={k:>2}: {:.2} Tgas, filled {:.4} base",
                    tg(o.gas_burnt),
                    filled as f64 / ONE_BASE as f64
                );
                sweeps.push(json!({"k": k, "gas": tg(o.gas_burnt), "ok": true}));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                println!("tonic taker sweep k={k:>2}: FAILED {}", &msg[..msg.len().min(160)]);
                sweeps.push(json!({"k": k, "ok": false, "error": msg}));
            }
        }
    }
    let mut cancels = vec![];
    for (i, id) in order_ids.iter().rev().take(12) {
        if let Ok(o) = near
            .call(
                &makers[*i],
                &d,
                "cancel_order",
                json!({"market_id": market_id, "order_id": id}),
                NearToken::from_yoctonear(0),
                300,
            )
            .await
        {
            cancels.push(tg(o.gas_burnt));
        }
    }
    println!("tonic cancel: median {:.2} Tgas", median(cancels.clone()));
    Ok(json!({"resting": resting, "sweeps": sweeps, "cancels": cancels}))
}

async fn obkit(near: &Near, root: &Actor, base: &str, quote: &str) -> Result<Value> {
    let ob = near.create_subaccount(root, "ob", NearToken::from_near(50)).await?;
    let c = ob.id.to_string();
    near.deploy(&ob, std::fs::read(obkit_near::stack::out_dir().join("obkit_settlement.wasm"))?)
        .await?;
    near.call(
        &ob,
        &c,
        "new",
        json!({"owner": root.id, "operator": root.id, "force_timeout_ns": "3600000000000"}),
        NearToken::from_yoctonear(0),
        30,
    )
    .await?;
    let set = Settlement::new(near.clone(), &c);
    for t in [base, quote] {
        set.add_token(root, t).await?;
        Ft {
            near: near.clone(),
            token: t.into(),
        }
        .storage_deposit(root, &c)
        .await?;
    }
    const USERS: usize = 64;
    let mut users = vec![];
    for i in 0..USERS {
        users.push(near.create_subaccount(root, &format!("u{i}"), NearToken::from_near(5)).await?);
    }
    let keys: Vec<Signer> = (0..USERS).map(|_| Signer::generate()).collect();
    for chunk in users.iter().zip(&keys).collect::<Vec<_>>().chunks(8) {
        join_all(chunk.iter().map(|(u, k)| async {
            fund(near, u, base, 10u128.pow(20), &c).await?;
            fund(near, u, quote, 10u128.pow(24), &c).await?;
            set.register_key(u, &k.public_key()).await?;
            Ft {
                near: near.clone(),
                token: base.into(),
            }
            .storage_deposit(u, u.id.as_str())
            .await
        }))
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    }
    let mut id = 0u64;
    let mut entries_run = vec![];
    for pairs in [1usize, 2, 4, 8, 12, 16, 24, 32] {
        let mut entries = vec![];
        for p in 0..pairs {
            let (a, b) = (users[2 * p].id.to_string(), users[2 * p + 1].id.to_string());
            let q = 1000 + id as i128;
            entries.extend([
                json!({"account": a, "token": base, "delta": q.to_string()}),
                json!({"account": a, "token": quote, "delta": (-q * 5).to_string()}),
                json!({"account": b, "token": base, "delta": (-q).to_string()}),
                json!({"account": b, "token": quote, "delta": (q * 5).to_string()}),
            ]);
        }
        id += 1;
        let o = set.settle_batch(root, &json!({"id": id.to_string(), "entries": entries, "withdrawals": [], "forced": [], "seal": true, "journal_seq": id.to_string(), "journal_hash": "00".repeat(32)})).await?;
        println!("obkit settle_batch entries={:>3}: {:.2} Tgas", entries.len(), tg(o.gas_burnt));
        entries_run.push(json!({"entries": entries.len(), "gas": tg(o.gas_burnt)}));
    }
    let mut withdraw_run = vec![];
    for (nonce, w) in [1usize, 2, 4, 8].into_iter().enumerate() {
        let ws: Vec<Value> = users[..w]
            .iter()
            .zip(&keys)
            .map(|(u, k)| {
                let msg = obkit_wire::withdraw_msg(&c, u.id.as_str(), base, 12_345, nonce as u64 + 1);
                json!({"account": u.id, "token": base, "amount": "12345", "nonce": (nonce + 1).to_string(), "public_key": k.public_key(), "signature": k.sign_b64(&msg)})
            })
            .collect();
        id += 1;
        let o = set.settle_batch(root, &json!({"id": id.to_string(), "entries": [], "withdrawals": ws, "forced": [], "seal": true, "journal_seq": id.to_string(), "journal_hash": "00".repeat(32)})).await?;
        println!("obkit settle_batch withdrawals={w}: {:.2} Tgas", tg(o.gas_burnt));
        withdraw_run.push(json!({"withdrawals": w, "gas": tg(o.gas_burnt)}));
    }
    let evs = set.events(1, 200).await?;
    let reverted = evs.iter().filter(|(_, e)| e["type"] == "withdrawal_reverted").count();
    if reverted > 0 {
        bail!("{reverted} benchmark withdrawals reverted");
    }
    Ok(json!({"entries_run": entries_run, "withdraw_run": withdraw_run}))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let sb = Sandbox::start().await?;
    let near = Near::rpc(&sb.rpc_url)?;
    let base = token(&near, &sb.root, "wbtc", 16).await?;
    let quote = token(&near, &sb.root, "usdc", 18).await?;
    let t = tonic(&near, &sb.root, &base, &quote).await?;
    let o = obkit(&near, &sb.root, &base, &quote).await?;
    let out = json!({"measured_at_unix": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(), "sandbox": "local nearcore sandbox", "tonic": t, "obkit": o});
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../research/data/onchain_bench.json");
    std::fs::write(path, serde_json::to_string_pretty(&out)?)?;
    println!("wrote research/data/onchain_bench.json");
    sb.stop().await;
    Ok(())
}
