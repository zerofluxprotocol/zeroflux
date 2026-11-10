//! Perp property test. Random flows of perp and spot orders, mark random walks (including gaps
//! that bankrupt accounts), funding, deposits, withdrawals and batch cuts. Every batch is applied
//! to a simulated on-chain ledger the way the settlement contract would apply it.
//!
//! Checked: engine invariants after every command; the on-chain ledger never goes negative and
//! every sealed group nets to zero; after each cut the ledger equals the engine's view of every
//! account (collateral + last settled perp value); conservation of collateral; replay and
//! snapshot determinism.

use obkit_engine::testkit::*;
use obkit_engine::*;
use proptest::prelude::*;
use std::collections::BTreeMap;

const USD: u128 = 1_000_000;
const N: u32 = 5;

#[derive(Clone, Debug)]
enum Op {
    Perp { a: u32, buy: bool, off: i64, qty: u64, ioc: bool },
    Cancel { a: u32 },
    Mark { pct: i64 },
    Funding,
    Deposit { a: u32, usd: u128 },
    Withdraw { a: u32, usd: u128 },
    Cut,
    Spot { a: u32, buy: bool, qty: u64 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        10 => (1..=N, any::<bool>(), -300i64..300, 1u64..30_000, any::<bool>()).prop_map(|(a, buy, off, qty, ioc)| Op::Perp { a, buy, off, qty, ioc }),
        2 => (1..=N).prop_map(|a| Op::Cancel { a }),
        // Mostly small moves, sometimes 20-40% gaps.
        4 => prop_oneof![8 => -3i64..=3, 1 => -40i64..=40].prop_map(|pct| Op::Mark { pct }),
        1 => Just(Op::Funding),
        1 => (1..=N, 1u128..300).prop_map(|(a, usd)| Op::Deposit { a, usd }),
        1 => (1..=N, 1u128..300).prop_map(|(a, usd)| Op::Withdraw { a, usd }),
        2 => Just(Op::Cut),
        1 => (1..=N, any::<bool>(), 1u64..50).prop_map(|(a, buy, qty)| Op::Spot { a, buy, qty }),
    ]
}

struct Sim {
    e: Engine,
    cmds: Vec<Command>,
    nonce: u64,
    mark: u64,
    /// On-chain balances (usdc) by account name, as the contract would hold them.
    chain: BTreeMap<String, i128>,
    deposited: u128,
    withdrawn: u128,
    /// Events of the most recent command.
    last_events: Vec<Event>,
}

impl Sim {
    fn new() -> Self {
        let e = perp_engine(N as usize, 300 * USD, 40_000);
        let mut chain = BTreeMap::new();
        for a in &e.accounts {
            chain.insert(a.name.clone(), a.balance(USDC).total as i128);
        }
        let deposited = e.totals()[USDC as usize];
        Sim {
            e,
            cmds: vec![],
            nonce: 0,
            mark: 40_000,
            chain,
            deposited,
            withdrawn: 0,
            last_events: vec![],
        }
    }

    fn apply(&mut self, c: Command) -> Result<Ack, Reject> {
        let mut out = vec![];
        let r = self.e.apply(&c, &mut out);
        if let Err(m) = self.e.check_invariants() {
            panic!("invariant after {c:?}: {m}");
        }
        for ev in &out {
            match ev {
                Event::Deposited { asset, amount, .. } if *asset == USDC => self.deposited += amount,
                Event::WithdrawQueued { asset, amount, .. } if *asset == USDC => self.withdrawn += amount,
                Event::BatchCut { batch } => self.apply_batch(batch),
                _ => {}
            }
        }
        if let Command::Chain(ChainEvent::Deposit { account, amount, .. }) = &c {
            *self.chain.entry(account.clone()).or_insert(0) += *amount as i128;
        }
        self.cmds.push(c);
        self.last_events = out;
        r
    }

    fn apply_batch(&mut self, b: &Batch) {
        for d in &b.entries {
            if d.token != "usdc.near" {
                continue;
            }
            let v = self.chain.entry(d.account.clone()).or_insert(0);
            *v += d.delta;
            assert!(*v >= 0, "on-chain balance of {} negative ({}) in batch {}", d.account, v, b.id);
        }
        for w in &b.withdrawals {
            if w.token == "usdc.near" {
                let v = self.chain.get_mut(&w.account).unwrap();
                *v -= w.amount as i128;
                assert!(*v >= 0, "withdrawal of {} would fail on-chain", w.account);
            }
        }
    }

