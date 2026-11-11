# zfx-perps: order book and clearing

This is the contract between users, the operator and the NEAR clearing contract of `zfx-perps`
(`markets/zfx-perps/`). It covers message formats, signatures and nonces, matching priority,
ledger invariants, margin, funding, liquidation, insurance, oracle staleness, and exactly what the
operator can and can't do. Parameters and their derivation are in `docs/MARKET_RISK.md`.

Status: testnet and paper trading only. Nothing here is audited. Model perps must not take real
money (spec 5.4).

## 1. Components

| Component | Path | Role |
|---|---|---|
| Clearing core | `crates/zfx-perps-clearing` | Every clearing rule, as pure functions over a storage trait. Compiled into the contract and into the engine |
| Clearing contract | `contracts/zfx-perps-clearing` | Authoritative ledger on NEAR: collateral, isolated positions, fees, insurance, order records, oracle state |
| Engine | `crates/zfx-perps-engine` (`clearing.rs`) | Price-time order book. Admits and matches signed orders against a predicted copy of the ledger |
| Node | `crates/zfx-perps-node` | Sequencer, journal, REST/WebSocket gateway (`openapi.yaml`) |
| Relayer | `crates/zfx-perps-near` (`clearing_adapter.rs`) | Forwards contract events to the node, submits batches to the contract |
| SDK | `packages/sdk-ts` (`@zeroflux/zfx-perps-sdk`) | Browser and Node client |

The contract and the engine run the same core code. The engine keeps two copies of the ledger:
`confirmed`, which equals the contract's state after the last chain event it has seen, and
`spec`, which is `confirmed` plus every op not yet settled. Tests check after every settled batch
that `confirmed` equals the contract's state exactly (`crates/zfx-perps-engine/tests/clearing.rs`).

## 2. Units

- Prices are integer ticks of the market; quantities are integer lots.
- `notional = lots × ticks × tick_value` in collateral atoms (tUSDC: 6 decimals).
- The index print's integer `value` (with `value_scale` decimals) maps to a mark of
  `value / index_units_per_tick` ticks.
- Collateral, margin and PnL are `u128`/`i128` atoms. Every rate is in basis points (1/10,000) or
  parts per million, and every rounding direction is fixed: requirements and fees round up.
- Every money field on the wire is a decimal string. The SDK uses `bigint`.

## 3. Signed messages

All user messages are `|`-separated UTF-8 strings signed raw (no pre-hash) with an ed25519
trading key. Each starts with a versioned domain tag and names the chain id and the clearing
contract, so a signature is valid on one deployment only. Booleans are `0`/`1`, enums upper case,
hashes lower-case hex, numbers base-10 without leading zeros. Account ids are checked against
`[a-z0-9._-]`, market ids against `[A-Z0-9-]` and chain ids against `[a-z0-9:-]`, so no field can
contain `|`. Rust (`wire.rs`) and TypeScript (`src/clearing/wire.ts`) produce identical bytes;
`packages/sdk-ts/test/clearing_vectors.json` pins both.

| Message | Format |
|---|---|
| Order | `zfx-perps-order-v1\|chainId\|contract\|account\|market\|BUY\|SELL\|LIMIT\|MARKET\|price\|qty\|reduceOnly\|postOnly\|GTC\|IOC\|FOK\|nonce\|expiryNs\|clientOrderId\|maxFeeBps\|policyHash` |
| Cancel | `zfx-perps-cancel-v1\|chainId\|contract\|account\|orderHash\|expiryNs` |
| Nonce floor | `zfx-perps-min-nonce-v1\|chainId\|contract\|account\|minNonce` |
| Withdraw | `zfx-perps-withdraw-v1\|chainId\|contract\|account\|token\|amount\|nonce` |
| Margin move | `zfx-perps-margin-v1\|chainId\|contract\|account\|market\|delta\|nonce` |
| Read auth | `zfx-perps-auth-v1\|chainId\|contract\|account\|tsMs` (gateway only, never on-chain) |

`orderHash = sha256(order message)`. The contract stores filled quantity, cancellation and expiry
under it.

