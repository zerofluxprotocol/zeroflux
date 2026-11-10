# ZeroFlux on NEAR: native mining (C) and a PoW light client (B)

Two ways to tie ZeroFlux to NEAR more tightly than the 2-of-3 committee bridge. Both are built, run end to end on a local NEAR sandbox, and rely on one new piece: a proof-of-work profile NEAR can check inside a single transaction.

| | C. ZFX native on NEAR | B. PoW light client on NEAR |
|---|---|---|
| What it is | ZFX is a NEP-141 token on NEAR; the only way to mint it is a valid proof of work, checked by the contract | ZeroFlux stays its own chain; NEAR verifies every header's proof of work and follows the most-work chain |
| Contract | `bridge/near/zfx-mine` | `bridge/near/zfx-light` |
| Consensus and accounts | NEAR | ZeroFlux miners |
| ZeroFlux to NEAR | no bridge needed | deposits minted from a Merkle proof, no committee |
| NEAR to ZeroFlux | not applicable | not built (would need a NEAR light client in ZeroFlux consensus; the committee remains for releases) |
| Test | `tests/e2e/near_mining.py`: 8/8 | `tests/e2e/near_light_client.py`: 6/6 |
| Run | `make e2e-near` | `make e2e-near` |

## The NEAR-verifiable profile (`zfx-near-v1`, id 3)

Same construction as v0.1 (64 → H → H → H → H → 256 integer layers, S-box requantization, SHA-256 digest), with H = 1024 (3.47 M multiply-adds per attempt, 15x fewer than v0.1) and weights from ChaCha8 keystreams keyed per layer by SHA-256, instead of SHAKE256. Golden vectors: `tests/vectors/near_v1.json`; bit-exact in Rust, C++ (CPU, CUDA, ROCm share host weight generation), NumPy, MLX and PyTorch MPS.

Measured gas inside a NEAR contract (`bench/results/onchain_pow_gas.json` and a probe):

| operation | gas |
|---|---|
| one ChaCha8 weight, computed in wasm | 83 Mgas |
| one multiply-add | 29 Mgas |
| one byte of stored weights read back | ~6 Mgas |

Regenerating 3.47 M weights per call would cost ~290 Tgas on its own, so weights depend only on the epoch (32 blocks) and are prepared once into contract storage, one layer per transaction, by anyone. Each check then reads them back.

| operation | measured |
|---|---|
| prepare one epoch (5 transactions) | 22.5 + 155.1 + 155.1 + 155.1 + 40.7 Tgas |
| C: `submit` (verify one attempt, mint, retarget) | 146.3 Tgas median (100.8 math, 42.9 reading weights) |
| B: `submit_header` (all header rules plus proof of work) | 147.7 Tgas median |
| B: `claim_deposit` (Merkle proof against a verified header) | 6.6 Tgas |
| weight storage per prepared epoch | 3.47 MB (about 35 NEAR locked while stored, returned when freed) |

The NEAR transaction gas limit is 300 Tgas, so both checks fit with room to spare.

## C: mining ZFX directly on NEAR

- Miners search off-chain with any existing backend and submit `submit(height, nonce)`. `zfx-bridge near-mine-proxy` serves the same `/v1/work` and `/v1/submit` API as a ZeroFlux node, so `zpu-cli` and every GPU miner work unchanged.
- The challenge commits to the submitting account, so a winning nonce seen in the mempool is useless to anyone else (tested).
- The contract keeps a hash chain of blocks, retargets difficulty with the same LWMA rule as the native chain, mints the block reward, and frees the previous epoch's weights.
- ZFX moves as a normal NEP-141 token, so NEAR wallets and NEAR Intents work with no bridge.

## B: verifying ZeroFlux on NEAR

- `zfx-bridge near-light-relay` feeds headers from a ZeroFlux node; anyone can relay. The contract checks parent link, chain id, profile, median time past, future limit, the exact expected target (computed from the same ancestors as the node) and the full proof of work, and tracks cumulative work.
- On a heavier fork the canonical index is rewritten back to the fork point (tested with a real network partition).
- `claim_deposit` mints for a lockbox transfer included in a canonical block with at least K confirmations. Security is the cost of out-mining the ZeroFlux chain for K blocks, not a committee.

## Costs and open questions

- **GPU advantage shrinks with the smaller profile.** Apple M5 Pro: GPU (MLX) 206,336 vs CPU 63,036 attempts/s on profile 3 (3.3x), against 80,131 vs 3,293 on v0.1 (24x). Data-center GPUs on profile 3 are not measured yet.
- **Relay cost for B.** One header per block at ~148 Tgas: about 0.015 NEAR per header at the minimum gas price, ~43 NEAR a day at 30-second blocks.
- **Storage deposit.** About 35 NEAR per stored epoch, which is why neither contract is on NEAR testnet yet (the faucet gives 10 NEAR per account).
- **Not built.** The NEAR to ZeroFlux direction for B, weight garbage collection in B, and an external review of both contracts.
