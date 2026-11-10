use super::*;
use ed25519_dalek::{Signer, SigningKey};
use near_sdk::test_utils::{VMContextBuilder, get_logs};
use near_sdk::testing_env;

fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}

const OB: &str = "ob.test.near";
const USDC: &str = "usdc.test.near";
const WNEAR: &str = "wrap.test.near";

fn ctx(pred: &str, deposit: NearToken, ts: u64) {
    testing_env!(
        VMContextBuilder::new()
            .current_account_id(acc(OB))
            .predecessor_account_id(acc(pred))
            .attached_deposit(deposit)
            .block_timestamp(ts)
            .build()
    );
}

fn setup() -> Contract {
    ctx("owner.near", NearToken::from_yoctonear(0), 1);
    let mut c = Contract::new(acc("owner.near"), acc("operator.near"), U64(1_000));
    c.add_token(acc(USDC));
    c.add_token(acc(WNEAR));
    c
}

fn deposit(c: &mut Contract, who: &str, token: &str, amount: u128) {
    ctx(token, NearToken::from_yoctonear(0), 1);
    assert!(matches!(
        c.ft_on_transfer(acc(who), U128(amount), String::new()),
        PromiseOrValue::Value(U128(0))
    ));
}

fn key(seed: u8) -> (SigningKey, PublicKey) {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    let mut b = vec![0u8];
    b.extend_from_slice(sk.verifying_key().as_bytes());
    (sk, PublicKey::try_from(b).unwrap())
}

fn register(c: &mut Contract, who: &str, seed: u8) -> (SigningKey, PublicKey) {
    let (sk, pk) = key(seed);
    ctx(who, KEY_DEPOSIT, 1);
    c.register_key(pk.clone());
    (sk, pk)
}

fn d(account: &str, token: &str, delta: i128) -> DeltaArg {
    DeltaArg {
        account: acc(account),
        token: acc(token),
        delta: I128(delta),
    }
}

fn batch(id: u64, entries: Vec<DeltaArg>, withdrawals: Vec<WithdrawArg>, seal: bool) -> BatchArgs {
    BatchArgs {
        id: U64(id),
        entries,
        withdrawals,
        forced: vec![],
        seal,
        journal_seq: U64(id * 10),
        journal_hash: format!("{id:064x}"),
    }
}

fn signed_withdraw(sk: &SigningKey, pk: &PublicKey, account: &str, token: &str, amount: u128, nonce: u64) -> WithdrawArg {
    let msg = zfx_perps_wire::withdraw_msg(OB, account, token, amount, nonce);
    WithdrawArg {
        account: acc(account),
        token: acc(token),
        amount: U128(amount),
        nonce: U64(nonce),
        public_key: pk.clone(),
        signature: Base64VecU8(sk.sign(&msg).to_bytes().to_vec()),
    }
}

fn last_event(c: &Contract) -> Event {
    let n = c.get_state().next_event.0;
    c.get_events(U64(n - 1), 1)[0].1.clone()
}

#[test]
fn deposits_credit_and_emit_sequenced_events() {
    let mut c = setup();
    deposit(&mut c, "alice.near", USDC, 500);
    assert!(get_logs().iter().any(|l| l.starts_with("EVENT_JSON:") && l.contains("\"seq\":1")));
    // Beneficiary in msg.
    ctx(USDC, NearToken::from_yoctonear(0), 1);
    let _ = c.ft_on_transfer(acc("alice.near"), U128(7), "bob.near".into());
    assert_eq!(c.get_balance(acc("alice.near"), acc(USDC)).0, 500);
    assert_eq!(c.get_balance(acc("bob.near"), acc(USDC)).0, 7);
    // Unknown token: refunded, nothing credited.
    ctx("evil.near", NearToken::from_yoctonear(0), 1);
    assert!(matches!(
        c.ft_on_transfer(acc("alice.near"), U128(9), String::new()),
        PromiseOrValue::Value(U128(9))
    ));
    let ev = c.get_events(U64(1), 10);
    assert_eq!(ev.len(), 2);
    assert_eq!(
        ev[0],
        (
            U64(1),
            Event::Deposit {
                account: acc("alice.near"),
                token: acc(USDC),
                amount: U128(500)
            }
        )
    );
}

#[test]
fn keys_unique_and_removable() {
    let mut c = setup();
    let (_, pk) = register(&mut c, "alice.near", 1);
    assert_eq!(c.get_keys(acc("alice.near")).len(), 1);
    ctx("bob.near", KEY_DEPOSIT, 1);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.register_key(pk.clone())));
    assert!(r.is_err(), "same key can't be registered twice");
    ctx("alice.near", NearToken::from_yoctonear(0), 1);
    c.remove_key(pk.clone()).detach();
    assert!(c.get_keys(acc("alice.near")).is_empty());
    assert_eq!(
        last_event(&c),
        Event::KeyRemoved {
            account: acc("alice.near"),
            public_key: pk
        }
    );
}

