//! zfx-node: ZeroFlux full node (CPU-only; no GPU runtime required).
//!
//!   zfx-node [--devnet | --testnet | --config FILE] [--data-dir DIR]
//!            [--p2p-port 9440] [--rpc-port 9441] [--peer HOST:PORT]...
//!            [--miner-address zfx1...] [--explorer-dir DIR]
//!   zfx-node print-config [--devnet|--testnet]

use zfx_node::config::ChainConfig;
use zfx_node::node::{parse_address, Node};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let all = |k: &str| -> Vec<String> { args.iter().enumerate().filter(|(_, a)| *a == k).filter_map(|(i, _)| args.get(i + 1).cloned()).collect() };
    let mut cfg = if args.iter().any(|a| a == "--testnet") { ChainConfig::testnet_v01() } else { ChainConfig::devnet() };
    if let Some(f) = val("--config") {
        cfg = serde_json::from_str(&std::fs::read_to_string(&f).expect("read config")).expect("parse config");
    }
    if args.first().map(String::as_str) == Some("print-config") {
        println!("{}", serde_json::to_string_pretty(&cfg).unwrap());
        return;
    }
    let p2p: u16 = val("--p2p-port").map(|v| v.parse().unwrap()).unwrap_or(9440);
    let rpc: u16 = val("--rpc-port").map(|v| v.parse().unwrap()).unwrap_or(9441);
    let miner = val("--miner-address").map(|a| parse_address(&a).expect("bad --miner-address"));
    let node = Node::new(cfg.clone(), val("--data-dir").as_deref(), miner, p2p).unwrap_or_else(|e| {
        eprintln!("fatal: {}", e);
        std::process::exit(1)
    });
    {
        let g = node.0.lock().unwrap();
        eprintln!(
            "[node] chain {:#x} profile {} genesis {} height {} | p2p :{} rpc :{}",
            cfg.chain_id, g.chain.profile.name, hex::encode(g.chain.genesis_hash), g.chain.height(), p2p, rpc
        );
    }
    node.listen(p2p).expect("p2p listen");
    for p in all("--peer") {
        node.maintain_peer(p);
    }
    zfx_node::api::serve(node.clone(), rpc, val("--explorer-dir"), 8).expect("rpc listen");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
