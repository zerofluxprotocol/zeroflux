//! zfx-perps-near: NEAR side of a zfx-perps exchange.
//!
//! - [`Near`]: thin typed wrapper over `near-api` (calls, views, account creation, deploys).
//! - [`Settlement`]: the settlement contract (user, operator and admin methods, views).
//! - [`Ft`]: NEP-141 helpers (mock-ft faucet mint, storage, balances).
//! - [`adapter`]: the operator's settlement adapter (contract events -> node, batches -> contract).
//! - [`stack`]: deploy tokens + settlement contract for sandbox/testnet.
//! - [`sandbox`]: start a local nearcore sandbox.

pub mod adapter;
pub mod clearing;
pub mod clearing_adapter;
pub mod sandbox;
pub mod stack;

use anyhow::{Context, Result, anyhow, bail};
use near_api::types::transaction::result::ExecutionFinalResult;
use near_api::{AccountId, Contract, NearGas, NearToken, NetworkConfig, Signer};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;

pub const TGAS: u64 = 1;

/// An account plus a signer for it.
#[derive(Clone)]
pub struct Actor {
    pub id: AccountId,
    pub secret: String,
    pub signer: Arc<Signer>,
}

impl Actor {
    pub fn new(id: &str, secret_key: &str) -> Result<Self> {
        let sk = secret_key.parse().map_err(|e| anyhow!("secret key for {id}: {e:?}"))?;
        Ok(Actor {
            id: id.parse().with_context(|| format!("account id {id}"))?,
            secret: secret_key.into(),
            signer: Signer::from_secret_key(sk)?,
        })
    }
}

impl std::fmt::Debug for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Actor({})", self.id)
    }
}

/// Outcome of a successful transaction.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub gas_burnt: u64,
    /// Transaction hash (base58), for explorers and audit records.
    pub tx_hash: String,
    pub value: Value,
    pub logs: Vec<String>,
}

#[derive(Clone)]
pub struct Near {
    pub network: NetworkConfig,
}

pub(crate) fn failure_text(r: &ExecutionFinalResult) -> String {
    let s = format!("{:?}", r.clone().into_result().err());
    // Pull out the contract panic message if there is one; the full debug dump is long.
    match s.find("ExecutionError(") {
        Some(i) => s[i..].chars().take(300).collect(),
        None => s.chars().take(600).collect(),
    }
}

impl Near {
    pub fn rpc(url: &str) -> Result<Self> {
        Ok(Near {
            network: NetworkConfig::from_rpc_url("custom", url.parse().context("rpc url")?),
        })
    }

    pub fn testnet() -> Self {
        Near {
            network: NetworkConfig::testnet(),
        }
    }

    pub fn mainnet() -> Self {
        Near {
            network: NetworkConfig::mainnet(),
        }
    }

    /// Sign and send a function call; error if any receipt failed.
    pub async fn call(&self, who: &Actor, contract: &str, method: &str, args: Value, deposit: NearToken, gas_tgas: u64) -> Result<Outcome> {
        let contract: AccountId = contract.parse()?;
        let r = Contract(contract.clone())
            .call_function(method, args)
            .transaction()
            .gas(NearGas::from_tgas(gas_tgas))
            .deposit(deposit)
            .with_signer(who.id.clone(), who.signer.clone())
            .send_to(&self.network)
            .await
            .with_context(|| format!("{method} on {contract}"))?;
        let full = r.into_full().ok_or_else(|| anyhow!("{method}: transaction still pending"))?;
        let gas_burnt = full.total_gas_burnt.as_gas();
        let tx_hash = full.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
        // Only the transaction's own status counts. Detached receipts may fail by design (e.g. a
        // withdrawal's ft_transfer to an unregistered account, which the contract re-credits).
        if full.clone().into_result().is_err() {
            bail!("{method} on {contract} failed (tx {tx_hash}): {}", failure_text(&full));
        }
        let logs = full.logs().into_iter().map(String::from).collect();
        let ok = full.into_result().map_err(|e| anyhow!("{method}: {e:?}"))?;
        let value = ok.json::<Value>().unwrap_or(Value::Null);
        Ok(Outcome { gas_burnt, tx_hash, value, logs })
    }

