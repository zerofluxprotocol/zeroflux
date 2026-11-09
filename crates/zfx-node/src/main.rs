//! zfx-node: ZeroFlux full node (CPU-only; no GPU runtime required).
//!
//!   zfx-node [--devnet | --testnet | --config FILE] [--data-dir DIR]
//!            [--p2p-port 9440] [--rpc-port 9441] [--rpc-bind 127.0.0.1]
//!            [--peer HOST:PORT]... [--peer-allow FILE] [--no-discovery]
//!            [--miner-address zfx1...] [--explorer-dir DIR] [--repair]
//!            [--faucet-seed-hex HEX --faucet-amount N --faucet-cooldown S]
//!            [--bridge-confirmations K] [--admin-token-file FILE]
//!   zfx-node print-config [--devnet|--testnet]
//!   zfx-node check-store --data-dir DIR [--config FILE]   verify the block log offline
//!
//! Admin JSON-RPC (zfx_admin_*) needs `Authorization: Bearer <token>`; the
//! token is read from --admin-token-file or created (0600) as
//! DATA_DIR/admin_token. SIGINT/SIGTERM trigger a clean shutdown
//! (snapshot + CLEAN_SHUTDOWN marker).

const USAGE: &str = r#"zfx-node: ZeroFlux full node (CPU-only; no GPU runtime required).

  zfx-node [--devnet | --testnet | --config FILE] [--data-dir DIR]
           [--p2p-port 9440] [--rpc-port 9441] [--rpc-bind 127.0.0.1]
           [--peer HOST:PORT]... [--peer-allow FILE] [--no-discovery]
           [--miner-address zfx1...] [--explorer-dir DIR] [--repair]
           [--faucet-seed-hex HEX --faucet-amount N --faucet-cooldown S]
           [--bridge-confirmations K] [--admin-token-file FILE]
  zfx-node print-config [--devnet|--testnet]
  zfx-node check-store --data-dir DIR [--config FILE]   verify the block log offline

Admin JSON-RPC (zfx_admin_*) needs `Authorization: Bearer <token>`; the
token is read from --admin-token-file or created (0600) as
DATA_DIR/admin_token. SIGINT/SIGTERM trigger a clean shutdown
(snapshot + CLEAN_SHUTDOWN marker).
"#;

use zfx_node::config::ChainConfig;
use zfx_node::node::{parse_address, FaucetConfig, Node, NodeOptions};
use zfx_node::store::BlockLog;

fn die(m: &str) -> ! {
    eprintln!("fatal: {}", m);
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", USAGE);
        return;
    }
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let flag = |k: &str| args.iter().any(|a| a == k);
    let all = |k: &str| -> Vec<String> { args.iter().enumerate().filter(|(_, a)| *a == k).filter_map(|(i, _)| args.get(i + 1).cloned()).collect() };
    let mut cfg = if flag("--testnet") { ChainConfig::testnet_v01() } else { ChainConfig::devnet() };
    if let Some(f) = val("--config") {
        cfg = serde_json::from_str(&std::fs::read_to_string(&f).unwrap_or_else(|e| die(&e.to_string()))).unwrap_or_else(|e| die(&e.to_string()));
    }
    match args.first().map(String::as_str) {
        Some("print-config") => {
            println!("{}", serde_json::to_string_pretty(&cfg).unwrap());
            return;
        }
        Some("check-store") => {
            let d = val("--data-dir").unwrap_or_else(|| die("--data-dir required"));
            match BlockLog::load(&std::path::Path::new(&d).join("blocks.zfxlog"), false) {
                Ok((blocks, rep)) => {
                    println!("{}", serde_json::json!({"ok": true, "records": blocks.len(), "torn_tail_dropped": rep.torn_tail_dropped}));
                    return;
                }
                Err(e) => {
                    println!("{}", serde_json::json!({"ok": false, "error": e.to_string()}));
                    std::process::exit(2);
                }
            }
        }
        _ => {}
    }
    let data_dir = val("--data-dir");
    let admin_token = match (val("--admin-token-file"), &data_dir) {
        (Some(f), _) => Some(std::fs::read_to_string(&f).unwrap_or_else(|e| die(&format!("admin token file: {}", e))).trim().to_string()),
        (None, Some(d)) => {
            let p = std::path::Path::new(d).join("admin_token");
            std::fs::create_dir_all(d).ok();
            if !p.exists() {
                let mut b = [0u8; 32];
                rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
                std::fs::write(&p, hex::encode(b)).unwrap_or_else(|e| die(&e.to_string()));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).ok();
                }
            }
            Some(std::fs::read_to_string(&p).unwrap().trim().to_string())
        }
        (None, None) => None,
    };
    let faucet = val("--faucet-seed-hex").map(|h| FaucetConfig {
        seed: hex::decode(&h).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| die("--faucet-seed-hex must be 32 bytes hex")),
        amount: val("--faucet-amount").and_then(|v| v.parse().ok()).unwrap_or(10_0000_0000),
        cooldown_secs: val("--faucet-cooldown").and_then(|v| v.parse().ok()).unwrap_or(60),
    });
    let peer_allow = val("--peer-allow").map(|f| {
        std::fs::read_to_string(&f)
            .unwrap_or_else(|e| die(&e.to_string()))
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| hex::decode(l.trim()).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| die("bad peer id in --peer-allow")))
            .collect::<Vec<[u8; 32]>>()
    });
    let p2p: u16 = val("--p2p-port").map(|v| v.parse().unwrap()).unwrap_or(9440);
    let rpc: u16 = val("--rpc-port").map(|v| v.parse().unwrap()).unwrap_or(9441);
    let opts = NodeOptions {
        data_dir: data_dir.clone(),
        listen_port: p2p,
        default_miner: val("--miner-address").map(|a| parse_address(&a).unwrap_or_else(|e| die(&e))),
        repair: flag("--repair"),
        admin_token,
        faucet,
        peer_allow,
        bridge_confirmations: val("--bridge-confirmations").and_then(|v| v.parse().ok()).unwrap_or(12),
        ..Default::default()
    };
    let node = Node::with_options(cfg.clone(), opts).unwrap_or_else(|e| die(&e));
    {
        let g = node.lock();
        eprintln!(
            "[node] chain {:#x} {} profile {} genesis {} height {} | peer_id {} | p2p :{} (ZFXP2P/2 encrypted) rpc :{}",
            cfg.chain_id,
            cfg.label,
            g.chain.profile.name,
            hex::encode(g.chain.genesis_hash),
            g.chain.height(),
            node.peer_id_hex(),
            p2p,
            rpc
        );
    }
    let n2 = node.clone();
    ctrlc::set_handler(move || {
        n2.clean_shutdown();
        std::process::exit(0);
    })
    .unwrap_or_else(|e| die(&e.to_string()));
    node.listen(p2p).unwrap_or_else(|e| die(&format!("p2p listen: {}", e)));
    for p in all("--peer") {
        node.maintain_peer(p);
    }
    if !flag("--no-discovery") {
        node.run_discovery();
    }
    let bind = val("--rpc-bind").unwrap_or_else(|| "0.0.0.0".into());
    zfx_node::api::serve(node.clone(), &bind, rpc, val("--explorer-dir"), 8).unwrap_or_else(|e| die(&format!("rpc listen: {}", e)));
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
