//! Risk stress on real index history (spec 5.5 Risk). Every one of the 50 ZFX daily model indices
//! (30 UTC days, 2026-10-11 to 2026-11-09) is replayed print by print against a book of 20
//! isolated positions (10 long, 10 short, leverage from the market's maximum down to a fifth of
//! it), with an insurance fund of 2% of open notional. After each print every position below
//! maintenance is liquidated the way the operator would (insurance first, ADL if the fund can't
//! margin the position, socialization if a deficit exceeds the fund).
//!
//! Parameter sets compared:
//! - `tiered`: the published v1 policy (docs/MARKET_RISK.md): tier from the largest daily move
//!   seen in the trailing window: <= 50% -> 2x (IM 50%, MM 25%), <= 100% -> 1x (IM 100%, MM 50%),
//!   otherwise not listed.
//! - `naive_10x`: the inherited crypto default (IM 10%, MM 5%) on every market.
//!
//! Tiers are fitted on the first 15 days and replayed on the last 15 (out of sample), and also on
//! the full window (in sample). Writes research/data/stress_results.json.

use serde_json::{Value, json};
use zfx_perps_clearing::ledger::{self, CEvent, Now, Orders};
use zfx_perps_clearing::plan::plan_liquidation;
use zfx_perps_clearing::print::{DataStatus, OracleStatus};
use zfx_perps_clearing::store::NativeCrypto;
use zfx_perps_clearing::testkit::*;
use zfx_perps_clearing::wire::Side;
use zfx_perps_clearing::{MarketStatus, NS_PER_DAY};

#[derive(Clone, Copy, Debug)]
struct Tier {
    im: u16,
    mm: u16,
    lev: u16,
    /// Mark limit per print; 0 = none (the mark jumps to the index).
    limit: u32,
}

fn tier(max_abs: f64) -> Option<Tier> {
    // Limits satisfy limit + 3% slippage + 0.75%/day funding <= maintenance.
    if max_abs <= 0.5 {
        Some(Tier { im: 5_000, mm: 2_500, lev: 2, limit: 2_000 })
    } else if max_abs <= 1.0 {
        Some(Tier { im: 10_000, mm: 5_000, lev: 1, limit: 4_500 })
    } else {
        None
    }
}

const NAIVE: Tier = Tier { im: 1_000, mm: 500, lev: 10, limit: 0 };

fn max_abs_move(v: &[Option<u64>]) -> f64 {
    let ok: Vec<f64> = v.iter().flatten().map(|x| *x as f64).collect();
    ok.windows(2).map(|w| ((w[1] - w[0]) / w[0]).abs()).fold(0.0, f64::max)
}

#[derive(Default, Debug, Clone, serde::Serialize)]
struct Out {
    markets: u32,
    not_listed: u32,
    liquidations: u32,
    takeovers: u32,
    adl_fills: u32,
    deficits: u32,
    deficit_usd: f64,
    insurance_paid_usd: f64,
    socialized_usd: f64,
    bad_debt_usd: f64,
    markets_with_bad_debt: u32,
    jump_halts: u32,
    close_only_days: u32,
    open_notional_usd: f64,
}

