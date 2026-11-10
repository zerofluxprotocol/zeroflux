# obkit progress

Orderly-style hybrid order book SDK on NEAR. Rust everywhere except the browser SDK
(the project owner, 2026-11-10: "only near", "all rust", "we need ts then add it" for browser use).

## Status (2026-11-10): spot + perps complete, all tests green

| Part | State | Tests |
|---|---|---|
| obkit-engine (spot + perps) | done | 13 spot + 9 perp scenario, 2x 300-case property (perp one simulates the on-chain ledger), coverage guards |
| obkit-journal | done | 4 |
| obkit-node (+ loadgen, oracle, funding) | done | 3 integration + 6 unit (+1 live oracle, ignored) |
| obkit-client (Rust) + examples | done | 3 unit + Rust/TS vectors |
| obkit-near (adapter, CLI, stack, sandbox) | done | sandbox e2e spot lifecycle (~3.5 min) + perps (~2 min) |
| contracts/obkit-settlement, mock-ft | done | 14 unit |
| packages/client-ts | done | 7 (parity vectors + live spot and perp vs Rust node) |
| research (Tonic/Spin comparison) | done | Rust examples onchain_bench, mainnet_survey |
| docs | README, docs/{architecture,api,trust-model,benchmarks}.md | |

## Not built (see README "Limits")
- Inverse/quanto perps, isolated margin, multiple collateral tokens.
- Hot standby / replication of the node.
- Signed-order journaling for provable matching.
- Linux fsync numbers (only measured on macOS).

## Resume
- `source ~/.cargo/env`; sandbox binary ~/.local/share/basis-amm/near-sandbox.
- `scripts/dev.sh` (port 8794; 8790 is taken by another local process). Runs spot + NEAR-PERP with the live exchange-ticker oracle.
- This shell has RUST_LOG=warn, which hides info logs (e.g. adapter `settled` lines).
