use obkit_engine::testkit::*;
use obkit_engine::*;

const LOT: u128 = 10_000_000_000_000_000_000_000; // 0.01 NEAR in yocto

fn run(e: &mut Engine, c: Command) -> (Result<Ack, Reject>, Vec<Event>) {
    let mut out = vec![];
    let r = e.apply(&c, &mut out);
    e.check_invariants().unwrap();
    (r, out)
}

fn ok(e: &mut Engine, c: Command) -> Vec<Event> {
    let (r, ev) = run(e, c.clone());
    assert!(r.is_ok(), "{c:?} -> {r:?}");
    ev
}

fn bal(e: &Engine, a: AccountId, s: AssetId) -> Balance {
    e.accounts[a as usize].balance(s)
}

fn fills(ev: &[Event]) -> Vec<(u64, u64, AccountId, AccountId)> {
    ev.iter()
        .filter_map(|x| {
            if let Event::Fill {
                price, qty, maker, taker, ..
            } = x
            {
                Some((*price, *qty, *maker, *taker))
            } else {
                None
            }
        })
        .collect()
}

fn placed(r: Result<Ack, Reject>) -> (OrderId, OrderStatus, u64, u64) {
    match r.unwrap() {
        Ack::Placed {
            order_id,
            status,
            filled,
            remaining,
        } => (order_id, status, filled, remaining),
        a => panic!("unexpected {a:?}"),
    }
}

// Accounts u0..u3 get ids 1..4 (0 is the fee account).
const A: AccountId = 1;
const B: AccountId = 2;
const C: AccountId = 3;

#[test]
fn match_at_maker_price_with_exact_fees_and_holds() {
    // 10 bps taker, 2 bps maker. 1000 NEAR and $10,000 each.
    let mut e = engine_with(3, 100_000 * LOT, 10_000_000_000, 2, 10);
    let (r, _) = run(&mut e, order(A, Side::Sell, 42_500, 1_000, Tif::Gtc));
    let (sell_id, st, _, rem) = placed(r);
    assert_eq!((st, rem), (OrderStatus::Resting, 1_000));
    assert_eq!(bal(&e, A, NEAR).held, 1_000 * LOT);
    // B buys 400 lots (4 NEAR) with limit $4.30; fills at the maker's $4.25.
    let (r, ev) = run(&mut e, order(B, Side::Buy, 43_000, 400, Tif::Gtc));
    let (_, st, filled, _) = placed(r);
    assert_eq!((st, filled), (OrderStatus::Filled, 400));
    assert_eq!(fills(&ev), vec![(42_500, 400, A, B)]);
    let notional = 400 * 42_500u128; // $17.00 = 17_000_000 atoms
    let tfee = (notional * 10).div_ceil(10_000);
    let mfee = (notional * 2).div_ceil(10_000);
    assert_eq!(
        bal(&e, B, USDC),
        Balance {
            total: 10_000_000_000 - notional - tfee,
            held: 0
        }
    );
    assert_eq!(bal(&e, B, NEAR).total, 100_000 * LOT + 400 * LOT);
    assert_eq!(bal(&e, A, USDC).total, 10_000_000_000 + notional - mfee);
    assert_eq!(
        bal(&e, A, NEAR),
        Balance {
            total: 100_000 * LOT - 400 * LOT,
            held: 600 * LOT
        }
    );
    assert_eq!(bal(&e, 0, USDC).total, tfee + mfee);
    assert_eq!(e.markets[0].book.get(sell_id).unwrap().rem, 600);
    assert!(ev.contains(&Event::Level {
        market: M,
        side: Side::Sell,
        price: 42_500,
        qty: 600
    }));
}

