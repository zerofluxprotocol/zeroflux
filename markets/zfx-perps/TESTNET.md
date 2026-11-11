# zfx-perps on NEAR testnet

Testnet only, test tokens with no value. Secret keys are in `~/.config/zeroflux/`, never in the
repo: stack files with the owner, operator and contract keys (0600), trader keys, admin tokens.

## Current deployment: `perps-v2.zeroflux.testnet` (2026-11-11)

Code: this branch after `d193c46`, with every rule in `docs/ORDERBOOK_AND_CLEARING.md`:
- the licensed 7-day index;
- the notional open-interest cap set from `licensed.index_7d.latest.max_open_interest_usd`;
- CLOSE_ONLY after a print that moves the index more than 25%, until the next clean print;
- CLOSE_ONLY on any non-FRESH print.

The wasm is 430,255 bytes. Chain id `near:testnet`. One reporter, the ZFX index key
`cadc2f85db7d936a1e993e13bd5da6dfcf5cad36a4688a5e76915c1f72f0ba48`
(`ed25519:Eet67qpH1vwWFLCC9sFxfzAnPqumVhWQGn2cpgpeBvUw`), quorum 1. Timelock 60 s, forced
withdrawal timeout 24 h, settlement grace 300 s.

| Account | Role |
|---|---|
| `zeroflux.testnet` | Owner and funder |
| `perps-v2.zeroflux.testnet` | Clearing contract (6 NEAR; its full-access key is kept in the stack file for testnet redeploys) |
| `perps-v2-operator.zeroflux.testnet` | Operator / relayer (1 NEAR) |
| `tusdc-perps.zeroflux.testnet` | Collateral `tUSDC` (mock NEP-141, 6 decimals, open faucet), shared with v1 |
| `perps-maker.zeroflux.testnet`, `perps-taker.zeroflux.testnet` | Test traders |
| `insurance.perps-v2.zeroflux.testnet` | Ledger-only insurance account |

### Deployment transactions

| What | Transaction |
|---|---|
| create account perps-v2-operator.zeroflux.testnet funded 1.00 NEAR by zeroflux.testnet | `8r4ZkD9XfUVFj2LVsZT9vZPnBv2kh121TiBYntaJrZ9X` |
| create account perps-v2.zeroflux.testnet funded 6.00 NEAR by zeroflux.testnet | `4hBeY9rEF6cwiL2qhrJak1uCpKtB4akyq8BrTVRUMVHv` |
| deploy 430255-byte wasm to perps-v2.zeroflux.testnet | `BxXUfchNoEon1yapFJUHsPZqB8DkWWp3mfpTU3RefDmC` |
| new on perps-v2.zeroflux.testnet by perps-v2.zeroflux.testnet | `DLYVYXMaa96CMe6XNCMKWefh22sDRK4GszfREeR4xq2M` |
| storage_deposit on tusdc-perps.zeroflux.testnet by zeroflux.testnet | `2P5Y1Tii9QCyrE4z9QGar67s2g3CnzXGnwfQ7498EzuC` |

### Markets (28 tradable licensed-7D markets)

The markets are the ones `deploy/testnet/gen_v2_markets.py` selected from `models.json`
(generated 2026-11-11T00:33:32.233Z): licensed 7D index
OK and an OI cap of at least $10,000. That is 28 of 54; `deploy/testnet/v2-markets.json` lists why
the other 26 aren't tradable. Parameters are in `deploy/testnet/v2-markets/<id>.json`. Each was
listed with `create_market`, its activation proposed with `set_market_status ACTIVE`, and the
proposal executed after the 60 s timelock. `GPT-5-6-LUNA-0709` was then set CLOSE_ONLY
(`9RZvgnQsYci8yWjJoka2Gtk1Y88zmgD3ePkj3kwjVCDe`): its licensed 7D print on 2026-11-09 was clamped (−40.4%), and a first on-chain
print has no predecessor to compare against. It reopens through a timelocked proposal after a
clean print.

