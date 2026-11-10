//! `OraclePrint` v1 codec (spec 5.2), the ZFX daily model index adapter's wire format.
//!
//! This module is deliberately isolated: it mirrors the layout frozen by the index workstream in
//! `markets/index/crates/zfx-oracle-print` (`ZFX-oracle-print-v1`), and is checked against that
//! crate's golden vectors in `tests/oracle_print_vectors.rs` once they exist. Nothing else in the
//! clearing core depends on the byte layout, only on [`OraclePrint`]'s fields.
//!
//! Layout (integers little-endian, strings u32-LE length + UTF-8, max 256 bytes):
//!
//! ```text
//!  DOMAIN b"ZFX-oracle-print-v1" | index_id str | version str | value_scale u8 | value u64
//!  source_start_ms i64 | source_end_ms i64 | observed_at_ms i64 | published_at_ms i64
//!  valid_until_ms i64 | source_hash [32] | formula_hash [32] | status u8 | data_status u8
//!  finality u8 | revision u32 | prev_print_hash [32]
//! ```
//!
//! Reporters sign the canonical bytes (ed25519, no pre-hash). `print_hash = sha256(bytes)`.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

pub const DOMAIN: &[u8] = b"ZFX-oracle-print-v1";
pub const MAX_STR_LEN: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[borsh(use_discriminant = true)]
pub enum OracleStatus {
    Fresh = 0,
    Stale = 1,
    Disputed = 2,
    Unavailable = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[borsh(use_discriminant = true)]
pub enum DataStatus {
    Ok = 0,
    /// The model was not in the source's reported set for the window. Not zero demand.
    NotReported = 1,
    Unpriced = 2,
    NoValidClosePrice = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[borsh(use_discriminant = true)]
pub enum Finality {
    Provisional = 0,
    Final = 1,
    Corrected = 2,
}

impl OracleStatus {
    fn from_u8(b: u8) -> Option<Self> {
        [Self::Fresh, Self::Stale, Self::Disputed, Self::Unavailable].get(b as usize).copied()
    }
    pub fn name(self) -> &'static str {
        ["FRESH", "STALE", "DISPUTED", "UNAVAILABLE"][self as usize]
    }
}
impl DataStatus {
    fn from_u8(b: u8) -> Option<Self> {
        [Self::Ok, Self::NotReported, Self::Unpriced, Self::NoValidClosePrice].get(b as usize).copied()
    }
    pub fn name(self) -> &'static str {
        ["OK", "NOT_REPORTED", "UNPRICED", "NO_VALID_CLOSE_PRICE"][self as usize]
    }
}
impl Finality {
    fn from_u8(b: u8) -> Option<Self> {
        [Self::Provisional, Self::Final, Self::Corrected].get(b as usize).copied()
    }
    pub fn name(self) -> &'static str {
        ["PROVISIONAL", "FINAL", "CORRECTED"][self as usize]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
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
pub enum CodecError {
    BadDomain,
    Truncated,
    StringTooLong,
    BadUtf8,
    BadEnum,
    TrailingBytes,
}

fn put_str(o: &mut Vec<u8>, s: &str) -> Result<(), CodecError> {
    if s.len() > MAX_STR_LEN {
        return Err(CodecError::StringTooLong);
    }
    o.extend_from_slice(&(s.len() as u32).to_le_bytes());
    o.extend_from_slice(s.as_bytes());
    Ok(())
}

impl OraclePrint {
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
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
        o.push(self.status as u8);
        o.push(self.data_status as u8);
        o.push(self.finality as u8);
        o.extend_from_slice(&self.revision.to_le_bytes());
        o.extend_from_slice(&self.prev_print_hash);
        Ok(o)
    }

    /// Strict: wrong domain, unknown enum values, oversize strings and trailing bytes are errors.
    pub fn decode(b: &[u8]) -> Result<Self, CodecError> {
        let mut r = Reader { b, i: 0 };
        if r.take(DOMAIN.len())? != DOMAIN {
            return Err(CodecError::BadDomain);
        }
        let p = OraclePrint {
            index_id: r.str()?,
            version: r.str()?,
            value_scale: r.u8()?,
            value: u64::from_le_bytes(r.arr()?),
            source_start_ms: i64::from_le_bytes(r.arr()?),
            source_end_ms: i64::from_le_bytes(r.arr()?),
            observed_at_ms: i64::from_le_bytes(r.arr()?),
            published_at_ms: i64::from_le_bytes(r.arr()?),
            valid_until_ms: i64::from_le_bytes(r.arr()?),
            source_hash: r.arr()?,
            formula_hash: r.arr()?,
            status: OracleStatus::from_u8(r.u8()?).ok_or(CodecError::BadEnum)?,
            data_status: DataStatus::from_u8(r.u8()?).ok_or(CodecError::BadEnum)?,
            finality: Finality::from_u8(r.u8()?).ok_or(CodecError::BadEnum)?,
            revision: u32::from_le_bytes(r.arr()?),
            prev_print_hash: r.arr()?,
        };
        if r.i != b.len() {
            return Err(CodecError::TrailingBytes);
        }
        Ok(p)
    }

    /// The print carries a usable index value.
    pub fn usable(&self) -> bool {
        self.status == OracleStatus::Fresh && self.data_status == DataStatus::Ok && self.value > 0
    }
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.i.checked_add(n).ok_or(CodecError::Truncated)?;
        let s = self.b.get(self.i..end).ok_or(CodecError::Truncated)?;
        self.i = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn str(&mut self) -> Result<String, CodecError> {
        let n = u32::from_le_bytes(self.arr()?) as usize;
        if n > MAX_STR_LEN {
            return Err(CodecError::StringTooLong);
        }
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| CodecError::BadUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sample() -> OraclePrint {
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
    fn roundtrip_and_length() {
        let p = sample();
        let b = p.encode().unwrap();
        // Same length formula as the index workstream's codec test.
        assert_eq!(b.len(), 19 + 4 + 37 + 4 + 25 + 1 + 8 + 40 + 64 + 3 + 4 + 32);
        assert_eq!(OraclePrint::decode(&b).unwrap(), p);
    }

    #[test]
    fn strict_decoding() {
        let b = sample().encode().unwrap();
        let mut t = b.clone();
        t.push(0);
        assert_eq!(OraclePrint::decode(&t), Err(CodecError::TrailingBytes));
        let mut d = b.clone();
        d[0] = b'X';
        assert_eq!(OraclePrint::decode(&d), Err(CodecError::BadDomain));
        let mut e = b.clone();
        let n = e.len();
        e[n - 37] = 9; // finality
        assert_eq!(OraclePrint::decode(&e), Err(CodecError::BadEnum));
        assert_eq!(OraclePrint::decode(&b[..10]), Err(CodecError::Truncated));
    }
}
