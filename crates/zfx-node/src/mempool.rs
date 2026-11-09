//! Bounded mempool for transactions and bridge operations. Items are
//! authorization-checked on entry and re-checked against tip state after
//! every tip change.

use std::collections::HashMap;
use zfx_verify::bridge::{op_id, BridgeOp};
use zfx_verify::hash::Hash32;
use zfx_verify::state::{ConsensusParams, State};
use zfx_verify::tx::Transaction;

pub const MAX_MEMPOOL_TXS: usize = 5000;
pub const MAX_MEMPOOL_OPS: usize = 256;

#[derive(Default)]
pub struct Mempool {
    pub txs: HashMap<Hash32, Transaction>,
    pub ops: HashMap<Hash32, BridgeOp>,
}

impl Mempool {
    /// Admit a tx if it is authorized and applies on top of the tip state
    /// plus the sender's already-pooled txs (nonce chaining). `height` is
    /// the height of the next block.
    pub fn admit(&mut self, tx: Transaction, p: &ConsensusParams, height: u64, tip: &State) -> Result<Hash32, String> {
        if tx.chain_id != p.chain_id {
            return Err("wrong chain id".into());
        }
        let id = tx.txid();
        if self.txs.contains_key(&id) {
            return Err("already in mempool".into());
        }
        if self.txs.len() >= MAX_MEMPOOL_TXS {
            return Err("mempool full".into());
        }
        if self.txs.values().any(|t| t.from == tx.from && t.nonce == tx.nonce) {
            return Err("conflicting tx with same sender nonce in mempool (double spend; replacement not supported)".into());
        }
        tx.verify_authorization()?;
        let mut st = tip.clone();
        let mut mine: Vec<&Transaction> = self.txs.values().filter(|t| t.from == tx.from).collect();
        mine.sort_by_key(|t| t.nonce);
        for t in mine {
            st.apply_tx(p, height, t)?;
        }
        st.apply_tx(p, height, &tx)?;
        self.txs.insert(id, tx);
        Ok(id)
    }

    /// Admit a bridge op if it applies to the tip state now (quorum
    /// signatures, replay, balance).
    pub fn admit_op(&mut self, op: BridgeOp, p: &ConsensusParams, height: u64, tip: &State) -> Result<Hash32, String> {
        let id = op_id(&op);
        if self.ops.contains_key(&id) {
            return Ok(id); // idempotent
        }
        if self.ops.len() >= MAX_MEMPOOL_OPS {
            return Err("bridge op pool full".into());
        }
        let mut st = tip.clone();
        for o in self.ops.values() {
            let _ = st.apply_bridge_op(p, height, o);
        }
        st.apply_bridge_op(p, height, &op)?;
        self.ops.insert(id, op);
        Ok(id)
    }

    pub fn ordered(&self) -> Vec<Transaction> {
        let mut v: Vec<Transaction> = self.txs.values().cloned().collect();
        v.sort_by(|a, b| a.nonce.cmp(&b.nonce).then(b.fee.cmp(&a.fee)).then(a.txid().cmp(&b.txid())));
        v
    }

    pub fn ordered_ops(&self) -> Vec<BridgeOp> {
        let mut v: Vec<(Hash32, BridgeOp)> = self.ops.iter().map(|(k, v)| (*k, v.clone())).collect();
        // admin ops first (by nonce), then releases by id: deterministic
        v.sort_by(|a, b| {
            let ka = matches!(a.1, BridgeOp::Release(_));
            let kb = matches!(b.1, BridgeOp::Release(_));
            ka.cmp(&kb).then(a.0.cmp(&b.0))
        });
        v.into_iter().map(|x| x.1).collect()
    }

    /// Drop items that no longer apply (mined, or invalidated by a reorg).
    pub fn revalidate(&mut self, p: &ConsensusParams, height: u64, tip: &State) {
        let mut st = tip.clone();
        let mut keep = HashMap::new();
        for t in self.ordered() {
            let mut s2 = st.clone();
            if s2.apply_tx(p, height, &t).is_ok() {
                st = s2;
                keep.insert(t.txid(), t);
            }
        }
        self.txs = keep;
        let mut keep_ops = HashMap::new();
        for o in self.ordered_ops() {
            let mut s2 = st.clone();
            if s2.apply_bridge_op(p, height, &o).is_ok() {
                st = s2;
                keep_ops.insert(op_id(&o), o);
            }
        }
        self.ops = keep_ops;
    }
}
