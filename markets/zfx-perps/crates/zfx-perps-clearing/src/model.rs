//! Ledger state: markets, accounts, isolated positions, trading keys, order records.

use crate::print::{DataStatus, OracleStatus};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

pub const NS_PER_SEC: u64 = 1_000_000_000;
pub const NS_PER_DAY: u64 = 86_400 * NS_PER_SEC;
/// Upper bound on one fill's notional in collateral atoms (2^100). Market specs are checked
/// against it so that no product of quantities, prices and rates can overflow i128.
pub const MAX_NOTIONAL: u128 = 1u128 << 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum MarketType {
    Perpetual,
    /// Reserved by the spec's interface. Not tradeable in v1: fills on it are rejected.
    ExpiringFuture,
}

/// Admin-set lifecycle status (spec 5.2). The effective status at a given time can be stricter
/// (see [`crate::ledger::effective_status`]): a stale or missing oracle print forces CLOSE_ONLY or
/// HALTED regardless of this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum MarketStatus {
    Proposed,
    Active,
    CloseOnly,
    Halted,
    Settling,
    Closed,
}

impl MarketStatus {
    pub fn name(self) -> &'static str {
        ["PROPOSED", "ACTIVE", "CLOSE_ONLY", "HALTED", "SETTLING", "CLOSED"][self as usize]
    }
}

/// Market parameters (spec 5.2 `MarketSpec`, in integer units). Everything here is covered by the
/// policy hash that every signed order commits to.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct MarketParams {
    pub market_id: String,
    pub market_type: MarketType,
    /// Oracle index this market tracks, and the formula version prints must carry.
    pub index_id: String,
    pub index_version: String,
    /// Decimal places of the print value (`value_scale` in the print).
    pub value_scale: u8,
    /// Print value units per price tick: `mark_ticks = value / index_units_per_tick`.
    pub index_units_per_tick: u64,
    /// Collateral atoms per lot per tick: `notional = lots × ticks × tick_value`.
    pub tick_value: u128,
    pub min_qty: u64,
    pub max_qty: u64,
    pub max_price: u64,
    pub initial_margin_bps: u16,
    pub maintenance_margin_bps: u16,
    /// Must satisfy `initial_margin_bps × max_leverage >= 10_000`.
    pub max_leverage: u16,
    /// Cap on long open interest (= short open interest), in lots.
    pub max_open_interest: u64,
    /// Cap on one account's absolute position, in lots.
    pub max_position: u64,
    pub maker_fee_bps: u16,
    pub taker_fee_bps: u16,
    /// Paid by a liquidated position to the insurance fund, bps of closed notional at the mark.
    pub liq_fee_bps: u16,
    /// Liquidation fills must be within this distance of the mark.
    pub liq_slippage_bps: u16,
    /// Every fill must be within this distance of the mark.
    pub price_band_bps: u16,
    /// A print older than this (by `published_at`) is stale.
    pub oracle_max_age_sec: u64,
    /// 0 disables funding.
    pub funding_interval_sec: u64,
    /// Funding rate per interval = clamp(premium / funding_divisor, ±max_funding_ppm).
    pub funding_divisor: u32,
    pub max_funding_ppm: u32,
    /// Daily-index cycle. 0 = off. Otherwise, from this many seconds after 00:00 UTC the market
    /// is CLOSE_ONLY until the print expected that day lands (see `print_lag_days`).
    pub print_close_only_from_sec: u32,
    /// How many days late the source publishes: the print expected on day D covers the UTC day
    /// ending at `D − print_lag_days + 1` days. 0 = covers day D itself (lands after midnight).
    /// A missing expected print makes the market CLOSE_ONLY until it arrives.
    pub print_lag_days: u8,
    /// Limit move: one print moves the mark at most this far toward the index. While the mark
    /// trails the index the market is CLOSE_ONLY. With `mark_limit_bps + liq_slippage_bps +` one
    /// day of maximum funding `<= maintenance_margin_bps` (enforced by the contract), a position that
    /// isn't liquidatable can't go bankrupt on the next print.
    pub mark_limit_bps: u32,
    /// A usable print further than this from the mark is treated as suspect data: the market halts
    /// for review and the mark doesn't move. 0 disables.
    pub halt_jump_bps: u32,
}

