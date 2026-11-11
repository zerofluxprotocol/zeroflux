//! Canonical `OraclePrint` v1: byte encoding, hash and ed25519 signature check.
//!
//! This crate is the frozen wire format shared by the index publisher, the perps
//! engine and the NEAR contract. It is `no_std` + `alloc` so it compiles for
//! wasm32. The format is documented in `markets/index/docs/ORACLE_PRINT_V1.md`
//! and pinned by golden vectors in `markets/index/vectors/oracle_print_v1.json`.
//!
//! Layout (all integers little-endian, strings are u32-LE length + UTF-8 bytes):
//!
//! ```text
//!  DOMAIN            19 bytes  b"ZFX-oracle-print-v1" (no length prefix)
//!  index_id          str       e.g. "or-model-spend/anthropic/claude-opus-5.5-20260921"
//!  version           str       formula version, e.g. "ZFX-OR-MODEL-SPEND-USD-v1"
//!  value_scale       u8        decimal places of `value` (6 = micro-USD)
//!  value             u64       fixed-point value; 0 when data_status != OK
//!  source_start_ms   i64       inclusive start of the source window, unix ms UTC
//!  source_end_ms     i64       exclusive end of the source window, unix ms UTC
//!  observed_at_ms    i64       when the publisher fetched the deciding input
//!  published_at_ms   i64       when this print was created
//!  valid_until_ms    i64       consumers must treat the print as stale after this
//!  source_hash       [u8;32]   SHA-256 commitment to the raw input blobs
//!  formula_hash      [u8;32]   SHA-256 of the formula specification
//!  status            u8        OracleStatus
//!  data_status       u8        DataStatus
//!  finality          u8        Finality
//!  revision          u32       1 for the first print of a (index_id, window)
//!  prev_print_hash   [u8;32]   hash of the print this one supersedes, or zeros
//! ```
//!
//! `print_hash = SHA-256(canonical bytes)`. The reporter signs the canonical
//! bytes themselves (ed25519, RFC 8032, no pre-hash).
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use sha2::{Digest, Sha256};

