> Netted mode (inherited from obkit). The NEAR v1 clearing path is documented in the repo-root
> `docs/ORDERBOOK_AND_CLEARING.md` and `openapi.yaml`.

# Node API

Base URL defaults to `http://127.0.0.1:8794`. Amounts that can exceed 2^53 are decimal strings.
Prices are integer ticks and quantities integer lots (see `/v1/info` for each market's `base_lot`
and `quote_tick`). Errors look like `{"error": "<code>", "message": "..."}`.

## Signing

Requests are signed with a trading key registered on the settlement contract. The signature is
ed25519 over the UTF-8 bytes of a `|`-separated message, sent base64 encoded. Public keys are
`ed25519:<base58>`.

| Request | Message |
|---|---|
| order | `zfx-perps-order-v1\|account\|market\|side\|price\|qty\|tif\|stp\|client_id\|ts_ms` |
| cancel | `zfx-perps-cancel-v1\|account\|market\|order_id\|client_id\|ts_ms` (one of the two ids is 0) |
| cancel all | `zfx-perps-cancel-all-v1\|account\|market or *\|ts_ms` |
| withdraw | `zfx-perps-withdraw-v1\|settlement_contract\|account\|token\|amount\|nonce` |
| account read, WS auth | `zfx-perps-auth-v1\|account\|ts_ms` |

Freshness and replay rules:

- Timestamps must be within 30 s of the node's clock.
- A signature can be used once. Use a unique `client_id` per order so identical orders signed in
  the same millisecond don't collide.
- Withdrawals have no timestamp. Their nonce must be greater than the account's last one, and the
  contract enforces this too.

## Public

| Method | Path | Returns |
|---|---|---|
| GET | `/v1/health` | `{ok, seq, durable_seq, published_seq, fsyncs}` |
| GET | `/v1/info` | settlement contract, fee account, assets, markets |
| GET | `/v1/book/{market}?depth=50` | `{market, seq, bids: [[price, qty]], asks}`, best first |
| GET | `/v1/trades/{market}?limit=100` | newest first |
| GET | `/v1/perp/{market}` | `mark`, `index`, `open_interest`, `last_funding_rate_ppm`, `cum_funding_per_lot`, `funding_interval_ms`, `params`, `oracle` (latest price and per-source results) |

## Signed

| Method | Path | Body |
|---|---|---|
| POST | `/v1/orders` | `{account, market, side, price, qty, tif, stp, client_id, ts, public_key, signature}` |
| POST | `/v1/orders/cancel` | `{account, market, order_id or client_id, ts, public_key, signature}` |
| POST | `/v1/orders/cancel_all` | `{account, market?, ts, public_key, signature}` |
| POST | `/v1/withdraw` | `{account, token, amount, nonce, public_key, signature}` |
| GET | `/v1/account/{account}` | headers `x-zfx-perps-key`, `x-zfx-perps-ts`, `x-zfx-perps-signature`. Returns balances, open orders, withdrawal nonce, `positions` (size, entry price, mark, unrealized PnL) and `margin` (equity, initial, maintenance, free, perp cash, settled perp value) |

Field values:

- `side`: `buy` or `sell`.
- `tif`: `gtc`, `ioc`, `fok` or `post_only`.
- `stp`: `cancel_taker` (the default), `cancel_maker` or `cancel_both`.

Successful responses:

- an order returns `{"type": "placed", "order_id", "status": "resting|filled|canceled", "filled", "remaining"}`;
- a cancel returns `{"type": "canceled", "order_id", "remaining"}`;
- cancel all returns `{"type": "canceled_all", "count"}`;
- a withdrawal returns `{"type": "withdraw_queued", "nonce"}`. The funds arrive after the next batch
  settles on-chain.

Status codes:

| Status | Codes |
|---|---|
| 401 | `bad_key`, `bad_signature`, `unknown_account`, `key_not_registered`, `stale_timestamp`, `replay` |
| 429 | `rate_limited` |
| 404 | `unknown_market` |
| 409 | engine rejects: `insufficient_margin`, `price_out_of_band`, `mark_unavailable`, `insufficient_balance`, `post_only_would_cross`, `fok_unfillable`, `duplicate_client_id`, `too_many_open_orders`, `invalid_qty`, `invalid_price`, `market_halted`, `unknown_order`, `nonce_too_low`, ... |
| 503 | sequencer queue full |

## WebSocket `/v1/ws`

Client to server:

```json
{"op": "subscribe", "channel": "book", "market": "NEAR-USDC"}
{"op": "subscribe", "channel": "trades", "market": "NEAR-USDC"}
{"op": "subscribe", "channel": "perp", "market": "NEAR-PERP"}
{"op": "auth", "account": "...", "ts": 1700000000000, "public_key": "ed25519:...", "signature": "..."}
{"op": "unsubscribe", "channel": "book", "market": "NEAR-USDC"}
```

Server to client:

- Book: first a `{"channel":"book","type":"snapshot","market","seq","bids","asks"}`, then
  `{"channel":"book","type":"delta","seq","bids","asks"}` where each level is `[price, total qty]`
  and a qty of 0 removes the level.
  - Deltas are sequenced per market. A delta whose `seq` isn't `last + 1` is a gap: resubscribe to
    get a fresh snapshot. Both SDKs do this automatically.
  - A slow client gets `{"type":"lagged"}` followed by fresh snapshots.
- Trades: `{"channel":"trades","market","trade":{trade_id, price, qty, taker_side, ts_ns}}`.
- Perp: `{"channel":"perp","type":"mark","market","mark","index"}`,
  `{"type":"funding","rate_ppm","payment_per_lot"}` and `{"type":"socialized_loss","amount","accounts"}`.
- Account, after auth: `accepted`, `fill` (with role, fee, remaining), `canceled` (with reason),
  `rejected`, `deposited`, `credited`, `withdraw_queued`, `forced_withdrawal_queued`, and for
  perps `position`, `liquidated`, `insurance_takeover`, `bankruptcy`.

## Internal (operator only)

Header `Authorization: Bearer <admin_token>`. Keep these endpoints off the public internet.

| Method | Path | Purpose |
|---|---|---|
| GET | `/internal/status` | `chain_cursor`, `last_settled`, `next_batch_id`, `unconfirmed`, `pending`, `accounts` |
| POST | `/internal/chain_events` | `{events: [...]}`. Contract events plus `seq`, applied in order. Stops at a gap |
| GET | `/internal/batches` | Unconfirmed batches in the exact `settle_batch` argument format |
| POST | `/internal/cut` | Cut a batch now |
| POST | `/internal/halt` | `{market, halted}` |
| POST | `/internal/mark` | `{market, mark, index?}`. Manual oracle update (tests, an external oracle) |
| POST | `/internal/funding` | `{market}`. Apply funding now |
