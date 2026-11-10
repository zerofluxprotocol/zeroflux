# ZFX VPN threat model

ZFX VPN is an application-layer prompt privacy gateway. It inspects what a client sends to its OpenAI-compatible API (`/v1/chat/completions`, `/v1/responses`) and what the model sends back. It is not a network VPN: it does not route, encrypt or filter other traffic, and it never sees what an agent does outside that API (shell commands, HTTP calls, browsers, file uploads made by the tool itself).

This document covers the three access modes, what verified TEE attestation does and does not prove, how streamed output is handled, who holds which keys, and the incident assumptions behind the design. The local daemon's own asset table and mitigation list are in `vpn/docs/threat-model.md`; this document is the product-level view and takes precedence where they differ.

## Who can see plaintext

| Mode | Raw prompt | Placeholder map | Model output with real values | Metadata (categories, actions, tier, token counts) |
|---|---|---|---|---|
| Local (`zfx-vpn` on your machine) | Your machine only | Your machine (encrypted vault, 24 h) | Your machine only | Your machine (30 days, MAC-chained) |
| Hosted (`zfx-vpn-hosted`, called directly) | **ZFX hosted service and its operators** | Hosted process memory, one request | Hosted process memory, one request | Hosted service (30 days default) |
| Hosted transport + local preprocessing | Your machine only | Your machine only | Your machine only | Both (hosted sees tier, action, counts) |

The model provider sees, in every mode, whatever the policy allows for that destination's privacy tier:

- `TRANSFORMED_PUBLIC` and `UNVERIFIED_EXTERNAL` destinations receive transformed content only: critical values blocked, configured values tokenized, redacted or generalized.
- An `ATTESTED_CONFIDENTIAL` destination (a NEAR AI model whose provider-level attestation verified) can receive content that could not be transformed, but only with the owner's explicit TEE consent, and still never critical categories (they are blocked for every remote tier).
- A `LOCAL_ONLY` destination (loopback Ollama, or a declared local endpoint) is on the device.

### Local mode

Detection rules, registered exact values, custom entity types, the placeholder map, held requests and the audit log stay on the device. The vault is XChaCha20-Poly1305 with a key set in the macOS Keychain or Linux Secret Service. The local daemon binds 127.0.0.1 only. Any local process can reach it, so callers need a per-app key, and browser requests are filtered by Host, Origin and `Sec-Fetch-Site`.

Not protected: an administrator or malware on the same machine (they can read process memory and the keychain), and traffic that does not go through the API.

### Hosted mode

The raw prompt reaches the ZFX hosted service before any value is transformed. **The operator can technically read it.** This mode is not confidential from ZFX, not zero-knowledge, and not running in an enclave. The service says so on every response (`X-ZFX-Trust: operator-sees-raw-prompt; not confidential from ZFX`) and at `GET /v1/zfx/privacy-policy`.

What the hosted service does to limit exposure:

- Placeholder maps live in a fresh in-memory store per request (`EphemeralStore`) and are dropped, and their values zeroized, when the response or stream ends. There is no global or cross-request map, so a token can never be restored for another request, conversation, key or tenant (test: `hosted/tests/hosted.rs::maps_are_per_request_and_tenants_are_isolated`).
- No request or response content is written to disk. Stored: tenant config, SHA-256 hashes of API keys, upstream credentials sealed in a per-tenant vault, usage counters, and metadata-only route and audit records (test: `transforms_restores_and_retains_nothing` scans every file in the data directory).
- No review queue: anything local mode would hold for the owner fails with `ZFX_APPROVAL_REQUIRED` and nothing is stored.
- Idempotent retries do not retain results by default. A retry of a completed request is refused rather than replayed. If the operator enables `--idempotency-response-ttl N`, completed non-streaming responses (which contain restored values) stay in process memory for N seconds; they are never written to disk.
- Tenants are isolated: separate engines, vaults, credentials and route records. Critical categories cannot be weakened by tenant config.
- The operator API listens on a separate address and needs the operator token.

Residual risk accepted in this mode: a compromised hosted process, host, or operator account can read prompts in flight. Logs are metadata-only by construction, but a debugger or a core dump would contain plaintext. Deployment controls (restricted operator access, no core dumps, audited images, short-lived hosts) are listed as release blockers in `vpn/RELEASE_NOTES_FOR_COORDINATOR.md`.

### Hosted transport with local preprocessing

The local daemon detects and transforms, keeps the map in its vault, and sends the transformed payload to the hosted gateway with `X-ZFX-Preprocessed: v1` and `zfx_privacy.preprocessed: true`. The hosted side routes it upstream and returns transformed output; the daemon restores placeholders locally.

The hosted gateway never needs the map. For preprocessed requests it uses a store that refuses to mint or resolve placeholders: if a preprocessed request still contains something the hosted policy would transform, the request is blocked, not transformed on the server (test: `hosted/tests/protected_transport.rs::hosted_refuses_to_transform_preprocessed_traffic`).

