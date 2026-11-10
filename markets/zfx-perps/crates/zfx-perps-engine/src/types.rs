//! Engine inputs and outputs. Everything here is serde-serializable: commands go to the journal,
//! events go to market data, account streams and the settlement adapter.

use serde::{Deserialize, Serialize};
use zfx_perps_clearing as zc;

/// Internal account index. Account 0 is the fee collector; real accounts are created from chain
/// events (first deposit or key registration) in chain order, so ids are deterministic.
pub type AccountId = u32;
pub type AssetId = u16;
pub type MarketId = u16;
pub type OrderId = u64;
pub type Key = [u8; 32];

pub const FEE_ACCOUNT: AccountId = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tif {
    /// Good till canceled: match what crosses, rest the remainder.
    Gtc,
    /// Immediate or cancel: match what crosses, cancel the remainder.
    Ioc,
    /// Fill or kill: fill the whole quantity immediately or do nothing.
    Fok,
    /// Rest only. Rejected if any part would cross.
    PostOnly,
}

impl Tif {
    pub fn as_str(self) -> &'static str {
        match self {
            Tif::Gtc => "gtc",
            Tif::Ioc => "ioc",
            Tif::Fok => "fok",
            Tif::PostOnly => "post_only",
        }
    }
}

/// Self-trade prevention, applied when an incoming order would match a resting order from the same
/// account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stp {
    /// Cancel the rest of the incoming order. Resting orders stay.
    CancelTaker,
    /// Cancel the resting order and keep matching.
    CancelMaker,
    /// Cancel both the resting order and the rest of the incoming order.
    CancelBoth,
}

impl Stp {
    pub fn as_str(self) -> &'static str {
        match self {
            Stp::CancelTaker => "cancel_taker",
            Stp::CancelMaker => "cancel_maker",
            Stp::CancelBoth => "cancel_both",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSpec {
    pub id: AssetId,
    pub symbol: String,
    /// Settlement-chain reference, e.g. the NEP-141 token contract id.
    pub chain_ref: String,
    pub decimals: u8,
}

/// A spot market. Prices are integer ticks and quantities integer lots, so all arithmetic is exact:
/// `notional (quote atoms) = lots * ticks * quote_tick` and `base atoms = lots * base_lot`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketSpec {
    pub id: MarketId,
    pub symbol: String,
    pub base: AssetId,
    pub quote: AssetId,
    /// Base atoms per lot.
    pub base_lot: u128,
    /// Quote atoms per lot per tick of price.
    pub quote_tick: u128,
    pub min_qty: u64,
    pub max_qty: u64,
    pub max_price: u64,
    /// Maker fee, must be <= taker fee (buy-side holds are sized with the taker fee).
    pub maker_fee_bps: u16,
    pub taker_fee_bps: u16,
    /// Max resting orders per account in this market.
    pub max_open_orders: u32,
    /// Set for a perpetual futures market; `None` for spot.
    pub perp: Option<PerpParams>,
}

/// A USDC-margined linear perpetual. Positions are in lots of the underlying and valued at
/// `lots × price × quote_tick` quote atoms, exactly like spot notional; `base` must equal `quote`
/// (the collateral asset), since the underlying is never delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerpParams {
    /// Initial margin, bps of notional (1000 = 10x max leverage).
    pub imr_bps: u16,
    /// Maintenance margin, bps of notional. Below it the account is liquidated.
    pub mmr_bps: u16,
    /// Liquidation fee, bps of closed notional, paid to the insurance fund.
    pub liq_fee_bps: u16,
    /// Orders priced further than this from the mark are rejected.
    pub price_band_bps: u16,
    /// Liquidation orders close at most this far from the mark; the rest goes to insurance.
    pub liq_slippage_bps: u16,
    /// Funding rate per interval = clamp(average premium / funding_divisor, ±max_funding_ppm).
    pub funding_divisor: u32,
    pub max_funding_ppm: u32,
}

