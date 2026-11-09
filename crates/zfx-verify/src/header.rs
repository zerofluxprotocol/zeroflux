//! Canonical block header encoding (CONSENSUS.md section 2). Fixed layout,
//! little-endian integers, no language serialization involved.

use crate::hash::{sha256, Hash32};
use crate::u256::U256;
use serde::{Deserialize, Serialize};

pub const HEADER_MAGIC: &[u8; 4] = b"ZFXH";
pub const HEADER_WITHOUT_NONCE_LEN: usize = 124;
pub const HEADER_LEN: usize = 132;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Consensus/PoW profile id (see `pow::Profile`).
    pub version: u32,
    pub chain_id: u32,
    pub height: u64,
    #[serde(with = "hex32")]
    pub parent: Hash32,
    #[serde(with = "hex32")]
    pub tx_root: Hash32,
    /// Unix seconds.
    pub timestamp: u64,
    /// uint256 big-endian.
    #[serde(with = "hex32")]
    pub target: [u8; 32],
    pub nonce: u64,
}

impl BlockHeader {
    pub fn encode_without_nonce(&self) -> [u8; HEADER_WITHOUT_NONCE_LEN] {
        let mut b = [0u8; HEADER_WITHOUT_NONCE_LEN];
        b[0..4].copy_from_slice(HEADER_MAGIC);
        b[4..8].copy_from_slice(&self.version.to_le_bytes());
        b[8..12].copy_from_slice(&self.chain_id.to_le_bytes());
        b[12..20].copy_from_slice(&self.height.to_le_bytes());
        b[20..52].copy_from_slice(&self.parent);
        b[52..84].copy_from_slice(&self.tx_root);
        b[84..92].copy_from_slice(&self.timestamp.to_le_bytes());
        b[92..124].copy_from_slice(&self.target);
        b
    }

    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[..HEADER_WITHOUT_NONCE_LEN].copy_from_slice(&self.encode_without_nonce());
        b[124..132].copy_from_slice(&self.nonce.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Result<Self, String> {
        if b.len() != HEADER_LEN {
            return Err(format!("header length {} != {}", b.len(), HEADER_LEN));
        }
        if &b[0..4] != HEADER_MAGIC {
            return Err("bad header magic".into());
        }
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let h32 = |o: usize| -> [u8; 32] { b[o..o + 32].try_into().unwrap() };
        Ok(BlockHeader {
            version: u32_at(4),
            chain_id: u32_at(8),
            height: u64_at(12),
            parent: h32(20),
            tx_root: h32(52),
            timestamp: u64_at(84),
            target: h32(92),
            nonce: u64_at(124),
        })
    }

    pub fn hash(&self) -> Hash32 {
        sha256(&[b"ZFX-block-v0.1", &self.encode()])
    }

    pub fn target_u256(&self) -> U256 {
        U256::from_be_bytes(&self.target)
    }
}

pub mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(&s).map_err(serde::de::Error::custom)?;
        v.try_into().map_err(|_| serde::de::Error::custom("expected 32 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let h = BlockHeader {
            version: 1,
            chain_id: 7,
            height: 9,
            parent: [1; 32],
            tx_root: [2; 32],
            timestamp: 1_800_000_000,
            target: [0xff; 32],
            nonce: 0xdead_beef,
        };
        let e = h.encode();
        assert_eq!(BlockHeader::decode(&e).unwrap(), h);
        assert!(BlockHeader::decode(&e[..131]).is_err());
    }
}