    pub async fn view<T: DeserializeOwned + Send + Sync + 'static>(&self, contract: &str, method: &str, args: Value) -> Result<T> {
        let contract: AccountId = contract.parse()?;
        let d = Contract(contract)
            .call_function(method, args)
            .read_only::<T>()
            .fetch_from(&self.network)
            .await
            .with_context(|| format!("view {method}"))?;
        Ok(d.data)
    }

    /// Create `<name>.<parent>` with a fresh full-access key.
    pub async fn create_subaccount(&self, parent: &Actor, name: &str, near: NearToken) -> Result<Actor> {
        let id: AccountId = format!("{name}.{}", parent.id).parse()?;
        let sk = near_api::signer::generate_secret_key()?;
        near_api::Account::create_account(id.clone())
            .fund_myself(parent.id.clone(), near)
            .with_public_key(sk.public_key())
            .with_signer(parent.signer.clone())
            .send_to(&self.network)
            .await
            .with_context(|| format!("create {id}"))?
            .into_result()
            .map_err(|e| anyhow!("create {id}: {e:?}"))?;
        Actor::new(id.as_str(), &sk.to_string())
    }

    pub async fn deploy(&self, who: &Actor, wasm: Vec<u8>) -> Result<()> {
        Contract::deploy(who.id.clone())
            .use_code(wasm)
            .without_init_call()
            .with_signer(who.signer.clone())
            .send_to(&self.network)
            .await?
            .into_result()
            .map_err(|e| anyhow!("deploy {}: {e:?}", who.id))?;
        Ok(())
    }
}

/// NEP-141 helpers.
#[derive(Clone)]
pub struct Ft {
    pub near: Near,
    pub token: String,
}

impl Ft {
    pub async fn storage_deposit(&self, payer: &Actor, account: &str) -> Result<()> {
        self.near
            .call(
                payer,
                &self.token,
                "storage_deposit",
                json!({"account_id": account, "registration_only": true}),
                NearToken::from_millinear(10),
                30,
            )
            .await?;
        Ok(())
    }

    /// mock-ft faucet (test tokens only).
    pub async fn mint(&self, who: &Actor, amount: u128) -> Result<()> {
        self.near
            .call(
                who,
                &self.token,
                "mint",
                json!({"account_id": who.id, "amount": amount.to_string()}),
                NearToken::from_millinear(10),
                30,
            )
            .await?;
        Ok(())
    }

    pub async fn balance(&self, account: &str) -> Result<u128> {
        let v: String = self.near.view(&self.token, "ft_balance_of", json!({"account_id": account})).await?;
        Ok(v.parse()?)
    }
}

/// The zfx-perps settlement contract.
#[derive(Clone)]
pub struct Settlement {
    pub near: Near,
    pub contract: String,
}

impl Settlement {
    pub fn new(near: Near, contract: &str) -> Self {
        Settlement {
            near,
            contract: contract.into(),
        }
    }

    // ---- views ----

    pub async fn state(&self) -> Result<Value> {
        self.near.view(&self.contract, "get_state", json!({})).await
    }

    /// Events with seq in [from, from+limit), as (seq, event JSON).
    pub async fn events(&self, from: u64, limit: u32) -> Result<Vec<(u64, Value)>> {
        let v: Vec<(String, Value)> = self
            .near
            .view(&self.contract, "get_events", json!({"from": from.to_string(), "limit": limit}))
            .await?;
        v.into_iter().map(|(s, e)| Ok((s.parse()?, e))).collect()
    }

    pub async fn balance(&self, account: &str, token: &str) -> Result<u128> {
        let v: String = self
            .near
            .view(&self.contract, "get_balance", json!({"account": account, "token": token}))
            .await?;
        Ok(v.parse()?)
    }

