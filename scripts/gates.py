#!/usr/bin/env python3
"""Gate runner behind the Makefile targets.

  python3 scripts/gates.py TARGET        (check, e2e-native, e2e-bridge, e2e-intents,
                                           security-test, gpu-test, testnet-smoke)

Each step runs as a subprocess from the repo root. The runner records its
command, exit code, duration and the tail of its output in
bench/results/gates/TARGET.json (full output under bench/results/gates/logs/,
gitignored). Step kinds:

  required   must pass; any failure makes the target exit 1
  external   needs hardware/credentials this host may lack; when unavailable
             it is recorded as BLOCKED with the exact reason, never as PASS,
             and does not fail the target (set ZFX_REQUIRE_EXTERNAL=1 to make
             a BLOCKED external step fail it)

Every runnable step executes even after an earlier failure, so one report
shows everything that is broken.
"""
import json
import os
import shutil
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUTDIR = os.path.join(ROOT, "bench", "results", "gates")
PY = os.path.join(ROOT, ".venv", "bin", "python")
if not os.path.exists(PY):
    PY = sys.executable
ENV = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ.get("PATH", ""))
VEC = "tests/vectors/fp8c_v0.1.json"


def have(cmd):
    return shutil.which(cmd) is not None


def py_has(mod, expr="True"):
    return subprocess.run([PY, "-c", f"import {mod}; assert {expr}"], capture_output=True).returncode == 0


def step(name, cmd, kind="required", avail=None, timeout=3600, env=None):
    return {"name": name, "cmd": cmd, "kind": kind, "avail": avail, "timeout": timeout, "env": env or {}}


def nvidia():
    return (True, "") if have("nvidia-smi") else (False, "no NVIDIA GPU/driver on this host (nvidia-smi absent)")


def amd():
    return (True, "") if (have("rocm-smi") or have("amd-smi")) else (False, "no AMD GPU/ROCm on this host (rocm-smi/amd-smi absent)")


def mps():
    return (True, "") if py_has("torch", "torch.backends.mps.is_available()") else (False, "torch MPS unavailable")


def mlx():
    return (True, "") if py_has("mlx.core") else (False, "mlx not installed")


def zig():
    return (True, "") if py_has("ziglang") else (False, "ziglang not installed in .venv (uv pip install ziglang)")


def near_testnet():
    r = subprocess.run([PY, "tests/e2e/testnet_smoke.py", "--preflight"], cwd=ROOT, capture_output=True, text=True)
    if r.returncode == 0:
        return True, ""
    # Never touch bench/results/testnet_smoke.json here: a blocked preflight
    # must not overwrite the evidence of an earlier real run.
    try:
        return False, json.loads(r.stdout.strip().splitlines()[-1])["reason"]
    except Exception:
        return False, r.stdout[-300:] + r.stderr[-300:]


def remote_gpu_runner():
    # Optional, kept outside the repository: a script that rents ephemeral GPU
    # machines and runs bench/gpu/box_run.sh on them.
    r = os.environ.get("ZFX_GPU_RUNNER", "")
    if not r or not os.path.exists(r):
        return False, "remote GPU rerun is opt-in: set ZFX_GPU_RUNNER to a runner script (it rents GPU machines)"
    return True, ""


