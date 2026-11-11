# Repository audit

What existed on the owner's machine when the launch work started (2026-11-10), what was reused, and what changed. Commit hashes refer to this repository unless another repository is named.

## Repositories found

| Path | What it is | State found | Used for |
|---|---|---|---|
| `~/zeroflux` (this repository) | ZFX mining: production PoW (`zfx-fp8c-v0.1`), miners (Rust, C++/CUDA/HIP, Python MLX/MPS), the `zfx-mine-opt` NEAR contract with optimistic checking, watcher, docs site | `51327d5`; `make check` 25/25, e2e on a local NEAR sandbox passing | Canonical repo. Mining is kept as is and hardened (below) |
| `~/promptvpn` | Prompt VPN 0.1: local prompt privacy proxy (Rust/Axum, SQLite, encrypted vault, GLiNER, React dashboard, NEAR AI attestation via `nearai-inference-sdk`) | `d2ac753` (2026-11-08); its suite 159 passed, 0 failed | Imported into `vpn/` and migrated in place to ZFX VPN. The original checkout was not modified |
| `~/obkit` | Hybrid order-book SDK on NEAR (off-chain matching, on-chain custody, netted settlement), MIT. Spot plus perps added on 2026-11-10 by another session | `d617503` | Forked into `markets/zfx-perps/` (see "Markets" below) |
| `~/basis-amm` | Basis-AMM: GMX-style leveraged positions against a counterparty vault, no order book | `ab01460` | Not reused. The launch brief rules out its pool-counterparty model for ZFX Markets |
| `~/numinous-vpn` | A network VPN for a dataset project (Fly.io) | n/a | Unrelated to ZFX VPN; not touched |

The earlier drafts the brief mentions (`ZeroFlux_ZFX_Technical_Spec_v0.1.md`, `ZeroFlux_Track_A_B_Implementation_Spec.md`, `Basis_AMM_Engineering_Spec_v0.1.md`, `Prompt_VPN_Engineering_Spec_v1.md`) are not on this machine. Prompt VPN's current spec is `vpn/docs/spec-v1.4.md`; the code and its tests were treated as the source of truth.

## Decisions on conflicts in the brief

- **One chain, no second chain.** ZFX is a NEP-141 token minted by `zfx-mine-opt` on NEAR. The own-chain design and its committee bridge (`crates/zfx-node`, `bridge/near/wzfx`) are kept and labeled as an alternative; nothing launches on them.
- **"ZPU".** The miner binary was `zpu-cli`; the roadmap uses ZPU for a future inference server. The miner is now installed as `zfx-miner` (public name) and still as `zpu-cli` (same program, `miner/cli/src/app.rs`), so existing scripts keep working. ZPU means the planned inference server only.
- **Prompt VPN to ZFX VPN.** Renamed with a working `promptvpn` alias and automatic migration of the data dir and keychain entries. Internal crate names (`pv-*`), the vault key-derivation salt and the recovery-bundle format id keep their old names on purpose: changing them would break existing encrypted data (`vpn/docs/deviations.md` entries 20 to 32).
- **Data source for ZFX Markets.** No OpenRouter API key was available at first, so the first index (the web index) reads OpenRouter's public web endpoints, the ones openrouter.ai pages call. Once a key was available, the dataset index was built from OpenRouter's documented daily dataset, and markets reference it; see `docs/DATA_METHOD.md` section 2.

## Existing, modified, new

| Area | Existing | Modified | New |
|---|---|---|---|
| Mining (`bridge/`, `miner/`, `crates/`) | PoW, miners, `zfx-mine-opt`, watcher, proxy | Contract refuses a bond below a pending block's storage cost (e2e O7); proxy attaches the contract's bond exactly, caches work, keeps a 0.3 NEAR reserve and withholds work when it can't bond; NEAR client fails over across RPC endpoints with backoff; watcher skips verified heights; miner retries on node errors; `zfx-miner` name | `scripts/mine-testnet.sh`; contract live at `zfx.zeroflux.testnet` (`bench/results/testnet_opt_mining.json`) |
| ZFX VPN (`vpn/`) | Prompt VPN 0.1 (226 files) | Rename, `ZFX_*` error codes with `PV_*` aliases, `[[ZFX_…]]` placeholders, schema v2 with rollback, `zfx_privacy` request extension, route fixed before first byte, provider-level TEE checks | Hosted gateway `vpn/hosted`, local-to-hosted protected transport, demos for both audiences, Python helper, `docs/VPN_THREAT_MODEL.md` |
| ZFX Markets index (`markets/index/`) | none | n/a | Daily index `ZFX-OR-MODEL-SPEND-USD-v1`, ledger, signed OraclePrint v1, publisher, read-only API, poller, `docs/DATA_METHOD.md` |
| ZFX Markets perps (`markets/zfx-perps/`) | obkit `d617503` | See `docs/ORDERBOOK_AND_CLEARING.md` | See `docs/ORDERBOOK_AND_CLEARING.md`, `docs/MARKET_RISK.md` |
| Site (`site/`) | Single landing page, docs renderer | Split into partials; docs nav | Home (Earn / Build / Trade), `/earn`, `/build`, `/trade` |

## Bugs found by running against public NEAR testnet

All fixed in `88e3175`, `d035f1d` and `599349d`; each was invisible on the local sandbox.

1. Bonds of 0.1 NEAR were smaller than the storage a pending block occupies (17,408-byte trace plus record), so miners drained the contract until every submit failed with `LackBalanceForState`.
2. `--bond-near 0.25` was parsed as a float: `0.25 * 1e24` is just under 0.25 NEAR, and the contract refused it.
3. `rpc.testnet.near.org` answered HTTP 429 to nearly every call at the proxy's polling rate.
4. The watcher re-read every pending block every 700 ms.
5. A miner could spend its whole balance on bonds and then could not pay for the `finalize` call that returns them.
6. The miner exited on a transient error body from the node.

The first testnet contract, `mine.zeroflux.testnet`, was deployed before fix 1 and is retired; it cannot be deleted (NEAR refuses to delete accounts with large state).
