//! Account-model transactions with Ed25519 signatures.

use crate::hash::{sha256, Hash32};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

pub type Address = [u8; 20];

pub fn address_of(pubkey: &[u8; 32]) -> Address {
    let h = sha256(&[b"ZFX-addr-v0.1", pubkey]);
    h[..20].try_into().unwrap()
}

/// Text form: "zfx" + hex(addr) + hex(first 4 bytes of sha256(addr)).
pub fn address_to_string(a: &Address) -> String {
    let c = sha256(&[b"ZFX-addrck", a]);
    format!("zfx{}{}", hex::encode(a), hex::encode(&c[..4]))
}

pub fn address_from_string(s: &str) -> Result<Address, String> {
    let body = s.strip_prefix("zfx").ok_or("address must start with zfx")?;
    let raw = hex::decode(body).map_err(|e| e.to_string())?;
    if raw.len() != 24 {
        return Err("address has wrong length".into());
    }
    let a: Address = raw[..20].try_into().unwrap();
    let c = sha256(&[b"ZFX-addrck", &a]);
    if c[..4] != raw[20..] {
        return Err("address checksum mismatch".into());
    }
    Ok(a)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub chain_id: u32,
    #[serde(with = "crate::header::hex32")]
    pub from_pubkey: [u8; 32],
    #[serde(with = "addr_hex")]
    pub to: Address,
    pub amount: u64,
    pub fee: u64,
    /// Sender account nonce; must equal the account's next nonce.
    pub nonce: u64,
    #[serde(with = "sig_hex")]
    pub signature: [u8; 64],
}

pub const TX_ENCODED_LEN: usize = 4 + 32 + 20 + 8 + 8 + 8 + 64;

impl Transaction {
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(96);
        b.extend_from_slice(b"ZFX-tx-v0.1");
        b.extend_from_slice(&self.chain_id.to_le_bytes());
        b.extend_from_slice(&self.from_pubkey);
        b.extend_from_slice(&self.to);
        b.extend_from_slice(&self.amount.to_le_bytes());
        b.extend_from_slice(&self.fee.to_le_bytes());
        b.extend_from_slice(&self.nonce.to_le_bytes());
        b
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = self.signing_bytes()[11..].to_vec();
        b.extend_from_slice(&self.signature);
        debug_assert_eq!(b.len(), TX_ENCODED_LEN);
        b
    }

    pub fn txid(&self) -> Hash32 {
        sha256(&[b"ZFX-txid-v0.1", &self.encode()])
    }

    pub fn from_address(&self) -> Address {
        address_of(&self.from_pubkey)
    }

    pub fn verify_signature(&self) -> Result<(), String> {
        let vk = VerifyingKey::from_bytes(&self.from_pubkey).map_err(|e| e.to_string())?;
        let sig = Signature::from_bytes(&self.signature);
        vk.verify_strict(&self.signing_bytes(), &sig).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coinbase {
    pub height: u64,
    #[serde(with = "addr_hex")]
    pub to: Address,
    /// Subsidy + fees; checked by the node against chain rules.
    pub amount: u64,
}

impl Coinbase {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(36);
        b.extend_from_slice(&self.height.to_le_bytes());
        b.extend_from_slice(&self.to);
        b.extend_from_slice(&self.amount.to_le_bytes());
        b
    }
    pub fn id(&self) -> Hash32 {
        sha256(&[b"ZFX-coinbase-v0.1", &self.encode()])
    }
}

pub mod addr_hex {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 20], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::address_to_string(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 20], D::Error> {
        let s = String::deserialize(d)?;
        super::address_from_string(&s).map_err(serde::de::Error::custom)
    }
}

pub mod sig_hex {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(&s).map_err(serde::de::Error::custom)?;
        v.try_into().map_err(|_| serde::de::Error::custom("expected 64 bytes"))
    }
}
