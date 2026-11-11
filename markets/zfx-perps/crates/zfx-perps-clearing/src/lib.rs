//! zfx-perps clearing core.
//!
//! One implementation of the clearing rules, compiled into both the NEAR clearing contract
//! (authoritative) and the operator's engine (which uses it to predict the chain and to admit
//! orders). Because both run the same functions over the same op sequence, the engine's view of
//! positions and collateral equals the chain's once a batch lands.
//!
//! - [`wire`]: messages users sign (orders, cancels, nonce floors, withdrawals, margin moves).
//! - [`print`]: the `OraclePrint` v1 codec of the ZFX daily model index.
//! - [`model`]: markets, accounts, isolated positions, keys, order records.
//! - [`ledger`]: fill verification, margin, liquidation and the loss waterfall, oracle, funding.
//! - [`batch`]: settlement batches and their in-order application.
//! - [`store`]: the storage trait, an in-memory store and native crypto.

pub mod batch;
pub mod ledger;
pub mod model;
pub mod plan;
pub mod print;
pub mod store;
pub mod wire;

pub use batch::{Applied, Batch, Op, OpAt, apply_batch, apply_op};
pub use ledger::{CEvent, Failure, Now, Orders, Party, Reject};
pub use model::*;
pub use print::{DataStatus, Finality, OraclePrint, OracleStatus};
pub use store::{Crypto, MemStore, Store};
#[cfg(feature = "native")]
pub use store::NativeCrypto;
pub use wire::{OrderType, Side, SignedOrder, TimeInForce};

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

#[cfg(test)]
mod tests;
