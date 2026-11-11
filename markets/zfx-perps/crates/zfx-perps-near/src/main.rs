//! zfx-perps-near CLI.
//!
//! ```text
//! zfx-perps-near sandbox     --out stack.json                 start a local chain, deploy, keep running
//! zfx-perps-near deploy      --rpc URL --master ID --master-key KEY --out stack.json
//! zfx-perps-near node-config --stack stack.json --data DIR [--listen ADDR] [--admin-token T]
//! zfx-perps-near adapter     --stack stack.json --node URL --admin-token T
//! zfx-perps-near dev-user    --stack stack.json --name alice [--near 1000] [--usdc 10000]
//! zfx-perps-near deposit | register-key | force-withdraw | freeze | escape  (see --help)
//! ```

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use near_api::NearToken;
use zfx_perps_client::{Admin, Signer};
use zfx_perps_near::adapter::Adapter;
use zfx_perps_near::stack::{self, Stack, StackOptions};
use zfx_perps_near::{Actor, Ft, Near, Settlement};
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "zfx-perps-near", about = "NEAR side of a zfx-perps exchange")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a local nearcore sandbox, deploy a stack, write it to --out, and keep running.
    Sandbox {
        #[arg(long, default_value = "stack.json")]
        out: PathBuf,
        #[arg(long, default_value_t = 3600)]
        force_timeout_secs: u64,
    },
    /// Deploy test tokens + settlement contract under an existing master account.
    Deploy {
        #[arg(long)]
        rpc: String,
        #[arg(long)]
        master: String,
        #[arg(long)]
        master_key: String,
        #[arg(long, default_value = "stack.json")]
        out: PathBuf,
        #[arg(long, default_value_t = 86400)]
        force_timeout_secs: u64,
    },
    /// Print a node config (NEAR-USDC market) for a deployed stack.
    NodeConfig {
        #[arg(long)]
        stack: PathBuf,
        #[arg(long)]
        data: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8794")]
        listen: String,
        #[arg(long, default_value = "dev-admin-token-change-me")]
        admin_token: String,
        /// Don't run the exchange-ticker oracle; marks come from POST /internal/mark.
        #[arg(long)]
        manual_marks: bool,
    },
    /// Run the settlement adapter (operator key from the stack file).
    Adapter {
        #[arg(long)]
        stack: PathBuf,
        #[arg(long, default_value = "http://127.0.0.1:8794")]
        node: String,
        #[arg(long, default_value = "dev-admin-token-change-me")]
        admin_token: String,
        #[arg(long, default_value_t = 500)]
        poll_ms: u64,
    },
    /// Test only: create a funded user, deposit test tokens, register a trading key. Prints JSON.
    DevUser {
        #[arg(long)]
        stack: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 1000)]
        near: u128,
        #[arg(long, default_value_t = 10000)]
        usdc: u128,
    },
    /// Test only: mint test USDC to the owner and deposit it for the perp insurance fund.
    FundInsurance {
        #[arg(long)]
        stack: PathBuf,
        #[arg(long, default_value_t = 100000)]
        usdc: u128,
    },
    /// Deposit a NEP-141 token into the exchange.
    Deposit {
        #[command(flatten)]
        who: Who,
        #[arg(long)]
        token: String,
        #[arg(long)]
        amount: u128,
    },
    /// Register an ed25519 trading key (prints a new one if --public-key is omitted).
    RegisterKey {
        #[command(flatten)]
        who: Who,
        #[arg(long)]
        public_key: Option<String>,
    },
    /// File a forced withdrawal on-chain.
    ForceWithdraw {
        #[command(flatten)]
        who: Who,
        #[arg(long)]
        token: String,
        #[arg(long)]
        amount: u128,
    },
    /// Freeze the contract (only works once a forced withdrawal is overdue).
    Freeze {
        #[command(flatten)]
        who: Who,
    },
    /// Clearing mode: deploy a mock collateral token, the clearing contract and an operator
    /// account under --master (testnet or sandbox only). Writes the stack file.
    ClearingDeploy {
        #[arg(long)]
        rpc: String,
        #[arg(long)]
        master: String,
        /// Master secret key; or a NEAR credentials JSON via --master-credentials.
        #[arg(long, env = "ZFX_PERPS_MASTER_KEY")]
        master_key: Option<String>,
        #[arg(long)]
        master_credentials: Option<PathBuf>,
        #[arg(long, default_value = "near:testnet")]
        chain_id: String,
        /// ed25519 reporter public keys (`ed25519:<base58>` or 64 hex chars).
        #[arg(long, num_args = 1..)]
        reporters: Vec<String>,
        #[arg(long, default_value_t = 1)]
        quorum: u8,
        #[arg(long, default_value_t = 60)]
        timelock_secs: u64,
        #[arg(long, default_value_t = 86400)]
        force_timeout_secs: u64,
        #[arg(long, default_value = "perps")]
        contract_name: String,
        #[arg(long, default_value = "tusdc-perps")]
        token_name: String,
        #[arg(long, default_value = "perps-operator")]
        operator_name: String,
        #[arg(long, default_value_t = 5)]
        contract_near: u128,
        #[arg(long, default_value_t = 2)]
        token_near: u128,
        #[arg(long, default_value_t = 1)]
        operator_near: u128,
        /// Reuse an existing collateral token instead of deploying a mock one.
        #[arg(long)]
        collateral: Option<String>,
        #[arg(long)]
        out: PathBuf,
        /// Append every transaction (what, hash) as JSON lines.
        #[arg(long)]
        tx_log: Option<PathBuf>,
    },
    /// Clearing mode: list every market in a directory of MarketParams JSON files, propose their
    /// activation, wait out the timelock once, and execute. Records every transaction.
    ClearingMarkets {
        #[command(flatten)]
        c: ClearingArgs,
        #[arg(long)]
        dir: PathBuf,
        #[arg(long)]
        tx_log: Option<PathBuf>,
    },
    /// Clearing mode: list a market from a MarketParams JSON file and activate it (waits out the
    /// timelock).
    ClearingMarket {
        #[command(flatten)]
        c: ClearingArgs,
        #[arg(long)]
        params: PathBuf,
    },
    /// Clearing mode: set a market's admin status (tightening is immediate; reopening is
    /// proposed and waits out the timelock).
    ClearingMarketStatus {
        #[command(flatten)]
        c: ClearingArgs,
        #[arg(long)]
        market: String,
        #[arg(long)]
        status: String,
        #[arg(long)]
        tx_log: Option<PathBuf>,
    },
    /// Clearing mode: print a node config for a clearing stack.
    ClearingNodeConfig {
        #[command(flatten)]
        c: ClearingArgs,
        #[arg(long)]
        data: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8900")]
        listen: String,
        #[arg(long)]
        admin_token: String,
        #[arg(long)]
        print_feed: Option<String>,
    },
    /// Clearing mode: run the settlement relayer.
    ClearingAdapter {
        #[command(flatten)]
        c: ClearingArgs,
        #[arg(long, default_value = "http://127.0.0.1:8900")]
        node: String,
        #[arg(long)]
        admin_token: String,
        #[arg(long, default_value_t = 1000)]
        poll_ms: u64,
        /// Append one JSON line per settled batch (id, tx hash, Tgas).
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// After a freeze: withdraw your whole settled balance of a token.
    Escape {
        #[command(flatten)]
        who: Who,
        #[arg(long)]
        token: String,
    },
}

