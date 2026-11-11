//! Client for the zfx-perps clearing contract, and a deployer for a clearing stack
//! (mock collateral token + clearing contract + operator) on a sandbox or testnet.

use crate::{Actor, Ft, Near, Outcome};
use anyhow::{Context, Result, anyhow, bail};
use near_api::{AccountId, Contract, NearGas, NearToken};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zfx_perps_clearing::{Batch, MarketParams};

#[derive(Clone)]
pub struct Clearing {
    pub near: Near,
    pub contract: String,
}

impl Near {
    /// A function call with raw (borsh) arguments.
    pub async fn call_raw(&self, who: &Actor, contract: &str, method: &str, args: Vec<u8>, gas_tgas: u64) -> Result<Outcome> {
        let contract: AccountId = contract.parse()?;
        let r = Contract(contract.clone())
            .call_function_raw(method, args)
            .transaction()
            .gas(NearGas::from_tgas(gas_tgas))
            .deposit(NearToken::from_yoctonear(0))
            .with_signer(who.id.clone(), who.signer.clone())
            .send_to(&self.network)
            .await
            .with_context(|| format!("{method} on {contract}"))?;
        let full = r.into_full().ok_or_else(|| anyhow!("{method}: transaction still pending"))?;
        let gas_burnt = full.total_gas_burnt.as_gas();
        let tx_hash = full.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
        self.record(format!("{method} on {contract} by {}", who.id), &tx_hash);
        if full.clone().into_result().is_err() {
            bail!("{method} on {contract} failed (tx {tx_hash}): {}", crate::failure_text(&full));
        }
        let logs = full.logs().into_iter().map(String::from).collect();
        Ok(Outcome {
            gas_burnt,
            tx_hash,
            value: Value::Null,
            logs,
        })
    }
}

impl Clearing {
    pub fn new(near: Near, contract: &str) -> Self {
        Clearing {
            near,
            contract: contract.into(),
        }
    }

    async fn call(&self, who: &Actor, method: &str, args: Value, deposit: NearToken, gas: u64) -> Result<Outcome> {
        self.near.call(who, &self.contract, method, args, deposit, gas).await
    }

    pub async fn view(&self, method: &str, args: Value) -> Result<Value> {
        self.near.view(&self.contract, method, args).await
    }

    pub async fn state(&self) -> Result<Value> {
        self.view("get_state", json!({})).await
    }
    pub async fn market(&self, market: &str) -> Result<Value> {
        self.view("get_market", json!({"market": market})).await
    }
    pub async fn markets(&self) -> Result<Value> {
        self.view("get_markets", json!({})).await
    }
    pub async fn account(&self, account: &str) -> Result<Value> {
        self.view("get_account", json!({"account": account})).await
    }
    pub async fn order(&self, hash_hex: &str) -> Result<Value> {
        self.view("get_order", json!({"order_hash": hash_hex})).await
    }

    /// Events with seq in [from, from+limit), as (seq, event JSON).
    pub async fn events(&self, from: u64, limit: u32) -> Result<Vec<(u64, Value)>> {
        let v: Vec<(String, Value)> = self
            .near
            .view(&self.contract, "get_events", json!({"from": from.to_string(), "limit": limit}))
            .await?;
        v.into_iter().map(|(s, e)| Ok((s.parse()?, e))).collect()
    }

