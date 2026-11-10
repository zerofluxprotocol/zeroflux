# Prompt VPN — Engineering Specification v1.4

**Status:** Implementation baseline  
**Date:** 2026-11-08  
**Product:** Prompt VPN (working name)  
**Audience:** Autonomous coding agents and human implementers  
**Priority:** Local-first privacy, fail-closed enforcement, compatibility, inspectability
**Release 1 platforms:** macOS (Apple Silicon and Intel) and Linux (x86_64 and ARM64). Windows is deferred.

**First-run product requirement:** A quick, explicit onboarding wizard must precede switching a client's API base URL. Explain exactly what is blocked, what is transformed, what can be sent to a verified NEAR TEE, and what stays on-device. Give the user control over built-in protection presets, custom exact secrets, semantic concepts, fallback behavior, classifier model, NEAR model, and notifications; all choices remain editable under Settings later.

**Inference destination precedence:** NEAR AI Gateway is the **strongly preferred/default destination for user-facing model inference**. The user may also configure a local Ollama *generation* model or a custom OpenAI-compatible endpoint as alternative/fallback destinations. The Qwen3Guard and LFM3 *classifier* installations are distinct from any Ollama *generation* model; do not silently use a guard classifier as the assistant model. Fallback never relaxes policy, privacy tier, or per-destination consent.

## 0. Objective and non-goals

Build an installable, cross-platform application that mediates explicitly configured outbound LLM requests locally, detects sensitive content, applies user/organization/agent policies, safely transforms permitted content, blocks prohibited content, restores approved placeholders in responses, and exposes local CLI and dashboard controls. All detection, rules, registered secrets, and reversible maps stay on-device. **NEAR AI Gateway integration is a mandatory product capability** for approved inference and TEE-private fallback; local detection and policy evaluation do not depend on its availability.

Deliver a single installable package consisting of a background daemon, CLI, local dashboard, and an **agent-installation skill** that helps coding agents safely install, configure, test, and troubleshoot Prompt VPN for users. Its universal integration is an **OpenAI-compatible local HTTP API**, not plugins or forks of coding agents.

### Non-goals for v1

- Do not build a new AI coding harness, fork existing agent products, or require users to switch clients.
- Do not silently capture arbitrary TLS traffic, insert a root certificate, modify system proxies globally, or install privileged network filters.
- Do not claim that configuring an LLM base URL covers arbitrary shell, browser, email, or tool egress.
- Do not send raw prompts, registered secrets, mappings, or audit logs to Prompt VPN servers.
- Do not run sensitive plaintext through public-chain contracts.
- Do not promise perfect PII recognition, legal compliance, or absolute confidentiality.
- Do not send raw input to NEAR merely to classify it. NEAR inference is on the request path only when the effective routing policy chooses NEAR AI.

## 1. Architectural decisions

| Layer | Default implementation | Reason |
|---|---|---|
| Local daemon | Rust, Tokio, Axum, Rustls | Async streaming proxy and security boundary |
| CLI | Rust, Clap; shares library crates with daemon | Single distributable tool |
| Dashboard | React + TypeScript + Vite, compiled into static assets served by daemon | No second production server |
| Local database | SQLite with migrations | Rules, users, policy versions, audit metadata |
| Key handling | OS keychain/credential store; per-install data encryption key | Avoid plaintext encryption keys in SQLite |
| Secret data | AEAD-encrypted local vault with per-record nonces and key IDs | Custom exact values and reversible mappings |
| Deterministic recognizers | Regex, checksums, keyword context, pluggable Presidio-compatible recognizers | Accurate, auditable common patterns |
| Span recognition | GLiNER, initially GLiNER PII Edge (pin exact revision after validation) | Local entity recognition; embedded ONNX-compatible runtime, **not Ollama** |
| Generative guard model | Qwen3Guard-Gen-0.6B | Ollama-hosted classification candidate; not interchangeable with ordinary Qwen3 0.6B |
| Semantic classification | Liquid AI LFM3-1.7B | Ollama-hosted contextual classification candidate |
| Local model management | Ollama for Qwen3Guard and LFM3; embedded local GLiNER runtime | Automatic model pulls; only one selected generative classifier active at a time in v1 |
| Required inference provider | NEAR AI Gateway, OpenAI-compatible API | Approved upstream plus independently verified TEE-hosted fallback routes |
| Other generation destinations | Configured Ollama model on loopback; user-specified OpenAI-compatible endpoint | Explicitly selectable backup/alternative, subject to destination-specific policy |

Model selections are candidates, not validated security guarantees. Pin exact revisions, digests, licenses, runtime formats, and conversion tooling in `models.lock.json`; benchmark before making one a default. **Do not claim GLiNER is served through Ollama:** it requires its own local classifier adapter. The implementation must support switching the selected generative model without changing policy semantics. If Ollama is absent, display an OS-specific install prompt with official source and await consent; do not silently install system software. Once available, automatically download the selected model with verified identity and progress display. For Qwen3Guard, verify the selected Ollama model actually contains the Guard weights (a community package may be required); don't pull ordinary `qwen3:0.6b` as a substitute.