/// Domain tag prepended to every canonical encoding.
pub const DOMAIN: &[u8] = b"ZFX-oracle-print-v1";
/// Maximum accepted length for string fields, in bytes.
pub const MAX_STR_LEN: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OracleStatus {
    Fresh = 0,
    Stale = 1,
    Disputed = 2,
    Unavailable = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DataStatus {
    /// Value computed from reported tokens and a valid price.
    Ok = 0,
    /// The source did not report the model for this window. Not zero demand.
    NotReported = 1,
    /// Tokens reported but no valid price or mix. Not zero spend.
    Unpriced = 2,
    /// Price-based close required but no valid close observation exists.
    NoValidClosePrice = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Finality {
    /// Reserved for an authorized live current-day feed. Not used by v1 sources.
    Provisional = 0,
    Final = 1,
    Corrected = 2,
}

macro_rules! enum_from_u8 {
    ($t:ty, $($n:literal => $v:expr),*) => {
        impl $t {
            pub fn from_u8(b: u8) -> Option<Self> {
                match b { $($n => Some($v),)* _ => None }
            }
            pub fn as_u8(self) -> u8 { self as u8 }
        }
    };
}
enum_from_u8!(OracleStatus, 0 => OracleStatus::Fresh, 1 => OracleStatus::Stale, 2 => OracleStatus::Disputed, 3 => OracleStatus::Unavailable);
enum_from_u8!(DataStatus, 0 => DataStatus::Ok, 1 => DataStatus::NotReported, 2 => DataStatus::Unpriced, 3 => DataStatus::NoValidClosePrice);
enum_from_u8!(Finality, 0 => Finality::Provisional, 1 => Finality::Final, 2 => Finality::Corrected);

impl OracleStatus {
    pub fn name(self) -> &'static str {
        match self {
            OracleStatus::Fresh => "FRESH",
            OracleStatus::Stale => "STALE",
            OracleStatus::Disputed => "DISPUTED",
            OracleStatus::Unavailable => "UNAVAILABLE",
        }
    }
}
impl DataStatus {
    pub fn name(self) -> &'static str {
        match self {
            DataStatus::Ok => "OK",
            DataStatus::NotReported => "NOT_REPORTED",
            DataStatus::Unpriced => "UNPRICED",
            DataStatus::NoValidClosePrice => "NO_VALID_CLOSE_PRICE",
        }
    }
}
impl Finality {
    pub fn name(self) -> &'static str {
        match self {
            Finality::Provisional => "PROVISIONAL",
            Finality::Final => "FINAL",
            Finality::Corrected => "CORRECTED",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OraclePrint {
    pub index_id: String,
    pub version: String,
    pub value_scale: u8,
    pub value: u64,
    pub source_start_ms: i64,
    pub source_end_ms: i64,
    pub observed_at_ms: i64,
    pub published_at_ms: i64,
    pub valid_until_ms: i64,
    pub source_hash: [u8; 32],
    pub formula_hash: [u8; 32],
    pub status: OracleStatus,
    pub data_status: DataStatus,
    pub finality: Finality,
    pub revision: u32,
    pub prev_print_hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    BadDomain,
    Truncated,
    StringTooLong,
    BadUtf8,
    BadEnum,
    TrailingBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    StringTooLong,
}

fn put_str(out: &mut Vec<u8>, s: &str) -> Result<(), EncodeError> {
    if s.len() > MAX_STR_LEN {
        return Err(EncodeError::StringTooLong);
    }
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

impl OraclePrint {
    /// Canonical bytes. This is the exact message the reporter signs.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let mut o = Vec::with_capacity(256);
        o.extend_from_slice(DOMAIN);
        put_str(&mut o, &self.index_id)?;
        put_str(&mut o, &self.version)?;
        o.push(self.value_scale);
        o.extend_from_slice(&self.value.to_le_bytes());
        for t in [
            self.source_start_ms,
            self.source_end_ms,
            self.observed_at_ms,
            self.published_at_ms,
            self.valid_until_ms,
        ] {
            o.extend_from_slice(&t.to_le_bytes());
        }
        o.extend_from_slice(&self.source_hash);
        o.extend_from_slice(&self.formula_hash);
        o.push(self.status.as_u8());
        o.push(self.data_status.as_u8());
        o.push(self.finality.as_u8());
        o.extend_from_slice(&self.revision.to_le_bytes());
        o.extend_from_slice(&self.prev_print_hash);
        Ok(o)
    }

    /// SHA-256 of the canonical bytes.
    pub fn hash(&self) -> Result<[u8; 32], EncodeError> {
        Ok(sha256(&self.encode()?))
    }

    /// Strict decoder: rejects wrong domain, unknown enum values and trailing bytes.
    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader { b, i: 0 };
        if r.take(DOMAIN.len())? != DOMAIN {
            return Err(DecodeError::BadDomain);
        }
        let index_id = r.str()?;
        let version = r.str()?;
        let value_scale = r.u8()?;
        let value = u64::from_le_bytes(r.arr::<8>()?);
        let source_start_ms = r.i64()?;
        let source_end_ms = r.i64()?;
        let observed_at_ms = r.i64()?;
        let published_at_ms = r.i64()?;
        let valid_until_ms = r.i64()?;
        let source_hash = r.arr::<32>()?;
        let formula_hash = r.arr::<32>()?;
        let status = OracleStatus::from_u8(r.u8()?).ok_or(DecodeError::BadEnum)?;
        let data_status = DataStatus::from_u8(r.u8()?).ok_or(DecodeError::BadEnum)?;
        let finality = Finality::from_u8(r.u8()?).ok_or(DecodeError::BadEnum)?;
        let revision = u32::from_le_bytes(r.arr::<4>()?);
        let prev_print_hash = r.arr::<32>()?;
        if r.i != b.len() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(OraclePrint {
            index_id,
            version,
            value_scale,
            value,
            source_start_ms,
            source_end_ms,
            observed_at_ms,
            published_at_ms,
            valid_until_ms,
            source_hash,
            formula_hash,
            status,
            data_status,
            finality,
            revision,
            prev_print_hash,
        })
    }
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.i.checked_add(n).ok_or(DecodeError::Truncated)?;
        if end > self.b.len() {
            return Err(DecodeError::Truncated);
        }
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.arr::<8>()?))
    }
    fn str(&mut self) -> Result<String, DecodeError> {
        let n = u32::from_le_bytes(self.arr::<4>()?) as usize;
        if n > MAX_STR_LEN {
            return Err(DecodeError::StringTooLong);
        }
        let s = self.take(n)?;
        core::str::from_utf8(s)
            .map(String::from)
            .map_err(|_| DecodeError::BadUtf8)
    }
}

