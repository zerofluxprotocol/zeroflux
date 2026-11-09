# Implementation status (Track A + Track B, spec v1.0 of 2026-11-09)

Living document. Each row links to evidence. States: EXISTING (verified working), INCOMPLETE, MISSING, BLOCKED (external), DONE (completed in this track).

## Gate 0 baseline (2026-11-09)

- Commit at start: `211428a`. Working tree clean.
- `tests/run_all.sh`: 13/13 PASS (cargo test, vectors Rust/C++/Python, stress NumPy/MPS/MLX, GPU device compile, explorer typecheck). Output: `/tmp/gate0_baseline.txt`, reproduced in TEST_REPORT.md.
- Prior GPU evidence (not rerun at Gate 0): H100 and MI355X bit-exact, `bench/results/gpu_summary.json`, sandbox ledger `bench/results/gpu_ledger.json`.
- Toolchains: Rust 1.99.1 with `wasm32-unknown-unknown`, cargo-near 0.23.0, near-cli-rs 0.31.1, NEAR sandbox (protocol 88) runs natively on macOS arm64.
- NEAR credentials: `~/.near-credentials/testnet` holds 14 accounts named `other*` belonging to a different project. None is a ZeroFlux account. Deploying onto them would overwrite another project's contracts. Gate 6 status below.
- NEAR Intents: Message Bus RPC/WebSocket requires a Partner Portal JWT plus KYC/KYB and a mainnet account. The official Verifier contract is open source (`near/intents`, MIT). Mainnet `intents.near` code hash `BtA1BEFNS619KkvXFsgcpQ21Tb5yMm32aaUkn7xNothk` matches no published release artifact (checked v0.4.0, v0.4.1, v0.4.2, global-deployer v0.1.0/v0.2.0).

### Requirement inventory at Gate 0

| req | item | Gate 0 state | evidence / gap |
|---|---|---|---|
| A1 | versioned header, hash, PoW challenge, tx serialization, Merkle, target, fork choice, MTP, LWMA, fees, subsidy | EXISTING | `crates/zfx-verify`, CONSENSUS.md, `chain_rules.rs` |
| A1 | coinbase maturity | MISSING | coinbase spendable immediately |
| A1 | state root / state commitment | MISSING | no consensus state commitment |
| A1 | consensus parameter activation rules, genesis spec doc | INCOMPLETE | genesis config exists, no activation framework or genesis vectors |
| A1 | golden vectors for genesis, header, tx, difficulty | INCOMPLETE | PoW vectors only |
| A1 | duplicate/overflow/oversize/wrong-chain negative tests | INCOMPLETE | some in `chain_rules.rs`; no oversize, wrong-chain replay test |
| A1 | 1000+ block run with supply audit | MISSING | longest run about 60 blocks |
| A1 | tokenomics alternatives + decision report | MISSING | |
| A2 | persistent store, crash recovery | INCOMPLETE | JSONL block log, truncated-tail recovery; no checksums, no corruption detection, no snapshots |
| A2 | encrypted authenticated P2P | MISSING | plaintext TCP JSON lines |
| A2 | peer identity, discovery/bootstrap, per-peer limits | MISSING | static `--peer` only |
| A2 | clean shutdown, health endpoint | MISSING | SIGTERM kills; `/metrics` exists |
| A2 | 3-node topology, partitions, reorg, restart | EXISTING | `testnet/zfxnet.py` 11/11 |
| A2 | 10 restart runs, corrupted DB detection, malformed frames fuzz | MISSING | |
| A3 | ledger, SLH-DSA P2MR, wallet new/address/balance/send/tx | EXISTING | `zfx-wallet`, `tx.rs` tests |
| A3 | history, backup/restore, fee estimate, watch-only, faucet, receipts | MISSING | |
| A3 | explorer | EXISTING | `explorer/` |
| A4 | CPU/CUDA/ROCm/MLX miners, golden vectors, stress | EXISTING | TEST_REPORT.md, `gpu_summary.json` |
| A4 | `--benchmark` JSON with versions; acceptance rate; GPU memory; launch overhead | INCOMPLETE | bench JSON lacks versions and memory |
| A4 | sanitizers | MISSING | |
| A4 | GPU container images | MISSING | CPU Dockerfile only |
| A5 | authenticated admin RPC | MISSING | no admin RPC |
| A5 | fuzzing, static analysis, dep scan, SBOM, license inventory | MISSING | clippy only |
| A5 | operator runbook | MISSING | |
| B1 | NEP-141/145/148 wZFX contract, 2-of-3 mint, pause, upgrade | MISSING | |
| B2 | native lockbox, 2-of-3 SLH-DSA release in consensus | MISSING | |
| B3 | 3 relayers, coordinator, job state machine, reconciliation | MISSING | |
| B4 | sandbox tests, testnet deploy scripts | MISSING | |
| B4 | NEAR testnet deployment | BLOCKED (pending) | no ZeroFlux testnet account configured; see Gate 6 |
| B5 | Intents adapter, test solver, local settlement | MISSING | |
| B5 | live Message Bus routing | BLOCKED | Partner JWT + KYC/KYB + mainnet account |
| Make targets | `make check/e2e-*/security-test/gpu-test/testnet-smoke/acceptance` | MISSING | `tests/run_all.sh` only |

## Progress log

(Updated per commit below.)