### Process topology

```text
AI client ──HTTP(S)──> localhost proxy ──> classifier pipeline ──> policy engine
                                     │                            │
CLI ──authenticated local IPC────────┤                            ├──allow/transform/route──> provider
Dashboard ──authenticated localhost──┤                            ├──hold──> approval queue
                                     └── SQLite + encrypted vault └──block──> structured error
```

Bind the proxy and admin API to `127.0.0.1` (and optionally `::1`) only. Require an install-generated authentication secret for administrative mutations; protect browser admin endpoints against CSRF and rebinding attacks using origin/host validation, token/session authentication, and restrictive CORS. Never expose the admin UI on `0.0.0.0` by default.

## 2. Five core components

### 2.1 Detection engine: three classifier types

**Type 1 — Built-in concepts:** EMAIL, PHONE, PERSON, ADDRESS, SSN/GOVERNMENT_ID, PASSPORT, DRIVER_LICENSE, DOB, FINANCIAL_ACCOUNT, PAYMENT_CARD, HEALTH_INFORMATION, AUTH_SECRET, API_KEY, PRIVATE_KEY, PUBLIC_INFORMATION. Country-specific subtypes and validators are extensible. No generative model is necessary for high-confidence structured patterns. Include context cues and checksum validation where applicable, but do not assume fake or invalid-checksum IDs are harmless.

**Type 2 — Exact sensitive values:** users register particular strings, values, identifiers, phrases, file fingerprints, or patterns. Values are entered only through local UI/CLI, encrypted at rest, and never placed in raw logs. Match exact values and policy-selected normalizations (casefold, whitespace, separators, Unicode normalization). Avoid unsafe broad normalization by default. Use keyed fingerprints for indexing where appropriate, but matching arbitrary substrings must run over authorized plaintext within the local trusted process; bare unsalted hashes of SSNs are unacceptable. Store an opaque `secret_id`, not the value, in events.

**Type 3 — Semantic concepts:** users define protected topics in natural language (e.g. unreleased accelerator specifications, confidential negotiations, proprietary algorithm details), with optional safe examples. The small local classifier analyzes intent and context, returning structured categories, confidence, evidence spans if meaningful, and a suggested transformation feasibility. Semantic labels are advisory findings: the deterministic policy engine alone decides disposition. If a concept cannot be safely isolated, block or route to a permitted local/confidential model, rather than blindly replacing a few words.

The pipeline merges overlapping findings using deterministic rules; a secret finding must not be dropped merely because a less sensitive PERSON finding overlaps it. Preserve source provenance and classifier version per finding.

**Finding IR:**

```json
{
  "finding_id": "uuid",
  "classifier_type": "builtin|exact|semantic",
  "category": "GOVERNMENT_ID",
  "start_byte": 12,
  "end_byte": 23,
  "confidence": 0.99,
  "recognizer_id": "ssn-us-v1",
  "evidence": "pattern+context",
  "sensitivity": "critical"
}
```

Offsets are UTF-8 byte offsets into the exact local input buffer. Validate bounds, UTF-8 boundaries, overlaps, and any model output before using them. Never include raw finding text in telemetry or logs.

### 2.2 Deterministic policy engine

Policy decision is independent of LLM-generated prose. **Release 1 is single-user, local-only**: the device owner manages policies, vault, and approvals. Model policy scopes as user > agent > application/request; reserve organization/team hierarchy in schema for later, but do not implement multi-user accounts, central reporting, role delegation, or enterprise policy distribution in v1. A lower level must not weaken an inherited non-overridable rule. Store immutable, versioned policies and an effective-policy hash per decision.

Actions: `ALLOW`, `TOKENIZE`, `REDACT`, `GENERALIZE`, `PRIVATE_ROUTE`, `REQUIRE_APPROVAL`, `BLOCK`. An explicit `FAIL_CLOSED` error occurs when required recognition, model inference, storage, policy loading, or provider approval cannot complete. **Default built-in policy blocks critical PII and credentials**, including SSNs/government IDs, passports, payment credentials, API keys, private keys, and authentication secrets, before any remote destination is considered. Uncertain noncritical findings and local generative-classifier failure default to `PRIVATE_ROUTE` to a previously configured independently verified NEAR TEE-hosted model **only after the user grants informed onboarding consent**. Onboarding and Settings must additionally offer **`BLOCK_AND_REVIEW`** as an alternative: stop the request locally, notify, queue it for owner action, and never route until an authorized new submission passes policy. If no eligible verified TEE route exists, `BLOCK` with a user-visible notification/event rather than forwarding publicly. This default never overrides known explicit `BLOCK`, `FORBIDDEN`, `never leave device`, or destination restrictions. It also does **not** apply if the deterministic policy engine/vault itself is unavailable, since authorization cannot then be established: block locally. A model/detection failure must never silently become `ALLOW`.

