//! Clearing mode: the engine against a simulated clearing contract running the same core.
//!
//! After every settled batch the engine's `confirmed` ledger must equal the simulated chain's
//! state exactly, both ledgers must conserve collateral, and replaying the command log (from
//! scratch or from a snapshot) must reproduce the same state hash.

use proptest::prelude::*;
use zfx_perps_clearing::batch::{Op, apply_batch};
use zfx_perps_clearing::ledger;
use zfx_perps_clearing::store::{MemStore, NativeCrypto, Store};
use zfx_perps_clearing::testkit::{self, CHAIN, CONTRACT, INSURANCE, MKT, T0, Trader, USDC};
use zfx_perps_clearing::wire::{self, OrderType, Sig64, SignedOrder, TimeInForce};
use zfx_perps_clearing::{MarketStatus, PendingDeposit};
use zfx_perps_engine::clearing::{ClearingConfig, ClearingEvent};
use zfx_perps_engine::*;

const SEC: u64 = 1_000_000_000;

struct Sim {
    e: Engine,
    chain: MemStore,
    log: Vec<Command>,
    seq: u64,
    now: u64,
    nonce: u64,
    reporter: Trader,
    next_deposit: u64,
    fills: u32,
    reverted: u32,
    liquidations: u32,
}

impl Sim {
    fn new() -> Self {
        let reporter = Trader::new("reporter", 201);
        let mut g = testkit::globals();
        g.reporters = vec![reporter.key.public()];
        let mut chain = MemStore::new(g);
        let mut m = ledger::new_market(&NativeCrypto, testkit::model_params(), T0).unwrap();
        m.status = MarketStatus::Active;
        let (params, policy) = (m.params.clone(), m.policy_hash);
        chain.put_market(MKT, m);
        let mut s = Sim {
            e: Engine::new(EngineConfig::default()),
            chain,
            log: vec![],
            seq: 0,
            now: T0,
            nonce: 1,
            reporter,
            next_deposit: 1,
            fills: 0,
            reverted: 0,
            liquidations: 0,
        };
        s.cmd(Command::ClearingInit(ClearingConfig {
            chain_id: CHAIN.into(),
            contract: CONTRACT.into(),
            collateral: USDC.into(),
            collateral_decimals: 6,
            insurance_account: INSURANCE.into(),
            settle_grace_ns: zfx_perps_clearing::batch::DEFAULT_SETTLE_GRACE_NS,
            max_order_ttl_ns: 30 * 86_400 * SEC,
        }))
        .unwrap();
        s.chain_event(ClearingEvent::MarketUpdated { params, policy_hash: policy, status: MarketStatus::Active, created_ns: T0 });
        let r = s.reporter.key.public();
        s.chain_event(ClearingEvent::ReportersUpdated { reporters: vec![r], quorum: 1 });
        s.deposit(INSURANCE, 500_000_000);
        s.print(1_000);
        s.settle_all();
        s
    }

    fn cmd(&mut self, c: Command) -> Result<Ack, Reject> {
        let mut out = vec![];
        let r = self.e.apply(&c, &mut out);
        for ev in &out {
            match ev {
                Event::Fill { .. } => self.fills += 1,
                Event::FillReverted { .. } => self.reverted += 1,
                Event::Clearing { event: zfx_perps_clearing::CEvent::Liquidated { .. } } => self.liquidations += 1,
                _ => {}
            }
        }
        self.log.push(c);
        r
    }

    fn chain_event(&mut self, event: ClearingEvent) {
        self.seq += 1;
        let r = self.cmd(Command::ClearingChain { seq: self.seq, event, ts_ns: self.now });
        assert!(r.is_ok(), "{r:?}");
    }

    fn deposit(&mut self, account: &str, amount: u128) {
        let id = self.next_deposit;
        self.next_deposit += 1;
        self.chain.deposits.insert(id, PendingDeposit { account: account.into(), amount, created_ns: self.now });
        self.chain_event(ClearingEvent::Deposit { deposit_id: id, account: account.into(), amount });
    }

    fn user(&mut self, name: &str, amount: u128) -> Trader {
        let t = Trader::new(name, 1);
        let k = zfx_perps_clearing::KeyInfo { account: name.into(), expires_ns: 0, can_withdraw: true };
        self.chain.add_key(t.key.public(), k);
        self.chain_event(ClearingEvent::KeyAdded { account: name.into(), public_key: t.key.public(), expires_ns: 0, can_withdraw: true });
        self.deposit(name, amount);
        t
    }

