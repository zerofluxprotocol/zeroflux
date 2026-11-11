# ZFX Markets

Trade AI demand: a daily index of how much each AI model is used on OpenRouter, priced in dollars,
and perpetuals on its 7-day average that clear on NEAR. NEAR testnet, paper trading only, no real
money. Product brief: the launch spec; risk and legal gates: `../docs/RELEASE_READINESS.md`.

| Part | Directory | Live on NEAR testnet |
|---|---|---|
| Index: ingestion, formula, ledger, signed prints, publisher, read-only API | [`index/`](index/README.md) | prints published at [zeroflux.network/v1](https://zeroflux.network/v1/markets/models.json) |
| On-chain oracle: every signed print, verifiable by anyone | [`index/contracts/zfx-oracle`](index/contracts/zfx-oracle) | `oracle.zeroflux.testnet` ([TESTNET_ORACLE.md](index/TESTNET_ORACLE.md)) |
| Exchange: matching engine, node, NEAR clearing contract, TS SDK | [`zfx-perps/`](zfx-perps/README.md) | `perps-v2.zeroflux.testnet`, 28 markets ([TESTNET.md](zfx-perps/TESTNET.md)) |
| Trade app | [`apps/trade/`](apps/trade) | [zeroflux.network/trade](https://zeroflux.network/trade/) |

## What is traded

- **The index.** For each model, per UTC day: OpenRouter tokens (from OpenRouter's documented
  daily dataset, CC BY 4.0) times a frozen billed share and input/output mix, times the documented
  listed prices. USD per day. It is implied spend on one venue, not the model's revenue, not
  royalties, not equity. Method: [`docs/DATA_METHOD.md`](../docs/DATA_METHOD.md).
- **The traded value** is the 7-day average (`ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1`). Pushing it
  means buying real OpenRouter usage for a week, and the published cost to move a market 1% is
  1% of its 7-day spend.
- **Caps.** Open interest per market is at most 25% of the 7-day spend, so the profit from pushing
  the index stays below what the push costs. Markets list only with an OI cap of at least $10,000
  and a clean licensed 7-day print.
- **Safety.** Missing or non-fresh data, or a day that moves the 7-day index more than 25% (the
  99th percentile of 22,654 observed moves), puts a market in close-only.
- **Clearing.** The NEAR contract checks every fill against both traders' signed orders and
  enforces isolated margin, caps, funding, liquidation and a finite loss waterfall. The operator
  can delay or reorder; it cannot forge a trade or move funds
  ([`docs/ORDERBOOK_AND_CLEARING.md`](../docs/ORDERBOOK_AND_CLEARING.md),
  [`docs/MARKET_RISK.md`](../docs/MARKET_RISK.md)).

## Trading

Trading is in private beta: request access at [zeroflux.network/#beta](https://zeroflux.network/#beta). The
exchange is built and deployed on NEAR testnet (`perps-v2.zeroflux.testnet`); the public site shows the
markets, the index and its history read-only.

Developers can run the whole flow themselves on testnet with their own node and a funded test account:
`zfx-perps/README.md`, "Run against NEAR testnet", and the `zfx-perps-near onboard-testnet` command.

## Tests

```sh
(cd index && cargo test && node js/test-vectors.mjs)                  # index, prints, oracle contract sandbox test
(cd zfx-perps && cargo test --workspace --release --no-fail-fast)      # engine, clearing core, node, sandbox end to end
(cd zfx-perps/contracts && cargo test --release)                       # clearing contract
(cd zfx-perps/packages/sdk-ts && pnpm test)                            # TS SDK, Rust/TS parity
```
