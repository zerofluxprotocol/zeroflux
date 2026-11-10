//! Scenario tests of the clearing rules. The randomized conservation test is in
//! `tests/props.rs`; these pin individual rules and every forged-fill rejection.

use crate::ledger::*;
use crate::model::*;
use crate::print::*;
use crate::store::*;
use crate::testkit::*;
use crate::wire::*;
use crate::{Op, OpAt, Batch, apply_batch};

fn fill(w: &mut World, maker: &SignedOrder, taker: &SignedOrder, qty: u64, price: u64) -> Res<Vec<CEvent>> {
    let list = vec![maker.clone(), taker.clone()];
    let mut o = Orders::new(&list, false);
    crate::ledger::fill(&mut w.s, &NativeCrypto, &mut o, Now::at(w.now), 0, 1, qty, price)
}

#[test]
fn open_and_close_moves_exact_amounts() {
    let mut w = World::new();
    let (a, b) = (w.trader("alice.near", 1_000_000_000), w.trader("bob.near", 1_000_000_000));
    // Mark 1_000 ticks; tick_value 1_000 atoms: 1 lot at mark = 1_000_000 atoms ($1).
    let sell = w.order(&b, Side::Sell, 1_010, 100, |_| {});
    let buy = w.order(&a, Side::Buy, 1_020, 100, |_| {});
    fill(&mut w, &sell, &buy, 100, 1_010).unwrap();
    let m = w.s.market(MKT).unwrap();
    let n = 100u128 * 1_010 * 1_000;
    let (mf, tf) = (fee(n, 2), fee(n, 5));
    let pa = w.s.position("alice.near", MKT);
    let pb = w.s.position("bob.near", MKT);
    assert_eq!((pa.size, pb.size), (100, -100));
    // Alice's margin is topped up to initial margin at the mark (50% IM).
    let im = initial_margin(&m, 100, 1_000) as i128;
    assert_eq!(equity(&m, &pa, 1_000), im);
    assert_eq!(equity(&m, &pb, 1_000), im);
    assert_eq!(m.open_interest, 100);
    assert_eq!(w.s.globals.fee_balance, mf + tf);
    w.check_conservation();

    // Close at 1_050: alice sells to bob.
    let bid = w.order(&b, Side::Buy, 1_050, 100, |_| {});
    let ask = w.order(&a, Side::Sell, 1_000, 100, |_| {});
    fill(&mut w, &bid, &ask, 100, 1_050).unwrap();
    assert!(w.s.position("alice.near", MKT) == Position::default());
    assert!(w.s.position("bob.near", MKT) == Position::default());
    let n2 = 100u128 * 1_050 * 1_000;
    let pnl = 100i128 * (1_050 - 1_010) * 1_000;
    let alice = 1_000_000_000i128 + pnl - tf as i128 - fee(n2, 5) as i128;
    let bob = 1_000_000_000i128 - pnl - mf as i128 - fee(n2, 2) as i128;
    assert_eq!(w.s.account("alice.near").free as i128, alice);
    assert_eq!(w.s.account("bob.near").free as i128, bob);
    assert_eq!(w.s.market(MKT).unwrap().open_interest, 0);
    w.check_conservation();
}

