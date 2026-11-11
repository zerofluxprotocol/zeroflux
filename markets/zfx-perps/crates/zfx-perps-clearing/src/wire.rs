//! Messages users sign with their registered trading key, in canonical form.
//!
//! Every message is a `|`-separated UTF-8 string that starts with a versioned domain tag and names
//! the chain id and the clearing contract, so a signature for one deployment (or one chain) is
//! useless on another. The signed bytes are the UTF-8 bytes of the string, unhashed. Fields never
//! contain `|`: account ids, market ids and chain ids are validated before formatting, enums are
//! fixed upper-case words, numbers are base-10 integers without leading zeros, hashes are lower
//! case hex. The TypeScript SDK builds the same strings; `vectors.json` pins both.
//!
//! The order hash (`sha256` of the order message) identifies an order on-chain: filled quantity,
//! cancellation and expiry are stored under it.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::fmt::Write;

pub const ORDER_V1: &str = "zfx-perps-order-v1";
pub const CANCEL_V1: &str = "zfx-perps-cancel-v1";
pub const MIN_NONCE_V1: &str = "zfx-perps-min-nonce-v1";
pub const WITHDRAW_V1: &str = "zfx-perps-withdraw-v1";
pub const MARGIN_V1: &str = "zfx-perps-margin-v1";
pub const AUTH_V1: &str = "zfx-perps-auth-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "BUY" => Some(Side::Buy),
            "SELL" => Some(Side::Sell),
            _ => None,
        }
    }
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
    /// +1 for buys, -1 for sells.
    pub fn sign(self) -> i64 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum OrderType {
    Limit,
    /// `price` is the worst acceptable price (protection bound); never rests.
    Market,
}

impl OrderType {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderType::Limit => "LIMIT",
            OrderType::Market => "MARKET",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "LIMIT" => Some(OrderType::Limit),
            "MARKET" => Some(OrderType::Market),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

impl TimeInForce {
    pub fn as_str(self) -> &'static str {
        match self {
            TimeInForce::Gtc => "GTC",
            TimeInForce::Ioc => "IOC",
            TimeInForce::Fok => "FOK",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "GTC" => Some(TimeInForce::Gtc),
            "IOC" => Some(TimeInForce::Ioc),
            "FOK" => Some(TimeInForce::Fok),
            _ => None,
        }
    }
}

/// A user's order with its trading-key signature (spec 5.2 `SignedOrder`). Prices are integer
/// ticks of the market, quantities integer lots.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct SignedOrder {
    pub chain_id: String,
    pub clearing_contract: String,
    pub account: String,
    pub market: String,
    pub side: Side,
    pub order_type: OrderType,
    /// Limit price, or for MARKET the worst acceptable price.
    pub price: u64,
    pub qty: u64,
    pub reduce_only: bool,
    pub post_only: bool,
    pub tif: TimeInForce,
    /// Must be >= the account's on-chain `min_nonce` when filled (nonce epoch).
    pub nonce: u64,
    /// Unix ns. The order can't be filled at or after this time.
    pub expiry_ns: u64,
    pub client_order_id: u64,
    /// The highest fee rate the user accepts for this order, in bps of notional.
    pub max_fee_bps: u16,
    /// The market's policy hash the user agreed to. Any parameter change invalidates the order.
    pub policy_hash: [u8; 32],
    pub public_key: [u8; 32],
    pub signature: Sig64,
}

/// A 64-byte ed25519 signature. Serializes as lower-case hex in JSON, raw bytes in borsh.
#[derive(Clone, Copy, PartialEq, Eq, Hash, BorshSerialize, BorshDeserialize)]
pub struct Sig64(pub [u8; 64]);

impl std::fmt::Debug for Sig64 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sig64({}..)", &hex(&self.0)[..16])
    }
}

impl Serialize for Sig64 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for Sig64 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <String as Deserialize>::deserialize(d)?;
        let v = unhex(&s).ok_or_else(|| serde::de::Error::custom("signature: 128 lower-case hex chars"))?;
        Ok(Sig64(v.try_into().map_err(|_| serde::de::Error::custom("signature: 64 bytes"))?))
    }
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || s.bytes().any(|c| c.is_ascii_uppercase()) {
        return None;
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect()
}

