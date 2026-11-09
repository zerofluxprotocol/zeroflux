//! Mandatory experiments: monetary schedule alternatives, difficulty/fork
//! resilience (LWMA under hashrate shocks and timestamp manipulation), and
//! CPU mining-workload sweeps.

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use zfx_verify::difficulty::{next_target, DifficultyParams, LWMA_WINDOW};
use zfx_verify::monetary::SubsidySchedule;
use zfx_verify::U256;

const ZFX: u64 = 100_000_000;

pub fn tokenomics() -> Value {
    let spacing = 30u64; // testnet target spacing (s)
    let per_year = 365 * 86_400 / spacing;
    let cands: Vec<(&str, SubsidySchedule, &str)> = vec![
        ("A_constant_50", SubsidySchedule::Constant { per_block: 50 * ZFX }, "current testnet; uncapped, inflation falls as 1/t"),
        ("B_halving_2y_tail", SubsidySchedule::Halving { initial: 50 * ZFX, interval: 2 * per_year, tail: ZFX / 2 }, "halving every 2 years, 0.5 ZFX tail"),
        ("C_halving_4y_cap", SubsidySchedule::Halving { initial: 50 * ZFX, interval: 4 * per_year, tail: 0 }, "Bitcoin-like, hard cap, no tail"),
        ("D_smooth_decay_tail", SubsidySchedule::SmoothDecay { initial: 50 * ZFX, epoch: 86_400 / spacing, decay_ppm: 474, tail: ZFX / 2 },
            "daily ~0.0474% decay (halves every ~4 y), 0.5 ZFX tail: no halving shocks"),
    ];
    let years = [1u64, 2, 4, 8, 16, 32, 64];
    let rows: Vec<Value> = cands
        .iter()
        .map(|(name, s, note)| {
            let pts: Vec<Value> = years
                .iter()
                .map(|y| {
                    let h = y * per_year;
                    let sup = s.cumulative(h);
                    let prev = s.cumulative(h - per_year);
                    let issued = sup - prev;
                    json!({"year": y, "supply_zfx": (sup / ZFX as u128) as u64, "annual_issuance_zfx": (issued / ZFX as u128) as u64,
                           "inflation_pct": if prev > 0 { issued as f64 * 100.0 / prev as f64 } else { f64::INFINITY },
                           "subsidy_per_block_zfx": s.subsidy_at(h) as f64 / ZFX as f64})
                })
                .collect();
            json!({"name": name, "schedule": s, "note": note, "points": pts})
        })
        .collect();
    json!({"experiment": "monetary_schedules", "target_spacing_s": spacing, "blocks_per_year": per_year, "candidates": rows,
           "decision": "OWNER DECISION REQUIRED (not made here). Data-backed recommendation: D_smooth_decay_tail - no step changes in miner revenue (halvings cause hashrate shocks the LWMA must absorb), a perpetual tail keeps a security budget once fees are small, and supply converges toward a predictable asymptote plus a small tail. Mainnet activation is blocked in the node until a schedule is selected and signed off."})
}

/// Simulate LWMA-1 with exponential block times. `hash(t)` is network
/// hashrate relative to the genesis difficulty (1.0 => mean = spacing).
fn simulate(blocks: usize, seed: u64, hash: &dyn Fn(usize) -> f64, attacker: Option<(f64, &str)>) -> Value {
    simulate_rule(blocks, seed, hash, attacker, zfx_verify::difficulty::SolvetimeRule::RunningMax)
}

fn simulate_rule(blocks: usize, seed: u64, hash: &dyn Fn(usize) -> f64, attacker: Option<(f64, &str)>, rule: zfx_verify::difficulty::SolvetimeRule) -> Value {
    let p = DifficultyParams { solvetime_rule: rule, target_spacing_secs: 60, pow_limit: U256::MAX.shr(2), fixed: false };
    let g = U256::MAX.shr(20);
    let mut rng = StdRng::seed_from_u64(seed);
    let mut hist: Vec<(u64, U256)> = vec![(0, g)];
    let mut true_time = 0f64;
    let mut solvetimes = Vec::new();
    for i in 1..=blocks {
        let tgt = if hist.len() < LWMA_WINDOW + 1 { g } else { next_target(&p, &g, &hist[hist.len() - LWMA_WINDOW - 1..]) };
        // difficulty relative to genesis
        let rel = (g.log2_f64() - tgt.log2_f64()).exp2();
        let mean = 60.0 * rel / hash(i);
        let st = -mean * (1.0 - rng.gen::<f64>()).ln();
        true_time += st;
        solvetimes.push(st);
        let mtp = {
            let n = hist.len().min(11);
            let mut v: Vec<u64> = hist[hist.len() - n..].iter().map(|x| x.0).collect();
            v.sort();
            v[v.len() / 2]
        };
        let mut ts = (true_time as u64).max(mtp + 1);
        if let Some((share, mode)) = attacker {
            if rng.gen::<f64>() < share {
                ts = match mode {
                    "future" => (true_time as u64 + 120).max(mtp + 1), // max future drift
                    _ => mtp + 1,                                       // minimal timestamps
                };
            }
        }
        hist.push((ts, tgt));
    }
    let tail = &solvetimes[solvetimes.len() / 2..];
    let mean_tail = tail.iter().sum::<f64>() / tail.len() as f64;
    json!({"blocks": blocks, "mean_block_time_second_half_s": mean_tail, "target_s": 60})
}

