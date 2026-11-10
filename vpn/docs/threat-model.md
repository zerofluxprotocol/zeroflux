# Threat model

## Assets

| Asset | Where it lives | Protection |
|---|---|---|
| Prompt content | Memory during a request; encrypted pending payload for held requests (max 15 min) | Never logged; pending payload AEAD-encrypted, destroyed on expiry, deny, cancel or use |
| Registered exact values | `secrets` table | XChaCha20-Poly1305, AAD bound to row; keyed HMAC fingerprints, no bare hashes |
| Placeholder mappings | `mappings` table | Encrypted; scoped to app + conversation; expire (24 h default) |
| Custom entity types | `sealed` table | Encrypted |
| Provider credentials | OS credential store (file-backend mode: encrypted in vault) | Never sent to other providers; never in config JSON |
| Data key set | macOS Keychain / Linux Secret Service | File fallback is reported as degraded |
| Audit log | `events` table | Metadata only; HMAC chain with retention anchors |
| Admin token | `admin.token`, 0600 | Constant-time compare |

## Trust boundaries

```
 ┌──────────────── this device (trusted: Prompt VPN process, OS keychain) ────────────────┐
 │                                                                                          │
 │  AI client ──(1) loopback HTTP, pvk_ key──> proxy ──> detectors ──> policy ──> transform │
 │  CLI ───────(2) loopback HTTP, admin token──> admin API                                  │
 │  Browser ───(3) loopback HTTP, session cookie + CSRF──> admin API / dashboard           │
 │  Ollama ────(4) loopback HTTP──> optional local generation (LOCAL tier)                 │
 │                                                                                          │
 └──────────────┬───────────────────────────────────┬──────────────────────────────────────┘
                │ (5) TLS, attested gateway + E2EE   │ (6) TLS to arbitrary endpoint
                ▼                                    ▼
        NEAR AI gateway + model TEE            custom / incognito endpoint
        (ATTESTED_TEE, remote)                 (EXTERNAL_UNVERIFIED)
```

1. Untrusted callers: any local process can reach loopback. Mitigations: per-app keys, Origin refusal, Host allowlist.
2. CLI: admin token file readable only by the user.
3. Browser: web pages can send requests to localhost. Mitigations: Host allowlist (DNS rebinding), Origin and `Sec-Fetch-Site` checks, SameSite=Strict HttpOnly cookie from a single-use ticket, CSRF header on mutations, no CORS headers, strict CSP on the dashboard.
4. Ollama (optional) is a generation destination only; detection never depends on it. The GLiNER model files are pinned by revision and SHA-256.
5. NEAR: attestation verified before private sends; field E2EE to the attested model key; response signatures verified after. Still remote.
6. Unverified endpoints receive only content that policy permits for `EXTERNAL_UNVERIFIED`.

## Threats and mitigations

| Threat | Mitigation | Test |
|---|---|---|
| Critical data reaches a provider | FORBIDDEN block for every remote tier, including TEEs; no matching rule for a critical category also blocks | `gates::ssn_never_reaches_upstream_*`, `routing::critical_pii_never_reaches_tee` |
| Evasion by encoding | Detection over NFKC/confusable-folded, zero-width-stripped, percent/`\u`/`\x`-unescaped and base64-decoded views | `adversarial::encoding_and_unicode_evasion_blocked` |
| Secrets in tool calls, tool results, system prompts | All content strings located by JSON pointer, including `tool_calls[].function.arguments` and tool definitions | `gates::tool_call_arguments_and_tool_results_inspected` |
| Prompt injection changes policy | Policy is data in SQLite; the detection model only returns spans, which can only add findings | `adversarial::prompt_injection_cannot_disable_policy` |
| Detection model missing or failing | `CLASSIFIER_FAILURE` finding, resolved by the fallback setting; never allow | `approvals::block_and_review_fallback_for_classifier_failure` |
| Approval reuse or forgery | Approval bound to request hash (incl. app), destination, policy hash, exact payload hash; single use; only the owner can decide | `approvals::*` |
| Downgrade from TEE to unverified endpoint | Private candidates are only verified TEEs or explicitly allowed local models; failover re-evaluates policy per destination | `routing::no_downgrade_when_tee_unverified_or_failing` |
| NEAR alias swaps the model | `x-no-aliasing` header and catalog alias check | `pv-provider` unit + live test |
| Placeholder restored for the wrong party | Tokens resolve only within `app:conversation` scope and before expiry; malformed tokens ignored | `adversarial::no_cross_conversation_or_cross_app_restoration` |
| Partial leak in streams | 256-byte hold-back, findings never split, PEM blocks held until END, tool-call arguments buffered whole | `gates::streaming_*`, `gates::output_block_policy_stops_stream` |
| Model generates sensitive values | Outbound policy (default: redact critical categories) before restoration | `gates::model_output_critical_values_redacted_*` |
| Credential crossover | Each destination uses only its own credential ref; client keys are never forwarded | `routing::credentials_never_cross_providers_or_leak_client_key` |
| Span model mislabels harmless IDs as critical | Model-only critical spans below 0.8 confidence become UNCERTAIN (review), not a hard block; shape checks on model spans | `pv-detectors` span tests |
| Audit tampering | HMAC chain; retention keeps an anchor; verify command | `adversarial::audit_chain_verifies_and_detects_tamper` |
| Key loss | Vault refuses to open when records exist without a key (no silent re-key); recovery bundle | `pv-vault` tests |
| Tampered install | `install.sh` builds from the public source over HTTPS (git clone) with a locked dependency set (`--locked`); no prebuilt binaries are downloaded; users can read the script before running it | `skills/promptvpn-setup/tests/run.sh` (no sudo, valid shell) |

## Out of scope

- A compromised OS or an administrator on the same machine: they can read process memory, the keychain and files.
- Traffic that does not go through the proxy, including tools agents execute themselves.
- Side channels in the NEAR infrastructure beyond what attestation covers. The research notes that GPU evidence is bound to the CVM only through the shared nonce, and that streamed NEAR responses carry gateway signatures rather than model signatures.