#[test]
fn batches_apply_in_order_and_net_to_zero() {
    let mut c = setup();
    deposit(&mut c, "alice.near", USDC, 1_000);
    deposit(&mut c, "bob.near", WNEAR, 50);
    ctx("operator.near", NearToken::from_yoctonear(0), 1);
    // Alice buys 50 wNEAR from Bob for 400 USDC, fee 4 to fees.near.
    let t = vec![
        d("alice.near", USDC, -404),
        d("alice.near", WNEAR, 50),
        d("bob.near", WNEAR, -50),
        d("bob.near", USDC, 400),
        d("fees.near", USDC, 4),
    ];
    c.settle_batch(batch(1, t, vec![], true));
    assert_eq!(c.get_balance(acc("alice.near"), acc(USDC)).0, 596);
    assert_eq!(c.get_balance(acc("bob.near"), acc(USDC)).0, 400);
    assert_eq!(c.get_balance(acc("fees.near"), acc(USDC)).0, 4);
    let st = c.get_state();
    assert_eq!((st.last_batch_id.0, st.last_journal_seq.0), (1, 10));
    assert_eq!(last_event(&c), Event::BatchSettled { batch_id: U64(1) });
}

fn expect_panic(f: impl FnOnce(), msg: &str) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    let e = r.expect_err("should panic");
    let s = e
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(s.contains(msg), "panic {s:?} does not contain {msg:?}");
}

// A panicking call rolls back on-chain but not in unit tests (and contracts in one test share the
// mocked storage), so every rejected case runs in its own test.
fn funded() -> Contract {
    let mut c = setup();
    deposit(&mut c, "alice.near", USDC, 100);
    ctx("operator.near", NearToken::from_yoctonear(0), 1);
    c
}

#[test]
fn rejects_value_creation() {
    expect_panic(
        || funded().settle_batch(batch(1, vec![d("bob.near", USDC, 5)], vec![], true)),
        "BATCH_GROUP_NOT_ZERO_SUM",
    );
}

#[test]
fn rejects_negative_balance() {
    expect_panic(
        || funded().settle_batch(batch(1, vec![d("alice.near", USDC, -101), d("bob.near", USDC, 101)], vec![], true)),
        "NEGATIVE_BALANCE",
    );
}

#[test]
fn rejects_out_of_order_batch() {
    expect_panic(|| funded().settle_batch(batch(2, vec![], vec![], true)), "BAD_BATCH_ID");
}

#[test]
fn rejects_non_operator() {
    let mut c = funded();
    ctx("mallory.near", NearToken::from_yoctonear(0), 1);
    expect_panic(|| c.settle_batch(batch(1, vec![], vec![], true)), "OPERATOR_ONLY");
}

#[test]
fn unsealed_batches_may_be_unbalanced_until_the_seal() {
    let mut c = funded();
    c.settle_batch(batch(1, vec![d("alice.near", USDC, -60)], vec![], false));
    c.settle_batch(batch(2, vec![d("bob.near", USDC, 60)], vec![], true));
    assert_eq!(c.get_balance(acc("bob.near"), acc(USDC)).0, 60);
    assert_eq!(c.get_balance(acc("alice.near"), acc(USDC)).0, 40);
}

#[test]
fn group_must_close_at_zero() {
    let mut c = funded();
    c.settle_batch(batch(1, vec![d("alice.near", USDC, -60)], vec![], false));
    expect_panic(
        || c.settle_batch(batch(2, vec![d("bob.near", USDC, 59)], vec![], true)),
        "BATCH_GROUP_NOT_ZERO_SUM",
    );
}

