# ZeroFlux one-command targets. Each target runs scripts/gates.py, which
# records every step in bench/results/gates/<target>.json and exits non-zero
# if any required step fails. Missing hardware or credentials are recorded as
# BLOCKED external steps with the exact reason (ZFX_REQUIRE_EXTERNAL=1 makes
# them fail the target instead).
PY := $(shell [ -x .venv/bin/python ] && echo .venv/bin/python || echo python3)
GATES := $(PY) scripts/gates.py

.PHONY: check e2e-native e2e-bridge e2e-intents e2e-near security-test gpu-test testnet-smoke acceptance localnet localnet-down

check:          ## unit, lint, types, all deterministic vectors
	$(GATES) check
e2e-native:     ## three ZeroFlux nodes, 1000 mined blocks, wallet and explorer
	$(GATES) e2e-native
e2e-bridge:     ## native + NEAR sandbox: 100 two-way transfers, reorgs, governance/recovery
	$(GATES) e2e-bridge
e2e-intents:    ## quotes, fill/expiry/refund against the local test solver
	$(GATES) e2e-intents
e2e-near:       ## ZFX mined natively on NEAR (PoW verified on-chain) and the ZeroFlux PoW light client on NEAR
	$(GATES) e2e-near
security-test:  ## fuzz/property/adversarial tests, sanitizers, audit, SBOM, licenses, repro build
	$(GATES) security-test
gpu-test:       ## all installed GPU backends; untested hardware reported as BLOCKED
	$(GATES) gpu-test
testnet-smoke:  ## real NEAR testnet, only with an explicitly configured account
	$(GATES) testnet-smoke
acceptance:     ## runs every target, writes ACCEPTANCE_REPORT.{json,md}
	$(PY) scripts/acceptance.py
localnet:       ## start 3 nodes + NEAR sandbox + relayers + coordinator; Ctrl-C stops
	$(PY) deploy/localnet.py up
localnet-down:
	$(PY) deploy/localnet.py down
