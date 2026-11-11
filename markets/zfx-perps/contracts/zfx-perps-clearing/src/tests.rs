//! Contract unit tests (near-sdk mocked runtime). Custody attacks from spec 5.5: wrong token,
//! replayed and forged fills, wrong-chain / wrong-contract signatures, double settlement, fee
//! accounting, key revocation, forced withdrawal, freeze and escape.

use super::*;
use near_sdk::test_utils::{VMContextBuilder, get_logs};
use near_sdk::testing_env;
use zfx_perps_clearing::batch::{Op, OpAt};
use zfx_perps_clearing::testkit::{self, CHAIN, CONTRACT, INSURANCE, MKT, T0, Trader, USDC};
use zfx_perps_clearing::wire::{OrderType, Side, SignedOrder, TimeInForce};

fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn ctx(pred: &str, deposit: NearToken, ts: u64) {
    testing_env!(
        VMContextBuilder::new()
            .current_account_id(acc(CONTRACT))
            .predecessor_account_id(acc(pred))
            .attached_deposit(deposit)
            .block_timestamp(ts)
            .build()
    );
}

fn npk(k: [u8; 32]) -> PublicKey {
    let mut b = vec![0u8];
    b.extend_from_slice(&k);
    PublicKey::try_from(b).unwrap()
}

struct H {
    c: Contract,
    reporter: Trader,
    now: u64,
    nonce: u64,
    batch: u64,
}

impl H {
    fn new() -> Self {
        let reporter = Trader::new("reporter", 201);
        ctx("owner.near", NearToken::from_yoctonear(0), T0);
        let mut c = Contract::new(
            acc("owner.near"),
            acc("operator.near"),
            CHAIN.into(),
            acc(USDC),
            acc(INSURANCE),
            vec![pk_str(&reporter.key.public())],
            1,
            U64(3_600 * NS_PER_SEC),
            U64(0),
            None,
        );
        let mut p = testkit::model_params();
        p.mark_limit_bps = 600;
        c.create_market(near_sdk::serde_json::to_value(p).unwrap());
        c.set_market_status(MKT.into(), "ACTIVE".into());
        let _ = c.execute_proposal(U64(1));
        let mut h = H { c, reporter, now: T0, nonce: 1, batch: 0 };
        let p = h.print(1_000);
        let r = h.settle(vec![], vec![h.oracle_op(&p)]);
        assert!(r.is_empty(), "{r:?}");
        h
    }

    fn policy(&self) -> [u8; 32] {
        self.c.markets.get(MKT).unwrap().policy_hash
    }

    fn print(&self, ticks: u64) -> Vec<u8> {
        let mut w = testkit::World::new();
        w.now = self.now;
        w.s.markets.get_mut(MKT).unwrap().print = self.c.markets.get(MKT).unwrap().print.clone();
        w.print(ticks, 1)
    }

    fn oracle_op(&self, p: &[u8]) -> Op {
        Op::Oracle {
            market: MKT.into(),
            print: p.to_vec(),
            sigs: vec![(self.reporter.key.public(), Sig64(self.reporter.key.sign(p)))],
        }
    }

    fn trader(&mut self, name: &str, amount: u128) -> Trader {
        let t = Trader::new(name, 1);
        ctx(name, KEY_DEPOSIT, self.now);
        self.c.register_key(npk(t.key.public()), None, None);
        if amount > 0 {
            ctx(USDC, NearToken::from_yoctonear(0), self.now);
            assert!(matches!(self.c.ft_on_transfer(acc(name), U128(amount), String::new()), PromiseOrValue::Value(U128(0))));
            let id = self.c.next_deposit - 1;
            assert!(self.settle(vec![], vec![Op::Credit { deposit_id: id }]).is_empty());
        }
        t
    }

