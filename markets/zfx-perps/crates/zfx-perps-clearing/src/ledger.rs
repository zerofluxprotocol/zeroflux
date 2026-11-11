//! The clearing ledger: every state transition the NEAR contract accepts, as pure functions over a
//! [`Store`]. The engine runs the same functions to predict the chain exactly.
//!
//! Each operation reads what it needs into a [`Tx`] working set, checks everything, and only
//! commits if every check passed. A rejected operation leaves the store untouched.
//!
//! ## Isolated margin
//!
//! Each (account, market) position has its own `margin`. Equity at mark `M` is
//! `margin + size·M·tick_value − size·(cum_funding − funding_index)`.
//! A fill that increases exposure tops the position's margin up from the account's free
//! collateral to the initial margin of the new size, at the mark. A fill that reduces exposure
//! must leave equity non-negative; a fill that closes the position returns its whole margin to free
//! collateral. Nothing ever moves between two positions of the same account, so a loss in one
//! market can't touch collateral in another (spec 5.3: no unbounded cross-market netting).

use crate::model::*;
use crate::print::{DataStatus, OraclePrint, OracleStatus};
use crate::store::{Crypto, Store};
use crate::wire::{self, Side, SignedOrder, TimeInForce, OrderType};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Op timestamps may lead the block time by at most this much (clock skew between the operator
/// and NEAR validators).
pub const MAX_FUTURE_SKEW_NS: u64 = 10 * NS_PER_SEC;
/// Prints may be published at most this far ahead of the op that applies them.
pub const PRINT_FUTURE_SKEW_NS: u64 = 60 * NS_PER_SEC;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reject {
    // Order validity.
    Malformed,
    WrongChain,
    WrongContract,
    WrongMarket,
    KeyNotRegistered,
    KeyExpired,
    KeyScope,
    BadSignature,
    PolicyMismatch,
    NonceBelowMin,
    Expired,
    ExpiryTooFar,
    OrderCanceled,
    Overfill,
    FeeAboveMax,
    ReduceOnlyViolated,
    PostOnlyTaker,
    MakerNotResting,
    InvalidQty,
    InvalidPrice,
    // Fill / market.
    UnknownMarket,
    MarketTypeUnsupported,
    MarketNotTrading,
    CloseOnly,
    NoMark,
    SameAccount,
    SideMismatch,
    PriceOutsideLimit,
    PriceOutOfBand,
    OpenInterestCap,
    PositionLimit,
    InsufficientMargin,
    NegativeEquity,
    // Liquidation.
    NoPosition,
    NotLiquidatable,
    LiquidationPrice,
    InsuranceCannotAbsorb,
    InsuranceCouldAbsorb,
    AdlInvalid,
    SocializeIncomplete,
    // Oracle.
    OracleDecode,
    OracleIndexMismatch,
    OracleScale,
    OracleQuorum,
    OracleNotNewer,
    OracleExpired,
    OracleFuture,
    // Funding.
    FundingDisabled,
    FundingNotDue,
    // Account actions.
    NonceUsed,
    InsufficientFree,
    ZeroAmount,
    UnknownDeposit,
    UnknownForced,
    ForcedDone,
    // Batch level.
    TimeBackwards,
    TimeFuture,
    BadOrderIndex,
    Overflow,
}

/// Which side of a fill a rejection is attributable to, so the matcher knows whether to drop the
/// resting order (maker) or stop the incoming one (taker).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum Party {
    Maker,
    Taker,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub reason: Reject,
    pub party: Party,
}

impl From<Reject> for Failure {
    fn from(reason: Reject) -> Self {
        Failure { reason, party: Party::Other }
    }
}

fn maker(r: Reject) -> Failure {
    Failure { reason: r, party: Party::Maker }
}
fn taker(r: Reject) -> Failure {
    Failure { reason: r, party: Party::Taker }
}

pub type Res<T> = Result<T, Failure>;

/// Times an operation is evaluated at: the op's own timestamp (set by the sequencer, never
/// decreasing) and the block time it lands in. The engine uses `block = op` when predicting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Now {
    pub op: u64,
    pub block: u64,
}

