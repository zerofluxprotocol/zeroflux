//! zpu-cli: the ZPU mining client.
//!
//!   zpu-cli mine     --node URL --address zfx1... [--backend cpu|exec] [--exec "CMD ARGS"]
//!                    [--threads N] [--batch N | --job-secs S (default 0.5)] [--max-blocks N] [--spot-check N]
//!   zpu-cli bench    [--profile v01|dev] [--backend ...] [--seconds S]
//!   zpu-cli selftest [--vectors FILE] [--backend ...]
//!
//! Backends:
//!   cpu   built-in Rust reference (zfx-verify), multi-threaded.
//!   exec  any external miner speaking the ZPU line protocol on stdin/stdout:
//!         miner/cuda/zpu-miner (CUDA / HIP / CPU, C++) or
//!         miner/python/zpu_miner.py (PyTorch CUDA/ROCm/MPS, MLX/Metal, NumPy).
//!         See miner/PROTOCOL.md.
//!
//! Every winning nonce from any backend is re-verified on the CPU before
//! submission, and `--spot-check N` re-verifies N random non-winning
//! digests per job, so a miscomputing GPU is detected instead of silently
//! wasting work.

const USAGE: &str = r#"zpu-cli: the ZPU mining client.

  zpu-cli mine     --node URL --address zfx1... [--backend cpu|exec] [--exec "CMD ARGS"]
                   [--threads N] [--batch N | --job-secs S (default 0.5)] [--max-blocks N] [--spot-check N]
  zpu-cli bench    [--profile v01|dev] [--backend ...] [--seconds S]
  zpu-cli selftest [--vectors FILE] [--backend ...]

Backends:
  cpu   built-in Rust reference (zfx-verify), multi-threaded.
  exec  any external miner speaking the ZPU line protocol on stdin/stdout:
        miner/cuda/zpu-miner (CUDA / HIP / CPU, C++) or
        miner/python/zpu_miner.py (PyTorch CUDA/ROCm/MPS, MLX/Metal, NumPy).
        See miner/PROTOCOL.md.

Every winning nonce from any backend is re-verified on the CPU before
submission, and `--spot-check N` re-verifies N random non-winning
digests per job, so a miscomputing GPU is detected instead of silently
wasting work.
"#;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zfx_verify::hash::{from_hex32, to_hex, Hash32};
use zfx_verify::pow::{self, profile_by_id, Profile, Weights, FP8C_V01, FP8C_V01_DEV};
use zfx_verify::U256;

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
}

fn die(m: &str) -> ! {
    eprintln!("error: {}", m);
    std::process::exit(1)
}

pub struct Job {
    pub profile: Profile,
    pub seed: Hash32,
    pub challenge: Hash32,
    pub target: U256,
    pub start: u64,
    pub count: u64,
    /// Return as soon as a winner is found (mining); false for benchmarks.
    pub stop_on_found: bool,
}

pub struct JobResult {
    pub found: Vec<(u64, Hash32)>,
    pub attempts: u64,
    pub elapsed: f64,
}

trait Backend {
    fn name(&self) -> String;
    fn run(&mut self, job: &Job, stop: &AtomicBool) -> JobResult;
    fn digests(&mut self, job: &Job, nonces: &[u64]) -> Vec<Hash32>;
}

struct CpuBackend {
    threads: usize,
    weights: Option<Arc<Weights>>,
}

impl CpuBackend {
    fn weights(&mut self, job: &Job) -> Arc<Weights> {
        if self.weights.as_ref().map(|w| w.seed != job.seed || w.profile != job.profile).unwrap_or(true) {
            self.weights = Some(Arc::new(Weights::generate_par(&job.profile, &job.seed, self.threads)));
        }
        self.weights.clone().unwrap()
    }
}

impl Backend for CpuBackend {
    fn name(&self) -> String {
        format!("cpu-rust x{}", self.threads)
    }
    fn run(&mut self, job: &Job, stop: &AtomicBool) -> JobResult {
        let w = self.weights(job);
        let t = Instant::now();
        let done = AtomicU64::new(0);
        let found = std::sync::Mutex::new(Vec::new());
        let nt = self.threads as u64;
        std::thread::scope(|s| {
            for th in 0..nt {
                let (w, done, found) = (&w, &done, &found);
                s.spawn(move || {
                    let mut n = job.start + th;
                    while n < job.start + job.count && !stop.load(Ordering::Relaxed) {
                        let d = pow::attempt(w, &job.challenge, n);
                        if pow::meets_target(&d, &job.target) {
                            found.lock().unwrap().push((n, d));
                            if job.stop_on_found {
                                stop.store(true, Ordering::Relaxed);
                            }
                        }
                        done.fetch_add(1, Ordering::Relaxed);
                        n += nt;
                    }
                });
            }
        });
        JobResult { found: found.into_inner().unwrap(), attempts: done.load(Ordering::Relaxed), elapsed: t.elapsed().as_secs_f64() }
    }
    fn digests(&mut self, job: &Job, nonces: &[u64]) -> Vec<Hash32> {
        let w = self.weights(job);
        nonces.iter().map(|n| pow::attempt(&w, &job.challenge, *n)).collect()
    }
}

