//! USDC-margined linear perpetuals with cross margin.
//!
//! ## Accounting
//!
//! Each account has a signed perp `cash` (quote atoms) next to its real collateral balance. A fill
//! of `q` lots at `p` moves `q·p·tick` of cash from buyer to seller and `q` lots of position from
//! seller to buyer, exactly like spot but with a virtual base. Fees, funding and liquidation fees
//! are cash transfers. Every one of these is zero-sum, and positions always net to zero, so
//!
//! ```text
//! equity(a) = collateral_available(a) + cash(a) + Σ size(a,m)·mark(m)·tick(m)
//! Σ_a [collateral(a) + cash(a)] = deposits − withdrawals          (positions value to 0 in sum)
//! ```
//!
//! ## Settlement
//!
//! The chain only knows collateral balances. At every batch cut each touched account (and, when a
//! mark moved, every holder in that market) settles `view = cash + Σ size·mark·tick` at the current
//! marks: the batch carries `view − last_view` as a collateral delta. Because every holder of a
//! market is marked at the same price, the deltas still sum to zero per token, and because
//! accounts are kept solvent at the current mark (liquidation, then bankruptcy cover, then
//! socialization), every on-chain balance stays non-negative.
//!
//! ## Margin
//!
//! `IM = Σ max(|size + open_buys|, |size − open_sells|) · mark · tick · imr`,
//! `MM = Σ |size| · mark · tick · mmr`. A new order must leave `equity − IM − fee − adverse ≥ 0`
//! (`adverse` is the immediate loss of trading away from the mark) unless it doesn't raise IM.
//! An account with `equity < MM` at a mark update is liquidated: orders canceled, positions closed
//! on the book within `liq_slippage_bps` of the mark, the rest taken over by the insurance fund at
//! the mark, a liquidation fee paid to insurance, any deficit covered by insurance, and anything
//! insurance can't cover clawed back pro rata from accounts in profit.

use crate::engine::{Engine, fee};
use crate::types::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// Signed lots: positive long, negative short.
    pub size: i64,
    /// Entry notional of the open portion (quote atoms); `entry / |size| / tick` is the average price.
    pub entry: u128,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountPerp {
    pub cash: i128,
    pub positions: BTreeMap<MarketId, Position>,
    /// Resting perp order quantity per market (lots).
    pub open_buy: BTreeMap<MarketId, u64>,
    pub open_sell: BTreeMap<MarketId, u64>,
    /// `cash + Σ size·mark·tick` as of the last batch cut (already on-chain once that batch lands).
    pub last_view: i128,
}

impl AccountPerp {
    pub fn has_exposure(&self) -> bool {
        self.cash != 0 || self.last_view != 0 || !self.positions.is_empty() || !self.open_buy.is_empty() || !self.open_sell.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerpState {
    pub mark: u64,
    pub index: u64,
    pub holders: BTreeSet<AccountId>,
    /// Long open interest in lots (equals short open interest).
    pub oi: u64,
    pub premium_sum_ppm: i128,
    pub premium_n: u64,
    pub last_rate_ppm: i64,
    /// Sum of all funding payments per lot paid by longs since listing.
    pub cum_funding_per_lot: i128,
}

/// Margin summary of one account at the current marks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarginSummary {
    pub equity: i128,
    pub initial: i128,
    pub maintenance: i128,
    /// Withdrawable / usable for new exposure: `max(0, equity − initial)`.
    pub free: i128,
}

fn nl(lots: i128, price: u64, tick: u128) -> i128 {
    lots * price as i128 * tick as i128
}

fn bps(x: i128, b: u16) -> i128 {
    // Round up: requirements are conservative.
    (x * b as i128 + 9_999) / 10_000
}

impl Engine {
    pub(crate) fn perp_params(&self, m: MarketId) -> Option<&PerpParams> {
        self.markets.get(m as usize).and_then(|x| x.spec.perp.as_ref())
    }

    fn tick(&self, m: MarketId) -> u128 {
        self.markets[m as usize].spec.quote_tick
    }

    fn mark(&self, m: MarketId) -> u64 {
        self.perps.get(&m).map_or(0, |s| s.mark)
    }

