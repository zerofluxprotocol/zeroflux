# Deviations

Two specifications apply: the original Prompt VPN spec (`docs/spec-v1.4.md`, entries 1 to 19) and the ZeroFlux launch brief for ZFX VPN (sections 1, 2, 3 and 6; entries 20 onward). Where the launch brief and the tested code disagree, the tested design was kept and the difference is recorded here. Each entry: what differs, why, user-visible impact. Items marked **needs the project owner** are product or access decisions I did not make unilaterally.

## Provider and model facts that contradict the spec

1. **`deepseek-ai/DeepSeek-V4-Flash` is not a NEAR TEE model (needs the project owner).** On the live gateway (checked 2026-11-08) this ID is an alias of `z-ai/glm-5.3-flash`. Without `x-no-aliasing`, a request for "DeepSeek" silently runs GLM and returns GLM's attestation. Prompt VPN sends `x-no-aliasing: true` and checks the catalog, so this ID is rejected as not eligible. The NEAR-operated TEE DeepSeek is `deepseek-ai/DeepSeek-V4.1-Flash-1017`, which passes full verification live. The wizard proposes it as the candidate but the user must select it explicitly, as section 2.4 requires when the preferred model is not eligible. Impact: the spec's default cannot be honored; the project owner should confirm V4.1-Flash-1017 (or another verified model) as the new preferred default.