    pub async fn keys(&self, account: &str) -> Result<Vec<String>> {
        self.near.view(&self.contract, "get_keys", json!({"account": account})).await
    }

    pub async fn withdraw_nonce(&self, account: &str) -> Result<u64> {
        let v: String = self
            .near
            .view(&self.contract, "get_withdraw_nonce", json!({"account": account}))
            .await?;
        Ok(v.parse()?)
    }

    // ---- users ----

    /// Deposit with `ft_transfer_call`. Credits `who` (or `beneficiary`).
    pub async fn deposit(&self, who: &Actor, token: &str, amount: u128, beneficiary: Option<&str>) -> Result<Outcome> {
        let out = self
            .near
            .call(
                who,
                token,
                "ft_transfer_call",
                json!({"receiver_id": self.contract, "amount": amount.to_string(), "msg": beneficiary.unwrap_or("")}),
                NearToken::from_yoctonear(1),
                100,
            )
            .await?;
        // ft_transfer_call returns the amount actually used; the rest was refunded.
        let used: u128 = out.value.as_str().unwrap_or("0").parse().unwrap_or(0);
        if used != amount {
            bail!("deposit refunded: used {used} of {amount} (token not listed, contract frozen, or bad beneficiary)");
        }
        Ok(out)
    }

    /// Register an ed25519 trading key (`ed25519:<base58>`). Costs 0.01 NEAR, refunded on removal.
    pub async fn register_key(&self, who: &Actor, public_key: &str) -> Result<Outcome> {
        self.near
            .call(
                who,
                &self.contract,
                "register_key",
                json!({"public_key": public_key}),
                NearToken::from_millinear(10),
                30,
            )
            .await
    }

    pub async fn remove_key(&self, who: &Actor, public_key: &str) -> Result<Outcome> {
        self.near
            .call(
                who,
                &self.contract,
                "remove_key",
                json!({"public_key": public_key}),
                NearToken::from_yoctonear(0),
                30,
            )
            .await
    }

    /// File a forced withdrawal the operator must honor within the contract's timeout.
    pub async fn request_forced_withdrawal(&self, who: &Actor, token: &str, amount: u128) -> Result<u64> {
        let o = self
            .near
            .call(
                who,
                &self.contract,
                "request_forced_withdrawal",
                json!({"token": token, "amount": amount.to_string()}),
                NearToken::from_millinear(10),
                30,
            )
            .await?;
        Ok(o.value.as_str().unwrap_or("0").parse()?)
    }

    pub async fn freeze(&self, who: &Actor) -> Result<Outcome> {
        self.near
            .call(who, &self.contract, "freeze", json!({}), NearToken::from_yoctonear(0), 30)
            .await
    }

    pub async fn escape_withdraw(&self, who: &Actor, token: &str) -> Result<Outcome> {
        self.near
            .call(
                who,
                &self.contract,
                "escape_withdraw",
                json!({"token": token}),
                NearToken::from_yoctonear(0),
                100,
            )
            .await
    }

    // ---- operator / owner ----

    pub async fn settle_batch(&self, operator: &Actor, batch: &Value) -> Result<Outcome> {
        self.near
            .call(
                operator,
                &self.contract,
                "settle_batch",
                json!({"batch": batch}),
                NearToken::from_yoctonear(0),
                300,
            )
            .await
    }

    pub async fn prune_events(&self, operator: &Actor, upto: u64) -> Result<Outcome> {
        self.near
            .call(
                operator,
                &self.contract,
                "prune_events",
                json!({"upto": upto.to_string()}),
                NearToken::from_yoctonear(0),
                300,
            )
            .await
    }

    pub async fn add_token(&self, owner: &Actor, token: &str) -> Result<Outcome> {
        self.near
            .call(
                owner,
                &self.contract,
                "add_token",
                json!({"token": token}),
                NearToken::from_yoctonear(0),
                30,
            )
            .await
    }
}
