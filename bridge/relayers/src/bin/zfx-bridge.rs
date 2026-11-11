//! zfx-bridge: operator and user CLI for the ZeroFlux <-> NEAR bridge.
//!
//!   zfx-bridge near-sandbox --dir DIR --accounts a,b,c     local NEAR node; writes DIR/sandbox.json + key files
//!   zfx-bridge keygen-relayer --out SEEDFILE               prints {ed25519, slh} public keys
//!   zfx-bridge keygen-near --account ID --out FILE         NEAR key file (near-cli credential format)
//!   zfx-bridge deploy --rpc URL --signer ID --key-file F --contract ID --wasm W --init JSON [--deposit-near N]
//!   zfx-bridge call --rpc URL --signer ID --key-file F --contract ID --method M (--args JSON | --raw-args-file F) [--deposit YOCTO] [--gas TGAS]
//!   zfx-bridge admin-sign --seed F --member I --chain near|native --epoch E --nonce N --action pause|unpause|rotate
//!              [--contract ID] [--chain-id N] [--committee K1,K2,K3] [--activate-height H]
//!   zfx-bridge create-subaccount --rpc URL --signer ID --key-file F --account NEW --deposit-near N --out KEYFILE
//!   zfx-bridge near-mine-proxy --rpc URL --signer ID --key-file F --contract ID [--listen ADDR]   mine NEAR-native ZFX with zpu-cli
//!   zfx-bridge near-light-relay --rpc URL --signer ID --key-file F --contract ID --zfx URL [--once] [--max N]
//!   zfx-bridge pow-search --profile ID --seed HEX --challenge HEX --target HEX
//!   zfx-bridge near-mine-proxy ... --optimistic [--bond-near N]   full v0.1 PoW with optimistic checking (zfx-mine-opt)
//!   zfx-bridge near-opt-watch --rpc URL --signer ID --key-file F --contract ID [--once]   challenge invalid pending blocks
//!   zfx-bridge near-opt-cheat --rpc URL --signer ID --key-file F --contract ID   test adversary (no PoW work)
//!   zfx-bridge view --rpc URL --contract ID --method M --args JSON
//!   zfx-bridge burn --rpc URL --signer ID --key-file F --contract ID --amount N --native-recipient zfx1...
//!   zfx-bridge reconcile --env ENV --zfx URL
//!   zfx-bridge sign-nep413 --key-file F --message M --nonce-b64 B --recipient R
const USAGE: &str = r#"zfx-bridge: operator and user CLI for the ZeroFlux <-> NEAR bridge.

  zfx-bridge near-sandbox --dir DIR --accounts a,b,c     local NEAR node; writes DIR/sandbox.json + key files
  zfx-bridge keygen-relayer --out SEEDFILE               prints {ed25519, slh} public keys
  zfx-bridge keygen-near --account ID --out FILE         NEAR key file (near-cli credential format)
  zfx-bridge deploy --rpc URL --signer ID --key-file F --contract ID --wasm W --init JSON [--deposit-near N]
  zfx-bridge call --rpc URL --signer ID --key-file F --contract ID --method M (--args JSON | --raw-args-file F) [--deposit YOCTO] [--gas TGAS]
  zfx-bridge admin-sign --seed F --member I --chain near|native --epoch E --nonce N --action pause|unpause|rotate
             [--contract ID] [--chain-id N] [--committee K1,K2,K3] [--activate-height H]
  zfx-bridge create-subaccount --rpc URL --signer ID --key-file F --account NEW --deposit-near N --out KEYFILE
  zfx-bridge view --rpc URL --contract ID --method M --args JSON
  zfx-bridge burn --rpc URL --signer ID --key-file F --contract ID --amount N --native-recipient zfx1...
  zfx-bridge reconcile --env ENV --zfx URL
  zfx-bridge sign-nep413 --key-file F --message M --nonce-b64 B --recipient R
"#;

