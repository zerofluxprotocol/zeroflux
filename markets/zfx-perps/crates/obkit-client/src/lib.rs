//! obkit-client: talk to an obkit node from Rust.
//!
//! ```no_run
//! # async fn demo() -> Result<(), obkit_client::Error> {
//! use obkit_client::{Client, Signer, Order};
//! let signer = Signer::from_near_secret("ed25519:...")?;
//! let c = Client::new("http://127.0.0.1:8794", "alice.near", signer);
//! let ack = c.place(&Order::limit("NEAR-USDC", "buy", 42_500, 10)).await?;
//! # Ok(()) }
//! ```

mod book;
mod client;
mod signer;

pub use book::{BookFeed, BookState, Feed, Gap};
pub use client::*;
pub use signer::Signer;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{status} {code}: {message}")]
    Api { status: u16, code: String, message: String },
    #[error("key: {0}")]
    Key(String),
    #[error("websocket: {0}")]
    Ws(String),
    #[error("decode: {0}")]
    Decode(String),
}
