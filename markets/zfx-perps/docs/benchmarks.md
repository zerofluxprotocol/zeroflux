# Benchmarks

Machine: Apple M5 Pro, 18 cores, 48 GB, macOS. Client and server run on the same machine, so
end-to-end numbers include the load generator's own CPU use. All runs are from 2026-11-10.

## Engine

`cargo bench -p zfx-perps-engine`. 1,000 accounts. The book is pre-filled with 20k orders, then 2M
operations run: 55% passive limits, 35% cancels and 10% IOC sweeps of 1 to 3 levels. Each
`apply` is timed individually, including timer overhead.

| Op | n | p50 | p99 | p99.9 |
|---|---|---|---|---|
| place (passive) | 1.10M | 83 ns | 167 ns | 1.4 µs |
| cancel | 700k | 42 ns | 209 ns | 292 ns |
| IOC sweep | 200k | 292 ns | 750 ns | 1.0 µs |

Throughput: 6.7M ops/s on one core, with 877k fills.

The same flow on NEAR-PERP, with 1,000 accounts and a mark update every 1,000 ops, gives 2.9M
ops/s with 942k fills and 1,000 position holders:

| Op | p50 | p99 | p99.9 |
|---|---|---|---|
| place (margin check included) | 208 ns | 667 ns | 2.3 µs |
| cancel | 42 ns | 291 ns | 417 ns |
| IOC sweep (position updates) | 917 ns | 2.3 µs | 4.4 µs |
| mark update, liquidation check over 1,000 holders | 53 µs | 66 µs | 85 µs |

Spot throughput dropped from 7.7M to 6.7M ops/s after perps were added, because spot buys now
also check perp margin. The p50s didn't change.

## Node over HTTP

`cargo run --release -p zfx-perps-node --bin zfx-perps-loadgen -- --clients N --seconds 10 [--no-fsync]`.
Same mix as the engine benchmark. Orders are signed before the timer starts. Each request goes
through signature verification, sequencing and the journal, and the reply waits for durability.

| Clients | fsync | Requests/s | p50 | p99 | Commands per fsync |
|---|---|---|---|---|---|
| 1 | off | 9.6k | 76 µs | 226 µs | 1 |
| 64 | off | 99k | 603 µs | 1.4 ms | 6.8 |
| 1 | on | 298 | 3.0 ms | 6.0 ms | 1 |
| 64 | on | 10.5k | 6.0 ms | 8.1 ms | 32 |

With fsync on, macOS uses `F_FULLFSYNC`, which flushes the drive's write cache and takes about
3 ms. Group commit spreads that cost over every request that arrives meanwhile. On Linux, `fdatasync`
on NVMe usually takes much less time, but that has not been measured.

## On-chain settlement

`cargo run --release -p zfx-perps-near --example onchain_bench` on a local nearcore sandbox.

| Batch | Gas |
|---|---|
| 4 entries | 3.4 Tgas |
| 16 entries | 6.7 Tgas |
| 48 entries | 15.9 Tgas |
| 128 entries | 38.8 Tgas |
| 1 withdrawal | 7.4 Tgas |
| 8 withdrawals | 43.2 Tgas |

Fit: 1.96 + 0.286 Tgas per entry. Each withdrawal adds about 5 Tgas, including the `ft_transfer` and
its callback. One entry is one (account, token) net change per batch, however many trades
produced it.

## Same benchmark, fully on-chain (Tonic)

The benchmark deploys `v1.orderbook.near`'s exact mainnet wasm. Its code hash,
`2HL5pnkfUhyeAjmjAK9XPjNxH3WaGxSnbwaYVn6kNiY9`, matches the deployed contract.

- **A resting order** costs 2.61 + 0.0091 × resting orders Tgas: 2.6 Tgas on an empty book and
  5.6 Tgas at 300 orders.
- **A taker order sweeping k makers** costs 6.1 Tgas at k=1, 15.4 at k=16 and 28.3 at k=56. At
  k=64 it fails: `The length of a log message 18039 exceeds the limit 16384`.
- **A cancel** costs 3.2 to 3.5 Tgas.

Mainnet, from `cargo run --release -p zfx-perps-near --example mainnet_survey`:

- **Chain.** Protocol 88, 10 shards, 0.57 to 0.59 s mean block time, 1,000 Tgas per shard per
  block, gas price 1e8 yocto (1 Tgas = 0.0001 NEAR).
- **Spin `batch_ops`.** Median 8.3 Tgas. Last order 2026-09-06.
- **Tonic `new_order`.** Median 8.25 Tgas. Last order 2024-01-05.

Raw data is in `research/data/`.

## End to end on a local chain

`cargo test -p zfx-perps-near --test e2e -- --nocapture`. An order was acked in 3.9 ms over HTTP, and
the batch containing its fill was settled on-chain 1.83 s later. The batch interval was 300 ms, and
the rest was sandbox block production and finality.
