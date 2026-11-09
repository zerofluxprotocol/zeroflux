//! Networked node: encrypted P2P (ZFXP2P/2, see p2p_crypto.rs), HTTP and
//! JSON-RPC APIs, persistence. Consensus decisions are made only by `Chain`
//! using zfx-verify; nothing a peer or miner sends is trusted (the node
//! recomputes every PoW digest, signature and state transition).

use crate::chain::{AddResult, Chain};
use crate::config::ChainConfig;
use crate::mempool::Mempool;
use crate::p2p_crypto::{self, Identity, SecureWriter};
use crate::store::{BlockLog, LoadError, Snapshot};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zfx_verify::block::{merkle_proof, Block};
use zfx_verify::bridge::{self, BridgeOp};
use zfx_verify::hash::{from_hex32, to_hex, Hash32};
use zfx_verify::tx::{address_from_string, address_to_string, Address, P2mrKeys, Transaction};

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_ORPHANS: usize = 256;
pub const MAX_INV: usize = 500;
pub const MAX_ADDRS: usize = 1000;
pub const BAN_SECS: u64 = 600;
/// Per-peer message budget: tokens refill at RATE per second up to BURST.
pub const PEER_MSG_RATE: f64 = 500.0;
pub const PEER_MSG_BURST: f64 = 2000.0;

pub fn now_secs() -> u64 {
    let off: i64 = std::env::var("ZFX_TIME_OFFSET").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 + off) as u64
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Msg {
    Hello { protocol: u32, chain_id: u32, genesis: String, height: u64, best: String, listen: Option<u16> },
    GetBlocks { locator: Vec<String> },
    Inv { hashes: Vec<String> },
    GetData { hashes: Vec<String> },
    Block { block: Block },
    Tx { tx: Transaction },
    BridgeOp { op: BridgeOp },
    GetAddr {},
    Addr { addrs: Vec<String> },
}

pub struct Peer {
    pub addr: SocketAddr,
    pub peer_id: Hash32,
    pub tx: Sender<Vec<u8>>,
    pub height: u64,
    pub outbound: bool,
    pub handshaken: bool,
    pub listen: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct FaucetConfig {
    pub seed: [u8; 32],
    pub amount: u64,
    pub cooldown_secs: u64,
}

#[derive(Clone, Debug)]
pub struct NodeOptions {
    pub data_dir: Option<String>,
    pub listen_port: u16,
    pub default_miner: Option<Address>,
    pub repair: bool,
    /// Bearer token for admin RPC. None: admin RPC disabled.
    pub admin_token: Option<String>,
    pub faucet: Option<FaucetConfig>,
    /// Permissioned networks: only these peer ids may connect.
    pub peer_allow: Option<Vec<Hash32>>,
    pub max_peers: usize,
    pub max_outbound: usize,
    /// Bridge deposit confirmation policy K (relayer-facing status).
    pub bridge_confirmations: u64,
    /// Identity seed (tests); otherwise data_dir/node_key or random.
    pub identity_seed: Option<[u8; 32]>,
    pub snapshot_interval: u64,
}

impl Default for NodeOptions {
    fn default() -> Self {
        NodeOptions {
            data_dir: None,
            listen_port: 9440,
            default_miner: None,
            repair: false,
            admin_token: None,
            faucet: None,
            peer_allow: None,
            max_peers: 32,
            max_outbound: 8,
            bridge_confirmations: 12,
            identity_seed: None,
            snapshot_interval: 100,
        }
    }
}

pub struct Inner {
    pub chain: Chain,
    pub mempool: Mempool,
    pub peers: HashMap<u64, Peer>,
    pub next_peer_id: u64,
    pub orphans: HashMap<Hash32, Block>,
    pub works: HashMap<String, Block>,
    pub bans: HashMap<std::net::IpAddr, Instant>,
    pub banned_ids: HashMap<Hash32, Instant>,
    pub submit_buckets: HashMap<std::net::IpAddr, (Instant, u32)>,
    pub faucet_last: HashMap<Address, u64>,
    pub store: Option<BlockLog>,
    pub addr_book: HashSet<String>,
    pub opts: NodeOptions,
    pub stats: Stats,
    pub started: Instant,
    pub load_report: Value,
}

#[derive(Default, Serialize, Clone)]
pub struct Stats {
    pub blocks_accepted: u64,
    pub blocks_rejected: u64,
    pub submissions: u64,
    pub submissions_accepted: u64,
    pub submissions_rate_limited: u64,
    pub peers_banned: u64,
    pub handshakes_ok: u64,
    pub handshakes_failed: u64,
    pub frames_rejected: u64,
    pub last_pow_verify_ms: f64,
    pub faucet_payouts: u64,
}

#[derive(Clone)]
pub struct Node {
    pub inner: Arc<Mutex<Inner>>,
    pub identity: Arc<Identity>,
    pub shutdown: Arc<AtomicBool>,
}

fn encode(m: &Msg) -> Vec<u8> {
    serde_json::to_vec(m).unwrap()
}

fn send(p: &Peer, m: &Msg) {
    let _ = p.tx.send(encode(m));
}

impl Node {
    /// Compatibility constructor used by tests.
    pub fn new(cfg: ChainConfig, data_dir: Option<&str>, default_miner: Option<Address>, listen_port: u16) -> Result<Node, String> {
        Node::with_options(cfg, NodeOptions { data_dir: data_dir.map(String::from), default_miner, listen_port, ..Default::default() })
    }

