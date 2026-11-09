//! Canonical NEAR-side digests; byte-identical to bridge/near/wzfx.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MintAttestation {
    pub epoch: u32,
    pub deposit_id: String,
    pub recipient: String,
    /// u128 as decimal string (NEAR U128 JSON form).
    pub amount: String,
    pub native_height: String,
    pub expiry_ns: String,
}

pub fn mint_digest(contract: &str, chain_id: u32, asset_id: &str, a: &MintAttestation) -> [u8; 32] {
    let mut b = b"ZFX-near-mint-v1".to_vec();
    put_str(&mut b, contract);
    b.extend_from_slice(&chain_id.to_le_bytes());
    put_str(&mut b, asset_id);
    b.extend_from_slice(&a.epoch.to_le_bytes());
    b.extend_from_slice(&hex::decode(&a.deposit_id).unwrap_or_default());
    put_str(&mut b, &a.recipient);
    b.extend_from_slice(&a.amount.parse::<u128>().unwrap_or(0).to_le_bytes());
    b.extend_from_slice(&a.native_height.parse::<u64>().unwrap_or(0).to_le_bytes());
    b.extend_from_slice(&a.expiry_ns.parse::<u64>().unwrap_or(0).to_le_bytes());
    Sha256::digest(&b).into()
}

pub enum NearAdmin<'a> {
    Pause,
    Unpause,
    Rotate(&'a [String]),
}

pub fn near_admin_digest(contract: &str, epoch: u32, admin_nonce: u64, action: NearAdmin) -> [u8; 32] {
    let mut b = b"ZFX-near-admin-v1".to_vec();
    put_str(&mut b, contract);
    b.extend_from_slice(&epoch.to_le_bytes());
    b.extend_from_slice(&admin_nonce.to_le_bytes());
    match action {
        NearAdmin::Pause => b.push(1),
        NearAdmin::Unpause => b.push(2),
        NearAdmin::Rotate(keys) => {
            b.push(3);
            for k in keys {
                put_str(&mut b, k);
            }
        }
    }
    Sha256::digest(&b).into()
}

pub fn withdrawal_id(contract: &str, nonce: u64) -> String {
    let mut b = b"ZFX-near-burn-v1".to_vec();
    put_str(&mut b, contract);
    b.extend_from_slice(&nonce.to_le_bytes());
    hex::encode(Sha256::digest(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden values shared with bridge/near/wzfx/src/tests.rs
    /// (`digests_match_relayer_golden_vectors`); both must change together.
    pub const GOLDEN_MINT: &str = "3507a0edc7baab770827cebebe5c7df439be2f91c33f9d3f7391c3b2a9647e98";
    pub const GOLDEN_ADMIN_ROTATE: &str = "038fb280438db03918560ae6fc462deb4003faa6e4d6c58380d486bfb24d139c";
    pub const GOLDEN_WITHDRAWAL: &str = "b1447206675f11affd8453b8d33f158c2b4e9b0ca809e2e5c6beaa0d26649b82";

    pub fn golden() -> (String, String, String) {
        let a = MintAttestation {
            epoch: 3,
            deposit_id: "ab".repeat(32),
            recipient: "alice.test.near".into(),
            amount: "123456789".into(),
            native_height: "77".into(),
            expiry_ns: "1800000000000000000".into(),
        };
        let keys = vec!["11".repeat(32), "22".repeat(32), "33".repeat(32)];
        (
            hex::encode(mint_digest("wzfx.test.near", 0x5a460002, "zfx:5a460002:native", &a)),
            hex::encode(near_admin_digest("wzfx.test.near", 3, 9, NearAdmin::Rotate(&keys))),
            withdrawal_id("wzfx.test.near", 42),
        )
    }

    #[test]
    fn digests_match_golden_vectors() {
        let (m, r, w) = golden();
        assert_eq!((m.as_str(), r.as_str(), w.as_str()), (GOLDEN_MINT, GOLDEN_ADMIN_ROTATE, GOLDEN_WITHDRAWAL));
    }
}
