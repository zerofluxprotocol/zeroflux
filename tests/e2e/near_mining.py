#!/usr/bin/env python3
"""Option C end to end: ZFX as a NEAR-native token minted by proof of work.

NEAR sandbox + the zfx-mine contract + two independent miners, each running
the unchanged zpu-cli against its own `zfx-bridge near-mine-proxy`. Checks:

  C1 epoch weights prepared on-chain, each layer within one transaction
  C2 blocks mined through the contract by two miners; gas per submit
  C3 supply == reward x height, and equals the sum of miner balances
  C4 a winning nonce is worthless to another account (front-running)
  C5 stale height and invalid nonce rejected
  C6 epoch boundary crossed: new weights prepared, previous epoch freed
  C7 difficulty retargets on-chain after the LWMA window
  C8 ZFX transfers as a standard NEP-141 token

Writes bench/results/e2e_near_mining.json.
"""
import json
import os
import re
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "deploy"))
from localnet import BIN, ENVV, jrun, sh  # noqa: E402

RUN = os.path.join(ROOT, "deploy", "run", "near-mining")
WASM = os.path.join(ROOT, "bridge", "near", "target", "near", "zfx_mine", "zfx_mine.wasm")
CONTRACT = "zfx.deployer.test.near"
MINERS = ["miner1.test.near", "miner2.test.near"]
TARGET_HEIGHT = int(os.environ.get("ZFX_NEAR_BLOCKS", "40"))
REPORT = {"checks": []}


def check(name, ok, ev):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": ev})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(ev, default=str)[:500], flush=True)
    return bool(ok)


