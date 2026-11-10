# ZFX VPN: release readiness inputs (for docs/RELEASE_READINESS.md)

Paste-ready section for the coordinator to merge. Engineering facts are separated from owner decisions. Branch `ws/vpn`, directory `vpn/`.

```
VPN_LOCAL_READY      = macOS: yes for a public beta of local mode; Linux: not run
VPN_HOSTED_READY     = false (service code and tests done; no deployment, KMS, shared state, review)
VPN_PRODUCTION_READY = false
CONFIDENTIAL_CLAIMS  = attestation verified live; confidential inference itself not exercised (no NEAR key)
```

## Verified (with evidence)

| Gate (brief 3.7) | Evidence |
|---|---|
| 1. Existing local policies, keys and audit DB survive upgrade and rollback; old CLI alias works | `cargo test -p pv-daemon --test migration` (5 tests, fixture written by the Prompt VPN 0.1 binary: byte-identical ciphertexts, key file, MAC chain, policy, app keys; keychain copy and rotated-key rollback with an in-memory store). `sh tests/migration/upgrade_rollback.sh` (20 checks: builds 0.1 from the import commit, upgrades through the `promptvpn` link, uses the install, rolls back, 0.1 serves it again) |
| 2. Synthetic critical PII and API keys never reach a public upstream; exact egress bytes | `streaming_redteam.rs::exact_egress_bytes_never_contain_critical_values` (6 values x 6 field placements x stream/non-stream, plus base64 forms; asserts recorded bodies and every header), `gates.rs`, `adversarial.rs` |
| 3. Faithful restoration in non-stream, SSE chunks and tool-call args; conversation and tenant isolation | `streaming_redteam.rs` (SSE split 1 to 64 bytes per write, UTF-8 and JSON split, streamed tool args, 24 concurrent conversations over 2 apps), `hosted.rs::maps_are_per_request_and_tenants_are_isolated` |
| 4. TEE-required route rejects unverified proxy, expired measurement, unavailable E2EE, route substitution, credential crossover | `tee_gates.rs` (8 tests, mutation-checked), `tee.rs` unit tests, `near_replay.rs` (real NEAR report recorded 2026-11-10, offline DCAP; tampering rejected), `live_near.rs --ignored` (passed live 2026-11-10) |
| 5. Hosted raw-prompt disclosure; local-to-hosted transmits no known raw secret | `hosted.rs::disclosure_is_explicit`, `protected_transport.rs` (relay records exact bytes reaching the hosted side; hosted refuses to transform preprocessed traffic) |
| 6. Chat + Responses, stream + non-stream; cancellation, timeouts, malformed chunks, retries, filtering; no downgrade | `streaming_redteam.rs`, `routing.rs`, `tee_gates.rs`, `gates.rs`, `hosted.rs` |
| 7. Onboarding, notifications, approvals, classifier install, NEAR skip, dashboard, packages on real macOS/Linux | Partly: dashboard rebuilt and type-checked, approvals and onboarding tests pass (`approvals.rs`, `gates.rs`), skill script tests pass. Not re-run for this release: the clean-install gate from a release archive, headless dashboard screenshot check, Linux |
| Both audiences demoed | `sh demos/run_all.sh` (builder app through hosted gateway; developer coding-agent session through local daemon, then protected transport) |

## Blockers for VPN_PRODUCTION_READY

| # | Blocker | Status | Owner |
|---|---|---|---|
| 1 | NEAR AI API key to exercise live confidential inference, E2EE (v3) and `provider_tee` response signatures end to end | Code path uses NEAR's official SDK; tested only with a mock backend | Project owner (key) |
| 2 | Third-party security review of the local daemon, hosted gateway and migration | Not started | External reviewer |
| 3 | Hosted deployment design: KMS for tenant vault keys (today a key file per tenant), shared idempotency and rate-limit store for more than one instance, no core dumps, restricted operator access, audited images | Single-process service only | ZeroFlux core |
| 4 | Hosted terms of service and privacy policy text that matches the disclosure (operator sees raw prompts in hosted mode) | Disclosure exists in API and docs; no legal text | Project owner, counsel |
| 5 | Linux run of the full check (`sh scripts/check.sh` on Linux x86_64 and arm64), including Secret Service keyring and notifications | Never run; no Linux host | ZeroFlux core |
| 6 | Real macOS Keychain upgrade run by a person (one "Always Allow" prompt) | Unattended run blocked by the OS prompt | Project owner (5 minutes at the Mac) |
| 7 | Measurement pin policy for NEAR models (which compose / OS image hashes to accept, for how long) | Mechanism built; no pins shipped; NEAR publishes no allowlist | Project owner + security reviewer |
| 8 | GLiNER capacity plan for the hosted service (one model instance serializes requests; see hosting numbers) | Measured on this Mac only | ZeroFlux core |
| 9 | Re-render demo videos with the new name | Not done | Design |

## Owner decisions needed (not engineering facts)

- Public hosted endpoint name (`api.zeroflux.network` per the brief) and who operates it.
- Whether hosted idempotent replay may keep results in memory (default: no).
- Default for confidential fallback in onboarding (today: off; local review is the default for uncertain content).
- Whether to accept NEAR models whose TCB is `OutOfDate` (today: rejected for confidential routes).

## Hosting cost inputs (measured on this Mac)

Machine: Apple M5 Pro, 18 cores, 48 GB. Release build. Full table and method: `vpn/docs/hosting-resources.md`.

- Without GLiNER (deterministic detectors only): p50 0.13 ms, p95 1.2 ms per request, 33 MB peak RSS for the evaluation run. Negligible CPU; misses names, free-text addresses and health details.
- With GLiNER base (205 MB on disk, the default): 1.3 s load, ~600 MB steady RSS per process (950 MB peak during load). Per request CPU: ~0.1 CPU-s for a 40-word prompt, ~0.8 CPU-s for 400 words, ~6.6 CPU-s for 3,000 words. One model instance runs one request at a time with up to 4 ONNX threads, so it serves ~40 req/s at 40 words, ~5 req/s at 400 words, ~0.65 req/s at 3,000 words regardless of concurrency.
- GLiNER edge (49 MB) is 10 to 15% cheaper per request than base and small is 15 to 20% more expensive; steady RSS is 0.56 to 0.65 GB for all three. Prompt length dominates cost: about 0.2 CPU-s per 100 words with base.
- Cloudflare Workers: not suitable as built. The gateway is native Rust with ONNX Runtime and a 50 to 200 MB model; it is not compiled to WASM and needs far more memory and CPU time per request than a Worker allows. A container or VM is needed: per model instance, plan 4 vCPU and 1 GB RAM. These figures come from Apple Silicon performance cores; cloud vCPUs were not measured and may be slower.
