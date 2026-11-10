//! Clearing mode: perpetual markets whose positions and collateral live on the NEAR clearing
//! contract (the NEAR v1 path).
//!
//! The engine keeps two copies of the clearing ledger, both driven by the shared clearing core:
//!
//! - `confirmed`: exactly the contract's state. It changes only through chain events, in chain
//!   order: deposits, key changes, cancels and nonce floors filed from wallets, admin changes, and
//!   each settled batch replayed with the contract's own results and block time.
//! - `spec`: `confirmed` plus every op the engine produced that hasn't settled yet. Order
//!   admission and matching run against it, so a fill the engine accepts is one the contract will
//!   accept unless something changed on-chain in between (a key revoked, an order canceled from a
//!   wallet). When that happens `spec` is rebuilt from `confirmed`, and fills that no longer hold
//!   are reported as `FillReverted`: a fill is final only when its batch settles.
//!
//! Matching is the engine's ordinary price-time book. Each candidate fill is first applied to
//! `spec` with the core's `fill`, which verifies both signed orders and both margins; a resting
//! order that fails is dropped from the book, an incoming order that fails stops matching.

use crate::engine::Engine;
use crate::types::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use zfx_perps_clearing as core;
use zfx_perps_clearing::batch::{self as cbatch, Batch, Op, OpAt};
use zfx_perps_clearing::ledger::{self, CEvent, Failure, Now, Orders, Party};
use zfx_perps_clearing::plan::{Candidate, plan_liquidation};
use zfx_perps_clearing::store::{Crypto, MemStore, NativeCrypto, Store};
use zfx_perps_clearing::wire::{self, SignedOrder};
use zfx_perps_clearing::{Globals, MarketParams, MarketStatus};

/// Orders must stay fillable at least this long after admission, so they can settle.
pub const MIN_ORDER_LIFE_NS: u64 = 10 * 1_000_000_000;
/// Gas budget per settlement batch (the contract limit is 300 Tgas per call).
pub const MAX_BATCH_TGAS: f64 = 200.0;
pub const MAX_BATCH_PAYOUTS: usize = 4;
/// Resting orders offered to one liquidation.
pub const LIQ_CANDIDATES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearingConfig {
    pub chain_id: String,
    pub contract: String,
    pub collateral: String,
    pub collateral_decimals: u8,
    pub insurance_account: String,
    pub settle_grace_ns: u64,
    pub max_order_ttl_ns: u64,
}

/// Events from the clearing contract, in its sequence order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClearingEvent {
    Deposit {
        deposit_id: u64,
        account: String,
        amount: u128,
    },
    DepositReclaimed {
        deposit_id: u64,
    },
    KeyAdded {
        account: String,
        public_key: Key,
        expires_ns: u64,
        can_withdraw: bool,
    },
    KeyRemoved {
        account: String,
        public_key: Key,
    },
    OrderCanceled {
        account: String,
        order_hash: [u8; 32],
        expiry_ns: u64,
    },
    MinNonce {
        account: String,
        min_nonce: u64,
    },
    ForcedWithdrawal {
        request_id: u64,
        account: String,
        amount: u128,
    },
    WithdrawalReverted {
        account: String,
        amount: u128,
    },
    BatchSettled {
        batch_id: u64,
        block_ns: u64,
        rejected: Vec<(u32, core::Reject)>,
    },
    MarketUpdated {
        params: MarketParams,
        policy_hash: [u8; 32],
        status: MarketStatus,
        created_ns: u64,
    },
    ReportersUpdated {
        reporters: Vec<Key>,
        quorum: u8,
    },
    /// Admin proposals and other informational events.
    Other,
    Frozen,
}