TARGETS = {
    "check": [
        step("rustfmt", "cargo fmt --all --check && (cd bridge/near && cargo fmt --all --check)"),
        step("clippy-workspace", "cargo clippy --release --workspace --all-targets -q -- -D warnings"),
        step("clippy-near-contracts", "cd bridge/near && cargo clippy -q --all-targets -- -D warnings"),
        step("cargo-test-workspace", "cargo test --release -q --workspace"),
        step("cargo-test-near-contracts", "cd bridge/near && cargo test -q"),
        step("wasm-build-wzfx", "cd bridge/near/wzfx && cargo near build non-reproducible-wasm"),
        step("wasm-build-test-ft", "cd bridge/near/test-ft && cargo near build non-reproducible-wasm"),
        step("wasm-build-pow-verifier", "cd bridge/near/pow-verifier && cargo near build non-reproducible-wasm --no-abi"),
        step("build-release", "cargo build --release -q"),
        step("vectors-rust", "./target/release/zpu-cli selftest"),
        step("vectors-consensus-v2", "cargo test --release -q -p zfx-verify --test consensus_v2"),
        step("cpp-build", "make -C miner/cuda cpu -s"),
        step("vectors-cpp", f"./miner/cuda/build/zpu-miner selftest {VEC}"),
        step("vectors-python-numpy", f"{PY} miner/python/zpu_miner.py selftest {VEC} --backend numpy"),
        step("stress-python-numpy", f"{PY} miner/python/zpu_miner.py stress --backend numpy"),
        step("vectors-mlx-metal", f"{PY} miner/python/zpu_miner.py selftest {VEC} --backend mlx", "external", mlx),
        step("vectors-torch-mps", f"{PY} miner/python/zpu_miner.py selftest {VEC} --backend torch --device mps", "external", mps),
        step("gpu-device-compile", f"CLANG='{PY} -m ziglang clang' bench/compile/device_check.sh", "external", zig),
        step("python-lint", "uvx pyflakes tests/e2e deploy/localnet.py testnet/zfxnet.py scripts bridge/intents/zfx_intents miner/python bench/gpu"),
        step("explorer-typecheck", "cd explorer && npx --no-install tsc -p . --noEmit"),
        step("intents-nep413-conformance", "cargo test --release -q -p zfx-bridge --test nep413_conformance"),
    ],
    "e2e-native": [
        step("native-3-nodes-1000-blocks", f"{PY} tests/e2e/native.py", timeout=7200),
        step("contract-api-schema-conformance", f"{PY} tests/contract_api/test_contract_api.py", timeout=1200),
    ],
    "e2e-bridge": [
        step("bridge-100-two-way-fault-injection", f"{PY} tests/e2e/bridge.py", timeout=5400),
        step("bridge-finality-reorg", f"{PY} tests/e2e/bridge_reorg.py", timeout=3600),
        step("bridge-governance-recovery", f"{PY} tests/e2e/bridge_governance.py", timeout=3600),
    ],
    "e2e-intents": [
        step("intents-local-solver-settlement", f"{PY} tests/e2e/intents.py", timeout=3600),
    ],
    "security-test": [
        step("fuzz-parsers", "cargo test --release -q -p zfx-node --test fuzz_parsers -- --nocapture", env={"ZFX_FUZZ_ITERS": "20000"}),
        step("node-integrity-crash-corruption-backup", "cargo test --release -q -p zfx-node --test node_integrity"),
        step("chain-rules-adversarial", "cargo test --release -q -p zfx-node --test chain_rules"),
        step("consensus-v2-adversarial", "cargo test --release -q -p zfx-verify --test consensus_v2"),
        step("wzfx-contract-negative", "cd bridge/near && cargo test -q -p wzfx"),
        step("miner-asan-ubsan", "make -C miner/cuda sanitize-test"),
        step("research-attacks", "./target/release/zfx-research attacks --quick --out /tmp/zfx-attacks"),
        step("cargo-audit-workspace", "cargo audit --deny warnings"),
        step("cargo-audit-near-contracts", "cd bridge/near && cargo audit --ignore RUSTSEC-2026-0387 --ignore RUSTSEC-2024-0449"),
        step("release-licenses-sbom-reproducible", f"{PY} scripts/release_check.py", timeout=3600),
    ],
    "gpu-test": [
        step("gpu-device-compile-sm90-sm100-sm120-gfx942-gfx950", f"CLANG='{PY} -m ziglang clang' bench/compile/device_check.sh", "external", zig),
        step("apple-mlx-selftest", f"{PY} miner/python/zpu_miner.py selftest {VEC} --backend mlx", "external", mlx),
        step("apple-mlx-stress", f"{PY} miner/python/zpu_miner.py stress --backend mlx", "external", mlx),
        step("apple-mps-selftest", f"{PY} miner/python/zpu_miner.py selftest {VEC} --backend torch --device mps", "external", mps),
        step("apple-mps-stress", f"{PY} miner/python/zpu_miner.py stress --backend torch --device mps", "external", mps),
        step("nvidia-local", "make -C miner/cuda cuda && ZPU_GPU_BIN=miner/cuda/build/zpu-miner-cuda deploy/gpu/entrypoint.sh gpu-test", "external", nvidia),
        step("amd-local", "make -C miner/cuda hip && ZPU_GPU_BIN=miner/cuda/build/zpu-miner-hip deploy/gpu/entrypoint.sh gpu-test", "external", amd),
        step("remote-h100-mi355x-cloud", f"{PY} \"$ZFX_GPU_RUNNER\" --tag gpu_rerun_$(date +%Y-%m-%d)", "external", remote_gpu_runner,
             timeout=7200),
    ],
    "testnet-smoke": [
        step("near-testnet-deploy-and-receipts", f"{PY} tests/e2e/testnet_smoke.py", "external", near_testnet, timeout=3600),
    ],
}


