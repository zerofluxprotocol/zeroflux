# Contract API

Versioned public interfaces. Machine-readable schemas live in `schemas/`; `tests/contract_api/test_contract_api.py` checks a live node against them and writes `bench/results/contract_api.json`. Captured fixtures are in `schemas/fixtures/`.

## 1. ZFX token contract on NEAR (`zfx-mine-opt`)

NEP-141 (`ft_transfer`, `ft_transfer_call`, `ft_balance_of`, `ft_total_supply`), NEP-145 storage and NEP-148 metadata (symbol `ZFX`, 8 decimals), plus:

| method | caller | effect |
|---|---|---|
| `new(genesis_seed, genesis_target, spacing_secs, reward, bond, window_secs)` | deployer | initialize |
| `work(miner)` (view) | anyone | next `height`, `profile` (1), `epoch_seed`, `target`, `work_bytes`, `challenge` |
| `submit(height, nonce, trace)` | miner, with `bond` attached | checks the output digest against the target; stores the block as pending. `trace` is base64 of a1..a4 (4,096 int8 each) followed by 256 int32le outputs |
| `challenge(height, layer, row)` | anyone | recomputes one value; on a mismatch reverts the block and its descendants and pays the bond to the caller |
| `finalize(max)` | anyone | mints rewards and returns bonds for blocks past the window |
| `block(height)` (view) | anyone | a pending block: miner, nonce, challenge, epoch seed, digest, trace |
| `stats()` (view) | anyone | heights, target, reward, bond, window, frauds, supply |

Byte layouts (work bytes, challenge, chain hash, result digest) are in `NEAR_SETTLEMENT.md`. Events (`EVENT_JSON`, standard `zfx-mine-opt`): `submitted`, `fraud`, plus NEP-141 `ft_mint` with memo `zfx-block:<height>`. Errors asserted by tests: `stale height: another miner found this block`, `digest does not meet the target`, `attach the bond`, `no fraud: the value is correct`, `block is not pending`.

## 2. NEAR Intents adapter (`bridge/intents/zfx_intents/adapter.py`)

| operation | maps to | terminal states |
|---|---|---|
| `intents.quote(token_in, token_out, exact_amount_in \| exact_amount_out, min_deadline_ms)` | Message Bus `quote` | list of solver quotes (empty when no liquidity or stale price) |
| `intents.submit(quote, signer, key, verifier, salt)` | signs a NEP-413 `token_diff` mirroring the quote; Message Bus `publish_intent` | returns `intent_hash` |
| `intents.status(intent_hash)` | Message Bus `get_status` | `SETTLED` (with NEAR tx hash) or `NOT_FOUND_OR_NOT_VALID` |

Token ids are `nep141:<contract>`. Nonces follow Verifier defuse/v0.4.2: `5628f6c6 | 0x00 | salt[4] | deadline_ns i64le | random[15]`, base64. Payload: `{"standard":"nep413","payload":{"recipient","nonce","message"},"public_key","signature"}`. See `docs/INTENTS_INTEGRATION.md`.

## Alternative design: own chain with a committee bridge

ZFX on NEAR uses none of this. In the current design the contract above mints ZFX directly on NEAR, so there is no wrapped token and no bridge. These interfaces belong to the own-chain alternative (`BRIDGE_TRUST_MODEL.md`): ZFX lives on a separate ZeroFlux chain, and wZFX is the copy a 2-of-3 committee mints on NEAR against ZFX locked there. They are kept because that design is still built and tested.

### ZeroFlux node JSON-RPC (API v1, consensus v2)