#[test]
fn forged_and_invalid_fills_are_rejected() {
    let mut w = World::new();
    let (a, b) = (w.trader("alice.near", 1_000_000_000), w.trader("bob.near", 1_000_000_000));
    let good_sell = w.order(&b, Side::Sell, 1_000, 10, |_| {});
    let good_buy = w.order(&a, Side::Buy, 1_000, 10, |_| {});
    let reject = |w: &mut World, m: &SignedOrder, t: &SignedOrder, q: u64, p: u64| fill(w, m, t, q, p).unwrap_err();

    // Signature by a different key (operator forging a user's order).
    let mut forged = good_buy.clone();
    forged.signature = Sig64(Trader::new("mallory.near", 99).key.sign(&forged.message()));
    assert_eq!(reject(&mut w, &good_sell, &forged, 10, 1_000), Failure { reason: Reject::BadSignature, party: Party::Taker });
    // Tampered quantity after signing.
    let mut tampered = good_buy.clone();
    tampered.qty = 20;
    assert_eq!(reject(&mut w, &good_sell, &tampered, 10, 1_000).reason, Reject::BadSignature);
    // Key not registered to the account (signed by a real key of another account).
    let mut stolen = good_buy.clone();
    stolen.public_key = b.key.public();
    stolen.signature = Sig64(b.key.sign(&stolen.message()));
    assert_eq!(reject(&mut w, &good_sell, &stolen, 10, 1_000).reason, Reject::KeyNotRegistered);
    // Wrong chain / wrong contract / wrong market, each properly signed.
    for (f, r) in [
        (Box::new(|o: &mut SignedOrder| o.chain_id = "near:mainnet".into()) as Box<dyn Fn(&mut SignedOrder)>, Reject::WrongChain),
        (Box::new(|o: &mut SignedOrder| o.clearing_contract = "other.near".into()), Reject::WrongContract),
        (Box::new(|o: &mut SignedOrder| o.market = "OTHER-PERP".into()), Reject::WrongMarket),
        (Box::new(|o: &mut SignedOrder| o.policy_hash = [9; 32]), Reject::PolicyMismatch),
        (Box::new(|o: &mut SignedOrder| o.max_fee_bps = 4), Reject::FeeAboveMax),
        (Box::new(|o: &mut SignedOrder| o.expiry_ns = 1), Reject::Expired),
        (Box::new(|o: &mut SignedOrder| o.expiry_ns = u64::MAX), Reject::ExpiryTooFar),
        (Box::new(|o: &mut SignedOrder| o.post_only = true), Reject::PostOnlyTaker),
        (Box::new(|o: &mut SignedOrder| o.price = 999), Reject::PriceOutsideLimit),
        (Box::new(|o: &mut SignedOrder| o.reduce_only = true), Reject::ReduceOnlyViolated),
    ] {
        let t = w.order(&a, Side::Buy, 1_000, 10, f);
        assert_eq!(reject(&mut w, &good_sell, &t, 10, 1_000), Failure { reason: r, party: Party::Taker }, "{r:?}");
    }
    // Same account on both sides.
    let self_sell = w.order(&a, Side::Sell, 1_000, 10, |_| {});
    assert_eq!(reject(&mut w, &self_sell, &good_buy, 10, 1_000).reason, Reject::SameAccount);
    // Both buys.
    let other_buy = w.order(&b, Side::Buy, 1_000, 10, |_| {});
    assert_eq!(reject(&mut w, &other_buy, &good_buy, 10, 1_000).reason, Reject::SideMismatch);
    // Fill price not the maker's price.
    assert_eq!(reject(&mut w, &good_sell, &good_buy, 10, 999), Failure { reason: Reject::PriceOutsideLimit, party: Party::Maker });
    // A market order can't be the maker.
    let mkt_sell = w.order(&b, Side::Sell, 1_000, 10, |o| {
        o.order_type = OrderType::Market;
        o.tif = TimeInForce::Ioc
    });
    assert_eq!(reject(&mut w, &mkt_sell, &good_buy, 10, 1_000).reason, Reject::MakerNotResting);
    // Outside the band around the mark (band 30%).
    let far_sell = w.order(&b, Side::Sell, 1_400, 10, |_| {});
    let far_buy = w.order(&a, Side::Buy, 1_400, 10, |_| {});
    assert_eq!(reject(&mut w, &far_sell, &far_buy, 10, 1_400).reason, Reject::PriceOutOfBand);

    // The good pair settles once; replaying the same fill is an overfill.
    fill(&mut w, &good_sell, &good_buy, 10, 1_000).unwrap();
    assert_eq!(reject(&mut w, &good_sell, &good_buy, 10, 1_000).reason, Reject::Overfill);
    // Partial fills can't exceed the signed quantity either.
    let s2 = w.order(&b, Side::Sell, 1_000, 5, |_| {});
    let b2 = w.order(&a, Side::Buy, 1_000, 50, |_| {});
    fill(&mut w, &s2, &b2, 3, 1_000).unwrap();
    assert_eq!(reject(&mut w, &s2, &b2, 3, 1_000), Failure { reason: Reject::Overfill, party: Party::Maker });
    fill(&mut w, &s2, &b2, 2, 1_000).unwrap();
    w.check_conservation();
}

