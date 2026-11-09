//! Consensus v2 state-transition tests: maturity, replay/domain separation,
//! overflow, bridge lockbox custody and 2-of-3 SLH-DSA committee rules.

use zfx_verify::block::Block;
use zfx_verify::bridge::{self, AdminAction, AdminOp, Approval, BridgeOp, BridgeParams, Release};
use zfx_verify::header::BlockHeader;
use zfx_verify::state::{ConsensusParams, State};
use zfx_verify::tx::{Coinbase, LeafKey, P2mrKeys};

const CID: u32 = 0x5a46_7777;

struct Committee {
    keys: Vec<LeafKey>,
}

impl Committee {
    fn new(tag: u8) -> Committee {
        Committee { keys: (0..3).map(|i| bridge::member_key(&[tag.wrapping_add(i); 32])).collect() }
    }
    fn hex(&self) -> Vec<String> {
        self.keys.iter().map(|k| hex::encode(k.public_key_bytes())).collect()
    }
    fn approve(&self, digest: &[u8; 32], who: &[u8]) -> Vec<Approval> {
        who.iter().map(|i| Approval { member: *i, sig: bridge::sign_digest(&self.keys[*i as usize], digest) }).collect()
    }
}

fn params(c: &Committee, maturity: u64) -> ConsensusParams {
    ConsensusParams {
        chain_id: CID,
        subsidy: zfx_verify::monetary::SubsidySchedule::Constant { per_block: 50 },
        coinbase_maturity: maturity,
        bridge: Some(BridgeParams {
            committee: c.hex(),
            threshold: 2,
            rotation_timelock_blocks: 10,
            min_lock: 100,
            activation_height: 1,
            near_contract: "wzfx.test.near".into(),
        }),
    }
}

fn block(height: u64, miner: [u8; 32], amount: u64, txs: Vec<zfx_verify::tx::Transaction>, ops: Vec<BridgeOp>) -> Block {
    Block {
        header: BlockHeader {
            version: 2,
            chain_id: CID,
            height,
            parent: [0; 32],
            tx_root: [0; 32],
            state_root: [0; 32],
            timestamp: 0,
            target: [0; 32],
            nonce: 0,
        },
        coinbase: Coinbase { height, to: miner, amount },
        txs,
        bridge_ops: ops,
    }
}

#[test]
fn coinbase_maturity_and_supply() {
    let c = Committee::new(1);
    let p = params(&c, 3);
    let mut st = State::genesis(&p, &[]).unwrap();
    let m = [7u8; 32];
    for h in 1..=5u64 {
        st.apply_block(&p, &block(h, m, 50, vec![], vec![])).unwrap();
        st.audit_with(CID).unwrap();
        // reward of block h unlocks at h+3, applied when block h+3 is processed
        let spendable = st.account(&m).balance;
        let expect = 50 * h.saturating_sub(3);
        assert_eq!(spendable, expect, "height {}", h);
        assert_eq!(st.immature_of(&m) + spendable, 50 * h);
    }
    // overpaying coinbase rejected
    assert!(st.apply_block(&p, &block(6, m, 51, vec![], vec![])).unwrap_err().contains("coinbase"));
}

#[test]
fn wrong_chain_replay_and_overflow_rejected() {
    let c = Committee::new(2);
    let p = params(&c, 0);
    let alice = P2mrKeys::from_seed(&[0xa1; 32]);
    let mut st = State::genesis(&p, &[(alice.address(), u64::MAX - 10)]).unwrap();
    let other_chain = alice.sign_tx(CID + 1, [9; 32], 1, 1, 0, 0).unwrap();
    assert!(st.apply_tx(&p, 1, &other_chain).unwrap_err().contains("chain"));
    let overflow = alice.sign_tx(CID, [9; 32], u64::MAX, 1, 0, 0).unwrap();
    assert!(st.apply_tx(&p, 1, &overflow).unwrap_err().contains("overflow"));
    let ok = alice.sign_tx(CID, [9; 32], 5, 1, 0, 0).unwrap();
    st.apply_tx(&p, 1, &ok).unwrap();
    assert!(st.apply_tx(&p, 1, &ok).unwrap_err().contains("nonce"), "same tx twice: replay");
}

