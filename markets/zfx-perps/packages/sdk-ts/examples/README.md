# Examples

Both examples use the v1 clearing API (`src/clearing`). `pnpm typecheck` type-checks them; the
tests do not run them. They need a running operator node that serves `openapi.yaml`, and an
account with collateral and a trading key registered on the clearing contract.

## market-maker.ts

Quotes a post-only ladder around the oracle mark on one market. When the mark moves it sends one
signed `setMinNonce`, which kills every resting quote here and on-chain, then quotes again with
larger nonces. Each quote is checked with the local `estimateMargin` against the account's
pending free collateral. If the oracle print fails `checkOracle` or the market is not ACTIVE, it
pulls all quotes and waits.

```sh
cd packages/sdk-ts
ZFX_NODE_URL=http://127.0.0.1:8796 \
ZFX_ACCOUNT=alice.testnet \
ZFX_TRADING_KEY=ed25519:<base58 secret> \
ZFX_MARKET=OPUS-PERP \
ZFX_NEAR_RPC=https://rpc.testnet.near.org \
pnpm dlx tsx examples/market-maker.ts
```

Optional: `ZFX_LEVELS` (default 3 per side), `ZFX_STEP_BPS` (25), `ZFX_SIZE` (10 lots),
`ZFX_POLL_MS` (2000). With `ZFX_NEAR_RPC` set, the reporter allowlist and quorum come from the
contract's `get_state` instead of the node. Ctrl-C pulls all quotes before exiting.

## retail-ui/

One page: markets, oracle provenance (print hash, source window, signers, rejections), an order
ticket with a local margin estimate, and positions shown twice, as pending (node prediction,
includes unsettled fills) and settled (last batch confirmed on NEAR). The submit button stays
disabled unless the print verifies and the market is ACTIVE (or CLOSE_ONLY with reduce-only).

```sh
cd packages/sdk-ts
pnpm dlx vite examples/retail-ui
# open http://localhost:5173/?node=http://127.0.0.1:8796&rpc=https://rpc.testnet.near.org
```

The page generates a trading key and keeps it in `localStorage`. "Show register_key
transaction" prints the wallet transaction that registers it (trading only, no withdrawals);
sign it with your NEAR wallet, then enter the account id. The node must allow the page's origin
(CORS) for the REST calls.