    fn print(&mut self, ticks: u64) {
        let mut w = testkit::World::new();
        w.now = self.now;
        w.s.markets.get_mut(MKT).unwrap().print = self.e.clearing().unwrap().spec.markets[MKT].print.clone();
        let p = w.print(ticks, 1);
        let sig = Sig64(self.reporter.key.sign(&p));
        let _ = self.cmd(Command::ClearingAction {
            op: Box::new(Op::Oracle { market: MKT.into(), print: p, sigs: vec![(self.reporter.key.public(), sig)] }),
            ts_ns: self.now,
        });
    }

    fn order(&mut self, t: &Trader, side: wire::Side, price: u64, qty: u64, f: impl FnOnce(&mut SignedOrder)) -> SignedOrder {
        self.nonce += 1;
        let policy = self.e.clearing().unwrap().spec.markets[MKT].policy_hash;
        let mut o = SignedOrder {
            chain_id: CHAIN.into(),
            clearing_contract: CONTRACT.into(),
            account: t.name.clone(),
            market: MKT.into(),
            side,
            order_type: OrderType::Limit,
            price,
            qty,
            reduce_only: false,
            post_only: false,
            tif: TimeInForce::Gtc,
            nonce: self.nonce,
            expiry_ns: self.now + 3_600 * SEC,
            client_order_id: self.nonce,
            max_fee_bps: 10,
            policy_hash: policy,
            public_key: t.key.public(),
            signature: Sig64([0; 64]),
        };
        f(&mut o);
        o.signature = Sig64(t.key.sign(&o.message()));
        o
    }

    fn place(&mut self, o: SignedOrder) -> Result<Ack, Reject> {
        self.cmd(Command::PlaceSigned { order: Box::new(o), stp: Stp::CancelTaker, ts_ns: self.now })
    }

    /// Cut, then let the simulated contract apply every unconfirmed batch `delay` after its ops.
    fn settle_with(&mut self, delay: u64) {
        self.cmd(Command::CutBatch { journal_seq: self.log.len() as u64, journal_hash: [0; 32] }).unwrap();
        let batches: Vec<_> = self.e.clearing().unwrap().unconfirmed.iter().map(|b| b.batch.clone()).collect();
        for b in batches {
            let block = b.ops.last().map_or(self.now, |o| o.ts_ns) + delay;
            let r = apply_batch(&mut self.chain, &NativeCrypto, &b, Some(block), false);
            let rejected: Vec<(u32, zfx_perps_clearing::Reject)> = r.iter().filter_map(|x| x.result.as_ref().err().map(|f| (x.index, f.reason))).collect();
            self.chain_event(ClearingEvent::BatchSettled { batch_id: b.id, block_ns: block, rejected });
            let c = self.e.clearing().unwrap();
            assert_eq!(c.diverged, None);
            // The engine drops records of expired orders (they can never fill again); compare the rest.
            let horizon = c.spec.globals.last_op_ns.saturating_sub(c.spec.globals.settle_grace_ns);
            let mut chain = self.chain.clone();
            chain.orders.retain(|_, o| o.expiry_ns > horizon);
            let mut conf = c.confirmed.clone();
            conf.orders.retain(|_, o| o.expiry_ns > horizon);
            if conf != chain {
                let (x, y) = (&conf, &chain);
                let d = [
                    ("globals", format!("{:?}", x.globals) != format!("{:?}", y.globals)),
                    ("markets", x.markets != y.markets),
                    ("accounts", x.accounts != y.accounts),
                    ("positions", x.positions != y.positions),
                    ("keys", x.keys != y.keys),
                    ("orders", x.orders != y.orders),
                    ("deposits", x.deposits != y.deposits),
                    ("forced", x.forced != y.forced),
                ];
                let bad: Vec<_> = d.iter().filter(|(_, v)| *v).map(|(k, _)| *k).collect();
                panic!("confirmed != chain after batch {}: {bad:?}\nengine accounts {:?}\nchain accounts {:?}\nengine pos {:?}\nchain pos {:?}\nrejected {:?}", b.id, x.accounts, y.accounts, x.positions, y.positions, r.iter().filter(|z| z.result.is_err()).map(|z| (z.index, z.result.clone().err())).collect::<Vec<_>>());
            }
        }
    }

    fn settle_all(&mut self) {
        self.settle_with(SEC)
    }

    fn check(&self) {
        self.e.check_invariants().unwrap();
    }

