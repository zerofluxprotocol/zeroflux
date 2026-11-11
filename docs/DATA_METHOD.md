# ZFX Markets data method: `ZFX-OR-MODEL-SPEND-USD-v1` and `-LIC-v1`

Status as of 2026-11-11: published, charted, paper-trading indices. **Not settlement-grade.**
Two variants. The dataset index (`LIC`, section 13) reads only OpenRouter's documented daily
dataset (`datasets/rankings-daily`) and documented price listings, and is the recommended market
reference (section 2). The web index (sections 3 to 12) reads OpenRouter's public web endpoints
(the ones openrouter.ai pages call), which can change shape without notice. No live or
leveraged market may settle on either.

Code: `markets/index/`. Formula text (hashed into every print): `markets/index/formula/ZFX-OR-MODEL-SPEND-USD-v1.txt`.
Print format: `markets/index/docs/ORACLE_PRINT_V1.md`.

## 1. What the index measures

For each listed model and each UTC day, an estimate of the US dollars that OpenRouter
customers were charged for that model on that day, computed as

```text
index_usd[m, d] = prompt_billed[m, d] x P_in_effective[m, d] / 1e6
               + completion_billed[m, d] x P_out_effective[m, d] / 1e6
```

with token counts in tokens and prices in USD per million tokens. `billed` means total
tokens minus bring-your-own-key (BYOK) tokens, because BYOK requests are billed by the
upstream provider, not by OpenRouter.

Market label: *"<model>: OpenRouter-implied daily billings proxy, USD/day"*.

### What it is not

- Not the model's revenue, the provider's revenue, or the model creator's earnings.
- Not royalties and not equity. Holding a position is a bet on an index level. It gives no
  claim on any company's cash flows or any share in a model.
- Not worldwide demand. It covers requests routed through OpenRouter only.
- Not real time. There is no intraday value. The UI shows the last completed UTC day.
- Not measured billings. It is an estimate built from token counts and token-weighted
  effective prices (spec mode B), not from invoices (mode A, which needs billed-dollar totals
  that no public endpoint provides).

## 2. Sources

Two variants are published side by side. As of 2026-11-11 the dataset index exists, and it is the
one markets should reference.

| | Dataset index (`LIC`) | Web index |
|---|---|---|
| index ids | `or-model-spend-lic/<slug>`, `or-model-spend-lic-7d/<slug>` | `or-model-spend/<slug>`, `or-model-spend-7d/<slug>` |
| formula | `ZFX-OR-MODEL-SPEND-USD-LIC-v1`, `-LIC-7D-v1` | `ZFX-OR-MODEL-SPEND-USD-v1`, `-7D-v1` |
| tokens | documented `datasets/rankings-daily`, API key | web endpoint `/api/frontend/v1/stats/model-activity` |
| prices | documented no-auth `/api/v1/models/{id}/endpoints` (catalog fallback), listed prices | web endpoint effective prices `/api/frontend/v1/stats/effective-pricing` |
| mix | frozen per model (`ESTIMATED_FIXED_MIX`) | observed per day (`ESTIMATED_OBSERVED_MIX`) |
| coverage | models in that day's dataset top 50 only | any model |
| history | daily tokens from 2025-01-01 | daily tokens for the last ~30 days |

### Why markets settle on the dataset 7-day index

Markets should reference **`ZFX-OR-MODEL-SPEND-USD-LIC-7D-v1`**. Reasons:

1. Every input comes from an endpoint OpenRouter documents for programmatic use, with a
   published schema and rate limit. The web endpoints have neither and can change shape
   without notice.
2. Its token totals are identical to the web endpoint's: 1,174 of 1,174 overlapping market-days
   match exactly (section 13.2). So the dataset index loses no token accuracy. What it loses is
   observed prices and the observed daily mix.
3. Its level tracks the web index to a median ratio of 0.97 (section 13.3). That is good enough
   for a demand index, and its day-to-day moves come from tokens rather than from a daily
   effective-price feed.
4. It has 22 months of history (from 2025-01-01). That history sets its clamp bound
   (section 13.4) on evidence, not on a guess.

Limits:

- The dataset index needs the model in the top 50 on every day of its 7-day window. On
  2026-11-09, 12 of the 54 tracked models were `NOT_REPORTED` in the dataset index (the 8 at web
  ranks 52 to 60 and the 4 listings of 2026-11-10). Those models keep a web index, which is
  published and pushed to the oracle (section "Tracked models").
- The web index stays published as a cross-check and as the only source of observed effective
  prices and of the BYOK, cache and mix shares that were frozen into `LIC-v1`. A new `LIC`
  version that refreshes those shares needs web data again, or the extra fields listed in
  section 10.

### Dataset and price listings

- Datasets API documentation (openrouter.ai/docs, "Datasets"): the dataset is published under
  CC BY 4.0, and republished values must cite *"Source: OpenRouter (openrouter.ai/rankings), as
  of {as_of}."* Limits: *"30 requests/minute per key and 500 requests/day per account."*
- The models and endpoints APIs (`/api/v1/models`, `/api/v1/models/{id}/endpoints`) are documented
  API endpoints that need no key. The dataset index publishes only derived values from them (the
  median listed price per side, with attribution), not the raw listings.

