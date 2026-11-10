# How ZeroFlux works on NEAR

ZeroFlux is a proof-of-work network on NEAR. ZFX is its token, a standard NEP-141 token on NEAR, and the only way to create it is to mine it. Miners do the work off-chain on their own hardware; NEAR keeps the ledger and referees every block through the `zfx-mine-opt` contract (`bridge/near/zfx-mine-opt`).

Blocks are checked optimistically: a block is accepted unless someone proves it wrong during a challenge window, and a fraud proof needs NEAR to recompute a single value.

## Roles

| role | does | tools |
|---|---|---|
| miner | searches for a nonce whose attempt digest is below the target, then submits the block with a bond | `zpu-cli` with any backend (CPU, CUDA, ROCm, MLX, MPS) through `zfx-bridge near-mine-proxy --optimistic` |
| watcher | recomputes every pending block off-chain and challenges the first wrong value | `zfx-bridge near-opt-watch` (ships with the mining tools) |
| anyone | finalizes blocks whose window has passed | `finalize` on the contract |
| NEAR | holds balances, runs the contract, orders transactions | the NEAR protocol |

## Mining a block

1. The miner reads `work(miner)`: the next height, the target, the epoch seed and the work bytes.
2. Work bytes: `"ZFXO-work-v1" || u32le(len) || contract id || u64le(height) || chain hash || u32le(len) || miner account`. The challenge is `SHA256("ZFX-FP8C-v0.1" || work bytes)`, so it commits to the miner's account and a found nonce is worthless to anyone else.
3. Each attempt is the production proof of work, profile `zfx-fp8c-v0.1` (`CONSENSUS.md` section 3): 64 → 4096 → 4096 → 4096 → 4096 → 256 integer layers, 51.6 M multiply-adds, epoch weights from SHAKE256, digest `SHA256("ZFX-result-v0.1" || challenge || nonce || outputs)`.
4. On a hit, the miner submits `submit(height, nonce, trace)` with a bond attached. `trace` is the attempt's intermediate values: a1, a2, a3, a4 (4,096 int8 each) followed by the 256 int32 outputs (17,408 bytes).

## What the contract checks

`submit` (about 6 Tgas):
- the bond is attached, `height` is the next height, every activation is in the operand domain;
- `SHA256("ZFX-result-v0.1" || challenge || nonce || outputs)` is below the current target.

It stores the trace, appends the block as pending, extends the chain hash (`SHA256("ZFXO-chain-v1" || chain hash || digest || miner)`) and retargets difficulty (LWMA over the last 31 blocks, running-max solvetimes). It does not recompute the work.

`challenge(height, layer, row)` (about 4.5 Tgas, anyone): names one value. For layers 0 to 3 the value is activation a(layer+1)[row]; for layer 4 it is output[row]. The contract regenerates that one weight row from the epoch seed and recomputes the value from the stored previous layer (for layer 0, the input is derived from the challenge and nonce). If the stored value is wrong:
- the block and every pending block built on it are reverted, and difficulty and chain hash are restored;
- the challenger receives the fraudulent miner's bond;
- miners of the reverted later blocks get their bonds back, with no reward.

If the value is correct, the challenge fails. Because every value depends only on values before it, a trace in which every (layer, row) relation holds is exactly the canonical computation; the watcher always challenges the earliest wrong value, which is provably wrong.

`finalize(max)` (anyone): for pending blocks older than the window, mints the block reward to the miner, returns the bond and deletes the stored trace. Supply is always reward × finalized blocks.

## Measured gas (`bench/results/e2e_near_opt_mining.json`)

| operation | gas |
|---|---|
| `submit` an honest block | 6.0 Tgas |
| fraud proof, layer 0 | 3.8 Tgas (0.1 Tgas of checking) |
| fraud proof, a 4,096-wide hidden layer or the outputs | 4.5 Tgas (about 0.85 Tgas of checking) |
| challenge against a correct value (rejected) | 3.1 Tgas |
| `finalize` 12 blocks | 24 Tgas |

The NEAR transaction gas limit is 300 Tgas. The stored trace (17 KB per pending block) is covered by the bond and released at finalization.

## Tested end to end (local NEAR sandbox)

- `tests/e2e/near_opt_mining.py`, 6/6: honest blocks from two miners, never challenged; a cheater who does no matrix work, and cheaters who fake only a deep hidden layer or only the outputs, each caught with one value; an honest block built on a fraudulent one reverted with it and its bond refunded; a challenge against a correct value rejected; only honest blocks mint.
- `tests/e2e/near_intents.py`, 5/5: ZFX mined this way is deposited into the official NEAR Intents Verifier and swapped both ways against a test asset, with no bridge.
- `make localnet` runs the same stack for manual mining.

## Security

- **Ledger:** NEAR. Hashpower decides who earns rewards, not the history of balances. A miner with most of the hashpower, including rented GPUs, wins most rewards but cannot reverse transfers or past blocks.
- **Validity of blocks:** at least one honest watcher sees each pending block within the window and can get a transaction included. The watcher runs alongside every miner, and catching fraud pays the cheater's bond.
- **Bond:** must exceed what a cheater could gain from one unchallenged block.
- **Finality of rewards:** after the window (seconds in tests; minutes or more on a public network).
- **Dependence:** NEAR's availability, gas pricing and contract rules.

## Other designs we built

These are tested and kept in the repository for comparison.

| design | how | cost per block | trade-off |
|---|---|---|---|
| NEAR checks every block in full (`bridge/near/zfx-mine`) | a smaller profile, `zfx-near-v1` (H = 1024, 3.47 M multiply-adds, ChaCha8 weights stored per epoch in the contract) | 146 Tgas | no watchers needed, but the GPU lead shrinks (Apple M5 Pro: 3.3x over CPU instead of 24x) and each epoch locks about 35 NEAR of storage |
| PoW light client of a ZeroFlux chain (`bridge/near/zfx-light`) | ZeroFlux runs as its own chain on profile `zfx-near-v1`; NEAR verifies every header and mints from Merkle-proven deposits | 148 Tgas per header | committee-free deposits into NEAR, but an own chain to operate and secure |
| Own chain with a committee bridge (`crates/zfx-node`, `bridge/near/wzfx`) | ZeroFlux L1 plus a 2-of-3 custodial bridge to NEAR | n/a | full sovereignty; a small chain is exposed to rented-hashpower attacks, and two bridge signers can steal locked funds (`BRIDGE_TRUST_MODEL.md`) |

Measured details for these: `bench/results/e2e_near_mining.json`, `e2e_near_light_client.json`, `onchain_pow_gas.json`.
