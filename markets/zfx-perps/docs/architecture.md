# Architecture

## Processes

| Process | Language | State | Talks to |
|---|---|---|---|
| `zfx-perps-node` | Rust | journal + snapshots in `data_dir` | clients (HTTP/WS), adapter (`/internal/*`) |
| `zfx-perps-near adapter` | Rust | none | node (`/internal/*`), NEAR RPC (operator key) |
| settlement contract | Rust (wasm) | balances, keys, nonces, events, forced requests | users' wallets, adapter |

## Inside the node

```text
HTTP/WS tasks ──try_send──▶ engine thread ──▶ journal thread ──▶ replies
 (tokio)        bounded      seq += 1          append group       publisher thread ──▶ WS fan-out
 verify sig     queue 65k    apply command     one fsync          L2 books, trades,
 replay guard                (speculative)     then release       key index, batches
 rate limit
```

- **Engine thread.** Owns the `Engine` and nothing else touches it. Each command gets the next
  sequence number, is serialized with bincode, extends the blake3 hash chain, and is applied. The
  serialized command, its events and its reply then go to the journal thread.
- **Journal thread.** Drains everything queued, appends it, and calls fsync once for the whole
  group. Only then does it send replies and pass events to the publisher. Under load one fsync
  covers dozens of commands (32 on average in the 64-client benchmark).
- **Speculative execution is safe.** A command that is applied but not yet durable has had no
  externally visible effect: no ack, no market data, no batch submission. If the process crashes,
  replaying the journal rebuilds exactly the acknowledged state plus possibly a few
  never-acknowledged commands at the tail.
- **Reads are ordered too.** `GET /v1/account` runs as a closure on the engine thread and is
  released by the journal thread, so a read sent after a write's ack always sees that write.
- **Publisher thread.** Builds read models from durable events (L2 books with a per-market
  sequence number, recent trades, the key and account index, pending batches) and broadcasts WS
  messages. REST book and trade reads never touch the engine thread.

## Determinism

`Engine::apply` is a pure function of state and command. The engine has no clock (timestamps
arrive inside commands), no randomness, and never iterates a hash map without sorting first.
The property test replays random streams from scratch and from mid-stream snapshots, and checks
`state_hash()`, which is independent of hash-map layout.

## Engine model

- **Prices and quantities.** Prices are ticks, quantities are lots, and notional in quote atoms is
  `lots × ticks × quote_tick`. Everything is integer math, using u128 for amounts.
- **Holds.** A sell holds `qty × base_lot`. A buy holds `qty × (price × quote_tick + ⌈taker fee per lot⌉)`.
  Rounding the fee up per lot keeps the hold linear in quantity, so releasing it on a partial fill
  always covers the fill's cost. A maker fee can't exceed the taker fee, so resting buys are
  covered too.
- **Fees.** Charged in quote, rounded up, and credited to account 0 (the configured fee account).
- **Matching.** Price-time priority. Book levels are kept in a `BTreeMap`, and orders at each level
  sit in a slab with an intrusive FIFO list.
- **Self-trade prevention.** `cancel_taker` stops matching and cancels the rest of the incoming
  order. `cancel_maker` cancels the resting order and keeps matching. `cancel_both` does both.
- **FOK.** Walks the book exactly as matching would (STP included) before it changes anything.

## Perpetuals

The code is in `crates/zfx-perps-engine/src/perp.rs`.

**Accounting.** Each account has a signed perp `cash` next to its collateral balance.

- A fill of `q` lots at `p` moves `q·p·tick` of cash from buyer to seller and `q` lots of position
  from seller to buyer.
- Fees, funding and liquidation fees are cash transfers.
- Everything is zero-sum, and positions net to zero per market, so the following holds at any
  time:

```text
equity(a) = available collateral + cash + Σ size · mark · tick
Σ_a (collateral + cash) = deposits − withdrawals
```

**Margin.** Cross margin:

- `IM = Σ max(|size + open buys|, |size − open sells|) · mark · tick · imr`;
- `MM = Σ |size| · mark · tick · mmr`.

Perp orders reserve margin, not balances. A spot buy or a withdrawal of the collateral token is
limited to `equity − IM`. Withdrawing profit first moves value from perp cash into the collateral
balance, with an offsetting settlement entry, so the chain sees the realization once.

**Mark updates (`SetMark`).** Each one records a premium sample, (book mid − index) / index, and
runs the liquidation check over every holder of the market.

