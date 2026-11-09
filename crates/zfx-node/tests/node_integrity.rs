//! Node-level integrity tests: block-log corruption vs torn writes,
//! repair, snapshots, admin auth, JSON-RPC errors, encrypted P2P sync.

use serde_json::json;
use zfx_node::api::{codes, rpc};
use zfx_node::chain::Chain;
use zfx_node::config::ChainConfig;
use zfx_node::node::{now_secs, Node, NodeOptions};
use zfx_verify::block::Block;
use zfx_verify::pow;
use zfx_verify::U256;

fn cfg() -> ChainConfig {
    let mut c = ChainConfig::devnet();
    c.genesis_timestamp = now_secs() - 100_000;
    c.genesis_target = U256::MAX.shr(4).to_hex();
    c.fixed_difficulty = true;
    c.coinbase_maturity = 2;
    c
}

fn mine(mut b: Block, chain: &mut Chain) -> Block {
    let anchor = chain.anchor_for(&b.header.parent, b.header.height).unwrap();
    let w = chain.weights_for(&anchor);
    let c = pow::challenge(&b.header);
    let t = b.header.target_u256();
    for n in 0.. {
        if pow::meets_target(&pow::attempt(&w, &c, n), &t) {
            b.header.nonce = n;
            return b;
        }
    }
    unreachable!()
}

fn mine_on(node: &Node, n: u64) {
    for i in 0..n {
        let b = { node.lock().chain.template([4u8; 32], &[], now_secs() - 5000 + i) };
        let b = { let mut g = node.lock(); mine(b, &mut g.chain) };
        node.process_block(b, None).unwrap();
    }
}

fn tmpdir(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("zfx-{}-{}", tag, rand::random::<u32>()));
    d.to_str().unwrap().to_string()
}