#[test]
fn cancels_and_nonce_floor_are_enforced() {
    let mut w = World::new();
    let (a, b) = (w.trader("alice.near", 1_000_000_000), w.trader("bob.near", 1_000_000_000));
    let sell = w.order(&b, Side::Sell, 1_000, 10, |o| o.nonce = 5);
    let buy = w.order(&a, Side::Buy, 1_000, 10, |_| {});
    let h = order_hash(&NativeCrypto, &sell);
    // Cancel signed by bob's key, relayed by anyone.
    let msg = cancel_message(CHAIN, CONTRACT, "bob.near", &h, sell.expiry_ns);
    let bad = a.key.sign(&msg);
    assert_eq!(cancel(&mut w.s, &NativeCrypto, "bob.near", &h, sell.expiry_ns, Some((&b.key.public(), &bad))).unwrap_err().reason, Reject::BadSignature);
    let sig = b.key.sign(&msg);
    cancel(&mut w.s, &NativeCrypto, "bob.near", &h, sell.expiry_ns, Some((&b.key.public(), &sig))).unwrap();
    assert_eq!(fill(&mut w, &sell, &buy, 10, 1_000).unwrap_err(), Failure { reason: Reject::OrderCanceled, party: Party::Maker });
    // Nonce floor.
    let sell2 = w.order(&b, Side::Sell, 1_000, 10, |o| o.nonce = 6);
    let m = min_nonce_message(CHAIN, CONTRACT, "bob.near", 7);
    set_min_nonce(&mut w.s, &NativeCrypto, "bob.near", 7, Some((&b.key.public(), &b.key.sign(&m)))).unwrap();
    assert_eq!(fill(&mut w, &sell2, &buy, 10, 1_000).unwrap_err().reason, Reject::NonceBelowMin);
    assert_eq!(set_min_nonce(&mut w.s, &NativeCrypto, "bob.near", 7, None).unwrap_err().reason, Reject::NonceUsed);
    let sell3 = w.order(&b, Side::Sell, 1_000, 10, |o| o.nonce = 7);
    fill(&mut w, &sell3, &buy, 10, 1_000).unwrap();
}

#[test]
fn margin_caps_and_limits() {
    let mut w = World::new();
    // 50% IM: 100 lots at mark 1_000 need 50_000_000 atoms.
    let a = w.trader("alice.near", 49_000_000);
    let b = w.trader("bob.near", 1_000_000_000);
    let sell = w.order(&b, Side::Sell, 1_000, 100, |_| {});
    let buy = w.order(&a, Side::Buy, 1_000, 100, |_| {});
    assert_eq!(fill(&mut w, &sell, &buy, 100, 1_000).unwrap_err(), Failure { reason: Reject::InsufficientMargin, party: Party::Taker });
    // Position cap (max_position 10_000).
    let rich = w.trader("rich.near", 1_000_000_000_000);
    let big_sell = w.order(&b, Side::Sell, 1_000, 10_001, |_| {});
    let big_buy = w.order(&rich, Side::Buy, 1_000, 10_001, |_| {});
    assert_eq!(fill(&mut w, &big_sell, &big_buy, 10_001, 1_000).unwrap_err().reason, Reject::PositionLimit);
    // Open-interest cap (max_open_interest 20_000).
    let t: Vec<Trader> = ["c.near", "d.near", "e.near", "f.near", "g.near", "h.near"]
        .iter()
        .map(|n| w.trader(n, 1_000_000_000_000))
        .collect();
    for (sell, buy) in [(0, 1), (2, 3)] {
        let s = w.order(&t[sell], Side::Sell, 1_000, 10_000, |_| {});
        let o = w.order(&t[buy], Side::Buy, 1_000, 10_000, |_| {});
        fill(&mut w, &s, &o, 10_000, 1_000).unwrap();
    }
    assert_eq!(w.s.market(MKT).unwrap().open_interest, 20_000);
    let s = w.order(&t[4], Side::Sell, 1_000, 1, |_| {});
    let o = w.order(&t[5], Side::Buy, 1_000, 1, |_| {});
    assert_eq!(fill(&mut w, &s, &o, 1, 1_000).unwrap_err().reason, Reject::OpenInterestCap);
    // Reducing trades still work at the cap.
    let s = w.order(&t[1], Side::Sell, 1_000, 1, |_| {});
    let o = w.order(&t[0], Side::Buy, 1_000, 1, |_| {});
    fill(&mut w, &s, &o, 1, 1_000).unwrap();
    w.check_conservation();
}