How the key is handled: it is read from `OPENROUTER_API_KEY` or `~/.config/zeroflux/keys.env`
(0600), sent only in the `Authorization` request header, and never logged, put in a URL, or
stored. The ledger keeps response headers from a fixed allowlist only (a unit test asserts it
contains no `authorization` or `cookie` header). A search of `fetches.jsonl` for the key prefix
returns nothing. Request budget: at least 6 s between requests (at most 10/min, under the
30/min limit) and at most 450 dataset requests per rolling 24 h. That leaves 50/day for other
use of the same account.

### Web endpoints

The web index polls the `/api/frontend/*` endpoints that openrouter.ai's pages call, at about
180 requests/day, at least 6 s apart plus 0 to 4 s of jitter, capped at 400 per rolling 24 h
(`source-health.json` reports the count). The dataset index never reads the `model_activity`,
`effective_pricing` or `rankings_day` fetches; it uses web data only through the shares frozen
into `LIC-v1`, so it keeps running if the web path is turned off.

The poller sends no cookies and does no login, browser emulation or challenge solving. The
User-Agent says what it is:
`zfx-index/0.1 (+https://github.com/zerofluxprotocol/zeroflux/tree/main/markets/index; public-data index research)`.

### Attribution

Every published value carries `Source: OpenRouter (openrouter.ai/rankings), as of <timestamp>`.
For the dataset index the timestamp is the dataset's `meta.as_of`; for the web index it is
the token response's `cachedAt`.

## 3. Endpoints

Discovery: headless Chrome loaded `/rankings`, `/models`, `/apps`, a model overview,
its `/activity` and `/providers` tabs, and a second model's activity page, recording every
request (script `markets/index/tools/discover.mjs`, manifest
`markets/index/discovery/endpoints.json`, 40 API paths seen).

| Endpoint | Documented | Auth | Used for |
|---|---|---|---|
| `GET /api/v1/models` | yes | none | catalog: canonical slug, name, author, Hugging Face id, listed price |
| `GET /api/v1/models/{author}/{slug}/endpoints` | yes | none | listed price per provider: dataset index prices (daily snapshot), web index price band |
| `GET /api/v1/datasets/rankings-daily` | yes | API key | dataset index tokens (top 50 + `other` per day, from 2025-01-01; max 366 days per request) |
| `GET /api/frontend/v1/stats/model-activity?permaslug=&variant=standard` | **no** | none | per model, per UTC day, last 31 days: prompt, completion, cached, reasoning tokens, request count, BYOK prompt/completion |
| `GET /api/frontend/v1/stats/effective-pricing?permaslug=&variant=standard&shape=v7&range=` | **no** | none | trailing-window token-weighted effective input/output price, per-provider token totals, per-endpoint daily effective prices since launch (`range=all`) |
| `GET /api/frontend/v1/rankings/models?view=day` | **no** | none | every model's prompt/completion totals for the last completed UTC day (653 rows on 2026-11-09, not just the top 50) |
| `GET /api/frontend/v1/rankings/models?view=day&metric=spend` | **no** | none | OpenRouter's own spend share per model (fractions summing to 1.0, no dollar amount); used as a cross-check |
| `GET /api/frontend/v1/rankings/model-rankings-chart` | no | none | not used: top 10 weekly only |
| `GET /api/frontend/v1/stats/listed-pricing` | no | none | not used: listed price history per endpoint |

Caching seen on the web endpoints: `model-activity` is served with
`s-maxage=10800` (CDN copies up to 3 h old) and carries `cachedAt` in the body;
`effective-pricing` uses `s-maxage=600`; `rankings/models` uses `max-age=300`. The index uses
the source's own time (`cachedAt`, else `Last-Modified`, else `Date` minus `Age`), not fetch
time, to decide completeness and close timing.

Fields **not available** from any endpoint found: invoice or billed-dollar totals per model,
refunds or credits, cached-token *price* per day, per-endpoint token counts per day,
historical listed prices by day for the whole model, provisional intraday increments with a
finality policy.

### Tracked models: how they enter and leave

The dataset decides which models enter; the web data keeps tracking them after that.

- **Enter.** Each time the poller fetches the dataset, every model in its newest day (the top 50)
  that has a catalog entry, a non-zero listed price and text output is listed if it is not
  already tracked. Free variants, stealth models and `other` are skipped. The `LIST` event in
  `ledger/markets.jsonl` records the dataset and catalog fetches it came from. Without an API key
  the rankings day view is used instead.
- **Stay.** A tracked model is polled through the web endpoints every day whether or not it is
  still in the dataset's top 50, so its web index continues when it drops out.
- **Leave.** A `RETIRE` event stops polling when the model is gone from the newest catalog, or
  when a model-activity response fetched after the day closed shows zero tokens on each of the
  last 7 days and the dataset lists it on none of them (`poll::retire_reason`). Models listed in
  the last 7 days are never retired. A retired model that reappears in the dataset is listed
  again. Its history stays published.
- **Check by hand.** `zfx-index universe --ledger <dir>` prints the models the dataset would add
  and the models that would be retired; `--apply` writes the events.
- **On chain.** Each push first registers the 7-day index ids of every tracked model that the
  oracle does not know yet (`zfx_oracle_near::ensure_indices`, owner key), so a new model's
  prints are accepted on its first push.