#[test]
fn corruption_is_detected_and_repair_truncates() {
    let d = tmpdir("corrupt");
    {
        let node = Node::new(cfg(), Some(&d), None, 0).unwrap();
        mine_on(&node, 8);
        node.clean_shutdown();
    }
    let log = std::path::Path::new(&d).join("blocks.zfxlog");
    // Flip one byte inside record 5 (complete line => corruption, not torn write).
    let mut data = std::fs::read(&log).unwrap();
    let mut line = 0;
    let mut pos = 0;
    for (i, c) in data.iter().enumerate() {
        if *c == b'\n' {
            line += 1;
            if line == 4 {
                pos = i + 80;
                break;
            }
        }
    }
    data[pos] = if data[pos] == b'a' { b'b' } else { b'a' };
    std::fs::write(&log, &data).unwrap();
    let err = Node::new(cfg(), Some(&d), None, 0).err().unwrap();
    assert!(err.contains("corrupt at line 5"), "{}", err);
    // --repair keeps the 4 good records and revalidates them.
    let node = Node::with_options(cfg(), NodeOptions { data_dir: Some(d.clone()), repair: true, ..Default::default() }).unwrap();
    assert_eq!(node.lock().chain.height(), 4);
    assert_eq!(node.lock().load_report["repaired_from_line"], 5);
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn torn_tail_recovers_and_unclean_shutdown_is_reported() {
    let d = tmpdir("torn");
    {
        let node = Node::new(cfg(), Some(&d), None, 0).unwrap();
        mine_on(&node, 5);
        // no clean_shutdown: simulate a crash
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(std::path::Path::new(&d).join("blocks.zfxlog")).unwrap();
    f.write_all(b"zfx2 deadbeef {\"header\":").unwrap();
    drop(f);
    let node = Node::new(cfg(), Some(&d), None, 0).unwrap();
    let g = node.lock();
    assert_eq!(g.chain.height(), 5);
    assert_eq!(g.load_report["torn_tail_dropped"], true);
    assert_eq!(g.load_report["previous_shutdown_clean"], false);
    drop(g);
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn snapshot_export_verify_and_tamper() {
    let d = tmpdir("snap");
    let node = Node::new(cfg(), Some(&d), None, 0).unwrap();
    mine_on(&node, 6);
    let p = format!("{}/s.json", d);
    node.snapshot_export(&p).unwrap();
    assert_eq!(node.snapshot_verify(&p).unwrap()["ok"], true);
    // tamper with a balance
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    v["state"]["accounts"][0][1] = json!(999_999_999u64);
    std::fs::write(&p, v.to_string()).unwrap();
    assert!(node.snapshot_verify(&p).unwrap_err().contains("checksum"));
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn admin_rpc_requires_token_and_rpc_errors_are_structured() {
    let node = Node::with_options(cfg(), NodeOptions { admin_token: Some("s3cret-token".into()), ..Default::default() }).unwrap();
    let call = |auth: Option<&str>, m: &str, p: serde_json::Value| rpc(&node, None, auth, &json!({"jsonrpc": "2.0", "id": 1, "method": m, "params": p}));
    let r = call(None, "zfx_admin_snapshot_export", json!({"path": "/tmp/x.json"}));
    assert_eq!(r["error"]["code"], codes::UNAUTHORIZED);
    let r = call(Some("wrong-token!"), "zfx_admin_snapshot_export", json!({"path": "/tmp/x.json"}));
    assert_eq!(r["error"]["code"], codes::UNAUTHORIZED);
    assert_eq!(call(None, "zfx_nope", json!({}))["error"]["code"], codes::METHOD_NOT_FOUND);
    assert_eq!(call(None, "zfx_node_get_transaction", json!({"txid": "zz"}))["error"]["code"], codes::INVALID_PARAMS);
    assert_eq!(call(None, "zfx_node_get_transaction", json!({"txid": "00".repeat(32)}))["error"]["code"], codes::NOT_FOUND);
    assert_eq!(call(None, "zfx_submit_mining_result", json!({"work_id": "x", "nonce": 1}))["error"]["code"], codes::STALE_WORK);
    assert!(call(None, "zfx_node_get_tip", json!({}))["result"]["height"].is_u64());
    let bad = rpc(&node, None, None, &json!({"id": 1, "method": "zfx_node_get_tip"}));
    assert_eq!(bad["error"]["code"], codes::INVALID_REQUEST);
}

#[test]
fn two_nodes_sync_over_encrypted_transport() {
    let a = Node::with_options(cfg(), NodeOptions { identity_seed: Some([1; 32]), listen_port: 0, ..Default::default() }).unwrap();
    mine_on(&a, 7);
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    a.listen(port).unwrap();
    let b = Node::with_options(cfg(), NodeOptions { identity_seed: Some([2; 32]), ..Default::default() }).unwrap();
    b.maintain_peer(format!("127.0.0.1:{}", port));
    for _ in 0..100 {
        if b.lock().chain.height() == 7 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(b.lock().chain.best, a.lock().chain.best, "b synced a's chain over ZFXP2P/2");
    assert!(a.lock().stats.handshakes_ok >= 1);
}

#[test]
fn long_sync_beyond_one_inventory_batch() {
    // 1200 blocks > MAX_INV (500): the syncing node must keep requesting.
    let mut c = cfg();
    c.genesis_target = U256::MAX.shr(1).to_hex(); // trivial PoW: fast to build
    let a = Node::with_options(c.clone(), NodeOptions { identity_seed: Some([5; 32]), ..Default::default() }).unwrap();
    for i in 0..1200u64 {
        let b = { a.lock().chain.template([4u8; 32], &[], now_secs() - 100_000 + i) };
        let b = { let mut g = a.lock(); mine(b, &mut g.chain) };
        a.process_block(b, None).unwrap();
    }
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    a.listen(port).unwrap();
    let b = Node::with_options(c, NodeOptions { identity_seed: Some([6; 32]), ..Default::default() }).unwrap();
    b.maintain_peer(format!("127.0.0.1:{}", port));
    for _ in 0..300 {
        if b.lock().chain.height() == 1200 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(b.lock().chain.height(), 1200, "synced past the 500-hash inventory limit");
}

#[test]
fn backup_and_restore_reproduce_state() {
    // Backup = copy of a cleanly shut down data dir (block log + snapshot).
    let d = tmpdir("backup");
    let (best, root) = {
        let node = Node::new(cfg(), Some(&d), None, 0).unwrap();
        mine_on(&node, 9);
        node.clean_shutdown();
        let g = node.lock();
        (*g.chain.best.last().unwrap(), g.chain.tip().block.header.state_root)
    };
    let r = tmpdir("restore");
    std::fs::create_dir_all(&r).unwrap();
    for f in ["blocks.zfxlog", "chain.json"] {
        std::fs::copy(std::path::Path::new(&d).join(f), std::path::Path::new(&r).join(f)).unwrap();
    }
    std::fs::create_dir_all(format!("{}/snapshots", r)).unwrap();
    std::fs::copy(format!("{}/snapshots/latest.json", d), format!("{}/snapshots/latest.json", r)).unwrap();
    let node = Node::new(cfg(), Some(&r), None, 0).unwrap();
    let g = node.lock();
    assert_eq!(*g.chain.best.last().unwrap(), best);
    assert_eq!(g.chain.tip().block.header.state_root, root);
    drop(g);
    assert_eq!(node.snapshot_verify(&format!("{}/snapshots/latest.json", r)).unwrap()["ok"], true);
    std::fs::remove_dir_all(&d).ok();
    std::fs::remove_dir_all(&r).ok();
}
