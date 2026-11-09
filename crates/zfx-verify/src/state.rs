//! Consensus state and state transitions (consensus v2).
//!
//! Every rule that changes balances lives here, so the node, tests and any
//! other implementation apply blocks identically. Integer-only, ordered
//! containers only (BTreeMap/BTreeSet): no hash-map iteration order, no
//! floats, no wall clock.

use crate::block::Block;
use crate::bridge::{self, AdminAction, BridgeOp, BridgeParams};
use crate::hash::{sha256, Hash32};
use crate::tx::{is_valid_near_account_id, Address, Coinbase, Transaction};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Consensus parameters derived from the genesis config.
#[derive(Clone, Debug)]
pub struct ConsensusParams {
    pub chain_id: u32,
    pub block_subsidy: u64,
    /// Blocks before a coinbase becomes spendable (0 = immediately).
    pub coinbase_maturity: u64,
    pub bridge: Option<BridgeParams>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Account {
    pub balance: u64,
    pub nonce: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BridgeState {
    pub epoch: u32,
    #[serde(serialize_with = "ser_keys")]
    pub members: Vec<Vec<u8>>,
    /// (members, activation height) of a scheduled rotation.
    #[serde(skip)]
    pub pending: Option<(Vec<Vec<u8>>, u64)>,
    pub pending_activation: Option<u64>,
    pub paused: bool,
    pub admin_nonce: u64,
    pub locked_total: u128,
    pub released_total: u128,
    pub deposits: u64,
    pub releases: u64,
    #[serde(skip)]
    pub used_withdrawals: BTreeSet<Hash32>,
}

fn ser_keys<S: serde::Serializer>(v: &[Vec<u8>], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(v.iter().map(hex::encode))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub accounts: BTreeMap<Address, Account>,
    /// Coinbase rewards not yet spendable: (unlock height, owner) -> amount.
    pub immature: BTreeMap<(u64, Address), u64>,
    pub supply: u128,
    pub bridge: Option<BridgeState>,
}

/// Effects of applying a block, for indexes and receipts.
#[derive(Clone, Debug, Default)]
pub struct BlockEffects {
    pub fees: u64,
    /// (txid, deposit amount, near recipient) for bridge deposits.
    pub deposits: Vec<(Hash32, u64, String)>,
    /// (op id, withdrawal id, amount) for releases.
    pub releases: Vec<(Hash32, Hash32, u64)>,
}

impl State {
    pub fn genesis(params: &ConsensusParams, allocations: &[(Address, u64)]) -> Result<State, String> {
        let mut st = State::default();
        for (a, v) in allocations {
            st.credit(a, *v)?;
            st.supply += *v as u128;
        }
        if let Some(bp) = &params.bridge {
            st.bridge = Some(BridgeState {
                epoch: 0,
                members: bp.committee_keys()?,
                pending: None,
                pending_activation: None,
                paused: false,
                admin_nonce: 0,
                locked_total: 0,
                released_total: 0,
                deposits: 0,
                releases: 0,
                used_withdrawals: BTreeSet::new(),
            });
        }
        Ok(st)
    }

    pub fn account(&self, a: &Address) -> Account {
        self.accounts.get(a).copied().unwrap_or_default()
    }

    pub fn immature_of(&self, a: &Address) -> u64 {
        self.immature.iter().filter(|((_, o), _)| o == a).map(|(_, v)| *v).sum()
    }

    fn credit(&mut self, a: &Address, v: u64) -> Result<(), String> {
        let acc = self.accounts.entry(*a).or_default();
        acc.balance = acc.balance.checked_add(v).ok_or("balance overflow")?;
        Ok(())
    }

    /// Stateful transaction rules (signatures are checked statelessly by
    /// `Transaction::verify_authorization` before a block is accepted).
    pub fn apply_tx(&mut self, p: &ConsensusParams, height: u64, tx: &Transaction) -> Result<Option<(u64, String)>, String> {
        if tx.chain_id != p.chain_id {
            return Err("wrong chain id (replay from another chain)".into());
        }
        tx.check_memo()?;
        if tx.from == tx.to {
            return Err("self-transfer not allowed".into());
        }
        let lockbox = bridge::lockbox_address(p.chain_id);
        if tx.from == lockbox {
            return Err("the bridge lockbox cannot sign transactions".into());
        }
        let mut deposit = None;
        if tx.to == lockbox {
            let bp = p.bridge.as_ref().ok_or("bridge not configured on this chain")?;
            let bs = self.bridge.as_ref().ok_or("bridge state missing")?;
            if height < bp.activation_height {
                return Err("bridge not active yet".into());
            }
            if bs.paused {
                return Err("bridge paused: deposits rejected".into());
            }
            let memo = tx.memo.as_deref().ok_or("bridge deposit requires a NEAR recipient memo")?;
            if !is_valid_near_account_id(memo) {
                return Err("bridge deposit memo is not a valid NEAR account id".into());
            }
            if tx.amount < bp.min_lock {
                return Err(format!("bridge deposit below minimum {}", bp.min_lock));
            }
            deposit = Some((tx.amount, memo.to_string()));
        }
        let from = self.account(&tx.from);
        if tx.nonce != from.nonce {
            return Err(format!("bad nonce {} (expected {})", tx.nonce, from.nonce));
        }
        let cost = tx.amount.checked_add(tx.fee).ok_or("amount overflow")?;
        if from.balance < cost {
            return Err(format!("insufficient spendable balance {} < {}", from.balance, cost));
        }
        let to = self.account(&tx.to);
        let to_bal = to.balance.checked_add(tx.amount).ok_or("recipient overflow")?;
        self.accounts.insert(tx.from, Account { balance: from.balance - cost, nonce: from.nonce + 1 });
        self.accounts.insert(tx.to, Account { balance: to_bal, nonce: to.nonce });
        self.supply -= tx.fee as u128; // fees are re-minted via the coinbase
        if let Some((amt, _)) = &deposit {
            let bs = self.bridge.as_mut().unwrap();
            bs.locked_total += *amt as u128;
            bs.deposits += 1;
        }
        Ok(deposit)
    }

    /// Bridge operation rules: quorum signatures from the *current*
    /// committee over the exact digest, plus replay and balance checks.
    pub fn apply_bridge_op(&mut self, p: &ConsensusParams, height: u64, op: &BridgeOp) -> Result<Option<(Hash32, u64)>, String> {
        let bp = p.bridge.as_ref().ok_or("bridge not configured on this chain")?;
        if height < bp.activation_height {
            return Err("bridge not active yet".into());
        }
        let lockbox = bridge::lockbox_address(p.chain_id);
        let threshold = bp.threshold as usize;
        let bs = self.bridge.as_ref().ok_or("bridge state missing")?.clone();
        match op {
            BridgeOp::Release(r) => {
                if bs.paused {
                    return Err("bridge paused: releases rejected".into());
                }
                if r.epoch != bs.epoch {
                    return Err(format!("release for epoch {} but committee epoch is {}", r.epoch, bs.epoch));
                }
                if r.near_burn_ref.is_empty() || r.near_burn_ref.len() > bridge::MAX_NEAR_REF_LEN {
                    return Err("bad near_burn_ref".into());
                }
                if height > r.expiry_height {
                    return Err("release approvals expired".into());
                }
                if r.amount == 0 {
                    return Err("zero release".into());
                }
                if bs.used_withdrawals.contains(&r.withdrawal_id) {
                    return Err("withdrawal id already released (replay)".into());
                }
                if r.recipient == lockbox {
                    return Err("cannot release to the lockbox".into());
                }
                let n = bridge::count_approvals(&bs.members, &bridge::release_digest(p.chain_id, r), &r.approvals)?;
                if n < threshold {
                    return Err(format!("release has {} approvals, needs {}", n, threshold));
                }
                let lb = self.account(&lockbox);
                if lb.balance < r.amount {
                    return Err("lockbox balance insufficient (would release unbacked funds)".into());
                }
                let rc = self.account(&r.recipient);
                let new_bal = rc.balance.checked_add(r.amount).ok_or("recipient overflow")?;
                self.accounts.insert(lockbox, Account { balance: lb.balance - r.amount, nonce: lb.nonce });
                self.accounts.insert(r.recipient, Account { balance: new_bal, nonce: rc.nonce });
                let bs = self.bridge.as_mut().unwrap();
                bs.used_withdrawals.insert(r.withdrawal_id);
                bs.released_total += r.amount as u128;
                bs.releases += 1;
                Ok(Some((r.withdrawal_id, r.amount)))
            }
            BridgeOp::Admin(a) => {
                if a.epoch != bs.epoch {
                    return Err("admin op for wrong epoch".into());
                }
                if a.op_nonce != bs.admin_nonce {
                    return Err(format!("admin op nonce {} != expected {} (replay)", a.op_nonce, bs.admin_nonce));
                }
                let need = match a.action {
                    AdminAction::Pause => 1,
                    _ => threshold,
                };
                let n = bridge::count_approvals(&bs.members, &bridge::admin_digest(p.chain_id, a), &a.approvals)?;
                if n < need {
                    return Err(format!("admin op has {} approvals, needs {}", n, need));
                }
                let bsm = self.bridge.as_mut().unwrap();
                match &a.action {
                    AdminAction::Pause => bsm.paused = true,
                    AdminAction::Unpause => bsm.paused = false,
                    AdminAction::Rotate { members, activate_height } => {
                        if *activate_height < height + bp.rotation_timelock_blocks {
                            return Err("rotation violates the timelock".into());
                        }
                        let keys: Vec<Vec<u8>> = members.iter().map(|h| hex::decode(h).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
                        bridge::check_member_set(&keys)?;
                        bsm.pending = Some((keys, *activate_height));
                        bsm.pending_activation = Some(*activate_height);
                    }
                }
                bsm.admin_nonce += 1;
                Ok(None)
            }
        }
    }

    /// Apply a full block body at `height`. Order: maturity, scheduled
    /// committee rotation, transactions, bridge ops, coinbase.
    pub fn apply_block(&mut self, p: &ConsensusParams, b: &Block) -> Result<BlockEffects, String> {
        let height = b.header.height;
        // 1. mature coinbases
        let ready: Vec<(u64, Address)> = self.immature.range(..=(height, [0xff; 32])).map(|(k, _)| *k).collect();
        for k in ready {
            let v = self.immature.remove(&k).unwrap();
            self.credit(&k.1, v)?;
        }
        // 2. scheduled rotation
        if let Some(bs) = self.bridge.as_mut() {
            if let Some((keys, at)) = bs.pending.clone() {
                if at <= height {
                    bs.members = keys;
                    bs.epoch += 1;
                    bs.pending = None;
                    bs.pending_activation = None;
                }
            }
        }
        let mut eff = BlockEffects::default();
        for t in &b.txs {
            if let Some((amt, memo)) = self.apply_tx(p, height, t)? {
                eff.deposits.push((t.txid(), amt, memo));
            }
            eff.fees = eff.fees.checked_add(t.fee).ok_or("fee overflow")?;
        }
        for op in &b.bridge_ops {
            if let Some((wid, amt)) = self.apply_bridge_op(p, height, op)? {
                eff.releases.push((bridge::op_id(op), wid, amt));
            }
        }
        self.apply_coinbase(p, &b.coinbase, height, eff.fees)?;
        Ok(eff)
    }

    pub fn apply_coinbase(&mut self, p: &ConsensusParams, cb: &Coinbase, height: u64, fees: u64) -> Result<(), String> {
        let want = p.block_subsidy.checked_add(fees).ok_or("reward overflow")?;
        if cb.amount != want {
            return Err(format!("coinbase {} != subsidy+fees {}", cb.amount, want));
        }
        if cb.height != height {
            return Err("coinbase height mismatch".into());
        }
        if p.coinbase_maturity == 0 {
            self.credit(&cb.to, cb.amount)?;
        } else {
            let e = self.immature.entry((height + p.coinbase_maturity, cb.to)).or_insert(0);
            *e = e.checked_add(cb.amount).ok_or("immature overflow")?;
        }
        self.supply += cb.amount as u128;
        Ok(())
    }

    /// Canonical state commitment (flat hash over ordered state). Not
    /// proof-friendly; inclusion proofs use tx_root (see bridge RPC).
    pub fn root(&self) -> Hash32 {
        let mut b = Vec::with_capacity(64 + self.accounts.len() * 48);
        // canonical: empty accounts are absent from the commitment
        let live = self.accounts.iter().filter(|(_, a)| a.balance != 0 || a.nonce != 0);
        b.extend_from_slice(&(live.clone().count() as u64).to_le_bytes());
        for (a, acc) in live {
            b.extend_from_slice(a);
            b.extend_from_slice(&acc.balance.to_le_bytes());
            b.extend_from_slice(&acc.nonce.to_le_bytes());
        }
        b.extend_from_slice(b"|immature");
        for ((h, a), v) in &self.immature {
            b.extend_from_slice(&h.to_le_bytes());
            b.extend_from_slice(a);
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(b"|supply");
        b.extend_from_slice(&self.supply.to_le_bytes());
        if let Some(bs) = &self.bridge {
            b.extend_from_slice(b"|bridge");
            b.extend_from_slice(&bs.epoch.to_le_bytes());
            for m in &bs.members {
                b.extend_from_slice(m);
            }
            if let Some((keys, at)) = &bs.pending {
                b.push(1);
                for k in keys {
                    b.extend_from_slice(k);
                }
                b.extend_from_slice(&at.to_le_bytes());
            } else {
                b.push(0);
            }
            b.push(bs.paused as u8);
            b.extend_from_slice(&bs.admin_nonce.to_le_bytes());
            b.extend_from_slice(&bs.locked_total.to_le_bytes());
            b.extend_from_slice(&bs.released_total.to_le_bytes());
            b.extend_from_slice(&bs.deposits.to_le_bytes());
            b.extend_from_slice(&bs.releases.to_le_bytes());
            for w in &bs.used_withdrawals {
                b.extend_from_slice(w);
            }
        }
        sha256(&[b"ZFX-state-v2", &b])
    }

    /// Supply audit: spendable + immature balances equal recorded supply.
    pub fn audit_with(&self, chain_id: u32) -> Result<(), String> {
        self.audit()?;
        if let Some(bs) = &self.bridge {
            let lb = self.account(&bridge::lockbox_address(chain_id)).balance as u128;
            if bs.locked_total < bs.released_total || lb != bs.locked_total - bs.released_total {
                return Err(format!("bridge custody mismatch: lockbox {} != locked {} - released {}", lb, bs.locked_total, bs.released_total));
            }
        }
        Ok(())
    }

    pub fn audit(&self) -> Result<(), String> {
        let spend: u128 = self.accounts.values().map(|a| a.balance as u128).sum();
        let imm: u128 = self.immature.values().map(|v| *v as u128).sum();
        if spend + imm != self.supply {
            return Err(format!("supply mismatch: balances {} + immature {} != supply {}", spend, imm, self.supply));
        }
        Ok(())
    }
}