On 2026-11-11 the dataset's newest day (2026-11-10) listed 51 rows. 42 were already tracked;
the other 9 are free variants, stealth or uncatalogued models, or `other`, so nothing was added
and nothing was retired.

## 4. Data coverage of the committed snapshot

- Backfill run 2026-11-10 23:11Z to 23:32Z, 153 requests, all HTTP 200.
- Markets: 50. Universe rule: from the 2026-11-09 rankings day, the top 50 `standard`-variant
  models by total tokens that exist in the documented catalog with a non-zero listed price and
  text output. Free, stealth and unpriced models are skipped, so listing ranks run 1 to 60.
- Days: 2026-10-11 to 2026-11-09 (30 UTC days). `model-activity` returns 31 rows, the last of
  which is the current partial day and is never used. Models launched later start later
  (fewest: 6 days).
- History limit: daily token counts go back only about 30 days on these endpoints. Prices go
  back to each model's launch (`range=all`). Older daily token history is not available
  without the documented dataset (which covers from 2025-01-01 per the spec).
- Revisions: 1,429 day records, 1,421 signed prints. 1,426 OK, 3 UNPRICED, 0 NOT_REPORTED.
  8 blocked (section 7).
- Fields per day: prompt, completion, total, cached prompt, reasoning, requests, BYOK prompt,
  BYOK completion, billed prompt/completion, observed mix, effective input/output price,
  price basis and endpoint weights, value, rank, checks, input hashes.

## 5. UTC boundary

Day `d` is `[d 00:00:00.000Z, d+1 00:00:00.000Z)`. Every day is exactly 86,400,000 ms; there
is no time zone and no DST anywhere in the code (`time.rs`, tested on the 2026 US and EU DST
dates). The source's day labels (`"2026-11-09 00:00:00"`) are accepted only at midnight; any
other hour rejects the response. A response is used for day `d` only if its source time is at
least `d+1 00:00Z + 6 h` (grace) and the day is before the response's own as-of day.

## 6. Formula details

### Mode

`ESTIMATED_OBSERVED_MIX`. The prompt/completion split is observed per model per day. No
assumed split is used anywhere. (`ESTIMATED_FIXED_MIX` is implemented and tested but not
used; `OBSERVED_BILLINGS` needs billed-dollar totals, which no public endpoint provides.)

### Prices

The source's effective prices already include cache-read discounts: for Claude Opus 5.5 the
effective input price is about $0.75/M while listed input prices are $4/M, because 89% of
prompt tokens were cache reads. That is why the formula multiplies **all** prompt tokens
(cached or not) by the effective input price, and does not add a cache term.

Two price bases, in order of preference:

1. `CLOSE_TRAILING_24H`: the effective-pricing summary (`weightedInputPrice`,
   `weightedOutputPrice`) observed in the last 60 minutes before `d+1 00:00Z`. Its window is a
   trailing ~24 h (the summary's provider token totals were 1,065B for Opus 5.5 vs 951B for
   the UTC day), so at 23:00Z to 24:00Z it nearly matches day `d`. The latest such
   observation is the close. Confidence MEDIUM.
2. `DAILY_ENDPOINT_REWEIGHTED`: the per-endpoint effective prices the source labels with day
   `d`, averaged with weights equal to each endpoint's token total from the response's summary.
   Those weights describe a recent trailing window, not day `d`. Confidence LOW. All 1,426
   backfilled OK days use this basis. Close observations for 2026-11-10 were captured for
   every market between 23:11Z and 23:32Z and will be the first `CLOSE_TRAILING_24H` days.

Measured error of the reweighting arithmetic: applying basis 2 to the 2026-11-10 chart point
with the same response's weights reproduces OpenRouter's own `weightedInputPrice` within a
median 0.10% (p90 0.79%, max 1.2%, 50 models). The remaining error for older days comes from
provider mix drift, which this number does not capture.

### Exact arithmetic

Prices are read from the literal JSON number text (serde_json `arbitrary_precision`) into
nano-USD per million tokens, rounding half-to-even. Values are micro-USD integers:
`round_half_even((prompt_billed x p_in + completion_billed x p_out) / 1e9)`. No floating point
touches a value. Signed prints carry `value` as u64 micro-USD with `valueScale = 6`.

### Worked example from the spec (unit test `spec_4_2_worked_example`)

`T = 100,000,000`, `p_in = $0.25/M`, `p_out = $2.00/M`, hypothetical 90/10 mix:
`p_blended = 0.9 x 0.25 + 0.1 x 2.00 = $0.425/M`, value `$42.50`. The wrong formula
`100 x (0.25 + 2.00)` gives `$225` and is asserted unequal.

### Worked examples from real data (2026-11-09, revision r1)

