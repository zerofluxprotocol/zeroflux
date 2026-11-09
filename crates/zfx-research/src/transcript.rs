//! Candidate C: FP8 (restricted-domain) GEMM with canonical tiled
//! transcript commitments.
//!
//! C1 "Merkle + sampled openings": the miner commits to every output tile's
//!    K-chunk partial-sum transcript in a Merkle tree; the lottery is
//!    H(challenge || nonce || root) < target; s tiles chosen by H(root) are
//!    opened and recomputed by the verifier.
//! C2 "tile tickets" (Pearl-style): each tile's transcript hash is its own
//!    lottery ticket; the proof is (nonce, tile index) and the verifier
//!    recomputes exactly one tile.
//!
//! Inputs are row-addressable XOF streams so a verifier can regenerate only
//! the rows/columns a tile touches.

use zfx_verify::hash::{sha256, shake256_into, Hash32};
use zfx_verify::pow::{byte_to_val, dot_i8};
use zfx_verify::U256;

#[derive(Clone, Copy, Debug)]
pub struct CParams {
    pub n: usize,
    pub tile: usize,
    pub kchunk: usize,
    pub samples: usize,
}

pub fn row(attempt: &Hash32, which: u8, r: usize, n: usize) -> Vec<i8> {
    let mut buf = vec![0u8; n];
    shake256_into(&[b"ZFX-C-in-v0.1", attempt, &[which], &(r as u32).to_le_bytes()], &mut buf);
    buf.into_iter().map(byte_to_val).collect()
}

/// A rows and B columns (as rows of B^T).
pub fn inputs(attempt: &Hash32, n: usize) -> (Vec<Vec<i8>>, Vec<Vec<i8>>) {
    ((0..n).map(|r| row(attempt, b'A', r, n)).collect(), (0..n).map(|c| row(attempt, b'B', c, n)).collect())
}

/// Transcript of tile (ti, tj): every K-chunk running accumulator, i32 LE,
/// row-major within the tile, chunk-major overall; hashed into a leaf.
pub fn tile_leaf(p: &CParams, a: &[Vec<i8>], bt: &[Vec<i8>], ti: usize, tj: usize) -> Hash32 {
    let t = p.tile;
    let mut acc = vec![0i32; t * t];
    let mut h = Vec::with_capacity((p.n / p.kchunk) * t * t * 4 + 16);
    h.extend_from_slice(&(ti as u32).to_le_bytes());
    h.extend_from_slice(&(tj as u32).to_le_bytes());
    for k0 in (0..p.n).step_by(p.kchunk) {
        for r in 0..t {
            for c in 0..t {
                acc[r * t + c] += dot_i8(&a[ti * t + r][k0..k0 + p.kchunk], &bt[tj * t + c][k0..k0 + p.kchunk]);
            }
        }
        for v in &acc {
            h.extend_from_slice(&v.to_le_bytes());
        }
    }
    sha256(&[b"ZFX-C-leaf-v0.1", &h])
}

pub fn merkle_levels(leaves: &[Hash32]) -> Vec<Vec<Hash32>> {
    let mut levels = vec![leaves.to_vec()];
    while levels.last().unwrap().len() > 1 {
        let l = levels.last().unwrap();
        levels.push(l.chunks(2).map(|c| sha256(&[b"ZFX-C-node", &c[0], c.get(1).unwrap_or(&c[0])])).collect());
    }
    levels
}

pub fn merkle_path(levels: &[Vec<Hash32>], mut idx: usize) -> Vec<Hash32> {
    let mut path = Vec::new();
    for l in &levels[..levels.len() - 1] {
        let sib = idx ^ 1;
        path.push(*l.get(sib).unwrap_or(&l[idx]));
        idx >>= 1;
    }
    path
}

pub fn merkle_verify(root: &Hash32, leaf: &Hash32, mut idx: usize, path: &[Hash32]) -> bool {
    let mut h = *leaf;
    for s in path {
        h = if idx & 1 == 0 { sha256(&[b"ZFX-C-node", &h, s]) } else { sha256(&[b"ZFX-C-node", s, &h]) };
        idx >>= 1;
    }
    &h == root
}

pub fn sample_indices(root: &Hash32, nonce: u64, samples: usize, ntiles: usize) -> Vec<usize> {
    (0..samples)
        .map(|i| {
            let h = sha256(&[b"ZFX-C-sample", root, &nonce.to_le_bytes(), &(i as u32).to_le_bytes()]);
            (u64::from_le_bytes(h[..8].try_into().unwrap()) % ntiles as u64) as usize
        })
        .collect()
}

