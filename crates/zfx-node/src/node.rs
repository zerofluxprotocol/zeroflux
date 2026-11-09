//! Networked node: P2P (newline-delimited JSON over TCP), HTTP API for
//! miners/wallets/explorer, persistence. Consensus decisions are made
//! only by `Chain` using zfx-verify; nothing a peer or miner sends is
//! trusted (the node recomputes every PoW digest itself).

use crate::chain::{AddResult, Chain};
use crate::config::ChainConfig;
use crate::mempool::Mempool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zfx_verify::block::Block;
use zfx_verify::hash::{from_hex32, to_hex, Hash32};
use zfx_verify::tx::{address_from_string, address_to_string, Address, Transaction};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_LINE_BYTES: u64 = 16 << 20;
pub const MAX_ORPHANS: usize = 256;
pub const MAX_INV: usize = 500;
pub const BAN_SECS: u64 = 600;

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
}

pub struct Peer {
    pub addr: SocketAddr,
    pub tx: Sender<String>,
    pub height: u64,
    pub outbound: bool,
    pub handshaken: bool,
}

pub struct Inner {
    pub chain: Chain,
    pub mempool: Mempool,
    pub peers: HashMap<u64, Peer>,
    pub next_peer_id: u64,
    pub orphans: HashMap<Hash32, Block>,
    pub works: HashMap<String, Block>,
    pub bans: HashMap<std::net::IpAddr, Instant>,
    pub submit_buckets: HashMap<std::net::IpAddr, (Instant, u32)>,
    pub store: Option<std::fs::File>,
    pub default_miner: Option<Address>,
    pub stats: Stats,
    pub listen_port: u16,
}

#[derive(Default, Serialize, Clone)]
pub struct Stats {
    pub blocks_accepted: u64,
    pub blocks_rejected: u64,
    pub submissions: u64,
    pub submissions_accepted: u64,
    pub submissions_rate_limited: u64,
    pub peers_banned: u64,
    pub last_pow_verify_ms: f64,
}

#[derive(Clone)]
pub struct Node(pub Arc<Mutex<Inner>>);

fn send(p: &Peer, m: &Msg) {
    let _ = p.tx.send(serde_json::to_string(m).unwrap());
}

impl Node {
    pub fn new(cfg: ChainConfig, data_dir: Option<&str>, default_miner: Option<Address>, listen_port: u16) -> Result<Node, String> {
        let mut chain = Chain::new(cfg.clone())?;
        let mut store = None;
        if let Some(d) = data_dir {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            let cpath = format!("{}/chain.json", d);
            if let Ok(old) = std::fs::read_to_string(&cpath) {
                let old: ChainConfig = serde_json::from_str(&old).map_err(|e| e.to_string())?;
                if old.commitment() != cfg.commitment() {
                    return Err("data dir belongs to a different chain config".into());
                }
            } else {
                std::fs::write(&cpath, serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())?;
            }
            let bpath = format!("{}/blocks.jsonl", d);
            if let Ok(f) = std::fs::File::open(&bpath) {
                let t = Instant::now();
                let mut n = 0;
                for line in BufReader::new(f).lines() {
                    let line = line.map_err(|e| e.to_string())?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Block>(&line) {
                        Ok(b) => {
                            if chain.add_block(b, now_secs()).is_ok() {
                                n += 1
                            }
                        }
                        Err(_) => break, // truncated final line after a crash
                    }
                }
                eprintln!("[node] recovered {} blocks from {} in {:.2}s, height {}", n, bpath, t.elapsed().as_secs_f64(), chain.height());
            }
            store = Some(std::fs::OpenOptions::new().create(true).append(true).open(&bpath).map_err(|e| e.to_string())?);
        }
        Ok(Node(Arc::new(Mutex::new(Inner {
            chain,
            mempool: Mempool::default(),
            peers: HashMap::new(),
            next_peer_id: 1,
            orphans: HashMap::new(),
            works: HashMap::new(),
            bans: HashMap::new(),
            submit_buckets: HashMap::new(),
            store,
            default_miner,
            stats: Stats::default(),
            listen_port,
        }))))
    }

