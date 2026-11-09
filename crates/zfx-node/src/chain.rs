//! Chain state: block tree, cumulative-work fork choice, account state,
//! reorgs. Pure logic: time is passed in, no I/O, so every rule is unit
//! testable.

use crate::config::ChainConfig;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use zfx_verify::block::Block;
use zfx_verify::difficulty::{check_timestamp, next_target, DifficultyParams, MTP_WINDOW, LWMA_WINDOW};
use zfx_verify::hash::{sha256, Hash32};
use zfx_verify::header::BlockHeader;
use zfx_verify::pow::{self, epoch_anchor_height, epoch_seed, profile_by_id, Profile, Weights};
use zfx_verify::tx::{Address, Coinbase, Transaction};
use zfx_verify::U256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Account {
    pub balance: u64,
    pub nonce: u64,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub accounts: HashMap<Address, Account>,
    pub supply: u128,
}

impl State {
    pub fn account(&self, a: &Address) -> Account {
        self.accounts.get(a).copied().unwrap_or_default()
    }

    /// Stateful tx check + apply. Signatures are checked separately.
    pub fn apply_tx(&mut self, tx: &Transaction) -> Result<(), String> {
        if tx.from == tx.to {
            return Err("self-transfer not allowed".into());
        }
        let from = self.account(&tx.from);
        if tx.nonce != from.nonce {
            return Err(format!("bad nonce {} (expected {})", tx.nonce, from.nonce));
        }
        let cost = tx.amount.checked_add(tx.fee).ok_or("amount overflow")?;
        if from.balance < cost {
            return Err(format!("insufficient balance {} < {}", from.balance, cost));
        }
        let to = self.account(&tx.to);
        let to_bal = to.balance.checked_add(tx.amount).ok_or("recipient overflow")?;
        self.accounts.insert(tx.from, Account { balance: from.balance - cost, nonce: from.nonce + 1 });
        self.accounts.insert(tx.to, Account { balance: to_bal, nonce: to.nonce });
        self.supply -= tx.fee as u128; // fees are re-minted via coinbase
        Ok(())
    }

    pub fn apply_coinbase(&mut self, cb: &Coinbase) -> Result<(), String> {
        let a = self.account(&cb.to);
        let bal = a.balance.checked_add(cb.amount).ok_or("coinbase overflow")?;
        self.accounts.insert(cb.to, Account { balance: bal, nonce: a.nonce });
        self.supply += cb.amount as u128;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Header, PoW, structure and signatures valid; state not yet applied.
    Valid,
    /// Fully connected at least once (state transitions valid).
    Connected,
    Invalid,
}

#[derive(Clone)]
pub struct Entry {
    pub block: Arc<Block>,
    pub height: u64,
    pub cum_work: U256,
    pub status: Status,
    pub pow_digest: Hash32,
    /// Arrival order; first-seen wins work ties.
    pub seq: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddResult {
    Duplicate,
    Orphan,
    /// Stored on a side branch.
    SideChain,
    /// Became the new tip. `reorg_depth` = blocks disconnected.
    NewTip { reorg_depth: u64 },
}

pub struct Chain {
    pub cfg: ChainConfig,
    pub profile: Profile,
    pub genesis_hash: Hash32,
    pub entries: HashMap<Hash32, Entry>,
    /// Best chain, index = height.
    pub best: Vec<Hash32>,
    pub state: State,
    pub tx_index: HashMap<Hash32, (Hash32, u64)>,
    weights: HashMap<Hash32, Arc<Weights>>,
    weights_lru: VecDeque<Hash32>,
    pub invalid_count: u64,
    pub reorg_count: u64,
    pub max_reorg_depth: u64,
    pub verify_threads: usize,
}

pub fn genesis_block(cfg: &ChainConfig) -> Block {
    let header = BlockHeader {
        version: cfg.profile,
        chain_id: cfg.chain_id,
        height: 0,
        parent: [0u8; 32],
        tx_root: sha256(&[b"ZFX-genesis-v0.1", &cfg.commitment()]),
        timestamp: cfg.genesis_timestamp,
        target: cfg.genesis_target_u256().to_be_bytes(),
        nonce: 0,
    };
    Block { header, coinbase: Coinbase { height: 0, to: [0u8; 32], amount: 0 }, txs: vec![] }
}

impl Chain {
    pub fn new(cfg: ChainConfig) -> Result<Chain, String> {
        let profile = profile_by_id(cfg.profile).ok_or("unknown PoW profile")?;
        if !profile.exact_in_fp32() {
            return Err("profile violates FP32 exactness bound".into());
        }
        let g = genesis_block(&cfg);
        let gh = g.hash();
        let mut state = State::default();
        for (a, v) in cfg.allocations_parsed()? {
            state.apply_coinbase(&Coinbase { height: 0, to: a, amount: v })?;
        }
        let mut entries = HashMap::new();
        entries.insert(gh, Entry { block: Arc::new(g), height: 0, cum_work: U256::ZERO, status: Status::Connected, pow_digest: [0; 32], seq: 0 });
        Ok(Chain {
            cfg,
            profile,
            genesis_hash: gh,
            entries,
            best: vec![gh],
            state,
            tx_index: HashMap::new(),
            weights: HashMap::new(),
            weights_lru: VecDeque::new(),
            invalid_count: 0,
            reorg_count: 0,
            max_reorg_depth: 0,
            verify_threads: std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1).min(16),
        })
    }

