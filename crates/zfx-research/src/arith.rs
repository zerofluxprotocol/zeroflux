//! Number formats and accumulation models used by candidates A-E.
//!
//! All candidate products are represented exactly as integers in a fixed
//! point scale (`scale_log2`). "Canonical" means: exact sum, then a single
//! round-to-nearest-even into FP32. The other accumulators model what real
//! hardware/library paths do, so we can measure divergence.

/// OCP FP8 E4M3 (E4M3FN): bias 7, no infinities, NaN = S.1111.111.
/// Returns the value times 2^9 as an exact integer, or None for NaN.
pub fn e4m3_to_fixed9(b: u8) -> Option<i64> {
    if b & 0x7f == 0x7f {
        return None;
    }
    let s = if b & 0x80 != 0 { -1 } else { 1 };
    let e = ((b >> 3) & 0xf) as i64;
    let m = (b & 7) as i64;
    let mag = if e == 0 { m } else { (8 + m) << (e - 1) };
    Some(s * mag)
}

#[cfg(test)]
pub fn e4m3_to_f32(b: u8) -> f32 {
    e4m3_to_fixed9(b).map(|v| v as f32 / 512.0).unwrap_or(f32::NAN)
}

/// BF16 restricted to unbiased exponents [-4, 3] (normal numbers only):
/// value = (128 + m) * 2^(e - 7). Fixed point scale 2^11 per operand.
pub fn bf16w_from_byte_pair(b0: u8, b1: u8) -> (u16, i64) {
    let s = (b0 >> 7) as u16;
    let e = ((b0 >> 4) & 7) as i64 - 4; // [-4, 3]
    let m = b1 as i64 & 0x7f;
    let biased = (e + 127) as u16;
    let bits = (s << 15) | (biased << 7) | m as u16;
    let mag = (128 + m) << (e + 4); // value * 2^11
    (bits, if s == 1 { -mag } else { mag })
}

#[cfg(test)]
pub fn bf16_to_f32(bits: u16) -> f32 {
    f32::from_bits((bits as u32) << 16)
}