```json
{
  "id": "no-ssn-egress",
  "version": 1,
  "scope": {"kind":"user","id":"local-owner"},
  "selector": {"categories":["GOVERNMENT_ID"],"destinations":["external"]},
  "action": "BLOCK",
  "severity": "critical",
  "notify": ["local_user","security_admin"],
  "override": "FORBIDDEN",
  "retention_days": 30
}
```

Override modes: `FORBIDDEN`, `APPROVAL_ONCE`, `POLICY_OWNER_ONLY`. Approvals bind to request hash, destination, policy version, actor, expiry, and exact transformed payload. Editing the request invalidates the approval and triggers a new classification. No approval permits bypassing a higher-level forbidden policy. User-facing controls: approve/whitelist once, deny/blacklist, edit and retry, cancel, and (only for authorized policy owners) **save a durable new exact-value, category, semantic rule, or local classifier guidance note** for future matching. Persist such edits as versioned policy data, not by unvalidated self-modification of model weights or system prompts. Every override and rule edit is audited.

### 2.3 Transformation engine

- `TOKENIZE`: opaque placeholders such as `[[PV_PERSON_a72f]]`; encrypted value mapping scoped by conversation and policy, with expiry; consistent pseudonyms only within authorized scope. Restore only exact known tokens in authorized assistant-facing text. The model must never receive mapping metadata.
- `REDACT`: irreversible replacement such as `[REDACTED_GOVERNMENT_ID]`; no value-restoration record.
- `GENERALIZE`: local transformation reducing specificity, followed by fresh classification and policy evaluation; fail closed if generated text still violates policy. Do not automatically generalize numbers/dates critical to task correctness.
- `PRIVATE_ROUTE`: original or partially transformed input goes exclusively to a destination explicitly permitted by the effective policy; distinguish local model from remote attested NEAR AI. A TEE is not equivalent to on-device execution.
- `BLOCK`: nothing is sent to the prohibited provider; produce structured error and event.

Preserve conversation semantics with stable mapping scope, but prohibit cross-user/cross-tenant restoration. Apply guards to outputs, tool arguments, and logs when those paths are integrated. Avoid storing raw prompts by default.

### 2.4 Confidential execution and secrets

Default classifications, policies, and mappings execute on device. Use OS-backed key protection where available, AEAD (e.g. XChaCha20-Poly1305 or AES-256-GCM) for vault records, separate key IDs, and a documented recovery/rotation procedure. Zeroize transient secret buffers on a best-effort basis. Never claim memory cannot be read by a compromised local OS. Authenticate all local admin actions.

**NEAR AI Gateway integration is required.** Configure its API base, credentials, model discovery, and OpenAI-compatible chat route. Distinguish (a) NEAR-operated models with verifiable confidential execution from (b) externally hosted/third-party models merely proxied through the gateway. A model is not eligible for `PRIVATE_ROUTE` unless the implementation verifies the relevant published attestation and binds that verification to the exact selected model/service and request; TLS and an API hostname alone are insufficient. If an available NEAR endpoint lacks attestable confidential execution, offer it only as a normal external destination, subject to the usual redaction policy. Protect against model-list changes and unavailable/failed verification; **never downgrade to a third-party or public endpoint**. Product planning assumes NEAR AI service availability, but runtime errors must still fail safely without data leakage. No prompt, private rule, or exact secret goes onchain. NEAR wallet payments are out of v1 scope.

**Initial preferred NEAR model:** `deepseek-ai/DeepSeek-V4-Flash`, which NEAR documents as GPU TEE-hosted, with model-specific attestation endpoint `GET /v1/attestation/report?model=deepseek-ai/DeepSeek-V4-Flash` and completion-ID signature endpoint `GET /v1/signature/{completion_id}?model=deepseek-ai/DeepSeek-V4-Flash&signing_algo=ecdsa`. This is a **preferred default subject to live discovery, attestation verification, and user selection**, never a hardcoded privacy claim. The UI must list **only verified TEE-eligible models** when choosing a private fallback. Distinguish this from similarly named `deepseek/deepseek-v4.1-flash`, which NEAR labels *Incognito* rather than TEE-hosted. Official source: https://near.ai/models/deepseek-ai/DeepSeek-V4-Flash and https://near.ai/models . If the preferred model is no longer live/eligible, require an explicit alternative eligible selection; do not silently choose a proxy model.

