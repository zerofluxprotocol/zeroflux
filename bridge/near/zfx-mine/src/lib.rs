//! ZeroFlux mining on NEAR (option C): ZFX is a NEP-141 token on NEAR and
//! the only way to mint it is a valid ZeroFlux proof of work.
//!
//! Miners search off-chain on any backend (CPU, CUDA, ROCm, Metal), exactly
//! as on the native chain, then call `submit(height, nonce)`. The contract
//! recomputes that single attempt with profile `zfx-near-v1` (3.47 M
//! multiply-adds) and the epoch weights held in storage, checks the digest
//! against the current target, mints the block reward to the caller, extends
//! the hash chain and retargets difficulty (LWMA, RunningMax solvetimes).
//!
//! The challenge commits to the caller's account, so a nonce copied from the
//! mempool is worthless to anyone else. Epoch weights (32 blocks) are
//! prepared once by `prepare_epoch`, one layer per call, by anyone.
use near_contract_standards::fungible_token::events::FtMint;
use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_pow::difficulty::{next_target, DifficultyParams, SolvetimeRule, LWMA_WINDOW};
use near_pow::hash::sha256;
use near_pow::pow::{self, NEAR_V1};
use near_pow::u256::U256;
use near_pow::weights;
use near_sdk::json_types::{U128, U64};
use near_sdk::serde_json::{json, Value};
use near_sdk::store::{LazyOption, LookupMap};
use near_sdk::{env, log, near, require, AccountId, BorshStorageKey, PanicOnDefault, PromiseOrValue};

pub const EVENT_STANDARD: &str = "zfx-mine";
pub const WORK_DOMAIN: &[u8] = b"ZFXM-work-v1";

#[derive(BorshStorageKey)]
#[near]
enum StorageKey {
    Token,
    Metadata,
    Anchors,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
    /// Number of mined blocks.
    height: u64,
    /// Hash chain over mined blocks.
    prev_hash: [u8; 32],
    target: [u8; 32],
    genesis_target: [u8; 32],
    pow_limit: [u8; 32],
    spacing_secs: u64,
    reward: u128,
    /// (timestamp, target) of the last LWMA_WINDOW + 1 blocks, oldest first.
    history: Vec<(u64, [u8; 32])>,
    /// Chain hash at every epoch anchor height (multiples of 32).
    anchors: LookupMap<u64, [u8; 32]>,
    /// Seed whose weights are stored for the previous epoch (freed lazily).
    stale_seed: Option<[u8; 32]>,
}

