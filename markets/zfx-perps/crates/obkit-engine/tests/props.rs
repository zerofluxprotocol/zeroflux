//! Property tests over random command streams: invariants after every command, conservation,
//! determinism of replay, and snapshot/restore equivalence.

use obkit_engine::testkit::*;
use obkit_engine::*;
use proptest::prelude::*;

const LOT: u128 = 10_000_000_000_000_000_000_000;
const ACCTS: u32 = 5;

#[derive(Clone, Debug)]
enum Op {
    Place {
        a: u32,
        buy: bool,
        px: u64,
        qty: u64,
        tif: u8,
        stp: u8,
        cid: u64,
    },
    Cancel {
        a: u32,
        pick: usize,
    },
    CancelAll {
        a: u32,
    },
    Withdraw {
        a: u32,
        amount: u128,
    },
    Deposit {
        a: u32,
        usdc: bool,
        amount: u128,
    },
    Cut,
    Settle,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => (1..=ACCTS, any::<bool>(), 42_490u64..42_510, 1u64..40, 0u8..4, 0u8..3, 0u64..6)
            .prop_map(|(a, buy, px, qty, tif, stp, cid)| Op::Place { a, buy, px, qty, tif, stp, cid }),
        3 => (1..=ACCTS, any::<usize>()).prop_map(|(a, pick)| Op::Cancel { a, pick }),
        1 => (1..=ACCTS).prop_map(|a| Op::CancelAll { a }),
        1 => (1..=ACCTS, 1u128..2_000_000).prop_map(|(a, amount)| Op::Withdraw { a, amount }),
        1 => (1..=ACCTS, any::<bool>(), 1u128..5_000_000).prop_map(|(a, usdc, amount)| Op::Deposit { a, usdc, amount }),
        1 => Just(Op::Cut),
        1 => Just(Op::Settle),
    ]
}

fn to_cmd(e: &Engine, o: &Op, nonce: &mut u64) -> Command {
    match o {
        Op::Place {
            a,
            buy,
            px,
            qty,
            tif,
            stp,
            cid,
        } => Command::Place(NewOrder {
            account: *a,
            market: M,
            side: if *buy { Side::Buy } else { Side::Sell },
            price: *px,
            qty: *qty,
            tif: [Tif::Gtc, Tif::Ioc, Tif::Fok, Tif::PostOnly][*tif as usize],
            stp: [Stp::CancelTaker, Stp::CancelMaker, Stp::CancelBoth][*stp as usize],
            client_id: *cid,
            ts_ns: 0,
        }),
        Op::Cancel { a, pick } => {
            let open = e.open_orders(*a);
            let id = if open.is_empty() { 999_999 } else { open[pick % open.len()].0 };
            Command::Cancel {
                account: *a,
                market: M,
                order_id: id,
            }
        }
        Op::CancelAll { a } => Command::CancelAll { account: *a, market: None },
        Op::Withdraw { a, amount } => {
            *nonce += 1;
            Command::Withdraw {
                account: *a,
                asset: USDC,
                amount: *amount,
                nonce: *nonce,
                public_key: [(*a - 1) as u8; 32],
                signature: vec![],
            }
        }
        Op::Deposit { a, usdc, amount } => Command::Chain(ChainEvent::Deposit {
            seq: e.chain_cursor + 1,
            account: format!("u{}.near", a - 1),
            token: if *usdc { "usdc.near" } else { "wrap.near" }.into(),
            amount: if *usdc { *amount } else { *amount * LOT / 1_000_000 },
        }),
        Op::Cut => Command::CutBatch {
            journal_seq: 0,
            journal_hash: [0; 32],
        },
        Op::Settle => Command::Chain(ChainEvent::BatchSettled {
            seq: e.chain_cursor + 1,
            batch_id: e.settle.next_batch_id - 1,
        }),
    }
}