    /// Core block intake used by P2P, the miner API, and tests.
    /// Returns the AddResult and relays new blocks.
    pub fn process_block(&self, b: Block, from_peer: Option<u64>) -> Result<AddResult, String> {
        let mut g = self.0.lock().unwrap();
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
                if let Some(f) = g.store.as_mut() {
                    let _ = writeln!(f, "{}", serde_json::to_string(&b).unwrap());
                    let _ = f.flush();
                }
                if let AddResult::NewTip { reorg_depth } = res {
                    g.works.clear();
                    if *reorg_depth > 0 {
                        eprintln!("[node] reorg depth {} -> height {}", reorg_depth, g.chain.height());
                        // Return disconnected txs to the mempool.
                        let new: HashSet<Hash32> = g.chain.best.iter().copied().collect();
                        let dis: Vec<Transaction> = old_best.iter().filter(|h| !new.contains(*h)).flat_map(|h| g.chain.entries[h].block.txs.clone()).collect();
                        let st = g.chain.state.clone();
                        let p = g.chain.params.clone();
                        let h = g.chain.height() + 1;
                        for t in dis {
                            let _ = g.mempool.admit(t, &p, h, &st);
                        }
                    }
                    let st = g.chain.state.clone();
                    let p = g.chain.params.clone();
                    let h = g.chain.height() + 1;
                    g.mempool.revalidate(&p, h, &st);
                }
                let inv = Msg::Inv { hashes: vec![to_hex(&hash)] };
                for (id, p) in g.peers.iter() {
                    if Some(*id) != from_peer && p.handshaken {
                        send(p, &inv);
                    }
                }
                // Connect any orphans that were waiting on this block.
                let ready: Vec<Hash32> = g.orphans.iter().filter(|(_, o)| o.header.parent == hash).map(|(h, _)| *h).collect();
                let blocks: Vec<Block> = ready.iter().filter_map(|h| g.orphans.remove(h)).collect();
                drop(g);
                for ob in blocks {
                    let _ = self.process_block(ob, None);
                }
                return r;
            }
            Err(_) => {
                g.stats.blocks_rejected += 1;
            }
        }
        r
    }

    pub fn submit_tx(&self, tx: Transaction, from_peer: Option<u64>) -> Result<Hash32, String> {
        let mut g = self.0.lock().unwrap();
        let st = g.chain.state.clone();
        let p = g.chain.params.clone();
        let h = g.chain.height() + 1;
        let id = g.mempool.admit(tx.clone(), &p, h, &st)?;
        let m = Msg::Tx { tx };
        for (pid, p) in g.peers.iter() {
            if Some(*pid) != from_peer && p.handshaken {
                send(p, &m);
            }
        }
        Ok(id)
    }

    fn hello(&self) -> Msg {
        let g = self.0.lock().unwrap();
        Msg::Hello {
            protocol: PROTOCOL_VERSION,
            chain_id: g.chain.cfg.chain_id,
            genesis: to_hex(&g.chain.genesis_hash),
            height: g.chain.height(),
            best: to_hex(g.chain.best.last().unwrap()),
            listen: Some(g.listen_port),
        }
    }

    fn ban(&self, pid: u64, why: &str) {
        let mut g = self.0.lock().unwrap();
        if let Some(p) = g.peers.remove(&pid) {
            // Loopback peers share one IP (local testnets), so they are
            // disconnected but not IP-banned.
            if p.addr.ip().is_loopback() {
                eprintln!("[p2p] disconnecting loopback peer {}: {}", p.addr, why);
            } else {
                eprintln!("[p2p] banning {} for {} s: {}", p.addr, BAN_SECS, why);
                g.bans.insert(p.addr.ip(), Instant::now());
            }
            g.stats.peers_banned += 1;
            let _ = p.tx.send("__close__".into());
        }
    }

    fn handle_msg(&self, pid: u64, m: Msg) -> Result<(), String> {
        match m {
            Msg::Hello { protocol, chain_id, genesis, height, .. } => {
                let mut g = self.0.lock().unwrap();
                if protocol != PROTOCOL_VERSION || chain_id != g.chain.cfg.chain_id || genesis != to_hex(&g.chain.genesis_hash) {
                    return Err("incompatible chain/protocol".into());
                }
                let loc: Vec<String> = g.chain.locator().iter().map(|h| to_hex(h)).collect();
                if let Some(p) = g.peers.get_mut(&pid) {
                    p.height = height;
                    p.handshaken = true;
                    send(p, &Msg::GetBlocks { locator: loc });
                }
            }
            Msg::GetBlocks { locator } => {
                let g = self.0.lock().unwrap();
                let loc: Vec<Hash32> = locator.iter().take(64).filter_map(|s| from_hex32(s)).collect();
                let hs = g.chain.hashes_after_locator(&loc, MAX_INV);
                if !hs.is_empty() {
                    if let Some(p) = g.peers.get(&pid) {
                        send(p, &Msg::Inv { hashes: hs.iter().map(|h| to_hex(h)).collect() });
                    }
                }
            }
            Msg::Inv { hashes } => {
                let g = self.0.lock().unwrap();
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
                let g = self.0.lock().unwrap();
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
                {
                    let mut g = self.0.lock().unwrap();
                    if let Some(p) = g.peers.get_mut(&pid) {
                        p.height = p.height.max(h);
                    }
                }
                if let Err(e) = self.process_block(block, Some(pid)) {
                    // Invalid PoW/structure/signature is misbehavior.
                    if !e.contains("timestamp") && !e.contains("parent is invalid") {
                        return Err(format!("invalid block: {}", e));
                    }
                }
            }
            Msg::Tx { tx } => {
                let _ = self.submit_tx(tx, Some(pid));
            }
        }
        Ok(())
    }

    pub fn attach_stream(&self, stream: TcpStream, outbound: bool) -> Option<u64> {
        let addr = stream.peer_addr().ok()?;
        {
            let g = self.0.lock().unwrap();
            if let Some(t) = g.bans.get(&addr.ip()) {
                if t.elapsed().as_secs() < BAN_SECS {
                    return None;
                }
            }
        }
        let (tx, rx) = channel::<String>();
        let pid = {
            let mut g = self.0.lock().unwrap();
            let pid = g.next_peer_id;
            g.next_peer_id += 1;
            g.peers.insert(pid, Peer { addr, tx: tx.clone(), height: 0, outbound, handshaken: false });
            pid
        };
        let mut w = stream.try_clone().ok()?;
        std::thread::spawn(move || {
            for line in rx {
                if line == "__close__" {
                    let _ = w.shutdown(std::net::Shutdown::Both);
                    break;
                }
                if w.write_all(line.as_bytes()).is_err() || w.write_all(b"\n").is_err() {
                    break;
                }
            }
        });
        let _ = tx.send(serde_json::to_string(&self.hello()).unwrap());
        let me = self.clone();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stream);
            loop {
                let mut line = String::new();
                match (&mut r).take(MAX_LINE_BYTES).read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) if !line.ends_with('\n') => {
                        me.ban(pid, "oversized message");
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
                match serde_json::from_str::<Msg>(&line) {
                    Ok(m) => {
                        if let Err(e) = me.handle_msg(pid, m) {
                            me.ban(pid, &e);
                            break;
                        }
                    }
                    Err(e) => {
                        me.ban(pid, &format!("malformed message: {}", e));
                        break;
                    }
                }
            }
            me.0.lock().unwrap().peers.remove(&pid);
        });
        Some(pid)
    }

    pub fn listen(&self, port: u16) -> std::io::Result<()> {
        let l = TcpListener::bind(("0.0.0.0", port))?;
        let me = self.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                me.attach_stream(s, false);
            }
        });
        Ok(())
    }

    /// Keep an outbound connection to `addr` (IP:port or hostname:port),
    /// re-resolving and reconnecting after drops.
    pub fn maintain_peer(&self, addr: String) {
        use std::net::ToSocketAddrs;
        let me = self.clone();
        std::thread::spawn(move || loop {
            let addrs: Vec<SocketAddr> = addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
            let connected = me.0.lock().unwrap().peers.values().any(|p| p.outbound && addrs.contains(&p.addr));
            if !connected {
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

    // ---------------- miner / wallet / explorer API ----------------

    pub fn get_work(&self, miner: Option<Address>) -> Result<Value, String> {
        let mut g = self.0.lock().unwrap();
        let miner = miner.or(g.default_miner).ok_or("no miner address (pass ?address= or start node with --miner-address)")?;
        let txs = g.mempool.ordered();
        let ops = g.mempool.ordered_ops();
        let b = g.chain.template_with_ops(miner, &txs, &ops, now_secs());
        let seed = g.chain.epoch_seed_for_next();
        let p = g.chain.profile;
        let work_id = format!("{:016x}", rand::random::<u64>());
        let v = json!({
            "api_version": 1,
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
        });
        if g.works.len() > 1000 {
            g.works.clear();
        }
        g.works.insert(work_id, b);
        Ok(v)
    }

    pub fn submit_work(&self, ip: Option<std::net::IpAddr>, work_id: &str, nonce: u64) -> Value {
        let b = {
            let mut g = self.0.lock().unwrap();
            g.stats.submissions += 1;
            if let Some(ip) = ip {
                let e = g.submit_buckets.entry(ip).or_insert((Instant::now(), 0));
                if e.0.elapsed() > Duration::from_secs(1) {
                    *e = (Instant::now(), 0);
                }
                e.1 += 1;
                if e.1 > 50 {
                    g.stats.submissions_rate_limited += 1;
                    return json!({"accepted": false, "error": "rate limited"});
                }
            }
            match g.works.get(work_id) {
                Some(b) => b.clone(),
                None => return json!({"accepted": false, "error": "unknown or stale work_id (tip changed)"}),
            }
        };
        let mut b = b;
        b.header.nonce = nonce;
        let h = b.hash();
        match self.process_block(b, None) {
            Ok(AddResult::NewTip { .. }) | Ok(AddResult::SideChain) => {
                self.0.lock().unwrap().stats.submissions_accepted += 1;
                json!({"accepted": true, "block_hash": to_hex(&h)})
            }
            Ok(AddResult::Duplicate) => json!({"accepted": false, "error": "duplicate block"}),
            Ok(AddResult::Orphan) => json!({"accepted": false, "error": "stale work (parent unknown)"}),
            Err(e) => json!({"accepted": false, "error": e}),
        }
    }

    pub fn status(&self) -> Value {
        let g = self.0.lock().unwrap();
        let tip = g.chain.tip();
        let tgt = tip.block.header.target_u256();
        json!({
            "api_version": 1,
            "chain_id": g.chain.cfg.chain_id,
            "profile": g.chain.profile.name,
            "genesis": to_hex(&g.chain.genesis_hash),
            "height": g.chain.height(),
            "best_hash": to_hex(g.chain.best.last().unwrap()),
            "target": tgt.to_hex(),
            "difficulty_log2": 256.0 - tgt.log2_f64(),
            "next_target": g.chain.expected_target(g.chain.best.last().unwrap()).to_hex(),
            "cum_work_log2": tip.cum_work.log2_f64(),
            "tip_timestamp": tip.block.header.timestamp,
            "peer_count": g.peers.values().filter(|p| p.handshaken).count(),
            "mempool": g.mempool.txs.len(),
            "orphans": g.orphans.len(),
            "supply": g.chain.state.supply.to_string(),
            "reorgs": g.chain.reorg_count,
            "max_reorg_depth": g.chain.max_reorg_depth,
            "invalid_blocks": g.chain.invalid_count,
            "stats": g.stats,
            "signature_schemes": ["SLH-DSA-SHA2-128s", "SLH-DSA-SHA2-192s"],
            "crypto": "no elliptic-curve or RSA primitives in consensus; SLH-DSA signatures; SHA-256/SHAKE256 hashing",
        })
    }

    pub fn block_json(&self, key: &str) -> Option<Value> {
        let g = self.0.lock().unwrap();
        let h = if let Ok(n) = key.parse::<u64>() { *g.chain.best.get(n as usize)? } else { from_hex32(key)? };
        let e = g.chain.entries.get(&h)?;
        let in_best = g.chain.best.get(e.height as usize) == Some(&h);
        Some(json!({
            "hash": to_hex(&h),
            "height": e.height,
            "in_best_chain": in_best,
            "status": format!("{:?}", e.status),
            "pow_digest": to_hex(&e.pow_digest),
            "cum_work_log2": e.cum_work.log2_f64(),
            "header": e.block.header,
            "coinbase": e.block.coinbase,
            "txs": e.block.txs.iter().map(|t| json!({"txid": to_hex(&t.txid()), "from": address_to_string(&t.from), "to": address_to_string(&t.to),
                "amount": t.amount, "fee": t.fee, "nonce": t.nonce, "scheme": zfx_verify::tx::scheme_name(t.witness.leaf.scheme), "bytes": t.encoded_len()})).collect::<Vec<_>>(),
            "size": e.block.encoded_size(),
        }))
    }

    pub fn blocks_json(&self, from: Option<u64>, count: u64) -> Value {
        let g = self.0.lock().unwrap();
        let top = g.chain.height();
        let start = from.unwrap_or(top);
        let mut v = Vec::new();
        let mut h = start.min(top) as i64;
        while h >= 0 && (v.len() as u64) < count.min(200) {
            let hash = g.chain.best[h as usize];
            let e = &g.chain.entries[&hash];
            v.push(json!({"height": h, "hash": to_hex(&hash), "timestamp": e.block.header.timestamp, "txs": e.block.txs.len(),
                "miner": address_to_string(&e.block.coinbase.to), "reward": e.block.coinbase.amount,
                "target": e.block.header.target_u256().to_hex(), "difficulty_log2": 256.0 - e.block.header.target_u256().log2_f64(),
                "nonce": e.block.header.nonce.to_string()}));
            h -= 1;
        }
        json!(v)
    }

    pub fn account_json(&self, a: &Address) -> Value {
        let g = self.0.lock().unwrap();
        let acc = g.chain.state.account(a);
        let pending: Vec<Value> = g.mempool.txs.values().filter(|t| &t.from == a || &t.to == a).map(|t| json!({"txid": to_hex(&t.txid()), "nonce": t.nonce, "amount": t.amount})).collect();
        let next_nonce = acc.nonce + g.mempool.txs.values().filter(|t| &t.from == a).count() as u64;
        json!({"address": address_to_string(a), "balance": acc.balance, "nonce": acc.nonce, "next_nonce": next_nonce, "pending": pending})
    }

    pub fn tx_json(&self, id: &Hash32) -> Option<Value> {
        let g = self.0.lock().unwrap();
        if let Some(t) = g.mempool.txs.get(id) {
            return Some(json!({"status": "mempool", "tx": t}));
        }
        let (bh, height) = g.chain.idx.tx.get(id)?;
        let t = g.chain.entries[bh].block.txs.iter().find(|t| &t.txid() == id)?;
        Some(json!({"status": "confirmed", "block": to_hex(bh), "height": height, "confirmations": g.chain.height() - height + 1, "tx": t}))
    }

    pub fn metrics(&self) -> String {
        let g = self.0.lock().unwrap();
        let s = &g.stats;
        format!(
            "zfx_height {}\nzfx_peers {}\nzfx_mempool {}\nzfx_blocks_accepted {}\nzfx_blocks_rejected {}\nzfx_submissions {}\nzfx_submissions_accepted {}\nzfx_reorgs {}\nzfx_invalid_blocks {}\nzfx_difficulty_log2 {}\nzfx_last_pow_verify_ms {}\n",
            g.chain.height(), g.peers.len(), g.mempool.txs.len(), s.blocks_accepted, s.blocks_rejected, s.submissions, s.submissions_accepted,
            g.chain.reorg_count, g.chain.invalid_count, 256.0 - g.chain.tip().block.header.target_u256().log2_f64(), s.last_pow_verify_ms
        )
    }

    pub fn peers_json(&self) -> Value {
        let g = self.0.lock().unwrap();
        json!(g.peers.values().map(|p| json!({"addr": p.addr.to_string(), "height": p.height, "outbound": p.outbound, "handshaken": p.handshaken})).collect::<Vec<_>>())
    }
}

pub fn parse_address(s: &str) -> Result<Address, String> {
    address_from_string(s)
}