| Market | Permaslug | OI cap (USD) | create_market | propose ACTIVE | execute |
|---|---|---|---|---|---|
| `CLAUDE-HAIKU-5-5-1007` | `anthropic/claude-haiku-5.5-20261007` | 39,741.70 | `FZD1yuVG2LVNrv4unjdcou9QUkj6uABSJgesB4vRQU4S` | `EFZBrCjakPB7YNvGa6HryWR27r3yun3Sd3ZucpQkHmdG` | `3LgennFFAGymJA9PhGNqCX9JDeJ95YeUddVBAaTn41ys` |
| `CLAUDE-OPUS-5-5-0921` | `anthropic/claude-opus-5.5-20260921` | 1,084,800.08 | `7MguyCaAGpF9HpzXAK9GQQaD75M6xwt5EAKVJ4LhfwD` | `AhsNCKZJCdUKxZxreFCmvMd71Q4Spza9Qj55Nv7siERK` | `AsWj3XgLFyu67KBc1hnv6mvVp9T1g5HqtovTbbcQATeV` |
| `CLAUDE-SONNET-5-5-0928` | `anthropic/claude-sonnet-5.5-20260928` | 294,630.66 | `ALPo1DszxDa7HsccP4QmzEsta2UNB8XeGM8nuZhcESsa` | `33W4YRiBuje55eevGys7QXLtSfqwc5ve8DKDpYcqdsDQ` | `CBJX2PjT3Kxz4rrhh98c8NbyTU4trLZA6ugGr9U7ngZs` |
| `DEEPSEEK-V4-1-FLASH-0910` | `deepseek/deepseek-v4.1-flash-20260910` | 166,727.20 | `FdqeLfk1zCvwnXTRCi3iM1JTJyUtBWsWSX4j365eeLNa` | `GK75CmgRD2jcY9JHXvNqd6bLAuhoAUVG4BS2NXV3dGgn` | `DCEjQraWBYsMqW646cVWw2J3eofKbH7hkyCNTihvEDp9` |
| `DEEPSEEK-V4-1-FLASH-1017` | `deepseek/deepseek-v4.1-flash-20261017` | 301,419.61 | `4yEosuAjQAkZyQmgSxXhNeCHFNweVJaBVpEWnxC6SnpA` | `BGhUeGnUq5iJzSUSK4uwc2JhoXJbwSAq9SDaX4Xm6vxw` | `8WqKCteZ8W3cgnzedHn2ttm3TVt1QkBEuqJ5SbLyURs` |
| `DEEPSEEK-V4-FLASH-0423` | `deepseek/deepseek-v4-flash-20260423` | 35,850.71 | `DiZHfq8jm75keB8kNxV7U8LpettHgD8RpMywMeSdfSQK` | `BaFmHcXiYN3bA6XL8FJc8UvT8xeGiacWi3HMj6nUawGe` | `HvsxCHavHUerH2ePn2sK1B1baAyxh936VkKQ7UyEiWzM` |
| `DEEPSEEK-V4-FLASH-0731` | `deepseek/deepseek-v4-flash-20260731` | 52,783.14 | `7w9iyy1wBycQFRE2i4wzWbPrPSvpD5tjdT9eGgTE55jS` | `qApaUEXvCJvgQpj1N5RXz9Xe99JTJ1CgpMagcHmcHKa` | `m1djACPXT1vyQWC24H5P99bWagZ2X1eek9c9VnBp3T8` |
| `DEEPSEEK-V4-PRO-0423` | `deepseek/deepseek-v4-pro-20260423` | 48,320.40 | `5xJtMiW1zhBMnd7Y4s6giMxn4yN1NXQydAebWJsu6HKx` | `ABQoWyFmMegLEJwmifdC3dDrvMsVVvKJ7sRJniLsZrnG` | `Hy7NPiKD7YDEFgJUpv9rLPzr5QtC6MBf2WhuGsrNFJKZ` |
| `DEEPSEEK-V4-PRO-0813` | `deepseek/deepseek-v4-pro-20260813` | 74,042.64 | `4MAi7MGtVgZzoD2aiBeqk8E6dYQ8NwfMaLVdTEcWxBqi` | `AqwU83kCmWpm7e1YHeobokCyhxmJM5kW19Dhmd5URoYK` | `F2ReLwK3QzwX6YtwyNpqiDNMSuhD2GwM73cLKJwL5pTG` |
| `GEMINI-3-1-FLASH-LITE-0507` | `google/gemini-3.1-flash-lite-20260507` | 26,557.16 | `8iridDnyTqF2yvDCXgb5L1p9xDNvPREPof3PQxM3NrGm` | `DAhoTWcqq5cLaK8YtZnFoX59viYNhYzQL63Cqy4pfHWJ` | `FkjpHwdyet3erFKxR4EozEp3ZZWaKfJXtumRNGFVaCY` |
| `GEMINI-3-8-FLASH-0902` | `google/gemini-3.8-flash-20260902` | 153,839.48 | `7YS2cJGTUgSpTWJTL9yS8aAF676tHjSGZPqZntzy3FS5` | `23kaN5rgaLSiymQLtD38d9qyR3w7jStM9xFsZzhEtB5W` | `93vg6TnKQua3zyGELgXyDetTLQ7EEN5LsHFKG1QCSQc7` |
| `GEMINI-3-FLASH-PREVIEW-1217` | `google/gemini-3-flash-preview-20251217` | 56,012.84 | `84ohKv34SpkRAUvtKYZ533PTv67K9tSn27RiajQvAzv3` | `D8nxF5n4tHTZZ4ERqPGrNwZotTMt73PZCAEFmhi4D52p` | `8pVDMETYVHvrSBJQshw3GJ63pTvZtHqSfWZxYutpadh9` |
| `GEMINI-4-ARGON-1013` | `google/gemini-4-argon-20261013` | 183,005.65 | `GXy6VZQrCxf8MExqcfEZMjnerb9NfEj1LAkxHx39yWxT` | `2xMrZWBLmMPyh2NawMBWdxGHoyNfx2qsiyyoEJRUKgF1` | `BkERxFPHP511Gzh2z1ACcKsBDFEqNircLp3MSdbSuZBo` |
| `GEMMA-4-31B-IT-0402` | `google/gemma-4-31b-it-20260402` | 16,469.96 | `DihkCyuq67EKgsbwKLnFpU6fAoXYmjZiZhc4uNffou3n` | `93nvQkfmvx4rV8Xd76Sj67i7Q8M1aM8zju931jSWMPCN` | `3CA3pBMHndfg7Mm8MupqrpXPPR2Lx6ZKQUL2TWT5tgEV` |
| `GLM-5-2-0616` | `z-ai/glm-5.2-20260616` | 103,194.98 | `FrFtH9TNJXZxoKJh76Vv7pDv7DCSrSFucMe9zSorjrvA` | `F6JZMhM9DW3Eo3v5T4LvAWKeWWMA75RYZ21LCS1n3WzR` | `6VeUUCmVZUWjZDpc11aLrCqi7f8KxAVGXVXALJyT7og7` |
| `GLM-5-3-0816` | `z-ai/glm-5.3-20260816` | 244,802.85 | `dYTYRLvgab7BsSYM3xBtEXXAG2twuAibvDVRW9eKkx6` | `H72SDnH5PQo6U3T9ZwUW2VU9NCFEq6SwvNTUDaLCsvT5` | `25ioxJ8WmXQrFiZBjteDitdrpRUSSThujjaMUhJcQrzT` |
| `GLM-5-3-FLASH-0826` | `z-ai/glm-5.3-flash-20260826` | 174,535.96 | `C3EYafSdcNQk6fktCgCnRt4DV3Tb5bscKGnWRaUbRiK9` | `7nCTBPrEbnKjEVEgGPmdZuxmw1LxbmZWE8a4q6NBNarX` | `6VrYZ1JCQK5Kba4cgtHZqD8NZjZZSiLtYMuxXtui8KUn` |
| `GPT-5-6-LUNA-0709` | `openai/gpt-5.6-luna-20260709` | 36,725.74 | `9woJfcd6keVscse1Na3tMFiNzaU1ecJ7cSNwAkdRezHZ` | `5uNmvoyDpGjdBvDMLowS5SAzu4u2xuMNGfbwGatWZ3xH` | `Ghiiyy6E7cyhu6djyJmwA18D3yJS1RKwEyeK21WJ9CJE` |
| `GPT-6-1-SOL-0929` | `openai/gpt-6.1-sol-20260929` | 425,946.55 | `2wK49pTe7X9gq8XVAvzXwAJzvxRCEbeW8uqsEutrSdZD` | `FjfFLPUgw5z1UzsHPwonGrdiPt4aNBySD26dEcm5UgNX` | `9mFGDskm1ArSHtAJVKbZks4UgCKvs8DFB2RPn8zgUDNs` |
| `GPT-6-1-TERRA-1026` | `openai/gpt-6.1-terra-20261026` | 52,654.76 | `Dce352de8Tw8QWNXhRu5FdRzkbGjuLZDtw8qFnRScTMJ` | `3EShWL325GKp25fGX3LSALoVLPnhazxUy4zCYxvQAMvd` | `AFGWmXnwCobW3w3McxwzWHFxus4mMBDGMpKKsCDm5Yun` |
| `GPT-6-LUNA-0922` | `openai/gpt-6-luna-20260922` | 70,533.35 | `DMAgZJmNjUamYThEjKnS3S8EB422bRPjAFRJGvCGz8yi` | `6vw6C8Gi5A25q7rCgvd2Gbs6CzJUti1UUpthEzYYWjtv` | `HmX8GgSMaMpzmFhgUV1mQmyZ1KQVSewyQFoMAxhSVHBq` |
| `KIMI-K3-0715` | `moonshotai/kimi-k3-20260715` | 300,093.31 | `6q4PTTAJn1gXZHpGd2tV2MjLmuLp1gQJS9U1D4cJRhb4` | `HC4viXJ3UzKcMjHGmw2gNKRGD6iVbumgQcL3kpvSfXcm` | `GFQTJABG7VjjCbMZzigiESCPk1HZYA2qHEtd8njXiejN` |
| `KIMI-K3-1-PRO-1023` | `moonshotai/kimi-k3.1-pro-20261023` | 405,081.26 | `C9ZjuMu6LuF48ZfanxUHikcVvndSTbXZ6czDhtRimK1K` | `4xTefSogMnzjizBc3DzhnKTfj8Y3i3yszCFdebHe1nRy` | `GqqnzULg5JZTcvzuMRs5DersRPDUHrAUtBdyDwRxnZZL` |
| `MIMO-V2-6-PRO-0921` | `xiaomi/mimo-v2.6-pro-20260921` | 17,501.68 | `2KShQUFojZRErbnmfRv92Bxkj9s6o5iDKALKnh1vGwyB` | `BCQW6gns7DdFhaSrSujAdjUdrwKZYFS6RncGsmJijNZH` | `FSFpE9qGCQct74P9EXZxfEx7Dk5bjMgVHoWQsQD77EA8` |
| `MUSE-SPARK-1-3-CONTRIBUTOR-0902` | `meta/muse-spark-1.3-contributor-20260902` | 10,116.98 | `AZKRhBFUSdpBaTV3CA2bjCgCStRuYbsYDbLzkkSEFiLE` | `DZ4udWaKnLz3G1aW39XTjVxkazSuosZ1FPvSBkGNmEAn` | `GSbz7HCtMyziyzW5tdA9a9eNuREPquPwhXgR92gpPK61` |
| `NEMOTRON-3-5-SUPER-VL-1015` | `nvidia/nemotron-3.5-super-vl-20261015` | 13,221.04 | `DbusYjtQWdD8G6cjBqzonWeKqWLnGkgXpbwWvTbETc8f` | `5QU8QtujdjB72PEZ3wjfimsJE4bsFEiKLZDDishmm39u` | `8RLn3tzSEaQZEk5teWYocmkNHb4fydQjAmgPCjsyN9rv` |
| `QWEN3-8-27B-0814` | `qwen/qwen3.8-27b-20260814` | 28,765.65 | `FASnqDtXuTGbEvSXoYPVPujhJJnU7sbp7fm8eq4wBKXK` | `C5NVWZZ8jnD6qfGYvns8Tn8M5bzvofKZJQRkNhsi6E5a` | `yY7H8ZAyYUiw13ZwwnFiPhcyUABxE6NpEcqx7GYE2Ax` |
| `QWEN3-8-FLASH-0826` | `qwen/qwen3.8-flash-20260826` | 10,341.01 | `HGpbRXrtm8f1ZaAr7xzrcPjCTd3My31cuBubcLFkwpKi` | `3cK2hWvQbwirFzPW1HNfdtEATZZmnqm9KPJz62epZ1wP` | `31dhPhR5y12VkFAKHd9SJXJK93ncjLzRz2daoqLawCYe` |

