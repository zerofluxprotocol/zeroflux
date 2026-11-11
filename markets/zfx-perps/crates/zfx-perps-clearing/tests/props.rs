//! Randomized ledger test: random fills, prints (jumps, NOT_REPORTED, disputes), funding,
//! liquidations planned the way the operator plans them, withdrawals and margin moves.
//! After every operation: conservation holds, positions net to zero, open interest matches, and
//! a rejected operation changed nothing.

use proptest::prelude::*;
use zfx_perps_clearing::ledger::{self, CEvent, Now, Orders};
use zfx_perps_clearing::plan::{Candidate, plan_liquidation};
use zfx_perps_clearing::testkit::*;
use zfx_perps_clearing::*;

#[derive(Default, Debug)]
struct Coverage {
    fills: u32,
    liquidations: u32,
    takeovers: u32,
    adl: u32,
    socialized: u32,
    bad_debt: u32,
    funding_paid: u32,
    close_only_rejects: u32,
}

const NAMES: [&str; 6] = ["a.near", "b.near", "c.near", "d.near", "e.near", "f.near"];

fn step(w: &mut World, traders: &[Trader], cov: &mut Coverage, kind: u8, x: u64, y: u64, z: u64) {
    let mark = w.s.markets[MKT].mark.max(1);
    let before = w.s.clone();
    let r: Option<ledger::Res<Vec<CEvent>>> = match kind % 8 {
        0..=2 => {
            let (mi, ti) = ((x % 6) as usize, (y % 6) as usize);
            let side = if z % 2 == 0 { Side::Sell } else { Side::Buy };
            // Prices within ±35% of the mark: some fall outside the 30% band.
            let px = (mark as i64 + (mark as i64 * ((x % 71) as i64 - 35)) / 100).max(1) as u64;
            let qty = 1 + y % 300;
            let mo = w.order(&traders[mi], side, px, qty, |_| {});
            let to = w.order(&traders[ti], side.opposite(), px, qty, |o| o.reduce_only = z % 7 == 0);
            let list = vec![mo, to];
            let mut o = Orders::new(&list, false);
            let r = ledger::fill(&mut w.s, &NativeCrypto, &mut o, Now::at(w.now), 0, 1, 1 + z % qty, px);
            if r.is_ok() {
                cov.fills += 1;
            }
            if r.as_ref().is_err_and(|e| e.reason == Reject::CloseOnly) {
                cov.close_only_rejects += 1;
            }
            Some(r)
        }
        3 => {
            // Next day's print: up to ±60% moves, sometimes NOT_REPORTED or DISPUTED.
            w.advance_day();
            let t = (mark as i64 + mark as i64 * ((x % 121) as i64 - 60) / 100).max(1) as u64;
            let p = match y % 10 {
                0 => w.print_with(0, 1, |p| {
                    p.status = OracleStatus::Unavailable;
                    p.data_status = DataStatus::NotReported
                }),
                1 => w.print_with(t, 1, |p| p.status = OracleStatus::Disputed),
                _ => w.print(t, 1),
            };
            Some(w.post_print(&p))
        }
        4 => {
            w.now += 8 * 3_600 * 1_000_000_000;
            let r = ledger::funding(&mut w.s, Now::at(w.now), MKT);
            if let Ok(ev) = &r
                && ev.iter().any(|e| matches!(e, CEvent::Funding { pay_per_lot, .. } if *pay_per_lot != 0))
            {
                cov.funding_paid += 1;
            }
            Some(r)
        }
        5 => {
            // Liquidation sweep, planned like the operator does, with a few fresh bids/asks.
            let holders: Vec<(String, i64)> = w.s.holders(MKT).map(|(a, p)| (a.to_string(), p.size)).collect();
            for (a, size) in holders {
                let side = if size > 0 { Side::Buy } else { Side::Sell };
                let mut list = vec![];
                let mut book = vec![];
                for k in 0..(x % 3) {
                    let t = &traders[((y + k) % 6) as usize];
                    if t.name == a {
                        continue;
                    }
                    let px = match side {
                        Side::Buy => mark - mark * ((z + k) % 4) / 100,
                        Side::Sell => mark + mark * ((z + k) % 4) / 100,
                    }
                    .max(1);
                    let o = w.order(t, side, px, 1 + (y + k) % 50, |_| {});
                    book.push(Candidate { index: list.len() as u32, account: t.name.clone(), price: px, available: o.qty });
                    list.push(o);
                }
                let now = Now::at(w.now);
                if let Some(plan) = plan_liquidation(&mut w.s, now, &a, MKT, &book) {
                    let snapshot = w.s.clone();
                    let mut o = Orders::new(&list, false);
                    let r = ledger::liquidate(&mut w.s, &NativeCrypto, &mut o, now, &a, MKT, &plan.fills, &plan.adl, &plan.socialize);
                    match &r {
                        Ok(ev) => {
                            cov.liquidations += 1;
                            for e in ev {
                                match e {
                                    CEvent::InsuranceTakeover { .. } => cov.takeovers += 1,
                                    CEvent::Adl { .. } => cov.adl += 1,
                                    CEvent::Deficit { socialized, bad_debt, .. } => {
                                        cov.socialized += (*socialized > 0) as u32;
                                        cov.bad_debt += (*bad_debt > 0) as u32;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(_) => assert_eq!(w.s, snapshot, "rejected liquidation changed state"),
                    }
                    check_conservation(&w.s).unwrap();
                }
            }
            None
        }
        6 => {
            let t = &traders[(x % 6) as usize];
            let amount = (y % 100_000_000) as u128 + 1;
            let n = w.s.account(&t.name).action_nonce + 1;
            let msg = wire::withdraw_message(CHAIN, CONTRACT, &t.name, USDC, amount, n);
            Some(ledger::withdraw(&mut w.s, &NativeCrypto, Now::at(w.now), &t.name, amount, n, &t.key.public(), &t.key.sign(&msg)))
        }
        _ => {
            let t = &traders[(x % 6) as usize];
            let delta = (y % 20_000_000) as i128 - 10_000_000;
            let n = w.s.account(&t.name).action_nonce + 1;
            let msg = wire::margin_message(CHAIN, CONTRACT, &t.name, MKT, delta, n);
            Some(ledger::adjust_margin(&mut w.s, &NativeCrypto, Now::at(w.now), &t.name, MKT, delta, n, &t.key.public(), &t.key.sign(&msg)))
        }
    };
    if let Some(Err(_)) = r {
        assert_eq!(w.s, before, "rejected op changed state");
    }
    // A halted market (bad debt or a suspect print) is reopened so the run keeps exercising rules.
    if let Some(m) = w.s.markets.get_mut(MKT)
        && m.status == MarketStatus::Halted
    {
        m.status = MarketStatus::Active;
    }
    check_conservation(&w.s).unwrap();
}

fn world(deposits: &[u64]) -> (World, Vec<Trader>) {
    let mut w = World::new();
    w.fund_insurance(200_000_000);
    let t = NAMES
        .iter()
        .zip(deposits)
        .map(|(n, d)| w.trader(n, 10_000_000 + (*d % 400_000_000) as u128))
        .collect();
    (w, t)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]
    #[test]
    fn random_operations_conserve_collateral(
        deposits in proptest::collection::vec(any::<u64>(), 6),
        ops in proptest::collection::vec((any::<u8>(), any::<u64>(), any::<u64>(), any::<u64>()), 1..150),
    ) {
        let (mut w, t) = world(&deposits);
        let mut cov = Coverage::default();
        for (k, x, y, z) in ops {
            step(&mut w, &t, &mut cov, k, x, y, z);
        }
    }
}

/// The generator reaches every branch of the loss waterfall (not just the happy path).
#[test]
fn generator_reaches_liquidation_waterfall() {
    use rand::{Rng, SeedableRng};
    let mut cov = Coverage::default();
    let mut rng = rand::rngs::SmallRng::seed_from_u64(7);
    for _ in 0..300 {
        let deposits: Vec<u64> = (0..6).map(|_| rng.random()).collect();
        let (mut w, t) = world(&deposits);
        for _ in 0..150 {
            step(&mut w, &t, &mut cov, rng.random(), rng.random(), rng.random(), rng.random());
        }
    }
    eprintln!("{cov:?}");
    assert!(cov.fills > 1_000 && cov.liquidations > 50 && cov.takeovers > 10 && cov.adl > 0 && cov.socialized > 0);
    assert!(cov.bad_debt > 0 && cov.funding_paid > 10 && cov.close_only_rejects > 10);
}
