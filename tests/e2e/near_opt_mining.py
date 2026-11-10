#!/usr/bin/env python3
"""Optimistic proof-of-work checking on NEAR (zfx-mine-opt), end to end.

Miners use the full production profile (zfx-fp8c-v0.1, 51.6 M multiply-adds
per attempt) through the unchanged zpu-cli; NEAR checks nothing heavy unless
challenged. Checks:

  O1 honest blocks posted cheaply; the watcher validates each one and never
     challenges an honest block
  O2 a cheater who does no matrix work is caught by the watcher with a
     one-row fraud proof; the cheater's bond goes to the watcher
  O3 blocks built on a fraudulent block are reverted with it; their miners
     get their bonds back and no reward
  O4 a challenge against a correct value fails
  O5 after the window, only honest blocks mint; supply == reward x final height

Writes bench/results/e2e_near_opt_mining.json.
"""
import json
import os
import subprocess
import sys
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "deploy"))
from localnet import BIN, ENVV, jrun, sh  # noqa: E402

RUN = os.path.join(ROOT, "deploy", "run", "near-opt")
WASM = os.path.join(ROOT, "bridge", "near", "target", "near", "zfx_mine_opt", "zfx_mine_opt.wasm")
CONTRACT = "opt.deployer.test.near"
HONEST = ["miner1.test.near", "miner2.test.near"]
REWARD = 50 * 10**8
WINDOW = 20
REPORT = {"checks": []}


def check(name, ok, ev):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": ev})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(ev, default=str)[:600], flush=True)
    return bool(ok)


def jsonl(path):
    out = []
    for line in open(path):
        line = line.strip()
        if line.startswith("{"):
            try:
                out.append(json.loads(line))
            except ValueError:
                pass
    return out


