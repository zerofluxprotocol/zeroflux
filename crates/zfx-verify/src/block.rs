//! Blocks and the transaction commitment.

use crate::hash::{sha256, Hash32};
use crate::header::BlockHeader;
use crate::tx::{Coinbase, Transaction, TX_ENCODED_LEN};
use serde::{Deserialize, Serialize};

pub const MAX_TXS_PER_BLOCK: usize = 2000;
pub const MAX_BLOCK_BYTES: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub coinbase: Coinbase,
    pub txs: Vec<Transaction>,
}

/// Binary Merkle tree; an odd node is promoted unchanged (no duplication,
/// avoiding the CVE-2012-2459 malleability), and the leaf count is bound
/// into the root.
pub fn merkle_root(leaves: &[Hash32]) -> Hash32 {
    let mut level: Vec<Hash32> = leaves.to_vec();
    if level.is_empty() {
        level.push([0u8; 32]);
    }
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            if pair.len() == 2 {
                next.push(sha256(&[b"ZFX-merkle-v0.1", &pair[0], &pair[1]]));
            } else {
                next.push(pair[0]);
            }
        }
        level = next;
    }
    sha256(&[b"ZFX-txroot-v0.1", &(leaves.len() as u32).to_le_bytes(), &level[0]])
}

impl Block {
    pub fn compute_tx_root(&self) -> Hash32 {
        let mut leaves = vec![self.coinbase.id()];
        leaves.extend(self.txs.iter().map(|t| t.txid()));
        merkle_root(&leaves)
    }

    pub fn encoded_size(&self) -> usize {
        crate::header::HEADER_LEN + 36 + 4 + self.txs.len() * TX_ENCODED_LEN
    }

    pub fn hash(&self) -> Hash32 {
        self.header.hash()
    }

    /// Context-free structural checks (no state, no PoW).
    pub fn check_structure(&self) -> Result<(), String> {
        if self.txs.len() > MAX_TXS_PER_BLOCK {
            return Err("too many transactions".into());
        }
        if self.encoded_size() > MAX_BLOCK_BYTES {
            return Err("block too large".into());
        }
        if self.coinbase.height != self.header.height {
            return Err("coinbase height mismatch".into());
        }
        if self.compute_tx_root() != self.header.tx_root {
            return Err("tx_root mismatch".into());
        }
        for t in &self.txs {
            if t.chain_id != self.header.chain_id {
                return Err("tx chain_id mismatch".into());
            }
        }
        Ok(())
    }
}
