use super::*;
use ed25519_dalek::{Signer, SigningKey};
use near_sdk::test_utils::{accounts, VMContextBuilder};
use near_sdk::testing_env;

const CID: u32 = 0x5a46_0002;

fn keys(tag: u8) -> Vec<SigningKey> {
    (0..3).map(|i| SigningKey::from_bytes(&[tag + i; 32])).collect()
}

fn hexes(k: &[SigningKey]) -> Vec<String> {
    k.iter().map(|s| hex::encode(s.verifying_key().to_bytes())).collect()
}

fn ctx(pred: AccountId, deposit: NearToken, ts: u64) {
    let mut b = VMContextBuilder::new();
    b.current_account_id("wzfx.test.near".parse().unwrap()).predecessor_account_id(pred).attached_deposit(deposit).block_timestamp(ts);
    testing_env!(b.build());
}

fn setup() -> (Contract, Vec<SigningKey>) {
    ctx(accounts(0), NearToken::from_yoctonear(0), 1_000);
    let k = keys(10);
    let c = Contract::new(accounts(0), hexes(&k), 2, CID, "zfx:5a460002:native".into(), 60, None);
    (c, k)
}

fn register(c: &mut Contract, who: AccountId) {
    ctx(who.clone(), NearToken::from_millinear(10), 1_000);
    c.storage_deposit(Some(who), None);
}

fn att(dep: u8, to: AccountId, amount: u128) -> MintAttestation {
    MintAttestation { epoch: 0, deposit_id: hex::encode([dep; 32]), recipient: to, amount: U128(amount), native_height: U64(42), expiry_ns: U64(10_000) }
}

fn sign(k: &[SigningKey], digest: &[u8], who: &[u8]) -> Vec<Approval> {
    who.iter().map(|i| Approval { member: *i, signature: hex::encode(k[*i as usize].sign(digest).to_bytes()) }).collect()
}

fn mint_sigs(k: &[SigningKey], a: &MintAttestation, who: &[u8]) -> Vec<Approval> {
    sign(k, &mint_digest("wzfx.test.near", CID, "zfx:5a460002:native", a), who)
}

fn native_addr(tag: u8) -> String {
    let a = [tag; 32];
    let mut m = b"ZFX-addrck".to_vec();
    m.extend_from_slice(&a);
    let ck = env::sha256(&m);
    format!("zfx1{}{}", hex::encode(a), hex::encode(&ck[..4]))
}

#[test]
fn metadata_and_two_of_three_mint() {
    let (mut c, k) = setup();
    let md = c.ft_metadata();
    assert_eq!((md.symbol.as_str(), md.decimals, md.spec.as_str()), ("wZFX", 8, FT_METADATA_SPEC));
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 2]));
    assert_eq!(c.ft_balance_of(accounts(1)).0, 500);
    assert_eq!(c.ft_total_supply().0, 500);
    assert!(c.is_minted(a.deposit_id.clone()));
}

#[test]
#[should_panic(expected = "insufficient committee approvals")]
fn one_of_three_cannot_mint() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[1]));
}

#[test]
#[should_panic(expected = "duplicate approval")]
fn duplicate_member_rejected() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    let s = mint_sigs(&k, &a, &[1]);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a, vec![s[0].clone(), s[0].clone()]);
}

#[test]
#[should_panic(expected = "deposit already minted (replay)")]
fn replay_rejected() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
}

#[test]
#[should_panic(expected = "attestation expired")]
fn expired_rejected() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    ctx(accounts(2), NearToken::from_yoctonear(0), 20_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
}

#[test]
#[should_panic(expected = "invalid committee signature")]
fn signature_for_other_contract_rejected() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    let d = mint_digest("other.test.near", CID, "zfx:5a460002:native", &a);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a, sign(&k, &d, &[0, 1]));
}

#[test]
#[should_panic(expected = "invalid committee signature")]
fn tampered_amount_rejected() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    let s = mint_sigs(&k, &a, &[0, 1]);
    let mut b = a.clone();
    b.amount = U128(5000);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(b, s);
}

#[test]
#[should_panic(expected = "recipient not registered")]
fn unregistered_recipient_rejected() {
    let (mut c, k) = setup();
    let a = att(1, accounts(3), 500);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
}

#[test]
#[should_panic(expected = "mint overflow")]
fn mint_overflow_rejected() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), u128::MAX);
    let b = att(2, accounts(1), 1);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
    c.bridge_mint(b.clone(), mint_sigs(&k, &b, &[0, 1]));
}

#[test]
fn burn_creates_withdrawal_record() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
    ctx(accounts(1), NearToken::from_yoctonear(1), 3_000);
    let r = c.bridge_burn(U128(200), native_addr(7));
    assert_eq!(r.nonce.0, 0);
    assert_eq!(r.withdrawal_id, withdrawal_id("wzfx.test.near", 0));
    assert_eq!(c.ft_balance_of(accounts(1)).0, 300);
    assert_eq!(c.ft_total_supply().0, 300);
    assert_eq!(c.get_burn(U64(0)).unwrap(), r);
    assert_eq!(c.burns_since(U64(0), 10).len(), 1);
}

