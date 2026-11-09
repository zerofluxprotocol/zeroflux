//! ZeroFlux <-> NEAR bridge services (v1, 2-of-3 custodial committee).
//! See docs/BRIDGE_TRUST_MODEL.md and docs/CONTRACT_API.md.

pub mod config;
pub mod coordinator;
pub mod digest;
pub mod jobs;
pub mod keys;
pub mod near;
pub mod reconcile;
pub mod relayer;
pub mod zfx;