pub fn hex32(h: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in h {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut o = [0u8; 32];
    for (i, c) in s.as_bytes().chunks(2).enumerate() {
        let hi = (c[0] as char).to_digit(16)?;
        let lo = (c[1] as char).to_digit(16)?;
        if c[0].is_ascii_uppercase() || c[1].is_ascii_uppercase() {
            return None;
        }
        o[i] = (hi * 16 + lo) as u8;
    }
    Some(o)
}

fn b(x: bool) -> u8 {
    x as u8
}

impl SignedOrder {
    /// The exact bytes the trading key signs.
    pub fn message(&self) -> Vec<u8> {
        order_message(
            &self.chain_id,
            &self.clearing_contract,
            &self.account,
            &self.market,
            self.side,
            self.order_type,
            self.price,
            self.qty,
            self.reduce_only,
            self.post_only,
            self.tif,
            self.nonce,
            self.expiry_ns,
            self.client_order_id,
            self.max_fee_bps,
            &self.policy_hash,
        )
    }

    /// Fields contain no separator and identifiers are well formed.
    pub fn well_formed(&self) -> bool {
        valid_chain_id(&self.chain_id)
            && valid_account(&self.clearing_contract)
            && valid_account(&self.account)
            && valid_market(&self.market)
            && self.qty > 0
            && self.price > 0
            && !(self.post_only && (self.order_type == OrderType::Market || self.tif != TimeInForce::Gtc))
            && !(self.order_type == OrderType::Market && self.tif == TimeInForce::Gtc)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn order_message(
    chain_id: &str,
    contract: &str,
    account: &str,
    market: &str,
    side: Side,
    order_type: OrderType,
    price: u64,
    qty: u64,
    reduce_only: bool,
    post_only: bool,
    tif: TimeInForce,
    nonce: u64,
    expiry_ns: u64,
    client_order_id: u64,
    max_fee_bps: u16,
    policy_hash: &[u8; 32],
) -> Vec<u8> {
    format!(
        "{ORDER_V1}|{chain_id}|{contract}|{account}|{market}|{}|{}|{price}|{qty}|{}|{}|{}|{nonce}|{expiry_ns}|{client_order_id}|{max_fee_bps}|{}",
        side.as_str(),
        order_type.as_str(),
        b(reduce_only),
        b(post_only),
        tif.as_str(),
        hex32(policy_hash)
    )
    .into_bytes()
}

/// Cancel one order by hash. Carries the order's expiry so the on-chain record can be pruned
/// once the order could no longer be filled anyway.
pub fn cancel_message(chain_id: &str, contract: &str, account: &str, order_hash: &[u8; 32], expiry_ns: u64) -> Vec<u8> {
    format!("{CANCEL_V1}|{chain_id}|{contract}|{account}|{}|{expiry_ns}", hex32(order_hash)).into_bytes()
}

/// Raise the account's nonce floor: every order with a smaller nonce becomes unfillable.
pub fn min_nonce_message(chain_id: &str, contract: &str, account: &str, min_nonce: u64) -> Vec<u8> {
    format!("{MIN_NONCE_V1}|{chain_id}|{contract}|{account}|{min_nonce}").into_bytes()
}

/// Withdraw free collateral to the account itself. `nonce` must exceed the account's action nonce.
pub fn withdraw_message(chain_id: &str, contract: &str, account: &str, token: &str, amount: u128, nonce: u64) -> Vec<u8> {
    format!("{WITHDRAW_V1}|{chain_id}|{contract}|{account}|{token}|{amount}|{nonce}").into_bytes()
}

/// Move collateral between free balance and one isolated position (`delta` > 0 adds margin).
pub fn margin_message(chain_id: &str, contract: &str, account: &str, market: &str, delta: i128, nonce: u64) -> Vec<u8> {
    format!("{MARGIN_V1}|{chain_id}|{contract}|{account}|{market}|{delta}|{nonce}").into_bytes()
}

/// Authenticates private REST reads and WebSocket channels (gateway only, never on-chain).
pub fn auth_message(chain_id: &str, contract: &str, account: &str, ts_ms: u64) -> Vec<u8> {
    format!("{AUTH_V1}|{chain_id}|{contract}|{account}|{ts_ms}").into_bytes()
}

/// NEAR account ids: 2-64 chars of `[a-z0-9._-]`.
pub fn valid_account(s: &str) -> bool {
    (2..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// Market ids: 3-32 chars of `[A-Z0-9-]`, e.g. `CLAUDE-OPUS-PERP`.
pub fn valid_market(s: &str) -> bool {
    (3..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
}

/// Chain ids: 1-32 chars of `[a-z0-9:-]`, e.g. `near:testnet`.
pub fn valid_chain_id(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b':' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_message_is_stable() {
        let m = order_message(
            "near:testnet",
            "perps.zeroflux.testnet",
            "alice.testnet",
            "OPUS-PERP",
            Side::Buy,
            OrderType::Limit,
            1_250,
            12,
            false,
            true,
            TimeInForce::Gtc,
            7,
            1_800_000_000_000_000_000,
            42,
            10,
            &[0xab; 32],
        );
        assert_eq!(
            String::from_utf8(m).unwrap(),
            format!(
                "zfx-perps-order-v1|near:testnet|perps.zeroflux.testnet|alice.testnet|OPUS-PERP|BUY|LIMIT|1250|12|0|1|GTC|7|1800000000000000000|42|10|{}",
                "ab".repeat(32)
            )
        );
    }

    #[test]
    fn hex_roundtrip_rejects_upper() {
        let h = [0x0f; 32];
        assert_eq!(parse_hex32(&hex32(&h)), Some(h));
        assert_eq!(parse_hex32(&"0F".repeat(32)), None);
        assert_eq!(parse_hex32("00"), None);
    }

    #[test]
    fn validators() {
        assert!(valid_chain_id("near:testnet"));
        assert!(!valid_chain_id("near|testnet"));
        assert!(valid_market("OPUS-PERP"));
        assert!(!valid_market("opus"));
        assert!(!valid_account("A.near"));
    }
}
