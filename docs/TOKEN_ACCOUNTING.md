# Token accounting

All amounts are integers in base units. 1 ZFX = 10^8 base units on the native chain, and wZFX uses `decimals: 8`, so one base unit maps to one wZFX base unit with no rounding anywhere.

## Native supply

- Supply grows only through coinbase subsidies. `crates/zfx-verify/src/monetary.rs` defines the schedule families (`Constant`, `Halving`, `SmoothDecay`, each with an optional tail); testnet uses `Constant` 50 ZFX per block.
- Fees are paid to the block's miner, so total supply = genesis allocations + sum of subsidies. Each node recomputes this audit on every `/health` request (`supply_audit`), and `tests/e2e/native.py` checks it at 1000 blocks.
- Coinbase outputs mature after `coinbase_maturity` blocks (testnet 20, bridge localnet 5). Immature balances are reported separately (`immature` in `zfx_get_account`) and cannot be spent.

The production schedule is an **owner decision**. `bench/results/tokenomics.json` compares four candidates; the data-backed recommendation is `D_smooth_decay_tail` (no halving shocks, perpetual tail). A node refuses `network: "mainnet"` until a schedule is chosen and signed off.

## Bridge accounts

| quantity | where it lives | changes on |
|---|---|---|
| `locked_total` | native consensus state (`zfx_bridge_info`) | each `zfx_bridge_lock` deposit included in a block |
| `released_total` | native consensus state | each 2-of-3 `Release` op included in a block |
| lockbox balance | native account `sha256("ZFX-bridge-lockbox-v1" \|\| chain_id)`, no key exists | deposits in, releases out |
| `total_minted` | wZFX contract (`bridge_state`) | each `bridge_mint` |
| `total_burned` | wZFX contract | each `bridge_burn`, plus `refund_burned` when an `ft_transfer_call` refund targets an unregistered account |
| `ft_total_supply` | wZFX contract (NEP-141) | equals `total_minted - total_burned` |

Deposit fees are ordinary native transaction fees paid by the depositor. The bridge itself takes no fee in v1.

## Identities checked by reconciliation

1. Backing: `minted - burned <= locked - released` at every moment.
2. Exact identity with in-flight items: `(locked - released) - (minted - burned) == pending_deposits - pending_releases`, where pending deposits are finalized locks not yet minted and pending releases are finalized burns not yet released.
3. Custody: lockbox balance `== locked - released`.
4. Wrapped supply: `ft_total_supply == total_minted - total_burned`.
5. No orphans: every mint references an on-chain deposit; every release references a NEAR burn.

`tests/e2e/bridge.py` samples identity 1 throughout 100 round trips under fault injection and checks all five at quiescence, plus per-user conservation on both chains (native = initial - deposits - fees + releases; wZFX = deposits - burns). Results: `bench/results/e2e_bridge.json`.

## What the system never does

It never mints, burns, releases or edits balances to repair a discrepancy. A failed identity halts the bridge and produces an incident record; resolution is a human decision documented in `docs/OPERATOR_RUNBOOK.md`.
