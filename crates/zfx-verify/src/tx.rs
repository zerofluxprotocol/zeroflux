//! Transactions (CONSENSUS.md section 5).
//!
//! Signatures: SLH-DSA (FIPS 205, stateless hash-based), parameter sets
//! SLH-DSA-SHA2-128s (scheme 1, default) and SLH-DSA-SHA2-192s (scheme 2).
//! No elliptic-curve cryptography exists anywhere in consensus.
//!
//! Addresses: P2MR-style (pay-to-Merkle-root, script-only). An address is a
//! 32-byte hash of the Merkle root of the owner's spend leaves; each leaf is
//! (scheme, public key). There is no key-path spend: public keys stay hidden
//! until a leaf is used, and spending reveals only that leaf plus its
//! Merkle path. Multiple leaves give backup keys or different parameter
//! sets under one address. 32-byte addresses keep collision
//! resistance high.
//!
//! Identifiers: `txid` hashes the transaction core (no witness), so the
//! signer's hedged-signature randomness cannot malleate it. The block
//! tx_root commits to `wtxid`, which includes the witness.

use crate::hash::{sha256, Hash32};
use fips205::traits::{SerDes, Signer, Verifier};
use fips205::{slh_dsa_sha2_128s as s128, slh_dsa_sha2_192s as s192};
use serde::{Deserialize, Serialize};

pub type Address = [u8; 32];

pub const SCHEME_SLH_DSA_SHA2_128S: u8 = 1;
pub const SCHEME_SLH_DSA_SHA2_192S: u8 = 2;
/// Reserved: XMSS (SP 800-208). Rejected in v0.1 because stateful
/// signatures need wallet index management that is not built yet.
pub const SCHEME_XMSS_RESERVED: u8 = 3;

pub const MAX_P2MR_DEPTH: usize = 8;
pub const SIG_CTX: &[u8] = b"ZFX-v0.1";

pub fn scheme_name(s: u8) -> &'static str {
    match s {
        SCHEME_SLH_DSA_SHA2_128S => "SLH-DSA-SHA2-128s",
        SCHEME_SLH_DSA_SHA2_192S => "SLH-DSA-SHA2-192s",
        SCHEME_XMSS_RESERVED => "XMSS (reserved, inactive)",
        _ => "unknown",
    }
}