    fn step(&mut self, o: &Op) {
        let c = match o {
            Op::Perp { a, buy, off, qty, ioc } => {
                let px = (self.mark as i64 + off * self.mark as i64 / 3_000).max(1) as u64;
                let side = if *buy { Side::Buy } else { Side::Sell };
                porder(*a, side, px, *qty, if *ioc { Tif::Ioc } else { Tif::Gtc })
            }
            Op::Cancel { a } => Command::CancelAll {
                account: *a,
                market: Some(P),
            },
            Op::Mark { pct } => {
                self.mark = (self.mark as i64 * (100 + pct) / 100).clamp(1_000, 1_000_000) as u64;
                Command::SetMark {
                    market: P,
                    mark: self.mark,
                    index: self.mark,
                    ts_ns: 0,
                }
            }
            Op::Funding => Command::FundingTick { market: P, ts_ns: 0 },
            Op::Deposit { a, usd } => Command::Chain(ChainEvent::Deposit {
                seq: self.e.chain_cursor + 1,
                account: format!("u{}.near", a - 1),
                token: "usdc.near".into(),
                amount: usd * USD,
            }),
            Op::Withdraw { a, usd } => {
                self.nonce += 1;
                Command::Withdraw {
                    account: *a,
                    asset: USDC,
                    amount: usd * USD,
                    nonce: self.nonce,
                    public_key: [(*a - 1) as u8; 32],
                    signature: vec![],
                }
            }
            Op::Cut => Command::CutBatch {
                journal_seq: 0,
                journal_hash: [0; 32],
            },
            Op::Spot { a, buy, qty } => order(*a, if *buy { Side::Buy } else { Side::Sell }, 42_500, *qty, Tif::Gtc),
        };
        let _ = self.apply(c);
    }

    fn check_settled(&self) {
        // With nothing pending, the chain holds exactly collateral + last settled perp value.
        for (i, a) in self.e.accounts.iter().enumerate() {
            let want = a.balance(USDC).total as i128 + a.perp.last_view;
            assert_eq!(
                self.chain.get(&a.name).copied().unwrap_or(0),
                want,
                "account {i} ({}) chain vs engine",
                a.name
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn perps_settle_safely(ops in prop::collection::vec(op(), 1..300), snap_at in 0usize..300) {
        let mut s = Sim::new();
        let mut snapshot = None;
        for (i, o) in ops.iter().enumerate() {
            if i == snap_at.min(ops.len() - 1) {
                snapshot = Some((s.cmds.len(), s.e.to_snapshot()));
            }
            s.step(o);
            if matches!(o, Op::Cut) && s.e.settle.withdrawals.is_empty() {
                s.check_settled();
            }
        }
        // Final cut: everything settles, ledger equals engine.
        let _ = s.apply(Command::CutBatch { journal_seq: 0, journal_hash: [0; 32] });
        s.check_settled();
        // Conservation of collateral: Σ (collateral + perp cash) = deposits − withdrawals.
        let total: i128 = s.e.accounts.iter().map(|a| a.balance(USDC).total as i128 + a.perp.cash).sum();
        prop_assert_eq!(total, s.deposited as i128 - s.withdrawn as i128);
        let chain_total: i128 = s.chain.values().sum();
        prop_assert_eq!(chain_total, s.deposited as i128 - s.withdrawn as i128);
        // Replay and snapshot determinism.
        let mut r = perp_engine(N as usize, 300 * USD, 40_000);
        for c in &s.cmds {
            let _ = r.apply(c, &mut vec![]);
        }
        prop_assert_eq!(r.state_hash(), s.e.state_hash());
        let (at, bytes) = snapshot.unwrap();
        let mut sn = Engine::from_snapshot(&bytes).unwrap();
        for c in &s.cmds[at..] {
            let _ = sn.apply(c, &mut vec![]);
        }
        prop_assert_eq!(sn.state_hash(), s.e.state_hash());
    }
}

/// The generator must reach liquidations, insurance takeovers, bankruptcies and funding.
#[test]
fn generator_reaches_the_risk_paths() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
    let mut runner = TestRunner::new_with_rng(Config::default(), TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for _ in 0..40 {
        let mut s = Sim::new();
        for _ in 0..300 {
            let o = op().new_tree(&mut runner).unwrap().current();
            s.step(&o);
            for ev in &s.last_events {
                let k = match ev {
                    Event::Liquidated { .. } => "liquidated",
                    Event::InsuranceTakeover { .. } => "takeover",
                    Event::Bankruptcy { .. } => "bankruptcy",
                    Event::Socialized { .. } => "socialized",
                    Event::Funding { payment_per_lot, .. } if *payment_per_lot != 0 => "funding",
                    Event::Fill { .. } => "fill",
                    _ => continue,
                };
                *counts.entry(k).or_insert(0) += 1;
            }
        }
    }
    println!("{counts:?}");
    for k in ["liquidated", "takeover", "bankruptcy", "socialized", "funding", "fill"] {
        assert!(counts.get(k).copied().unwrap_or(0) > 0, "never reached {k}: {counts:?}");
    }
}
