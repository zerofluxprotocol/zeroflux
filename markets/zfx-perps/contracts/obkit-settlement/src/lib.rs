//! obkit settlement contract.
//!
//! Custody for an off-chain matched exchange. Users deposit NEP-141 tokens with
//! `ft_transfer_call`; the operator matches off-chain and posts netted balance changes here in
//! sequenced batches. The contract enforces what it can check without seeing orders:
//!
//! - batches apply strictly in order (`id = last + 1`); a sealed batch group nets to zero per token,
//!   so the operator can move value between accounts but never create it;
//! - no balance goes negative;
//! - a withdrawal pays out only with the account's own trading-key signature over
//!   `obkit-withdraw-v1|contract|account|token|amount|nonce`, only to that account, and only once;
//! - any user can file a forced withdrawal on-chain. If the operator doesn't include it within
//!   `force_timeout_ns`, anyone can freeze the contract, after which every user withdraws their last
//!   settled balance directly (`escape_withdraw`), no operator involvement.
//!
//! What stays trusted (as with Orderly): the operator's ordering and matching. The batch records
//! the journal sequence and hash it came from, so a third party holding the journal can replay
//! the engine and check every posted batch.
//!
//! Every state change appends a sequenced event that the operator's adapter consumes
//! (`get_events`), so the off-chain engine sees deposits, key changes, forced withdrawals,
//! reverted withdrawals and batch confirmations in one ordered stream.

use near_sdk::json_types::{Base64VecU8, I128, U64, U128};
use near_sdk::serde_json::json;
use near_sdk::store::{IterableSet, LookupMap};
use near_sdk::{
    AccountId, BorshStorageKey, CurveType, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue, PublicKey, env, near, require,
};

/// Paid when registering a trading key or filing a forced withdrawal; covers storage. Refunded
/// when a key is removed.
pub const KEY_DEPOSIT: NearToken = NearToken::from_millinear(10);
pub const FORCE_DEPOSIT: NearToken = NearToken::from_millinear(10);
pub const MAX_KEYS_PER_ACCOUNT: usize = 16;
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_CALLBACK: Gas = Gas::from_tgas(8);

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
#[allow(clippy::enum_variant_names)]
enum Key {
    Tokens,
    Balances,
    KeyOwner,
    AccountKeys,
    Nonces,
    Events,
    Forced,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Deposit {
        account: AccountId,
        token: AccountId,
        amount: U128,
    },
    KeyAdded {
        account: AccountId,
        public_key: PublicKey,
    },
    KeyRemoved {
        account: AccountId,
        public_key: PublicKey,
    },
    ForcedWithdrawal {
        request_id: U64,
        account: AccountId,
        token: AccountId,
        amount: U128,
    },
    WithdrawalReverted {
        account: AccountId,
        token: AccountId,
        amount: U128,
        reason: String,
    },
    BatchSettled {
        batch_id: U64,
    },
}

#[near(serializers = [json])]
pub struct DeltaArg {
    pub account: AccountId,
    pub token: AccountId,
    pub delta: I128,
}

#[near(serializers = [json])]
pub struct WithdrawArg {
    pub account: AccountId,
    pub token: AccountId,
    pub amount: U128,
    pub nonce: U64,
    pub public_key: PublicKey,
    pub signature: Base64VecU8,
}

#[near(serializers = [json])]
pub struct ForcedArg {
    pub request_id: U64,
    pub account: AccountId,
    /// Ignored when `amount` is 0 (the engine pays nothing for tokens it doesn't list).
    pub token: String,
    pub amount: U128,
}