    fn order(&mut self, t: &Trader, side: Side, price: u64, qty: u64, f: impl FnOnce(&mut SignedOrder)) -> SignedOrder {
        self.nonce += 1;
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
            expiry_ns: self.now + 3_600 * NS_PER_SEC,
            client_order_id: self.nonce,
            max_fee_bps: 10,
            policy_hash: self.policy(),
            public_key: t.key.public(),
            signature: Sig64([0; 64]),
        };
        f(&mut o);
        o.signature = Sig64(t.key.sign(&o.message()));
        o
    }

    /// Submit a batch as the operator; returns the rejected (op index, reason) pairs.
    fn settle(&mut self, orders: Vec<SignedOrder>, ops: Vec<Op>) -> Vec<(u64, String)> {
        self.now += 1_000;
        self.batch += 1;
        ctx("operator.near", NearToken::from_yoctonear(0), self.now);
        let ts = self.now;
        self.c.settle_batch(Batch {
            id: self.batch,
            orders,
            ops: ops.into_iter().map(|op| OpAt { ts_ns: ts, op }).collect(),
            journal_seq: self.batch,
            journal_hash: [0; 32],
        });
        let ev = self.c.get_events(U64(self.c.next_event - 1), 1);
        ev[0].1["rejected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r[0].as_u64().unwrap(), r[1].as_str().unwrap().to_string()))
            .collect()
    }

    fn free(&self, a: &str) -> u128 {
        self.c.accounts.get(a).map_or(0, |x| x.free)
    }
    fn size(&self, a: &str) -> i64 {
        self.c.positions.get(&(MKT.to_string(), a.to_string())).map_or(0, |p| p.size)
    }
}

fn fill(m: u32, t: u32, qty: u64, price: u64) -> Op {
    Op::Fill { maker: m, taker: t, qty, price }
}

#[test]
fn wrong_token_and_bad_beneficiary_are_refunded() {
    let mut h = H::new();
    ctx("wrap.test.near", NearToken::from_yoctonear(0), h.now);
    assert!(matches!(h.c.ft_on_transfer(acc("a.near"), U128(5), String::new()), PromiseOrValue::Value(U128(5))));
    ctx(USDC, NearToken::from_yoctonear(0), h.now);
    assert!(matches!(h.c.ft_on_transfer(acc("a.near"), U128(5), "Not An Account".into()), PromiseOrValue::Value(U128(5))));
    assert!(matches!(h.c.ft_on_transfer(acc("a.near"), U128(0), String::new()), PromiseOrValue::Value(U128(0))));
    // A deposit is pending until credited, and only credited once.
    assert!(matches!(h.c.ft_on_transfer(acc("a.near"), U128(7), String::new()), PromiseOrValue::Value(U128(0))));
    let id = h.c.next_deposit - 1;
    assert_eq!(h.free("a.near"), 0);
    assert!(h.settle(vec![], vec![Op::Credit { deposit_id: id }]).is_empty());
    assert_eq!(h.free("a.near"), 7);
    assert_eq!(h.settle(vec![], vec![Op::Credit { deposit_id: id }]), vec![(0, "unknown_deposit".into())]);
    assert_eq!(h.c.g.total_collateral, 7);
}