impl Now {
    pub fn at(t: u64) -> Self {
        Now { op: t, block: t }
    }
    /// True while `deadline` hasn't passed for the op, and hadn't passed more than `grace` before
    /// the block. Bounds how far an operator can backdate an op.
    fn before(&self, deadline: u64, grace: u64) -> bool {
        self.op < deadline && self.block < deadline.saturating_add(grace)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub enum CEvent {
    Fill {
        market: String,
        maker: String,
        taker: String,
        maker_order: [u8; 32],
        taker_order: [u8; 32],
        taker_side: Side,
        price: u64,
        qty: u64,
        maker_fee: u128,
        taker_fee: u128,
    },
    Position {
        account: String,
        market: String,
        size: i64,
        margin: i128,
        funding_index: i128,
    },
    Free {
        account: String,
        free: u128,
    },
    Liquidated {
        account: String,
        market: String,
        size: i64,
        equity: i128,
        maintenance: i128,
    },
    InsuranceTakeover {
        account: String,
        market: String,
        size: i64,
        price: u64,
    },
    Adl {
        target: String,
        market: String,
        qty: u64,
        price: u64,
    },
    LiquidationFee {
        account: String,
        market: String,
        fee: u128,
    },
    /// A closed position's loss beyond its margin and how it was absorbed.
    Deficit {
        account: String,
        market: String,
        deficit: u128,
        insurance_paid: u128,
        socialized: u128,
        bad_debt: u128,
    },
    OracleAccepted {
        market: String,
        print_hash: [u8; 32],
        mark: u64,
        usable: bool,
    },
    MarketHalted {
        market: String,
        reason: HaltReason,
    },
    Funding {
        market: String,
        rate_ppm: i64,
        pay_per_lot: i128,
    },
    Payout {
        account: String,
        amount: u128,
        kind: PayoutKind,
    },
    Credited {
        deposit_id: u64,
        account: String,
        amount: u128,
    },
    OrderCanceled {
        account: String,
        order_hash: [u8; 32],
    },
    MinNonce {
        account: String,
        min_nonce: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltReason {
    /// A print moved the index by more than `halt_jump_bps` (suspect data).
    IndexJump,
    /// A print's value can't be expressed as a mark (zero or above `max_price`).
    IndexOutOfRange,
    /// A loss nobody could absorb.
    BadDebt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayoutKind {
    Withdraw,
    Forced,
}

// ---------------------------------------------------------------------------------------------
// Math
// ---------------------------------------------------------------------------------------------

fn ceil_bps(x: u128, bps: u32) -> u128 {
    (x * bps as u128).div_ceil(10_000)
}

/// Initial margin of `lots` at `mark`.
pub fn initial_margin(m: &MarketState, lots: u64, mark: u64) -> u128 {
    ceil_bps(m.notional(lots, mark), m.params.initial_margin_bps as u32)
}

pub fn maintenance_margin(m: &MarketState, lots: u64, mark: u64) -> u128 {
    ceil_bps(m.notional(lots, mark), m.params.maintenance_margin_bps as u32)
}

pub fn fee(notional: u128, bps: u16) -> u128 {
    ceil_bps(notional, bps as u32)
}

/// Funding owed by a position since it was last settled (positive = it pays).
pub fn funding_owed(m: &MarketState, p: &Position) -> i128 {
    p.size as i128 * (m.cum_funding - p.funding_index)
}

/// Equity of a position at `mark`, funding included.
pub fn equity(m: &MarketState, p: &Position, mark: u64) -> i128 {
    p.margin + p.size as i128 * mark as i128 * m.params.tick_value as i128 - funding_owed(m, p)
}

/// Settle accrued funding into the position's margin.
pub fn touch(m: &MarketState, p: &mut Position) {
    p.margin -= funding_owed(m, p);
    p.funding_index = m.cum_funding;
}

/// Bankruptcy-free liquidation price estimate for display: the mark at which equity equals
/// maintenance margin. `None` for a flat position.
pub fn liquidation_price(m: &MarketState, p: &Position) -> Option<u64> {
    if p.size == 0 {
        return None;
    }
    let tv = m.params.tick_value as i128;
    let s = p.size as i128;
    let mmr = m.params.maintenance_margin_bps as i128;
    let cash = p.margin - funding_owed(m, p);
    // cash + s·M·tv = |s|·M·tv·mmr/1e4  =>  M = −cash / (tv·(s − |s|·mmr/1e4))
    let denom = tv * (s * 10_000 - s.abs() * mmr);
    if denom == 0 {
        return None;
    }
    let num = -cash * 10_000;
    let px = num / denom;
    Some(px.max(0) as u64)
}

/// The mark after one print: the index, clamped to `limit_bps` of the current mark.
pub fn limit_move(mark: u64, index: u64, limit_bps: u32) -> u64 {
    if mark == 0 {
        return index;
    }
    let step = (mark as u128 * limit_bps as u128 / 10_000).max(1) as u64;
    if index > mark { index.min(mark.saturating_add(step)) } else { index.max(mark.saturating_sub(step).max(1)) }
}

/// The publisher's jump clamp, on raw print values: the new value differs from the previous
/// usable (OK) value by more than `bps`. Same definition as the index's `clamped` flag
/// (`docs/DATA_METHOD.md` 12.4): strict inequality, exact integer arithmetic.
pub fn print_move_exceeds(prev: u64, value: u64, bps: u32) -> bool {
    bps > 0 && prev > 0 && (value.abs_diff(prev) as u128) * 10_000 > prev as u128 * bps as u128
}

/// The contract's listing rule: a print can't move the mark further than a non-liquidatable
/// position can absorb. `mark_limit + liq_slippage + max funding over one day <= maintenance`.
pub fn check_solvency_rule(p: &MarketParams) -> Result<(), &'static str> {
    let per_day = if p.funding_interval_sec == 0 { 0 } else { 86_400u64.div_ceil(p.funding_interval_sec) };
    let funding_bps = (p.max_funding_ppm as u64 * per_day).div_ceil(100);
    if p.mark_limit_bps as u64 + p.liq_slippage_bps as u64 + funding_bps > p.maintenance_margin_bps as u64 {
        return Err("mark_limit_bps + liq_slippage_bps + daily max funding must not exceed maintenance_margin_bps");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Market status
// ---------------------------------------------------------------------------------------------

/// The print is usable and neither expired nor older than `oracle_max_age_sec`.
pub fn mark_fresh(m: &MarketState, now: Now, grace: u64) -> bool {
    let Some(p) = &m.print else { return false };
    if m.mark == 0 || p.status != OracleStatus::Fresh || p.data_status != DataStatus::Ok {
        return false;
    }
    let valid_until = (p.valid_until_ms.max(0) as u64).saturating_mul(1_000_000);
    let published = (p.published_at_ms.max(0) as u64).saturating_mul(1_000_000);
    let max_age_until = published.saturating_add(m.params.oracle_max_age_sec.saturating_mul(NS_PER_SEC));
    now.before(valid_until.min(max_age_until), grace)
}

/// Effective status at `now`: the admin status, made stricter by the oracle.
///
/// - No print at all: HALTED.
/// - Any non-FRESH print (STALE, DISPUTED, UNAVAILABLE, data NOT_REPORTED / UNPRICED /
///   NO_VALID_CLOSE_PRICE), a print past `validUntil` or older than `oracleMaxAgeSec`, or a last
///   print that moved the index more than `print_move_close_only_bps`: CLOSE_ONLY. The last mark is
///   kept for reducing trades; a non-fresh mark is never used for liquidation or funding.
/// - Daily cycle (`print_close_only_from_sec > 0`): CLOSE_ONLY from that time of day until the
///   print expected that day lands, and all day if the previous day's expected print is missing.
pub fn effective_status(m: &MarketState, now: Now, grace: u64) -> MarketStatus {
    match m.status {
        MarketStatus::Active => {}
        s => return s,
    }
    let Some(p) = &m.print else { return MarketStatus::Halted };
    if m.mark == 0 {
        return MarketStatus::Halted;
    }
    // Any non-FRESH oracle state (STALE, DISPUTED, UNAVAILABLE / NOT_REPORTED ...): close-only
    // until the next clean print. The last mark only prices reductions.
    if p.status != OracleStatus::Fresh || p.data_status != DataStatus::Ok {
        return MarketStatus::CloseOnly;
    }
    if m.print_move_flag {
        return MarketStatus::CloseOnly;
    }
    if !mark_fresh(m, now, grace) {
        return MarketStatus::CloseOnly;
    }
    // Limit-locked: the mark hasn't caught up with the index yet.
    if m.index_ticks != 0 && m.mark != m.index_ticks {
        return MarketStatus::CloseOnly;
    }
    let from = m.params.print_close_only_from_sec as u64;
    if from > 0 {
        let lag = m.params.print_lag_days as u64 * NS_PER_DAY;
        let day_start = now.op - now.op % NS_PER_DAY;
        let end_ns = (p.source_end_ms.max(0) as u64).saturating_mul(1_000_000);
        // Yesterday's expected print never arrived.
        if end_ns.saturating_add(lag) < day_start {
            return MarketStatus::CloseOnly;
        }
        // Today's expected print is due and not in yet.
        if now.op - day_start >= from * NS_PER_SEC && end_ns.saturating_add(lag) < day_start + NS_PER_DAY {
            return MarketStatus::CloseOnly;
        }
    }
    MarketStatus::Active
}

// ---------------------------------------------------------------------------------------------
// Market admin
// ---------------------------------------------------------------------------------------------

pub const POLICY_DOMAIN: &[u8] = b"zfx-perps-policy-v1";

pub fn policy_hash<C: Crypto>(c: &C, p: &MarketParams) -> [u8; 32] {
    let mut b = POLICY_DOMAIN.to_vec();
    b.extend(borsh::to_vec(p).expect("params serialize"));
    c.sha256(&b)
}

pub fn validate_params(p: &MarketParams) -> Result<(), &'static str> {
    let ok = |c: bool, m: &'static str| if c { Ok(()) } else { Err(m) };
    ok(wire::valid_market(&p.market_id), "market_id must be [A-Z0-9-], 3-32 chars")?;
    ok(!p.index_id.is_empty() && p.index_id.len() <= 256 && p.index_version.len() <= 256, "index_id/version length")?;
    ok(p.index_units_per_tick > 0 && p.tick_value > 0, "index_units_per_tick and tick_value must be > 0")?;
    ok(p.min_qty > 0 && p.min_qty <= p.max_qty && p.max_price > 0, "qty/price bounds")?;
    ok(
        (p.max_qty as u128)
            .checked_mul(p.max_price as u128)
            .and_then(|x| x.checked_mul(p.tick_value))
            .is_some_and(|x| x < MAX_NOTIONAL),
        "max_qty × max_price × tick_value must stay below 2^100",
    )?;
    ok(
        p.maintenance_margin_bps > 0 && p.maintenance_margin_bps < p.initial_margin_bps && p.initial_margin_bps <= 10_000,
        "0 < maintenance < initial <= 10000 bps",
    )?;
    ok(
        p.max_leverage >= 1 && p.initial_margin_bps as u32 * p.max_leverage as u32 >= 10_000,
        "initial_margin_bps × max_leverage must be >= 10000",
    )?;
    ok(p.max_open_interest > 0 && p.max_position > 0, "open interest and position caps must be > 0")?;
    ok(p.maker_fee_bps <= 1_000 && p.taker_fee_bps <= 1_000, "fees <= 10%")?;
    ok(p.liq_fee_bps <= 1_000, "liquidation fee <= 10%")?;
    ok(p.liq_slippage_bps < 10_000 && p.price_band_bps > 0 && p.price_band_bps < 10_000, "band/slippage < 100%")?;
    ok(
        p.liq_slippage_bps <= p.price_band_bps,
        "liquidation slippage must be within the price band",
    )?;
    ok(p.oracle_max_age_sec > 0, "oracle_max_age_sec must be > 0")?;
    ok(
        p.funding_interval_sec == 0 || (p.funding_divisor > 0 && p.max_funding_ppm < 1_000_000),
        "funding divisor/cap",
    )?;
    ok(p.print_close_only_from_sec < 86_400, "print_close_only_from_sec < 86400")?;
    ok(p.print_lag_days <= 3, "print_lag_days <= 3")?;
    ok(p.mark_limit_bps > 0, "mark_limit_bps must be > 0")?;
    Ok(())
}

/// A freshly listed market (status PROPOSED until the admin activates it).
pub fn new_market<C: Crypto>(c: &C, params: MarketParams, now_ns: u64) -> Result<MarketState, &'static str> {
    validate_params(&params)?;
    Ok(MarketState {
        policy_hash: policy_hash(c, &params),
        params,
        status: MarketStatus::Proposed,
        mark: 0,
        index_ticks: 0,
        print_move_flag: false,
        index_value: 0,
        print: None,
        open_interest: 0,
        cum_funding: 0,
        last_funding_ns: now_ns,
        last_funding_rate_ppm: 0,
        fund_px_qty: 0,
        fund_qty: 0,
        bad_debt: 0,
    })
}

/// Does `next` only tighten risk relative to `cur`? Tightening applies immediately; anything
/// else goes through the contract's timelock.
pub fn is_tightening(cur: &MarketParams, next: &MarketParams) -> bool {
    cur.market_id == next.market_id
        && cur.market_type == next.market_type
        && cur.index_id == next.index_id
        && cur.index_version == next.index_version
        && cur.value_scale == next.value_scale
        && cur.index_units_per_tick == next.index_units_per_tick
        && cur.tick_value == next.tick_value
        && next.min_qty >= cur.min_qty
        && next.max_qty <= cur.max_qty
        && next.max_price <= cur.max_price
        && next.initial_margin_bps >= cur.initial_margin_bps
        && next.maintenance_margin_bps >= cur.maintenance_margin_bps
        && next.max_leverage <= cur.max_leverage
        && next.max_open_interest <= cur.max_open_interest
        && next.max_position <= cur.max_position
        && (cur.max_open_interest_notional == 0
            || (next.max_open_interest_notional > 0 && next.max_open_interest_notional <= cur.max_open_interest_notional))
        && (cur.print_move_close_only_bps == 0 || (next.print_move_close_only_bps > 0 && next.print_move_close_only_bps <= cur.print_move_close_only_bps))
        && next.maker_fee_bps <= cur.maker_fee_bps
        && next.taker_fee_bps <= cur.taker_fee_bps
        && next.liq_fee_bps <= cur.liq_fee_bps
        && next.liq_slippage_bps <= cur.liq_slippage_bps
        && next.price_band_bps <= cur.price_band_bps
        && next.oracle_max_age_sec <= cur.oracle_max_age_sec
        && next.funding_interval_sec == cur.funding_interval_sec
        && next.funding_divisor == cur.funding_divisor
        && next.max_funding_ppm <= cur.max_funding_ppm
        && (cur.print_close_only_from_sec == 0 || (next.print_close_only_from_sec > 0 && next.print_close_only_from_sec <= cur.print_close_only_from_sec))
        && next.print_lag_days <= cur.print_lag_days
        && next.mark_limit_bps <= cur.mark_limit_bps
        && (cur.halt_jump_bps == 0 || (next.halt_jump_bps > 0 && next.halt_jump_bps <= cur.halt_jump_bps))
}

// ---------------------------------------------------------------------------------------------
// Working set
// ---------------------------------------------------------------------------------------------

/// Reads accounts and positions on first use, mutates copies, commits all at once.
pub struct Tx<'s, S: Store> {
    pub s: &'s mut S,
    accounts: BTreeMap<String, AccountState>,
    positions: BTreeMap<(String, String), Position>,
    markets: BTreeMap<String, MarketState>,
    orders: BTreeMap<[u8; 32], OrderState>,
    pub events: Vec<CEvent>,
    fee_delta: u128,
}

impl<'s, S: Store> Tx<'s, S> {
    pub fn new(s: &'s mut S) -> Self {
        Tx {
            s,
            accounts: BTreeMap::new(),
            positions: BTreeMap::new(),
            markets: BTreeMap::new(),
            orders: BTreeMap::new(),
            events: vec![],
            fee_delta: 0,
        }
    }
    pub fn acct(&mut self, a: &str) -> &mut AccountState {
        if !self.accounts.contains_key(a) {
            let v = self.s.account(a);
            self.accounts.insert(a.to_string(), v);
        }
        self.accounts.get_mut(a).unwrap()
    }
    pub fn pos(&mut self, a: &str, m: &str) -> &mut Position {
        let k = (a.to_string(), m.to_string());
        if !self.positions.contains_key(&k) {
            let v = self.s.position(a, m);
            self.positions.insert(k.clone(), v);
        }
        self.positions.get_mut(&k).unwrap()
    }
    pub fn market(&mut self, id: &str) -> Option<&mut MarketState> {
        if !self.markets.contains_key(id) {
            let v = self.s.market(id)?;
            self.markets.insert(id.to_string(), v);
        }
        self.markets.get_mut(id)
    }
    fn order_state(&mut self, h: &[u8; 32]) -> Option<&mut OrderState> {
        if !self.orders.contains_key(h) {
            let v = self.s.order(h)?;
            self.orders.insert(*h, v);
        }
        self.orders.get_mut(h)
    }
    fn set_order(&mut self, h: [u8; 32], o: OrderState) {
        self.orders.insert(h, o);
    }
    pub fn commit(self) -> Vec<CEvent> {
        let Tx {
            s,
            accounts,
            positions,
            markets,
            orders,
            mut events,
            fee_delta,
        } = self;
        for ((a, m), p) in positions {
            events.push(CEvent::Position {
                account: a.clone(),
                market: m.clone(),
                size: p.size,
                margin: p.margin,
                funding_index: p.funding_index,
            });
            s.put_position(&a, &m, p);
        }
        for (a, st) in accounts {
            events.push(CEvent::Free {
                account: a.clone(),
                free: st.free,
            });
            s.put_account(&a, st);
        }
        for (id, m) in markets {
            s.put_market(&id, m);
        }
        for (h, o) in orders {
            s.put_order(&h, o);
        }
        s.globals_mut().fee_balance += fee_delta;
        events
    }
}

// ---------------------------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------------------------

/// The batch's order table with per-order verification memoized: a resting order filled many
/// times in one batch has its signature checked once.
pub struct Orders<'a> {
    pub list: &'a [SignedOrder],
    hashes: Vec<Option<[u8; 32]>>,
    sig_ok: Vec<Option<bool>>,
    /// Skip the ed25519 check (the gateway already verified it). Key registration and every other
    /// check still run. Never set on-chain.
    pub trust_signatures: bool,
}

impl<'a> Orders<'a> {
    pub fn new(list: &'a [SignedOrder], trust_signatures: bool) -> Self {
        Orders {
            list,
            hashes: vec![None; list.len()],
            sig_ok: vec![None; list.len()],
            trust_signatures,
        }
    }
    pub fn get(&self, i: u32) -> Result<&'a SignedOrder, Reject> {
        self.list.get(i as usize).ok_or(Reject::BadOrderIndex)
    }
    fn hash<C: Crypto>(&mut self, c: &C, i: u32) -> [u8; 32] {
        let i = i as usize;
        *self.hashes[i].get_or_insert_with(|| c.sha256(&self.list[i].message()))
    }
    fn sig<C: Crypto>(&mut self, c: &C, i: u32) -> bool {
        if self.trust_signatures {
            return true;
        }
        let i = i as usize;
        let o = &self.list[i];
        *self.sig_ok[i].get_or_insert_with(|| c.ed25519_verify(&o.signature.0, &o.message(), &o.public_key))
    }
}

pub fn order_hash<C: Crypto>(c: &C, o: &SignedOrder) -> [u8; 32] {
    c.sha256(&o.message())
}

/// Validate a trading key for `account` at `now`.
fn check_key<S: Store>(s: &mut S, pk: &[u8; 32], account: &str, now: Now, need_withdraw: bool) -> Result<(), Reject> {
    let g = s.globals().settle_grace_ns;
    let k = s.key(pk).ok_or(Reject::KeyNotRegistered)?;
    if k.account != account {
        return Err(Reject::KeyNotRegistered);
    }
    if k.expires_ns != 0 && !now.before(k.expires_ns, g) {
        return Err(Reject::KeyExpired);
    }
    if need_withdraw && !k.can_withdraw {
        return Err(Reject::KeyScope);
    }
    Ok(())
}

/// Every check on one order that doesn't depend on the counterparty. Returns its hash.
fn check_order<S: Store, C: Crypto>(
    tx: &mut Tx<S>,
    c: &C,
    orders: &mut Orders,
    i: u32,
    m: &MarketState,
    now: Now,
    qty: u64,
) -> Result<[u8; 32], Reject> {
    let o = orders.get(i)?;
    let g = tx.s.globals().clone();
    if !o.well_formed() {
        return Err(Reject::Malformed);
    }
    if o.chain_id != g.chain_id {
        return Err(Reject::WrongChain);
    }
    if o.clearing_contract != g.contract_id {
        return Err(Reject::WrongContract);
    }
    if o.market != m.params.market_id {
        return Err(Reject::WrongMarket);
    }
    if o.qty < m.params.min_qty || o.qty > m.params.max_qty {
        return Err(Reject::InvalidQty);
    }
    if o.price > m.params.max_price {
        return Err(Reject::InvalidPrice);
    }
    check_key(tx.s, &o.public_key, &o.account, now, false)?;
    if !orders.sig(c, i) {
        return Err(Reject::BadSignature);
    }
    if o.policy_hash != m.policy_hash {
        return Err(Reject::PolicyMismatch);
    }
    if o.nonce < tx.acct(&o.account).min_nonce {
        return Err(Reject::NonceBelowMin);
    }
    if !now.before(o.expiry_ns, g.settle_grace_ns) {
        return Err(Reject::Expired);
    }
    let h = orders.hash(c, i);
    match tx.order_state(&h) {
        Some(st) => {
            if st.canceled {
                return Err(Reject::OrderCanceled);
            }
            if st.filled.checked_add(qty).is_none_or(|f| f > o.qty) {
                return Err(Reject::Overfill);
            }
            st.filled += qty;
        }
        None => {
            if o.expiry_ns > now.op.saturating_add(g.max_order_ttl_ns) {
                return Err(Reject::ExpiryTooFar);
            }
            if qty > o.qty {
                return Err(Reject::Overfill);
            }
            tx.set_order(
                h,
                OrderState {
                    account: o.account.clone(),
                    filled: qty,
                    canceled: false,
                    expiry_ns: o.expiry_ns,
                },
            );
        }
    }
    Ok(h)
}

fn check_reduce_only(o: &SignedOrder, p: &Position, qty: u64) -> Result<(), Reject> {
    if o.reduce_only {
        let reduces = (p.size > 0 && o.side == Side::Sell) || (p.size < 0 && o.side == Side::Buy);
        if !reduces || qty > p.size.unsigned_abs() {
            return Err(Reject::ReduceOnlyViolated);
        }
    }
    Ok(())
}

/// Apply one side of a trade to an isolated position: cash, fee, size, then the margin rule.
#[allow(clippy::too_many_arguments)]
fn trade_side<S: Store>(
    tx: &mut Tx<S>,
    m: &MarketState,
    account: &str,
    delta: i64,
    price: u64,
    fee: u128,
    mark: u64,
    close_only: bool,
) -> Result<(), Reject> {
    let mid = m.params.market_id.clone();
    let mut p = tx.pos(account, &mid).clone();
    touch(m, &mut p);
    let old = p.size;
    let new = old.checked_add(delta).ok_or(Reject::Overflow)?;
    let increasing = new.unsigned_abs() > old.unsigned_abs() || (old != 0 && new != 0 && (old > 0) != (new > 0));
    if close_only && increasing {
        return Err(Reject::CloseOnly);
    }
    let cash = delta as i128 * price as i128 * m.params.tick_value as i128;
    p.margin = p.margin - cash - fee as i128;
    p.size = new;
    if increasing {
        if new.unsigned_abs() > m.params.max_position {
            return Err(Reject::PositionLimit);
        }
        let eq = equity(m, &p, mark);
        let req = initial_margin(m, new.unsigned_abs(), mark) as i128;
        if eq < req {
            let need = (req - eq) as u128;
            let a = tx.acct(account);
            if a.free < need {
                return Err(Reject::InsufficientMargin);
            }
            a.free -= need;
            p.margin += need as i128;
        }
    } else {
        if equity(m, &p, mark) < 0 {
            return Err(Reject::NegativeEquity);
        }
        if new == 0 {
            let back = p.margin as u128;
            p.margin = 0;
            tx.acct(account).free += back;
        }
    }
    *tx.pos(account, &mid) = p;
    Ok(())
}

fn update_oi(m: &mut MarketState, changes: &[(i64, i64)]) -> Result<(), Reject> {
    update_oi_lots(m, changes)
}

fn update_oi_lots(m: &mut MarketState, changes: &[(i64, i64)]) -> Result<(), Reject> {
    let before = m.open_interest as i128;
    let mut oi = before;
    for (old, new) in changes {
        oi += (*new).max(0) as i128 - (*old).max(0) as i128;
    }
    if oi < 0 {
        return Err(Reject::Overflow);
    }
    if oi > before && oi > m.params.max_open_interest as i128 {
        return Err(Reject::OpenInterestCap);
    }
    if oi > before
        && m.params.max_open_interest_notional > 0
        && m.notional(oi as u64, m.mark) > m.params.max_open_interest_notional
    {
        return Err(Reject::OpenInterestCap);
    }
    m.open_interest = oi as u64;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Fill
// ---------------------------------------------------------------------------------------------

/// Settle one matched trade between a resting maker order and an incoming taker order, both
/// signed by their owners' registered trading keys.
pub fn fill<S: Store, C: Crypto>(
    s: &mut S,
    c: &C,
    orders: &mut Orders,
    now: Now,
    maker_i: u32,
    taker_i: u32,
    qty: u64,
    price: u64,
) -> Res<Vec<CEvent>> {
    let mo = orders.get(maker_i)?;
    let to = orders.get(taker_i)?;
    let grace = s.globals().settle_grace_ns;
    let mut tx = Tx::new(s);
    let mut m = tx.market(&mo.market).ok_or(Reject::UnknownMarket)?.clone();
    if m.params.market_type != MarketType::Perpetual {
        return Err(Reject::MarketTypeUnsupported.into());
    }
    let st = effective_status(&m, now, grace);
    if !matches!(st, MarketStatus::Active | MarketStatus::CloseOnly) {
        return Err(Reject::MarketNotTrading.into());
    }
    let close_only = st == MarketStatus::CloseOnly;
    let mark = m.mark;
    if mark == 0 {
        return Err(Reject::NoMark.into());
    }
    if qty == 0 {
        return Err(Reject::InvalidQty.into());
    }
    let mh = check_order(&mut tx, c, orders, maker_i, &m, now, qty).map_err(maker)?;
    let th = check_order(&mut tx, c, orders, taker_i, &m, now, qty).map_err(taker)?;
    if mo.account == to.account {
        return Err(taker(Reject::SameAccount));
    }
    if mo.side == to.side {
        return Err(taker(Reject::SideMismatch));
    }
    if mo.order_type != OrderType::Limit || mo.tif != TimeInForce::Gtc {
        return Err(maker(Reject::MakerNotResting));
    }
    if to.post_only {
        return Err(taker(Reject::PostOnlyTaker));
    }
    if price != mo.price {
        return Err(maker(Reject::PriceOutsideLimit));
    }
    let taker_ok = match to.side {
        Side::Buy => price <= to.price,
        Side::Sell => price >= to.price,
    };
    if !taker_ok {
        return Err(taker(Reject::PriceOutsideLimit));
    }
    if (price.abs_diff(mark) as u128) * 10_000 > mark as u128 * m.params.price_band_bps as u128 {
        return Err(taker(Reject::PriceOutOfBand));
    }
    let n = m.notional(qty, price);
    let (mfee, tfee) = (fee(n, m.params.maker_fee_bps), fee(n, m.params.taker_fee_bps));
    if m.params.maker_fee_bps > mo.max_fee_bps {
        return Err(maker(Reject::FeeAboveMax));
    }
    if m.params.taker_fee_bps > to.max_fee_bps {
        return Err(taker(Reject::FeeAboveMax));
    }
    let mid = m.params.market_id.clone();
    let (m_old, t_old) = (tx.pos(&mo.account, &mid).size, tx.pos(&to.account, &mid).size);
    check_reduce_only(mo, &tx.pos(&mo.account, &mid).clone(), qty).map_err(maker)?;
    check_reduce_only(to, &tx.pos(&to.account, &mid).clone(), qty).map_err(taker)?;
    let q = qty as i64;
    trade_side(&mut tx, &m, &mo.account, mo.side.sign() * q, price, mfee, mark, close_only).map_err(maker)?;
    trade_side(&mut tx, &m, &to.account, to.side.sign() * q, price, tfee, mark, close_only).map_err(taker)?;
    let (m_new, t_new) = (tx.pos(&mo.account, &mid).size, tx.pos(&to.account, &mid).size);
    update_oi(&mut m, &[(m_old, m_new), (t_old, t_new)]).map_err(taker)?;
    if st == MarketStatus::Active {
        m.fund_px_qty += price as u128 * qty as u128;
        m.fund_qty += qty as u128;
    }
    *tx.market(&mid).unwrap() = m;
    tx.fee_delta += mfee + tfee;
    tx.events.push(CEvent::Fill {
        market: mid,
        maker: mo.account.clone(),
        taker: to.account.clone(),
        maker_order: mh,
        taker_order: th,
        taker_side: to.side,
        price,
        qty,
        maker_fee: mfee,
        taker_fee: tfee,
    });
    Ok(tx.commit())
}

// ---------------------------------------------------------------------------------------------
// Liquidation
// ---------------------------------------------------------------------------------------------

/// Is the position below maintenance margin at a fresh mark?
pub fn liquidatable<S: Store>(s: &mut S, account: &str, market: &str, now: Now) -> bool {
    let grace = s.globals().settle_grace_ns;
    let Some(m) = s.market(market) else { return false };
    if !matches!(m.status, MarketStatus::Active | MarketStatus::CloseOnly) || !mark_fresh(&m, now, grace) {
        return false;
    }
    let p = s.position(account, market);
    p.size != 0 && equity(&m, &p, m.mark) < maintenance_margin(&m, p.size.unsigned_abs(), m.mark) as i128
}

/// Could the insurance fund take `qty` lots on `side` at the mark right now?
pub fn insurance_can_absorb<S: Store>(s: &mut S, market: &str, side: Side, qty: u64) -> bool {
    let ins = s.globals().insurance_account.clone();
    let Some(m) = s.market(market) else { return false };
    let mut tx = Tx::new(s);
    trade_side(&mut tx, &m, &ins, side.sign() * qty as i64, m.mark, 0, m.mark, false).is_ok()
}

/// Liquidate one isolated position below maintenance margin.
///
/// 1. `fills`: resting orders that take the position, each within `liq_slippage_bps` of the mark,
///    each a real signed order checked like any maker.
/// 2. Any remainder goes to the insurance fund at the mark, or, only if the fund can't margin it,
///    to the `adl` targets (holders of the opposite side, closed at the mark).
/// 3. A liquidation fee, capped at what equity remains, goes to the insurance fund.
/// 4. A deficit (equity below zero) is paid by the insurance fund's free collateral, then
///    socialized pro rata by size over every holder of the profiting side. The op must list all of
///    them (sorted, checked against open interest). Whatever still can't be collected is bad
///    debt and halts the market: there is no protocol backstop beyond this.
#[allow(clippy::too_many_arguments)]
pub fn liquidate<S: Store, C: Crypto>(
    s: &mut S,
    c: &C,
    orders: &mut Orders,
    now: Now,
    account: &str,
    market: &str,
    fills: &[(u32, u64)],
    adl: &[(String, u64)],
    socialize: &[String],
) -> Res<Vec<CEvent>> {
    let g = s.globals().clone();
    let mut tx = Tx::new(s);
    let mut m = tx.market(market).ok_or(Reject::UnknownMarket)?.clone();
    if !matches!(m.status, MarketStatus::Active | MarketStatus::CloseOnly) || !mark_fresh(&m, now, g.settle_grace_ns) {
        return Err(Reject::MarketNotTrading.into());
    }
    let close_only = effective_status(&m, now, g.settle_grace_ns) == MarketStatus::CloseOnly;
    let mark = m.mark;
    let tv = m.params.tick_value as i128;
    let mut p = tx.pos(account, market).clone();
    touch(&m, &mut p);
    let size0 = p.size;
    if size0 == 0 {
        return Err(Reject::NoPosition.into());
    }
    let eq0 = equity(&m, &p, mark);
    let mm0 = maintenance_margin(&m, size0.unsigned_abs(), mark) as i128;
    if eq0 >= mm0 {
        return Err(Reject::NotLiquidatable.into());
    }
    tx.events.push(CEvent::Liquidated {
        account: account.into(),
        market: market.into(),
        size: size0,
        equity: eq0,
        maintenance: mm0,
    });
    let liq_side = if size0 > 0 { Side::Sell } else { Side::Buy };
    let slip = mark as u128 * m.params.liq_slippage_bps as u128 / 10_000;
    let mut changes = vec![];
    let mut closed = 0u64;
    for (i, q) in fills {
        let o = orders.get(*i).map_err(maker)?;
        if *q == 0 || closed + q > size0.unsigned_abs() {
            return Err(maker(Reject::InvalidQty));
        }
        if o.side != liq_side.opposite() || o.order_type != OrderType::Limit || o.tif != TimeInForce::Gtc {
            return Err(maker(Reject::MakerNotResting));
        }
        if o.account == account {
            return Err(maker(Reject::SameAccount));
        }
        let px_ok = match liq_side {
            Side::Sell => o.price as u128 + slip >= mark as u128,
            Side::Buy => o.price as u128 <= mark as u128 + slip,
        };
        if !px_ok {
            return Err(maker(Reject::LiquidationPrice));
        }
        check_order(&mut tx, c, orders, *i, &m, now, *q).map_err(maker)?;
        if m.params.maker_fee_bps > o.max_fee_bps {
            return Err(maker(Reject::FeeAboveMax));
        }
        let mpos = tx.pos(&o.account, market).clone();
        check_reduce_only(o, &mpos, *q).map_err(maker)?;
        let mfee = fee(m.notional(*q, o.price), m.params.maker_fee_bps);
        trade_side(&mut tx, &m, &o.account, o.side.sign() * *q as i64, o.price, mfee, mark, close_only).map_err(maker)?;
        changes.push((mpos.size, tx.pos(&o.account, market).size));
        tx.fee_delta += mfee;
        let d = liq_side.sign() * *q as i64;
        p.margin -= d as i128 * o.price as i128 * tv;
        p.size += d;
        closed += q;
        tx.events.push(CEvent::Fill {
            market: market.into(),
            maker: o.account.clone(),
            taker: account.into(),
            maker_order: orders.hash(c, *i),
            taker_order: [0; 32],
            taker_side: liq_side,
            price: o.price,
            qty: *q,
            maker_fee: mfee,
            taker_fee: 0,
        });
    }
    let rem = size0.unsigned_abs() - closed;
    if rem > 0 {
        let take_side = liq_side.opposite();
        if account == g.insurance_account && adl.is_empty() {
            return Err(Reject::InsuranceCannotAbsorb.into());
        }
        if adl.is_empty() {
            let ins = g.insurance_account.clone();
            let before = tx.pos(&ins, market).size;
            trade_side(&mut tx, &m, &ins, take_side.sign() * rem as i64, mark, 0, mark, false)
                .map_err(|_| Failure::from(Reject::InsuranceCannotAbsorb))?;
            changes.push((before, tx.pos(&ins, market).size));
            tx.events.push(CEvent::InsuranceTakeover {
                account: account.into(),
                market: market.into(),
                size: take_side.sign() * rem as i64,
                price: mark,
            });
        } else {
            // ADL only when the fund genuinely can't take the position.
            {
                let mut probe = Tx::new(&mut *tx.s);
                if trade_side(&mut probe, &m, &g.insurance_account, take_side.sign() * rem as i64, mark, 0, mark, false).is_ok() {
                    return Err(Reject::InsuranceCouldAbsorb.into());
                }
            }
            let mut sum = 0u64;
            let mut seen = std::collections::BTreeSet::new();
            for (t, q) in adl {
                if t == account || !seen.insert(t.clone()) || *q == 0 {
                    return Err(Reject::AdlInvalid.into());
                }
                let tp = tx.pos(t, market).clone();
                // Targets hold the opposite side and only reduce.
                if tp.size == 0 || (tp.size > 0) == (size0 > 0) || *q > tp.size.unsigned_abs() {
                    return Err(Reject::AdlInvalid.into());
                }
                trade_side(&mut tx, &m, t, take_side.sign() * *q as i64, mark, 0, mark, false)
                    .map_err(|_| Failure::from(Reject::AdlInvalid))?;
                changes.push((tp.size, tx.pos(t, market).size));
                sum += q;
                tx.events.push(CEvent::Adl {
                    target: t.clone(),
                    market: market.into(),
                    qty: *q,
                    price: mark,
                });
            }
            if sum != rem {
                return Err(Reject::AdlInvalid.into());
            }
        }
        let d = liq_side.sign() * rem as i64;
        p.margin -= d as i128 * mark as i128 * tv;
        p.size += d;
    }
    debug_assert_eq!(p.size, 0);
    changes.push((size0, 0));
    update_oi(&mut m, &changes)?;

    // Liquidation fee, capped at remaining equity.
    let full = ceil_bps(m.notional(size0.unsigned_abs(), mark), m.params.liq_fee_bps as u32) as i128;
    let liq_fee = full.min(p.margin.max(0));
    p.margin -= liq_fee;
    let ins = g.insurance_account.clone();
    tx.acct(&ins).free += liq_fee as u128;
    tx.events.push(CEvent::LiquidationFee {
        account: account.into(),
        market: market.into(),
        fee: liq_fee as u128,
    });

    // Deficit waterfall.
    if p.margin < 0 {
        let deficit = (-p.margin) as u128;
        p.margin = 0;
        let ins_free = tx.acct(&ins).free;
        let paid = deficit.min(ins_free);
        tx.acct(&ins).free -= paid;
        let rest = deficit - paid;
        let mut socialized = 0u128;
        if rest > 0 {
            // Every holder of the profiting side, sorted, distinct; sizes sum to open interest.
            let profit_long = size0 < 0;
            let mut total = 0u128;
            let mut sizes = vec![];
            for (k, a) in socialize.iter().enumerate() {
                if k > 0 && socialize[k - 1] >= *a {
                    return Err(Reject::SocializeIncomplete.into());
                }
                let tp = tx.pos(a, market).clone();
                if tp.size == 0 || (tp.size > 0) != profit_long {
                    return Err(Reject::SocializeIncomplete.into());
                }
                total += tp.size.unsigned_abs() as u128;
                sizes.push(tp.size.unsigned_abs() as u128);
            }
            if total != m.open_interest as u128 {
                return Err(Reject::SocializeIncomplete.into());
            }
            if total > 0 {
                let base: Vec<u128> = sizes.iter().map(|z| rest * z / total).collect();
                let mut extra = rest - base.iter().sum::<u128>();
                for (k, a) in socialize.iter().enumerate() {
                    let mut share = base[k];
                    if extra > 0 {
                        share += 1;
                        extra -= 1;
                    }
                    let mut tp = tx.pos(a, market).clone();
                    touch(&m, &mut tp);
                    let cap = equity(&m, &tp, mark).max(0) as u128;
                    let take = share.min(cap);
                    tp.margin -= take as i128;
                    socialized += take;
                    *tx.pos(a, market) = tp;
                }
            }
        }
        let bad = rest - socialized;
        if bad > 0 {
            m.bad_debt += bad;
            m.status = MarketStatus::Halted;
            tx.events.push(CEvent::MarketHalted {
                market: market.into(),
                reason: HaltReason::BadDebt,
            });
        }
        tx.events.push(CEvent::Deficit {
            account: account.into(),
            market: market.into(),
            deficit,
            insurance_paid: paid,
            socialized,
            bad_debt: bad,
        });
    }
    // Close: what's left goes back to free collateral.
    let back = p.margin as u128;
    p.margin = 0;
    tx.acct(account).free += back;
    *tx.pos(account, market) = p;
    *tx.market(market).unwrap() = m;
    Ok(tx.commit())
}

// ---------------------------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------------------------

/// Accept a reporter-signed print for one market. Signatures from at least `quorum` distinct
/// allowlisted reporters are required. Prints only move forward: a later source window, or a
/// higher revision of the same window that names the current print as its predecessor.
pub fn oracle<S: Store, C: Crypto>(s: &mut S, c: &C, now: Now, market: &str, bytes: &[u8], sigs: &[([u8; 32], wire::Sig64)]) -> Res<Vec<CEvent>> {
    let g = s.globals().clone();
    let mut tx = Tx::new(s);
    let mut m = tx.market(market).ok_or(Reject::UnknownMarket)?.clone();
    let p = OraclePrint::decode(bytes).map_err(|_| Reject::OracleDecode)?;
    if p.index_id != m.params.index_id || p.version != m.params.index_version {
        return Err(Reject::OracleIndexMismatch.into());
    }
    if p.value_scale != m.params.value_scale {
        return Err(Reject::OracleScale.into());
    }
    let mut signers = std::collections::BTreeSet::new();
    for (pk, sig) in sigs {
        if g.reporters.contains(pk) && !signers.contains(pk) && c.ed25519_verify(&sig.0, bytes, pk) {
            signers.insert(*pk);
        }
    }
    if g.quorum == 0 || signers.len() < g.quorum as usize {
        return Err(Reject::OracleQuorum.into());
    }
    let ms = |x: i64| (x.max(0) as u64).saturating_mul(1_000_000);
    if ms(p.published_at_ms) > now.op.saturating_add(PRINT_FUTURE_SKEW_NS) {
        return Err(Reject::OracleFuture.into());
    }
    if !now.before(ms(p.valid_until_ms), g.settle_grace_ns) {
        return Err(Reject::OracleExpired.into());
    }
    let hash = c.sha256(bytes);
    let newer = match &m.print {
        None => true,
        Some(cur) => {
            p.source_end_ms > cur.source_end_ms
                || (p.source_end_ms == cur.source_end_ms && p.revision > cur.revision && p.prev_print_hash == cur.hash)
        }
    };
    if !newer {
        return Err(Reject::OracleNotNewer.into());
    }
    if p.usable() {
        let t = p.value / m.params.index_units_per_tick;
        if t == 0 || t > m.params.max_price {
            m.status = m.status.max(MarketStatus::Halted);
            tx.events.push(CEvent::MarketHalted {
                market: market.into(),
                reason: HaltReason::IndexOutOfRange,
            });
        } else if m.mark > 0
            && m.params.halt_jump_bps > 0
            && (t.abs_diff(m.mark) as u128) * 10_000 > m.mark as u128 * m.params.halt_jump_bps as u128
        {
            m.status = m.status.max(MarketStatus::Halted);
            tx.events.push(CEvent::MarketHalted {
                market: market.into(),
                reason: HaltReason::IndexJump,
            });
        } else {
            m.print_move_flag = print_move_exceeds(m.index_value, p.value, m.params.print_move_close_only_bps);
            m.index_value = p.value;
            m.index_ticks = t;
            m.mark = limit_move(m.mark, t, m.params.mark_limit_bps);
        }
    }
    m.print = Some(PrintInfo {
        hash,
        value: p.value,
        status: p.status,
        data_status: p.data_status,
        source_start_ms: p.source_start_ms,
        source_end_ms: p.source_end_ms,
        published_at_ms: p.published_at_ms,
        valid_until_ms: p.valid_until_ms,
        revision: p.revision,
    });
    tx.events.push(CEvent::OracleAccepted {
        market: market.into(),
        print_hash: hash,
        mark: m.mark,
        usable: p.usable(),
    });
    *tx.market(market).unwrap() = m;
    Ok(tx.commit())
}

// ---------------------------------------------------------------------------------------------
// Funding
// ---------------------------------------------------------------------------------------------

/// Apply funding for the interval since the last application. The rate comes from the volume
/// weighted price of verified fills since then against the mark (the index):
/// `rate = clamp((vwap − mark) / mark / funding_divisor, ±max_funding_ppm)`. Each long lot pays
/// `rate × mark × tick_value` (truncated to an integer), each short lot receives the same, so the
/// payments net to zero exactly. Nothing accrues while the market isn't ACTIVE with a fresh mark,
/// or when nothing traded.
pub fn funding<S: Store>(s: &mut S, now: Now, market: &str) -> Res<Vec<CEvent>> {
    let grace = s.globals().settle_grace_ns;
    let mut m = s.market(market).ok_or(Reject::UnknownMarket)?;
    if m.params.funding_interval_sec == 0 {
        return Err(Reject::FundingDisabled.into());
    }
    if now.op < m.last_funding_ns.saturating_add(m.params.funding_interval_sec * NS_PER_SEC) {
        return Err(Reject::FundingNotDue.into());
    }
    let active = effective_status(&m, now, grace) == MarketStatus::Active && mark_fresh(&m, now, grace);
    let (rate, pay) = if active && m.fund_qty > 0 && m.mark > 0 {
        let mark = m.mark as i128;
        let vwap_e6 = (m.fund_px_qty * 1_000_000 / m.fund_qty) as i128;
        let premium_ppm = (vwap_e6 - mark * 1_000_000) / mark;
        let cap = m.params.max_funding_ppm as i128;
        let rate = (premium_ppm / m.params.funding_divisor as i128).clamp(-cap, cap);
        (rate, rate * mark * m.params.tick_value as i128 / 1_000_000)
    } else {
        (0, 0)
    };
    m.cum_funding += pay;
    m.last_funding_ns = now.op;
    m.last_funding_rate_ppm = rate as i64;
    m.fund_px_qty = 0;
    m.fund_qty = 0;
    s.put_market(market, m);
    Ok(vec![CEvent::Funding {
        market: market.into(),
        rate_ppm: rate as i64,
        pay_per_lot: pay,
    }])
}

// ---------------------------------------------------------------------------------------------
// Account actions
// ---------------------------------------------------------------------------------------------

pub fn credit<S: Store>(s: &mut S, deposit_id: u64) -> Res<Vec<CEvent>> {
    let d = s.deposit(deposit_id).ok_or(Reject::UnknownDeposit)?;
    s.remove_deposit(deposit_id);
    let mut tx = Tx::new(s);
    tx.acct(&d.account).free += d.amount;
    tx.s.globals_mut().total_collateral += d.amount;
    tx.events.push(CEvent::Credited {
        deposit_id,
        account: d.account,
        amount: d.amount,
    });
    Ok(tx.commit())
}

#[allow(clippy::too_many_arguments)]
pub fn withdraw<S: Store, C: Crypto>(
    s: &mut S,
    c: &C,
    now: Now,
    account: &str,
    amount: u128,
    nonce: u64,
    pk: &[u8; 32],
    sig: &[u8; 64],
) -> Res<Vec<CEvent>> {
    let g = s.globals().clone();
    check_key(s, pk, account, now, true)?;
    let msg = wire::withdraw_message(&g.chain_id, &g.contract_id, account, &g.collateral, amount, nonce);
    if !c.ed25519_verify(sig, &msg, pk) {
        return Err(Reject::BadSignature.into());
    }
    if amount == 0 {
        return Err(Reject::ZeroAmount.into());
    }
    let mut tx = Tx::new(s);
    let a = tx.acct(account);
    if nonce <= a.action_nonce {
        return Err(Reject::NonceUsed.into());
    }
    if a.free < amount {
        return Err(Reject::InsufficientFree.into());
    }
    a.free -= amount;
    a.action_nonce = nonce;
    tx.s.globals_mut().total_collateral -= amount;
    tx.events.push(CEvent::Payout {
        account: account.into(),
        amount,
        kind: PayoutKind::Withdraw,
    });
    Ok(tx.commit())
}

#[allow(clippy::too_many_arguments)]
pub fn adjust_margin<S: Store, C: Crypto>(
    s: &mut S,
    c: &C,
    now: Now,
    account: &str,
    market: &str,
    delta: i128,
    nonce: u64,
    pk: &[u8; 32],
    sig: &[u8; 64],
) -> Res<Vec<CEvent>> {
    let g = s.globals().clone();
    check_key(s, pk, account, now, false)?;
    let msg = wire::margin_message(&g.chain_id, &g.contract_id, account, market, delta, nonce);
    if !c.ed25519_verify(sig, &msg, pk) {
        return Err(Reject::BadSignature.into());
    }
    if delta == 0 {
        return Err(Reject::ZeroAmount.into());
    }
    let mut tx = Tx::new(s);
    let m = tx.market(market).ok_or(Reject::UnknownMarket)?.clone();
    if nonce <= tx.acct(account).action_nonce {
        return Err(Reject::NonceUsed.into());
    }
    tx.acct(account).action_nonce = nonce;
    let mut p = tx.pos(account, market).clone();
    if p.size == 0 {
        return Err(Reject::NoPosition.into());
    }
    touch(&m, &mut p);
    if delta > 0 {
        let a = tx.acct(account);
        if a.free < delta as u128 {
            return Err(Reject::InsufficientFree.into());
        }
        a.free -= delta as u128;
        p.margin += delta;
    } else {
        let out = delta.unsigned_abs();
        if m.mark == 0 || !mark_fresh(&m, now, g.settle_grace_ns) {
            return Err(Reject::NoMark.into());
        }
        let left = equity(&m, &p, m.mark) - out as i128;
        if left < initial_margin(&m, p.size.unsigned_abs(), m.mark) as i128 {
            return Err(Reject::InsufficientMargin.into());
        }
        p.margin -= out as i128;
        tx.acct(account).free += out;
    }
    *tx.pos(account, market) = p;
    Ok(tx.commit())
}

/// Pay a forced-withdrawal request: `min(requested, free)`. Exact on both sides because deposits
/// only become free collateral through ops in the same sequence.
pub fn forced<S: Store>(s: &mut S, request_id: u64) -> Res<Vec<CEvent>> {
    let mut r = s.forced(request_id).ok_or(Reject::UnknownForced)?;
    if r.done {
        return Err(Reject::ForcedDone.into());
    }
    let mut tx = Tx::new(s);
    let a = tx.acct(&r.account);
    let paid = r.amount.min(a.free);
    a.free -= paid;
    tx.s.globals_mut().total_collateral -= paid;
    r.done = true;
    r.paid = paid;
    let acct = r.account.clone();
    tx.s.put_forced(request_id, r);
    if paid > 0 {
        tx.events.push(CEvent::Payout {
            account: acct,
            amount: paid,
            kind: PayoutKind::Forced,
        });
    }
    Ok(tx.commit())
}

/// Mark an order canceled on-chain. `check` = verify the trading-key signature (relayed cancel);
/// the contract's direct wallet path passes `None` (the caller is the account itself).
#[allow(clippy::too_many_arguments)]
pub fn cancel<S: Store, C: Crypto>(
    s: &mut S,
    c: &C,
    account: &str,
    order_hash: &[u8; 32],
    expiry_ns: u64,
    sig: Option<(&[u8; 32], &[u8; 64])>,
) -> Res<Vec<CEvent>> {
    let g = s.globals().clone();
    if let Some((pk, sg)) = sig {
        // Any key the account registered may cancel, even an expired one: canceling only restricts.
        if s.key(pk).is_none_or(|k| k.account != account) {
            return Err(Reject::KeyNotRegistered.into());
        }
        let msg = wire::cancel_message(&g.chain_id, &g.contract_id, account, order_hash, expiry_ns);
        if !c.ed25519_verify(sg, &msg, pk) {
            return Err(Reject::BadSignature.into());
        }
    }
    let mut o = s.order(order_hash).unwrap_or(OrderState {
        account: account.into(),
        filled: 0,
        canceled: false,
        expiry_ns,
    });
    if o.account != account {
        return Err(Reject::KeyNotRegistered.into());
    }
    o.canceled = true;
    o.expiry_ns = o.expiry_ns.max(expiry_ns);
    s.put_order(order_hash, o);
    Ok(vec![CEvent::OrderCanceled {
        account: account.into(),
        order_hash: *order_hash,
    }])
}

pub fn set_min_nonce<S: Store, C: Crypto>(s: &mut S, c: &C, account: &str, min_nonce: u64, sig: Option<(&[u8; 32], &[u8; 64])>) -> Res<Vec<CEvent>> {
    let g = s.globals().clone();
    if let Some((pk, sg)) = sig {
        if s.key(pk).is_none_or(|k| k.account != account) {
            return Err(Reject::KeyNotRegistered.into());
        }
        let msg = wire::min_nonce_message(&g.chain_id, &g.contract_id, account, min_nonce);
        if !c.ed25519_verify(sg, &msg, pk) {
            return Err(Reject::BadSignature.into());
        }
    }
    let mut a = s.account(account);
    if min_nonce <= a.min_nonce {
        return Err(Reject::NonceUsed.into());
    }
    a.min_nonce = min_nonce;
    s.put_account(account, a);
    Ok(vec![CEvent::MinNonce {
        account: account.into(),
        min_nonce,
    }])
}