#[near(serializers = [json])]
pub struct BatchArgs {
    pub id: U64,
    pub entries: Vec<DeltaArg>,
    pub withdrawals: Vec<WithdrawArg>,
    pub forced: Vec<ForcedArg>,
    pub seal: bool,
    pub journal_seq: U64,
    /// Hex blake3 journal hash at `journal_seq`.
    pub journal_hash: String,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct ForcedRequest {
    pub account: AccountId,
    pub token: AccountId,
    pub amount: U128,
    pub created_ns: U64,
    pub done: bool,
}

#[near(serializers = [json])]
pub struct State {
    pub owner: AccountId,
    pub operator: AccountId,
    pub tokens: Vec<AccountId>,
    pub last_batch_id: U64,
    pub last_journal_seq: U64,
    pub last_journal_hash: String,
    pub first_event: U64,
    pub next_event: U64,
    pub next_forced: U64,
    pub forced_cursor: U64,
    pub force_timeout_ns: U64,
    pub frozen: bool,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    operator: AccountId,
    tokens: IterableSet<AccountId>,
    balances: LookupMap<(AccountId, AccountId), u128>,
    key_owner: LookupMap<[u8; 32], AccountId>,
    account_keys: LookupMap<AccountId, Vec<[u8; 32]>>,
    withdraw_nonce: LookupMap<AccountId, u64>,
    events: LookupMap<u64, Event>,
    first_event: u64,
    next_event: u64,
    last_batch_id: u64,
    last_journal_seq: u64,
    last_journal_hash: String,
    /// Running net delta per token across the current (unsealed) batch group.
    imbalance: Vec<(AccountId, i128)>,
    forced: LookupMap<u64, ForcedRequest>,
    next_forced: u64,
    forced_cursor: u64,
    force_timeout_ns: u64,
    frozen: bool,
}

fn ed25519_bytes(pk: &PublicKey) -> [u8; 32] {
    require!(pk.curve_type() == CurveType::ED25519, "ED25519_ONLY");
    pk.as_bytes()[1..].try_into().unwrap()
}

#[near]
impl Contract {
    #[init]
    pub fn new(owner: AccountId, operator: AccountId, force_timeout_ns: U64) -> Self {
        Self {
            owner,
            operator,
            tokens: IterableSet::new(Key::Tokens),
            balances: LookupMap::new(Key::Balances),
            key_owner: LookupMap::new(Key::KeyOwner),
            account_keys: LookupMap::new(Key::AccountKeys),
            withdraw_nonce: LookupMap::new(Key::Nonces),
            events: LookupMap::new(Key::Events),
            first_event: 1,
            next_event: 1,
            last_batch_id: 0,
            last_journal_seq: 0,
            last_journal_hash: String::new(),
            imbalance: vec![],
            forced: LookupMap::new(Key::Forced),
            next_forced: 0,
            forced_cursor: 0,
            force_timeout_ns: force_timeout_ns.0,
            frozen: false,
        }
    }

    // ---------- admin ----------

    pub fn add_token(&mut self, token: AccountId) {
        self.only_owner();
        self.tokens.insert(token);
    }

    pub fn set_operator(&mut self, operator: AccountId) {
        self.only_owner();
        self.operator = operator;
    }