Transport: `POST /rpc`, JSON-RPC 2.0. `params` is an object or a one-element array holding the object. Schema: `schemas/zfx-node-rpc-v1.json` (every method's params, result and listed error codes).

| method | purpose | notes |
|---|---|---|
| `zfx_node_get_tip` | height, hash, state root, target, chain id, consensus version | deterministic |
| `zfx_node_get_block` | by `hash` (64 hex) or `height` | a 64-char key is always a hash |
| `zfx_node_get_transaction` | tx, status, block, confirmations, receipt | receipt includes the header and a Merkle inclusion proof against `tx_root` |
| `zfx_get_account`, `zfx_get_address_history` | balance, immature, nonce, pending; history | |
| `zfx_estimate_fee` | median fee of recent blocks, floor 1000 | |
| `zfx_submit_transaction` | full validation, then mempool | idempotent: an identical resubmission returns the same txid with `duplicate: true` |
| `zfx_bridge_lock` | a transaction to the lockbox; memo = NEAR recipient | `deposit_id` = txid |
| `zfx_get_mining_work`, `zfx_submit_mining_result` | mining | stale work returns 1002 |
| `zfx_bridge_info` | committee, epoch, admin nonce, paused, totals, trust model text | |
| `zfx_bridge_deposit_status` | `unknown`, `mempool`, `awaiting_confirmations`, `final` | `final` includes block, height, Merkle inclusion proof |
| `zfx_bridge_redeem_status` | `unknown`, `mempool`, `released_awaiting_confirmations`, `final` | |
| `zfx_bridge_submit_op` | 2-of-3 release or admin op | consensus enforces quorum, nonce, replay, timelock |
| `zfx_bridge_list_deposits`, `zfx_bridge_list_releases` | indexes used by relayers and reconciliation | |
| `zfx_faucet` | testnet only, rate limited | |
| `zfx_admin_*` | peer add, snapshot export/verify, shutdown | `Authorization: Bearer <token>`, compared in constant time |

Error codes: `-32700` parse, `-32600` invalid request, `-32601` unknown method, `-32602` invalid params (missing field, bad hex, bad address checksum), `1001` tx rejected, `1002` stale work, `1003` not found, `1004` unauthorized, `1005` rate limited, `1006` bridge rejected, `1007` faucet, `1099` internal.

Chain identity: `chain_id` u32 (testnet `0x5a460002`); `consensus_version` 2; header magic `ZFX2`, 164 bytes.

#### Signing bytes

Transaction (wallet, SLH-DSA, FIPS 205 context `ZFX-v0.1`):

    core   = chain_id u32le | from[32] | to[32] | amount u64le | fee u64le | nonce u64le
             [| 0x01 | memo_len u8 | memo utf8]          (only when a memo is set; <= 64 bytes)
    signed = "ZFX-tx-v0.1" | core
    txid   = sha256("ZFX-txid-v0.1" | core)

Bridge release and admin approvals (relayer SLH-DSA-SHA2-128s, context `ZFX-bridge-v1`), `put_str` = u16le length + bytes:

    release = sha256("ZFX-bridge-release-v1" | chain_id u32le | put_str(asset_id) | epoch u32le
                     | withdrawal_id[32] | recipient[32] | amount u64le | expiry_height u64le | sha256(near_burn_ref))
    admin   = sha256("ZFX-bridge-admin-v1" | chain_id u32le | epoch u32le | op_nonce u64le | action)
    action  = 0x01 pause | 0x02 unpause | 0x03 (put_str(member_pubkey_hex) x3 | activate_height u64le)

Golden vectors for all of these: `tests/vectors/consensus_v2.json`, recomputed by `cargo test -p zfx-verify`.

### wZFX, the bridged copy of own-chain ZFX on NEAR

NEP-141 (fungible token), NEP-145 (storage), NEP-148 (metadata: symbol `wZFX`, decimals 8). Source `bridge/near/wzfx/src/lib.rs`.

| method | caller | effect / checks |
|---|---|---|
| `ft_metadata`, `ft_total_supply`, `ft_balance_of`, `ft_transfer`, `ft_transfer_call`, `storage_*` | standard | NEP-141/145/148 behavior from `near-contract-standards` |
| `bridge_mint(attestation, approvals)` | anyone (relay) | not paused; `attestation.epoch == epoch`; `block_timestamp <= expiry_ns`; amount > 0; `deposit_id` 32 bytes and never minted; >= 2 distinct valid committee signatures; recipient registered |
| `bridge_burn(amount, native_recipient)` | holder, 1 yoctoNEAR | not paused; valid `zfx1` checksum; burns and records `BurnRecord` with `withdrawal_id = sha256("ZFX-near-burn-v1" \| put_str(contract) \| nonce u64le)` |
| `bridge_pause(approvals?)` | owner, or anyone with 1 committee signature | sets paused |
| `bridge_unpause(approvals)` | anyone with 2 committee signatures | clears paused |
| `propose_rotation(committee, approvals)` / `apply_rotation()` | 2 signatures / anyone after the timelock | new ed25519 keys, epoch + 1 |
| `stage_upgrade(code_hash)` / `deploy_upgrade()` (raw WASM input) | owner | deploy only after the timelock and only the staged sha256; then `migrate` |
| views | anyone | `bridge_state`, `version`, `is_minted`, `get_burn`, `burns_since` |

NEAR-side `put_str` is a u32le length + bytes. Committee signatures are ed25519 over:

    mint  = sha256("ZFX-near-mint-v1" | put_str(contract) | native_chain_id u32le | put_str(native_asset_id) | epoch u32le
                   | deposit_id[32] | put_str(recipient) | amount u128le | native_height u64le | expiry_ns u64le)
    admin = sha256("ZFX-near-admin-v1" | put_str(contract) | epoch u32le | admin_nonce u64le | action)
    action = 0x01 pause | 0x02 unpause | 0x03 put_str(key_hex) x3

The relayer computes the same bytes in `bridge/relayers/src/digest.rs`. Golden-vector tests in both crates (`digests_match_golden_vectors`, `digests_match_relayer_golden_vectors`) pin them to identical values.

Events (`EVENT_JSON:` logs): NEP-141 `ft_mint` (memo `zfx-deposit:<deposit_id>`) and `ft_burn` (memo `zfx-withdrawal:<withdrawal_id>`); bridge events with `standard: "zfx-bridge"`, `version: "1.0.0"`: `mint`, `burn`, `pause`, `unpause`, `rotation_proposed`, `rotation_applied`, `upgrade_staged`, `upgrade_deployed`, `refund_burned`.

Panic messages are the error codes on NEAR. Examples asserted by tests: `insufficient committee approvals`, `invalid committee signature`, `deposit already minted (replay)`, `attestation expired`, `attestation for wrong committee epoch`, `bridge paused`, `unpause needs threshold approvals`, `rotation timelock not elapsed`, `owner only`, `upgrade timelock not elapsed`, `WASM does not match the staged code hash`.