    /// Unsettled perp value: `cash + Σ size·mark·tick` at current marks.
    pub fn perp_view(&self, a: AccountId) -> i128 {
        let p = &self.accounts[a as usize].perp;
        p.cash
            + p.positions
                .iter()
                .map(|(m, pos)| nl(pos.size as i128, self.mark(*m), self.tick(*m)))
                .sum::<i128>()
    }

    /// Cross-margin equity: available collateral + perp view.
    pub fn equity(&self, a: AccountId) -> i128 {
        let coll = self
            .perp_collateral
            .map_or(0, |c| self.accounts[a as usize].balance(c).available() as i128);
        coll + self.perp_view(a)
    }

    /// (initial, maintenance) requirement, optionally with an extra hypothetical resting order.
    pub fn requirements(&self, a: AccountId, extra: Option<(MarketId, Side, u64)>) -> (i128, i128) {
        let p = &self.accounts[a as usize].perp;
        let mut markets: BTreeSet<MarketId> = p
            .positions
            .keys()
            .chain(p.open_buy.keys())
            .chain(p.open_sell.keys())
            .copied()
            .collect();
        if let Some((m, _, _)) = extra {
            markets.insert(m);
        }
        let (mut im, mut mm) = (0i128, 0i128);
        for m in markets {
            let Some(pp) = self.perp_params(m) else { continue };
            let size = p.positions.get(&m).map_or(0, |x| x.size) as i128;
            let mut ob = p.open_buy.get(&m).copied().unwrap_or(0) as i128;
            let mut os = p.open_sell.get(&m).copied().unwrap_or(0) as i128;
            if let Some((em, side, q)) = extra
                && em == m
            {
                match side {
                    Side::Buy => ob += q as i128,
                    Side::Sell => os += q as i128,
                }
            }
            let worst = (size + ob).abs().max((size - os).abs());
            let (mark, tick) = (self.mark(m), self.tick(m));
            im += bps(nl(worst, mark, tick), pp.imr_bps);
            mm += bps(nl(size.abs(), mark, tick), pp.mmr_bps);
        }
        (im, mm)
    }

    pub fn margin_summary(&self, a: AccountId) -> MarginSummary {
        let equity = self.equity(a);
        let (initial, maintenance) = self.requirements(a, None);
        MarginSummary {
            equity,
            initial,
            maintenance,
            free: (equity - initial).max(0),
        }
    }

    fn exposed(&self, a: AccountId) -> bool {
        self.accounts[a as usize].perp.has_exposure()
    }

    /// How much of `asset` the account may take out (withdraw, or hold for a spot buy) without
    /// breaking its perp margin.
    pub fn withdrawable(&self, a: AccountId, asset: AssetId) -> u128 {
        let avail = self.accounts[a as usize].balance(asset).available();
        if Some(asset) != self.perp_collateral || !self.exposed(a) {
            return avail;
        }
        let ms = self.margin_summary(a);
        ms.free.max(0) as u128
    }

    /// Take `amount` of collateral out of the account (withdrawals). If it exceeds the available
    /// balance, the difference is realized from perp value first: moved from perp cash into the
    /// collateral balance, with a matching settlement delta so the chain sees no net change.
    pub(crate) fn debit_collateral(&mut self, a: AccountId, asset: AssetId, amount: u128) {
        let avail = self.accounts[a as usize].balance(asset).available();
        if Some(asset) == self.perp_collateral && amount > avail {
            let r = amount - avail;
            let acc = &mut self.accounts[a as usize];
            acc.perp.cash -= r as i128;
            acc.bal_mut(asset).total += r;
            self.settle_add(a, asset, r as i128);
            self.perp_dirty.insert(a);
        }
        self.accounts[a as usize].bal_mut(asset).total -= amount;
    }

