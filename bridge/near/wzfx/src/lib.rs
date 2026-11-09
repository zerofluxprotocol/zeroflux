//! wZFX: wrapped ZeroFlux on NEAR (testnet).
//!
//! Asset identity: wZFX is a claim on native ZFX locked in the ZeroFlux
//! lockbox, 1:1 in base units (8 decimals on both sides). It is not a
//! second coin. Supply changes only through:
//!   - `bridge_mint`: needs `threshold` (2) distinct committee ed25519
//!     signatures over `mint_digest`, bound to this contract account, the
//!     native chain id/asset id, the committee epoch, a single-use deposit
//!     id (the native deposit txid) and an expiry;
//!   - `bridge_burn`: the holder burns, producing a burn record whose
//!     `withdrawal_id` the committee must see *finalized* before signing a
//!     native release (2-of-3 SLH-DSA, enforced by ZeroFlux consensus).
//!
//! Trust model: 2-of-3 custodial committee. Two compromised members can
//! mint unbacked wZFX.
// `new` takes 8 named JSON fields (the public init ABI); near-sdk expands it
// into a generated extern fn, so the allow has to be crate-level.
#![allow(clippy::too_many_arguments)]

use near_contract_standards::fungible_token::events::{FtBurn, FtMint};
use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_sdk::json_types::{U128, U64};
use near_sdk::serde_json::json;
use near_sdk::store::{LazyOption, LookupMap, LookupSet};
use near_sdk::{env, log, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue};

pub const CONTRACT_VERSION: &str = if cfg!(feature = "upgrade-test-v2") { "wzfx-1.0.0-upgrade-test-v2" } else { "wzfx-1.0.0" };
pub const COMMITTEE_SIZE: usize = 3;
pub const EVENT_STANDARD: &str = "zfx-bridge";