pub fn scheme_lens(s: u8) -> Option<(usize, usize)> {
    match s {
        SCHEME_SLH_DSA_SHA2_128S => Some((s128::PK_LEN, s128::SIG_LEN)),
        SCHEME_SLH_DSA_SHA2_192S => Some((s192::PK_LEN, s192::SIG_LEN)),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendLeaf {
    pub scheme: u8,
    #[serde(with = "hexvec")]
    pub pubkey: Vec<u8>,
}

impl SpendLeaf {
    pub fn hash(&self) -> Hash32 {
        sha256(&[b"ZFX-p2mr-leaf-v0.1", &[self.scheme], &(self.pubkey.len() as u16).to_le_bytes(), &self.pubkey])
    }
}

fn p2mr_node(a: &Hash32, b: &Hash32) -> Hash32 {
    // Sorted pair hashing: path needs no direction bits.
    let (l, r) = if a <= b { (a, b) } else { (b, a) };
    sha256(&[b"ZFX-p2mr-node-v0.1", l, r])
}

/// Merkle root over leaf hashes (odd node promoted).
pub fn p2mr_root(leaves: &[Hash32]) -> Hash32 {
    assert!(!leaves.is_empty());
    let mut lvl = leaves.to_vec();
    while lvl.len() > 1 {
        lvl = lvl.chunks(2).map(|c| if c.len() == 2 { p2mr_node(&c[0], &c[1]) } else { c[0] }).collect();
    }
    lvl[0]
}

pub fn p2mr_path(leaves: &[Hash32], mut idx: usize) -> Vec<Hash32> {
    let mut lvl = leaves.to_vec();
    let mut path = Vec::new();
    while lvl.len() > 1 {
        let sib = idx ^ 1;
        if sib < lvl.len() {
            path.push(lvl[sib]);
        }
        lvl = lvl.chunks(2).map(|c| if c.len() == 2 { p2mr_node(&c[0], &c[1]) } else { c[0] }).collect();
        idx /= 2;
    }
    path
}

pub fn address_from_root(root: &Hash32) -> Address {
    sha256(&[b"ZFX-p2mr-addr-v0.1", root])
}

pub fn address_of_leaves(leaves: &[SpendLeaf]) -> Address {
    let h: Vec<Hash32> = leaves.iter().map(|l| l.hash()).collect();
    address_from_root(&p2mr_root(&h))
}

/// Text form: "zfx1" + hex(32-byte address) + hex(4-byte checksum).
pub fn address_to_string(a: &Address) -> String {
    let c = sha256(&[b"ZFX-addrck", a]);
    format!("zfx1{}{}", hex::encode(a), hex::encode(&c[..4]))
}

pub fn address_from_string(s: &str) -> Result<Address, String> {
    let body = s.strip_prefix("zfx1").ok_or("address must start with zfx1")?;
    let raw = hex::decode(body).map_err(|e| e.to_string())?;
    if raw.len() != 36 {
        return Err("address has wrong length".into());
    }
    let a: Address = raw[..32].try_into().unwrap();
    let c = sha256(&[b"ZFX-addrck", &a]);
    if c[..4] != raw[32..] {
        return Err("address checksum mismatch".into());
    }
    Ok(a)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub leaf: SpendLeaf,
    #[serde(with = "hexvec_list")]
    pub path: Vec<Hash32>,
    #[serde(with = "hexvec")]
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub chain_id: u32,
    #[serde(with = "addr_str")]
    pub from: Address,
    #[serde(with = "addr_str")]
    pub to: Address,
    pub amount: u64,
    pub fee: u64,
    /// Sender account nonce; must equal the account's next nonce.
    pub nonce: u64,
    pub witness: Witness,
}

pub const TX_CORE_LEN: usize = 4 + 32 + 32 + 8 + 8 + 8;

impl Transaction {
    pub fn core_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(TX_CORE_LEN);
        b.extend_from_slice(&self.chain_id.to_le_bytes());
        b.extend_from_slice(&self.from);
        b.extend_from_slice(&self.to);
        b.extend_from_slice(&self.amount.to_le_bytes());
        b.extend_from_slice(&self.fee.to_le_bytes());
        b.extend_from_slice(&self.nonce.to_le_bytes());
        b
    }

    pub fn signing_message(&self) -> Vec<u8> {
        let mut m = b"ZFX-tx-v0.1".to_vec();
        m.extend_from_slice(&self.core_bytes());
        m
    }

    pub fn encode(&self) -> Vec<u8> {
        let w = &self.witness;
        let mut b = self.core_bytes();
        b.push(w.leaf.scheme);
        b.extend_from_slice(&(w.leaf.pubkey.len() as u16).to_le_bytes());
        b.extend_from_slice(&w.leaf.pubkey);
        b.push(w.path.len() as u8);
        for p in &w.path {
            b.extend_from_slice(p);
        }
        b.extend_from_slice(&(w.signature.len() as u16).to_le_bytes());
        b.extend_from_slice(&w.signature);
        b
    }

    pub fn encoded_len(&self) -> usize {
        TX_CORE_LEN + 1 + 2 + self.witness.leaf.pubkey.len() + 1 + 32 * self.witness.path.len() + 2 + self.witness.signature.len()
    }

    pub fn txid(&self) -> Hash32 {
        sha256(&[b"ZFX-txid-v0.1", &self.core_bytes()])
    }

    pub fn wtxid(&self) -> Hash32 {
        sha256(&[b"ZFX-wtxid-v0.1", &self.encode()])
    }

    /// Stateless authorization: leaf is in the sender's P2MR tree and the
    /// SLH-DSA signature verifies under that leaf's key.
    pub fn verify_authorization(&self) -> Result<(), String> {
        let w = &self.witness;
        let (pk_len, sig_len) = scheme_lens(w.leaf.scheme).ok_or_else(|| format!("inactive signature scheme {}", w.leaf.scheme))?;
        if w.leaf.pubkey.len() != pk_len || w.signature.len() != sig_len {
            return Err("pubkey/signature length mismatch".into());
        }
        if w.path.len() > MAX_P2MR_DEPTH {
            return Err("P2MR path too deep".into());
        }
        let mut h = w.leaf.hash();
        for p in &w.path {
            h = p2mr_node(&h, p);
        }
        if address_from_root(&h) != self.from {
            return Err("spend leaf not committed by sender address".into());
        }
        let msg = self.signing_message();
        let ok = match w.leaf.scheme {
            SCHEME_SLH_DSA_SHA2_128S => {
                let pk = s128::PublicKey::try_from_bytes(w.leaf.pubkey.clone().try_into().unwrap()).map_err(|e| e.to_string())?;
                pk.verify(&msg, &w.signature.clone().try_into().unwrap(), SIG_CTX)
            }
            SCHEME_SLH_DSA_SHA2_192S => {
                let pk = s192::PublicKey::try_from_bytes(w.leaf.pubkey.clone().try_into().unwrap()).map_err(|e| e.to_string())?;
                pk.verify(&msg, &w.signature.clone().try_into().unwrap(), SIG_CTX)
            }
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err("SLH-DSA signature invalid".into())
        }
    }
}

/// A signing key for one P2MR leaf. Key material derives deterministically
/// from a 32-byte wallet seed and a leaf index (SHAKE256, domain separated).
pub enum LeafKey {
    S128(s128::PrivateKey, s128::PublicKey),
    S192(s192::PrivateKey, s192::PublicKey),
}

impl LeafKey {
    pub fn derive(seed: &[u8; 32], scheme: u8, index: u32) -> Result<LeafKey, String> {
        let m = crate::hash::shake256(&[b"ZFX-wallet-leaf-v0.1", seed, &[scheme], &index.to_le_bytes()], 72);
        use fips205::traits::KeyGen;
        match scheme {
            SCHEME_SLH_DSA_SHA2_128S => {
                let s = |o: usize| -> [u8; 16] { m[o..o + 16].try_into().unwrap() };
                let (pk, sk) = s128::KG::keygen_from_seed(&s(0), &s(16), &s(32));
                Ok(LeafKey::S128(sk, pk))
            }
            SCHEME_SLH_DSA_SHA2_192S => {
                let s = |o: usize| -> [u8; 24] { m[o..o + 24].try_into().unwrap() };
                let (pk, sk) = s192::KG::keygen_from_seed(&s(0), &s(24), &s(48));
                Ok(LeafKey::S192(sk, pk))
            }
            _ => Err(format!("cannot derive keys for scheme {}", scheme_name(scheme))),
        }
    }

    pub fn leaf(&self) -> SpendLeaf {
        match self {
            LeafKey::S128(_, pk) => SpendLeaf { scheme: SCHEME_SLH_DSA_SHA2_128S, pubkey: pk.clone().into_bytes().to_vec() },
            LeafKey::S192(_, pk) => SpendLeaf { scheme: SCHEME_SLH_DSA_SHA2_192S, pubkey: pk.clone().into_bytes().to_vec() },
        }
    }

    /// Hedged signing (FIPS 205 default) using the OS RNG.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        match self {
            LeafKey::S128(sk, _) => sk.try_sign(msg, SIG_CTX, true).map(|s| s.to_vec()).map_err(String::from),
            LeafKey::S192(sk, _) => sk.try_sign(msg, SIG_CTX, true).map(|s| s.to_vec()).map_err(String::from),
        }
    }
}