use base64::Engine;
use serde_json::{json, Value};
use zfx_bridge::near::{public_key_string, secret_key_string, Action, NearClient, NearSigner};

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
}
fn need(args: &[String], k: &str) -> String {
    arg(args, k).unwrap_or_else(|| {
        eprintln!("missing {}", k);
        std::process::exit(2)
    })
}
fn signer(args: &[String]) -> NearSigner {
    let kf: Value = serde_json::from_str(&std::fs::read_to_string(need(args, "--key-file")).unwrap()).unwrap();
    NearSigner::new(&need(args, "--signer"), kf["private_key"].as_str().unwrap()).unwrap()
}
fn new_key_file(account: &str, path: &str) -> ed25519_dalek::SigningKey {
    let k = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    zfx_bridge::keys::write_secret(
        path,
        &json!({"account_id": account, "public_key": public_key_string(&k), "private_key": secret_key_string(&k)}).to_string(),
    )
    .unwrap();
    k
}
/// Full trace a1|a2|a3|a4|out of one v0.1 attempt (weights cached per epoch).
fn opt_trace(cache: &mut Option<([u8; 32], zfx_verify::pow::Weights)>, seed: &[u8; 32], ch: &[u8; 32], nonce: u64) -> Vec<u8> {
    use zfx_verify::pow;
    if cache.as_ref().map(|c| &c.0 != seed).unwrap_or(true) {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        *cache = Some((*seed, pow::Weights::generate_par(&pow::FP8C_V01, seed, threads)));
    }
    let w = &cache.as_ref().unwrap().1;
    let x = pow::x0(&pow::attempt_id(ch, nonce), pow::FP8C_V01.d_in);
    let (out, acts) = pow::forward(w, &x, true);
    let mut t = Vec::with_capacity(4 * 4096 + 1024);
    for a in &acts {
        t.extend(a.iter().map(|v| *v as u8));
    }
    for v in &out {
        t.extend_from_slice(&v.to_le_bytes());
    }
    t
}

/// First (layer, row) where a posted trace differs from the honest one,
/// in computation order (the earliest wrong value is provably wrong).
fn first_mismatch(posted: &[u8], honest: &[u8]) -> Option<(u8, u32)> {
    if posted.len() != honest.len() {
        return Some((0, 0));
    }
    for l in 0..4usize {
        for r in 0..4096usize {
            if posted[l * 4096 + r] != honest[l * 4096 + r] {
                return Some((l as u8, r as u32));
            }
        }
    }
    for r in 0..256usize {
        let o = 4 * 4096 + 4 * r;
        if posted[o..o + 4] != honest[o..o + 4] {
            return Some((4, r as u32));
        }
    }
    None
}

/// Total gas burnt by a transaction and all its receipts, in Tgas.
fn gas_burnt_tgas(raw: &Value) -> f64 {
    let t = raw["transaction_outcome"]["outcome"]["gas_burnt"].as_u64().unwrap_or(0);
    let r: u64 = raw["receipts_outcome"].as_array().map(|a| a.iter().filter_map(|x| x["outcome"]["gas_burnt"].as_u64()).sum()).unwrap_or(0);
    (t + r) as f64 / 1e12
}

