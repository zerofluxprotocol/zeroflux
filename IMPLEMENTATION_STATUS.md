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

## Final state of each requirement

| req | item | state | evidence |
|---|---|---|---|
| A1 | header v2 + state root, coinbase maturity, activation (consensus_version, bridge activation height, mainnet gate), golden vectors for v2 encodings | DONE | `docs/CONSENSUS.md`, `tests/vectors/consensus_v2.json`, `consensus_v2.rs` |
| A1 | negative tests: duplicate, overflow, oversize, wrong-chain replay | DONE | `consensus_v2.rs`, `chain_rules.rs`, fuzzing |
| A1 | 1000+ blocks with supply audit on 3 nodes | DONE | `make e2e-native` (dev PoW profile) |
| A1 | tokenomics alternatives + recommendation | DONE (decision is the owner's) | `bench/results/tokenomics.json`, `docs/TOKEN_ACCOUNTING.md` |
| A2 | checksummed store, corruption detection, `--repair`, snapshots, clean shutdown, health | DONE | `node_integrity.rs`, e2e-native |
| A2 | encrypted authenticated P2P, peer ids, discovery, per-peer limits, bans | DONE | `p2p_crypto.rs`, e2e-native |
| A2 | 10 restarts, corruption, malformed frames, parser fuzzing | DONE | e2e-native, `fuzz_parsers.rs` |
| A3 | history, backup/restore, fee estimate, watch-only, faucet, receipts with Merkle proofs | DONE | e2e-native |
| A4 | benchmark JSON with toolchain, memory, launch overhead, acceptance | DONE | `zpu-miner bench`, GPU rerun |
| A4 | sanitizers | DONE | `make -C miner/cuda sanitize-test` |
| A4 | GPU container files | DONE, unbuilt here (no Docker) | `deploy/gpu/` |
| A4 | GPU rerun with real block submissions | see Gate 2 in ACCEPTANCE_REPORT.md | `bench/results/gpu_rerun_*` |
| A5 | authenticated admin RPC | DONE | api.rs, contract API test |
| A5 | fuzzing, clippy, cargo audit, SBOM, license gate, reproducible build | DONE | `make security-test`, `release_check.json` |
| A5 | operator runbook, procedures tested | DONE | `docs/OPERATOR_RUNBOOK.md`, governance e2e |
| B1 | wZFX NEP-141/145/148, 2-of-3 mint, pause, rotation, timelocked upgrade | DONE | wzfx unit tests, governance e2e |
| B2 | native lockbox, 2-of-3 SLH-DSA release/admin in consensus | DONE | `consensus_v2.rs`, e2e-bridge |
| B3 | relayers (anti-equivocation), coordinator, job state machine, reconciliation, halt and audited resume | DONE | e2e-bridge, relayer unit tests |
| B4 | sandbox e2e: 100 round trips, reorgs, governance | DONE | `make e2e-bridge` |
| B4 | NEAR testnet deployment | BLOCKED | needs a ZeroFlux testnet account (`make testnet-smoke` names it) |
| B5 | Intents adapter, test solver, local settlement and failure cases | DONE | `make e2e-intents` |
| B5 | live Message Bus routing | BLOCKED | Partner JWT |
| experiments | algorithm, tokenomics, fork resilience, workload, trust-minimization (on-chain PoW, STARK) | DONE | `bench/results/*.json` |
| make targets | check, e2e-native, e2e-bridge, e2e-intents, security-test, gpu-test, testnet-smoke, acceptance | DONE | `Makefile`, `scripts/gates.py`, `scripts/acceptance.py` |

## Progress log

Commits since the Gate 0 baseline (`211428a`):

- `c1ae238` Gate 0: baseline inventory (IMPLEMENTATION_STATUS.md), 13/13 suites PASS at 211428a
- `e6c245c` Consensus v2 (hard fork, PoW vectors preserved): header v2 with state_root, coinbase maturity, signed memos, native bridge module
- `fc363a6` A2: encrypted authenticated P2P (ML-KEM-768+X25519 hybrid, ML-DSA-65 identities, ChaCha20-Poly1305), checksummed block store with corruption detection and --repair, verified state snapshots, clean shutdown, peer discovery/limits/rate limits, JSON-RPC 2.0 API with admin bearer auth, faucet, history, receipts with Merkle proofs, bridge status RPC; consensus-v2 golden vectors
- `a0398fb` A3 wallet workflows (history, fee, faucet, backup/restore, watch-only, bridge deposit/status), zfx-p2ptool adversary (encrypted invalid-block injection, malformed frames, recording proxy), tests/e2e/native.py: 18/18 at 847 blocks
- `3672f02` B1: wZFX NEP-141/145/148 contract (2-of-3 ed25519 committee mint, burn records, pause/unpause, timelocked rotation and upgrade) + test-ft fixture; 16 unit tests; deployable WASM + ABI via cargo-near
- `ca0cfcd` B2/B3: bridge services crate: minimal NEAR client (borsh tx signing, verified against a live sandbox), relayer witnesses with independent evidence checks, coordinator job state machine, reconciliation with orphan detection and auto-halt, CLI (sandbox, deploy, burn, reconcile, NEP-413)
- `e82293b` Bridge e2e: localnet topology (NEAR sandbox + 3 bridge nodes + 3 relayers + coordinator), live security probes, fault-injected round trips; keep-alive HTTP agents, node refuses future-invalid work, miner backoff
- `3e6cbe0` Gate 4 PASS: 100 two-way bridge transfers under fault injection; NEP-413 conformance against 13 official doc signatures; Intents protocol lib, local Message Bus, solver, adapter; reorg and intents e2e drivers
- `f898cc0` Fix: P2P sync continues past a full 500-hash inventory batch (deep forks stalled); finality/reorg experiment 2/2: below-K reorg never minted, deep reorg after mint detected -> incident + 1-of-3 pause on both chains
- `bdbce1c` Gate 5: e2e-intents 11/11 on the official Verifier (defuse v0.4.2) in sandbox: two-way swaps, liquidity/stale/expiry/duplicate/cancel/refund cases, native->wZFX->swap->native flow; public relay probe recorded as external-blocked (HTTP 403)
- `1ce8eff` Experiments: monetary schedule families (consensus-parameterized; mainnet gated on owner choice), CPU workload sweep, difficulty resilience. Found and fixed an LWMA timestamp-manipulation weakness: solvetimes now measured against the running max timestamp (30% attacker: 24s -> 61s vs 60s target)
- `24e84a3` Trust-minimization experiment: on-chain PoW verifier compiled from consensus sources (dev profile matches golden vector at 71.8 Tgas; v0.1 exceeds 300 Tgas); Winterfell STARK GEMV prototype (proves/verifies, rejects tampering; measured cost + labeled lower-bound model)
- `79bd2d2` Security tests: deterministic mutation fuzzing of header/tx/block/P2P/bridge-op/store/snapshot parsers and the handshake responder; backup/restore state test
- `849e577` Lint: clippy clean with -D warnings across the workspace (doc fix, as_chunks, documented allow for lockstep index loops)
- `79da175` Lint: NEAR contracts clippy clean with -D warnings
- `865cddb` Packaging/security: miner bench JSON (launch overhead, memory, toolchain, CPU-reference acceptance), ASan+UBSan miner target, CUDA/ROCm container files, SBOM + SPDX license gate + reproducible-build check, drop unmaintained wee_alloc from NEAR contracts (bridge e2e 10/10 on rebuilt WASM), zfx-bridge admin-sign and raw-input calls
- `381573c` Bridge governance e2e (7/7): live pause/unpause, 2-of-3 committee rotation on NEAR and native with timelocks, recovery on new keys with retired keys rejected, timelocked contract upgrade preserving state, coordinator job-store loss with zero duplicate mints
- `ed7322e` Style: rustfmt (160 cols, matches existing style) across both workspaces
- `3f69e48` Gate 6: NEAR testnet smoke (preflight names the exact missing prerequisite; full deploy + code-hash verify + two-way receipt flow when an explicit ZeroFlux testnet account is configured); zfx-bridge create-subaccount; parametrized localnet accounts
- `c545427` CLIs: --help/-h prints usage and exits (zfx-node and zpu-cli previously started running on unknown flags)
- `6059df3` GPU rerun harness: real GPU block submissions on the v0.1 profile validated by a CPU node and re-synced by an independent CPU node; tarball includes all workspace members; Rust on AMD; tagged results dir keeps prior evidence
- `d73f3aa` Makefile + gate runner (check, e2e-native, e2e-bridge, e2e-intents, security-test, gpu-test, testnet-smoke): per-step evidence JSON, non-zero exit on any required failure, BLOCKED externals with exact reasons. Tighter e2e checks: Gate 4 now asserts 100 completed round trips and fails on any accepted duplicate mint; lint fixes
- `041e756` Contract API: machine-readable RPC schema with exact signing bytes and error codes; live contract test (positive schema validation, generated + semantic negatives, envelope); keep GPU mining logs
