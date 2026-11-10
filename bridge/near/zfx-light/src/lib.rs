//! ZeroFlux light client on NEAR (option B).
//!
//! Anyone relays ZeroFlux block headers. The contract applies the node's own
//! header rules: parent link, chain id, PoW profile `zfx-near-v1`, median
//! time past and future limit, the exact expected target (fixed or LWMA with
//! RunningMax solvetimes, from the same ancestors the node uses), and the full
//! proof of work recomputed on-chain. It follows the chain with the most
//! cumulative work and records reorgs.
//!
//! Deposits then need no committee: `claim_deposit` checks a Merkle inclusion
//! proof of a lockbox transfer against a canonical header with at least K
//! confirmations and mints the amount to the NEAR account in the transfer's
//! memo, once. Security is that of the ZeroFlux proof of work at depth K.
use near_contract_standards::fungible_token::events::FtMint;
use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_pow::difficulty::{check_timestamp, next_target, DifficultyParams, SolvetimeRule, LWMA_WINDOW, MTP_WINDOW};
use near_pow::hash::sha256;
use near_pow::header::BlockHeader;
use near_pow::pow::{self, NEAR_V1};
use near_pow::u256::U256;
use near_pow::{parse32, weights};
use near_sdk::json_types::{U128, U64};
use near_sdk::serde_json::{json, Value};
use near_sdk::store::{LazyOption, LookupMap, LookupSet};
use near_sdk::{env, log, near, require, AccountId, BorshStorageKey, PanicOnDefault, PromiseOrValue};

pub const EVENT_STANDARD: &str = "zfx-light";

#[derive(BorshStorageKey)]
#[near]
enum StorageKey {
    Headers,
    Canon,
    Token,
    Metadata,
    Claimed,
}

#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct HeaderRec {
    header: Vec<u8>,
    height: u64,
    parent: [u8; 32],
    timestamp: u64,
    target: [u8; 32],
    cum_work: [u8; 32],
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    chain_id: u32,
    genesis: [u8; 32],
    genesis_target: [u8; 32],
    pow_limit: [u8; 32],
    fixed_difficulty: bool,
    spacing_secs: u64,
    max_future_secs: u64,
    confirmations: u64,
    headers: LookupMap<[u8; 32], HeaderRec>,
    canon: LookupMap<u64, [u8; 32]>,
    best: [u8; 32],
    best_height: u64,
    reorgs: u64,
    max_reorg_depth: u64,
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
    claimed: LookupSet<[u8; 32]>,
    lockbox: [u8; 32],
}

fn event(name: &str, data: Value) {
    log!("EVENT_JSON:{}", json!({"standard": EVENT_STANDARD, "version": "1.0.0", "event": name, "data": [data]}));
}

fn verify_merkle(tx_root: &[u8; 32], leaf_count: u32, leaf: &[u8; 32], proof: &[([u8; 32], bool)]) -> bool {
    let mut h = *leaf;
    for (sib, right) in proof {
        h = if *right { sha256(&[b"ZFX-merkle-v0.1", &h, sib]) } else { sha256(&[b"ZFX-merkle-v0.1", sib, &h]) };
    }
    &sha256(&[b"ZFX-txroot-v0.1", &leaf_count.to_le_bytes(), &h]) == tx_root
}