#[derive(BorshStorageKey)]
#[near]
enum StorageKey {
    Token,
    Metadata,
    Minted,
    Burns,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct Approval {
    /// Committee index 0..3.
    pub member: u8,
    /// ed25519 signature, hex (64 bytes).
    pub signature: String,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct MintAttestation {
    pub epoch: u32,
    /// Native deposit id (ZeroFlux txid), hex 32 bytes. Single use.
    pub deposit_id: String,
    pub recipient: AccountId,
    pub amount: U128,
    /// Native height of the deposit's block (evidence, logged).
    pub native_height: U64,
    /// Attestation invalid after this block timestamp (ns).
    pub expiry_ns: U64,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct BurnRecord {
    pub nonce: U64,
    pub withdrawal_id: String,
    pub account_id: AccountId,
    pub amount: U128,
    pub native_recipient: String,
    pub block_height: U64,
    pub timestamp_ns: U64,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum AdminAction {
    Pause,
    Unpause,
    /// New committee (3 hex ed25519 keys); activates after the timelock.
    Rotate {
        committee: Vec<String>,
    },
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct PendingRotation {
    pub committee: Vec<String>,
    pub activate_ns: U64,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
    owner_id: AccountId,
    committee: Vec<[u8; 32]>,
    threshold: u8,
    epoch: u32,
    pending_rotation: Option<PendingRotation>,
    timelock_ns: u64,
    native_chain_id: u32,
    native_asset_id: String,
    paused: bool,
    minted: LookupSet<String>,
    total_minted: u128,
    total_burned: u128,
    burn_nonce: u64,
    burns: LookupMap<u64, BurnRecord>,
    admin_nonce: u64,
    staged_upgrade: Option<(String, u64)>,
}

fn decode_key(h: &str) -> [u8; 32] {
    let v = hex::decode(h).unwrap_or_else(|_| env::panic_str("committee key must be hex"));
    v.try_into().unwrap_or_else(|_| env::panic_str("committee key must be 32 bytes"))
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u32).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

fn event(name: &str, data: near_sdk::serde_json::Value) {
    log!("EVENT_JSON:{}", json!({"standard": EVENT_STANDARD, "version": "1.0.0", "event": name, "data": [data]}));
}

/// Mint digest signed by committee members (also computed off-chain by
/// relayers; see bridge/relayers and docs/CONTRACT_API.md).
pub fn mint_digest(contract: &str, native_chain_id: u32, native_asset_id: &str, a: &MintAttestation) -> Vec<u8> {
    let mut b = b"ZFX-near-mint-v1".to_vec();
    put_str(&mut b, contract);
    b.extend_from_slice(&native_chain_id.to_le_bytes());
    put_str(&mut b, native_asset_id);
    b.extend_from_slice(&a.epoch.to_le_bytes());
    b.extend_from_slice(&hex::decode(&a.deposit_id).unwrap_or_default());
    put_str(&mut b, a.recipient.as_str());
    b.extend_from_slice(&a.amount.0.to_le_bytes());
    b.extend_from_slice(&a.native_height.0.to_le_bytes());
    b.extend_from_slice(&a.expiry_ns.0.to_le_bytes());
    env::sha256(&b)
}

pub fn admin_digest(contract: &str, epoch: u32, admin_nonce: u64, action: &AdminAction) -> Vec<u8> {
    let mut b = b"ZFX-near-admin-v1".to_vec();
    put_str(&mut b, contract);
    b.extend_from_slice(&epoch.to_le_bytes());
    b.extend_from_slice(&admin_nonce.to_le_bytes());
    match action {
        AdminAction::Pause => b.push(1),
        AdminAction::Unpause => b.push(2),
        AdminAction::Rotate { committee } => {
            b.push(3);
            for k in committee {
                put_str(&mut b, k);
            }
        }
    }
    env::sha256(&b)
}

pub fn withdrawal_id(contract: &str, nonce: u64) -> String {
    let mut b = b"ZFX-near-burn-v1".to_vec();
    put_str(&mut b, contract);
    b.extend_from_slice(&nonce.to_le_bytes());
    hex::encode(env::sha256(&b))
}

/// "zfx1" + hex(32-byte address) + hex(first 4 bytes of SHA256("ZFX-addrck"||addr)).
fn valid_native_address(s: &str) -> bool {
    let Some(body) = s.strip_prefix("zfx1") else { return false };
    let Ok(raw) = hex::decode(body) else { return false };
    if raw.len() != 36 {
        return false;
    }
    let mut m = b"ZFX-addrck".to_vec();
    m.extend_from_slice(&raw[..32]);
    env::sha256(&m)[..4] == raw[32..]
}

#[near]
impl Contract {
    #[init]
    pub fn new(
        owner_id: AccountId,
        committee: Vec<String>,
        threshold: u8,
        native_chain_id: u32,
        native_asset_id: String,
        timelock_sec: u64,
        metadata: Option<FungibleTokenMetadata>,
    ) -> Self {
        require!(committee.len() == COMMITTEE_SIZE, "committee must have exactly 3 members");
        require!((2..=3).contains(&threshold), "threshold must be 2 or 3");
        let keys: Vec<[u8; 32]> = committee.iter().map(|k| decode_key(k)).collect();
        require!(keys[0] != keys[1] && keys[0] != keys[2] && keys[1] != keys[2], "committee keys must be distinct");
        let md = metadata.unwrap_or(FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "Wrapped ZeroFlux (testnet, 2-of-3 bridge)".to_string(),
            symbol: "wZFX".to_string(),
            icon: None,
            reference: None,
            reference_hash: None,
            decimals: 8,
        });
        md.assert_valid();
        Self {
            token: FungibleToken::new(StorageKey::Token),
            metadata: LazyOption::new(StorageKey::Metadata, Some(md)),
            owner_id,
            committee: keys,
            threshold,
            epoch: 0,
            pending_rotation: None,
            timelock_ns: timelock_sec.checked_mul(1_000_000_000).unwrap_or_else(|| env::panic_str("timelock overflow")),
            native_chain_id,
            native_asset_id,
            paused: false,
            minted: LookupSet::new(StorageKey::Minted),
            total_minted: 0,
            total_burned: 0,
            burn_nonce: 0,
            burns: LookupMap::new(StorageKey::Burns),
            admin_nonce: 0,
            staged_upgrade: None,
        }
    }

    /// Count distinct valid member signatures over `digest`. Malformed,
    /// duplicate or invalid approvals abort the call.
    fn count_approvals(&self, digest: &[u8], approvals: &[Approval]) -> usize {
        require!(approvals.len() <= COMMITTEE_SIZE, "too many approvals");
        let mut seen = [false; COMMITTEE_SIZE];
        for a in approvals {
            let i = a.member as usize;
            require!(i < self.committee.len(), "approval from unknown member");
            require!(!seen[i], "duplicate approval from the same member");
            let sig: [u8; 64] =
                hex::decode(&a.signature).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| env::panic_str("signature must be 64 bytes hex"));
            require!(env::ed25519_verify(&sig, digest, &self.committee[i]), "invalid committee signature");
            seen[i] = true;
        }
        seen.iter().filter(|s| **s).count()
    }

    /// Mint wZFX for a native deposit. Anyone may submit; the quorum
    /// signatures are the authorization. The recipient must be registered
    /// (NEP-145) or the call fails before any state change.
    pub fn bridge_mint(&mut self, attestation: MintAttestation, approvals: Vec<Approval>) {
        require!(!self.paused, "bridge paused");
        require!(attestation.epoch == self.epoch, "attestation for wrong committee epoch");
        require!(env::block_timestamp() <= attestation.expiry_ns.0, "attestation expired");
        require!(attestation.amount.0 > 0, "zero amount");
        let dep = hex::decode(&attestation.deposit_id).unwrap_or_else(|_| env::panic_str("deposit_id must be hex"));
        require!(dep.len() == 32, "deposit_id must be 32 bytes");
        require!(!self.minted.contains(&attestation.deposit_id), "deposit already minted (replay)");
        let digest = mint_digest(env::current_account_id().as_str(), self.native_chain_id, &self.native_asset_id, &attestation);
        let n = self.count_approvals(&digest, &approvals);
        require!(n >= self.threshold as usize, "insufficient committee approvals");
        require!(self.token.storage_balance_of(attestation.recipient.clone()).is_some(), "recipient not registered (storage_deposit first)");
        self.total_minted = self.total_minted.checked_add(attestation.amount.0).unwrap_or_else(|| env::panic_str("mint overflow"));
        self.minted.insert(attestation.deposit_id.clone());
        self.token.internal_deposit(&attestation.recipient, attestation.amount.0);
        FtMint { owner_id: &attestation.recipient, amount: attestation.amount, memo: Some(&format!("zfx-deposit:{}", attestation.deposit_id)) }.emit();
        event(
            "mint",
            json!({"deposit_id": attestation.deposit_id, "recipient": attestation.recipient, "amount": attestation.amount,
            "native_height": attestation.native_height, "epoch": attestation.epoch, "approvals": n}),
        );
    }

    /// Burn wZFX to redeem native ZFX at `native_recipient` (zfx1...).
    /// Returns the burn record; its `withdrawal_id` keys the native release.
    #[payable]
    pub fn bridge_burn(&mut self, amount: U128, native_recipient: String) -> BurnRecord {
        near_sdk::assert_one_yocto();
        require!(!self.paused, "bridge paused");
        require!(amount.0 > 0, "zero amount");
        require!(valid_native_address(&native_recipient), "native_recipient is not a valid zfx1 address (checksum)");
        let account = env::predecessor_account_id();
        self.token.internal_withdraw(&account, amount.0);
        self.total_burned = self.total_burned.checked_add(amount.0).unwrap_or_else(|| env::panic_str("burn overflow"));
        let nonce = self.burn_nonce;
        self.burn_nonce = nonce.checked_add(1).unwrap();
        let rec = BurnRecord {
            nonce: U64(nonce),
            withdrawal_id: withdrawal_id(env::current_account_id().as_str(), nonce),
            account_id: account.clone(),
            amount,
            native_recipient: native_recipient.clone(),
            block_height: U64(env::block_height()),
            timestamp_ns: U64(env::block_timestamp()),
        };
        self.burns.insert(nonce, rec.clone());
        FtBurn { owner_id: &account, amount, memo: Some(&format!("zfx-withdrawal:{}", rec.withdrawal_id)) }.emit();
        event(
            "burn",
            json!({"nonce": rec.nonce, "withdrawal_id": rec.withdrawal_id, "account_id": account, "amount": amount,
            "native_recipient": native_recipient, "block_height": rec.block_height}),
        );
        rec
    }

    /// Emergency pause: the owner, or a single committee member signature.
    pub fn bridge_pause(&mut self, approvals: Option<Vec<Approval>>) {
        if env::predecessor_account_id() != self.owner_id {
            let a = approvals.unwrap_or_default();
            let d = admin_digest(env::current_account_id().as_str(), self.epoch, self.admin_nonce, &AdminAction::Pause);
            require!(self.count_approvals(&d, &a) >= 1, "pause needs the owner or one committee signature");
            self.admin_nonce += 1;
        }
        self.paused = true;
        event("pause", json!({"by": env::predecessor_account_id()}));
    }

    /// Unpause needs `threshold` committee signatures (the owner alone
    /// cannot resume a bridge the committee halted).
    pub fn bridge_unpause(&mut self, approvals: Vec<Approval>) {
        let d = admin_digest(env::current_account_id().as_str(), self.epoch, self.admin_nonce, &AdminAction::Unpause);
        require!(self.count_approvals(&d, &approvals) >= self.threshold as usize, "unpause needs threshold approvals");
        self.admin_nonce += 1;
        self.paused = false;
        event("unpause", json!({}));
    }

    /// Schedule a committee rotation (threshold approvals); it activates
    /// only after the timelock via `apply_rotation`.
    pub fn propose_rotation(&mut self, committee: Vec<String>, approvals: Vec<Approval>) {
        require!(committee.len() == COMMITTEE_SIZE, "committee must have 3 members");
        let keys: Vec<[u8; 32]> = committee.iter().map(|k| decode_key(k)).collect();
        require!(keys[0] != keys[1] && keys[0] != keys[2] && keys[1] != keys[2], "committee keys must be distinct");
        let action = AdminAction::Rotate { committee: committee.clone() };
        let d = admin_digest(env::current_account_id().as_str(), self.epoch, self.admin_nonce, &action);
        require!(self.count_approvals(&d, &approvals) >= self.threshold as usize, "rotation needs threshold approvals");
        self.admin_nonce += 1;
        let at = env::block_timestamp().checked_add(self.timelock_ns).unwrap();
        self.pending_rotation = Some(PendingRotation { committee, activate_ns: U64(at) });
        event("rotation_proposed", json!({"activate_ns": U64(at)}));
    }

    pub fn apply_rotation(&mut self) {
        let p = self.pending_rotation.clone().unwrap_or_else(|| env::panic_str("no pending rotation"));
        require!(env::block_timestamp() >= p.activate_ns.0, "rotation timelock not elapsed");
        self.committee = p.committee.iter().map(|k| decode_key(k)).collect();
        self.epoch += 1;
        self.pending_rotation = None;
        event("rotation_applied", json!({"epoch": self.epoch}));
    }

    /// Upgrade governance: the owner stages a code hash; after the timelock
    /// the owner calls `deploy_upgrade` with the exact WASM as raw input.
    pub fn stage_upgrade(&mut self, code_hash: String) {
        require!(env::predecessor_account_id() == self.owner_id, "owner only");
        require!(code_hash.len() == 64 && hex::decode(&code_hash).is_ok(), "code_hash must be sha256 hex");
        let ready = env::block_timestamp().checked_add(self.timelock_ns).unwrap();
        self.staged_upgrade = Some((code_hash.clone(), ready));
        event("upgrade_staged", json!({"code_hash": code_hash, "ready_ns": U64(ready)}));
    }

    pub fn deploy_upgrade(&mut self) -> Promise {
        require!(env::predecessor_account_id() == self.owner_id, "owner only");
        let (hash, ready) = self.staged_upgrade.clone().unwrap_or_else(|| env::panic_str("no staged upgrade"));
        require!(env::block_timestamp() >= ready, "upgrade timelock not elapsed");
        let code = env::input().unwrap_or_else(|| env::panic_str("pass the WASM as raw input"));
        require!(hex::encode(env::sha256(&code)) == hash, "WASM does not match the staged code hash");
        self.staged_upgrade = None;
        event("upgrade_deployed", json!({"code_hash": hash}));
        Promise::new(env::current_account_id()).deploy_contract(code).function_call(
            "migrate".to_string(),
            vec![],
            NearToken::from_yoctonear(0),
            Gas::from_tgas(20),
        )
    }

    /// State layout is unchanged across these versions; migrate re-reads it.
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        env::state_read().unwrap_or_else(|| env::panic_str("no state"))
    }

    // ------------------------- views -------------------------

    pub fn version(&self) -> String {
        CONTRACT_VERSION.to_string()
    }

    pub fn is_minted(&self, deposit_id: String) -> bool {
        self.minted.contains(&deposit_id)
    }

    pub fn get_burn(&self, nonce: U64) -> Option<BurnRecord> {
        self.burns.get(&nonce.0).cloned()
    }

    pub fn burns_since(&self, from_nonce: U64, limit: u32) -> Vec<BurnRecord> {
        (from_nonce.0..self.burn_nonce.min(from_nonce.0.saturating_add(limit.min(100) as u64))).filter_map(|n| self.burns.get(&n).cloned()).collect()
    }

    pub fn bridge_state(&self) -> near_sdk::serde_json::Value {
        json!({
            "version": CONTRACT_VERSION,
            "owner_id": self.owner_id,
            "committee": self.committee.iter().map(hex::encode).collect::<Vec<_>>(),
            "threshold": self.threshold,
            "epoch": self.epoch,
            "pending_rotation": self.pending_rotation,
            "paused": self.paused,
            "native_chain_id": self.native_chain_id,
            "native_asset_id": self.native_asset_id,
            "total_minted": U128(self.total_minted),
            "total_burned": U128(self.total_burned),
            "total_supply": self.token.ft_total_supply(),
            "burn_nonce": U64(self.burn_nonce),
            "admin_nonce": U64(self.admin_nonce),
            "timelock_ns": U64(self.timelock_ns),
            "staged_upgrade": self.staged_upgrade.as_ref().map(|(h, r)| json!({"code_hash": h, "ready_ns": U64(*r)})),
            "trust_model": "2-of-3 custodial committee",
        })
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
        let (used, burned) = self.token.internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        if burned > 0 {
            // Refund to an account that unregistered meanwhile: the standard
            // burns it. Count it so wrapped supply accounting stays exact.
            self.total_burned = self.total_burned.checked_add(burned).unwrap();
            event("refund_burned", json!({"account_id": sender_id, "amount": U128(burned)}));
        }
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
        // Forbid force-unregistering with a balance: it would burn wZFX
        // outside the bridge path and blur the backing accounting.
        let me = env::predecessor_account_id();
        require!(self.token.ft_balance_of(me).0 == 0, "withdraw or bridge_burn your wZFX before unregistering");
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

#[cfg(test)]
mod tests;