    // ---- users ----

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
        let used: u128 = out.value.as_str().unwrap_or("0").parse().unwrap_or(0);
        if used != amount {
            bail!("deposit refunded: used {used} of {amount}");
        }
        Ok(out)
    }

    pub async fn register_key(&self, who: &Actor, public_key: &str, expires_ns: Option<u64>, can_withdraw: bool) -> Result<Outcome> {
        self.call(
            who,
            "register_key",
            json!({"public_key": public_key, "expires_ns": expires_ns.map(|x| x.to_string()), "can_withdraw": can_withdraw}),
            NearToken::from_millinear(10),
            30,
        )
        .await
    }

    pub async fn remove_key(&self, who: &Actor, public_key: &str) -> Result<Outcome> {
        self.call(who, "remove_key", json!({"public_key": public_key}), NearToken::from_yoctonear(0), 30)
            .await
    }

    pub async fn cancel_order(&self, who: &Actor, hash_hex: &str, expiry_ns: u64) -> Result<Outcome> {
        self.call(
            who,
            "cancel_order",
            json!({"order_hash": hash_hex, "expiry_ns": expiry_ns.to_string()}),
            NearToken::from_yoctonear(0),
            30,
        )
        .await
    }

    pub async fn set_min_nonce(&self, who: &Actor, min_nonce: u64) -> Result<Outcome> {
        self.call(who, "set_min_nonce", json!({"min_nonce": min_nonce.to_string()}), NearToken::from_yoctonear(0), 30)
            .await
    }

    pub async fn request_forced_withdrawal(&self, who: &Actor, amount: u128) -> Result<u64> {
        let o = self
            .call(who, "request_forced_withdrawal", json!({"amount": amount.to_string()}), NearToken::from_millinear(10), 30)
            .await?;
        Ok(o.value.as_str().unwrap_or("0").parse()?)
    }

    pub async fn freeze(&self, who: &Actor) -> Result<Outcome> {
        self.call(who, "freeze", json!({}), NearToken::from_yoctonear(0), 30).await
    }

    pub async fn escape_scan(&self, who: &Actor, limit: u32) -> Result<Outcome> {
        self.call(who, "escape_scan", json!({"limit": limit}), NearToken::from_yoctonear(0), 300).await
    }

    pub async fn escape_withdraw(&self, who: &Actor) -> Result<Outcome> {
        self.call(who, "escape_withdraw", json!({}), NearToken::from_yoctonear(0), 100).await
    }

    // ---- operator / owner ----

    /// Submit a settlement batch (borsh). Returns gas burnt and the tx hash.
    pub async fn settle_batch(&self, operator: &Actor, batch: &Batch) -> Result<Outcome> {
        let args = borsh::to_vec(batch)?;
        self.near.call_raw(operator, &self.contract, "settle_batch", args, 300).await
    }

    pub async fn prune_events(&self, operator: &Actor, upto: u64) -> Result<Outcome> {
        self.call(operator, "prune_events", json!({"upto": upto.to_string()}), NearToken::from_yoctonear(0), 300)
            .await
    }

    pub async fn create_market(&self, owner: &Actor, params: &MarketParams) -> Result<Outcome> {
        self.call(owner, "create_market", json!({"params": params}), NearToken::from_yoctonear(0), 50)
            .await
    }

    pub async fn set_market_status(&self, owner: &Actor, market: &str, status: &str) -> Result<Option<u64>> {
        let o = self
            .call(owner, "set_market_status", json!({"market": market, "status": status}), NearToken::from_yoctonear(0), 50)
            .await?;
        Ok(o.value.as_str().map(|s| s.parse()).transpose()?)
    }

    pub async fn execute_proposal(&self, owner: &Actor, id: u64) -> Result<Outcome> {
        self.call(owner, "execute_proposal", json!({"id": id.to_string()}), NearToken::from_yoctonear(0), 100)
            .await
    }

    pub async fn set_operators(&self, owner: &Actor, operators: &[&str]) -> Result<Outcome> {
        self.call(owner, "set_operators", json!({"operators": operators}), NearToken::from_yoctonear(0), 30)
            .await
    }
}

/// Deployed clearing stack. Contains secret keys: keep it out of version control.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClearingStack {
    pub rpc_url: String,
    pub chain_id: String,
    pub contract: String,
    pub collateral: String,
    pub insurance_account: String,
    pub owner: crate::stack::ActorInfo,
    pub operator: crate::stack::ActorInfo,
    /// Full-access key of the contract account (testnet/sandbox only, so the code can be
    /// redeployed). A production deployment deletes every key on the contract account.
    #[serde(default)]
    pub contract_key: Option<crate::stack::ActorInfo>,
    /// ed25519 reporter public keys allowlisted at init.
    pub reporters: Vec<String>,
    pub quorum: u8,
}

