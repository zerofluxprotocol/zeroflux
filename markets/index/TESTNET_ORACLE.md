# zfx-oracle on NEAR testnet

Contract: `oracle.zeroflux.testnet` (code `markets/index/contracts/zfx-oracle`, built with
`cargo near build non-reproducible-wasm --no-abi`, 205,418 bytes). Relayer (pays push gas,
holds no contract rights): `oracle-relayer.zeroflux.testnet`. Owner: `oracle.zeroflux.testnet` itself.
Keys: `~/.config/zeroflux/oracle.zeroflux.testnet.json`, `~/.config/zeroflux/oracle-relayer.zeroflux.testnet.json`
(both 0600, outside the repo). RPC: `https://rpc.testnet.near.org`, `https://test.rpc.fastnear.com`
(fallback). `https://rpc.testnet.fastnear.com` rate-limited this workstream (HTTP 429) on 2026-11-11.

## Configuration on chain

- Reporter allowlist: `cadc2f85db7d936a1e993e13bd5da6dfcf5cad36a4688a5e76915c1f72f0ba48`
  (`keys/oracle-reporter.pub.json`), quorum 1.
- Indices: 132 on 2026-11-11. The first 56 were `or-model-spend-lic/<slug>`
  (`ZFX-OR-MODEL-SPEND-USD-LIC-v1`) and `or-model-spend-lic-7d/<slug>` (`ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1`)
  for the 28 tradable markets in `markets/zfx-perps/deploy/testnet/v2-markets.json`. The poller then
  registered 76 more with the owner key: the dataset 7-day index and the web 7-day index
  (`or-model-spend-7d/<slug>`, `ZFX-OR-MODEL-SPEND-USD-7D-v1`) of every tracked model. New models are
  registered automatically on their first push.
- Storage reserve (`min_free_yocto`): 0.2 NEAR. Pushes that would go below it are refused.

## Backfill

**Latest 7 days, not 30.** The window is 2026-11-03 to 2026-11-09, both indices for all 28 markets:
392 prints (all revisions in that window), all accepted. 30 days would have been 1,680 prints, about 4.57 NEAR of print
storage. On top of 2.05 NEAR of code and 0.18 NEAR of index registry, that is over the 5 NEAR budget.

Check (any machine): `zfx-oracle-near view --contract oracle.zeroflux.testnet --method latest --args '{"index_id":"or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921"}'`
returns 2026-11-09, value 619885.757986. Its `canonical_hex` and `print_hash` are byte-identical to
`public/v1/indices/or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921/latest.json`. The signature
verified with the independent JS codec (`js/oracle-print.mjs`).

## Transactions

| # | what | tx hash |
|---|---|---|
| 1 | create `oracle.zeroflux.testnet`, funded 4.80 NEAR by zeroflux.testnet | `QN5dAdvUo7AxbVFovceDzcyMk1XZzXooFUBpWHfjWYe` |
| 2 | create `oracle-relayer.zeroflux.testnet`, funded 0.15 NEAR | `3w94EAt996meK23kdWe4ZxvMQarKbN4fUMJdwKhGBjfo` |
| 3 | deploy 205,543-byte wasm (first build) | `HmUWomiygoiT5gsCnnqH8TkXznkKu2CSEmJ9qVLfAV8U` |
| 4 | `new` (owner, reporter, quorum 1, reserve 0.2 NEAR) | `2XFyydjr66S56ArPsi1V5a8Gs7rKBkWemQrnvLPPAAT4` |
| 5 | `set_indices` (20) | `FVk39vJvE11w4QbmJcE7fayhe2A4UADBfRB5JqvX2Lpf` |
| 6 | `set_indices` (20) | `9cCbh9HDu6ECgBB1tPLmwXR9rP3MmY5ehHpXWCVdNdBg` |
| 7 | `set_indices` (16) | `AreJjd5dCcH86wuh2xEspLMH5XWG6ba6v7NS6DpQ3BLk` |
| 8 | `push` 10 prints (first test batch, 40 Tgas prepaid) | `EvH8VbRLCfKDt9T5CjnjUneEnyvfEVX8nV7dtaopmRMi` |
| 9 | `push` 100 prints, **failed and reverted**: event log 20,856 bytes over NEAR's 16,384-byte total log limit | `6BGBDkPZBNU9tf8qpRDeCxLGV9UkcLVb9eatkj9V33Ao` |
| 10 | deploy fixed 205,418-byte wasm (compact events), state kept | `AHKrDS7rnqMsoGw6fMeCuz3HJ2GamcsNLyaoQeqCCHRi` |
| 11 | `push` 100 prints, 69.8 Tgas | `7c3Vje5hgmw366s8y8dxs6Loj6icb4yqiYovDpP6UdmH` |
| 12 | `push` 100 prints, 69.6 Tgas | `GsRZiYJYccgUJw2sMZJbMS74bHdxYeRA7CMxGDinPQzD` |
| 13 | `push` 100 prints, 69.7 Tgas | `FW9ZTB3o4zkuxxtvaQZ5fiPavKPhEAH3D6L1b4n3KXaz` |
| 14 | `push` 82 prints, 57.6 Tgas | `6mpCq9L4HcgYFqPRYyXF9iB9MTZPRAWk9D8aeoMs6M1C` |