fn print_outcome(o: &zfx_bridge::near::Outcome) {
    println!(
        "{}",
        json!({"tx_hash": o.tx_hash, "success": o.success, "value": o.value, "failure": o.failure, "logs": o.logs, "gas_burnt_tgas": gas_burnt_tgas(&o.raw)})
    );
    if !o.success {
        std::process::exit(3);
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", USAGE);
        return;
    }
    match args.first().map(String::as_str) {
        Some("near-sandbox") => {
            let dir = need(&args, "--dir");
            std::fs::create_dir_all(&dir).unwrap();
            let sb = near_sandbox::Sandbox::start_sandbox().await.expect("start NEAR sandbox");
            let mut accounts = serde_json::Map::new();
            for a in arg(&args, "--accounts").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
                let path = format!("{}/{}.key.json", dir, a);
                let k = new_key_file(a, &path);
                sb.create_account(a.parse().unwrap())
                    .initial_balance(near_token::NearToken::from_near(1000))
                    .public_key(public_key_string(&k))
                    .send()
                    .await
                    .expect("create account");
                accounts.insert(a.into(), json!(path));
            }
            let info = json!({"rpc": sb.rpc_addr, "home": sb.home_dir.path(), "accounts": accounts, "pid": std::process::id()});
            std::fs::write(format!("{}/sandbox.json", dir), serde_json::to_string_pretty(&info).unwrap()).unwrap();
            println!("{}", info);
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let s2 = stop.clone();
            ctrlc::set_handler(move || s2.store(true, std::sync::atomic::Ordering::SeqCst)).unwrap();
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            drop(sb); // stops neard
        }
        Some("create-subaccount") => {
            // Create NEW (a subaccount of the signer) with a fresh full-access
            // key written to --out (0600) and --deposit-near NEAR from the signer.
            let near = NearClient::new(&need(&args, "--rpc"));
            let s = signer(&args);
            let acct = need(&args, "--account");
            let out = need(&args, "--out");
            if near.account_exists(&acct) {
                println!("{}", json!({"account": acct, "created": false, "reason": "exists"}));
                return;
            }
            let dep = near_to_yocto(&need(&args, "--deposit-near")).unwrap_or_else(|e| die(&format!("--deposit-near: {}", e)));
            let k = new_key_file(&acct, &out);
            let o = near
                .send(&s, &acct, vec![Action::CreateAccount, Action::Transfer(dep), Action::AddFullAccessKey(k.verifying_key().to_bytes())])
                .unwrap_or_else(|e| panic!("{}", e));
            println!("{}", json!({"account": acct, "created": o.success, "tx_hash": o.tx_hash, "failure": o.failure, "key_file": out}));
            if !o.success {
                std::process::exit(3);
            }
        }
        Some("near-opt-watch") => {
            // Recompute every pending block's trace and challenge the first
            // wrong value (layer, row). One honest watcher keeps the chain valid.
            let near = NearClient::new(&need(&args, "--rpc"));
            let s = signer(&args);
            let contract = need(&args, "--contract");
            let once = args.iter().any(|a| a == "--once");
            let mut wcache: Option<([u8; 32], zfx_verify::pow::Weights)> = None;
            let mut checked = std::collections::HashSet::new();
            // Heights already verified valid. A pending block can only change by being
            // reverted, which lowers the head, so after a revert the heights above the new
            // head are forgotten and re-checked when blocks reappear there.
            let mut verified: std::collections::BTreeSet<u64> = Default::default();
            let mut last_head: u64 = 0;
            let interval: u64 = arg(&args, "--interval-ms").and_then(|v| v.parse().ok()).unwrap_or(700);
            loop {
                let st = match near.view(&contract, "stats", json!({}), "optimistic") {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[near-opt-watch] stats: {} (retrying)", e);
                        std::thread::sleep(std::time::Duration::from_millis(interval.max(2000)));
                        continue;
                    }
                };
                let fin: u64 = st["final_height"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0);
                let head: u64 = st["head_height"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0);
                if head < last_head {
                    verified.retain(|&h| h <= head);
                }
                last_head = head;
                verified.retain(|&h| h > fin);
                for h in fin + 1..=head {
                    if verified.contains(&h) {
                        continue;
                    }
                    let Ok(b) = near.view(&contract, "block", json!({"height": h.to_string()}), "optimistic") else { continue };
                    if b.is_null() || !checked.insert((h, b["digest"].as_str().unwrap_or("").to_string())) {
                        continue;
                    }
                    let seed: [u8; 32] = hex::decode(b["epoch_seed"].as_str().unwrap()).unwrap().try_into().unwrap();
                    let ch: [u8; 32] = hex::decode(b["challenge"].as_str().unwrap()).unwrap().try_into().unwrap();
                    let nonce: u64 = b["nonce"].as_str().unwrap().parse().unwrap();
                    let posted = base64::engine::general_purpose::STANDARD.decode(b["trace"].as_str().unwrap()).unwrap();
                    let honest = opt_trace(&mut wcache, &seed, &ch, nonce);
                    let bad = first_mismatch(&posted, &honest);
                    match bad {
                        None => {
                            verified.insert(h);
                            println!("{}", json!({"event": "valid", "height": h, "miner": b["miner"]}))
                        }
                        Some((layer, row)) => {
                            // A one-row proof burns about 4.5 Tgas plus a little per reverted
                            // descendant; 100 Tgas keeps the prepaid gas (and the balance a
                            // watcher needs) at about 0.1 NEAR instead of 0.3.
                            let o = near.call(&s, &contract, "challenge", json!({"height": h.to_string(), "layer": layer, "row": row}), 100, 0);
                            println!(
                                "{}",
                                json!({"event": "challenge", "height": h, "layer": layer, "row": row, "miner": b["miner"],
                                "ok": o.as_ref().map(|o| o.success).unwrap_or(false), "result": o.as_ref().ok().and_then(|o| o.value.clone()),
                                "failure": o.as_ref().ok().and_then(|o| o.failure.clone()), "gas_burnt_tgas": o.as_ref().map(|o| gas_burnt_tgas(&o.raw)).unwrap_or(0.0)})
                            );
                            break; // the chain above it was reverted; rescan
                        }
                    }
                }
                if once {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(interval));
            }
        }
        Some("near-opt-cheat") => {
            // Test adversary: no matrix work at all. Grind fake outputs until
            // their digest meets the target and post garbage activations.
            use zfx_verify::pow;
            let near = NearClient::new(&need(&args, "--rpc"));
            let s = signer(&args);
            let contract = need(&args, "--contract");
            let w = near.view(&contract, "work", json!({"miner": s.account_id}), "optimistic").unwrap_or_else(|e| panic!("{}", e));
            let ch: [u8; 32] = hex::decode(w["challenge"].as_str().unwrap()).unwrap().try_into().unwrap();
            let target = zfx_verify::u256::U256::from_hex(w["target"].as_str().unwrap()).unwrap();
            let mut out = vec![7i32; 256];
            let mut i = 0i32;
            loop {
                out[0] = i;
                if pow::meets_target(&pow::result_digest(&ch, 0, &out), &target) {
                    break;
                }
                i += 1;
            }
            // --honest-layers N: compute activations a1..aN honestly (N <= 4)
            // and fake the rest, so the fraud sits deep in the trace.
            let honest_layers: usize = arg(&args, "--honest-layers").and_then(|v| v.parse().ok()).unwrap_or(0);
            let mut trace = vec![1u8; 4 * 4096];
            if honest_layers > 0 {
                let seed: [u8; 32] = hex::decode(w["epoch_seed"].as_str().unwrap()).unwrap().try_into().unwrap();
                let mut cache = None;
                let real = opt_trace(&mut cache, &seed, &ch, 0);
                trace[..honest_layers.min(4) * 4096].copy_from_slice(&real[..honest_layers.min(4) * 4096]);
            }
            for v in &out {
                trace.extend_from_slice(&v.to_le_bytes());
            }
            let o = near
                .call(
                    &s,
                    &contract,
                    "submit",
                    json!({"height": w["height"], "nonce": "0", "trace": base64::engine::general_purpose::STANDARD.encode(trace)}),
                    300,
                    1_000_000_000_000_000_000_000_000,
                )
                .unwrap_or_else(|e| panic!("{}", e));
            println!(
                "{}",
                json!({"event": "cheat", "height": w["height"], "grind_iterations": i + 1, "ok": o.success, "failure": o.failure, "tx": o.tx_hash,
                                  "gas_burnt_tgas": gas_burnt_tgas(&o.raw)})
            );
        }
        Some("pow-search") => {
            // Find a nonce meeting --target for --challenge with the given
            // profile and epoch seed (CPU, all cores). Prints {nonce, digest}.
            use zfx_verify::pow;
            let p = pow::profile_by_id(need(&args, "--profile").parse().unwrap()).expect("profile");
            let seed: [u8; 32] = hex::decode(need(&args, "--seed")).unwrap().try_into().unwrap();
            let ch: [u8; 32] = hex::decode(need(&args, "--challenge")).unwrap().try_into().unwrap();
            let target = zfx_verify::u256::U256::from_hex(&need(&args, "--target")).unwrap();
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            let w = std::sync::Arc::new(pow::Weights::generate_par(&p, &seed, threads));
            let found = std::sync::Arc::new(std::sync::Mutex::new(None));
            std::thread::scope(|sc| {
                for t in 0..threads as u64 {
                    let (w, found) = (w.clone(), found.clone());
                    sc.spawn(move || {
                        let mut n = t;
                        while found.lock().unwrap().is_none() {
                            let d = pow::attempt(&w, &ch, n);
                            if pow::meets_target(&d, &target) {
                                *found.lock().unwrap() = Some((n, d));
                            }
                            n += threads as u64;
                        }
                    });
                }
            });
            let (n, d) = found.lock().unwrap().unwrap();
            println!("{}", json!({"nonce": n.to_string(), "digest": hex::encode(d)}));
        }
        Some("near-mine-proxy") => {
            // Serve the node mining API (/v1/work, /v1/submit) backed by the
            // zfx-mine contract, so every existing miner (Rust, C++, Python,
            // GPU) mines NEAR-native ZFX unchanged through zpu-cli.
            let near = std::sync::Arc::new(NearClient::new(&need(&args, "--rpc")));
            let s = std::sync::Arc::new(signer(&args));
            let contract = need(&args, "--contract");
            let listen = arg(&args, "--listen").unwrap_or("127.0.0.1:9551".into());
            let optimistic = args.iter().any(|a| a == "--optimistic");
            // The bond attached to each submit: exactly --bond-near if given (parsed without
            // floats; 0.25 * 1e24 in f64 comes out just under 0.25 NEAR), otherwise the bond the
            // contract itself requires, read from `stats`.
            let bond: u128 = match arg(&args, "--bond-near") {
                Some(v) => near_to_yocto(&v).unwrap_or_else(|e| die(&format!("--bond-near: {}", e))),
                None if optimistic => near
                    .view(&contract, "stats", json!({}), "final")
                    .ok()
                    .and_then(|st| st["bond"].as_str().and_then(|b| b.parse::<u128>().ok()))
                    .unwrap_or_else(|| die("could not read the contract's bond from stats; pass --bond-near")),
                None => 0,
            };
            eprintln!("[near-mine-proxy] bond per submit: {} yoctoNEAR", bond);
            let mut wcache: Option<([u8; 32], zfx_verify::pow::Weights)> = None;
            let server = tiny_http::Server::http(&listen).unwrap_or_else(|e| panic!("{}", e));
            eprintln!("[near-mine-proxy] {} -> {} as {}", listen, contract, s.account_id);
            let respond = |req: tiny_http::Request, v: Value| {
                let _ = req.respond(tiny_http::Response::from_string(v.to_string()));
            };
            let mut work_cache: Option<(std::time::Instant, Value)> = None;
            let mut balance_ok: Option<(std::time::Instant, bool)> = None;
            // Keep a reserve so the same account can still pay for `finalize` (which returns
            // bonds) and for challenges. Spending it all on bonds deadlocks: no finalize, so no
            // bonds come back.
            const RESERVE: u128 = 300_000_000_000_000_000_000_000; // 0.3 NEAR
            const SUBMIT_GAS_COST: u128 = 50_000_000_000_000_000_000_000; // 50 Tgas prepaid, upper bound
            for mut req in server.incoming_requests() {
                let url = req.url().to_string();
                if url.starts_with("/v1/work") {
                    // Miners poll every job (often twice a second); reuse a view younger than
                    // WORK_CACHE so a public RPC isn't hit at that rate. A stale answer only costs
                    // a rejected stale-height submit, which the miner already handles.
                    const WORK_CACHE: std::time::Duration = std::time::Duration::from_millis(1500);
                    // Out of bond money: hand out no work until bonds come back, so the miner
                    // waits instead of finding blocks it can't submit. Balance checked at most
                    // every 10 s.
                    if optimistic {
                        let stale = balance_ok.as_ref().map(|(at, _)| at.elapsed() > std::time::Duration::from_secs(10)).unwrap_or(true);
                        if stale {
                            let have = near.view_account(&s.account_id).ok().and_then(|a| a["amount"].as_str().and_then(|v| v.parse::<u128>().ok()));
                            balance_ok = Some((std::time::Instant::now(), have.map(|h| h >= bond + SUBMIT_GAS_COST + RESERVE).unwrap_or(true)));
                        }
                        if let Some((_, false)) = balance_ok {
                            respond(
                                req,
                                json!({"error": "waiting for pending blocks to finalize and return their bonds (keeping 0.3 NEAR for finalize and challenges)"}),
                            );
                            continue;
                        }
                    }
                    if let Some((at, v)) = &work_cache {
                        if at.elapsed() < WORK_CACHE {
                            let v = v.clone();
                            respond(req, v);
                            continue;
                        }
                    }
                    let mut w = near.view(&contract, "work", json!({"miner": s.account_id}), "optimistic");
                    // Prepare missing epoch weight layers (one transaction each).
                    if let Ok(v) = &w {
                        let missing: Vec<u64> = v["missing_layers"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64()).collect()).unwrap_or_default();
                        for l in &missing {
                            let o = near.call(&s, &contract, "prepare_epoch", json!({"layer": l}), 300, 0);
                            eprintln!("[near-mine-proxy] prepare_epoch layer {}: {:?}", l, o.as_ref().map(|o| (o.success, gas_burnt_tgas(&o.raw))));
                        }
                        if !missing.is_empty() {
                            w = near.view(&contract, "work", json!({"miner": s.account_id}), "optimistic");
                        }
                    }
                    match w {
                        Ok(v) => {
                            let out = json!({"api_version": 2, "work_id": v["height"].as_str().unwrap_or("0").to_string(), "chain_id": 0, "version": v["profile"],
                                   "height": v["height"], "header_without_nonce": v["work_bytes"], "challenge": v["challenge"], "target": v["target"],
                                   "epoch_seed": v["epoch_seed"], "profile": "zfx-near-v1", "expiry": {"height": v["height"]}});
                            work_cache = Some((std::time::Instant::now(), out.clone()));
                            respond(req, out)
                        }
                        Err(e) => {
                            eprintln!("[near-mine-proxy] work view failed: {}", e);
                            respond(req, json!({"error": e}))
                        }
                    }
                } else if url.starts_with("/v1/submit") {
                    work_cache = None; // the head moves after a submit; fetch fresh work next time
                    let mut body = String::new();
                    let _ = req.as_reader().read_to_string(&mut body);
                    let b: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    let height = b["work_id"].as_str().unwrap_or("0").to_string();
                    let nonce = b["nonce"].as_str().map(String::from).unwrap_or_else(|| b["nonce"].to_string());
                    if optimistic {
                        let have = near.view_account(&s.account_id).ok().and_then(|a| a["amount"].as_str().and_then(|v| v.parse::<u128>().ok()));
                        if let Some(have) = have {
                            if have < bond + SUBMIT_GAS_COST + RESERVE {
                                let msg = "not enough NEAR for another bond while keeping 0.3 NEAR for finalize and challenges: mining resumes as pending blocks finalize and return their bonds";
                                eprintln!("[near-mine-proxy] {}", msg);
                                balance_ok = Some((std::time::Instant::now(), false));
                                respond(req, json!({"accepted": false, "error": msg}));
                                continue;
                            }
                        }
                    }
                    let res = if optimistic {
                        // Recompute the winning attempt's full trace and post it with the bond.
                        let w = near.view(&contract, "work", json!({"miner": s.account_id}), "optimistic").unwrap_or(Value::Null);
                        let seed: [u8; 32] = hex::decode(w["epoch_seed"].as_str().unwrap_or("")).ok().and_then(|v| v.try_into().ok()).unwrap_or([0; 32]);
                        let ch: [u8; 32] = hex::decode(w["challenge"].as_str().unwrap_or("")).ok().and_then(|v| v.try_into().ok()).unwrap_or([0; 32]);
                        let trace = opt_trace(&mut wcache, &seed, &ch, nonce.parse().unwrap_or(0));
                        near.call(
                            &s,
                            &contract,
                            "submit",
                            json!({"height": height, "nonce": nonce, "trace": base64::engine::general_purpose::STANDARD.encode(trace)}),
                            // A submit burns about 6 Tgas; 300 Tgas would reserve ~0.3 NEAR of
                            // prepaid gas per block on top of the bond.
                            50,
                            bond,
                        )
                    } else {
                        near.call(&s, &contract, "submit", json!({"height": height, "nonce": nonce}), 300, 0)
                    };
                    match res {
                        Ok(o) if o.success => {
                            eprintln!(
                                "{}",
                                json!({"event": "mined", "height": height, "nonce": nonce, "tx": o.tx_hash, "gas_burnt_tgas": gas_burnt_tgas(&o.raw), "result": o.value})
                            );
                            respond(req, json!({"accepted": true, "block_hash": o.tx_hash, "result": o.value, "gas_burnt_tgas": gas_burnt_tgas(&o.raw)}))
                        }
                        Ok(o) => respond(req, json!({"accepted": false, "error": o.failure, "code": 1002})),
                        Err(e) if e.contains("NotEnoughBalance") => {
                            // Every pending block holds a bond until its challenge window passes.
                            let msg = "not enough NEAR for another bond: mining resumes as pending blocks finalize and return their bonds";
                            eprintln!("[near-mine-proxy] {}", msg);
                            respond(req, json!({"accepted": false, "error": msg}))
                        }
                        Err(e) => respond(req, json!({"accepted": false, "error": e})),
                    }
                } else {
                    respond(req, json!({"error": "not found"}));
                }
            }
        }
        Some("near-light-relay") => {
            // Feed a ZeroFlux node's headers to the zfx-light contract, from
            // the fork point with the contract's canonical chain to the tip.
            let near = NearClient::new(&need(&args, "--rpc"));
            let s = signer(&args);
            let contract = need(&args, "--contract");
            let zfx = zfx_bridge::zfx::ZfxClient::new(&need(&args, "--zfx"));
            let once = args.iter().any(|a| a == "--once");
            let max_per_round: u64 = arg(&args, "--max").and_then(|v| v.parse().ok()).unwrap_or(u64::MAX);
            loop {
                let best = near.view(&contract, "best", json!({}), "optimistic").unwrap_or_else(|e| panic!("{}", e));
                let lc_height: u64 = best["height"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0);
                let tip: u64 = zfx.call("zfx_node_get_tip", json!({})).ok().and_then(|t| t["height"].as_u64()).unwrap_or(0);
                let node_hash = |h: u64| zfx.call("zfx_node_get_block", json!({"height": h})).ok().and_then(|b| b["hash"].as_str().map(String::from));
                let mut fork = lc_height.min(tip);
                while fork > 0 {
                    let lc = near.view(&contract, "canonical", json!({"height": fork.to_string()}), "optimistic").ok();
                    if lc.as_ref().and_then(|v| v.as_str().map(String::from)) == node_hash(fork) {
                        break;
                    }
                    fork -= 1;
                }
                let mut sent = 0u64;
                for h in (fork + 1)..=tip {
                    if sent >= max_per_round {
                        break;
                    }
                    let Ok(b) = zfx.call("zfx_node_get_block", json!({"height": h})) else { break };
                    let parent = b["header"]["parent"].as_str().unwrap_or("").to_string();
                    let m = near.view(&contract, "missing_layers", json!({"parent": parent, "height": h.to_string()}), "optimistic").unwrap_or(Value::Null);
                    for l in m["missing_layers"].as_array().cloned().unwrap_or_default() {
                        let o = near.call(&s, &contract, "prepare_weights", json!({"parent": parent, "height": h.to_string(), "layer": l}), 300, 0);
                        println!(
                            "{}",
                            json!({"event": "prepare_weights", "height": h, "layer": l, "ok": o.as_ref().map(|o| o.success).unwrap_or(false),
                                               "gas_burnt_tgas": o.as_ref().map(|o| gas_burnt_tgas(&o.raw)).unwrap_or(0.0)})
                        );
                    }
                    match near.call(&s, &contract, "submit_header", json!({"header": b["header_hex"]}), 300, 0) {
                        Ok(o) => println!(
                            "{}",
                            json!({"event": "header", "height": h, "ok": o.success, "tx": o.tx_hash, "result": o.value,
                                                     "failure": o.failure, "gas_burnt_tgas": gas_burnt_tgas(&o.raw)})
                        ),
                        Err(e) => println!("{}", json!({"event": "header", "height": h, "ok": false, "error": e})),
                    }
                    sent += 1;
                }
                if once && (sent == 0 || fork + sent >= tip) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        Some("admin-sign") => {
            // Offline operator approval for a governance action, signed with
            // one committee member's seed. Prints the approval JSON to paste
            // into the NEAR call or the native zfx_bridge_submit_op request.
            use ed25519_dalek::Signer;
            use zfx_bridge::digest::{near_admin_digest, NearAdmin};
            use zfx_verify::bridge as nb;
            let keys = zfx_bridge::keys::RelayerKeys::from_seed(&zfx_bridge::keys::read_seed(&need(&args, "--seed")).unwrap());
            let member: u8 = need(&args, "--member").parse().expect("--member 0..2");
            let epoch: u32 = need(&args, "--epoch").parse().expect("--epoch");
            let nonce: u64 = need(&args, "--nonce").parse().expect("--nonce");
            let action = need(&args, "--action");
            let committee: Vec<String> = arg(&args, "--committee").map(|c| c.split(',').map(String::from).collect()).unwrap_or_default();
            match need(&args, "--chain").as_str() {
                "near" => {
                    let a = match action.as_str() {
                        "pause" => NearAdmin::Pause,
                        "unpause" => NearAdmin::Unpause,
                        "rotate" => NearAdmin::Rotate(&committee),
                        x => panic!("unknown action {}", x),
                    };
                    let d = near_admin_digest(&need(&args, "--contract"), epoch, nonce, a);
                    println!("{}", json!({"member": member, "signature": hex::encode(keys.ed25519.sign(&d).to_bytes())}));
                }
                "native" => {
                    let chain_id: u32 = need(&args, "--chain-id").parse().expect("--chain-id");
                    let a = match action.as_str() {
                        "pause" => nb::AdminAction::Pause,
                        "unpause" => nb::AdminAction::Unpause,
                        "rotate" => nb::AdminAction::Rotate { members: committee, activate_height: need(&args, "--activate-height").parse().unwrap() },
                        x => panic!("unknown action {}", x),
                    };
                    let op = nb::AdminOp { epoch, op_nonce: nonce, action: a, approvals: vec![] };
                    let d = nb::admin_digest(chain_id, &op);
                    println!("{}", json!({"member": member, "sig": hex::encode(nb::sign_digest(&keys.slh, &d))}));
                }
                x => panic!("--chain near|native, got {}", x),
            }
        }
        Some("keygen-relayer") => {
            let out = need(&args, "--out");
            let mut seed = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
            zfx_bridge::keys::write_secret(&out, &hex::encode(seed)).unwrap();
            let k = zfx_bridge::keys::RelayerKeys::from_seed(&seed);
            println!("{}", json!({"seed_file": out, "ed25519": k.ed25519_hex(), "slh": k.slh_hex()}));
        }
        Some("keygen-near") => {
            let k = new_key_file(&need(&args, "--account"), &need(&args, "--out"));
            println!("{}", json!({"public_key": public_key_string(&k)}));
        }
        Some("deploy") => {
            let near = NearClient::new(&need(&args, "--rpc"));
            let s = signer(&args);
            let contract = need(&args, "--contract");
            let wasm = std::fs::read(need(&args, "--wasm")).unwrap();
            let deposit: u128 = arg(&args, "--deposit-near")
                .map(|v| near_to_yocto(&v).unwrap_or_else(|e| die(&format!("--deposit-near: {}", e))))
                .unwrap_or(10 * 1_000_000_000_000_000_000_000_000);
            let mut actions = Vec::new();
            if !near.account_exists(&contract) {
                actions.push(Action::CreateAccount);
                actions.push(Action::Transfer(deposit));
                actions.push(Action::AddFullAccessKey(s.key.verifying_key().to_bytes()));
            }
            actions.push(Action::DeployContract(wasm.clone()));
            if let Some(init) = arg(&args, "--init") {
                actions.push(Action::FunctionCall { method: "new".into(), args: init.into_bytes(), gas: 100 * zfx_bridge::near::TGAS, deposit: 0 });
            }
            let o = near.send(&s, &contract, actions).unwrap_or_else(|e| panic!("{}", e));
            let hash = sha2::Digest::finalize(<sha2::Sha256 as sha2::Digest>::new_with_prefix(&wasm));
            println!(
                "{}",
                json!({"contract": contract, "tx_hash": o.tx_hash, "success": o.success, "failure": o.failure,
                "wasm_sha256": hex::encode(hash), "code_hash_b58": bs58::encode(hash).into_string()})
            );
            if !o.success {
                std::process::exit(3);
            }
        }
        Some("call") => {
            let near = NearClient::new(&need(&args, "--rpc"));
            let a: Value = serde_json::from_str(&arg(&args, "--args").unwrap_or("{}".into())).unwrap();
            let dep: u128 = arg(&args, "--deposit").and_then(|v| v.parse().ok()).unwrap_or(0);
            let gas: u64 = arg(&args, "--gas").and_then(|v| v.parse().ok()).unwrap_or(100);
            let o = match arg(&args, "--raw-args-file") {
                // Raw (non-JSON) input, e.g. a WASM for `deploy_upgrade`.
                Some(f) => near.call_raw(&signer(&args), &need(&args, "--contract"), &need(&args, "--method"), std::fs::read(f).unwrap(), gas, dep),
                None => near.call(&signer(&args), &need(&args, "--contract"), &need(&args, "--method"), a, gas, dep),
            }
            .unwrap_or_else(|e| panic!("{}", e));
            print_outcome(&o);
        }
        Some("view") => {
            let near = NearClient::new(&need(&args, "--rpc"));
            let a: Value = serde_json::from_str(&arg(&args, "--args").unwrap_or("{}".into())).unwrap();
            println!(
                "{}",
                near.view(&need(&args, "--contract"), &need(&args, "--method"), a, &arg(&args, "--finality").unwrap_or("final".into()))
                    .unwrap_or_else(|e| panic!("{}", e))
            );
        }
        Some("burn") => {
            let near = NearClient::new(&need(&args, "--rpc"));
            let o = near
                .call(
                    &signer(&args),
                    &need(&args, "--contract"),
                    "bridge_burn",
                    json!({"amount": need(&args, "--amount"), "native_recipient": need(&args, "--native-recipient")}),
                    30,
                    1,
                )
                .unwrap_or_else(|e| panic!("{}", e));
            print_outcome(&o);
        }
        Some("reconcile") => {
            let env: zfx_bridge::config::BridgeEnv = zfx_bridge::config::load(&need(&args, "--env")).unwrap();
            let r = zfx_bridge::reconcile::reconcile(
                &zfx_bridge::zfx::ZfxClient::new(&need(&args, "--zfx")),
                &NearClient::new(&env.near_rpc),
                &env.wzfx_contract,
                env.native_chain_id,
            )
            .unwrap();
            println!("{}", serde_json::to_string_pretty(&r).unwrap());
            if r["ok"] != true {
                std::process::exit(4);
            }
        }
        Some("sign-nep413") => {
            let kf: Value = serde_json::from_str(&std::fs::read_to_string(need(&args, "--key-file")).unwrap()).unwrap();
            let k = zfx_bridge::near::parse_secret_key(kf["private_key"].as_str().unwrap()).unwrap();
            let nonce: [u8; 32] = base64::engine::general_purpose::STANDARD.decode(need(&args, "--nonce-b64")).unwrap().try_into().unwrap();
            let sig = zfx_bridge::near::nep413_sign(&k, &need(&args, "--message"), &nonce, &need(&args, "--recipient"));
            println!("{}", json!({"public_key": public_key_string(&k), "signature": sig}));
        }
        _ => eprintln!("see source header for usage"),
    }
}