#[test]
fn price_then_time_priority() {
    let mut e = engine_with(4, 100_000 * LOT, 10_000_000_000, 0, 0);
    ok(&mut e, order(A, Side::Sell, 42_600, 10, Tif::Gtc));
    ok(&mut e, order(B, Side::Sell, 42_500, 10, Tif::Gtc));
    ok(&mut e, order(C, Side::Sell, 42_500, 10, Tif::Gtc));
    let (_, ev) = run(&mut e, order(4, Side::Buy, 42_600, 25, Tif::Ioc));
    assert_eq!(fills(&ev), vec![(42_500, 10, B, 4), (42_500, 10, C, 4), (42_600, 5, A, 4)]);
}

#[test]
fn ioc_remainder_canceled_and_hold_released() {
    let mut e = engine_with(2, 100_000 * LOT, 10_000_000_000, 0, 10);
    ok(&mut e, order(A, Side::Sell, 42_500, 10, Tif::Gtc));
    let (r, ev) = run(&mut e, order(B, Side::Buy, 42_500, 50, Tif::Ioc));
    let (id, st, filled, rem) = placed(r);
    assert_eq!((st, filled, rem), (OrderStatus::Canceled, 10, 0));
    assert!(ev.contains(&Event::Canceled {
        order_id: id,
        account: B,
        market: M,
        remaining: 40,
        reason: CancelReason::Unfilled
    }));
    assert_eq!(bal(&e, B, USDC).held, 0);
    assert!(e.markets[0].book.best_bid().is_none());
}

#[test]
fn fok_all_or_nothing() {
    let mut e = engine_with(3, 100_000 * LOT, 10_000_000_000, 0, 0);
    ok(&mut e, order(A, Side::Sell, 42_500, 10, Tif::Gtc));
    ok(&mut e, order(A, Side::Sell, 42_600, 10, Tif::Gtc));
    let before = e.state_hash();
    let (r, ev) = run(&mut e, order(B, Side::Buy, 42_600, 21, Tif::Fok));
    assert_eq!(r, Err(Reject::FokUnfillable));
    assert!(matches!(
        ev[..],
        [Event::Rejected {
            reason: Reject::FokUnfillable,
            ..
        }]
    ));
    assert_eq!(e.state_hash(), before, "a rejected FOK changes nothing");
    let (r, _) = run(&mut e, order(B, Side::Buy, 42_600, 20, Tif::Fok));
    assert_eq!(placed(r).1, OrderStatus::Filled);
}

#[test]
fn post_only_rejected_if_crossing() {
    let mut e = engine_with(2, 100_000 * LOT, 10_000_000_000, 0, 0);
    ok(&mut e, order(A, Side::Sell, 42_500, 10, Tif::Gtc));
    assert_eq!(
        run(&mut e, order(B, Side::Buy, 42_500, 1, Tif::PostOnly)).0,
        Err(Reject::PostOnlyWouldCross)
    );
    assert_eq!(
        placed(run(&mut e, order(B, Side::Buy, 42_499, 1, Tif::PostOnly)).0).1,
        OrderStatus::Resting
    );
}

