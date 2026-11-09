//! zfx-wallet: CLI wallet.
//!
//!   zfx-wallet new      [--wallet FILE]              create a seed file (0600)
//!   zfx-wallet address  [--wallet FILE]              print P2MR address + leaves
//!   zfx-wallet balance  [--wallet FILE] [--node URL] [--address zfx1...]
//!   zfx-wallet send     --to zfx1... --amount N [--fee F] [--leaf 0|1] [--wallet FILE] [--node URL]
//!   zfx-wallet tx       TXID [--node URL]
//!
//! The wallet file holds a 32-byte seed. Every SLH-DSA key derives from it
//! (FIPS 205 keygen_from_seed over SHAKE256-derived seeds), so the file is
//! the only secret. SLH-DSA is stateless: re-signing with the same leaf is
//! safe, unlike XMSS.

use rand_core::{OsRng, RngCore};
use serde_json::Value;
use zfx_verify::tx::{address_from_string, address_to_string, scheme_name, P2mrKeys, Transaction};

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
}

fn load_seed(path: &str) -> [u8; 32] {
    let s = std::fs::read_to_string(path).unwrap_or_else(|_| die(&format!("cannot read wallet {} (run `zfx-wallet new`)", path)));
    let v: Value = serde_json::from_str(&s).unwrap_or_else(|e| die(&e.to_string()));
    hex::decode(v["seed"].as_str().unwrap_or("")).ok().and_then(|b| b.try_into().ok()).unwrap_or_else(|| die("bad seed in wallet file"))
}

fn die(m: &str) -> ! {
    eprintln!("error: {}", m);
    std::process::exit(1)
}

fn get(node: &str, path: &str) -> Value {
    ureq::get(&format!("{}{}", node, path)).call().map(|r| r.into_json().unwrap()).unwrap_or_else(|e| match e {
        ureq::Error::Status(_, r) => r.into_json().unwrap_or(Value::Null),
        e => die(&e.to_string()),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| "help".into());
    let wallet = arg(&args, "--wallet").unwrap_or_else(|| format!("{}/.zeroflux/wallet.json", std::env::var("HOME").unwrap_or(".".into())));
    let node = arg(&args, "--node").unwrap_or_else(|| "http://127.0.0.1:9441".into());
    match cmd.as_str() {
        "new" => {
            if std::path::Path::new(&wallet).exists() && !args.iter().any(|a| a == "--force") {
                die(&format!("{} exists (use --force to overwrite)", wallet));
            }
            let mut seed = [0u8; 32];
            if let Some(h) = arg(&args, "--seed-hex") {
                seed = hex::decode(h).ok().and_then(|b| b.try_into().ok()).unwrap_or_else(|| die("--seed-hex must be 32 bytes"));
            } else {
                OsRng.fill_bytes(&mut seed);
            }
            if let Some(p) = std::path::Path::new(&wallet).parent() {
                std::fs::create_dir_all(p).ok();
            }
            let k = P2mrKeys::from_seed(&seed);
            let v = serde_json::json!({"format": "zfx-wallet-v0.1", "seed": hex::encode(seed), "address": address_to_string(&k.address())});
            std::fs::write(&wallet, serde_json::to_string_pretty(&v).unwrap()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&wallet, std::fs::Permissions::from_mode(0o600)).ok();
            }
            println!("{}", address_to_string(&k.address()));
        }
        "address" => {
            let k = P2mrKeys::from_seed(&load_seed(&wallet));
            println!("address {}", address_to_string(&k.address()));
            for (i, l) in k.leaves().iter().enumerate() {
                println!("  leaf {} {} pubkey {}", i, scheme_name(l.scheme), hex::encode(&l.pubkey));
            }
        }
        "balance" => {
            let a = arg(&args, "--address").unwrap_or_else(|| address_to_string(&P2mrKeys::from_seed(&load_seed(&wallet)).address()));
            println!("{}", serde_json::to_string_pretty(&get(&node, &format!("/v1/account/{}", a))).unwrap());
        }
        "send" => {
            let to = address_from_string(&arg(&args, "--to").unwrap_or_else(|| die("--to required"))).unwrap_or_else(|e| die(&e));
            let amount: u64 = arg(&args, "--amount").and_then(|v| v.parse().ok()).unwrap_or_else(|| die("--amount required"));
            let fee: u64 = arg(&args, "--fee").and_then(|v| v.parse().ok()).unwrap_or(1000);
            let leaf: usize = arg(&args, "--leaf").and_then(|v| v.parse().ok()).unwrap_or(0);
            let k = P2mrKeys::from_seed(&load_seed(&wallet));
            let st = get(&node, "/v1/status");
            let cid = st["chain_id"].as_u64().unwrap_or_else(|| die("node unreachable")) as u32;
            let acct = get(&node, &format!("/v1/account/{}", address_to_string(&k.address())));
            let nonce = acct["next_nonce"].as_u64().unwrap_or(0);
            let t0 = std::time::Instant::now();
            let tx: Transaction = k.sign_tx(cid, to, amount, fee, nonce, leaf).unwrap_or_else(|e| die(&e));
            eprintln!("signed with {} in {:.2}s ({} bytes)", scheme_name(tx.witness.leaf.scheme), t0.elapsed().as_secs_f64(), tx.encoded_len());
            let r = ureq::post(&format!("{}/v1/tx", node)).send_json(serde_json::to_value(&tx).unwrap());
            let v: Value = match r {
                Ok(r) => r.into_json().unwrap(),
                Err(ureq::Error::Status(_, r)) => r.into_json().unwrap_or(Value::Null),
                Err(e) => die(&e.to_string()),
            };
            println!("{}", v);
            if v["accepted"] != true {
                std::process::exit(2);
            }
        }
        "tx" => {
            let id = args.get(1).cloned().unwrap_or_else(|| die("txid required"));
            println!("{}", serde_json::to_string_pretty(&get(&node, &format!("/v1/tx/{}", id))).unwrap());
        }
        _ => {
            eprintln!("usage: zfx-wallet new|address|balance|send|tx [--wallet FILE] [--node URL]");
        }
    }
}