| | Claude Opus 5.5 | DeepSeek V4.1 Flash (1017) | GLM 5.3 |
|---|---|---|---|
| permaslug | `anthropic/claude-opus-5.5-20260921` | `deepseek/deepseek-v4.1-flash-20261017` | `z-ai/glm-5.3-20260816` |
| prompt tokens | 942,820,566,056 | 7,095,511,192,946 | 450,105,304,685 |
| of which cached | 837,791,312,152 | 6,557,828,384,787 | 402,508,331,347 |
| of which BYOK | 336,274,817,067 | 283,846,150,663 | 27,815,203,519 |
| completion tokens | 8,336,472,698 | 95,954,838,485 | 8,555,664,670 |
| of which BYOK | 2,239,854,989 | 3,731,716,897 | 424,115,147 |
| billed prompt / completion | 606,545,748,989 / 6,096,617,709 | 6,811,665,042,283 / 92,223,121,588 | 422,290,101,166 / 8,131,549,523 |
| observed mix (billed) | 99.0049% / 0.9951% | 98.6642% / 1.3358% | 98.1108% / 1.8892% |
| effective in / out, $/M | 0.753222717 / 20.540785281 | 0.015465935 / 0.688754297 | 0.207573628 / 3.274049856 |
| endpoints weighted | 11 | 51 | 45 |
| **index, USD/day** | **582,093.352339** | **168,867.840062** | **114,279.386912** |
| same proxy incl. BYOK (info) | 881,391.56 | 175,828.02 | 121,441.66 |
| OpenRouter spend share | 0.149048 | 0.041174 | (in source-health.json) |

Check for Opus: `606,545,748,989 x 0.753222717 / 1e6 = 456,864.04` plus
`6,096,617,709 x 20.540785281 / 1e6 = 125,229.32`, total `$582,093.35`.

## 7. Quality gates

Blocking (no signed print; the market shows HALTED for that day):

- `TOKENS_OUTLIER`: total tokens above 10x or below 0.1x the median of up to 7 prior reported
  days (needs at least 3). Fired 8 times in the backfill, all real events: StepFun Step 5
  Preview fell from about 3T tokens/day to 0.18T on 2026-10-16 and stayed near 5B; MiniMax M3.1
  Flash jumped from about 0.2B to 52B to 134B in its first week. These need a human decision,
  which is the point of the gate.
