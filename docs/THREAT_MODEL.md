# Threat model (v0.1)

Adversary (spec section 3): arbitrary CPUs, GPUs, ASICs, memory and network nodes. It chooses nonces and headers, parallelizes, precomputes anything that does not depend on the challenge, modifies kernels, crafts proofs, and runs malicious peers.

## 1. Security invariants and their status

| invariant | mechanism | status |
|---|---|---|
| Challenge binding | `challenge = SHA256(domain, header_without_nonce)`. Every attempt input derives from `(challenge, nonce)`. | Holds by construction, tested (`deterministic_and_nonce_bound`). |
| No cheap equivalent | Dense random weights, nonlinear S-box between layers, digest over the full final output. | **Unproven.** No shortcut found (section 2). Needs external cryptanalysis. |
| Unambiguous arithmetic | Integer domain with `|sum| < 2^24`, consensus on integers, not hardware formats. | Proven by the bound, tested on 3 implementations and 6 backends. NVIDIA/AMD untested. |
| Bounded verification | One attempt (51.6 M MACs, about 1 ms on one CPU core). Header and structure checks run before PoW. Caps: 16 MiB message, 4 MiB block, 1,000 txs, P2MR depth 8, hex fields 40 KB. | Holds. DoS economics (section 4) are open. |
| Difficulty control | 256-bit target, LWMA-1, digests uniform. | Measured: z = 1.51 over 200k attempts, chi² 215/255. |
| Mining fairness | No zero (no sparsity), uniform inputs, full-rank weights, no algebraic layer collapse. | Partly. Strassen favors int8 hardware by up to 1.49x over FP8-only hardware (section 2.6). |

## 2. Attacks attempted against the selected design (E)

Each attack has code in `crates/zfx-research/src/attacks.rs` and data in `bench/results/attacks.json`.

1. **GF(2) affine parity shortcut.** With a naive requantizer (`val(y & 31)`), the parity of every activation is an affine GF(2) function of the previous layer's parities. All layers collapse into one 64 -> 256 bit-matrix per epoch, predicting **100%** of output parities with no integer GEMM. With the shipped S-box requantizer the same predictor scores **49.1%**. That is chance, so the attack fails. This is why the S-box exists, and it shows that "nonlinear enough" must be tested, not assumed.
2. **Partial computation.** A +1 error in a single layer-0 accumulator changes 97.5% of the outputs and 125.9 of 256 digest bits on average. Skipping or approximating work produces an unrelated digest.
3. **Low rank.** The 256x256 dev hidden layer and a 384x384 block of a 4096x4096 v0.1 layer are full rank mod p, so there is no factorization shortcut.
4. **Sparsity and structured inputs.** The domain contains no zero, and X0 comes from SHAKE256 of a hash the miner cannot steer except by grinding nonces, which costs a full attempt each.
5. **Precomputation and nonce reuse.** Weights are public per epoch, and that is fine: the only per-weight precomputation that helps is the Four Russians method, and with |D| = 32, g = 2 needs a 32 GiB table per 4096² layer, streamed every attempt (memory-bound, slower than a tensor core). Activations of different nonces agree at the 1/32 chance rate, so nothing carries over between attempts.
6. **Fast matrix multiplication.** Strassen is exact over the integers, so it is an equally valid way to compute the canonical output, not a forgery. Int8 hardware can hold 3 Strassen levels (operands up to 111 fit in int8), which is 0.67 of the MACs, a 1.49x algorithmic ceiling. FP8 E4M3 holds 0 levels, because sums such as 17 or 31 need 5 significant bits. This is a real fairness asymmetry between int8-capable GPUs (H100, B200, MI300X, MI355X) and FP8-first parts with weak int8 (Blackwell Ultra B300). Measured CPU gains were only 1.02x to 1.23x, because the added memory traffic eats the savings.
7. **Reduced-precision hardware paths.** FP16 accumulation diverges on 510 of 512 rows. A 14-bit truncating FP8 MMA accumulator diverges on 3 of 512 rows at K=4096. A miner on such a path does not split consensus, because nodes verify on the CPU reference. It only loses those attempts. `zpu-cli` re-verifies every winner and random spot checks, and refuses a miscomputing backend.
8. **Header grinding.** Changing the timestamp or tx set changes the challenge, which costs a full attempt per variant, the same as a nonce.
9. **Epoch-weight manipulation.** A miner who finds an anchor block could withhold it to choose the next epoch's weights. Weights are uniform, and no weak-weight class is known, so the gain is nil under current knowledge. It is listed so that review can check it.

## 3. Attacks on the rejected candidates (evidence for the decision)

- **C1 (Merkle transcript plus sampled openings): forged.** Unopened leaves are unconstrained, so a miner computes a fraction `f` of the tiles and grinds a junk leaf. Each grind is a Merkle path update, and a forgery needs about `1/(p·f^s)` grinds. The verifier accepted forged proofs at f = 0.75, 0.5 and 0.25. Disqualified.
- **C1' (lottery first, then open): forged.** The lottery is evaluated before any work, so the attacker computes only the s opened tiles of winning nonces, 6.25% of the honest work. Disqualified.
- **A and B: canonical arithmetic is not native.** 63% to 100% of outputs differ from the canonical bits under every hardware-like reduction order, so honest GPU miners would need 9 int8 GEMMs of Ozaki emulation.
- **D: generation-bound.** 512 MACs per SHAKE byte means a GPU spends most of its time hashing.

## 4. Network and DoS

| threat | mitigation | status |
|---|---|---|
| Invalid-block spam | Structure, header-chain linkage, target and timestamp checks run before the 1 ms PoW. Invalid PoW, structure or signature means disconnect plus a 10-minute IP ban (loopback is only disconnected). | Tested (`push_invalid_block`). The cost to the attacker of one invalid block is one TCP connection. Proof-of-work spam pricing is **open**. |
| Oversized messages | 16 MiB line cap, 64 KiB HTTP body, 40 KB hex fields. | Implemented. |
| Submission flood | 50 submits per second per IP. | Implemented. |
| Reorg cost | Replay from genesis, O(chain). | Fine on the testnet. **Open** for public networks (needs undo data). |
| Eclipse / Sybil | Static peers only, no address gossip. | **Open.** |
| Transport | Plain TCP. No encryption or peer authentication. | **Open.** When added it should use an ML-KEM-768 hybrid key exchange. |
| Orphan flood | 256-entry orphan pool, oldest evicted. | Implemented. |

## 6. Assumptions

1. SHA-256 and SHAKE256 behave as random oracles for domain-separated inputs.
2. The S-box chain has no algebraic or statistical shortcut cheaper than evaluating the layers. **This is the primary unproven assumption.**
3. SLH-DSA security reduces to its hash function (FIPS 205). The `fips205` crate is correct, as its ACVP vector tests indicate, but it is not audited for ZeroFlux.
4. An honest majority of mining work.

## 7. Known vulnerabilities and unresolved items

- No independent review of the hardness assumption (Gate 3 not passed).
- No NVIDIA or AMD hardware measurement of exactness or throughput (Gate 1 incomplete for those vendors).
- No transport encryption, no eclipse resistance, and O(n) reorg replay.
- int8 vs FP8 Strassen asymmetry (1.49x ceiling). ASICs are not prevented (spec non-goal).
- Wallet seed files are unencrypted (0600 permissions only).
