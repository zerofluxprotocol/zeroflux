//! zfx-bridge-coordinator --env env.json --zfx URL --near-account ID --near-key-file FILE
//!     --store jobs.json --incidents DIR [--auto-register] [--fault-rate F]
//!     [--status-port P] [--interval-ms N] [--once]
use serde_json::json;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let v = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let need = |k: &str| v(k).unwrap_or_else(|| {
        eprintln!("missing {}", k);
        std::process::exit(2)
    });
    let env: zfx_bridge::config::BridgeEnv = zfx_bridge::config::load(&need("--env")).unwrap_or_else(|e| panic!("{}", e));
    let kf: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(need("--near-key-file")).unwrap()).unwrap();
    let signer = zfx_bridge::near::NearSigner::new(&need("--near-account"), kf["private_key"].as_str().unwrap()).unwrap();
    let c = Arc::new(
        zfx_bridge::coordinator::Coordinator::new(
            env,
            &need("--zfx"),
            signer,
            &need("--store"),
            &v("--incidents").unwrap_or_else(|| "incidents".into()),
            args.iter().any(|a| a == "--auto-register"),
            v("--fault-rate").and_then(|x| x.parse().ok()).unwrap_or(0.0),
        )
        .unwrap_or_else(|e| panic!("{}", e)),
    );
    if let Some(port) = v("--status-port") {
        let c2 = c.clone();
        std::thread::spawn(move || {
            let s = tiny_http::Server::http(format!("127.0.0.1:{}", port)).unwrap();
            for req in s.incoming_requests() {
                let url = req.url().to_string();
                let body = if let Some(id) = url.strip_prefix("/jobs/") {
                    json!(c2.store.lock().unwrap().jobs.get(id))
                } else if url == "/jobs" {
                    json!(c2.store.lock().unwrap().jobs)
                } else {
                    c2.status()
                };
                let _ = req.respond(tiny_http::Response::from_string(body.to_string()));
            }
        });
    }
    let interval = v("--interval-ms").and_then(|x| x.parse().ok()).unwrap_or(1000u64);
    let mut n = 0u64;
    loop {
        match c.tick() {
            Ok(k) if k > 0 => eprintln!("[coordinator] advanced {} job(s): {}", k, json!(c.store.lock().unwrap().counts())),
            Ok(_) => {}
            Err(e) => eprintln!("[coordinator] tick: {}", e),
        }
        n += 1;
        if n % 5 == 0 {
            if let Err(e) = c.reconcile_and_guard() {
                eprintln!("[coordinator] reconcile: {}", e);
            }
        }
        if args.iter().any(|a| a == "--once") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(interval));
    }
}
