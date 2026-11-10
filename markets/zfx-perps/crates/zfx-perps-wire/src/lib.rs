//! Canonical messages that users sign with their ed25519 trading key.
//!
//! Every message is a `|`-separated UTF-8 string with a versioned prefix. The bytes signed are the
//! UTF-8 bytes of the string, unhashed. Fields never contain `|`: account ids, token ids and market
//! symbols are validated against `[a-z0-9._-]` / `[A-Z0-9-]` before formatting, and numbers are
//! base-10 integers. The TypeScript client (`packages/client/src/wire.ts`) builds the same strings;
//! `vectors.json` pins both implementations to the same bytes.
//!
//! The withdrawal message is also verified on-chain by the settlement contract, which is why it
//! names the contract: a signature for one deployment can't be replayed against another.

use std::fmt::Write;

pub const ORDER_V1: &str = "zfx-perps-order-v1";
pub const CANCEL_V1: &str = "zfx-perps-cancel-v1";
pub const CANCEL_ALL_V1: &str = "zfx-perps-cancel-all-v1";
pub const WITHDRAW_V1: &str = "zfx-perps-withdraw-v1";
pub const AUTH_V1: &str = "zfx-perps-auth-v1";

/// Max accepted clock skew between a signed timestamp and the gateway, in ms.
pub const MAX_SKEW_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderMsg<'a> {
    pub account: &'a str,
    pub market: &'a str,
    /// "buy" | "sell"
    pub side: &'a str,
    /// Price in ticks.
    pub price: u64,
    /// Quantity in lots.
    pub qty: u64,
    /// "gtc" | "ioc" | "fok" | "post_only"
    pub tif: &'a str,
    /// "cancel_taker" | "cancel_maker" | "cancel_both"
    pub stp: &'a str,
    pub client_id: u64,
    pub ts_ms: u64,
}

impl OrderMsg<'_> {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut s = String::with_capacity(96);
        let _ = write!(
            s,
            "{ORDER_V1}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.account, self.market, self.side, self.price, self.qty, self.tif, self.stp, self.client_id, self.ts_ms
        );
        s.into_bytes()
    }
}

/// Cancel one order. Exactly one of `order_id` / `client_id` is non-zero.
pub fn cancel_msg(account: &str, market: &str, order_id: u64, client_id: u64, ts_ms: u64) -> Vec<u8> {
    format!("{CANCEL_V1}|{account}|{market}|{order_id}|{client_id}|{ts_ms}").into_bytes()
}

/// Cancel all of an account's orders, in one market or `*` for every market.
pub fn cancel_all_msg(account: &str, market: Option<&str>, ts_ms: u64) -> Vec<u8> {
    format!("{CANCEL_ALL_V1}|{account}|{}|{ts_ms}", market.unwrap_or("*")).into_bytes()
}

/// Withdrawal authorization, verified by the gateway and again on-chain.
/// `amount` is in the token's smallest unit. `nonce` must be strictly greater than the account's
/// previous withdrawal nonce.
pub fn withdraw_msg(contract: &str, account: &str, token: &str, amount: u128, nonce: u64) -> Vec<u8> {
    format!("{WITHDRAW_V1}|{contract}|{account}|{token}|{amount}|{nonce}").into_bytes()
}

/// Authenticates private reads and private WebSocket channels.
pub fn auth_msg(account: &str, ts_ms: u64) -> Vec<u8> {
    format!("{AUTH_V1}|{account}|{ts_ms}").into_bytes()
}

/// NEAR account ids: 2-64 chars of `[a-z0-9._-]`. Also guarantees no `|` in messages.
pub fn valid_account(s: &str) -> bool {
    (2..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// Market symbols: 3-32 chars of `[A-Z0-9-]`, e.g. `NEAR-USDC`.
pub fn valid_market(s: &str) -> bool {
    (3..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_bytes_are_stable() {
        let m = OrderMsg {
            account: "alice.test.near",
            market: "NEAR-USDC",
            side: "buy",
            price: 4_250,
            qty: 12,
            tif: "gtc",
            stp: "cancel_taker",
            client_id: 7,
            ts_ms: 1_700_000_000_000,
        };
        assert_eq!(
            String::from_utf8(m.to_bytes()).unwrap(),
            "zfx-perps-order-v1|alice.test.near|NEAR-USDC|buy|4250|12|gtc|cancel_taker|7|1700000000000"
        );
    }

    #[test]
    fn withdraw_bytes_are_stable() {
        assert_eq!(
            String::from_utf8(withdraw_msg("ob.test.near", "bob.test.near", "usdc.test.near", 1_000_000, 3)).unwrap(),
            "zfx-perps-withdraw-v1|ob.test.near|bob.test.near|usdc.test.near|1000000|3"
        );
    }

    #[test]
    fn validators_reject_separators() {
        assert!(valid_account("alice.near"));
        assert!(!valid_account("ali|ce.near"));
        assert!(!valid_account("Alice.near"));
        assert!(valid_market("NEAR-USDC"));
        assert!(!valid_market("near-usdc"));
        assert!(!valid_market("A|B"));
    }
}
