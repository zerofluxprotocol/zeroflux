//! zfx-research: runs the spec section 2A experiments and writes
//! machine-readable results to bench/results/.
//!
//! usage: zfx-research [all|divergence|throughput|transcript|attacks|vectors] [--out DIR] [--quick]

mod arith;
mod attacks;
mod candidates;
mod transcript;

use arith::{accum_name, Accum, ALL_ACCUMS};
use candidates::*;
use serde_json::{json, Value};
use std::time::Instant;
use zfx_verify::hash::{sha256, to_hex};
use zfx_verify::pow::{self, Weights, FP8C_V01, FP8C_V01_DEV};
use zfx_verify::U256;

fn host() -> Value {
    let cpu = std::process::Command::new("sysctl").args(["-n", "machdep.cpu.brand_string"]).output().ok()
        .and_then(|o| String::from_utf8(o.stdout).ok()).map(|s| s.trim().to_string())
        .or_else(|| std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|c| c.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())))
        .unwrap_or_else(|| "unknown".into());
    json!({
        "cpu": cpu,
        "threads": std::thread::available_parallelism().map(|x| x.get()).unwrap_or(1),
        "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
        "rustc_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
    })
}

fn timeit<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64())
}

/// Candidates A, B, D under every accumulation model: does a non-canonical
/// (hardware-like) reduction reproduce the canonical bits?
fn divergence(n: usize) -> Value {
    let at = sha256(&[b"divergence"]);
    let mut out = serde_json::Map::new();
    // A: unrestricted FP8 E4M3, products scaled 2^18
    let (a8, b8) = gen_a(&at, n);
    let af: Vec<i64> = a8.iter().map(|x| arith::e4m3_to_fixed9(*x).unwrap()).collect();
    let bf: Vec<i64> = b8.iter().map(|x| arith::e4m3_to_fixed9(*x).unwrap()).collect();
    // B: windowed BF16, products scaled 2^22
    let (ab, bb) = gen_b(&at, n);
    // D: restricted domain, scale 0
    let (ad, bd) = gen_d(&at, n);
    let adi: Vec<i64> = ad.iter().map(|v| *v as i64).collect();
    let bdi: Vec<i64> = bd.iter().map(|v| *v as i64).collect();
    for (name, a, b, sc) in [("A_fp8_e4m3_unrestricted", &af, &bf, 18), ("B_bf16_window", &ab, &bb, 22), ("D_fp8_restricted", &adi, &bdi, 0)] {
        let canon = gemm_model(a, b, n, sc, Accum::Canonical);
        let cd = digest_f32(b"div", &at, &canon);
        let mut res = serde_json::Map::new();
        for acc in ALL_ACCUMS {
            let c = if acc == Accum::Canonical { canon.clone() } else { gemm_model(a, b, n, sc, acc) };
            let mism = c.iter().zip(&canon).filter(|(x, y)| x.to_bits() != y.to_bits()).count();
            res.insert(accum_name(acc), json!({
                "mismatched_elements": mism,
                "mismatch_fraction": mism as f64 / (n * n) as f64,
                "digest_matches_canonical": digest_f32(b"div", &at, &c) == cd,
            }));
        }
        out.insert(name.into(), Value::Object(res));
    }
    // Ozaki exact emulation of A on int8
    let (oz, gemms) = gemm_a_ozaki(&a8, &b8, n);
    let canon_a = gemm_a_canonical(&a8, &b8, n);
    json!({
        "experiment": "accumulation_divergence", "n": n, "results": out,
        "ozaki_int8_emulation_of_A": {"int8_gemms": gemms, "bit_exact_vs_canonical": oz.iter().zip(&canon_a).all(|(x, y)| x.to_bits() == y.to_bits())},
    })
}

