#!/usr/bin/env python3
"""Bridge governance and recovery e2e (part of make e2e-bridge).

Runs on the same local topology as bridge.py (3 ZeroFlux nodes, NEAR
sandbox, 3 relayers, coordinator) and exercises, live on both chains:

  G1 emergency pause: 1-of-3 pauses NEAR and native; 1-of-3 unpause is
     rejected; 2-of-3 unpause works; a replayed unpause is rejected.
  G2 committee rotation: the old committee approves a new key set 2-of-3;
     NEAR applies it only after the timelock, native only at the scheduled
     height. Relayers restart on the new keys and a full deposit -> mint ->
     burn -> release round trip completes. Old keys are then rejected.
  G3 contract upgrade: non-owner staging rejected; deploy before timelock
     rejected; a WASM that does not match the staged hash rejected; the
     staged v2 deploys after the timelock with token state preserved, and
     the bridge keeps working on the upgraded contract.
  G4 coordinator disaster recovery: the coordinator's job store is deleted;
     the restarted coordinator rebuilds from chain state without a single
     duplicate mint or release, and reconciliation stays exact.

Writes bench/results/e2e_bridge_governance.json.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "deploy"))
sys.path.insert(0, os.path.join(ROOT, "testnet"))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import localnet  # noqa: E402
from localnet import BIN, CONTRACT, WASM, Localnet, jrun, sh, user_wallets  # noqa: E402
import zfxnet  # noqa: E402
from bridge import burn, deposit, near_bal, wait, zrpc  # noqa: E402

REPORT = {"started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "checks": []}
OWNER = "deployer.test.near"
V2_DIR = os.path.join(ROOT, "bridge", "near", "target", "near-upgrade-v2")
V2_WASM = os.path.join(V2_DIR, "wzfx.wasm")


def check(name, ok, ev):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": ev})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(ev, default=str)[:600], flush=True)
    return ok


def failure(r):
    return str(r.get("failure") or r.get("error") or r.get("_err") or "")[:300]


def near_state(ln):
    return ln.near_view(CONTRACT, "bridge_state")


def native_info(ln):
    return zrpc(ln, 1, "zfx_bridge_info", {})["result"]


def admin_sign(seed, member, chain, epoch, nonce, action, **kw):
    cmd = [f"{BIN}/zfx-bridge", "admin-sign", "--seed", seed, "--member", str(member), "--chain", chain, "--epoch", str(epoch),
           "--nonce", str(nonce), "--action", action]
    for k, v in kw.items():
        cmd += ["--" + k.replace("_", "-"), str(v)]
    return json.loads(sh(cmd).stdout.strip().splitlines()[-1])


def near_admin(ln, signer, method, args, gas=100):
    return ln.near_call(signer, CONTRACT, method, args, gas=gas, check=False)


def native_admin(ln, epoch, nonce, action, approvals, **extra):
    op = {"op": "admin", "epoch": epoch, "op_nonce": nonce, "action": action, "approvals": approvals}
    op.update(extra)
    return zrpc(ln, 1, "zfx_bridge_submit_op", {"op": op})


def seeds(ln):
    return [r["seed"] for r in ln.relayers]


def build_v2():
    os.makedirs(V2_DIR, exist_ok=True)
    env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
    r = subprocess.run(["cargo", "near", "build", "non-reproducible-wasm", "--features", "upgrade-test-v2", "--out-dir", V2_DIR],
                       cwd=os.path.join(ROOT, "bridge", "near", "wzfx"), env=env, capture_output=True, text=True)
    if r.returncode != 0 or not os.path.exists(V2_WASM):
        raise RuntimeError("v2 build failed: " + r.stderr[-600:])
    return hashlib.sha256(open(V2_WASM, "rb").read()).hexdigest()


def round_trip(ln, w, amount, label):
    """deposit -> 2-of-3 mint -> burn -> 2-of-3 native release; returns evidence."""
    before = near_bal(ln, w["near"])
    d = deposit(ln, w, amount, node=1)
    if not d.get("accepted", True) and d.get("error"):
        raise RuntimeError(f"{label}: deposit rejected {d}")
    wait(lambda: near_bal(ln, w["near"]) == before + amount, 300, f"{label}: minted")
    rel0 = native_info(ln)["released_total"]
    b = burn(ln, w, amount)
    if not b.get("success"):
        raise RuntimeError(f"{label}: burn failed {failure(b)}")
    wait(lambda: int(native_info(ln)["released_total"]) >= int(rel0) + amount, 300, f"{label}: released on native chain")
    return {"deposit": d.get("deposit_id"), "near_burn_tx": b.get("tx_hash"), "released_total": native_info(ln)["released_total"]}


def main():
    subprocess.run(["rm", "-rf", localnet.RUN])
    os.makedirs(os.path.join(localnet.RUN, "zfx"), exist_ok=True)
    zfxnet.RUN = os.path.join(localnet.RUN, "zfx")
    ws = user_wallets()
    ln = Localnet(confirmations=6)
    ok = True
    try:
        v2_hash = build_v2()
        ln.up([{"address": w["addr"], "amount": 10**12} for w in ws])
        cid = ln.chain_id
        # Keep the epoch-0 seeds in separate files: the live seed paths are
        # overwritten when relayers move to the new committee.
        s0 = []
        for i, path in enumerate(seeds(ln)):
            keep = os.path.join(localnet.RUN, f"retired_seed_{i}")
            shutil.copyfile(path, keep)
            os.chmod(keep, 0o600)
            s0.append(keep)
        user = ws[0]["near"]  # an ordinary account, not the owner

        # ---------------- G1 pause / unpause ----------------
        ns = near_state(ln)
        e, n = ns["epoch"], int(ns["admin_nonce"])
        p1 = near_admin(ln, user, "bridge_pause", {"approvals": [admin_sign(s0[2], 2, "near", e, n, "pause", contract=CONTRACT)]})
        paused_near = near_state(ln)["paused"]
        u1 = near_admin(ln, user, "bridge_unpause", {"approvals": [admin_sign(s0[0], 0, "near", e, n + 1, "unpause", contract=CONTRACT)]})
        two = [admin_sign(s0[i], i, "near", e, n + 1, "unpause", contract=CONTRACT) for i in (0, 1)]
        u2 = near_admin(ln, user, "bridge_unpause", {"approvals": two})
        replay = near_admin(ln, user, "bridge_unpause", {"approvals": two})
        ok &= check("G1 NEAR: 1-of-3 pause works; 1-of-3 unpause rejected; 2-of-3 unpause works; replayed unpause rejected",
                    p1.get("success") and paused_near is True and not u1.get("success") and u2.get("success")
                    and near_state(ln)["paused"] is False and not replay.get("success"),
                    {"pause_tx": p1.get("tx_hash"), "unpause_1of3": failure(u1), "unpause_tx": u2.get("tx_hash"), "replay": failure(replay)})

        ni = native_info(ln)
        e, n = ni["epoch"], ni["admin_nonce"]
        r = native_admin(ln, e, n, "pause", [admin_sign(s0[1], 1, "native", e, n, "pause", chain_id=cid)])
        wait(lambda: native_info(ln)["paused"] is True, 60, "native paused")
        bad = native_admin(ln, e, n + 1, "unpause", [admin_sign(s0[1], 1, "native", e, n + 1, "unpause", chain_id=cid)])
        dep_paused = deposit(ln, ws[1], 5_000, node=1)
        good = [admin_sign(s0[i], i, "native", e, n + 1, "unpause", chain_id=cid) for i in (1, 2)]
        r2 = native_admin(ln, e, n + 1, "unpause", good)
        wait(lambda: native_info(ln)["paused"] is False, 60, "native unpaused")
        rp = native_admin(ln, e, n + 1, "unpause", good)
        ok &= check("G1 native: 1-of-3 pause included by consensus; deposits refused while paused; 1-of-3 unpause rejected; 2-of-3 unpause; replay rejected",
                    "result" in r and "error" in bad and not dep_paused.get("accepted", False) and "result" in r2 and "error" in rp,
                    {"pause_op": r.get("result"), "unpause_1of3": bad.get("error"), "deposit_while_paused": dep_paused.get("error") or dep_paused,
                     "unpause_op": r2.get("result"), "replay": rp.get("error")})

        # ---------------- G2 committee rotation ----------------
        new = []
        for i in range(3):
            path = os.path.join(localnet.RUN, f"relayer{i}", "seed.epoch1")
            k = jrun([f"{BIN}/zfx-bridge", "keygen-relayer", "--out", path])
            new.append({"seed": path, "ed25519": k["ed25519"], "slh": k["slh"]})
        ned = ",".join(k["ed25519"] for k in new)
        nslh = ",".join(k["slh"] for k in new)
        ns = near_state(ln)
        e, n = ns["epoch"], int(ns["admin_nonce"])
        appr = [admin_sign(s0[i], i, "near", e, n, "rotate", contract=CONTRACT, committee=ned) for i in (0, 2)]
        pr = near_admin(ln, user, "propose_rotation", {"committee": ned.split(","), "approvals": appr})
        early = near_admin(ln, user, "apply_rotation", {})
        time.sleep(ns_timelock_s(ns) + 2)
        ap = near_admin(ln, user, "apply_rotation", {})
        ns2 = near_state(ln)
        ni = native_info(ln)
        e2, n2 = ni["epoch"], ni["admin_nonce"]
        # Margin over the 20-block timelock: two SLH-DSA-128s signatures take
        # long enough that a fast localnet mines dozens of blocks meanwhile.
        act = ni["height"] + 120
        nappr = [admin_sign(s0[i], i, "native", e2, n2, "rotate", chain_id=cid, committee=nslh, activate_height=act) for i in (0, 1)]
        rot = native_admin(ln, e2, n2, "rotate", nappr, members=nslh.split(","), activate_height=act)
        if "result" not in rot:
            raise RuntimeError(f"native rotation op rejected: {rot.get('error')} (epoch {e2}, nonce {n2}, height {ni['height']}, act {act})")
        try:
            wait(lambda: native_info(ln)["epoch"] == e2 + 1, 600, "native rotation activated")
        except AssertionError:
            raise RuntimeError(f"rotation not activated: submitted at {ni['height']} for {act}; now {native_info(ln)}")
        ni2 = native_info(ln)
        ok &= check("G2 rotation: 2-of-3 old committee approves; NEAR applies only after timelock; native activates at the scheduled height",
                    pr.get("success") and not early.get("success") and ap.get("success") and ns2["epoch"] == e + 1
                    and ns2["committee"] == ned.split(",") and "result" in rot and ni2["members"] == nslh.split(",") and ni2["height"] >= act,
                    {"propose_tx": pr.get("tx_hash"), "apply_before_timelock": failure(early), "apply_tx": ap.get("tx_hash"),
                     "near_epoch": ns2["epoch"], "native_epoch": ni2["epoch"], "native_activate_height": act, "native_op": rot.get("result")})

        # relayers move to the new keys; coordinator picks up the new env
        ln.stop("coordinator")
        for i in range(3):
            ln.stop(f"relayer{i}")
            os.replace(new[i]["seed"], ln.relayers[i]["seed"])
            ln.relayers[i]["ed25519"], ln.relayers[i]["slh"] = new[i]["ed25519"], new[i]["slh"]
        ln.env_file()
        for i in range(3):
            ln.start_relayer(i)
        ln.start_coordinator(fault_rate=0.0)
        rt = round_trip(ln, ws[2], 30_000, "post-rotation")
        ns3, ni3 = near_state(ln), native_info(ln)
        old_near = near_admin(ln, user, "bridge_pause", {"approvals": [admin_sign(s0[0], 0, "near", ns3["epoch"], int(ns3["admin_nonce"]), "pause", contract=CONTRACT)]})
        old_nat = native_admin(ln, ni3["epoch"], ni3["admin_nonce"], "pause",
                               [admin_sign(s0[0], 0, "native", ni3["epoch"], ni3["admin_nonce"], "pause", chain_id=cid)])
        ok &= check("G2 recovery: new committee completes deposit -> mint -> burn -> release; retired keys rejected on both chains",
                    not old_near.get("success") and "error" in old_nat and near_state(ln)["paused"] is False and native_info(ln)["paused"] is False,
                    {"round_trip": rt, "old_key_near": failure(old_near), "old_key_native": old_nat.get("error")})

        # ---------------- G3 contract upgrade ----------------
        snap = {"supply": ln.near_view(CONTRACT, "ft_total_supply"), "bal": near_bal(ln, ws[0]["near"]),
                "bal2": near_bal(ln, ws[2]["near"]), "state": near_state(ln)}
        st_user = near_admin(ln, user, "stage_upgrade", {"code_hash": v2_hash})
        st = near_admin(ln, OWNER, "stage_upgrade", {"code_hash": v2_hash})
        raw = lambda f: jrun([f"{BIN}/zfx-bridge", "call", "--rpc", ln.rpc, "--signer", OWNER, "--key-file", ln.key(OWNER), "--contract", CONTRACT,  # noqa: E731
                              "--method", "deploy_upgrade", "--raw-args-file", f, "--gas", "300"], check=False)
        too_early = raw(V2_WASM)
        time.sleep(ns_timelock_s(snap["state"]) + 2)
        wrong = raw(WASM)
        dep = raw(V2_WASM)
        time.sleep(3)
        after = {"version": ln.near_view(CONTRACT, "version"), "supply": ln.near_view(CONTRACT, "ft_total_supply"),
                 "bal": near_bal(ln, ws[0]["near"]), "bal2": near_bal(ln, ws[2]["near"]), "state": near_state(ln)}
        keep = all(after["state"][k] == snap["state"][k] for k in ("epoch", "committee", "total_minted", "total_burned", "burn_nonce", "admin_nonce"))
        ok &= check("G3 upgrade: non-owner stage rejected; deploy before timelock rejected; mismatched WASM rejected; staged v2 deployed with state preserved",
                    not st_user.get("success") and st.get("success") and not too_early.get("success") and not wrong.get("success") and dep.get("success")
                    and after["version"].endswith("upgrade-test-v2") and after["supply"] == snap["supply"] and after["bal"] == snap["bal"]
                    and after["bal2"] == snap["bal2"] and keep,
                    {"v2_sha256": v2_hash, "stage_by_user": failure(st_user), "deploy_before_timelock": failure(too_early),
                     "mismatched_wasm": failure(wrong), "upgrade_tx": dep.get("tx_hash"), "version_after": after["version"],
                     "supply_before_after": [snap["supply"], after["supply"]]})
        rt2 = round_trip(ln, ws[3], 12_000, "post-upgrade")
        rec = ln.reconcile()
        ok &= check("G3 bridge operates on the upgraded contract; reconciliation exact", rec.get("ok") is True,
                    {"round_trip": rt2, "reconcile_ok": rec.get("ok"), "invariant_ok": rec.get("invariant_ok")})

        # ---------------- G4 coordinator disaster recovery ----------------
        before = near_state(ln)
        ln.stop("coordinator")
        store = os.path.join(localnet.RUN, "jobs.json")
        existed = os.path.exists(store)
        if existed:
            os.remove(store)
        ln.start_coordinator(fault_rate=0.0)
        time.sleep(25)
        rt3 = round_trip(ln, ws[4], 7_000, "post-recovery")
        after = near_state(ln)
        rec = ln.reconcile()
        ni = native_info(ln)
        ok &= check("G4 disaster recovery: coordinator job store deleted; restart rebuilds from chain with zero duplicate mints/releases; books exact",
                    existed and int(after["total_minted"]) == int(before["total_minted"]) + 7_000 and rec.get("ok") is True
                    and rec.get("invariant_ok") is True and not rec.get("orphans"),
                    {"store_deleted": existed, "total_minted_before": before["total_minted"], "total_minted_after": after["total_minted"],
                     "round_trip": rt3, "native_releases": ni["released_total"], "reconcile_ok": rec.get("ok")})

        # ---------------- G5 resume after an investigated incident ----------------
        ln.stop("coordinator")
        store = os.path.join(localnet.RUN, "jobs.json")
        js = json.load(open(store))
        js["halted"] = "drill: simulated invariant alarm"
        json.dump(js, open(store, "w"))
        incdir = os.path.join(localnet.RUN, "incidents")
        os.makedirs(incdir, exist_ok=True)
        inc = os.path.join(incdir, "incident-drill.json")
        json.dump({"reason": "drill: simulated invariant alarm", "action": "bridge halted; manual review required"}, open(inc, "w"))
        ln.start_coordinator(fault_rate=0.0)
        time.sleep(3)
        halted_before = ln.coordinator_status()["halted"]
        ln.stop("coordinator")
        rr = subprocess.run([f"{BIN}/zfx-bridge-coordinator", "--env", ln.env_path, "--zfx", ln.zfx_url(1), "--near-account", ln.coordinator_account,
                             "--near-key-file", ln.key(ln.coordinator_account), "--store", store, "--incidents", incdir,
                             "--resume-after-incident", inc, "--once"], capture_output=True, text=True, env=localnet.ENVV)
        ln.start_coordinator(fault_rate=0.0)
        rt4 = round_trip(ln, ws[5], 6_000, "post-resume")
        audited = json.load(open(inc)).get("resumed")
        ok &= check("G5 resume: a halted coordinator stays halted; --resume-after-incident clears it only with clean books and records the resume in the incident",
                    halted_before is not None and rr.returncode == 0 and audited is not None and ln.coordinator_status()["halted"] is None,
                    {"halted_before": halted_before, "resume_rc": rr.returncode, "resume_log": rr.stderr[-200:], "round_trip": rt4,
                     "incident_resume_record": bool(audited)})
    except Exception as ex:
        ok = False
        check("bridge governance e2e aborted", False, {"exception": repr(ex)})
    finally:
        ln.stop_all()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    out = os.path.join(ROOT, "bench", "results", "e2e_bridge_governance.json")
    json.dump(REPORT, open(out, "w"), indent=2, default=str)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed -> {out}")
    sys.exit(0 if ok and REPORT["passed"] == REPORT["total"] else 1)


def ns_timelock_s(ns):
    return int(ns["timelock_ns"]) / 1e9


if __name__ == "__main__":
    main()
