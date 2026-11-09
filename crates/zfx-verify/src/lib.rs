//! ZeroFlux consensus library. Everything a CPU-only node needs to decide
//! whether a block is valid lives here; no GPU runtime is linked.

pub mod block;
pub mod bridge;
pub mod difficulty;
pub mod hash;
pub mod header;
pub mod pow;
pub mod state;
pub mod tx;
pub mod u256;
pub mod vectors;

pub use block::Block;
pub use header::BlockHeader;
pub use u256::U256;
