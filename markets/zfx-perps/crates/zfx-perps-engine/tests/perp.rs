//! Perpetuals: PnL, settlement deltas, margin, funding, liquidation, insurance and socialization.

use zfx_perps_engine::testkit::*;
use zfx_perps_engine::*;

const USD: u128 = 1_000_000;
// Accounts u0..u3 are 1..4; the insurance fund is created when the perp is listed (5 here).
const A: AccountId = 1;
const B: AccountId = 2;
const C: AccountId = 3;

fn run(e: &mut Engine, c: Command) -> (Result<Ack, Reject>, Vec<Event>) {
    let mut out = vec![];
    let r = e.apply(&c, &mut out);
    e.check_invariants().unwrap_or_else(|m| panic!("invariant after {c:?}: {m}"));
    (r, out)
}

fn ok(e: &mut Engine, c: Command) -> Vec<Event> {
    let (r, ev) = run(e, c.clone());
    assert!(r.is_ok(), "{c:?} -> {r:?}");
    ev
}

fn mark(e: &mut Engine, px: u64) -> Vec<Event> {
    ok(
        e,
        Command::SetMark {
            market: P,
            mark: px,
            index: px,
            ts_ns: 0,
        },
    )
}

fn cut(e: &mut Engine) -> Vec<Batch> {
    let ev = ok(
        e,
        Command::CutBatch {
            journal_seq: 0,
            journal_hash: [0; 32],
        },
    );
    ev.into_iter()
        .filter_map(|x| if let Event::BatchCut { batch } = x { Some(batch) } else { None })
        .collect()
}

fn delta(batches: &[Batch], account: &str) -> i128 {
    batches
        .iter()
        .flat_map(|b| &b.entries)
        .filter(|d| d.account == account && d.token == "usdc.near")
        .map(|d| d.delta)
        .sum()
}

fn size(e: &Engine, a: AccountId) -> i64 {
    e.accounts[a as usize].perp.positions.get(&P).map_or(0, |p| p.size)
}

fn ins(e: &Engine) -> AccountId {
    e.insurance.unwrap()
}

#[test]
fn pnl_is_marked_and_settled_zero_sum() {
    // $1,000 each, no fees for clean numbers.
    let mut e = perp_engine(2, 1_000 * USD, 40_000);
    e.markets[P as usize].spec.maker_fee_bps = 0;
    e.markets[P as usize].spec.taker_fee_bps = 0;
    // A buys 10,000 lots (100 NEAR) at $4.00 from B: $400 notional each side.
    ok(&mut e, porder(B, Side::Sell, 40_000, 10_000, Tif::Gtc));
    let ev = ok(&mut e, porder(A, Side::Buy, 40_000, 10_000, Tif::Ioc));
    assert!(ev.contains(&Event::Position {
        account: A,
        market: P,
        size: 10_000,
        entry_notional: 400 * USD
    }));
    assert_eq!((size(&e, A), size(&e, B)), (10_000, -10_000));
    assert_eq!(e.perps[&P].oi, 10_000);
    // Mark to $4.20: A +$20, B -$20.
    mark(&mut e, 42_000);
    assert_eq!(e.equity(A), 1_020 * USD as i128);
    assert_eq!(e.equity(B), 980 * USD as i128);
    let b = cut(&mut e);
    assert_eq!(delta(&b, "u0.near"), 20 * USD as i128);
    assert_eq!(delta(&b, "u1.near"), -20 * USD as i128);
    assert!(b.last().unwrap().seal);
    // Nothing moved: the next cut is empty.
    assert!(cut(&mut e).is_empty());
    // A closes at $4.10 against a new bid from B: realizes $10, gives $10 back vs the $4.20 mark.
    ok(&mut e, porder(B, Side::Buy, 41_000, 10_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Sell, 41_000, 10_000, Tif::Ioc));
    assert_eq!((size(&e, A), size(&e, B)), (0, 0));
    assert_eq!(e.perps[&P].oi, 0);
    let b = cut(&mut e);
    assert_eq!(delta(&b, "u0.near"), -10 * USD as i128);
    assert_eq!(delta(&b, "u1.near"), 10 * USD as i128);
    assert_eq!(e.equity(A), 1_010 * USD as i128);
}

