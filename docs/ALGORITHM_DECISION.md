# Algorithm decision: ZFX-FP8C v0.1

Decision date: 2026-11-09. Spec: `docs/SPEC_v0.1.md` section 2A plus `docs/SPEC_AMENDMENT_A.md`.

**Selected:** candidate E, ZFX-FP8C. Each attempt pushes one row through a chain of restricted-domain FP8 GEMM layers, with per-epoch random weights and S-box requantization between layers.

**Security readiness:** `MAINNET_READY = false`. `PRIVATE_TESTNET_READY = true`.

Every number below comes from `bench/results/*.json`, produced by `zfx-research all` and `bench/run_benchmarks.sh` on an Apple M5 Pro (18 CPU threads, integrated GPU). No NVIDIA or AMD GPU was available. Every benchmark on those vendors is therefore `UNSUPPORTED`, and no number here is extrapolated from one device to another.

## 1. What was tried

Spec 2A required candidates A to D. I added E after the D measurements showed a structural problem that none of A to D can fix.

| id | construction | where |
|---|---|---|
| A | FP8 E4M3 unrestricted operands (253 finite values, NaN and -0 rejected), N=1024 square GEMM. Canonical output: exact sum, then one RNE rounding to FP32. | `crates/zfx-research/src/candidates.rs` |
| B | BF16 with exponent window [-4, 3], the same canonical rule. | same |
| C1 | Restricted-domain GEMM, Merkle commitment over every tile's K-chunk partial-sum transcript, lottery on the root, s post-commit sampled tile openings. | `transcript.rs` |
| C1' | Same commitment, lottery on `H(challenge, nonce)`, then openings. | `transcript.rs` |
| C2 | Pearl-style tile tickets. Each tile's transcript hash is its own lottery ticket, and the verifier recomputes one tile. | `transcript.rs` |
| D | Restricted domain {±1..±16}, single N=1024 GEMM, both operands fresh per nonce. | `candidates.rs` |
| E | D's domain, layered: 64 -> 4096 -> 4096 -> 4096 -> 4096 -> 256, weights per 32-block epoch, nonlinear S-box requantization. | `crates/zfx-verify/src/pow.rs` |

## 2. Gate 1: correctness (bit-exact across implementations)

I ran each candidate's real inputs through seven accumulation models of what hardware and libraries do: sequential, reversed, pairwise tree, split-K 4, FP16 accumulate, and a truncating-alignment model of an FP8 MMA accumulator that keeps 14 bits (the behavior DeepSeek-V3 reports for H800). The test is whether the output still matches the canonical bits. N=256, so 65,536 outputs per candidate (`divergence.json`).

| model | A: FP8 unrestricted | B: BF16 window | D/E: restricted |
|---|---|---|---|
| sequential FP32 | 82.0% of outputs differ | 83.6% | 0% |
| reversed FP32 | 81.8% | 83.8% | 0% |
| pairwise FP32 | 63.3% | 64.2% | 0% |
| split-K 4 | 71.7% | 73.6% | 0% |
| truncating 14-bit FP8 MMA | 99.97% | 99.98% | 0% at K=256; 3 of 512 rows at K=4096 |
| FP16 accumulate | 100% | 100% | 26% (rejected path) |

Readings:

- A and B fail Gate 1 on every native GEMM path. A miner cannot use cuBLAS, hipBLASLt or any tensor-core FP32 accumulator and get consensus bits. A canonical result is reachable only by emulation. The Ozaki-style split into three signed 7-bit slices gives 9 int8 GEMMs with exact int32 accumulation, and that output is bit-exact against the canonical reference (`ozaki_int8_emulation_of_A`). So A costs 9x the int8 GEMM work of D/E for the same MAC count. B needs the same treatment.
- D and E are reduction-order independent by construction (`|sum| < 2^24`), and the models confirm it. The one hazard is a reduced-precision accumulator, which diverges on the rare rows where `|partial| > 2^14`. At K=4096 that was 0.6% of rows. A random self-test would miss it, so the published vectors and `zpu_miner.py stress` force `|sum| = 2^20` rows, and miners must refuse any backend that fails them.
- Measured cross-implementation agreement for E: Rust (`zpu-cli`), C++ (`zpu-miner`), Python NumPy, PyTorch CPU, PyTorch MPS (Apple GPU) and MLX/Metal (Apple GPU) all pass the 278 published checks plus the 512x4096x4096 max-magnitude stress test with 0 mismatches (`miners.jsonl`). NVIDIA and AMD: `UNSUPPORTED` (no hardware). Device code for all six kernels compiles for sm_90, sm_100, sm_120, gfx942 and gfx950 (`device_check.json`).

## 3. Gate 2: security (forgeries and shortcuts)