`policyHash = sha256("zfx-perps-policy-v1" ‖ borsh(MarketParams))`. Every order commits to the
market's full parameter set, so any parameter change (fees, margin, band, oracle rules) makes
every resting order unfillable on-chain until the user re-signs.

### Trading keys (session keys)

A user registers a trading key from their wallet with `register_key(public_key, expires_ns,
can_withdraw)` (0.01 NEAR storage deposit, refunded on removal). The wallet transaction is the
authorization; the contract checks the key on every fill, withdrawal and relayed cancel.

- Expiry: `expires_ns` (0 = none). An expired key signs nothing new.
- Scope: `can_withdraw = false` makes a trading-only key.
- Rotation: register a new key, then `remove_key` the old one. Fills of orders signed by a removed
  key are rejected from the block the removal lands in.
- At most 16 keys per account.
- A leaked trading key can trade the account's collateral and, if allowed, sign withdrawals. A
  withdrawal still pays only the account itself.

NEP-413 wallet message signing is not used for orders: wallets prompt per message, which can't
support order rates. It fits login flows; the registered session key carries trading.

### Nonces, expiry and cancels

- `nonce`: an order can be filled only while `nonce >= min_nonce` of its account.
- `expiryNs`: an order can be filled only if the op that fills it is stamped before expiry and lands
  in a block less than `settle_grace_ns` (300 s) after expiry. Orders may not expire more than 30
  days out. The engine refuses orders with less than 10 s of life left.
- Cancel finality, three levels:
  1. `POST /v1/orders/cancel`: signed, the engine removes the order immediately. This is a promise
     by the operator, not yet enforced on-chain.
  2. `relayOnChain: true`, or `POST /v1/orders/min_nonce`: the signed cancel or nonce floor goes
     into the next batch. From the block it lands in, the contract refuses any fill of that order.
  3. Wallet `cancel_order(order_hash, expiry_ns)` or `set_min_nonce(n)`, or anyone relaying the
     signed cancel with `submit_cancel`: final from that block, with no operator involvement.
  An operator can still settle a fill it matched before the on-chain cancel landed, if that fill
  lands first. Short expiries bound the exposure.

## 4. Matching