/// Snapshot of the last accepted print (the canonical bytes are not kept; their hash is).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct PrintInfo {
    pub hash: [u8; 32],
    pub value: u64,
    pub status: OracleStatus,
    pub data_status: DataStatus,
    pub source_start_ms: i64,
    pub source_end_ms: i64,
    pub published_at_ms: i64,
    pub valid_until_ms: i64,
    pub revision: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct MarketState {
    pub params: MarketParams,
    pub policy_hash: [u8; 32],
    pub status: MarketStatus,
    /// Current mark in ticks (0 = none yet). Only usable prints move it, by at most the limit.
    pub mark: u64,
    /// The last usable print's value in ticks. The mark converges to it print by print.
    pub index_ticks: u64,
    pub print: Option<PrintInfo>,
    /// Long open interest in lots; always equals short open interest.
    pub open_interest: u64,
    /// Cumulative funding paid by one long lot since listing (collateral atoms; negative = received).
    pub cum_funding: i128,
    pub last_funding_ns: u64,
    pub last_funding_rate_ppm: i64,
    /// Fill notional (price × qty, in ticks × lots) and quantity since the last funding, for the
    /// premium of traded prices over the index.
    pub fund_px_qty: u128,
    pub fund_qty: u128,
    /// Losses nobody could absorb (insurance and socialization exhausted). Non-zero halts the market.
    pub bad_debt: u128,
}

impl MarketState {
    pub fn notional(&self, lots: u64, price: u64) -> u128 {
        lots as u128 * price as u128 * self.params.tick_value
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct AccountState {
    /// Collateral not allocated to any position.
    pub free: u128,
    /// Orders with a smaller nonce can't be filled.
    pub min_nonce: u64,
    /// Last withdrawal / margin-adjustment nonce used (strictly increasing).
    pub action_nonce: u64,
}

/// One isolated-margin position. Its equity at mark `M` is
/// `margin + size × M × tick_value − size × (cum_funding − funding_index)`.
/// `margin` absorbs every cash flow of the position (trade cash, fees, funding once settled), so
/// realized and unrealized PnL never need separate books.
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct Position {
    pub size: i64,
    pub margin: i128,
    pub funding_index: i128,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct KeyInfo {
    pub account: String,
    /// Unix ns after which the key signs nothing (0 = no expiry).
    pub expires_ns: u64,
    /// May the key sign withdrawals? Trading-only keys can't.
    pub can_withdraw: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct OrderState {
    pub account: String,
    pub filled: u64,
    pub canceled: bool,
    /// Prunable after this.
    pub expiry_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct PendingDeposit {
    pub account: String,
    pub amount: u128,
    pub created_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct ForcedRequest {
    pub account: String,
    pub amount: u128,
    pub created_ns: u64,
    pub done: bool,
    pub paid: u128,
}

/// Deployment-wide settings and running totals.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct Globals {
    pub chain_id: String,
    pub contract_id: String,
    /// The single allowlisted collateral token (NEP-141 contract id).
    pub collateral: String,
    /// Ledger account of the insurance fund (holds free collateral and can hold positions).
    pub insurance_account: String,
    /// Trading fees accrued, withdrawable by the owner after a timelock.
    pub fee_balance: u128,
    pub reporters: Vec<[u8; 32]>,
    pub quorum: u8,
    /// Timestamp of the last applied op; op timestamps never go backwards.
    pub last_op_ns: u64,
    /// How long after an expiry (order, key, print) the chain still accepts an op stamped before
    /// it. Bounds how far the operator can backdate.
    pub settle_grace_ns: u64,
    /// Orders may not expire further out than this from their first fill.
    pub max_order_ttl_ns: u64,
    /// Collateral the contract owes in total (credited deposits minus payouts); the token balance
    /// held for the ledger.
    pub total_collateral: u128,
}