#[test]
fn oracle_quorum_order_and_status() {
    let mut w = World::new();
    let r2 = Trader::new("r2", 202);
    w.s.globals.reporters.push(r2.key.public());
    w.s.globals.quorum = 2;
    let p = w.print(1_100, 2);
    let one = vec![w.reporter_sig(&p)];
    assert_eq!(oracle(&mut w.s, &NativeCrypto, Now::at(w.now), MKT, &p, &one).unwrap_err().reason, Reject::OracleQuorum);
    // The same reporter twice still counts once.
    let dup = vec![w.reporter_sig(&p), w.reporter_sig(&p)];
    assert_eq!(oracle(&mut w.s, &NativeCrypto, Now::at(w.now), MKT, &p, &dup).unwrap_err().reason, Reject::OracleQuorum);
    // A non-allowlisted signer doesn't count.
    let x = Trader::new("x", 77);
    let rogue = vec![w.reporter_sig(&p), (x.key.public(), Sig64(x.key.sign(&p)))];
    assert_eq!(oracle(&mut w.s, &NativeCrypto, Now::at(w.now), MKT, &p, &rogue).unwrap_err().reason, Reject::OracleQuorum);
    let two = vec![w.reporter_sig(&p), (r2.key.public(), Sig64(r2.key.sign(&p)))];
    oracle(&mut w.s, &NativeCrypto, Now::at(w.now), MKT, &p, &two).unwrap();
    assert_eq!(w.s.market(MKT).unwrap().mark, 1_100);
    // Replaying the same (or an older) print is refused.
    assert_eq!(oracle(&mut w.s, &NativeCrypto, Now::at(w.now), MKT, &p, &two).unwrap_err().reason, Reject::OracleNotNewer);
}

#[test]
fn not_reported_stale_and_disputed_prints_restrict_trading() {
    let mut w = World::new();
    let (a, b) = (w.trader("alice.near", 1_000_000_000), w.trader("bob.near", 1_000_000_000));
    let s = w.order(&b, Side::Sell, 1_000, 10, |_| {});
    let t = w.order(&a, Side::Buy, 1_000, 10, |_| {});
    fill(&mut w, &s, &t, 10, 1_000).unwrap();
    // NOT_REPORTED for the next day: the mark stays, the market goes CLOSE_ONLY.
    w.advance_day();
    let p = w.print_with(0, 2, |p| {
        p.status = OracleStatus::Unavailable;
        p.data_status = DataStatus::NotReported;
    });
    w.post_print(&p).unwrap();
    let m = w.s.market(MKT).unwrap();
    assert_eq!(m.mark, 1_000);
    assert_eq!(effective_status(&m, Now::at(w.now), w.s.globals.settle_grace_ns), MarketStatus::CloseOnly);
    // Opening is refused, reducing works.
    let s2 = w.order(&b, Side::Sell, 1_000, 5, |_| {});
    let t2 = w.order(&a, Side::Buy, 1_000, 5, |_| {});
    assert_eq!(fill(&mut w, &s2, &t2, 5, 1_000).unwrap_err().reason, Reject::CloseOnly);
    let s3 = w.order(&a, Side::Sell, 1_000, 5, |_| {});
    let t3 = w.order(&b, Side::Buy, 1_000, 5, |_| {});
    fill(&mut w, &s3, &t3, 5, 1_000).unwrap();
    // No liquidation on a stale mark.
    assert!(!liquidatable(&mut w.s, "alice.near", MKT, Now::at(w.now)));
    // 24h with no new print: still CLOSE_ONLY (the last print is never treated as fresh).
    w.now += NS_PER_DAY;
    let m = w.s.market(MKT).unwrap();
    assert_eq!(effective_status(&m, Now::at(w.now), w.s.globals.settle_grace_ns), MarketStatus::CloseOnly);
    // A DISPUTED print halts the market completely.
    let d = w.print_with(1_000, 3, |p| p.status = OracleStatus::Disputed);
    w.post_print(&d).unwrap();
    let m = w.s.market(MKT).unwrap();
    assert_eq!(effective_status(&m, Now::at(w.now), w.s.globals.settle_grace_ns), MarketStatus::Halted);
    let s4 = w.order(&a, Side::Sell, 1_000, 5, |_| {});
    let t4 = w.order(&b, Side::Buy, 1_000, 5, |_| {});
    assert_eq!(fill(&mut w, &s4, &t4, 5, 1_000).unwrap_err().reason, Reject::MarketNotTrading);
    w.check_conservation();
}