impl Default for PerpParams {
    fn default() -> Self {
        PerpParams {
            imr_bps: 1_000,
            mmr_bps: 500,
            liq_fee_bps: 100,
            price_band_bps: 1_000,
            liq_slippage_bps: 300,
            funding_divisor: 8,
            max_funding_ppm: 750,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewOrder {
    pub account: AccountId,
    pub market: MarketId,
    pub side: Side,
    pub price: u64,
    pub qty: u64,
    pub tif: Tif,
    pub stp: Stp,
    pub client_id: u64,
    /// Gateway receive time, journaled so replay is deterministic.
    pub ts_ns: u64,
}

/// Events read from the settlement chain, in the chain's own sequence order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChainEvent {
    Deposit {
        seq: u64,
        account: String,
        token: String,
        amount: u128,
    },
    KeyAdded {
        seq: u64,
        account: String,
        public_key: Key,
    },
    KeyRemoved {
        seq: u64,
        account: String,
        public_key: Key,
    },
    /// A user asked the contract for a withdrawal that the operator must include within the
    /// contract's timeout, or the contract freezes.
    ForcedWithdrawal {
        seq: u64,
        request_id: u64,
        account: String,
        token: String,
        amount: u128,
    },
    /// A withdrawal from a settled batch did not pay out (bad signature, key removed, or the token
    /// transfer failed). The contract kept the funds on the account; the engine credits them back.
    WithdrawalReverted {
        seq: u64,
        account: String,
        token: String,
        amount: u128,
    },
    BatchSettled {
        seq: u64,
        batch_id: u64,
    },
}

impl ChainEvent {
    pub fn seq(&self) -> u64 {
        match self {
            ChainEvent::Deposit { seq, .. }
            | ChainEvent::KeyAdded { seq, .. }
            | ChainEvent::KeyRemoved { seq, .. }
            | ChainEvent::ForcedWithdrawal { seq, .. }
            | ChainEvent::WithdrawalReverted { seq, .. }
            | ChainEvent::BatchSettled { seq, .. } => *seq,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    AddAsset(AssetSpec),
    AddMarket(MarketSpec),
    SetHalted {
        market: MarketId,
        halted: bool,
    },
    Chain(ChainEvent),
    Place(NewOrder),
    Cancel {
        account: AccountId,
        market: MarketId,
        order_id: OrderId,
    },
    CancelByClientId {
        account: AccountId,
        market: MarketId,
        client_id: u64,
    },
    CancelAll {
        account: AccountId,
        market: Option<MarketId>,
    },
    /// Signed withdrawal. The signature is checked by the gateway and again on-chain.
    Withdraw {
        account: AccountId,
        asset: AssetId,
        amount: u128,
        nonce: u64,
        public_key: Key,
        signature: Vec<u8>,
    },
    /// Oracle update for a perp market (ticks). Journaled, so replay is deterministic. Runs the
    /// liquidation check for every account holding a position in the market.
    SetMark {
        market: MarketId,
        mark: u64,
        index: u64,
        ts_ns: u64,
    },
    /// Apply funding for the interval since the last tick, from the premium samples collected by
    /// `SetMark`.
    FundingTick {
        market: MarketId,
        ts_ns: u64,
    },
    /// Close the pending settlement accumulator into one or more batches. `journal_seq` and
    /// `journal_hash` identify the command log prefix the batch is derived from, so anyone with the
    /// journal can replay and check what the operator posted.
    CutBatch {
        journal_seq: u64,
        journal_hash: [u8; 32],
    },
    /// Enable clearing mode (NEAR clearing contract). Journaled once at bootstrap.
    ClearingInit(crate::clearing::ClearingConfig),
    /// A user-signed order for a clearing market. The gateway verified the signature.
    PlaceSigned {
        order: Box<zc::SignedOrder>,
        stp: Stp,
        ts_ns: u64,
    },
    /// Withdraw, margin move, relayed cancel, nonce floor, oracle print or funding for clearing
    /// markets. Applied to the predicted ledger and queued for the next batch.
    ClearingAction {
        op: Box<zc::Op>,
        ts_ns: u64,
    },
    /// An event from the clearing contract, in its sequence. `ts_ns` is the gateway time it arrived.
    ClearingChain {
        seq: u64,
        event: crate::clearing::ClearingEvent,
        ts_ns: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reject {
    UnknownMarket,
    UnknownAsset,
    UnknownAccount,
    UnknownOrder,
    MarketHalted,
    MarketExists,
    AssetExists,
    InvalidSpec,
    InvalidQty,
    InvalidPrice,
    InsufficientBalance,
    PostOnlyWouldCross,
    FokUnfillable,
    DuplicateClientId,
    TooManyOpenOrders,
    NonceTooLow,
    KeyNotRegistered,
    ChainEventGap,
    UnknownBatch,
    NotAPerp,
    MarkUnavailable,
    PriceOutOfBand,
    InsufficientMargin,
    /// Refused by the clearing rules (same check the contract runs).
    Clearing(zc::Reject),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    User,
    /// IOC/FOK remainder, or STP cancel of the incoming order.
    Unfilled,
    SelfTrade,
    ForcedWithdrawal,
    Liquidation,
    /// A resting clearing order failed its fill check (expired, margin, revoked key...).
    Unfillable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Resting,
    Filled,
    /// Matched partially or not at all; the remainder was canceled (IOC/FOK/STP).
    Canceled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Ack {
    Ok,
    /// A chain event that was already applied (seq <= cursor). Safe to resend.
    Duplicate,
    Placed {
        order_id: OrderId,
        status: OrderStatus,
        filled: u64,
        remaining: u64,
    },
    Canceled {
        order_id: OrderId,
        remaining: u64,
    },
    CanceledAll {
        count: u32,
    },
    WithdrawQueued {
        nonce: u64,
    },
    BatchesCut {
        first: u64,
        count: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaEntry {
    pub account: String,
    pub token: String,
    pub delta: i128,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawEntry {
    pub account: String,
    pub token: String,
    pub amount: u128,
    pub nonce: u64,
    pub public_key: Key,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForcedEntry {
    pub request_id: u64,
    pub account: String,
    pub token: String,
    pub amount: u128,
}

/// One settlement transaction. Batches cut together form a group; only the last (`seal`) must
/// leave every token's running net delta at zero. Deltas for one account never span batches, so
/// each batch keeps every account's on-chain balance non-negative on its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub id: u64,
    pub entries: Vec<DeltaEntry>,
    pub withdrawals: Vec<WithdrawEntry>,
    pub forced: Vec<ForcedEntry>,
    pub seal: bool,
    pub journal_seq: u64,
    pub journal_hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    AssetAdded {
        asset: AssetSpec,
    },
    MarketAdded {
        market: MarketSpec,
    },
    MarketHalted {
        market: MarketId,
        halted: bool,
    },
    AccountCreated {
        account: AccountId,
        name: String,
    },
    KeyAdded {
        account: AccountId,
        public_key: Key,
    },
    KeyRemoved {
        account: AccountId,
        public_key: Key,
    },
    Deposited {
        account: AccountId,
        asset: AssetId,
        amount: u128,
    },
    Credited {
        account: AccountId,
        asset: AssetId,
        amount: u128,
    },
    Accepted {
        order_id: OrderId,
        account: AccountId,
        market: MarketId,
        side: Side,
        price: u64,
        qty: u64,
        client_id: u64,
        tif: Tif,
    },
    Rejected {
        account: AccountId,
        client_id: u64,
        reason: Reject,
    },
    Fill {
        trade_id: u64,
        market: MarketId,
        price: u64,
        qty: u64,
        taker_side: Side,
        maker: AccountId,
        maker_order: OrderId,
        maker_fee: u128,
        maker_remaining: u64,
        taker: AccountId,
        taker_order: OrderId,
        taker_fee: u128,
        ts_ns: u64,
    },
    Canceled {
        order_id: OrderId,
        account: AccountId,
        market: MarketId,
        remaining: u64,
        reason: CancelReason,
    },
    /// L2 level update: total resting quantity at a price after this command (0 = level gone).
    Level {
        market: MarketId,
        side: Side,
        price: u64,
        qty: u64,
    },
    WithdrawQueued {
        account: AccountId,
        asset: AssetId,
        amount: u128,
        nonce: u64,
    },
    ForcedQueued {
        request_id: u64,
        account: AccountId,
        asset: AssetId,
        amount: u128,
    },
    BatchCut {
        batch: Batch,
    },
    BatchSettled {
        batch_id: u64,
    },
    Mark {
        market: MarketId,
        mark: u64,
        index: u64,
    },
    /// Funding applied: longs paid `payment_per_lot` quote atoms per lot (negative: received).
    Funding {
        market: MarketId,
        rate_ppm: i64,
        payment_per_lot: i128,
    },
    /// Position after a change: signed lots and the open portion's entry notional.
    Position {
        account: AccountId,
        market: MarketId,
        size: i64,
        entry_notional: u128,
    },
    /// An account fell below maintenance margin and was liquidated.
    Liquidated {
        account: AccountId,
        equity: i128,
        maintenance: i128,
    },
    /// Part of a liquidated position the book couldn't absorb, taken by the insurance fund at the mark.
    InsuranceTakeover {
        account: AccountId,
        market: MarketId,
        size: i64,
        price: u64,
    },
    /// Losses beyond an account's collateral, covered by the insurance fund.
    Bankruptcy {
        account: AccountId,
        deficit: i128,
    },
    /// Losses beyond the insurance fund, clawed back pro rata from accounts in profit.
    Socialized {
        amount: i128,
        from_accounts: u32,
    },
    /// Clearing position after a change (predicted ledger).
    ClearingPosition {
        account: AccountId,
        market: String,
        size: i64,
        margin: i128,
    },
    ClearingFree {
        account: AccountId,
        free: u128,
    },
    /// Any other clearing-core event (fills, liquidations, oracle, funding, payouts).
    Clearing {
        event: zc::CEvent,
    },
    ClearingBatchCut {
        batch: Box<zc::Batch>,
    },
    ClearingOpRejected {
        batch_id: u64,
        index: u32,
        reason: zc::Reject,
    },
    /// A fill the engine reported that the chain will not settle (an on-chain change raced it).
    FillReverted {
        market: String,
        maker: String,
        taker: String,
        qty: u64,
        price: u64,
    },
    ClearingOpDropped {
        op: Box<zc::Op>,
    },
    ClearingLiquidationRetry {
        account: String,
        reason: zc::Reject,
    },
    ClearingLiquidationFailed {
        account: String,
        reason: zc::Reject,
    },
    /// The chain applied something this engine can't reproduce. Clearing markets stop admitting
    /// orders until an operator investigates.
    ClearingDivergence {
        detail: String,
    },
}