#[test]
fn self_trade_prevention_modes() {
    let place = |stp| {
        Command::Place(NewOrder {
            account: A,
            market: M,
            side: Side::Buy,
            price: 42_600,
            qty: 30,
            tif: Tif::Gtc,
            stp,
            client_id: 0,
            ts_ns: 0,
        })
    };
    // Book: A sells 10 @42500, then B sells 10 @42500 (behind A), C sells 10 @42600.
    let setup = || {
        let mut e = engine_with(3, 100_000 * LOT, 10_000_000_000, 0, 0);
        ok(&mut e, order(A, Side::Sell, 42_500, 10, Tif::Gtc));
        ok(&mut e, order(B, Side::Sell, 42_500, 10, Tif::Gtc));
        ok(&mut e, order(C, Side::Sell, 42_600, 10, Tif::Gtc));
        e
    };
    // cancel_taker: A's own ask is first in line, so A's buy is canceled without trading.
    let mut e = setup();
    let (r, ev) = run(&mut e, place(Stp::CancelTaker));
    assert_eq!(placed(r), (4, OrderStatus::Canceled, 0, 0));
    assert!(fills(&ev).is_empty());
    assert_eq!(e.markets[0].book.levels(Side::Sell)[&42_500].qty, 20);
    // cancel_maker: A's ask is canceled, A trades with B then C, nothing rests.
    let mut e = setup();
    let (r, ev) = run(&mut e, place(Stp::CancelMaker));
    assert_eq!(placed(r), (4, OrderStatus::Resting, 20, 10));
    assert_eq!(fills(&ev), vec![(42_500, 10, B, A), (42_600, 10, C, A)]);
    assert!(ev.iter().any(|x| matches!(
        x,
        Event::Canceled {
            order_id: 1,
            reason: CancelReason::SelfTrade,
            ..
        }
    )));
    // cancel_both: A's ask and A's buy both canceled.
    let mut e = setup();
    let (r, ev) = run(&mut e, place(Stp::CancelBoth));
    assert_eq!(placed(r).1, OrderStatus::Canceled);
    assert!(fills(&ev).is_empty());
    assert_eq!(e.markets[0].book.levels(Side::Sell)[&42_500].qty, 10);
    assert_eq!(bal(&e, A, NEAR).held, 0);
}

#[test]
fn balance_checks_and_cancels() {
    let mut e = engine_with(2, 10 * LOT, 1_000_000, 0, 10);
    // $1 buys at most 23 lots at $4.25 (+ fee): 24 lots cost $1.02 + fee.
    assert_eq!(
        run(&mut e, order(A, Side::Buy, 42_500, 24, Tif::Gtc)).0,
        Err(Reject::InsufficientBalance)
    );
    let (id, ..) = placed(run(&mut e, order(A, Side::Buy, 42_500, 23, Tif::Gtc)).0);
    assert_eq!(
        run(&mut e, order(A, Side::Sell, 50_000, 11, Tif::Gtc)).0,
        Err(Reject::InsufficientBalance)
    );
    assert_eq!(
        run(
            &mut e,
            Command::Cancel {
                account: B,
                market: M,
                order_id: id
            }
        )
        .0,
        Err(Reject::UnknownOrder)
    );
    assert_eq!(
        run(
            &mut e,
            Command::Cancel {
                account: A,
                market: M,
                order_id: id
            }
        )
        .0,
        Ok(Ack::Canceled {
            order_id: id,
            remaining: 23
        })
    );
    assert_eq!(bal(&e, A, USDC).held, 0);
    // client ids: duplicate rejected while open, cancel by client id.
    let co = |cid| {
        Command::Place(NewOrder {
            account: A,
            market: M,
            side: Side::Sell,
            price: 50_000,
            qty: 1,
            tif: Tif::Gtc,
            stp: Stp::CancelTaker,
            client_id: cid,
            ts_ns: 0,
        })
    };
    ok(&mut e, co(77));
    assert_eq!(run(&mut e, co(77)).0, Err(Reject::DuplicateClientId));
    assert!(matches!(
        run(
            &mut e,
            Command::CancelByClientId {
                account: A,
                market: M,
                client_id: 77
            }
        )
        .0,
        Ok(Ack::Canceled { .. })
    ));
    ok(&mut e, co(77));
    ok(&mut e, co(78));
    assert_eq!(
        run(&mut e, Command::CancelAll { account: A, market: None }).0,
        Ok(Ack::CanceledAll { count: 2 })
    );
}

