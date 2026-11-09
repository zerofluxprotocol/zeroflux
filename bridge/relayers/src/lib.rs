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

/// Shared keep-alive HTTP agent: connection reuse keeps the many small RPC
/// calls from exhausting ephemeral ports (TIME_WAIT) under load.
pub fn agent() -> &'static ureq::Agent {
    static A: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    A.get_or_init(|| ureq::AgentBuilder::new().max_idle_connections(64).max_idle_connections_per_host(32).build())
}