#[test]
fn withdrawals_need_the_owners_signature_and_fresh_nonce() {
    let mut c = setup();
    deposit(&mut c, "alice.near", USDC, 1_000);
    let (sk, pk) = register(&mut c, "alice.near", 1);
    let (bob_sk, bob_pk) = register(&mut c, "bob.near", 2);
    ctx("operator.near", NearToken::from_yoctonear(0), 1);
    let good = signed_withdraw(&sk, &pk, "alice.near", USDC, 300, 1);
    let mut tampered = signed_withdraw(&sk, &pk, "alice.near", USDC, 300, 2);
    tampered.amount = U128(900);
    let wrong_key = signed_withdraw(&bob_sk, &bob_pk, "alice.near", USDC, 10, 3);
    let replay = signed_withdraw(&sk, &pk, "alice.near", USDC, 300, 1);
    c.settle_batch(batch(1, vec![], vec![good, tampered, wrong_key, replay], true));
    assert_eq!(c.get_balance(acc("alice.near"), acc(USDC)).0, 700);
    assert_eq!(c.get_withdraw_nonce(acc("alice.near")).0, 1);
    let reasons: Vec<String> = c
        .get_events(U64(1), 50)
        .into_iter()
        .filter_map(|(_, e)| {
            if let Event::WithdrawalReverted { reason, .. } = e {
                Some(reason)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(reasons, vec!["bad signature", "key not registered to account", "nonce reused"]);
}

#[test]
fn failed_payout_is_recredited() {
    let mut c = setup();
    deposit(&mut c, "alice.near", USDC, 10);
    testing_env!(
        VMContextBuilder::new()
            .current_account_id(acc(OB))
            .predecessor_account_id(acc(OB))
            .build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![near_sdk::PromiseResult::Failed],
    );
    assert!(!c.on_payout(acc("alice.near"), acc(USDC), U128(5)));
    assert_eq!(c.get_balance(acc("alice.near"), acc(USDC)).0, 15);
    assert!(matches!(last_event(&c), Event::WithdrawalReverted { .. }));
}

#[test]
fn forced_withdrawal_timeout_freeze_and_escape() {
    let mut c = setup();
    deposit(&mut c, "alice.near", USDC, 1_000);
    deposit(&mut c, "bob.near", USDC, 500);
    ctx("alice.near", FORCE_DEPOSIT, 100);
    let id = c.request_forced_withdrawal(acc(USDC), U128(400));
    assert_eq!(id.0, 0);
    // Not overdue yet.
    ctx("anyone.near", NearToken::from_yoctonear(0), 1_099);
    expect_panic(|| c.freeze(), "NOT_OVERDUE");
    // The operator processes it in time: forced cursor advances, nothing to freeze.
    ctx("operator.near", NearToken::from_yoctonear(0), 500);
    let mut b = batch(1, vec![], vec![], true);
    b.forced = vec![ForcedArg {
        request_id: U64(0),
        account: acc("alice.near"),
        token: USDC.into(),
        amount: U128(400),
    }];
    c.settle_batch(b);
    assert_eq!(c.get_balance(acc("alice.near"), acc(USDC)).0, 600);
    assert_eq!(c.get_state().forced_cursor.0, 1);
    ctx("anyone.near", NearToken::from_yoctonear(0), 10_000);
    expect_panic(|| c.freeze(), "NOT_OVERDUE");
    // Bob files one the operator ignores.
    ctx("bob.near", FORCE_DEPOSIT, 20_000);
    c.request_forced_withdrawal(acc(USDC), U128(1));
    ctx("anyone.near", NearToken::from_yoctonear(0), 21_000);
    c.freeze();
    assert!(c.get_state().frozen);
    ctx("operator.near", NearToken::from_yoctonear(0), 21_001);
    expect_panic(|| c.settle_batch(batch(2, vec![], vec![], true)), "FROZEN");
    // Escape: everyone gets their settled balance back without the operator.
    ctx("bob.near", NearToken::from_yoctonear(0), 21_002);
    c.escape_withdraw(acc(USDC)).detach();
    assert_eq!(c.get_balance(acc("bob.near"), acc(USDC)).0, 0);
    ctx("alice.near", NearToken::from_yoctonear(0), 21_003);
    c.escape_withdraw(acc(USDC)).detach();
    expect_panic(
        || {
            c.escape_withdraw(acc(USDC)).detach();
        },
        "NOTHING_TO_WITHDRAW",
    );
    // Deposits are refunded while frozen.
    ctx(USDC, NearToken::from_yoctonear(0), 21_004);
    assert!(matches!(
        c.ft_on_transfer(acc("alice.near"), U128(3), String::new()),
        PromiseOrValue::Value(U128(3))
    ));
}

#[test]
fn forced_overpay_and_double_processing_rejected() {
    let mut c = setup();
    deposit(&mut c, "alice.near", USDC, 1_000);
    ctx("alice.near", FORCE_DEPOSIT, 100);
    c.request_forced_withdrawal(acc(USDC), U128(10));
    ctx("operator.near", NearToken::from_yoctonear(0), 200);
    let f = |amount| ForcedArg {
        request_id: U64(0),
        account: acc("alice.near"),
        token: USDC.into(),
        amount: U128(amount),
    };
    let mut b = batch(1, vec![], vec![], true);
    b.forced = vec![f(11)];
    expect_panic(|| c.settle_batch(b), "FORCED_OVERPAY");
    let mut b = batch(1, vec![], vec![], true);
    b.forced = vec![f(10)];
    c.settle_batch(b);
    let mut b = batch(2, vec![], vec![], true);
    b.forced = vec![f(0)];
    expect_panic(|| c.settle_batch(b), "FORCED_ALREADY_DONE");
}

#[test]
fn event_pruning() {
    let mut c = setup();
    for i in 0..5 {
        deposit(&mut c, "alice.near", USDC, 1 + i);
    }
    ctx("operator.near", NearToken::from_yoctonear(0), 1);
    c.prune_events(U64(3));
    let ev = c.get_events(U64(1), 10);
    assert_eq!(ev.iter().map(|(s, _)| s.0).collect::<Vec<_>>(), vec![4, 5]);
    assert_eq!(c.get_state().first_event.0, 4);
}
