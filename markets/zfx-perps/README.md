# zfx-perps

The ZeroFlux hybrid order-book perpetuals exchange on NEAR. An operator matches signed orders
off-chain. A NEAR clearing contract holds collateral and isolated positions and verifies every
fill: both users' signed orders, nonces, expiry, limits, policy hash, fees and margin. A matcher
or relayer can delay or reorder, but can't forge a trade or move funds. Markets track signed index
prints. The first adapter is the ZFX daily and 7-day model-spend index.

Testnet and paper trading only. Nothing is audited. Model perps must not take real money (spec
5.4).

Built on [obkit](https://github.com/numinousmuses/obkit) (`d617503`, MIT, see `LICENSE`):
deterministic matching engine, journal, node, NEAR tooling and TypeScript SDK. The repository
is Apache-2.0; this directory keeps obkit's MIT license. obkit's netted settlement mode (spot and
cross-margin perps, operator-trusted) is still here; see `docs/netted-mode.md`. It is not the NEAR
v1 path.

## Components

| Path | What it does |
|---|---|
| `crates/zfx-perps-clearing` | Clearing core shared by contract and engine: signed-message formats, `OraclePrint` v1 codec, isolated-margin ledger, fill verification, liquidation and loss waterfall, funding, batches, liquidation planner, test fixtures |
| `contracts/zfx-perps-clearing` | NEAR clearing contract: vault, verified batches, oracle quorum, timelocked admin, forced withdrawal, freeze and escape |
| `crates/zfx-perps-engine` | Price-time book. `clearing.rs` admits and matches signed orders against the predicted ledger and reconciles with chain events |
| `crates/zfx-perps-node` | Sequencer, journal, REST/WebSocket gateway. `clearing_api.rs` implements `openapi.yaml` |
| `crates/zfx-perps-near` | Contract client, deployer, relayer (`clearing_adapter.rs`), sandbox, `zfx-perps-near` CLI |
| `packages/sdk-ts` | `@zeroflux/zfx-perps-sdk`: `listMarkets, getBook, getOracle, getPortfolio, estimateMargin, signOrder, placeOrder, cancelOrder, subscribeBook, subscribeOrders, deposit, withdraw, getFunding`, print verification, local margin math, wallet transactions. Examples: market maker, retail UI, testnet end to end |
| `openapi.yaml` | REST/WS contract (also served at `/v1/openapi.yaml`) |
| `deploy/testnet` | Testnet market parameters and a generator from the index's `models.json` |
| `research/` | Gas, risk and stress data; obkit's on-chain order book comparison |

Design, message formats and operator powers: `../../docs/ORDERBOOK_AND_CLEARING.md`.
Parameters and stress results: `../../docs/MARKET_RISK.md`. Testnet accounts and transactions:
`TESTNET.md`.

## Quick start (local chain)

Requirements: Rust (pinned by `rust-toolchain.toml`), `cargo-near`, a `near-sandbox` binary
(`NEAR_SANDBOX_BIN`, default `~/.local/share/basis-amm/near-sandbox`), Node 22 and pnpm.

```sh
scripts/build-contracts.sh                                   # out/zfx_perps_clearing.wasm, mock_ft.wasm
cargo test -p zfx-perps-near --test clearing_e2e -- --nocapture   # chain + node + relayer, open and close
```

## Run against NEAR testnet

The live testnet deployment is `perps-v2.zeroflux.testnet`: 28 licensed 7-day model markets,
collateral `tusdc-perps.zeroflux.testnet` (open faucet). Accounts and every transaction are in
`TESTNET.md`. To run your own node and relayer against it you need the operator key, i.e. the
stack file `~/.config/zeroflux/perps-v2-testnet-stack.json` from the deployer. Anyone else can
run a read-only node, but only the operator's relayer can settle.

```sh
cd markets/zfx-perps
scripts/build-contracts.sh && cargo build --release -p zfx-perps-node -p zfx-perps-near
B=target/release; S=~/.config/zeroflux/perps-v2-testnet-stack.json
TOKEN=$(cat ~/.config/zeroflux/perps-v2-testnet-admin-token)       # any 16+ char secret
PRINTS=~/.local/share/zfx-index/public/v1/indices/prints/latest.json   # or the publisher's https URL
$B/zfx-perps-near clearing-node-config --stack $S --data data/testnet-v2/node \
  --listen 127.0.0.1:8900 --admin-token "$TOKEN" --print-feed "$PRINTS" > data/testnet-v2/node.json
$B/zfx-perps-node --config data/testnet-v2/node.json &                 # REST + WS on 127.0.0.1:8900
$B/zfx-perps-near clearing-adapter --stack $S --node http://127.0.0.1:8900 --admin-token "$TOKEN" \
  --log data/testnet-v2/relayer-batches.jsonl &
curl -s http://127.0.0.1:8900/v1/markets | head -c 400
```

The node rebuilds from `data/testnet-v2/node` on restart, and the relayer is stateless. The print
feed is polled every minute; the contract accepts only newer, reporter-signed prints.

Connect the trade app: open the site's trade page and press *Connect venue* with
`http://127.0.0.1:8900`, or pass `?venue=http://127.0.0.1:8900`.

**Browser access (CORS).** `cors_allow` in the node config defaults to
`["https://zeroflux.network", "http://127.0.0.1:*", "http://localhost:*"]`, where `*` stands for
any port. Preflights get `Access-Control-Allow-Private-Network: true`, so a page on the public
site may call a node on 127.0.0.1. `"*"` allows every origin.

**Onboard a trader** (any testnet account, no operator files needed):

```sh
near account create-account sponsor-by-faucet-service <name>.testnet autogenerate-new-keypair \
  save-to-legacy-keychain network-config testnet create            # optional: a fresh account
target/release/zfx-perps-near onboard-testnet --stack-info perps-v2.zeroflux.testnet \
  --account <name>.testnet --key-file ~/.near-credentials/testnet/<name>.testnet.json --deposit 1000
```

It reads the collateral token from the contract, then:
1. mints test USDC from the token's faucet;
2. generates a trading key;
3. registers the key on the contract, signed with the account's own NEAR key;
4. deposits the tokens;
5. saves the key to `~/.config/zeroflux/trading-<name>.testnet.json` (0600).

It prints the account id and the `ed25519:...` trading secret to paste into the trade app. The
deposit is usable after the node's next batch (about 1 s). `examples/place-cancel.ts` places
and cancels one order with that key. The command refuses anything that isn't on testnet.

Wallet steps (register a trading key, deposit) are NEAR transactions from the user's wallet; the
SDK builds them (`clearing.clearingTx`). `packages/sdk-ts/examples/testnet-e2e.ts` runs the whole
flow with near-api-js.

### Venue API for the trade app

All numbers are decimal strings except book levels, bps and enums. Prices are ticks: the market's
`index_units_per_tick` index units per tick, so 1 tick = $1 of index per day on these markets.
Sizes are lots, and the notional of `q` lots at price `p` is `q × p × tick_value` tUSDC atoms (6
decimals). Match a board market to a venue market by `params.index_id ==
"or-model-spend-lic-7d/" + permaslug` (also given as `permaslug`). On-chain market ids are
`[A-Z0-9-]` and differ from the board's `market_id`.

| Need | Call | Shape |
|---|---|---|
| Deployment | `GET /v1/info` | `{chain_id, clearing_contract, collateral, collateral_decimals, insurance_account, mode}` |
| Markets with mark, funding, OI | `GET /v1/markets` | `{chain_id, clearing_contract, collateral, markets: [{market_id, permaslug, params{...MarketParams, index_id, tick_value, max_open_interest_notional, ...}, policy_hash, status, effective_status, mark, index_ticks, mark_fresh, print_move_flag, open_interest (lots), open_interest_notional (atoms), bad_debt, funding: {last_rate_ppm, cum_funding, last_funding_ns, next_funding_ns}}]}` |
| Funding detail | `GET /v1/funding/{market}` | `{last_rate_ppm, cum_funding, next_funding_ns, window_vwap_e6, history: [{ts_ns, rate_ppm, pay_per_lot}]}` |
| Oracle and provenance | `GET /v1/oracle/{market}` | `{mark, mark_fresh, effective_status, reporters, quorum, print: {hash, canonical_hex, signatures, value, status, data_status, source_start_ms, source_end_ms, published_at_ms, valid_until_ms, revision}}`; verify with SDK `checkOracle` |
| L2 book | `GET /v1/book/{market}?depth=50`, then WS `{"op":"subscribe","channel":"book","market":M}` | `{channel:"book", type:"snapshot"\|"delta", market, seq, bids: [[price, qty]], asks: [[price, qty]]}`, levels as JSON integers, qty 0 removes a level, seq strictly increasing (a gap means re-subscribe); SDK `subscribeBook` handles this |
| Trades | `GET /v1/trades/{market}?limit=100`, WS channel `trades` | `{market, trades: [{trade_id, price, qty, taker_side, ts_ns}]}` |
| Positions, free collateral, open orders | `GET /v1/portfolio/{account}` (signed headers) | `{account, pending: {free, positions: [P]}, settled: {free, positions: [P]}, min_nonce, action_nonce, open_orders: [{order_id, order_hash, market, side, price, quantity, remaining, nonce, expiry_ns, client_order_id}], last_settled_batch, unconfirmed_batches, pending_ops}` where `P = {market, size, margin, equity, mark, initial_margin, maintenance_margin, liquidation_price, funding_owed}`. `pending` includes fills not yet settled on NEAR; `settled` is the contract's state |
| Fills | `GET /v1/fills/{account}` (signed headers), WS account channel `fill` | `{account, fills: [{market, order_id, trade_id, role: "maker"\|"taker", side, price, qty, fee, remaining, ts_ns}]}`, newest first, since node start |
| Margin estimate | `POST /v1/estimate` `{accountId?, marketId, side, price, quantity}` | `{initial_margin, fee, required_free, free, ok, mark, liquidation_price}` (or SDK `estimateMargin`, local) |
| Place a signed order | `POST /v1/orders` with SDK `signOrder` output | body `{chainId, clearingContract, accountId, marketId, side, type, price, quantity, reduceOnly, postOnly, timeInForce, nonce, expiryNs, clientOrderId, maxFeeBps, policyHash, publicKey, signature (base64)}`. Reply `{type:"placed", order_id, order_hash, status: "resting"\|"filled"\|"canceled", filled, remaining}` or HTTP 409 `{error, message}` |
| Cancel | `POST /v1/orders/cancel` `{accountId, orderHash, expiryNs, publicKey, signature, relayOnChain?}` (SDK `cancelOrder`) | `{type:"canceled", order_id, remaining}`; with `relayOnChain` the cancel is also made final on-chain in the next batch |
| Cancel all | `POST /v1/orders/min_nonce` (SDK `setMinNonce`) | kills every order below the new floor, here and on-chain |
| Account events | WS `{"op":"auth","account","ts","public_key","signature"}` | `accepted`, `fill`, `canceled`, `rejected`, `position`, `free`, `fill_reverted`, `payout_queued`, `liquidated`, `adl` |

Signed read headers: `x-zfx-perps-key` (`ed25519:<base58>`), `x-zfx-perps-ts` (unix ms, within
30 s), and `x-zfx-perps-signature` (base64 signature over
`zfx-perps-auth-v1|<chain_id>|<clearing_contract>|<account>|<ts>`). Each signature can be used
once, so sign a fresh one per request.

The ledger doesn't store an entry price: isolated margin absorbs every cash flow. For display,
derive the average entry from `/v1/fills`, and show `equity`, `liquidation_price` and pending
vs settled.

## Tests

```sh
cargo test --workspace                 # core, engine, journal, node, client, NEAR crate incl. 4 sandbox suites
(cd contracts && cargo test)           # both contracts (near-sdk mocked runtime)
(cd packages/sdk-ts && pnpm test)      # TS SDK: Rust parity vectors, print verification, margin math, live node
cargo test --release -p zfx-perps-clearing --test stress -- --nocapture   # real-history stress replay
```

What they establish (details in the docs):

- **Clearing core.** Forged, tampered, replayed, wrong-chain, wrong-contract, expired, canceled,
  below-nonce-floor, over-filled and over-fee fills are rejected. Margin, position and
  open-interest caps hold. Oracle quorum, ordering, staleness and the daily window work. Funding
  is zero-sum. The full liquidation waterfall is covered. A 300-case randomized conservation test
  covers every branch of the waterfall; a rejected op never changes state. The codec matches the
  index's golden vectors and a real signed print.
- **Contract.** The same attacks against the contract code, plus wrong-token deposits, double
  settlement, non-operator settlement, key revocation, nonce floors, wallet cancels, timelocked
  admin, forced withdrawal, freeze and escape. Custody attacks and gas per verified fill are also
  run on the real wasm on a local nearcore sandbox.
- **Engine.** After every batch, the engine's confirmed ledger equals a simulated contract
  running the same core. This holds under random trading, prints, funding, wallet cancels, key
  revocations and late batches. Replay and snapshot restore are deterministic.
- **End to end.** On a sandbox: open and close through node and relayer across a node crash and
  restart, matching reference math. On testnet: the same through the public TS SDK with real
  signed prints (`TESTNET.md`).
- **obkit's spot and netted-perp suites** are unchanged and green.