#[test]
fn daily_window_and_jump_halt() {
    let mut w = World::new();
    let grace = w.s.globals.settle_grace_ns;
    let day = w.now - w.now % NS_PER_DAY;
    // 21:59:59 UTC: active. 22:00: close-only until midnight.
    let m = w.s.market(MKT).unwrap();
    assert_eq!(effective_status(&m, Now::at(day + 79_199 * NS_PER_SEC), grace), MarketStatus::Active);
    assert_eq!(effective_status(&m, Now::at(day + 79_200 * NS_PER_SEC), grace), MarketStatus::CloseOnly);
    // After midnight, still close-only until a print covering the finished day lands.
    assert_eq!(effective_status(&m, Now::at(day + NS_PER_DAY + 3_600 * NS_PER_SEC), grace), MarketStatus::CloseOnly);
    w.now = day + NS_PER_DAY + 6 * 3_600 * NS_PER_SEC;
    let p = w.print(1_300, 2);
    w.post_print(&p).unwrap();
    let m = w.s.market(MKT).unwrap();
    assert_eq!(m.mark, 1_300);
    assert_eq!(effective_status(&m, Now::at(w.now), grace), MarketStatus::Active);
    // A print beyond max_jump_bps (200%) halts instead of moving the mark.
    w.now += NS_PER_DAY;
    let p = w.print(5_000, 3);
    w.post_print(&p).unwrap();
    let m = w.s.market(MKT).unwrap();
    assert_eq!((m.mark, m.status), (1_300, MarketStatus::Halted));
}

#[test]
fn funding_is_zero_sum_and_pauses_when_stale() {
    let mut w = World::new();
    let (a, b) = (w.trader("alice.near", 1_000_000_000), w.trader("bob.near", 1_000_000_000));
    let s = w.order(&b, Side::Sell, 1_100, 100, |_| {});
    let t = w.order(&a, Side::Buy, 1_100, 100, |_| {});
    fill(&mut w, &s, &t, 100, 1_100).unwrap();
    assert_eq!(funding(&mut w.s, Now::at(w.now), MKT).unwrap_err().reason, Reject::FundingNotDue);
    w.now += 8 * 3_600 * NS_PER_SEC;
    let ev = funding(&mut w.s, Now::at(w.now), MKT).unwrap();
    // Traded 10% over the index: premium 100_000 ppm / divisor 24 = 4_166, capped at 2_500 ppm.
    assert_eq!(ev, vec![CEvent::Funding { market: MKT.into(), rate_ppm: 2_500, pay_per_lot: 2_500 }]);
    let m = w.s.market(MKT).unwrap();
    let (pa, pb) = (w.s.position("alice.near", MKT), w.s.position("bob.near", MKT));
    assert_eq!(funding_owed(&m, &pa), 250_000);
    assert_eq!(funding_owed(&m, &pa) + funding_owed(&m, &pb), 0);
    w.check_conservation();
    // Stale mark: funding is applied with rate 0.
    w.now += 3 * NS_PER_DAY;
    let ev = funding(&mut w.s, Now::at(w.now), MKT).unwrap();
    assert_eq!(ev, vec![CEvent::Funding { market: MKT.into(), rate_ppm: 0, pay_per_lot: 0 }]);
}