fn event(name: &str, data: Value) {
    log!("EVENT_JSON:{}", json!({"standard": EVENT_STANDARD, "version": "1.0.0", "event": name, "data": [data]}));
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

#[near]
impl Contract {
    #[init]
    pub fn new(genesis_seed: String, genesis_target: String, spacing_secs: u64, reward: U128, metadata: Option<FungibleTokenMetadata>) -> Self {
        let gt = U256::from_hex(&genesis_target).unwrap_or_else(|| env::panic_str("genesis_target must be 64 hex chars"));
        require!(!gt.is_zero() && spacing_secs > 0 && reward.0 > 0, "bad parameters");
        let md = metadata.unwrap_or(FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "ZeroFlux (testnet, proof-of-work minted)".to_string(),
            symbol: "ZFX".to_string(),
            icon: None,
            reference: None,
            reference_hash: None,
            decimals: 8,
        });
        md.assert_valid();
        let g = sha256(&[b"ZFXM-genesis-v1", genesis_seed.as_bytes()]);
        let mut anchors = LookupMap::new(StorageKey::Anchors);
        anchors.insert(0, g);
        Self {
            token: FungibleToken::new(StorageKey::Token),
            metadata: LazyOption::new(StorageKey::Metadata, Some(md)),
            height: 0,
            prev_hash: g,
            target: gt.to_be_bytes(),
            genesis_target: gt.to_be_bytes(),
            pow_limit: U256::MAX.shr(1).to_be_bytes(),
            spacing_secs,
            reward: reward.0,
            history: vec![(env::block_timestamp() / 1_000_000_000, gt.to_be_bytes())],
            anchors,
            stale_seed: None,
        }
    }

    fn seed_for_next(&self) -> [u8; 32] {
        let anchor_h = pow::epoch_anchor_height(self.height + 1);
        let anchor = *self.anchors.get(&anchor_h).expect("anchor recorded");
        pow::epoch_seed(&anchor, &NEAR_V1)
    }

    /// Bytes the miner's challenge commits to (passed to miners as the
    /// "header_without_nonce"): contract, next height, chain hash, miner.
    fn work_bytes(&self, miner: &AccountId) -> Vec<u8> {
        let mut b = WORK_DOMAIN.to_vec();
        put_str(&mut b, env::current_account_id().as_str());
        b.extend_from_slice(&(self.height + 1).to_le_bytes());
        b.extend_from_slice(&self.prev_hash);
        put_str(&mut b, miner.as_str());
        b
    }

    /// Prepare one layer of the next block's epoch weights (anyone may call;
    /// about 115 Tgas for the largest layer). Frees the previous epoch.
    pub fn prepare_epoch(&mut self, layer: u8) -> Value {
        let seed = self.seed_for_next();
        let written = weights::prepare_layer(&NEAR_V1, &seed, layer as usize);
        if let Some(old) = self.stale_seed {
            if old != seed {
                weights::remove(&NEAR_V1, &old);
                self.stale_seed = None;
            }
        }
        let missing = weights::missing_layers(&NEAR_V1, &seed);
        json!({"epoch_seed": hex::encode(seed), "layer": layer, "written": written, "missing_layers": missing})
    }

    /// Submit a solution for block `height` (= current height + 1).
    pub fn submit(&mut self, height: U64, nonce: U64) -> Value {
        let miner = env::predecessor_account_id();
        require!(height.0 == self.height + 1, "stale height: another miner found this block");
        let seed = self.seed_for_next();
        let g0 = env::used_gas();
        let w = weights::load(&NEAR_V1, &seed).unwrap_or_else(|| env::panic_str("epoch weights not prepared: call prepare_epoch for each missing layer"));
        let g1 = env::used_gas();
        let challenge = pow::challenge_bytes(&self.work_bytes(&miner));
        let digest = pow::attempt(&w, &challenge, nonce.0);
        let g2 = env::used_gas();
        let target = U256::from_be_bytes(&self.target);
        require!(pow::meets_target(&digest, &target), "digest does not meet the target");

        if self.token.accounts.get(&miner).is_none() {
            self.token.internal_register_account(&miner);
        }
        self.token.internal_deposit(&miner, self.reward);
        FtMint { owner_id: &miner, amount: U128(self.reward), memo: Some(&format!("zfx-block:{}", self.height + 1)) }.emit();

        self.height += 1;
        self.prev_hash = sha256(&[b"ZFXM-chain-v1", &self.prev_hash, &digest, miner.as_bytes()]);
        if self.height % pow::EPOCH_LEN == 0 {
            self.anchors.insert(self.height, self.prev_hash);
            self.stale_seed = Some(seed);
        }
        let now = env::block_timestamp() / 1_000_000_000;
        self.history.push((now, self.target));
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
        let hist = if self.height < LWMA_WINDOW as u64 + 1 { vec![] } else { hist };
        self.target = next_target(&params, &U256::from_be_bytes(&self.genesis_target), &hist).to_be_bytes();
        let gas = json!({"load_weights_tgas": (g1.as_gas() - g0.as_gas()) as f64 / 1e12, "attempt_tgas": (g2.as_gas() - g1.as_gas()) as f64 / 1e12});
        event("mined", json!({"height": U64(self.height), "miner": miner, "reward": U128(self.reward), "digest": hex::encode(digest), "gas": gas}));
        json!({"height": U64(self.height), "digest": hex::encode(digest), "reward": U128(self.reward), "gas": gas})
    }

    // ------------------------- views -------------------------

    /// Work for a miner: what zpu-cli needs (see `zfx-bridge near-mine-proxy`).
    pub fn work(&self, miner: AccountId) -> Value {
        let seed = self.seed_for_next();
        let wb = self.work_bytes(&miner);
        json!({
            "height": U64(self.height + 1),
            "profile": NEAR_V1.id,
            "epoch_seed": hex::encode(seed),
            "target": hex::encode(self.target),
            "work_bytes": hex::encode(&wb),
            "challenge": hex::encode(pow::challenge_bytes(&wb)),
            "missing_layers": weights::missing_layers(&NEAR_V1, &seed),
        })
    }

    pub fn stats(&self) -> Value {
        json!({
            "height": U64(self.height),
            "prev_hash": hex::encode(self.prev_hash),
            "target": hex::encode(self.target),
            "reward": U128(self.reward),
            "spacing_secs": U64(self.spacing_secs),
            "total_supply": self.token.ft_total_supply(),
            "expected_supply": U128(self.reward * self.height as u128),
            "profile": NEAR_V1.name,
            "macs_per_attempt": U64(NEAR_V1.macs_per_attempt()),
        })
    }

    pub fn verify(&self, miner: AccountId, nonce: U64) -> Value {
        let seed = self.seed_for_next();
        let w = weights::load(&NEAR_V1, &seed).unwrap_or_else(|| env::panic_str("epoch weights not prepared"));
        let digest = pow::attempt(&w, &pow::challenge_bytes(&self.work_bytes(&miner)), nonce.0);
        json!({"digest": hex::encode(digest), "meets_target": pow::meets_target(&digest, &U256::from_be_bytes(&self.target))})
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
    fn storage_withdraw(&mut self, amount: Option<near_sdk::NearToken>) -> StorageBalance {
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