/// External miner process speaking the ZPU line protocol.
struct ExecBackend {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    info: Value,
}

impl ExecBackend {
    fn spawn(cmdline: &str) -> ExecBackend {
        let parts: Vec<&str> = cmdline.split_whitespace().collect();
        let mut child = Command::new(parts[0])
            .args(&parts[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| die(&format!("cannot start miner `{}`: {}", cmdline, e)));
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut b = ExecBackend { _child: child, stdin, stdout, info: Value::Null };
        b.send(&json!({"cmd": "info"}));
        b.info = b.recv_event("info");
        b
    }
    fn send(&mut self, v: &Value) {
        writeln!(self.stdin, "{}", v).and_then(|_| self.stdin.flush()).unwrap_or_else(|e| die(&format!("miner pipe closed: {}", e)));
    }
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        if self.stdout.read_line(&mut line).unwrap_or(0) == 0 {
            die("miner process exited");
        }
        serde_json::from_str(&line).unwrap_or_else(|e| die(&format!("bad miner output `{}`: {}", line.trim(), e)))
    }
    fn recv_event(&mut self, ev: &str) -> Value {
        loop {
            let v = self.recv();
            if v["event"] == "error" {
                die(&format!("miner error: {}", v["message"]));
            }
            if v["event"] == ev {
                return v;
            }
        }
    }
    fn job_json(job: &Job, cmd: &str) -> Value {
        let p = &job.profile;
        json!({"cmd": cmd, "profile": {"id": p.id, "d_in": p.d_in, "hidden": p.hidden, "n_hidden": p.n_hidden, "d_out": p.d_out,
               "shift_in": p.shift_in, "shift_hidden": p.shift_hidden},
               "epoch_seed": to_hex(&job.seed), "challenge": to_hex(&job.challenge), "target": job.target.to_hex(),
               "nonce_start": job.start.to_string(), "nonce_count": job.count.to_string(), "stop_on_found": job.stop_on_found})
    }
}

impl Backend for ExecBackend {
    fn name(&self) -> String {
        format!("exec:{}", self.info["backend"].as_str().unwrap_or("?"))
    }
    fn run(&mut self, job: &Job, _stop: &AtomicBool) -> JobResult {
        self.send(&Self::job_json(job, "job"));
        let mut found = Vec::new();
        loop {
            let v = self.recv();
            match v["event"].as_str() {
                Some("found") => {
                    let n: u64 = v["nonce"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                    found.push((n, from_hex32(v["digest"].as_str().unwrap_or("")).unwrap_or([0; 32])));
                }
                Some("done") => {
                    return JobResult {
                        found,
                        attempts: v["attempts"].as_str().and_then(|s| s.parse().ok()).or(v["attempts"].as_u64()).unwrap_or(0),
                        elapsed: v["elapsed_s"].as_f64().unwrap_or(0.0),
                    }
                }
                Some("error") => die(&format!("miner error: {}", v["message"])),
                _ => {}
            }
        }
    }
    fn digests(&mut self, job: &Job, nonces: &[u64]) -> Vec<Hash32> {
        let mut j = Self::job_json(job, "digest");
        j["nonces"] = json!(nonces.iter().map(|n| n.to_string()).collect::<Vec<_>>());
        self.send(&j);
        let v = self.recv_event("digests");
        v["digests"].as_array().map(|a| a.iter().map(|d| from_hex32(d.as_str().unwrap_or("")).unwrap_or([0; 32])).collect()).unwrap_or_default()
    }
}

fn make_backend(args: &[String]) -> Box<dyn Backend> {
    let threads = arg(args, "--threads").and_then(|v| v.parse().ok()).unwrap_or_else(|| std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1));
    match arg(args, "--backend").as_deref().unwrap_or("cpu") {
        "cpu" => Box::new(CpuBackend { threads, weights: None }),
        "exec" => Box::new(ExecBackend::spawn(&arg(args, "--exec").unwrap_or_else(|| die("--exec \"CMD\" required for exec backend")))),
        b => die(&format!("unknown backend {}", b)),
    }
}

/// One keep-alive agent for the whole miner (no per-request sockets).
fn agent() -> &'static ureq::Agent {
    static A: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    A.get_or_init(|| ureq::AgentBuilder::new().max_idle_connections_per_host(4).build())
}