    /// Pre-trade checks for a perp order: mark known, price within band, margin.
    pub(crate) fn perp_check_order(&self, o: &NewOrder) -> Result<(), Reject> {
        let pp = self.perp_params(o.market).ok_or(Reject::NotAPerp)?;
        let mark = self.mark(o.market);
        if mark == 0 {
            return Err(Reject::MarkUnavailable);
        }
        if (o.price.abs_diff(mark) as u128) * 10_000 > mark as u128 * pp.price_band_bps as u128 {
            return Err(Reject::PriceOutOfBand);
        }
        let (im_old, _) = self.requirements(o.account, None);
        let (im_new, _) = self.requirements(o.account, Some((o.market, o.side, o.qty)));
        if im_new > im_old {
            let tick = self.tick(o.market);
            let spec = &self.markets[o.market as usize].spec;
            let reserve = fee(o.qty as u128 * o.price as u128 * tick, spec.taker_fee_bps) as i128;
            let adverse = match o.side {
                Side::Buy => o.price.saturating_sub(mark),
                Side::Sell => mark.saturating_sub(o.price),
            };
            let adverse = nl(o.qty as i128, adverse, tick);
            if self.equity(o.account) - im_new - reserve - adverse < 0 {
                return Err(Reject::InsufficientMargin);
            }
        }
        Ok(())
    }

    pub(crate) fn perp_open_add(&mut self, a: AccountId, m: MarketId, side: Side, q: u64) {
        let p = &mut self.accounts[a as usize].perp;
        let map = if side == Side::Buy { &mut p.open_buy } else { &mut p.open_sell };
        *map.entry(m).or_insert(0) += q;
    }

    pub(crate) fn perp_open_sub(&mut self, a: AccountId, m: MarketId, side: Side, q: u64) {
        let p = &mut self.accounts[a as usize].perp;
        let map = if side == Side::Buy { &mut p.open_buy } else { &mut p.open_sell };
        if let Some(v) = map.get_mut(&m) {
            *v -= q;
            if *v == 0 {
                map.remove(&m);
            }
        }
    }

    /// Settle one perp fill between `buyer` and `seller`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn perp_fill(
        &mut self,
        m: MarketId,
        buyer: AccountId,
        seller: AccountId,
        q: u64,
        p: u64,
        bfee: u128,
        sfee: u128,
        out: &mut Vec<Event>,
    ) {
        let tick = self.tick(m);
        let n = nl(q as i128, p, tick);
        self.accounts[buyer as usize].perp.cash -= n + bfee as i128;
        self.accounts[seller as usize].perp.cash += n - sfee as i128;
        if bfee + sfee > 0 {
            let c = self.perp_collateral.expect("perp market implies collateral");
            self.accounts[FEE_ACCOUNT as usize].bal_mut(c).total += bfee + sfee;
            self.settle_add(FEE_ACCOUNT, c, (bfee + sfee) as i128);
        }
        self.update_position(buyer, m, q as i64, p, out);
        self.update_position(seller, m, -(q as i64), p, out);
    }

    fn update_position(&mut self, a: AccountId, m: MarketId, d: i64, p: u64, out: &mut Vec<Event>) {
        let tick = self.tick(m);
        let pos = self.accounts[a as usize].perp.positions.entry(m).or_default();
        let old = pos.size;
        let new = old + d;
        let ad = d.unsigned_abs() as u128;
        if old == 0 || (old > 0) == (d > 0) {
            pos.entry += ad * p as u128 * tick;
        } else {
            let ao = old.unsigned_abs() as u128;
            let closed = ad.min(ao);
            if closed == ao {
                pos.entry = 0;
            } else {
                pos.entry -= pos.entry * closed / ao;
            }
            if ad > ao {
                pos.entry = (ad - ao) * p as u128 * tick;
            }
        }
        pos.size = new;
        let entry = pos.entry;
        if new == 0 {
            self.accounts[a as usize].perp.positions.remove(&m);
        }
        let st = self.perps.get_mut(&m).expect("perp state");
        if new == 0 {
            st.holders.remove(&a);
        } else {
            st.holders.insert(a);
        }
        st.oi = st.oi + new.max(0) as u64 - old.max(0) as u64;
        self.perp_dirty.insert(a);
        out.push(Event::Position {
            account: a,
            market: m,
            size: new,
            entry_notional: entry,
        });
    }

