//! ZFX on NEAR with optimistic proof-of-work checking.
//!
//! Miners use the full production profile (zfx-fp8c-v0.1, 51.6 M
//! multiply-adds per attempt), which NEAR cannot recompute in one
//! transaction. Instead a block is accepted unless someone proves it wrong:
//!
//! * `submit` (payable, bond): the miner posts the nonce, the four 4096-value
//!   hidden activations and the 256 outputs of the winning attempt. The
//!   contract checks only that the result digest of those outputs meets the
//!   target, stores the trace and appends the block as pending.
//! * `challenge(height, layer, row)`: anyone can point at one value. The
//!   contract regenerates that one weight row (at most 4096 weights) and
//!   recomputes the value from the stored previous layer (layer 0 input is
//!   derived from the challenge and nonce). A mismatch reverts that block
//!   and every pending block built on it; the challenger receives the
//!   fraudulent miner's bond; later miners get their bonds back.
//! * `finalize`: after the challenge window, blocks mint their reward and
//!   return the bond.
//!
//! If every (layer, row) relation holds, the trace is exactly the canonical
//! computation, so one honest watcher suffices to reject any invalid block.
use near_contract_standards::fungible_token::events::FtMint;
use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_pow::difficulty::{next_target, DifficultyParams, SolvetimeRule, LWMA_WINDOW};
use near_pow::hash::sha256;
use near_pow::pow::{self, FP8C_V01};
use near_pow::u256::U256;
use near_sdk::json_types::{Base64VecU8, U128, U64};
use near_sdk::serde_json::{json, Value};
use near_sdk::store::{LazyOption, LookupMap};
use near_sdk::{env, log, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault, Promise, PromiseOrValue};

pub const EVENT_STANDARD: &str = "zfx-mine-opt";
pub const WORK_DOMAIN: &[u8] = b"ZFXO-work-v1";
const H: usize = 4096;
const OUT: usize = 256;
/// Trace layout: a1 | a2 | a3 | a4 (4096 int8 each) | out (256 int32 le).
pub const TRACE_LEN: usize = 4 * H + 4 * OUT;

#[derive(BorshStorageKey)]
#[near]
enum StorageKey {
    Token,
    Metadata,
    Anchors,
    Pending,
    Traces,
}

#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct Pending {
    miner: AccountId,
    nonce: u64,
    challenge: [u8; 32],
    digest: [u8; 32],
    epoch_seed: [u8; 32],
    submitted_ns: u64,
    bond: u128,
    // State before this block, restored if it is reverted.
    prev_hash: [u8; 32],
    target_before: [u8; 32],
    history_before: Vec<(u64, [u8; 32])>,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
    final_height: u64,
    head_height: u64,
    head_hash: [u8; 32],
    target: [u8; 32],
    genesis_target: [u8; 32],
    pow_limit: [u8; 32],
    spacing_secs: u64,
    reward: u128,
    bond: u128,
    window_ns: u64,
    history: Vec<(u64, [u8; 32])>,
    anchors: LookupMap<u64, [u8; 32]>,
    pending: LookupMap<u64, Pending>,
    traces: LookupMap<u64, Vec<u8>>,
    frauds: u64,
}

