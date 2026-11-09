//! ZFX-FP8C v0.1: the selected proof-of-work (see docs/ALGORITHM_DECISION.md).
//!
//! An attempt is one row pushed through a chain of restricted-domain FP8
//! GEMM layers with per-epoch random weights and a nonlinear S-box
//! requantization between layers. Operands are integers in
//! D = {-16..-1, 1..16}. Every value in D is exact in OCP FP8 E4M3,
//! AMD E4M3FNUZ, and int8. Every layer's dot product satisfies
//! |sum| <= K * 256 <= 2^20 < 2^24, so FP32 accumulation is exact in any
//! reduction order and int32 accumulation is exact trivially. Consensus is
//! defined on the integers; hardware formats are an implementation choice.

use crate::hash::{sha256, shake256_into, Hash32};
use crate::header::BlockHeader;
use crate::u256::U256;

pub const CHALLENGE_DOMAIN: &[u8] = b"ZFX-FP8C-v0.1";
pub const ATTEMPT_DOMAIN: &[u8] = b"ZFX-attempt-v0.1";
pub const RESULT_DOMAIN: &[u8] = b"ZFX-result-v0.1";
pub const EPOCH_DOMAIN: &[u8] = b"ZFX-FP8C-epoch-v0.1";
pub const WEIGHT_DOMAIN: &[u8] = b"ZFX-FP8C-W-v0.1";
pub const X0_DOMAIN: &[u8] = b"ZFX-FP8C-X0-v0.1";

/// Blocks per weight epoch. Weights for height h >= 1 derive from the hash
/// of the ancestor at `epoch_anchor_height(h)`.
pub const EPOCH_LEN: u64 = 32;

/// AES S-box (FIPS-197). Used only as a fixed, balanced, highly nonlinear
/// byte permutation; no AES key schedule is involved.
pub const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// A PoW parameter set. The header `version` field selects the profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub id: u32,
    pub name: &'static str,
    /// Width of the per-attempt input vector X0.
    pub d_in: usize,
    /// Hidden width H.
    pub hidden: usize,
    /// Number of H x H hidden layers.
    pub n_hidden: usize,
    /// Output width; the digest commits to d_out int32 values.
    pub d_out: usize,
    /// Requantization shift after the input layer and after hidden layers.
    pub shift_in: u32,
    pub shift_hidden: u32,
}

/// Production-candidate profile (private testnet default).
pub const FP8C_V01: Profile = Profile {
    id: 1,
    name: "zfx-fp8c-v0.1",
    d_in: 64,
    hidden: 4096,
    n_hidden: 3,
    d_out: 256,
    shift_in: 0,
    shift_hidden: 2,
};

/// Small profile for unit tests and fast local devnets. Same construction,
/// H = 256. Not for any shared network.
pub const FP8C_V01_DEV: Profile = Profile {
    id: 2,
    name: "zfx-fp8c-v0.1-dev",
    d_in: 64,
    hidden: 256,
    n_hidden: 3,
    d_out: 64,
    shift_in: 0,
    shift_hidden: 0,
};

pub const PROFILES: [Profile; 2] = [FP8C_V01, FP8C_V01_DEV];

pub fn profile_by_id(id: u32) -> Option<Profile> {
    PROFILES.iter().copied().find(|p| p.id == id)
}

impl Profile {
    /// (K, N, requant shift or None for the output layer) per layer.
    pub fn layers(&self) -> Vec<(usize, usize, Option<u32>)> {
        let mut v = vec![(self.d_in, self.hidden, Some(self.shift_in))];
        for _ in 0..self.n_hidden {
            v.push((self.hidden, self.hidden, Some(self.shift_hidden)));
        }
        v.push((self.hidden, self.d_out, None));
        v
    }

    /// Multiply-accumulates per attempt.
    pub fn macs_per_attempt(&self) -> u64 {
        self.layers().iter().map(|(k, n, _)| (*k * *n) as u64).sum()
    }

    pub fn weight_elems(&self) -> u64 {
        self.macs_per_attempt()
    }

    /// Exactness precondition: every dot product fits FP32's 24-bit
    /// significand. Checked in tests and at profile construction sites.
    pub fn exact_in_fp32(&self) -> bool {
        self.layers().iter().all(|(k, _, _)| (*k as u64) * 256 < (1 << 24))
    }
}

/// Map a uniform 5-bit index to D. 0..15 -> 1..16, 16..31 -> -1..-16.
#[inline(always)]
pub fn idx_to_val(u: u8) -> i8 {
    let u = u & 31;
    if u < 16 {
        (u + 1) as i8
    } else {
        15 - u as i8
    }
}

/// Byte to operand: low 5 bits, unbiased because 256 = 8 * 32.
#[inline(always)]
pub fn byte_to_val(b: u8) -> i8 {
    idx_to_val(b & 31)
}

/// Requantize an exact accumulator into D: S-box on bits [s, s+8) of y
/// (two's complement, arithmetic shift), top 5 bits of the S-box output.
#[inline(always)]
pub fn requant(y: i32, s: u32) -> i8 {
    let b = ((y >> s) & 0xff) as u8;
    idx_to_val(SBOX[b as usize] >> 3)
}

