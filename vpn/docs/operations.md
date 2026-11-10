# Operations: keys, backup, recovery, retention

## Where data lives

- macOS: `~/Library/Application Support/network.zeroflux.zfx-vpn/`
- Linux: `$XDG_DATA_HOME/zfx-vpn` (usually `~/.local/share/zfx-vpn`)
- Override with `ZFX_VPN_HOME` (the Prompt VPN name `PV_HOME` is still honored).

Files: `zfx-vpn.db` (SQLite, 0600), `admin.token` (0600), `apps/*.key` (local caller keys, 0600), `client-backups/`, `models/gliner-pii-edge/`, `daemon.log`. The directory is 0700.

## Keys

A key set (256-bit data keys with IDs `k1`, `k2`, ...) is stored in the OS credential store under service `zfx-vpn` (Prompt VPN used `promptvpn`; see Upgrades). Vault records use XChaCha20-Poly1305 with a random 192-bit nonce and associated data naming the table and row, so a ciphertext cannot be moved to another row. Subkeys derived with HKDF-SHA256 key the fingerprint HMAC and the audit chain MAC.

`ZFX_KEY_BACKEND=file` (legacy `PV_KEY_BACKEND`) stores the key set in `keys.insecure.json` (0600) for headless machines. Health reports this as degraded; the setup skill stops unless the user accepts it.

If encrypted records exist and no key can be loaded, the vault refuses to open (`ZFX_VAULT_LOCKED`) rather than creating a new key and orphaning data.

## Rotation

`zfx-vpn vault rotate` creates a new key, re-encrypts every secret, mapping, pending payload and sealed record, recomputes fingerprints, re-MACs the audit chain (after verifying it), then deletes the old key. Run it after suspected exposure of the keychain entry.

## Backup and recovery

1. `zfx-vpn vault recovery-export recovery.json` writes the key set wrapped with your passphrase (Argon2id m=64 MiB, t=3, then XChaCha20-Poly1305). Keep it offline.
2. Back up the data directory (stop the daemon first, or use `sqlite3 zfx-vpn.db ".backup copy.db"`).
3. To restore on a new machine: copy the data directory, run `zfx-vpn vault recover recovery.json` with the daemon stopped, then `zfx-vpn start`.

Without the recovery bundle, a lost keychain entry means the encrypted contents (secrets, mappings, custom entity types) are unrecoverable. Policies, settings and audit metadata are not encrypted and remain readable, but the audit chain cannot be verified without the key.

## Retention

- Audit events: 30 days by default (`retention_days` in Settings). Expired events are deleted as a contiguous prefix and the last deleted MAC becomes the chain anchor, so `zfx-vpn events verify` still passes.
- Held request payloads: destroyed 15 minutes after creation (configurable), on deny/cancel, or when the approval is used. Expiry runs every 30 seconds and at startup, so a crash cannot keep a payload alive past its TTL.
- Placeholder mappings: 24 hours from last use.
- `zfx-vpn events export` writes metadata JSONL; `zfx-vpn events delete` removes all events and records a new anchor.

## Upgrades

Database migrations are ordered and append-only (`PRAGMA user_version`). A database from a newer release is refused rather than modified. Automatic updates are not built in; re-run `install.sh`.

### From Prompt VPN 0.1

The first `zfx-vpn` command (or the `promptvpn` link, which runs the same binary) finds a Prompt VPN install in the old default directory or in `PV_HOME`, and migrates it once:

1. Refuses if a Prompt VPN daemon is still running from it (stop it with `promptvpn stop`).
2. Checkpoints the SQLite WAL and copies `promptvpn.db` byte for byte to `backups/promptvpn.db.pre-zfx-vpn-<UTC time>`.
3. Keychain mode: copies the key set from `promptvpn`/`keyset:<hash of old path>` to `zfx-vpn`/`keyset:<hash of new path>`, and each provider credential from `promptvpn`/`cred:<ref>` to `zfx-vpn`/`cred:<ref>`, reading every copy back. The old entries are kept. If the old key set is missing while encrypted records exist, nothing is changed. On macOS the new binary's first read of the old item can show a keychain prompt; choose Always Allow.
4. Default location: renames the directory to the new default and leaves a symlink at the old path. With `PV_HOME`/`ZFX_VPN_HOME` the directory stays where it is.
5. Renames `promptvpn.db` to `zfx-vpn.db` and applies schema 2 (adds the `privacy_routes` table only).
6. Writes `migration.json` describing all of the above.

No record is decrypted or re-encrypted, and the audit MAC chain is untouched. A Prompt VPN `launchd`/`systemd` unit keeps working (its `PV_HOME` still resolves); `zfx-vpn service install` replaces it with the ZFX VPN unit.

Rollback: stop the daemon, run `zfx-vpn migrate rollback`, then reinstall Prompt VPN 0.1 (the installer keeps the old binary as `promptvpn.0.1.bak`). Rollback drops schema 2 (route records only), writes the *current* key set back to the old keychain entry (so keys rotated after the upgrade still work), copies credentials back, renames the database and directory back, and removes the ZFX VPN keychain entries. Placeholders minted after the upgrade (`[[ZFX_...]]`) are not understood by 0.1 and stay unrestored until they expire (24 h).

Tested by `crates/pv-daemon/tests/migration.rs` (on a fixture written by the 0.1 binary) and `tests/migration/upgrade_rollback.sh` (builds 0.1 from source, upgrades through the `promptvpn` link, rolls back, and serves the install with 0.1 again).

## Uninstall

`zfx-vpn uninstall` stops the daemon and removes the autostart unit and keeps data. `--purge` also deletes the data directory and the keychain entry after you type `DELETE`. Client configs are restored only through `zfx-vpn client rollback <client>`.