    pub fn tip(&self) -> &Entry {
        &self.entries[self.best.last().unwrap()]
    }

    pub fn height(&self) -> u64 {
        self.best.len() as u64 - 1
    }

    pub fn diff_params(&self) -> DifficultyParams {
        DifficultyParams {
            target_spacing_secs: self.cfg.target_spacing_secs,
            pow_limit: self.cfg.pow_limit_u256(),
            fixed: self.cfg.fixed_difficulty,
        }
    }

    /// Hash of the ancestor of `from` (inclusive) at `height`.
    pub fn ancestor(&self, from: &Hash32, height: u64) -> Option<Hash32> {
        let mut h = *from;
        loop {
            let e = self.entries.get(&h)?;
            if e.height == height {
                return Some(h);
            }
            if e.height < height {
                return None;
            }
            if self.best.get(e.height as usize) == Some(&h) {
                return self.best.get(height as usize).copied();
            }
            h = e.block.header.parent;
        }
    }

    /// Up to `n` (timestamp, target) pairs ending at `from`, oldest first.
    fn history(&self, from: &Hash32, n: usize) -> Vec<(u64, U256)> {
        let mut v = Vec::with_capacity(n);
        let mut h = *from;
        while v.len() < n {
            let Some(e) = self.entries.get(&h) else { break };
            v.push((e.block.header.timestamp, e.block.header.target_u256()));
            if e.height == 0 {
                break;
            }
            h = e.block.header.parent;
        }
        v.reverse();
        v
    }

    pub fn expected_target(&self, parent: &Hash32) -> U256 {
        let hist = self.history(parent, LWMA_WINDOW + 1);
        // Genesis is excluded from retarget windows: its timestamp is fixed.
        let hist: Vec<(u64, U256)> = if self.entries[parent].height < LWMA_WINDOW as u64 + 1 { vec![] } else { hist };
        next_target(&self.diff_params(), &self.cfg.genesis_target_u256(), &hist)
    }

    pub fn mtp(&self, parent: &Hash32) -> u64 {
        let ts: Vec<u64> = self.history(parent, MTP_WINDOW).iter().map(|x| x.0).collect();
        zfx_verify::difficulty::median_time_past(&ts)
    }

    pub fn weights_for(&mut self, anchor: &Hash32) -> Arc<Weights> {
        let seed = epoch_seed(anchor, &self.profile);
        if let Some(w) = self.weights.get(&seed) {
            return w.clone();
        }
        let w = Arc::new(Weights::generate_par(&self.profile, &seed, self.verify_threads));
        self.weights.insert(seed, w.clone());
        self.weights_lru.push_back(seed);
        while self.weights_lru.len() > 4 {
            let old = self.weights_lru.pop_front().unwrap();
            self.weights.remove(&old);
        }
        w
    }

    /// Epoch anchor hash for a block at `height` whose parent is `parent`.
    pub fn anchor_for(&self, parent: &Hash32, height: u64) -> Option<Hash32> {
        self.ancestor(parent, epoch_anchor_height(height))
    }

