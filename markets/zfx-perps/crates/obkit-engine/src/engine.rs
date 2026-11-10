//! The deterministic state machine. `Engine::apply` is a pure function of the state and the
//! command: no clocks, no randomness, no hash-map iteration without sorting. Replaying the same
//! command log always yields the same state and the same events.

use crate::book::{Book, NIL};
use crate::perp::{AccountPerp, PerpState};
use crate::types::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    /// Everything the account owns in this asset, including held amounts.
    pub total: u128,
    /// Reserved by resting orders.
    pub held: u128,
}

impl Balance {
    pub fn available(&self) -> u128 {
        self.total - self.held
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub balances: Vec<Balance>,
    /// Open orders: id -> (market, client_id).
    pub open: FxHashMap<OrderId, (MarketId, u64)>,
    /// (market, client_id) -> order id, for client-id cancels and duplicate checks.
    pub client: FxHashMap<(MarketId, u64), OrderId>,
    pub open_per_market: FxHashMap<MarketId, u32>,
    pub keys: Vec<Key>,
    pub withdraw_nonce: u64,
    pub perp: AccountPerp,
}

impl Account {
    fn bal(&mut self, asset: AssetId) -> &mut Balance {
        let i = asset as usize;
        if self.balances.len() <= i {
            self.balances.resize(i + 1, Balance::default());
        }
        &mut self.balances[i]
    }
    pub(crate) fn bal_mut(&mut self, asset: AssetId) -> &mut Balance {
        self.bal(asset)
    }
    pub fn balance(&self, asset: AssetId) -> Balance {
        self.balances.get(asset as usize).copied().unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Market {
    pub spec: MarketSpec,
    pub book: Book,
    pub halted: bool,
}

impl Market {
    /// Amount held per lot by a resting order: base atoms for sells; for buys the quote notional at
    /// the limit price plus the taker fee on it, rounded up per lot. Per-lot rounding keeps the hold
    /// linear in quantity, so a fill of `q` lots always releases at least what it costs.
    pub fn hold_per_lot(&self, side: Side, price: u64) -> u128 {
        match side {
            Side::Sell => self.spec.base_lot,
            Side::Buy => {
                let n = price as u128 * self.spec.quote_tick;
                n + fee(n, self.spec.taker_fee_bps)
            }
        }
    }
}

/// Upper bound on one order's notional or base amount in atoms (2^120, ~1.3e36).
pub const MAX_ORDER_ATOMS: u128 = 1u128 << 120;

/// Fee in quote atoms, rounded up.
pub fn fee(notional: u128, bps: u16) -> u128 {
    (notional * bps as u128).div_ceil(10_000)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Settlement-chain account that receives fees (internal account 0).
    pub fee_account: String,
    /// Delta entries per settlement transaction. Bounded by the contract's gas per entry.
    pub max_batch_entries: usize,
    /// Withdrawals plus forced withdrawals per settlement transaction (each is a token transfer).
    pub max_batch_withdrawals: usize,
    /// Settlement-chain account of the perp insurance fund (created when the first perp is listed).
    pub insurance_account: String,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            fee_account: "fees.obkit.near".into(),
            max_batch_entries: 48,
            max_batch_withdrawals: 4,
            insurance_account: "insurance.obkit.near".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settlement {
    /// Net balance change per (account, asset) since the last cut.
    pub deltas: FxHashMap<(AccountId, AssetId), i128>,
    pub withdrawals: Vec<(AccountId, AssetId, u128, u64, Key, Vec<u8>)>,
    pub forced: Vec<(u64, AccountId, AssetId, u128)>,
    pub next_batch_id: u64,
    /// Cut but not yet confirmed on-chain, oldest first.
    pub unconfirmed: VecDeque<Batch>,
    pub last_settled: u64,
}

impl Settlement {
    fn add(&mut self, account: AccountId, asset: AssetId, d: i128) {
        if d != 0 {
            *self.deltas.entry((account, asset)).or_insert(0) += d;
        }
    }
    pub fn is_empty(&self) -> bool {
        self.deltas.values().all(|d| *d == 0) && self.withdrawals.is_empty() && self.forced.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Engine {
    pub config: EngineConfig,
    pub assets: Vec<AssetSpec>,
    pub asset_by_ref: FxHashMap<String, AssetId>,
    pub markets: Vec<Market>,
    pub market_by_symbol: FxHashMap<String, MarketId>,
    pub accounts: Vec<Account>,
    pub account_by_name: FxHashMap<String, AccountId>,
    pub order_market: FxHashMap<OrderId, MarketId>,
    pub next_order_id: OrderId,
    pub next_trade_id: u64,
    /// Last applied settlement-chain event sequence number.
    pub chain_cursor: u64,
    pub settle: Settlement,
    pub perps: BTreeMap<MarketId, PerpState>,
    /// The single collateral asset of all perp markets (set by the first perp listed).
    pub perp_collateral: Option<AssetId>,
    pub insurance: Option<AccountId>,
    /// Accounts whose perp view changed since the last cut.
    pub perp_dirty: BTreeSet<AccountId>,
    /// Perp markets whose mark moved since the last cut.
    pub marks_changed: BTreeSet<MarketId>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        let fee = config.fee_account.clone();
        let mut e = Engine {
            config,
            assets: vec![],
            asset_by_ref: FxHashMap::default(),
            markets: vec![],
            market_by_symbol: FxHashMap::default(),
            accounts: vec![],
            account_by_name: FxHashMap::default(),
            order_market: FxHashMap::default(),
            next_order_id: 1,
            next_trade_id: 1,
            chain_cursor: 0,
            settle: Settlement {
                next_batch_id: 1,
                ..Default::default()
            },
            perps: BTreeMap::new(),
            perp_collateral: None,
            insurance: None,
            perp_dirty: BTreeSet::new(),
            marks_changed: BTreeSet::new(),
        };
        e.accounts.push(Account {
            name: fee.clone(),
            ..Default::default()
        });
        e.account_by_name.insert(fee, FEE_ACCOUNT);
        e
    }

    pub fn apply(&mut self, cmd: &Command, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        match cmd {
            Command::AddAsset(a) => self.add_asset(a, out),
            Command::AddMarket(m) => self.add_market(m, out),
            Command::SetHalted { market, halted } => {
                let m = self.markets.get_mut(*market as usize).ok_or(Reject::UnknownMarket)?;
                m.halted = *halted;
                out.push(Event::MarketHalted {
                    market: *market,
                    halted: *halted,
                });
                Ok(Ack::Ok)
            }
            Command::Chain(ev) => self.chain(ev, out),
            Command::Place(o) => self.place(o, out),
            Command::Cancel { account, market, order_id } => {
                let m = self.order_market.get(order_id).copied().ok_or(Reject::UnknownOrder)?;
                if m != *market {
                    return Err(Reject::UnknownOrder);
                }
                self.cancel(*account, *order_id, CancelReason::User, out)
            }
            Command::CancelByClientId {
                account,
                market,
                client_id,
            } => {
                let acct = self.accounts.get(*account as usize).ok_or(Reject::UnknownAccount)?;
                let id = *acct.client.get(&(*market, *client_id)).ok_or(Reject::UnknownOrder)?;
                self.cancel(*account, id, CancelReason::User, out)
            }
            Command::CancelAll { account, market } => {
                let n = self.cancel_all(*account, *market, CancelReason::User, out)?;
                Ok(Ack::CanceledAll { count: n })
            }
            Command::Withdraw {
                account,
                asset,
                amount,
                nonce,
                public_key,
                signature,
            } => self.withdraw(*account, *asset, *amount, *nonce, *public_key, signature, out),
            Command::CutBatch { journal_seq, journal_hash } => Ok(self.cut(*journal_seq, *journal_hash, out)),
            Command::SetMark {
                market,
                mark,
                index,
                ts_ns,
            } => self.set_mark(*market, *mark, *index, *ts_ns, out),
            Command::FundingTick { market, ts_ns } => self.funding_tick(*market, *ts_ns, out),
        }
    }

    pub(crate) fn settle_add(&mut self, a: AccountId, asset: AssetId, d: i128) {
        self.settle.add(a, asset, d);
    }

    pub(crate) fn cancel_all_internal(&mut self, a: AccountId, reason: CancelReason, out: &mut Vec<Event>) -> Result<u32, Reject> {
        self.cancel_all(a, None, reason, out)
    }

    fn add_asset(&mut self, a: &AssetSpec, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        if a.id as usize != self.assets.len() {
            return Err(Reject::InvalidSpec);
        }
        if self.asset_by_ref.contains_key(&a.chain_ref) {
            return Err(Reject::AssetExists);
        }
        self.asset_by_ref.insert(a.chain_ref.clone(), a.id);
        self.assets.push(a.clone());
        out.push(Event::AssetAdded { asset: a.clone() });
        Ok(Ack::Ok)
    }

    fn add_market(&mut self, m: &MarketSpec, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        if self.market_by_symbol.contains_key(&m.symbol) {
            return Err(Reject::MarketExists);
        }
        let ok = m.id as usize == self.markets.len()
            && (m.base as usize) < self.assets.len()
            && (m.quote as usize) < self.assets.len()
            && (m.base != m.quote) == m.perp.is_none()
            && m.base_lot > 0
            && m.quote_tick > 0
            && m.min_qty > 0
            && m.min_qty <= m.max_qty
            && m.max_price > 0
            && m.maker_fee_bps <= m.taker_fee_bps
            && m.taker_fee_bps <= 1_000
            && m.max_open_orders > 0
            // One order's worst-case notional/hold must fit in u128 with headroom for fees and
            // sums. Balances themselves are bounded by real token supply (holds are checked
            // against them), so this per-order bound is the only overflow risk.
            && (m.max_qty as u128).checked_mul(m.max_price as u128).and_then(|x| x.checked_mul(m.quote_tick)).is_some_and(|x| x < MAX_ORDER_ATOMS)
            && (m.max_qty as u128).checked_mul(m.base_lot).is_some_and(|x| x < MAX_ORDER_ATOMS);
        if !ok {
            return Err(Reject::InvalidSpec);
        }
        if let Some(pp) = &m.perp {
            let sane = pp.mmr_bps > 0
                && pp.mmr_bps < pp.imr_bps
                && pp.imr_bps <= 10_000
                && pp.price_band_bps > 0
                && pp.price_band_bps < 10_000
                && pp.liq_slippage_bps < 10_000
                && pp.funding_divisor > 0
                && pp.max_funding_ppm < 1_000_000
                && self.perp_collateral.is_none_or(|c| c == m.quote);
            if !sane {
                return Err(Reject::InvalidSpec);
            }
            self.perp_collateral = Some(m.quote);
            if self.insurance.is_none() {
                let name = self.config.insurance_account.clone();
                self.insurance = Some(self.account_for(&name, out));
            }
            self.perps.insert(m.id, PerpState::default());
        }
        self.market_by_symbol.insert(m.symbol.clone(), m.id);
        self.markets.push(Market {
            spec: m.clone(),
            book: Book::default(),
            halted: false,
        });
        out.push(Event::MarketAdded { market: m.clone() });
        Ok(Ack::Ok)
    }

    fn account_for(&mut self, name: &str, out: &mut Vec<Event>) -> AccountId {
        if let Some(id) = self.account_by_name.get(name) {
            return *id;
        }
        let id = self.accounts.len() as AccountId;
        self.accounts.push(Account {
            name: name.to_string(),
            ..Default::default()
        });
        self.account_by_name.insert(name.to_string(), id);
        out.push(Event::AccountCreated {
            account: id,
            name: name.to_string(),
        });
        id
    }

    /// Chain events always advance the cursor when they are next in sequence, even if their
    /// payload can't be applied (e.g. a token the engine doesn't list); that case returns an error
    /// after advancing so the stream never stalls.
    fn chain(&mut self, ev: &ChainEvent, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        let seq = ev.seq();
        if seq <= self.chain_cursor {
            return Ok(Ack::Duplicate);
        }
        if seq != self.chain_cursor + 1 {
            return Err(Reject::ChainEventGap);
        }
        self.chain_cursor = seq;
        match ev {
            ChainEvent::Deposit {
                account, token, amount, ..
            } => {
                let asset = *self.asset_by_ref.get(token).ok_or(Reject::UnknownAsset)?;
                let id = self.account_for(account, out);
                self.accounts[id as usize].bal(asset).total += amount;
                out.push(Event::Deposited {
                    account: id,
                    asset,
                    amount: *amount,
                });
            }
            ChainEvent::KeyAdded { account, public_key, .. } => {
                let id = self.account_for(account, out);
                let a = &mut self.accounts[id as usize];
                if !a.keys.contains(public_key) {
                    a.keys.push(*public_key);
                }
                out.push(Event::KeyAdded {
                    account: id,
                    public_key: *public_key,
                });
            }
            ChainEvent::KeyRemoved { account, public_key, .. } => {
                let id = *self.account_by_name.get(account).ok_or(Reject::UnknownAccount)?;
                self.accounts[id as usize].keys.retain(|k| k != public_key);
                out.push(Event::KeyRemoved {
                    account: id,
                    public_key: *public_key,
                });
            }
            ChainEvent::ForcedWithdrawal {
                request_id,
                account,
                token,
                amount,
                ..
            } => {
                let id = self.account_for(account, out);
                // Unknown token: queue a zero payout so the contract still marks the request done.
                let asset = self.asset_by_ref.get(token).copied();
                let paid = match asset {
                    Some(asset) => {
                        self.cancel_all(id, None, CancelReason::ForcedWithdrawal, out)?;
                        let paid = (*amount).min(self.withdrawable(id, asset));
                        self.debit_collateral(id, asset, paid);
                        out.push(Event::ForcedQueued {
                            request_id: *request_id,
                            account: id,
                            asset,
                            amount: paid,
                        });
                        paid
                    }
                    None => 0,
                };
                self.settle.forced.push((*request_id, id, asset.unwrap_or(AssetId::MAX), paid));
            }
            ChainEvent::WithdrawalReverted {
                account, token, amount, ..
            } => {
                let asset = *self.asset_by_ref.get(token).ok_or(Reject::UnknownAsset)?;
                let id = self.account_for(account, out);
                self.accounts[id as usize].bal(asset).total += amount;
                out.push(Event::Credited {
                    account: id,
                    asset,
                    amount: *amount,
                });
            }
            ChainEvent::BatchSettled { batch_id, .. } => {
                while self.settle.unconfirmed.front().is_some_and(|b| b.id <= *batch_id) {
                    self.settle.unconfirmed.pop_front();
                }
                self.settle.last_settled = self.settle.last_settled.max(*batch_id);
                out.push(Event::BatchSettled { batch_id: *batch_id });
            }
        }
        Ok(Ack::Ok)
    }

    fn place(&mut self, o: &NewOrder, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        let r = self.place_inner(o, out);
        if let Err(reason) = r {
            out.push(Event::Rejected {
                account: o.account,
                client_id: o.client_id,
                reason,
            });
        }
        r
    }

    fn place_inner(&mut self, o: &NewOrder, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        let mi = o.market as usize;
        let market = self.markets.get(mi).ok_or(Reject::UnknownMarket)?;
        if market.halted {
            return Err(Reject::MarketHalted);
        }
        let spec = &market.spec;
        if o.qty < spec.min_qty || o.qty > spec.max_qty {
            return Err(Reject::InvalidQty);
        }
        if o.price == 0 || o.price > spec.max_price {
            return Err(Reject::InvalidPrice);
        }
        let acct = self.accounts.get(o.account as usize).ok_or(Reject::UnknownAccount)?;
        if o.account == FEE_ACCOUNT {
            return Err(Reject::UnknownAccount);
        }
        if o.client_id != 0 && acct.client.contains_key(&(o.market, o.client_id)) {
            return Err(Reject::DuplicateClientId);
        }
        let rests = matches!(o.tif, Tif::Gtc | Tif::PostOnly);
        if rests && acct.open_per_market.get(&o.market).copied().unwrap_or(0) >= spec.max_open_orders {
            return Err(Reject::TooManyOpenOrders);
        }
        let is_perp = spec.perp.is_some();
        let hold_asset = match o.side {
            Side::Buy => spec.quote,
            Side::Sell => spec.base,
        };
        // Perps reserve margin instead of holding balances.
        let per_lot = if is_perp { 0 } else { market.hold_per_lot(o.side, o.price) };
        let hold = per_lot * o.qty as u128;
        if is_perp {
            self.perp_check_order(o)?;
        } else if acct.balance(hold_asset).available() < hold || self.withdrawable(o.account, hold_asset) < hold {
            return Err(Reject::InsufficientBalance);
        }
        let market = &self.markets[mi];
        let crosses = |best: Option<u64>| match (o.side, best) {
            (Side::Buy, Some(a)) => a <= o.price,
            (Side::Sell, Some(b)) => b >= o.price,
            _ => false,
        };
        let opp = market.book.best(o.side.opposite());
        if o.tif == Tif::PostOnly && crosses(opp) {
            return Err(Reject::PostOnlyWouldCross);
        }
        if o.tif == Tif::Fok && self.fillable(o) < o.qty {
            return Err(Reject::FokUnfillable);
        }

        // Accepted: take the hold, assign an id, match.
        if !is_perp {
            self.accounts[o.account as usize].bal(hold_asset).held += hold;
        }
        let id = self.next_order_id;
        self.next_order_id += 1;
        out.push(Event::Accepted {
            order_id: id,
            account: o.account,
            market: o.market,
            side: o.side,
            price: o.price,
            qty: o.qty,
            client_id: o.client_id,
            tif: o.tif,
        });

        let (filled, taker_killed) = if crosses(opp) { self.match_order(id, o, out) } else { (0, false) };
        let rem = o.qty - filled;
        let status = if rem == 0 {
            OrderStatus::Filled
        } else if rests && !taker_killed {
            let m = &mut self.markets[mi];
            m.book.insert(id, o.account, o.side, o.price, rem, o.client_id, o.ts_ns);
            let lvl = m.book.levels(o.side)[&o.price].qty;
            out.push(Event::Level {
                market: o.market,
                side: o.side,
                price: o.price,
                qty: lvl,
            });
            self.order_market.insert(id, o.market);
            let a = &mut self.accounts[o.account as usize];
            a.open.insert(id, (o.market, o.client_id));
            if o.client_id != 0 {
                a.client.insert((o.market, o.client_id), id);
            }
            *a.open_per_market.entry(o.market).or_insert(0) += 1;
            if is_perp {
                self.perp_open_add(o.account, o.market, o.side, rem);
            }
            OrderStatus::Resting
        } else {
            if !is_perp {
                self.accounts[o.account as usize].bal(hold_asset).held -= per_lot * rem as u128;
            }
            let reason = if taker_killed {
                CancelReason::SelfTrade
            } else {
                CancelReason::Unfilled
            };
            out.push(Event::Canceled {
                order_id: id,
                account: o.account,
                market: o.market,
                remaining: rem,
                reason,
            });
            OrderStatus::Canceled
        };
        Ok(Ack::Placed {
            order_id: id,
            status,
            filled,
            remaining: if status == OrderStatus::Resting { rem } else { 0 },
        })
    }

    /// Quantity a FOK order would fill, walking the book exactly as matching would (including
    /// self-trade prevention, which can stop the walk).
    fn fillable(&self, o: &NewOrder) -> u64 {
        let book = &self.markets[o.market as usize].book;
        let opp = o.side.opposite();
        let prices: Box<dyn Iterator<Item = &u64>> = match o.side {
            Side::Buy => Box::new(book.asks.range(..=o.price).map(|(p, _)| p)),
            Side::Sell => Box::new(book.bids.range(o.price..).rev().map(|(p, _)| p)),
        };
        let mut sum = 0u64;
        for p in prices {
            let mut s = book.head(opp, *p);
            while s != NIL {
                let n = book.node(s);
                if n.account == o.account {
                    match o.stp {
                        Stp::CancelMaker => {}
                        Stp::CancelTaker | Stp::CancelBoth => return sum,
                    }
                } else {
                    sum += n.rem;
                    if sum >= o.qty {
                        return sum;
                    }
                }
                s = book.next_slot(s);
            }
        }
        sum
    }

    /// Match an incoming order against the book. Returns (filled lots, whether STP killed the
    /// incoming order's remainder).
    pub(crate) fn match_order(&mut self, taker_id: OrderId, o: &NewOrder, out: &mut Vec<Event>) -> (u64, bool) {
        let mi = o.market as usize;
        let (base, quote, base_lot, tick, mk_bps, tk_bps) = {
            let s = &self.markets[mi].spec;
            (s.base, s.quote, s.base_lot, s.quote_tick, s.maker_fee_bps, s.taker_fee_bps)
        };
        let opp = o.side.opposite();
        let is_perp = self.markets[mi].spec.perp.is_some();
        let taker_per_lot = if is_perp {
            0
        } else {
            self.markets[mi].hold_per_lot(o.side, o.price)
        };
        let mut rem = o.qty;
        let mut killed = false;
        'levels: while rem > 0 {
            let Some(p) = self.markets[mi].book.best(opp) else { break };
            if (o.side == Side::Buy && p > o.price) || (o.side == Side::Sell && p < o.price) {
                break;
            }
            let maker_per_lot = if is_perp { 0 } else { self.markets[mi].hold_per_lot(opp, p) };
            let mut s = self.markets[mi].book.head(opp, p);
            let mut level_left = 0u64;
            while s != NIL && rem > 0 {
                let next = self.markets[mi].book.next_slot(s);
                let (mid, macct, mrem) = {
                    let n = self.markets[mi].book.node(s);
                    (n.id, n.account, n.rem)
                };
                if macct == o.account {
                    match o.stp {
                        Stp::CancelTaker => {
                            killed = true;
                            level_left = self.markets[mi].book.levels(opp)[&p].qty;
                            out.push(Event::Level {
                                market: o.market,
                                side: opp,
                                price: p,
                                qty: level_left,
                            });
                            break 'levels;
                        }
                        Stp::CancelMaker | Stp::CancelBoth => {
                            let (_, left) = self.remove_resting(mi, s, CancelReason::SelfTrade, out);
                            level_left = left;
                            if o.stp == Stp::CancelBoth {
                                killed = true;
                                out.push(Event::Level {
                                    market: o.market,
                                    side: opp,
                                    price: p,
                                    qty: level_left,
                                });
                                break 'levels;
                            }
                            s = next;
                            continue;
                        }
                    }
                }
                let q = rem.min(mrem);
                let notional = q as u128 * p as u128 * tick;
                let base_amt = q as u128 * base_lot;
                let mfee = fee(notional, mk_bps);
                let tfee = fee(notional, tk_bps);
                let (buyer, seller, bfee, sfee) = match o.side {
                    Side::Buy => (o.account, macct, tfee, mfee),
                    Side::Sell => (macct, o.account, mfee, tfee),
                };
                if is_perp {
                    self.perp_open_sub(macct, o.market, opp, q);
                    self.perp_fill(o.market, buyer, seller, q, p, bfee, sfee, out);
                } else {
                    // Release holds: the taker's at its limit price, the maker's at its resting price.
                    let (t_rel, m_rel) = (taker_per_lot * q as u128, maker_per_lot * q as u128);
                    {
                        let t_asset = if o.side == Side::Buy { quote } else { base };
                        let m_asset = if o.side == Side::Buy { base } else { quote };
                        self.accounts[o.account as usize].bal(t_asset).held -= t_rel;
                        self.accounts[macct as usize].bal(m_asset).held -= m_rel;
                    }
                    let paid = notional + bfee;
                    let got = notional - sfee;
                    {
                        let b = &mut self.accounts[buyer as usize];
                        b.bal(quote).total -= paid;
                        b.bal(base).total += base_amt;
                    }
                    {
                        let sl = &mut self.accounts[seller as usize];
                        sl.bal(base).total -= base_amt;
                        sl.bal(quote).total += got;
                    }
                    self.accounts[FEE_ACCOUNT as usize].bal(quote).total += bfee + sfee;
                    let st = &mut self.settle;
                    st.add(buyer, quote, -(paid as i128));
                    st.add(buyer, base, base_amt as i128);
                    st.add(seller, base, -(base_amt as i128));
                    st.add(seller, quote, got as i128);
                    st.add(FEE_ACCOUNT, quote, (bfee + sfee) as i128);
                }

                let (mleft, lvl) = self.markets[mi].book.reduce(s, q);
                level_left = lvl;
                rem -= q;
                let trade_id = self.next_trade_id;
                self.next_trade_id += 1;
                out.push(Event::Fill {
                    trade_id,
                    market: o.market,
                    price: p,
                    qty: q,
                    taker_side: o.side,
                    maker: macct,
                    maker_order: mid,
                    maker_fee: mfee,
                    maker_remaining: mleft,
                    taker: o.account,
                    taker_order: taker_id,
                    taker_fee: tfee,
                    ts_ns: o.ts_ns,
                });
                if mleft == 0 {
                    let (_, left) = self.remove_resting_filled(mi, s);
                    level_left = left;
                }
                s = next;
            }
            out.push(Event::Level {
                market: o.market,
                side: opp,
                price: p,
                qty: level_left,
            });
        }
        (o.qty - rem, killed)
    }

    /// Remove a fully filled resting order (no hold left to release).
    fn remove_resting_filled(&mut self, mi: usize, slot: u32) -> (OrderId, u64) {
        let (n, left) = self.markets[mi].book.remove(slot);
        self.forget_open(n.account, n.id, mi as MarketId, n.client_id);
        (n.id, left)
    }

    /// Cancel a resting order: unlink, release its hold, emit Canceled.
    fn remove_resting(&mut self, mi: usize, slot: u32, reason: CancelReason, out: &mut Vec<Event>) -> (OrderId, u64) {
        let m = &mut self.markets[mi];
        let (n, left) = m.book.remove(slot);
        if m.spec.perp.is_some() {
            self.perp_open_sub(n.account, mi as MarketId, n.side, n.rem);
        } else {
            let per_lot = m.hold_per_lot(n.side, n.price);
            let asset = if n.side == Side::Buy { m.spec.quote } else { m.spec.base };
            self.accounts[n.account as usize].bal(asset).held -= per_lot * n.rem as u128;
        }
        self.forget_open(n.account, n.id, mi as MarketId, n.client_id);
        out.push(Event::Canceled {
            order_id: n.id,
            account: n.account,
            market: mi as MarketId,
            remaining: n.rem,
            reason,
        });
        (n.id, left)
    }

    fn forget_open(&mut self, account: AccountId, id: OrderId, market: MarketId, client_id: u64) {
        self.order_market.remove(&id);
        let a = &mut self.accounts[account as usize];
        a.open.remove(&id);
        if client_id != 0 {
            a.client.remove(&(market, client_id));
        }
        if let Some(c) = a.open_per_market.get_mut(&market) {
            *c -= 1;
            if *c == 0 {
                a.open_per_market.remove(&market);
            }
        }
    }

    fn cancel(&mut self, account: AccountId, id: OrderId, reason: CancelReason, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        let mi = *self.order_market.get(&id).ok_or(Reject::UnknownOrder)? as usize;
        let slot = self.markets[mi].book.slot(id).ok_or(Reject::UnknownOrder)?;
        let n = self.markets[mi].book.node(slot);
        if n.account != account {
            // Same error as a missing order: don't reveal other accounts' order ids.
            return Err(Reject::UnknownOrder);
        }
        let (side, price, rem) = (n.side, n.price, n.rem);
        let (_, left) = self.remove_resting(mi, slot, reason, out);
        out.push(Event::Level {
            market: mi as MarketId,
            side,
            price,
            qty: left,
        });
        Ok(Ack::Canceled {
            order_id: id,
            remaining: rem,
        })
    }

    pub(crate) fn cancel_all(
        &mut self,
        account: AccountId,
        market: Option<MarketId>,
        reason: CancelReason,
        out: &mut Vec<Event>,
    ) -> Result<u32, Reject> {
        let a = self.accounts.get(account as usize).ok_or(Reject::UnknownAccount)?;
        let mut ids: Vec<OrderId> = a
            .open
            .iter()
            .filter(|(_, (m, _))| market.is_none_or(|x| x == *m))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in &ids {
            self.cancel(account, *id, reason, out)?;
        }
        Ok(ids.len() as u32)
    }

    #[allow(clippy::too_many_arguments)]
    fn withdraw(
        &mut self,
        account: AccountId,
        asset: AssetId,
        amount: u128,
        nonce: u64,
        public_key: Key,
        signature: &[u8],
        out: &mut Vec<Event>,
    ) -> Result<Ack, Reject> {
        if asset as usize >= self.assets.len() {
            return Err(Reject::UnknownAsset);
        }
        if account == FEE_ACCOUNT {
            return Err(Reject::UnknownAccount);
        }
        let a = self.accounts.get(account as usize).ok_or(Reject::UnknownAccount)?;
        if !a.keys.contains(&public_key) {
            return Err(Reject::KeyNotRegistered);
        }
        if nonce <= a.withdraw_nonce {
            return Err(Reject::NonceTooLow);
        }
        if amount == 0 {
            return Err(Reject::InvalidQty);
        }
        if self.withdrawable(account, asset) < amount {
            return Err(Reject::InsufficientBalance);
        }
        self.debit_collateral(account, asset, amount);
        self.accounts[account as usize].withdraw_nonce = nonce;
        self.settle
            .withdrawals
            .push((account, asset, amount, nonce, public_key, signature.to_vec()));
        out.push(Event::WithdrawQueued {
            account,
            asset,
            amount,
            nonce,
        });
        Ok(Ack::WithdrawQueued { nonce })
    }

    /// Turn the accumulator into batches. Entries are sorted by (account, asset) and packed whole
    /// accounts at a time; withdrawals and forced withdrawals follow in later slots (after every
    /// delta, so a withdrawal funded by this group's trades is covered on-chain); the last batch
    /// is sealed.
    fn cut(&mut self, journal_seq: u64, journal_hash: [u8; 32], out: &mut Vec<Event>) -> Ack {
        self.perp_cut(out);
        let first = self.settle.next_batch_id;
        if self.settle.is_empty() {
            return Ack::BatchesCut { first, count: 0 };
        }
        let mut deltas: Vec<((AccountId, AssetId), i128)> = self.settle.deltas.drain().filter(|(_, d)| *d != 0).collect();
        deltas.sort_unstable_by_key(|(k, _)| *k);
        let withdrawals = std::mem::take(&mut self.settle.withdrawals);
        let forced = std::mem::take(&mut self.settle.forced);

        let mut batches: Vec<Batch> = vec![];
        let new_batch = |id| Batch {
            id,
            entries: vec![],
            withdrawals: vec![],
            forced: vec![],
            seal: false,
            journal_seq,
            journal_hash,
        };
        let mut cur = new_batch(first);
        let mut i = 0;
        while i < deltas.len() {
            let acct = deltas[i].0.0;
            let j = i + deltas[i..].iter().take_while(|(k, _)| k.0 == acct).count();
            if !cur.entries.is_empty() && cur.entries.len() + (j - i) > self.config.max_batch_entries {
                let id = cur.id + 1;
                batches.push(std::mem::replace(&mut cur, new_batch(id)));
            }
            for ((a, s), d) in &deltas[i..j] {
                cur.entries.push(DeltaEntry {
                    account: self.accounts[*a as usize].name.clone(),
                    token: self.assets[*s as usize].chain_ref.clone(),
                    delta: *d,
                });
            }
            i = j;
        }
        let token = |s: AssetId, assets: &[AssetSpec]| assets.get(s as usize).map_or(String::new(), |x| x.chain_ref.clone());
        let mut slots = 0usize;
        let max_w = self.config.max_batch_withdrawals.max(1);
        let mut push_slot = |cur: &mut Batch, batches: &mut Vec<Batch>| {
            if slots == max_w {
                let id = cur.id + 1;
                batches.push(std::mem::replace(cur, new_batch(id)));
                slots = 0;
            }
            slots += 1;
        };
        for (a, s, amount, nonce, pk, sig) in withdrawals {
            push_slot(&mut cur, &mut batches);
            cur.withdrawals.push(WithdrawEntry {
                account: self.accounts[a as usize].name.clone(),
                token: token(s, &self.assets),
                amount,
                nonce,
                public_key: pk,
                signature: sig,
            });
        }
        for (rid, a, s, amount) in forced {
            push_slot(&mut cur, &mut batches);
            cur.forced.push(ForcedEntry {
                request_id: rid,
                account: self.accounts[a as usize].name.clone(),
                token: token(s, &self.assets),
                amount,
            });
        }
        cur.seal = true;
        batches.push(cur);
        let count = batches.len() as u32;
        self.settle.next_batch_id = first + count as u64;
        for b in batches {
            out.push(Event::BatchCut { batch: b.clone() });
            self.settle.unconfirmed.push_back(b);
        }
        Ack::BatchesCut { first, count }
    }

    // ---- read-only views (not commands; never journaled) ----

    pub fn account_id(&self, name: &str) -> Option<AccountId> {
        self.account_by_name.get(name).copied()
    }

    pub fn market_id(&self, symbol: &str) -> Option<MarketId> {
        self.market_by_symbol.get(symbol).copied()
    }

    /// Open orders of an account, sorted by id: (id, market, side, price, remaining, client_id).
    pub fn open_orders(&self, account: AccountId) -> Vec<(OrderId, MarketId, Side, u64, u64, u64)> {
        let Some(a) = self.accounts.get(account as usize) else {
            return vec![];
        };
        let mut v: Vec<_> = a
            .open
            .iter()
            .filter_map(|(id, (m, _))| {
                self.markets[*m as usize]
                    .book
                    .get(*id)
                    .map(|n| (*id, *m, n.side, n.price, n.rem, n.client_id))
            })
            .collect();
        v.sort_unstable_by_key(|x| x.0);
        v
    }

    /// Sum of `total` per asset across all accounts. With nothing in flight this equals deposits
    /// minus withdrawals; used by invariant checks.
    pub fn totals(&self) -> Vec<u128> {
        let mut t = vec![0u128; self.assets.len()];
        for a in &self.accounts {
            for (i, b) in a.balances.iter().enumerate() {
                t[i] += b.total;
            }
        }
        t
    }

    /// Recompute every hold from the books and compare with the balances, and check each book.
    /// Test helper; O(orders + accounts).
    pub fn check_invariants(&self) -> Result<(), String> {
        let mut held: FxHashMap<(AccountId, AssetId), u128> = FxHashMap::default();
        let mut open = 0usize;
        for m in &self.markets {
            m.book.check()?;
            for side in [Side::Buy, Side::Sell] {
                for (price, orders) in m.book.l3(side) {
                    let asset = if side == Side::Buy { m.spec.quote } else { m.spec.base };
                    for (_, acct, rem, _) in orders {
                        // Perp orders reserve margin, not balances.
                        if m.spec.perp.is_none() {
                            *held.entry((acct, asset)).or_insert(0) += m.hold_per_lot(side, price) * rem as u128;
                        }
                        open += 1;
                    }
                }
            }
        }
        if open != self.order_market.len() {
            return Err(format!("order_market has {} entries, books {open}", self.order_market.len()));
        }
        for (id, a) in self.accounts.iter().enumerate() {
            for (s, b) in a.balances.iter().enumerate() {
                let want = held.get(&(id as AccountId, s as AssetId)).copied().unwrap_or(0);
                if b.held != want || b.held > b.total {
                    return Err(format!("account {id} asset {s}: held {} want {want} total {}", b.held, b.total));
                }
            }
        }
        // Trades conserve every asset: pending deltas sum to zero per asset. For the perp
        // collateral, perp value changes not yet cut (view − last_view) complete the sum.
        let mut sums: FxHashMap<AssetId, i128> = FxHashMap::default();
        for ((_, s), d) in &self.settle.deltas {
            *sums.entry(*s).or_insert(0) += d;
        }
        if let Some(c) = self.perp_collateral {
            let unsettled: i128 = (0..self.accounts.len() as AccountId)
                .map(|a| self.perp_view(a) - self.accounts[a as usize].perp.last_view)
                .sum();
            *sums.entry(c).or_insert(0) += unsettled;
        }
        self.perp_invariants()?;
        if let Some((s, d)) = sums.iter().find(|(_, d)| **d != 0) {
            return Err(format!("pending deltas for asset {s} sum to {d}"));
        }
        Ok(())
    }

    /// Hash of the logical state, independent of hash-map layout (so a snapshot-restored engine
    /// and a live one hash the same).
    pub fn state_hash(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        let mut put = |b: &[u8]| {
            h.update(&(b.len() as u64).to_le_bytes());
            h.update(b);
        };
        let enc = |v: &dyn erased::Ser| v.bytes();
        put(&enc(&(&self.assets, self.next_order_id, self.next_trade_id, self.chain_cursor)));
        put(&enc(&(
            &self.perps,
            self.perp_collateral,
            self.insurance,
            &self.perp_dirty,
            &self.marks_changed,
        )));
        for m in &self.markets {
            put(&enc(&(&m.spec, m.halted, m.book.l3(Side::Buy), m.book.l3(Side::Sell))));
        }
        for a in &self.accounts {
            let mut keys = a.keys.clone();
            keys.sort_unstable();
            let mut open: Vec<_> = a.open.iter().map(|(k, v)| (*k, *v)).collect();
            open.sort_unstable();
            let bals: Vec<Balance> = a.balances.to_vec();
            put(&enc(&(&a.name, &bals, keys, open, a.withdraw_nonce, &a.perp)));
        }
        let mut d: Vec<_> = self.settle.deltas.iter().filter(|(_, v)| **v != 0).map(|(k, v)| (*k, *v)).collect();
        d.sort_unstable();
        put(&enc(&(
            d,
            &self.settle.withdrawals,
            &self.settle.forced,
            self.settle.next_batch_id,
            &self.settle.unconfirmed,
            self.settle.last_settled,
        )));
        *h.finalize().as_bytes()
    }

    pub fn to_snapshot(&self) -> Vec<u8> {
        bincode::serialize(self).expect("engine serializes")
    }

    pub fn from_snapshot(b: &[u8]) -> Result<Self, String> {
        bincode::deserialize(b).map_err(|e| e.to_string())
    }

    /// Distinct accounts touched by an open-order set; used by tests.
    pub fn accounts_with_orders(&self) -> FxHashSet<AccountId> {
        self.accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.open.is_empty())
            .map(|(i, _)| i as AccountId)
            .collect()
    }
}

mod erased {
    pub trait Ser {
        fn bytes(&self) -> Vec<u8>;
    }
    impl<T: serde::Serialize> Ser for T {
        fn bytes(&self) -> Vec<u8> {
            bincode::serialize(self).expect("serialize")
        }
    }
}