**Generation routing and failover contract:** Route normal allowed model requests to the user's chosen NEAR model by default. If NEAR is not configured/unavailable, prefer an **explicitly configured local Ollama generation endpoint**; permit a configured external custom endpoint **only when the effective policy has authorized that exact destination and the post-transformation payload is safe for it**. Mark every destination `LOCAL`, `ATTESTED_TEE`, or `EXTERNAL_UNVERIFIED` and enforce distinct egress permissions. For uncertain raw prompts that require TEE confidentiality, a local Ollama generation model is eligible only if local-only handling is explicitly configured; an unverified custom endpoint is **never** an automatic replacement for a verified TEE. If the only available alternate violates policy, block/queue with an actionable error. Prevent cross-provider credential reuse and store custom endpoint tokens in the OS credential store. Test no-downgrade on provider failures, retries, and streams.

### 2.5 Local audit, dashboard, and admin management

Record every decision and significant classifier/policy failure, **not** raw secret values. Minimum event schema: event ID, timestamp, app/agent/user ID, destination ID, classifier categories, action, policy ID/hash, model/recognizer versions, processing latency, finding counts, approval status, error code, and sanitized context pointer if separately enabled. Hash-chain or MAC-protect events for tamper evidence; do not describe local logs as tamper-proof against an OS administrator.

Dashboard: Overview; Events with filters and severity; Event detail; Policies with testing/playground; Built-in category toggles; Exact Secret Vault; Semantic Concepts; Agents/Applications; Destinations/Models; **Approval Queue**; Settings; Health and Diagnostics. **Automatic action + desktop notification** is the default for flagged requests; do not pause all flagged traffic for human approval. Notify for block, redaction, private fallback, and review-worthy actions, with rate limiting and configurable categories; never reveal plaintext secrets in OS notification banners. Only the **authenticated local device owner** may reveal the actual sensitive prompt when deliberately retained as an encrypted pending-approval payload; admins, remote accounts and other applications cannot read it. Initial preview is redacted. The queue supports approve once, deny, edit-and-retry, whitelist/blacklist, and save as a future rule or guidance note. Record **metadata-only audit events for 30 days by default**, configurable; enforce expiry, export and deletion tests. No enterprise central reporting in v1.

**Onboarding wizard — required, brief, and revisit-able:** (1) explain local processing, the OpenAI-compatible base URL, and limits of tool interception; (2) show a ready-made **critical PII block preset enabled by default** with clear category examples; (3) allow per-category choices for noncritical built-in data (email, name, phone, address, etc.) such as block/tokenize/redact/allow; offer add-an-exact-value and add-a-semantic-concept forms without requiring completion; (4) show uncertain/failure handling choice between **verified NEAR TEE auto-route with informed consent** and **local block-and-review**; explain that TEE routing still sends data off-device; (5) choose Qwen3Guard or LFM3, install/auto-pull, initialize GLiNER, and **prominently offer NEAR AI as the preferred inference provider** with DeepSeek V4 Flash selected if eligible; allow **Skip for now** to finish local-only setup, choose an Ollama generation model, or add a custom OpenAI-compatible endpoint with explicit disclosure of its privacy tier; (6) enable notifications, preview the event dashboard, and show the local API base URL/agent setup skill. Save a versioned policy and consent record. Re-run configuration from Settings without losing unrelated data. Never pre-check consent on behalf of the user.

**Default approval retention:** Encrypt pending request bodies locally and destroy them after **15 minutes** without approval, cancellation, or completion (configurable), independently of 30-day metadata retention. Crash/restart cleanup must enforce expiration. Events must not preserve sensitive prompt copies.

**Owner-controlled critical-category exceptions:** All critical protection switches begin enabled. The authenticated local owner **may disable an individual critical category only after an explicit warning and confirmation**, creating an auditable, versioned policy exception. Do not have a single global "disable all security" button or allow an agent/client to modify these settings. Onboarding presents these switches with the default protection clearly visible; disabling must be deliberate. Re-enabling is available instantly in Settings.

## 3. Request and response protocol

**Release 1 must support both** OpenAI-compatible `/v1/chat/completions` **and** `/v1/responses`, including non-streaming, streaming SSE, tool-call request/response structures and structured errors. Each protocol requires dedicated conformance fixtures and error/stream coverage; do not advertise support until its tests pass. Keep headers and credentials local, forward only allowlisted headers, and never log Authorization. Providers and routes are configured per-app or client.

Internal flow:

1. Authenticate local caller; identify application/agent, requested destination, and conversation. Expose a standard OpenAI-compatible local base URL, with client-supplied OpenAI API key accepted as a **local caller credential**, never confused with NEAR credentials.
2. Parse request and identify all applicable content-bearing fields, including LLM-visible `tool_calls` arguments, tool-result messages, tool definitions and multimodal metadata where supported. Unsupported content types fail closed under protective policies. **Only inspect content passing through the OpenAI-compatible API; do not hook into or execute agent tools.**
3. Classify deterministically; invoke span and semantic models when required by policy.
4. Merge findings; resolve immutable effective policy; produce decision with explanations.
5. For automatic actions (including default uncertain/failure private routing), apply policy without a human pause and send an informational desktop notification. **If a policy requests `REQUIRE_APPROVAL` or fallback mode is `BLOCK_AND_REVIEW`, immediately return `PV_APPROVAL_REQUIRED` with a nonsecret approval ID**, queue a pending encrypted request with 15-minute TTL and dispatch a desktop notification. **Do not hold the original HTTP connection open.** After owner approval/edit, require the client to retry or explicitly resubmit through the authenticated dashboard; the proxy must not invisibly replay the original request. Recheck policy/version and destination on resubmission. Only the authenticated local device owner can choose to reveal the full encrypted pending prompt in the dashboard; initial preview is redacted. On editing, re-run all classifiers and policies. Do not silently forward.
6. Transform; reclassify generalized text; check destination permissions; send only approved payload.
7. **Inspect model-generated outputs as well as inputs**, including newly generated sensitive values and tool-call arguments, using the same policy engine with separate inbound/outbound selectors. Restore approved placeholders only to an authorized caller. Stream through a bounded safety buffer; never release uninspected output chunks when the active policy requires full-span or semantic evaluation. Where safe incremental inspection cannot be proven, buffer the entire bounded response (with size cap) or return a policy-safe error instead of leaking partial output. Reclassify transformed output before release.
8. Emit sanitized event; release/expire mappings according to policy. Agent **tool-call arguments/results** are inspected when present inside supported LLM protocol messages; **execution of separate HTTP/shell/browser tool calls is not intercepted** by this OpenAI-compatible proxy. Document this coverage boundary prominently.

Error shape (with HTTP statuses appropriate to transport):

```json
{
  "error": {
    "type": "promptvpn_policy_block",
    "code": "PV_POLICY_BLOCKED",
    "message": "Request blocked by local policy",
    "event_id": "uuid",
    "approval_id": null,
    "retryable": false
  }
}
```

Other codes: `PV_APPROVAL_REQUIRED`, `PV_CLASSIFIER_UNAVAILABLE`, `PV_UNSUPPORTED_CONTENT`, `PV_ROUTE_FORBIDDEN`, `PV_POLICY_INVALID`, `PV_VAULT_LOCKED`, `PV_PROVIDER_ERROR`. Never include sensitive matched spans in error text.

## 4. CLI and installed UX

Single binary command `promptvpn`, with daemon mode. Initial commands:

```text
promptvpn init                 # interactive local setup
promptvpn start|stop|status
promptvpn dashboard            # open localhost UI
promptvpn doctor               # diagnostics without secrets
promptvpn model list|select|install|test
promptvpn ollama status|setup   # prompt for installation; do not silently install
promptvpn near login|models|test|verify-attestation
promptvpn policy list|validate|test|import|export
promptvpn secret add|list|remove
promptvpn concept add|list|remove
promptvpn events list|export
promptvpn approvals list|approve|deny|edit|save-rule
promptvpn connect test          # validate standard OpenAI API compatibility
promptvpn uninstall            # explicit confirmation for data removal
```

First-run `promptvpn init` launches the onboarding wizard described in §2.5, either in the local dashboard or an equivalent guided CLI flow. Detect Ollama; if absent prompt installation with official instructions, then resume after Ollama is detected. Select **Qwen3Guard-Gen-0.6B** or **LFM3-1.7B** (one active generative classifier at a time); automatically pull the selected model, show download/integrity status, and install **GLiNER PII Edge** by default. Allow switching later. **All three model configurations require automated regression evaluation in every release**, irrespective of installed user choice. **Promote NEAR setup in the wizard**: collect Gateway credentials, discover eligible models, prefer verified `deepseek-ai/DeepSeek-V4-Flash`, and test confidential invocation/attestation. But the user **may skip NEAR configuration and finish local-only onboarding**; mark `NEAR_NOT_CONFIGURED` in Health, disable only NEAR-dependent inference/private-routing features, and let local enforcement/approved local Ollama generation work. Never silently alter existing clients; provide standard OpenAI `base_url` and local `api_key` examples for clients, with a first-party agent skill that can make approved reversible config edits. Installation must not change provider credentials, system-wide proxy, or trust store without explicit user consent. **Release 1 requires macOS Apple Silicon/Intel and Linux x86_64/ARM64** install, daemon, CLI, notifications, model runtime and integration tests. Windows is deferred; do not advertise it as supported.

The built dashboard is embedded in daemon assets and available at `http://127.0.0.1:8787/dashboard`. Proxy routes at `/v1/*`; management routes at `/api/admin/v1/*`; health endpoint exposes no secrets. Select a port conflict strategy that never silently binds externally. Development frontend may run separately, but production installation needs no Node runtime.

## 5. Agent-installation skill (required deliverable)

Ship a first-party **`promptvpn-setup` agent skill** in `skills/promptvpn-setup/SKILL.md`, along with deterministic scripts and tests. This is a distributable instruction package for supported coding assistants, not a requirement to fork their tools. Where assistants use different skill manifests, generate adapters from one canonical instruction source.

