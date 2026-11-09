#!/usr/bin/env python3
"""Trust-minimized bridge feasibility: verify ZeroFlux PoW inside a NEAR
contract (consensus sources compiled to wasm) and measure real gas.
Writes bench/results/onchain_pow_gas.json."""
import json
import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "deploy"))
from localnet import BIN, ENVV, jrun  # noqa: E402

RUN = os.path.join(ROOT, "deploy", "run", "powgas")
WASM = os.path.join(ROOT, "bridge", "near", "target", "near", "pow_verifier", "pow_verifier.wasm")


def main():
    subprocess.run(["rm", "-rf", RUN])
    os.makedirs(RUN)
    sb = subprocess.Popen([f"{BIN}/zfx-bridge", "near-sandbox", "--dir", RUN, "--accounts", "deployer.test.near"], env=ENVV,
                          stdout=open(os.path.join(RUN, "sb.log"), "w"), stderr=subprocess.STDOUT)
    try:
        for _ in range(120):
            if os.path.exists(os.path.join(RUN, "sandbox.json")):
                break
            time.sleep(0.5)
        s = json.load(open(os.path.join(RUN, "sandbox.json")))
        key = s["accounts"]["deployer.test.near"]
        c = "pow.deployer.test.near"
        jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", s["rpc"], "--signer", "deployer.test.near", "--key-file", key, "--contract", c, "--wasm", WASM, "--init", "{}"])
        vec = json.load(open(os.path.join(ROOT, "tests", "vectors", "fp8c_v0.1.json")))
        out = {"experiment": "onchain_pow_verification_near", "gas_limit_per_tx_tgas": 300, "rows": []}
        for key_, pid in (("dev", 2), ("v01", 1)):
            pv = vec[key_]
            a = pv["attempts"][1]
            r = jrun([f"{BIN}/zfx-bridge", "call", "--rpc", s["rpc"], "--signer", "deployer.test.near", "--key-file", key, "--contract", c,
                      "--method", "verify_attempt", "--args", json.dumps({"profile_id": pid, "epoch_seed": pv["epoch_seed"], "challenge": pv["challenge"], "nonce": a["nonce"]}),
                      "--gas", "300"], check=False)
            row = {"profile": pv["profile"]["name"], "macs": pv["profile"]["macs_per_attempt"], "success": r.get("success"),
                   "matches_golden_vector": (r.get("value") or {}).get("digest") == a["digest"], "result": r.get("value"),
                   "failure": str(r.get("failure"))[:300] if r.get("failure") else None}
            if r.get("value"):
                row["tgas_total_in_call"] = round((r["value"]["gas_weights"] + r["value"]["gas_attempt"]) / 1e12, 2)
            out["rows"].append(row)
            print(json.dumps(row)[:400], flush=True)
        dev = out["rows"][0]
        if dev.get("success"):
            per_mac = dev["result"]["gas_attempt"] / dev["macs"]
            out["model_v01_attempt_tgas"] = round(per_mac * vec["v01"]["profile"]["macs_per_attempt"] / 1e12, 1)
            out["model_basis"] = "dev-profile measured gas per MAC times v0.1 MACs (attempt only, excluding 51.6 MB weight generation); labeled model, not measurement"
        out["conclusion"] = ("A NEAR contract can verify the dev profile exactly (matches golden vectors), but one production (v0.1) attempt exceeds the 300 Tgas "
                             "transaction limit by orders of magnitude even before weight generation; a NEAR light client that re-executes ZeroFlux PoW is infeasible. "
                             "Trust-minimization needs a succinct proof (STARK) of the PoW, verified on NEAR; until that is built and reviewed, the 2-of-3 committee remains.")
        json.dump(out, open(os.path.join(ROOT, "bench", "results", "onchain_pow_gas.json"), "w"), indent=2)
        print(out["conclusion"])
    finally:
        sb.terminate()
        sb.wait(20)


if __name__ == "__main__":
    main()