### Settlement batches (relayer, `research/data/testnet-v2/relayer-batches.jsonl`)

| Batch | Contents | Tgas | Transaction |
|---|---|---|---|
| 1 | 28 oracle prints (every market's signed licensed-7D print, from the node's print feed) | 18.41 | `9j4HqLdWfYqZXPAAPxuWh6F4VhULCK2xnUY6VUz2GMqx` |
| 2 | credit maker deposit | 3.02 | `5hPeWUbanY6WGB8gVVNsxFsVNm7MLgkUnYimuugGWNeA` |
| 3 | credit taker deposit | 3.02 | `DReKaDdrhTTjw3ykqNcHVAosaZRk5W8wdmRdzG9tPeEU` |
| 4 | open fill (2 signed orders verified) | 5.23 | `GPU3c3qQrE1tndG1ibbLEXRmDMKc2miibjENW6aprqHa` |
| 5 | close fill (2 signed orders verified) | 5.37 | `6vQUVFqsyzW5DMPoKsvevmfeWQP9UHcdiDLjScr7rDKc` |
| 6 | signed withdrawal + token payout | 8.04 | `D614LF3hMysdAWvbE5gut4sXJHYnwL35nuKbDc47WxD2` |

### Public-SDK open and close (`research/data/testnet-v2/e2e.json`)