- `PRICE_OUTLIER`: effective output price outside `[min listed / 4, max listed x 4]`, or
  effective input above `max listed x 4`, where min/max run over all providers in the
  documented `/endpoints` listing. (A first version used the catalog price, which is only the
  cheapest provider's, and blocked 31 legitimate days; it was replaced before publication.)
- `SOURCE_SHAPE`: the newest token or price response for a market failed strict validation
  (missing or renamed field, float where an integer count belongs, negative number, duplicate
  row, non-midnight day label, mixed dates). The market is held: nothing new is signed and
  history is not re-revised. Source health goes `FAILING`.

Warnings (recorded, value still printed):

- `TOP_N_DROPOUT`: the model ranked below 50 among all variants on that day's rankings. 8 times in
  backfill, all on 2026-11-09 for the 8 markets listed at ranks 52 to 60 (they are in the top 50
  priced standard models but not in the top 50 of all rows, which include free and stealth models).
- `RECONCILE_MISMATCH`: rankings day total and model-activity total differ by more than 1%
  (0 times on 2026-11-09).
- `LOW_WEIGHT_COVERAGE`: endpoints present on the day carry less than half of the current
  weights (13 days, all early days of new models whose provider set grew later).
- `MODEL_ID_CHANGED`: the catalog id now points at a different canonical slug.
  Real example of why this matters: between the same two fetches OpenRouter re-keyed two
  NVIDIA free models from `(nvidia/nemotron-3-ultra-550b-a55b-20260604, variant=free)` to
  `(nvidia/nemotron-3-ultra-550b-a55b-20260604:free, variant=standard)`. Free models are not
  listed as markets, but a re-key of a listed model would make its rows look NOT_REPORTED under
  the old key. The index keys markets by permaslug + `standard` and records such changes; it
  does not auto-merge aliases.

Feed-level checks in `source-health.json`: per-endpoint fetch and failure counts, the newest
failing input, rolling-24 h request count against the budget, late data (yesterday not seen
6 h after it ended), top-50 coverage, and the spend-share cross-check.

### Spend-share cross-check

OpenRouter's rankings page ranks models by spend and exposes each model's share of total
spend. If the index matched OpenRouter's own spend accounting, `index_usd / share` would be
the same number for every model (the market total). On 2026-11-09 over 50 markets:

| | p10 | median | p90 |
|---|---|---|---|
| implied total, index (excl. BYOK) | $3.02M | $3.69M | $4.06M |
| implied total, same proxy incl. BYOK | $3.73M | $4.28M | $16.54M |

Excluding BYOK makes the spread tight. Including it, BYOK-heavy models (StepFun 100% BYOK,
Upstage Solar Mini 4 99%, Tencent HY3 99%) are overstated up to 150x. This is the evidence for
the BYOK exclusion and also a usable estimate that total OpenRouter spend on 2026-11-09 was
around $3.7M. Treat it as an estimate: it is derived from the index itself.

## 8. Statuses

- Data status: `OK`; `NOT_REPORTED` (the source covered the day but had no row for the
  model; never zero, never a -100% move); `UNPRICED` (tokens known, no valid price, or both
  prices zero; never zero spend); `NO_VALID_CLOSE_PRICE` (reserved).
  In the backfill: 3 `UNPRICED` days for MiniMax M3.1 Flash (2026-10-19 to 10-21) where the
  price chart had no point.
- Finality: `FINAL` is the first revision of a day; any later change is a new immutable
  `CORRECTED` revision with `prev_revision_id`, the previous print hash, and a `correction`
  object showing what changed. `PROVISIONAL` is reserved for a live current-day
  feed and is never produced, so the live UI always shows the last completed UTC day.
- Oracle print status: `FRESH` when data is OK, `UNAVAILABLE` otherwise; consumers derive
  `STALE` after `validUntil` (day end + 48 h).
- Market lifecycle (published, paper only): `ACTIVE`; `CLOSE_ONLY` if the latest day is not
  OK; `HALTED` if the latest day is blocked or its print has expired; `PROPOSED` before the
  first revision.

## 9. Corrections, finality and latency

- The index uses the most recent usable token response for each day, so any later change in
  the source's numbers for a day within its 31-day window produces a CORRECTED revision on the
  next poll. Revisions are append-only in `ledger/revisions.jsonl` with a hash chain; raw
  responses are content-addressed by SHA-256 in `ledger/blobs/`. `zfx-index verify` checks the
  chain, every blob and every signature.
- Price inputs are fixed at first selection (the close observation, or the first usable
  chart response), so later weight drift does not rewrite history.
- Corrections so far: none in the index. Observed in the raw data: between the 23:11Z and
  00:00Z fetches on 2026-11-10/11 the rankings rows for 2026-11-09 changed for two `batch`
  variants (GLM 5.3 Flash batch 588,302,814 to 589,307,379 prompt tokens; DeepSeek V4.1 Flash
  batch 36,563,306 to 36,775,187). Batch traffic is counted late, about 24 h after the day ends.
  Standard variants, which the index uses, did not change in that interval. The source docs for the keyed dataset say
  its view "continuously ingests upstream events", so late changes are expected.
- Publication latency: measured as the first poll of `rankings/models?view=day` that shows
  day `d`, minus `d+1 00:00Z`. The poller checks every 10 minutes until the new day appears.
  See `source-health.json` → `publication_latency`. The first live sample (UTC day
  2026-11-10) showed the new day within 10 minutes of 00:00Z. The spec requires 30 days of measurement before
  any timing is relied on; there is no hardcoded publication time.
- Token completeness lags further: `model-activity` responses are CDN-cached for up to 3 h,
  and the 6 h grace means a day is first evaluated at the earliest about 06:00Z the next day.

## 10. Fields not available from public endpoints

These would make the index a measurement instead of an estimate:

- (a) per-model daily input/output/cached token counts or actual billed-dollar sums,
- (b) historical effective price points,
- (c) provisional intraday increments,
- (d) a correction and finality policy,
- (e) coverage of all models, not only the daily top 50.

The endpoints in section 3 show OpenRouter already computes (a) except dollars, (b), and a
spend ranking. Until those fields exist, and the gates in spec 5.4 are met, this index stays a
chart and a paper-trading reference.

## 11. Oracle trust note

A print's ed25519 signature proves that the reporter key signed it. It does not make the
OpenRouter input true, complete or free of manipulation, and one reporter is one point of
failure. Independent reporters and an independent benchmark source are future work.

## 12. Manipulation resistance: the traded 7D index

Owner decision (2026-11-11): markets trade a trailing weekly index, and open interest is capped
by the cost of moving it. The daily index stays published as the input and for charts.

### 12.1 `ZFX-OR-MODEL-SPEND-USD-7D-v1`

Frozen in `markets/index/formula/ZFX-OR-MODEL-SPEND-USD-7D-v1.txt` (its SHA-256 is the 7D
`formulaHash`). Code: `markets/index/crates/zfx-index/src/weekly.rs`.

- Window for day `d`: UTC days `d-6 .. d`, using the latest daily revision (FINAL or CORRECTED) of each.
- A window day counts only if its daily data status is `OK` and it is not blocked.
  `NOT_REPORTED`, `UNPRICED`, blocked and not-yet-evaluated days are excluded, never counted as zero.
- If at least 6 of the 7 days count: `value = round_half_even(S / n)`, the mean over the `n`
  counted days, where `S` is their summed daily value. Otherwise the 7D day is `NOT_REPORTED`
  (print `UNAVAILABLE`, value 0, market `CLOSE_ONLY`). Example: six days at $120 and one
  NOT_REPORTED day give $120, not the $102.86 that a zero would give (test
  `six_of_seven_is_mean_of_six_never_zero`).
- Any change to a window input (a daily correction, a day becoming valid) creates a new
  immutable 7D revision; one corrected daily day revises the 7 windows that contain it (test
  `daily_correction_revises_every_window_containing_it`).
- Print: OraclePrint v1, `indexId = or-model-spend-7d/<permaslug>`,
  `version = ZFX-OR-MODEL-SPEND-USD-7D-v1`, `sourceStartUtc = d-6 00:00Z`, `sourceEndUtc = d+1 00:00Z`,
  `sourceHash` = source-set hash of the counted daily days' print hashes in date order.
  Golden vector `weekly_7d_final` in `markets/index/vectors/oracle_print_v1.json`.
- API: `/v1/indices/or-model-spend-7d/{permaslug}?from=&to=`, `/latest`, `/provenance/{revision_id}`;
  the WS stream carries both daily and 7D revisions (field `index`).

On the live data (backfill 2026-10-11 to 2026-11-09): 1,429 7D days, 1,163 OK, 266
NOT_REPORTED. The NOT_REPORTED days are each market's first 5 days, before 6 days of history
exist. Daily data has no gaps inside a market's history.

### 12.2 Manipulation cost per 1%

Let `S` be the counted spend in the window and `n` the counted days, so `I = S / n`.
An attacker who adds `A` dollars of counted spend anywhere in the window gets
`I' = (S + A) / n`, so the relative move is `(I' - I) / I = A / S`. It does not matter how
`A` is spread across the days. Therefore

```text
manipulation_cost_usd_per_pct = S / 100            (1% of the 7-day counted spend)
```

which for a full window equals `7 x I x 1%`. It is published per 7D day in the 7D series,
in each daily point's `index_7d` block, and per market (latest) in `models.json`.

Why counted spend is close to what the attacker pays: the index prices tokens at OpenRouter's
effective prices, which are cost divided by tokens over the same traffic. An attacker's tokens
pull the effective price toward the price they actually pay (cheap provider, heavy caching),
so each dollar billed to them adds about a dollar to the index. The residual error is the
proxy error measured in section 7 (implied market total spread about 10 to 20% around the
median) plus the window mismatch of the price basis.

### 12.3 Open-interest cap

```text
max_open_interest_usd = 0.25 x S
```

Pushing the index by a fraction `x` costs `x x S` and earns at most `OI x x` on positions of
size `OI` on the winning side. With `OI <= 0.25 x S` the profit is at most a quarter of the cost,
so a push loses money with a 4x margin, before fees and before the attacker's own price impact.
The cap applies to total open interest per side, summed over all accounts.

Real values on 2026-11-09 (every market: `models.json`): Claude Opus 5.5 has
7D $629,765/day, 1% costs $44,084, OI cap $1,102,089. Tencent HY3 has 7D $224/day, 1% costs
$16, OI cap $392. Markets this small cannot carry meaningful positions; a listing floor on
`max_open_interest_usd` is recommended (not yet set).

### 12.4 Jump clamp

If a 7D value differs from the previous day's OK 7D value by more than 15%
(`clamp_bound_bps = 1500`, configurable in `Params7d`), the record carries `clamped: true` and
`change_vs_prev`. The signed print still carries the computed value, but that day's lifecycle
is `CLOSE_ONLY` (`lifecycle` field in each 7D point and `lifecycle.status` in `models.json`),
which the perps engine reads. Daily anomaly blocking (TOKENS_OUTLIER, PRICE_OUTLIER,
SOURCE_SHAPE) still applies and still halts the market. A blocked daily day is excluded from
the 7D window.

On the live data the clamp fired on 45 of 1,163 OK 7D days (3.9%): 13 in a model's first week
of 7D values (launch ramps) and the rest in real swings, mostly low-volume models (Upstage Solar
Mini 4 alone has 10). At hand-off two markets are `CLOSE_ONLY` because of it:
`openai/gpt-5.6-luna-20260709` (-35% on 2026-11-09) and `upstage/solar-mini4-20260922` (-34%).

### 12.5 Assumptions and what this does not cover

This bounds the cost of a push by an outside party. It does not make manipulation impossible.

- **The attacker pays OpenRouter prices.** An attacker with negotiated discounts, credits,
  promotional pricing or free quota pays less than the index counts.
- **BYOK is excluded**, so traffic on an attacker's own provider keys adds nothing.
  Free models and `:free` variants are not markets and are not counted.
- **A sustained attack.** A push affects every window containing the pushed days. Holding the
  index up for a week costs about 7x a one-day push of the same daily size. The clamp means a
  push above 15% in one day turns the market close-only for that day, so a large one-day push
  cannot be followed by opening new positions at the pushed level.
- **Provider or insider wash trading is not bounded by this.** A provider that serves its own
  model on OpenRouter gets most of the attacker's spend back (all but OpenRouter's fee and its
  own compute). For first-party models the real cost of a push can be a small fraction of `S / 100`.
  Detecting that needs provider-level data the public endpoints do not give.
