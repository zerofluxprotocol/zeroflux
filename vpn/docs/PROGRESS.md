# Prompt VPN build progress

Tracks every requirement in `docs/spec-v1.4.md` against what is built and tested.
Status keys: `[x]` done and tested, `[~]` done with a stated gap, `[!]` blocked on the project owner.

## Phase 0: contracts and threat model
- [x] Typed, versioned schemas (Policy, Finding, Decision, TransformMap, AuditEvent, PendingApproval, ProviderConfig, ClientIntegration) in `pv-core`
- [x] JSON Schema export (`promptvpn schema`, `schemas/`), TypeScript generation (`apps/dashboard npm run gen:types`) (`scripts/check.sh` checks both are current)
- [~] OpenAPI: not generated; the admin API is described in the dashboard's typed client and the schemas. Low value for a local-only API
- [x] Threat model + trust boundaries (`docs/threat-model.md`), policy semantics (`docs/policy-semantics.md`)
- [x] Executable architecture tests: precedence, audit privacy, block behavior (`pv-core/tests/policy.rs`, `pv-daemon/tests/gates.rs`)
- [x] Migration tests (`pv-audit/tests/store.rs`: idempotent apply, newer schema refused)

## Phase 1: daemon, CLI, proxy, vault, dashboard shell
- [x] Daemon bound to 127.0.0.1 only; port conflict fails loudly, never binds elsewhere
- [x] Admin auth (install token), single-use login ticket → HttpOnly SameSite=Strict cookie, CSRF header, Origin/Sec-Fetch-Site/Host checks, no CORS, strict CSP
- [x] AEAD vault (XChaCha20-Poly1305, AAD per row), key IDs, OS keychain key set, rotation, Argon2id recovery bundle, fail-closed lock
- [x] Audit events: metadata only, HMAC chain, retention with anchors, export, delete, verify
- [x] Mock upstream recording exact bytes and auth headers
- [x] **Gate:** SSN never reaches mock upstream; tokenized email restored; events and disk omit values

## Phase 2: detection (re-architected 2026-11-08: no generative classifiers)
- [x] 15 built-in categories with checksums and context cues; encoded (base64, %XX, \u, \x) and Unicode-confusable views
- [x] Exact values, patterns, file fingerprints; keyed fingerprints; allowlist
- [x] GLiNER PII edge/small/base embedded (vendored gline-rs, token + span mode), pinned + sha256-verified downloads, base default, loads off the startup path
- [x] Custom entity types (GLiNER zero-shot labels → `CUSTOM:<id>` spans, tokenize by default)
- [x] Fail closed when the installed model fails or custom entities exist without a model; model-only critical spans < 0.8 → review
- [x] Overlap merge keeps secret findings
- [x] models.lock.json (GLiNER sizes)
- [x] Eval corpus (270 cases incl. free-text PII and custom entities); results per GLiNER size in `docs/evaluation.md`
- [x] Generative classifiers (Qwen3Guard, LFM3) and semantic concepts removed by the project owner's decision after measurement (`docs/deviations.md` #3–4)

## Phase 3: approvals, streaming, integrations
- [x] REQUIRE_APPROVAL / BLOCK_AND_REVIEW → immediate 428 `PV_APPROVAL_REQUIRED`, encrypted 15-min payload, crash-safe expiry
- [x] Approve once, deny, cancel, edit-and-retry (reclassified), whitelist, blacklist, save-as-rule (exact/category/concept/guidance)
- [x] Owner-only reveal with confirmation, audited
- [~] Desktop notifications: macOS osascript (verified), Linux notify-send (untested: no Linux host), rate limited, no values
- [x] Chat Completions and Responses: non-stream, SSE, tools; Responses↔Chat translation
- [x] Output inspection, 256-byte streaming hold-back, scoped restoration
- [x] Client integrations (codex, aider, opencode, env) with backup, idempotent re-run, rollback
- [x] **Gate:** no bypass via streaming, retries, edits, approval reuse (`approvals.rs`, `routing.rs`, `adversarial.rs`)