Market `CLAUDE-OPUS-5-5-0921`. Print: `or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921`
(`ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1`), $619,885.757986/day, hash
`e6538439acbba621c6fef8c343f08ee29881383f949ef95176b8756f5bfd85d2`. `checkOracle` in the SDK
verified it against the contract's own reporter allowlist. Mark 619,885.

| Step | Transaction |
|---|---|
| faucet_mint perps-maker.zeroflux.testnet | `3cHw265bGWMhM5iXtiSvtUnfjuryAzux3ruHupPUB83M` |
| register_trading_key perps-maker.zeroflux.testnet | `ADBshoFZAikmhpuusQmHhGRikAzqKTvSPcxwAGKBTFX3` |
| deposit perps-maker.zeroflux.testnet | `FzCN2bfYbvZY6YFpHvb6xa1ytG2ZoAwxzSYJ1p2pYAor` |
| faucet_mint perps-taker.zeroflux.testnet | `3n2ZCahV97bjHXNeihMvQfLJXiW4AtmFyZBdwvnvKWL8` |
| register_trading_key perps-taker.zeroflux.testnet | `FXyrfwmsxzBwKSjnNZ3naK4G4xCFSEXEigQynatKbsuF` |
| deposit perps-taker.zeroflux.testnet | `D1oqdMwcFuXUJy3YkLnda68Ko71N2tD9XUKKJs17baxD` |