pub fn sha256(b: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b);
    h.finalize().into()
}

/// Commitment to an ordered list of input blob hashes:
/// `SHA-256("ZFX-source-set-v1" || u32le(n) || h_1 || ... || h_n)`.
pub fn source_set_hash(hashes: &[[u8; 32]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"ZFX-source-set-v1");
    h.update((hashes.len() as u32).to_le_bytes());
    for x in hashes {
        h.update(x);
    }
    h.finalize().into()
}

/// Verify an ed25519 signature over the canonical bytes. Uses strict verification
/// (rejects small-order keys and non-canonical signatures).
pub fn verify(canonical: &[u8], pubkey: &[u8; 32], sig: &[u8; 64]) -> bool {
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let s = ed25519_dalek::Signature::from_bytes(sig);
    vk.verify_strict(canonical, &s).is_ok()
}

/// Sign canonical bytes with a 32-byte ed25519 seed. Returns (pubkey, signature).
pub fn sign(canonical: &[u8], seed: &[u8; 32]) -> ([u8; 32], [u8; 64]) {
    use ed25519_dalek::Signer;
    let sk = ed25519_dalek::SigningKey::from_bytes(seed);
    let sig = sk.sign(canonical);
    (sk.verifying_key().to_bytes(), sig.to_bytes())
}

/// Public key for a seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    ed25519_dalek::SigningKey::from_bytes(seed)
        .verifying_key()
        .to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OraclePrint {
        OraclePrint {
            index_id: "or-model-spend/example/model-20260101".into(),
            version: "ZFX-OR-MODEL-SPEND-USD-v1".into(),
            value_scale: 6,
            value: 42_500_000,
            source_start_ms: 1_767_225_600_000,
            source_end_ms: 1_767_312_000_000,
            observed_at_ms: 1_767_340_800_000,
            published_at_ms: 1_767_340_801_000,
            valid_until_ms: 1_767_484_800_000,
            source_hash: [0x11; 32],
            formula_hash: [0x22; 32],
            status: OracleStatus::Fresh,
            data_status: DataStatus::Ok,
            finality: Finality::Final,
            revision: 1,
            prev_print_hash: [0; 32],
        }
    }

    #[test]
    fn roundtrip() {
        let p = sample();
        let b = p.encode().unwrap();
        assert_eq!(b.len(), 19 + 4 + 37 + 4 + 25 + 1 + 8 + 40 + 64 + 3 + 4 + 32);
        assert_eq!(OraclePrint::decode(&b).unwrap(), p);
    }

    #[test]
    fn rejects_tampering() {
        let p = sample();
        let mut b = p.encode().unwrap();
        b.push(0);
        assert_eq!(OraclePrint::decode(&b), Err(DecodeError::TrailingBytes));
        b.pop();
        b[0] = b'X';
        assert_eq!(OraclePrint::decode(&b), Err(DecodeError::BadDomain));
        let mut b2 = p.encode().unwrap();
        let n = b2.len();
        b2[n - 32 - 4 - 1] = 9; // finality byte
        assert_eq!(OraclePrint::decode(&b2), Err(DecodeError::BadEnum));
        assert_eq!(OraclePrint::decode(&b2[..10]), Err(DecodeError::Truncated));
    }

    #[test]
    fn sign_verify() {
        let b = sample().encode().unwrap();
        let (pk, sig) = sign(&b, &[7u8; 32]);
        assert!(verify(&b, &pk, &sig));
        let mut b2 = b.clone();
        b2[30] ^= 1;
        assert!(!verify(&b2, &pk, &sig));
        let mut sig2 = sig;
        sig2[0] ^= 1;
        assert!(!verify(&b, &pk, &sig2));
    }

    #[test]
    fn long_string_rejected() {
        let mut p = sample();
        p.index_id = "x".repeat(MAX_STR_LEN + 1);
        assert_eq!(p.encode(), Err(EncodeError::StringTooLong));
    }
}
