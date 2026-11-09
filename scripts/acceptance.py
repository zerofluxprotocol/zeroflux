#!/usr/bin/env python3
"""make acceptance: run every gate target, then write ACCEPTANCE_REPORT.json
and ACCEPTANCE_REPORT.md mapping the evidence onto Gates 0-8 of the Track A+B
specification.

  python3 scripts/acceptance.py            run all targets, then report
  ZFX_ACCEPT_REUSE=1 python3 scripts/...   report from existing evidence only

Gate statuses: PASS, FAIL, or PASS_WITH_BLOCKED (everything runnable here
passed; named external items are blocked). A gate is never PASS because a
check was skipped. Exit status is 1 if any gate FAILs.
"""
import glob
import json
import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RES = os.path.join(ROOT, "bench", "results")
GATES = os.path.join(RES, "gates")
PY = os.path.join(ROOT, ".venv", "bin", "python")
TARGETS = ["check", "e2e-native", "e2e-bridge", "e2e-intents", "security-test", "gpu-test", "testnet-smoke"]
MAINNET_READY = False


def load(path, default=None):
    try:
        return json.load(open(path))
    except (OSError, ValueError):
        return default


def target(name):
    return load(os.path.join(GATES, f"{name}.json"), {"result": "NOT_RUN", "steps": []})


def step(tname, sname):
    for s in target(tname)["steps"]:
        if s["name"] == sname:
            return s
    return {"result": "NOT_RUN"}


def checks(path):
    d = load(os.path.join(RES, path), {})
    return d.get("passed"), d.get("total"), d.get("checks", [])


def gpu_rerun():
    """Newest gpu_rerun_* evidence per vendor (kept separate from the prior run in bench/results/gpu)."""
    out = {}
    for d in sorted(glob.glob(os.path.join(RES, "gpu_rerun_*"))):
        for vendor in ("nvidia", "amd"):
            f = os.path.join(d, vendor, "results", "results.jsonl")
            if not os.path.exists(f):
                continue
            rows = [json.loads(l) for l in open(f) if l.strip().startswith("{")]
            sel = [r for r in rows if r.get("step", "").startswith(("selftest_", "stress_", "py_selftest", "py_stress", "cpu_selftest"))]
            mine = next((r for r in rows if r.get("step") == "gpu_block_submissions"), {})
            bench = {r["step"][len("bench_json_"):]: r["data"] for r in rows if r.get("step", "").startswith("bench_json_") and r.get("data")}
            gpu = next((r["data"]["backend"] for r in rows if r.get("step", "").startswith("bench_json_") and r.get("data")), None)
            out[vendor] = {"dir": os.path.relpath(d, ROOT), "device": gpu, "correctness_steps": len(sel),
                           "correctness_failed": [r["step"] for r in sel if r.get("result") != "PASS"],
                           "block_submissions": {k: mine.get(k) for k in ("result", "blocks_accepted_by_cpu_node", "independent_revalidation_matches", "nodeA_tip")},
                           "bench": {g: {k: b.get(k) for k in ("attempts_per_s", "tera_macs_per_s", "launch_overhead_s", "acceptance")} for g, b in bench.items()}}
    return out


def gate(n, title, status, evidence, blocked=None):
    return {"gate": n, "title": title, "status": status, "evidence": evidence, "blocked_external": blocked or []}


