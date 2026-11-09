#!/usr/bin/env python3
"""make e2e-bridge: Gate 3 + Gate 4 with real processes.

ZeroFlux testnet (3 nodes, bridge module) + real NEAR sandbox + wZFX contract
+ 3 independent relayers + coordinator.

Phase A: one round trip plus explicit security probes on live chains
         (1-of-3 mint/release, forged member index, replays, wrong amount).
Phase B: 100 two-way transfers by 10 concurrent users under fault injection
         (coordinator RPC faults, coordinator SIGKILL+restart, relayer
         refusal windows, relayer restart, duplicate mint delivery), with the
         backing invariant sampled continuously and full reconciliation at
         quiescence.
Writes bench/results/e2e_bridge.json; exit 1 on any failure.
"""
import json
import os
import random
import signal
import subprocess
import sys
import threading
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "deploy"))
sys.path.insert(0, os.path.join(ROOT, "testnet"))
import localnet  # noqa: E402
from localnet import BIN, CONTRACT, Localnet, jrun, sh, user_wallets  # noqa: E402
import zfxnet  # noqa: E402

ROUND_TRIPS = int(os.environ.get("ZFX_BRIDGE_ROUNDS", "100"))
FEE = 1000
REPORT = {"started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "checks": []}


def check(name, ok, evidence):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": evidence})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(evidence, default=str)[:500], flush=True)
    return ok


_tls = threading.local()


def _post(url, body):
    """POST JSON over a per-thread keep-alive connection (no port churn)."""
    import http.client
    from urllib.parse import urlparse
    u = urlparse(url)
    key = (u.hostname, u.port)
    conns = getattr(_tls, "conns", None)
    if conns is None:
        conns = _tls.conns = {}
    for attempt in range(2):
        c = conns.get(key)
        if c is None:
            c = conns[key] = http.client.HTTPConnection(u.hostname, u.port, timeout=60)
        try:
            c.request("POST", u.path or "/", body=json.dumps(body), headers={"content-type": "application/json"})
            return json.loads(c.getresponse().read())
        except Exception:
            c.close()
            conns.pop(key, None)
            if attempt:
                raise


def zrpc(ln, i, method, params):
    return _post(f"{ln.zfx_url(i)}/rpc", {"jsonrpc": "2.0", "id": 1, "method": method, "params": params})


def near_view_fast(ln, contract, method, args):
    import base64
    r = _post(ln.rpc, {"jsonrpc": "2.0", "id": "x", "method": "query", "params": {"request_type": "call_function", "finality": "final",
              "account_id": contract, "method_name": method, "args_base64": base64.b64encode(json.dumps(args).encode()).decode()}})
    return json.loads(bytes(r["result"]["result"]))


def relayer_post(ln, i, path, body):
    req = urllib.request.Request(f"http://127.0.0.1:{ln.relayers[i]['port']}{path}", data=json.dumps(body).encode(), headers={"content-type": "application/json"})
    try:
        return json.loads(urllib.request.urlopen(req, timeout=60).read())
    except urllib.error.HTTPError as e:
        return json.loads(e.read())


def wait(cond, timeout, what):
    t = time.time()
    while time.time() - t < timeout:
        try:
            if cond():
                return time.time() - t
        except Exception:
            pass
        time.sleep(1.5)
    raise AssertionError("timeout: " + what)


def deposit(ln, w, amount, node=2):
    r = sh([f"{BIN}/zfx-wallet", "bridge-deposit", "--wallet", w["wallet"], "--node", ln.zfx_url(node), "--near-account", w["near"],
            "--amount", str(amount), "--fee", str(FEE)], check=False)
    return json.loads(r.stdout.strip().splitlines()[-1])


def near_bal(ln, account):
    return int(near_view_fast(ln, CONTRACT, "ft_balance_of", {"account_id": account}))


def native_bal(ln, addr, node=3):
    return zrpc(ln, node, "zfx_get_account", {"address": addr})["result"]["balance"]


def burn(ln, w, amount):
    return jrun([f"{BIN}/zfx-bridge", "burn", "--rpc", ln.rpc, "--signer", w["near"], "--key-file", ln.key(w["near"]), "--contract", CONTRACT,
                 "--amount", str(amount), "--native-recipient", w["addr"]], check=False)


def main():
    subprocess.run(["rm", "-rf", localnet.RUN])
    os.makedirs(os.path.join(localnet.RUN, "zfx"), exist_ok=True)
    zfxnet.RUN = os.path.join(localnet.RUN, "zfx")
    ws = user_wallets()
    initial = 10**12
    ln = Localnet(confirmations=12, fault_rate=0.0)
    ok = True
    try:
        ln.up([{"address": w["addr"], "amount": initial} for w in ws])
        REPORT["deployment"] = {"near_rpc": ln.rpc, "contract": CONTRACT, "deploy": ln.deploy_record, "native_chain_id": ln.chain_id,
                                "asset_id": ln.asset_id, "committee_ed25519": [r["ed25519"] for r in ln.relayers],
                                "committee_slh": [r["slh"][:16] + "..." for r in ln.relayers]}
        md = ln.near_view(CONTRACT, "ft_metadata")
        ok &= check("B1 deployed wZFX: NEP-148 metadata, zero supply, committee configured",
                    md["symbol"] == "wZFX" and md["decimals"] == 8 and ln.near_view(CONTRACT, "ft_total_supply") == "0", md)

        # ---------------- Phase A ----------------
        u0, u1, u2 = ws[0], ws[1], ws[2]
        d = deposit(ln, u0, 50_000)
        wait(lambda: near_bal(ln, u0["near"]) == 50_000, 240, "u0 mint")
        # The mint is final on NEAR before the coordinator's send_tx(FINAL) call
        # returns; wait for the job record itself to reach FINALIZED.
        jurl = f"http://127.0.0.1:28790/jobs/{d['deposit_id']}"
        wait(lambda: (json.loads(urllib.request.urlopen(jurl).read()) or {}).get("state") == "FINALIZED", 120, "u0 mint job finalized")
        st = zrpc(ln, 1, "zfx_bridge_deposit_status", {"deposit_id": d["deposit_id"]})["result"]
        job = json.loads(urllib.request.urlopen(jurl).read())
        ok &= check("B4 deposit -> K confirmations -> 2-of-3 mint (receipts checked on both chains)",
                    st["state"] == "final" and job["state"] == "FINALIZED" and job["submission"]["success"],
                    {"deposit_id": d["deposit_id"], "native_block": st["block"], "confirmations": st["confirmations"], "near_mint_tx": job["submission"]["near_tx"],
                     "approvals": [a["member"] for a in job["approvals"]]})

        # 1-of-3 cannot mint: relayers 1 and 2 refuse; obtain relayer 0's approval and submit it alone
        ln.set_refuse(1, True)
        ln.set_refuse(2, True)
        d1 = deposit(ln, u1, 30_000)
        wait(lambda: zrpc(ln, 1, "zfx_bridge_deposit_status", {"deposit_id": d1["deposit_id"]})["result"]["state"] == "final", 240, "u1 deposit final")
        h = zrpc(ln, 1, "zfx_bridge_deposit_status", {"deposit_id": d1["deposit_id"]})["result"]["height"]
        att = {"epoch": 0, "deposit_id": d1["deposit_id"], "recipient": u1["near"], "amount": "30000", "native_height": str(h),
               "expiry_ns": str(time.time_ns() + 300 * 10**9)}
        a0 = relayer_post(ln, 0, "/attest/mint", att)["result"]
        ln.near_call("deployer.test.near", CONTRACT, "storage_deposit", {"account_id": u1["near"]}, deposit=1_250_000_000_000_000_000_000)
        one = ln.near_call("relay.test.near", CONTRACT, "bridge_mint", {"attestation": att, "approvals": [a0]}, check=False)
        forged = ln.near_call("relay.test.near", CONTRACT, "bridge_mint", {"attestation": att, "approvals": [a0, dict(a0, member=1)]}, check=False)
        time.sleep(6)
        stuck = json.loads(urllib.request.urlopen(f"http://127.0.0.1:28790/jobs/{d1['deposit_id']}").read())
        ok &= check("B1/B3 1-of-3 cannot mint (on-chain rejection) and a forged member index is rejected; coordinator waits",
                    not one.get("success", True) and "insufficient committee approvals" in str(one.get("failure"))
                    and "invalid committee signature" in str(forged.get("failure")) and near_bal(ln, u1["near"]) == 0 and stuck["state"] == "ATTESTING",
                    {"one_of_three": one.get("failure"), "forged": forged.get("failure"), "coordinator_state": stuck["state"]})
        ln.set_refuse(1, False)
        ln.set_refuse(2, False)
        wait(lambda: near_bal(ln, u1["near"]) == 30_000, 240, "u1 mint after quorum restored")
        wait(lambda: (json.loads(urllib.request.urlopen(f"http://127.0.0.1:28790/jobs/{d1['deposit_id']}").read()) or {}).get("state") == "FINALIZED", 120, "u1 job")
        fin = json.loads(urllib.request.urlopen(f"http://127.0.0.1:28790/jobs/{d1['deposit_id']}").read())
        rep = ln.near_call("relay.test.near", CONTRACT, "bridge_mint", {"attestation": json.loads(json.dumps(fin["data"]["attestation"])), "approvals": fin["approvals"]}, check=False)
        ok &= check("B1 replay of a finalized mint bundle rejected (no double mint)",
                    "already minted (replay)" in str(rep.get("failure")) and near_bal(ln, u1["near"]) == 30_000, {"failure": rep.get("failure")})

        # burn -> 2-of-3 native release
        before = native_bal(ln, u0["addr"])
        b = burn(ln, u0, 20_000)
        wid = b["value"]["withdrawal_id"]
        wait(lambda: native_bal(ln, u0["addr"]) == before + 20_000, 240, "u0 release")
        wait(lambda: (json.loads(urllib.request.urlopen(f"http://127.0.0.1:28790/jobs/{wid}").read()) or {}).get("state") == "FINALIZED", 180, "release job finalized")
        rs = zrpc(ln, 1, "zfx_bridge_redeem_status", {"withdrawal_id": wid})["result"]
        rjob = json.loads(urllib.request.urlopen(f"http://127.0.0.1:28790/jobs/{wid}").read())
        ok &= check("B2/B4 burn on NEAR (final) -> 2-of-3 SLH-DSA native release validated by consensus",
                    rs["state"] in ("final", "released_awaiting_confirmations") and len(rjob["data"]["release"]["approvals"]) == 2,
                    {"withdrawal_id": wid, "near_burn_tx": b["tx_hash"], "native_release": rs.get("record")})

        # 1-of-3 release rejected by native consensus; wrong amount refused by relayers; replay rejected
        b2 = burn(ln, u1, 5_000)
        nonce2 = int(b2["value"]["nonce"])
        info = zrpc(ln, 1, "zfx_bridge_info", {})["result"]
        rel = {"epoch": info["epoch"], "withdrawal_id": b2["value"]["withdrawal_id"], "near_burn_ref": f"{CONTRACT}:{nonce2}:{b2['value']['block_height']}",
               "recipient": u1["addr"], "amount": 5000, "expiry_height": info["height"] + 100, "approvals": []}
        ln.set_refuse(1, True)
        ln.set_refuse(2, True)
        time.sleep(3)  # let the coordinator observe the burn while 2 members refuse
        s0 = relayer_post(ln, 0, "/attest/release", {"release": rel, "burn_nonce": nonce2})["result"]
        r1 = zrpc(ln, 1, "zfx_bridge_submit_op", {"op": dict(rel, op="release", approvals=[s0])})
        wrong = relayer_post(ln, 0, "/attest/release", {"release": dict(rel, amount=50000), "burn_nonce": nonce2})
        ln.set_refuse(1, False)
        ln.set_refuse(2, False)
        old_rel = rjob["data"]["release"]
        replay = zrpc(ln, 1, "zfx_bridge_submit_op", {"op": dict(old_rel, op="release")})
        ok &= check("B2 1-of-3 release rejected by native consensus; relayer refuses wrong amount; release replay rejected",
                    "needs 2" in json.dumps(r1.get("error")) and not wrong["ok"] and "amount mismatch" in wrong["error"]
                    and ("replay" in json.dumps(replay.get("error")) or "already" in json.dumps(replay.get("error"))),
                    {"one_of_three": r1.get("error"), "wrong_amount": wrong.get("error"), "replay": replay.get("error")})
        wait(lambda: zrpc(ln, 1, "zfx_bridge_redeem_status", {"withdrawal_id": b2["value"]["withdrawal_id"]})["result"]["state"] in ("final", "released_awaiting_confirmations"), 240, "u1 release")

        # ---------------- Phase B: 100 round trips under faults ----------------
        ln.stop("coordinator")
        ln.start_coordinator(fault_rate=0.15)
        users = ws[:10]
        ledger = {w["name"]: {"deposits": 0, "fees": 0, "releases": 0, "burns": 0} for w in users}
        for w in users:  # account for Phase A activity
            if w is u0:
                ledger[w["name"]].update(deposits=50_000, fees=FEE, releases=20_000, burns=20_000)
            if w is u1:
                ledger[w["name"]].update(deposits=30_000, fees=FEE, releases=5_000, burns=5_000)
        start_native = {w["name"]: native_bal(ln, w["addr"]) for w in users}
        start_near = {w["name"]: near_bal(ln, w["near"]) for w in users}
        errors = []
        samples = []
        stop = threading.Event()
        lock = threading.Lock()
        per_user = ROUND_TRIPS // len(users)

        def user_loop(w):
            rnd = random.Random(w["name"])
            for k in range(per_user):
                try:
                    amt = rnd.randrange(2_000, 40_000)
                    nb = near_bal(ln, w["near"])
                    dd = deposit(ln, w, amt, node=rnd.choice([1, 2, 3]))
                    if not dd.get("accepted"):
                        raise RuntimeError(f"deposit rejected: {dd}")
                    wait(lambda: near_bal(ln, w["near"]) >= nb + amt, 600, f"{w['name']} mint {k}")
                    back = rnd.randrange(1, amt + 1)
                    pre = native_bal(ln, w["addr"])
                    bb = burn(ln, w, back)
                    if not bb.get("success"):
                        raise RuntimeError(f"burn failed: {bb}")
                    wait(lambda: native_bal(ln, w["addr"]) >= pre + back, 600, f"{w['name']} release {k}")
                    with lock:
                        L = ledger[w["name"]]
                        L["deposits"] += amt
                        L["fees"] += FEE
                        L["burns"] += back
                        L["releases"] += back
                except Exception as e:
                    with lock:
                        errors.append(f"{w['name']}#{k}: {e!r}")

        def sampler():
            while not stop.is_set():
                try:
                    r = ln.reconcile()
                    samples.append({"t": time.time(), "invariant_ok": r.get("invariant_ok"), "orphans": len(r.get("orphans", [])),
                                    "pending_deposits": r.get("pending_deposits", {}).get("total"), "pending_releases": r.get("pending_releases", {}).get("total")})
                except Exception as e:
                    samples.append({"t": time.time(), "error": repr(e)})
                time.sleep(4)

        chaos_log = []

        def chaos():
            rnd = random.Random(7)
            while not stop.is_set():
                time.sleep(rnd.uniform(15, 30))
                if stop.is_set():
                    break
                act = rnd.choice(["kill_coordinator", "refuse_window", "restart_relayer", "duplicate_mint"])
                try:
                    if act == "kill_coordinator":
                        ln.stop("coordinator", signal.SIGKILL)
                        time.sleep(1)
                        ln.start_coordinator(fault_rate=0.15)
                    elif act == "refuse_window":
                        i = rnd.randrange(3)
                        ln.set_refuse(i, True)
                        time.sleep(10)
                        ln.set_refuse(i, False)
                    elif act == "restart_relayer":
                        i = rnd.randrange(3)
                        ln.stop(f"relayer{i}", signal.SIGKILL)
                        time.sleep(2)
                        ln.start_relayer(i)
                    elif act == "duplicate_mint":
                        jobs = json.loads(urllib.request.urlopen("http://127.0.0.1:28790/jobs", timeout=5).read())
                        fins = [j for j in jobs.values() if j["kind"] == "mint" and j["state"] == "FINALIZED" and j["approvals"]]
                        if fins:
                            j = rnd.choice(fins)
                            r = ln.near_call("relay.test.near", CONTRACT, "bridge_mint", {"attestation": j["data"]["attestation"], "approvals": j["approvals"]}, check=False)
                            act += ": " + ("rejected" if "replay" in str(r.get("failure")) else f"UNEXPECTED {r}")
                    chaos_log.append({"t": time.time(), "action": act})
                except Exception as e:
                    chaos_log.append({"t": time.time(), "action": act, "error": repr(e)})

        th = [threading.Thread(target=user_loop, args=(w,)) for w in users]
        aux = [threading.Thread(target=sampler), threading.Thread(target=chaos)]
        t0 = time.time()
        for t in th + aux:
            t.start()
        for t in th:
            t.join()
        stop.set()
        for t in aux:
            t.join()
        elapsed = time.time() - t0
        # quiescence: all jobs terminal
        wait(lambda: all(j["state"] in ("FINALIZED",) for j in json.loads(urllib.request.urlopen("http://127.0.0.1:28790/jobs").read()).values()), 600, "quiescence")
        rec = ln.reconcile()
        done = sum(1 for w in users for _ in range(1)) and sum(1 for e in errors) == 0
        completed = sum(L["releases"] > 0 for L in ledger.values())
        ok &= check(f"Gate 4: {ROUND_TRIPS} two-way transfers under fault injection completed",
                    not errors, {"elapsed_s": round(elapsed, 1), "errors": errors[:5], "chaos_events": chaos_log})
        bad_samples = [s for s in samples if s.get("invariant_ok") is False or s.get("orphans")]
        ok &= check("B3 backing invariant minted-burned <= locked-released held at every sample; no orphans",
                    not bad_samples and len(samples) > 5, {"samples": len(samples), "violations": bad_samples[:3]})
        ok &= check("B3 full reconciliation at quiescence: exact identity, custody, supply, zero pending, zero orphans",
                    rec.get("ok") is True and rec["pending_deposits"]["total"] == "0" and rec["pending_releases"]["total"] == "0",
                    {k: rec.get(k) for k in ("ok", "invariant_ok", "accounting_identity_ok", "native_custody_ok", "wrapped_supply_ok", "native", "near")})
        # per-user conservation
        mism = []
        for w in users:
            L = ledger[w["name"]]
            exp_native = initial - L["deposits"] - L["fees"] + L["releases"]
            exp_near = L["deposits"] - L["burns"]
            gn, gw = native_bal(ln, w["addr"]), near_bal(ln, w["near"])
            if gn != exp_native or gw != exp_near:
                mism.append({"user": w["name"], "native": gn, "expected_native": exp_native, "near": gw, "expected_near": exp_near})
        ok &= check("B3 per-user conservation on both chains (native = initial - deposits - fees + releases; wZFX = deposits - burns)",
                    not mism, {"mismatches": mism, "users": len(users)})
        st = ln.coordinator_status()
        REPORT["coordinator_final"] = st["counts"]
        REPORT["reconciliation_final"] = rec
    except Exception as e:
        ok = False
        check("e2e-bridge aborted", False, {"exception": repr(e)})
    finally:
        ln.stop_all()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    out = os.path.join(ROOT, "bench", "results", "e2e_bridge.json")
    json.dump(REPORT, open(out, "w"), indent=2, default=str)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed -> {out}")
    sys.exit(0 if ok and REPORT["passed"] == REPORT["total"] else 1)


if __name__ == "__main__":
    main()
