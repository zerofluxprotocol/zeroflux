//! zfx-perps clearing contract.
//!
//! The authoritative ledger of a zfx-perps exchange. Collateral, isolated positions, fees and the
//! insurance fund live here, and every change to them is checked here by the same clearing core
//! (`zfx-perps-clearing`) the operator's engine runs:
//!
//! - **Fills.** The operator submits batches of matched trades. Each fill carries both users'
//!   signed orders; the contract checks the signatures against keys the users registered, the
//!   chain id and contract id in the signed bytes, nonce floor, expiry, market and policy hash,
//!   price limits and the band around the oracle mark, remaining quantity, fee caps, reduce-only
//!   and post-only, then margin, position caps and open interest. An invalid fill is skipped and
//!   reported, never applied.
//! - **Oracle.** Prints must carry signatures from a quorum of allowlisted reporters and move
//!   forward in time. Stale, NOT_REPORTED or disputed prints force CLOSE_ONLY or HALTED.
//! - **Liquidation.** Only positions below maintenance at a fresh mark; the loss waterfall
//!   (position margin, insurance fund, complete pro-rata socialization, then recorded bad debt and
//!   a halt) is finite and enforced here.
//! - **Custody.** Deposits via `ft_transfer_call` of the one allowlisted collateral token. Payouts
//!   only to the account itself, only with its trading-key signature (or its own forced request).
//!   If the operator ignores a forced withdrawal past the timeout, anyone can freeze the contract
//!   and every account withdraws its settled claim at the last mark without the operator.
//!
//! The operator still controls ordering, matching, timing and which valid liquidation it submits.
//! See `docs/ORDERBOOK_AND_CLEARING.md`.

use near_sdk::json_types::{Base64VecU8, U64, U128};
use near_sdk::serde_json::{Value, json};
use near_sdk::store::{IterableMap, LookupMap};
use near_sdk::{
    AccountId, BorshStorageKey, CurveType, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue, PublicKey, env, near, require,
};
use zfx_perps_clearing::batch::{self, Batch};
use zfx_perps_clearing::ledger::{self, CEvent, Now, PayoutKind};
use zfx_perps_clearing::model::*;
use zfx_perps_clearing::store::{Crypto, Store};
use zfx_perps_clearing::wire::{self, Sig64};

pub const KEY_DEPOSIT: NearToken = NearToken::from_millinear(10);
pub const FORCE_DEPOSIT: NearToken = NearToken::from_millinear(10);
pub const MAX_KEYS_PER_ACCOUNT: usize = 16;
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_CALLBACK: Gas = Gas::from_tgas(8);

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum K {
    Markets,
    Accounts,
    Positions,
    Keys,
    AccountKeys,
    Orders,
    Deposits,
    Forced,
    Events,
    Proposals,
}

/// Admin changes that loosen risk wait out `timelock_ns`.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub enum Proposal {
    /// JSON of the new `MarketParams`.
    MarketParams { params: String },
    MarketStatus { market: String, status: String },
    Reporters { reporters: Vec<String>, quorum: u8 },
    FeeWithdrawal { amount: U128 },
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Pending {
    pub proposal: Proposal,
    pub ready_ns: u64,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, Default)]
pub struct Escape {
    pub cursor: u32,
    pub done: bool,
    pub claims: u128,
    pub deficits: u128,
    pub shortfall: u128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    operators: Vec<AccountId>,
    g: Globals,
    market_ids: Vec<String>,
    markets: LookupMap<String, MarketState>,
    accounts: LookupMap<String, AccountState>,
    /// (market, account). Iterable so the escape hatch can value every position.
    positions: IterableMap<(String, String), Position>,
    keys: LookupMap<[u8; 32], KeyInfo>,
    account_keys: LookupMap<String, Vec<[u8; 32]>>,
    orders: LookupMap<[u8; 32], OrderState>,
    deposits: LookupMap<u64, PendingDeposit>,
    next_deposit: u64,
    forced: LookupMap<u64, ForcedRequest>,
    next_forced: u64,
    forced_cursor: u64,
    force_timeout_ns: u64,
    deposit_reclaim_ns: u64,
    timelock_ns: u64,
    proposals: LookupMap<u64, Pending>,
    next_proposal: u64,
    events: LookupMap<u64, String>,
    first_event: u64,
    next_event: u64,
    last_batch_id: u64,
    last_journal_seq: u64,
    last_journal_hash: [u8; 32],
    frozen: bool,
    escape: Escape,
}

struct NearCrypto;

impl Crypto for NearCrypto {
    fn sha256(&self, data: &[u8]) -> [u8; 32] {
        env::sha256_array(data)
    }
    fn ed25519_verify(&self, sig: &[u8; 64], msg: &[u8], pk: &[u8; 32]) -> bool {
        env::ed25519_verify(sig, msg, pk)
    }
}