#[test]
fn fees_go_to_the_fee_account_and_still_net_to_zero() {
    let mut e = perp_engine(2, 1_000 * USD, 40_000); // 2 bps maker, 5 bps taker
    ok(&mut e, porder(B, Side::Sell, 40_000, 10_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Buy, 40_000, 10_000, Tif::Ioc));
    let b = cut(&mut e);
    let fees = (400 * USD * 5).div_ceil(10_000) + (400 * USD * 2).div_ceil(10_000);
    assert_eq!(delta(&b, "fees.zfx-perps.near"), fees as i128);
    assert_eq!(delta(&b, "u0.near") + delta(&b, "u1.near") + delta(&b, "fees.zfx-perps.near"), 0);
}

#[test]
fn margin_band_and_mark_checks() {
    let mut e = engine_with(2, 0, 100 * USD, 2, 5);
    ok(&mut e, Command::AddMarket(perp_spec()));
    // No mark yet.
    assert_eq!(
        run(&mut e, porder(A, Side::Buy, 40_000, 1, Tif::Gtc)).0,
        Err(Reject::MarkUnavailable)
    );
    mark(&mut e, 40_000);
    // Band is 10%: $4.41 is out, $4.39 is in.
    assert_eq!(
        run(&mut e, porder(A, Side::Buy, 44_100, 1, Tif::Gtc)).0,
        Err(Reject::PriceOutOfBand)
    );
    // $100 at 10x buys at most $1,000 of notional: 25,000 lots (250 NEAR) at $4 is exactly $1,000,
    // which leaves nothing for the fee reserve.
    assert_eq!(
        run(&mut e, porder(A, Side::Buy, 40_000, 25_000, Tif::Gtc)).0,
        Err(Reject::InsufficientMargin)
    );
    ok(&mut e, porder(A, Side::Buy, 40_000, 24_000, Tif::Gtc));
    // Open orders count: a second order on the same side is rejected, an opposite one that
    // doesn't raise the worst case is fine.
    assert_eq!(
        run(&mut e, porder(A, Side::Buy, 40_000, 2_000, Tif::Gtc)).0,
        Err(Reject::InsufficientMargin)
    );
    ok(&mut e, porder(A, Side::Sell, 41_000, 24_000, Tif::Gtc));
    // Collateral backing perp orders can't be withdrawn or spent on spot.
    let ms = e.margin_summary(A);
    assert!(ms.free < 5 * USD as i128, "{ms:?}");
    assert_eq!(e.withdrawable(A, USDC), ms.free as u128);
    assert_eq!(
        run(&mut e, order(A, Side::Buy, 42_500, 200, Tif::Gtc)).0,
        Err(Reject::InsufficientBalance)
    );
}

#[test]
fn funding_moves_cash_from_longs_to_shorts_exactly() {
    let mut e = perp_engine(3, 1_000 * USD, 40_000);
    ok(&mut e, porder(B, Side::Sell, 40_000, 10_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Buy, 40_000, 10_000, Tif::Ioc));
    // C quotes a book whose mid sits 1% over the index: premium 10,000 ppm.
    ok(&mut e, porder(C, Side::Buy, 40_390, 1, Tif::Gtc));
    ok(&mut e, porder(C, Side::Sell, 40_410, 1, Tif::Gtc));
    for _ in 0..3 {
        mark(&mut e, 40_000);
    }
    let (ca, cb) = (e.accounts[A as usize].perp.cash, e.accounts[B as usize].perp.cash);
    let ev = ok(&mut e, Command::FundingTick { market: P, ts_ns: 0 });
    // rate = clamp(10,000 / 8, ±750) = 750 ppm; per lot = 750e-6 * 40,000 * 1 = 30 atoms.
    assert!(ev.contains(&Event::Funding {
        market: P,
        rate_ppm: 750,
        payment_per_lot: 30
    }));
    assert_eq!(e.accounts[A as usize].perp.cash, ca - 300_000);
    assert_eq!(e.accounts[B as usize].perp.cash, cb + 300_000);
    // Samples reset: the next tick with no new marks pays nothing.
    let ev = ok(&mut e, Command::FundingTick { market: P, ts_ns: 0 });
    assert!(ev.contains(&Event::Funding {
        market: P,
        rate_ppm: 0,
        payment_per_lot: 0
    }));
}