Orders, all signed in the SDK with registered trading keys:

| Order | Hash | Fill |
|---|---|---|
| maker_post_only_ask | `1ab027717f559fa00db45150b0dab58bd2968a4c2ff2b5c2914af290d32fcba7` | 620504 resting |
| taker_ioc_buy | `ed5c8fd38c164859fc59e9f19ee589074b41507ddb6db6102baa28c8f1aaf2ce` |  filled 1000 |
| maker_post_only_bid | `00d56730a2bad5a7581f9001d19d8ab00b7210aee4c855ea74cff3976274dcc1` | 621744 resting |
| taker_reduce_only_sell | `d4019ba6d06be8c4822c54d11a7b5829e8b44fcb66e2c952f6747d633bef48cc` |  filled 1000 |

The reference math uses only order prices and fee rates (tick value 1 atom, 1,000 lots):
- open at 620,504 and close at 621,744, so the taker's PnL is +1,240,000;
- taker fees (5 bps, rounded up): 310,252 + 310,872;
- maker fees (2 bps): 124,101 + 124,349.

| Account | Expected free (atoms) | On-chain `get_account.free` | Match |
|---|---|---|---|
| taker | 1,000,618,876 | 1,000,618,876 | True |
| maker | 998,511,550 | 998,511,550 | True |

