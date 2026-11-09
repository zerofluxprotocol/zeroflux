//! Bridge deployment description shared by relayers, coordinator and CLI.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeEnv {
    pub native_chain_id: u32,
    pub native_asset_id: String,
    pub near_rpc: String,
    pub near_network: String,
    pub wzfx_contract: String,
    /// Native confirmations before a deposit can be attested (policy K).
    pub confirmations: u64,
    /// Native blocks a release approval stays valid.
    pub release_validity_blocks: u64,
    /// Seconds a mint attestation stays valid.
    pub mint_validity_secs: u64,
    pub relayers: Vec<RelayerEndpoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayerEndpoint {
    pub member: u8,
    pub url: String,
    pub ed25519: String,
    pub slh: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayerConfig {
    pub member: u8,
    pub seed_file: String,
    /// This relayer's *own* ZeroFlux node (independent evidence).
    pub zfx_node: String,
    pub near_rpc: String,
    pub wzfx_contract: String,
    pub native_chain_id: u32,
    pub native_asset_id: String,
    pub confirmations: u64,
    pub listen: String,
    pub data_dir: String,
    /// Optional NEAR account used to submit emergency pauses directly.
    #[serde(default)]
    pub near_account: Option<String>,
    #[serde(default)]
    pub near_key_file: Option<String>,
}

pub fn load<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T, String> {
    serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path, e))?).map_err(|e| format!("{}: {}", path, e))
}
