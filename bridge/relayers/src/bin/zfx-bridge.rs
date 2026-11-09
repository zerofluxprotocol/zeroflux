//! zfx-bridge: operator and user CLI for the ZeroFlux <-> NEAR bridge.
//!
//!   zfx-bridge near-sandbox --dir DIR --accounts a,b,c     local NEAR node; writes DIR/sandbox.json + key files
//!   zfx-bridge keygen-relayer --out SEEDFILE               prints {ed25519, slh} public keys
//!   zfx-bridge keygen-near --account ID --out FILE         NEAR key file (near-cli credential format)
//!   zfx-bridge deploy --rpc URL --signer ID --key-file F --contract ID --wasm W --init JSON [--deposit-near N]
//!   zfx-bridge call --rpc URL --signer ID --key-file F --contract ID --method M (--args JSON | --raw-args-file F) [--deposit YOCTO] [--gas TGAS]
//!   zfx-bridge admin-sign --seed F --member I --chain near|native --epoch E --nonce N --action pause|unpause|rotate
//!              [--contract ID] [--chain-id N] [--committee K1,K2,K3] [--activate-height H]
//!   zfx-bridge view --rpc URL --contract ID --method M --args JSON
//!   zfx-bridge burn --rpc URL --signer ID --key-file F --contract ID --amount N --native-recipient zfx1...
//!   zfx-bridge reconcile --env ENV --zfx URL
//!   zfx-bridge sign-nep413 --key-file F --message M --nonce-b64 B --recipient R
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
    zfx_bridge::keys::write_secret(path, &json!({"account_id": account, "public_key": public_key_string(&k), "private_key": secret_key_string(&k)}).to_string()).unwrap();
    k
}
fn print_outcome(o: &zfx_bridge::near::Outcome) {
    println!("{}", json!({"tx_hash": o.tx_hash, "success": o.success, "value": o.value, "failure": o.failure, "logs": o.logs}));
    if !o.success {
        std::process::exit(3);
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
            let deposit: u128 = arg(&args, "--deposit-near").and_then(|v| v.parse::<u128>().ok()).unwrap_or(10) * 1_000_000_000_000_000_000_000_000;
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
            println!("{}", json!({"contract": contract, "tx_hash": o.tx_hash, "success": o.success, "failure": o.failure,
                "wasm_sha256": hex::encode(hash), "code_hash_b58": bs58::encode(hash).into_string()}));
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
            println!("{}", near.view(&need(&args, "--contract"), &need(&args, "--method"), a, &arg(&args, "--finality").unwrap_or("final".into())).unwrap_or_else(|e| panic!("{}", e)));
        }
        Some("burn") => {
            let near = NearClient::new(&need(&args, "--rpc"));
            let o = near
                .call(&signer(&args), &need(&args, "--contract"), "bridge_burn", json!({"amount": need(&args, "--amount"), "native_recipient": need(&args, "--native-recipient")}), 30, 1)
                .unwrap_or_else(|e| panic!("{}", e));
            print_outcome(&o);
        }
        Some("reconcile") => {
            let env: zfx_bridge::config::BridgeEnv = zfx_bridge::config::load(&need(&args, "--env")).unwrap();
            let r = zfx_bridge::reconcile::reconcile(&zfx_bridge::zfx::ZfxClient::new(&need(&args, "--zfx")), &NearClient::new(&env.near_rpc), &env.wzfx_contract, env.native_chain_id).unwrap();
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