- **Downward pushes do not need spend.** A provider can cut its price or degrade its endpoint,
  which lowers effective prices or traffic. That costs it revenue, but the formula does not price it.
- **The source itself.** Changes to OpenRouter's web endpoints, data errors, or an
  attack on the reporter are outside this bound (section 11).
- The 25% fraction, the 15% clamp and the 6-of-7 rule are judgment calls, written into the
  formula text. They should be revisited once there are 30+ days of live data, as spec 5.4
  requires for volatility and jump limits.

## 13. Dataset index: `ZFX-OR-MODEL-SPEND-USD-LIC-v1` and `-LIC-7D-v1`

Formula texts: `markets/index/formula/ZFX-OR-MODEL-SPEND-USD-LIC-v1.txt`,
`...-LIC-v1.params.json` (frozen shares) and `...-LIC-7D-v1.txt`. Code:
`crates/zfx-index/src/licensed.rs`, `weekly.rs` (`Spec7d::lic`), `report.rs`. The LIC daily
`formulaHash` covers the text and the params file together.

### 13.1 Formula

```text
p_in     = (1 - cache_share) x L_in + cache_share x L_cache_read
p_blend  = w_in x p_in + (1 - w_in) x L_out
index    = total_tokens x billed_share x p_blend / 1e6        (USD/day)
```

