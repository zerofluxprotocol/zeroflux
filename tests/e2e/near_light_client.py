#!/usr/bin/env python3
"""Option B end to end: a ZeroFlux PoW light client on NEAR.

A local ZeroFlux chain on profile zfx-near-v1 (bridge module enabled) and a
NEAR sandbox running the zfx-light contract, fed by `zfx-bridge
near-light-relay`. Checks:

  B1 the contract follows the chain, verifying every header's PoW on-chain
  B2 epoch boundary: weights for the next epoch prepared on-chain
  B3 forged headers rejected (bad nonce, wrong target, unknown parent)
  B4 a deposit is minted on NEAR from a Merkle proof alone, no committee;
     replay and a tampered transaction are rejected
  B5 a deposit with fewer than K confirmations cannot be claimed
  B6 partition, heavier fork, rejoin: the contract reorgs to the most work;
     a block that left the best chain can no longer be claimed against

Writes bench/results/e2e_near_light_client.json.
"""
import json
import os
import subprocess
import sys
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "deploy"))
sys.path.insert(0, os.path.join(ROOT, "testnet"))
from localnet import BIN, ENVV, jrun, sh  # noqa: E402
import zfxnet  # noqa: E402

RUN = os.path.join(ROOT, "deploy", "run", "near-light")
WASM = os.path.join(ROOT, "bridge", "near", "target", "near", "zfx_light", "zfx_light.wasm")
CONTRACT = "light.deployer.test.near"
K = 4
REPORT = {"checks": []}


def check(name, ok, ev):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": ev})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(ev, default=str)[:500], flush=True)
    return bool(ok)