#[test]
#[should_panic(expected = "not a valid zfx1 address")]
fn burn_to_bad_address_rejected() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 500);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
    let mut bad = native_addr(7);
    bad.replace_range(10..11, if &bad[10..11] == "a" { "b" } else { "a" });
    ctx(accounts(1), NearToken::from_yoctonear(1), 3_000);
    c.bridge_burn(U128(100), bad);
}

#[test]
#[should_panic(expected = "doesn't have enough balance")]
fn cannot_burn_more_than_balance() {
    let (mut c, _) = setup();
    register(&mut c, accounts(1));
    ctx(accounts(1), NearToken::from_yoctonear(1), 3_000);
    c.bridge_burn(U128(1), native_addr(7));
}

#[test]
fn pause_by_one_member_unpause_needs_two() {
    let (mut c, k) = setup();
    let d = admin_digest("wzfx.test.near", 0, 0, &AdminAction::Pause);
    ctx(accounts(3), NearToken::from_yoctonear(0), 2_000);
    c.bridge_pause(Some(sign(&k, &d, &[2])));
    assert_eq!(c.bridge_state()["paused"], true);
    let d2 = admin_digest("wzfx.test.near", 0, 1, &AdminAction::Unpause);
    let one = sign(&k, &d2, &[0]);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut c2 = Contract::new(accounts(0), hexes(&k), 2, CID, "x".into(), 60, None);
        c2.bridge_pause(None); // owner? no: predecessor is accounts(3)
    }));
    assert!(r.is_err(), "non-owner without approvals cannot pause");
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut cc = Contract::new(accounts(0), hexes(&k), 2, CID, "zfx:5a460002:native".into(), 60, None);
        cc.paused = true;
        cc.admin_nonce = 1;
        cc.bridge_unpause(one.clone());
    }));
    assert!(r.is_err(), "unpause with one approval must fail");
    c.bridge_unpause(sign(&k, &d2, &[0, 1]));
    assert_eq!(c.bridge_state()["paused"], false);
}

#[test]
#[should_panic(expected = "bridge paused")]
fn paused_blocks_mint() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    ctx(accounts(0), NearToken::from_yoctonear(0), 2_000);
    c.bridge_pause(None); // owner
    let a = att(1, accounts(1), 500);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
}

#[test]
fn rotation_respects_timelock_and_changes_epoch() {
    let (mut c, k) = setup();
    let next = keys(50);
    let action = AdminAction::Rotate { committee: hexes(&next) };
    let d = admin_digest("wzfx.test.near", 0, 0, &action);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.propose_rotation(hexes(&next), sign(&k, &d, &[1, 2]));
    let early = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut cc = Contract::new(accounts(0), hexes(&k), 2, CID, "x".into(), 60, None);
        cc.pending_rotation = Some(PendingRotation { committee: hexes(&next), activate_ns: U64(u64::MAX) });
        cc.apply_rotation();
    }));
    assert!(early.is_err());
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000 + 60_000_000_000);
    c.apply_rotation();
    assert_eq!(c.bridge_state()["epoch"], 1);
    // old committee no longer mints; new committee with epoch 1 does
    register(&mut c, accounts(1));
    let mut a = att(9, accounts(1), 10);
    a.epoch = 1;
    a.expiry_ns = U64(u64::MAX);
    ctx(accounts(2), NearToken::from_yoctonear(0), 70_000_000_000);
    c.bridge_mint(a.clone(), mint_sigs(&next, &a, &[0, 1]));
    assert_eq!(c.ft_balance_of(accounts(1)).0, 10);
}

#[test]
#[should_panic(expected = "withdraw or bridge_burn")]
fn force_unregister_with_balance_forbidden() {
    let (mut c, k) = setup();
    register(&mut c, accounts(1));
    let a = att(1, accounts(1), 5);
    ctx(accounts(2), NearToken::from_yoctonear(0), 2_000);
    c.bridge_mint(a.clone(), mint_sigs(&k, &a, &[0, 1]));
    ctx(accounts(1), NearToken::from_yoctonear(1), 3_000);
    c.storage_unregister(Some(true));
}

/// Same inputs and expected values as bridge/relayers/src/digest.rs
/// (`digests_match_golden_vectors`): the on-chain digests and the relayer's
/// off-chain digests are pinned to identical bytes.
#[test]
fn digests_match_relayer_golden_vectors() {
    let a = MintAttestation {
        epoch: 3,
        deposit_id: "ab".repeat(32),
        recipient: "alice.test.near".parse().unwrap(),
        amount: U128(123456789),
        native_height: U64(77),
        expiry_ns: U64(1800000000000000000),
    };
    assert_eq!(hex::encode(mint_digest("wzfx.test.near", 0x5a460002, "zfx:5a460002:native", &a)), "3507a0edc7baab770827cebebe5c7df439be2f91c33f9d3f7391c3b2a9647e98");
    let rot = AdminAction::Rotate { committee: vec!["11".repeat(32), "22".repeat(32), "33".repeat(32)] };
    assert_eq!(hex::encode(admin_digest("wzfx.test.near", 3, 9, &rot)), "038fb280438db03918560ae6fc462deb4003faa6e4d6c58380d486bfb24d139c");
    assert_eq!(withdrawal_id("wzfx.test.near", 42), "b1447206675f11affd8453b8d33f158c2b4e9b0ca809e2e5c6beaa0d26649b82");
}
