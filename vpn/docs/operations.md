# Operations: keys, backup, recovery, retention

## Where data lives

- macOS: `~/Library/Application Support/dev.promptvpn.promptvpn/`
- Linux: `$XDG_DATA_HOME/promptvpn` (usually `~/.local/share/promptvpn`)
- Override with `PV_HOME`.

Files: `promptvpn.db` (SQLite, 0600), `admin.token` (0600), `apps/*.key` (local caller keys, 0600), `client-backups/`, `models/gliner-pii-edge/`, `daemon.log`. The directory is 0700.

## Keys

A key set (256-bit data keys with IDs `k1`, `k2`, ...) is stored in the OS credential store under service `promptvpn`. Vault records use XChaCha20-Poly1305 with a random 192-bit nonce and associated data naming the table and row, so a ciphertext cannot be moved to another row. Subkeys derived with HKDF-SHA256 key the fingerprint HMAC and the audit chain MAC.

`PV_KEY_BACKEND=file` stores the key set in `keys.insecure.json` (0600) for headless machines. Health reports this as degraded; the setup skill stops unless the user accepts it.

If encrypted records exist and no key can be loaded, the vault refuses to open (`PV_VAULT_LOCKED`) rather than creating a new key and orphaning data.

## Rotation

`promptvpn vault rotate` creates a new key, re-encrypts every secret, mapping, pending payload and sealed record, recomputes fingerprints, re-MACs the audit chain (after verifying it), then deletes the old key. Run it after suspected exposure of the keychain entry.

## Backup and recovery

1. `promptvpn vault recovery-export recovery.json` writes the key set wrapped with your passphrase (Argon2id m=64 MiB, t=3, then XChaCha20-Poly1305). Keep it offline.
2. Back up the data directory (stop the daemon first, or use `sqlite3 promptvpn.db ".backup copy.db"`).
3. To restore on a new machine: copy the data directory, run `promptvpn vault recover recovery.json` with the daemon stopped, then `promptvpn start`.

Without the recovery bundle, a lost keychain entry means the encrypted contents (secrets, mappings, custom entity types) are unrecoverable. Policies, settings and audit metadata are not encrypted and remain readable, but the audit chain cannot be verified without the key.

## Retention

- Audit events: 30 days by default (`retention_days` in Settings). Expired events are deleted as a contiguous prefix and the last deleted MAC becomes the chain anchor, so `promptvpn events verify` still passes.
- Held request payloads: destroyed 15 minutes after creation (configurable), on deny/cancel, or when the approval is used. Expiry runs every 30 seconds and at startup, so a crash cannot keep a payload alive past its TTL.
- Placeholder mappings: 24 hours from last use.
- `promptvpn events export` writes metadata JSONL; `promptvpn events delete` removes all events and records a new anchor.

## Upgrades

Database migrations are ordered and append-only (`PRAGMA user_version`). A database from a newer release is refused rather than modified. Automatic updates are not built in; install a newer signed release with the setup skill's `install.sh`.

## Uninstall

`promptvpn uninstall` stops the daemon and removes the autostart unit and keeps data. `--purge` also deletes the data directory and the keychain entry after you type `DELETE`. Client configs are restored only through `promptvpn client rollback <client>`.