- Price-time priority: best price first, then arrival order (the sequencer's order) at a price.
- Time in force: GTC rests the remainder; IOC cancels it; FOK fills completely or not at all
  (walked before any state changes); post-only rests or is rejected.
- MARKET orders are IOC/FOK with `price` as the worst acceptable price.
- Fills happen at the resting (maker) order's price.
- Self-trade prevention is `cancel_taker` by default. The contract refuses any fill with the same
  account on both sides.
- Before each candidate fill the engine applies it to `spec` with the core's `fill`, which runs
  the contract's checks. A resting order that fails (expired, key revoked, margin) is removed
  with reason `unfillable`; an incoming order that fails stops matching.

The contract checks the price and size rules (limit, band, quantities) but can't see the book, so
price-time priority is a promise of the operator (section 10). FOK all-or-nothing is enforced by
the matcher; on-chain an FOK order is protected like an IOC order (never more than its quantity,
never worse than its limit).

## 5. What the contract checks on every fill

`Fill { maker, taker, qty, price }` refers to two signed orders in the batch's order table.
Each order is verified once per batch. The fill is applied only if all of these hold:

1. Both orders are well formed, name this chain id, this contract and this market.
2. Each is signed by a key registered to its account, not expired at the op time.
3. `nonce >= min_nonce`; not expired (with grace); not canceled; expiry within 30 days.
4. Policy hash equals the market's current policy hash.
5. Quantity within the order's remaining quantity (filled so far plus this fill <= signed qty),
   and within the market's `min_qty`/`max_qty`; price within `max_price`.
6. Opposite sides, different accounts. Maker is a resting LIMIT/GTC order; taker isn't post-only.
7. Fill price equals the maker's limit and is within the taker's limit.
8. Fill price within `price_band_bps` of the mark.
9. Market fee rates at or below each order's `maxFeeBps`.
10. Reduce-only orders only reduce, never flip.
11. Market type PERPETUAL, effective status ACTIVE or CLOSE_ONLY; in CLOSE_ONLY both sides must
    reduce.
12. Margin (section 6), position cap, open-interest cap.

A failed check skips that op and lists it in `batch_settled.rejected` with a reason code; the rest
of the batch applies. Batch ids must be consecutive; an already-used id fails the whole
transaction. Only allowlisted operator accounts can call `settle_batch`.

## 6. Margin (isolated, v1)

Each (account, market) has its own isolated position: `size`, `margin`, `funding_index`. Equity at
mark `M` is `margin + size·M·tick_value − size·(cum_funding − funding_index)`. `margin` absorbs
trade cash, fees and settled funding, so there is no separate realized-PnL book.

- `IM(n) = ceil(|n|·M·tick_value·initial_margin_bps/10,000)`, `MM` likewise with maintenance.
- Increasing exposure (or flipping): after the trade, equity at the mark must reach `IM` of the new
  size. The shortfall moves from free collateral to the position; not enough free collateral
  rejects the fill. Buying above the mark or selling below it costs margin right away, because
  equity is valued at the mark.
- Reducing: equity after the trade must be at least zero. Closing to zero returns the position's
  margin to free collateral.
- Margin moves: signed `AdjustMargin` adds free collateral to a position, or removes it down to IM.
- No cross-margin: a loss in one market never touches collateral in another.

The engine's admission check is stricter and assumes every resting order fills: the sum over
markets of `max(0, IM(worst case) + fees + adverse price − equity)` must fit in free collateral.

The inherited cross-margin perps (`perp.rs`) still exist in the engine for the operator-trusted
netted mode. They are not the NEAR v1 path, and the clearing contract has no cross margin.

## 7. Oracle

A market tracks one index (`index_id`, `index_version`, `value_scale`). Prints are
`OraclePrint` v1 (`markets/index/docs/ORACLE_PRINT_V1.md`), decoded strictly by
`crates/zfx-perps-clearing/src/print.rs`. They're checked against the index workstream's golden
vectors and a real signed print in `tests/oracle_print_vectors.rs`.

The contract accepts a print op only if all of these hold:

- signatures from at least `quorum` distinct keys in the reporter allowlist verify over the
  canonical bytes;
- the index id, formula version and value scale match the market;
- `published_at` isn't more than 60 s ahead of the op, and `valid_until` hasn't passed;
- the print is newer than the current one: a later source window, or a higher revision of the
  same window whose `prev_print_hash` is the current print's hash (a correction).

Then:

- A usable print (FRESH, data OK, value > 0) sets the target `index_ticks`. The mark moves toward
  it by at most `mark_limit_bps` (limit move). A move beyond `halt_jump_bps` is treated as suspect
  data: the market halts and the mark stays.
- An unusable print (NOT_REPORTED, UNPRICED, NO_VALID_CLOSE_PRICE, STALE) is recorded but doesn't
  move the mark.

Corrections only change the mark from the moment they land. Past fills and funding are never
rewritten.

### Effective status

The admin status (PROPOSED, ACTIVE, CLOSE_ONLY, HALTED, SETTLING, CLOSED) is made stricter by the
oracle. Every fill, liquidation and funding op evaluates this at its own timestamp:

| Condition | Effective status |
|---|---|
| no print, a DISPUTED print, or UNAVAILABLE with data OK | HALTED |
| print past `valid_until`, older than `oracle_max_age_sec`, or data not OK | CLOSE_ONLY |
| mark limit-locked (mark ≠ index) | CLOSE_ONLY |
| daily cycle: an expected print missing, or after `print_close_only_from_sec` with that day's print not in | CLOSE_ONLY |
| otherwise | the admin status |

In CLOSE_ONLY nobody can add exposure. Liquidations still run if the mark is fresh (limit-lock,
daily window). With a stale or unusable print the mark is never used as fresh: no liquidations, no
funding, and the last mark only prices reductions. A day without a print, or 24 hours past
`valid_until`, leaves the market CLOSE_ONLY, never ACTIVE.

## 8. Funding

`Funding { market }` applies at most once per `funding_interval_sec`, measured by op time:

- `vwap` = volume-weighted price of verified fills since the last application (ACTIVE fills only);
- `premium_ppm = (vwap − mark) / mark`;
- `rate_ppm = clamp(premium_ppm / funding_divisor, ±max_funding_ppm)`;
- `pay_per_lot = trunc(rate_ppm × mark × tick_value / 10^6)`.

Each long lot pays `pay_per_lot` and each short lot receives it, through the cumulative index
`cum_funding`. Each position settles lazily as `size × (cum_funding − funding_index)` when it is next
touched. Since positions sum to zero, funding sums to zero exactly.

The rate is zero when the market isn't ACTIVE with a fresh mark, or when nothing traded. The
funding mark is the latest daily print. It is a premium over a published number, never an invented
intraday index. Fees are separate (section 9), so funding is never a hidden holding fee.

## 9. Fees, liquidation, insurance and losses

Trading fees are `ceil(notional × bps / 10,000)`, charged from each side's position margin and
credited to `fee_balance`. Fee withdrawal is owner only, behind the timelock.

Liquidation (`Liquidate` op) is valid only for a position below maintenance at a fresh mark:

1. **Book.** Resting signed orders take the position. Each fill is within `liq_slippage_bps` of
   the mark and is verified like any maker fill. The liquidated side pays no trading fee.
2. **Insurance.** The insurance fund takes any remainder at the mark, if the fund's free
   collateral can margin it.
3. **ADL.** Only if the fund can't margin the remainder (the contract checks this): the remainder
   is closed against opposite-side holders at the mark. Each target must hold the opposite side
   and only reduce.
4. **Fee.** A liquidation fee of `liq_fee_bps` of closed notional at the mark goes to the
   insurance fund, capped at the equity left.
5. **Deficit.** If equity is below zero after closing, the position's margin is gone, the
   insurance fund pays from free collateral, and any rest is socialized. It is split pro rata by
   size over every holder of the profiting side. The op must list all of them, sorted; the
   contract checks that their sizes sum to open interest. Each take is capped at that holder's
   equity.
6. **Bad debt.** Whatever still can't be collected is recorded as the market's `bad_debt`, and
   the market halts. There is no protocol backstop beyond this: no minting, no draw on fees, no
   other market's collateral.

With `mark_limit_bps + liq_slippage_bps + one day of max funding <= maintenance_margin_bps`
(enforced by `create_market` and `update_market`), one print can't take a position that wasn't
liquidatable below zero equity. Steps 5 and 6 then happen only if liquidations are late: the
operator is down, or ops are delayed past several prints. The stress replay of all 50 index
histories shows zero deficits with the v1 parameters (`docs/MARKET_RISK.md`).

The insurance fund is a ledger account (`insurance.<contract>`). Anyone can top it up by
depositing with `msg` = that account. There is no withdrawal path for it in v1.

## 10. Operator powers and failure modes

The operator runs the node (sequencer and engine) and the relayer key. The contract bounds it as
follows.

| Power | Bound |
|---|---|
| Forge a fill, change its size or price, fill a canceled or expired order | No: every fill needs both users' signatures and passes section 5 |
| Mint collateral, move collateral between accounts outside fills | No: every op conserves `Σ free + Σ position value + fees = total collateral + bad debt` |
| Change fees for existing orders | No: orders commit to the policy hash and a max fee |
| Withdraw user funds | No: payouts need the account's key signature and go only to the account |
| Order and match | Trusted. The contract can't see the book, so price-time priority, front-running and selective matching are not provable. The journal records arrival order |
| Censor orders, cancels, withdrawals | Can delay. Bounds: wallet `cancel_order` and `set_min_nonce`, `submit_cancel` by anyone, short order expiries, forced withdrawal then freeze |
| Backdate an op | Op timestamps never decrease and can't lead the block by more than 10 s. Expiries (orders, keys, prints) are enforced against the block with a 300 s grace |
| Withhold oracle prints | The market goes stale and fails closed (CLOSE_ONLY). The operator can't invent a print: quorum signatures are required |
| Choose which valid liquidation to submit | Choice of book makers (within slippage) and ADL targets (opposite side, reduce only). The ADL ranking is an operator policy (most collateralized first), not verified on-chain |
| Delay liquidations | Possible. Losses beyond margin then go through the waterfall; this is the main residual risk |
| Funding cadence | Can delay funding, not apply it more than once per interval, and can't choose the rate (computed from verified fills) |

Owner (admin) powers: tightening risk is immediate (lower leverage, lower caps, CLOSE_ONLY or
HALTED status, removing a reporter). Loosening risk, activating a market, changing the reporter
set and withdrawing fees wait out the timelock (`timelock_ns`; 60 s on testnet, should be days
before real money). The owner should be a multisig. There is no upgrade method: the deployed code
is fixed.

Failure modes:

- **Operator stops.** Trading and withdrawals through the node stop. A user files
  `request_forced_withdrawal`. After `force_timeout_ns` anyone calls `freeze`, then anyone pages
  `escape_scan` over all positions at their markets' last marks. Deficits are covered by the
  insurance fund, and any remaining shortfall is spread pro rata over positive position claims.
  Each user then calls `escape_withdraw` for free collateral plus their claim. Fills not yet in a
  settled batch are lost.
- **Chain rejects an op the engine predicted** (on-chain cancel, key revocation or reclaimed
  deposit racing a batch). The batch event lists the rejections. The engine rebuilds `spec`, drops
  pending ops that no longer apply and reports `fill_reverted`. A fill is final only once its batch
  settles.
- **Engine diverges** (the chain applied something the engine can't reproduce, or the contract
  was initialized with other parameters). The engine stops admitting clearing orders and the
  relayer stops submitting until an operator investigates.
- **Congestion or RPC outage.** Batches settle late. Ops past their expiry grace are rejected
  on-chain and reconciled as above.
- **Reporter key compromise.** Owner removes the reporter immediately. With a single reporter
  (testnet), prints then stop and markets fail closed.
- **Wrong collateral or beneficiary.** `ft_on_transfer` refunds anything but the allowlisted token,
  a frozen contract, a bad beneficiary or a zero amount. A deposit stays pending until a batch
  credits it; after one hour (or once frozen) its owner can `reclaim_deposit`.

## 11. Ledger invariants

Checked after every operation by the randomized tests (`crates/zfx-perps-clearing/tests/props.rs`,
`crates/zfx-perps-engine/tests/clearing.rs`):

- Positions sum to zero per market; open interest equals total long size.
- `Σ free + Σ (margin − funding owed) + fee_balance = total_collateral + Σ bad_debt`.
- A rejected op leaves the ledger byte-for-byte unchanged.
- The engine's `confirmed` ledger equals the simulated contract after every batch. Replay from the
  journal, or from a mid-stream snapshot, reproduces the same state hash.

## 12. Batches and gas

A batch carries an order table, with each signed order once, and timestamped ops. Ops are
borsh-encoded. Measured on the deployed wasm on a local nearcore sandbox
(`research/data/clearing_gas.json`):

| Batch | Tgas | per fill |
|---|---|---|
| 1 fill, 2 new orders | 4.81 | 4.81 |
| 10 fills, 20 new orders | 16.24 | 1.62 |
| 40 fills, 80 new orders | 47.91 | 1.20 |
| 40 fills against one resting order (41 orders) | 29.68 | 0.74 |
| one oracle print (one signature) | 3.02 | |
| one withdrawal (token transfer + callback) | 7.87 | |

That works out to about 3.7 Tgas per batch, 0.47 per order in the table and 0.17 per fill. The
engine packs batches below 200 Tgas, leaving margin under NEAR's 300 Tgas per call. That is about
170 independent fills, or more when makers are reused. At 1,000 Tgas per shard per block (0.6 s),
one clearing contract settles in the order of 1,000 verified fills per second at full shard
capacity.