def build():
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    t = {k: target(k) for k in TARGETS}
    ok = lambda *xs: all(x.get("result") == "PASS" for x in xs)  # noqa: E731
    gates = []

    status_md = open(os.path.join(ROOT, "IMPLEMENTATION_STATUS.md")).read() if os.path.exists(os.path.join(ROOT, "IMPLEMENTATION_STATUS.md")) else ""
    gates.append(gate(0, "baseline", "PASS" if "Gate 0 baseline" in status_md and t["check"]["result"] != "NOT_RUN" else "FAIL",
                      {"inventory": "IMPLEMENTATION_STATUS.md (Gate 0 baseline, requirement inventory)", "check_target": t["check"]["result"]}))

    np_, nt, _ = checks("e2e_native.json")
    s1 = step("e2e-native", "native-3-nodes-1000-blocks")
    api = load(os.path.join(RES, "contract_api.json"), {})
    g1 = "PASS" if ok(s1, step("e2e-native", "contract-api-schema-conformance")) and np_ == nt else "FAIL"
    gates.append(gate(1, "native chain", g1, {"e2e_native": f"{np_}/{nt} checks", "contract_api": f"{api.get('passed')}/{api.get('total')} cases",
                                              "note": "e2e uses PoW profile zfx-fp8c-v0.1-dev (H=256) for speed; v0.1 profile mined on GPUs below"}))

    rr = gpu_rerun()
    local = {s: step("check", s)["result"] for s in ("vectors-rust", "vectors-cpp", "vectors-python-numpy", "vectors-mlx-metal", "vectors-torch-mps", "gpu-device-compile")}
    gpu_ok = all(v.get("correctness_steps", 0) > 0 and not v["correctness_failed"] for v in rr.values())
    mined = {k: v["block_submissions"].get("result") for k, v in rr.items()}
    g2_blocked = ["NVIDIA B200/B300 (sm_100), RTX 50 (sm_120) and AMD MI300X (gfx942): code compiles for these targets; no such device was reachable, so no hardware result"]
    if not rr:
        g2_blocked.append("no GPU rerun evidence in bench/results/gpu_rerun_*")
    g2 = "FAIL" if any(v != "PASS" for v in local.values()) or (rr and not gpu_ok) else "PASS_WITH_BLOCKED"
    gates.append(gate(2, "miner", g2, {"local": local, "gpu_rerun_new": rr, "gpu_block_submissions": mined,
                                       "prior_evidence": "bench/results/gpu_summary.json (H100, MI355X, earlier run; not re-measured by this report)"}, g2_blocked))

    bp, bt, bchecks = checks("e2e_bridge.json")
    gp, gt, _ = checks("e2e_bridge_governance.json")
    g3 = "PASS" if ok(step("check", "cargo-test-near-contracts"), step("check", "wasm-build-wzfx"), step("e2e-bridge", "bridge-governance-recovery")) and gp == gt else "FAIL"
    gates.append(gate(3, "bridge contracts", g3, {"wzfx_unit_tests": step("check", "cargo-test-near-contracts")["result"],
                                                  "conformance_replay_access_in_e2e_bridge": [c["check"] for c in bchecks if c["check"].startswith(("B1", "B2"))],
                                                  "governance_upgrade_rotation": f"{gp}/{gt} checks (bench/results/e2e_bridge_governance.json)"}))

    rp, rt, _ = checks("e2e_bridge_reorg.json")
    g4 = "PASS" if ok(step("e2e-bridge", "bridge-100-two-way-fault-injection"), step("e2e-bridge", "bridge-finality-reorg")) and bp == bt and rp == rt else "FAIL"
    gates.append(gate(4, "real two-way bridge", g4, {"two_way_fault_injection": f"{bp}/{bt} checks", "finality_reorg": f"{rp}/{rt} checks"}))

    ip, it, ichecks = checks("e2e_intents.json")
    g5 = "PASS_WITH_BLOCKED" if ok(step("e2e-intents", "intents-local-solver-settlement")) and ip == it else "FAIL"
    gates.append(gate(5, "Intents", g5, {"local": f"{ip}/{it} checks"},
                      ["public NEAR Intents Message Bus requires a Partner JWT (HTTP 403 without one); no live liquidity is solicited"]))

    ts = load(os.path.join(RES, "testnet_smoke.json"), {})
    if ts.get("result") != "PASS" and step("testnet-smoke", "near-testnet-deploy-and-receipts").get("result") == "BLOCKED":
        ts = {"result": "BLOCKED", "reason": step("testnet-smoke", "near-testnet-deploy-and-receipts")["reason"], "prerequisite": "see tests/e2e/testnet_smoke.py"}
    g6 = {"PASS": "PASS", "BLOCKED": "PASS_WITH_BLOCKED"}.get(ts.get("result"), "FAIL")
    gates.append(gate(6, "testnet deployment", g6, {"testnet_smoke": ts.get("result"), "txs": ts.get("txs", [])},
                      [ts["reason"] + ". Prerequisite: " + ts.get("prerequisite", "")] if ts.get("result") == "BLOCKED" else []))

    rc = load(os.path.join(RES, "release_check.json"), {})
    sec = t["security-test"]
    docs = ["ALGORITHM_DECISION", "THREAT_MODEL", "TOKEN_ACCOUNTING", "BRIDGE_TRUST_MODEL", "CONTRACT_API", "INTENTS_INTEGRATION", "TEST_REPORT",
            "HARDWARE", "RELEASE_READINESS", "OPERATOR_RUNBOOK"]
    missing = [d for d in docs if not os.path.exists(os.path.join(ROOT, "docs", f"{d}.md"))]
    g7_ok = sec["result"] == "PASS" and t["check"]["result"] == "PASS" and not missing and (rc.get("reproducible_build") or {}).get("result") == "PASS"
    gates.append(gate(7, "packaging and handoff", "PASS_WITH_BLOCKED" if g7_ok else "FAIL",
                      {"check": t["check"]["result"], "security_test": sec["result"], "reproducible_build": (rc.get("reproducible_build") or {}).get("result"),
                       "sbom": (rc.get("sbom") or {}).get("result"), "licenses": (rc.get("licenses") or {}).get("result"), "docs_missing": missing,
                       "one_command_env": "make localnet", "schemas": "schemas/zfx-node-rpc-v1.json"},
                      ["container images not built: Docker is not installed on this host",
                       "release artifacts unsigned: no owner release key exists",
                       "no live public testnet explorer/API: not authorized and no testnet deployment"]))

    gates.append(gate(8, "truthful readiness", "PASS", {"MAINNET_READY": MAINNET_READY, "tracked_in": "docs/RELEASE_READINESS.md"},
                      ["independent cryptanalysis of the PoW hardness assumption", "external security audit (node, P2P, wZFX, relayers)",
                       "owner decision on the monetary schedule (recommendation in bench/results/tokenomics.json)", "real liquidity for wZFX (none solicited)",
                       "unmeasured GPU models (see Gate 2)"]))

    counts = {k: sum(t[x].get("counts", {}).get(k, 0) for x in TARGETS) for k in ("PASS", "FAIL", "BLOCKED")}
    return {"generated": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "commit": commit, "MAINNET_READY": MAINNET_READY,
            "targets": {k: {"result": t[k]["result"], "counts": t[k].get("counts"), "commit": t[k].get("commit"), "dirty_tree": t[k].get("dirty_tree"),
                            "finished": t[k].get("finished")} for k in TARGETS},
            "step_counts": counts, "gates": gates,
            "steps": {k: [{x: s.get(x) for x in ("name", "result", "cmd", "seconds", "reason", "log")} for s in t[k]["steps"]] for k in TARGETS}}