fn fresh() -> Engine {
    engine_with(ACCTS as usize, 2_000 * LOT, 100_000_000, 2, 7)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn invariants_conservation_replay_snapshot(ops in prop::collection::vec(op(), 1..250), snap_at in 0usize..250) {
        let mut e = fresh();
        let mut cmds = vec![];
        let mut nonce = 0u64;
        let mut deposited = e.totals();
        let mut withdrawn = [0u128; 2];
        let mut snapshot: Option<(usize, Vec<u8>)> = None;
        for (i, o) in ops.iter().enumerate() {
            if i == snap_at.min(ops.len() - 1) {
                snapshot = Some((i, e.to_snapshot()));
            }
            let c = to_cmd(&e, o, &mut nonce);
            let mut out = vec![];
            let r = e.apply(&c, &mut out);
            if let Err(msg) = e.check_invariants() {
                prop_assert!(false, "invariant broken after {c:?}: {msg}");
            }
            if r.is_ok() {
                for ev in &out {
                    match ev {
                        Event::Deposited { asset, amount, .. } => deposited[*asset as usize] += amount,
                        Event::WithdrawQueued { asset, amount, .. } => withdrawn[*asset as usize] += amount,
                        _ => {}
                    }
                }
            } else {
                prop_assert!(!out.iter().any(|x| matches!(x, Event::Fill { .. })), "rejected command produced fills");
            }
            cmds.push(c);
        }
        // Conservation: every asset's total equals deposits minus withdrawals.
        let t = e.totals();
        for s in 0..2 {
            prop_assert_eq!(t[s], deposited[s] - withdrawn[s]);
        }
        // Replay from scratch reproduces the exact state.
        let mut r = fresh();
        for c in &cmds {
            r.apply(c, &mut vec![]).ok();
        }
        prop_assert_eq!(r.state_hash(), e.state_hash());
        // Restore from a mid-stream snapshot and replay the tail: same state.
        let (at, bytes) = snapshot.unwrap();
        let mut s = Engine::from_snapshot(&bytes).unwrap();
        for c in &cmds[at..] {
            s.apply(c, &mut vec![]).ok();
        }
        prop_assert_eq!(s.state_hash(), e.state_hash());
        // Cutting everything leaves batches whose sealed groups net to zero per token.
        let mut out = vec![];
        e.apply(&Command::CutBatch { journal_seq: 0, journal_hash: [0; 32] }, &mut out).unwrap();
        let mut net: std::collections::BTreeMap<String, i128> = Default::default();
        for b in &e.settle.unconfirmed {
            for d in &b.entries {
                *net.entry(d.token.clone()).or_default() += d.delta;
            }
            if b.seal {
                prop_assert!(net.values().all(|v| *v == 0), "sealed group not zero-sum: {net:?}");
            }
        }
    }
}

/// Guard against a vacuous property test: the generator must actually produce fills, every kind
/// of rejection and cancel path, and multi-command batches.
#[test]
fn generator_exercises_the_interesting_paths() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, TestRng, TestRunner};
    let mut runner = TestRunner::new_with_rng(
        Config::default(),
        TestRng::deterministic_rng(proptest::test_runner::RngAlgorithm::ChaCha),
    );
    let mut e = fresh();
    let mut nonce = 0;
    let (mut fills, mut stp, mut fok, mut post, mut batches) = (0, 0, 0, 0, 0);
    let mut rej = std::collections::BTreeMap::<String, usize>::new();
    for _ in 0..5_000 {
        let o = op().new_tree(&mut runner).unwrap().current();
        let c = to_cmd(&e, &o, &mut nonce);
        let mut out = vec![];
        let r = e.apply(&c, &mut out);
        fills += out.iter().filter(|x| matches!(x, Event::Fill { .. })).count();
        stp += out
            .iter()
            .filter(|x| {
                matches!(
                    x,
                    Event::Canceled {
                        reason: CancelReason::SelfTrade,
                        ..
                    }
                )
            })
            .count();
        fok += matches!(r, Err(Reject::FokUnfillable)) as usize;
        post += matches!(r, Err(Reject::PostOnlyWouldCross)) as usize;
        batches += out.iter().filter(|x| matches!(x, Event::BatchCut { .. })).count();
        if let Err(r) = r {
            *rej.entry(format!("{r:?}")).or_insert(0) += 1;
        }
    }
    println!("rejects {rej:?} resting={}", e.markets[0].book.len());
    e.check_invariants().unwrap();
    println!("fills={fills} stp={stp} fok_rejects={fok} post_rejects={post} batches={batches}");
    assert!(fills > 300 && stp > 20 && fok > 20 && post > 20 && batches > 50);
}
