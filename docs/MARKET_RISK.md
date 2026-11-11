# zfx-perps market risk

Parameters for model-index perpetuals, how they were derived, and stress results. Testnet and
paper trading only (spec 5.4). Every number below comes from a file in
`markets/zfx-perps/research/data/` and can be reproduced with the commands at the end.

## 1. What the index does day to day

Source: the index workstream's 30-day backfill of `ZFX-OR-MODEL-SPEND-USD-v1`, 50 models,
2026-10-11 to 2026-11-09 (`research/data/index_daily_moves.json`, from
`research/risk/derive_params.py`). The figures are day-over-day changes of the daily index. That
is the jump a position takes when the next print lands.

| Statistic (1,376 daily moves, 50 markets) | Value |
|---|---|
| median absolute move | 13.2% |
| 90th / 95th / 99th percentile absolute move | 44.5% / 63.1% / 178% |
| largest fall | −97.0% |
| largest rise | +9,029% (a small model's launch day) |
| share of days moving more than 20% / 50% / 100% | 33.6% / 7.6% / 2.1% |
| markets with at least one move over 40% in 30 days | 42 of 50 |

For one large, established market, Claude Opus 5.5, the largest 30-day move is +44.9% and the
largest fall −33.1%; the 95th percentile absolute move is 33.1%.

This history is short: 30 days, with backfilled prices of low confidence (`DAILY_ENDPOINT_REWEIGHTED`,
`docs/DATA_METHOD.md` section 6). The tails are fat and skewed upward, since new models grow quickly. A short loses
more than 100% of notional on about 2% of days. Ordinary leverage is not defensible on these
markets.

Model perps reference the dataset 7-day trailing index (`or-model-spend-lic-7d/<permaslug>`,
`ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1`), built from documented OpenRouter sources only. It averages
daily values, so its day-over-day moves are much smaller: Opus's latest dataset 7D change was
−0.4%. The index side's jump clamp is 25%, the 99th percentile of 22,654 observed 7D moves. The
web 7D index remains an alternate source. The parameters below are sized on the daily moves,
which bounds the 7D moves.

A market is tradable only if its dataset 7D index is OK and its OI cap is at least $10,000. On
2026-11-11 that was 28 of 54 models: 15 failed on the index status and 11 on the cap
(`deploy/testnet/params_from_models.py` refuses the rest).

## 2. The mechanism that bounds losses: limit moves

A print moves the mark at most `mark_limit_bps` toward the index (a "limit move"). The market
stays CLOSE_ONLY until the mark reaches the index. The contract refuses to list a market unless

```
mark_limit_bps + liq_slippage_bps + (max_funding_ppm / 100) × fundings per day  <=  maintenance_margin_bps
```

A position that isn't liquidatable has equity of at least `MM` of its notional. The next print
moves the mark by at most `mark_limit`, and a liquidation fills at most `liq_slippage` worse than
the mark. One day of funding costs at most `funding`. So the position is still at zero equity or
above when it is liquidated. Deficits, socialization and bad debt can then only come from
liquidations that run late: the operator is down across prints, or batches are delayed.

The cost is time. After a large move the market spends one or more print cycles CLOSE_ONLY. In
the stress replay that was 127 to 149 market-days out of about 900 (section 5).

## 3. v1 parameters

Tier rule: the largest daily move in the trailing window (at least 10 days of data):

| Tier | Largest move | IM | MM | Max leverage | Mark limit | Listed |
|---|---|---|---|---|---|---|
| A | ≤ 50% | 50% | 25% | 2x | 20% | yes |
| B | ≤ 100% | 100% | 50% | 1x | 45% | yes |
| C | > 100%, or < 10 days | | | | | no |

Both tiers satisfy the solvency rule: 2,000 + 300 + 75 ≤ 2,500, and 4,500 + 300 + 75 ≤ 5,000.

Per-market parameters for the testnet markets (`deploy/testnet/*.json`):

| Parameter | Value | Why |
|---|---|---|
| `initial_margin_bps` / `max_leverage` | 5,000 / 2 | Tier A: Opus's largest 30-day move is 45% |
| `maintenance_margin_bps` | 2,500 | Above limit + slippage + a day of funding |
| `mark_limit_bps` | 2,000 (1,500 on the web-7D testnet market) | Solvency rule: 2,000 + 300 + 75 ≤ 2,500 |
| `print_move_close_only_bps` (v2) | 2,500 | The dataset 7D jump clamp. A print moving the index more than 25% keeps the market CLOSE_ONLY until the next clean print |
| `price_band_bps` | 5,000 | Fills may be up to 50% from the stale daily mark. The book has to be able to price the next print, whose move exceeds 44% on 10% of days |
| `liq_slippage_bps` | 300 | Liquidations fill within 3% of the mark, otherwise insurance or ADL |
| `liq_fee_bps` | 100 | To the insurance fund, capped at equity left |
| maker / taker fee | 2 / 5 bps | Inherited from obkit; testnet only |
| `funding_interval_sec` / divisor / cap | 8 h / 24 / 2,500 ppm | Book-vs-index premium paid three times a day. The cap of 0.75% a day is small next to the daily jump, so funding never dominates PnL |
| `oracle_max_age_sec` | 108,000 (30 h) | One publication every ~24 h; 6 h of slack before CLOSE_ONLY |
| `print_lag_days` / `print_close_only_from_sec` | 1 / 79,200 (22:00 UTC) | The print for day d was first seen at 23:08 to 23:40 UTC on day d+1 (one live sample). The market is CLOSE_ONLY from 22:00 until it lands. That is provisional until 30 days of latency are measured |
| `halt_jump_bps` | 100,000 (10x) | Defense against bad data beyond the publisher's own 10x outlier block |
| `max_open_interest_notional` (v2) | 25% of the 7-day spend window | The index side's manipulation-cost bound (`licensed.index_7d.latest.max_open_interest_usd` in `models.json`). For Opus this is $1,084,800.08 |
| `max_open_interest` (lots) | 1,750,002 on `OPUS-LIC7D-PERP` | The same cap in lots at the listing mark, for the v1 contract. Re-derive it when the mark moves |
| `max_position` | 1,000,000 lots | About $630k at the mark; a single account can't hold most of the OI cap |

`deploy/testnet/params_from_models.py` regenerates a 7D market's parameters from `models.json`.
A lower cap applies at once through `update_market`; a higher one waits out the timelock.

The open-interest cap prices manipulation. Pushing the 7D index by 1% costs about 1% of the
7-day window's spend (the publisher's `manipulation_cost_usd_per_pct`, $43,392 for Opus on the dataset index). With
open interest at most 25% of the window, a 1% push gains a manipulator at most 0.25% of the
window at 1x, or 0.5% at 2x. That is less than the cost of the push. It doesn't make manipulation
impossible: the bound assumes manipulation shows up as billed spend, and it ignores positions held
elsewhere.

