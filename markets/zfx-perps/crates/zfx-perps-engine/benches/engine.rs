//! Engine latency and throughput on a realistic mixed flow, for the spot market and the perp.
//!
//! Book pre-filled with 20k resting orders across ~400 levels around $4.25, then 2M operations:
//! 55% passive limit placements near the touch, 35% cancels of random resting orders, 10%
//! marketable IOC orders that sweep 1-3 levels. The perp run also moves the mark every 1,000 ops
//! (each mark update runs the liquidation check over every position holder). Each `apply` is
//! timed individually; Instant overhead (~20-40 ns on macOS) is included.
//!
//! Run: cargo bench -p zfx-perps-engine

use hdrhistogram::Histogram;
use zfx_perps_engine::testkit::*;
use zfx_perps_engine::*;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use std::time::Instant;

const ACCOUNTS: usize = 1_000;
const OPS: usize = 2_000_000;

fn run(name: &str, mut e: Engine, market: MarketId) {
    let mut rng = SmallRng::seed_from_u64(7);
    let mut out = Vec::with_capacity(64);
    let mut mid = 42_500u64;
    let mut live: Vec<OrderId> = Vec::with_capacity(64_000);
    let mk = |acct: u32, side: Side, price: u64, qty: u64, tif: Tif| {
        Command::Place(NewOrder {
            account: acct,
            market,
            side,
            price,
            qty,
            tif,
            stp: Stp::CancelTaker,
            client_id: 0,
            ts_ns: 0,
        })
    };
    for _ in 0..20_000 {
        let side = if rng.gen_bool(0.5) { Side::Buy } else { Side::Sell };
        let off = rng.gen_range(1..200u64);
        let price = if side == Side::Buy { mid - off } else { mid + off };
        out.clear();
        if let Ok(Ack::Placed {
            order_id,
            status: OrderStatus::Resting,
            ..
        }) = e.apply(
            &mk(rng.gen_range(1..=ACCOUNTS as u32), side, price, rng.gen_range(1..500), Tif::Gtc),
            &mut out,
        ) {
            live.push(order_id);
        }
    }
    let mut hist: Vec<Histogram<u64>> = (0..4).map(|_| Histogram::<u64>::new(3).unwrap()).collect();
    let mut fills = 0u64;
    let is_perp = e.markets[market as usize].spec.perp.is_some();
    let start = Instant::now();
    for i in 0..OPS {
        let r: f64 = rng.r#gen();
        let (kind, cmd) = if is_perp && i % 1_000 == 999 {
            mid = (mid as i64 + rng.gen_range(-20..=20)) as u64;
            (
                3,
                Command::SetMark {
                    market,
                    mark: mid,
                    index: mid,
                    ts_ns: 0,
                },
            )
        } else if r < 0.55 || live.is_empty() {
            let side = if rng.gen_bool(0.5) { Side::Buy } else { Side::Sell };
            let off = rng.gen_range(1..200u64);
            let price = if side == Side::Buy { mid - off } else { mid + off };
            (
                0,
                mk(rng.gen_range(1..=ACCOUNTS as u32), side, price, rng.gen_range(1..500), Tif::Gtc),
            )
        } else if r < 0.90 {
            let id = live.swap_remove(rng.gen_range(0..live.len()));
            let acct = e.markets[market as usize].book.get(id).map_or(1, |n| n.account);
            (
                1,
                Command::Cancel {
                    account: acct,
                    market,
                    order_id: id,
                },
            )
        } else {
            let side = if rng.gen_bool(0.5) { Side::Buy } else { Side::Sell };
            let lim = if side == Side::Buy { mid + 200 } else { mid - 200 };
            (
                2,
                mk(rng.gen_range(1..=ACCOUNTS as u32), side, lim, rng.gen_range(200..1500), Tif::Ioc),
            )
        };
        out.clear();
        let t = Instant::now();
        let res = e.apply(&cmd, &mut out);
        let ns = t.elapsed().as_nanos() as u64;
        hist[kind].record(ns.max(1)).unwrap();
        if let Ok(Ack::Placed {
            order_id,
            status: OrderStatus::Resting,
            ..
        }) = res
        {
            live.push(order_id);
        }
        fills += out.iter().filter(|ev| matches!(ev, Event::Fill { .. })).count() as u64;
    }
    let secs = start.elapsed().as_secs_f64();
    e.check_invariants().expect("invariants after bench");
    let holders = e.perps.get(&market).map_or(0, |s| s.holders.len());
    println!(
        "{name}: {OPS} ops in {secs:.2}s = {:.2}M ops/s (harness overhead included), {fills} fills, {} resting{}",
        OPS as f64 / secs / 1e6,
        e.markets[market as usize].book.len(),
        if is_perp {
            format!(", {holders} position holders")
        } else {
            String::new()
        }
    );
    for (name, h) in ["place (passive)", "cancel", "IOC sweep", "mark update"].iter().zip(&hist) {
        if h.is_empty() {
            continue;
        }
        println!(
            "  {name:16} n={:>8}  p50={:>6}ns  p99={:>6}ns  p99.9={:>6}ns  max={:>7}ns",
            h.len(),
            h.value_at_quantile(0.5),
            h.value_at_quantile(0.99),
            h.value_at_quantile(0.999),
            h.max()
        );
    }
}

fn main() {
    run(
        "spot NEAR-USDC",
        engine_with(ACCOUNTS, u128::MAX / 1_000_000, u128::MAX / 1_000_000, 2, 5),
        M,
    );
    run("perp NEAR-PERP", perp_engine(ACCOUNTS, 1u128 << 90, 42_500), P);
}