/// A wallet's P2MR key set: an ordered list of leaf keys under one address.
pub struct P2mrKeys {
    pub keys: Vec<LeafKey>,
}

impl P2mrKeys {
    /// Default layout: leaf 0 = SLH-DSA-SHA2-128s (daily use), leaf 1 =
    /// SLH-DSA-SHA2-192s (higher-security backup).
    pub fn from_seed(seed: &[u8; 32]) -> P2mrKeys {
        P2mrKeys {
            keys: vec![
                LeafKey::derive(seed, SCHEME_SLH_DSA_SHA2_128S, 0).unwrap(),
                LeafKey::derive(seed, SCHEME_SLH_DSA_SHA2_192S, 1).unwrap(),
            ],
        }
    }

    pub fn leaves(&self) -> Vec<SpendLeaf> {
        self.keys.iter().map(|k| k.leaf()).collect()
    }

    pub fn address(&self) -> Address {
        address_of_leaves(&self.leaves())
    }

    /// Build and sign a transaction spending via leaf `leaf_idx`.
    pub fn sign_tx(&self, chain_id: u32, to: Address, amount: u64, fee: u64, nonce: u64, leaf_idx: usize) -> Result<Transaction, String> {
        let leaves = self.leaves();
        let hashes: Vec<Hash32> = leaves.iter().map(|l| l.hash()).collect();
        let mut tx = Transaction {
            chain_id,
            from: self.address(),
            to,
            amount,
            fee,
            nonce,
            witness: Witness { leaf: leaves[leaf_idx].clone(), path: p2mr_path(&hashes, leaf_idx), signature: vec![] },
        };
        tx.witness.signature = self.keys[leaf_idx].sign(&tx.signing_message())?;
        Ok(tx)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coinbase {
    pub height: u64,
    #[serde(with = "addr_str")]
    pub to: Address,
    /// Subsidy + fees; checked by the node against chain rules.
    pub amount: u64,
}

impl Coinbase {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(48);
        b.extend_from_slice(&self.height.to_le_bytes());
        b.extend_from_slice(&self.to);
        b.extend_from_slice(&self.amount.to_le_bytes());
        b
    }
    pub fn id(&self) -> Hash32 {
        sha256(&[b"ZFX-coinbase-v0.1", &self.encode()])
    }
}

pub mod addr_str {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::address_to_string(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        super::address_from_string(&s).map_err(serde::de::Error::custom)
    }
}

pub mod hexvec {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() > 2 * 40_000 {
            return Err(serde::de::Error::custom("hex field too large"));
        }
        hex::decode(&s).map_err(serde::de::Error::custom)
    }
}