    pub fn with_options(cfg: ChainConfig, opts: NodeOptions) -> Result<Node, String> {
        let mut chain = Chain::new(cfg.clone())?;
        let mut store = None;
        let mut load_report = json!({});
        let identity = match (&opts.identity_seed, &opts.data_dir) {
            (Some(s), _) => Identity::from_seed(s),
            (None, Some(d)) => {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
                Identity::load_or_create(&std::path::Path::new(d).join("node_key"))?
            }
            (None, None) => {
                let mut s = [0u8; 32];
                rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut s);
                Identity::from_seed(&s)
            }
        };
        if let Some(d) = &opts.data_dir {
            std::fs::create_dir_all(format!("{}/snapshots", d)).map_err(|e| e.to_string())?;
            let cpath = format!("{}/chain.json", d);
            if let Ok(old) = std::fs::read_to_string(&cpath) {
                let old: ChainConfig = serde_json::from_str(&old).map_err(|e| e.to_string())?;
                if old.commitment() != cfg.commitment() {
                    return Err("data dir belongs to a different chain config".into());
                }
            } else {
                std::fs::write(&cpath, serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())?;
            }
            let clean_marker = std::path::Path::new(d).join("CLEAN_SHUTDOWN");
            let was_clean = clean_marker.exists();
            let _ = std::fs::remove_file(&clean_marker);
            let bpath = std::path::Path::new(d).join("blocks.zfxlog");
            let t = Instant::now();
            let (blocks, rep) = BlockLog::load(&bpath, opts.repair).map_err(|e: LoadError| e.to_string())?;
            let mut n = 0;
            let mut rejected = 0;
            for b in blocks {
                // Blocks are fully revalidated. Timestamps of old blocks are in
                // the past, so the future-time rule cannot reject them.
                match chain.add_block(b, now_secs()) {
                    Ok(_) => n += 1,
                    Err(_) => rejected += 1,
                }
            }
            chain.state.audit_with(cfg.chain_id)?;
            eprintln!(
                "[node] loaded {} blocks ({} rejected on revalidation) from {} in {:.2}s; height {}; previous shutdown {}",
                n,
                rejected,
                bpath.display(),
                t.elapsed().as_secs_f64(),
                chain.height(),
                if was_clean { "clean" } else { "UNCLEAN (crash recovery performed)" }
            );
            load_report = json!({"records": rep.records, "torn_tail_dropped": rep.torn_tail_dropped, "repaired_from_line": rep.repaired_from_line,
                                  "revalidated": n, "rejected": rejected, "previous_shutdown_clean": was_clean, "height": chain.height()});
            store = Some(BlockLog::open(&bpath)?);
        }
        let node = Node {
            inner: Arc::new(Mutex::new(Inner {
                chain,
                mempool: Mempool::default(),
                peers: HashMap::new(),
                next_peer_id: 1,
                orphans: HashMap::new(),
                works: HashMap::new(),
                bans: HashMap::new(),
                banned_ids: HashMap::new(),
                submit_buckets: HashMap::new(),
                faucet_last: HashMap::new(),
                store,
                addr_book: HashSet::new(),
                opts,
                stats: Stats::default(),
                started: Instant::now(),
                load_report,
            })),
            identity: Arc::new(identity),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        Ok(node)
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    pub fn peer_id_hex(&self) -> String {
        hex::encode(self.identity.peer_id)
    }

    /// Core block intake used by P2P, the miner API and tests.
    pub fn process_block(&self, b: Block, from_peer: Option<u64>) -> Result<AddResult, String> {
        let mut g = self.lock();
        let hash = b.hash();
        let old_best = g.chain.best.clone();
        let t = Instant::now();
        let r = g.chain.add_block(b.clone(), now_secs());
        g.stats.last_pow_verify_ms = t.elapsed().as_secs_f64() * 1e3;
        match &r {
            Ok(AddResult::Orphan) => {
                if g.orphans.len() >= MAX_ORPHANS {
                    let k = *g.orphans.keys().next().unwrap();
                    g.orphans.remove(&k);
                }
                g.orphans.insert(hash, b);
                if let Some(pid) = from_peer {
                    let loc: Vec<String> = g.chain.locator().iter().map(|h| to_hex(h)).collect();
                    if let Some(p) = g.peers.get(&pid) {
                        send(p, &Msg::GetBlocks { locator: loc });
                    }
                }
            }
            Ok(AddResult::Duplicate) => {}
            Ok(res) => {
                g.stats.blocks_accepted += 1;
                if let Some(s) = g.store.as_mut() {
                    if let Err(e) = s.append(&b) {
                        eprintln!("[node] FATAL: cannot persist block: {}", e);
                    }
                }
                if let AddResult::NewTip { reorg_depth } = res {
                    g.works.clear();
                    let p = g.chain.params.clone();
                    let h = g.chain.height() + 1;
                    if *reorg_depth > 0 {
                        eprintln!("[node] reorg depth {} -> height {}", reorg_depth, g.chain.height());
                        let new: HashSet<Hash32> = g.chain.best.iter().copied().collect();
                        let dis: Vec<Block> = old_best.iter().filter(|x| !new.contains(*x)).map(|x| (*g.chain.entries[x].block).clone()).collect();
                        let st = g.chain.state.clone();
                        for blk in dis {
                            for t in blk.txs {
                                let _ = g.mempool.admit(t, &p, h, &st);
                            }
                            for o in blk.bridge_ops {
                                let _ = g.mempool.admit_op(o, &p, h, &st);
                            }
                        }
                    }
                    let st = g.chain.state.clone();
                    g.mempool.revalidate(&p, h, &st);
                    let iv = g.opts.snapshot_interval;
                    let ch = g.chain.height();
                    if iv > 0 && ch.is_multiple_of(iv) {
                        if let Some(d) = g.opts.data_dir.clone() {
                            let snap = Snapshot::new(&g.chain.cfg.commitment(), ch, g.chain.best.last().unwrap(), &g.chain.state);
                            let _ = snap.write_atomic(&std::path::Path::new(&d).join("snapshots").join("latest.json"));
                        }
                    }
                }
                let inv = Msg::Inv { hashes: vec![to_hex(&hash)] };
                for (id, p) in g.peers.iter() {
                    if Some(*id) != from_peer && p.handshaken {
                        send(p, &inv);
                    }
                }
                let ready: Vec<Hash32> = g.orphans.iter().filter(|(_, o)| o.header.parent == hash).map(|(h, _)| *h).collect();
                let blocks: Vec<Block> = ready.iter().filter_map(|h| g.orphans.remove(h)).collect();
                drop(g);
                for ob in blocks {
                    let _ = self.process_block(ob, None);
                }
                return r;
            }
            Err(_) => g.stats.blocks_rejected += 1,
        }
        r
    }

    pub fn submit_tx(&self, tx: Transaction, from_peer: Option<u64>) -> Result<Hash32, String> {
        let mut g = self.lock();
        let st = g.chain.state.clone();
        let p = g.chain.params.clone();
        let h = g.chain.height() + 1;
        let id = g.mempool.admit(tx.clone(), &p, h, &st)?;
        let m = Msg::Tx { tx };
        for (pid, peer) in g.peers.iter() {
            if Some(*pid) != from_peer && peer.handshaken {
                send(peer, &m);
            }
        }
        Ok(id)
    }

    pub fn submit_bridge_op(&self, op: BridgeOp, from_peer: Option<u64>) -> Result<Hash32, String> {
        let mut g = self.lock();
        let st = g.chain.state.clone();
        let p = g.chain.params.clone();
        let h = g.chain.height() + 1;
        let fresh = !g.mempool.ops.contains_key(&bridge::op_id(&op));
        let id = g.mempool.admit_op(op.clone(), &p, h, &st)?;
        if fresh {
            let m = Msg::BridgeOp { op };
            for (pid, peer) in g.peers.iter() {
                if Some(*pid) != from_peer && peer.handshaken {
                    send(peer, &m);
                }
            }
        }
        Ok(id)
    }

    fn hello(&self) -> Msg {
        let g = self.lock();
        Msg::Hello {
            protocol: PROTOCOL_VERSION,
            chain_id: g.chain.cfg.chain_id,
            genesis: to_hex(&g.chain.genesis_hash),
            height: g.chain.height(),
            best: to_hex(g.chain.best.last().unwrap()),
            listen: Some(g.opts.listen_port),
        }
    }

    pub fn ban(&self, pid: u64, why: &str) {
        let mut g = self.lock();
        if let Some(p) = g.peers.remove(&pid) {
            if p.addr.ip().is_loopback() {
                eprintln!("[p2p] disconnecting loopback peer {} ({}): {}", p.addr, hex::encode(&p.peer_id[..6]), why);
            } else {
                eprintln!("[p2p] banning {} ({}) for {} s: {}", p.addr, hex::encode(&p.peer_id[..6]), BAN_SECS, why);
                g.bans.insert(p.addr.ip(), Instant::now());
            }
            g.banned_ids.insert(p.peer_id, Instant::now());
            g.stats.peers_banned += 1;
            let _ = p.tx.send(Vec::new()); // empty = close
        }
    }

    fn handle_msg(&self, pid: u64, m: Msg) -> Result<(), String> {
        match m {
            Msg::Hello { protocol, chain_id, genesis, height, listen, .. } => {
                let mut g = self.lock();
                if protocol != PROTOCOL_VERSION || chain_id != g.chain.cfg.chain_id || genesis != to_hex(&g.chain.genesis_hash) {
                    return Err("incompatible chain/protocol".into());
                }
                let loc: Vec<String> = g.chain.locator().iter().map(|h| to_hex(h)).collect();
                let mut learned = None;
                if let Some(p) = g.peers.get_mut(&pid) {
                    p.height = height;
                    p.handshaken = true;
                    p.listen = listen;
                    if let Some(lp) = listen {
                        learned = Some(SocketAddr::new(p.addr.ip(), lp).to_string());
                    }
                    send(p, &Msg::GetBlocks { locator: loc });
                    send(p, &Msg::GetAddr {});
                }
                if let Some(a) = learned {
                    if g.addr_book.len() < MAX_ADDRS {
                        g.addr_book.insert(a);
                    }
                }
            }
            Msg::GetAddr {} => {
                let g = self.lock();
                let mut addrs: Vec<String> = g.peers.values().filter_map(|p| p.listen.map(|l| SocketAddr::new(p.addr.ip(), l).to_string())).collect();
                addrs.extend(g.addr_book.iter().take(200).cloned());
                addrs.sort();
                addrs.dedup();
                if let Some(p) = g.peers.get(&pid) {
                    send(p, &Msg::Addr { addrs });
                }
            }
            Msg::Addr { addrs } => {
                let mut g = self.lock();
                for a in addrs.into_iter().take(MAX_ADDRS) {
                    if a.parse::<SocketAddr>().is_ok() && g.addr_book.len() < MAX_ADDRS {
                        g.addr_book.insert(a);
                    }
                }
            }
            Msg::GetBlocks { locator } => {
                let g = self.lock();
                let loc: Vec<Hash32> = locator.iter().take(64).filter_map(|s| from_hex32(s)).collect();
                let hs = g.chain.hashes_after_locator(&loc, MAX_INV);
                if !hs.is_empty() {
                    if let Some(p) = g.peers.get(&pid) {
                        send(p, &Msg::Inv { hashes: hs.iter().map(|h| to_hex(h)).collect() });
                    }
                }
            }
            Msg::Inv { hashes } => {
                let g = self.lock();
                // A full batch means the peer has more: ask for the next one
                // starting after the batch's last hash (deep forks / long sync).
                if hashes.len() >= MAX_INV {
                    let mut loc = vec![hashes[hashes.len() - 1].clone()];
                    loc.extend(g.chain.locator().iter().map(|h| to_hex(h)));
                    if let Some(p) = g.peers.get(&pid) {
                        send(p, &Msg::GetBlocks { locator: loc });
                    }
                }
                let want: Vec<String> = hashes
                    .into_iter()
                    .take(MAX_INV)
                    .filter(|s| from_hex32(s).map(|h| !g.chain.entries.contains_key(&h) && !g.orphans.contains_key(&h)).unwrap_or(false))
                    .collect();
                if !want.is_empty() {
                    if let Some(p) = g.peers.get(&pid) {
                        send(p, &Msg::GetData { hashes: want });
                    }
                }
            }
            Msg::GetData { hashes } => {
                let g = self.lock();
                if let Some(p) = g.peers.get(&pid) {
                    for s in hashes.iter().take(MAX_INV) {
                        if let Some(e) = from_hex32(s).and_then(|h| g.chain.entries.get(&h)) {
                            send(p, &Msg::Block { block: (*e.block).clone() });
                        }
                    }
                }
            }
            Msg::Block { block } => {
                let h = block.header.height;
                if let Some(p) = self.lock().peers.get_mut(&pid) {
                    p.height = p.height.max(h);
                }
                if let Err(e) = self.process_block(block, Some(pid)) {
                    if !e.contains("timestamp") && !e.contains("parent is invalid") {
                        return Err(format!("invalid block: {}", e));
                    }
                }
            }
            Msg::Tx { tx } => {
                let _ = self.submit_tx(tx, Some(pid));
            }
            Msg::BridgeOp { op } => {
                let _ = self.submit_bridge_op(op, Some(pid));
            }
        }
        Ok(())
    }

    /// Run the encrypted handshake and the peer's reader/writer threads.
    pub fn attach_stream(&self, stream: TcpStream, outbound: bool) {
        let Ok(addr) = stream.peer_addr() else { return };
        {
            let g = self.lock();
            if let Some(t) = g.bans.get(&addr.ip()) {
                if t.elapsed().as_secs() < BAN_SECS {
                    return;
                }
            }
            if g.peers.len() >= g.opts.max_peers {
                return;
            }
        }
        let me = self.clone();
        std::thread::spawn(move || {
            let (tag, allow) = {
                let g = me.lock();
                (g.chain.genesis_hash, g.opts.peer_allow.clone())
            };
            let sess = if outbound {
                p2p_crypto::handshake_initiator(stream, &me.identity, &tag, allow.as_deref())
            } else {
                p2p_crypto::handshake_responder(stream, &me.identity, &tag, allow.as_deref())
            };
            let sess = match sess {
                Ok(s) => s,
                Err(e) => {
                    me.lock().stats.handshakes_failed += 1;
                    eprintln!("[p2p] handshake with {} failed: {}", addr, e);
                    return;
                }
            };
            let (tx, rx) = channel::<Vec<u8>>();
            let pid = {
                let mut g = me.lock();
                g.stats.handshakes_ok += 1;
                if let Some(t) = g.banned_ids.get(&sess.peer_id) {
                    if t.elapsed().as_secs() < BAN_SECS && !addr.ip().is_loopback() {
                        return;
                    }
                }
                if g.peers.values().any(|p| p.peer_id == sess.peer_id) {
                    return; // duplicate connection to the same identity
                }
                let pid = g.next_peer_id;
                g.next_peer_id += 1;
                g.peers.insert(pid, Peer { addr, peer_id: sess.peer_id, tx: tx.clone(), height: 0, outbound, handshaken: false, listen: None });
                pid
            };
            let mut writer: SecureWriter = sess.writer;
            std::thread::spawn(move || {
                for frame in rx {
                    if frame.is_empty() || writer.send(&frame).is_err() {
                        writer.shutdown();
                        break;
                    }
                }
            });
            let _ = tx.send(encode(&me.hello()));
            let mut reader = sess.reader;
            let mut tokens = PEER_MSG_BURST;
            let mut last = Instant::now();
            loop {
                let frame = match reader.recv() {
                    Ok(f) => f,
                    Err(e) => {
                        if e.contains("authentication") || e.contains("exceeds cap") {
                            me.lock().stats.frames_rejected += 1;
                            me.ban(pid, &e);
                        }
                        break;
                    }
                };
                tokens = (tokens + last.elapsed().as_secs_f64() * PEER_MSG_RATE).min(PEER_MSG_BURST);
                last = Instant::now();
                if tokens < 1.0 {
                    me.ban(pid, "message rate limit exceeded");
                    break;
                }
                tokens -= 1.0;
                match serde_json::from_slice::<Msg>(&frame) {
                    Ok(m) => {
                        if let Err(e) = me.handle_msg(pid, m) {
                            me.ban(pid, &e);
                            break;
                        }
                    }
                    Err(e) => {
                        me.lock().stats.frames_rejected += 1;
                        me.ban(pid, &format!("malformed message: {}", e));
                        break;
                    }
                }
            }
            me.lock().peers.remove(&pid);
        });
    }

    pub fn listen(&self, port: u16) -> std::io::Result<()> {
        let l = TcpListener::bind(("0.0.0.0", port))?;
        let me = self.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                if me.shutdown.load(Ordering::Relaxed) {
                    break;
                }
                me.attach_stream(s, false);
            }
        });
        Ok(())
    }

    fn connected_to(&self, addrs: &[SocketAddr]) -> bool {
        let g = self.lock();
        g.peers.values().any(|p| addrs.contains(&p.addr) || p.listen.map(|l| addrs.contains(&SocketAddr::new(p.addr.ip(), l))).unwrap_or(false))
    }

    /// Keep an outbound connection to a bootstrap peer.
    pub fn maintain_peer(&self, addr: String) {
        use std::net::ToSocketAddrs;
        let me = self.clone();
        std::thread::spawn(move || loop {
            if me.shutdown.load(Ordering::Relaxed) {
                break;
            }
            let addrs: Vec<SocketAddr> = addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
            if !me.connected_to(&addrs) {
                for sa in &addrs {
                    if let Ok(s) = TcpStream::connect_timeout(sa, Duration::from_secs(2)) {
                        me.attach_stream(s, true);
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        });
    }

    /// Peer discovery: dial addresses learned via Addr gossip.
    pub fn run_discovery(&self) {
        let me = self.clone();
        std::thread::spawn(move || loop {
            if me.shutdown.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_secs(3));
            let (cands, room, my_port) = {
                let g = me.lock();
                let out = g.peers.values().filter(|p| p.outbound).count();
                (g.addr_book.iter().cloned().collect::<Vec<_>>(), g.opts.max_outbound.saturating_sub(out), g.opts.listen_port)
            };
            for a in cands.into_iter().take(room) {
                let Ok(sa) = a.parse::<SocketAddr>() else { continue };
                if sa.port() == my_port && (sa.ip().is_loopback() || sa.ip().is_unspecified()) {
                    continue; // our own listener
                }
                if me.connected_to(&[sa]) {
                    continue;
                }
                if let Ok(s) = TcpStream::connect_timeout(&sa, Duration::from_secs(2)) {
                    me.attach_stream(s, true);
                }
            }
        });
    }

    /// Flush state for a clean exit: snapshot + CLEAN_SHUTDOWN marker.
    pub fn clean_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let g = self.lock();
        for p in g.peers.values() {
            let _ = p.tx.send(Vec::new());
        }
        if let Some(d) = g.opts.data_dir.clone() {
            let snap = Snapshot::new(&g.chain.cfg.commitment(), g.chain.height(), g.chain.best.last().unwrap(), &g.chain.state);
            let _ = snap.write_atomic(&std::path::Path::new(&d).join("snapshots").join("latest.json"));
            let _ = std::fs::write(std::path::Path::new(&d).join("CLEAN_SHUTDOWN"), format!("{}\n", g.chain.height()));
        }
        eprintln!("[node] clean shutdown at height {}", g.chain.height());
    }

    // ---------------- miner / wallet / explorer API ----------------

    pub fn get_work(&self, miner: Option<Address>) -> Result<Value, String> {
        let mut g = self.lock();
        let miner = miner.or(g.opts.default_miner).ok_or("no miner address (pass ?address= or start node with --miner-address)")?;
        // If the chain's median time past has run ahead of the clock, any
        // template would carry a timestamp beyond max_future and be rejected:
        // tell the miner to wait instead of handing out invalid work.
        let tip_hash = *g.chain.best.last().unwrap();
        let earliest = g.chain.mtp(&tip_hash) + 1;
        let now = now_secs();
        if earliest > now + g.chain.cfg.max_future_secs {
            return Err(format!("chain time is {} s ahead of this node's clock; retry later", earliest - now));
        }
        let txs = g.mempool.ordered();
        let ops = g.mempool.ordered_ops();
        let b = g.chain.template_with_ops(miner, &txs, &ops, now_secs());
        let seed = g.chain.epoch_seed_for_next();
        let p = g.chain.profile;
        let work_id = format!("{:016x}", rand::random::<u64>());
        let v = json!({
            "api_version": 2,
            "work_id": work_id,
            "chain_id": b.header.chain_id,
            "version": b.header.version,
            "height": b.header.height,
            "header_without_nonce": hex::encode(b.header.encode_without_nonce()),
            "challenge": to_hex(&zfx_verify::pow::challenge(&b.header)),
            "target": hex::encode(b.header.target),
            "epoch_seed": to_hex(&seed),
            "profile": {"id": p.id, "name": p.name, "d_in": p.d_in, "hidden": p.hidden, "n_hidden": p.n_hidden,
                        "d_out": p.d_out, "shift_in": p.shift_in, "shift_hidden": p.shift_hidden},
            "expiry": {"parent": to_hex(&b.header.parent)},
            "txs": b.txs.len(),
            "bridge_ops": b.bridge_ops.len(),
        });
        if g.works.len() > 1000 {
            g.works.clear();
        }
        g.works.insert(work_id, b);
        Ok(v)
    }

    pub fn submit_work(&self, ip: Option<std::net::IpAddr>, work_id: &str, nonce: u64) -> Value {
        let b = {
            let mut g = self.lock();
            g.stats.submissions += 1;
            if let Some(ip) = ip {
                let e = g.submit_buckets.entry(ip).or_insert((Instant::now(), 0));
                if e.0.elapsed() > Duration::from_secs(1) {
                    *e = (Instant::now(), 0);
                }
                e.1 += 1;
                if e.1 > 50 {
                    g.stats.submissions_rate_limited += 1;
                    return json!({"accepted": false, "error": "rate limited", "code": 1005});
                }
            }
            match g.works.get(work_id) {
                Some(b) => b.clone(),
                None => return json!({"accepted": false, "error": "unknown or stale work_id (tip changed)", "code": 1002}),
            }
        };
        let mut b = b;
        b.header.nonce = nonce;
        let h = b.hash();
        match self.process_block(b, None) {
            Ok(AddResult::NewTip { .. }) | Ok(AddResult::SideChain) => {
                self.lock().stats.submissions_accepted += 1;
                json!({"accepted": true, "block_hash": to_hex(&h)})
            }
            Ok(AddResult::Duplicate) => json!({"accepted": false, "error": "duplicate block", "code": 1001}),
            Ok(AddResult::Orphan) => json!({"accepted": false, "error": "stale work (parent unknown)", "code": 1002}),
            Err(e) => json!({"accepted": false, "error": e, "code": 1001}),
        }
    }

    pub fn status(&self) -> Value {
        let g = self.lock();
        let tip = g.chain.tip();
        let tgt = tip.block.header.target_u256();
        let bs = g.chain.state.bridge.as_ref().map(|b| {
            json!({"epoch": b.epoch, "paused": b.paused, "locked_total": b.locked_total.to_string(),
            "released_total": b.released_total.to_string(), "deposits": b.deposits, "releases": b.releases})
        });
        json!({
            "api_version": 2,
            "chain_id": g.chain.cfg.chain_id,
            "consensus_version": g.chain.cfg.consensus_version,
            "profile": g.chain.profile.name,
            "genesis": to_hex(&g.chain.genesis_hash),
            "height": g.chain.height(),
            "best_hash": to_hex(g.chain.best.last().unwrap()),
            "state_root": to_hex(&tip.block.header.state_root),
            "target": tgt.to_hex(),
            "difficulty_log2": 256.0 - tgt.log2_f64(),
            "next_target": g.chain.expected_target(g.chain.best.last().unwrap()).to_hex(),
            "cum_work_log2": tip.cum_work.log2_f64(),
            "tip_timestamp": tip.block.header.timestamp,
            "peer_count": g.peers.values().filter(|p| p.handshaken).count(),
            "peer_id": hex::encode(self.identity.peer_id),
            "mempool": g.mempool.txs.len(),
            "bridge_ops_pending": g.mempool.ops.len(),
            "orphans": g.orphans.len(),
            "supply": g.chain.state.supply.to_string(),
            "coinbase_maturity": g.chain.cfg.coinbase_maturity,
            "reorgs": g.chain.reorg_count,
            "max_reorg_depth": g.chain.max_reorg_depth,
            "invalid_blocks": g.chain.invalid_count,
            "bridge": bs,
            "stats": g.stats,
            "load_report": g.load_report,
            "transport": "ZFXP2P/2: ML-KEM-768 + X25519 hybrid key exchange, ML-DSA-65 peer identities, ChaCha20-Poly1305 frames",
            "signature_schemes": ["SLH-DSA-SHA2-128s", "SLH-DSA-SHA2-192s"],
            "crypto": "consensus: SLH-DSA + SHA-256/SHAKE256 only; transport: ML-KEM-768 hybrid",
        })
    }

    pub fn block_json(&self, key: &str) -> Option<Value> {
        let g = self.lock();
        let h = if let Ok(n) = key.parse::<u64>() { *g.chain.best.get(n as usize)? } else { from_hex32(key)? };
        let e = g.chain.entries.get(&h)?;
        let in_best = g.chain.best.get(e.height as usize) == Some(&h);
        Some(json!({
            "hash": to_hex(&h),
            "height": e.height,
            "in_best_chain": in_best,
            "confirmations": if in_best { g.chain.height() - e.height + 1 } else { 0 },
            "status": format!("{:?}", e.status),
            "pow_digest": to_hex(&e.pow_digest),
            "cum_work_log2": e.cum_work.log2_f64(),
            "header": e.block.header,
            "header_hex": hex::encode(e.block.header.encode()),
            "coinbase": e.block.coinbase,
            "txs": e.block.txs.iter().map(|t| json!({"txid": to_hex(&t.txid()), "from": address_to_string(&t.from), "to": address_to_string(&t.to),
                "amount": t.amount, "fee": t.fee, "nonce": t.nonce, "memo": t.memo, "scheme": zfx_verify::tx::scheme_name(t.witness.leaf.scheme), "bytes": t.encoded_len()})).collect::<Vec<_>>(),
            "bridge_ops": e.block.bridge_ops.iter().map(|o| json!({"op_id": to_hex(&bridge::op_id(o)), "op": o})).collect::<Vec<_>>(),
            "size": e.block.encoded_size(),
        }))
    }

    pub fn blocks_json(&self, from: Option<u64>, count: u64) -> Value {
        let g = self.lock();
        let top = g.chain.height();
        let start = from.unwrap_or(top);
        let mut v = Vec::new();
        let mut h = start.min(top) as i64;
        while h >= 0 && (v.len() as u64) < count.min(200) {
            let hash = g.chain.best[h as usize];
            let e = &g.chain.entries[&hash];
            v.push(json!({"height": h, "hash": to_hex(&hash), "timestamp": e.block.header.timestamp, "txs": e.block.txs.len(),
                "bridge_ops": e.block.bridge_ops.len(),
                "miner": address_to_string(&e.block.coinbase.to), "reward": e.block.coinbase.amount,
                "target": e.block.header.target_u256().to_hex(), "difficulty_log2": 256.0 - e.block.header.target_u256().log2_f64(),
                "nonce": e.block.header.nonce.to_string()}));
            h -= 1;
        }
        json!(v)
    }

    pub fn account_json(&self, a: &Address) -> Value {
        let g = self.lock();
        let acc = g.chain.state.account(a);
        let pending: Vec<Value> = g
            .mempool
            .txs
            .values()
            .filter(|t| &t.from == a || &t.to == a)
            .map(|t| json!({"txid": to_hex(&t.txid()), "nonce": t.nonce, "amount": t.amount}))
            .collect();
        let next_nonce = acc.nonce + g.mempool.txs.values().filter(|t| &t.from == a).count() as u64;
        json!({"address": address_to_string(a), "balance": acc.balance, "immature": g.chain.state.immature_of(a), "nonce": acc.nonce,
               "next_nonce": next_nonce, "pending": pending})
    }

    /// Transaction receipt with a Merkle inclusion proof against tx_root.
    pub fn tx_json(&self, id: &Hash32) -> Option<Value> {
        let g = self.lock();
        if let Some(t) = g.mempool.txs.get(id) {
            return Some(json!({"status": "mempool", "tx": t}));
        }
        let (bh, height) = g.chain.idx.tx.get(id)?;
        let b = &g.chain.entries[bh].block;
        let i = b.txs.iter().position(|t| &t.txid() == id)?;
        let leaves = b.leaves();
        let proof: Vec<Value> = merkle_proof(&leaves, i + 1).iter().map(|(h, r)| json!({"hash": to_hex(h), "right": r})).collect();
        Some(json!({"status": "confirmed", "block": to_hex(bh), "height": height, "confirmations": g.chain.height() - height + 1,
            "tx": b.txs[i], "receipt": {"index": i, "leaf": to_hex(&leaves[i + 1]), "leaf_count": leaves.len(), "merkle_proof": proof,
            "tx_root": to_hex(&b.header.tx_root), "header_hex": hex::encode(b.header.encode())}}))
    }

    pub fn history_json(&self, a: &Address, limit: usize) -> Value {
        let g = self.lock();
        let mut v: Vec<Value> = g
            .chain
            .idx
            .by_address
            .get(a)
            .map(|l| {
                l.iter()
                    .rev()
                    .take(limit)
                    .filter_map(|(h, id)| {
                        let (bh, _) = g.chain.idx.tx.get(id)?;
                        let t = g.chain.entries[bh].block.txs.iter().find(|t| &t.txid() == id)?;
                        Some(json!({"txid": to_hex(id), "height": h, "direction": if &t.from == a { "out" } else { "in" },
                            "counterparty": address_to_string(if &t.from == a { &t.to } else { &t.from }), "amount": t.amount, "fee": t.fee, "memo": t.memo}))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // coinbase rewards
        for h in (0..=g.chain.height()).rev() {
            if v.len() >= limit * 2 {
                break;
            }
            let b = g.chain.block_at(h).unwrap();
            if &b.coinbase.to == a && h > 0 {
                v.push(json!({"txid": to_hex(&b.coinbase.id()), "height": h, "direction": "coinbase", "amount": b.coinbase.amount,
                    "mature_at": h + g.chain.cfg.coinbase_maturity}));
            }
        }
        json!(v)
    }

    pub fn fee_estimate(&self) -> Value {
        let g = self.lock();
        json!({"fee_per_tx": g.chain.fee_estimate(), "basis": "median per-tx fee of the last 100 blocks with transactions, floor 1000", "mempool": g.mempool.txs.len()})
    }

    pub fn faucet(&self, to: Address) -> Result<Value, String> {
        let (fc, cid, nonce) = {
            let mut g = self.lock();
            let fc = g.opts.faucet.clone().ok_or("faucet disabled on this node")?;
            let now = now_secs();
            if let Some(t) = g.faucet_last.get(&to) {
                if now < t + fc.cooldown_secs {
                    return Err(format!("faucet cooldown: retry in {} s", t + fc.cooldown_secs - now));
                }
            }
            g.faucet_last.insert(to, now);
            let keys_addr = P2mrKeys::from_seed(&fc.seed).address();
            let acc = g.chain.state.account(&keys_addr);
            let pend = g.mempool.txs.values().filter(|t| t.from == keys_addr).count() as u64;
            (fc, g.chain.cfg.chain_id, acc.nonce + pend)
        };
        let keys = P2mrKeys::from_seed(&fc.seed);
        let tx = keys.sign_tx(cid, to, fc.amount, 1000, nonce, 0)?;
        let id = self.submit_tx(tx, None)?;
        self.lock().stats.faucet_payouts += 1;
        Ok(json!({"txid": to_hex(&id), "amount": fc.amount, "to": address_to_string(&to)}))
    }

    pub fn bridge_info(&self) -> Value {
        let g = self.lock();
        let cid = g.chain.cfg.chain_id;
        match (&g.chain.cfg.bridge, &g.chain.state.bridge) {
            (Some(bp), Some(bs)) => json!({
                "enabled": true,
                "trust_model": "v1 is custodied by a 2-of-3 committee; two compromised members can steal locked ZFX or authorize releases.",
                "native_asset_id": bridge::native_asset_id(cid),
                "lockbox": address_to_string(&bridge::lockbox_address(cid)),
                "near_contract": bp.near_contract,
                "min_lock": bp.min_lock,
                "threshold": bp.threshold,
                "confirmations_required": g.opts.bridge_confirmations,
                "epoch": bs.epoch,
                "members": bs.members.iter().map(hex::encode).collect::<Vec<_>>(),
                "pending_rotation_height": bs.pending_activation,
                "paused": bs.paused,
                "admin_nonce": bs.admin_nonce,
                "locked_total": bs.locked_total.to_string(),
                "released_total": bs.released_total.to_string(),
                "lockbox_balance": g.chain.state.account(&bridge::lockbox_address(cid)).balance,
                "height": g.chain.height(),
            }),
            _ => json!({"enabled": false}),
        }
    }

    /// Deposit status with an independently verifiable inclusion proof.
    pub fn deposit_status(&self, id: &Hash32) -> Value {
        let g = self.lock();
        if let Some(t) = g.mempool.txs.get(id) {
            if t.to == bridge::lockbox_address(g.chain.cfg.chain_id) {
                return json!({"deposit_id": to_hex(id), "state": "mempool", "confirmations": 0});
            }
        }
        let Some(rec) = g.chain.idx.deposits.get(id) else {
            return json!({"deposit_id": to_hex(id), "state": "unknown"});
        };
        let conf = g.chain.height() - rec.height + 1;
        let k = g.opts.bridge_confirmations;
        let bh = from_hex32(&rec.block).unwrap();
        let b = &g.chain.entries[&bh].block;
        let i = b.txs.iter().position(|t| &t.txid() == id).unwrap();
        let leaves = b.leaves();
        let proof: Vec<Value> = merkle_proof(&leaves, i + 1).iter().map(|(h, r)| json!({"hash": to_hex(h), "right": r})).collect();
        json!({
            "deposit_id": to_hex(id),
            "state": if conf >= k { "final" } else { "awaiting_confirmations" },
            "confirmations": conf,
            "confirmations_required": k,
            "amount": rec.amount,
            "near_recipient": rec.near_recipient,
            "sender": rec.sender,
            "native_asset_id": bridge::native_asset_id(g.chain.cfg.chain_id),
            "chain_id": g.chain.cfg.chain_id,
            "block": rec.block,
            "height": rec.height,
            "inclusion": {"leaf": to_hex(&leaves[i + 1]), "leaf_index": i + 1, "leaf_count": leaves.len(), "merkle_proof": proof,
                          "header_hex": hex::encode(b.header.encode()), "tx": b.txs[i]},
        })
    }

    /// Deposits on the best chain with height >= from (ordered), for relayers.
    pub fn list_deposits(&self, from: u64, limit: usize) -> Value {
        let g = self.lock();
        let mut v: Vec<&crate::chain::DepositRecord> = g.chain.idx.deposits.values().filter(|d| d.height >= from).collect();
        v.sort_by(|a, b| a.height.cmp(&b.height).then(a.deposit_id.cmp(&b.deposit_id)));
        let tip = g.chain.height();
        json!(v
            .into_iter()
            .take(limit)
            .map(|d| json!({"deposit_id": d.deposit_id, "height": d.height, "amount": d.amount,
            "near_recipient": d.near_recipient, "confirmations": tip - d.height + 1, "block": d.block}))
            .collect::<Vec<_>>())
    }

    pub fn list_releases(&self) -> Value {
        let g = self.lock();
        let mut v: Vec<&crate::chain::ReleaseRecord> = g.chain.idx.releases.values().collect();
        v.sort_by(|a, b| a.height.cmp(&b.height).then(a.withdrawal_id.cmp(&b.withdrawal_id)));
        json!(v)
    }

    pub fn redeem_status(&self, wid: &Hash32) -> Value {
        let g = self.lock();
        if let Some(rec) = g.chain.idx.releases.get(wid) {
            let conf = g.chain.height() - rec.height + 1;
            return json!({"withdrawal_id": to_hex(wid), "state": if conf >= g.opts.bridge_confirmations { "final" } else { "released_awaiting_confirmations" },
                "confirmations": conf, "record": rec});
        }
        let pending = g.mempool.ops.values().any(|o| matches!(o, BridgeOp::Release(r) if &r.withdrawal_id == wid));
        json!({"withdrawal_id": to_hex(wid), "state": if pending { "mempool" } else { "unknown" }})
    }

    pub fn metrics(&self) -> String {
        let g = self.lock();
        let s = &g.stats;
        format!(
            "zfx_height {}\nzfx_peers {}\nzfx_mempool {}\nzfx_blocks_accepted {}\nzfx_blocks_rejected {}\nzfx_submissions {}\nzfx_submissions_accepted {}\nzfx_reorgs {}\nzfx_invalid_blocks {}\nzfx_difficulty_log2 {}\nzfx_last_pow_verify_ms {}\nzfx_handshakes_ok {}\nzfx_handshakes_failed {}\nzfx_frames_rejected {}\nzfx_peers_banned {}\nzfx_uptime_seconds {}\n",
            g.chain.height(), g.peers.len(), g.mempool.txs.len(), s.blocks_accepted, s.blocks_rejected, s.submissions, s.submissions_accepted,
            g.chain.reorg_count, g.chain.invalid_count, 256.0 - g.chain.tip().block.header.target_u256().log2_f64(), s.last_pow_verify_ms,
            s.handshakes_ok, s.handshakes_failed, s.frames_rejected, s.peers_banned, g.started.elapsed().as_secs()
        )
    }

    pub fn health(&self) -> Value {
        let g = self.lock();
        let age = now_secs().saturating_sub(g.chain.tip().block.header.timestamp);
        let audit = g.chain.state.audit_with(g.chain.cfg.chain_id);
        json!({"ok": audit.is_ok(), "height": g.chain.height(), "tip_age_secs": age, "peers": g.peers.len(),
               "supply_audit": audit.err().unwrap_or_else(|| "ok".into()), "shutting_down": self.shutdown.load(Ordering::Relaxed)})
    }

    pub fn peers_json(&self) -> Value {
        let g = self.lock();
        json!(g
            .peers
            .values()
            .map(|p| json!({"addr": p.addr.to_string(), "peer_id": hex::encode(p.peer_id), "height": p.height,
            "outbound": p.outbound, "handshaken": p.handshaken, "listen": p.listen}))
            .collect::<Vec<_>>())
    }

    pub fn snapshot_export(&self, path: &str) -> Result<Value, String> {
        let g = self.lock();
        let snap = Snapshot::new(&g.chain.cfg.commitment(), g.chain.height(), g.chain.best.last().unwrap(), &g.chain.state);
        snap.write_atomic(std::path::Path::new(path))?;
        Ok(json!({"path": path, "height": snap.height, "state_root": snap.state_root, "checksum": snap.checksum}))
    }

    /// Verify a snapshot file against this node's chain.
    pub fn snapshot_verify(&self, path: &str) -> Result<Value, String> {
        let snap = Snapshot::read(std::path::Path::new(path))?;
        let g = self.lock();
        if snap.chain_commitment != hex::encode(g.chain.cfg.commitment()) {
            return Err("snapshot is for a different chain".into());
        }
        let bh = from_hex32(&snap.block_hash).ok_or("bad block hash")?;
        let e = g.chain.entries.get(&bh).ok_or("snapshot block not known to this node")?;
        let st = snap.verify(Some(&e.block.header.state_root))?;
        Ok(json!({"ok": true, "height": snap.height, "state_root": snap.state_root, "accounts": st.accounts.len()}))
    }
}

pub fn parse_address(s: &str) -> Result<Address, String> {
    address_from_string(s)
}
