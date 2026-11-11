use std::path::PathBuf;
use zfx_index::engine::{self, Params};
use zfx_index::fetch::RateBudget;
use zfx_index::ledger::Ledger;
use zfx_index::time::now_ms;
use zfx_index::{keys, poll, publish, server, vectors};

const USAGE: &str = "zfx-index <command> [options]

commands:
  keygen                 create the reporter key (refuses to overwrite) and print the public key
  backfill               fetch catalog + rankings, list top-N markets, fetch history for each
  engine                 compute revisions from the ledger and sign prints
  publish                write the static JSON tree
  poll [--once]          run the bounded poller (fetch, engine, publish) forever
  serve                  read-only REST + WS server over the static tree
  verify                 check ledger hash chains, blob hashes and every signed print
  vectors                write OraclePrint v1 golden vectors
  health                 print source health JSON

options:
  --ledger DIR   (default ./ledger)        --out DIR   (default ./public)
  --key PATH     (default ~/.config/zeroflux/oracle-reporter.key)
  --addr HOST:PORT (default 127.0.0.1:0, an ephemeral port)
  --top N        (default 50)              --vectors-out FILE (default ./vectors/oracle_print_v1.json)";

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let ledger_dir = PathBuf::from(arg(&args, "--ledger").unwrap_or_else(|| "ledger".into()));
    let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "public".into()));
    let key = arg(&args, "--key").map(PathBuf::from).unwrap_or_else(keys::default_key_path);
    let mut params = Params::default();
    if let Some(n) = arg(&args, "--top").and_then(|x| x.parse().ok()) {
        params.top_n = n;
    }
    let budget = RateBudget::default();
    let r: Result<(), String> = (|| match cmd.as_str() {
        "keygen" => {
            let pk = keys::generate(&key)?;
            println!("{}", serde_json::json!({"key_path": key, "pubkey_hex": hex::encode(pk)}));
            Ok(())
        }
        "backfill" => {
            let l = Ledger::open_write(&ledger_dir).map_err(|e| e.to_string())?;
            poll::backfill(&l, budget.clone(), params.top_n).map_err(|e| e.to_string())
        }
        "engine" => {
            let signer = keys::load(&key)?;
            let l = Ledger::open_write(&ledger_dir).map_err(|e| e.to_string())?;
            let rep = engine::run(&l, &signer, &params, now_ms()).map_err(|e| e.to_string())?;
            let rep7 = zfx_index::weekly::run(&l, &signer, &Default::default(), now_ms()).map_err(|e| e.to_string())?;
            println!("{}", serde_json::json!({"daily": rep, "7d": rep7}));
            Ok(())
        }
        "publish" => {
            let signer = keys::load(&key)?;
            let l = Ledger::open_read(&ledger_dir);
            let n = publish::publish(&l, &out, &signer.pubkey(), &params, &budget, now_ms()).map_err(|e| e.to_string())?;
            println!("published {n} files to {}", out.display());
            Ok(())
        }
        "poll" => {
            let signer = keys::load(&key)?;
            poll::run_loop(&ledger_dir, &out, &signer, &params, budget.clone(), args.iter().any(|a| a == "--once")).map_err(|e| e.to_string())
        }
        "serve" => {
            let addr = arg(&args, "--addr").unwrap_or_else(|| "127.0.0.1:0".into());
            server::serve(&addr, out.clone(), ledger_dir.clone()).map_err(|e| e.to_string())
        }
        "verify" => {
            let l = Ledger::open_read(&ledger_dir);
            let rep = l.verify().map_err(|e| e.to_string())?;
            let mut ok = 0;
            let mut bad = 0;
            let mut all = l.read_json("revisions.jsonl").map_err(|e| e.to_string())?;
            all.extend(l.read_json(zfx_index::weekly::REVISIONS_7D).map_err(|e| e.to_string())?);
            for r in all {
                let p = &r["print"];
                if p.is_null() {
                    continue;
                }
                let bytes = hex::decode(p["canonicalHex"].as_str().unwrap_or("")).unwrap_or_default();
                let decoded = zfx_oracle_print::OraclePrint::decode(&bytes);
                let mut good = decoded.is_ok() && hex::encode(zfx_oracle_print::sha256(&bytes)) == p["printHash"].as_str().unwrap_or("");
                for s in p["reporterSignatures"].as_array().cloned().unwrap_or_default() {
                    let parts: Vec<&str> = s.as_str().unwrap_or("").split(':').collect();
                    let pk: Option<[u8; 32]> = parts.get(1).and_then(|x| hex::decode(x).ok()).and_then(|b| b.try_into().ok());
                    let sg: Option<[u8; 64]> = parts.get(2).and_then(|x| hex::decode(x).ok()).and_then(|b| b.try_into().ok());
                    good &= matches!((pk, sg), (Some(pk), Some(sg)) if zfx_oracle_print::verify(&bytes, &pk, &sg));
                }
                if good { ok += 1 } else { bad += 1 }
            }
            println!("{}", serde_json::json!({"ledger": rep, "prints_valid": ok, "prints_invalid": bad}));
            if bad > 0 { Err("invalid prints".into()) } else { Ok(()) }
        }
        "vectors" => {
            let p = PathBuf::from(arg(&args, "--vectors-out").unwrap_or_else(|| "vectors/oracle_print_v1.json".into()));
            std::fs::write(&p, serde_json::to_vec_pretty(&vectors::generate()).unwrap()).map_err(|e| e.to_string())?;
            println!("wrote {}", p.display());
            Ok(())
        }
        "health" => {
            let l = Ledger::open_read(&ledger_dir);
            let s = engine::load(&l).map_err(|e| e.to_string())?;
            let h = publish::source_health(&l, &s, &params, &budget, now_ms()).map_err(|e| e.to_string())?;
            println!("{}", serde_json::to_string_pretty(&h).unwrap());
            Ok(())
        }
        _ => Err(format!("unknown command {cmd}\n\n{USAGE}")),
    })();
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