The golden test (`hosted_side_never_receives_raw_values_or_the_map`) puts a recording relay between the daemon and the hosted gateway and checks the exact bytes (headers and body) for an email, a phone number and a value registered only on the device, across Chat, streamed Chat and Responses. None of them reach the hosted side or its model.

What the hosted side still learns: the transformed text (including surrounding context the policy allowed), placeholder labels such as `EMAIL`, timing, sizes and token counts. A placeholder hides a value, not the fact that a value of that kind was there.

## Transformation limits

- `TOKENIZE` restores only exact placeholders the model copies back. A paraphrased value is not restored, and a model can reason badly about a placeholder (a tokenized file path breaks a code edit). Owners can choose `PRIVATE_ROUTE` or `BLOCK` for such categories instead.
- `REDACT` is irreversible.
- Detection is not perfect. The deterministic benchmark (270 synthetic cases) has 1 critical escape and 0 false blocks; GLiNER adds names, addresses and health details. Zero escapes on a finite corpus is not proof of zero risk.
- Placeholders are `[[ZFX_<LABEL>_<16 hex>]]` (64 random bits), scoped to app and conversation (local) or to one request (hosted). Prompt VPN 0.1 placeholders (`[[PV_<LABEL>_<8 hex>]]`, 32 bits) still restore within their scope until their 24 h expiry.
- Images, audio and files inside requests cannot be inspected and are blocked for remote destinations by default.

## What NEAR AI TEE verification proves

ZFX VPN treats a NEAR AI model as `ATTESTED_CONFIDENTIAL` only when all of these hold for that exact model and endpoint, at most 10 minutes before sending (`vpn/crates/pv-provider/src/tee.rs`):

1. The NEAR AI inference SDK (0.3.0) verified the gateway TDX quote with TLS key binding, and **at least one provider-level model report** (`model_attestations`, `provider=near`): Intel DCAP quote signature and TCB, report-data binding of the client nonce and the model signing key, MRCONFIGID to the measured compose file, RTMR3 event-log replay, and the compose-manager attestation. A gateway-only proof is rejected; NEAR's own docs say the gateway attestation "is not evidence for a model environment."
2. NVIDIA GPU evidence verified through NRAS, for every report.
3. Every report carries a public key for end-to-end encryption, and private sends use E2EE to it. If E2EE fails, the send fails; there is no plaintext retry.
4. Intel TCB status is `UpToDate` or `SWHardeningNeeded` (stricter than the SDK default, which also accepts `OutOfDate` and `TDRelaunchAdvised`). Debug TDs are rejected.
5. The requested id is the canonical catalog id (not an alias; `x-no-aliasing: true` on every call) and the reports are for that model.
6. The evidence was fetched from the configured provider and base URL; verification is never reused for another destination.
7. If the owner pinned measurements (compose hash, OS image hash) for the model, they match and the pin has not expired.

After sending, the per-response signature is checked. The route stays `PENDING_VERIFICATION` until then. A `provider_tee` signature whose signer was one of the model identities verified before sending sets `RECEIPT_VERIFIED`; a gateway-only signature sets `GATEWAY_ONLY`; anything else sets `FAILED` (likely route substitution). Callers that need the proof before seeing any output set `zfx_privacy.require_receipt: true`: output is buffered (`X-ZFX-Notice: ZFX_STREAM_SAFETY_BUFFERED`), and is withheld with `ZFX_TEE_UNVERIFIED` unless a provider-TEE signature verifies. NEAR documents that streamed completions, the Responses API and alias resolution get gateway signatures, so `require_receipt` on those paths fails closed.

What verification does not prove:

- That the software inside the CVM is free of vulnerabilities. A valid quote shows which measured configuration booted, not that it cannot be exploited.
- Which runtime image is serving now. On NEAR model CVMs the measured compose launches a compose manager; model-serving containers come from later compose-manager actions. The SDK checks the action log's binding, not the images' provenance.
- That the instance that answered is the one that was attested. NEAR documents that instances of a model share one signing key, so a matching signer identifies the deployment, not the instance.
- Anything about NEAR's other software: the gateway is attested separately, and a `provider_tee` signature does not bind to the preflight gateway or TLS evidence (NEAR's documented gap).
- Freshness beyond the nonce. NEAR publishes no expiry; ZFX VPN uses a 10 minute maximum evidence age.
- Measurement allowlists. NEAR publishes none. Pins are owner-captured and expire. On 2026-11-10 two attestations of `deepseek-ai/DeepSeek-V4.1-Flash-1017` taken minutes apart reported different compose hashes (`c53e254e...` and `448683dd...`) with the same OS image hash, so compose-hash pins must allow for several valid deployments or they will reject legitimate instances.

Status of live verification: attestation for `deepseek-ai/DeepSeek-V4.1-Flash-1017` verified live on 2026-11-10 (`cargo test -p pv-provider --test live_near -- --ignored`, Intel DCAP via PCCS and NVIDIA NRAS, TCB `UpToDate`). A report recorded the same day replays offline with full DCAP signature checks (`tests/near_replay.rs`). No NEAR API key was available, so live inference, live E2EE and live response signatures have not been exercised; those paths are tested against a mock backend whose evidence is labelled synthetic.