The skill must enable an agent to:

1. Discover OS, CPU architecture, Ollama availability, available RAM, and compatible OpenAI-base-URL clients **without extracting secrets**.
2. Explain the install plan and request user approval before altering client configurations or installing background services.
3. Install or locate signed/checksummed Prompt VPN binaries; verify provenance and integrity.
4. If Ollama is absent, **ask the user to install it**, show official platform-specific instructions, and verify installation; do not install without informed consent. Run `promptvpn init`, set local administrative authentication, select policy presets, **automatically pull the user-selected Qwen3Guard or LFM3 Ollama model**, provision the local GLiNER runtime, and show progress/errors. Verify model identity rather than assuming similarly named models are equivalent.
5. Strongly recommend NEAR AI Gateway credentials and an eligible attested private model during setup, but support an explicit **Skip for now** local-only path and clearly mark private-TEE fallback unavailable until configured. Explain that externally proxied models are not equivalent to NEAR-hosted TEE-private models, and verify a working confidential invocation when NEAR is set up. Also allow owner-configured Ollama generation or custom OpenAI-compatible destinations without assuming they inherit TEE confidentiality. For an existing coding agent or chat client, explain how to point its standard OpenAI-compatible `base_url` and credential at local Prompt VPN. **Only with user approval** edit that client's configuration; back it up and provide rollback. Do not build bespoke integration plugins or claim unsupported clients work.
6. Verify with a synthetic email/SSN/API-key test that protected data is transformed or blocked **before external egress**, using a local mock provider; never use real user secrets as test fixtures.
7. Launch local dashboard; explain policy edits, secret registration, semantic concepts, audit review, desktop notifications, approval queue, save-as-rule behavior, and emergency stop.
8. Provide `doctor`, rollback, and uninstall instructions; preserve existing client configuration and user data unless explicitly asked to remove them.
9. Never send secrets into model context, shell history, telemetry, logs, screenshots, or remote support prompts.

Skill must have clear stop conditions for missing permissions, unsupported clients, failed checksum, failed protection test, model-download failure, or disabled secure storage. All scripts should be idempotent, bounded, non-destructive, and tested on supported platforms. Include `install.sh`/PowerShell equivalents only if installation flows require them, and never use `curl | sh` blindly.

### Skill acceptance demonstration

A clean machine or VM with a supported OpenAI-compatible client can follow the skill to install Prompt VPN, prompt for Ollama installation when absent, download the selected classifier, configure the client's standard API base (with explicit approval), test a synthetic secret against a local mock endpoint, exercise a NEAR confidential model/attestation test, observe block/tokenize/review in the dashboard, and rollback client configuration. The test fails if plaintext synthetic secrets reach the mock upstream when policy forbids them.

## 6. Repository structure and interface contracts

```text
promptvpn/
  Cargo.toml
  crates/
    pv-core/          # typed IRs, findings, policy evaluation
    pv-detectors/     # deterministic, exact, span and semantic adapters
    pv-transform/     # tokenize, redact, generalize, restoration
    pv-vault/         # encryption, secrets, mapping lifecycle
    pv-audit/         # SQLite events and retention
    pv-proxy/         # OpenAI-compatible protocol and streaming
    pv-daemon/        # Axum server, local auth, health
    pv-cli/           # Clap commands
    pv-provider/      # OpenAI-compatible proxy + required NEAR AI Gateway
  apps/dashboard/     # React/Vite/TypeScript static SPA
  models/             # manifests, checksums, adapters; no large weights committed
  skills/promptvpn-setup/SKILL.md
  scripts/
  tests/
    fixtures/         # synthetic only
    integration/
    adversarial/
    e2e/
  docs/
  models.lock.json
  README.md
```

Formalize serialized schemas and version fields for Policy, Finding, Decision, TransformMap, AuditEvent, PendingApproval, ProviderConfig, and ClientIntegration. Rust types are source-of-truth; generate TypeScript types/OpenAPI from checked-in schemas. Version changes must have migration tests.

## 7. Model and evaluation protocol

Benchmark these configurations, subject to compatible licensing and local packaging:

| Candidate | Purpose |
|---|---|
| Regex/checksum + Presidio-compatible rules | deterministic baseline |
| GLiNER PII Edge / pinned GLiNER release | local span-level PII extraction through embedded runtime; separate from Ollama |
| Qwen3Guard-Gen-0.6B | selected Ollama generative guard/classification candidate; verify intended label coverage |
| LFM3-1.7B | alternate selected Ollama semantic classification candidate; structured output validation |

Obtain model weights from approved source with pinned revisions and hashes; review redistribution/commercial licenses before bundling. Keep models local. Never use a hosted model to classify raw sensitive content unless the effective policy explicitly allows that route.