/// An op not yet cut into a batch, with its own order table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOp {
    pub ts_ns: u64,
    pub op: Op,
    pub orders: Vec<SignedOrder>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutBatch {
    pub batch: Batch,
    /// Op indices the engine expects the contract to reject (normally none).
    pub predicted_rejected: BTreeSet<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintRecord {
    pub print: Vec<u8>,
    pub sigs: Vec<(Key, wire::Sig64)>,
    pub ts_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearingState {
    pub cfg: ClearingConfig,
    pub collateral_asset: AssetId,
    pub confirmed: MemStore,
    pub spec: MemStore,
    pub pending: Vec<PendingOp>,
    pub unconfirmed: VecDeque<CutBatch>,
    pub next_batch_id: u64,
    pub last_settled: u64,
    /// Signed orders resting on the book.
    pub orders: BTreeMap<OrderId, SignedOrder>,
    /// Order hash -> resting order id.
    pub by_hash: BTreeMap<[u8; 32], OrderId>,
    /// Engine market id per clearing market id.
    pub markets: BTreeMap<String, MarketId>,
    /// Last accepted print per market, with signatures (provenance for the API).
    pub prints: BTreeMap<String, PrintRecord>,
    /// Recent funding applications per market: (ts, rate ppm, pay per lot).
    pub funding_log: BTreeMap<String, VecDeque<(u64, i64, i128)>>,
    /// Set if the chain applied something the engine couldn't (operator must investigate).
    pub diverged: Option<String>,
    pub frozen: bool,
}

fn rej(r: core::Reject) -> Reject {
    Reject::Clearing(r)
}

impl Engine {
    pub fn clearing(&self) -> Option<&ClearingState> {
        self.clearing.as_deref()
    }

    fn cl(&mut self) -> &mut ClearingState {
        self.clearing.as_mut().expect("clearing mode")
    }

    pub fn is_clearing_market(&self, m: MarketId) -> bool {
        self.markets.get(m as usize).is_some_and(|x| x.clearing)
    }

    pub(crate) fn clearing_init(&mut self, cfg: &ClearingConfig, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        if self.clearing.is_some() {
            return Err(Reject::InvalidSpec);
        }
        let asset = match self.asset_by_ref.get(&cfg.collateral) {
            Some(a) => *a,
            None => {
                let id = self.assets.len() as AssetId;
                self.apply(
                    &Command::AddAsset(AssetSpec {
                        id,
                        symbol: "COLLATERAL".into(),
                        chain_ref: cfg.collateral.clone(),
                        decimals: cfg.collateral_decimals,
                    }),
                    out,
                )?;
                id
            }
        };
        let g = Globals {
            chain_id: cfg.chain_id.clone(),
            contract_id: cfg.contract.clone(),
            collateral: cfg.collateral.clone(),
            insurance_account: cfg.insurance_account.clone(),
            fee_balance: 0,
            reporters: vec![],
            quorum: 1,
            last_op_ns: 0,
            settle_grace_ns: cfg.settle_grace_ns,
            max_order_ttl_ns: cfg.max_order_ttl_ns,
            total_collateral: 0,
        };
        self.clearing = Some(Box::new(ClearingState {
            cfg: cfg.clone(),
            collateral_asset: asset,
            confirmed: MemStore::new(g.clone()),
            spec: MemStore::new(g),
            pending: vec![],
            unconfirmed: VecDeque::new(),
            next_batch_id: 1,
            last_settled: 0,
            orders: BTreeMap::new(),
            by_hash: BTreeMap::new(),
            markets: BTreeMap::new(),
            prints: BTreeMap::new(),
            funding_log: BTreeMap::new(),
            diverged: None,
            frozen: false,
        }));
        Ok(Ack::Ok)
    }

    /// Apply an op to `spec` at `ts` and queue it for the next batch if it succeeds.
    fn spec_apply(&mut self, ts: u64, op: Op, orders: Vec<SignedOrder>, out: &mut Vec<Event>) -> Result<Vec<CEvent>, Failure> {
        let cl = self.cl();
        let ts = ts.max(cl.spec.globals.last_op_ns);
        let saved = cl.spec.globals.last_op_ns;
        let now = cbatch::check_time(&mut cl.spec, ts, ts).map_err(Failure::from)?;
        let mut o = Orders::new(&orders, true);
        let r = cbatch::apply_op(&mut cl.spec, &NativeCrypto, &mut o, now, &op);
        match r {
            Ok(ev) => {
                cl.pending.push(PendingOp { ts_ns: ts, op, orders });
                self.publish(&ev, out);
                Ok(ev)
            }
            Err(f) => {
                self.cl().spec.globals.last_op_ns = saved;
                Err(f)
            }
        }
    }

    /// Translate core events into engine events (account names to engine ids).
    fn publish(&mut self, ev: &[CEvent], out: &mut Vec<Event>) {
        for e in ev {
            match e {
                CEvent::Position {
                    account,
                    market,
                    size,
                    margin,
                    ..
                } => {
                    let a = self.account_for_pub(account, out);
                    out.push(Event::ClearingPosition {
                        account: a,
                        market: market.clone(),
                        size: *size,
                        margin: *margin,
                    });
                }
                CEvent::Free { account, free } => {
                    let a = self.account_for_pub(account, out);
                    out.push(Event::ClearingFree { account: a, free: *free });
                }
                other => out.push(Event::Clearing { event: other.clone() }),
            }
        }
    }

    pub(crate) fn account_for_pub(&mut self, name: &str, out: &mut Vec<Event>) -> AccountId {
        self.account_for_name(name, out)
    }

    fn market_of(&self, id: &str) -> Option<MarketId> {
        self.clearing().and_then(|c| c.markets.get(id).copied())
    }

    // ------------------------------------------------------------------ admission

    /// Place a signed order: admission checks against `spec`, then the book.
    pub(crate) fn place_signed(&mut self, so: &SignedOrder, ts: u64, stp: Stp, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        let r = self.place_signed_inner(so, ts, stp, out);
        if let Err(reason) = r
            && let Some(a) = self.account_by_name.get(&so.account).copied()
        {
            out.push(Event::Rejected {
                account: a,
                client_id: so.client_order_id,
                reason,
            });
        }
        r
    }

    fn place_signed_inner(&mut self, so: &SignedOrder, ts: u64, stp: Stp, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        let cl = self.clearing.as_deref().ok_or(Reject::NotAPerp)?;
        if cl.frozen || cl.diverged.is_some() {
            return Err(Reject::MarketHalted);
        }
        let mi = cl.markets.get(&so.market).copied().ok_or(Reject::UnknownMarket)?;
        if !so.well_formed() {
            return Err(rej(core::Reject::Malformed));
        }
        if so.chain_id != cl.cfg.chain_id {
            return Err(rej(core::Reject::WrongChain));
        }
        if so.clearing_contract != cl.cfg.contract {
            return Err(rej(core::Reject::WrongContract));
        }
        let account = *self.account_by_name.get(&so.account).ok_or(Reject::UnknownAccount)?;
        let cl = self.clearing.as_deref_mut().unwrap();
        let s = &mut cl.spec;
        let m = s.market(&so.market).ok_or(Reject::UnknownMarket)?;
        let grace = s.globals.settle_grace_ns;
        let k = s.key(&so.public_key).ok_or(Reject::KeyNotRegistered)?;
        if k.account != so.account {
            return Err(Reject::KeyNotRegistered);
        }
        if k.expires_ns != 0 && k.expires_ns <= ts + MIN_ORDER_LIFE_NS {
            return Err(rej(core::Reject::KeyExpired));
        }
        if so.policy_hash != m.policy_hash {
            return Err(rej(core::Reject::PolicyMismatch));
        }
        let acct = s.account(&so.account);
        if so.nonce < acct.min_nonce {
            return Err(rej(core::Reject::NonceBelowMin));
        }
        if so.expiry_ns <= ts + MIN_ORDER_LIFE_NS {
            return Err(rej(core::Reject::Expired));
        }
        if so.expiry_ns > ts + s.globals.max_order_ttl_ns {
            return Err(rej(core::Reject::ExpiryTooFar));
        }
        if so.qty < m.params.min_qty || so.qty > m.params.max_qty {
            return Err(Reject::InvalidQty);
        }
        if so.price > m.params.max_price {
            return Err(Reject::InvalidPrice);
        }
        if so.max_fee_bps < m.params.maker_fee_bps || (!so.post_only && so.max_fee_bps < m.params.taker_fee_bps) {
            return Err(rej(core::Reject::FeeAboveMax));
        }
        let h = NativeCrypto.sha256(&so.message());
        if cl.by_hash.contains_key(&h) || s.order(&h).is_some() {
            return Err(Reject::DuplicateClientId);
        }
        let now = Now::at(ts);
        let status = ledger::effective_status(&m, now, grace);
        let pos = s.position(&so.account, &so.market);
        let reduces = (pos.size > 0 && so.side == wire::Side::Sell) || (pos.size < 0 && so.side == wire::Side::Buy);
        let reducing_only = reduces && so.qty <= pos.size.unsigned_abs();
        match status {
            MarketStatus::Active => {}
            MarketStatus::CloseOnly if reducing_only => {}
            MarketStatus::CloseOnly => return Err(rej(core::Reject::CloseOnly)),
            _ => return Err(Reject::MarketHalted),
        }
        if so.reduce_only && !reducing_only {
            return Err(rej(core::Reject::ReduceOnlyViolated));
        }
        if m.mark == 0 {
            return Err(Reject::MarkUnavailable);
        }
        if (so.price.abs_diff(m.mark) as u128) * 10_000 > m.mark as u128 * m.params.price_band_bps as u128 {
            return Err(Reject::PriceOutOfBand);
        }
        if !reducing_only && !self.margin_admits(account, so, &m) {
            return Err(Reject::InsufficientMargin);
        }
        let tif = match (so.post_only, so.tif) {
            (true, _) => Tif::PostOnly,
            (false, wire::TimeInForce::Gtc) => Tif::Gtc,
            (false, wire::TimeInForce::Ioc) => Tif::Ioc,
            (false, wire::TimeInForce::Fok) => Tif::Fok,
        };
        let o = NewOrder {
            account,
            market: mi,
            side: match so.side {
                wire::Side::Buy => Side::Buy,
                wire::Side::Sell => Side::Sell,
            },
            price: so.price,
            qty: so.qty,
            tif,
            stp,
            client_id: 0,
            ts_ns: ts,
        };
        self.clearing_taker = Some((so.clone(), h));
        let r = self.place_book(&o, out);
        self.clearing_taker = None;
        if let Ok(Ack::Placed {
            order_id,
            status: OrderStatus::Resting,
            ..
        }) = &r
        {
            let cl = self.cl();
            cl.orders.insert(*order_id, so.clone());
            cl.by_hash.insert(h, *order_id);
        }
        r
    }

    /// Conservative admission: the collateral every resting order of the account could need, at
    /// the mark, fits in its free collateral. The contract re-checks at each fill.
    fn margin_admits(&mut self, account: AccountId, so: &SignedOrder, _m: &core::MarketState) -> bool {
        let name = self.accounts[account as usize].name.clone();
        let markets: Vec<(String, MarketId)> = self.clearing().unwrap().markets.iter().map(|(k, v)| (k.clone(), *v)).collect();
        let mut need: i128 = 0;
        for (id, mi) in markets {
            let p = &self.accounts[account as usize].perp;
            let mut ob = p.open_buy.get(&mi).copied().unwrap_or(0) as i128;
            let mut os = p.open_sell.get(&mi).copied().unwrap_or(0) as i128;
            if id == so.market {
                match so.side {
                    wire::Side::Buy => ob += so.qty as i128,
                    wire::Side::Sell => os += so.qty as i128,
                }
            }
            let s = &mut self.clearing.as_mut().unwrap().spec;
            let Some(m) = s.market(&id) else { continue };
            let pos = s.position(&name, &id);
            let worst = ((pos.size as i128 + ob).abs()).max((pos.size as i128 - os).abs()) as u64;
            if worst == 0 && pos.size == 0 {
                continue;
            }
            let eq = if pos.size == 0 { 0 } else { ledger::equity(&m, &pos, m.mark) };
            let mut req = ledger::initial_margin(&m, worst, m.mark) as i128;
            req += ledger::fee(m.notional((ob + os) as u64, m.mark), m.params.taker_fee_bps) as i128;
            if id == so.market {
                let adverse = match so.side {
                    wire::Side::Buy => so.price.saturating_sub(m.mark),
                    wire::Side::Sell => m.mark.saturating_sub(so.price),
                };
                req += m.notional(so.qty, adverse) as i128;
                req += ledger::fee(m.notional(so.qty, so.price), m.params.taker_fee_bps) as i128;
            }
            need += (req - eq).max(0);
        }
        let s = &mut self.clearing.as_mut().unwrap().spec;
        need <= s.account(&name).free as i128
    }

    /// One candidate fill during matching. `Err(Party::Maker)` drops the resting order.
    pub(crate) fn clearing_fill(&mut self, maker_id: OrderId, q: u64, p: u64, ts: u64, out: &mut Vec<Event>) -> Result<(u128, u128), Party> {
        let (taker, _) = self.clearing_taker.clone().expect("clearing taker set");
        let maker = self.clearing().unwrap().orders.get(&maker_id).cloned().ok_or(Party::Maker)?;
        let op = Op::Fill {
            maker: 0,
            taker: 1,
            qty: q,
            price: p,
        };
        match self.spec_apply(ts, op, vec![maker, taker], out) {
            Ok(ev) => Ok(ev
                .iter()
                .find_map(|e| match e {
                    CEvent::Fill { maker_fee, taker_fee, .. } => Some((*maker_fee, *taker_fee)),
                    _ => None,
                })
                .unwrap_or((0, 0))),
            Err(f) => Err(f.party),
        }
    }

    /// Called when a resting clearing order leaves the book (filled or canceled).
    pub(crate) fn clearing_forget(&mut self, id: OrderId) {
        if let Some(cl) = self.clearing.as_deref_mut()
            && let Some(so) = cl.orders.remove(&id)
        {
            let h = NativeCrypto.sha256(&so.message());
            cl.by_hash.remove(&h);
        }
    }

    pub fn clearing_order_id(&self, hash: &[u8; 32]) -> Option<OrderId> {
        self.clearing().and_then(|c| c.by_hash.get(hash).copied())
    }

    // ------------------------------------------------------------------ account actions

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn clearing_action(&mut self, op: Op, ts: u64, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        let c = self.clearing().ok_or(Reject::NotAPerp)?;
        if c.frozen {
            return Err(Reject::MarketHalted);
        }
        // Side effects on the book before the op is queued.
        let (account, cancel_hash, min_nonce) = match &op {
            Op::Cancel { account, order_hash, .. } => (account.clone(), Some(*order_hash), None),
            Op::MinNonce { account, min_nonce, .. } => (account.clone(), None, Some(*min_nonce)),
            Op::Withdraw { account, .. } | Op::AdjustMargin { account, .. } => (account.clone(), None, None),
            Op::Oracle { .. } | Op::Funding { .. } => (String::new(), None, None),
            _ => return Err(Reject::InvalidSpec),
        };
        let market = match &op {
            Op::Oracle { market, .. } | Op::Funding { market } => Some(market.clone()),
            _ => None,
        };
        let rec = match &op {
            Op::Oracle { market, print, sigs } => Some((market.clone(), print.clone(), sigs.clone())),
            _ => None,
        };
        let ev = self.spec_apply(ts, op, vec![], out).map_err(|f| rej(f.reason))?;
        if let Some((m, print, sigs)) = rec {
            self.cl().prints.insert(m, PrintRecord { print, sigs, ts_ns: ts });
        }
        for e in &ev {
            if let CEvent::Funding { market, rate_ppm, pay_per_lot } = e {
                let log = self.cl().funding_log.entry(market.clone()).or_default();
                log.push_back((ts, *rate_ppm, *pay_per_lot));
                if log.len() > 200 {
                    log.pop_front();
                }
            }
        }
        if let Some(h) = cancel_hash {
            self.cancel_resting_hash(&h, CancelReason::User, out);
        }
        if let Some(n) = min_nonce {
            self.cancel_below_nonce(&account, n, out);
        }
        if let Some(m) = market {
            self.clearing_sweep(&m, ts, out);
        }
        Ok(Ack::Ok)
    }

    fn cancel_resting_hash(&mut self, h: &[u8; 32], reason: CancelReason, out: &mut Vec<Event>) {
        if let Some(id) = self.clearing_order_id(h) {
            let acct = self.clearing().unwrap().orders[&id].account.clone();
            if let Some(a) = self.account_by_name.get(&acct).copied() {
                let _ = self.cancel_order_internal(a, id, reason, out);
            }
        }
    }

    fn cancel_where(&mut self, f: impl Fn(&SignedOrder) -> bool, reason: CancelReason, out: &mut Vec<Event>) {
        let ids: Vec<(OrderId, String)> = self
            .clearing()
            .map(|c| c.orders.iter().filter(|(_, o)| f(o)).map(|(i, o)| (*i, o.account.clone())).collect())
            .unwrap_or_default();
        for (id, acct) in ids {
            if let Some(a) = self.account_by_name.get(&acct).copied() {
                let _ = self.cancel_order_internal(a, id, reason, out);
            }
        }
    }

    fn cancel_below_nonce(&mut self, account: &str, n: u64, out: &mut Vec<Event>) {
        self.cancel_where(|o| o.account == account && o.nonce < n, CancelReason::User, out);
    }

    // ------------------------------------------------------------------ liquidation

    /// Liquidate every position in `market` that is below maintenance at the current mark.
    pub(crate) fn clearing_sweep(&mut self, market: &str, ts: u64, out: &mut Vec<Event>) {
        let Some(mi) = self.market_of(market) else { return };
        let ts = ts.max(self.clearing().unwrap().spec.globals.last_op_ns);
        let holders: Vec<String> = self.clearing().unwrap().spec.holders(market).map(|(a, _)| a.to_string()).collect();
        for a in holders {
            let now = Now::at(ts);
            if !ledger::liquidatable(&mut self.cl().spec, &a, market, now) {
                continue;
            }
            // The account's own resting orders in this market go first.
            let acct = a.clone();
            let mk = market.to_string();
            self.cancel_where(|o| o.account == acct && o.market == mk, CancelReason::Liquidation, out);
            let size = self.cl().spec.position(&a, market).size;
            let take = if size > 0 { Side::Buy } else { Side::Sell };
            let (cands, ids, orders) = self.liq_candidates(mi, take, &a);
            let mut tried_book = !cands.is_empty();
            loop {
                let book: &[Candidate] = if tried_book { &cands } else { &[] };
                let Some(plan) = plan_liquidation(&mut self.cl().spec, now, &a, market, book) else { break };
                let op = Op::Liquidate {
                    account: a.clone(),
                    market: market.to_string(),
                    fills: plan.fills.clone(),
                    adl: plan.adl.clone(),
                    socialize: plan.socialize.clone(),
                };
                let used: Vec<SignedOrder> = if tried_book { orders.clone() } else { vec![] };
                match self.spec_apply(ts, op, used, out) {
                    Ok(_) => {
                        if tried_book {
                            for (i, q) in &plan.fills {
                                self.book_take(mi, ids[*i as usize], *q, out);
                            }
                        }
                        break;
                    }
                    Err(f) if tried_book => {
                        out.push(Event::ClearingLiquidationRetry { account: a.clone(), reason: f.reason });
                        tried_book = false;
                    }
                    Err(f) => {
                        out.push(Event::ClearingLiquidationFailed { account: a.clone(), reason: f.reason });
                        break;
                    }
                }
            }
        }
    }

    /// Resting orders on `side` in price-time order, as liquidation candidates.
    fn liq_candidates(&self, mi: MarketId, side: Side, exclude: &str) -> (Vec<Candidate>, Vec<OrderId>, Vec<SignedOrder>) {
        let c = self.clearing().unwrap();
        let book = &self.markets[mi as usize].book;
        let (mut cands, mut ids, mut orders) = (vec![], vec![], vec![]);
        let levels: Vec<u64> = match side {
            Side::Buy => book.bids.keys().rev().copied().collect(),
            Side::Sell => book.asks.keys().copied().collect(),
        };
        'outer: for p in levels {
            let mut s = book.head(side, p);
            while s != crate::book::NIL {
                let n = book.node(s);
                if let Some(so) = c.orders.get(&n.id)
                    && so.account != exclude
                {
                    cands.push(Candidate {
                        index: orders.len() as u32,
                        account: so.account.clone(),
                        price: p,
                        available: n.rem,
                    });
                    ids.push(n.id);
                    orders.push(so.clone());
                    if cands.len() >= LIQ_CANDIDATES {
                        break 'outer;
                    }
                }
                s = book.next_slot(s);
            }
        }
        (cands, ids, orders)
    }

    /// A resting order absorbed `q` lots of a liquidation: shrink it on the book.
    fn book_take(&mut self, mi: MarketId, id: OrderId, q: u64, out: &mut Vec<Event>) {
        let m = &mut self.markets[mi as usize];
        let Some(slot) = m.book.slot(id) else { return };
        let n = m.book.node(slot).clone();
        let (left, lvl) = m.book.reduce(slot, q);
        self.perp_open_sub(n.account, mi, n.side, q);
        let lvl = if left == 0 {
            let (_, l) = self.remove_filled_slot(mi as usize, slot);
            l
        } else {
            lvl
        };
        out.push(Event::Level {
            market: mi,
            side: n.side,
            price: n.price,
            qty: lvl,
        });
    }

    // ------------------------------------------------------------------ batches

    /// Pack pending ops into batches within the gas and payout budgets.
    pub(crate) fn clearing_cut(&mut self, journal_seq: u64, journal_hash: [u8; 32], out: &mut Vec<Event>) {
        let Some(cl) = self.clearing.as_deref_mut() else { return };
        if cl.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut cl.pending);
        let mut batches: Vec<Batch> = vec![];
        let new = |id| Batch {
            id,
            orders: vec![],
            ops: vec![],
            journal_seq,
            journal_hash,
        };
        let mut cur = new(cl.next_batch_id);
        let mut index: BTreeMap<[u8; 32], u32> = BTreeMap::new();
        let (mut gas, mut payouts) = (cbatch::BATCH_BASE_TGAS, 0usize);
        for p in pending {
            let new_orders: Vec<&SignedOrder> = p
                .orders
                .iter()
                .filter(|o| !index.contains_key(&NativeCrypto.sha256(&o.message())))
                .collect();
            let g = cbatch::estimate_tgas(&p.op, new_orders.len());
            let is_payout = matches!(p.op, Op::Withdraw { .. } | Op::Forced { .. });
            if !cur.ops.is_empty() && (gas + g > MAX_BATCH_TGAS || (is_payout && payouts >= MAX_BATCH_PAYOUTS)) {
                let id = cur.id + 1;
                batches.push(std::mem::replace(&mut cur, new(id)));
                index.clear();
                gas = cbatch::BATCH_BASE_TGAS;
                payouts = 0;
            }
            let mut map = vec![];
            for o in &p.orders {
                let h = NativeCrypto.sha256(&o.message());
                let i = *index.entry(h).or_insert_with(|| {
                    cur.orders.push(o.clone());
                    (cur.orders.len() - 1) as u32
                });
                map.push(i);
            }
            let op = match p.op {
                Op::Fill { maker, taker, qty, price } => Op::Fill {
                    maker: map[maker as usize],
                    taker: map[taker as usize],
                    qty,
                    price,
                },
                Op::Liquidate {
                    account,
                    market,
                    fills,
                    adl,
                    socialize,
                } => Op::Liquidate {
                    account,
                    market,
                    fills: fills.into_iter().map(|(i, q)| (map[i as usize], q)).collect(),
                    adl,
                    socialize,
                },
                other => other,
            };
            gas += cbatch::estimate_tgas(&op, new_orders.len());
            payouts += is_payout as usize;
            cur.ops.push(OpAt { ts_ns: p.ts_ns, op });
        }
        batches.push(cur);
        cl.next_batch_id = batches.last().unwrap().id + 1;
        for b in batches {
            out.push(Event::ClearingBatchCut { batch: Box::new(b.clone()) });
            cl.unconfirmed.push_back(CutBatch {
                batch: b,
                predicted_rejected: BTreeSet::new(),
            });
        }
        // Expired order records can't matter to any future fill; drop them from both ledgers.
        let horizon = cl.spec.globals.last_op_ns.saturating_sub(cl.spec.globals.settle_grace_ns);
        cl.spec.orders.retain(|_, o| o.expiry_ns > horizon);
        cl.confirmed.orders.retain(|_, o| o.expiry_ns > horizon);
    }

    // ------------------------------------------------------------------ chain events

    pub(crate) fn clearing_chain(&mut self, ev: &ClearingEvent, ts: u64, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        if self.clearing.is_none() {
            return Err(Reject::NotAPerp);
        }
        let mut rebuild = false;
        match ev {
            ClearingEvent::Deposit { deposit_id, account, amount } => {
                let d = core::PendingDeposit {
                    account: account.clone(),
                    amount: *amount,
                    created_ns: ts,
                };
                let cl = self.cl();
                cl.confirmed.deposits.insert(*deposit_id, d.clone());
                cl.spec.deposits.insert(*deposit_id, d);
                let id = self.account_for_pub(account, out);
                let asset = self.cl().collateral_asset;
                out.push(Event::Deposited { account: id, asset, amount: *amount });
                let _ = self.spec_apply(ts, Op::Credit { deposit_id: *deposit_id }, vec![], out);
            }
            ClearingEvent::DepositReclaimed { deposit_id } => {
                self.cl().confirmed.deposits.remove(deposit_id);
                rebuild = true;
            }
            ClearingEvent::KeyAdded {
                account,
                public_key,
                expires_ns,
                can_withdraw,
            } => {
                let k = core::KeyInfo {
                    account: account.clone(),
                    expires_ns: *expires_ns,
                    can_withdraw: *can_withdraw,
                };
                let cl = self.cl();
                cl.confirmed.keys.insert(*public_key, k.clone());
                cl.spec.keys.insert(*public_key, k);
                rebuild = self.clearing().unwrap().unconfirmed.iter().any(|b| !b.predicted_rejected.is_empty());
                let id = self.account_for_pub(account, out);
                let a = &mut self.accounts[id as usize];
                if !a.keys.contains(public_key) {
                    a.keys.push(*public_key);
                }
                out.push(Event::KeyAdded { account: id, public_key: *public_key });
            }
            ClearingEvent::KeyRemoved { account, public_key } => {
                self.cl().confirmed.keys.remove(public_key);
                if let Some(id) = self.account_by_name.get(account).copied() {
                    self.accounts[id as usize].keys.retain(|k| k != public_key);
                    out.push(Event::KeyRemoved { account: id, public_key: *public_key });
                }
                let pk = *public_key;
                self.cancel_where(|o| o.public_key == pk, CancelReason::User, out);
                rebuild = true;
            }
            ClearingEvent::OrderCanceled { account, order_hash, expiry_ns } => {
                let cl = self.cl();
                let _ = ledger::cancel(&mut cl.confirmed, &NativeCrypto, account, order_hash, *expiry_ns, None);
                self.cancel_resting_hash(order_hash, CancelReason::User, out);
                rebuild = true;
            }
            ClearingEvent::MinNonce { account, min_nonce } => {
                let cl = self.cl();
                let _ = ledger::set_min_nonce(&mut cl.confirmed, &NativeCrypto, account, *min_nonce, None);
                self.cancel_below_nonce(account, *min_nonce, out);
                rebuild = true;
            }
            ClearingEvent::ForcedWithdrawal { request_id, account, amount } => {
                let r = core::ForcedRequest {
                    account: account.clone(),
                    amount: *amount,
                    created_ns: ts,
                    done: false,
                    paid: 0,
                };
                let cl = self.cl();
                cl.confirmed.forced.insert(*request_id, r.clone());
                cl.spec.forced.insert(*request_id, r);
                let _ = self.spec_apply(ts, Op::Forced { request_id: *request_id }, vec![], out);
            }
            ClearingEvent::WithdrawalReverted { account, amount } => {
                let cl = self.cl();
                for s in [&mut cl.confirmed, &mut cl.spec] {
                    let mut a = s.account(account);
                    a.free += amount;
                    s.put_account(account, a);
                    s.globals.total_collateral += amount;
                }
                let id = self.account_for_pub(account, out);
                let asset = self.cl().collateral_asset;
                out.push(Event::Credited { account: id, asset, amount: *amount });
            }
            ClearingEvent::BatchSettled { batch_id, block_ns, rejected } => {
                rebuild = self.settle_confirmed(*batch_id, *block_ns, rejected, out);
            }
            ClearingEvent::MarketUpdated {
                params,
                policy_hash,
                status,
                created_ns,
            } => {
                rebuild = self.market_updated(params, policy_hash, *status, *created_ns, out)?;
            }
            ClearingEvent::ReportersUpdated { reporters, quorum } => {
                let cl = self.cl();
                for s in [&mut cl.confirmed, &mut cl.spec] {
                    s.globals.reporters = reporters.clone();
                    s.globals.quorum = *quorum;
                }
                rebuild = true;
            }
            ClearingEvent::Frozen => {
                self.cl().frozen = true;
                self.cancel_where(|_| true, CancelReason::User, out);
            }
            ClearingEvent::Other => {}
        }
        if rebuild {
            self.rebuild_spec(out);
        }
        Ok(Ack::Ok)
    }

    fn market_updated(&mut self, params: &MarketParams, policy: &[u8; 32], status: MarketStatus, created_ns: u64, out: &mut Vec<Event>) -> Result<bool, Reject> {
        let id = params.market_id.clone();
        let computed = ledger::policy_hash(&NativeCrypto, params);
        if computed != *policy {
            self.cl().diverged = Some(format!("policy hash of {id} differs from the contract's"));
            return Err(Reject::InvalidSpec);
        }
        if self.market_of(&id).is_none() {
            let mi = self.markets.len() as MarketId;
            let asset = self.cl().collateral_asset;
            let spec = MarketSpec {
                id: mi,
                symbol: id.clone(),
                base: asset,
                quote: asset,
                base_lot: 1,
                quote_tick: params.tick_value,
                min_qty: params.min_qty,
                max_qty: params.max_qty,
                max_price: params.max_price,
                maker_fee_bps: params.maker_fee_bps,
                taker_fee_bps: params.taker_fee_bps,
                max_open_orders: 1_000,
                perp: None,
            };
            self.add_clearing_market(spec, out);
            let mut m = ledger::new_market(&NativeCrypto, params.clone(), created_ns).map_err(|_| Reject::InvalidSpec)?;
            m.status = status;
            let cl = self.cl();
            cl.markets.insert(id.clone(), mi);
            cl.confirmed.markets.insert(id.clone(), m.clone());
            cl.spec.markets.insert(id, m);
            return Ok(false);
        }
        let mut changed = false;
        let cl = self.cl();
        for s in [&mut cl.confirmed, &mut cl.spec] {
            let m = s.markets.get_mut(&id).unwrap();
            changed |= m.policy_hash != *policy;
            m.params = params.clone();
            m.policy_hash = *policy;
            m.status = status;
        }
        if changed {
            let mk = id.clone();
            self.cancel_where(|o| o.market == mk, CancelReason::User, out);
        }
        Ok(true)
    }

    /// Apply a settled batch to `confirmed` with the chain's results. Returns whether `spec` must
    /// be rebuilt (the chain's outcome differs from the prediction).
    fn settle_confirmed(&mut self, batch_id: u64, block_ns: u64, rejected: &[(u32, core::Reject)], out: &mut Vec<Event>) -> bool {
        let cl = self.cl();
        let Some(pos) = cl.unconfirmed.iter().position(|b| b.batch.id == batch_id) else {
            cl.diverged = Some(format!("batch {batch_id} settled on-chain but is unknown here"));
            return false;
        };
        if pos != 0 {
            cl.diverged = Some(format!("batch {batch_id} settled before earlier batches"));
        }
        let cb = cl.unconfirmed.remove(pos).unwrap();
        let skip: BTreeSet<u32> = rejected.iter().map(|(i, _)| *i).collect();
        let mut orders = Orders::new(&cb.batch.orders, true);
        for (i, o) in cb.batch.ops.iter().enumerate() {
            let i = i as u32;
            let Ok(now) = cbatch::check_time(&mut cl.confirmed, o.ts_ns, block_ns) else {
                if !skip.contains(&i) {
                    cl.diverged = Some(format!("batch {batch_id} op {i}: chain accepted, time check fails here"));
                }
                continue;
            };
            if skip.contains(&i) {
                continue;
            }
            if let Err(f) = cbatch::apply_op(&mut cl.confirmed, &NativeCrypto, &mut orders, now, &o.op) {
                cl.diverged = Some(format!("batch {batch_id} op {i}: chain accepted, here {:?}", f.reason));
            }
        }
        cl.last_settled = cl.last_settled.max(batch_id);
        let differs = skip != cb.predicted_rejected;
        for (i, r) in rejected {
            out.push(Event::ClearingOpRejected {
                batch_id,
                index: *i,
                reason: *r,
            });
        }
        out.push(Event::BatchSettled { batch_id });
        if let Some(d) = self.clearing().unwrap().diverged.clone() {
            out.push(Event::ClearingDivergence { detail: d });
        }
        differs
    }

    /// `spec = confirmed + unconfirmed batches + pending ops`. Pending ops that no longer apply
    /// are dropped and reported.
    pub(crate) fn rebuild_spec(&mut self, out: &mut Vec<Event>) {
        let cl = self.cl();
        let mut spec = cl.confirmed.clone();
        for b in cl.unconfirmed.iter_mut() {
            let r = cbatch::apply_batch(&mut spec, &NativeCrypto, &b.batch, None, true);
            b.predicted_rejected = r.iter().filter(|x| x.result.is_err()).map(|x| x.index).collect();
        }
        let pending = std::mem::take(&mut cl.pending);
        let mut kept = vec![];
        let mut dropped = vec![];
        for p in pending {
            let ts = p.ts_ns.max(spec.globals.last_op_ns);
            let saved = spec.globals.last_op_ns;
            let ok = cbatch::check_time(&mut spec, ts, ts).ok().and_then(|now| {
                let mut o = Orders::new(&p.orders, true);
                cbatch::apply_op(&mut spec, &NativeCrypto, &mut o, now, &p.op).ok()
            });
            if ok.is_some() {
                kept.push(p);
            } else {
                spec.globals.last_op_ns = saved;
                dropped.push(p);
            }
        }
        cl.spec = spec;
        cl.pending = kept;
        for p in dropped {
            if let Op::Fill { maker, taker, qty, price } = &p.op {
                let (m, t) = (&p.orders[*maker as usize], &p.orders[*taker as usize]);
                out.push(Event::FillReverted {
                    market: m.market.clone(),
                    maker: m.account.clone(),
                    taker: t.account.clone(),
                    qty: *qty,
                    price: *price,
                });
            } else {
                out.push(Event::ClearingOpDropped { op: Box::new(p.op) });
            }
        }
    }

    /// Clearing-ledger invariants (test helper): conservation in both copies, and the book's
    /// open quantities match the engine's resting signed orders.
    pub fn clearing_invariants(&self) -> Result<(), String> {
        let Some(c) = self.clearing() else { return Ok(()) };
        zfx_perps_clearing::testkit::check_conservation(&c.confirmed).map_err(|e| format!("confirmed: {e}"))?;
        zfx_perps_clearing::testkit::check_conservation(&c.spec).map_err(|e| format!("spec: {e}"))?;
        for (id, so) in &c.orders {
            let mi = c.markets[&so.market];
            if self.markets[mi as usize].book.get(*id).is_none() {
                return Err(format!("signed order {id} not on the book"));
            }
        }
        Ok(())
    }
}