## 4. Insurance fund and loss waterfall

The order is: position margin, then the insurance fund's free collateral, then pro-rata
socialization over the profiting side (the full list is checked against open interest
on-chain), then recorded bad debt, which halts the market. There is no protocol backstop past
that. The stress replay seeds the fund at 2% of open notional. At that size the fund can margin
very few takeovers, and most liquidated size goes to ADL at the mark: 205 ADL fills against 7
takeovers out of sample. ADL at the mark costs the targets their unrealized gain beyond the
mark, but not principal. A fund large enough to take over the largest single position (at IM)
would replace most ADL. Sizing that requires owner and treasury decisions on real funds, which are
out of scope on testnet.

## 5. Stress replay on real history

`crates/zfx-perps-clearing/tests/stress.rs` replays every market's real index series print by
print. Each market has 20 isolated positions: 10 long, 10 short, margin from IM up to 2×IM, about
$10,000 notional each. After each print, every position below maintenance is liquidated through
the operator's planner (`plan.rs`) and the contract's rules. Tiers are fitted on the first 15 days
and replayed on the last 15 (out of sample), and also on the whole window.
`research/data/stress_results.json`:

| Out of sample (last 15 days) | v1 tiered | tiered, no mark limit | inherited 10x |
|---|---|---|---|
| markets traded (not listed) | 38 (10) | 38 (10) | 48 (0) |
| open notional | $25.0M | $25.0M | $32.0M |
| liquidations | 212 | 241 | 558 |
| positions closing below zero equity | 0 | 20 | 380 |
| deficit total | $0 | $196,531 | $6,738,897 |
| paid by insurance / socialized | $0 / $0 | $67,891 / $111,422 | $955,372 / $4,705,084 |
| bad debt (markets) | $0 (0) | $17,217 (1) | $1,078,441 (23) |
| market-days CLOSE_ONLY after a print | 127 | 0 | 0 |