    fn replay_matches(&self) {
        let mut e = Engine::new(EngineConfig::default());
        let mut out = vec![];
        for (i, c) in self.log.iter().enumerate() {
            let _ = e.apply(c, &mut out);
            if i == self.log.len() / 2 {
                e = Engine::from_snapshot(&e.to_snapshot()).unwrap();
            }
        }
        assert_eq!(e.state_hash(), self.e.state_hash(), "replay diverged");
    }
}

#[test]
fn signed_orders_match_and_settle_identically_on_chain() {
    let mut s = Sim::new();
    let a = s.user("alice.near", 1_000_000_000);
    let b = s.user("bob.near", 1_000_000_000);
    let ask = s.order(&b, wire::Side::Sell, 1_010, 100, |o| o.post_only = true);
    assert!(matches!(s.place(ask).unwrap(), Ack::Placed { status: OrderStatus::Resting, .. }));
    let bid = s.order(&a, wire::Side::Buy, 1_020, 60, |o| o.tif = TimeInForce::Ioc);
    assert!(matches!(s.place(bid).unwrap(), Ack::Placed { status: OrderStatus::Filled, filled: 60, .. }));
    s.check();
    s.settle_all();
    assert_eq!(s.chain.position("alice.near", MKT).size, 60);
    assert_eq!(s.chain.position("bob.near", MKT).size, -60);
    // Close: alice sells into a bob bid (below bob's own resting ask, so STP doesn't fire).
    let bid = s.order(&b, wire::Side::Buy, 1_005, 60, |_| {});
    s.place(bid).unwrap();
    let ask = s.order(&a, wire::Side::Sell, 1_000, 60, |o| o.reduce_only = true);
    let r = s.place(ask);
    assert!(matches!(r, Ok(Ack::Placed { filled: 60, .. })), "{r:?}");
    s.settle_all();
    assert_eq!(s.chain.position("alice.near", MKT).size, 0);
    // Independent reference math: bought 60 at 1010, sold 60 at 1005, taker both times (5 bps).
    let pnl: i128 = 60 * (1_005 - 1_010) * 1_000;
    let alice = s.chain.account("alice.near").free as i128;
    let expect = 1_000_000_000i128 + pnl - ledger::fee(60 * 1_010 * 1_000, 5) as i128 - ledger::fee(60 * 1_005 * 1_000, 5) as i128;
    assert_eq!(alice, expect);
    s.check();
    s.replay_matches();
}

#[test]
fn admission_rejects_what_the_contract_would() {
    let mut s = Sim::new();
    let a = s.user("alice.near", 10_000_000);
    let wrong = s.order(&a, wire::Side::Buy, 1_000, 1, |o| o.chain_id = "near:mainnet".into());
    assert_eq!(s.place(wrong), Err(Reject::Clearing(zfx_perps_clearing::Reject::WrongChain)));
    let stale = s.order(&a, wire::Side::Buy, 1_000, 1, |o| o.policy_hash = [1; 32]);
    assert_eq!(s.place(stale), Err(Reject::Clearing(zfx_perps_clearing::Reject::PolicyMismatch)));
    let big = s.order(&a, wire::Side::Buy, 1_000, 100, |_| {});
    assert_eq!(s.place(big), Err(Reject::InsufficientMargin));
    let far = s.order(&a, wire::Side::Buy, 1_400, 1, |_| {});
    assert_eq!(s.place(far), Err(Reject::PriceOutOfBand));
    let ro = s.order(&a, wire::Side::Sell, 1_000, 1, |o| o.reduce_only = true);
    assert_eq!(s.place(ro), Err(Reject::Clearing(zfx_perps_clearing::Reject::ReduceOnlyViolated)));
    let ok = s.order(&a, wire::Side::Buy, 990, 5, |_| {});
    s.place(ok.clone()).unwrap();
    assert_eq!(s.place(ok), Err(Reject::DuplicateClientId));
}

#[test]
fn onchain_key_revocation_reverts_a_pending_fill() {
    let mut s = Sim::new();
    let a = s.user("alice.near", 1_000_000_000);
    let b = s.user("bob.near", 1_000_000_000);
    let ask = s.order(&b, wire::Side::Sell, 1_000, 10, |_| {});
    s.place(ask).unwrap();
    let bid = s.order(&a, wire::Side::Buy, 1_000, 10, |_| {});
    s.place(bid).unwrap();
    assert_eq!(s.fills, 1);
    // Bob revokes his key from his wallet before the batch lands (the chain sees it first).
    s.chain.keys.remove(&b.key.public());
    s.chain_event(ClearingEvent::KeyRemoved { account: "bob.near".into(), public_key: b.key.public() });
    assert_eq!(s.reverted, 1, "the engine predicts the contract will reject the fill");
    s.settle_all();
    assert_eq!(s.chain.position("alice.near", MKT).size, 0);
    s.check();
    s.replay_matches();
}