## Phase 4: setup skill and packaging
- [x] `skills/promptvpn-setup/SKILL.md`, scripts (discover, verified install, egress test, rollback), 10 script tests, generated adapters
- [x] Install via `install.sh` (curl, build from source, no sudo, offers rustup with consent); optional archives via `scripts/package.sh`; macOS x86_64 cross build verified under Rosetta
- [~] Linux x86_64/arm64: code paths exist but never built or run (no Linux host; deprioritized by the project owner since most users are on macOS). Run `sh scripts/check.sh` on a Linux box when needed
- [x] Daemon autostart (launchd / systemd user) only with confirmation
- [x] Uninstall keeps data by default; `--purge` needs typed DELETE
- [x] **Gate (macOS):** clean install from the signed release archive via the skill's `install.sh` into a fresh prefix and data dir, real Keychain, GLiNER base downloaded and loaded, egress test pass, SSN blocked live, client configure + rollback, NEAR verified, dashboard served, `uninstall --purge` removed data and the Keychain entry

## Phase 5: NEAR AI Gateway
- [x] Model discovery with privacy tiers; alias detection; `x-no-aliasing`
- [x] Attestation via official `nearai-inference-sdk` 0.3.0: gateway TDX + TLS SPKI binding, model TDX, NRAS GPU evidence required; live test passes
- [x] PRIVATE_ROUTE only to verified eligible models; E2EE to attested key; no downgrade (tested with mock backend)
- [x] Attestation verifies live without a key. Live chat inference intentionally not tested (the project owner); users add a NEAR key with `promptvpn near login`
- [x] Default NEAR model `deepseek-ai/DeepSeek-V4.1-Flash-1017` (confirmed by the project owner); onboarding dropdown lists live NEAR models that pass attestation; `promptvpn near use <model>`

## Dashboard
- [x] Onboarding wizard (6 steps, consent unchecked by default, Skip for now, critical toggles with typed confirmation)
- [x] Overview, Activity, Event detail, Approvals/Review, Policies + playground, Categories, Secrets, Concepts, Apps, Models & destinations, Settings, Health
- [x] Headless check of every page and wizard step against the production daemon: no console errors

## Docs
- [x] README (coverage boundary and limits), threat model, policy semantics, operations, evaluation, deviations

## Log
- 2026-11-08: core crates, daemon, CLI built; ~200 tests green (unit, HTTP integration, approvals, routing, adversarial, skill scripts).
- 2026-11-08: NEAR live: DeepSeek-V4.1-Flash-1017 verifies; DeepSeek-V4-Flash rejected as alias. No API key here.
- 2026-11-08: gline-rs fixes (ModernBERT token IDs, logits layout); GLiNER plausibility filter after real false positives.
- 2026-11-08: LFM3 on Apple Silicon runs on Ollama's MLX runner without schema enforcement; bounded recovery parser.
- 2026-11-08: Real end-to-end: fresh PV_HOME, init, LFM3 + GLiNER active, concept prompt blocked, public spec allowed, email tokenized and restored.
- 2026-11-08: Re-architecture: generative classifiers and concepts removed; GLiNER base default (accuracy 0.944 vs 0.881 rules-only; person recall 0.26 → 0.89; health 0.52 → 0.86; p50 21 ms); custom entity types added.
- 2026-11-08: Demo videos (Remotion, real captures) in docs/demo/; repo public at github.com/numinousmuses/promptvpn; generated eval corpus no longer committed (fake keys tripped GitHub push protection).
- 2026-11-08: Hosted CI removed at the project owner's request (one accidental run cancelled ~1 min in; Actions disabled on the repo). Local `scripts/check.sh` replaces it.
- 2026-11-08: Clean-install gate passed on macOS from the signed release archive (see Phase 4).
- 2026-11-08: Signing dropped for build-from-source `install.sh`; old signing key backed up to macOS Keychain; dashboard dist committed so source builds need only Rust.