## NEAR AI SGLang incident (9 November 2026)

NEAR's report: an attacker exploited a remote code execution vulnerability in SGLang to run code on one inference server and reach their infrastructure. Exposed: account details of about 100 users of the deprecated Private Chat, 18 legacy non-TEE agents (chat history, settings, logs), 2 Cloud API keys found in logs (revoked), and "On November 8, they could also have observed user requests being processed on the accessed server, including requests running in confidential mode." NEAR's remediation: credential rotation, session revocation, Private Chat offline, an SGLang fix on all inference servers, forensic review. The report names no CVE or version. NEAR states: "Confidential computing protects data from the operator and the host. It does not protect against a vulnerability in the software running inside the environment."

Assumptions ZFX VPN makes because of it:

- A verified TEE route is an *additional* layer, not a guarantee. Critical categories are blocked for TEE destinations too, and the default for uncertain content is local review, not TEE routing. Sending raw content to a TEE needs explicit owner consent.
- The serving stack inside an attested CVM (SGLang/vLLM, sidecars, debug endpoints, logs) is untrusted code that can be compromised. The product copy never says a TEE makes prompts inaccessible to the provider. (NEAR's private-inference page still says "technically impossible for anyone to see your data"; we do not repeat that.)
- Shared per-model signing keys mean one compromised instance could weaken signatures and E2EE for all instances of that model. Owners who need stronger assurance should pin measurements, require receipts, and watch NEAR advisories.
- API keys found in logs are a real failure mode. ZFX VPN never logs credentials, sends each destination only its own credential, and never forwards the caller's key upstream (tests: `tee_gates.rs::credentials_and_verification_never_cross_destinations`, `streaming_redteam.rs::exact_egress_bytes_never_contain_critical_values`).
- The same class of bug in the ZFX hosted gateway would expose raw prompts in hosted mode. That is why hosted mode is disclosed as operator-visible and why sensitive users are pointed at local mode or protected transport.

## Streaming and output

- Route choice is final before the first byte reaches the caller. Failover to another destination happens only when the upstream fails before answering (connection error, 5xx, 429), and every failover target is re-evaluated under policy. After any byte is released, an upstream failure ends the stream with a `ZFX_UPSTREAM_FAILURE` error event; nothing is replayed elsewhere. A failed confidential route is never retried on a public model.
- Output is inspected with outbound rules before restoration. Text is released through a hold-back window (256 bytes, longer for unfinished PEM blocks) so a finding or a placeholder is never split; tool-call arguments are buffered whole per call. The placeholder matcher holds back any suffix that could be the start of a token (longest token 65 bytes).
- When output must be classified as a whole (outbound span model, or `require_receipt`), the stream is buffered to completion (size-capped) and announced with `X-ZFX-Notice: ZFX_STREAM_SAFETY_BUFFERED` and `stream=buffered` in `X-ZFX-Privacy-Route`. Nothing is released and then retracted. Callers that cannot accept buffering set `zfx_privacy.stream_buffering: reject` and get `ZFX_STREAM_SAFETY_BUFFERED` before anything is sent.
- Malformed upstream chunks, upstream error objects mid-stream (which may echo input), invalid UTF-8 and idle upstreams end the stream with an error event; upstream error text is not forwarded. Client cancellation stops the upstream read and is audited.
- Tested with SSE delivered 1 to 64 bytes per network write (splitting frames, JSON objects and UTF-8 characters), fake and partial placeholders, 24 concurrent conversations across two apps, and streamed tool arguments (`vpn/crates/pv-daemon/tests/streaming_redteam.rs`).

## Key custody

| Key | Local | Hosted |
|---|---|---|
| Vault data key set | OS keychain (service `zfx-vpn`); file fallback reported as degraded | Per-tenant key file in the data dir (degraded; production needs a KMS, listed as a blocker) |
| Upstream provider credentials | OS keychain (`zfx-vpn`/`cred:<ref>`) | Sealed in the tenant vault |
| Caller keys | Local app keys `zfxk_...` (Prompt VPN `pvk_...` still valid), SHA-256 in SQLite | Tenant keys `zfx_sk_...`, SHA-256 in SQLite, scoped, rotatable |
| Admin | `admin.token` file, 0600 | Operator token (env or `operator.token`, 0600), separate listener |
| NEAR model keys | Held by NEAR's CVMs; ZFX only verifies and encrypts to them | Same |

Upgrade from Prompt VPN copies the keychain key set and credentials from service `promptvpn` to `zfx-vpn` and reads them back byte for byte; the old entries are kept for rollback. Nothing is re-encrypted (test: `vpn/crates/pv-daemon/tests/migration.rs`). On macOS the first read of the old entry by the new binary can show a keychain access prompt.

## Error and header hygiene

Error bodies, headers (`X-ZFX-Privacy-Route`, `X-ZFX-Request-Id`, `X-ZFX-Notice`, `X-ZFX-Trust`) and route records never contain detected values. The `zfx_privacy` extension is parsed strictly and stripped before forwarding; validation errors do not echo field contents.