2. **Live NEAR inference not tested (the project owner's decision).** Attestation verification passes live without a key (`cargo test -p pv-provider --test live_near -- --ignored`). Chat inference, E2EE and response signatures go through NEAR's official SDK and are exercised with a mock backend in tests; users need a NEAR API key (`promptvpn near login`) for real replies.

3. **Generative classifiers removed (the project owner's decision, 2026-11-08).** The spec called for a user choice between Qwen3Guard-Gen-0.6B and LFM3-1.7B through Ollama, automatic pulls, and a three-model regression suite. Measured on the benchmark, Qwen3Guard caught 0 of 8 custom topics, and LFM3 false-blocked 13% of ordinary prompts at up to 3 s latency (on Apple Silicon it also ignored the JSON schema). Neither returned reliable spans. Detection is now deterministic rules + exact terms + GLiNER PII (edge/small/base, base default), all producing exact spans. Ollama is no longer needed for protection; it remains an optional local generation destination. The regression suite covers the three GLiNER sizes.

4. **Semantic concepts replaced by custom entity types (the project owner's decision).** Topic-level concepts ("anything about unreleased financials") needed a generative judge. They are replaced by owner-defined entity types that GLiNER finds as spans ("insurance policy number", "employee id"), which can be tokenized and restored, plus exact terms for codenames and client names. Guidance notes, which only fed the generative model, are removed.

5. **Model-only critical findings below 0.8 confidence go to review.** GLiNER labels ID-like strings (e.g. policy numbers) as driver's licenses or passports at moderate confidence. A hard block on that alone would break ordinary requests, so such spans become `UNCERTAIN` and follow the fallback (review or private route). Pattern-matched critical data is still blocked outright.

6. **GLiNER runtime is vendored and patched.** gline-rs 1.1.0 assumed DeBERTa token IDs and an older logits layout; with `gliner-pii-edge-v1.0` every score was negative. `third_party/gline-rs/PROMPTVPN_PATCH.md` documents both fixes. GLiNER output also passes a shape check (an ID needs digits, an address needs a number) because the model labels product names and countries as IDs and addresses. Base uses span mode (DeBERTa); edge and small use token mode (ModernBERT); both are supported.

## Behavioral choices made within the contract

7. **Responses API on private routes is translated to Chat Completions.** NEAR field E2EE covers Chat only. Clients still receive Responses-format output.
8. **Streamed NEAR responses carry a gateway signature**, not a model-TEE signature (SDK and gateway behavior). Receipts record which kind was verified.
9. **Placeholder scope without a conversation header** is `app:<hash of the first two content fields>`. Clients that send `X-PromptVPN-Conversation` get exact scoping.
10. **`FALLBACK` action.** An internal policy action that resolves to `PRIVATE_ROUTE` or `REQUIRE_APPROVAL` from the owner's fallback setting, so default rules don't hardcode a choice the user makes in onboarding.
11. **Local private handling without TEE consent.** With TEE routing selected and only "allow local model" enabled, uncertain content goes to local Ollama (it never leaves the device). With block-and-review selected it is always held.
12. **Whitelist / blacklist on held requests.** Whitelist registers the flagged noncritical values as allowlisted and approves once; critical values cannot be allowlisted. Blacklist registers flagged values as blocked exact secrets and denies.
13. **The detection model is required once installed.** If an installed GLiNER model fails to load or run, or custom entity types exist without a model, requests fail closed to review (`CLASSIFIER_FAILURE`). With no model installed, only deterministic recognizers run and Health says so.
14. **Additional error codes**: `PV_UNAUTHORIZED`, `PV_BAD_REQUEST`, `PV_PAYLOAD_TOO_LARGE`, `PV_OUTPUT_BLOCKED`. Approval-required uses HTTP 428.
15. **Exact values default to case-insensitive, Unicode-folded, whitespace-normalized matching.** Separator stripping stays opt-in to avoid broad collisions.
16. **Client integrations** cover Codex, Aider, opencode and environment variables. Other clients get instructions only, per the skill's stop conditions.

## Verification gaps

17. **Linux runtime not exercised.** There is no Linux host or Docker here, and the project does not use hosted CI. Linux builds and tests are pending a Linux machine: run `sh scripts/check.sh` there. The Linux keyring backend (`linux-native-sync-persistent`) and `notify-send` notifications are untested.
18. **No signed releases (the project owner's decision).** Users install with `install.sh`, which builds from source on their machine, like other coding-agent installers. The earlier release signing key is kept only as a backup in the macOS Keychain (`promptvpn-release-signing-key`) and is not used.
19. **Desktop notifications** use `osascript` (macOS) and `notify-send` (Linux) without an app bundle identity, so macOS shows them as Script Editor notifications.

## ZFX VPN launch brief (2026-11-10)

20. **No generative classifiers (brief 3.5 lists "GLiNER + selected Qwen3Guard/LFM3 via Ollama, one generative classifier active at once").** Kept the 2026-11-08 re-architecture (entries 3 and 4): GLiNER spans plus deterministic recognizers and exact values. The brief's "semantic topics" detector (3.2 step 2) is therefore custom entity types, not topic classification. Reason: measured on the benchmark, Qwen3Guard caught 0 of 8 custom topics and LFM3 false-blocked 13% of ordinary prompts. Impact: whole-message topic policies ("anything about unreleased financials") are not supported; owners describe entity types or register exact terms.

21. **Crate names stay `pv-*`.** Public surfaces were renamed (binary, data dir, keychain service, env vars, headers, error codes, placeholders, skill, docs, UI). Internal crate names were not, to keep the diff reviewable and the history traceable. The vault's HKDF salt (`promptvpn-v1`), AAD formats and the recovery-bundle format id (`promptvpn-recovery-v1`) are frozen on purpose: changing them would make existing encrypted data unreadable.

22. **Placeholders are `[[ZFX_<LABEL>_<16 hex>]]`, not `[[ZFX_E_...]]`.** The brief's example is illustrative. The label is kept because models use it to write sensible text around the placeholder, and the random part grew from 32 to 64 bits. Legacy `[[PV_..._<8 hex>]]` placeholders still restore.

23. **Error codes.** The brief's eight codes exist. `ZFX_DETECTION_UNAVAILABLE` replaces `PV_CLASSIFIER_UNAVAILABLE`, `ZFX_UPSTREAM_FAILURE` replaces `PV_PROVIDER_ERROR`; `PV_ROUTE_FORBIDDEN` is split into `ZFX_ROUTE_FORBIDDEN`, `ZFX_TEE_UNVERIFIED` and `ZFX_FALLBACK_UNAVAILABLE` (all report `legacy_code: PV_ROUTE_FORBIDDEN`). Additional codes for the hosted service: `ZFX_FORBIDDEN_SCOPE`, `ZFX_RATE_LIMITED`, `ZFX_QUOTA_EXCEEDED`, `ZFX_IDEMPOTENCY_CONFLICT`, `ZFX_NOT_FOUND`. `ZFX_STREAM_SAFETY_BUFFERED` is a notice header when buffering happens, and an error (409) only when the caller set `stream_buffering: reject`. Audit events written before the upgrade keep their `PV_*` codes: they are MAC-chained and are not rewritten.

24. **Hosted idempotency keeps no results by default.** The brief requires idempotent retries and no raw retention by default; replaying a result requires keeping model output (with restored values). Default: a retried completed request is refused with `ZFX_IDEMPOTENCY_CONFLICT` and never re-executed or re-billed. Operators can enable in-memory replay for N seconds (`--idempotency-response-ttl`). Idempotency state is per process; a multi-instance deployment needs a shared store (blocker in the release notes).

25. **Hosted mode has no review queue.** Local mode holds uncertain requests for the owner; the hosted service would have to store raw payloads to do that. It returns `ZFX_APPROVAL_REQUIRED` and stores nothing.

26. **Hosted tenants have no exact-value registry or custom entity types yet.** Those need tenant secrets stored on the hosted side. Tenants that need them use protected transport, where the local daemon's registry applies.

27. **Default NEAR model and "eligible" remain as confirmed on 2026-11-08** (entry 1). The brief's default-to-NEAR rule is unchanged: NEAR is the recommended destination, onboarding may skip it.

28. **Measurement pins are owner-captured, not shipped.** NEAR publishes no measurement allowlist. ZFX VPN checks pinned compose and OS image hashes with an expiry when the owner sets them; none are set by default. Two attestations of the default model taken minutes apart had different compose hashes, so a single-value pin would reject legitimate instances.

29. **TCB policy is stricter than the NEAR SDK default.** Confidential routes accept `UpToDate` and `SWHardeningNeeded` only. The SDK also accepts `OutOfDate` and `TDRelaunchAdvised`. If NEAR's fleet falls behind on microcode, confidential routes fail closed until it is patched.

30. **Linux not exercised** (entry 17 still applies): migration, keychain (Secret Service) and the hosted service were tested on macOS only.

31. **Real-keychain migration is not in the automated suite.** On macOS, the first read of a keychain item created by the old binary shows an access prompt, which blocks unattended runs (observed on 2026-11-10: the read waited for the dialog until the test timed out). The keychain path is tested with an in-memory store that holds exactly what Prompt VPN 0.1 writes (same service, account derivation and JSON), and the file-backend path end to end with the real 0.1 binary.

32. **Demo videos and the Remotion project (`demo/`, `docs/demo/`) still say "Prompt VPN".** Re-rendering them was out of scope; the README says so.