Before #8, two push attempts with 300 Tgas prepaid were refused by the RPC as `InvalidTransaction`
and never reached the chain. NEAR reserves prepaid gas at a pessimistic price, so 300 Tgas needs more
balance than the 0.15 NEAR relayer holds. The pusher now prepays `20 + 1 Tgas per print`
(120 Tgas for 100 prints, against about 70 Tgas burnt).

Pushes made by the poller from now on are logged in `~/.local/share/zfx-index/near-push.jsonl`
(tx hash, prints, accepted, gas), outside the repo.

## Cost

| item | NEAR |
|---|---|
| spent from zeroflux.testnet (14.8646 to 9.9004) | 4.9642 (4.95 funding plus account-creation gas) |
| locked for code (205,418 bytes) | 2.054 |
| locked for config and 56 index entries (18,097 bytes, about 323 per index) | 0.181 |
| locked for 392 prints (106,624 bytes, 272 per print) | 1.066 |
| relayer gas so far (including the failed tx) | 0.033 |
| free in the contract above the 0.2 reserve | 1.293 |

**Runway.** Daily pushing of both indices for 28 markets adds 56 prints a day: 15,232 bytes, or
0.152 NEAR/day. The contract's free balance covers about **8 days**. After that the contract refuses
pushes until someone tops it up (any account can call `prepay` or transfer NEAR). Options:

- add about 4.6 NEAR per month for both indices;
- push only the dataset 7D (the traded index): 0.076 NEAR/day, 2.3 NEAR/month. To do that, set
  `"files": ["revisions_lic_7d.jsonl"]` in `~/.config/zeroflux/oracle-push.json` and disable the
  daily indices with `set_enabled`.

Relayer gas runs about 0.005 NEAR/day, so 0.117 NEAR lasts about 3 weeks.

## Measured gas and storage (sandbox, `crates/zfx-oracle-near/tests/sandbox.rs`)

| | gas |
|---|---|
| 1 print | 2.77 Tgas |
| 10 prints | 8.20 Tgas (0.82 / print) |
| 50 prints | 32.35 Tgas (0.65 / print) |
| 100 prints | 62.75 Tgas (0.63 / print) |
| 30 real dataset 7D prints | 0.69 Tgas / print |
| testnet, 100 real prints | 69.6 to 69.8 Tgas (0.70 / print) |
| in-wasm ed25519-dalek verification instead of the host function (10 prints) | 46.97 Tgas (4.70 / print, 6.3x; wasm 238,384 bytes) |

Storage: 272 bytes per print. That is 152 bytes of canonical tail plus a 65-byte signature entry plus
map overhead; the index_id and version strings are not stored per print. At 10^19 yocto per byte
that is 0.00272 NEAR per print.

## Operating commands

```sh
cd markets/index
N=./target/release/zfx-oracle-near
$N status --contract oracle.zeroflux.testnet
$N view --contract oracle.zeroflux.testnet --method history --args '{"index_id":"or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921","from_date":"2026-11-03","limit":10}'
./target/release/zfx-index push-near --ledger ~/.local/share/zfx-index/ledger --contract oracle.zeroflux.testnet \
  --signer oracle-relayer.zeroflux.testnet --key-file ~/.config/zeroflux/oracle-relayer.zeroflux.testnet.json --dry-run
```

## Funding

The coordinator sent 6 NEAR to `oracle.zeroflux.testnet` (balance 10.79 NEAR, 3.30 NEAR locked by
storage) and set the poller to push only the traded index, `or-model-spend-lic-7d` (config
`~/.config/zeroflux/oracle-push.json`, `files: ["revisions_lic_7d.jsonl"]`). That is 28 prints a day,
about 0.076 NEAR a day of storage: roughly 98 days before the next top-up.

Later on 2026-11-11 the poller started pushing both 7-day indices for every tracked model
(`files: ["revisions_lic_7d.jsonl", "revisions_7d.jsonl"]`, plus `owner` and `owner_key_file` for
registration). The first push registered 76 indices and accepted 531 prints in 6 transactions
(about 70 Tgas per 100 prints); the contract then held 923 prints. That is about 108 prints a
day, roughly 0.29 NEAR a day of storage. zeroflux.testnet sent 5 NEAR to the contract
(`27eVfkNVSAmHYEBZYp3oMAJ8Jdy5uznVpr1MJpGPHJKK`, 10.6 NEAR available, about 36 days) and 1 NEAR
to the relayer for gas (`HzHqtUVdnMp8L5jdjhXes6amaWmizuVxTGPtTokThmi7`). Top-ups:
`zfx-oracle-near transfer --from zeroflux.testnet --key-file <key> --to oracle.zeroflux.testnet --near 5`.

Spot check: `latest` for `or-model-spend-7d/x-ai/grok-4.7-20260916` (a model outside the dataset's
top 50) returns 2026-11-09, 35466.850595, FINAL, matching `public/v1/indices/or-model-spend-7d/...`.
