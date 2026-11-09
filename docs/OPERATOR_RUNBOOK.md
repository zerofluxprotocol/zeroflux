# Operator runbook (testnet)

Every procedure here has an automated test; the test name is given so you can rehearse it on a localnet first (`make localnet`). Times are for the localnet; testnet timelocks are longer.

## 0. Bring-up

| what | command |
|---|---|
| full local stack (3 nodes, NEAR sandbox, wZFX, 3 relayers, coordinator) | `make localnet` (stop: `make localnet-down`) |
| a node | `zfx-node --config chain.json --data-dir DIR --peer HOST:9440 [--bridge-confirmations K]` |
| relayer i | `zfx-relayer --config relayer.json` (seed file 0600, its own ZeroFlux node, NEAR RPC) |
| coordinator | `zfx-bridge-coordinator --env bridge_env.json --zfx URL --near-account ID --near-key-file F --store jobs.json --incidents DIR --status-port 28790` |
| health | node `GET /health` (`supply_audit` must be `ok`); coordinator `GET :28790/status`; `zfx-bridge reconcile --env bridge_env.json --zfx URL` |

Keys stay outside the repository: relayer seeds and NEAR keys in 0600 files under the operator's data directory, admin tokens in `DATA_DIR/admin_token`.

## 1. Emergency pause (any one relayer, or the NEAR owner)

    zfx-bridge admin-sign --seed SEED --member I --chain near   --contract wzfx.ACCOUNT --epoch E --nonce N --action pause
    zfx-bridge call ... --method bridge_pause --args '{"approvals":[<approval>]}'
    zfx-bridge admin-sign --seed SEED --member I --chain native --chain-id C --epoch E --nonce N --action pause
    POST /rpc zfx_bridge_submit_op {"op":{"op":"admin","epoch":E,"op_nonce":N,"action":"pause","approvals":[<approval>]}}

Read `E` and `N` from `bridge_state` (NEAR: `epoch`, `admin_nonce`) and `zfx_bridge_info` (native: `epoch`, `admin_nonce`). Test: `bridge_governance.py` G1.

The coordinator does this automatically when reconciliation fails 3 times in a row (`bridge_reorg.py` scenario 2).

## 2. Unpause (2 of 3)

Same commands with `--action unpause`, two approvals from different members, nonce = current admin nonce. The owner cannot unpause alone. Test: G1.

## 3. Incident handling

1. The coordinator writes `incidents/incident-<ms>.json` with the failing reconciliation report and stops processing. Both chains get paused.
2. Do not mint, burn or release anything to "fix" balances.
3. Find the cause from the report: `orphans` (mint without deposit, release without burn), `invariant_ok`, `native_custody_ok`, `wrapped_supply_ok`. Typical causes: a reorg deeper than `K` (raise `K`), a compromised member (rotate, section 5), a bug (stop and escalate).
4. When the books are clean again, resume the coordinator:

       zfx-bridge-coordinator ...same flags... --resume-after-incident incidents/incident-<ms>.json

   It refuses (exit 2) unless a fresh reconciliation is fully clean, and records the resume inside the incident file. Then unpause both chains with 2-of-3 (section 2). Tests: `bridge_reorg.py` (refusal while dirty), `bridge_governance.py` G5 (resume with clean books).

An unbacked mint after a deep reorg cannot be cleaned up by software. Resolving it (for example by an owner-funded backing deposit) is a governance decision outside this runbook.

## 4. Relayer or coordinator restart and loss

- Relayer restart: safe at any time; signing records in its data dir stop it from signing conflicting amounts. Test: `bridge.py` chaos (`restart_relayer`).
- Coordinator crash: restart with the same flags; leased jobs resume. Test: `bridge.py` chaos (`kill_coordinator`).
- Coordinator job store lost: start it with an empty store. It rebuilds jobs from chain state; already-minted deposits and released withdrawals are skipped by on-chain replay protection. Test: G4 (zero duplicate mints).

## 5. Committee rotation (2 of 3, timelocked)

1. Each new member runs `zfx-bridge keygen-relayer --out SEED` and publishes `ed25519` and `slh`.
2. Two current members sign `--action rotate --committee K1,K2,K3` for NEAR (ed25519 keys) and for native (SLH keys plus `--activate-height H`, at least `rotation_timelock_blocks` ahead; leave a margin, SLH-DSA signing takes seconds).
3. Submit `propose_rotation` on NEAR; after `timelock_sec` anyone calls `apply_rotation`. Submit the native `rotate` op; it activates at height `H`.
4. Restart the relayers on the new seeds and regenerate the coordinator env file. Retire the old seeds.

Test: G2 (round trip on the new committee; retired keys rejected on both chains).

## 6. Contract upgrade (owner, timelocked)

    sha256sum new.wasm
    zfx-bridge call ... --method stage_upgrade --args '{"code_hash":"<sha256>"}'
    # wait timelock_sec
    zfx-bridge call ... --method deploy_upgrade --raw-args-file new.wasm --gas 300

A different WASM, an early deploy, or a non-owner caller is rejected. Token state carries over through `migrate`. Test: G3.

## 7. Node backup, restore and disaster recovery

- Backup: stop the node cleanly (SIGTERM writes a snapshot and a clean-shutdown marker), copy `blocks.zfxlog`, `chain.json`, `snapshots/latest.json`. Test: `node_integrity::backup_and_restore_reproduce_state`.
- Restore: copy into a new data dir and start; every block is fully revalidated on load. Verify a snapshot offline with `zfx_admin_snapshot_verify`.
- Corrupted log: the node refuses to start and names the bad line. `zfx-node check-store --data-dir DIR` inspects; `--repair` truncates at the last good record and the node resyncs the rest from peers. A torn final write from a crash is repaired automatically. Tests: `node_integrity`.
- Wallet: `zfx-wallet backup --out FILE` (checksummed seed, 0600), `zfx-wallet restore --backup FILE --wallet NEW`.

## 8. Things that are never done

No mainnet deployment, no live token issuance, no real assets, and no automatic mint or release to cover a discrepancy. `network: "mainnet"` is refused by the node until the owner signs off on the monetary schedule and the gates in `docs/RELEASE_READINESS.md`.