fn event(name: &str, data: Value) {
    log!("EVENT_JSON:{}", json!({"standard": EVENT_STANDARD, "version": "1.0.0", "event": name, "data": [data]}));
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

fn out_values(trace: &[u8]) -> Vec<i32> {
    trace[4 * H..].chunks(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect()
}

/// Upper bound on the storage one pending block occupies: the 17,408-byte trace, the
/// pending record (including its copy of the difficulty history) and history growth.
const PENDING_STORAGE_BYTES: u128 = 20_000;

#[near]
impl Contract {
    #[init]
    pub fn new(genesis_seed: String, genesis_target: String, spacing_secs: u64, reward: U128, bond: U128, window_secs: u64) -> Self {
        let gt = U256::from_hex(&genesis_target).unwrap_or_else(|| env::panic_str("genesis_target must be 64 hex chars"));
        require!(!gt.is_zero() && spacing_secs > 0 && reward.0 > 0 && bond.0 > 0 && window_secs > 0, "bad parameters");
        // A pending block keeps its trace in contract storage until it finalizes, and the
        // contract pays for that storage out of the bond. A smaller bond lets miners drain the
        // contract's balance until every submit fails with LackBalanceForState.
        let min_bond = env::storage_byte_cost().as_yoctonear() * PENDING_STORAGE_BYTES;
        require!(bond.0 >= min_bond, format!("bond must cover a pending block's storage: at least {} yoctoNEAR", min_bond));
        let md = FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "ZeroFlux (testnet, proof-of-work minted)".to_string(),
            symbol: "ZFX".to_string(),
            icon: None,
            reference: None,
            reference_hash: None,
            decimals: 8,
        };
        let g = sha256(&[b"ZFXO-genesis-v1", genesis_seed.as_bytes()]);
        let mut anchors = LookupMap::new(StorageKey::Anchors);
        anchors.insert(0, g);
        Self {
            token: FungibleToken::new(StorageKey::Token),
            metadata: LazyOption::new(StorageKey::Metadata, Some(md)),
            final_height: 0,
            head_height: 0,
            head_hash: g,
            target: gt.to_be_bytes(),
            genesis_target: gt.to_be_bytes(),
            pow_limit: U256::MAX.shr(1).to_be_bytes(),
            spacing_secs,
            reward: reward.0,
            bond: bond.0,
            window_ns: window_secs * 1_000_000_000,
            history: vec![(env::block_timestamp() / 1_000_000_000, gt.to_be_bytes())],
            anchors,
            pending: LookupMap::new(StorageKey::Pending),
            traces: LookupMap::new(StorageKey::Traces),
            frauds: 0,
        }
    }

    fn seed_at(&self, height: u64) -> [u8; 32] {
        let anchor = *self.anchors.get(&pow::epoch_anchor_height(height)).expect("anchor recorded");
        pow::epoch_seed(&anchor, &FP8C_V01)
    }

    fn work_bytes(&self, miner: &AccountId) -> Vec<u8> {
        let mut b = WORK_DOMAIN.to_vec();
        put_str(&mut b, env::current_account_id().as_str());
        b.extend_from_slice(&(self.head_height + 1).to_le_bytes());
        b.extend_from_slice(&self.head_hash);
        put_str(&mut b, miner.as_str());
        b
    }

    /// Post a block for height head + 1 with its full trace and a bond.
    #[payable]
    pub fn submit(&mut self, height: U64, nonce: U64, trace: Base64VecU8) -> Value {
        let miner = env::predecessor_account_id();
        require!(env::attached_deposit().as_yoctonear() >= self.bond, "attach the bond");
        require!(height.0 == self.head_height + 1, "stale height: another miner found this block");
        let trace = trace.0;
        require!(trace.len() == TRACE_LEN, "trace must be a1|a2|a3|a4|out");
        require!(
            trace[..4 * H].iter().all(|b| {
                let v = *b as i8;
                v != 0 && (-16..=16).contains(&v)
            }),
            "activations must be in the operand domain"
        );
        let challenge = pow::challenge_bytes(&self.work_bytes(&miner));
        let digest = pow::result_digest(&challenge, nonce.0, &out_values(&trace));
        require!(pow::meets_target(&digest, &U256::from_be_bytes(&self.target)), "digest does not meet the target");
        let p = Pending {
            miner: miner.clone(),
            nonce: nonce.0,
            challenge,
            digest,
            epoch_seed: self.seed_at(height.0),
            submitted_ns: env::block_timestamp(),
            bond: env::attached_deposit().as_yoctonear(),
            prev_hash: self.head_hash,
            target_before: self.target,
            history_before: self.history.clone(),
        };
        self.pending.insert(height.0, p);
        self.traces.insert(height.0, trace);
        self.head_height += 1;
        self.head_hash = sha256(&[b"ZFXO-chain-v1", &self.head_hash, &digest, miner.as_bytes()]);
        if self.head_height.is_multiple_of(pow::EPOCH_LEN) {
            self.anchors.insert(self.head_height, self.head_hash);
        }
        self.history.push((env::block_timestamp() / 1_000_000_000, self.target));
        if self.history.len() > LWMA_WINDOW + 1 {
            self.history.remove(0);
        }
        let params = DifficultyParams {
            solvetime_rule: SolvetimeRule::RunningMax,
            target_spacing_secs: self.spacing_secs,
            pow_limit: U256::from_be_bytes(&self.pow_limit),
            fixed: false,
        };
        let hist: Vec<(u64, U256)> = self.history.iter().map(|(t, x)| (*t, U256::from_be_bytes(x))).collect();
        let hist = if self.head_height < LWMA_WINDOW as u64 + 1 { vec![] } else { hist };
        self.target = next_target(&params, &U256::from_be_bytes(&self.genesis_target), &hist).to_be_bytes();
        event("submitted", json!({"height": height, "miner": miner, "digest": hex::encode(digest)}));
        json!({"height": height, "digest": hex::encode(digest), "finalizes_after_ns": U64(env::block_timestamp() + self.window_ns)})
    }

    /// Prove that value `row` of `layer` in block `height` is wrong.
    /// layer 0..=3: activation a(layer+1)[row]; layer 4: out[row].
    pub fn challenge(&mut self, height: U64, layer: u8, row: u32) -> Value {
        let h = height.0;
        require!(h > self.final_height && h <= self.head_height, "block is not pending");
        let p = self.pending.get(&h).cloned().unwrap();
        let trace = self.traces.get(&h).cloned().unwrap();
        let layers = FP8C_V01.layers();
        let l = layer as usize;
        require!(l < layers.len(), "no such layer");
        let (k, n, shift) = layers[l];
        require!((row as usize) < n, "no such row");
        let g0 = env::used_gas();
        let input: Vec<i8> =
            if l == 0 { pow::x0(&pow::attempt_id(&p.challenge, p.nonce), FP8C_V01.d_in) } else { trace[(l - 1) * H..l * H].iter().map(|b| *b as i8).collect() };
        let mut w = vec![0i8; k];
        pow::weight_row_prf(FP8C_V01.prf, &p.epoch_seed, l as u32, row, &mut w);
        let y = pow::dot_i8(&input, &w);
        let (claimed, correct) = match shift {
            Some(s) => (trace[l * H + row as usize] as i8 as i32, pow::requant(y, s) as i32),
            None => (out_values(&trace)[row as usize], y),
        };
        let gas = (env::used_gas().as_gas() - g0.as_gas()) as f64 / 1e12;
        require!(claimed != correct, "no fraud: the value is correct");
        // Fraud: revert this block and everything built on it.
        let challenger = env::predecessor_account_id();
        let mut refunded = Vec::new();
        for hh in (h + 1..=self.head_height).rev() {
            let q = self.pending.remove(&hh).unwrap();
            self.traces.remove(&hh);
            Promise::new(q.miner.clone()).transfer(NearToken::from_yoctonear(q.bond)).detach();
            refunded.push(json!({"height": U64(hh), "miner": q.miner}));
        }
        for a in (h..=self.head_height).filter(|x| x.is_multiple_of(pow::EPOCH_LEN)) {
            self.anchors.remove(&a);
        }
        self.pending.remove(&h);
        self.traces.remove(&h);
        self.head_height = h - 1;
        self.head_hash = p.prev_hash;
        self.target = p.target_before;
        self.history = p.history_before;
        self.frauds += 1;
        Promise::new(challenger.clone()).transfer(NearToken::from_yoctonear(p.bond)).detach();
        event("fraud", json!({"height": height, "layer": layer, "row": row, "miner": p.miner, "challenger": challenger}));
        json!({"fraud": true, "height": height, "claimed": claimed, "correct": correct, "slashed_bond": U128(p.bond), "reverted_descendants": refunded,
               "check_tgas": gas})
    }

    /// Finalize pending blocks whose challenge window has passed (anyone).
    pub fn finalize(&mut self, max: Option<u32>) -> Value {
        let now = env::block_timestamp();
        let mut done = 0u32;
        while done < max.unwrap_or(20) && self.final_height < self.head_height {
            let h = self.final_height + 1;
            let p = self.pending.get(&h).cloned().unwrap();
            if p.submitted_ns + self.window_ns > now {
                break;
            }
            if self.token.accounts.get(&p.miner).is_none() {
                self.token.internal_register_account(&p.miner);
            }
            self.token.internal_deposit(&p.miner, self.reward);
            FtMint { owner_id: &p.miner, amount: U128(self.reward), memo: Some(&format!("zfx-block:{}", h)) }.emit();
            Promise::new(p.miner.clone()).transfer(NearToken::from_yoctonear(p.bond)).detach();
            self.pending.remove(&h);
            self.traces.remove(&h);
            self.final_height = h;
            done += 1;
        }
        json!({"finalized": done, "final_height": U64(self.final_height), "head_height": U64(self.head_height)})
    }

    // ------------------------- views -------------------------

    pub fn work(&self, miner: AccountId) -> Value {
        let wb = self.work_bytes(&miner);
        json!({"height": U64(self.head_height + 1), "profile": FP8C_V01.id, "epoch_seed": hex::encode(self.seed_at(self.head_height + 1)),
               "target": hex::encode(self.target), "work_bytes": hex::encode(&wb), "challenge": hex::encode(pow::challenge_bytes(&wb))})
    }

    pub fn block(&self, height: U64) -> Option<Value> {
        let p = self.pending.get(&height.0)?;
        let t = self.traces.get(&height.0)?;
        Some(json!({"height": height, "miner": p.miner, "nonce": U64(p.nonce), "challenge": hex::encode(p.challenge), "epoch_seed": hex::encode(p.epoch_seed),
                    "digest": hex::encode(p.digest), "submitted_ns": U64(p.submitted_ns), "trace": Base64VecU8(t.clone())}))
    }

    pub fn stats(&self) -> Value {
        json!({"final_height": U64(self.final_height), "head_height": U64(self.head_height), "target": hex::encode(self.target),
               "reward": U128(self.reward), "bond": U128(self.bond), "window_ns": U64(self.window_ns), "frauds": U64(self.frauds),
               "total_supply": self.token.ft_total_supply(), "expected_supply": U128(self.reward * self.final_height as u128),
               "profile": FP8C_V01.name, "macs_per_attempt": U64(FP8C_V01.macs_per_attempt())})
    }
}

#[near]
impl FungibleTokenCore for Contract {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        self.token.ft_transfer(receiver_id, amount, memo)
    }
    #[payable]
    fn ft_transfer_call(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>, msg: String) -> PromiseOrValue<U128> {
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }
    fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }
    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenResolver for Contract {
    #[private]
    fn ft_resolve_transfer(&mut self, sender_id: AccountId, receiver_id: AccountId, amount: U128) -> U128 {
        let (used, _burned) = self.token.internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        used.into()
    }
}

#[near]
impl StorageManagement for Contract {
    #[payable]
    fn storage_deposit(&mut self, account_id: Option<AccountId>, registration_only: Option<bool>) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }
    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }
    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        require!(self.token.ft_balance_of(env::predecessor_account_id()).0 == 0, "transfer your ZFX before unregistering");
        self.token.internal_storage_unregister(force).is_some()
    }
    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }
    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for Contract {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        self.metadata.get().clone().unwrap()
    }
}
