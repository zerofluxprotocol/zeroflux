//! `mock-ft`: a test-only NEP-141 token with an open faucet, for sandbox and testnet. No value.

use near_contract_standards::fungible_token::metadata::{FT_METADATA_SPEC, FungibleTokenMetadata, FungibleTokenMetadataProvider};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_sdk::borsh::BorshSerialize;
use near_sdk::collections::LazyOption;
use near_sdk::json_types::U128;
use near_sdk::{AccountId, BorshStorageKey, NearToken, PanicOnDefault, PromiseOrValue, env, near, require};

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum StorageKey {
    Token,
    Metadata,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
    faucet_limit: u128,
    owner: AccountId,
}

#[near]
impl Contract {
    #[init]
    pub fn new(owner: AccountId, name: String, symbol: String, decimals: u8, faucet_limit: U128) -> Self {
        let metadata = FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name,
            symbol,
            icon: None,
            reference: None,
            reference_hash: None,
            decimals,
        };
        Self {
            token: FungibleToken::new(StorageKey::Token),
            metadata: LazyOption::new(StorageKey::Metadata, Some(&metadata)),
            faucet_limit: faucet_limit.0,
            owner,
        }
    }

    /// Faucet mint. Registers the receiver if needed (caller attaches storage deposit when the
    /// receiver is new; the minimum bound is returned by `storage_balance_bounds`).
    #[payable]
    pub fn mint(&mut self, account_id: AccountId, amount: U128) {
        require!(amount.0 > 0 && amount.0 <= self.faucet_limit, "FAUCET_LIMIT");
        if !self.token.accounts.contains_key(&account_id) {
            let min = self.token.storage_balance_bounds().min;
            require!(env::attached_deposit() >= min, "STORAGE_DEPOSIT_REQUIRED");
            self.token.internal_register_account(&account_id);
        }
        self.token.internal_deposit(&account_id, amount.0);
        near_contract_standards::fungible_token::events::FtMint {
            owner_id: &account_id,
            amount,
            memo: Some("faucet"),
        }
        .emit();
    }

    pub fn faucet_limit(&self) -> U128 {
        U128(self.faucet_limit)
    }

    pub fn owner(&self) -> AccountId {
        self.owner.clone()
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
        self.metadata.get().unwrap()
    }
}