def markdown(r):
    L = ["# Acceptance report", "", f"Generated {r['generated']} at commit `{r['commit'][:12]}`. **MAINNET_READY = {str(r['MAINNET_READY']).lower()}.**",
         "", "Regenerate: `make acceptance` (runs every target) or `ZFX_ACCEPT_REUSE=1 .venv/bin/python scripts/acceptance.py` (report only).", "",
         "## Gates", "", "| gate | status | evidence | blocked externally |", "|---|---|---|---|"]
    for g in r["gates"]:
        ev = "; ".join(f"{k}: {v if not isinstance(v, (dict, list)) else json.dumps(v, default=str)[:160]}" for k, v in g["evidence"].items())
        L.append(f"| {g['gate']} {g['title']} | {g['status']} | {ev.replace('|', '/')} | {'; '.join(g['blocked_external']) or '-'} |")
    L += ["", "## Targets", "", "| target | result | PASS | FAIL | BLOCKED | commit | finished |", "|---|---|---|---|---|---|---|"]
    for k, v in r["targets"].items():
        c = v.get("counts") or {}
        L.append(f"| `make {k}` | {v['result']} | {c.get('PASS', 0)} | {c.get('FAIL', 0)} | {c.get('BLOCKED', 0)} | `{(v.get('commit') or '')[:10]}`{' (dirty)' if v.get('dirty_tree') else ''} | {v.get('finished') or '-'} |")
    L += ["", "## Steps", ""]
    for k, steps in r["steps"].items():
        L.append(f"### make {k}")
        L.append("")
        L.append("| step | result | seconds | command or reason |")
        L.append("|---|---|---|---|")
        for s in steps:
            what = s.get("reason") or f"`{s.get('cmd')}`"
            L.append(f"| {s['name']} | {s['result']} | {s.get('seconds') or '-'} | {str(what).replace('|', '/')} |")
        L.append("")
    L.append("Full output of every step: `bench/results/gates/logs/<target>/<step>.log`. Raw experiment and e2e results: `bench/results/`.")
    return "\n".join(L) + "\n"


def main():
    if os.environ.get("ZFX_ACCEPT_REUSE") != "1":
        for k in TARGETS:
            subprocess.run([PY if os.path.exists(PY) else sys.executable, os.path.join(ROOT, "scripts", "gates.py"), k], cwd=ROOT)
    r = build()
    home = os.path.expanduser("~")
    clean = lambda t: t.replace(ROOT + "/", "").replace(ROOT, ".").replace(home, "~")  # noqa: E731
    open(os.path.join(ROOT, "ACCEPTANCE_REPORT.json"), "w").write(clean(json.dumps(r, indent=2, default=str)))
    open(os.path.join(ROOT, "ACCEPTANCE_REPORT.md"), "w").write(clean(markdown(r)))
    for g in r["gates"]:
        print(f"Gate {g['gate']} {g['title']}: {g['status']}")
    print(f"MAINNET_READY={str(r['MAINNET_READY']).lower()}")
    failed_targets = [k for k, v in r["targets"].items() if v["result"] in ("FAIL", "NOT_RUN")]
    if failed_targets:
        print(f"targets failed or not run: {failed_targets}")
    sys.exit(1 if failed_targets or any(g["status"] == "FAIL" for g in r["gates"]) else 0)


if __name__ == "__main__":
    main()