pub struct C1Proof {
    pub nonce: u64,
    pub root: Hash32,
    pub openings: Vec<(usize, Hash32, Vec<Hash32>)>,
}

impl C1Proof {
    pub fn size_bytes(&self) -> usize {
        8 + 32 + self.openings.iter().map(|(_, _, p)| 4 + 32 + 32 * p.len()).sum::<usize>()
    }
}

pub fn c1_lottery(challenge: &Hash32, nonce: u64, root: &Hash32) -> Hash32 {
    sha256(&[b"ZFX-C1-lottery", challenge, &nonce.to_le_bytes(), root])
}

pub fn attempt_of(challenge: &Hash32, nonce: u64) -> Hash32 {
    sha256(&[b"ZFX-C-attempt", challenge, &nonce.to_le_bytes()])
}

/// Honest C1 attempt: compute every tile.
pub fn c1_honest(p: &CParams, challenge: &Hash32, nonce: u64) -> (Hash32, Vec<Vec<Hash32>>) {
    let at = attempt_of(challenge, nonce);
    let (a, bt) = inputs(&at, p.n);
    let nt = p.n / p.tile;
    let leaves: Vec<Hash32> = (0..nt * nt).map(|x| tile_leaf(p, &a, &bt, x / nt, x % nt)).collect();
    let levels = merkle_levels(&leaves);
    (c1_lottery(challenge, nonce, levels.last().unwrap().first().unwrap()), levels)
}

pub fn c1_make_proof(p: &CParams, nonce: u64, levels: &[Vec<Hash32>]) -> C1Proof {
    let root = levels.last().unwrap()[0];
    let nt = p.n / p.tile;
    let idx = sample_indices(&root, nonce, p.samples, nt * nt);
    C1Proof { nonce, root, openings: idx.into_iter().map(|i| (i, levels[0][i], merkle_path(levels, i))).collect() }
}

/// Verifier: lottery, sample positions, Merkle paths, and recomputation of
/// each sampled tile from regenerated rows/columns only.
/// Returns (ok, verifier MACs).
pub fn c1_verify(p: &CParams, challenge: &Hash32, target: &U256, pr: &C1Proof) -> (bool, u64) {
    if U256::from_be_bytes(&c1_lottery(challenge, pr.nonce, &pr.root)) >= *target {
        return (false, 0);
    }
    let nt = p.n / p.tile;
    let want = sample_indices(&pr.root, pr.nonce, p.samples, nt * nt);
    if want.len() != pr.openings.len() {
        return (false, 0);
    }
    let at = attempt_of(challenge, pr.nonce);
    let mut macs = 0u64;
    for (w, (i, leaf, path)) in want.iter().zip(&pr.openings) {
        if w != i || !merkle_verify(&pr.root, leaf, *i, path) {
            return (false, macs);
        }
        let (ti, tj) = (i / nt, i % nt);
        let a: Vec<Vec<i8>> = (0..p.tile).map(|r| row(&at, b'A', ti * p.tile + r, p.n)).collect();
        let bt: Vec<Vec<i8>> = (0..p.tile).map(|c| row(&at, b'B', tj * p.tile + c, p.n)).collect();
        // Re-index into a sparse full-size view for tile_leaf.
        let mut af = vec![Vec::new(); p.n];
        let mut bf = vec![Vec::new(); p.n];
        for r in 0..p.tile {
            af[ti * p.tile + r] = a[r].clone();
            bf[tj * p.tile + r] = bt[r].clone();
        }
        macs += (p.tile * p.tile * p.n) as u64;
        if &tile_leaf(p, &af, &bf, ti, tj) != leaf {
            return (false, macs);
        }
    }
    (true, macs)
}

