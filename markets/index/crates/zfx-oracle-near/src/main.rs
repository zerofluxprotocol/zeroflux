//! zfx-oracle-near: admin and relay commands for the zfx-oracle contract.
//!
//! ```text
//! zfx-oracle-near create-account --parent zeroflux.testnet --parent-key-file F --name oracle --fund-near 4 --out-key-file F2
//! zfx-oracle-near deploy --account oracle.zeroflux.testnet --key-file F2 --wasm W --reporter HEX [--reporter HEX] --quorum 1 [--min-free-near 0.2]
//! zfx-oracle-near set-indices --contract C --key-file F2 --models PUBLIC/v1/markets/models.json --markets v2-markets.json
//! zfx-oracle-near push --contract C --signer A --key-file F --ledger DIR [--from-date D] [--dry-run]
//! zfx-oracle-near status --contract C
//! zfx-oracle-near view --contract C --method M --args JSON
//! ```
//! Common: `--rpc URL[,URL...]` (default https://rpc.testnet.fastnear.com,https://rpc.testnet.near.org),
//! `--tx-log FILE` appends every transaction hash as a JSON line.

use anyhow::{bail, Context, Result};
use near_api::NearToken;
use serde_json::{json, Value};
use std::path::PathBuf;
use zfx_oracle_near::*;

fn arg(a: &[String], k: &str) -> Option<String> {
    a.iter().position(|x| x == k).and_then(|i| a.get(i + 1).cloned())
}
fn args_all(a: &[String], k: &str) -> Vec<String> {
    a.iter().enumerate().filter(|(_, x)| *x == k).filter_map(|(i, _)| a.get(i + 1).cloned()).collect()
}
fn need(a: &[String], k: &str) -> Result<String> {
    arg(a, k).with_context(|| format!("missing {k}"))
}
fn near_amount(s: &str) -> Result<NearToken> {
    // decimal NEAR with up to 24 places, exact
    let (i, f) = s.split_once('.').unwrap_or((s, ""));
    if f.len() > 24 {
        bail!("too many decimals");
    }
    let y: u128 = i.parse::<u128>()? * 10u128.pow(24) + if f.is_empty() { 0 } else { f.parse::<u128>()? * 10u128.pow(24 - f.len() as u32) };
    Ok(NearToken::from_yoctonear(y))
}

pub fn default_rpcs() -> Vec<String> {
    vec!["https://rpc.testnet.fastnear.com".into(), "https://rpc.testnet.near.org".into()]
}

fn write_tx_log(path: Option<&String>, near: &Near) -> Result<()> {
    let Some(p) = path else { return Ok(()) };
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    for (what, h) in near.tx_log.lock().unwrap().iter() {
        writeln!(f, "{}", json!({"what": what, "tx": h}))?;
    }
    Ok(())
}