def main():
    subprocess.run(["rm", "-rf", RUN])
    os.makedirs(RUN)
    procs = []
    ok = True
    sb = subprocess.Popen([f"{BIN}/zfx-bridge", "near-sandbox", "--dir", RUN, "--accounts", ",".join(["deployer.test.near"] + MINERS)], env=ENVV,
                          stdout=open(f"{RUN}/sb.log", "w"), stderr=subprocess.STDOUT)
    procs.append(sb)
    try:
        for _ in range(180):
            if os.path.exists(f"{RUN}/sandbox.json"):
                break
            time.sleep(0.5)
        s = json.load(open(f"{RUN}/sandbox.json"))
        rpc, keys = s["rpc"], s["accounts"]

        def view(method, args=None):
            return json.loads(sh([f"{BIN}/zfx-bridge", "view", "--rpc", rpc, "--contract", CONTRACT, "--method", method, "--args", json.dumps(args or {}),
                                  "--finality", "optimistic"]).stdout.strip().splitlines()[-1])

        def call(signer, method, args, gas=300):
            return jrun([f"{BIN}/zfx-bridge", "call", "--rpc", rpc, "--signer", signer, "--key-file", keys[signer], "--contract", CONTRACT,
                         "--method", method, "--args", json.dumps(args), "--gas", str(gas)], check=False)

        genesis_target = "0000" + "f" * 60
        init = {"genesis_seed": "zfx-near-mining-e2e", "genesis_target": genesis_target, "spacing_secs": 6, "reward": str(50 * 10**8), "metadata": None}
        d = jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", rpc, "--signer", "deployer.test.near", "--key-file", keys["deployer.test.near"], "--contract", CONTRACT,
                  "--wasm", WASM, "--init", json.dumps(init), "--deposit-near", "300"])
        REPORT["deploy_tx"] = d.get("tx_hash")

        # C1: epoch weights, one layer per transaction
        prep = []
        for layer in range(5):
            r = call("deployer.test.near", "prepare_epoch", {"layer": layer})
            prep.append({"layer": layer, "ok": r.get("success"), "gas_tgas": r.get("gas_burnt_tgas"), "failure": (r.get("failure") or "")[:160]})
        w = view("work", {"miner": MINERS[0]})
        ok &= check("C1 epoch weights prepared on-chain, one layer per transaction (5 layers, 3.47 M weights)",
                    all(p["ok"] for p in prep) and w["missing_layers"] == [], {"layers": prep, "missing_after": w["missing_layers"]})

        # C4/C5 before mining starts: front-running, stale height, invalid nonce
        found = jrun([f"{BIN}/zfx-bridge", "pow-search", "--profile", "3", "--seed", w["epoch_seed"], "--challenge", w["challenge"], "--target", w["target"]])
        stolen = call(MINERS[1], "submit", {"height": w["height"], "nonce": found["nonce"]})
        bogus = call(MINERS[0], "submit", {"height": w["height"], "nonce": str((int(found["nonce"]) + 1) * 7919)})
        own = call(MINERS[0], "submit", {"height": w["height"], "nonce": found["nonce"]})
        replay = call(MINERS[0], "submit", {"height": w["height"], "nonce": found["nonce"]})
        ok &= check("C4 a winning nonce is worthless to another account: the challenge commits to the submitter",
                    not stolen.get("success") and "does not meet the target" in str(stolen.get("failure")) and own.get("success"),
                    {"winning_nonce": found["nonce"], "stolen_by_miner2": (stolen.get("failure") or "")[-120:], "miner1_tx": own.get("tx_hash"),
                     "miner1_result": own.get("value"), "miner1_submit_gas_tgas": own.get("gas_burnt_tgas")})
        ok &= check("C5 invalid nonce and stale height rejected",
                    not bogus.get("success") and "does not meet the target" in str(bogus.get("failure"))
                    and not replay.get("success") and "stale height" in str(replay.get("failure")),
                    {"bogus": (bogus.get("failure") or "")[-100:], "replay": (replay.get("failure") or "")[-100:]})

        # C2: two independent miners through the unchanged zpu-cli
        logs = []
        for i, m in enumerate(MINERS):
            port = 9551 + i
            log = f"{RUN}/proxy{i}.log"
            logs.append(log)
            procs.append(subprocess.Popen([f"{BIN}/zfx-bridge", "near-mine-proxy", "--rpc", rpc, "--signer", m, "--key-file", keys[m], "--contract", CONTRACT,
                                           "--listen", f"127.0.0.1:{port}"], env=ENVV, stdout=open(log, "w"), stderr=subprocess.STDOUT))
        time.sleep(1)
        t0 = time.time()
        for i, m in enumerate(MINERS):
            procs.append(subprocess.Popen([f"{BIN}/zpu-cli", "mine", "--node", f"http://127.0.0.1:{9551 + i}", "--threads", "8"], env=ENVV,
                                          stdout=open(f"{RUN}/miner{i}.log", "w"), stderr=subprocess.STDOUT))
        while time.time() - t0 < 900:
            if int(view("stats")["height"]) >= TARGET_HEIGHT:
                break
            time.sleep(2)
        elapsed = time.time() - t0
        for p in procs[3:]:
            p.terminate()
        time.sleep(2)
        st = view("stats")
        mined = []
        prep_events = []
        for log in logs:
            for line in open(log):
                line = line.strip()
                if line.startswith("{") and '"mined"' in line:
                    mined.append(json.loads(line))
                m = re.search(r"prepare_epoch layer (\d+): Ok\(\((true|false), ([0-9.]+)\)\)", line)
                if m:
                    prep_events.append({"layer": int(m.group(1)), "ok": m.group(2) == "true", "gas_tgas": float(m.group(3))})
        gas = sorted(x["gas_burnt_tgas"] for x in mined)
        bal = {m: int(view("ft_balance_of", {"account_id": m})) for m in MINERS}
        blocks_by = {m: bal[m] // (50 * 10**8) for m in MINERS}
        REPORT["submit_gas_tgas"] = {"min": gas[0] if gas else None, "median": gas[len(gas) // 2] if gas else None, "max": gas[-1] if gas else None, "n": len(gas)}
        REPORT["prepare_epoch_gas_tgas"] = prep_events
        ok &= check("C2 blocks mined through the contract by two independent miners (unchanged zpu-cli via near-mine-proxy)",
                    int(st["height"]) >= TARGET_HEIGHT and all(v > 0 for v in blocks_by.values()),
                    {"height": st["height"], "blocks_by_miner": blocks_by, "seconds": round(elapsed, 1), "submit_gas_tgas": REPORT["submit_gas_tgas"]})
        h = int(st["height"])
        ok &= check("C3 supply == reward x height == sum of miner balances",
                    int(st["total_supply"]) == int(st["expected_supply"]) == sum(bal.values()) == h * 50 * 10**8,
                    {"height": h, "total_supply": st["total_supply"], "balances": bal})
        ok &= check("C6 epoch boundary crossed: next epoch's weights prepared on-chain by the miners' proxies",
                    h > 32 and len([e for e in prep_events if e["ok"]]) >= 5, {"height": h, "prepare_events": prep_events})
        ok &= check("C7 difficulty retargets on-chain after the LWMA window", st["target"] != genesis_target,
                    {"genesis_target": genesis_target, "target_now": st["target"]})
        tr = jrun([f"{BIN}/zfx-bridge", "call", "--rpc", rpc, "--signer", MINERS[0], "--key-file", keys[MINERS[0]], "--contract", CONTRACT, "--method",
                   "ft_transfer", "--args", json.dumps({"receiver_id": MINERS[1], "amount": str(10**8)}), "--deposit", "1"], check=False)
        bal2 = {m: int(view("ft_balance_of", {"account_id": m})) for m in MINERS}
        ok &= check("C8 ZFX moves as a standard NEP-141 token (ft_transfer)", tr.get("success") and bal2[MINERS[1]] == bal[MINERS[1]] + 10**8,
                    {"tx": tr.get("tx_hash"), "before": bal, "after": bal2})
    except Exception as e:
        ok = False
        check("near mining e2e aborted", False, {"exception": repr(e)})
    finally:
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(20)
            except Exception:
                p.kill()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    json.dump(REPORT, open(os.path.join(ROOT, "bench", "results", "e2e_near_mining.json"), "w"), indent=2, default=str)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed")
    sys.exit(0 if ok and REPORT["passed"] == REPORT["total"] else 1)


if __name__ == "__main__":
    main()
