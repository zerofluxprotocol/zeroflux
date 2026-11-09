//! zfx-p2ptool: adversarial and diagnostic P2P client for tests.
//!
//!   zfx-p2ptool inject-invalid-block --peer HOST:PORT --rpc URL --config CHAIN.json
//!       handshake as a fresh identity, then send a block with a valid
//!       template but a nonce that fails PoW; prints whether the node
//!       disconnected us.
//!   zfx-p2ptool garbage --peer HOST:PORT [--count N]
//!       send random bytes / oversized and malformed frames N times.
//!   zfx-p2ptool proxy --listen PORT --to HOST:PORT --dump FILE
//!       transparent TCP relay that records every byte (encryption evidence).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use zfx_node::config::ChainConfig;
use zfx_node::node::Msg;
use zfx_node::p2p_crypto::{handshake_initiator, Identity};

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
}

fn genesis_of(cfg_path: &str) -> [u8; 32] {
    let cfg: ChainConfig = serde_json::from_str(&std::fs::read_to_string(cfg_path).unwrap()).unwrap();
    zfx_node::chain::Chain::new(cfg).unwrap().genesis_hash
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("inject-invalid-block") => {
            let peer = arg(&args, "--peer").unwrap();
            let rpc = arg(&args, "--rpc").unwrap();
            let cfg_path = arg(&args, "--config").unwrap();
            let genesis = genesis_of(&cfg_path);
            let cfg: ChainConfig = serde_json::from_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
            // A real template from the node, with a nonce that fails PoW.
            let tip: serde_json::Value = ureq_get(&format!("{}/v1/status", rpc));
            let blk_json: serde_json::Value = ureq_get(&format!("{}/v1/block/{}", rpc, tip["height"]));
            let miner = blk_json["coinbase"]["to"].as_str().unwrap().to_string();
            let work: serde_json::Value = ureq_get(&format!("{}/v1/work?address={}", rpc, miner));
            let hwn = hex::decode(work["header_without_nonce"].as_str().unwrap()).unwrap();
            let mut full = hwn.clone();
            full.extend_from_slice(&7u64.to_le_bytes());
            let header = zfx_verify::BlockHeader::decode(&full).unwrap();
            let block = zfx_verify::Block {
                header,
                coinbase: zfx_verify::tx::Coinbase {
                    height: work["height"].as_u64().unwrap(),
                    to: zfx_verify::tx::address_from_string(&miner).unwrap(),
                    amount: cfg.block_subsidy,
                },
                txs: vec![],
                bridge_ops: vec![],
            };
            let s = TcpStream::connect(&peer).unwrap();
            let me = Identity::from_seed(&rand::random::<[u8; 32]>());
            let mut sess = handshake_initiator(s, &me, &genesis, None).expect("handshake");
            let hello = Msg::Hello { protocol: 2, chain_id: cfg.chain_id, genesis: hex::encode(genesis), height: 0, best: hex::encode(genesis), listen: None };
            sess.writer.send(&serde_json::to_vec(&hello).unwrap()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            sess.writer.send(&serde_json::to_vec(&Msg::Block { block }).unwrap()).unwrap();
            // Drain until the node closes the link (or 5 s pass).
            let t = std::time::Instant::now();
            let mut closed = false;
            while t.elapsed().as_secs() < 5 {
                if sess.reader.recv().is_err() {
                    closed = true;
                    break;
                }
            }
            println!("{}", serde_json::json!({"handshake": "ok", "invalid_block_sent": true, "disconnected_by_node": closed}));
        }
        Some("garbage") => {
            let peer = arg(&args, "--peer").unwrap();
            let n: usize = arg(&args, "--count").and_then(|v| v.parse().ok()).unwrap_or(50);
            let mut sent = 0;
            for i in 0..n {
                if let Ok(mut s) = TcpStream::connect(&peer) {
                    let payload: Vec<u8> = match i % 4 {
                        0 => (0..rand::random::<u16>() % 4096).map(|_| rand::random::<u8>()).collect(),
                        1 => (u32::MAX).to_be_bytes().to_vec(), // absurd length prefix
                        2 => {
                            let mut v = 40u32.to_be_bytes().to_vec();
                            v.extend_from_slice(b"ZFXP2P/2");
                            v.extend((0..32).map(|_| rand::random::<u8>()));
                            v
                        }
                        _ => b"{\"type\":\"hello\"}\n".to_vec(), // plaintext v1-style message
                    };
                    let _ = s.write_all(&payload);
                    let _ = s.shutdown(std::net::Shutdown::Write);
                    let mut sink = [0u8; 64];
                    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                    let _ = s.read(&mut sink);
                    sent += 1;
                }
            }
            println!("{}", serde_json::json!({"malformed_connections": sent}));
        }
        Some("proxy") => {
            let listen: u16 = arg(&args, "--listen").unwrap().parse().unwrap();
            let to = arg(&args, "--to").unwrap();
            let dump = arg(&args, "--dump").unwrap();
            let l = TcpListener::bind(("127.0.0.1", listen)).unwrap();
            for (conn, inbound) in l.incoming().flatten().enumerate() {
                let Ok(outbound) = TcpStream::connect(&to) else { continue };
                // One file per connection and direction: .i2r (initiator->responder), .r2i
                for (dir, mut a, mut b) in [("i2r", inbound.try_clone().unwrap(), outbound.try_clone().unwrap()), ("r2i", outbound, inbound)] {
                    let dump = format!("{}.{}.{}", dump, conn, dir);
                    std::thread::spawn(move || {
                        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dump).unwrap();
                        let mut buf = [0u8; 65536];
                        loop {
                            match a.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let _ = f.write_all(&buf[..n]);
                                    if b.write_all(&buf[..n]).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        let _ = b.shutdown(std::net::Shutdown::Both);
                    });
                }
            }
        }
        _ => eprintln!("usage: zfx-p2ptool inject-invalid-block|garbage|proxy ..."),
    }
}

fn ureq_get(url: &str) -> serde_json::Value {
    let out = std::process::Command::new("curl").args(["-s", url]).output().expect("curl");
    serde_json::from_slice(&out.stdout).expect("json")
}
