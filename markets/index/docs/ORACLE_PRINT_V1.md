# OraclePrint v1: canonical encoding and signature

Status: frozen. Changing any byte of the layout requires a new domain tag (`ZFX-oracle-print-v2`).
Implementations: Rust `crates/zfx-oracle-print` (no_std + alloc, builds for `wasm32-unknown-unknown`),
JavaScript `js/oracle-print.mjs` (no dependencies). Both are checked against
`vectors/oracle_print_v1.json` by `cargo test` and `node js/test-vectors.mjs`.

## Fields

The JSON field names follow the `OraclePrint` interface in spec section 5.2. v1 adds
`valueScale`, `dataStatus`, `finality`, `revision` and `prevPrintHash` so a contract can
tell a missing day from a zero day and follow the correction chain.

| JSON | Canonical type | Meaning |
|---|---|---|
| `indexId` | str | `or-model-spend/<model_permaslug>` (daily) or `or-model-spend-7d/<model_permaslug>` (trailing 7-day, traded) |
| `version` | str | formula version, `ZFX-OR-MODEL-SPEND-USD-v1` or `ZFX-OR-MODEL-SPEND-USD-7D-v1` |
| `valueScale` | u8 | decimal places of `value`; 6 means micro-USD |
| `value` | u64 | fixed-point value; `"42.500000"` in JSON is `42500000` on the wire. 0 when `dataStatus != OK` |
| `sourceStartUtc` | i64 ms | inclusive start of the UTC day (7D: start of day d-6) |
| `sourceEndUtc` | i64 ms | exclusive end (next 00:00:00.000Z) |
| `observedAt` | i64 ms | fetch time of the token response that decided the value |
| `publishedAt` | i64 ms | when the reporter created the print |
| `validUntil` | i64 ms | `sourceEndUtc + 48 h`; treat as STALE after this |
| `sourceHash` | 32 bytes | `SHA-256("ZFX-source-set-v1" ‖ u32le(n) ‖ h1 ‖ … ‖ hn)` over the raw response hashes used for the value (token response, then price response) |
| `formulaHash` | 32 bytes | SHA-256 of `formula/ZFX-OR-MODEL-SPEND-USD-v1.txt` |
| `status` | u8 | 0 FRESH, 1 STALE, 2 DISPUTED, 3 UNAVAILABLE |
| `dataStatus` | u8 | 0 OK, 1 NOT_REPORTED, 2 UNPRICED, 3 NO_VALID_CLOSE_PRICE |
| `finality` | u8 | 0 PROVISIONAL (reserved, not produced), 1 FINAL, 2 CORRECTED |
| `revision` | u32 | 1 for the first print of a day |
| `prevPrintHash` | 32 bytes | `printHash` of the revision this one supersedes; zeros for revision 1 |

## Byte layout

All integers little-endian. Strings are a u32 little-endian byte length followed by UTF-8
bytes, at most 256 bytes. No padding, no optional fields, no trailing bytes.

```text
offset  size  field
0       19    "ZFX-oracle-print-v1" (ASCII, no length prefix)
19      4+n   index_id
.       4+n   version
.       1     value_scale
.       8     value (u64)
.       8     source_start_ms (i64)
.       8     source_end_ms (i64)
.       8     observed_at_ms (i64)
.       8     published_at_ms (i64)
.       8     valid_until_ms (i64)
.       32    source_hash
.       32    formula_hash
.       1     status
.       1     data_status
.       1     finality
.       4     revision (u32)
.       32    prev_print_hash
```

Fixed part: 19 + 8 (two length prefixes) + 1 + 8 + 40 + 64 + 3 + 4 + 32 = 179 bytes, plus the two strings.

## Hash and signature

- `printHash = SHA-256(canonical bytes)`.
- Reporter signature: Ed25519 (RFC 8032, pure Ed25519, no prehash) over the canonical bytes.
  The domain tag is the first 19 bytes of the message, so a print signature cannot be replayed
  as any other ZeroFlux message that uses a different tag.
- Published form: `reporterSignatures: ["ed25519:<pubkey hex>:<signature hex>"]`.
- Verifiers should use strict verification (reject small-order keys and non-canonical S).
  `ed25519-dalek::VerifyingKey::verify_strict` in Rust; NEAR contracts can call
  `env::ed25519_verify(signature, message, public_key)` with the canonical bytes as message.

## Consumer rules

1. Decode strictly; reject unknown enum values and trailing bytes.
2. Check the signature against the configured reporter key set (today one key; see
   `public/v1/oracle/reporter.json`). Multi-reporter quorum is future work.
3. Check `formulaHash` and `version` against the market spec's `indexVersion`/`policyHash`.
4. `dataStatus != OK` means no value. Never read `value = 0` as zero demand.
5. Prefer the highest `revision` for a `(indexId, sourceStartUtc)`; follow `prevPrintHash` to audit.
6. `now > validUntil` means STALE; per spec 5.4 the market goes CLOSE_ONLY or HALTED.

## Trust note

A valid signature proves that the holder of the reporter key signed this print. It does not
prove the OpenRouter input is true, complete, or free of manipulation. Prints are for charts
and paper trading only.