#[test]
fn liquidation_on_book_then_insurance_then_socialized() {
    let mut w = World::new();
    w.fund_insurance(0);
    let (a, b, c) = (
        w.trader("alice.near", 50_050_000),
        w.trader("bob.near", 1_000_000_000),
        w.trader("carol.near", 1_000_000_000),
    );
    // Alice long 100 at 1_000 with exactly initial margin (50%).
    let s = w.order(&b, Side::Sell, 1_000, 100, |_| {});
    let t = w.order(&a, Side::Buy, 1_000, 100, |_| {});
    fill(&mut w, &s, &t, 100, 1_000).unwrap();
    assert!(!liquidatable(&mut w.s, "alice.near", MKT, Now::at(w.now)));
    // Index drops 60%: equity 50_000_000 - 60_000_000 < 0, bankrupt.
    w.next_print(400);
    assert!(liquidatable(&mut w.s, "alice.near", MKT, Now::at(w.now)));
    // Carol bids 40 lots at 390 (within 3% slippage of 400).
    let bid = w.order(&c, Side::Buy, 390, 40, |_| {});
    let list = vec![bid.clone()];
    let mut o = Orders::new(&list, false);
    // Insurance can't margin the other 60 lots (it's empty), and ADL is required instead.
    let err = liquidate(&mut w.s, &NativeCrypto, &mut o, Now::at(w.now), "alice.near", MKT, &[(0, 40)], &[], &[]).unwrap_err();
    assert_eq!(err.reason, Reject::InsuranceCannotAbsorb);
    // ADL against bob (the only short), and the deficit is socialized over all shorts: an
    // incomplete list is refused.
    let mut o = Orders::new(&list, false);
    let err = liquidate(&mut w.s, &NativeCrypto, &mut o, Now::at(w.now), "alice.near", MKT, &[(0, 40)], &[("bob.near".into(), 60)], &[]).unwrap_err();
    assert_eq!(err.reason, Reject::SocializeIncomplete);
    let mut o = Orders::new(&list, false);
    let ev = liquidate(
        &mut w.s,
        &NativeCrypto,
        &mut o,
        Now::at(w.now),
        "alice.near",
        MKT,
        &[(0, 40)],
        &[("bob.near".into(), 60)],
        &["bob.near".into(), "carol.near".into()],
    )
    .unwrap_err();
    // After ADL closed bob, carol (long 40) isn't on the profiting side: still refused.
    assert_eq!(ev.reason, Reject::SocializeIncomplete);
    let mut o = Orders::new(&list, false);
    let ev = liquidate(&mut w.s, &NativeCrypto, &mut o, Now::at(w.now), "alice.near", MKT, &[(0, 40)], &[("bob.near".into(), 60)], &["bob.near".into()]).unwrap();
    let def = ev.iter().find_map(|e| match e {
        CEvent::Deficit { deficit, insurance_paid, socialized, bad_debt, .. } => Some((*deficit, *insurance_paid, *socialized, *bad_debt)),
        _ => None,
    });
    // Loss: 40 lots at 390 and 60 at 400 vs 1_000 entry = 40·610k + 60·600k = 60.4M; margin 50M.
    assert_eq!(def, Some((10_400_000, 0, 10_400_000, 0)));
    assert_eq!(w.s.position("alice.near", MKT), Position::default());
    assert_eq!(w.s.account("alice.near").free, 0);
    w.check_conservation();
}