/// Round an exact fixed-point sum (value * 2^scale_log2) to FP32 (RNE).
/// Rust's i128 -> f32 cast is round-to-nearest-even; the power-of-two
/// rescale is exact for the magnitudes used here.
pub fn fixed_to_f32_rne(v: i128, scale_log2: i32) -> f32 {
    ((v as f32) as f64 * 2f64.powi(-scale_log2)) as f32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accum {
    /// Exact sum, one RNE rounding (consensus-canonical for A/B).
    Canonical,
    /// Sequential FP32 FMA-free accumulation, k = 0..K.
    SeqF32,
    /// Sequential FP32, reversed order.
    RevF32,
    /// Pairwise tree reduction in FP32.
    PairwiseF32,
    /// Split-K into 4 chunks, FP32 partials summed (typical split-K GEMM).
    SplitK4F32,
    /// Tensor-core-like: chunks of 32 products added exactly, then the chunk
    /// sum is aligned to the running accumulator's exponent and truncated to
    /// `keep` significant bits (models the reduced-precision FP8 WGMMA
    /// accumulator documented by DeepSeek-V3 for H800, ~14 bits).
    TruncAlign { chunk: usize, keep: u32 },
    /// FP16 accumulator (RTX FP8 MMA with FP16 accumulate).
    F16Acc,
}

pub const ALL_ACCUMS: [Accum; 7] =
    [Accum::Canonical, Accum::SeqF32, Accum::RevF32, Accum::PairwiseF32, Accum::SplitK4F32, Accum::TruncAlign { chunk: 32, keep: 14 }, Accum::F16Acc];

pub fn accum_name(a: Accum) -> String {
    match a {
        Accum::Canonical => "canonical_exact_rne".into(),
        Accum::SeqF32 => "seq_f32".into(),
        Accum::RevF32 => "rev_f32".into(),
        Accum::PairwiseF32 => "pairwise_f32".into(),
        Accum::SplitK4F32 => "splitk4_f32".into(),
        Accum::TruncAlign { chunk, keep } => format!("trunc_align_c{}_k{}", chunk, keep),
        Accum::F16Acc => "f16_acc".into(),
    }
}

fn f16_round(x: f32) -> f32 {
    // Round f32 to binary16 (RNE) and back; magnitudes here stay in range
    // or saturate to inf, which is itself a divergence.
    let bits = x.to_bits();
    let sign = bits & 0x8000_0000;
    let ax = f32::from_bits(bits & 0x7fff_ffff);
    if ax >= 65520.0 {
        return f32::from_bits(sign | 0x7f80_0000);
    }
    if ax < 6.103_515_6e-5 {
        // subnormal f16: step 2^-24
        let q = 2f32.powi(-24);
        let r = (ax / q).round_ties_even() * q;
        return f32::from_bits(sign | r.to_bits());
    }
    let e = ax.log2().floor() as i32;
    let q = 2f32.powi(e - 10);
    let r = (ax / q).round_ties_even() * q;
    f32::from_bits(sign | r.to_bits())
}

/// Truncate `v` (exact, scaled) to `keep` significant bits relative to
/// the larger magnitude of `v` and `reference` (alignment to max exponent).
fn trunc_align(v: i128, reference: i128, keep: u32) -> i128 {
    let m = v.unsigned_abs().max(reference.unsigned_abs());
    if m == 0 {
        return v;
    }
    let msb = 128 - m.leading_zeros();
    if msb <= keep {
        return v;
    }
    let drop = msb - keep;
    // truncation toward zero, as alignment shifters do
    let mag = (v.unsigned_abs() >> drop) << drop;
    if v < 0 {
        -(mag as i128)
    } else {
        mag as i128
    }
}

/// Dot product of exact scaled products `p` (each = a*b * 2^scale_log2).
pub fn reduce(p: &[i128], scale_log2: i32, a: Accum) -> f32 {
    let f = |v: i128| fixed_to_f32_rne(v, scale_log2);
    match a {
        Accum::Canonical => f(p.iter().sum()),
        Accum::SeqF32 => p.iter().fold(0f32, |acc, v| acc + f(*v)),
        Accum::RevF32 => p.iter().rev().fold(0f32, |acc, v| acc + f(*v)),
        Accum::PairwiseF32 => {
            let mut lvl: Vec<f32> = p.iter().map(|v| f(*v)).collect();
            while lvl.len() > 1 {
                lvl = lvl.chunks(2).map(|c| if c.len() == 2 { c[0] + c[1] } else { c[0] }).collect();
            }
            lvl.first().copied().unwrap_or(0.0)
        }
        Accum::SplitK4F32 => {
            let q = p.len().div_ceil(4).max(1);
            p.chunks(q).map(|c| c.iter().fold(0f32, |acc, v| acc + f(*v))).fold(0f32, |a, b| a + b)
        }
        Accum::TruncAlign { chunk, keep } => {
            // Accumulator kept exactly as an integer but truncated on every
            // chunk add, then rounded to f32 at the end.
            let mut acc: i128 = 0;
            for c in p.chunks(chunk) {
                let s: i128 = c.iter().sum();
                let s_t = trunc_align(s, acc, keep);
                let acc_t = trunc_align(acc, s, keep);
                acc = acc_t + s_t;
            }
            f(acc)
        }
        Accum::F16Acc => p.iter().fold(0f32, |acc, v| f16_round(acc + f16_round(f(*v)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn e4m3_known_values() {
        assert_eq!(e4m3_to_f32(0x00), 0.0);
        assert_eq!(e4m3_to_f32(0x38), 1.0); // e=7 m=0
        assert_eq!(e4m3_to_f32(0x7e), 448.0);
        assert_eq!(e4m3_to_f32(0x01), 2f32.powi(-9));
        assert!(e4m3_to_f32(0x7f).is_nan());
        assert_eq!(e4m3_to_f32(0xb8), -1.0);
        // all integers 1..=16 are representable
        for i in 1..=16 {
            assert!((0..=0x7eu8).any(|b| e4m3_to_f32(b) == i as f32), "{}", i);
        }
    }
    #[test]
    fn bf16w_consistent() {
        for b0 in [0u8, 0x7f, 0x80, 0xff] {
            for b1 in [0u8, 1, 0x7f] {
                let (bits, fx) = bf16w_from_byte_pair(b0, b1);
                assert_eq!(bf16_to_f32(bits) as f64, fx as f64 / 2048.0);
            }
        }
    }
}