    pub(crate) fn set_mark(&mut self, m: MarketId, mark: u64, index: u64, ts_ns: u64, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        self.perp_params(m).ok_or(Reject::NotAPerp)?;
        let max = self.markets[m as usize].spec.max_price;
        if mark == 0 || index == 0 || mark > max || index > max {
            return Err(Reject::InvalidPrice);
        }
        let book = &self.markets[m as usize].book;
        let mid = match (book.best_bid(), book.best_ask()) {
            (Some(b), Some(a)) => Some((b as i128 + a as i128) / 2),
            _ => None,
        };
        let st = self.perps.get_mut(&m).unwrap();
        st.mark = mark;
        st.index = index;
        if let Some(mid) = mid {
            st.premium_sum_ppm += (mid - index as i128) * 1_000_000 / index as i128;
            st.premium_n += 1;
        }
        self.marks_changed.insert(m);
        out.push(Event::Mark { market: m, mark, index });
        let holders: Vec<AccountId> = self.perps[&m].holders.iter().copied().collect();
        self.liquidation_sweep(holders, ts_ns, out);
        self.ensure_insurance_solvent(out);
        Ok(Ack::Ok)
    }

    pub(crate) fn funding_tick(&mut self, m: MarketId, ts_ns: u64, out: &mut Vec<Event>) -> Result<Ack, Reject> {
        let pp = self.perp_params(m).ok_or(Reject::NotAPerp)?.clone();
        let tick = self.tick(m);
        let st = self.perps.get_mut(&m).unwrap();
        if st.mark == 0 {
            return Err(Reject::MarkUnavailable);
        }
        let avg = if st.premium_n > 0 {
            st.premium_sum_ppm / st.premium_n as i128
        } else {
            0
        };
        let cap = pp.max_funding_ppm as i128;
        let rate = (avg / pp.funding_divisor.max(1) as i128).clamp(-cap, cap);
        // Per-lot payment is one integer for everyone, so Σ size·pay = 0 exactly.
        let pay = rate * st.mark as i128 * tick as i128 / 1_000_000;
        st.premium_sum_ppm = 0;
        st.premium_n = 0;
        st.last_rate_ppm = rate as i64;
        st.cum_funding_per_lot += pay;
        let holders: Vec<AccountId> = st.holders.iter().copied().collect();
        for a in &holders {
            let size = self.accounts[*a as usize].perp.positions[&m].size as i128;
            self.accounts[*a as usize].perp.cash -= size * pay;
            self.perp_dirty.insert(*a);
        }
        out.push(Event::Funding {
            market: m,
            rate_ppm: rate as i64,
            payment_per_lot: pay,
        });
        self.liquidation_sweep(holders, ts_ns, out);
        self.ensure_insurance_solvent(out);
        Ok(Ack::Ok)
    }

    fn liquidation_sweep(&mut self, candidates: Vec<AccountId>, ts_ns: u64, out: &mut Vec<Event>) {
        for a in candidates {
            if Some(a) == self.insurance || a == FEE_ACCOUNT || self.accounts[a as usize].perp.positions.is_empty() {
                continue;
            }
            let eq = self.equity(a);
            let (_, mm) = self.requirements(a, None);
            if eq < mm {
                self.liquidate(a, eq, mm, ts_ns, out);
            }
        }
    }