    fn only_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "OWNER_ONLY");
    }

    fn only_operator(&self) {
        require!(env::predecessor_account_id() == self.operator, "OPERATOR_ONLY");
    }

    // ---------- events ----------

    fn emit(&mut self, ev: Event) {
        env::log_str(&format!(
            "EVENT_JSON:{}",
            json!({"standard": "obkit", "version": "1.0.0", "seq": self.next_event, "event": ev})
        ));
        self.events.insert(self.next_event, ev);
        self.next_event += 1;
    }

    /// Drop consumed events to free storage. Operator only; the operator's engine must already
    /// have applied them.
    pub fn prune_events(&mut self, upto: U64) {
        self.only_operator();
        let upto = upto.0.min(self.next_event - 1);
        while self.first_event <= upto {
            self.events.remove(&self.first_event);
            self.first_event += 1;
        }
    }

    // ---------- balances ----------

    fn bal(&self, account: &AccountId, token: &AccountId) -> u128 {
        self.balances.get(&(account.clone(), token.clone())).copied().unwrap_or(0)
    }

    fn set_bal(&mut self, account: &AccountId, token: &AccountId, v: u128) {
        let k = (account.clone(), token.clone());
        if v == 0 {
            self.balances.remove(&k);
        } else {
            self.balances.insert(k, v);
        }
    }

    // ---------- deposits ----------

    /// NEP-141 receiver. `msg` may name a beneficiary account; empty means the sender. Unknown
    /// tokens, a frozen contract or a bad beneficiary refund the full amount.
    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        let token = env::predecessor_account_id();
        if self.frozen || !self.tokens.contains(&token) || amount.0 == 0 {
            return PromiseOrValue::Value(amount);
        }
        let account = if msg.is_empty() {
            sender_id
        } else {
            match msg.parse::<AccountId>() {
                Ok(a) => a,
                Err(_) => return PromiseOrValue::Value(amount),
            }
        };
        let b = self.bal(&account, &token);
        self.set_bal(&account, &token, b.checked_add(amount.0).expect("OVERFLOW"));
        self.emit(Event::Deposit { account, token, amount });
        PromiseOrValue::Value(U128(0))
    }

    // ---------- trading keys ----------

    /// Register an ed25519 trading key for the caller. The key signs orders off-chain and
    /// withdrawals on- and off-chain. It can't move funds anywhere except back to this account.
    #[payable]
    pub fn register_key(&mut self, public_key: PublicKey) {
        require!(env::attached_deposit() >= KEY_DEPOSIT, "KEY_DEPOSIT_REQUIRED");
        let k = ed25519_bytes(&public_key);
        let account = env::predecessor_account_id();
        require!(self.key_owner.get(&k).is_none(), "KEY_TAKEN");
        let mut keys = self.account_keys.get(&account).cloned().unwrap_or_default();
        require!(keys.len() < MAX_KEYS_PER_ACCOUNT, "TOO_MANY_KEYS");
        keys.push(k);
        self.account_keys.insert(account.clone(), keys);
        self.key_owner.insert(k, account.clone());
        self.emit(Event::KeyAdded { account, public_key });
    }

    pub fn remove_key(&mut self, public_key: PublicKey) -> Promise {
        let k = ed25519_bytes(&public_key);
        let account = env::predecessor_account_id();
        require!(self.key_owner.get(&k) == Some(&account), "NOT_YOUR_KEY");
        self.key_owner.remove(&k);
        let mut keys = self.account_keys.get(&account).cloned().unwrap_or_default();
        keys.retain(|x| *x != k);
        self.account_keys.insert(account.clone(), keys);
        self.emit(Event::KeyRemoved {
            account: account.clone(),
            public_key,
        });
        Promise::new(account).transfer(KEY_DEPOSIT)
    }

    // ---------- forced withdrawal and escape hatch ----------

    /// File a withdrawal the operator must process within `force_timeout_ns`. The engine cancels
    /// the account's orders and pays `min(amount, available)`.
    #[payable]
    pub fn request_forced_withdrawal(&mut self, token: AccountId, amount: U128) -> U64 {
        require!(env::attached_deposit() >= FORCE_DEPOSIT, "FORCE_DEPOSIT_REQUIRED");
        require!(!self.frozen, "FROZEN");
        require!(self.tokens.contains(&token), "UNKNOWN_TOKEN");
        require!(amount.0 > 0, "ZERO_AMOUNT");
        let id = self.next_forced;
        self.next_forced += 1;
        let account = env::predecessor_account_id();
        let req = ForcedRequest {
            account: account.clone(),
            token: token.clone(),
            amount,
            created_ns: U64(env::block_timestamp()),
            done: false,
        };
        self.forced.insert(id, req);
        self.emit(Event::ForcedWithdrawal {
            request_id: U64(id),
            account,
            token,
            amount,
        });
        U64(id)
    }

    /// Anyone may freeze the exchange once the oldest open forced withdrawal is overdue.
    pub fn freeze(&mut self) {
        require!(!self.frozen, "FROZEN");
        let oldest = self.forced.get(&self.forced_cursor).filter(|r| !r.done);
        let overdue = oldest.is_some_and(|r| env::block_timestamp() >= r.created_ns.0.saturating_add(self.force_timeout_ns));
        require!(overdue, "NOT_OVERDUE");
        self.frozen = true;
        env::log_str("EVENT_JSON:{\"standard\":\"obkit\",\"version\":\"1.0.0\",\"event\":{\"type\":\"frozen\"}}");
    }

    /// After a freeze: withdraw the caller's whole settled balance of `token`.
    pub fn escape_withdraw(&mut self, token: AccountId) -> Promise {
        require!(self.frozen, "NOT_FROZEN");
        let account = env::predecessor_account_id();
        let amount = self.bal(&account, &token);
        require!(amount > 0, "NOTHING_TO_WITHDRAW");
        self.set_bal(&account, &token, 0);
        self.pay(account, token, amount, "escape")
    }

    // ---------- settlement ----------

    /// Apply one settlement batch. Operator only. Deltas first, then withdrawals, then forced
    /// withdrawals. Invalid withdrawals don't fail the batch: they are skipped and reported as
    /// `withdrawal_reverted` so the engine credits them back.
    pub fn settle_batch(&mut self, batch: BatchArgs) {
        self.only_operator();
        require!(!self.frozen, "FROZEN");
        require!(batch.id.0 == self.last_batch_id + 1, "BAD_BATCH_ID");
        require!(batch.journal_seq.0 >= self.last_journal_seq, "JOURNAL_SEQ_BACKWARDS");

        for e in &batch.entries {
            require!(self.tokens.contains(&e.token), "UNKNOWN_TOKEN");
            let b = self.bal(&e.account, &e.token) as i128;
            let nb = b.checked_add(e.delta.0).expect("OVERFLOW");
            require!(nb >= 0, "NEGATIVE_BALANCE");
            self.set_bal(&e.account, &e.token, nb as u128);
            match self.imbalance.iter_mut().find(|(t, _)| *t == e.token) {
                Some((_, v)) => *v = v.checked_add(e.delta.0).expect("OVERFLOW"),
                None => self.imbalance.push((e.token.clone(), e.delta.0)),
            }
        }
        if batch.seal {
            require!(self.imbalance.iter().all(|(_, v)| *v == 0), "BATCH_GROUP_NOT_ZERO_SUM");
            self.imbalance.clear();
        }

        for w in batch.withdrawals {
            if let Err(reason) = self.check_withdrawal(&w) {
                self.emit(Event::WithdrawalReverted {
                    account: w.account,
                    token: w.token,
                    amount: w.amount,
                    reason: reason.into(),
                });
                continue;
            }
            let b = self.bal(&w.account, &w.token);
            self.set_bal(&w.account, &w.token, b - w.amount.0);
            self.withdraw_nonce.insert(w.account.clone(), w.nonce.0);
            self.pay(w.account, w.token, w.amount.0, "withdraw").detach();
        }

        for f in batch.forced {
            let mut req = self.forced.get(&f.request_id.0).cloned().expect("UNKNOWN_FORCED");
            require!(!req.done, "FORCED_ALREADY_DONE");
            require!(req.account == f.account, "FORCED_ACCOUNT_MISMATCH");
            require!(f.amount.0 <= req.amount.0, "FORCED_OVERPAY");
            req.done = true;
            self.forced.insert(f.request_id.0, req.clone());
            if f.amount.0 > 0 {
                require!(f.token == req.token.as_str(), "FORCED_TOKEN_MISMATCH");
                let b = self.bal(&req.account, &req.token);
                require!(b >= f.amount.0, "NEGATIVE_BALANCE");
                self.set_bal(&req.account, &req.token, b - f.amount.0);
                self.pay(req.account, req.token, f.amount.0, "forced").detach();
            }
        }
        while self.forced.get(&self.forced_cursor).is_some_and(|r| r.done) {
            self.forced_cursor += 1;
        }

        self.last_batch_id = batch.id.0;
        self.last_journal_seq = batch.journal_seq.0;
        self.last_journal_hash = batch.journal_hash;
        self.emit(Event::BatchSettled { batch_id: batch.id });
    }

    fn check_withdrawal(&self, w: &WithdrawArg) -> Result<(), &'static str> {
        if !self.tokens.contains(&w.token) {
            return Err("unknown token");
        }
        if w.public_key.curve_type() != CurveType::ED25519 {
            return Err("not ed25519");
        }
        let k: [u8; 32] = w.public_key.as_bytes()[1..].try_into().unwrap();
        if self.key_owner.get(&k) != Some(&w.account) {
            return Err("key not registered to account");
        }
        if w.nonce.0 <= self.withdraw_nonce.get(&w.account).copied().unwrap_or(0) {
            return Err("nonce reused");
        }
        let Ok(sig) = <[u8; 64]>::try_from(w.signature.0.as_slice()) else {
            return Err("bad signature length");
        };
        let msg = obkit_wire::withdraw_msg(
            env::current_account_id().as_str(),
            w.account.as_str(),
            w.token.as_str(),
            w.amount.0,
            w.nonce.0,
        );
        if !env::ed25519_verify(&sig, &msg, &k) {
            return Err("bad signature");
        }
        if self.bal(&w.account, &w.token) < w.amount.0 {
            return Err("insufficient settled balance");
        }
        Ok(())
    }

    fn pay(&mut self, account: AccountId, token: AccountId, amount: u128, kind: &str) -> Promise {
        Promise::new(token.clone())
            .function_call(
                "ft_transfer",
                json!({"receiver_id": account, "amount": U128(amount), "memo": format!("obkit {kind}")})
                    .to_string()
                    .into_bytes(),
                NearToken::from_yoctonear(1),
                GAS_FT_TRANSFER,
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CALLBACK)
                    .on_payout(account, token, U128(amount)),
            )
    }

    /// Re-credit a payout whose token transfer failed (e.g. receiver not registered with the
    /// token). The engine sees `withdrawal_reverted` and credits its side too.
    #[private]
    pub fn on_payout(&mut self, account: AccountId, token: AccountId, amount: U128) -> bool {
        let ok = env::promise_results_count() == 1 && env::promise_result_checked(0, 64).is_ok();
        if !ok {
            let b = self.bal(&account, &token);
            self.set_bal(&account, &token, b + amount.0);
            if !self.frozen {
                self.emit(Event::WithdrawalReverted {
                    account,
                    token,
                    amount,
                    reason: "token transfer failed".into(),
                });
            }
        }
        ok
    }

    // ---------- views ----------

    pub fn get_balance(&self, account: AccountId, token: AccountId) -> U128 {
        U128(self.bal(&account, &token))
    }

    pub fn get_balances(&self, account: AccountId) -> Vec<(AccountId, U128)> {
        self.tokens
            .iter()
            .map(|t| (t.clone(), U128(self.bal(&account, t))))
            .filter(|(_, b)| b.0 > 0)
            .collect()
    }

    pub fn get_keys(&self, account: AccountId) -> Vec<String> {
        self.account_keys
            .get(&account)
            .map(|ks| {
                ks.iter()
                    .map(|k| format!("ed25519:{}", near_sdk::bs58::encode(k).into_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_withdraw_nonce(&self, account: AccountId) -> U64 {
        U64(self.withdraw_nonce.get(&account).copied().unwrap_or(0))
    }

    /// Events with seq in `[from, from + limit)`, skipping pruned ones.
    pub fn get_events(&self, from: U64, limit: u32) -> Vec<(U64, Event)> {
        let start = from.0.max(self.first_event);
        let end = start.saturating_add(limit.min(200) as u64).min(self.next_event);
        (start..end)
            .filter_map(|s| self.events.get(&s).map(|e| (U64(s), e.clone())))
            .collect()
    }

    pub fn get_forced(&self, request_id: U64) -> Option<ForcedRequest> {
        self.forced.get(&request_id.0).cloned()
    }

    pub fn get_state(&self) -> State {
        State {
            owner: self.owner.clone(),
            operator: self.operator.clone(),
            tokens: self.tokens.iter().cloned().collect(),
            last_batch_id: U64(self.last_batch_id),
            last_journal_seq: U64(self.last_journal_seq),
            last_journal_hash: self.last_journal_hash.clone(),
            first_event: U64(self.first_event),
            next_event: U64(self.next_event),
            next_forced: U64(self.next_forced),
            forced_cursor: U64(self.forced_cursor),
            force_timeout_ns: U64(self.force_timeout_ns),
            frozen: self.frozen,
        }
    }
}

#[cfg(test)]
mod tests;