#[test]
fn fills_settle_and_forgeries_are_skipped() {
    let mut h = H::new();
    let a = h.trader("alice.near", 1_000_000_000);
    let b = h.trader("bob.near", 1_000_000_000);
    let sell = h.order(&b, Side::Sell, 1_000, 100, |_| {});
    let buy = h.order(&a, Side::Buy, 1_000, 100, |_| {});
    let mut forged = buy.clone();
    forged.qty = 1_000;
    let other_chain = h.order(&a, Side::Buy, 1_000, 100, |o| o.chain_id = "near:mainnet".into());
    let other_contract = h.order(&a, Side::Buy, 1_000, 100, |o| o.clearing_contract = "evil.near".into());
    let orders = vec![sell, buy, forged, other_chain, other_contract];
    let r = h.settle(
        orders.clone(),
        vec![fill(0, 2, 100, 1_000), fill(0, 3, 10, 1_000), fill(0, 4, 10, 1_000), fill(0, 1, 60, 1_000), fill(0, 1, 40, 1_000), fill(0, 1, 1, 1_000)],
    );
    assert_eq!(
        r,
        vec![
            (0, "bad_signature".into()),
            (1, "wrong_chain".into()),
            (2, "wrong_contract".into()),
            (5, "overfill".into())
        ]
    );
    assert_eq!((h.size("alice.near"), h.size("bob.near")), (100, -100));
    // Fees: 2 + 5 bps of 100M notional, credited to the fee ledger exactly once.
    assert_eq!(h.c.g.fee_balance, 20_000 + 50_000);
    // Replaying the whole batch (as a new id) settles nothing more.
    let r = h.settle(orders, vec![fill(0, 1, 60, 1_000)]);
    assert_eq!(r, vec![(0, "overfill".into())]);
    // Resubmitting an old batch id fails outright.
    h.batch -= 1;
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.settle(vec![], vec![])));
    assert!(res.is_err());
    // Only the operator may settle.
    ctx("mallory.near", NearToken::from_yoctonear(0), h.now);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        h.c.settle_batch(Batch { id: h.c.last_batch_id + 1, orders: vec![], ops: vec![], journal_seq: 99, journal_hash: [0; 32] })
    }));
    assert!(res.is_err());
}

#[test]
fn revoked_key_cancel_and_nonce_floor_bind_the_operator() {
    let mut h = H::new();
    let a = h.trader("alice.near", 1_000_000_000);
    let b = h.trader("bob.near", 1_000_000_000);
    let s1 = h.order(&b, Side::Sell, 1_000, 10, |o| o.nonce = 100);
    let s2 = h.order(&b, Side::Sell, 1_000, 10, |o| o.nonce = 101);
    let s3 = h.order(&b, Side::Sell, 1_000, 10, |o| o.nonce = 5);
    let buy = h.order(&a, Side::Buy, 1_000, 100, |_| {});
    // Bob cancels s1 from his wallet, and raises his nonce floor past s3.
    ctx("bob.near", NearToken::from_yoctonear(0), h.now);
    let hash = wire::hex32(&ledger::order_hash(&NearCrypto, &s1));
    h.c.cancel_order(hash, U64(s1.expiry_ns));
    h.c.set_min_nonce(U64(6));
    let r = h.settle(vec![s1, s2.clone(), s3, buy.clone()], vec![fill(0, 3, 10, 1_000), fill(2, 3, 10, 1_000), fill(1, 3, 5, 1_000)]);
    assert_eq!(r, vec![(0, "order_canceled".into()), (1, "nonce_below_min".into())]);
    // Bob revokes his key: no more fills of orders it signed.
    ctx("bob.near", NearToken::from_yoctonear(0), h.now);
    let _ = h.c.remove_key(npk(b.key.public()));
    let r = h.settle(vec![s2, buy], vec![fill(0, 1, 5, 1_000)]);
    assert_eq!(r, vec![(0, "key_not_registered".into())]);
    assert_eq!(h.size("bob.near"), -5);
}

