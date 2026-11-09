//! Deterministic mutation fuzzing of every untrusted-input parser.
//! Each target starts from valid seeds (golden vectors, real blocks/txs),
//! applies random byte/field mutations and truncations, and asserts that the
//! parser/validator never panics (errors are fine). Iteration counts are
//! recorded by `make security-test` in bench/results/security.json.
//!
//! ZFX_FUZZ_ITERS overrides the per-target iteration count (default 3000).

use rand::{rngs::StdRng, Rng, SeedableRng};
use std::panic::{catch_unwind, AssertUnwindSafe};
use zfx_node::node::Msg;
use zfx_verify::block::Block;
use zfx_verify::header::BlockHeader;
use zfx_verify::tx::{P2mrKeys, Transaction};

fn iters() -> usize {
    std::env::var("ZFX_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000)
}

fn mutate(rng: &mut StdRng, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    match rng.gen_range(0..6) {
        0 => {
            for _ in 0..rng.gen_range(1..8) {
                if !v.is_empty() {
                    let i = rng.gen_range(0..v.len());
                    v[i] = rng.gen();
                }
            }
        }
        1 => v.truncate(rng.gen_range(0..=v.len())),
        2 => {
            let n = rng.gen_range(1..64);
            let i = rng.gen_range(0..=v.len());
            let ins: Vec<u8> = (0..n).map(|_| rng.gen()).collect();
            v.splice(i..i, ins);
        }
        3 => v = (0..rng.gen_range(0..512)).map(|_| rng.gen()).collect(),
        4 => {
            if v.len() > 2 {
                let a = rng.gen_range(0..v.len());
                let b = rng.gen_range(0..v.len());
                v.swap(a, b);
            }
        }
        _ => v.extend_from_slice(seed),
    }
    v
}

fn run<F: Fn(&[u8])>(name: &str, seeds: &[Vec<u8>], f: F) {
    let mut rng = StdRng::seed_from_u64(0x2f4a);
    let n = iters();
    for i in 0..n {
        let s = &seeds[i % seeds.len()];
        let input = mutate(&mut rng, s);
        let r = catch_unwind(AssertUnwindSafe(|| f(&input)));
        assert!(r.is_ok(), "{}: panic on input #{} ({} bytes): {:?}", name, i, input.len(), hex::encode(&input[..input.len().min(64)]));
    }
    eprintln!("fuzz {}: {} inputs, 0 panics", name, n);
}

fn sample_tx() -> Transaction {
    P2mrKeys::from_seed(&[9; 32]).sign_tx_memo(7, [3; 32], 100, 10, 0, 0, Some("bob.near".into())).unwrap()
}

#[test]
fn fuzz_header_decode() {
    let h = BlockHeader { version: 2, chain_id: 1, height: 5, parent: [1; 32], tx_root: [2; 32], state_root: [3; 32], timestamp: 9, target: [0xff; 32], nonce: 4 };
    run("header_decode", &[h.encode().to_vec()], |b| {
        let _ = BlockHeader::decode(b);
    });
}

#[test]
fn fuzz_tx_json_and_authorization() {
    let tx = sample_tx();
    let seeds = vec![serde_json::to_vec(&tx).unwrap()];
    run("tx_json_verify", &seeds, |b| {
        if let Ok(t) = serde_json::from_slice::<Transaction>(b) {
            let _ = t.verify_authorization();
            let _ = t.encode();
            let _ = t.txid();
        }
    });
}

#[test]
fn fuzz_block_json_and_structure() {
    let tx = sample_tx();
    let b = Block {
        header: BlockHeader { version: 2, chain_id: 7, height: 1, parent: [0; 32], tx_root: [0; 32], state_root: [0; 32], timestamp: 1, target: [0xff; 32], nonce: 0 },
        coinbase: zfx_verify::tx::Coinbase { height: 1, to: [1; 32], amount: 5 },
        txs: vec![tx],
        bridge_ops: vec![],
    };
    run("block_json_structure", &[serde_json::to_vec(&b).unwrap()], |x| {
        if let Ok(b) = serde_json::from_slice::<Block>(x) {
            let _ = b.check_structure();
            let _ = b.compute_tx_root();
        }
    });
}

#[test]
fn fuzz_p2p_messages() {
    let seeds: Vec<Vec<u8>> = vec![
        serde_json::to_vec(&Msg::Hello { protocol: 2, chain_id: 1, genesis: "00".repeat(32), height: 3, best: "11".repeat(32), listen: Some(9440) }).unwrap(),
        serde_json::to_vec(&Msg::GetBlocks { locator: vec!["ab".repeat(32); 5] }).unwrap(),
        serde_json::to_vec(&Msg::Addr { addrs: vec!["127.0.0.1:9440".into(); 3] }).unwrap(),
        serde_json::to_vec(&Msg::Tx { tx: sample_tx() }).unwrap(),
    ];
    run("p2p_msg_json", &seeds, |b| {
        let _ = serde_json::from_slice::<Msg>(b);
    });
}

#[test]
fn fuzz_bridge_ops() {
    use zfx_verify::bridge::{BridgeOp, Release};
    let op = BridgeOp::Release(Release { epoch: 0, withdrawal_id: [1; 32], near_burn_ref: "w:1:2".into(), recipient: [2; 32], amount: 5, expiry_height: 9, approvals: vec![] });
    run("bridge_op_json", &[serde_json::to_vec(&op).unwrap()], |b| {
        if let Ok(o) = serde_json::from_slice::<BridgeOp>(b) {
            let _ = zfx_verify::bridge::encode_op(&o);
            let _ = zfx_verify::bridge::op_id(&o);
        }
    });
}

#[test]
fn fuzz_store_records() {
    let dir = std::env::temp_dir().join(format!("zfx-fuzz-store-{}", rand::random::<u32>()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("blocks.zfxlog");
    let mut seed = b"zfx2 ".to_vec();
    seed.extend_from_slice(b"00ff 00\n");
    run("store_load", &[seed], |b| {
        std::fs::write(&p, b).unwrap();
        let _ = zfx_node::store::BlockLog::load(&p, false);
        std::fs::write(&p, b).unwrap();
        let _ = zfx_node::store::BlockLog::load(&p, true);
    });
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn fuzz_snapshot_json() {
    let st = zfx_verify::state::State::default();
    let s = zfx_node::store::Snapshot::new(&[1; 32], 3, &[2; 32], &st);
    run("snapshot_verify", &[serde_json::to_vec(&s).unwrap()], |b| {
        if let Ok(sn) = serde_json::from_slice::<zfx_node::store::Snapshot>(b) {
            let _ = sn.verify(None);
        }
    });
}

#[test]
fn fuzz_handshake_bytes() {
    // Raw bytes thrown at a live responder must fail cleanly.
    use std::io::Write;
    let id = std::sync::Arc::new(zfx_node::p2p_crypto::Identity::from_seed(&[8; 32]));
    let mut rng = StdRng::seed_from_u64(7);
    let n = (iters() / 30).max(50);
    for _ in 0..n {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let id2 = id.clone();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            catch_unwind(AssertUnwindSafe(|| zfx_node::p2p_crypto::handshake_responder(s, &id2, &[7; 32], None).is_err())).is_ok()
        });
        let mut c = std::net::TcpStream::connect(addr).unwrap();
        let len: usize = rng.gen_range(0..3000);
        let mut junk: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
        if rng.gen_bool(0.5) && junk.len() > 12 {
            let n4 = (junk.len() - 4) as u32;
            junk[..4].copy_from_slice(&n4.to_be_bytes());
            junk[4..12].copy_from_slice(b"ZFXP2P/2");
        }
        let _ = c.write_all(&junk);
        drop(c);
        assert!(t.join().unwrap(), "handshake responder panicked on junk");
    }
    eprintln!("fuzz handshake: {} connections, 0 panics", n);
}
