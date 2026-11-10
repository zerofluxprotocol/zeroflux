//! Mainnet survey of NEAR order books, read-only:
//! - chain parameters: protocol version, per-chunk gas limit, gas price, shards, measured block time;
//! - liveness of on-chain CLOBs (Tonic, Spin) and Orderly's hybrid asset manager;
//! - real gas burnt per order/cancel transaction, from archival RPC.
//!
//! cargo run --release -p obkit-near --example mainnet_survey
//! Writes research/data/mainnet_params.json and research/data/mainnet_orders.json.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::time::Duration;

const RPC: &str = "https://rpc.mainnet.fastnear.com";
const ARCHIVAL: &str = "https://archival-rpc.mainnet.fastnear.com";
const WANT: usize = 40;

async fn rpc(http: &reqwest::Client, url: &str, method: &str, params: Value) -> Result<Value> {
    for attempt in 0..6 {
        let r = http
            .post(url)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await;
        if let Ok(r) = r
            && let Ok(v) = r.json::<Value>().await
            && v.get("result").is_some()
        {
            return Ok(v["result"].clone());
        }
        tokio::time::sleep(Duration::from_secs(2 + 2 * attempt)).await;
    }
    anyhow::bail!("{method} failed")
}

async fn get(http: &reqwest::Client, url: &str) -> Result<Value> {
    for attempt in 0..8 {
        if let Ok(r) = http.get(url).header("user-agent", "obkit-research").send().await
            && let Ok(v) = r.json::<Value>().await
        {
            return Ok(v);
        }
        tokio::time::sleep(Duration::from_secs(3 + 3 * attempt)).await;
    }
    anyhow::bail!("GET {url} failed")
}

async fn params(http: &reqwest::Client) -> Result<Value> {
    let pc = rpc(http, RPC, "EXPERIMENTAL_protocol_config", json!({"finality": "final"})).await?;
    let head = rpc(http, RPC, "block", json!({"finality": "final"})).await?;
    let h = head["header"]["height"].as_u64().context("height")?;
    let old = rpc(http, RPC, "block", json!({"block_id": h - 5000})).await?;
    let dt = (head["header"]["timestamp"].as_u64().unwrap() - old["header"]["timestamp"].as_u64().unwrap()) as f64 / 1e9;
    let n = h - old["header"]["height"].as_u64().unwrap();
    let gp = rpc(http, RPC, "gas_price", json!([null])).await?;
    Ok(json!({
        "protocol_version": pc["protocol_version"],
        "chunk_gas_limit": pc["gas_limit"],
        "max_gas_burnt_per_fn_call": pc["runtime_config"]["wasm_config"]["limit_config"]["max_gas_burnt"],
        "current_gas_price_yocto": gp["gas_price"],
        "shards": head["chunks"].as_array().map_or(0, |c| c.len()),
        "blocks_measured": n,
        "mean_block_time_s": dt / n as f64,
    }))
}

async fn survey(http: &reqwest::Client, account: &str, methods: &[&str]) -> Result<Value> {
    let count = get(http, &format!("https://api.nearblocks.io/v1/account/{account}/txns-only/count")).await?;
    let mut found: Vec<(String, Vec<Value>)> = methods.iter().map(|m| (m.to_string(), vec![])).collect();
    let (mut cursor, mut pages, mut latest) = (None::<String>, 0, None::<u64>);
    while pages < 60 && found.iter().any(|(_, v)| v.len() < WANT) {
        let url = format!(
            "https://api.nearblocks.io/v1/account/{account}/txns-only?per_page=25&order=desc{}",
            cursor.as_ref().map(|c| format!("&cursor={c}")).unwrap_or_default()
        );
        // nearblocks rate-limits with a JSON error body; retry until we get a page.
        let mut r = get(http, &url).await?;
        for wait in [10, 20, 40, 60] {
            if r.get("txns").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
            r = get(http, &url).await?;
        }
        anyhow::ensure!(r.get("txns").is_some(), "nearblocks keeps refusing: {r}");
        pages += 1;
        for t in r["txns"].as_array().into_iter().flatten() {
            let ts = t["block_timestamp"]
                .as_str()
                .and_then(|s| s.parse::<u128>().ok())
                .map(|x| (x / 1_000_000_000) as u64);
            latest = latest.or(ts);
            for a in t["actions"].as_array().into_iter().flatten() {
                if let Some((_, v)) = found.iter_mut().find(|(m, _)| Some(m.as_str()) == a["method"].as_str())
                    && v.len() < WANT
                    && t["receiver_account_id"] == account
                {
                    v.push(json!({"hash": t["transaction_hash"], "signer": t["signer_account_id"], "ts": ts}));
                }
            }
        }
        cursor = r["cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(3500)).await;
    }
    for (m, txs) in found.iter_mut() {
        for t in txs.iter_mut() {
            let r = rpc(
                http,
                ARCHIVAL,
                "tx",
                json!({"tx_hash": t["hash"], "sender_account_id": t["signer"], "wait_until": "NONE"}),
            )
            .await;
            if let Ok(r) = r {
                let gas = r["transaction_outcome"]["outcome"]["gas_burnt"].as_u64().unwrap_or(0)
                    + r["receipts_outcome"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|x| x["outcome"]["gas_burnt"].as_u64().unwrap_or(0))
                        .sum::<u64>();
                t["gas"] = json!(gas);
                t["ok"] = json!(r["status"].get("SuccessValue").is_some());
            }
        }
        let mut g: Vec<f64> = txs
            .iter()
            .filter(|t| t["ok"] == true)
            .filter_map(|t| t["gas"].as_u64())
            .map(|x| x as f64 / 1e12)
            .collect();
        g.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if !g.is_empty() {
            println!("{account:38} {m:18} n={:>3} median={:>6.2} Tgas", g.len(), g[g.len() / 2]);
        } else {
            println!("{account:38} {m:18} n={:>3}", txs.len());
        }
    }
    Ok(
        json!({"lifetime_txns": count["txns"][0]["count"], "latest_tx_unix": latest, "pages_scanned": pages, "methods": found.into_iter().map(|(k, v)| (k, Value::Array(v))).collect::<serde_json::Map<_, _>>()}),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let http = reqwest::Client::new();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../research/data");
    let p = params(&http).await?;
    println!("{}", serde_json::to_string_pretty(&p)?);
    std::fs::write(format!("{dir}/mainnet_params.json"), serde_json::to_string_pretty(&p)?)?;
    let mut out = serde_json::Map::new();
    for (acct, methods) in [
        (
            "spot.spin-fi.near",
            &["place_bid", "place_ask", "cancel_order", "batch_ops", "cancel_orders"][..],
        ),
        ("v1.orderbook.near", &["new_order", "cancel_order", "cancel_all_orders"][..]),
    ] {
        out.insert(acct.into(), survey(&http, acct, methods).await?);
    }
    std::fs::write(format!("{dir}/mainnet_orders.json"), serde_json::to_string_pretty(&out)?)?;
    println!("wrote research/data/mainnet_params.json, mainnet_orders.json");
    Ok(())
}
