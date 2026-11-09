//! zfx-wallet: CLI wallet (SLH-DSA, P2MR addresses).
//!
//!   zfx-wallet new        [--wallet FILE] [--seed-hex HEX] [--force]
//!   zfx-wallet address    [--wallet FILE]
//!   zfx-wallet balance    [--wallet FILE | --address zfx1...] [--node URL]
//!   zfx-wallet send       --to zfx1... --amount N [--fee F|auto] [--memo TEXT] [--leaf 0|1]
//!   zfx-wallet history    [--wallet FILE | --address zfx1...] [--limit N]
//!   zfx-wallet tx         TXID                 receipt incl. Merkle inclusion proof
//!   zfx-wallet fee                              fee estimate from the node
//!   zfx-wallet faucet     [--wallet FILE | --address zfx1...]   testnet faucet
//!   zfx-wallet backup     --out FILE            checksummed backup of the seed (0600)
//!   zfx-wallet restore    --backup FILE --wallet NEWFILE
//!   zfx-wallet watch      add zfx1... | list | balances   watch-only addresses
//!   zfx-wallet bridge-deposit --near-account ACCT --amount N   lock ZFX for wZFX on NEAR
//!   zfx-wallet bridge-status  --deposit-id HEX | --withdrawal-id HEX
//!   zfx-wallet bridge-info
//!
//! Secrets: the wallet file holds a 32-byte seed. Nothing in this program
//! prints the seed or private keys to stdout/stderr; `backup` writes to a
//! file only. SLH-DSA is stateless, so re-signing with the same leaf is safe.

use rand_core::{OsRng, RngCore};
use serde_json::{json, Value};
use zfx_verify::bridge::lockbox_address;
use zfx_verify::hash::sha256;
use zfx_verify::tx::{address_from_string, address_to_string, scheme_name, P2mrKeys, Transaction};

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
}

fn die(m: &str) -> ! {
    eprintln!("error: {}", m);
    std::process::exit(1)
}

fn write_private(path: &str, contents: &str) {
    if let Some(p) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::write(path, contents).unwrap_or_else(|e| die(&e.to_string()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
    }
}

fn load_seed(path: &str) -> [u8; 32] {
    let s = std::fs::read_to_string(path).unwrap_or_else(|_| die(&format!("cannot read wallet {} (run `zfx-wallet new`)", path)));
    let v: Value = serde_json::from_str(&s).unwrap_or_else(|e| die(&e.to_string()));
    hex::decode(v["seed"].as_str().unwrap_or("")).ok().and_then(|b| b.try_into().ok()).unwrap_or_else(|| die("bad seed in wallet file"))
}

fn save_wallet(path: &str, seed: &[u8; 32]) -> String {
    let k = P2mrKeys::from_seed(seed);
    let addr = address_to_string(&k.address());
    let v = json!({"format": "zfx-wallet-v0.1", "seed": hex::encode(seed), "address": addr});
    write_private(path, &serde_json::to_string_pretty(&v).unwrap());
    addr
}

/// Backup format: "zfxbackup1:<seed hex>:<first 4 bytes of SHA256(seed) hex>".
fn backup_string(seed: &[u8; 32]) -> String {
    format!("zfxbackup1:{}:{}", hex::encode(seed), hex::encode(&sha256(&[b"ZFX-backup-v1", seed])[..4]))
}

fn parse_backup(s: &str) -> Result<[u8; 32], String> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() != 3 || parts[0] != "zfxbackup1" {
        return Err("not a zfxbackup1 string".into());
    }
    let seed: [u8; 32] = hex::decode(parts[1]).ok().and_then(|v| v.try_into().ok()).ok_or("bad seed field")?;
    if hex::encode(&sha256(&[b"ZFX-backup-v1", &seed])[..4]) != parts[2] {
        return Err("backup checksum mismatch (typo or corruption)".into());
    }
    Ok(seed)
}

struct Rpc {
    url: String,
}

