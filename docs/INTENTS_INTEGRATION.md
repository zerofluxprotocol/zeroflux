# NEAR Intents integration

## ZFX on Intents (current design)

ZFX is a NEP-141 token on NEAR, so it trades through NEAR Intents directly, with no bridge. `tests/e2e/near_intents.py` (5/5) mines ZFX into two NEAR accounts with the production proof of work, deposits it into the official Verifier with `ft_transfer_call`, and settles a ZFX to tUSDC swap and the reverse through the Message Bus protocol and a test solver. ZFX in wallets plus ZFX held by the Verifier always equals the minted supply.

The rest of this page describes the protocol mapping, which is the same for any NEP-141 token, and the original tests with wZFX from the own-chain alternative.

## Protocol details

wZFX trades through NEAR Intents the same way any NEP-141 token does: users and solvers hold balances inside the official Verifier contract and settle by signing matching `token_diff` intents. Nothing in this integration is custom protocol; the custom parts are a solver that quotes wZFX and an adapter that talks to the Message Bus.

### What runs where

| component | code | local test | real network |
|---|---|---|---|
| Verifier contract | official `near/intents` release `defuse/v0.4.2`, fetched by `bridge/intents/fetch_verifier.sh`, SHA-256 checked against the release | deployed in the NEAR sandbox | `intents.near` (not touched by this repo) |
| Message Bus | local implementation of the documented JSON-RPC (`quote`, `publish_intent`, `get_status`) and WebSocket (`subscribe`, `quote_response`, `quote_status`) | `127.0.0.1:28810/28811` | public relay `solver-relay-v2.chaindefuser.com`, needs a Partner JWT |
| test solver | `bridge/intents/zfx_intents/solver.py` | quotes wZFX against `tUSDC` (a test NEP-141) | same code with `--api-key JWT` |
| adapter | `bridge/intents/zfx_intents/adapter.py` | `intents.quote/submit/status` | same code, `X-API-Key` header |

### Protocol mapping

- Token ids `nep141:<contract>`.
- The user's intent is a NEP-413 signed `token_diff` that mirrors the chosen quote exactly (in amount negative, out amount positive). The solver signs the opposite diff. The Verifier settles both atomically in `execute_intents` or neither.
- Nonce (32 bytes, base64): `5628f6c6 | 0x00 | salt[4] | deadline_ns i64le | random[15]`. The salt comes from the Verifier's `current_salt`; the deadline makes the intent expire on-chain.
- NEP-413 hash: `sha256(u32le(2^31 + 413) | borsh(message, nonce[32], recipient, callback_url option))`. `bridge/relayers/tests/nep413_conformance.rs` verifies 13 of the 14 signatures published in the NEAR Intents docs with this implementation; the 14th is the Message Bus RPC page's illustrative `publish_intent` sample, whose message omits the nonce, so it cannot verify. The test also checks that a one-byte change to a message breaks verification.

### Solver risk rules

The solver quotes only when its Verifier inventory of the output token covers the trade, the trade is under `max_trade_fraction` of that inventory, and its price feed is younger than `max_price_age_s`. It never quotes from wallet balances outside the Verifier. Spread is configurable (`--spread-bps`).

### Verified cases (`tests/e2e/intents.py`, `bench/results/e2e_intents.json`, 11/11)

1. Verifier deployed from the pinned release with the published hash; `tUSDC` fixture deployed.
2. Solver and user deposit bridged wZFX and tUSDC into the Verifier.
3. wZFX to tUSDC: quote, NEP-413 `token_diff`, settled on-chain with a receipt.
4. tUSDC to wZFX settles, using the solver's bridged wZFX as liquidity.
5. Insufficient liquidity and a stale price produce no quotes; a fresh price quotes again.
6. Duplicate fill rejected by the bus; on-chain nonce replay rejected by the Verifier.
7. Expired quote refused by the bus; expired deadline refused by the Verifier.
8. Cancellation: an unpublished quote never settles.
9. Refund: a failed `ft_withdraw` to an unregistered receiver returns to the user's Verifier balance.
10. Full loop: native ZFX, bridge to wZFX, swap both ways, bridge back to native ZFX; bridge books exact.
11. Real public Message Bus probed: HTTP 403 without a Partner JWT. Recorded as EXTERNAL_BLOCKED, not as a pass of the real network.

### External prerequisites for live Intents

- A NEAR Intents Partner JWT (KYC through the NEAR Intents partner program) for the public Message Bus.
- Real liquidity: a market maker willing to hold wZFX inventory in `intents.near`. None is solicited or provided by this repository, and no live economic token exists.
- The wZFX contract on mainnet, which is out of scope until the bridge and tokenomics gates in `docs/RELEASE_READINESS.md` pass.