#[test]
fn insurance_takeover_and_fee_then_bad_debt_halts() {
    let mut w = World::new();
    w.fund_insurance(10_000_000_000);
    let (a, b) = (w.trader("alice.near", 50_050_000), w.trader("bob.near", 1_000_000_000));
    let s = w.order(&b, Side::Sell, 1_000, 100, |_| {});
    let t = w.order(&a, Side::Buy, 1_000, 100, |_| {});
    fill(&mut w, &s, &t, 100, 1_000).unwrap();
    // Down 30%: equity 20M < MM 7M? No: MM at 700 is 100·700k·10% = 7M; equity 50M − 30M = 20M.
    w.next_print(700);
    assert!(!liquidatable(&mut w.s, "alice.near", MKT, Now::at(w.now)));
    // Down 45%: equity 5M < MM 5.5M.
    w.next_print(550);
    assert!(liquidatable(&mut w.s, "alice.near", MKT, Now::at(w.now)));
    let ins0 = w.s.account(INSURANCE).free;
    let mut o = Orders::new(&[], false);
    liquidate(&mut w.s, &NativeCrypto, &mut o, Now::at(w.now), "alice.near", MKT, &[], &[], &[]).unwrap();
    // Insurance holds alice's long at the mark; it was charged initial margin and received the
    // liquidation fee (1% of 55M = 550k, below the 5M equity left).
    assert_eq!(w.s.position(INSURANCE, MKT).size, 100);
    assert_eq!(w.s.account("alice.near").free, 5_000_000 - 550_000);
    let im = initial_margin(&w.s.market(MKT).unwrap(), 100, 550);
    assert_eq!(w.s.account(INSURANCE).free, ins0 - im + 550_000);
    w.check_conservation();

    // A second, empty fund and an empty opposite side: bad debt, market halted.
    let mut w = World::new();
    w.fund_insurance(0);
    let (a, b) = (w.trader("alice.near", 50_050_000), w.trader("bob.near", 1_000_000_000));
    let s = w.order(&b, Side::Sell, 1_000, 100, |_| {});
    let t = w.order(&a, Side::Buy, 1_000, 100, |_| {});
    fill(&mut w, &s, &t, 100, 1_000).unwrap();
    w.next_print(400);
    let mut o = Orders::new(&[], false);
    // ADL closes bob at the mark; the deficit then has no profiting holders left.
    let ev = liquidate(&mut w.s, &NativeCrypto, &mut o, Now::at(w.now), "alice.near", MKT, &[], &[("bob.near".into(), 100)], &[]).unwrap();
    assert!(ev.iter().any(|e| matches!(e, CEvent::MarketHalted { reason: HaltReason::BadDebt, .. })));
    let m = w.s.market(MKT).unwrap();
    assert_eq!((m.status, m.bad_debt), (MarketStatus::Halted, 10_000_000));
    w.check_conservation();
}