pub fn challenge(header: &BlockHeader) -> Hash32 {
    challenge_bytes(&header.encode_without_nonce())
}

/// PoW challenge over raw `header_without_nonce` bytes (any header version).
pub fn challenge_bytes(header_without_nonce: &[u8]) -> Hash32 {
    sha256(&[CHALLENGE_DOMAIN, header_without_nonce])
}

/// Spec 4.1 form: SHA256(challenge || uint64_le(nonce)), domain-separated.
pub fn attempt_id(challenge: &Hash32, nonce: u64) -> Hash32 {
    sha256(&[ATTEMPT_DOMAIN, challenge, &nonce.to_le_bytes()])
}

pub fn epoch_anchor_height(height: u64) -> u64 {
    assert!(height >= 1, "genesis has no PoW");
    ((height - 1) / EPOCH_LEN) * EPOCH_LEN
}

pub fn epoch_seed(anchor_hash: &Hash32, profile: &Profile) -> Hash32 {
    sha256(&[EPOCH_DOMAIN, anchor_hash, &profile.id.to_le_bytes()])
}

/// Per-epoch weights. layers[l] is row-major [N][K] (output-major, like a
/// PyTorch Linear weight), so y[j] = sum_k x[k] * w[l][j*K + k].
#[derive(Clone)]
pub struct Weights {
    pub profile: Profile,
    pub seed: Hash32,
    pub layers: Vec<Vec<i8>>,
}

pub fn weight_row(seed: &Hash32, layer: u32, row: u32, out: &mut [i8]) {
    let mut buf = vec![0u8; out.len()];
    shake256_into(
        &[WEIGHT_DOMAIN, seed, &layer.to_le_bytes(), &row.to_le_bytes()],
        &mut buf,
    );
    for (o, b) in out.iter_mut().zip(buf) {
        *o = byte_to_val(b);
    }
}

impl Weights {
    pub fn generate(profile: &Profile, seed: &Hash32) -> Weights {
        Self::generate_par(profile, seed, 1)
    }

    /// Rows are independent XOF streams, so generation parallelizes.
    pub fn generate_par(profile: &Profile, seed: &Hash32, threads: usize) -> Weights {
        let threads = threads.max(1);
        let mut layers = Vec::new();
        for (l, (k, n, _)) in profile.layers().into_iter().enumerate() {
            let mut w = vec![0i8; k * n];
            let rows_per = n.div_ceil(threads);
            std::thread::scope(|s| {
                for (t, chunk) in w.chunks_mut(rows_per * k).enumerate() {
                    let seed = *seed;
                    s.spawn(move || {
                        for (i, row) in chunk.chunks_mut(k).enumerate() {
                            weight_row(&seed, l as u32, (t * rows_per + i) as u32, row);
                        }
                    });
                }
            });
            layers.push(w);
        }
        Weights { profile: *profile, seed: *seed, layers }
    }

    pub fn layer_digest(&self, l: usize) -> Hash32 {
        let b: Vec<u8> = self.layers[l].iter().map(|v| *v as u8).collect();
        sha256(&[&b])
    }
}

pub fn x0(attempt: &Hash32, d_in: usize) -> Vec<i8> {
    let mut buf = vec![0u8; d_in];
    shake256_into(&[X0_DOMAIN, attempt], &mut buf);
    buf.into_iter().map(byte_to_val).collect()
}

#[inline]
pub fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
    debug_assert_eq!(a.len(), b.len());
    // Widening i8*i8 -> i32 with 4 independent accumulators; LLVM lowers this
    // to NEON smull/sadalp or AVX2 vpmaddwd. Exact: |sum| <= K*256 < 2^31.
    let mut acc = [0i32; 4];
    let (ca, ra) = a.split_at(a.len() / 64 * 64);
    let (cb, rb) = b.split_at(ca.len());
    for (x, y) in ca.chunks_exact(64).zip(cb.chunks_exact(64)) {
        for q in 0..4 {
            let mut s = 0i32;
            for i in 0..16 {
                s += x[q * 16 + i] as i32 * y[q * 16 + i] as i32;
            }
            acc[q] += s;
        }
    }
    let tail: i32 = ra.iter().zip(rb).map(|(x, y)| *x as i32 * *y as i32).sum();
    acc[0] + acc[1] + acc[2] + acc[3] + tail
}

/// Full trace of one attempt; used for test vectors and cross-checks.
pub struct Trace {
    pub x0: Vec<i8>,
    /// Requantized activations after each non-final layer.
    pub acts: Vec<Vec<i8>>,
    pub out: Vec<i32>,
}

pub fn forward(w: &Weights, x0v: &[i8], keep_trace: bool) -> (Vec<i32>, Vec<Vec<i8>>) {
    let mut x = x0v.to_vec();
    let mut acts = Vec::new();
    let layers = w.profile.layers();
    let mut y = Vec::new();
    for (l, (k, n, shift)) in layers.into_iter().enumerate() {
        debug_assert_eq!(x.len(), k);
        let wl = &w.layers[l];
        y.clear();
        y.extend((0..n).map(|j| dot_i8(&x, &wl[j * k..(j + 1) * k])));
        if let Some(s) = shift {
            x = y.iter().map(|v| requant(*v, s)).collect();
            if keep_trace {
                acts.push(x.clone());
            }
        }
    }
    (y, acts)
}