#[test]
fn chain_events_sequence_and_dedupe() {
    let mut e = engine_with(1, LOT, 1, 0, 0); // used seqs 1..=3
    let dep = |seq| {
        Command::Chain(ChainEvent::Deposit {
            seq,
            account: "x.near".into(),
            token: "usdc.near".into(),
            amount: 5,
        })
    };
    assert_eq!(run(&mut e, dep(5)).0, Err(Reject::ChainEventGap));
    assert_eq!(run(&mut e, dep(4)).0, Ok(Ack::Ok));
    assert_eq!(run(&mut e, dep(4)).0, Ok(Ack::Duplicate));
    let id = e.account_id("x.near").unwrap();
    assert_eq!(bal(&e, id, USDC).total, 5);
    // Unknown token: cursor still advances.
    let bad = Command::Chain(ChainEvent::Deposit {
        seq: 5,
        account: "x.near".into(),
        token: "nope.near".into(),
        amount: 5,
    });
    assert_eq!(run(&mut e, bad).0, Err(Reject::UnknownAsset));
    assert_eq!(e.chain_cursor, 5);
}

fn wd(account: AccountId, amount: u128, nonce: u64) -> Command {
    Command::Withdraw {
        account,
        asset: USDC,
        amount,
        nonce,
        public_key: [(account - 1) as u8; 32],
        signature: vec![9; 64],
    }
}

#[test]
fn withdrawals_and_batches() {
    let mut e = engine_with(2, 100 * LOT, 10_000_000, 0, 10);
    assert_eq!(
        run(
            &mut e,
            Command::Withdraw {
                account: A,
                asset: USDC,
                amount: 1,
                nonce: 1,
                public_key: [42; 32],
                signature: vec![]
            }
        )
        .0,
        Err(Reject::KeyNotRegistered)
    );
    assert_eq!(run(&mut e, wd(A, 10_000_001, 1)).0, Err(Reject::InsufficientBalance));
    assert_eq!(run(&mut e, wd(A, 1_000_000, 5)).0, Ok(Ack::WithdrawQueued { nonce: 5 }));
    assert_eq!(run(&mut e, wd(A, 1, 5)).0, Err(Reject::NonceTooLow));
    // A trade: B sells 10 lots to A at $4.25.
    ok(&mut e, order(B, Side::Sell, 42_500, 10, Tif::Gtc));
    ok(&mut e, order(A, Side::Buy, 42_500, 10, Tif::Ioc));
    let (r, ev) = run(
        &mut e,
        Command::CutBatch {
            journal_seq: 99,
            journal_hash: [1; 32],
        },
    );
    assert_eq!(r, Ok(Ack::BatchesCut { first: 1, count: 1 }));
    let Event::BatchCut { batch } = &ev[0] else { panic!() };
    let fee = (425_000u128 * 10).div_ceil(10_000);
    assert_eq!(
        batch.entries,
        vec![
            DeltaEntry {
                account: "fees.obkit.near".into(),
                token: "usdc.near".into(),
                delta: fee as i128
            },
            DeltaEntry {
                account: "u0.near".into(),
                token: "wrap.near".into(),
                delta: (10 * LOT) as i128
            },
            DeltaEntry {
                account: "u0.near".into(),
                token: "usdc.near".into(),
                delta: -((425_000 + fee) as i128)
            },
            DeltaEntry {
                account: "u1.near".into(),
                token: "wrap.near".into(),
                delta: -((10 * LOT) as i128)
            },
            DeltaEntry {
                account: "u1.near".into(),
                token: "usdc.near".into(),
                delta: 425_000
            },
        ]
    );
    assert_eq!(batch.withdrawals.len(), 1);
    assert!(batch.seal && batch.journal_seq == 99);
    assert_eq!(e.settle.unconfirmed.len(), 1);
    // Nothing pending: an empty cut makes no batch.
    assert_eq!(
        run(
            &mut e,
            Command::CutBatch {
                journal_seq: 100,
                journal_hash: [0; 32]
            }
        )
        .0,
        Ok(Ack::BatchesCut { first: 2, count: 0 })
    );
    // Chain confirms it.
    let seq = e.chain_cursor + 1;
    ok(&mut e, Command::Chain(ChainEvent::BatchSettled { seq, batch_id: 1 }));
    assert!(e.settle.unconfirmed.is_empty());
    // A reverted withdrawal is credited back.
    let before = bal(&e, A, USDC).total;
    ok(
        &mut e,
        Command::Chain(ChainEvent::WithdrawalReverted {
            seq: seq + 1,
            account: "u0.near".into(),
            token: "usdc.near".into(),
            amount: 1_000_000,
        }),
    );
    assert_eq!(bal(&e, A, USDC).total, before + 1_000_000);
}