fn http_get(url: &str) -> Result<Value, String> {
    match agent().get(url).timeout(Duration::from_secs(10)).call() {
        Ok(r) => r.into_json().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(c, r)) => Err(format!("HTTP {}: {}", c, r.into_string().unwrap_or_default())),
        Err(e) => Err(e.to_string()),
    }
}

fn mine(args: &[String]) {
    let node = arg(args, "--node").unwrap_or_else(|| "http://127.0.0.1:9441".into());
    let address = arg(args, "--address");
    let max_blocks: u64 = arg(args, "--max-blocks").and_then(|v| v.parse().ok()).unwrap_or(u64::MAX);
    let spot: usize = arg(args, "--spot-check").and_then(|v| v.parse().ok()).unwrap_or(2);
    let mut backend = make_backend(args);
    let mut cpu = CpuBackend { threads: 2, weights: None };
    eprintln!("[zpu] backend {} -> node {}", backend.name(), node);
    let batch: u64 = arg(args, "--batch").and_then(|v| v.parse().ok()).unwrap_or(0);
    // Adaptive job size: aim for `job_secs` of backend work per job so the
    // fixed per-job cost (get_work, spot checks, process round trip) stays
    // small on fast GPUs. Measured from the previous job; --batch overrides.
    let job_secs: f64 = arg(args, "--job-secs").and_then(|v| v.parse().ok()).unwrap_or(0.5);
    let mut adaptive: Option<u64> = None;
    let (mut accepted, mut rejected, mut total_attempts, mut gpu_errors) = (0u64, 0u64, 0u64, 0u64);
    let t_start = Instant::now();
    let stop = AtomicBool::new(false);
    let mut nonce_base: u64 = rand::random::<u64>() >> 1;
    while accepted < max_blocks {
        let url = match &address {
            Some(a) => format!("{}/v1/work?address={}", node, a),
            None => format!("{}/v1/work", node),
        };
        let w = match http_get(&url) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[zpu] get_work failed: {} (retrying)", e);
                std::thread::sleep(Duration::from_millis(if e.contains("ahead of this node's clock") { 500 } else { 2000 }));
                continue;
            }
        };
        // A node or proxy may answer with an error body (for example a public NEAR RPC
        // timing out or rate limiting). That is transient: wait and ask again.
        if let Some(e) = w.get("error") {
            eprintln!("[zpu] get_work: node reported an error: {} (retrying)", e);
            std::thread::sleep(Duration::from_millis(2000));
            continue;
        }
        let profile = profile_by_id(w["version"].as_u64().unwrap_or(0) as u32).unwrap_or_else(|| die("node uses an unknown PoW profile"));
        let job = Job {
            profile,
            seed: from_hex32(w["epoch_seed"].as_str().unwrap()).unwrap(),
            challenge: from_hex32(w["challenge"].as_str().unwrap()).unwrap(),
            target: U256::from_hex(w["target"].as_str().unwrap()).unwrap(),
            start: nonce_base,
            count: if batch > 0 {
                batch
            } else if let Some(c) = adaptive {
                c
            } else if profile.hidden > 1024 {
                2000
            } else {
                200_000
            },
            stop_on_found: true,
        };
        // Header-binding self-check: the challenge must match the header bytes.
        let hb = hex::decode(w["header_without_nonce"].as_str().unwrap()).unwrap();
        let chk = zfx_verify::hash::sha256(&[pow::CHALLENGE_DOMAIN, &hb]);
        if chk != job.challenge {
            die("node returned a challenge that does not match its header");
        }
        nonce_base = nonce_base.wrapping_add(job.count);
        stop.store(false, Ordering::Relaxed);
        let t_job = Instant::now();
        let r = backend.run(&job, &stop);
        let el = t_job.elapsed().as_secs_f64();
        total_attempts += r.attempts;
        if batch == 0 && r.attempts > 0 && el > 0.0 && r.found.is_empty() {
            let next = (r.attempts as f64 / el * job_secs) as u64;
            // grow at most 16x per step; bounded to [1000, 2^30]
            adaptive = Some(next.clamp(1000, job.count.saturating_mul(16)).min(1 << 30));
        }
        // GPU correctness checks: random spot checks + every winner.
        let mut check: Vec<u64> = (0..spot).map(|_| job.start + rand::random::<u64>() % job.count.max(1)).collect();
        check.extend(r.found.iter().map(|f| f.0));
        if !check.is_empty() {
            let theirs = if spot > 0 { backend.digests(&job, &check) } else { vec![] };
            let ours = cpu.digests(&job, &check);
            for (i, n) in check.iter().enumerate() {
                let reported = theirs.get(i).copied().or_else(|| r.found.iter().find(|f| f.0 == *n).map(|f| f.1));
                if reported.is_some() && reported != Some(ours[i]) {
                    gpu_errors += 1;
                    eprintln!("[zpu] CORRECTNESS FAILURE: backend digest for nonce {} != CPU reference", n);
                }
            }
            if gpu_errors > 0 {
                die("backend produced non-canonical digests; refusing to mine (run `zpu-cli selftest`)");
            }
        }
        for (n, _) in r.found {
            let res: Value = agent()
                .post(&format!("{}/v1/submit", node))
                .send_json(json!({"work_id": w["work_id"], "nonce": n.to_string()}))
                .map(|r| r.into_json().unwrap_or(Value::Null))
                .unwrap_or_else(|e| json!({"accepted": false, "error": e.to_string()}));
            if res["accepted"] == true {
                accepted += 1;
                eprintln!("[zpu] block {} accepted at height {} (nonce {})", res["block_hash"].as_str().unwrap_or("?"), w["height"], n);
                break; // template is stale now
            } else {
                rejected += 1;
                eprintln!("[zpu] submit rejected: {}", res["error"]);
                std::thread::sleep(Duration::from_millis(200)); // back off on rejection
            }
        }
        let el = t_start.elapsed().as_secs_f64();
        eprintln!(
            "[zpu] h={} attempts={} rate={:.1}/s job_rate={:.1}/s accepted={} rejected={} gpu_errors={}",
            w["height"],
            total_attempts,
            total_attempts as f64 / el,
            r.attempts as f64 / r.elapsed.max(1e-9),
            accepted,
            rejected,
            gpu_errors
        );
    }
}

