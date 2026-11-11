# zfx-perps on NEAR testnet

Testnet only, test tokens with no value. Deployed and exercised on 2026-11-11 (UTC) from this
Mac. Secret keys are in `~/.config/zeroflux/` (stack file, trader keys, admin token), never in the
repo.

## Code

Deployed contract: `contracts/zfx-perps-clearing` built at commit `2e7ad1f` (branch `ws/perps`),
428,391-byte wasm. This build predates the coordinator's v2 rules (notional open-interest cap,
print-move close-only flag, non-FRESH prints as CLOSE_ONLY); see "v2 not yet on testnet" below.

## Accounts

| Account | Role |
|---|---|
| `zeroflux.testnet` | Owner (admin, timelocked loosening) and funder of every account below |
| `perps.zeroflux.testnet` | Clearing contract (5 NEAR, ~4.3 locked for storage) |
| `tusdc-perps.zeroflux.testnet` | Mock collateral token `tUSDC`, 6 decimals, open faucet (test only) |
| `perps-operator.zeroflux.testnet` | Operator / relayer key (submits batches; can't move funds) |
| `perps-maker.zeroflux.testnet` | Trader (market maker side) |
| `perps-taker.zeroflux.testnet` | Trader (taker side) |
| `insurance.perps.zeroflux.testnet` | Ledger-only insurance account (not a NEAR account) |

Contract init: chain id `near:testnet`, one allowlisted reporter, the ZFX index reporter key
`cadc2f85db7d936a1e993e13bd5da6dfcf5cad36a4688a5e76915c1f72f0ba48`
(`ed25519:Eet67qpH1vwWFLCC9sFxfzAnPqumVhWQGn2cpgpeBvUw`), quorum 1, timelock 60 s, forced
withdrawal timeout 24 h, settlement grace 300 s. Deploy and init transaction hashes were not
captured (the deployer didn't record them, and the nearblocks v1 API it would have used was
retired on 2026-11-05). The deployer now records them and keeps the contract key for testnet
redeploys.

## Markets

| Market | Index | Listed | Activated (timelock proposal executed) |
|---|---|---|---|
| `OPUS-PERP` | `or-model-spend/anthropic/claude-opus-5.5-20260921` (daily, informational) | hash not captured | hash not captured |
| `OPUS-7D-PERP` | `or-model-spend-7d/anthropic/claude-opus-5.5-20260921` (web 7-day, alternate source) | `6it7FKsdMyU3zTfTmKJ6sKyiSvgPs6hyTSgJA1Mh1To` | `FYB8Vw8tueLJQhicRLJ6s7jZzdLLhSupuG3PfELgCvd7` (proposal 2) |
| `OPUS-LIC7D-PERP` | `or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921` (licensed 7-day, the reference index) | `EJqsZ2oUUhSrmtZ5ZBuaR2WH7qMQ1QmM7WNwUPHJXwQd` | `8eXdmk2f4srJR83iLwtFUoMqV9CBrNcAzw8t62MCKYgN` (proposal 3) |

Parameters: `deploy/testnet/opus-perp.json`, `opus-7d-perp.json` and `opus-lic7d-perp.json` (2x, IM 50%,
MM 25%, 20% / 15% mark limit, 50% band, 1-day print lag, close-only from 22:00 UTC). The 7D
market's lot cap of 1,750,000 lots is `max_open_interest_usd` ($1,102,088.76 from `models.json`)
at the 7D mark of 629,765 ticks. The licensed market's cap is 1,750,002 lots: the licensed
`max_open_interest_usd` of $1,084,800.08 at its mark of 619,885 ticks.

## Run 1: OPUS-PERP, real daily print

Print: Claude Opus 5.5, day 2026-11-09, $582,093.352339/day, hash
`39df75b8edb96521d4337c2bf368830d18e782ea1b2a9fff1a0ec2fb06573d2d`, published
2026-11-10T23:40:13.579Z, valid until 2026-11-12T00:00Z, reporter signature verified by the
contract. Mark 582,093 ticks.

| Step | Transaction |
|---|---|
| create `perps-maker` | `J9Ab6qP6m7Re6i2LE3H739BzBExPPXhjy6jJEmNMMxBZ` |
| create `perps-taker` | `GyVrgMBdgnyw1Uw7iBsqLbUp7ts5qqXtUF1fsrU9LByK` |
| maker: register trading key | `EphJmW6Fg3mJZ3KuGsrXZUVrNx38AVfuyQWiA79mDk6t` |
| maker: faucet mint 1,000 tUSDC (first and second attempt) | `CUtjFFmt86NcHPCCDz51DrbnYrpwGNkdcCCBEiPdNMUz`, `AHvBCfm57enDp8472euYUUo14gGks9vJPFKBMBHhTMx8` |
| maker: deposit 1,000 tUSDC (`ft_transfer_call`) | `5Dd1DZGQAPsyDjxeoBy9HD9zncTf7wJnPqD7Sn63BXj2` |
| taker: faucet mint | `BohWtrerJm4cUCsaMfsCBof9NbMHmCqbpo3aRJnTr2zp` |
| taker: register trading key | `BrDy1QAzJXBcmVLJ8oWjaFcrMXzApCGvcBL16cMKC5JW` |
| taker: deposit 1,000 tUSDC | `7FoFjNVkgbwsB15YUWRZ6t3frV5PHnYcsGVnTPqLiFSB` |
| batch 1: credit maker deposit (2.93 Tgas) | `fxxH1jf5HJYftxP15HefJxjajpFrpo27rgRsnft4vTN` |
| batch 2: credit taker deposit (2.93 Tgas) | `9XvcF752nTQcEcA4aJ8En8p2wTzbMPvmUy2Vsf45aU1x` |
| batch 3: oracle print + open fill, 2 signed orders verified (5.43 Tgas) | `FHjWEv9ZG788PeBLLTysL24QwLjDLgfgSucZpgpE3h76` |
| batch 4: close fill, 2 signed orders verified (5.29 Tgas) | `9NM6kY2vhdd9f8FSbrfqZjF1toRmKGEVrVWZPSxwhMwh` |
| batch 5: signed withdrawal + token payout (7.84 Tgas) | `415NNu68BJnvTXAkE9MBsjdxALzvhFpyPFZwqVdryQrH` |

Orders, signed by the traders' trading keys through `@zeroflux/zfx-perps-sdk`:

- open: maker post-only SELL 1,000 lots @ 582,675 (`587a31f3...`), taker IOC BUY @ 582,675
  (`81003f50...`);
- close: maker post-only BUY 1,000 @ 583,839 (`17005a66...`), taker IOC reduce-only SELL
  (`c708ded4...`).

Reference math (only prices and fee rates; tick value 1 atom): notional 582,675,000 then
583,839,000; taker PnL +1,164,000, taker fees 291,338 + 291,920, maker fees 116,535 + 116,768.

| Account | Expected free (atoms) | On-chain `get_account.free` |
|---|---|---|
| taker | 1,000,580,742 | 1,000,580,742 |
| maker | 998,602,697 | 998,602,697 |

The taker withdrew 1,000,580,742 atoms (1,000.580742 tUSDC). Its token balance rose by exactly
that amount.

## Run 2: OPUS-7D-PERP, real 7-day print

Print: `or-model-spend-7d/anthropic/claude-opus-5.5-20260921`, window 2026-11-03 to 2026-11-09,
$629,765.005054/day, hash `9584b280ff568f351781f7c437124e8435c7b49a27b07c09cb0e53aaeea00d68`,
published 2026-11-11T00:19:19.221Z. Mark 629,765 ticks.

| Step | Transaction |
|---|---|
| maker: faucet mint, deposit | `6ssY43n6JCnFR9fsErzb4EXtPnpY9YZveji3bww6GDv4`, `BCDMHdCcZoDGpCZuZwSMNTn7VAQimfWyPU3bzTWKZ7W7` |
| taker: faucet mint, deposit | `G3nRDjmH9M8oUJ6s1mbUGt19Fhj2haj6MdfusyvfSNA6`, `5SmmFHDWMaHYkdhMu5SPveqqZvyYbt8dnbAykCW4BRrG` |
| batches 6-7: credit deposits | batch 6 `h6WV5sEKrqcbos7XNwrKtnAgTjACvdzJpwCD7ihmEMj`, batch 7 `64DdiHBQ4UR2Fjym9RyiKrkLzMNX57LqbLZFBxGMEzkW` |
| batch 8: print + open (5.45 Tgas) | `4FNbBZ3DwFo998BQC2Pmqvh2uQoXq8rS9MSMHUCXjfRP` |
| batch 9: close (5.30 Tgas) | `4rd9cP1Zg1rEiFUTL35vvMCj35KyhvzPGBPUKLD1HDMg` |
| batch 10: withdrawal (7.85 Tgas) | `5tiPdqj8rNMsoU5JjLoqtijoLLFgMNsKdpN1YfJvjLWD` |

Open at 630,394, close at 631,654, 1,000 lots.

| Account | Expected free | On-chain |
|---|---|---|
| taker | 1,000,628,976 | 1,000,628,976 |
| maker | 1,997,090,287 | 1,997,090,287 |

## Run 3: OPUS-LIC7D-PERP, real licensed 7-day print (the reference index)

Print: `or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921`
(`ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1`), $619,885.757986/day, hash
`e6538439acbba621c6fef8c343f08ee29881383f949ef95176b8756f5bfd85d2`, published
2026-11-11T00:33:23.058Z. The node and relayer were restarted from the run 1 and 2 journal
before this run, using v1 binaries built from `2e7ad1f`. Mark 619,885 ticks.

| Step | Transaction |
|---|---|
| maker: faucet mint, deposit | `EEnNdYnHJ9psnnTd7euB8DgRMnSMHKWDcQqcd2HTW8AX`, `3BAF7YVwaPfmDDQqqEEBLVwbMU5YspDDihzdxGgycrre` |
| taker: faucet mint, deposit | `DFcga8YmfPGeBSGR5wqaGbj7rtYPbE2QrzCEzJYp8oNb`, `ADVTwiA4iXXM2Xy4m1SYQv5aFRune7SfhZ9FMBw1gHRd` |
| batches 11-12: credit deposits | `8etABPg71rZx4tqNNhfNNZiBRV5nFNfrSMRgpnKg3r3d`, `ErRDkE3XBhkNxn1yM7cVfNSgvnbiCGAghGWnRfjEBcqc` |
| batch 13: oracle print (3.12 Tgas) | `FFXLSdM1q4VZiDiDQKncVdPwq71GStbXaFiimTS1UEuH` |
| batch 14: open (5.16 Tgas) | `78i6rMmTCaUH7kmYahBXULYTLZVLgVaDq27WsTQrbNYH` |
| batch 15: close (5.31 Tgas) | `GKroyHKoY2vFZ1jrCwBt1Ax8nitSsgSJiAnSXp62dovE` |
| batch 16: withdrawal (7.85 Tgas) | `6XTwA7bkxDrWo4UKFjnvDUfjGPBEjysamZi9mNFuXifP` |

Open at 620,504, close at 621,744, 1,000 lots.

| Account | Expected free | On-chain |
|---|---|---|
| taker | 1,000,618,876 | 1,000,618,876 |
| maker | 2,995,601,837 | 2,995,601,837 |

## Contract totals after all three runs

- `fee_balance` = 2,569,569. That is the hand-computed sum of all twelve fees: 816,561 + 883,434
  + 869,574.
- `total_collateral` = 2,998,171,406, which is 6,000,000,000 deposited minus 3,001,828,594
  withdrawn.
- `last_batch_id` = 16, with no rejected ops.

Full step logs with timestamps: `research/data/testnet_e2e.json`, `testnet_e2e_7d.json`,
`testnet_e2e_lic7d.json`; every relayer batch is in `research/data/testnet_relayer_batches.jsonl`. Reproduce with `packages/sdk-ts/examples/testnet-e2e.ts`
(see `README.md`).

## v2 not yet on testnet

The current code adds the coordinator's rules: `max_open_interest_notional` (set from
`max_open_interest_usd`), `print_move_close_only_bps` (CLOSE_ONLY after a >25% licensed-7D move
until the next clean print), and every non-FRESH print as CLOSE_ONLY. These change the market layout and
policy hash, so they need a fresh contract account. The deployed contract has no upgrade method
and its account key was not kept.

A v2 contract reusing the existing token and operator needs about 5 NEAR. zeroflux.testnet has
1.91 NEAR. Ready to deploy: `deploy/testnet/opus-lic7d-perp-v2.json`, generated by
`deploy/testnet/params_from_models.py`.

The running v1 deployment also differs from current code in one place: a DISPUTED print HALTS a
v1 market instead of making it CLOSE_ONLY.