pub fn result_digest(challenge: &Hash32, nonce: u64, out: &[i32]) -> Hash32 {
    let mut ob = Vec::with_capacity(out.len() * 4);
    for v in out {
        ob.extend_from_slice(&v.to_le_bytes());
    }
    sha256(&[RESULT_DOMAIN, challenge, &nonce.to_le_bytes(), &ob])
}

/// Compute the PoW digest for (header-without-nonce challenge, nonce).
pub fn attempt(w: &Weights, challenge: &Hash32, nonce: u64) -> Hash32 {
    let a = attempt_id(challenge, nonce);
    let x = x0(&a, w.profile.d_in);
    let (out, _) = forward(w, &x, false);
    result_digest(challenge, nonce, &out)
}

pub fn trace(w: &Weights, challenge: &Hash32, nonce: u64) -> (Trace, Hash32) {
    let a = attempt_id(challenge, nonce);
    let x = x0(&a, w.profile.d_in);
    let (out, acts) = forward(w, &x, true);
    let d = result_digest(challenge, nonce, &out);
    (Trace { x0: x, acts, out }, d)
}

pub fn meets_target(digest: &Hash32, target: &U256) -> bool {
    U256::from_be_bytes(digest) < *target
}

#[derive(Debug, PartialEq, Eq)]
pub enum PowError {
    UnknownProfile(u32),
    SeedMismatch,
    AboveTarget,
}

/// Verify a header's PoW given the epoch weights. The caller must supply
/// weights generated from the correct epoch anchor; this is re-checked.
pub fn verify_header_pow(header: &BlockHeader, w: &Weights, anchor_hash: &Hash32) -> Result<Hash32, PowError> {
    let p = profile_by_id(header.version).ok_or(PowError::UnknownProfile(header.version))?;
    if w.profile != p || w.seed != epoch_seed(anchor_hash, &p) {
        return Err(PowError::SeedMismatch);
    }
    let c = challenge(header);
    let d = attempt(w, &c, header.nonce);
    if meets_target(&d, &header.target_u256()) {
        Ok(d)
    } else {
        Err(PowError::AboveTarget)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_is_balanced_and_exact() {
        let mut counts = std::collections::HashMap::new();
        for b in 0..=255u8 {
            *counts.entry(byte_to_val(b)).or_insert(0) += 1;
        }
        assert_eq!(counts.len(), 32);
        assert!(counts.values().all(|c| *c == 8));
        assert!(!counts.contains_key(&0));
        for p in PROFILES {
            assert!(p.exact_in_fp32(), "{}", p.name);
        }
    }

    #[test]
    fn requant_balanced() {
        // The S-box is a permutation, so each of the 32 values gets 8 bytes.
        let mut counts = std::collections::HashMap::new();
        for b in 0..=255i32 {
            *counts.entry(requant(b, 0)).or_insert(0) += 1;
        }
        assert_eq!(counts.len(), 32);
        assert!(counts.values().all(|c| *c == 8));
        // Arithmetic shift on negatives.
        assert_eq!(requant(-1, 2), requant(0xff, 0));
    }

    #[test]
    fn fp32_accumulation_is_order_independent() {
        let w = Weights::generate(&FP8C_V01_DEV, &[3u8; 32]);
        let x = x0(&[9u8; 32], 64);
        let (k, n, _) = FP8C_V01_DEV.layers()[0];
        for j in 0..n {
            let row = &w.layers[0][j * k..(j + 1) * k];
            let exact = dot_i8(&x, row);
            let fwd: f32 = x.iter().zip(row).fold(0f32, |a, (p, q)| a + (*p as f32) * (*q as f32));
            let rev: f32 = x.iter().zip(row).rev().fold(0f32, |a, (p, q)| a + (*p as f32) * (*q as f32));
            assert_eq!(fwd, exact as f32);
            assert_eq!(rev, exact as f32);
        }
    }

    #[test]
    fn deterministic_and_nonce_bound() {
        let w = Weights::generate(&FP8C_V01_DEV, &[1u8; 32]);
        let wp = Weights::generate_par(&FP8C_V01_DEV, &[1u8; 32], 4);
        assert_eq!(w.layers, wp.layers);
        let c = [5u8; 32];
        assert_eq!(attempt(&w, &c, 1), attempt(&w, &c, 1));
        assert_ne!(attempt(&w, &c, 1), attempt(&w, &c, 2));
        assert_ne!(attempt(&w, &c, 1), attempt(&w, &[6u8; 32], 1));
    }

    #[test]
    fn epoch_anchor() {
        assert_eq!(epoch_anchor_height(1), 0);
        assert_eq!(epoch_anchor_height(32), 0);
        assert_eq!(epoch_anchor_height(33), 32);
    }
}