| attack | target | result | evidence |
|---|---|---|---|
| Root grinding (junk in unopened leaves, grind one junk leaf) | C1 | **SUCCEEDS.** The verifier accepted forged proofs after computing 75%, 50% and 25% of tiles. At f=0.25 it took 922 root tries (7,804 SHA-256 calls) and 25% of the GEMM work. Cost per extra forged proof is hashes only. | `transcript.json` |
| Lottery-first then open | C1' | **SUCCEEDS.** The attacker computes 4 of 64 tiles (6.25% of the work) for winning nonces only. | `transcript.json` |
| Low-rank / factorization | D, E | FAILS. Weight blocks are full rank mod p (256/256, 384/384). | `attacks.json: low_rank` |
| Sparsity / structured inputs | all | FAILS. The domain has no zero, and inputs come from SHAKE256 and are not miner-controlled. | `reuse_*` |
| Precomputation / nonce reuse | D, E | FAILS. Activations of different nonces agree at the 1/32 chance rate. | `reuse_*` |
| GF(2) affine shortcut on output parity | E with naive mod-32 requant | **SUCCEEDS**: 100% of output parities predicted with no integer GEMM. | `parity_dev` |
| same | E with S-box requant (shipped) | FAILS: 49.1% (chance). | `parity_dev` |
| Partial / approximate computation | E | FAILS. One +1 error in one layer-0 accumulator changes 97.5% of outputs and 125.9/256 digest bits. | `avalanche_dev` |
| Strassen (exact fast matmul) | A, D, E | Valid algorithm, not a forgery. Int8 hardware fits 3 levels (0.67 of the MACs, up to 1.49x). FP8 E4M3 fits 0 levels because pairwise sums need 5 significant bits. | `strassen` |
| Four Russians / table lookup with fixed weights | E | FAILS in practice. g=2 halves additions but needs a 32 GiB table per 4096x4096 layer, streamed for every attempt (memory-bound). | `four_russians` |
| Difficulty control | E | Acceptance 12,663 of 200,000 at p=1/16 (expected 12,500, z=1.51). First-byte chi² = 215 on 255 dof. | `acceptance_dev` |

C1 and C1' are disqualified: a cheap proof-forging path disqualifies a candidate "regardless of throughput" (spec 2A). The flaw is structural for any sampled-opening scheme whose lottery input depends on miner-chosen unopened data, or whose lottery runs before the work. C2 avoids it, because every tile is its own ticket and the verifier recomputes the ticket's whole tile, but C2 then inherits D's input-generation problem (section 5).

## 4. Gate 3: verification cost

| candidate | verifier work per block | CPU verify latency (1 thread) | proof size | notes |
|---|---|---|---|---|
| A | full 1024³ GEMM, exact int64 | 34 ms (18 threads) | nonce only | 9x int8 GEMMs if a GPU emulates it |
| B | full GEMM | (N=256: 2 ms) | nonce only | |
| C1/C1' | s tiles | 0.9 ms | 952 B | disqualified |
| C2 | 1 tile (262k MACs at n=256, t=32) | under 1 ms | 16 B | |
| D | full 1024³ GEMM + 2 MiB SHAKE | 9.6 ms (18 threads) | nonce only | |
| **E** | **one attempt: 51.6 M MACs + 64 B SHAKE + 1 KiB SHA-256** | **0.985 ms** | **nonce only (header)** | weights: 103 ms single-thread or 7.8 ms on 18 threads, once per 32-block epoch |

E's verifier does exactly one attempt's work, which is the hashcash asymmetry Bitcoin has: mining costs `1/p` attempts and verifying costs one. A dedicated GPU runs millions of attempts per second against a 1 ms CPU check. Validation of 1,000,000 headers costs about 1,000 CPU-seconds, parallel across cores. The open questions are DoS economics (an invalid block costs a peer 1 ms before the ban) and initial sync, both listed as blockers below. No recursive or succinct proof was adopted. Recomputing 1 ms of integer arithmetic is cheaper to verify, and easier to audit, than any SNARK verifier would be.

## 5. Gate 4: GPU efficiency, and why E exists

