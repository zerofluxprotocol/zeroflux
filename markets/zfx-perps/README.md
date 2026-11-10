# obkit

An order book SDK for building a hybrid exchange on NEAR, in the style of Orderly. Orders are
matched off-chain by an operator; custody and settlement live in a NEAR contract. Users keep
control of withdrawals, and if the operator stops responding they can get their funds out
without it.

It runs spot markets and USDC-margined perpetuals with cross margin, oracle marks, funding,
liquidation, an insurance fund, and loss socialization. Perp profit and loss settles on NEAR as
real token balances through the same netted batches as spot.

The matching engine, node, NEAR adapter, CLI and contracts are Rust. A TypeScript SDK covers
browser apps (trading keys, signed orders, live feeds, wallet transactions).

## Components

| Crate / package | What it does |
|---|---|
| `crates/obkit-engine` | Deterministic matching engine: price-time priority, GTC/IOC/FOK/post-only, self-trade prevention, spot balances with holds, maker/taker fees, perps (positions, cross margin, funding, liquidation, insurance, socialization), netted settlement batches |
| `crates/obkit-journal` | Append-only command log with CRC framing, a blake3 hash chain, snapshots and torn-tail recovery |
| `crates/obkit-node` | The operator server: sequencer, group-commit journal, REST and WebSocket API, ed25519 request auth, index oracle, funding timer, internal API for the adapter. Also `obkit-loadgen` |
| `crates/obkit-near` | NEAR adapter (contract events into the node, batches onto the chain), contract client, stack deployer, sandbox launcher, `obkit-near` CLI |
| `crates/obkit-client` | Rust trading client: signer, typed REST calls, gap-checked book feed. Market maker and taker examples |
| `crates/obkit-wire` | The exact message formats users sign. Shared by the node, the contract and both SDKs |
| `contracts/obkit-settlement` | NEAR contract: deposits, trading keys, batch settlement, signed withdrawals, forced withdrawals, freeze and escape |
| `contracts/mock-ft` | Test NEP-141 token with an open faucet (sandbox and testnet only) |
| `packages/client-ts` | `@obkit/client` for browsers and Node |

## Quick start