#[test]
fn liquidation_closes_on_the_book_and_pays_the_insurance_fee() {
    // A ends up with $50 of collateral and a $400 long (10,000 lots = 100 NEAR at $4): 8x.
    let mut e = perp_engine(3, 1_000 * USD, 40_000);
    ok(
        &mut e,
        Command::Withdraw {
            account: A,
            asset: USDC,
            amount: 950 * USD,
            nonce: 1,
            public_key: [0; 32],
            signature: vec![],
        },
    );
    ok(&mut e, porder(B, Side::Sell, 40_000, 10_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Buy, 40_000, 10_000, Tif::Ioc)); // $400 notional on $50: 8x
    // C bids below the market, ready to absorb the liquidation.
    ok(&mut e, porder(C, Side::Buy, 37_000, 20_000, Tif::Gtc));
    // Mark $3.80: equity 50 - 20 - 0.2 fee = $29.8 vs maintenance 5% of $380 = $19: safe.
    let ev = mark(&mut e, 38_000);
    assert!(!ev.iter().any(|x| matches!(x, Event::Liquidated { .. })));
    // Mark $3.68: equity 50 - 32 - 0.2 = $17.8 vs maintenance $18.4: liquidated.
    let ev = mark(&mut e, 36_800);
    assert!(ev.iter().any(|x| matches!(x, Event::Liquidated { account: A, .. })), "{ev:?}");
    assert_eq!(size(&e, A), 0);
    // Closed against C's $3.70 bid (within 3% of the mark), nothing for insurance to take over.
    assert_eq!(size(&e, C), 10_000);
    assert!(!ev.iter().any(|x| matches!(x, Event::InsuranceTakeover { .. })));
    // Liquidation fee 1% of $368 went to insurance.
    assert_eq!(e.perp_view(ins(&e)), 3_680_000);
    assert!(e.equity(A) >= 0);
}

#[test]
fn empty_book_means_insurance_takeover_and_bankruptcy_cover() {
    let mut e = perp_engine(3, 1_000 * USD, 40_000);
    // Fund the insurance account with $500 (an on-chain deposit to the insurance account).
    let seq = e.chain_cursor + 1;
    ok(
        &mut e,
        Command::Chain(ChainEvent::Deposit {
            seq,
            account: "insurance.zfx-perps.near".into(),
            token: "usdc.near".into(),
            amount: 500 * USD,
        }),
    );
    ok(
        &mut e,
        Command::Withdraw {
            account: A,
            asset: USDC,
            amount: 950 * USD,
            nonce: 1,
            public_key: [0; 32],
            signature: vec![],
        },
    );
    ok(&mut e, porder(B, Side::Sell, 40_000, 10_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Buy, 40_000, 10_000, Tif::Ioc));
    // Gap down to $3.00: A's $50 is gone ($100 loss); no bids.
    let ev = mark(&mut e, 30_000);
    let i = ins(&e);
    assert!(ev.contains(&Event::InsuranceTakeover {
        account: A,
        market: P,
        size: 10_000,
        price: 30_000
    }));
    assert_eq!((size(&e, A), size(&e, i)), (0, 10_000));
    let deficit = ev
        .iter()
        .find_map(|x| {
            if let Event::Bankruptcy { account: A, deficit } = x {
                Some(*deficit)
            } else {
                None
            }
        })
        .expect("bankruptcy");
    // Loss $100 + fee 0.2 + liquidation fee $3 on $300 - collateral $50.
    assert_eq!(deficit, 100 * USD as i128 + 200_000 + 3 * USD as i128 - 50 * USD as i128);
    assert_eq!(e.equity(A), 0);
    assert_eq!(e.equity(i), 500 * USD as i128 - deficit + 3 * USD as i128);
    // Settlement keeps every on-chain balance non-negative and nets to zero.
    let b = cut(&mut e);
    for acct in &e.accounts {
        let usdc = acct.balance(USDC).total as i128;
        assert!(usdc + acct.perp.last_view >= 0, "{} negative on-chain", acct.name);
    }
    let sum: i128 = b
        .iter()
        .flat_map(|x| &x.entries)
        .filter(|d| d.token == "usdc.near")
        .map(|d| d.delta)
        .sum();
    assert_eq!(sum, 0);
}