#[test]
fn withdraw_forced_and_margin_adjust() {
    let mut w = World::new();
    let a = w.trader("alice.near", 100_000_000);
    let b = w.trader("bob.near", 1_000_000_000);
    let msg = withdraw_message(CHAIN, CONTRACT, "alice.near", USDC, 10, 1);
    // Trading-only key can't withdraw.
    let t = Trader::new("alice.near", 55);
    w.s.add_key(t.key.public(), KeyInfo { account: "alice.near".into(), expires_ns: 0, can_withdraw: false });
    assert_eq!(withdraw(&mut w.s, &NativeCrypto, Now::at(w.now), "alice.near", 10, 1, &t.key.public(), &t.key.sign(&msg)).unwrap_err().reason, Reject::KeyScope);
    // Wrong contract in the signed message.
    let other = withdraw_message(CHAIN, "evil.near", "alice.near", USDC, 10, 1);
    assert_eq!(withdraw(&mut w.s, &NativeCrypto, Now::at(w.now), "alice.near", 10, 1, &a.key.public(), &a.key.sign(&other)).unwrap_err().reason, Reject::BadSignature);
    let ev = withdraw(&mut w.s, &NativeCrypto, Now::at(w.now), "alice.near", 10, 1, &a.key.public(), &a.key.sign(&msg)).unwrap();
    assert!(ev.contains(&CEvent::Payout { account: "alice.near".into(), amount: 10, kind: PayoutKind::Withdraw }));
    assert_eq!(withdraw(&mut w.s, &NativeCrypto, Now::at(w.now), "alice.near", 10, 1, &a.key.public(), &a.key.sign(&msg)).unwrap_err().reason, Reject::NonceUsed);
    // Margin: open, add, remove down to initial margin only.
    let s = w.order(&b, Side::Sell, 1_000, 100, |_| {});
    let t = w.order(&a, Side::Buy, 1_000, 100, |_| {});
    fill(&mut w, &s, &t, 100, 1_000).unwrap();
    let add = margin_message(CHAIN, CONTRACT, "alice.near", MKT, 1_000_000, 2);
    adjust_margin(&mut w.s, &NativeCrypto, Now::at(w.now), "alice.near", MKT, 1_000_000, 2, &a.key.public(), &a.key.sign(&add)).unwrap();
    let rm = margin_message(CHAIN, CONTRACT, "alice.near", MKT, -1_000_001, 3);
    assert_eq!(adjust_margin(&mut w.s, &NativeCrypto, Now::at(w.now), "alice.near", MKT, -1_000_001, 3, &a.key.public(), &a.key.sign(&rm)).unwrap_err().reason, Reject::InsufficientMargin);
    // Forced withdrawal pays min(requested, free).
    let free = w.s.account("alice.near").free;
    w.s.put_forced(0, ForcedRequest { account: "alice.near".into(), amount: u128::MAX, created_ns: w.now, done: false, paid: 0 });
    forced(&mut w.s, 0).unwrap();
    assert_eq!(w.s.account("alice.near").free, 0);
    assert_eq!(w.s.forced(0).unwrap().paid, free);
    assert_eq!(forced(&mut w.s, 0).unwrap_err().reason, Reject::ForcedDone);
    w.check_conservation();
}

#[test]
fn batch_times_are_monotonic_and_bounded() {
    let mut w = World::new();
    let b = Batch {
        id: 1,
        orders: vec![],
        ops: vec![
            OpAt { ts_ns: w.now, op: Op::Funding { market: MKT.into() } },
            OpAt { ts_ns: w.now - 1, op: Op::Funding { market: MKT.into() } },
            OpAt { ts_ns: w.now + 3_600 * NS_PER_SEC, op: Op::Funding { market: MKT.into() } },
        ],
        journal_seq: 0,
        journal_hash: [0; 32],
    };
    let r = apply_batch(&mut w.s, &NativeCrypto, &b, Some(w.now), false);
    assert_eq!(r[0].result.as_ref().unwrap_err().reason, Reject::FundingNotDue);
    assert_eq!(r[1].result.as_ref().unwrap_err().reason, Reject::TimeBackwards);
    assert_eq!(r[2].result.as_ref().unwrap_err().reason, Reject::TimeFuture);
}

#[test]
fn expiry_grace_bounds_backdating() {
    let mut w = World::new();
    let (a, b) = (w.trader("alice.near", 1_000_000_000), w.trader("bob.near", 1_000_000_000));
    let exp = w.now + 60 * NS_PER_SEC;
    let s = w.order(&b, Side::Sell, 1_000, 10, |o| o.expiry_ns = exp);
    let t = w.order(&a, Side::Buy, 1_000, 10, |_| {});
    let list = vec![s, t];
    // Stamped before expiry but landing more than the grace (300 s) after it: refused.
    let mut o = Orders::new(&list, false);
    let late = Now { op: exp - 1, block: exp + 301 * NS_PER_SEC };
    assert_eq!(crate::ledger::fill(&mut w.s, &NativeCrypto, &mut o, late, 0, 1, 10, 1_000).unwrap_err().reason, Reject::Expired);
    let ok = Now { op: exp - 1, block: exp + 299 * NS_PER_SEC };
    crate::ledger::fill(&mut w.s, &NativeCrypto, &mut o, ok, 0, 1, 10, 1_000).unwrap();
}