#[near]
impl Contract {
    /// `genesis_header`: the chain's genesis header (164 bytes hex).
    #[init]
    pub fn new(genesis_header: String, fixed_difficulty: bool, spacing_secs: u64, max_future_secs: u64, confirmations: u64, pow_limit: String) -> Self {
        let raw = hex::decode(&genesis_header).unwrap_or_else(|_| env::panic_str("genesis_header must be hex"));
        let g = BlockHeader::decode(&raw).unwrap_or_else(|e| env::panic_str(&e));
        require!(g.height == 0 && g.version == NEAR_V1.id, "genesis must be height 0 on profile zfx-near-v1");
        require!(confirmations >= 1, "confirmations >= 1");
        let gh = g.hash();
        let mut headers = LookupMap::new(StorageKey::Headers);
        headers.insert(gh, HeaderRec { header: raw, height: 0, parent: [0; 32], timestamp: g.timestamp, target: g.target, cum_work: [0; 32] });
        let mut canon = LookupMap::new(StorageKey::Canon);
        canon.insert(0, gh);
        let md = FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "ZeroFlux (testnet, light-client bridged)".to_string(),
            symbol: "lZFX".to_string(),
            icon: None,
            reference: None,
            reference_hash: None,
            decimals: 8,
        };
        Self {
            chain_id: g.chain_id,
            genesis: gh,
            genesis_target: g.target,
            pow_limit: parse32(&pow_limit),
            fixed_difficulty,
            spacing_secs,
            max_future_secs,
            confirmations,
            headers,
            canon,
            best: gh,
            best_height: 0,
            reorgs: 0,
            max_reorg_depth: 0,
            token: FungibleToken::new(StorageKey::Token),
            metadata: LazyOption::new(StorageKey::Metadata, Some(md)),
            claimed: LookupSet::new(StorageKey::Claimed),
            lockbox: sha256(&[b"ZFX-bridge-lockbox-v1", &g.chain_id.to_le_bytes()]),
        }
    }

    fn rec(&self, h: &[u8; 32]) -> HeaderRec {
        self.headers.get(h).cloned().unwrap_or_else(|| env::panic_str("unknown header"))
    }

    /// (timestamp, target) of up to `n` ancestors ending at `from`, oldest first
    /// (the node's `history`).
    fn history(&self, from: &[u8; 32], n: usize) -> Vec<(u64, U256)> {
        let mut v = Vec::with_capacity(n);
        let mut h = *from;
        while v.len() < n {
            let Some(r) = self.headers.get(&h) else { break };
            v.push((r.timestamp, U256::from_be_bytes(&r.target)));
            if r.height == 0 {
                break;
            }
            h = r.parent;
        }
        v.reverse();
        v
    }

    fn ancestor_at(&self, from: &[u8; 32], height: u64) -> [u8; 32] {
        let mut h = *from;
        loop {
            let r = self.rec(&h);
            if r.height == height {
                return h;
            }
            require!(r.height > height, "ancestor below height");
            h = r.parent;
        }
    }

    fn seed(&self, parent: &[u8; 32], height: u64) -> [u8; 32] {
        let anchor = self.ancestor_at(parent, pow::epoch_anchor_height(height));
        pow::epoch_seed(&anchor, &NEAR_V1)
    }

    /// Prepare one weight layer for the epoch of the block at `height` whose
    /// parent is `parent` (anyone; seeds come only from stored headers).
    pub fn prepare_weights(&mut self, parent: String, height: U64, layer: u8) -> Value {
        let seed = self.seed(&parse32(&parent), height.0);
        let written = weights::prepare_layer(&NEAR_V1, &seed, layer as usize);
        json!({"epoch_seed": hex::encode(seed), "written": written, "missing_layers": weights::missing_layers(&NEAR_V1, &seed)})
    }

    pub fn submit_header(&mut self, header: String) -> Value {
        let raw = hex::decode(&header).unwrap_or_else(|_| env::panic_str("header must be hex"));
        let h = BlockHeader::decode(&raw).unwrap_or_else(|e| env::panic_str(&e));
        let hash = h.hash();
        require!(self.headers.get(&hash).is_none(), "header already known");
        let parent = self.rec(&h.parent);
        require!(h.height == parent.height + 1, "height must be parent + 1");
        require!(h.chain_id == self.chain_id, "wrong chain id");
        require!(h.version == NEAR_V1.id, "wrong PoW profile");
        let anc: Vec<u64> = self.history(&h.parent, MTP_WINDOW).iter().map(|x| x.0).collect();
        check_timestamp(h.timestamp, &anc, env::block_timestamp() / 1_000_000_000, self.max_future_secs).unwrap_or_else(|e| env::panic_str(&e));
        let params = DifficultyParams {
            solvetime_rule: SolvetimeRule::RunningMax,
            target_spacing_secs: self.spacing_secs,
            pow_limit: U256::from_be_bytes(&self.pow_limit),
            fixed: self.fixed_difficulty,
        };
        let hist = if parent.height < LWMA_WINDOW as u64 + 1 { vec![] } else { self.history(&h.parent, LWMA_WINDOW + 1) };
        let expected = next_target(&params, &U256::from_be_bytes(&self.genesis_target), &hist);
        require!(U256::from_be_bytes(&h.target) == expected, "target does not follow the difficulty rule");
        let seed = self.seed(&h.parent, h.height);
        let g0 = env::used_gas();
        let w = weights::load(&NEAR_V1, &seed).unwrap_or_else(|| env::panic_str("epoch weights not prepared: call prepare_weights"));
        let digest = pow::attempt(&w, &pow::challenge(&h), h.nonce);
        let pow_tgas = (env::used_gas().as_gas() - g0.as_gas()) as f64 / 1e12;
        require!(pow::meets_target(&digest, &U256::from_be_bytes(&h.target)), "invalid proof of work");
        let cum = U256::from_be_bytes(&parent.cum_work).saturating_add(&U256::work_for_target(&U256::from_be_bytes(&h.target)));
        self.headers
            .insert(hash, HeaderRec { header: raw, height: h.height, parent: h.parent, timestamp: h.timestamp, target: h.target, cum_work: cum.to_be_bytes() });
        let best_cum = U256::from_be_bytes(&self.rec(&self.best).cum_work);
        let mut reorg_depth = 0u64;
        if cum > best_cum {
            // Rewrite the canonical index back to the fork point.
            let old_best_height = self.best_height;
            let mut cur = hash;
            let mut ht = h.height;
            loop {
                if self.canon.get(&ht) == Some(&cur) {
                    break;
                }
                if self.canon.get(&ht).is_some() {
                    reorg_depth += 1;
                }
                self.canon.insert(ht, cur);
                if ht == 0 {
                    break;
                }
                cur = self.rec(&cur).parent;
                ht -= 1;
            }
            for stale in (h.height + 1)..=old_best_height {
                self.canon.remove(&stale);
                reorg_depth += 1;
            }
            if reorg_depth > 0 {
                self.reorgs += 1;
                self.max_reorg_depth = self.max_reorg_depth.max(reorg_depth);
                event("reorg", json!({"depth": U64(reorg_depth), "new_best": hex::encode(hash)}));
            }
            self.best = hash;
            self.best_height = h.height;
        }
        event("header", json!({"hash": hex::encode(hash), "height": U64(h.height), "best": self.best == hash}));
        json!({"hash": hex::encode(hash), "height": U64(h.height), "is_best": self.best == hash, "reorg_depth": U64(reorg_depth), "pow_tgas": pow_tgas})
    }

    /// Mint for a ZeroFlux lockbox deposit proven by Merkle inclusion in a
    /// canonical block with at least K confirmations. `core`/`witness` are
    /// the transaction's canonical encoding (the node's deposit receipt).
    pub fn claim_deposit(&mut self, block: String, core: String, witness: String, leaf_count: u32, proof: Vec<(String, bool)>) -> Value {
        let bh = parse32(&block);
        let r = self.rec(&bh);
        require!(self.canon.get(&r.height) == Some(&bh), "block is not on the best chain");
        let confs = self.best_height - r.height + 1;
        require!(confs >= self.confirmations, "not enough confirmations");
        let core = hex::decode(&core).unwrap_or_else(|_| env::panic_str("core must be hex"));
        let witness = hex::decode(&witness).unwrap_or_else(|_| env::panic_str("witness must be hex"));
        require!(core.len() >= 94 && core[92] == 1, "deposit must carry a memo");
        let mlen = core[93] as usize;
        require!(core.len() == 94 + mlen, "malformed transaction core");
        require!(u32::from_le_bytes(core[0..4].try_into().unwrap()) == self.chain_id, "wrong chain");
        require!(core[36..68] == self.lockbox, "not a transfer to the lockbox");
        let amount = u64::from_le_bytes(core[68..76].try_into().unwrap()) as u128;
        let recipient: AccountId =
            std::str::from_utf8(&core[94..]).ok().and_then(|s| s.parse().ok()).unwrap_or_else(|| env::panic_str("memo is not a NEAR account"));
        let txid = sha256(&[b"ZFX-txid-v0.1", &core]);
        require!(!self.claimed.contains(&txid), "deposit already claimed");
        let mut enc = core.clone();
        enc.extend_from_slice(&witness);
        let leaf = sha256(&[b"ZFX-wtxid-v0.1", &enc]);
        let proof: Vec<([u8; 32], bool)> = proof.iter().map(|(h, r)| (parse32(h), *r)).collect();
        let hdr = BlockHeader::decode(&r.header).unwrap();
        require!(verify_merkle(&hdr.tx_root, leaf_count, &leaf, &proof), "inclusion proof does not match the block");
        self.claimed.insert(txid);
        if self.token.accounts.get(&recipient).is_none() {
            self.token.internal_register_account(&recipient);
        }
        self.token.internal_deposit(&recipient, amount);
        FtMint { owner_id: &recipient, amount: U128(amount), memo: Some(&format!("zfx-deposit:{}", hex::encode(txid))) }.emit();
        json!({"deposit_id": hex::encode(txid), "recipient": recipient, "amount": U128(amount), "confirmations": U64(confs)})
    }

    // ------------------------- views -------------------------

    pub fn best(&self) -> Value {
        let r = self.rec(&self.best);
        json!({"hash": hex::encode(self.best), "height": U64(self.best_height), "cum_work": hex::encode(r.cum_work), "reorgs": U64(self.reorgs),
               "max_reorg_depth": U64(self.max_reorg_depth), "confirmations_required": U64(self.confirmations), "total_supply": self.token.ft_total_supply()})
    }

    pub fn canonical(&self, height: U64) -> Option<String> {
        self.canon.get(&height.0).map(hex::encode)
    }

    pub fn missing_layers(&self, parent: String, height: U64) -> Value {
        let seed = self.seed(&parse32(&parent), height.0);
        json!({"epoch_seed": hex::encode(seed), "missing_layers": weights::missing_layers(&NEAR_V1, &seed)})
    }

    pub fn is_claimed(&self, deposit_id: String) -> bool {
        self.claimed.contains(&parse32(&deposit_id))
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
        require!(self.token.ft_balance_of(env::predecessor_account_id()).0 == 0, "transfer your tokens before unregistering");
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