/// The clearing core's view of contract storage. near-sdk's collections cache reads and writes
/// for the duration of the call and flush once at the end.
struct Ledger<'a>(&'a mut Contract);

impl Store for Ledger<'_> {
    fn globals(&self) -> &Globals {
        &self.0.g
    }
    fn globals_mut(&mut self) -> &mut Globals {
        &mut self.0.g
    }
    fn market(&mut self, id: &str) -> Option<MarketState> {
        self.0.markets.get(id).cloned()
    }
    fn put_market(&mut self, id: &str, m: MarketState) {
        self.0.markets.insert(id.to_string(), m);
    }
    fn account(&mut self, a: &str) -> AccountState {
        self.0.accounts.get(a).cloned().unwrap_or_default()
    }
    fn put_account(&mut self, a: &str, s: AccountState) {
        if s == AccountState::default() {
            self.0.accounts.remove(a);
        } else {
            self.0.accounts.insert(a.to_string(), s);
        }
    }
    fn position(&mut self, a: &str, market: &str) -> Position {
        self.0
            .positions
            .get(&(market.to_string(), a.to_string()))
            .cloned()
            .unwrap_or_default()
    }
    fn put_position(&mut self, a: &str, market: &str, p: Position) {
        let k = (market.to_string(), a.to_string());
        if p.size == 0 && p.margin == 0 {
            self.0.positions.remove(&k);
        } else {
            self.0.positions.insert(k, p);
        }
    }
    fn key(&mut self, pk: &[u8; 32]) -> Option<KeyInfo> {
        self.0.keys.get(pk).cloned()
    }
    fn order(&mut self, h: &[u8; 32]) -> Option<OrderState> {
        self.0.orders.get(h).cloned()
    }
    fn put_order(&mut self, h: &[u8; 32], o: OrderState) {
        self.0.orders.insert(*h, o);
    }
    fn deposit(&mut self, id: u64) -> Option<PendingDeposit> {
        self.0.deposits.get(&id).cloned()
    }
    fn remove_deposit(&mut self, id: u64) {
        self.0.deposits.remove(&id);
    }
    fn forced(&mut self, id: u64) -> Option<ForcedRequest> {
        self.0.forced.get(&id).cloned()
    }
    fn put_forced(&mut self, id: u64, r: ForcedRequest) {
        self.0.forced.insert(id, r);
    }
}

fn ed25519_bytes(pk: &PublicKey) -> [u8; 32] {
    require!(pk.curve_type() == CurveType::ED25519, "ED25519_ONLY");
    pk.as_bytes()[1..].try_into().unwrap()
}

fn pk_str(k: &[u8; 32]) -> String {
    format!("ed25519:{}", near_sdk::bs58::encode(k).into_string())
}

fn parse_pk(s: &str) -> [u8; 32] {
    let pk: PublicKey = s.parse().unwrap_or_else(|_| env::panic_str("BAD_PUBLIC_KEY"));
    ed25519_bytes(&pk)
}

fn parse_hash(s: &str) -> [u8; 32] {
    wire::parse_hex32(s).unwrap_or_else(|| env::panic_str("BAD_HASH"))
}

fn sig64(b: &[u8]) -> [u8; 64] {
    b.try_into().unwrap_or_else(|_| env::panic_str("BAD_SIGNATURE_LENGTH"))
}

fn status_from(s: &str) -> MarketStatus {
    match s {
        "PROPOSED" => MarketStatus::Proposed,
        "ACTIVE" => MarketStatus::Active,
        "CLOSE_ONLY" => MarketStatus::CloseOnly,
        "HALTED" => MarketStatus::Halted,
        "SETTLING" => MarketStatus::Settling,
        "CLOSED" => MarketStatus::Closed,
        _ => env::panic_str("BAD_STATUS"),
    }
}

fn params_from(v: &Value) -> MarketParams {
    near_sdk::serde_json::from_value(v.clone()).unwrap_or_else(|e| env::panic_str(&format!("BAD_PARAMS: {e}")))
}

fn market_json(m: &MarketState, now: u64, grace: u64) -> Value {
    let p = &m.params;
    json!({
        "market_id": p.market_id, "params": p, "policy_hash": wire::hex32(&m.policy_hash),
        "status": m.status.name(), "effective_status": ledger::effective_status(m, Now::at(now), grace).name(),
        "mark": m.mark.to_string(), "mark_fresh": ledger::mark_fresh(m, Now::at(now), grace),
        "open_interest": m.open_interest.to_string(), "cum_funding": m.cum_funding.to_string(),
        "last_funding_ns": m.last_funding_ns.to_string(), "last_funding_rate_ppm": m.last_funding_rate_ppm,
        "fund_px_qty": m.fund_px_qty.to_string(), "fund_qty": m.fund_qty.to_string(), "bad_debt": m.bad_debt.to_string(),
        "print": m.print.as_ref().map(|x| json!({
            "hash": wire::hex32(&x.hash), "value": x.value.to_string(), "status": x.status.name(), "data_status": x.data_status.name(),
            "source_start_ms": x.source_start_ms.to_string(), "source_end_ms": x.source_end_ms.to_string(),
            "published_at_ms": x.published_at_ms.to_string(), "valid_until_ms": x.valid_until_ms.to_string(), "revision": x.revision,
        })),
    })
}