/// Parse a NEAR amount like "2", "1.5" or "0.125" into yoctoNEAR exactly (no floats).
fn near_to_yocto(v: &str) -> Result<u128, String> {
    let v = v.trim();
    let (int, frac) = v.split_once('.').unwrap_or((v, ""));
    if int.is_empty() && frac.is_empty() || frac.len() > 24 || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(format!("not a NEAR amount: {:?}", v));
    }
    let whole: u128 = if int.is_empty() { 0 } else { int.parse().map_err(|_| format!("not a NEAR amount: {:?}", v))? };
    let frac_y: u128 = if frac.is_empty() { 0 } else { format!("{:0<24}", frac).parse().map_err(|_| format!("not a NEAR amount: {:?}", v))? };
    whole.checked_mul(1_000_000_000_000_000_000_000_000).and_then(|w| w.checked_add(frac_y)).ok_or_else(|| format!("amount too large: {:?}", v))
}

fn die(msg: &str) -> ! {
    eprintln!("zfx-bridge: {}", msg);
    std::process::exit(2)
}

#[cfg(test)]
mod near_amount_tests {
    use super::near_to_yocto;
    #[test]
    fn parses_exact_decimals() {
        assert_eq!(near_to_yocto("2").unwrap(), 2 * 10u128.pow(24));
        assert_eq!(near_to_yocto("1.5").unwrap(), 15 * 10u128.pow(23));
        assert_eq!(near_to_yocto("0.000000000000000000000001").unwrap(), 1);
        assert!(near_to_yocto("1.5.0").is_err() && near_to_yocto("abc").is_err() && near_to_yocto("").is_err() && near_to_yocto("-1").is_err());
    }
}