    /// Full context-dependent header validation including PoW.
    pub fn validate_header(&mut self, h: &BlockHeader, now: u64) -> Result<Hash32, String> {
        let parent = self.entries.get(&h.parent).ok_or("unknown parent")?;
        if parent.status == Status::Invalid {
            return Err("parent is invalid".into());
        }
        let ph = parent.height;
        if h.height != ph + 1 {
            return Err(format!("height {} != parent height + 1", h.height));
        }
        if h.version != self.cfg.profile {
            return Err(format!("version {} != chain profile {}", h.version, self.cfg.profile));
        }
        if h.chain_id != self.cfg.chain_id {
            return Err("wrong chain id".into());
        }
        let anc_ts: Vec<u64> = self.history(&h.parent, MTP_WINDOW).iter().map(|x| x.0).collect();
        check_timestamp(h.timestamp, &anc_ts, now, self.cfg.max_future_secs)?;
        let want = self.expected_target(&h.parent);
        if h.target_u256() != want {
            return Err(format!("target {} != expected {}", h.target_u256().to_hex(), want.to_hex()));
        }
        let anchor = self.anchor_for(&h.parent, h.height).ok_or("missing epoch anchor")?;
        let w = self.weights_for(&anchor);
        pow::verify_header_pow(h, &w, &anchor).map_err(|e| format!("pow: {:?}", e))
    }

    /// Add a block. Signatures and PoW are checked here (stateless);
    /// balances/nonces when the block is connected to the best chain.
    pub fn add_block(&mut self, b: Block, now: u64) -> Result<AddResult, String> {
        let hash = b.hash();
        if self.entries.contains_key(&hash) {
            return Ok(AddResult::Duplicate);
        }
        if !self.entries.contains_key(&b.header.parent) {
            return Ok(AddResult::Orphan);
        }
        let r = (|| {
            b.check_structure()?;
            let digest = self.validate_header(&b.header, now)?;
            let mut seen = HashSet::new();
            for t in &b.txs {
                if !seen.insert(t.txid()) {
                    return Err("duplicate transaction in block".to_string());
                }
            }
            // SLH-DSA verification parallelized across threads.
            let errs: Vec<String> = std::thread::scope(|s| {
                let chunk = b.txs.len().div_ceil(self.verify_threads).max(1);
                let hs: Vec<_> = b.txs.chunks(chunk).map(|c| s.spawn(move || c.iter().find_map(|t| t.verify_authorization().err()))).collect();
                hs.into_iter().filter_map(|h| h.join().unwrap()).collect()
            });
            if let Some(e) = errs.into_iter().next() {
                return Err(format!("tx authorization: {}", e));
            }
            Ok(digest)
        })();
        let digest = match r {
            Ok(d) => d,
            Err(e) => {
                self.invalid_count += 1;
                return Err(e);
            }
        };
        let parent = &self.entries[&b.header.parent];
        let cum = parent.cum_work.saturating_add(&U256::work_for_target(&b.header.target_u256()));
        let height = parent.height + 1;
        let seq = self.entries.len() as u64;
        self.entries.insert(hash, Entry { block: Arc::new(b), height, cum_work: cum, status: Status::Valid, pow_digest: digest, seq });
        if cum > self.tip().cum_work {
            self.reorganize()
        } else {
            Ok(AddResult::SideChain)
        }
    }

    /// Path of hashes from genesis to `tip`.
    fn path_to(&self, tip: &Hash32) -> Vec<Hash32> {
        let mut v = Vec::new();
        let mut h = *tip;
        loop {
            v.push(h);
            let e = &self.entries[&h];
            if e.height == 0 {
                break;
            }
            h = e.block.header.parent;
        }
        v.reverse();
        v
    }

    fn connect_state(&self, path: &[Hash32]) -> Result<(State, HashMap<Hash32, (Hash32, u64)>), (usize, String)> {
        let mut st = State::default();
        for (a, v) in self.cfg.allocations_parsed().map_err(|e| (0, e))? {
            st.apply_coinbase(&Coinbase { height: 0, to: a, amount: v }).map_err(|e| (0, e))?;
        }
        let mut idx = HashMap::new();
        for (i, h) in path.iter().enumerate().skip(1) {
            let b = &self.entries[h].block;
            let mut fees: u64 = 0;
            for t in &b.txs {
                st.apply_tx(t).map_err(|e| (i, e))?;
                fees = fees.checked_add(t.fee).ok_or((i, "fee overflow".to_string()))?;
                idx.insert(t.txid(), (*h, b.header.height));
            }
            let want = self.cfg.block_subsidy.checked_add(fees).ok_or((i, "reward overflow".to_string()))?;
            if b.coinbase.amount != want {
                return Err((i, format!("coinbase {} != subsidy+fees {}", b.coinbase.amount, want)));
            }
            st.apply_coinbase(&b.coinbase).map_err(|e| (i, e))?;
        }
        Ok((st, idx))
    }

    fn mark_invalid_from(&mut self, bad: Hash32) {
        let mut stack = vec![bad];
        while let Some(h) = stack.pop() {
            if let Some(e) = self.entries.get_mut(&h) {
                e.status = Status::Invalid;
            }
            let kids: Vec<Hash32> = self.entries.iter().filter(|(_, e)| e.block.header.parent == h && e.status != Status::Invalid).map(|(k, _)| *k).collect();
            stack.extend(kids);
        }
        self.invalid_count += 1;
    }

