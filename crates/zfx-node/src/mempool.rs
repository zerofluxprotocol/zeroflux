//! Bounded mempool. Transactions are authorization-checked on entry and
//! re-checked against tip state after every tip change.

use crate::chain::State;
use std::collections::HashMap;
use zfx_verify::hash::Hash32;
use zfx_verify::tx::Transaction;

pub const MAX_MEMPOOL_TXS: usize = 5000;

#[derive(Default)]
pub struct Mempool {
    pub txs: HashMap<Hash32, Transaction>,
}

impl Mempool {
    /// Admit a tx if it is authorized and applies on top of the tip state
    /// plus the sender's already-pooled txs (nonce chaining).
    pub fn admit(&mut self, tx: Transaction, chain_id: u32, tip: &State) -> Result<Hash32, String> {
        if tx.chain_id != chain_id {
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
            return Err("conflicting tx with same sender nonce in mempool (double spend)".into());
        }
        tx.verify_authorization()?;
        let mut st = tip.clone();
        let mut mine: Vec<&Transaction> = self.txs.values().filter(|t| t.from == tx.from).collect();
        mine.sort_by_key(|t| t.nonce);
        for t in mine {
            st.apply_tx(t)?;
        }
        st.apply_tx(&tx)?;
        self.txs.insert(id, tx);
        Ok(id)
    }

    /// Ordered for block building: by sender nonce, then fee descending.
    pub fn ordered(&self) -> Vec<Transaction> {
        let mut v: Vec<Transaction> = self.txs.values().cloned().collect();
        v.sort_by(|a, b| a.nonce.cmp(&b.nonce).then(b.fee.cmp(&a.fee)).then(a.txid().cmp(&b.txid())));
        v
    }

    /// Drop txs that no longer apply (mined, or invalidated by a reorg).
    pub fn revalidate(&mut self, tip: &State) {
        let mut st = tip.clone();
        let mut keep = HashMap::new();
        for t in self.ordered() {
            let mut s2 = st.clone();
            if s2.apply_tx(&t).is_ok() {
                st = s2;
                keep.insert(t.txid(), t);
            }
        }
        self.txs = keep;
    }
}