def main():
    subprocess.run(["rm", "-rf", RUN])
    os.makedirs(RUN)
    procs = {}
    ok = True
    accts = ["deployer.test.near"] + HONEST + ["cheater.test.near", "watcher.test.near"]
    procs["sb"] = subprocess.Popen([f"{BIN}/zfx-bridge", "near-sandbox", "--dir", RUN, "--accounts", ",".join(accts)], env=ENVV,
                                   stdout=open(f"{RUN}/sb.log", "w"), stderr=subprocess.STDOUT)
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

        def call(signer, method, args, gas=300, deposit="0"):
            return jrun([f"{BIN}/zfx-bridge", "call", "--rpc", rpc, "--signer", signer, "--key-file", keys[signer], "--contract", CONTRACT,
                         "--method", method, "--args", json.dumps(args), "--gas", str(gas), "--deposit", deposit], check=False)

        def near_balance(acct):
            body = {"jsonrpc": "2.0", "id": 1, "method": "query", "params": {"request_type": "view_account", "finality": "final", "account_id": acct}}
            r = json.loads(urllib.request.urlopen(urllib.request.Request(rpc, data=json.dumps(body).encode(), headers={"content-type": "application/json"})).read())
            return int(r["result"]["amount"]) / 1e24

        def st():
            v = view("stats")
            return {k: (int(v[k]) if k in ("final_height", "head_height", "frauds") else v[k]) for k in v}

        init = {"genesis_seed": "zfx-near-opt-e2e", "genesis_target": "001" + "f" * 61, "spacing_secs": 5, "reward": str(REWARD),
                "bond": str(10**24), "window_secs": WINDOW}
        jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", rpc, "--signer", "deployer.test.near", "--key-file", keys["deployer.test.near"], "--contract", CONTRACT,
              "--wasm", WASM, "--init", json.dumps(init), "--deposit-near", "20"])

        watch_cmd = [f"{BIN}/zfx-bridge", "near-opt-watch", "--rpc", rpc, "--signer", "watcher.test.near", "--key-file", keys["watcher.test.near"],
                     "--contract", CONTRACT]
        procs["watch"] = subprocess.Popen(watch_cmd, env=ENVV, stdout=open(f"{RUN}/watch.log", "w"), stderr=subprocess.STDOUT)
        for i, m in enumerate(HONEST):
            procs[f"proxy{i}"] = subprocess.Popen([f"{BIN}/zfx-bridge", "near-mine-proxy", "--optimistic", "--rpc", rpc, "--signer", m, "--key-file", keys[m],
                                                   "--contract", CONTRACT, "--listen", f"127.0.0.1:{9561 + i}"], env=ENVV,
                                                  stdout=open(f"{RUN}/proxy{i}.log", "w"), stderr=subprocess.STDOUT)
        time.sleep(1)

        # O1: honest mining on the production profile
        for i in range(2):
            procs[f"miner{i}"] = subprocess.Popen([f"{BIN}/zpu-cli", "mine", "--node", f"http://127.0.0.1:{9561 + i}", "--threads", "8"], env=ENVV,
                                                  stdout=open(f"{RUN}/miner{i}.log", "w"), stderr=subprocess.STDOUT)
        t0 = time.time()
        while time.time() - t0 < 600 and st()["head_height"] < 12:
            time.sleep(1)
        for i in range(2):
            procs.pop(f"miner{i}").terminate()
        time.sleep(4)
        mined = [e for i in range(2) for e in jsonl(f"{RUN}/proxy{i}.log") if e.get("event") == "mined"]
        gas = sorted(e["gas_burnt_tgas"] for e in mined)
        REPORT["submit_gas_tgas"] = {"n": len(gas), "min": gas[0], "median": gas[len(gas) // 2], "max": gas[-1]}
        wl = jsonl(f"{RUN}/watch.log")
        s1 = st()
        ok &= check("O1 honest v0.1 blocks posted cheaply; the watcher re-checks each one and challenges none",
                    s1["head_height"] >= 12 and len([e for e in wl if e["event"] == "valid"]) >= s1["head_height"] - s1["final_height"] - 1
                    and not [e for e in wl if e["event"] == "challenge"],
                    {"head": s1["head_height"], "submit_gas_tgas": REPORT["submit_gas_tgas"], "watcher_valid": len([e for e in wl if e["event"] == "valid"]),
                     "profile": s1["profile"], "macs_per_attempt": s1["macs_per_attempt"]})

        # O2: cheater caught live
        cb, wb = near_balance("cheater.test.near"), near_balance("watcher.test.near")
        h_before = st()["head_height"]
        cheat = jrun([f"{BIN}/zfx-bridge", "near-opt-cheat", "--rpc", rpc, "--signer", "cheater.test.near", "--key-file", keys["cheater.test.near"],
                      "--contract", CONTRACT])
        t0 = time.time()
        ch = []
        while time.time() - t0 < 60 and not (st()["frauds"] >= 1 and ch):
            time.sleep(1)
            ch = [e for e in jsonl(f"{RUN}/watch.log") if e["event"] == "challenge"]
        s2 = st()
        ca, wa = near_balance("cheater.test.near"), near_balance("watcher.test.near")
        ok &= check("O2 a cheater who skipped all matrix work is caught by a one-row fraud proof; the bond moves to the watcher",
                    cheat.get("ok") and s2["frauds"] == 1 and s2["head_height"] == h_before and ch and ch[0]["ok"] and ca < cb - 0.9 and wa > wb + 0.9,
                    {"cheat_grind_iterations": cheat.get("grind_iterations"), "cheat_submit_gas_tgas": cheat.get("gas_burnt_tgas"),
                     "challenge": ch[0] if ch else None, "cheater_near": [round(cb, 3), round(ca, 3)], "watcher_near": [round(wb, 3), round(wa, 3)]})

        # O3: an honest block built on a fraudulent one is reverted with it
        procs.pop("watch").terminate()
        jrun([f"{BIN}/zfx-bridge", "near-opt-cheat", "--rpc", rpc, "--signer", "cheater.test.near", "--key-file", keys["cheater.test.near"],
              "--contract", CONTRACT])
        h_fraud = st()["head_height"]
        m1b = near_balance(HONEST[0])
        subprocess.run([f"{BIN}/zpu-cli", "mine", "--node", "http://127.0.0.1:9561", "--threads", "8", "--max-blocks", "1"], env=ENVV,
                       stdout=open(f"{RUN}/miner_o3.log", "w"), stderr=subprocess.STDOUT, timeout=300)
        h_child = st()["head_height"]
        once = subprocess.run(watch_cmd + ["--once"], env=ENVV, capture_output=True, text=True)
        ev = [json.loads(line) for line in once.stdout.splitlines() if line.startswith("{")]
        fr = [e for e in ev if e["event"] == "challenge"]
        s3 = st()
        m1a = near_balance(HONEST[0])
        ok &= check("O3 an honest block mined on top of a fraudulent block is reverted with it; its miner's bond is refunded",
                    h_child == h_fraud + 1 and fr and fr[0]["ok"] and s3["head_height"] == h_fraud - 1
                    and len((fr[0]["result"] or {}).get("reverted_descendants", [])) == 1 and m1a > m1b - 0.1,
                    {"fraud_height": h_fraud, "child_height": h_child, "head_after": s3["head_height"], "challenge": fr[0] if fr else None,
                     "honest_miner_near": [round(m1b, 3), round(m1a, 3)]})

        # O6: deep frauds (honest early layers, fake later ones) proven on-chain
        deep = []
        for honest_layers, want_layer in ((2, 2), (4, 4)):
            jrun([f"{BIN}/zfx-bridge", "near-opt-cheat", "--rpc", rpc, "--signer", "cheater.test.near", "--key-file", keys["cheater.test.near"],
                  "--contract", CONTRACT, "--honest-layers", str(honest_layers)])
            o = subprocess.run(watch_cmd + ["--once"], env=ENVV, capture_output=True, text=True)
            c = [json.loads(line) for line in o.stdout.splitlines() if line.startswith("{") and '"challenge"' in line]
            deep.append({"honest_layers": honest_layers, "expected_layer": want_layer, "challenge_layer": c[0]["layer"] if c else None,
                         "ok": bool(c and c[0]["ok"]), "tx_gas_tgas": c[0]["gas_burnt_tgas"] if c else None,
                         "check_tgas": (c[0].get("result") or {}).get("check_tgas") if c else None})
        ok &= check("O6 deep frauds proven with one row: a fake hidden layer (4096-weight row regenerated on-chain) and fake outputs",
                    all(d["ok"] and d["challenge_layer"] == d["expected_layer"] for d in deep), {"cases": deep})

        # O4: a correct value cannot be challenged
        hb = s3["head_height"]
        bogus = call("watcher.test.near", "challenge", {"height": str(hb), "layer": 2, "row": 17})
        ok &= check("O4 a challenge against a correct value fails", not bogus.get("success") and "no fraud" in str(bogus.get("failure")),
                    {"height": hb, "failure": (bogus.get("failure") or "")[-70:], "check_gas_tgas": bogus.get("gas_burnt_tgas")})

        # O5: finalize
        time.sleep(WINDOW + 3)
        fin = None
        for _ in range(5):
            fin = call("deployer.test.near", "finalize", {"max": 50})
            if st()["final_height"] == st()["head_height"]:
                break
        s5 = st()
        bal = {m: int(view("ft_balance_of", {"account_id": m})) for m in HONEST + ["cheater.test.near"]}
        ok &= check("O5 after the window only honest blocks mint: supply == reward x final height, the cheater holds no ZFX",
                    s5["final_height"] == s5["head_height"] and int(s5["total_supply"]) == int(s5["expected_supply"]) == sum(bal.values())
                    and bal["cheater.test.near"] == 0,
                    {"final_height": s5["final_height"], "supply": s5["total_supply"], "balances": bal, "finalize_gas_tgas": fin and fin.get("gas_burnt_tgas"),
                     "frauds": s5["frauds"]})
    except Exception as e:
        ok = False
        check("optimistic mining e2e aborted", False, {"exception": repr(e)})
    finally:
        for p in procs.values():
            p.terminate()
        for p in procs.values():
            try:
                p.wait(20)
            except Exception:
                p.kill()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    json.dump(REPORT, open(os.path.join(ROOT, "bench", "results", "e2e_near_opt_mining.json"), "w"), indent=2, default=str)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed")
    sys.exit(0 if ok and REPORT["passed"] == REPORT["total"] else 1)


if __name__ == "__main__":
    main()
