//! Chain state: block tree, cumulative-work fork choice, consensus state
//! (from `zfx_verify::state`), reorgs. Pure logic: time is passed in, no
//! I/O, so every rule is unit testable.

use crate::config::ChainConfig;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use zfx_verify::block::Block;
use zfx_verify::bridge::{self, BridgeOp};
use zfx_verify::difficulty::{check_timestamp, next_target, DifficultyParams, LWMA_WINDOW, MTP_WINDOW};
use zfx_verify::hash::{sha256, Hash32};
use zfx_verify::header::BlockHeader;
use zfx_verify::pow::{self, epoch_anchor_height, epoch_seed, profile_by_id, Profile, Weights};
use zfx_verify::state::{ConsensusParams, State};
use zfx_verify::tx::{Address, Coinbase, Transaction};
use zfx_verify::U256;

pub use zfx_verify::state::Account;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Header, PoW, structure and signatures valid; state not yet applied.
    Valid,
    /// Connected at least once (state transition and state_root valid).
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
    SideChain,
    NewTip { reorg_depth: u64 },
}

/// Bridge deposit record indexed from the best chain.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DepositRecord {
    pub deposit_id: String,
    pub block: String,
    pub height: u64,
    pub amount: u64,
    pub near_recipient: String,
    pub sender: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ReleaseRecord {
    pub withdrawal_id: String,
    pub op_id: String,
    pub block: String,
    pub height: u64,
    pub amount: u64,
}

/// Indexes derived from the best chain (rebuilt on reorg).
#[derive(Clone, Default)]
pub struct Indexes {
    pub tx: HashMap<Hash32, (Hash32, u64)>,
    pub by_address: HashMap<Address, Vec<(u64, Hash32)>>,
    pub deposits: HashMap<Hash32, DepositRecord>,
    pub releases: HashMap<Hash32, ReleaseRecord>,
    pub fees: VecDeque<u64>,
}

pub struct Chain {
    pub cfg: ChainConfig,
    pub params: ConsensusParams,
    pub profile: Profile,
    pub genesis_hash: Hash32,
    pub genesis_state: State,
    pub entries: HashMap<Hash32, Entry>,
    /// Best chain, index = height.
    pub best: Vec<Hash32>,
    pub state: State,
    pub idx: Indexes,
    weights: HashMap<Hash32, Arc<Weights>>,
    weights_lru: VecDeque<Hash32>,
    pub invalid_count: u64,
    pub reorg_count: u64,
    pub max_reorg_depth: u64,
    pub verify_threads: usize,
}

pub fn genesis_block(cfg: &ChainConfig, state_root: Hash32) -> Block {
    let header = BlockHeader {
        version: cfg.profile,
        chain_id: cfg.chain_id,
        height: 0,
        parent: [0u8; 32],
        tx_root: sha256(&[b"ZFX-genesis-v0.1", &cfg.commitment()]),
        state_root,
        timestamp: cfg.genesis_timestamp,
        target: cfg.genesis_target_u256().to_be_bytes(),
        nonce: 0,
    };
    Block { header, coinbase: Coinbase { height: 0, to: [0u8; 32], amount: 0 }, txs: vec![], bridge_ops: vec![] }
}