#[test]
fn delayed_settlement_past_grace_is_rejected_and_reconciled() {
    let mut s = Sim::new();
    let a = s.user("alice.near", 1_000_000_000);
    let b = s.user("bob.near", 1_000_000_000);
    let ask = s.order(&b, wire::Side::Sell, 1_000, 10, |o| o.expiry_ns = s_now() + 20 * SEC);
    fn s_now() -> u64 {
        T0
    }
    s.place(ask).unwrap();
    let bid = s.order(&a, wire::Side::Buy, 1_000, 10, |_| {});
    s.place(bid).unwrap();
    // The chain is congested: the batch lands 10 minutes later, past expiry + 300 s grace.
    s.settle_with(600 * SEC);
    assert_eq!(s.chain.position("alice.near", MKT).size, 0);
    let spec = &s.e.clearing().unwrap().spec;
    assert_eq!(spec.positions.len(), s.chain.positions.len());
    s.check();
}

#[test]
fn print_drop_liquidates_and_chain_agrees() {
    let mut s = Sim::new();
    let a = s.user("alice.near", 50_100_000);
    let b = s.user("bob.near", 1_000_000_000);
    let c = s.user("carol.near", 1_000_000_000);
    let ask = s.order(&b, wire::Side::Sell, 1_000, 100, |_| {});
    s.place(ask).unwrap();
    let bid = s.order(&a, wire::Side::Buy, 1_000, 100, |_| {});
    s.place(bid).unwrap();
    s.settle_all();
    // Carol bids under the next mark.
    s.now += 86_400 * SEC;
    s.print(600);
    let resting = s.order(&c, wire::Side::Buy, 590, 30, |_| {});
    s.place(resting).unwrap();
    s.now += 86_400 * SEC;
    s.print(540);
    assert!(s.liquidations >= 1);
    s.settle_all();
    assert_eq!(s.chain.position("alice.near", MKT).size, 0);
    s.check();
    s.replay_matches();
}

#[derive(Debug, Clone)]
enum Act {
    Place { who: usize, buy: bool, off: i8, qty: u16, ioc: bool, reduce: bool },
    Print { pct: i8 },
    Funding,
    Cut { delay_s: u16 },
    Withdraw { who: usize, amount: u32 },
    ChainCancelAll { who: usize },
    Revoke { who: usize },
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        12 => (0..5usize, any::<bool>(), -6i8..6, 1u16..60, any::<bool>(), 0u8..8)
            .prop_map(|(who, buy, off, qty, ioc, r)| Act::Place { who, buy, off, qty, ioc, reduce: r == 0 }),
        1 => (-60i8..60).prop_map(|pct| Act::Print { pct }),
        1 => Just(Act::Funding),
        2 => (0u16..400).prop_map(|delay_s| Act::Cut { delay_s }),
        1 => (0..5usize, any::<u32>()).prop_map(|(who, amount)| Act::Withdraw { who, amount }),
        1 => (0..5usize).prop_map(|who| Act::ChainCancelAll { who }),
        1 => (0..5usize).prop_map(|who| Act::Revoke { who }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 120, ..ProptestConfig::default() })]
    /// Random trading against a simulated contract with on-chain races (wallet cancels, key
    /// revocations, late batches). Invariants after every action; confirmed == chain after every
    /// batch; replay and snapshot determinism at the end.
    #[test]
    fn engine_tracks_chain_exactly(acts in proptest::collection::vec(act(), 1..80)) {
        run(acts);
    }
}