The taker withdrew 1,000,618,876 atoms. Its tUSDC balance rose by exactly that amount.

Contract totals afterwards:
- `fee_balance` 869,574, the sum of the four fees;
- `total_collateral` 999,381,124, which is 2,000,000,000 deposited minus 1,000,618,876 withdrawn;
- `last_batch_id` 6, no rejected ops, no open positions.

NEAR spent on v2: 7.04 (21.906 to 14.865 on zeroflux.testnet). Most of it is storage held by the
contract and operator accounts.

## Superseded: `perps.zeroflux.testnet` (v1)

Deployed earlier on 2026-11-11 from commit `2e7ad1f`, before the design updates. Superseded by
`perps-v2.zeroflux.testnet`; do not point venues at it. It enforced the open-interest cap in lots
only, halted on a DISPUTED print, had no print-move rule, and its account key was not kept. It
holds three markets (`OPUS-PERP` daily, `OPUS-7D-PERP` web 7D, `OPUS-LIC7D-PERP` licensed 7D),
each opened and closed through the SDK with balances matching reference math. Its record follows,
unchanged.


Testnet only, test tokens with no value. Deployed and exercised on 2026-11-11 (UTC) from this
Mac. Secret keys are in `~/.config/zeroflux/` (stack file, trader keys, admin token), never in the
repo.

### Code

Deployed contract: `contracts/zfx-perps-clearing` built at commit `2e7ad1f` (branch `ws/perps`),
428,391-byte wasm. This build predates the coordinator's v2 rules (notional open-interest cap,
print-move close-only flag, non-FRESH prints as CLOSE_ONLY); see "v2 not yet on testnet" below.

### Accounts

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

### Markets

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

### Run 1: OPUS-PERP, real daily print

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

### Run 2: OPUS-7D-PERP, real 7-day print

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

### Run 3: OPUS-LIC7D-PERP, real licensed 7-day print (the reference index)

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

### Contract totals after all three runs

- `fee_balance` = 2,569,569. That is the hand-computed sum of all twelve fees: 816,561 + 883,434
  + 869,574.
- `total_collateral` = 2,998,171,406, which is 6,000,000,000 deposited minus 3,001,828,594
  withdrawn.
- `last_batch_id` = 16, with no rejected ops.

Full step logs with timestamps: `research/data/testnet_e2e.json`, `testnet_e2e_7d.json`,
`testnet_e2e_lic7d.json`; every relayer batch is in `research/data/testnet_relayer_batches.jsonl`. Reproduce with `packages/sdk-ts/examples/testnet-e2e.ts`
(see `README.md`).

### v2 not yet on testnet

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