#[test]
fn batch_packing_respects_limits_and_keeps_accounts_whole() {
    let mut e = Engine::new(EngineConfig {
        fee_account: "fees.near".into(),
        max_batch_entries: 5,
        max_batch_withdrawals: 2,
        insurance_account: "insurance.near".into(),
    });
    let mut out = vec![];
    let src = engine_with(0, 0, 0, 0, 0);
    for a in &src.assets {
        e.apply(&Command::AddAsset(a.clone()), &mut out).unwrap();
    }
    e.apply(&Command::AddMarket(spec(0, 10)), &mut out).unwrap();
    let mut seq = 0;
    for i in 0..6 {
        for (t, amt) in [("wrap.near", 1_000 * LOT), ("usdc.near", 1_000_000_000)] {
            seq += 1;
            e.apply(
                &Command::Chain(ChainEvent::Deposit {
                    seq,
                    account: format!("u{i}.near"),
                    token: t.into(),
                    amount: amt,
                }),
                &mut out,
            )
            .unwrap();
        }
        seq += 1;
        e.apply(
            &Command::Chain(ChainEvent::KeyAdded {
                seq,
                account: format!("u{i}.near"),
                public_key: [i as u8; 32],
            }),
            &mut out,
        )
        .unwrap();
    }
    // Accounts 1-3 sell to accounts 4-6: every account gets 2 entries, the fee account 1.
    for i in 1..=3 {
        e.apply(&order(i, Side::Sell, 42_500, 1, Tif::Gtc), &mut out).unwrap();
        e.apply(&order(i + 3, Side::Buy, 42_500, 1, Tif::Ioc), &mut out).unwrap();
    }
    for i in 1..=5u32 {
        e.apply(
            &Command::Withdraw {
                account: i,
                asset: USDC,
                amount: 1,
                nonce: 1,
                public_key: [(i - 1) as u8; 32],
                signature: vec![],
            },
            &mut out,
        )
        .unwrap();
    }
    out.clear();
    let r = e
        .apply(
            &Command::CutBatch {
                journal_seq: 1,
                journal_hash: [0; 32],
            },
            &mut out,
        )
        .unwrap();
    let batches: Vec<&Batch> = out
        .iter()
        .filter_map(|x| if let Event::BatchCut { batch } = x { Some(batch) } else { None })
        .collect();
    assert_eq!(
        r,
        Ack::BatchesCut {
            first: 1,
            count: batches.len() as u32
        }
    );
    let n_entries: usize = batches.iter().map(|b| b.entries.len()).sum();
    assert_eq!(n_entries, 13);
    for b in &batches {
        assert!(b.entries.len() <= 5);
        assert!(b.withdrawals.len() + b.forced.len() <= 2);
        assert_eq!(b.seal, b.id == batches.last().unwrap().id);
    }
    // No account's entries span two batches.
    let mut seen = std::collections::HashMap::new();
    for b in &batches {
        for d in &b.entries {
            assert_eq!(*seen.entry(d.account.clone()).or_insert(b.id), b.id);
        }
    }
    // Withdrawals only after the last delta batch.
    let last_delta = batches.iter().filter(|b| !b.entries.is_empty()).map(|b| b.id).max().unwrap();
    let first_w = batches.iter().filter(|b| !b.withdrawals.is_empty()).map(|b| b.id).min().unwrap();
    assert!(first_w >= last_delta);
    assert_eq!(batches.iter().map(|b| b.withdrawals.len()).sum::<usize>(), 5);
}

