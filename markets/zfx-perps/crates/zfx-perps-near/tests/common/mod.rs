//! Helpers for the clearing-contract sandbox tests: real-time prints and signed orders.

#![allow(dead_code)]

use anyhow::Result;
use near_api::NearToken;
use serde_json::Value;
use zfx_perps_clearing::testkit::{Key, Trader};
use zfx_perps_clearing::wire::{OrderType, Side, Sig64, SignedOrder, TimeInForce};
use zfx_perps_clearing::*;
use zfx_perps_near::clearing::{Clearing, ClearingStack};
use zfx_perps_near::{Actor, Ft, Near};

pub const DAY_MS: i64 = 86_400_000;

pub fn now_ns() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
}

pub fn pk_str(k: &Key) -> String {
    format!("ed25519:{}", bs58::encode(k.public()).into_string())
}

pub fn reporter() -> Trader {
    Trader::new("reporter", 201)
}

/// A FINAL print for the UTC day that ended at the last midnight, published now.
pub fn print(index_id: &str, version: &str, value: u64, revision: u32, prev: [u8; 32]) -> Vec<u8> {
    let now_ms = (now_ns() / 1_000_000) as i64;
    let end = now_ms - now_ms % DAY_MS;
    OraclePrint {
        index_id: index_id.into(),
        version: version.into(),
        value_scale: 6,
        value,
        source_start_ms: end - DAY_MS,
        source_end_ms: end,
        observed_at_ms: now_ms,
        published_at_ms: now_ms,
        valid_until_ms: end + 2 * DAY_MS,
        source_hash: [0x11; 32],
        formula_hash: [0x22; 32],
        status: OracleStatus::Fresh,
        data_status: DataStatus::Ok,
        finality: if revision > 1 { Finality::Corrected } else { Finality::Final },
        revision,
        prev_print_hash: prev,
    }
    .encode()
    .unwrap()
}

pub fn oracle_op(market: &str, p: &[u8], r: &Trader) -> Op {
    Op::Oracle {
        market: market.into(),
        print: p.to_vec(),
        sigs: vec![(r.key.public(), Sig64(r.key.sign(p)))],
    }
}

pub struct Ctx {
    pub near: Near,
    pub stk: ClearingStack,
    pub c: Clearing,
    pub operator: Actor,
    pub owner: Actor,
    pub policy: [u8; 32],
    pub nonce: u64,
    pub batch: u64,
    pub market: String,
}

impl Ctx {
    pub async fn refresh_policy(&mut self) -> Result<()> {
        let m = self.c.market(&self.market).await?;
        self.policy = wire::parse_hex32(m["policy_hash"].as_str().unwrap()).unwrap();
        Ok(())
    }

    pub fn order(&mut self, t: &Trader, side: Side, price: u64, qty: u64, f: impl FnOnce(&mut SignedOrder)) -> SignedOrder {
        self.nonce += 1;
        let mut o = SignedOrder {
            chain_id: self.stk.chain_id.clone(),
            clearing_contract: self.stk.contract.clone(),
            account: t.name.clone(),
            market: self.market.clone(),
            side,
            order_type: OrderType::Limit,
            price,
            qty,
            reduce_only: false,
            post_only: false,
            tif: TimeInForce::Gtc,
            nonce: self.nonce,
            expiry_ns: now_ns() + 3_600_000_000_000,
            client_order_id: self.nonce,
            max_fee_bps: 10,
            policy_hash: self.policy,
            public_key: t.key.public(),
            signature: Sig64([0; 64]),
        };
        f(&mut o);
        o.signature = Sig64(t.key.sign(&o.message()));
        o
    }

    /// Settle a batch stamped now. Returns (gas Tgas, tx hash, rejected [(index, reason)]).
    pub async fn settle(&mut self, orders: Vec<SignedOrder>, ops: Vec<Op>) -> Result<(f64, String, Vec<(u64, String)>)> {
        self.batch += 1;
        let ts = now_ns().saturating_sub(2_000_000_000);
        let b = Batch {
            id: self.batch,
            orders,
            ops: ops.into_iter().map(|op| OpAt { ts_ns: ts, op }).collect(),
            journal_seq: self.batch,
            journal_hash: [0; 32],
        };
        let out = self.c.settle_batch(&self.operator, &b).await?;
        let st = self.c.state().await?;
        let next: u64 = st["next_event"].as_str().unwrap().parse()?;
        let ev = self.c.events(next - 1, 1).await?;
        let rej = ev[0].1["rejected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r[0].as_u64().unwrap(), r[1].as_str().unwrap().to_string()))
            .collect();
        Ok((out.gas_burnt as f64 / 1e12, out.tx_hash, rej))
    }

    /// Create `<name>.<root>`, mint test USDC, deposit it, register the trading key, credit it.
    pub async fn user(&mut self, root: &Actor, name: &str, usdc: u128) -> Result<(Actor, Trader)> {
        let a = self.near.create_subaccount(root, name, NearToken::from_near(2)).await?;
        let t = Trader::new(a.id.as_str(), 1);
        let ft = Ft {
            near: self.near.clone(),
            token: self.stk.collateral.clone(),
        };
        self.near
            .call(&a, &self.stk.collateral, "mint", serde_json::json!({"account_id": a.id, "amount": usdc.to_string()}), NearToken::from_millinear(10), 30)
            .await?;
        let _ = ft;
        self.c.register_key(&a, &pk_str(&t.key), None, true).await?;
        self.c.deposit(&a, &self.stk.collateral, usdc, None).await?;
        let st = self.c.state().await?;
        let id: u64 = st["next_deposit"].as_str().unwrap().parse::<u64>()? - 1;
        let (_, _, rej) = self.settle(vec![], vec![Op::Credit { deposit_id: id }]).await?;
        assert!(rej.is_empty(), "{rej:?}");
        Ok((a, t))
    }

    pub async fn free(&self, a: &str) -> Result<u128> {
        let v: Value = self.c.account(a).await?;
        Ok(v["free"].as_str().unwrap().parse()?)
    }

    pub async fn size(&self, a: &str) -> Result<i64> {
        let v: Value = self.c.account(a).await?;
        Ok(v["positions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["market"] == self.market.as_str())
            .map_or(0, |p| p["size"].as_str().unwrap().parse().unwrap()))
    }
}
