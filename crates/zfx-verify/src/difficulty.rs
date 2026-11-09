//! Deterministic difficulty and timestamp rules (private-testnet version).
//! LWMA-1 retarget over a 30-block window; integer-only arithmetic.

use crate::u256::U256;

pub const LWMA_WINDOW: usize = 30;
pub const MTP_WINDOW: usize = 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolvetimeRule {
    /// Classic LWMA-1: t_i - t_{i-1} (kept for the experiment's comparison).
    PreviousBlock,
    /// Consensus v2 rule: t_i - max(t_0..t_{i-1}).
    RunningMax,
}

#[derive(Clone, Debug)]
pub struct DifficultyParams {
    pub solvetime_rule: SolvetimeRule,
    pub target_spacing_secs: u64,
    pub pow_limit: U256,
    /// Genesis-configured fixed difficulty (test environments only).
    pub fixed: bool,
}

/// `history` holds (timestamp, target) for the most recent blocks, oldest
/// first, ending at the parent of the block being validated. The genesis
/// target is used until LWMA_WINDOW + 1 blocks exist.
pub fn next_target(p: &DifficultyParams, genesis_target: &U256, history: &[(u64, U256)]) -> U256 {
    if p.fixed || history.len() < LWMA_WINDOW + 1 {
        return *genesis_target;
    }
    let n = LWMA_WINDOW as u64;
    let t = p.target_spacing_secs;
    let w = &history[history.len() - LWMA_WINDOW - 1..];
    let mut weighted: u64 = 0;
    let mut avg = U256::ZERO;
    // Solvetimes are measured against the running maximum of earlier
    // timestamps, not the immediately preceding block, so an out-of-order
    // (MTP+1) timestamp cannot make the *next* honest block look slow.
    // Selected by the timestamp-manipulation experiment
    // (bench/results/fork_resilience.json): a 30% miner using minimal
    // timestamps sped the chain to 24 s/60 s with the naive form.
    let mut prev_max = w[0].0;
    for i in 1..=LWMA_WINDOW {
        let st = match p.solvetime_rule {
            SolvetimeRule::PreviousBlock => w[i].0.saturating_sub(w[i - 1].0).clamp(1, 6 * t),
            SolvetimeRule::RunningMax => w[i].0.saturating_sub(prev_max).clamp(1, 6 * t),
        };
        prev_max = prev_max.max(w[i].0);
        weighted += i as u64 * st;
        avg = avg.saturating_add(&w[i].1.div_u64(n));
    }
    let k = n * (n + 1) / 2 * t;
    let next = avg.saturating_mul_u64(weighted).div_u64(k);
    if next > p.pow_limit {
        p.pow_limit
    } else if next.is_zero() {
        U256::from_u64(1)
    } else {
        next
    }
}

pub fn median_time_past(timestamps: &[u64]) -> u64 {
    let n = timestamps.len().min(MTP_WINDOW);
    let mut v: Vec<u64> = timestamps[timestamps.len() - n..].to_vec();
    v.sort_unstable();
    v[v.len() / 2]
}

/// timestamp > MTP(last 11) and timestamp <= now + max_future.
pub fn check_timestamp(ts: u64, ancestors: &[u64], now: u64, max_future: u64) -> Result<(), String> {
    let mtp = median_time_past(ancestors);
    if ts <= mtp {
        return Err(format!("timestamp {} <= median time past {}", ts, mtp));
    }
    if ts > now + max_future {
        return Err(format!("timestamp {} too far in future (now {})", ts, now));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lwma_tracks_hashrate() {
        let p = DifficultyParams { solvetime_rule: SolvetimeRule::RunningMax, target_spacing_secs: 10, pow_limit: U256::MAX.shr(1), fixed: false };
        let g = U256::MAX.shr(20);
        let mut hist: Vec<(u64, U256)> = (0..31).map(|i| (i * 10, g)).collect();
        assert_eq!(next_target(&p, &g, &hist), g.div_u64(30).saturating_mul_u64(30));
        // Blocks twice as fast -> target roughly halves (harder).
        hist = (0..31).map(|i| (i * 5, g)).collect();
        let nt = next_target(&p, &g, &hist);
        assert!(nt < g && nt > g.div_u64(3));
    }
    #[test]
    fn mtp_rule() {
        let ts: Vec<u64> = (1..=11).collect();
        assert!(check_timestamp(6, &ts, 100, 10).is_err());
        assert!(check_timestamp(7, &ts, 100, 10).is_ok());
        assert!(check_timestamp(200, &ts, 100, 10).is_err());
    }
}
