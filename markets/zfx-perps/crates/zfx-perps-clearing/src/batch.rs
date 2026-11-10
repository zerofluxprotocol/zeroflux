//! Settlement batches: the ordered operations the matcher submits to the clearing contract.
//!
//! A batch carries an order table (each signed order once) and a list of timestamped ops. The
//! contract applies the ops in order with [`apply_batch`]. An op that fails any check is skipped
//! and reported; the rest still apply, so one stale order can't block settlement. The engine runs
//! the same function on its copy of the ledger, so it knows the outcome before the chain does.

use crate::ledger::{self, CEvent, Failure, Now, Orders, Reject};
use crate::model::NS_PER_SEC;
use crate::store::{Crypto, Store};
use crate::wire::{Sig64, SignedOrder};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum Op {
    /// Move a pending on-chain deposit into free collateral.
    Credit { deposit_id: u64 },
    /// One matched trade. `maker`/`taker` index the batch's order table.
    Fill { maker: u32, taker: u32, qty: u64, price: u64 },
    /// A reporter-signed index print for one market.
    Oracle {
        market: String,
        print: Vec<u8>,
        sigs: Vec<([u8; 32], Sig64)>,
    },
    Funding { market: String },
    Liquidate {
        account: String,
        market: String,
        fills: Vec<(u32, u64)>,
        adl: Vec<(String, u64)>,
        socialize: Vec<String>,
    },
    Withdraw {
        account: String,
        amount: u128,
        nonce: u64,
        public_key: [u8; 32],
        signature: Sig64,
    },
    AdjustMargin {
        account: String,
        market: String,
        delta: i128,
        nonce: u64,
        public_key: [u8; 32],
        signature: Sig64,
    },
    Forced { request_id: u64 },
    /// A trading-key-signed cancel relayed on-chain, making it final for every relayer.
    Cancel {
        account: String,
        order_hash: [u8; 32],
        expiry_ns: u64,
        public_key: [u8; 32],
        signature: Sig64,
    },
    MinNonce {
        account: String,
        min_nonce: u64,
        public_key: [u8; 32],
        signature: Sig64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct OpAt {
    pub ts_ns: u64,
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct Batch {
    pub id: u64,
    pub orders: Vec<SignedOrder>,
    pub ops: Vec<OpAt>,
    /// Journal position the batch was cut at (audit trail; the contract stores the latest).
    pub journal_seq: u64,
    pub journal_hash: [u8; 32],
}

/// Outcome of one op in a batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub index: u32,
    pub result: Result<Vec<CEvent>, Failure>,
}

pub fn apply_op<S: Store, C: Crypto>(s: &mut S, c: &C, orders: &mut Orders, now: Now, op: &Op) -> Result<Vec<CEvent>, Failure> {
    match op {
        Op::Credit { deposit_id } => ledger::credit(s, *deposit_id),
        Op::Fill { maker, taker, qty, price } => ledger::fill(s, c, orders, now, *maker, *taker, *qty, *price),
        Op::Oracle { market, print, sigs } => ledger::oracle(s, c, now, market, print, sigs),
        Op::Funding { market } => ledger::funding(s, now, market),
        Op::Liquidate {
            account,
            market,
            fills,
            adl,
            socialize,
        } => ledger::liquidate(s, c, orders, now, account, market, fills, adl, socialize),
        Op::Withdraw {
            account,
            amount,
            nonce,
            public_key,
            signature,
        } => ledger::withdraw(s, c, now, account, *amount, *nonce, public_key, &signature.0),
        Op::AdjustMargin {
            account,
            market,
            delta,
            nonce,
            public_key,
            signature,
        } => ledger::adjust_margin(s, c, now, account, market, *delta, *nonce, public_key, &signature.0),
        Op::Forced { request_id } => ledger::forced(s, *request_id),
        Op::Cancel {
            account,
            order_hash,
            expiry_ns,
            public_key,
            signature,
        } => ledger::cancel(s, c, account, order_hash, *expiry_ns, Some((public_key, &signature.0))),
        Op::MinNonce {
            account,
            min_nonce,
            public_key,
            signature,
        } => ledger::set_min_nonce(s, c, account, *min_nonce, Some((public_key, &signature.0))),
    }
}

/// Check an op's timestamp against the last op and the block time, and advance the clock.
pub fn check_time<S: Store>(s: &mut S, ts_ns: u64, block_ns: u64) -> Result<Now, Reject> {
    let g = s.globals_mut();
    if ts_ns < g.last_op_ns {
        return Err(Reject::TimeBackwards);
    }
    if ts_ns > block_ns.saturating_add(ledger::MAX_FUTURE_SKEW_NS) {
        return Err(Reject::TimeFuture);
    }
    g.last_op_ns = ts_ns;
    Ok(Now { op: ts_ns, block: block_ns })
}

/// Apply every op of a batch in order. Failed ops are skipped and reported.
/// `block_ns` is the block timestamp (on-chain) or the op's own timestamp (engine prediction).
pub fn apply_batch<S: Store, C: Crypto>(s: &mut S, c: &C, batch: &Batch, block_ns: Option<u64>, trust_signatures: bool) -> Vec<Applied> {
    let mut orders = Orders::new(&batch.orders, trust_signatures);
    let mut out = Vec::with_capacity(batch.ops.len());
    for (i, o) in batch.ops.iter().enumerate() {
        let result = check_time(s, o.ts_ns, block_ns.unwrap_or(o.ts_ns))
            .map_err(Failure::from)
            .and_then(|now| apply_op(s, c, &mut orders, now, &o.op));
        out.push(Applied { index: i as u32, result });
    }
    out
}

/// Rough gas model for packing batches (Tgas, measured on the sandbox; see docs).
pub fn estimate_tgas(op: &Op, new_orders: usize) -> f64 {
    let sig = 0.75 * new_orders as f64;
    sig + match op {
        Op::Fill { .. } => 2.5,
        Op::Liquidate { fills, adl, socialize, .. } => 4.0 + 2.0 * fills.len() as f64 + 1.0 * adl.len() as f64 + 0.6 * socialize.len() as f64,
        Op::Oracle { sigs, .. } => 1.5 + 0.75 * sigs.len() as f64,
        Op::Withdraw { .. } | Op::Forced { .. } => 9.0,
        _ => 1.5,
    }
}

pub const DEFAULT_SETTLE_GRACE_NS: u64 = 300 * NS_PER_SEC;