/// Index specs for the licensed daily and licensed 7D index of each tradable market.
fn specs_for(models: &Value, tradable: &[String]) -> Result<Vec<Value>> {
    let f = &models["formula_licensed"];
    let (v1, h1, v7, h7) = (f["version"].as_str().context("formula_licensed.version")?, f["hash"].as_str().context("hash")?, f["version_7d"].as_str().context("version_7d")?, f["hash_7d"].as_str().context("hash_7d")?);
    let mut out = Vec::new();
    for slug in tradable {
        out.push(json!({"index_id": format!("or-model-spend-lic/{slug}"), "version": v1, "formula_hash": h1, "value_scale": 6}));
        out.push(json!({"index_id": format!("or-model-spend-lic-7d/{slug}"), "version": v7, "formula_hash": h7, "value_scale": 6}));
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let cmd = a.first().cloned().unwrap_or_default();
    let rpcs = arg(&a, "--rpc").map(|s| s.split(',').map(String::from).collect()).unwrap_or_else(default_rpcs);
    let near = Near::rpcs(&rpcs)?;
    let txlog = arg(&a, "--tx-log");
    let res: Result<Value> = async {
        match cmd.as_str() {
            "create-account" => {
                let parent = Actor::from_key_file(Some(&need(&a, "--parent")?), &PathBuf::from(need(&a, "--parent-key-file")?))?;
                let out = PathBuf::from(need(&a, "--out-key-file")?);
                if out.exists() {
                    bail!("{} exists", out.display());
                }
                let (acct, secret, public) = near.create_subaccount(&parent, &need(&a, "--name")?, near_amount(&need(&a, "--fund-near")?)?).await?;
                {
                    use std::io::Write;
                    use std::os::unix::fs::OpenOptionsExt;
                    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&out)?;
                    f.write_all(serde_json::to_string_pretty(&json!({"account_id": acct.id.to_string(), "public_key": public, "private_key": secret}))?.as_bytes())?;
                }
                Ok(json!({"account": acct.id.to_string(), "key_file": out}))
            }
            "deploy" => {
                let who = Actor::from_key_file(Some(&need(&a, "--account")?), &PathBuf::from(need(&a, "--key-file")?))?;
                let wasm = std::fs::read(need(&a, "--wasm")?)?;
                let n = wasm.len();
                let h = near.deploy(&who, wasm).await?;
                let reporters = args_all(&a, "--reporter");
                let quorum: u8 = arg(&a, "--quorum").unwrap_or_else(|| "1".into()).parse()?;
                let min_free = near_amount(&arg(&a, "--min-free-near").unwrap_or_else(|| "0.2".into()))?;
                let r = near
                    .call(&who, who.id.as_str(), "new", json!({"owner": who.id.to_string(), "reporters": reporters, "quorum": quorum, "min_free_yocto": min_free.as_yoctonear().to_string()}), NearToken::from_yoctonear(0), 30)
                    .await?;
                Ok(json!({"deploy_tx": h, "wasm_bytes": n, "init_tx": r.tx_hash}))
            }
            "set-indices" => {
                let who = Actor::from_key_file(arg(&a, "--account").as_deref(), &PathBuf::from(need(&a, "--key-file")?))?;
                let contract = need(&a, "--contract")?;
                let models: Value = serde_json::from_str(&std::fs::read_to_string(need(&a, "--models")?)?)?;
                let mk: Value = serde_json::from_str(&std::fs::read_to_string(need(&a, "--markets")?)?)?;
                let slugs: Vec<String> = mk["tradable"].as_array().context("tradable")?.iter().filter_map(|m| m["permaslug"].as_str().map(String::from)).collect();
                let specs = specs_for(&models, &slugs)?;
                let mut txs = Vec::new();
                for ch in specs.chunks(20) {
                    let r = near.call(&who, &contract, "set_indices", json!({"specs": ch}), NearToken::from_yoctonear(0), 100).await?;
                    txs.push(json!({"tx": r.tx_hash, "indices": ch.len(), "gas_tgas": r.gas_burnt / 1_000_000_000_000}));
                }
                Ok(json!({"markets": slugs.len(), "indices": specs.len(), "txs": txs}))
            }
            "push" => {
                let who = Actor::from_key_file(arg(&a, "--signer").as_deref(), &PathBuf::from(need(&a, "--key-file")?))?;
                let from = arg(&a, "--from-date");
                let est: u64 = arg(&a, "--est-tgas-per-print").map(|x| x.parse()).transpose()?.unwrap_or(8);
                push_new(&near, &who, &need(&a, "--contract")?, &PathBuf::from(need(&a, "--ledger")?), DEFAULT_FILES, &PushOpts { from_date: from.as_deref(), only: None, est_tgas_per_print: est, dry_run: a.iter().any(|x| x == "--dry-run") }).await
            }
            "status" => {
                let c = need(&a, "--contract")?;
                Ok(json!({"config": near.view(&c, "config", json!({})).await?, "storage": near.view(&c, "storage_status", json!({})).await?, "balance_yocto": near.balance(&c).await?.to_string()}))
            }
            "view" => {
                let args: Value = serde_json::from_str(&arg(&a, "--args").unwrap_or_else(|| "{}".into()))?;
                near.view(&need(&a, "--contract")?, &need(&a, "--method")?, args).await
            }
            "balance" => Ok(json!({"account": need(&a, "--account")?, "yocto": near.balance(&need(&a, "--account")?).await?.to_string()})),
            _ => bail!("commands: create-account deploy set-indices push status view balance"),
        }
    }
    .await;
    write_tx_log(txlog.as_ref(), &near)?;
    match res {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        Err(e) => Err(e),
    }
}
