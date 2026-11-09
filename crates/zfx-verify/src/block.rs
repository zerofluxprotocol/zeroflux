//! Blocks and the transaction commitment.

use crate::hash::{sha256, Hash32};
use crate::header::BlockHeader;
use crate::bridge::{encode_op, op_id, BridgeOp, MAX_BRIDGE_OPS_PER_BLOCK};
use crate::tx::{Coinbase, Transaction};
use serde::{Deserialize, Serialize};

pub const MAX_TXS_PER_BLOCK: usize = 1000;
/// SLH-DSA-SHA2-128s transactions are ~8 KB, so 4 MiB holds ~500.
pub const MAX_BLOCK_BYTES: usize = 4 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub coinbase: Coinbase,
    pub txs: Vec<Transaction>,
    /// Bridge operations (consensus v2). Committed after the txs in tx_root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bridge_ops: Vec<BridgeOp>,
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
        leaves.extend(self.txs.iter().map(|t| t.wtxid()));
        leaves.extend(self.bridge_ops.iter().map(op_id));
        merkle_root(&leaves)
    }

    pub fn encoded_size(&self) -> usize {
        crate::header::HEADER_LEN + 48 + 4 + self.txs.iter().map(|t| t.encoded_len()).sum::<usize>() + 4 + self.bridge_ops.iter().map(|o| encode_op(o).len()).sum::<usize>()
    }

    pub fn hash(&self) -> Hash32 {
        self.header.hash()
    }

    /// Context-free structural checks (no state, no PoW).
    pub fn check_structure(&self) -> Result<(), String> {
        if self.txs.len() > MAX_TXS_PER_BLOCK {
            return Err("too many transactions".into());
        }
        if self.bridge_ops.len() > MAX_BRIDGE_OPS_PER_BLOCK {
            return Err("too many bridge operations".into());
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
            t.check_memo()?;
        }
        Ok(())
    }
}

/// Merkle inclusion proof for leaf `index` of `leaves` (tx_root form):
/// sibling hashes bottom-up, each tagged with whether the sibling is on the
/// right. Odd promoted nodes contribute no step.
pub fn merkle_proof(leaves: &[Hash32], mut index: usize) -> Vec<(Hash32, bool)> {
    let mut level: Vec<Hash32> = leaves.to_vec();
    let mut proof = Vec::new();
    while level.len() > 1 {
        let sib = index ^ 1;
        if sib < level.len() {
            proof.push((level[sib], sib > index));
        }
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            next.push(if pair.len() == 2 { sha256(&[b"ZFX-merkle-v0.1", &pair[0], &pair[1]]) } else { pair[0] });
        }
        level = next;
        index /= 2;
    }
    proof
}

/// Verify a `merkle_proof` against a tx_root.
pub fn verify_merkle_proof(tx_root: &Hash32, leaf_count: u32, leaf: &Hash32, proof: &[(Hash32, bool)]) -> bool {
    let mut h = *leaf;
    for (sib, right) in proof {
        h = if *right { sha256(&[b"ZFX-merkle-v0.1", &h, sib]) } else { sha256(&[b"ZFX-merkle-v0.1", sib, &h]) };
    }
    &sha256(&[b"ZFX-txroot-v0.1", &leaf_count.to_le_bytes(), &h]) == tx_root
}

impl Block {
    /// tx_root leaves in order: coinbase, wtxids, bridge op ids.
    pub fn leaves(&self) -> Vec<Hash32> {
        let mut leaves = vec![self.coinbase.id()];
        leaves.extend(self.txs.iter().map(|t| t.wtxid()));
        leaves.extend(self.bridge_ops.iter().map(op_id));
        leaves
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merkle_proofs_verify_for_every_leaf() {
        for n in 1..=9usize {
            let leaves: Vec<Hash32> = (0..n).map(|i| sha256(&[&[i as u8]])).collect();
            let root = merkle_root(&leaves);
            for i in 0..n {
                let p = merkle_proof(&leaves, i);
                assert!(verify_merkle_proof(&root, n as u32, &leaves[i], &p), "n={} i={}", n, i);
                assert!(!verify_merkle_proof(&root, n as u32, &sha256(&[b"x"]), &p));
                assert!(!verify_merkle_proof(&root, n as u32 + 1, &leaves[i], &p), "leaf count is bound");
            }
        }
    }
}
