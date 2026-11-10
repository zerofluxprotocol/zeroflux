//! ZeroFlux proof-of-work verification for NEAR contracts.
//!
//! The modules below are the consensus sources of crates/zfx-verify, included
//! verbatim, so a contract computes exactly what every node and miner does.
//! `weights` stores each epoch's weights in contract storage: generating them
//! inside a call costs ~83 Mgas per weight, reading them back ~6 Mgas, so a
//! one-time preparation per epoch makes each attempt check fit one transaction.
#![allow(dead_code, clippy::needless_range_loop, clippy::manual_is_multiple_of)]
#[path = "../../../../crates/zfx-verify/src/difficulty.rs"]
pub mod difficulty;
#[path = "../../../../crates/zfx-verify/src/hash.rs"]
pub mod hash;
#[path = "../../../../crates/zfx-verify/src/header.rs"]
pub mod header;
#[path = "../../../../crates/zfx-verify/src/pow.rs"]
pub mod pow;
#[path = "../../../../crates/zfx-verify/src/u256.rs"]
pub mod u256;

pub mod weights {
    use crate::pow::{weight_row_prf, Profile, Weights};
    use near_sdk::env;

    fn key(seed: &[u8; 32], layer: usize) -> Vec<u8> {
        let mut k = b"zfxw".to_vec();
        k.extend_from_slice(seed);
        k.push(layer as u8);
        k
    }

    pub fn has_layer(seed: &[u8; 32], layer: usize) -> bool {
        env::storage_has_key(&key(seed, layer))
    }

    pub fn missing_layers(p: &Profile, seed: &[u8; 32]) -> Vec<u8> {
        (0..p.layers().len()).filter(|l| !has_layer(seed, *l)).map(|l| l as u8).collect()
    }

    /// Generate and store one layer; false if it was already stored.
    pub fn prepare_layer(p: &Profile, seed: &[u8; 32], layer: usize) -> bool {
        let layers = p.layers();
        assert!(layer < layers.len(), "no such layer");
        if has_layer(seed, layer) {
            return false;
        }
        let (k, n, _) = layers[layer];
        let mut w = vec![0i8; k * n];
        for (j, row) in w.chunks_mut(k).enumerate() {
            weight_row_prf(p.prf, seed, layer as u32, j as u32, row);
        }
        let bytes: Vec<u8> = w.into_iter().map(|v| v as u8).collect();
        env::storage_write(&key(seed, layer), &bytes);
        true
    }

    /// All layers of an epoch, or None if any layer is not prepared yet.
    pub fn load(p: &Profile, seed: &[u8; 32]) -> Option<Weights> {
        let mut layers = Vec::new();
        for l in 0..p.layers().len() {
            let b = env::storage_read(&key(seed, l))?;
            // i8 and u8 share size and alignment: reinterpret without copying.
            let mut b = std::mem::ManuallyDrop::new(b);
            layers.push(unsafe { Vec::from_raw_parts(b.as_mut_ptr() as *mut i8, b.len(), b.capacity()) });
        }
        Some(Weights { profile: *p, seed: *seed, layers })
    }

    pub fn remove(p: &Profile, seed: &[u8; 32]) {
        for l in 0..p.layers().len() {
            env::storage_remove(&key(seed, l));
        }
    }
}

pub fn parse32(s: &str) -> [u8; 32] {
    hex::decode(s).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| near_sdk::env::panic_str("expected 32-byte hex"))
}