#[test]
fn forced_withdrawal_cancels_orders_and_pays_available() {
    let mut e = engine_with(2, 100 * LOT, 10_000_000, 0, 0);
    ok(&mut e, order(A, Side::Buy, 42_500, 100, Tif::Gtc));
    let seq = e.chain_cursor + 1;
    let (r, ev) = run(
        &mut e,
        Command::Chain(ChainEvent::ForcedWithdrawal {
            seq,
            request_id: 0,
            account: "u0.near".into(),
            token: "usdc.near".into(),
            amount: u128::MAX,
        }),
    );
    assert_eq!(r, Ok(Ack::Ok));
    assert!(ev.iter().any(|x| matches!(
        x,
        Event::Canceled {
            reason: CancelReason::ForcedWithdrawal,
            ..
        }
    )));
    assert!(ev.contains(&Event::ForcedQueued {
        request_id: 0,
        account: A,
        asset: USDC,
        amount: 10_000_000
    }));
    assert_eq!(bal(&e, A, USDC), Balance { total: 0, held: 0 });
    let mut out = vec![];
    e.apply(
        &Command::CutBatch {
            journal_seq: 0,
            journal_hash: [0; 32],
        },
        &mut out,
    )
    .unwrap();
    let Event::BatchCut { batch } = &out[0] else { panic!() };
    assert_eq!(
        batch.forced,
        vec![ForcedEntry {
            request_id: 0,
            account: "u0.near".into(),
            token: "usdc.near".into(),
            amount: 10_000_000
        }]
    );
}

#[test]
fn halted_market_and_spec_validation() {
    let mut e = engine_with(1, 100 * LOT, 10_000_000, 0, 0);
    ok(&mut e, Command::SetHalted { market: M, halted: true });
    assert_eq!(run(&mut e, order(A, Side::Buy, 1, 1, Tif::Gtc)).0, Err(Reject::MarketHalted));
    let mut bad = spec(5, 1);
    bad.id = 1;
    bad.symbol = "X-Y".into();
    assert_eq!(
        run(&mut e, Command::AddMarket(bad)).0,
        Err(Reject::InvalidSpec),
        "maker fee above taker fee"
    );
    assert_eq!(run(&mut e, Command::AddMarket(spec(0, 0))).0, Err(Reject::MarketExists));
}

#[test]
fn realistic_24_decimal_market_specs_validate() {
    // 1 lot = 0.01 NEAR (1e22 yocto), up to 1e9 lots (10M NEAR) per order, prices up to 1e9 ticks.
    let mut e = engine_with(0, 0, 0, 0, 0);
    let mut s = spec(2, 5);
    s.id = 1;
    s.symbol = "BIG-USDC".into();
    s.max_qty = 1_000_000_000;
    s.max_price = 1_000_000_000;
    assert_eq!(run(&mut e, Command::AddMarket(s.clone())).0, Ok(Ack::Ok));
    // And an order at the extremes doesn't overflow.
    let big = engine_with(1, u128::MAX / 4, u128::MAX / 4, 2, 5);
    let mut b = big;
    let mut s2 = s;
    s2.id = 1;
    ok(&mut b, Command::AddMarket(s2));
    ok(
        &mut b,
        Command::Place(NewOrder {
            account: 1,
            market: 1,
            side: Side::Buy,
            price: 1_000_000_000,
            qty: 1_000_000_000,
            tif: Tif::Gtc,
            stp: Stp::CancelTaker,
            client_id: 0,
            ts_ns: 0,
        }),
    );
    ok(
        &mut b,
        Command::Place(NewOrder {
            account: 1,
            market: 1,
            side: Side::Sell,
            price: 1_000_000_000,
            qty: 1_000_000_000,
            tif: Tif::Gtc,
            stp: Stp::CancelMaker,
            client_id: 0,
            ts_ns: 0,
        }),
    );
}