fn bench(args: &[String]) {
    let profile = match arg(args, "--profile").as_deref().unwrap_or("v01") {
        "dev" => FP8C_V01_DEV,
        _ => FP8C_V01,
    };
    let secs: f64 = arg(args, "--seconds").and_then(|v| v.parse().ok()).unwrap_or(5.0);
    let mut b = make_backend(args);
    let job0 = |count| Job { profile, seed: [0x5e; 32], challenge: [0xc4; 32], target: U256::ZERO, start: 0, count, stop_on_found: false };
    let stop = AtomicBool::new(false);
    let warm = b.run(&job0(if profile.hidden > 1024 { 64 } else { 4096 }), &stop);
    let mut count = ((warm.attempts as f64 / warm.elapsed.max(1e-6)) * secs).max(16.0) as u64;
    if let Some(c) = arg(args, "--count").and_then(|v| v.parse().ok()) {
        count = c;
    }
    let r = b.run(&job0(count), &stop);
    let rate = r.attempts as f64 / r.elapsed;
    println!(
        "{}",
        json!({"backend": b.name(), "profile": profile.name, "attempts": r.attempts, "elapsed_s": r.elapsed,
               "attempts_per_s": rate, "tera_macs_per_s": rate * profile.macs_per_attempt() as f64 / 1e12})
    );
}

fn selftest(args: &[String]) {
    let path = arg(args, "--vectors").unwrap_or_else(|| "tests/vectors/fp8c_v0.1.json".into());
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| die(&format!("{}: {}", path, e)))).unwrap();
    let mut b = make_backend(args);
    let mut fails = 0;
    let mut total = 0;
    for key in ["dev", "v01"] {
        let pv = &v[key];
        let profile = profile_by_id(pv["profile"]["id"].as_u64().unwrap() as u32).unwrap();
        let job = Job {
            profile,
            seed: from_hex32(pv["epoch_seed"].as_str().unwrap()).unwrap(),
            challenge: from_hex32(pv["challenge"].as_str().unwrap()).unwrap(),
            target: U256::ZERO,
            start: 0,
            count: 0,
            stop_on_found: false,
        };
        let nonces: Vec<u64> = pv["attempts"].as_array().unwrap().iter().map(|a| a["nonce"].as_str().unwrap().parse().unwrap()).collect();
        let got = b.digests(&job, &nonces);
        for (i, a) in pv["attempts"].as_array().unwrap().iter().enumerate() {
            total += 1;
            let ok = got.get(i).map(|d| to_hex(d)) == a["digest"].as_str().map(String::from);
            if !ok {
                fails += 1;
            }
            println!("{} {} nonce={} {}", if ok { "PASS" } else { "FAIL" }, profile.name, a["nonce"].as_str().unwrap(), b.name());
        }
    }
    println!("{}/{} vectors bit-exact on {}", total - fails, total, b.name());
    if fails > 0 {
        std::process::exit(1);
    }
}

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", USAGE);
        return;
    }
    match args.first().map(String::as_str) {
        Some("mine") => mine(&args),
        Some("bench") => bench(&args),
        Some("selftest") => selftest(&args),
        _ => eprintln!("usage: zpu-cli mine|bench|selftest [options]  (see source header)"),
    }
}