#[test]
fn withdrawals_pay_only_the_owner_once() {
    let mut h = H::new();
    let a = h.trader("alice.near", 1_000);
    let ok = wire::withdraw_message(CHAIN, CONTRACT, "alice.near", USDC, 400, 1);
    let wrong_token = wire::withdraw_message(CHAIN, CONTRACT, "alice.near", "fake.near", 400, 2);
    let op = |m: &[u8], n: u64| Op::Withdraw { account: "alice.near".into(), amount: 400, nonce: n, public_key: a.key.public(), signature: Sig64(a.key.sign(m)) };
    let r = h.settle(vec![], vec![op(&ok, 1), op(&ok, 1), op(&wrong_token, 2)]);
    assert_eq!(r, vec![(1, "nonce_used".into()), (2, "bad_signature".into())]);
    assert_eq!(h.free("alice.near"), 600);
    assert_eq!(h.c.g.total_collateral, 600);
    assert!(get_logs().iter().any(|l| l.contains("batch_settled")));
    // A failed token transfer is re-credited.
    testing_env!(
        VMContextBuilder::new().current_account_id(acc(CONTRACT)).predecessor_account_id(acc(CONTRACT)).build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![near_sdk::PromiseResult::Failed]
    );
    assert!(!h.c.on_payout(acc("alice.near"), U128(400), true));
    assert_eq!(h.free("alice.near"), 1_000);
}

#[test]
fn forced_withdrawal_freeze_and_escape() {
    let mut h = H::new();
    let a = h.trader("alice.near", 1_000_000_000);
    let b = h.trader("bob.near", 1_000_000_000);
    let sell = h.order(&b, Side::Sell, 1_000, 100, |_| {});
    let buy = h.order(&a, Side::Buy, 1_000, 100, |_| {});
    assert!(h.settle(vec![sell, buy], vec![fill(0, 1, 100, 1_000)]).is_empty());
    ctx("alice.near", FORCE_DEPOSIT, h.now);
    let id = h.c.request_forced_withdrawal(U128(1));
    // Too early.
    ctx("anyone.near", NearToken::from_yoctonear(0), h.now + 10);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.c.freeze())).is_err());
    // Operator ignores it past the timeout: anyone freezes.
    h.now += 3_601 * NS_PER_SEC;
    ctx("anyone.near", NearToken::from_yoctonear(0), h.now);
    h.c.freeze();
    assert!(h.c.escape_scan(100));
    let total = h.c.g.total_collateral;
    let fees = h.c.g.fee_balance;
    let mut paid = 0;
    for who in ["alice.near", "bob.near"] {
        ctx(who, NearToken::from_yoctonear(0), h.now);
        let before = h.c.g.total_collateral;
        let _ = h.c.escape_withdraw();
        paid += before - h.c.g.total_collateral;
    }
    // Everyone's claim at the last mark, nothing created: what's left is exactly the fee ledger.
    assert_eq!(total - paid, fees);
    let _ = id;
}

#[test]
fn admin_loosening_is_timelocked_and_tightening_is_not() {
    ctx("owner.near", NearToken::from_yoctonear(0), T0);
    let r = Trader::new("reporter", 201);
    let mut c = Contract::new(acc("owner.near"), acc("operator.near"), CHAIN.into(), acc(USDC), acc(INSURANCE), vec![pk_str(&r.key.public())], 1, U64(1), U64(86_400 * NS_PER_SEC), None);
    let mut p = testkit::model_params();
    p.mark_limit_bps = 600;
    c.create_market(near_sdk::serde_json::to_value(&p).unwrap());
    let id = c.set_market_status(MKT.into(), "ACTIVE".into()).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.execute_proposal(id))).is_err());
    // Lower leverage: immediate, policy hash changes.
    let h0 = c.markets.get(MKT).unwrap().policy_hash;
    p.initial_margin_bps = 6_000;
    assert!(c.update_market(near_sdk::serde_json::to_value(&p).unwrap()).is_none());
    assert_ne!(c.markets.get(MKT).unwrap().policy_hash, h0);
    // Higher leverage: queued.
    p.initial_margin_bps = 2_000;
    p.max_leverage = 5;
    assert!(c.update_market(near_sdk::serde_json::to_value(&p).unwrap()).is_some());
    assert_eq!(c.markets.get(MKT).unwrap().params.initial_margin_bps, 6_000);
    // Non-owner can't touch markets.
    ctx("mallory.near", NearToken::from_yoctonear(0), T0);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.set_market_status(MKT.into(), "HALTED".into()))).is_err());
}