impl Chain {
    pub fn new(cfg: ChainConfig) -> Result<Chain, String> {
        if cfg.network == "mainnet" {
            return Err("mainnet activation is gated: it requires an owner-selected monetary policy, independent cryptanalysis, a security audit and owner sign-off (docs/RELEASE_READINESS.md); MAINNET_READY=false".into());
        }
        if cfg.network != "testnet" {
            return Err(format!("unknown network {:?}", cfg.network));
        }
        if cfg.consensus_version != 2 {
            return Err(format!("unsupported consensus_version {} (this node implements 2)", cfg.consensus_version));
        }
        let profile = profile_by_id(cfg.profile).ok_or("unknown PoW profile")?;
        if !profile.exact_in_fp32() {
            return Err("profile violates FP32 exactness bound".into());
        }
        let params = cfg.params();
        let state = State::genesis(&params, &cfg.allocations_parsed()?)?;
        let g = genesis_block(&cfg, state.root());
        let gh = g.hash();
        let mut entries = HashMap::new();
        entries.insert(gh, Entry { block: Arc::new(g), height: 0, cum_work: U256::ZERO, status: Status::Connected, pow_digest: [0; 32], seq: 0 });
        Ok(Chain {
            cfg,
            params,
            profile,
            genesis_hash: gh,
            genesis_state: state.clone(),
            entries,
            best: vec![gh],
            state,
            idx: Indexes::default(),
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
            solvetime_rule: zfx_verify::difficulty::SolvetimeRule::RunningMax,
            target_spacing_secs: self.cfg.target_spacing_secs,
            pow_limit: self.cfg.pow_limit_u256(),
            fixed: self.cfg.fixed_difficulty,
        }
    }

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

    pub fn anchor_for(&self, parent: &Hash32, height: u64) -> Option<Hash32> {
        self.ancestor(parent, epoch_anchor_height(height))
    }

    /// Full context-dependent header validation including PoW.
    pub fn validate_header(&mut self, h: &BlockHeader, now: u64) -> Result<Hash32, String> {
        let parent = self.entries.get(&h.parent).ok_or("unknown parent")?;
        if parent.status == Status::Invalid {
            return Err("parent is invalid".into());
        }
        if h.height != parent.height + 1 {
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

    /// Context-free checks that need no state: structure, duplicates and
    /// SLH-DSA authorization (parallel). Run before PoW is trusted? No: PoW
    /// first (cheap, 1 ms), then signatures (up to ~0.7 ms each).
    fn stateless_checks(&self, b: &Block) -> Result<(), String> {
        b.check_structure()?;
        let mut seen = HashSet::new();
        for t in &b.txs {
            if !seen.insert(t.txid()) {
                return Err("duplicate transaction in block".to_string());
            }
        }
        let mut ops = HashSet::new();
        for o in &b.bridge_ops {
            if !ops.insert(bridge::op_id(o)) {
                return Err("duplicate bridge op in block".to_string());
            }
        }
        let errs: Vec<String> = std::thread::scope(|s| {
            let chunk = b.txs.len().div_ceil(self.verify_threads).max(1);
            let hs: Vec<_> = b.txs.chunks(chunk).map(|c| s.spawn(move || c.iter().find_map(|t| t.verify_authorization().err()))).collect();
            hs.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        if let Some(e) = errs.into_iter().next() {
            return Err(format!("tx authorization: {}", e));
        }
        Ok(())
    }

    pub fn add_block(&mut self, b: Block, now: u64) -> Result<AddResult, String> {
        let hash = b.hash();
        if self.entries.contains_key(&hash) {
            return Ok(AddResult::Duplicate);
        }
        if !self.entries.contains_key(&b.header.parent) {
            return Ok(AddResult::Orphan);
        }
        let digest = match self.validate_header(&b.header, now).and_then(|d| self.stateless_checks(&b).map(|_| d)) {
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

    /// Apply one block to `st`, checking the committed state_root.
    pub fn connect_one(params: &ConsensusParams, st: &mut State, b: &Block, idx: Option<&mut Indexes>) -> Result<(), String> {
        let eff = st.apply_block(params, b)?;
        let root = st.root();
        if root != b.header.state_root {
            return Err(format!("state_root mismatch: header {} computed {}", hex::encode(b.header.state_root), hex::encode(root)));
        }
        if let Some(ix) = idx {
            let bh = b.hash();
            let h = b.header.height;
            for t in &b.txs {
                let id = t.txid();
                ix.tx.insert(id, (bh, h));
                ix.by_address.entry(t.from).or_default().push((h, id));
                ix.by_address.entry(t.to).or_default().push((h, id));
            }
            for (dep, amt, memo) in eff.deposits {
                let sender = b.txs.iter().find(|t| t.txid() == dep).map(|t| zfx_verify::tx::address_to_string(&t.from)).unwrap_or_default();
                ix.deposits
                    .insert(dep, DepositRecord { deposit_id: hex::encode(dep), block: hex::encode(bh), height: h, amount: amt, near_recipient: memo, sender });
            }
            for (op, wid, amt) in eff.releases {
                ix.releases
                    .insert(wid, ReleaseRecord { withdrawal_id: hex::encode(wid), op_id: hex::encode(op), block: hex::encode(bh), height: h, amount: amt });
            }
            ix.fees.push_back(if b.txs.is_empty() { 0 } else { eff.fees / b.txs.len() as u64 });
            if ix.fees.len() > 100 {
                ix.fees.pop_front();
            }
        }
        Ok(())
    }

    fn connect_path(&self, path: &[Hash32]) -> Result<(State, Indexes), (usize, String)> {
        let mut st = self.genesis_state.clone();
        let mut idx = Indexes::default();
        for (i, h) in path.iter().enumerate().skip(1) {
            Self::connect_one(&self.params, &mut st, &self.entries[h].block, Some(&mut idx)).map_err(|e| (i, e))?;
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

    /// Switch to the valid tip with the most cumulative work. Extending the
    /// current tip applies one block incrementally; a true reorg replays
    /// state from genesis along the candidate path.
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
            let ce = self.entries[&cand].clone();
            if Some(&ce.block.header.parent) == self.best.last() {
                // Fast path: extends the current tip.
                let mut st = self.state.clone();
                let mut idx = self.idx.clone();
                match Self::connect_one(&self.params, &mut st, &ce.block, Some(&mut idx)) {
                    Ok(()) => {
                        self.state = st;
                        self.idx = idx;
                        self.best.push(cand);
                        self.entries.get_mut(&cand).unwrap().status = Status::Connected;
                        return Ok(AddResult::NewTip { reorg_depth: 0 });
                    }
                    Err(_) => {
                        self.mark_invalid_from(cand);
                        continue;
                    }
                }
            }
            let path = self.path_to(&cand);
            match self.connect_path(&path) {
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
                    self.idx = idx;
                    if depth > 0 {
                        self.reorg_count += 1;
                        self.max_reorg_depth = self.max_reorg_depth.max(depth);
                    }
                    return Ok(AddResult::NewTip { reorg_depth: depth });
                }
                Err((i, _e)) => self.mark_invalid_from(path[i]),
            }
        }
    }

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

    pub fn hashes_after_locator(&self, loc: &[Hash32], max: usize) -> Vec<Hash32> {
        let start = loc.iter().find_map(|h| self.entries.get(h).filter(|e| self.best.get(e.height as usize) == Some(h)).map(|e| e.height)).unwrap_or(0);
        self.best.iter().skip(start as usize + 1).take(max).copied().collect()
    }

    pub fn block_at(&self, height: u64) -> Option<&Arc<Block>> {
        self.best.get(height as usize).map(|h| &self.entries[h].block)
    }

    /// Mining template on the current tip, with the post-state root filled in.
    pub fn template(&self, miner: Address, txs: &[Transaction], now: u64) -> Block {
        self.template_with_ops(miner, txs, &[], now)
    }

    pub fn template_with_ops(&self, miner: Address, txs: &[Transaction], ops: &[BridgeOp], now: u64) -> Block {
        let parent = *self.best.last().unwrap();
        self.template_on(parent, &self.state, miner, txs, ops, now)
    }

    /// Consensus state after `hash` (replays from genesis unless `hash` is
    /// the tip). Used for fork construction and audits.
    pub fn state_at(&self, hash: &Hash32) -> Result<State, String> {
        if Some(hash) == self.best.last() {
            return Ok(self.state.clone());
        }
        let path = self.path_to(hash);
        self.connect_path(&path).map(|(s, _)| s).map_err(|(_, e)| e)
    }

    /// Template on an arbitrary parent whose post-state is `base`.
    pub fn template_on(&self, parent: Hash32, base: &State, miner: Address, txs: &[Transaction], ops: &[BridgeOp], now: u64) -> Block {
        let height = self.entries[&parent].height + 1;
        let ts = now.max(self.mtp(&parent) + 1);
        let mut st = base.clone();
        // Apply maturity + rotation exactly as apply_block will, by building
        // the block incrementally and re-validating each candidate item.
        let mut chosen = Vec::new();
        let mut chosen_ops = Vec::new();
        let mut size = zfx_verify::header::HEADER_LEN + 64;
        let probe = |txs: &Vec<Transaction>, ops: &Vec<BridgeOp>| -> Option<(State, u64)> {
            let mut s = base.clone();
            let b = Block {
                header: BlockHeader { version: 0, chain_id: 0, height, parent, tx_root: [0; 32], state_root: [0; 32], timestamp: 0, target: [0; 32], nonce: 0 },
                coinbase: Coinbase { height, to: miner, amount: 0 },
                txs: txs.clone(),
                bridge_ops: ops.clone(),
            };
            let fees: u64 = txs.iter().map(|t| t.fee).sum();
            let mut b2 = b;
            b2.coinbase.amount = self.params.subsidy.subsidy_at(height) + fees;
            s.apply_block(&self.params, &b2).ok().map(|_| (s, fees))
        };
        for t in txs {
            if chosen.len() >= zfx_verify::block::MAX_TXS_PER_BLOCK || size + t.encoded_len() > zfx_verify::block::MAX_BLOCK_BYTES {
                break;
            }
            let mut try_txs = chosen.clone();
            try_txs.push(t.clone());
            if probe(&try_txs, &chosen_ops).is_some() {
                size += t.encoded_len();
                chosen = try_txs;
            }
        }
        for o in ops {
            if chosen_ops.len() >= bridge::MAX_BRIDGE_OPS_PER_BLOCK {
                break;
            }
            let mut try_ops = chosen_ops.clone();
            try_ops.push(o.clone());
            if probe(&chosen, &try_ops).is_some() {
                chosen_ops = try_ops;
            }
        }
        let fees: u64 = chosen.iter().map(|t| t.fee).sum();
        let coinbase = Coinbase { height, to: miner, amount: self.params.subsidy.subsidy_at(height) + fees };
        let mut b = Block {
            header: BlockHeader {
                version: self.cfg.profile,
                chain_id: self.cfg.chain_id,
                height,
                parent,
                tx_root: [0; 32],
                state_root: [0; 32],
                timestamp: ts,
                target: self.expected_target(&parent).to_be_bytes(),
                nonce: 0,
            },
            coinbase,
            txs: chosen,
            bridge_ops: chosen_ops,
        };
        st.apply_block(&self.params, &b).expect("template must apply");
        b.header.state_root = st.root();
        b.header.tx_root = b.compute_tx_root();
        b
    }

    pub fn epoch_seed_for_next(&self) -> Hash32 {
        let parent = *self.best.last().unwrap();
        let anchor = self.anchor_for(&parent, self.height() + 1).unwrap();
        epoch_seed(&anchor, &self.profile)
    }

    /// Median per-tx fee over recent blocks (fee estimation).
    pub fn fee_estimate(&self) -> u64 {
        let mut v: Vec<u64> = self.idx.fees.iter().copied().filter(|f| *f > 0).collect();
        if v.is_empty() {
            return 1000;
        }
        v.sort_unstable();
        v[v.len() / 2].max(1000)
    }
}