    fn liquidate(&mut self, a: AccountId, equity: i128, maintenance: i128, ts_ns: u64, out: &mut Vec<Event>) {
        out.push(Event::Liquidated {
            account: a,
            equity,
            maintenance,
        });
        let _ = self.cancel_all_internal(a, CancelReason::Liquidation, out);
        let ins = self.insurance.expect("insurance account exists once a perp is listed");
        let positions: Vec<(MarketId, i64)> = self.accounts[a as usize].perp.positions.iter().map(|(m, p)| (*m, p.size)).collect();
        for (m, size) in positions {
            let pp = self.perp_params(m).unwrap().clone();
            let (mark, tick) = (self.mark(m), self.tick(m));
            let side = if size > 0 { Side::Sell } else { Side::Buy };
            let slip = (mark as u128 * pp.liq_slippage_bps as u128 / 10_000) as u64;
            let bound = match side {
                Side::Sell => mark.saturating_sub(slip).max(1),
                Side::Buy => mark.saturating_add(slip).min(self.markets[m as usize].spec.max_price),
            };
            let qty = size.unsigned_abs();
            let o = NewOrder {
                account: a,
                market: m,
                side,
                price: bound,
                qty,
                tif: Tif::Ioc,
                stp: Stp::CancelMaker,
                client_id: 0,
                ts_ns,
            };
            let id = self.next_order_id;
            self.next_order_id += 1;
            out.push(Event::Accepted {
                order_id: id,
                account: a,
                market: m,
                side,
                price: bound,
                qty,
                client_id: 0,
                tif: Tif::Ioc,
            });
            let filled = if self.markets[m as usize].book.best(side.opposite()).is_some() {
                self.match_order(id, &o, out).0
            } else {
                0
            };
            let rem = qty - filled;
            if rem > 0 {
                out.push(Event::Canceled {
                    order_id: id,
                    account: a,
                    market: m,
                    remaining: rem,
                    reason: CancelReason::Liquidation,
                });
                let (buyer, seller) = if side == Side::Sell { (ins, a) } else { (a, ins) };
                self.perp_fill(m, buyer, seller, rem, mark, 0, 0, out);
                out.push(Event::InsuranceTakeover {
                    account: a,
                    market: m,
                    size: if side == Side::Sell { rem as i64 } else { -(rem as i64) },
                    price: mark,
                });
            }
            let liq_fee = bps(nl(qty as i128, mark, tick), pp.liq_fee_bps);
            self.accounts[a as usize].perp.cash -= liq_fee;
            self.accounts[ins as usize].perp.cash += liq_fee;
            self.perp_dirty.insert(ins);
        }
        self.cover_bankruptcy(a, out);
    }

    /// If an account's equity is negative, the insurance fund pays the deficit; if that leaves the
    /// fund negative, the shortfall is socialized.
    pub(crate) fn cover_bankruptcy(&mut self, a: AccountId, out: &mut Vec<Event>) {
        let Some(ins) = self.insurance else { return };
        if a == ins {
            return;
        }
        let eq = self.equity(a);
        if eq >= 0 {
            return;
        }
        let deficit = -eq;
        self.accounts[a as usize].perp.cash += deficit;
        self.accounts[ins as usize].perp.cash -= deficit;
        self.perp_dirty.insert(a);
        self.perp_dirty.insert(ins);
        out.push(Event::Bankruptcy { account: a, deficit });
        self.ensure_insurance_solvent(out);
    }

    /// Keep the insurance fund solvent: if its equity is negative (it covered deficits, or a
    /// position it took over moved against it), claw the shortfall back.
    pub(crate) fn ensure_insurance_solvent(&mut self, out: &mut Vec<Event>) {
        let Some(ins) = self.insurance else { return };
        let eq = self.equity(ins);
        if eq < 0 {
            self.socialize(-eq, out);
        }
    }

