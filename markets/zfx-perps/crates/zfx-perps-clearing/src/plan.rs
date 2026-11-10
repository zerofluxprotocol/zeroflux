//! Building liquidation ops from an in-memory ledger (operator side).
//!
//! The contract decides whether a liquidation op is valid; this module decides which valid op to
//! submit: which resting orders take the position, whether the insurance fund or ADL takes the
//! rest, and the complete socialization list when a deficit could exceed the insurance fund.
//! Choices made here are operator powers and are documented as such (ADL ranking in particular).

use crate::ledger::{self, Now};
use crate::store::{MemStore, Store};
use crate::wire::Side;
use std::collections::BTreeMap;

/// A resting order the matcher could use to absorb a liquidation, in priority order.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// Index in the batch order table.
    pub index: u32,
    pub account: String,
    pub price: u64,
    pub available: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiquidationPlan {
    pub fills: Vec<(u32, u64)>,
    pub adl: Vec<(String, u64)>,
    pub socialize: Vec<String>,
}

/// Plan the liquidation of `account` in `market`. `book` holds candidates on the side that
/// takes the position (bids for a long), best first; only those within the slippage bound are
/// used. Returns `None` if the position isn't liquidatable now.
pub fn plan_liquidation(s: &mut MemStore, now: Now, account: &str, market: &str, book: &[Candidate]) -> Option<LiquidationPlan> {
    if !ledger::liquidatable(s, account, market, now) {
        return None;
    }
    let m = s.market(market)?;
    let mark = m.mark;
    let p = s.position(account, market);
    let size = p.size;
    let liq_side = if size > 0 { Side::Sell } else { Side::Buy };
    let slip = mark as u128 * m.params.liq_slippage_bps as u128 / 10_000;
    let tv = m.params.tick_value as i128;
    let mut need = size.unsigned_abs();
    let mut fills = vec![];
    let mut delta: BTreeMap<String, i64> = BTreeMap::new();
    let mut adverse: i128 = 0;
    for c in book {
        if need == 0 {
            break;
        }
        let ok = match liq_side {
            Side::Sell => c.price as u128 + slip >= mark as u128,
            Side::Buy => c.price as u128 <= mark as u128 + slip,
        };
        if !ok || c.account == account || c.available == 0 {
            continue;
        }
        let q = c.available.min(need);
        fills.push((c.index, q));
        need -= q;
        *delta.entry(c.account.clone()).or_insert(0) += -liq_side.sign() * q as i64;
        // Cash the liquidated account gives up relative to closing at the mark.
        adverse += match liq_side {
            Side::Sell => (mark as i128 - c.price as i128) * q as i128 * tv,
            Side::Buy => (c.price as i128 - mark as i128) * q as i128 * tv,
        };
    }
    let mut adl = vec![];
    if need > 0 && !ledger::insurance_can_absorb(s, market, liq_side.opposite(), need) {
        // ADL ranking: most collateralized opposite positions first (equity / notional), then by
        // account id. The contract checks each target is opposite and only reduces.
        let mut targets: Vec<(i128, String, u64)> = s
            .holders(market)
            .filter(|(a, q)| *a != account && q.size != 0 && (q.size > 0) != (size > 0))
            .map(|(a, q)| {
                let notional = (q.size.unsigned_abs() as i128 * mark as i128 * tv).max(1);
                (ledger::equity(&m, q, mark) * 1_000_000 / notional, a.to_string(), q.size.unsigned_abs())
            })
            .collect();
        targets.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
        for (_, a, avail) in targets {
            if need == 0 {
                break;
            }
            let after = delta.get(&a).copied().unwrap_or(0);
            // A maker in this liquidation may already have reduced; never flip it.
            let cur = s.position(&a, market).size + after;
            let room = if (cur > 0) != (size > 0) { cur.unsigned_abs().min(avail) } else { 0 };
            let q = room.min(need);
            if q == 0 {
                continue;
            }
            adl.push((a.clone(), q));
            *delta.entry(a).or_insert(0) += -liq_side.sign() * q as i64;
            need -= q;
        }
    }
    // Deficit before the insurance fund: equity at the mark minus the adverse fill prices.
    let eq = ledger::equity(&m, &p, mark) - adverse;
    let mut socialize = vec![];
    if eq < 0 {
        let profit_long = size < 0;
        for (a, q) in s.holders(market) {
            if a == account {
                continue;
            }
            let after = q.size + delta.get(a).copied().unwrap_or(0);
            if after != 0 && (after > 0) == profit_long {
                socialize.push(a.to_string());
            }
        }
        // Accounts that weren't holders but become profit-side holders through fills.
        for (a, d) in &delta {
            let base = s.position(a, market).size;
            if base == 0 && *d != 0 && (*d > 0) == profit_long && !socialize.contains(a) {
                socialize.push(a.clone());
            }
        }
        socialize.sort();
    }
    Some(LiquidationPlan { fills, adl, socialize })
}
