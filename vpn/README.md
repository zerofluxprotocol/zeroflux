# ZFX VPN

A private AI endpoint for the models you already use.

ZFX VPN is an application-layer prompt privacy gateway, not a network VPN. Clients send OpenAI-compatible requests (`/v1/chat/completions`, `/v1/responses`, streaming or not) to it instead of to the model provider. It finds sensitive values in every message, system prompt, tool definition, tool-call argument and tool result, then per policy blocks, replaces with placeholders, redacts, holds for review, or routes the whole request to a verified confidential NEAR AI model. Placeholders in the reply are restored only for the caller and conversation that created them.

It serves two kinds of users:

- **Builders protecting their end users.** Put the hosted gateway (or a local daemon) between your app or agent and the model. Your users' emails, phone numbers, health details and IDs never reach the model provider; replies come back with the values restored. Demo: [`demos/builder-app/run.sh`](demos/builder-app/run.sh).
- **Developers protecting their own sessions.** Point a coding agent or chat client at `http://127.0.0.1:8787/v1`. Keys and passwords in files the agent reads are blocked on your machine; your details and project codenames are swapped for placeholders. Demo: [`demos/developer-agent/run.sh`](demos/developer-agent/run.sh).

ZFX VPN was called Prompt VPN until release 0.2. The `promptvpn` command still works and existing installs are migrated in place (see [Upgrading from Prompt VPN](#upgrading-from-prompt-vpn)).

## Three ways to run it

| Mode | Who sees the raw prompt | Use for |
|---|---|---|
| **Local** `zfx-vpn` daemon | Only your machine | Anything sensitive. Strongest control |
| **Hosted** `zfx-vpn-hosted` gateway | **The ZFX hosted service and its operators**, before transformation | Convenience, when the operator is trusted. Not zero-knowledge, not an enclave |
| **Hosted transport + local preprocessing** | Only your machine; the hosted side gets transformed payloads | Teams that want hosted routing, quotas and billing without exposing values |

The hosted gateway states this on every response (`X-ZFX-Trust`) and at `GET /v1/zfx/privacy-policy`. Full analysis: [`../docs/VPN_THREAT_MODEL.md`](../docs/VPN_THREAT_MODEL.md).

## Privacy tiers

Every destination and every routed request carries one of these (`privacy_tier` in `/v1/models`, `X-ZFX-Privacy-Route` on responses, badges in the dashboard):

| Tier | Meaning | May receive untransformed content? |
|---|---|---|
| `LOCAL_ONLY` | Runs on this device (loopback Ollama or a declared local endpoint) | Only if you allow local private handling |
| `TRANSFORMED_PUBLIC` | Public or unverified model | Never: transformed content only |
| `ATTESTED_CONFIDENTIAL` | NEAR AI model whose provider-level TEE attestation verified for that exact model, minutes ago | Only with your explicit consent; never critical categories |
| `UNVERIFIED_EXTERNAL` | Claims confidentiality (a NEAR AI model) but did not verify | Never: treated like a public model |

A verified TEE is still remote and still runs software that can have bugs (see NEAR's 9 Nov 2026 SGLang incident in the threat model). A gateway signature or a model merely listed on NEAR AI does not count as verified.

## Install (local)

```sh
curl -fsSL https://raw.githubusercontent.com/zerofluxprotocol/zeroflux/main/vpn/install.sh | sh
zfx-vpn init            # guided setup (or: zfx-vpn init --dashboard)
zfx-vpn connect test    # proves protection with synthetic data against a local mock
zfx-vpn dashboard
```

The installer builds from source with Rust (it offers to install Rust if missing), puts `zfx-vpn` in `~/.local/bin` with a `promptvpn` link, and never uses sudo. macOS is the primary platform; Linux builds but has not been run (no Linux host was available).

Point a client at it:

```sh
export OPENAI_BASE_URL=http://127.0.0.1:8787/v1
export OPENAI_API_KEY="$(cat "<data dir>/apps/default.key")"   # path shown by `zfx-vpn connect show`
```

or let ZFX VPN edit a supported client's config with a backup: `zfx-vpn client configure codex|aider|opencode`, undo with `zfx-vpn client rollback <client>`. Coding agents can follow [skills/zfx-vpn-setup/SKILL.md](skills/zfx-vpn-setup/SKILL.md).

## Hosted gateway

Built and tested; ZeroFlux does not serve it publicly yet. It is in private beta: request access at [zeroflux.network/#beta](https://zeroflux.network/#beta), or run it yourself:

```sh
ZFX_HOSTED_ADMIN_TOKEN=<32+ chars> zfx-vpn-hosted serve --listen 0.0.0.0:8800 --admin-listen 127.0.0.1:8801 --data /var/lib/zfx-vpn-hosted --models-dir <dir with gliner-pii-base/>
```

The operator API (tenants, scoped API keys, sealed upstream credentials, usage) listens separately and should stay private. Tenant keys look like `zfx_sk_...` and carry scopes (`inference`, `models:read`, `privacy:read`); each key has a rate limit and each tenant a monthly request and token quota. `Idempotency-Key` makes retries safe: a key never causes a second upstream call or charge. Nothing raw is written to disk, placeholder maps live for one request, and there is no review queue (requests local mode would hold fail with `ZFX_APPROVAL_REQUIRED`).

Example request:

```http
POST /v1/chat/completions
Authorization: Bearer zfx_sk_...
Content-Type: application/json

{"model": "frontier/gpt-x", "stream": true,
 "messages": [{"role": "user", "content": "Email Alice at alice@example.test about Project Cedar"}],
 "zfx_privacy": {"mode": "auto", "fallback_model": "deepseek-ai/DeepSeek-V4.1-Flash-1017", "on_unavailable": "block"}}
```

Protected transport from a local daemon:

```sh
zfx-vpn destination add-hosted zfx-hosted https://api.zeroflux.network/v1 frontier/gpt-x --default
```

The daemon transforms, keeps the map, sends with `X-ZFX-Preprocessed: v1`, and restores locally. The hosted side refuses to transform such requests itself.

## The `zfx_privacy` request extension

Optional and strict (unknown fields are rejected); removed before anything is forwarded. It can only make a request stricter than the owner's policy:

| Field | Values |
|---|---|
| `mode` | `auto` (default), `transform_only` (never a confidential fallback), `confidential` (whole request to a verified TEE or this device, never a public model), `local_only` |
| `fallback_model` | NEAR AI model for confidential fallback; must verify; owner consent still required |
| `on_unavailable` | `block` (default) or `review` (local only) |
| `stream_buffering` | `allow` (default) or `reject`: fail with `ZFX_STREAM_SAFETY_BUFFERED` instead of buffering |
| `require_receipt` | release output only after a provider-TEE response signature verifies |
| `conversation_id` | placeholder scope (or header `X-ZFX-Conversation`) |

`GET /v1/zfx/requests/{id}/privacy` returns the route record (tier, action, verification state such as `PENDING_VERIFICATION` or `RECEIPT_VERIFIED`) for the caller's own requests. The id is in `X-ZFX-Request-Id`.

## Error codes

`ZFX_POLICY_BLOCKED`, `ZFX_APPROVAL_REQUIRED`, `ZFX_DETECTION_UNAVAILABLE`, `ZFX_TEE_UNVERIFIED`, `ZFX_FALLBACK_UNAVAILABLE`, `ZFX_UNSUPPORTED_CONTENT`, `ZFX_STREAM_SAFETY_BUFFERED`, `ZFX_UPSTREAM_FAILURE`, `ZFX_ROUTE_FORBIDDEN`, `ZFX_OUTPUT_BLOCKED`, `ZFX_VAULT_LOCKED`, `ZFX_POLICY_INVALID`, `ZFX_UNAUTHORIZED`, `ZFX_FORBIDDEN_SCOPE`, `ZFX_RATE_LIMITED`, `ZFX_QUOTA_EXCEEDED`, `ZFX_IDEMPOTENCY_CONFLICT`, `ZFX_BAD_REQUEST`, `ZFX_PAYLOAD_TOO_LARGE`, `ZFX_NOT_FOUND`. Error bodies never contain matched values.

Compatibility: every error also carries `legacy_code` with the Prompt VPN 0.1 name (`PV_*`) when one existed; requests may send `X-ZFX-Error-Codes: pv1` to get the 0.1 `code` and `type` back; `PV_*` names are accepted wherever codes are parsed.

## What it covers, and what it does not

It inspects the requests your client sends to its API and the model's output on the way back. It does **not** intercept anything an agent does outside that API (running `curl`, reading a file and posting it, driving a browser). It installs no root certificates and changes no system proxy.

- Detection is not perfect. [docs/evaluation.md](docs/evaluation.md) reports measured misses.
- Placeholders restore only when the model copies them exactly; a tokenized value can hurt a model's reasoning about it. Use `PRIVATE_ROUTE` or `BLOCK` for such categories.
- Images, audio and files inside requests cannot be inspected and are blocked for remote destinations by default.
- Local logs and the vault are MAC-chained and encrypted; they do not defend against an administrator of the same machine.

## Upgrading from Prompt VPN

Stop the old daemon (`promptvpn stop`), re-run the installer, then run any `zfx-vpn` command. It migrates the install once: database backup, keychain entries copied (old ones kept), database renamed and upgraded, nothing re-encrypted. `zfx-vpn migrate status` shows the state; `zfx-vpn migrate rollback` hands it back to Prompt VPN 0.1. Details: [docs/operations.md](docs/operations.md#from-prompt-vpn-01).

Renamed: binary `promptvpn` to `zfx-vpn` (old name still works), data directory, keychain service `promptvpn` to `zfx-vpn`, env vars `PV_HOME`/`PV_KEY_BACKEND` to `ZFX_VPN_HOME`/`ZFX_KEY_BACKEND` (old names honored), headers `X-PromptVPN-*` to `X-ZFX-*` (old names accepted), error codes `PV_*` to `ZFX_*`, placeholders `[[PV_...]]` to `[[ZFX_...]]` (old ones still restore), skill `promptvpn-setup` to `zfx-vpn-setup` (pointer left at the old path). Kept on purpose: crate names (`pv-*`), the vault's HKDF salt and the recovery-bundle format id, since changing them would break existing encrypted data.

## Repository

```
crates/pv-core        typed contracts, policy engine, privacy tiers, zfx_privacy, error codes
crates/pv-detectors   recognizers, exact matcher, encoded views, GLiNER span adapter, custom entities
crates/pv-transform   tokenize/redact/generalize, scoped restoration, TokenStore (vault or per-request memory)
crates/pv-vault       XChaCha20-Poly1305 vault, key sets, rotation, recovery
crates/pv-audit       SQLite store, migrations (with rollback), MAC-chained events, route records
crates/pv-provider    destinations, NEAR AI (official SDK) + provider-level TEE decision (tee.rs)
crates/pv-proxy       request pipeline, routing, approvals, output inspection, SSE
crates/pv-daemon      loopback server, auth, admin API, migration from Prompt VPN, mock upstream
crates/pv-cli         the `zfx-vpn` binary (also answers to `promptvpn`)
hosted/               `zfx-vpn-hosted`: multi-tenant gateway, keys, quotas, idempotency
apps/dashboard        React dashboard, embedded in the binary
skills/zfx-vpn-setup  agent setup skill, scripts, tests, adapters (old path: skills/promptvpn-setup)
demos/                builder and developer demos (run_all.sh)
sdk/                  client helper for the zfx_privacy extension and route metadata
tests/                fixtures (Prompt VPN 0.1 install), upgrade/rollback script
schemas/              JSON Schemas generated from the Rust types
third_party/gline-rs  vendored GLiNER runtime with two fixes for ModernBERT models
models.lock.json      pinned model revisions and hashes (no weights committed)
```

Docs: [threat model (product)](../docs/VPN_THREAT_MODEL.md), [daemon threat model](docs/threat-model.md), [policy semantics](docs/policy-semantics.md), [operations](docs/operations.md), [evaluation](docs/evaluation.md), [deviations](docs/deviations.md), [progress](docs/PROGRESS.md), [hosting resources](docs/hosting-resources.md).

## Development

```sh
sh scripts/check.sh                                     # everything below plus schemas, dashboard, skill adapters, privacy gate
cargo test --workspace                                  # unit, HTTP integration, migration, TEE gates, streaming red-team, hosted
sh tests/migration/upgrade_rollback.sh                  # builds Prompt VPN 0.1 from git, upgrades, rolls back, runs 0.1 again
sh demos/run_all.sh                                     # both audience demos, with assertions
sh skills/zfx-vpn-setup/tests/run.sh                    # skill script tests
cargo test -p pv-provider --test live_near -- --ignored # live NEAR attestation (network, no key needed)
cargo run --release -p zfx-vpn-hosted --example measure -- <models dir> gliner-pii-base   # hosting resource numbers
```

The demo videos in `docs/demo/` and the Remotion project in `demo/` predate the rename and still show "Prompt VPN".

License: Apache-2.0. The GLiNER PII models are Apache-2.0.
