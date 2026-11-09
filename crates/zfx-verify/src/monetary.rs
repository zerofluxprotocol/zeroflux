//! Monetary policy parameterization (consensus v2).
//!
//! The production schedule is an owner decision (docs/TOKEN_ACCOUNTING.md);
//! these are the deterministic, integer-only families the chain can enforce.
//! The private testnet keeps `Constant { per_block: 50 ZFX }`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubsidySchedule {
    /// Fixed reward every block (testnet default; uncapped supply).
    Constant { per_block: u64 },
    /// initial >> (height / interval), never below `tail` (Bitcoin-like with tail).
    Halving { initial: u64, interval: u64, tail: u64 },
    /// Every `epoch` blocks the reward is multiplied by (1e6 - decay_ppm)/1e6,
    /// rounded down, never below `tail`.
    SmoothDecay { initial: u64, epoch: u64, decay_ppm: u32, tail: u64 },
}

impl SubsidySchedule {
    pub fn subsidy_at(&self, height: u64) -> u64 {
        if height == 0 {
            return 0;
        }
        match self {
            SubsidySchedule::Constant { per_block } => *per_block,
            SubsidySchedule::Halving { initial, interval, tail } => {
                let n = (height - 1) / (*interval).max(1);
                let v = if n >= 64 { 0 } else { initial >> n };
                v.max(*tail)
            }
            SubsidySchedule::SmoothDecay { initial, epoch, decay_ppm, tail } => {
                let n = (height - 1) / (*epoch).max(1);
                // integer exponentiation by squaring on a 1e12 fixed point
                let keep = 1_000_000u128 - (*decay_ppm as u128).min(1_000_000);
                let mut factor: u128 = 1_000_000_000_000; // 1.0
                let mut base: u128 = keep * 1_000_000; // keep/1e6 on 1e12 scale
                let mut e = n;
                while e > 0 && factor > 0 {
                    if e & 1 == 1 {
                        factor = factor * base / 1_000_000_000_000;
                    }
                    base = base * base / 1_000_000_000_000;
                    e >>= 1;
                }
                let v = (*initial as u128 * factor / 1_000_000_000_000) as u64;
                v.max(*tail)
            }
        }
    }

    /// Total issuance for heights 1..=to (exact, O(number of eras)).
    pub fn cumulative(&self, to: u64) -> u128 {
        match self {
            SubsidySchedule::Constant { per_block } => *per_block as u128 * to as u128,
            SubsidySchedule::Halving { interval, .. } | SubsidySchedule::SmoothDecay { epoch: interval, .. } => {
                let step = (*interval).max(1);
                let mut total = 0u128;
                let mut h = 1u64;
                while h <= to {
                    let end = (((h - 1) / step) + 1) * step;
                    let end = end.min(to);
                    total += self.subsidy_at(h) as u128 * (end - h + 1) as u128;
                    h = end + 1;
                }
                total
            }
        }
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        match self {
            SubsidySchedule::Constant { per_block } => {
                b.push(1);
                b.extend_from_slice(&per_block.to_le_bytes());
            }
            SubsidySchedule::Halving { initial, interval, tail } => {
                b.push(2);
                for v in [initial, interval, tail] {
                    b.extend_from_slice(&v.to_le_bytes());
                }
            }
            SubsidySchedule::SmoothDecay { initial, epoch, decay_ppm, tail } => {
                b.push(3);
                b.extend_from_slice(&initial.to_le_bytes());
                b.extend_from_slice(&epoch.to_le_bytes());
                b.extend_from_slice(&decay_ppm.to_le_bytes());
                b.extend_from_slice(&tail.to_le_bytes());
            }
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedules_are_deterministic_and_cumulative_matches_sum() {
        let s = [
            SubsidySchedule::Constant { per_block: 50 },
            SubsidySchedule::Halving { initial: 5000, interval: 10, tail: 3 },
            SubsidySchedule::SmoothDecay { initial: 5000, epoch: 7, decay_ppm: 100_000, tail: 2 },
        ];
        for sc in &s {
            let mut sum = 0u128;
            for h in 1..=200u64 {
                sum += sc.subsidy_at(h) as u128;
                assert_eq!(sc.cumulative(h), sum, "{:?} at {}", sc, h);
            }
        }
        assert_eq!(s[1].subsidy_at(1), 5000);
        assert_eq!(s[1].subsidy_at(11), 2500);
        assert_eq!(s[1].subsidy_at(10_000), 3);
        assert_eq!(s[2].subsidy_at(8), 4500); // one epoch of 10% decay
        assert_eq!(s[0].subsidy_at(0), 0);
    }
}
