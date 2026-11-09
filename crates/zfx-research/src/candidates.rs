//! Candidates A, B, D as fully specified single-GEMM PoW functions, plus
//! the Ozaki-style exact int8 emulation of candidate A. Candidate C lives
//! in transcript.rs; candidate E is zfx_verify::pow (the selected design).

use crate::arith::*;
use zfx_verify::hash::{sha256, Hash32};
use zfx_verify::pow::byte_to_val;
use sha2::Digest;

pub fn xof_bytes(domain: &[u8], seed: &Hash32, len: usize) -> Vec<u8> {
    zfx_verify::hash::shake256(&[domain, seed], len)
}

/// Candidate A input: 2*N*N finite E4M3 bytes, rejection-sampled (0x7f,
/// 0xff NaN and 0x80 negative zero rejected => 253 equiprobable values).
pub fn gen_a(attempt: &Hash32, n: usize) -> (Vec<u8>, Vec<u8>) {
    let need = 2 * n * n;
    // Expected rejection rate 3/256; oversample and extend if needed.
    let mut stream = xof_bytes(b"ZFX-A-in-v0.1", attempt, need + need / 32 + 64);
    let mut out = Vec::with_capacity(need);
    let mut extra = 0u32;
    loop {
        for &b in &stream {
            if b != 0x7f && b != 0xff && b != 0x80 {
                out.push(b);
                if out.len() == need {
                    let bv = out.split_off(n * n);
                    return (out, bv);
                }
            }
        }
        extra += 1;
        let s2 = sha256(&[attempt, &extra.to_le_bytes()]);
        stream = xof_bytes(b"ZFX-A-in-v0.1", &s2, need / 16 + 64);
    }
}

/// Candidate B input: windowed BF16 (exponent in [-4,3]).
pub fn gen_b(attempt: &Hash32, n: usize) -> (Vec<i64>, Vec<i64>) {
    let s = xof_bytes(b"ZFX-B-in-v0.1", attempt, 4 * n * n);
    let v: Vec<i64> = s.chunks(2).map(|c| bf16w_from_byte_pair(c[0], c[1]).1).collect();
    let (a, b) = v.split_at(n * n);
    (a.to_vec(), b.to_vec())
}

/// Candidate D input: restricted domain {+-1..+-16}.
pub fn gen_d(attempt: &Hash32, n: usize) -> (Vec<i8>, Vec<i8>) {
    let s = xof_bytes(b"ZFX-D-in-v0.1", attempt, 2 * n * n);
    let v: Vec<i8> = s.into_iter().map(byte_to_val).collect();
    let (a, b) = v.split_at(n * n);
    (a.to_vec(), b.to_vec())
}

/// Row-major A (n x n) times row-major B (n x n), exact, producing for each
/// output the f32 result under accumulation model `acc`. `scale_log2` is
/// the fixed-point scale of each product.
pub fn gemm_model(a: &[i64], b: &[i64], n: usize, scale_log2: i32, acc: Accum) -> Vec<f32> {
    let mut bt = vec![0i64; n * n];
    for i in 0..n {
        for j in 0..n {
            bt[j * n + i] = b[i * n + j];
        }
    }
    let mut out = vec![0f32; n * n];
    let threads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4);
    let rows_per = n.div_ceil(threads);
    std::thread::scope(|s| {
        for (t, chunk) in out.chunks_mut(rows_per * n).enumerate() {
            let a = &a;
            let bt = &bt;
            s.spawn(move || {
                let mut p = vec![0i128; n];
                for (ri, row) in chunk.chunks_mut(n).enumerate() {
                    let i = t * rows_per + ri;
                    for j in 0..n {
                        for k in 0..n {
                            p[k] = a[i * n + k] as i128 * bt[j * n + k] as i128;
                        }
                        row[j] = reduce(&p, scale_log2, acc);
                    }
                }
            });
        }
    });
    out
}