The key measurement for D: **input generation per MAC.** Fresh `A, B` per nonce cost `2N²` SHAKE bytes for `N³` MACs, which is 512 MACs per generated byte at N=1024. On the M5 CPU, generation was already 39% of D's attempt time (3.7 ms of 9.6 ms). A GPU tensor core running 10^15 MAC/s would need about 2 TB/s of SHAKE256 output to keep up. No GPU produces Keccak at that rate, so D on a GPU would be a SHAKE256 PoW with a matmul attached. A and B have the same ratio. C2 has it per ticket. Pearl's fix is low-rank noise on a fixed base matrix. That cuts generation, but it replaces "dense random matmul" with an algebraic hardness assumption ("batch low-rank random linear equations", per Pearl's own write-up), which ZeroFlux does not need because it makes no usefulness claim.

E removes the problem by amortization plus nonlinearity:

- Weights (51.6 M values) are generated once per 32-block epoch and shared by every attempt.
- Per attempt, only 64 input bytes are generated, so **806,912 MACs per generated byte**, about 1,576x D's ratio.
- The S-box requantization between layers blocks the shortcut that makes fixed weights dangerous. Without it, `X·W1·W2·W3` collapses to one precomputed matrix. With it, there is no algebraic composition, and the parity experiment shows the naive modular alternative is not enough.
- Batching nonces makes each layer a plain `[batch x 4096] x [4096 x 4096]` GEMM. That is the shape of a transformer MLP forward pass at inference, and every vendor BLAS runs it near peak.

Measured attempts/s on the v0.1 profile (`miners.jsonl`):

| backend | device | attempts/s | TMAC/s |
|---|---|---|---|
| MLX / Metal fp32 | Apple M5 Pro GPU | 80,131 | 4.14 |
| PyTorch MPS fp32 | Apple M5 Pro GPU | 42,351 | 2.19 |
| NumPy / Accelerate fp32 | M5 CPU (AMX) | 7,735 | 0.40 |
| Rust `zpu-cli`, 18 threads | M5 CPU | 3,293 | 0.17 |
| C++ `zpu-miner`, 18 threads | M5 CPU | 3,047 | 0.16 |
| CUDA / ROCm (int8, FP8) | H100, B200, RTX PRO 6000, MI300X, MI355X | UNSUPPORTED | no hardware |

Even on a laptop the integrated GPU beats every CPU path by 10 to 25x, which is the direction the design targets. Joules per attempt are `BLOCKED`: `powermetrics` needs root, and there is no datacenter GPU to measure. No cross-vendor efficiency claim is made.

## 6. Gate 5: implementation complexity

| | A | B | C1/C1' | C2 | D | E |
|---|---|---|---|---|---|---|
| native vendor GEMM usable for canonical bits | no (9x emulation) | no | yes | yes | yes | yes |
| kernel portability (NVIDIA + AMD + Apple + CPU) | poor | poor | good | good | good | good: int8 BLAS, FP8 Lt (E4M3 / E4M3FNUZ), fp32 everywhere |
| new hardness assumption | none | none | broken | low-rank noise if amortized | none | nonlinear chain (S-box) |

## 7. Scores and selection

Gates are applied in spec order. A gate failure ends the evaluation for production use.

| candidate | 1 correctness | 2 security | 3 verification | 4 efficiency (verified work/J proxy) | 5 complexity | outcome |
|---|---|---|---|---|---|---|
| A | FAIL natively; PASS only via 9x emulation | no known forgery | full GEMM, 34 ms | 1/9 of D/E per MAC, and generation-bound | high | rejected |
| B | FAIL natively | no known forgery | full GEMM | generation-bound, emulation-bound | high | rejected |
| C1 | PASS | **FAIL (forgery)** | 0.9 ms | n/a | medium | disqualified |
| C1' | PASS | **FAIL (forgery)** | n/a | n/a | medium | disqualified |
| C2 | PASS | no known forgery | 1 tile, 16 B proof | generation-bound (2N² bytes per nonce) unless Pearl-style low-rank noise is added | medium | runner-up |
| D | PASS | no known forgery | full GEMM, 9.6 ms | generation-bound: 512 MAC/byte | low | rejected on Gate 4 |
| **E** | **PASS** (6 backends, 3 implementations, Apple GPU) | **no known forgery**; parity and avalanche checked | **0.985 ms, nonce-only proof** | **806,912 MAC/byte**; GPU 10 to 25x CPU on this host | low to medium | **selected** |

E is the only candidate that passes Gates 1 to 3 and is compute-bound on tensor-core hardware. The runner-up, C2, is the fallback if review finds a structural flaw in the S-box chain. Its generation cost would then need Pearl-style noise, together with that noise's hardness assumption.

## 8. Frozen parameters (private testnet)

Profile 1 (`zfx-fp8c-v0.1`): d_in 64, H 4096, 3 hidden layers, d_out 256, shifts 0 and 2, epoch length 32 blocks, LWMA N = 30, testnet target spacing 30 s, `MAINNET_READY = false`. The full byte-level rules are in CONSENSUS.md.

## 9. Blockers to MAINNET_READY

See RELEASE_READINESS.md for owners. In short: no measurement on NVIDIA or AMD silicon, no external cryptanalysis of the S-box-chain hardness assumption, no DoS/initial-sync economics study, no P2P transport encryption (it must use ML-KEM when added), an unaudited SLH-DSA integration, and no economic or legal review.

## Sign-off

Selected and implemented by the ZeroFlux build agent, 2026-11-09, for the private testnet only. A production decision needs the Gate 3 to 6 reviews in RELEASE_READINESS.md.