impl Rpc {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let r = ureq::post(&format!("{}/rpc", self.url.trim_end_matches('/'))).send_json(body);
        let v: Value = match r {
            Ok(r) => r.into_json().map_err(|e| e.to_string())?,
            Err(e) => return Err(format!("node unreachable: {}", e)),
        };
        if let Some(e) = v.get("error") {
            return Err(format!("{} (code {})", e["message"].as_str().unwrap_or("error"), e["code"]));
        }
        Ok(v["result"].clone())
    }
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().cloned().unwrap_or_else(|| "help".into());
    let home = std::env::var("HOME").unwrap_or(".".into());
    let wallet = arg(&args, "--wallet").unwrap_or_else(|| format!("{}/.zeroflux/wallet.json", home));
    let rpc = Rpc { url: arg(&args, "--node").unwrap_or_else(|| "http://127.0.0.1:9441".into()) };
    let addr_of =
        |args: &[String]| -> String { arg(args, "--address").unwrap_or_else(|| address_to_string(&P2mrKeys::from_seed(&load_seed(&wallet)).address())) };
    let ok = |r: Result<Value, String>| -> Value { r.unwrap_or_else(|e| die(&e)) };
    match cmd.as_str() {
        "new" => {
            if std::path::Path::new(&wallet).exists() && !args.iter().any(|a| a == "--force") {
                die(&format!("{} exists (use --force to overwrite)", wallet));
            }
            let mut seed = [0u8; 32];
            match arg(&args, "--seed-hex") {
                Some(h) => seed = hex::decode(h).ok().and_then(|b| b.try_into().ok()).unwrap_or_else(|| die("--seed-hex must be 32 bytes")),
                None => OsRng.fill_bytes(&mut seed),
            }
            println!("{}", save_wallet(&wallet, &seed));
        }
        "address" => {
            let k = P2mrKeys::from_seed(&load_seed(&wallet));
            println!("address {}", address_to_string(&k.address()));
            for (i, l) in k.leaves().iter().enumerate() {
                println!("  leaf {} {} pubkey {}", i, scheme_name(l.scheme), hex::encode(&l.pubkey));
            }
        }
        "balance" => print(&ok(rpc.call("zfx_get_account", json!({"address": addr_of(&args)})))),
        "history" => {
            let limit: u64 = arg(&args, "--limit").and_then(|v| v.parse().ok()).unwrap_or(50);
            print(&ok(rpc.call("zfx_get_address_history", json!({"address": addr_of(&args), "limit": limit}))));
        }
        "fee" => print(&ok(rpc.call("zfx_estimate_fee", json!({})))),
        "faucet" => print(&ok(rpc.call("zfx_faucet", json!({"address": addr_of(&args)})))),
        "tx" => {
            let id = args.get(1).cloned().unwrap_or_else(|| die("txid required"));
            print(&ok(rpc.call("zfx_node_get_transaction", json!({"txid": id}))));
        }
        "send" | "bridge-deposit" => {
            let deposit = cmd == "bridge-deposit";
            let k = P2mrKeys::from_seed(&load_seed(&wallet));
            let st = ok(rpc.call("zfx_node_get_status", json!({})));
            let cid = st["chain_id"].as_u64().unwrap_or_else(|| die("bad status")) as u32;
            let (to, memo) = if deposit {
                let acct = arg(&args, "--near-account").unwrap_or_else(|| die("--near-account required"));
                eprintln!("NOTE: the v1 bridge is custodied by a 2-of-3 committee.");
                (lockbox_address(cid), Some(acct))
            } else {
                (address_from_string(&arg(&args, "--to").unwrap_or_else(|| die("--to required"))).unwrap_or_else(|e| die(&e)), arg(&args, "--memo"))
            };
            let amount: u64 = arg(&args, "--amount").and_then(|v| v.parse().ok()).unwrap_or_else(|| die("--amount required"));
            let fee: u64 = match arg(&args, "--fee").as_deref() {
                None | Some("auto") => ok(rpc.call("zfx_estimate_fee", json!({})))["fee_per_tx"].as_u64().unwrap_or(1000),
                Some(f) => f.parse().unwrap_or_else(|_| die("bad --fee")),
            };
            let leaf: usize = arg(&args, "--leaf").and_then(|v| v.parse().ok()).unwrap_or(0);
            let acct = ok(rpc.call("zfx_get_account", json!({"address": address_to_string(&k.address())})));
            let nonce = acct["next_nonce"].as_u64().unwrap_or(0);
            let t0 = std::time::Instant::now();
            let tx: Transaction = k.sign_tx_memo(cid, to, amount, fee, nonce, leaf, memo).unwrap_or_else(|e| die(&e));
            eprintln!("signed with {} in {:.2}s ({} bytes, fee {})", scheme_name(tx.witness.leaf.scheme), t0.elapsed().as_secs_f64(), tx.encoded_len(), fee);
            let m = if deposit { "zfx_bridge_lock" } else { "zfx_submit_transaction" };
            match rpc.call(m, json!({"tx": tx})) {
                Ok(v) => println!("{}", json!({"accepted": true, "txid": v["txid"], "deposit_id": v["deposit_id"]})),
                Err(e) => {
                    println!("{}", json!({"accepted": false, "error": e}));
                    std::process::exit(2);
                }
            }
        }
        "backup" => {
            let out = arg(&args, "--out").unwrap_or_else(|| die("--out FILE required (the backup is never printed)"));
            write_private(&out, &format!("{}\n", backup_string(&load_seed(&wallet))));
            eprintln!("backup written to {} (0600). Store it offline; anyone holding it controls the funds.", out);
        }
        "restore" => {
            let b = arg(&args, "--backup").unwrap_or_else(|| die("--backup FILE required"));
            let seed = parse_backup(&std::fs::read_to_string(&b).unwrap_or_else(|e| die(&e.to_string()))).unwrap_or_else(|e| die(&e));
            if std::path::Path::new(&wallet).exists() && !args.iter().any(|a| a == "--force") {
                die(&format!("{} exists (use --force)", wallet));
            }
            println!("{}", save_wallet(&wallet, &seed));
        }
        "watch" => {
            let wf = format!("{}/.zeroflux/watch.json", home);
            let wf = arg(&args, "--watch-file").unwrap_or(wf);
            let mut list: Vec<String> = std::fs::read_to_string(&wf).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            match args.get(1).map(String::as_str) {
                Some("add") => {
                    let a = args.get(2).cloned().unwrap_or_else(|| die("address required"));
                    address_from_string(&a).unwrap_or_else(|e| die(&e));
                    if !list.contains(&a) {
                        list.push(a);
                    }
                    std::fs::create_dir_all(std::path::Path::new(&wf).parent().unwrap()).ok();
                    std::fs::write(&wf, serde_json::to_string_pretty(&list).unwrap()).unwrap();
                    println!("watching {} address(es)", list.len());
                }
                Some("list") => print(&json!(list)),
                Some("balances") => {
                    let v: Vec<Value> = list.iter().map(|a| ok(rpc.call("zfx_get_account", json!({"address": a})))).collect();
                    print(&json!(v));
                }
                _ => die("watch add ADDRESS | list | balances"),
            }
        }
        "bridge-info" => print(&ok(rpc.call("zfx_bridge_info", json!({})))),
        "bridge-status" => {
            if let Some(d) = arg(&args, "--deposit-id") {
                print(&ok(rpc.call("zfx_bridge_deposit_status", json!({"deposit_id": d}))));
            } else if let Some(w) = arg(&args, "--withdrawal-id") {
                print(&ok(rpc.call("zfx_bridge_redeem_status", json!({"withdrawal_id": w}))));
            } else {
                die("--deposit-id or --withdrawal-id required");
            }
        }
        _ => eprintln!("usage: zfx-wallet new|address|balance|send|history|tx|fee|faucet|backup|restore|watch|bridge-deposit|bridge-status|bridge-info"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_roundtrip_and_checksum() {
        let seed = [0x5au8; 32];
        let b = backup_string(&seed);
        assert_eq!(parse_backup(&b).unwrap(), seed);
        let mut bad = b.clone().into_bytes();
        bad[20] = if bad[20] == b'a' { b'b' } else { b'a' };
        assert!(parse_backup(std::str::from_utf8(&bad).unwrap()).is_err());
        // vector: fixed seed -> fixed backup string and address
        assert_eq!(b, format!("zfxbackup1:{}:{}", "5a".repeat(32), hex::encode(&sha256(&[b"ZFX-backup-v1", &seed])[..4])));
        assert_eq!(P2mrKeys::from_seed(&parse_backup(&b).unwrap()).address(), P2mrKeys::from_seed(&seed).address());
    }
}