- `total_tokens`: the documented dataset row for the model and day. Use requires
  `meta.as_of >= day end + 6 h`; the most recently fetched usable response wins, so corrections
  propagate. A day with no row is `NOT_REPORTED` (outside the top 50), never zero.
- `L_in`, `L_out`, `L_cache_read`: the lower median over providers of the listed prices in the
  documented endpoints listing. The snapshot is the first one taken after the day ends (within
  24 h), else the last one during the day: basis `LISTED_SNAPSHOT`, MEDIUM confidence. Before the
  first snapshot (2026-11-10), days from 2026-10-11 use that first snapshot: basis
  `LISTED_BACKFILL`, LOW confidence. Earlier days have no USD value; their documented token
  history is published in each dataset series as `token_history`.
- `w_in`, `billed_share` (1 - BYOK share) and `cache_share`: frozen per model, in parts per
  million, from the web index's counted days 2026-10-11 to 2026-11-09. Example, Claude Opus 5.5:
  w_in 0.990423, billed 0.760678, cache 0.899243. 50 models are frozen. The 4 markets listed on
  2026-11-10 have no shares and stay `UNPRICED` under `LIC-v1`. These shares are the only
  inputs derived from the web endpoints, and they are fixed for the life of the version. A
  refresh means `LIC-v2`.
- 7D: same rules as section 12 (mean of counted days, 6 of 7 required, 25% OI cap, cost per
  1% = 1% of window spend), with a **25% clamp** (section 13.4).
- Blocking: TOKENS_OUTLIER on dataset totals. PRICE_OUTLIER does not apply because listed
  prices are the price input.

Live results (2026-10-11 to 2026-11-09, 54 markets): 1,620 daily records, of which 1,174 OK,
418 NOT_REPORTED, 28 UNPRICED, and 4 blocked: StepFun Step 5 Preview on 2026-10-16 and
Tencent HY4 Preview on 3 days. 7D: 882 OK, 738 NOT_REPORTED, 4 clamped. On 2026-11-09, 42 markets
had an OK dataset value; 37 are ACTIVE, 17 CLOSE_ONLY.

### 13.2 Documented vs web token totals

Over the overlap 2026-10-11 to 2026-11-09, for every market-day present in both:

| pairs compared | exact matches | exact-match rate | max relative difference | market-days missing from documented |
|---|---|---|---|---|
| 1,174 | 1,174 | 100% | 0 | 255 (outside that day's top 50) |

The two endpoints report the same `prompt + completion` totals. Published live in
`source-health.json` → `documented_vs_web_tokens`.

### 13.3 How the dataset index tracks the web index

Ratio `LIC / web` on market-days where both are OK (`source-health.json` → `licensed_vs_web_tracking`):

| | n | ratio p10 | median | p90 | median abs. deviation | day-over-day direction agreement | median abs. difference of daily % change |
|---|---|---|---|---|---|---|---|
| daily | 1,170 market-days | 0.632 | 0.978 | 1.296 | 14.3% | 81.2% | 6.3 pp |
| 7D | 879 market-days | 0.646 | 0.974 | 1.281 | 12.8% | 77.3% | 1.5 pp |
| per-market median ratio (daily) | 50 markets | 0.671 | 0.947 | 1.129 | 10.5% | | |

Reading it:

- Level: half the markets sit within about 10% of the web index. The tails are models where listed
  prices differ from what traffic actually pays. One cause is a cheap dominant provider: DeepSeek
  V4 Flash (0423) runs at 0.58 because its effective input is $0.099 against a $0.054 listed
  composite. Claude Haiku 5.5 runs at 0.62, likely because of cache-write charges, which the listed
  composite does not include (not verified). On the high side, Upstage Solar Mini 4 runs at 2.2 and Z.AI GLM 5.2 at 1.3.
- Moves: tokens are identical, so differences in daily moves come from the web index's
  day-to-day effective-price changes, which the dataset index does not see.
- **This is in-sample.** The frozen shares were fitted on these same days, and LISTED_BACKFILL
  applies 2026-11-10 prices to them. Out-of-sample tracking starts 2026-11-10 and needs weeks
  of live data.

### 13.4 Clamp bound and the 6-of-7 rule, from 2025-01-01 to 2026-11-09

Data: the documented dataset, 364 models (excluding `other` and `:free`), tokens only (constant
price), so it measures demand moves. Published live in `source-health.json` → `clamp_calibration`.

Established 7D day-over-day moves (both days with at least 6 of 7 days, at least 20 days after the
model's first row), n = 22,654:

| p50 | p90 | p95 | p99 | p99.5 | p99.9 |
|---|---|---|---|---|---|
| 2.2% | 8.9% | 12.7% | 26.6% | 34.4% | 56.7% |

| bound | share of days above |
|---|---|
| 10% | 7.9% |
| 15% (web 7D-v1) | 3.7% |
| 20% | 1.9% |
| **25% (LIC-7D-v1)** | **1.1%** |
| 30% | 0.7% |

**Chosen: 25%, the p99 of established moves rounded down to the nearest 5%.** At 15% a market
would go close-only on about 13 days a year from ordinary demand swings. At 25% it is about 4.
A move above 25% is in the top 1% of observed organic moves, where a push or a data problem is a
plausible explanation and a close-only day is a fair price. The bound interacts with the OI cap.
A 25% push costs 25% of the window spend (`0.25 x S`) and earns at most `OI x 0.25`, which is at
most `0.0625 x S` under the cap.

Min valid days. Error of the window mean when days are removed at random from full windows:

| removed | p50 | p90 | p95 | p99 | p99.5 |
|---|---|---|---|---|---|
| 1 of 7 | 1.9% | 6.0% | 7.8% | 13.7% | 17.4% |
| 2 of 7 | 3.0% | 9.5% | 12.8% | 22.0% | 26.9% |

**Kept: 6 of 7.** With one day missing, the p99 error (13.7%) is about half the clamp bound. With
two missing, the p99 error (22%) is close to the bound, so missing data alone could look like a
clamp-sized move. Real missing days are dropouts from the top 50, i.e. low-volume days, so the
remaining mean is biased upward. That argues against relaxing the rule further. Valid-day counts
across all model-days (after each model's first 6 days): 7 of 7 in 23,695 windows, 6 in 2,108,
5 in 1,508.

The web 7D-v1 keeps its frozen 15% bound. If the web 7D were ever used as a reference, it
should move to a `7D-v2` with 25%.

### 13.5 Request budget used

- Dataset backfill: 3 requests for 2025-01-01 to 2026-11-10. The first asked for 679 days and got
  `400 Date range cannot exceed 366 days (requested 679)`; then 2025-01-01..2025-12-06 (17,238 rows)
  and 2025-12-07..2026-11-10 (17,289 rows). The minimum possible is 2.
- Ongoing: one 30-day request per poll hour until yesterday appears, plus one after the grace
  window. The 2026-11-10 data was already in the dataset at 00:25:44Z (`meta.as_of`), 26 minutes
  after the day ended. Grace still applies before a day is used.
- Documented price snapshots: one endpoints listing per market per day.

## 14. On-chain oracle

Contract `markets/index/contracts/zfx-oracle`, deployed on NEAR testnet as `oracle.zeroflux.testnet`
(record and every transaction hash: `markets/index/TESTNET_ORACLE.md`).

What it stores: for each enabled index id, an append-only history of reporter-signed OraclePrint v1
prints, ordered by (source window end, revision), plus the latest print. Anyone may relay prints with
`push`. A print is accepted only if all of the following hold:

1. it decodes as canonical OraclePrint v1 with the same strict decoder as everywhere else
   (`zfx-oracle-print`, compiled to wasm);
2. its index id is enabled and its `version`, `formulaHash` and `valueScale` match what the owner
   registered for that id. The formula hash pins the exact formula text (and, for `LIC`, the frozen
   parameters);
3. its source window ended before it was published, and it is not published more than 10 minutes in
   the future;
4. at least `quorum` distinct, active, owner-listed reporter keys signed the canonical bytes
   (ed25519, NEAR host `ed25519_verify`);
5. it moves the index forward: a later window, or a higher revision of the latest window whose
   `prevPrintHash` is the latest print's hash. This is the same rule the perps clearing contract uses.
   Replays, older windows and unchained corrections are rejected.

Corrections to a day older than the latest on-chain window cannot be recorded, by design. They stay
in the off-chain ledger and provenance files. A consumer that needs them should read the published
`provenance/` records.

Views return everything needed to re-verify without trusting the contract: value, status, data
status, finality, window, `valid_until`, a `stale` flag from block time, the print hash, the exact
canonical bytes (hex) and every signature (`ed25519:<pubkey>:<sig>`). The contract stores only the
bytes after the two strings, because they are implied by the index, and rebuilds the exact canonical
bytes for views. A sandbox test and a testnet check confirm the rebuilt bytes hash to the published
`printHash`.

Measured cost: 272 bytes of storage per print (0.00272 NEAR at testnet prices) and 0.63 to 0.70 Tgas
per print in batches of 100. Verifying signatures in wasm with ed25519-dalek instead of the host
function costs 4.70 Tgas per print, 6.3x more, which is why the contract uses the host function.
The owner prepays storage. A push that would leave less than the configured reserve is refused as a
whole.

Operation: `zfx-index poll` runs `push-near` after publishing new dataset-index revisions (and at least
every 6 hours) when `~/.config/zeroflux/oracle-push.json` is present and enabled. The pusher reads
the on-chain latest of every index in one view, sends only newer prints in order, and is idempotent.
On testnet the backfill covers the latest 7 days (2026-11-03 to 2026-11-09). 30 days did not fit the
5 NEAR budget. Daily pushing of both dataset indices for 28 markets costs about 0.15 NEAR/day of
storage. The deployed balance covers about 8 days; see `TESTNET_ORACLE.md` for top-up options.

### Trust model

- The contract proves which allowlisted reporter keys signed a print, and that prints only move
  forward under the published formula hash. It does not make the OpenRouter source data true,
  complete or free of manipulation. It does not check that the formula was applied correctly.
  Anyone can do that off-chain from the published inputs and provenance.
- Today there is one reporter key and quorum 1. Whoever holds that key, or the owner key that
  changes the allowlist, controls what gets accepted. A real deployment needs independent reporters,
  a quorum above 1, and a timelocked or multisig owner.
- The owner can disable an index, change its formula hash, or replace reporters. History already
  stored is never rewritten or deleted.
- Testnet only. No value settles against this contract.
