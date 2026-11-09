//! Hash primitives used by consensus. All domain strings are ASCII and are
//! prepended verbatim (no length prefix); every consensus hash input that
//! follows a domain string has a fixed length, so there is no ambiguity.

use sha2::{Digest, Sha256};
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::Shake256;

pub type Hash32 = [u8; 32];

pub fn sha256(parts: &[&[u8]]) -> Hash32 {
    let mut h = Sha256::new();
    for p in parts {
        sha2::Digest::update(&mut h, p);
    }
    h.finalize().into()
}

/// SHAKE256 over the concatenation of `parts`, squeezed into `out`.
pub fn shake256_into(parts: &[&[u8]], out: &mut [u8]) {
    let mut x = Shake256::default();
    for p in parts {
        x.update(p);
    }
    x.finalize_xof().read(out);
}

pub fn shake256(parts: &[&[u8]], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    shake256_into(parts, &mut out);
    out
}

pub fn to_hex(b: &[u8]) -> String {
    hex::encode(b)
}

pub fn from_hex32(s: &str) -> Option<Hash32> {
    let v = hex::decode(s).ok()?;
    v.try_into().ok()
}