#[test]
fn losses_beyond_insurance_are_socialized() {
    let mut e = perp_engine(3, 1_000 * USD, 40_000);
    ok(
        &mut e,
        Command::Withdraw {
            account: A,
            asset: USDC,
            amount: 950 * USD,
            nonce: 1,
            public_key: [0; 32],
            signature: vec![],
        },
    );
    // B is short against A, C is short against A too.
    ok(&mut e, porder(B, Side::Sell, 40_000, 5_000, Tif::Gtc));
    ok(&mut e, porder(C, Side::Sell, 40_000, 5_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Buy, 40_000, 10_000, Tif::Ioc));
    // Insurance is empty. Gap to $3.00: A loses $100 on $50.
    let ev = mark(&mut e, 30_000);
    let soc = ev
        .iter()
        .find_map(|x| {
            if let Event::Socialized { amount, from_accounts } = x {
                Some((*amount, *from_accounts))
            } else {
                None
            }
        })
        .expect("socialized");
    assert!(soc.1 >= 2, "{soc:?}");
    assert!(e.equity(ins(&e)) >= 0);
    // B and C split the clawback; both still in profit.
    assert!(e.perp_view(B) > 0 && e.perp_view(C) > 0);
    let b = cut(&mut e);
    let sum: i128 = b
        .iter()
        .flat_map(|x| &x.entries)
        .filter(|d| d.token == "usdc.near")
        .map(|d| d.delta)
        .sum();
    assert_eq!(sum, 0);
    for acct in &e.accounts {
        assert!(acct.balance(USDC).total as i128 + acct.perp.last_view >= 0);
    }
}

#[test]
fn profit_can_be_withdrawn_and_the_chain_sees_it_once() {
    let mut e = perp_engine(2, 1_000 * USD, 40_000);
    e.markets[P as usize].spec.maker_fee_bps = 0;
    e.markets[P as usize].spec.taker_fee_bps = 0;
    ok(&mut e, porder(B, Side::Sell, 40_000, 10_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Buy, 40_000, 10_000, Tif::Ioc));
    mark(&mut e, 44_000); // A +$40
    ok(&mut e, porder(B, Side::Buy, 44_000, 10_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Sell, 44_000, 10_000, Tif::Ioc)); // A flat with $1,040 equity
    assert_eq!(e.withdrawable(A, USDC), 1_040 * USD);
    assert_eq!(
        run(
            &mut e,
            Command::Withdraw {
                account: A,
                asset: USDC,
                amount: 1_041 * USD,
                nonce: 1,
                public_key: [0; 32],
                signature: vec![]
            }
        )
        .0,
        Err(Reject::InsufficientBalance)
    );
    ok(
        &mut e,
        Command::Withdraw {
            account: A,
            asset: USDC,
            amount: 1_040 * USD,
            nonce: 1,
            public_key: [0; 32],
            signature: vec![],
        },
    );
    let b = cut(&mut e);
    // On-chain: A was at $1,000; the batch adds +$40 of PnL, then pays out $1,040.
    assert_eq!(delta(&b, "u0.near"), 40 * USD as i128);
    assert_eq!(b.iter().flat_map(|x| &x.withdrawals).map(|w| w.amount).sum::<u128>(), 1_040 * USD);
    assert_eq!(e.equity(A), 0);
    assert_eq!(
        e.accounts[A as usize].balance(USDC).total as i128 + e.accounts[A as usize].perp.last_view,
        0
    );
}

#[test]
fn snapshot_and_replay_include_perp_state() {
    let mut e = perp_engine(2, 1_000 * USD, 40_000);
    ok(&mut e, porder(B, Side::Sell, 40_000, 3_000, Tif::Gtc));
    ok(&mut e, porder(A, Side::Buy, 40_000, 1_000, Tif::Ioc));
    mark(&mut e, 40_500);
    let s = Engine::from_snapshot(&e.to_snapshot()).unwrap();
    assert_eq!(s.state_hash(), e.state_hash());
    assert_eq!(s.margin_summary(A), e.margin_summary(A));
}
