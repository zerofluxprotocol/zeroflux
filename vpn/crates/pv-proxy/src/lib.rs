//! OpenAI-compatible proxy pipeline (Chat Completions and Responses).

pub mod approvals;
pub mod engine;
pub mod fields;
pub mod notify;
pub mod output;
pub mod protocol;
pub mod sse;

pub use engine::*;