pub struct ClearingOptions {
    pub chain_id: String,
    /// Subaccount names under the master.
    pub contract_name: String,
    pub token_name: String,
    pub operator_name: String,
    pub contract_near: u128,
    pub token_near: u128,
    pub operator_near: u128,
    pub reporters: Vec<String>,
    pub quorum: u8,
    pub force_timeout_ns: u64,
    pub timelock_ns: u64,
    /// Deploy a mock collateral token (sandbox/testnet). Otherwise `collateral` must be set.
    pub collateral: Option<String>,
}

impl Default for ClearingOptions {
    fn default() -> Self {
        ClearingOptions {
            chain_id: "near:sandbox".into(),
            contract_name: "perps".into(),
            token_name: "usdc".into(),
            operator_name: "operator".into(),
            contract_near: 10,
            token_near: 3,
            operator_near: 2,
            reporters: vec![],
            quorum: 1,
            force_timeout_ns: 24 * 3_600 * 1_000_000_000,
            timelock_ns: 0,
            collateral: None,
        }
    }
}

pub async fn deploy_clearing(near: &Near, rpc_url: &str, master: &Actor, o: &ClearingOptions) -> Result<ClearingStack> {
    let wasm = |n: &str| -> Result<Vec<u8>> {
        let p = crate::stack::out_dir().join(n);
        std::fs::read(&p).with_context(|| format!("{} (run scripts/build-contracts.sh)", p.display()))
    };
    let collateral = match &o.collateral {
        Some(c) => c.clone(),
        None => {
            let t = near
                .create_subaccount(master, &o.token_name, NearToken::from_near(o.token_near))
                .await?;
            near.deploy(&t, wasm("mock_ft.wasm")?).await?;
            near.call(
                &t,
                t.id.as_str(),
                "new",
                json!({"owner": master.id, "name": "Test USDC (zfx-perps test token, no value)", "symbol": "tUSDC", "decimals": 6,
                       "faucet_limit": "1000000000000"}),
                NearToken::from_yoctonear(0),
                30,
            )
            .await?;
            t.id.to_string()
        }
    };
    let operator = near
        .create_subaccount(master, &o.operator_name, NearToken::from_near(o.operator_near))
        .await?;
    let c = near
        .create_subaccount(master, &o.contract_name, NearToken::from_near(o.contract_near))
        .await?;
    near.deploy(&c, wasm("zfx_perps_clearing.wasm")?).await?;
    let insurance = format!("insurance.{}", c.id);
    near.call(
        &c,
        c.id.as_str(),
        "new",
        json!({"owner": master.id, "operator": operator.id, "chain_id": o.chain_id, "collateral": collateral,
               "insurance_account": insurance, "reporters": o.reporters, "quorum": o.quorum,
               "force_timeout_ns": o.force_timeout_ns.to_string(), "timelock_ns": o.timelock_ns.to_string()}),
        NearToken::from_yoctonear(0),
        30,
    )
    .await?;
    // The contract must be registered with the token to receive and send it.
    Ft {
        near: near.clone(),
        token: collateral.clone(),
    }
    .storage_deposit(master, c.id.as_str())
    .await?;
    Ok(ClearingStack {
        rpc_url: rpc_url.into(),
        chain_id: o.chain_id.clone(),
        contract: c.id.to_string(),
        collateral,
        insurance_account: insurance,
        owner: crate::stack::ActorInfo {
            account: master.id.to_string(),
            secret_key: master.secret.clone(),
        },
        operator: crate::stack::ActorInfo {
            account: operator.id.to_string(),
            secret_key: operator.secret.clone(),
        },
        contract_key: Some(crate::stack::ActorInfo {
            account: c.id.to_string(),
            secret_key: c.secret.clone(),
        }),
        reporters: o.reporters.clone(),
        quorum: o.quorum,
    })
}