**Funding (`FundingTick`).** The rate is the clamped average premium divided by `funding_divisor`.
The payment per lot is a single integer applied to every holder.

**Liquidation.** Steps, in order:

1. Cancel all of the account's orders.
2. Send an IOC for each position, bounded by `liq_slippage_bps` from the mark.
3. The insurance fund takes any unfilled remainder at the mark.
4. A `liq_fee_bps` fee goes to insurance.
5. Negative equity is covered by insurance.

**Insurance solvency.** After every mark update, funding tick and cut, the insurance fund must have
non-negative equity. Any shortfall is clawed back pro rata, from perp profits first and then from
positive equity.

**Settlement.** At each cut, before packing:

1. Resolve any negative equity.
2. For every account whose perp value changed, plus all holders of markets whose mark moved, add
   `view − last_view` to the collateral entry, where `view = cash + Σ size·mark·tick` at the
   current marks.

All holders of a market are marked at the same price, so the entries net to zero. Every settled
account is solvent at that mark, so `collateral + view ≥ 0` and on-chain balances stay
non-negative. The perp property test applies every batch to a simulated contract ledger to check
both properties.

**Cost.** When marks move, every position holder gets an entry each batch. At 0.29 Tgas per entry,
1,000 holders cost about 290 Tgas per batch interval. Size `batch_interval_ms` to the number of
holders.

## Settlement batches

The engine adds every fill to a netting map keyed by (account, asset):

- the buyer pays quote and receives base;
- the seller pays base and receives quote;
- fees go to account 0.

Each asset therefore sums to zero.

`CutBatch` (a timer sends one every `batch_interval_ms`) packs the map into batches:

- Entries are sorted by (account, asset) and packed a whole account at a time, so one account's
  entries never span two transactions and each batch keeps every balance non-negative on its own.
- Withdrawals and forced withdrawals go after all the deltas, at most `max_batch_withdrawals` per
  batch.
- The last batch of the group is marked `seal`. The contract requires each token's running total to
  be zero at the seal.
- `CutBatch` carries the journal sequence and hash at the cut, and the contract stores them.

Batches stay in the engine until a `batch_settled` event arrives from the chain. They survive
restarts because they're part of engine state.

## Chain events

The contract numbers every event: `deposit`, `key_added`, `key_removed`, `forced_withdrawal`,
`withdrawal_reverted`, `batch_settled`. The adapter forwards them in order, and the engine applies
them strictly by sequence:

- a duplicate is a no-op;
- a gap is refused;
- an event the engine can't apply (for example an unlisted token) still advances the cursor, so
  the stream never stalls.

## Journal format

- **Segments.** Files named `journal-<first seq>.log` hold records of
  `[len u32][crc32 u32][seq u64][bincode(Command)]`.
- **Hash chain.** `h_n = blake3(h_{n-1} ‖ seq_n ‖ payload_n)`.
- **Snapshots.** `snap-<seq>.bin` is written to a temp file, fsynced, then renamed. The newest
  three are kept.
- **Recovery.** Load the newest valid snapshot and replay records after it. A torn or corrupt tail
  in the last segment is truncated. Corruption anywhere earlier is an error.

## Settlement contract

The contract stores:

- balances per `(account, token)`;
- trading keys, each owned by exactly one account (at most 16 per account, 0.01 NEAR deposit
  refunded on removal);
- a withdrawal nonce per account;
- numbered events, which the operator prunes once the node has consumed them;
- forced-withdrawal requests, with a cursor at the oldest open one.

`settle_batch` (operator only, not while frozen):

1. Requires `id == last_batch_id + 1` and that the journal sequence doesn't go backwards.
2. Applies the deltas. Any balance going negative fails the transaction. Each token's running total
   is tracked.
3. If the batch is sealed, requires every token's running total to be zero.
4. Processes withdrawals, which only pay out if all of these hold:
   - the token is listed;
   - the key is registered to the account;
   - the nonce is greater than the stored one;
   - the ed25519 signature over `zfx-perps-withdraw-v1|contract|account|token|amount|nonce` verifies;
   - the settled balance covers the amount.

   A withdrawal that fails a check is skipped and reported as `withdrawal_reverted`, so the engine
   credits it back. The batch still goes through.
5. Pays out with `ft_transfer`. If the transfer fails, the callback re-credits the balance and emits
   `withdrawal_reverted`.
6. Processes forced withdrawals: each must match its request's account and token, pay at most the
   requested amount, and its request must not already be done.