    /// Claw `shortfall` back into the insurance fund. First pro rata from accounts whose perp value
    /// is positive (the winning side of the move), then, if that isn't enough, pro rata from every
    /// account's positive equity. Collateral is conserved, so the second tier always suffices
    /// while the exchange as a whole is solvent.
    fn socialize(&mut self, shortfall: i128, out: &mut Vec<Event>) {
        let ins = self.insurance.unwrap();
        let ids: Vec<AccountId> = (0..self.accounts.len() as AccountId)
            .filter(|a| *a != ins && *a != FEE_ACCOUNT)
            .collect();
        let mut taken_total = 0i128;
        let mut touched = BTreeSet::new();
        for tier in 0..2 {
            let need = shortfall - taken_total;
            if need <= 0 {
                break;
            }
            // Capacity per account: profit (tier 0) or remaining positive equity (tier 1).
            let caps: Vec<(AccountId, i128)> = ids
                .iter()
                .map(|a| (*a, if tier == 0 { self.perp_view(*a) } else { self.equity(*a) }))
                .filter(|(_, v)| *v > 0)
                .collect();
            let total: i128 = caps.iter().map(|(_, v)| v).sum();
            if total <= 0 {
                continue;
            }
            let target = need.min(total);
            let mut takes: Vec<i128> = caps.iter().map(|(_, v)| target * v / total).collect();
            let mut taken: i128 = takes.iter().sum();
            // Distribute the floor remainder one atom at a time, largest capacity first.
            let mut order: Vec<usize> = (0..caps.len()).collect();
            order.sort_by(|i, j| caps[*j].1.cmp(&caps[*i].1).then(caps[*i].0.cmp(&caps[*j].0)));
            let mut k = 0;
            while taken < target {
                let i = order[k % order.len()];
                if takes[i] < caps[i].1 {
                    takes[i] += 1;
                    taken += 1;
                }
                k += 1;
            }
            for ((a, _), t) in caps.iter().zip(&takes) {
                if *t > 0 {
                    self.accounts[*a as usize].perp.cash -= t;
                    self.perp_dirty.insert(*a);
                    touched.insert(*a);
                }
            }
            taken_total += taken;
        }
        self.accounts[ins as usize].perp.cash += taken_total;
        self.perp_dirty.insert(ins);
        out.push(Event::Socialized {
            amount: taken_total,
            from_accounts: touched.len() as u32,
        });
    }

    /// Batch-cut step: resolve any negative equity, then add `view − last_view` for every dirty
    /// account (all holders of markets whose mark moved) to the settlement accumulator.
    pub(crate) fn perp_cut(&mut self, out: &mut Vec<Event>) {
        let Some(c) = self.perp_collateral else { return };
        let mut dirty = std::mem::take(&mut self.perp_dirty);
        for m in std::mem::take(&mut self.marks_changed) {
            dirty.extend(self.perps[&m].holders.iter().copied());
        }
        for a in dirty.clone() {
            if Some(a) != self.insurance && a != FEE_ACCOUNT && self.equity(a) < 0 {
                self.cover_bankruptcy(a, out);
            }
        }
        self.ensure_insurance_solvent(out);
        dirty.extend(std::mem::take(&mut self.perp_dirty));
        for a in dirty {
            let view = self.perp_view(a);
            let p = &mut self.accounts[a as usize].perp;
            let d = view - p.last_view;
            p.last_view = view;
            self.settle_add(a, c, d);
        }
    }

    /// Perp invariants (test helper).
    pub(crate) fn perp_invariants(&self) -> Result<(), String> {
        for (m, st) in &self.perps {
            let mut sum = 0i128;
            let mut longs = 0u64;
            let mut holders = BTreeSet::new();
            for (i, a) in self.accounts.iter().enumerate() {
                if let Some(p) = a.perp.positions.get(m) {
                    if p.size == 0 {
                        return Err(format!("zero position stored for {i} in {m}"));
                    }
                    sum += p.size as i128;
                    longs += p.size.max(0) as u64;
                    holders.insert(i as AccountId);
                }
            }
            if sum != 0 {
                return Err(format!("market {m}: positions sum to {sum}"));
            }
            if holders != st.holders || longs != st.oi {
                return Err(format!("market {m}: holders/oi index out of date"));
            }
            // Open quantities equal the resting book.
            let book = &self.markets[*m as usize].book;
            let mut ob: BTreeMap<AccountId, u64> = BTreeMap::new();
            let mut os: BTreeMap<AccountId, u64> = BTreeMap::new();
            for (side, map) in [(Side::Buy, &mut ob), (Side::Sell, &mut os)] {
                for (_, orders) in book.l3(side) {
                    for (_, acct, rem, _) in orders {
                        *map.entry(acct).or_insert(0) += rem;
                    }
                }
            }
            for (i, a) in self.accounts.iter().enumerate() {
                let id = i as AccountId;
                if a.perp.open_buy.get(m).copied().unwrap_or(0) != ob.get(&id).copied().unwrap_or(0)
                    || a.perp.open_sell.get(m).copied().unwrap_or(0) != os.get(&id).copied().unwrap_or(0)
                {
                    return Err(format!("account {id} market {m}: open qty mismatch"));
                }
            }
        }
        Ok(())
    }
}
