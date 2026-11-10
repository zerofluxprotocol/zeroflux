//! Storage abstraction. The NEAR contract implements [`Store`] over its persistent maps with a
//! write-back cache; the engine uses [`MemStore`]. The ledger logic in [`crate::ledger`] only
//! talks to this trait, so both run identical code on identical inputs.

use crate::model::*;
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Hashing and signature checks. The contract uses NEAR host functions, native code uses
/// `sha2` and `ed25519-dalek` ([`NativeCrypto`]).
pub trait Crypto {
    fn sha256(&self, data: &[u8]) -> [u8; 32];
    fn ed25519_verify(&self, sig: &[u8; 64], msg: &[u8], pk: &[u8; 32]) -> bool;
}

/// Reads take `&mut self` so a storage-backed implementation can cache what it loaded.
pub trait Store {
    fn globals(&self) -> &Globals;
    fn globals_mut(&mut self) -> &mut Globals;
    fn market(&mut self, id: &str) -> Option<MarketState>;
    fn put_market(&mut self, id: &str, m: MarketState);
    /// Missing accounts read as the default (empty) account.
    fn account(&mut self, a: &str) -> AccountState;
    fn put_account(&mut self, a: &str, s: AccountState);
    /// Missing positions read as flat.
    fn position(&mut self, a: &str, market: &str) -> Position;
    /// Implementations delete flat positions (size 0 and margin 0).
    fn put_position(&mut self, a: &str, market: &str, p: Position);
    fn key(&mut self, pk: &[u8; 32]) -> Option<KeyInfo>;
    fn order(&mut self, hash: &[u8; 32]) -> Option<OrderState>;
    fn put_order(&mut self, hash: &[u8; 32], o: OrderState);
    fn deposit(&mut self, id: u64) -> Option<PendingDeposit>;
    fn remove_deposit(&mut self, id: u64);
    fn forced(&mut self, id: u64) -> Option<ForcedRequest>;
    fn put_forced(&mut self, id: u64, r: ForcedRequest);
}

#[cfg(feature = "native")]
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeCrypto;

#[cfg(feature = "native")]
impl Crypto for NativeCrypto {
    fn sha256(&self, data: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        sha2::Sha256::digest(data).into()
    }
    fn ed25519_verify(&self, sig: &[u8; 64], msg: &[u8], pk: &[u8; 32]) -> bool {
        ed25519_dalek::VerifyingKey::from_bytes(pk)
            .is_ok_and(|vk| vk.verify_strict(msg, &ed25519_dalek::Signature::from_bytes(sig)).is_ok())
    }
}

/// In-memory ledger with deterministic (sorted) iteration. Used by the engine (speculative and
/// chain-confirmed copies), tests and the reference model.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct MemStore {
    pub globals: Globals,
    pub markets: BTreeMap<String, MarketState>,
    pub accounts: BTreeMap<String, AccountState>,
    /// Keyed (market, account) so a market's holders are a contiguous range.
    pub positions: BTreeMap<(String, String), Position>,
    pub keys: BTreeMap<[u8; 32], KeyInfo>,
    pub orders: BTreeMap<[u8; 32], OrderState>,
    pub deposits: BTreeMap<u64, PendingDeposit>,
    pub forced: BTreeMap<u64, ForcedRequest>,
}

impl MemStore {
    pub fn new(globals: Globals) -> Self {
        MemStore {
            globals,
            markets: BTreeMap::new(),
            accounts: BTreeMap::new(),
            positions: BTreeMap::new(),
            keys: BTreeMap::new(),
            orders: BTreeMap::new(),
            deposits: BTreeMap::new(),
            forced: BTreeMap::new(),
        }
    }

    /// Non-flat positions of one market, sorted by account.
    pub fn holders(&self, market: &str) -> impl Iterator<Item = (&str, &Position)> {
        self.positions
            .range((market.to_string(), String::new())..)
            .take_while(move |((m, _), _)| m == market)
            .map(|((_, a), p)| (a.as_str(), p))
    }

    /// All positions of one account: (market, position).
    pub fn positions_of<'a>(&'a self, account: &'a str) -> impl Iterator<Item = (&'a str, &'a Position)> + 'a {
        self.positions
            .iter()
            .filter(move |((_, a), _)| a == account)
            .map(|((m, _), p)| (m.as_str(), p))
    }

    pub fn add_key(&mut self, pk: [u8; 32], k: KeyInfo) {
        self.keys.insert(pk, k);
    }
}

impl Store for MemStore {
    fn globals(&self) -> &Globals {
        &self.globals
    }
    fn globals_mut(&mut self) -> &mut Globals {
        &mut self.globals
    }
    fn market(&mut self, id: &str) -> Option<MarketState> {
        self.markets.get(id).cloned()
    }
    fn put_market(&mut self, id: &str, m: MarketState) {
        self.markets.insert(id.to_string(), m);
    }
    fn account(&mut self, a: &str) -> AccountState {
        self.accounts.get(a).cloned().unwrap_or_default()
    }
    fn put_account(&mut self, a: &str, s: AccountState) {
        if s == AccountState::default() {
            self.accounts.remove(a);
        } else {
            self.accounts.insert(a.to_string(), s);
        }
    }
    fn position(&mut self, a: &str, market: &str) -> Position {
        self.positions
            .get(&(market.to_string(), a.to_string()))
            .cloned()
            .unwrap_or_default()
    }
    fn put_position(&mut self, a: &str, market: &str, p: Position) {
        let k = (market.to_string(), a.to_string());
        if p.size == 0 && p.margin == 0 {
            self.positions.remove(&k);
        } else {
            self.positions.insert(k, p);
        }
    }
    fn key(&mut self, pk: &[u8; 32]) -> Option<KeyInfo> {
        self.keys.get(pk).cloned()
    }
    fn order(&mut self, hash: &[u8; 32]) -> Option<OrderState> {
        self.orders.get(hash).cloned()
    }
    fn put_order(&mut self, hash: &[u8; 32], o: OrderState) {
        self.orders.insert(*hash, o);
    }
    fn deposit(&mut self, id: u64) -> Option<PendingDeposit> {
        self.deposits.get(&id).cloned()
    }
    fn remove_deposit(&mut self, id: u64) {
        self.deposits.remove(&id);
    }
    fn forced(&mut self, id: u64) -> Option<ForcedRequest> {
        self.forced.get(&id).cloned()
    }
    fn put_forced(&mut self, id: u64, r: ForcedRequest) {
        self.forced.insert(id, r);
    }
}