def tail(path, n=25):
    try:
        return open(path, errors="replace").read().splitlines()[-n:]
    except OSError:
        return []


def run_target(target):
    os.makedirs(os.path.join(OUTDIR, "logs", target), exist_ok=True)
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    # Source changes only: the suites themselves rewrite tracked evidence files.
    dirty = bool(subprocess.run(["git", "status", "--porcelain", "--untracked-files=no", "--", ".", ":!bench/results", ":!schemas/fixtures",
                                 ":!ACCEPTANCE_REPORT.json", ":!ACCEPTANCE_REPORT.md"], cwd=ROOT, capture_output=True, text=True).stdout.strip())
    rep = {"target": target, "commit": commit, "dirty_tree": dirty, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "host": os.uname().machine,
           "steps": []}
    require_ext = os.environ.get("ZFX_REQUIRE_EXTERNAL") == "1"
    for s in TARGETS[target]:
        rec = {"name": s["name"], "kind": s["kind"], "cmd": s["cmd"]}
        if s["avail"]:
            ok, why = s["avail"]()
            if not ok:
                rec.update(result="BLOCKED", reason=why)
                rep["steps"].append(rec)
                print(f"BLOCKED  {s['name']}: {why}", flush=True)
                continue
        log = os.path.join(OUTDIR, "logs", target, s["name"] + ".log")
        t0 = time.time()
        try:
            with open(log, "w") as f:
                r = subprocess.run(s["cmd"], shell=True, cwd=ROOT, env=dict(ENV, **s["env"]), stdout=f, stderr=subprocess.STDOUT, timeout=s["timeout"])
            rc = r.returncode
        except subprocess.TimeoutExpired:
            rc = 124
        rec.update(rc=rc, seconds=round(time.time() - t0, 1), result="PASS" if rc == 0 else "FAIL", log=os.path.relpath(log, ROOT), tail=tail(log))
        rep["steps"].append(rec)
        print(f"{rec['result']:8s} {s['name']} ({rec['seconds']}s)" + ("" if rc == 0 else f"  rc={rc} see {rec['log']}"), flush=True)
    st = rep["steps"]
    rep["finished"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    rep["counts"] = {k: sum(x["result"] == k for x in st) for k in ("PASS", "FAIL", "BLOCKED")}
    failed = [x["name"] for x in st if x["result"] == "FAIL" or (require_ext and x["result"] == "BLOCKED")]
    rep["result"] = "FAIL" if failed else ("PASS" if not rep["counts"]["BLOCKED"] else "PASS_WITH_BLOCKED_EXTERNAL")
    rep["failed"] = failed
    # Evidence is committed: keep machine-specific paths out of it.
    text = json.dumps(rep, indent=2).replace(ROOT + "/", "").replace(ROOT, ".").replace(os.path.expanduser("~"), "~")
    open(os.path.join(OUTDIR, f"{target}.json"), "w").write(text)
    print(f"{target}: {rep['result']} {rep['counts']}")
    return 1 if failed else 0


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in TARGETS:
        sys.exit(f"usage: gates.py {{{','.join(TARGETS)}}}")
    sys.exit(run_target(sys.argv[1]))