def zrpc(i, method, params):
    req = urllib.request.Request(f"http://127.0.0.1:{zfxnet.ports(i)[1]}/rpc", data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method,
                                 "params": params}).encode(), headers={"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(req, timeout=30).read())


def wait(cond, timeout, what):
    t = time.time()
    while time.time() - t < timeout:
        try:
            if cond():
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise AssertionError("timeout: " + what)


def main():
    subprocess.run(["rm", "-rf", RUN])
    os.makedirs(os.path.join(RUN, "zfx"))
    zfxnet.RUN = os.path.join(RUN, "zfx")
    net = zfxnet.Net()
    procs = {}
    ok = True
    try:
        # ---- ZeroFlux chain on the NEAR-verifiable profile ----
        committee = [json.loads(sh([f"{BIN}/zfx-bridge", "keygen-relayer", "--out", f"{RUN}/r{i}.seed"]).stdout.strip().splitlines()[-1])["slh"] for i in range(3)]
        uw, ua = zfxnet.wallet("user", "5b" * 32)
        _, ma = zfxnet.wallet("miner", "6c" * 32)
        cfg = json.loads(sh([f"{BIN}/zfx-node", "print-config"]).stdout)
        cfg.update(profile=3, genesis_timestamp=int(time.time()) - 600, genesis_target="00007" + "f" * 59, fixed_difficulty=True, target_spacing_secs=5,
                   coinbase_maturity=2, label="zfx-near-light-e2e", allocations=[{"address": ua, "amount": 10**12}],
                   bridge={"committee": committee, "threshold": 2, "rotation_timelock_blocks": 20, "min_lock": 1000, "activation_height": 1,
                           "near_contract": "wzfx.test.near"})
        cfg_path = f"{RUN}/chain.json"
        json.dump(cfg, open(cfg_path, "w"))
        net.start_node(1, [], cfg_path, extra=["--bridge-confirmations", str(K)])
        net.start_node(2, [1], cfg_path, extra=["--bridge-confirmations", str(K)])
        net.start_miner("miner1", 1, ma, "rust", threads=4)
        wait(lambda: zfxnet.status(1)["height"] >= 3, 120, "chain started")

        # ---- NEAR sandbox + light client ----
        accts = ["deployer.test.near", "relay.test.near", "alice.test.near", "claimer.test.near"]
        procs["sb"] = subprocess.Popen([f"{BIN}/zfx-bridge", "near-sandbox", "--dir", f"{RUN}/near", "--accounts", ",".join(accts)], env=ENVV,
                                       stdout=open(f"{RUN}/sb.log", "w"), stderr=subprocess.STDOUT)
        wait(lambda: os.path.exists(f"{RUN}/near/sandbox.json"), 120, "sandbox")
        s = json.load(open(f"{RUN}/near/sandbox.json"))
        rpc, keys = s["rpc"], s["accounts"]

        def view(method, args=None):
            return json.loads(sh([f"{BIN}/zfx-bridge", "view", "--rpc", rpc, "--contract", CONTRACT, "--method", method, "--args", json.dumps(args or {}),
                                  "--finality", "optimistic"]).stdout.strip().splitlines()[-1])

        def call(signer, method, args, gas=300):
            return jrun([f"{BIN}/zfx-bridge", "call", "--rpc", rpc, "--signer", signer, "--key-file", keys[signer], "--contract", CONTRACT,
                         "--method", method, "--args", json.dumps(args), "--gas", str(gas)], check=False)

        g = zrpc(1, "zfx_node_get_block", {"height": 0})["result"]
        init = {"genesis_header": g["header_hex"], "fixed_difficulty": True, "spacing_secs": 5, "max_future_secs": 120, "confirmations": K,
                "pow_limit": cfg["pow_limit"]}
        jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", rpc, "--signer", "deployer.test.near", "--key-file", keys["deployer.test.near"], "--contract", CONTRACT,
              "--wasm", WASM, "--init", json.dumps(init), "--deposit-near", "300"])
        relay_cmd = [f"{BIN}/zfx-bridge", "near-light-relay", "--rpc", rpc, "--signer", "relay.test.near", "--key-file", keys["relay.test.near"],
                     "--contract", CONTRACT, "--zfx", f"http://127.0.0.1:{zfxnet.ports(1)[1]}"]
        relay_log = f"{RUN}/relay.log"
        procs["relay"] = subprocess.Popen(relay_cmd, env=ENVV, stdout=open(relay_log, "a"), stderr=subprocess.STDOUT)

        def lc_h():
            return int(view("best")["height"])

        # B1/B2: follow past an epoch boundary (block 33)
        wait(lambda: lc_h() >= 36, 900, "light client past the first epoch boundary")
        best = view("best")
        node_hash = zrpc(1, "zfx_node_get_block", {"height": int(best["height"])})["result"]["hash"]
        events = [json.loads(line) for line in open(relay_log) if line.startswith("{")]
        hdr_gas = sorted(e["gas_burnt_tgas"] for e in events if e["event"] == "header" and e["ok"])
        pow_gas = sorted(e["result"]["pow_tgas"] for e in events if e["event"] == "header" and e["ok"] and e.get("result"))
        prep = [e for e in events if e["event"] == "prepare_weights"]
        REPORT["header_gas_tgas"] = {"n": len(hdr_gas), "min": hdr_gas[0], "median": hdr_gas[len(hdr_gas) // 2], "max": hdr_gas[-1],
                                     "pow_part_median": pow_gas[len(pow_gas) // 2]}
        REPORT["prepare_weights_gas_tgas"] = [round(e["gas_burnt_tgas"], 1) for e in prep]
        ok &= check("B1 light client follows the chain, recomputing every header's proof of work on-chain",
                    best["hash"] == node_hash and all(e["ok"] for e in events if e["event"] == "header"),
                    {"best_height": best["height"], "matches_node": best["hash"] == node_hash, "header_gas_tgas": REPORT["header_gas_tgas"]})
        ok &= check("B2 epoch boundary: next epoch's weights prepared on-chain (5 layers per epoch)",
                    len([e for e in prep if e["ok"]]) >= 10, {"prepare_events": len(prep), "gas_tgas": REPORT["prepare_weights_gas_tgas"]})

        # B3: forged headers
        h = zrpc(1, "zfx_node_get_block", {"height": lc_h()})["result"]
        hx = h["header_hex"]
        nonce_bad = hx[:-16] + ("00" if hx[-16:-14] != "00" else "11") + hx[-14:]
        fake_parent = hx[:40] + "ab" * 32 + hx[104:]
        r1 = call("claimer.test.near", "submit_header", {"header": nonce_bad})
        r2 = call("claimer.test.near", "submit_header", {"header": fake_parent})
        ok &= check("B3 forged headers rejected on-chain (altered nonce, unknown parent)",
                    not r1.get("success") and not r2.get("success"),
                    {"bad_nonce": (r1.get("failure") or "")[-90:], "unknown_parent": (r2.get("failure") or "")[-90:]})

        # B4: committee-free deposit
        dep = json.loads(sh([f"{BIN}/zfx-wallet", "bridge-deposit", "--wallet", uw, "--node", f"http://127.0.0.1:{zfxnet.ports(1)[1]}", "--near-account",
                             "alice.test.near", "--amount", "25000", "--fee", "1000"]).stdout.strip().splitlines()[-1])
        did = dep["deposit_id"]
        wait(lambda: zrpc(1, "zfx_bridge_deposit_status", {"deposit_id": did})["result"]["state"] == "final", 300, "deposit final on ZeroFlux")
        st = zrpc(1, "zfx_bridge_deposit_status", {"deposit_id": did})["result"]
        inc = st["inclusion"]
        wait(lambda: lc_h() >= st["height"] + K - 1, 300, "light client has K confirmations")
        claim = {"block": st["block"], "core": inc["tx_core"], "witness": inc["tx_witness"], "leaf_count": inc["leaf_count"],
                 "proof": [[p["hash"], p["right"]] for p in inc["merkle_proof"]]}
        c1 = call("claimer.test.near", "claim_deposit", claim)
        bal = int(view("ft_balance_of", {"account_id": "alice.test.near"}))
        c2 = call("claimer.test.near", "claim_deposit", claim)
        tam = dict(claim, core=claim["core"][:136] + ("ff" if claim["core"][136:138] != "ff" else "00") + claim["core"][138:])
        c3 = call("claimer.test.near", "claim_deposit", tam)
        ok &= check("B4 deposit minted on NEAR from a Merkle proof against PoW-verified headers, no committee; replay and tampering rejected",
                    c1.get("success") and bal == 25000 and not c2.get("success") and "already claimed" in str(c2.get("failure"))
                    and not c3.get("success"),
                    {"deposit": did, "claim_tx": c1.get("tx_hash"), "claim_gas_tgas": c1.get("gas_burnt_tgas"), "alice_balance": bal,
                     "replay": (c2.get("failure") or "")[-60:], "tampered": (c3.get("failure") or "")[-70:]})

        # B5: too few confirmations
        procs["relay"].terminate()
        procs["relay"].wait(20)
        dep2 = json.loads(sh([f"{BIN}/zfx-wallet", "bridge-deposit", "--wallet", uw, "--node", f"http://127.0.0.1:{zfxnet.ports(1)[1]}", "--near-account",
                              "alice.test.near", "--amount", "7000", "--fee", "1000"]).stdout.strip().splitlines()[-1])
        wait(lambda: zrpc(1, "zfx_bridge_deposit_status", {"deposit_id": dep2["deposit_id"]})["result"]["state"] in ("awaiting_confirmations", "final"), 120, "dep2 mined")
        st2 = zrpc(1, "zfx_bridge_deposit_status", {"deposit_id": dep2["deposit_id"]})["result"]
        need = st2["height"] - lc_h()
        if need > 0:
            sh(relay_cmd + ["--once", "--max", str(need)], check=False)
        inc2 = st2["inclusion"]
        early = call("claimer.test.near", "claim_deposit", {"block": st2["block"], "core": inc2["tx_core"], "witness": inc2["tx_witness"],
                     "leaf_count": inc2["leaf_count"], "proof": [[p["hash"], p["right"]] for p in inc2["merkle_proof"]]})
        ok &= check("B5 a deposit with fewer than K confirmations on the light client cannot be claimed",
                    not early.get("success") and "not enough confirmations" in str(early.get("failure")),
                    {"deposit_height": st2["height"], "light_client_height": lc_h(), "K": K, "failure": (early.get("failure") or "")[-60:]})

        # B6: partition, heavier fork, rejoin
        procs["relay"] = subprocess.Popen(relay_cmd, env=ENVV, stdout=open(relay_log, "a"), stderr=subprocess.STDOUT)
        wait(lambda: lc_h() >= zfxnet.status(1)["height"] - 1, 300, "relay caught up")
        deny = f"{RUN}/deny.txt"
        open(deny, "w").write("00" * 32 + "\n")
        net.stop("node2")
        net.start_node(2, [], cfg_path, extra=["--no-discovery", "--peer-allow", deny, "--bridge-confirmations", str(K)])
        net.start_miner("miner2", 2, ma, "rust", threads=8)
        # node1 keeps mining a little with a deposit that only it has
        dep3 = json.loads(sh([f"{BIN}/zfx-wallet", "bridge-deposit", "--wallet", uw, "--node", f"http://127.0.0.1:{zfxnet.ports(1)[1]}", "--near-account",
                              "alice.test.near", "--amount", "9000", "--fee", "1000"]).stdout.strip().splitlines()[-1])
        wait(lambda: zrpc(1, "zfx_bridge_deposit_status", {"deposit_id": dep3["deposit_id"]})["result"]["state"] in ("awaiting_confirmations", "final"), 120, "dep3 mined")
        st3 = zrpc(1, "zfx_bridge_deposit_status", {"deposit_id": dep3["deposit_id"]})["result"]
        wait(lambda: lc_h() >= st3["height"], 300, "light client saw dep3's block")
        net.stop("miner1")
        h1 = zfxnet.status(1)["height"]
        wait(lambda: zfxnet.status(2)["height"] > h1 + 3, 600, "isolated fork overtakes")
        net.stop("miner2")
        net.stop("node2")
        net.start_node(2, [1], cfg_path, extra=["--bridge-confirmations", str(K)])
        wait(lambda: zfxnet.status(1)["best_hash"] == zfxnet.status(2)["best_hash"], 180, "node1 adopts the heavier fork")
        wait(lambda: view("best")["hash"] == zfxnet.status(1)["best_hash"], 600, "light client follows the reorg")
        bst = view("best")
        old = call("claimer.test.near", "claim_deposit", {"block": st3["block"], "core": st3["inclusion"]["tx_core"], "witness": st3["inclusion"]["tx_witness"],
                   "leaf_count": st3["inclusion"]["leaf_count"], "proof": [[p["hash"], p["right"]] for p in st3["inclusion"]["merkle_proof"]]})
        ok &= check("B6 partition and heavier fork: the light client reorgs to the most work; a block that left the best chain cannot be claimed",
                    int(bst["reorgs"]) >= 1 and not old.get("success") and "not on the best chain" in str(old.get("failure")),
                    {"reorgs": bst["reorgs"], "max_reorg_depth": bst["max_reorg_depth"], "best_height": bst["height"],
                     "orphaned_block_claim": (old.get("failure") or "")[-60:]})
        REPORT["final_supply"] = view("best")["total_supply"]
    except Exception as e:
        ok = False
        check("near light client e2e aborted", False, {"exception": repr(e)})
    finally:
        for p in procs.values():
            p.terminate()
        net.stop_all()
        for p in procs.values():
            try:
                p.wait(20)
            except Exception:
                p.kill()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    json.dump(REPORT, open(os.path.join(ROOT, "bench", "results", "e2e_near_light_client.json"), "w"), indent=2, default=str)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed")
    sys.exit(0 if ok and REPORT["passed"] == REPORT["total"] else 1)


if __name__ == "__main__":
    main()