#[near]
impl Contract {
    /// `chain_id` is part of every signed message (e.g. `near:testnet`).
    #[init]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: AccountId,
        operator: AccountId,
        chain_id: String,
        collateral: AccountId,
        insurance_account: AccountId,
        reporters: Vec<String>,
        quorum: u8,
        force_timeout_ns: U64,
        timelock_ns: U64,
        settle_grace_ns: Option<U64>,
    ) -> Self {
        require!(wire::valid_chain_id(&chain_id), "BAD_CHAIN_ID");
        require!(quorum as usize <= reporters.len() && quorum > 0, "BAD_QUORUM");
        Self {
            owner,
            operators: vec![operator],
            g: Globals {
                chain_id,
                contract_id: env::current_account_id().to_string(),
                collateral: collateral.to_string(),
                insurance_account: insurance_account.to_string(),
                fee_balance: 0,
                reporters: reporters.iter().map(|s| parse_pk(s)).collect(),
                quorum,
                last_op_ns: 0,
                settle_grace_ns: settle_grace_ns.map_or(batch::DEFAULT_SETTLE_GRACE_NS, |x| x.0),
                max_order_ttl_ns: 30 * NS_PER_DAY,
                total_collateral: 0,
            },
            market_ids: vec![],
            markets: LookupMap::new(K::Markets),
            accounts: LookupMap::new(K::Accounts),
            positions: IterableMap::new(K::Positions),
            keys: LookupMap::new(K::Keys),
            account_keys: LookupMap::new(K::AccountKeys),
            orders: LookupMap::new(K::Orders),
            deposits: LookupMap::new(K::Deposits),
            next_deposit: 1,
            forced: LookupMap::new(K::Forced),
            next_forced: 0,
            forced_cursor: 0,
            force_timeout_ns: force_timeout_ns.0,
            deposit_reclaim_ns: 3_600 * NS_PER_SEC,
            timelock_ns: timelock_ns.0,
            proposals: LookupMap::new(K::Proposals),
            next_proposal: 1,
            events: LookupMap::new(K::Events),
            first_event: 1,
            next_event: 1,
            last_batch_id: 0,
            last_journal_seq: 0,
            last_journal_hash: [0; 32],
            frozen: false,
            escape: Escape::default(),
        }
    }

    // ---------------- events ----------------

    /// Every state change outside a batch, and each batch's outcome, is a numbered event the
    /// operator's adapter forwards to the engine in order.
    fn emit(&mut self, ev: Value) {
        let s = ev.to_string();
        env::log_str(&format!(
            "EVENT_JSON:{{\"standard\":\"zfx-perps-clearing\",\"version\":\"1.0.0\",\"seq\":{},\"event\":{s}}}",
            self.next_event
        ));
        self.events.insert(self.next_event, s);
        self.next_event += 1;
    }

    pub fn prune_events(&mut self, upto: U64) {
        self.only_operator();
        let upto = upto.0.min(self.next_event - 1);
        while self.first_event <= upto {
            self.events.remove(&self.first_event);
            self.first_event += 1;
        }
    }

    // ---------------- roles ----------------

    fn only_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "OWNER_ONLY");
    }

    fn only_operator(&self) {
        require!(self.operators.contains(&env::predecessor_account_id()), "OPERATOR_ONLY");
    }

    /// Replace the operator set (key rotation). Immediate: operators can order and match but
    /// can't move funds or forge fills, so rotating them is a liveness decision.
    pub fn set_operators(&mut self, operators: Vec<AccountId>) {
        self.only_owner();
        require!(!operators.is_empty(), "NO_OPERATORS");
        self.operators = operators;
    }

    // ---------------- markets and admin ----------------

    /// List a market (PROPOSED). Activation is timelocked like any loosening.
    pub fn create_market(&mut self, params: Value) {
        self.only_owner();
        let p = params_from(&params);
        require!(!self.markets.contains_key(&p.market_id), "MARKET_EXISTS");
        let m = ledger::new_market(&NearCrypto, p, env::block_timestamp()).unwrap_or_else(|e| env::panic_str(e));
        let id = m.params.market_id.clone();
        self.market_ids.push(id.clone());
        self.markets.insert(id, m.clone());
        self.emit_market(&m);
    }

    fn emit_market(&mut self, m: &MarketState) {
        self.emit(json!({"type": "market_updated", "market": m.params.market_id, "params": m.params,
            "policy_hash": wire::hex32(&m.policy_hash), "status": m.status.name(), "created_ns": m.last_funding_ns.to_string()}));
    }

    fn market_mut(&mut self, id: &str) -> MarketState {
        self.markets.get(id).cloned().unwrap_or_else(|| env::panic_str("UNKNOWN_MARKET"))
    }

    /// New parameters. Applied immediately if they only tighten risk, otherwise queued behind the
    /// timelock. Either way the policy hash changes, so every resting order must be re-signed.
    pub fn update_market(&mut self, params: Value) -> Option<U64> {
        self.only_owner();
        let p = params_from(&params);
        let cur = self.market_mut(&p.market_id);
        ledger::validate_params(&p).unwrap_or_else(|e| env::panic_str(e));
        if ledger::is_tightening(&cur.params, &p) {
            self.apply_params(p);
            None
        } else {
            Some(self.propose(Proposal::MarketParams { params: params.to_string() }))
        }
    }

    fn apply_params(&mut self, p: MarketParams) {
        let mut m = self.market_mut(&p.market_id);
        m.policy_hash = ledger::policy_hash(&NearCrypto, &p);
        m.params = p;
        self.markets.insert(m.params.market_id.clone(), m.clone());
        self.emit_market(&m);
    }

    /// Tightening transitions (toward HALTED/CLOSED) are immediate; reopening is timelocked.
    pub fn set_market_status(&mut self, market: String, status: String) -> Option<U64> {
        self.only_owner();
        let next = status_from(&status);
        let cur = self.market_mut(&market);
        let tighten = next > cur.status && cur.status != MarketStatus::Proposed;
        if tighten {
            self.apply_status(&market, next);
            None
        } else {
            Some(self.propose(Proposal::MarketStatus { market, status }))
        }
    }

    fn apply_status(&mut self, market: &str, s: MarketStatus) {
        let mut m = self.market_mut(market);
        m.status = s;
        self.markets.insert(market.to_string(), m.clone());
        self.emit_market(&m);
    }

    /// Remove a reporter immediately (compromise response). Prints then need a quorum of the rest;
    /// if none is possible, markets go stale and fail closed.
    pub fn remove_reporter(&mut self, public_key: String) {
        self.only_owner();
        let k = parse_pk(&public_key);
        self.g.reporters.retain(|x| *x != k);
        self.g.quorum = self.g.quorum.min(self.g.reporters.len() as u8).max(1);
        self.emit_reporters();
    }

    fn emit_reporters(&mut self) {
        let r: Vec<String> = self.g.reporters.iter().map(pk_str).collect();
        let q = self.g.quorum;
        self.emit(json!({"type": "reporters_updated", "reporters": r, "quorum": q}));
    }

    pub fn propose_reporters(&mut self, reporters: Vec<String>, quorum: u8) -> U64 {
        self.only_owner();
        require!(quorum > 0 && quorum as usize <= reporters.len(), "BAD_QUORUM");
        self.propose(Proposal::Reporters { reporters, quorum })
    }

    pub fn propose_fee_withdrawal(&mut self, amount: U128) -> U64 {
        self.only_owner();
        self.propose(Proposal::FeeWithdrawal { amount })
    }

    fn propose(&mut self, p: Proposal) -> U64 {
        let id = self.next_proposal;
        self.next_proposal += 1;
        let ready = env::block_timestamp() + self.timelock_ns;
        self.proposals.insert(id, Pending { proposal: p.clone(), ready_ns: ready });
        self.emit(json!({"type": "proposal", "id": id.to_string(), "proposal": p, "ready_ns": ready.to_string()}));
        U64(id)
    }

    pub fn cancel_proposal(&mut self, id: U64) {
        self.only_owner();
        self.proposals.remove(&id.0);
    }

    /// Execute a proposal whose timelock has passed. Owner only.
    pub fn execute_proposal(&mut self, id: U64) -> PromiseOrValue<()> {
        self.only_owner();
        let p = self.proposals.remove(&id.0).unwrap_or_else(|| env::panic_str("UNKNOWN_PROPOSAL"));
        require!(env::block_timestamp() >= p.ready_ns, "TIMELOCKED");
        match p.proposal {
            Proposal::MarketParams { params } => {
                let v: Value = near_sdk::serde_json::from_str(&params).unwrap();
                let p = params_from(&v);
                ledger::validate_params(&p).unwrap_or_else(|e| env::panic_str(e));
                self.apply_params(p);
            }
            Proposal::MarketStatus { market, status } => self.apply_status(&market, status_from(&status)),
            Proposal::Reporters { reporters, quorum } => {
                self.g.reporters = reporters.iter().map(|s| parse_pk(s)).collect();
                self.g.quorum = quorum;
                self.emit_reporters();
            }
            Proposal::FeeWithdrawal { amount } => {
                require!(!self.frozen, "FROZEN");
                require!(self.g.fee_balance >= amount.0, "INSUFFICIENT_FEES");
                self.g.fee_balance -= amount.0;
                self.g.total_collateral -= amount.0;
                return PromiseOrValue::Promise(self.pay(self.owner.clone(), amount.0, "fees", false));
            }
        }
        PromiseOrValue::Value(())
    }

    // ---------------- deposits ----------------

    /// NEP-141 receiver. Only the allowlisted collateral token is accepted; anything else, a
    /// frozen contract or a bad beneficiary is refunded in full. `msg` may name a beneficiary.
    /// The deposit is pending until the operator's next batch credits it (so the engine and the
    /// chain apply it at the same point in the sequence).
    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        let token = env::predecessor_account_id();
        if self.frozen || token.as_str() != self.g.collateral || amount.0 == 0 {
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
        if !wire::valid_account(account.as_str()) {
            return PromiseOrValue::Value(amount);
        }
        let id = self.next_deposit;
        self.next_deposit += 1;
        self.deposits.insert(
            id,
            PendingDeposit {
                account: account.to_string(),
                amount: amount.0,
                created_ns: env::block_timestamp(),
            },
        );
        self.emit(json!({"type": "deposit", "deposit_id": id.to_string(), "account": account, "amount": amount}));
        PromiseOrValue::Value(U128(0))
    }

    /// Take back a deposit the operator never credited (after `deposit_reclaim_ns`, or once frozen).
    pub fn reclaim_deposit(&mut self, deposit_id: U64) -> Promise {
        let d = self.deposits.get(&deposit_id.0).cloned().unwrap_or_else(|| env::panic_str("UNKNOWN_DEPOSIT"));
        let me = env::predecessor_account_id();
        require!(me.as_str() == d.account, "NOT_YOUR_DEPOSIT");
        require!(
            self.frozen || env::block_timestamp() >= d.created_ns + self.deposit_reclaim_ns,
            "TOO_EARLY"
        );
        self.deposits.remove(&deposit_id.0);
        self.emit(json!({"type": "deposit_reclaimed", "deposit_id": deposit_id}));
        // Pending deposits are not part of total_collateral, so a failed refund simply restores it.
        Promise::new(self.g.collateral.parse().unwrap()).function_call(
            "ft_transfer",
            json!({"receiver_id": me, "amount": U128(d.amount), "memo": "zfx-perps reclaim"}).to_string().into_bytes(),
            NearToken::from_yoctonear(1),
            GAS_FT_TRANSFER,
        )
    }

    // ---------------- trading keys ----------------

    /// Register an ed25519 trading key for the caller (a wallet transaction). `expires_ns` (0 = no
    /// expiry) bounds the session; `can_withdraw = false` makes a trading-only key.
    #[payable]
    pub fn register_key(&mut self, public_key: PublicKey, expires_ns: Option<U64>, can_withdraw: Option<bool>) {
        require!(env::attached_deposit() >= KEY_DEPOSIT, "KEY_DEPOSIT_REQUIRED");
        let k = ed25519_bytes(&public_key);
        let account = env::predecessor_account_id().to_string();
        require!(!self.keys.contains_key(&k), "KEY_TAKEN");
        let mut ks = self.account_keys.get(&account).cloned().unwrap_or_default();
        require!(ks.len() < MAX_KEYS_PER_ACCOUNT, "TOO_MANY_KEYS");
        ks.push(k);
        self.account_keys.insert(account.clone(), ks);
        let info = KeyInfo {
            account: account.clone(),
            expires_ns: expires_ns.map_or(0, |x| x.0),
            can_withdraw: can_withdraw.unwrap_or(true),
        };
        self.keys.insert(k, info.clone());
        self.emit(json!({"type": "key_added", "account": account, "public_key": pk_str(&k),
            "expires_ns": info.expires_ns.to_string(), "can_withdraw": info.can_withdraw}));
    }

    /// Revoke a key. Fills of orders it signed that land after this are rejected.
    pub fn remove_key(&mut self, public_key: PublicKey) -> Promise {
        let k = ed25519_bytes(&public_key);
        let account = env::predecessor_account_id();
        require!(
            self.keys.get(&k).is_some_and(|i| i.account == account.as_str()),
            "NOT_YOUR_KEY"
        );
        self.keys.remove(&k);
        let mut ks = self.account_keys.get(account.as_str()).cloned().unwrap_or_default();
        ks.retain(|x| *x != k);
        self.account_keys.insert(account.to_string(), ks);
        self.emit(json!({"type": "key_removed", "account": account, "public_key": pk_str(&k)}));
        Promise::new(account).transfer(KEY_DEPOSIT)
    }

    // ---------------- cancels and nonce floor ----------------

    /// Cancel one of the caller's orders on-chain (wallet transaction). Final for every relayer.
    pub fn cancel_order(&mut self, order_hash: String, expiry_ns: U64) {
        let a = env::predecessor_account_id().to_string();
        let h = parse_hash(&order_hash);
        ledger::cancel(&mut Ledger(self), &NearCrypto, &a, &h, expiry_ns.0, None).unwrap_or_else(|f| env::panic_str(&format!("{:?}", f.reason)));
        self.emit(json!({"type": "order_canceled", "account": a, "order_hash": order_hash, "expiry_ns": expiry_ns}));
    }

    /// Relay a cancel signed by one of the account's trading keys. Anyone may submit it.
    pub fn submit_cancel(&mut self, account: String, order_hash: String, expiry_ns: U64, public_key: String, signature: Base64VecU8) {
        let (h, pk, sig) = (parse_hash(&order_hash), parse_pk(&public_key), sig64(&signature.0));
        ledger::cancel(&mut Ledger(self), &NearCrypto, &account, &h, expiry_ns.0, Some((&pk, &sig)))
            .unwrap_or_else(|f| env::panic_str(&format!("{:?}", f.reason)));
        self.emit(json!({"type": "order_canceled", "account": account, "order_hash": order_hash, "expiry_ns": expiry_ns}));
    }

    /// Raise the caller's nonce floor: all orders with a smaller nonce become unfillable.
    pub fn set_min_nonce(&mut self, min_nonce: U64) {
        let a = env::predecessor_account_id().to_string();
        ledger::set_min_nonce(&mut Ledger(self), &NearCrypto, &a, min_nonce.0, None).unwrap_or_else(|f| env::panic_str(&format!("{:?}", f.reason)));
        self.emit(json!({"type": "min_nonce", "account": a, "min_nonce": min_nonce}));
    }

    /// Delete records of orders that expired (plus the settlement grace). Anyone may call.
    pub fn prune_orders(&mut self, order_hashes: Vec<String>) -> u32 {
        let now = env::block_timestamp();
        let mut n = 0;
        for h in order_hashes {
            let h = parse_hash(&h);
            if self
                .orders
                .get(&h)
                .is_some_and(|o| now >= o.expiry_ns.saturating_add(self.g.settle_grace_ns))
            {
                self.orders.remove(&h);
                n += 1;
            }
        }
        n
    }

    // ---------------- forced withdrawal and escape ----------------

    /// File a withdrawal of free collateral the operator must include within `force_timeout_ns`.
    #[payable]
    pub fn request_forced_withdrawal(&mut self, amount: U128) -> U64 {
        require!(env::attached_deposit() >= FORCE_DEPOSIT, "FORCE_DEPOSIT_REQUIRED");
        require!(!self.frozen, "FROZEN");
        require!(amount.0 > 0, "ZERO_AMOUNT");
        let id = self.next_forced;
        self.next_forced += 1;
        let account = env::predecessor_account_id().to_string();
        self.forced.insert(
            id,
            ForcedRequest {
                account: account.clone(),
                amount: amount.0,
                created_ns: env::block_timestamp(),
                done: false,
                paid: 0,
            },
        );
        self.emit(json!({"type": "forced_withdrawal", "request_id": id.to_string(), "account": account, "amount": amount}));
        U64(id)
    }

    /// Anyone may freeze once the oldest open forced withdrawal is overdue. Trading stops; every
    /// position is valued at its market's last mark.
    pub fn freeze(&mut self) {
        require!(!self.frozen, "FROZEN");
        let overdue = self
            .forced
            .get(&self.forced_cursor)
            .filter(|r| !r.done)
            .is_some_and(|r| env::block_timestamp() >= r.created_ns.saturating_add(self.force_timeout_ns));
        require!(overdue, "NOT_OVERDUE");
        self.frozen = true;
        self.emit(json!({"type": "frozen"}));
    }

    /// Step 1 after a freeze: value positions in pages (anyone may call until done). Deficits are
    /// covered by the insurance fund; any remaining shortfall is spread pro rata over positive
    /// position claims.
    pub fn escape_scan(&mut self, limit: u32) -> bool {
        require!(self.frozen, "NOT_FROZEN");
        require!(!self.escape.done, "SCAN_DONE");
        let start = self.escape.cursor as usize;
        let rows: Vec<((String, String), Position)> = self.positions.iter().skip(start).take(limit as usize).map(|(k, v)| (k.clone(), v.clone())).collect();
        for ((market, _), p) in &rows {
            let m = self.markets.get(market).cloned().unwrap();
            let eq = ledger::equity(&m, p, m.mark);
            if eq >= 0 {
                self.escape.claims += eq as u128;
            } else {
                self.escape.deficits += (-eq) as u128;
            }
        }
        self.escape.cursor += rows.len() as u32;
        if self.escape.cursor as usize >= self.positions.len() as usize {
            let ins = self.g.insurance_account.clone();
            let mut a = self.accounts.get(&ins).cloned().unwrap_or_default();
            let covered = self.escape.deficits.min(a.free);
            a.free -= covered;
            self.accounts.insert(ins, a);
            self.escape.shortfall = self.escape.deficits - covered;
            self.escape.done = true;
        }
        self.escape.done
    }

    /// Step 2: the caller withdraws free collateral plus its positions' claims (after any haircut).
    pub fn escape_withdraw(&mut self) -> Promise {
        require!(self.frozen && self.escape.done, "SCAN_NOT_DONE");
        let me = env::predecessor_account_id();
        let a = me.to_string();
        let mut total = self.accounts.get(&a).map_or(0, |x| x.free);
        self.accounts.remove(&a);
        let e = self.escape.clone();
        for id in self.market_ids.clone() {
            let k = (id.clone(), a.clone());
            if let Some(p) = self.positions.remove(&k) {
                let m = self.markets.get(&id).cloned().unwrap();
                let eq = ledger::equity(&m, &p, m.mark);
                if eq > 0 && e.claims > 0 {
                    total += (eq as u128) * (e.claims - e.shortfall.min(e.claims)) / e.claims;
                }
            }
        }
        require!(total > 0, "NOTHING_TO_WITHDRAW");
        self.g.total_collateral = self.g.total_collateral.saturating_sub(total);
        self.pay(me, total, "escape", false)
    }

    // ---------------- settlement ----------------

    /// Apply one batch (borsh-encoded `zfx_perps_clearing::Batch`). Operator only. Ops apply in
    /// order; an op that fails a check is skipped and listed in the `batch_settled` event.
    pub fn settle_batch(&mut self, #[serializer(borsh)] batch: Batch) {
        self.only_operator();
        require!(!self.frozen, "FROZEN");
        require!(batch.id == self.last_batch_id + 1, "BAD_BATCH_ID");
        require!(batch.journal_seq >= self.last_journal_seq, "JOURNAL_SEQ_BACKWARDS");
        let block = env::block_timestamp();
        let results = batch::apply_batch(&mut Ledger(self), &NearCrypto, &batch, Some(block), false);
        let mut rejected = vec![];
        let mut payouts = vec![];
        for r in &results {
            match &r.result {
                Ok(evs) => {
                    for e in evs {
                        if let CEvent::Payout { account, amount, kind } = e {
                            payouts.push((account.clone(), *amount, *kind));
                        }
                    }
                }
                Err(f) => rejected.push(json!([r.index, f.reason])),
            }
        }
        for (account, amount, kind) in payouts {
            let to: AccountId = account.parse().unwrap_or_else(|_| env::panic_str("BAD_ACCOUNT"));
            let k = if kind == PayoutKind::Withdraw { "withdraw" } else { "forced" };
            self.pay(to, amount, k, true).detach();
        }
        while self.forced.get(&self.forced_cursor).is_some_and(|r| r.done) {
            self.forced_cursor += 1;
        }
        self.last_batch_id = batch.id;
        self.last_journal_seq = batch.journal_seq;
        self.last_journal_hash = batch.journal_hash;
        let n = results.len();
        self.emit(json!({"type": "batch_settled", "batch_id": batch.id.to_string(), "block_ns": block.to_string(),
            "ops": n, "rejected": rejected}));
    }

    fn pay(&mut self, account: AccountId, amount: u128, kind: &str, recredit: bool) -> Promise {
        Promise::new(self.g.collateral.parse().unwrap())
            .function_call(
                "ft_transfer",
                json!({"receiver_id": account, "amount": U128(amount), "memo": format!("zfx-perps {kind}")})
                    .to_string()
                    .into_bytes(),
                NearToken::from_yoctonear(1),
                GAS_FT_TRANSFER,
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CALLBACK)
                    .on_payout(account, U128(amount), recredit),
            )
    }

    /// A payout whose token transfer failed (e.g. the receiver isn't registered with the token)
    /// goes back to the account's free collateral. The engine sees `withdrawal_reverted`.
    #[private]
    pub fn on_payout(&mut self, account: AccountId, amount: U128, recredit: bool) -> bool {
        let ok = env::promise_results_count() == 1 && env::promise_result_checked(0, 64).is_ok();
        if !ok {
            let mut a = self.accounts.get(account.as_str()).cloned().unwrap_or_default();
            a.free += amount.0;
            self.accounts.insert(account.to_string(), a);
            self.g.total_collateral += amount.0;
            if recredit && !self.frozen {
                self.emit(json!({"type": "withdrawal_reverted", "account": account, "amount": amount}));
            }
        }
        ok
    }

    // ---------------- views ----------------

    pub fn get_state(&self) -> Value {
        json!({
            "owner": self.owner, "operators": self.operators, "chain_id": self.g.chain_id, "contract_id": self.g.contract_id,
            "collateral": self.g.collateral, "insurance_account": self.g.insurance_account,
            "fee_balance": self.g.fee_balance.to_string(), "total_collateral": self.g.total_collateral.to_string(),
            "reporters": self.g.reporters.iter().map(pk_str).collect::<Vec<_>>(), "quorum": self.g.quorum,
            "last_op_ns": self.g.last_op_ns.to_string(), "settle_grace_ns": self.g.settle_grace_ns.to_string(),
            "max_order_ttl_ns": self.g.max_order_ttl_ns.to_string(),
            "markets": self.market_ids, "last_batch_id": self.last_batch_id.to_string(),
            "last_journal_seq": self.last_journal_seq.to_string(), "last_journal_hash": wire::hex32(&self.last_journal_hash),
            "first_event": self.first_event.to_string(), "next_event": self.next_event.to_string(),
            "next_deposit": self.next_deposit.to_string(), "next_forced": self.next_forced.to_string(),
            "forced_cursor": self.forced_cursor.to_string(), "force_timeout_ns": self.force_timeout_ns.to_string(),
            "timelock_ns": self.timelock_ns.to_string(), "frozen": self.frozen, "positions": self.positions.len(),
            "escape": {"cursor": self.escape.cursor, "done": self.escape.done, "claims": self.escape.claims.to_string(),
                "deficits": self.escape.deficits.to_string(), "shortfall": self.escape.shortfall.to_string()},
        })
    }

    pub fn get_market(&self, market: String) -> Option<Value> {
        self.markets
            .get(&market)
            .map(|m| market_json(m, env::block_timestamp(), self.g.settle_grace_ns))
    }

    pub fn get_markets(&self) -> Vec<Value> {
        self.market_ids
            .iter()
            .filter_map(|id| self.markets.get(id))
            .map(|m| market_json(m, env::block_timestamp(), self.g.settle_grace_ns))
            .collect()
    }

    /// Free collateral, nonces and every isolated position with its equity and margin at the mark.
    pub fn get_account(&self, account: String) -> Value {
        let a = self.accounts.get(&account).cloned().unwrap_or_default();
        let mut positions = vec![];
        for id in &self.market_ids {
            if let Some(p) = self.positions.get(&(id.clone(), account.clone())) {
                let m = self.markets.get(id).unwrap();
                positions.push(json!({
                    "market": id, "size": p.size.to_string(), "margin": p.margin.to_string(),
                    "funding_index": p.funding_index.to_string(), "funding_owed": ledger::funding_owed(m, p).to_string(),
                    "equity": ledger::equity(m, p, m.mark).to_string(), "mark": m.mark.to_string(),
                    "initial_margin": ledger::initial_margin(m, p.size.unsigned_abs(), m.mark).to_string(),
                    "maintenance_margin": ledger::maintenance_margin(m, p.size.unsigned_abs(), m.mark).to_string(),
                    "liquidation_price": ledger::liquidation_price(m, p).map(|x| x.to_string()),
                }));
            }
        }
        json!({"account": account, "free": a.free.to_string(), "min_nonce": a.min_nonce.to_string(),
            "action_nonce": a.action_nonce.to_string(), "positions": positions})
    }

    pub fn get_order(&self, order_hash: String) -> Option<Value> {
        self.orders.get(&parse_hash(&order_hash)).map(|o| {
            json!({"account": o.account, "filled": o.filled.to_string(), "canceled": o.canceled, "expiry_ns": o.expiry_ns.to_string()})
        })
    }

    pub fn get_keys(&self, account: String) -> Vec<Value> {
        self.account_keys
            .get(&account)
            .map(|ks| {
                ks.iter()
                    .filter_map(|k| self.keys.get(k).map(|i| (k, i)))
                    .map(|(k, i)| json!({"public_key": pk_str(k), "expires_ns": i.expires_ns.to_string(), "can_withdraw": i.can_withdraw}))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Events with seq in `[from, from + limit)`, as `[seq, event]`.
    pub fn get_events(&self, from: U64, limit: u32) -> Vec<(U64, Value)> {
        let start = from.0.max(self.first_event);
        let end = start.saturating_add(limit.min(200) as u64).min(self.next_event);
        (start..end)
            .filter_map(|s| self.events.get(&s).map(|e| (U64(s), near_sdk::serde_json::from_str(e).unwrap())))
            .collect()
    }

    pub fn get_deposit(&self, deposit_id: U64) -> Option<Value> {
        self.deposits
            .get(&deposit_id.0)
            .map(|d| json!({"account": d.account, "amount": d.amount.to_string(), "created_ns": d.created_ns.to_string()}))
    }

    pub fn get_forced(&self, request_id: U64) -> Option<Value> {
        self.forced.get(&request_id.0).map(|r| {
            json!({"account": r.account, "amount": r.amount.to_string(), "created_ns": r.created_ns.to_string(), "done": r.done, "paid": r.paid.to_string()})
        })
    }
}

/// Signature newtype re-export so tests can build ops.
pub type Signature = Sig64;

#[cfg(test)]
mod tests;