fn run(acts: Vec<Act>) -> (u32, u32, u32) {
    let mut s = Sim::new();
    let mut users: Vec<Trader> = (0..5).map(|i| s.user(&format!("u{i}.near"), 200_000_000 + i as u128 * 100_000_000)).collect();
    for a in acts {
        s.now += SEC;
        match a {
            Act::Place { who, buy, off, qty, ioc, reduce } => {
                let mark = s.e.clearing().unwrap().spec.markets[MKT].mark as i64;
                let px = (mark + mark * off as i64 / 100).max(1) as u64;
                let side = if buy { wire::Side::Buy } else { wire::Side::Sell };
                let t = users[who].clone();
                let o = s.order(&t, side, px, qty as u64, |o| {
                    if ioc { o.tif = TimeInForce::Ioc }
                    o.reduce_only = reduce;
                });
                let r = s.place(o);
                if std::env::var("ZFX_DEBUG").is_ok() { eprintln!("PLACE {:?}", r); }
            }
            Act::Print { pct } => {
                s.now += 86_400 * SEC - s.now % (86_400 * SEC) + 8 * 3_600 * SEC;
                let mark = s.e.clearing().unwrap().spec.markets[MKT].mark as i64;
                s.print((mark + mark * pct as i64 / 100).max(1) as u64);
                if let Some(m) = s.chain.markets.get_mut(MKT) && m.status == MarketStatus::Halted {
                    // Keep exercising: the test reopens halted markets on both sides.
                    m.status = MarketStatus::Active;
                    let (p, h) = (m.params.clone(), m.policy_hash);
                    s.chain_event(ClearingEvent::MarketUpdated { params: p, policy_hash: h, status: MarketStatus::Active, created_ns: T0 });
                }
            }
            Act::Funding => {
                s.now += 8 * 3_600 * SEC;
                // Stay inside the daily trading window (08:00 to 22:00 UTC): roll to the next day's print.
                if s.now % (86_400 * SEC) >= 21 * 3_600 * SEC {
                    s.now += 86_400 * SEC - s.now % (86_400 * SEC) + 8 * 3_600 * SEC;
                    let mark = s.e.clearing().unwrap().spec.markets[MKT].mark;
                    s.print(mark);
                }
                let _ = s.cmd(Command::ClearingAction { op: Box::new(Op::Funding { market: MKT.into() }), ts_ns: s.now });
            }
            Act::Cut { delay_s } => s.settle_with(delay_s as u64 * SEC),
            Act::Withdraw { who, amount } => {
                let t = users[who].clone();
                let n = s.e.clearing().unwrap().spec.accounts.get(&t.name).map_or(0, |x| x.action_nonce) + 1;
                let amt = amount as u128 + 1;
                let msg = wire::withdraw_message(CHAIN, CONTRACT, &t.name, USDC, amt, n);
                let op = Op::Withdraw { account: t.name.clone(), amount: amt, nonce: n, public_key: t.key.public(), signature: Sig64(t.key.sign(&msg)) };
                let _ = s.cmd(Command::ClearingAction { op: Box::new(op), ts_ns: s.now });
            }
            Act::ChainCancelAll { who } => {
                let name = users[who].name.clone();
                let n = s.chain.account(&name).min_nonce.max(s.nonce) + 1;
                let _ = ledger::set_min_nonce(&mut s.chain, &NativeCrypto, &name, n, None);
                s.chain_event(ClearingEvent::MinNonce { account: name, min_nonce: n });
            }
            Act::Revoke { who } => {
                let t = users[who].clone();
                if s.chain.keys.remove(&t.key.public()).is_some() {
                    s.chain_event(ClearingEvent::KeyRemoved { account: t.name.clone(), public_key: t.key.public() });
                    // Register a fresh key so the account keeps trading.
                    let nt = Trader::new(&t.name, (s.nonce % 200) as u8 + 2);
                    let k = zfx_perps_clearing::KeyInfo { account: t.name.clone(), expires_ns: 0, can_withdraw: true };
                    s.chain.add_key(nt.key.public(), k);
                    s.chain_event(ClearingEvent::KeyAdded { account: t.name.clone(), public_key: nt.key.public(), expires_ns: 0, can_withdraw: true });
                    users[who] = nt;
                }
            }
        }
        s.check();
    }
    s.settle_all();
    s.check();
    s.replay_matches();
    (s.fills, s.reverted, s.liquidations)
}

/// The random actions reach fills, on-chain races that revert fills, and liquidations.
#[test]
fn generator_coverage() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let (mut f, mut r, mut l) = (0, 0, 0);
    for _ in 0..60 {
        let acts = proptest::collection::vec(act(), 80).new_tree(&mut runner).unwrap().current();
        let (a, b, c) = run(acts);
        f += a;
        r += b;
        l += c;
    }
    eprintln!("fills {f} reverted {r} liquidations {l}");
    assert!(f > 300 && r > 0 && l > 0, "fills {f} reverted {r} liquidations {l}");
}
