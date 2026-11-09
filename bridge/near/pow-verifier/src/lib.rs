//! On-chain ZeroFlux PoW verification prototype (research, not deployed).
//! The modules below are the consensus sources of crates/zfx-verify, included
//! verbatim with #[path], so on-chain results equal the reference by
//! construction; tests check them against the published vectors.
#![allow(dead_code, clippy::needless_range_loop, clippy::manual_is_multiple_of)]
#[path = "../../../../crates/zfx-verify/src/hash.rs"]
pub mod hash;
#[path = "../../../../crates/zfx-verify/src/header.rs"]
pub mod header;
#[path = "../../../../crates/zfx-verify/src/pow.rs"]
pub mod pow;
#[path = "../../../../crates/zfx-verify/src/u256.rs"]
pub mod u256;

use near_sdk::{env, near, PanicOnDefault};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {}

#[near]
impl Contract {
    #[init]
    pub fn new() -> Self {
        Self {}
    }

    /// Generate the epoch weights and evaluate one attempt; returns the
    /// digest (hex) and the gas used inside the call.
    pub fn verify_attempt(&self, profile_id: u32, epoch_seed: String, challenge: String, nonce: String) -> near_sdk::serde_json::Value {
        let p = pow::profile_by_id(profile_id).unwrap_or_else(|| env::panic_str("unknown profile"));
        let seed: [u8; 32] = hex::decode(&epoch_seed).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| env::panic_str("bad seed"));
        let ch: [u8; 32] = hex::decode(&challenge).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| env::panic_str("bad challenge"));
        let n: u64 = nonce.parse().unwrap_or_else(|_| env::panic_str("bad nonce"));
        let g0 = env::used_gas();
        let w = pow::Weights::generate(&p, &seed);
        let g1 = env::used_gas();
        let d = pow::attempt(&w, &ch, n);
        let g2 = env::used_gas();
        near_sdk::serde_json::json!({"digest": hex::encode(d), "gas_weights": g1.as_gas() - g0.as_gas(), "gas_attempt": g2.as_gas() - g1.as_gas(),
            "macs": p.macs_per_attempt()})
    }
}