Build an evaluation corpus of synthetic but realistic prompts and agent/tool payloads with span-level labels, category labels, allowed transformations, and expected decisions. Include: overlapping entities, SSN-like strings with invalid checksums, Unicode confusables, API keys, emails in code, JSON/string escaping, large context, markdown, malicious instructions, base64 and encoded data, semantic-only business disclosures, repeated aliases, partial streaming placeholders, logs/tool arguments, and benign hard negatives (dates, random identifiers, public company names). Test both privacy leakage and utility degradation. Do not assert a particular model's accuracy before benchmarking.

Required metrics: per-category precision/recall/FNR, critical-secret escape rate, false-block rate, transformed-task utility, local CPU/Apple Silicon latency p50/p95, peak RAM, tokens/sec for semantic model, and end-to-end overhead. Treat zero observed escapes as a finite-sample result, not a proof of zero risk.

## 8. Security and operational requirements

- Fail closed if active mandatory classifier/policy cannot execute; never silently bypass.
- Model prompts and classifier outputs are untrusted data and cannot change configured policy.
- Use size limits, timeouts, cancellation, memory bounds, and content-type validation for adversarial requests.
- Reject malformed placeholder tokens; use cryptographically random identifiers and scoped authorization.
- Do not log secrets, bearer tokens, original prompts, provider API keys, or reversible mapping content.
- Block local admin access from remote origins, prevent DNS rebinding, use least-privilege file permissions.
- Treat provider responses and tool outputs as untrusted; do not let them induce policy overrides.
- Clearly communicate the proxy's coverage: explicitly configured LLM routes and **tool-call payloads embedded within them**, not independently executed OS/network tool calls. Future outbound-agent-tool interception requires additional hooks/egress controls.
- Local classification, vault, dashboard, and policy enforcement must function without remote dependencies, including when NEAR is unreachable. NEAR AI Gateway is nevertheless a **mandatory configured and verified feature** of a complete release; never silently bypass a required remote route on outage.
- Automatic updates are opt-in with signed artifacts, release notes, and rollback; policy updates never weaken defaults without owner authorization.
- For sensitive policies, stream handling must not leak before an inspection decision is available.
- Encryption of audit and vault data must have documented backup, passphrase/key recovery, and retention semantics.

## 9. Test and release gates

**Unit tests:** recognizer fixtures; offsets/overlap; policy precedence; override scope; mapping isolation; encrypted vault; audit sanitization; error serialization.

**Integration tests:** separate Chat Completions and Responses API compatibility suites; local mock upstream verifies exact bytes; SSE chunk boundaries; output PII blocking and tool-call output coverage; concurrent conversations; retries; timeouts; provider errors; unsupported payloads; model unavailable; SQLite corruption/locked DB; service restart; policy changes during pending requests; TEE consent opt-in/out; critical PII never reaches TEE under default block policy; NEAR preferred route, local Ollama generation fallback, approved custom provider fallback, rejection of unverified remote fallback for TEE-required payloads, and no credential crossover.

**Adversarial tests:** forged approval IDs; cross-tenant restoration; prompt-injection to disable policy; secrets in tool arguments/system prompts; malformed SSE; header injection; Unicode/binary payloads; localhost cross-site attacks; DNS rebinding; client config rollback.

**UI E2E:** complete onboarding on a clean device, confirm critical PII default blocks, disable one critical category with owner warning/confirmation and verify audit, choose noncritical built-in actions, register exact secret and semantic concept, choose TEE-consent vs block-and-review, finish with NEAR `Skip for now` and resume later, configure local Ollama and custom endpoint alternatives, verify return-to-Settings changes, create/edit/delete policy, inspect blocked event without raw value, approve/deny with immediate error then explicit retry, verify effective policy precedence, health and diagnostics. Verify only the device owner can reveal queued plaintext and that expired payloads are gone after 15 minutes.

**Packaging:** clean installation, Ollama absence/install prompt, model auto-pull/integrity check, model switching, GLiNER provisioning, daemon autostart choice, CLI functionality, dashboard/desktop notification approval flow, local-only policy execution, mandatory working NEAR AI route/TEE verification, rollback, uninstall preserving data by default, and agent skill on clean test environments.

Release v1 only when critical policy tests pass, no known raw-secret exfiltration in supported routes, all supported adapters meet compatibility tests, and the README accurately documents limitations. Security features cannot be declared working solely from UI mockups.

## 10. Implementation phases and acceptance criteria

### Phase 0 — Contracts and threat model

Write typed schemas, trust-boundary diagrams, policy semantics, detector contracts, threat model, and fixture corpus. **Gate:** architecture tests for policy precedence, audit privacy, and block behavior are agreed and executable.

### Phase 1 — Local daemon, CLI, proxy, vault, dashboard shell

Build basic proxy and mock provider with ALLOW/BLOCK/TOKENIZE, vault and event log, admin API, CLI and dashboard. **Gate:** synthetic SSN never reaches mock upstream under blocking policy; tokenized email is restored correctly; events omit values.