pub mod hexvec_list {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[[u8; 32]], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(v.iter().map(hex::encode))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<[u8; 32]>, D::Error> {
        let v: Vec<String> = Vec::deserialize(d)?;
        if v.len() > super::MAX_P2MR_DEPTH {
            return Err(serde::de::Error::custom("path too long"));
        }
        v.into_iter()
            .map(|s| hex::decode(&s).map_err(serde::de::Error::custom)?.try_into().map_err(|_| serde::de::Error::custom("expected 32 bytes")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p2mr_sign_verify_both_leaves() {
        let k = P2mrKeys::from_seed(&[7u8; 32]);
        let to = [9u8; 32];
        for leaf in 0..2 {
            let tx = k.sign_tx(1, to, 100, 1, 0, leaf).unwrap();
            tx.verify_authorization().unwrap();
            let mut bad = tx.clone();
            bad.amount = 101;
            assert!(bad.verify_authorization().is_err(), "tampered amount must fail");
            let mut bad2 = tx.clone();
            bad2.from = [1u8; 32];
            assert!(bad2.verify_authorization().is_err(), "leaf not in sender tree");
            // txid stable under witness change, wtxid not
            let mut w = tx.clone();
            w.witness.signature[0] ^= 1;
            assert_eq!(w.txid(), tx.txid());
            assert_ne!(w.wtxid(), tx.wtxid());
            assert!(w.verify_authorization().is_err());
            assert_eq!(tx.encode().len(), tx.encoded_len());
        }
    }

    #[test]
    fn deterministic_keys_and_addresses() {
        let a = P2mrKeys::from_seed(&[1u8; 32]).address();
        assert_eq!(a, P2mrKeys::from_seed(&[1u8; 32]).address());
        assert_ne!(a, P2mrKeys::from_seed(&[2u8; 32]).address());
        let s = address_to_string(&a);
        assert_eq!(address_from_string(&s).unwrap(), a);
        let mut t = s.clone().into_bytes();
        t[10] = if t[10] == b'0' { b'1' } else { b'0' };
        assert!(address_from_string(std::str::from_utf8(&t).unwrap()).is_err());
    }

    #[test]
    fn xmss_reserved_is_rejected() {
        assert!(LeafKey::derive(&[0; 32], SCHEME_XMSS_RESERVED, 0).is_err());
        assert!(scheme_lens(SCHEME_XMSS_RESERVED).is_none());
    }
}