Requirements: Rust (pinned by `rust-toolchain.toml`), [cargo-near](https://github.com/near/cargo-near),
and a `near-sandbox` binary (`NEAR_SANDBOX_BIN`, default `~/.local/share/basis-amm/near-sandbox`).

```sh
scripts/build-contracts.sh   # contracts -> out/*.wasm
scripts/dev.sh               # local chain + contracts + node + adapter + market maker + dev users
```

`dev.sh` prints the API address (default `http://127.0.0.1:8794`) and a ready-made command to
trade as a funded dev user:

```sh
OBKIT_URL=http://127.0.0.1:8794 OBKIT_ACCOUNT=alice.test.near OBKIT_TRADING_KEY=ed25519:... \
  target/release/examples/taker buy 50
```

## How it works

```text
 user wallet                     operator                                   NEAR
 ───────────                     ────────                                   ────
 register_key(trading key) ────────────────────────────────────────────▶ settlement contract
 ft_transfer_call (deposit) ───────────────────────────────────────────▶   credits balance, emits event
                                 adapter ◀── get_events ──────────────────  deposit, key_added, ...
                                 node ◀── /internal/chain_events
 signed order (trading key) ───▶ node: verify ─▶ engine ─▶ journal fsync ─▶ ack + market data
                                 every batch interval: cut netted batch
                                 adapter ── settle_batch ────────────────▶   apply deltas, pay withdrawals
                                 node ◀── batch_settled event ◀──────────
```

1. A user generates an ed25519 trading key and registers it on the contract with a wallet
   transaction. The key signs orders, cancels and withdrawals. It can't send funds anywhere except
   back to the user's own account.
2. Deposits go to the contract with `ft_transfer_call`. The adapter forwards the contract's event
   stream to the node in order.
3. The node verifies each signed request and passes it to a single engine thread. A journal thread
   writes commands to disk with one fsync per group. Acks and market data go out only after the
   write is durable.
4. Each batch interval the engine nets every balance change since the last batch, one entry per
   (account, token). 10,000 trades among 50 accounts settle as 101 entries in about 31 Tgas.
5. The contract applies the batch: batches in order, no negative balances, each sealed group nets
   to zero per token. Withdrawals pay out only with the owner's signature, and only to the owner.
6. Forced withdrawal: a user files it on-chain. If the operator doesn't process it within the
   timeout, anyone can freeze the contract, and every user withdraws their last settled balance
   directly.

[docs/architecture.md](docs/architecture.md) has the details. [docs/trust-model.md](docs/trust-model.md)
covers what the operator can and can't do.

## Perpetuals

| | |
|---|---|
| Contract | Linear, USDC-margined. Notional is `lots × price × quote_tick`, the same as spot |
| Margin | Cross margin over all perp markets. Initial margin covers positions plus resting orders, `max(|size + buys|, |size − sells|) × mark × imr`. Maintenance is `|size| × mark × mmr`. Defaults: 10x max leverage, 5% maintenance |
| Order checks | A mark must exist. The price must be within the band around the mark (default 10%). After the order, equity must cover initial margin plus the taker fee plus the immediate loss of trading away from the mark |
| Mark price | Index from the node's oracle: the median mid of exchange tickers (Coinbase, Kraken, OKX, Binance.US, Gemini, Bitget, Binance, Bybit, or Pyth with an API key). Sources beyond 2% of the median are dropped, and at least 2 must agree. Marks are journaled commands, so replay never touches the network |
| Funding | Every `funding_interval_ms` (default hourly): `rate = clamp(average premium of book mid over index / 8, ±0.075%)`. Longs pay shorts when it's positive. Every holder pays the same integer amount per lot, so funding nets to zero exactly |
| Liquidation | Runs at every mark update and funding tick, for accounts with equity below maintenance. In order: (1) cancel the account's orders; (2) close its positions on the book within 3% of the mark; (3) the insurance fund takes any unfilled rest at the mark; (4) a 1% fee goes to insurance |
| Losses | Equity below zero is covered by the insurance fund. If that leaves the fund negative, the shortfall is clawed back pro rata, first from accounts in profit, then from positive equity |
| Settlement | At each batch, every account whose perp value changed (all holders, when a mark moved) settles `cash + Σ size × mark` as a collateral delta. Everyone is marked at the same price, so deltas net to zero, and solvency is enforced first, so on-chain balances never go negative. The contract needs no perp logic |

The NEAR on-chain oracles were checked on 2026-11-10 and can't drive marks. `priceoracle.near` had
no fresh prices, and `pyth-oracle.near`'s NEAR/USD was 77 days old. Pyth Hermes now needs an API
key.

## Performance

All figures measured on an Apple M5 Pro (18 cores) with client and server on the same machine.
Reproduction commands are in [docs/benchmarks.md](docs/benchmarks.md).

| What | Result |
|---|---|
| Engine alone, spot, 2M mixed ops | 6.7M ops/s. p50: place 83 ns, cancel 42 ns, IOC sweep 292 ns |
| Engine alone, perp, 2M mixed ops | 2.9M ops/s. p50: place 208 ns (margin check included), IOC sweep 917 ns, mark update with liquidation check over 1,000 holders 53 µs |
| Node over HTTP, 1 client, no fsync | p50 76 µs, p99 226 µs per order |
| Node over HTTP, 64 clients, no fsync | 99k requests/s, p50 603 µs |
| Node over HTTP, 64 clients, fsync on | 10.5k requests/s, 32 commands per fsync, p50 6.0 ms |
| Node over HTTP, 1 client, fsync on | p50 3.0 ms, which is macOS `F_FULLFSYNC` (full drive-cache flush) |
| Local chain, order ack to settled on-chain | ack 3.9 ms, settled 1.8 s later (300 ms batch interval plus finality) |
| Settlement gas | 1.96 + 0.286 Tgas per netted entry. Each withdrawal adds about 5 Tgas |

The fsync numbers are a Mac laptop's. They have not been measured on a Linux NVMe server.

## Why not a fully on-chain order book on NEAR?

Measured, not assumed: [research/](research/data) holds the raw data and the Rust harnesses are
`crates/obkit-near/examples/{onchain_bench,mainnet_survey}.rs`.

- **Tonic** (`v1.orderbook.near`). The benchmark runs the byte-identical mainnet wasm. Each order
  costs `2.61 + 0.0091 × resting orders` Tgas because the whole book is loaded and saved on every
  call. A taker order touching 64 makers fails on NEAR's 16 KB log limit. Its last order on
  mainnet was in January 2024.
- **Spin** (`spot.spin-fi.near`). About 8.3 Tgas per batched order on mainnet. Its last order was
  in September 2026, and only withdrawals have happened since.
- **NEAR mainnet**. Blocks every 0.57 s, and 1,000 Tgas per shard per block. One on-chain book
  tops out at roughly 150 to 670 orders/s depending on depth. A market maker requoting 20 orders
  a second would burn about 200 Tgas/s.

## SDKs

Rust:

```rust
use obkit_client::{Client, Order, Signer};
let c = Client::new("http://127.0.0.1:8794", "alice.near", Signer::from_near_secret(key)?);
c.place(&Order::limit("NEAR-USDC", "buy", 42_500, 10).tif("post_only")).await?;
c.withdraw_next("usdc.near", 5_000_000).await?;
```

TypeScript, in a browser app with NEAR Wallet Selector:

```ts
import { KeySigner, ObkitClient, ObkitFeed, registerKeyTx, depositTx } from "@obkit/client";
const key = KeySigner.generate();                                  // keep in IndexedDB
await wallet.signAndSendTransaction(registerKeyTx(contract, key.publicKey));
await wallet.signAndSendTransaction(depositTx(contract, usdc, 100_000_000n));
const client = new ObkitClient(api, accountId, key);
await client.place({ market: "NEAR-USDC", side: "buy", price: 42_500, qty: 10 });
const feed = new ObkitFeed(api, { onBook: (m, b) => render(b.topBids(), b.topAsks()) });
await feed.subscribeBook("NEAR-USDC");
```

Prices are integer ticks and quantities integer lots. Each market defines `base_lot` (base atoms
per lot) and `quote_tick` (quote atoms per lot per tick), so all arithmetic is exact. Give every
order a unique `client_id` (the TS client does this by default): two identical orders signed in
the same millisecond would otherwise have the same signature and the second one would be rejected
as a replay. The HTTP API is documented in [docs/api.md](docs/api.md).

## Running an exchange

```sh
obkit-near deploy --rpc https://rpc.testnet.fastnear.com --master you.testnet --master-key ed25519:... --out stack.json
obkit-near node-config --stack stack.json --data ./data/node > node.json   # edit markets, admin token
obkit-node --config node.json
obkit-near adapter --stack stack.json --node http://127.0.0.1:8794 --admin-token <token>
```

`stack.json` holds secret keys, so keep it out of version control. The node rebuilds its state from
`data_dir` on start: the newest snapshot, then the journal after it. The adapter has no state of its
own and can be restarted at any time. It checks the contract's `last_batch_id`, so it never submits
a batch twice.

## Tests

```sh
cargo test --workspace                                # engine, journal, node, client, CLI unit tests
cargo test -p obkit-near --test e2e                   # full lifecycle on a local nearcore sandbox (~3 min)
cargo test -p obkit-near --test e2e_perp              # perps settling real tokens on the sandbox (~2 min)
(cd contracts && cargo test)                          # settlement contract unit tests
(cd packages/client-ts && pnpm test)                  # TS SDK: Rust parity vectors + live test against the node
cargo bench -p obkit-engine                           # engine latency
cargo run --release -p obkit-node --bin obkit-loadgen # node latency/throughput over HTTP
```

What they cover:

- **Engine.** 13 spot and 9 perp scenario tests, plus two 300-case property tests.
  - They check invariants after every command, conservation, replay determinism, snapshot
    equivalence and zero-sum batches.
  - The perp property test applies every batch to a simulated on-chain ledger. It checks that no
    balance ever goes negative and that the ledger always equals the engine.
  - Its generator reaches liquidations, insurance takeovers, bankruptcies, socialization and
    funding. A coverage test checks this.
- **Node.** Integration tests for trading, REST and WebSocket feeds agreeing, auth failures
  (replay, stale timestamp, tampering, wrong key), batch cutting, crash recovery and concurrent
  clients.
- **End to end.** Runs on a real chain: deposits, trading, on-chain settlement, signed withdrawals,
  a failed payout being re-credited, a forced withdrawal, a node restart, and freeze plus escape
  after the operator stops.
- **Perp end to end.** On a local chain a long wins against a short and withdraws $1,039.58
  after depositing $1,000. An under-margined account is liquidated, chain balances equal engine
  balances throughout, and on-chain collateral is conserved.
- **TypeScript.** Byte- and signature-identical to Rust, and trades against the real Rust node.

## Limits

- **Perps are linear and USDC-margined only.** There are no inverse or quanto contracts, and no
  isolated margin. All perp markets share one collateral token.
- **The operator runs the oracle.** Marks are journaled and auditable, but not verified on-chain.
  The contract never needs a price: it only sees netted balance changes.
- **The operator decides ordering and matching (as with Orderly).** Batches record the journal
  sequence and hash they came from, so anyone holding the journal can replay and check them. The
  contract doesn't verify individual trades.
- **One node, no replication.** Durability comes from the journal on one machine. A hot standby
  that tails the journal is not built.
- **No TLS or admin UI.** Run the node behind a TLS proxy. `/internal/*` uses a bearer token and
  should not be reachable from the internet.
- **Test tokens only.** `mock-ft` exists for sandbox and testnet. Nothing has been audited.
