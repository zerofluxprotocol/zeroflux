//! Resource measurement for hosting estimates: model load cost, resident
//! memory, per-request CPU time and latency for detection with GLiNER, both
//! detector-only and end to end through the hosted gateway (with a local mock
//! upstream, so model inference time is excluded).
//!
//!   cargo run --release -p zfx-vpn-hosted --example measure -- <models dir> <model> [requests]
//!
//! <models dir> holds `<model>/{model.onnx,tokenizer.json,gliner_config.json}`.
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

fn rss_mb() -> f64 {
    let o = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().unwrap_or(0.0) / 1024.0
}

fn cpu_secs() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |tv: libc::timeval| tv.tv_sec as f64 + tv.tv_usec as f64 / 1e6;
    t(u.ru_utime) + t(u.ru_stime)
}

fn max_rss_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    // macOS reports bytes, Linux kilobytes.
    if cfg!(target_os = "macos") { u.ru_maxrss as f64 / 1048576.0 } else { u.ru_maxrss as f64 / 1024.0 }
}

fn prompt(words: usize) -> String {
    let base = "Hi, I am Maria Gonzalez, my email is maria.gonzalez@example.com and my phone is 415-555-0134. I live at 1200 Market Street, San Francisco. I was diagnosed with type 2 diabetes last year and take metformin. Please help me draft a short note to my insurer about policy renewal and my upcoming appointment with Dr. Alan Smith. ";
    let mut s = String::new();
    while s.split_whitespace().count() < words {
        s.push_str(base);
    }
    s.split_whitespace().take(words).collect::<Vec<_>>().join(" ")
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("models dir"));
    let model = a.next().unwrap_or_else(|| "gliner-pii-base".into());
    let n: usize = a.next().and_then(|x| x.parse().ok()).unwrap_or(20);
    let mut out = serde_json::Map::new();
    out.insert("model".into(), json!(model));
    out.insert("cpus".into(), json!(std::thread::available_parallelism().map(|x| x.get()).unwrap_or(0)));
    out.insert("rss_mb_start".into(), json!(rss_mb()));

    let det = Arc::new(pv_detectors::Detector::default());
    let parent = dir.parent().unwrap().to_path_buf();
    let mm = pv_daemon::models::ModelManager::new(pv_daemon::state::Paths::at(parent), det.clone());
    let (c0, t0) = (cpu_secs(), Instant::now());
    assert!(mm.load(&model), "model failed to load: {}", mm.status());
    let mut s = det.settings();
    s.span_required = true;
    s.span_timeout_ms = 120_000;
    det.set_settings(s);
    out.insert("load_ms".into(), json!(t0.elapsed().as_millis()));
    out.insert("load_cpu_s".into(), json!(cpu_secs() - c0));
    out.insert("rss_mb_after_load".into(), json!(rss_mb()));

    // Detector only (what the gateway adds per request, before any network).
    let mut det_rows = vec![];
    for words in [40usize, 400, 3000] {
        let p = prompt(words);
        let _ = det.detect(&[p.clone()], true).await; // warm
        let mut lat = vec![];
        let c = cpu_secs();
        for _ in 0..n {
            let t = Instant::now();
            let r = det.detect(&[p.clone()], true).await;
            assert!(r.failures.is_empty(), "{:?}", r.failures);
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let cpu_ms = (cpu_secs() - c) * 1000.0 / n as f64;
        det_rows.push(json!({"words": words, "chars": p.len(), "p50_ms": pct(&mut lat.clone(), 0.5), "p95_ms": pct(&mut lat, 0.95), "cpu_ms_per_request": cpu_ms}));
    }
    out.insert("detector_only".into(), Value::Array(det_rows));
    out.insert("rss_mb_after_detector_runs".into(), json!(rss_mb()));

    // End to end through the hosted gateway with a mock upstream.
    let rec = Arc::new(pv_daemon::mock::Recorded::default());
    let (mbase, _) = pv_daemon::mock::serve(0, rec.clone()).await.unwrap();
    let h = zfx_vpn_hosted::Hosted::new(zfx_vpn_hosted::Config { admin_token: zeroize::Zeroizing::new("x".repeat(40)), ..Default::default() }, Some(det.clone())).unwrap();
    let cfg: zfx_vpn_hosted::store::TenantConfig = serde_json::from_value(json!({"name": "bench", "upstreams": [{"schema_version": 1, "id": "frontier", "kind": "OPENAI_COMPATIBLE", "display_name": "f", "base_url": mbase, "default_model": "mock-echo", "declared_tier": "EXTERNAL_UNVERIFIED", "enabled": true, "allow_failover": false, "supports_responses": true, "timeout_secs": 60}], "default_destination": "frontier", "quota": {"requests_per_minute": 0}})).unwrap();
    let t = h.store.create_tenant(&cfg).unwrap();
    let (_, key) = h.store.create_key(&t.tenant_id, &["inference".into()]).unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let r = zfx_vpn_hosted::router(h.clone());
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut e2e = vec![];
    for (words, conc) in [(40usize, 1usize), (400, 1), (400, 4), (400, 8), (3000, 1)] {
        let p = prompt(words);
        let body = json!({"model": "m", "messages": [{"role": "user", "content": p}]});
        let _ = http.post(format!("{base}/v1/chat/completions")).bearer_auth(&key).json(&body).send().await.unwrap().text().await;
        let c = cpu_secs();
        let t = Instant::now();
        let mut tasks = vec![];
        for _ in 0..conc {
            let (http, base, key, body) = (http.clone(), base.clone(), key.clone(), body.clone());
            tasks.push(tokio::spawn(async move {
                let mut lat = vec![];
                for _ in 0..n {
                    let t = Instant::now();
                    let r = http.post(format!("{base}/v1/chat/completions")).bearer_auth(&key).json(&body).send().await.unwrap();
                    assert_eq!(r.status(), 200);
                    let _ = r.text().await;
                    lat.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                lat
            }));
        }
        let mut lat = vec![];
        for t in tasks {
            lat.extend(t.await.unwrap());
        }
        let wall = t.elapsed().as_secs_f64();
        let total = (n * conc) as f64;
        e2e.push(json!({"words": words, "concurrency": conc, "requests": total, "throughput_rps": total / wall, "p50_ms": pct(&mut lat.clone(), 0.5), "p95_ms": pct(&mut lat, 0.95), "cpu_ms_per_request": (cpu_secs() - c) * 1000.0 / total, "rss_mb": rss_mb()}));
    }
    out.insert("hosted_end_to_end_mock_upstream".into(), Value::Array(e2e));
    out.insert("peak_rss_mb".into(), json!(max_rss_mb()));
    println!("{}", serde_json::to_string_pretty(&Value::Object(out)).unwrap());
}