| In sample (30 days) | v1 tiered | tiered, no mark limit | inherited 10x |
|---|---|---|---|
| markets traded (not listed) | 32 (17) | 32 (17) | 49 (0) |
| deficits / bad debt | 0 / $0 | 10 / $966 | 474 / $1,900,237 (36 markets) |

The test asserts zero deficits and zero bad debt for the tiered policy in both windows. Without
the mark limit, the same tiers lose money out of sample: a market fitted at ≤ 50% moved more than
100% in the replay. With the inherited 10x defaults, about half the markets end in bad debt.

Other risk scenarios covered by tests:

| Scenario | Test |
|---|---|
| Bankruptcy gap: insurance pays, then socialization over the full profiting side; an incomplete list is refused | `liquidation_on_book_then_insurance_then_socialized` |
| Insurance exhaustion: an empty fund and no counterparties left leave bad debt and halt the market | `insurance_takeover_and_fee_then_bad_debt_halts` |
| Oracle 24 h staleness: CLOSE_ONLY, no liquidation and no funding on the stale mark | `not_reported_stale_and_disputed_prints_restrict_trading`, `funding_is_zero_sum_and_pauses_when_stale` |
| Daily print window, lagged publication | `daily_window_and_jump_halt`, `lagged_daily_print_window` |
| Clamped 7D day | `large_print_move_is_close_only_until_a_clean_print`; `print_move_rule_matches_published_clamp_on_real_history`: the contract's rule (raw print values, strictly more than 25% from the previous OK value) equals the publisher's `clamped` flag on all 882 OK dataset-7D days of 54 markets, 4 of them clamped |
| Manipulation-cost OI cap | `open_interest_notional_cap_from_manipulation_cost` (core), `admission_rejects_what_the_contract_would` (engine) |
| Cascading liquidations: 300 random runs with ±60% prints; coverage counts 945 liquidations, 662 takeovers, 281 ADL, 104 socializations, 17 bad-debt events, all conserving | `props.rs::generator_reaches_liquidation_waterfall` |
| Engine against contract under liquidation cascades and on-chain races | `engine_tracks_chain_exactly` (120 cases), `print_drop_liquidates_and_chain_agrees` |

## 6. What this does not establish

- 30 days of history, with backfilled prices, is not enough to set real-money margins. The
  tiers must be re-fitted on at least a year of the keyed dataset once available.
- Publication latency is one live sample, so the 22:00 window is a placeholder.
- The index is predictable before it prints: the day's activity is visible on OpenRouter's site
  during the day. Traders with that view can position before the print; the close-only window and
  the 50% band reduce but don't remove this. It is a structural reason model perps stay paper only.
- The insurance fund size and the ADL ranking are operator and owner policies with no data behind
  them yet.

## Reproduce

```sh
cd markets/zfx-perps
python3 -I research/risk/derive_params.py ~/zfx-wt/index/markets/index research/data/index_daily_moves.json
cargo test --release -p zfx-perps-clearing --test stress -- --nocapture
```