pub fn fork_resilience() -> Value {
    let n = 4000;
    let steady = simulate(n, 1, &|_| 1.0, None);
    let jump = simulate(n, 2, &|i| if i > 1000 { 10.0 } else { 1.0 }, None);
    let drop = simulate(n, 3, &|i| if i > 1000 { 0.1 } else { 1.0 }, None);
    let osc = simulate(n, 4, &|i| if (i / 200) % 2 == 0 { 1.0 } else { 5.0 }, None);
    let fut = simulate(n, 5, &|_| 1.0, Some((0.3, "future")));
    let low = simulate(n, 6, &|_| 1.0, Some((0.3, "minimal")));
    // recovery speed after a 10x jump: blocks until a 30-block mean is within 25% of target
    let recovery = {
        let p = DifficultyParams { solvetime_rule: zfx_verify::difficulty::SolvetimeRule::RunningMax, target_spacing_secs: 60, pow_limit: U256::MAX.shr(2), fixed: false };
        let g = U256::MAX.shr(20);
        let mut rng = StdRng::seed_from_u64(9);
        let mut hist: Vec<(u64, U256)> = (0..31).map(|i| (i * 60, g)).collect();
        let mut t = 30.0 * 60.0;
        let mut win: Vec<f64> = Vec::new();
        let mut found = None;
        for i in 0..2000 {
            let tgt = next_target(&p, &g, &hist[hist.len() - 31..]);
            let rel = (g.log2_f64() - tgt.log2_f64()).exp2();
            let st = -(60.0 * rel / 10.0) * (1.0 - rng.gen::<f64>()).ln();
            t += st;
            hist.push((t as u64, tgt));
            win.push(st);
            if win.len() > 30 {
                win.remove(0);
            }
            if win.len() == 30 && found.is_none() && (win.iter().sum::<f64>() / 30.0 - 60.0).abs() < 15.0 {
                found = Some(i);
            }
        }
        found
    };
    use zfx_verify::difficulty::SolvetimeRule::{PreviousBlock, RunningMax};
    let mut cmp = Vec::new();
    for (label, rule) in [("previous_block (classic LWMA-1)", PreviousBlock), ("running_max (selected)", RunningMax)] {
        cmp.push(json!({"rule": label,
            "steady": simulate_rule(n, 1, &|_| 1.0, None, rule)["mean_block_time_second_half_s"],
            "x10_jump": simulate_rule(n, 2, &|i| if i > 1000 { 10.0 } else { 1.0 }, None, rule)["mean_block_time_second_half_s"],
            "x0.1_drop": simulate_rule(n, 3, &|i| if i > 1000 { 0.1 } else { 1.0 }, None, rule)["mean_block_time_second_half_s"],
            "attacker_30pct_minimal_ts": simulate_rule(n, 6, &|_| 1.0, Some((0.3, "minimal")), rule)["mean_block_time_second_half_s"],
            "attacker_30pct_future_ts": simulate_rule(n, 5, &|_| 1.0, Some((0.3, "future")), rule)["mean_block_time_second_half_s"],
            "attacker_50pct_minimal_ts": simulate_rule(n, 7, &|_| 1.0, Some((0.5, "minimal")), rule)["mean_block_time_second_half_s"]}));
    }
    json!({"experiment": "difficulty_and_fork_resilience", "algorithm": "LWMA-1 N=30, solvetime clamp [1, 6T], MTP-11, +120 s future limit",
           "solvetime_rule_comparison_target_60s": cmp,
           "steady": steady, "hashrate_x10_jump_at_1000": jump, "hashrate_x0.1_drop_at_1000": drop, "oscillating_1x_5x_every_200": osc,
           "attacker_30pct_max_future_timestamps": fut, "attacker_30pct_minimal_timestamps": low,
           "blocks_to_recover_after_10x_jump": recovery,
           "multi_node_evidence": "bench/results/e2e_native.json (partition/rejoin reorg, 1000+ blocks), e2e_bridge_reorg.json (deep fork adoption)",
           "verdict": "block time stays near target under 10x shocks and 30% timestamp manipulation; manipulation biases the mean by the clamp-bounded amount reported above"})
}

pub fn workload() -> Value {
    use zfx_verify::pow::{self, Weights, FP8C_V01};
    let p = FP8C_V01;
    let w = Weights::generate_par(&p, &[3u8; 32], 16);
    let ch = [5u8; 32];
    let threads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4);
    let mut rows = Vec::new();
    for t in [1usize, 2, 4, 8, threads] {
        let per = 64usize;
        let t0 = std::time::Instant::now();
        std::thread::scope(|s| {
            for k in 0..t {
                let w = &w;
                s.spawn(move || {
                    for i in 0..per {
                        std::hint::black_box(pow::attempt(w, &ch, (k * per + i) as u64));
                    }
                });
            }
        });
        let el = t0.elapsed().as_secs_f64();
        rows.push(json!({"threads": t, "attempts_per_s": (t * per) as f64 / el}));
    }
    json!({"experiment": "mining_workload_portability_cpu", "profile": p.name, "weights_bytes": p.weight_elems(),
           "activation_bytes_per_attempt": p.hidden * 4 + p.hidden, "macs_per_attempt": p.macs_per_attempt(),
           "cpu_thread_scaling": rows,
           "gpu_batch_evidence": "bench/results/gpu/*_round4|5/results/results.jsonl stage profiles at B=8192 and B=32768 (H100, MI355X)",
           "gpu_memory_estimate_bytes_at_B32768": {"weights": p.weight_elems(), "X_ping_pong": 2 * 32768 * p.hidden, "Y_int32": 32768 * p.hidden * 4},
           "decision": "GPU default batch 32768 (measured +14% H100 / +40% MI355X over 8192); CPU miners use one thread per core; minimum supported GPU memory ~0.7 GB"})
}