#[test]
fn bridge_lockbox_deposit_and_quorum_release() {
    let c = Committee::new(3);
    let p = params(&c, 0);
    let alice = P2mrKeys::from_seed(&[0xa2; 32]);
    let bob = [0xb0u8; 32];
    let lockbox = bridge::lockbox_address(CID);
    let mut st = State::genesis(&p, &[(alice.address(), 10_000)]).unwrap();

    // deposits: memo required, NEAR id valid, min amount
    let no_memo = alice.sign_tx(CID, lockbox, 500, 1, 0, 0).unwrap();
    assert!(st.clone().apply_tx(&p, 1, &no_memo).unwrap_err().contains("memo"));
    let bad_memo = alice.sign_tx_memo(CID, lockbox, 500, 1, 0, 0, Some("Not_A.near".into())).unwrap();
    assert!(st.clone().apply_tx(&p, 1, &bad_memo).is_err());
    let small = alice.sign_tx_memo(CID, lockbox, 99, 1, 0, 0, Some("alice.test.near".into())).unwrap();
    assert!(st.clone().apply_tx(&p, 1, &small).unwrap_err().contains("minimum"));
    let dep = alice.sign_tx_memo(CID, lockbox, 1000, 1, 0, 0, Some("alice.test.near".into())).unwrap();
    let eff = st.apply_block(&p, &block(1, [1; 32], 51, vec![dep.clone()], vec![])).unwrap();
    assert_eq!(eff.deposits.len(), 1);
    assert_eq!(eff.deposits[0].0, dep.txid());
    assert_eq!(st.account(&lockbox).balance, 1000);
    st.audit_with(CID).unwrap();

    let mk = |wid: u8, amount: u64, epoch: u32, expiry: u64| Release {
        epoch,
        withdrawal_id: [wid; 32],
        near_burn_ref: format!("wzfx.test.near:{}:tx", wid),
        recipient: bob,
        amount,
        expiry_height: expiry,
        approvals: vec![],
    };
    // 1-of-3 cannot release
    let mut r = mk(1, 400, 0, 100);
    r.approvals = c.approve(&bridge::release_digest(CID, &r), &[0]);
    assert!(st.clone().apply_bridge_op(&p, 2, &BridgeOp::Release(r.clone())).unwrap_err().contains("needs 2"));
    // duplicate member counted once -> rejected
    let d = bridge::release_digest(CID, &r);
    let mut dup = r.clone();
    dup.approvals = vec![c.approve(&d, &[1])[0].clone(), c.approve(&d, &[1])[0].clone()];
    assert!(st.clone().apply_bridge_op(&p, 2, &BridgeOp::Release(dup)).unwrap_err().contains("duplicate"));
    // signature over a different amount (tampered after signing) rejected
    let mut tampered = r.clone();
    tampered.approvals = c.approve(&d, &[0, 1]);
    tampered.amount = 999;
    assert!(st.clone().apply_bridge_op(&p, 2, &BridgeOp::Release(tampered)).unwrap_err().contains("invalid signature"));
    // signatures for another chain id rejected (domain separation)
    let mut other = r.clone();
    other.approvals = c.approve(&bridge::release_digest(CID + 1, &r), &[0, 1]);
    assert!(st.clone().apply_bridge_op(&p, 2, &BridgeOp::Release(other)).is_err());
    // 2-of-3 succeeds
    r.approvals = c.approve(&d, &[0, 2]);
    st.apply_block(&p, &block(2, [1; 32], 50, vec![], vec![BridgeOp::Release(r.clone())])).unwrap();
    assert_eq!(st.account(&bob).balance, 400);
    assert_eq!(st.account(&lockbox).balance, 600);
    st.audit_with(CID).unwrap();
    // replay of the same withdrawal id rejected
    assert!(st.clone().apply_bridge_op(&p, 3, &BridgeOp::Release(r.clone())).unwrap_err().contains("replay"));
    // cannot release more than locked
    let mut big = mk(2, 601, 0, 100);
    big.approvals = c.approve(&bridge::release_digest(CID, &big), &[1, 2]);
    assert!(st.clone().apply_bridge_op(&p, 3, &BridgeOp::Release(big)).unwrap_err().contains("insufficient"));
    // expired approvals rejected
    let mut exp = mk(3, 10, 0, 2);
    exp.approvals = c.approve(&bridge::release_digest(CID, &exp), &[0, 1]);
    assert!(st.clone().apply_bridge_op(&p, 3, &BridgeOp::Release(exp)).unwrap_err().contains("expired"));
    // the lockbox cannot sign a normal transaction
    let fake = zfx_verify::tx::Transaction { from: lockbox, ..alice.sign_tx(CID, bob, 1, 1, 1, 0).unwrap() };
    assert!(st.clone().apply_tx(&p, 3, &fake).unwrap_err().contains("lockbox"));
    assert!(fake.verify_authorization().is_err(), "and no witness can authorize it");
}