### Phase 2 — Three classifier types, Ollama setup, and policy editor

Add built-in concepts, exact-value matcher, GLiNER span adapter, Qwen3Guard/LFM3 Ollama adapter with one selected active model, model auto-download and user-prompted Ollama installation, semantic rules and guidance notes; ship model lock manifest and benchmark harness. **Gate:** fixture-level evaluation produces reproducible precision/recall and privacy metrics; model failure triggers policy-specific review/block/private routing without data leakage.

### Phase 3 — Approvals, streaming, integrations

Implement desktop notifications, pending review, one-time authorization, whitelist/blacklist/edit/save-as-rule, SSE restoration, OpenAI-compatible local integration and reversible skill-driven client config. **Gate:** no policy bypass through streaming, retries, edited requests, or approval reuse.

### Phase 4 — Agent setup skill and release packaging

Ship the install skill, cross-platform package/test matrix, upgrade/rollback, dashboard polish, docs and security review. **Gate:** clean-machine skill demo works end-to-end with approval and synthetic egress test.

### Phase 5 — Mandatory NEAR AI Gateway integration

Integrate actual NEAR AI Gateway model discovery, authentication and OpenAI-compatible invocation; distinguish verified TEE-hosted inference from third-party proxying; verify binding of attestation to selected model; implement `PRIVATE_ROUTE` only to validated eligible endpoints. **Gate:** a real NEAR Gateway inference request and verification flow pass, unsupported/proxied models are rejected for private fallback, and sensitive policy fixtures cannot escape on route failure or downgrade. NEAR AI is assumed available for demonstration, but runtime errors must still preserve privacy.

## 11. Product-owner decisions still required — do not delegate to implementer

**Confirmed by the project owner:** Integration is a universal OpenAI-compatible local API, not product-specific plugins. Ollama auto-pulls user-selected Qwen3Guard-Gen-0.6B or LFM3-1.7B after installation; GLiNER uses its local non-Ollama adapter; only one generative classifier is active at once. **NEAR AI Gateway is the heavily preferred/default generation provider**, with verified TEE models prioritized and local Ollama/custom OpenAI endpoints available as policy-constrained fallbacks. NEAR support is mandatory in the product, but **NEAR onboarding may be skipped** so users can start local-only. On uncertain findings and Ollama classifier failure, default to a verified NEAR TEE route when consented/configured, respecting local-only/explicit-block rules. Apply automatic action + desktop notification when flagged. Only the local owner may view raw sensitive pending prompts. Retain audit metadata 30 days by default. Release 1 targets macOS and Linux; Windows is deferred. Release 1 is single-user local-only, without enterprise administration or signed central policy sync. Inspect tool-call payloads in OpenAI API messages only, not actual execution. OS-wide egress control is not in v1. NEAR availability is assumed for the hackathon demo but does not excuse unsafe runtime fallback.

**Further confirmed:** 15-minute encrypted pending-payload expiry; onboarding consent with configurable local block-and-review alternative; critical PII and secrets blocked by default; guided onboarding with toggles, custom exact values, semantic concepts, and revisitable Settings; GLiNER PII Edge default and all-three-model regression suite; user choice of NEAR model with DeepSeek V4 Flash as verified preferred default; both OpenAI Chat Completions and Responses endpoints; input and output protection.

**All previously asked release-defining product choices are resolved:** immediate approval-required response and explicit retry; critical blocks editable by owner after warning; NEAR strongly promoted but skippable during local-only onboarding; local Ollama/custom OpenAI-compatible inference endpoints can be selected as fallbacks, constrained by privacy tier and policy. NEAR AI support remains a mandatory product release feature, not a mandatory user account.

**Engineering verification tasks (not product-owner decisions):** independently verify client-visible NEAR attestation, enclave measurement, selected model, response signature, and any enclave-bound encryption against live official SDK/docs. Do not conflate signatures, proxy attestations, or TLS with enclave privacy. If verification fails, block TEE-required routing. Validate real capabilities of each supported protocol/model/OS rather than implementing a fake passing adapter. If a technical limitation prevents a confirmed product contract, report it to the project owner instead of changing behavior unilaterally.

## 12. Instructions to implementing agent

Treat the confirmed decisions in this file as the product contract. **Do not make new release-defining product choices on behalf of the project owner.** The remaining section 11 items are engineering verification work, not product-owner questions. If actual provider/API/security limitations contradict this contract, report the precise blocker to the project owner before changing behavior or claiming completion. Implement in phases, with tests and runnable demos at each gate. Record deviations in `docs/deviations.md` with rationale and user-visible impact. Never claim a privacy guarantee without a test, measurement, or threat-model qualification. Keep existing user files/configuration intact and make integration changes reversible. Implement the `promptvpn-setup` skill as part of the product deliverable, not as optional documentation.