fn throughput(quick: bool) -> Value {
    let threads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4);
    let at = sha256(&[b"throughput"]);
    let mut rows = Vec::new();
    let na = if quick { 256 } else { 1024 };
    // A canonical (exact int64 fixed point, all threads)
    let ((a8, b8), t_gen) = timeit(|| gen_a(&at, na));
    let (_, t_gemm) = timeit(|| gemm_a_canonical(&a8, &b8, na));
    let (_, t_oz) = timeit(|| gemm_a_ozaki(&a8, &b8, na.min(512)));
    rows.push(json!({"candidate": "A", "n": na, "gen_s": t_gen, "gemm_s": t_gemm, "attempt_s": t_gen + t_gemm,
        "attempts_per_s": 1.0 / (t_gen + t_gemm), "ozaki_9xint8_s_at_n": [na.min(512), t_oz],
        "gen_bytes": 2 * na * na, "macs": (na * na * na) as u64}));
    // B canonical (exact i128 model; slow, so smaller n)
    let nb = 256;
    let ((ab, bb), t_gen) = timeit(|| gen_b(&at, nb));
    let (_, t_gemm) = timeit(|| gemm_model(&ab, &bb, nb, 22, Accum::Canonical));
    rows.push(json!({"candidate": "B", "n": nb, "gen_s": t_gen, "gemm_s": t_gemm, "attempt_s": t_gen + t_gemm,
        "attempts_per_s": 1.0 / (t_gen + t_gemm), "gen_bytes": 4 * nb * nb, "macs": (nb * nb * nb) as u64}));
    // D restricted single GEMM
    let nd = if quick { 512 } else { 1024 };
    let ((ad, bd), t_gen) = timeit(|| gen_d(&at, nd));
    let (_, t_gemm) = timeit(|| gemm_i8_exact(&ad, &bd, nd));
    rows.push(json!({"candidate": "D", "n": nd, "gen_s": t_gen, "gemm_s": t_gemm, "attempt_s": t_gen + t_gemm,
        "attempts_per_s": 1.0 / (t_gen + t_gemm), "gen_bytes": 2 * nd * nd, "macs": (nd * nd * nd) as u64,
        "macs_per_generated_byte": nd as f64 / 2.0}));
    // E selected, both profiles
    for p in [FP8C_V01_DEV, FP8C_V01] {
        let seed = [0xabu8; 32];
        let (w, t_w1) = timeit(|| Weights::generate(&p, &seed));
        let (_, t_wn) = timeit(|| Weights::generate_par(&p, &seed, threads));
        let ch = [0x10u8; 32];
        let n1 = if p.hidden > 1024 { if quick { 20 } else { 100 } } else { 2000 };
        let (_, t1) = timeit(|| for i in 0..n1 { std::hint::black_box(pow::attempt(&w, &ch, i)); });
        let nn = n1 * threads as u64;
        let (_, tn) = timeit(|| {
            std::thread::scope(|s| {
                for t in 0..threads as u64 {
                    let w = &w;
                    s.spawn(move || for i in 0..n1 { std::hint::black_box(pow::attempt(w, &ch, t * n1 + i)); });
                }
            })
        });
        let (_, t_x0) = timeit(|| for i in 0..10_000u64 { std::hint::black_box(pow::x0(&pow::attempt_id(&ch, i), p.d_in)); });
        rows.push(json!({"candidate": "E", "profile": p.name, "macs": p.macs_per_attempt(),
            "weight_bytes": p.weight_elems(), "weight_gen_1thread_s": t_w1, "weight_gen_all_threads_s": t_wn,
            "verify_latency_s": t1 / n1 as f64, "attempts_per_s_1thread": n1 as f64 / t1,
            "attempts_per_s_all_threads": nn as f64 / tn, "threads": threads,
            "per_attempt_gen_bytes": p.d_in, "per_attempt_gen_s": t_x0 / 10_000.0,
            "macs_per_generated_byte_per_attempt": p.macs_per_attempt() as f64 / p.d_in as f64,
            "cpu_gmac_per_s_all_threads": nn as f64 * p.macs_per_attempt() as f64 / tn / 1e9}));
    }
    json!({"experiment": "cpu_throughput", "host": host(), "rows": rows})
}