#[test]
fn bridge_admin_pause_unpause_rotate() {
    let c = Committee::new(4);
    let next = Committee::new(40);
    let p = params(&c, 0);
    let alice = P2mrKeys::from_seed(&[0xa3; 32]);
    let lockbox = bridge::lockbox_address(CID);
    let mut st = State::genesis(&p, &[(alice.address(), 10_000)]).unwrap();
    let dep = alice.sign_tx_memo(CID, lockbox, 1000, 1, 0, 0, Some("alice.near".into())).unwrap();
    st.apply_block(&p, &block(1, [1; 32], 51, vec![dep], vec![])).unwrap();

    let admin = |nonce: u64, epoch: u32, action: AdminAction, who: &[u8], comm: &Committee| {
        let mut op = AdminOp { epoch, op_nonce: nonce, action, approvals: vec![] };
        op.approvals = comm.approve(&bridge::admin_digest(CID, &op), who);
        BridgeOp::Admin(op)
    };
    // pause by a single member
    st.apply_bridge_op(&p, 2, &admin(0, 0, AdminAction::Pause, &[2], &c)).unwrap();
    assert!(st.bridge.as_ref().unwrap().paused);
    let dep2 = alice.sign_tx_memo(CID, lockbox, 1000, 1, 1, 0, Some("alice.near".into())).unwrap();
    assert!(st.clone().apply_tx(&p, 2, &dep2).unwrap_err().contains("paused"));
    // replaying the pause op (old nonce) rejected
    assert!(st.clone().apply_bridge_op(&p, 2, &admin(0, 0, AdminAction::Pause, &[2], &c)).unwrap_err().contains("replay"));
    // unpause needs 2
    assert!(st.clone().apply_bridge_op(&p, 2, &admin(1, 0, AdminAction::Unpause, &[0], &c)).unwrap_err().contains("needs 2"));
    st.apply_bridge_op(&p, 2, &admin(1, 0, AdminAction::Unpause, &[0, 1], &c)).unwrap();
    assert!(!st.bridge.as_ref().unwrap().paused);
    // rotation must respect the timelock (10 blocks)
    let rot = |at: u64| AdminAction::Rotate { members: next.hex(), activate_height: at };
    assert!(st.clone().apply_bridge_op(&p, 3, &admin(2, 0, rot(5), &[0, 1], &c)).unwrap_err().contains("timelock"));
    st.apply_bridge_op(&p, 3, &admin(2, 0, rot(13), &[0, 1], &c)).unwrap();
    // still old committee before activation
    for h in 4..13 {
        st.apply_block(&p, &block(h, [1; 32], 50, vec![], vec![])).unwrap();
    }
    assert_eq!(st.bridge.as_ref().unwrap().epoch, 0);
    st.apply_block(&p, &block(13, [1; 32], 50, vec![], vec![])).unwrap();
    assert_eq!(st.bridge.as_ref().unwrap().epoch, 1);
    // old committee can no longer release; new one can
    let mut r =
        Release { epoch: 1, withdrawal_id: [9; 32], near_burn_ref: "x:1:t".into(), recipient: [5; 32], amount: 10, expiry_height: 100, approvals: vec![] };
    let d = bridge::release_digest(CID, &r);
    r.approvals = c.approve(&d, &[0, 1]);
    assert!(st.clone().apply_bridge_op(&p, 14, &BridgeOp::Release(r.clone())).is_err());
    r.approvals = next.approve(&d, &[1, 2]);
    st.apply_bridge_op(&p, 14, &BridgeOp::Release(r)).unwrap();
    st.audit_with(CID).unwrap();
}

#[test]
fn state_root_is_order_independent_of_insertion_and_changes_with_state() {
    let c = Committee::new(5);
    let p = params(&c, 0);
    let a = [1u8; 32];
    let b = [2u8; 32];
    let s1 = State::genesis(&p, &[(a, 5), (b, 7)]).unwrap();
    let s2 = State::genesis(&p, &[(b, 7), (a, 5)]).unwrap();
    assert_eq!(s1.root(), s2.root());
    let s3 = State::genesis(&p, &[(a, 5), (b, 8)]).unwrap();
    assert_ne!(s1.root(), s3.root());
}