/// Root-grinding forgery against C1. The attacker computes only the tiles
/// in `honest_tiles` (a fraction f of all tiles), fills the remaining
/// leaves with junk, and grinds one junk leaf's counter: each try costs
/// one Merkle path update (log2 T hashes) and zero GEMM work. A try wins
/// if the lottery passes AND all s sampled tiles are honest ones.
/// Returns (proof, root tries, hash calls).
pub fn c1_grind_attack(
    p: &CParams,
    challenge: &Hash32,
    target: &U256,
    nonce: u64,
    honest_frac: f64,
    max_tries: u64,
) -> (Option<C1Proof>, u64, u64) {
    let at = attempt_of(challenge, nonce);
    let nt = p.n / p.tile;
    let ntiles = nt * nt;
    let n_honest = ((ntiles as f64) * honest_frac).round() as usize;
    // Only rows/cols for the first n_honest tiles are needed; generate all
    // for simplicity, but count MACs only for computed tiles.
    let (a, bt) = inputs(&at, p.n);
    let mut leaves: Vec<Hash32> = (0..ntiles)
        .map(|x| if x < n_honest { tile_leaf(p, &a, &bt, x / nt, x % nt) } else { sha256(&[b"junk", &(x as u64).to_le_bytes()]) })
        .collect();
    let grind_leaf = ntiles - 1;
    let mut hashes = 0u64;
    for ctr in 0..max_tries {
        leaves[grind_leaf] = sha256(&[b"junk-grind", &ctr.to_le_bytes()]);
        let levels = merkle_levels(&leaves); // attacker could update one path; we count log2 T
        hashes += 1 + (ntiles as f64).log2().ceil() as u64 + 1;
        let root = levels.last().unwrap()[0];
        if U256::from_be_bytes(&c1_lottery(challenge, nonce, &root)) >= *target {
            continue;
        }
        let idx = sample_indices(&root, nonce, p.samples, ntiles);
        hashes += p.samples as u64;
        if idx.iter().all(|i| *i < n_honest) {
            return (Some(c1_make_proof(p, nonce, &levels)), ctr + 1, hashes);
        }
    }
    (None, max_tries, hashes)
}

/// "Lottery-first" variant C1' (lottery on H(challenge||nonce) only, then
/// open tiles): the attacker evaluates the lottery for free and computes
/// only the opened tiles of winners. Returns the fraction of honest GEMM
/// work the attacker performed per accepted proof.
pub fn c1_lottery_first_work_ratio(p: &CParams) -> f64 {
    let nt = p.n / p.tile;
    p.samples as f64 / (nt * nt) as f64
}

/// C2 tile ticket: H(challenge || nonce || ti || tj || leaf) per tile.
pub fn c2_ticket(challenge: &Hash32, nonce: u64, ti: usize, tj: usize, leaf: &Hash32) -> Hash32 {
    sha256(&[b"ZFX-C2-ticket", challenge, &nonce.to_le_bytes(), &(ti as u32).to_le_bytes(), &(tj as u32).to_le_bytes(), leaf])
}

/// C2 verification: regenerate one tile's rows/cols and recompute.
pub fn c2_verify(p: &CParams, challenge: &Hash32, target: &U256, nonce: u64, ti: usize, tj: usize) -> (bool, u64, u64) {
    let at = attempt_of(challenge, nonce);
    let mut af = vec![Vec::new(); p.n];
    let mut bf = vec![Vec::new(); p.n];
    for r in 0..p.tile {
        af[ti * p.tile + r] = row(&at, b'A', ti * p.tile + r, p.n);
        bf[tj * p.tile + r] = row(&at, b'B', tj * p.tile + r, p.n);
    }
    let leaf = tile_leaf(p, &af, &bf, ti, tj);
    let ok = U256::from_be_bytes(&c2_ticket(challenge, nonce, ti, tj, &leaf)) < *target;
    (ok, (p.tile * p.tile * p.n) as u64, (2 * p.tile * p.n) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn c1_honest_roundtrip_and_grind_attack_succeeds() {
        let p = CParams { n: 64, tile: 16, kchunk: 16, samples: 2 };
        let ch = [7u8; 32];
        let target = U256::MAX.shr(2); // p = 1/4
        let mut found = None;
        for nonce in 0..64 {
            let (lot, levels) = c1_honest(&p, &ch, nonce);
            if U256::from_be_bytes(&lot) < target {
                found = Some(c1_make_proof(&p, nonce, &levels));
                break;
            }
        }
        let pr = found.unwrap();
        assert!(c1_verify(&p, &ch, &target, &pr).0);
        // attacker computes half the tiles; with s=2 the pass rate is 1/4 * 1/4
        let (forged, _tries, _h) = c1_grind_attack(&p, &ch, &target, 999, 0.5, 10_000);
        let forged = forged.expect("grinding should find a forgery");
        assert!(c1_verify(&p, &ch, &target, &forged).0, "verifier accepts forged C1 proof");
    }
}