fn run_market(values: &[Option<u64>], t: Tier, o: &mut Out) {
    let mut w = World::new();
    let first = values.iter().flatten().next().copied().unwrap();
    {
        let m = w.s.markets.get_mut(MKT).unwrap();
        let p = &mut m.params;
        p.index_units_per_tick = 1_000; // 0.001 USD of index per tick
        p.tick_value = 1;
        p.max_price = 1_000_000_000_000;
        p.max_qty = 1_000_000_000_000;
        p.max_position = 1_000_000_000_000;
        p.max_open_interest = u64::MAX / 4;
        p.initial_margin_bps = t.im;
        p.maintenance_margin_bps = t.mm;
        p.max_leverage = t.lev;
        p.price_band_bps = 9_999;
        p.liq_slippage_bps = 300;
        p.print_close_only_from_sec = 0;
        p.mark_limit_bps = if t.limit == 0 { 1_000_000 } else { t.limit };
        p.halt_jump_bps = 0;
        m.policy_hash = ledger::policy_hash(&NativeCrypto, &m.params.clone());
        m.print = None;
        m.mark = 0;
    }
    let p = w.print(first / 1_000, 1);
    w.post_print(&p).unwrap();
    let mark = w.s.markets[MKT].mark;
    let lots = (10_000_000_000u64 / mark).max(1); // about $10,000 notional per position
    let notional = lots as u128 * mark as u128;
    w.fund_insurance(notional * 20 * 2 / 100);
    for i in 0..10u32 {
        // Margin fraction: IM, 1.25 IM, ... 2 IM (cycling), plus fees.
        let frac = t.im as u128 * (4 + (i % 5) as u128) / 4;
        let dep = notional * frac / 10_000 + notional / 1_000 + 1;
        let l = w.trader(&format!("long{i}.near"), dep);
        let s = w.trader(&format!("short{i}.near"), dep);
        let ask = w.order(&s, Side::Sell, mark, lots, |o| o.max_fee_bps = 1_000);
        let bid = w.order(&l, Side::Buy, mark, lots, |o| o.max_fee_bps = 1_000);
        let list = vec![ask, bid];
        let mut ord = Orders::new(&list, false);
        ledger::fill(&mut w.s, &NativeCrypto, &mut ord, Now::at(w.now), 0, 1, lots, mark).unwrap();
    }
    o.markets += 1;
    o.open_notional_usd += (notional * 10) as f64 / 1e6;
    let usd = |x: u128| x as f64 / 1e6;
    for v in &values[1..] {
        w.advance_day();
        let print = match v {
            Some(x) => w.print((x / 1_000).max(1), 1),
            None => w.print_with(0, 1, |p| {
                p.status = OracleStatus::Unavailable;
                p.data_status = DataStatus::NotReported;
            }),
        };
        let ev = w.post_print(&print).unwrap();
        if ev.iter().any(|e| matches!(e, CEvent::MarketHalted { .. })) {
            // Governance reviews the jump and accepts the print (the move is real in this data).
            o.jump_halts += 1;
            let m = w.s.markets.get_mut(MKT).unwrap();
            m.status = MarketStatus::Active;
            m.mark = (v.unwrap() / 1_000).max(1);
        }
        let now = Now::at(w.now);
        let st = ledger::effective_status(&w.s.markets[MKT], now, w.s.globals.settle_grace_ns);
        if st == MarketStatus::CloseOnly {
            o.close_only_days += 1;
        }
        let holders: Vec<String> = w.s.holders(MKT).map(|(a, _)| a.to_string()).collect();
        for a in holders {
            let Some(plan) = plan_liquidation(&mut w.s, now, &a, MKT, &[]) else { continue };
            let mut ord = Orders::new(&[], false);
            match ledger::liquidate(&mut w.s, &NativeCrypto, &mut ord, now, &a, MKT, &plan.fills, &plan.adl, &plan.socialize) {
                Ok(ev) => {
                    o.liquidations += 1;
                    for e in ev {
                        match e {
                            CEvent::InsuranceTakeover { .. } => o.takeovers += 1,
                            CEvent::Adl { .. } => o.adl_fills += 1,
                            CEvent::Deficit { deficit, insurance_paid, socialized, bad_debt, .. } => {
                                o.deficits += 1;
                                o.deficit_usd += usd(deficit);
                                o.insurance_paid_usd += usd(insurance_paid);
                                o.socialized_usd += usd(socialized);
                                o.bad_debt_usd += usd(bad_debt);
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => panic!("liquidation of {a} failed: {e:?}"),
            }
            if w.s.markets[MKT].status == MarketStatus::Halted {
                w.s.markets.get_mut(MKT).unwrap().status = MarketStatus::Active;
            }
        }
        check_conservation(&w.s).unwrap();
    }
    if w.s.markets[MKT].bad_debt > 0 {
        o.markets_with_bad_debt += 1;
    }
}

#[test]
fn replay_real_index_history() {
    let data: Value = serde_json::from_str(include_str!("data/index_series.json")).unwrap();
    let markets = data["markets"].as_object().unwrap();
    let mut results = serde_json::Map::new();
    for (label, fit, replay) in [("out_of_sample_last_15_days", 0..15usize, 15..30usize), ("in_sample_30_days", 0..30, 0..30)] {
        let (mut tiered, mut naive, mut nolimit) = (Out::default(), Out::default(), Out::default());
        let mut tiers = serde_json::Map::new();
        for (id, m) in markets {
            let vals: Vec<Option<u64>> = m["values"].as_array().unwrap().iter().map(|v| v.as_u64()).collect();
            let fit_v = &vals[fit.clone().start.min(vals.len())..fit.end.min(vals.len())];
            let rep_v: Vec<Option<u64>> = vals[replay.start.min(vals.len())..replay.end.min(vals.len())].to_vec();
            if rep_v.iter().flatten().count() < 2 || rep_v[0].is_none() {
                continue;
            }
            let t = if fit_v.iter().flatten().count() < 10 { None } else { tier(max_abs_move(fit_v)) };
            tiers.insert(id.clone(), json!(t.map(|t| t.lev)));
            match t {
                Some(t) => {
                    run_market(&rep_v, t, &mut tiered);
                    run_market(&rep_v, Tier { limit: 0, ..t }, &mut nolimit);
                }
                None => {
                    tiered.not_listed += 1;
                    nolimit.not_listed += 1;
                }
            }
            run_market(&rep_v, NAIVE, &mut naive);
        }
        eprintln!("{label}\n  tiered  {tiered:?}\n  tiered_no_limit {nolimit:?}\n  naive10x {naive:?}");
        results.insert(label.into(), json!({"tiered": tiered, "tiered_without_mark_limit": nolimit, "naive_10x": naive, "leverage_by_market": tiers}));
        // The tiered policy must never produce bad debt on history it wasn't fitted on.
        // With the mark limit, liquidation always happens before equity reaches zero.
        assert_eq!(tiered.deficits, 0, "{label}: tiered policy produced a deficit");
        assert_eq!(tiered.bad_debt_usd, 0.0, "{label}: tiered policy produced bad debt");
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../research/data/stress_results.json");
    std::fs::write(path, serde_json::to_string_pretty(&Value::Object(results)).unwrap() + "\n").unwrap();
    let _ = NS_PER_DAY;
}
