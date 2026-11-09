//! Genesis / chain configuration. The genesis block commits to the
//! canonical encoding of this config, so peers with different configs
//! have different genesis hashes and refuse each other.

use serde::{Deserialize, Serialize};
use zfx_verify::bridge::BridgeParams;
use zfx_verify::hash::{sha256, Hash32};
use zfx_verify::state::ConsensusParams;
use zfx_verify::tx::{address_from_string, Address};
use zfx_verify::U256;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Allocation {
    pub address: String,
    pub amount: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainConfig {
    pub chain_id: u32,
    /// PoW profile id (header version). 1 = zfx-fp8c-v0.1, 2 = dev.
    pub profile: u32,
    pub genesis_timestamp: u64,
    /// Hex uint256 big-endian.
    pub genesis_target: String,
    pub pow_limit: String,
    pub target_spacing_secs: u64,
    pub fixed_difficulty: bool,
    /// Base units per block (testnet; 1 ZFX = 1e8 units). No monetary value.
    pub block_subsidy: u64,
    pub max_future_secs: u64,
    #[serde(default)]
    pub allocations: Vec<Allocation>,
    /// Consensus rule-set version committed at genesis (2 = header v2 with
    /// state_root, coinbase maturity, memos, bridge module).
    #[serde(default = "default_consensus_version")]
    pub consensus_version: u32,
    /// Blocks before a coinbase reward becomes spendable.
    #[serde(default)]
    pub coinbase_maturity: u64,
    /// Native bridge parameters; None disables the bridge module.
    #[serde(default)]
    pub bridge: Option<BridgeParams>,
    /// Testnet faucet amount per request (0 disables; node-local policy
    /// is in NodeOptions, this only documents the testnet default).
    #[serde(default)]
    pub label: String,
    /// Monetary policy. None = Constant { per_block: block_subsidy } (testnet).
    #[serde(default)]
    pub subsidy_schedule: Option<zfx_verify::monetary::SubsidySchedule>,
    /// "testnet" (default) or "mainnet". Mainnet activation is gated.
    #[serde(default = "default_network")]
    pub network: String,
}

fn default_network() -> String {
    "testnet".into()
}

fn default_consensus_version() -> u32 {
    2
}

impl ChainConfig {
    pub fn devnet() -> Self {
        ChainConfig {
            chain_id: 0x5a46_0002,
            profile: 2,
            genesis_timestamp: 1_780_000_000,
            genesis_target: U256::MAX.shr(10).to_hex(),
            pow_limit: U256::MAX.shr(2).to_hex(),
            target_spacing_secs: 5,
            fixed_difficulty: false,
            block_subsidy: 50_0000_0000,
            max_future_secs: 120,
            allocations: vec![],
            consensus_version: 2,
            coinbase_maturity: 5,
            bridge: None,
            label: "zeroflux-devnet".into(),
            subsidy_schedule: None,
            network: "testnet".into(),
        }
    }

    pub fn testnet_v01() -> Self {
        ChainConfig {
            chain_id: 0x5a46_0001,
            profile: 1,
            genesis_target: U256::MAX.shr(12).to_hex(),
            target_spacing_secs: 30,
            coinbase_maturity: 20,
            label: "zeroflux-testnet-v0.2".into(),
            ..Self::devnet()
        }
    }

    pub fn params(&self) -> ConsensusParams {
        ConsensusParams { chain_id: self.chain_id, subsidy: self.schedule(), coinbase_maturity: self.coinbase_maturity, bridge: self.bridge.clone() }
    }

    pub fn schedule(&self) -> zfx_verify::monetary::SubsidySchedule {
        self.subsidy_schedule.clone().unwrap_or(zfx_verify::monetary::SubsidySchedule::Constant { per_block: self.block_subsidy })
    }

    pub fn genesis_target_u256(&self) -> U256 {
        U256::from_hex(&self.genesis_target).expect("genesis_target hex")
    }

    pub fn pow_limit_u256(&self) -> U256 {
        U256::from_hex(&self.pow_limit).expect("pow_limit hex")
    }

    pub fn allocations_parsed(&self) -> Result<Vec<(Address, u64)>, String> {
        self.allocations.iter().map(|a| Ok((address_from_string(&a.address)?, a.amount))).collect()
    }

    /// Fixed-layout encoding (not JSON) for the genesis commitment.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut b = b"ZFX-genesis-config-v0.1".to_vec();
        b.extend_from_slice(&self.chain_id.to_le_bytes());
        b.extend_from_slice(&self.profile.to_le_bytes());
        b.extend_from_slice(&self.genesis_timestamp.to_le_bytes());
        b.extend_from_slice(&self.genesis_target_u256().to_be_bytes());
        b.extend_from_slice(&self.pow_limit_u256().to_be_bytes());
        b.extend_from_slice(&self.target_spacing_secs.to_le_bytes());
        b.push(self.fixed_difficulty as u8);
        b.extend_from_slice(&self.block_subsidy.to_le_bytes());
        b.extend_from_slice(&self.max_future_secs.to_le_bytes());
        b.extend_from_slice(&self.consensus_version.to_le_bytes());
        b.extend_from_slice(&self.coinbase_maturity.to_le_bytes());
        match &self.bridge {
            None => b.push(0),
            Some(bp) => {
                b.push(1);
                b.extend_from_slice(&bp.canonical_bytes());
            }
        }
        if let Some(sc) = &self.subsidy_schedule {
            b.extend_from_slice(b"|subsidy");
            b.extend_from_slice(&sc.canonical_bytes());
        }
        if self.network != "testnet" {
            b.extend_from_slice(b"|network");
            b.extend_from_slice(self.network.as_bytes());
        }
        let al = self.allocations_parsed().expect("allocation addresses");
        b.extend_from_slice(&(al.len() as u32).to_le_bytes());
        for (a, v) in al {
            b.extend_from_slice(&a);
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    pub fn commitment(&self) -> Hash32 {
        sha256(&[&self.canonical_bytes()])
    }
}