    /// Switch to the valid tip with most cumulative work. Replays state
    /// from genesis along the candidate path (O(chain); fine for testnet).
    fn reorganize(&mut self) -> Result<AddResult, String> {
        loop {
            let cand = self
                .entries
                .iter()
                .filter(|(_, e)| e.status != Status::Invalid)
                .max_by(|a, b| {
                    a.1.cum_work
                        .cmp(&b.1.cum_work)
                        .then_with(|| (self.best.last() == Some(a.0)).cmp(&(self.best.last() == Some(b.0))))
                        .then_with(|| b.1.seq.cmp(&a.1.seq))
                })
                .map(|(h, _)| *h)
                .unwrap();
            if Some(&cand) == self.best.last() {
                return Ok(AddResult::SideChain);
            }
            let path = self.path_to(&cand);
            match self.connect_state(&path) {
                Ok((st, idx)) => {
                    let fork = self.best.iter().zip(&path).take_while(|(a, b)| a == b).count();
                    let depth = (self.best.len() - fork) as u64;
                    for h in &path {
                        if let Some(e) = self.entries.get_mut(h) {
                            e.status = Status::Connected;
                        }
                    }
                    self.best = path;
                    self.state = st;
                    self.tx_index = idx;
                    if depth > 0 {
                        self.reorg_count += 1;
                        self.max_reorg_depth = self.max_reorg_depth.max(depth);
                    }
                    return Ok(AddResult::NewTip { reorg_depth: depth });
                }
                Err((i, _e)) => {
                    self.mark_invalid_from(path[i]);
                }
            }
        }
    }

    /// Locator: best-chain hashes at heights tip, tip-1, ..., then
    /// exponentially spaced back to genesis.
    pub fn locator(&self) -> Vec<Hash32> {
        let mut v = Vec::new();
        let mut step = 1u64;
        let mut h = self.height() as i64;
        while h > 0 {
            v.push(self.best[h as usize]);
            if v.len() >= 10 {
                step *= 2;
            }
            h -= step as i64;
        }
        v.push(self.genesis_hash);
        v
    }

    /// Given a peer's locator, best-chain hashes after the fork point.
    pub fn hashes_after_locator(&self, loc: &[Hash32], max: usize) -> Vec<Hash32> {
        let start = loc
            .iter()
            .find_map(|h| self.entries.get(h).filter(|e| self.best.get(e.height as usize) == Some(h)).map(|e| e.height))
            .unwrap_or(0);
        self.best.iter().skip(start as usize + 1).take(max).copied().collect()
    }

    pub fn block_at(&self, height: u64) -> Option<&Arc<Block>> {
        self.best.get(height as usize).map(|h| &self.entries[h].block)
    }

    /// Mining template on the current tip.
    pub fn template(&self, miner: Address, txs: &[Transaction], now: u64) -> Block {
        let tip = self.tip();
        let parent = *self.best.last().unwrap();
        let height = tip.height + 1;
        let ts = now.max(self.mtp(&parent) + 1);
        let mut st = self.state.clone();
        let mut chosen = Vec::new();
        let mut fees = 0u64;
        let mut size = zfx_verify::header::HEADER_LEN + 52;
        for t in txs {
            if chosen.len() >= zfx_verify::block::MAX_TXS_PER_BLOCK || size + t.encoded_len() > zfx_verify::block::MAX_BLOCK_BYTES {
                break;
            }
            let mut s2 = st.clone();
            if s2.apply_tx(t).is_ok() {
                st = s2;
                fees += t.fee;
                size += t.encoded_len();
                chosen.push(t.clone());
            }
        }
        let coinbase = Coinbase { height, to: miner, amount: self.cfg.block_subsidy + fees };
        let mut b = Block {
            header: BlockHeader {
                version: self.cfg.profile,
                chain_id: self.cfg.chain_id,
                height,
                parent,
                tx_root: [0; 32],
                timestamp: ts,
                target: self.expected_target(&parent).to_be_bytes(),
                nonce: 0,
            },
            coinbase,
            txs: chosen,
        };
        b.header.tx_root = b.compute_tx_root();
        b
    }

    pub fn epoch_seed_for_next(&self) -> Hash32 {
        let parent = *self.best.last().unwrap();
        let anchor = self.anchor_for(&parent, self.height() + 1).unwrap();
        epoch_seed(&anchor, &self.profile)
    }
}
