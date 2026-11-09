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