#[derive(clap::Args)]
struct ClearingArgs {
    /// Clearing stack file written by clearing-deploy (contains secret keys; keep outside git).
    #[arg(long)]
    stack: PathBuf,
}

#[derive(clap::Args)]
struct Who {
    #[arg(long)]
    rpc: String,
    #[arg(long)]
    contract: String,
    #[arg(long)]
    account: String,
    #[arg(long, env = "ZFX_PERPS_ACCOUNT_KEY")]
    key: String,
}

impl Who {
    fn parts(&self) -> Result<(Settlement, Actor)> {
        let near = Near::rpc(&self.rpc)?;
        Ok((Settlement::new(near, &self.contract), Actor::new(&self.account, &self.key)?))
    }
}

fn load(p: &PathBuf) -> Result<Stack> {
    serde_json::from_str(&std::fs::read_to_string(p).with_context(|| p.display().to_string())?).context("stack file")
}

fn save(p: &PathBuf, s: &Stack) -> Result<()> {
    std::fs::write(p, serde_json::to_string_pretty(s)?)?;
    eprintln!("wrote {} (contains secret keys; keep it out of git)", p.display());
    Ok(())
}

/// Ctrl-C or SIGTERM, so child processes (the sandbox node) are always cleaned up.
async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

fn write_tx_log(near: &Near, path: Option<&PathBuf>) -> Result<()> {
    let log = near.tx_log.lock().unwrap().clone();
    match path {
        Some(p) => {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
            for (what, hash) in log {
                writeln!(f, "{}", json!({"what": what, "tx": hash}))?;
            }
        }
        None => {
            for (what, hash) in log {
                eprintln!("tx {hash}  {what}");
            }
        }
    }
    Ok(())
}

