# Trust model

obkit makes the same bargain as Orderly. A trusted operator orders and matches trades off-chain;
custody and settlement stay on-chain. This page lists what the contract enforces and what it
leaves to trust.

## The contract enforces

| Property | How |
|---|---|
| The operator can't create value | Each token's running delta must be zero at every sealed batch |
| No balance goes negative | Every delta is checked |
| Batches can't be replayed or skipped | `id` must be `last_batch_id + 1` |
| Only the owner authorizes a withdrawal | ed25519 signature by a key the account registered, over a message naming this contract |
| A withdrawal can't be replayed | The nonce must increase per account |
| Funds can't be redirected | Payouts always go to the account itself (`ft_transfer` to the owner) |
| Users can leave without the operator | A forced withdrawal not honored within `force_timeout_ns` lets anyone call `freeze`. After that, `escape_withdraw` pays each user their settled balance and `settle_batch` is disabled |
| Deposits stop when frozen | `ft_on_transfer` refunds |

## What you trust the operator with

- **Ordering and matching.** The operator decides which order arrives first, and could match
  orders that were never placed. The contract sees net balance changes, not trades.
- **Liveness.** If the operator stops, trading stops, and so do withdrawals through the node.
  The escape hatch covers this, but only at the last settled balances: trades in a batch that
  never landed are lost.
- **Perp marks.** The operator's oracle sets the mark that decides liquidations and settles PnL.
  Marks are journaled with the per-source prices published at `/v1/perp/{market}`, so a bad mark
  is visible and replayable, but the contract does not check prices.
- **Signed withdrawals sitting in its queue.** The operator could delay submitting them. The forced
  withdrawal deadline bounds that delay.

## Auditing

Every batch records `journal_seq` and `journal_hash` (blake3 hash chain over the command log), and
the contract stores the latest ones. Anyone with the journal (`Journal::read_all`) can:

1. check that the hash chain matches the on-chain hash;
2. replay the commands through `zfx-perps-engine`;
3. check that the batches it produces equal the ones posted on-chain.

Orders in the journal carry no user signatures, since the gateway verifies them before
sequencing. To make matching itself provable, the next step would be to journal the signed order
bytes too, so a replay can check that every fill came from a real signed order.

## The trading key

The trading key is an ed25519 key the app creates, separate from the user's NEAR account key. If it
leaks, the attacker can trade (and lose money for the user against the book), cancel orders, and
sign withdrawals. Those withdrawals still pay out only to the user's own account. The user revokes
the key with `remove_key` from their wallet.