/// Exact int32 GEMM for restricted-domain candidates (fast path).
pub fn gemm_i8_exact(a: &[i8], b: &[i8], n: usize) -> Vec<i32> {
    let mut bt = vec![0i8; n * n];
    for i in 0..n {
        for j in 0..n {
            bt[j * n + i] = b[i * n + j];
        }
    }
    let mut out = vec![0i32; n * n];
    let threads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4);
    let rows_per = n.div_ceil(threads);
    std::thread::scope(|s| {
        for (t, chunk) in out.chunks_mut(rows_per * n).enumerate() {
            let a = &a;
            let bt = &bt;
            s.spawn(move || {
                for (ri, row) in chunk.chunks_mut(n).enumerate() {
                    let i = t * rows_per + ri;
                    for j in 0..n {
                        row[j] = zfx_verify::pow::dot_i8(&a[i * n..(i + 1) * n], &bt[j * n..(j + 1) * n]);
                    }
                }
            });
        }
    });
    out
}

/// Exact canonical candidate-A GEMM via int64 fixed point (CPU reference).
pub fn gemm_a_canonical(a: &[u8], b: &[u8], n: usize) -> Vec<f32> {
    let af: Vec<i64> = a.iter().map(|x| e4m3_to_fixed9(*x).unwrap()).collect();
    let bf: Vec<i64> = b.iter().map(|x| e4m3_to_fixed9(*x).unwrap()).collect();
    let mut bt = vec![0i64; n * n];
    for i in 0..n {
        for j in 0..n {
            bt[j * n + i] = bf[i * n + j];
        }
    }
    let mut out = vec![0f32; n * n];
    let threads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4);
    let rows_per = n.div_ceil(threads);
    std::thread::scope(|s| {
        for (t, chunk) in out.chunks_mut(rows_per * n).enumerate() {
            let af = &af;
            let bt = &bt;
            s.spawn(move || {
                for (ri, row) in chunk.chunks_mut(n).enumerate() {
                    let i = t * rows_per + ri;
                    let ar = &af[i * n..(i + 1) * n];
                    for j in 0..n {
                        let br = &bt[j * n..(j + 1) * n];
                        let s: i64 = ar.iter().zip(br).map(|(x, y)| x * y).sum();
                        row[j] = fixed_to_f32_rne(s as i128, 18);
                    }
                }
            });
        }
    });
    out
}

/// Ozaki-style exact emulation of candidate A on int8 hardware: split each
/// fixed9 operand (|v| <= 229376 < 2^18) into three signed 7-bit slices,
/// run 9 int8 x int8 -> int32 GEMMs, recombine in int64, round once.
/// Returns (result, number_of_int8_gemms).
pub fn gemm_a_ozaki(a: &[u8], b: &[u8], n: usize) -> (Vec<f32>, usize) {
    let slices = |v: &[u8]| -> [Vec<i8>; 3] {
        let mut s = [vec![0i8; v.len()], vec![0i8; v.len()], vec![0i8; v.len()]];
        for (idx, x) in v.iter().enumerate() {
            let f = e4m3_to_fixed9(*x).unwrap();
            let sg = f.signum();
            let m = f.abs();
            for (q, sl) in s.iter_mut().enumerate() {
                sl[idx] = (sg * ((m >> (7 * q)) & 0x7f)) as i8;
            }
        }
        s
    };
    let sa = slices(a);
    let sb = slices(b);
    let mut acc = vec![0i64; n * n];
    let mut gemms = 0;
    for (p, ap) in sa.iter().enumerate() {
        for (q, bq) in sb.iter().enumerate() {
            let c = gemm_i8_exact(ap, bq, n);
            gemms += 1;
            let sh = 7 * (p + q) as u32;
            for (o, v) in acc.iter_mut().zip(c) {
                *o += (v as i64) << sh;
            }
        }
    }
    (acc.into_iter().map(|v| fixed_to_f32_rne(v as i128, 18)).collect(), gemms)
}

pub fn digest_f32(domain: &[u8], attempt: &Hash32, c: &[f32]) -> Hash32 {
    let mut h = sha2::Sha256::new();
    h.update(domain);
    h.update(attempt);
    for v in c {
        h.update(v.to_bits().to_le_bytes());
    }
    h.finalize().into()
}

pub fn digest_i32(domain: &[u8], attempt: &Hash32, c: &[i32]) -> Hash32 {
    let mut h = sha2::Sha256::new();
    h.update(domain);
    h.update(attempt);
    for v in c {
        h.update(v.to_le_bytes());
    }
    h.finalize().into()
}
