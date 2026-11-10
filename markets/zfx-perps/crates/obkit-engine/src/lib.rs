//! obkit-engine: a deterministic central limit order book with spot balances, holds, fees and
//! netted settlement batches. No I/O; the node crate feeds it commands from a journal.

pub mod book;
pub mod engine;
pub mod perp;
pub mod testkit;
pub mod types;

pub use engine::{Account, Balance, Engine, EngineConfig, Market, fee};
pub use perp::{AccountPerp, MarginSummary, PerpState, Position};
pub use types::*;
