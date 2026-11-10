//! Deploy a complete settlement stack (test tokens + settlement contract + operator and fee
//! accounts) under a master account. Used by the e2e tests, `scripts/dev.sh` and
//! `zfx-perps-near deploy`.

use crate::{Actor, Ft, Near, Settlement};
use anyhow::{Context, Result};
use near_api::NearToken;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenInfo {
    pub symbol: String,
    pub token: String,
    pub decimals: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActorInfo {
    pub account: String,
    pub secret_key: String,
}

impl ActorInfo {
    pub fn actor(&self) -> Result<Actor> {
        Actor::new(&self.account, &self.secret_key)
    }
    fn of(a: &Actor) -> Self {
        ActorInfo {
            account: a.id.to_string(),
            secret_key: a.secret.clone(),
        }
    }
}

/// Everything needed to run a node and adapter against a deployed stack. Contains secret keys:
/// keep it out of version control.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stack {
    pub rpc_url: String,
    pub contract: String,
    pub owner: ActorInfo,
    pub operator: ActorInfo,
    pub fees: ActorInfo,
    /// Perp insurance fund account (fund it by depositing collateral to it).
    #[serde(default)]
    pub insurance: Option<ActorInfo>,
    pub tokens: Vec<TokenInfo>,
    pub force_timeout_ns: u64,
}

pub struct StackOptions {
    /// (symbol, decimals) of test tokens to deploy.
    pub tokens: Vec<(String, u8)>,
    pub force_timeout_ns: u64,
    pub contract_near: u128,
}

impl Default for StackOptions {
    fn default() -> Self {
        StackOptions {
            tokens: vec![("NEAR".into(), 24), ("USDC".into(), 6)],
            force_timeout_ns: 3_600 * 1_000_000_000,
            contract_near: 30,
        }
    }
}

/// Directory with built wasm (`scripts/build-contracts.sh`). `$ZFX_PERPS_OUT` overrides.
pub fn out_dir() -> PathBuf {
    std::env::var("ZFX_PERPS_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out"))
}

fn wasm(name: &str) -> Result<Vec<u8>> {
    let p = out_dir().join(name);
    std::fs::read(&p).with_context(|| format!("{} (run scripts/build-contracts.sh)", p.display()))
}

pub async fn deploy(near: &Near, rpc_url: &str, master: &Actor, o: &StackOptions) -> Result<Stack> {
    let operator = near.create_subaccount(master, "operator", NearToken::from_near(10)).await?;
    let fees = near.create_subaccount(master, "fees", NearToken::from_near(2)).await?;
    let insurance = near.create_subaccount(master, "insurance", NearToken::from_near(2)).await?;
    let ob = near.create_subaccount(master, "ob", NearToken::from_near(o.contract_near)).await?;
    near.deploy(&ob, wasm("zfx_perps_settlement.wasm")?).await?;
    near.call(
        &ob,
        ob.id.as_str(),
        "new",
        json!({"owner": master.id, "operator": operator.id, "force_timeout_ns": o.force_timeout_ns.to_string()}),
        NearToken::from_yoctonear(0),
        30,
    )
    .await?;
    let settlement = Settlement::new(near.clone(), ob.id.as_str());
    let mut tokens = vec![];
    for (symbol, decimals) in &o.tokens {
        let t = near
            .create_subaccount(master, &symbol.to_lowercase(), NearToken::from_near(5))
            .await?;
        near.deploy(&t, wasm("mock_ft.wasm")?).await?;
        near.call(
            &t,
            t.id.as_str(),
            "new",
            json!({"owner": master.id, "name": format!("Test {symbol} (no value)"), "symbol": symbol, "decimals": decimals, "faucet_limit": u128::MAX.to_string()}),
            NearToken::from_yoctonear(0),
            30,
        )
        .await?;
        let ft = Ft {
            near: near.clone(),
            token: t.id.to_string(),
        };
        ft.storage_deposit(master, ob.id.as_str()).await?;
        settlement.add_token(master, t.id.as_str()).await?;
        tokens.push(TokenInfo {
            symbol: symbol.clone(),
            token: t.id.to_string(),
            decimals: *decimals,
        });
    }
    Ok(Stack {
        rpc_url: rpc_url.into(),
        contract: ob.id.to_string(),
        owner: ActorInfo::of(master),
        operator: ActorInfo::of(&operator),
        fees: ActorInfo::of(&fees),
        insurance: Some(ActorInfo::of(&insurance)),
        tokens,
        force_timeout_ns: o.force_timeout_ns,
    })
}
