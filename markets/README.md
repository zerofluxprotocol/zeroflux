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

## Trade on testnet

The public site shows the index and the markets. Orders need a zfx-perps node with the operator's
relayer behind it; ZeroFlux has not hosted one publicly yet, so the site shows "venue offline"
until it does. With a node running (`zfx-perps/README.md`, "Run against NEAR testnet"):

```sh
# a free testnet account from the official faucet (NEAR CLI: cargo install near-cli-rs)
near account create-account sponsor-by-faucet-service you.testnet \
  autogenerate-new-keypair save-to-legacy-keychain network-config testnet create

# test USDC from the faucet, a new trading key registered on perps-v2, a deposit
cd markets/zfx-perps && cargo build --release -p zfx-perps-near
target/release/zfx-perps-near onboard-testnet --stack-info perps-v2.zeroflux.testnet \
  --account you.testnet --key-file ~/.near-credentials/testnet/you.testnet.json --deposit 1000
```

Then open the trade app with `?venue=<node URL>`, press *Sign in* and paste the account and the
trading secret the command printed. Orders are signed in the browser; the trading key is stored in
that browser only.

## Tests

```sh
(cd index && cargo test && node js/test-vectors.mjs)                  # index, prints, oracle contract sandbox test
(cd zfx-perps && cargo test --workspace --release --no-fail-fast)      # engine, clearing core, node, sandbox end to end
(cd zfx-perps/contracts && cargo test --release)                       # clearing contract
(cd zfx-perps/packages/sdk-ts && pnpm test)                            # TS SDK, Rust/TS parity
```
