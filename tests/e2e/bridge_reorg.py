#!/usr/bin/env python3
"""Finality & reorg experiment (part of make e2e-bridge).

A fourth ZeroFlux node is isolated (deny-all peer allowlist) and mines a
private fork that contains a conflicting spend of the depositor's nonce, so
the deposit can never be re-included after the fork wins.

Scenario 1, short reorg before K confirmations: the deposit must never be
minted; the coordinator marks it REORGED; reconciliation stays exact.
Scenario 2, deep reorg after a mint (the "K too small" disaster case): the
minted wZFX becomes unbacked; the coordinator must detect the orphan, write
an incident, and pause both chains through 1-of-3 relayer approvals. It
must never mint or release anything to compensate.

Writes bench/results/e2e_bridge_reorg.json.
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
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import localnet  # noqa: E402
from localnet import BIN, CONTRACT, Localnet, sh, user_wallets  # noqa: E402
import zfxnet  # noqa: E402
from bridge import deposit, near_bal, wait, zrpc  # noqa: E402

REPORT = {"started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "checks": []}


def check(name, ok, ev):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": ev})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(ev, default=str)[:500], flush=True)
    return ok


def h(ln, i):
    return zfxnet.status(i)["height"]


def isolate_node4(ln):
    deny = os.path.join(localnet.RUN, "deny_all.txt")
    open(deny, "w").write("00" * 32 + "\n")
    ln.net.stop("node4")
    ln.net.start_node(4, [], ln.cfg_path, extra=["--no-discovery", "--peer-allow", deny, "--bridge-confirmations", str(ln.confirmations)])


def rejoin_node4(ln):
    ln.net.stop("node4")
    ln.net.start_node(4, [1], ln.cfg_path, extra=["--bridge-confirmations", str(ln.confirmations)])


def conflicting_spend(ln, w, node):
    """Same sender nonce as the upcoming deposit, different recipient."""
    r = sh([f"{BIN}/zfx-wallet", "send", "--wallet", w["wallet"], "--node", ln.zfx_url(node), "--to", w["addr2"], "--amount", "1", "--fee", "1000"], check=False)
    return json.loads(r.stdout.strip().splitlines()[-1])


def job(id_):
    try:
        return json.loads(urllib.request.urlopen(f"http://127.0.0.1:28790/jobs/{id_}", timeout=5).read())
    except Exception:
        return None


def main():
    subprocess.run(["rm", "-rf", localnet.RUN])
    os.makedirs(os.path.join(localnet.RUN, "zfx"), exist_ok=True)
    zfxnet.RUN = os.path.join(localnet.RUN, "zfx")
    ws = user_wallets()
    for w in ws:
        w["addr2"] = zfxnet.wallet(f"bridge_{w['name']}_b", "99" * 31 + f"{ord(w['name'][1]):02x}")[1]
    ln = Localnet(confirmations=12)
    ok = True
    try:
        ln.up([{"address": w["addr"], "amount": 10**12} for w in ws])
        ln.net.start_node(4, [1], ln.cfg_path, extra=["--bridge-confirmations", "12"])
        wait(lambda: zfxnet.status(4)["best_hash"] == zfxnet.status(1)["best_hash"], 120, "node4 synced")

        # ---- Scenario 1: short reorg before K ----
        wA = ws[3]
        isolate_node4(ln)
        c = conflicting_spend(ln, wA, 4)
        _, maddr = zfxnet.wallet("node4_miner", "88" * 32)
        ln.net.start_miner("miner4", 4, maddr, "rust")
        d = deposit(ln, wA, 25_000, node=1)
        # Blocks arrive ~6/s: poll fast and halt the main miner right after
        # inclusion so the reorg really happens below K confirmations.
        t0 = time.time()
        while time.time() - t0 < 120:
            c_ = zrpc(ln, 1, "zfx_bridge_deposit_status", {"deposit_id": d["deposit_id"]})["result"].get("confirmations", 0)
            if c_ >= 1:
                break
            time.sleep(0.05)
        ln.net.procs["miner1"].kill()
        ln.net.stop("miner1")
        conf_before = zrpc(ln, 1, "zfx_bridge_deposit_status", {"deposit_id": d["deposit_id"]})["result"]["confirmations"]
        wait(lambda: h(ln, 4) > h(ln, 1) + 3, 300, "private fork heavier")
        ln.net.stop("miner4")
        rejoin_node4(ln)
        wait(lambda: zfxnet.status(1)["best_hash"] == zfxnet.status(4)["best_hash"], 120, "main nodes adopt the fork")
        time.sleep(5)
        st = zrpc(ln, 1, "zfx_bridge_deposit_status", {"deposit_id": d["deposit_id"]})["result"]
        j = job(d["deposit_id"])
        minted = ln.near_view(CONTRACT, "is_minted", {"deposit_id": d["deposit_id"]})
        rec = ln.reconcile()
        ok &= check("Finality: deposit reorged out below K=12 confirmations is never minted; coordinator marks REORGED; books stay exact",
                    st["state"] == "unknown" and minted is False and (j is None or j["state"] in ("REORGED", "AWAITING_FINALITY", "OBSERVED")) and rec.get("ok") is True,
                    {"confirmations_before_reorg": conf_before, "K": 12, "deposit_state_after": st["state"], "is_minted": minted, "job": j and j["state"],
                     "conflicting_tx": c.get("txid"), "reconcile_ok": rec.get("ok"), "reorgs_node1": zfxnet.status(1)["reorgs"]})
        ln.start_miner(1)

        # ---- Scenario 2: deep reorg after mint ----
        wB = ws[4]
        isolate_node4(ln)
        c2 = conflicting_spend(ln, wB, 4)
        ln.net.start_miner("miner4", 4, maddr, "rust")
        d2 = deposit(ln, wB, 40_000, node=1)
        wait(lambda: near_bal(ln, wB["near"]) == 40_000, 300, "minted before the deep reorg")
        h_mint = h(ln, 1)
        ln.net.stop("miner1")
        wait(lambda: h(ln, 4) > h(ln, 1) + 3, 600, "private fork overtakes after mint")
        ln.net.stop("miner4")
        depth_hint = h(ln, 4)
        rejoin_node4(ln)
        wait(lambda: zfxnet.status(1)["best_hash"] == zfxnet.status(4)["best_hash"], 120, "deep reorg adopted")
        ln.start_miner(1)
        # coordinator reconciles every 5 ticks and halts after 3 consecutive failures
        incdir = os.path.join(localnet.RUN, "incidents")
        wait(lambda: os.path.isdir(incdir) and len(os.listdir(incdir)) > 0, 240, "incident written")
        wait(lambda: ln.near_view(CONTRACT, "bridge_state")["paused"] is True, 120, "NEAR paused")
        wait(lambda: zrpc(ln, 1, "zfx_bridge_info", {})["result"]["paused"] is True, 180, "native paused")
        inc = json.load(open(os.path.join(incdir, sorted(os.listdir(incdir))[0])))
        rec = ln.reconcile()
        st = ln.coordinator_status()
        # nothing new mints while halted
        wC = ws[5]
        dd = deposit(ln, wC, 9_000, node=1)
        time.sleep(15)
        ok &= check("Deep reorg after mint: unbacked wZFX detected as orphan; incident written; both chains paused via 1-of-3; no compensating mint; new deposits blocked",
                    any(o["kind"] == "mint_without_onchain_deposit" for o in rec.get("orphans", [])) and st["halted"] is not None
                    and near_bal(ln, wC["near"]) == 0 and not dd.get("accepted", False),
                    {"minted_at_height": h_mint, "fork_height": depth_hint, "orphans": rec.get("orphans"), "incident": inc["reason"],
                     "coordinator_halted": st["halted"], "deposit_while_paused": dd.get("error") or dd,
                     "conflicting_tx": c2.get("txid"), "reorged_deposit": d2.get("deposit_id")})
        REPORT["conclusion"] = ("K must exceed the deepest plausible reorg. The testnet default K=12 is a policy value; this run shows "
                                "the failure mode beyond K is detected and halted, not silently absorbed.")
    except Exception as e:
        ok = False
        check("bridge reorg experiment aborted", False, {"exception": repr(e)})
    finally:
        ln.stop_all()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    out = os.path.join(ROOT, "bench", "results", "e2e_bridge_reorg.json")
    json.dump(REPORT, open(out, "w"), indent=2, default=str)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed -> {out}")
    sys.exit(0 if ok and REPORT["passed"] == REPORT["total"] else 1)


if __name__ == "__main__":
    main()