fn unit(decimals: u8) -> u128 {
    10u128.pow(decimals as u32)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().cmd {
        Cmd::Sandbox { out, force_timeout_secs } => {
            let sb = zfx_perps_near::sandbox::Sandbox::start().await?;
            let near = Near::rpc(&sb.rpc_url)?;
            let s = stack::deploy(
                &near,
                &sb.rpc_url,
                &sb.root,
                &StackOptions {
                    force_timeout_ns: force_timeout_secs * 1_000_000_000,
                    ..Default::default()
                },
            )
            .await?;
            save(&out, &s)?;
            eprintln!("sandbox at {}  contract {}  (Ctrl-C to stop)", sb.rpc_url, s.contract);
            shutdown_signal().await;
            sb.stop().await;
        }
        Cmd::Deploy {
            rpc,
            master,
            master_key,
            out,
            force_timeout_secs,
        } => {
            let near = Near::rpc(&rpc)?;
            let m = Actor::new(&master, &master_key)?;
            let s = stack::deploy(
                &near,
                &rpc,
                &m,
                &StackOptions {
                    force_timeout_ns: force_timeout_secs * 1_000_000_000,
                    ..Default::default()
                },
            )
            .await?;
            save(&out, &s)?;
        }
        Cmd::NodeConfig {
            stack,
            data,
            listen,
            admin_token,
            manual_marks,
        } => {
            let s = load(&stack)?;
            let near_t = s.tokens.iter().find(|t| t.symbol == "NEAR").context("stack has no NEAR token")?;
            let usdc_t = s.tokens.iter().find(|t| t.symbol == "USDC").context("stack has no USDC token")?;
            // 1 lot = 0.01 NEAR; 1 tick = $0.0001 per NEAR, so quote atoms per lot per tick =
            // 0.01 * 0.0001 * 10^usdc_decimals.
            let base_lot = unit(near_t.decimals) / 100;
            let quote_tick = (unit(usdc_t.decimals) / 1_000_000).max(1);
            let insurance = s
                .insurance
                .as_ref()
                .map(|i| i.account.clone())
                .unwrap_or_else(|| format!("insurance.{}", s.owner.account));
            // NEAR-PERP: same lot (0.01 NEAR) and tick ($0.0001) as the spot market, so 10,000
            // ticks per USD. Index from exchange tickers unless --manual-marks.
            let oracle = (!manual_marks).then(|| {
                json!({
                    "interval_ms": 1000, "min_sources": 2, "max_deviation_bps": 200,
                    "feeds": {"NEAR-PERP": {"ticks_per_usd": 10000, "sources": [
                        "coinbase:NEAR-USD", "kraken:NEARUSD", "okx:NEAR-USDT", "binanceus:NEARUSDT",
                        "gemini:nearusd", "bitget:NEARUSDT", "binance:NEARUSDT", "bybit:NEARUSDT"]}}
                })
            });
            let cfg = json!({
                "listen": listen, "data_dir": data, "fsync": true,
                "settlement_contract": s.contract, "fee_account": s.fees.account, "admin_token": admin_token,
                "insurance_account": insurance, "batch_interval_ms": 1000, "funding_interval_ms": 3600000,
                "oracle": oracle,
                "assets": s.tokens.iter().map(|t| json!({"symbol": t.symbol, "token": t.token, "decimals": t.decimals})).collect::<Vec<_>>(),
                "markets": [
                    {"symbol": "NEAR-USDC", "base": "NEAR", "quote": "USDC", "base_lot": base_lot.to_string(), "quote_tick": quote_tick.to_string(),
                     "min_qty": 1, "max_qty": 100000000, "max_price": 100000000, "maker_fee_bps": 2, "taker_fee_bps": 5},
                    {"symbol": "NEAR-PERP", "base": "NEAR", "quote": "USDC", "base_lot": "1", "quote_tick": quote_tick.to_string(),
                     "min_qty": 1, "max_qty": 100000000, "max_price": 100000000, "maker_fee_bps": 2, "taker_fee_bps": 5,
                     "perp": {"imr_bps": 1000, "mmr_bps": 500}}
                ]
            });
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
        Cmd::Adapter {
            stack,
            node,
            admin_token,
            poll_ms,
        } => {
            let s = load(&stack)?;
            let near = Near::rpc(&s.rpc_url)?;
            let mut a = Adapter::new(
                Settlement::new(near, &s.contract),
                s.operator.actor()?,
                Admin::new(&node, &admin_token),
            );
            a.poll = std::time::Duration::from_millis(poll_ms);
            eprintln!("adapter: {} <-> {}", node, s.contract);
            tokio::select! {
                _ = a.run() => {}
                _ = shutdown_signal() => {}
            }
        }
        Cmd::DevUser { stack, name, near, usdc } => {
            let s = load(&stack)?;
            let n = Near::rpc(&s.rpc_url)?;
            let set = Settlement::new(n.clone(), &s.contract);
            let owner = s.owner.actor()?;
            let user = n.create_subaccount(&owner, &name, NearToken::from_near(5)).await?;
            let trading = Signer::generate();
            set.register_key(&user, &trading.public_key()).await?;
            for t in &s.tokens {
                let amount = if t.symbol == "NEAR" {
                    near * unit(t.decimals)
                } else {
                    usdc * unit(t.decimals)
                };
                let ft = Ft {
                    near: n.clone(),
                    token: t.token.clone(),
                };
                ft.mint(&user, amount).await?;
                set.deposit(&user, &t.token, amount, None).await?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"account": user.id, "account_key": user.secret, "trading_key": trading.near_secret(), "trading_public_key": trading.public_key()})
                )?
            );
        }
        Cmd::FundInsurance { stack, usdc } => {
            let s = load(&stack)?;
            let n = Near::rpc(&s.rpc_url)?;
            let owner = s.owner.actor()?;
            let ins = s.insurance.as_ref().context("stack has no insurance account")?;
            let t = s.tokens.iter().find(|t| t.symbol == "USDC").context("stack has no USDC")?;
            let amount = usdc * unit(t.decimals);
            Ft {
                near: n.clone(),
                token: t.token.clone(),
            }
            .mint(&owner, amount)
            .await?;
            Settlement::new(n, &s.contract)
                .deposit(&owner, &t.token, amount, Some(&ins.account))
                .await?;
            println!("insurance {} funded with {usdc} USDC", ins.account);
        }
        Cmd::Deposit { who, token, amount } => {
            let (set, a) = who.parts()?;
            let o = set.deposit(&a, &token, amount, None).await?;
            println!("deposited {amount} of {token} ({:.2} Tgas)", o.gas_burnt as f64 / 1e12);
        }
        Cmd::RegisterKey { who, public_key } => {
            let (set, a) = who.parts()?;
            let (pk, secret) = match public_key {
                Some(pk) => (pk, None),
                None => {
                    let s = Signer::generate();
                    (s.public_key(), Some(s.near_secret()))
                }
            };
            set.register_key(&a, &pk).await?;
            println!("{}", json!({"public_key": pk, "secret_key": secret}));
        }
        Cmd::ForceWithdraw { who, token, amount } => {
            let (set, a) = who.parts()?;
            println!("request_id {}", set.request_forced_withdrawal(&a, &token, amount).await?);
        }
        Cmd::Freeze { who } => {
            let (set, a) = who.parts()?;
            set.freeze(&a).await?;
            println!("frozen");
        }
        Cmd::ClearingDeploy {
            rpc,
            master,
            master_key,
            master_credentials,
            chain_id,
            reporters,
            quorum,
            timelock_secs,
            force_timeout_secs,
            contract_name,
            token_name,
            operator_name,
            contract_near,
            token_near,
            operator_near,
            collateral,
            out,
            tx_log,
        } => {
            if rpc.contains("mainnet") {
                anyhow::bail!("refusing to deploy to mainnet");
            }
            let key = match (master_key, master_credentials) {
                (Some(k), _) => k,
                (None, Some(p)) => {
                    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
                    v["private_key"].as_str().or(v["secret_key"].as_str()).context("credentials: private_key")?.to_string()
                }
                _ => anyhow::bail!("--master-key or --master-credentials"),
            };
            let master = Actor::new(&master, &key)?;
            let reporters = reporters
                .iter()
                .map(|r| {
                    if r.len() == 64 && r.chars().all(|c| c.is_ascii_hexdigit()) {
                        let b = zfx_perps_clearing::wire::unhex(&r.to_lowercase()).context("hex key")?;
                        Ok(format!("ed25519:{}", bs58::encode(b).into_string()))
                    } else {
                        Ok(r.clone())
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            let near = Near::rpc(&rpc)?;
            let o = zfx_perps_near::clearing::ClearingOptions {
                chain_id,
                contract_name,
                token_name,
                operator_name,
                contract_near,
                token_near,
                operator_near,
                reporters,
                quorum,
                force_timeout_ns: force_timeout_secs * 1_000_000_000,
                timelock_ns: timelock_secs * 1_000_000_000,
                collateral,
            };
            let r = zfx_perps_near::clearing::deploy_clearing(&near, &rpc, &master, &o).await;
            write_tx_log(&near, tx_log.as_ref())?;
            let st = r?;
            std::fs::write(&out, serde_json::to_string_pretty(&st)?)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600))?;
            }
            eprintln!("wrote {} (contains secret keys; keep it out of git)", out.display());
            println!("{}", json!({"contract": st.contract, "collateral": st.collateral, "operator": st.operator.account, "insurance": st.insurance_account}));
        }
        Cmd::ClearingMarket { c, params } => {
            let st: zfx_perps_near::clearing::ClearingStack = serde_json::from_str(&std::fs::read_to_string(&c.stack)?)?;
            let p: zfx_perps_clearing::MarketParams = serde_json::from_str(&std::fs::read_to_string(&params)?)?;
            let cl = zfx_perps_near::clearing::Clearing::new(Near::rpc(&st.rpc_url)?, &st.contract);
            let owner = st.owner.actor()?;
            let o = cl.create_market(&owner, &p).await?;
            println!("{}", json!({"step": "create_market", "tx": o.tx_hash}));
            if let Some(id) = cl.set_market_status(&owner, &p.market_id, "ACTIVE").await? {
                let state = cl.state().await?;
                let wait: u64 = state["timelock_ns"].as_str().unwrap_or("0").parse()?;
                eprintln!("activation proposal {id}; waiting {} s for the timelock", wait / 1_000_000_000 + 5);
                tokio::time::sleep(std::time::Duration::from_nanos(wait) + std::time::Duration::from_secs(5)).await;
                let o = cl.execute_proposal(&owner, id).await?;
                println!("{}", json!({"step": "activate", "proposal": id, "tx": o.tx_hash}));
            }
            println!("{}", serde_json::to_string_pretty(&cl.market(&p.market_id).await?)?);
        }
        Cmd::ClearingMarkets { c, dir, tx_log } => {
            let st: zfx_perps_near::clearing::ClearingStack = serde_json::from_str(&std::fs::read_to_string(&c.stack)?)?;
            let near = Near::rpc(&st.rpc_url)?;
            let cl = zfx_perps_near::clearing::Clearing::new(near.clone(), &st.contract);
            let owner = st.owner.actor()?;
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
            files.sort();
            let mut proposals = vec![];
            let res: Result<()> = async {
                for f in &files {
                    let p: zfx_perps_clearing::MarketParams = serde_json::from_str(&std::fs::read_to_string(f)?)?;
                    if cl.market(&p.market_id).await.is_ok_and(|v| !v.is_null()) {
                        eprintln!("{} already listed", p.market_id);
                        continue;
                    }
                    cl.create_market(&owner, &p).await?;
                    if let Some(id) = cl.set_market_status(&owner, &p.market_id, "ACTIVE").await? {
                        proposals.push((p.market_id.clone(), id));
                    }
                    eprintln!("listed {}", p.market_id);
                }
                let wait: u64 = cl.state().await?["timelock_ns"].as_str().unwrap_or("0").parse()?;
                if !proposals.is_empty() {
                    eprintln!("waiting {} s for the timelock", wait / 1_000_000_000 + 5);
                    tokio::time::sleep(std::time::Duration::from_nanos(wait) + std::time::Duration::from_secs(5)).await;
                }
                for (m, id) in &proposals {
                    cl.execute_proposal(&owner, *id).await?;
                    eprintln!("activated {m}");
                }
                Ok(())
            }
            .await;
            write_tx_log(&near, tx_log.as_ref())?;
            res?;
            println!("{}", serde_json::to_string(&cl.markets().await?.as_array().map(|v| v.iter().map(|m| json!([m["market_id"], m["status"]])).collect::<Vec<_>>()))?);
        }
        Cmd::ClearingMarketStatus { c, market, status, tx_log } => {
            let st: zfx_perps_near::clearing::ClearingStack = serde_json::from_str(&std::fs::read_to_string(&c.stack)?)?;
            let near = Near::rpc(&st.rpc_url)?;
            let cl = zfx_perps_near::clearing::Clearing::new(near.clone(), &st.contract);
            let r = cl.set_market_status(&st.owner.actor()?, &market, &status).await;
            write_tx_log(&near, tx_log.as_ref())?;
            println!("{}", json!({"market": market, "status": status, "proposal": r?}));
        }
        Cmd::ClearingNodeConfig { c, data, listen, admin_token, print_feed } => {
            let st: zfx_perps_near::clearing::ClearingStack = serde_json::from_str(&std::fs::read_to_string(&c.stack)?)?;
            let cfg = json!({
                "listen": listen, "data_dir": data, "fsync": true, "settlement_contract": st.contract, "fee_account": st.contract,
                "admin_token": admin_token, "batch_interval_ms": 1000,
                "clearing": {"chain_id": st.chain_id, "contract": st.contract, "collateral": st.collateral, "collateral_decimals": 6,
                             "insurance_account": st.insurance_account, "print_feed": print_feed},
            });
            println!("{}", serde_json::to_string_pretty(&cfg)?);
        }
        Cmd::ClearingAdapter { c, node, admin_token, poll_ms, log } => {
            let st: zfx_perps_near::clearing::ClearingStack = serde_json::from_str(&std::fs::read_to_string(&c.stack)?)?;
            let cl = zfx_perps_near::clearing::Clearing::new(Near::rpc(&st.rpc_url)?, &st.contract);
            let mut a = zfx_perps_near::clearing_adapter::ClearingAdapter::new(cl, st.operator.actor()?, &node, &admin_token);
            a.poll = std::time::Duration::from_millis(poll_ms);
            a.log_path = log;
            eprintln!("relaying {} <-> {}", node, st.contract);
            tokio::select! {
                _ = a.run() => {}
                _ = shutdown_signal() => {}
            }
        }
        Cmd::Escape { who, token } => {
            let (set, a) = who.parts()?;
            let owed = set.balance(a.id.as_str(), &token).await?;
            set.escape_withdraw(&a, &token).await?;
            println!("escaped {owed} of {token}");
        }
    }
    Ok(())
}