fn transcript_exp(quick: bool) -> Value {
    use transcript::*;
    let p = CParams { n: 256, tile: 32, kchunk: 32, samples: 4 };
    let ch = [0x44u8; 32];
    let target = U256::MAX.shr(3); // p = 1/8
    // honest C1
    let (mut proof, mut tries) = (None, 0);
    let (_, t_honest) = timeit(|| {
        for nonce in 0..256 {
            tries += 1;
            let (lot, levels) = c1_honest(&p, &ch, nonce);
            if U256::from_be_bytes(&lot) < target {
                proof = Some(c1_make_proof(&p, nonce, &levels));
                break;
            }
        }
    });
    let proof = proof.unwrap();
    let ((ok, vmacs), t_verify) = timeit(|| c1_verify(&p, &ch, &target, &proof));
    // grinding attack at several honest fractions
    let mut attacks = Vec::new();
    for f in [0.75, 0.5, 0.25] {
        let ((forged, rtries, hashes), t) = timeit(|| c1_grind_attack(&p, &ch, &target, 1_000_000, f, if quick { 200_000 } else { 2_000_000 }));
        let accepted = forged.as_ref().map(|pr| c1_verify(&p, &ch, &target, pr).0).unwrap_or(false);
        attacks.push(json!({"honest_tile_fraction": f, "found": forged.is_some(), "verifier_accepted": accepted,
            "root_tries": rtries, "hash_calls": hashes, "wall_s": t,
            "expected_tries": 1.0 / (0.125 * f64::powi(f, p.samples as i32)),
            "gemm_macs_spent": (f * (p.n * p.n * p.n) as f64) as u64}));
    }
    // C2 tile tickets
    let nt = p.n / p.tile;
    let ((ok2, m2, g2), t2) = timeit(|| {
        let at = attempt_of(&ch, 7);
        let (a, bt) = inputs(&at, p.n);
        let mut best = (0usize, 0usize, [0xffu8; 32]);
        for x in 0..nt * nt {
            let l = tile_leaf(&p, &a, &bt, x / nt, x % nt);
            let t = c2_ticket(&ch, 7, x / nt, x % nt, &l);
            if t < best.2 { best = (x / nt, x % nt, t); }
        }
        let tgt = U256::from_be_bytes(&best.2).checked_add(&U256::from_u64(1)).unwrap();
        c2_verify(&p, &ch, &tgt, 7, best.0, best.1)
    });
    json!({
        "experiment": "candidate_C_transcripts",
        "params": {"n": p.n, "tile": p.tile, "kchunk": p.kchunk, "samples": p.samples, "lottery_p": 0.125},
        "C1_honest": {"attempts_to_win": tries, "wall_s": t_honest, "proof_bytes": proof.size_bytes(),
            "verify_ok": ok, "verify_macs": vmacs, "full_gemm_macs": (p.n * p.n * p.n) as u64, "verify_s": t_verify},
        "C1_root_grinding_attack": attacks,
        "C1_lottery_first_variant": {"attacker_work_fraction_per_proof": c1_lottery_first_work_ratio(&p),
            "verdict": "lottery evaluated before any GEMM; attacker computes only s opened tiles of winning nonces"},
        "C2_tile_ticket": {"verify_ok": ok2, "verify_macs": m2, "verify_generated_bytes": g2,
            "ticket_macs": (p.tile * p.tile * p.n) as u64, "proof_bytes": 8 + 4 + 4,
            "honest_generated_bytes_per_ticket": (2 * p.n * p.n) as f64 / (nt * nt) as f64, "wall_s_all_tiles_plus_verify": t2},
    })
}

fn attacks_exp(quick: bool) -> Value {
    json!({
        "parity_dev": attacks::parity_attack(&FP8C_V01_DEV, if quick { 50 } else { 300 }),
        "avalanche_dev": attacks::avalanche(&FP8C_V01_DEV, 200),
        "accumulators_v01": attacks::accumulator_models(&FP8C_V01, if quick { 64 } else { 512 }),
        "accumulators_dev": attacks::accumulator_models(&FP8C_V01_DEV, 256),
        "strassen": attacks::strassen_experiment(if quick { 256 } else { 512 }),
        "low_rank": attacks::low_rank_experiment(),
        "four_russians": attacks::four_russians_memory(),
        "acceptance_dev": attacks::acceptance(&FP8C_V01_DEV, if quick { 20_000 } else { 200_000 }, 4),
        "reuse_dev": attacks::reuse_experiment(&FP8C_V01_DEV),
        "reuse_v01": attacks::reuse_experiment(&FP8C_V01),
    })
}

fn write(dir: &str, name: &str, v: &Value) {
    std::fs::create_dir_all(dir).unwrap();
    let p = format!("{}/{}.json", dir, name);
    std::fs::write(&p, serde_json::to_string_pretty(v).unwrap()).unwrap();
    eprintln!("wrote {}", p);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("all");
    let quick = args.iter().any(|a| a == "--quick");
    let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| "bench/results".into());
    let run = |c: &str| cmd == "all" || cmd == c;
    if cmd == "vectors" {
        let v = zfx_verify::vectors::all_vectors();
        std::fs::create_dir_all("tests/vectors").unwrap();
        std::fs::write("tests/vectors/fp8c_v0.1.json", serde_json::to_string_pretty(&v).unwrap()).unwrap();
        eprintln!("wrote tests/vectors/fp8c_v0.1.json (challenge {})", v["v01"]["challenge"]);
        return;
    }
    if run("divergence") {
        let (v, t) = timeit(|| divergence(if quick { 64 } else { 256 }));
        eprintln!("divergence done in {:.1}s", t);
        write(&out, "divergence", &json!({"host": host(), "data": v}));
    }
    if run("transcript") {
        let (v, t) = timeit(|| transcript_exp(quick));
        eprintln!("transcript done in {:.1}s", t);
        write(&out, "transcript", &json!({"host": host(), "data": v}));
    }
    if run("attacks") {
        let (v, t) = timeit(|| attacks_exp(quick));
        eprintln!("attacks done in {:.1}s", t);
        write(&out, "attacks", &json!({"host": host(), "data": v}));
    }
    if run("throughput") {
        let (v, t) = timeit(|| throughput(quick));
        eprintln!("throughput done in {:.1}s", t);
        write(&out, "throughput_cpu", &v);
    }
    let _ = to_hex(&[0]);
}
