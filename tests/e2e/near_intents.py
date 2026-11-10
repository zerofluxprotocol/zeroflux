#!/usr/bin/env python3
"""ZFX (mined on NEAR, optimistic PoW) traded through NEAR Intents.

No bridge: ZFX is mined directly into NEAR accounts by zfx-mine-opt, then
deposited into the official Verifier and swapped against a test asset by a
local solver through the Message Bus protocol.

  I1 the solver and a user mine ZFX on NEAR (blocks finalized, watcher on)
  I2 both deposit mined ZFX into the official Verifier (NEP-141 ft_transfer_call)
  I3 swap ZFX -> tUSDC: quote, NEP-413 token_diff, settled on-chain
  I4 swap tUSDC -> ZFX settles
  I5 Verifier balances and the ZFX contract's supply stay consistent

Writes bench/results/e2e_near_intents.json.
"""
import json
import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "deploy"))
sys.path.insert(0, os.path.join(ROOT, "bridge", "intents"))
from localnet import BIN, ENVV, FT_WASM, jrun  # noqa: E402
from near_local import CONTRACT, REWARD, NearLocal  # noqa: E402
from zfx_intents import protocol as P  # noqa: E402
from zfx_intents.adapter import IntentsAdapter  # noqa: E402

RUN = os.path.join(ROOT, "deploy", "run", "near-intents")
VERIFIER = "intents.deployer.test.near"
TUSDC = "tusdc.deployer.test.near"
VWASM = os.path.join(ROOT, "bridge", "intents", "vendor", "defuse.wasm")
PY = os.path.join(ROOT, ".venv", "bin", "python")
STORAGE = 1_250_000_000_000_000_000_000
REPORT = {"checks": []}


def check(name, ok, ev):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": ev})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(ev, default=str)[:500], flush=True)
    return bool(ok)


def main():
    subprocess.run(["rm", "-rf", RUN])
    if not os.path.exists(VWASM):
        subprocess.run([os.path.join(ROOT, "bridge", "intents", "fetch_verifier.sh")], check=True)
    solver, user = "solver.test.near", "user.test.near"
    n = NearLocal(miners=(solver, user), extra_accounts=("relay.test.near",), window_secs=8, run_dir=RUN, proxy_base_port=9571)
    procs = []
    ok = True
    try:
        n.up()
        # I1: mine until both accounts hold finalized ZFX
        for i in range(2):
            n.start_miner(i, threads=8)
        t0 = time.time()
        while time.time() - t0 < 900:
            n.finalize()
            if n.zfx_balance(solver) >= 4 * REWARD and n.zfx_balance(user) >= 2 * REWARD:
                break
            time.sleep(3)
        for i in range(2):
            n.stop(f"miner{i}")
        time.sleep(n.window_secs + 2)
        n.finalize()
        st = n.stats()
        sb, ub = n.zfx_balance(solver), n.zfx_balance(user)
        ok &= check("I1 solver and user mine ZFX directly on NEAR (production PoW, optimistic checks, blocks finalized)",
                    sb >= 4 * REWARD and ub >= 2 * REWARD and st["frauds"] == 0,
                    {"final_height": st["final_height"], "solver_zfx": sb, "user_zfx": ub, "frauds": st["frauds"]})

        # Verifier + test asset
        dk = n.keys["deployer.test.near"]
        v = jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", n.rpc, "--signer", "deployer.test.near", "--key-file", dk, "--contract", VERIFIER, "--wasm", VWASM,
                  "--deposit-near", "50", "--init", json.dumps({"config": {"wnear_id": "wrap.test.near", "fees": {"fee": 0, "fee_collector": "deployer.test.near"},
                                                                           "roles": {}}})])
        t = jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", n.rpc, "--signer", "deployer.test.near", "--key-file", dk, "--contract", TUSDC, "--wasm", FT_WASM,
                  "--init", json.dumps({"owner_id": "deployer.test.near", "symbol": "tUSDC", "decimals": 6})])
        assert v["success"] and t["success"], (v, t)
        for tok in (CONTRACT, TUSDC):
            for acct in (VERIFIER, solver, user):
                n.call("deployer.test.near", tok, "storage_deposit", {"account_id": acct, "registration_only": True}, deposit=STORAGE)
        n.call("deployer.test.near", TUSDC, "mint", {"account_id": solver, "amount": str(1_000_000 * 10**6)})

        # I2: deposit mined ZFX into the Verifier
        z_solver, z_user = 3 * REWARD, 2 * REWARD
        r1 = n.call(solver, CONTRACT, "ft_transfer_call", {"receiver_id": VERIFIER, "amount": str(z_solver), "msg": ""}, gas=150, deposit=1)
        r2 = n.call(solver, TUSDC, "ft_transfer_call", {"receiver_id": VERIFIER, "amount": str(1_000_000 * 10**6), "msg": ""}, gas=150, deposit=1)
        r3 = n.call(user, CONTRACT, "ft_transfer_call", {"receiver_id": VERIFIER, "amount": str(z_user), "msg": ""}, gas=150, deposit=1)
        for acct in (solver, user):
            n.call(acct, VERIFIER, "add_public_key", {"public_key": P.public_key_of(n.keys[acct])}, deposit=1)

        def mt(acct, token):
            return int(n.view(VERIFIER, "mt_balance_of", {"account_id": acct, "token_id": P.token_id(token)}))

        ok &= check("I2 mined ZFX deposited into the official NEAR Intents Verifier (standard ft_transfer_call)",
                    r1.get("success") and r2.get("success") and r3.get("success") and mt(user, CONTRACT) == z_user and mt(solver, CONTRACT) == z_solver,
                    {"solver_zfx_in_verifier": mt(solver, CONTRACT), "user_zfx_in_verifier": mt(user, CONTRACT), "solver_tusdc": mt(solver, TUSDC)})

        # bus + solver
        pricef = os.path.join(RUN, "price.json")
        json.dump({"usdc_per_zfx": 2.5, "timestamp": time.time()}, open(pricef, "w"))
        bus = open(os.path.join(RUN, "bus.log"), "w")
        procs.append(subprocess.Popen([PY, "-m", "zfx_intents.messagebus", "--near-rpc", n.rpc, "--verifier", VERIFIER, "--relay-account", "relay.test.near",
                                       "--relay-key", n.keys["relay.test.near"], "--http-port", "28820", "--ws-port", "28821"],
                                      cwd=os.path.join(ROOT, "bridge", "intents"), stdout=bus, stderr=bus, env=ENVV))
        time.sleep(1.5)
        sol = open(os.path.join(RUN, "solver.log"), "w")
        procs.append(subprocess.Popen([PY, "-m", "zfx_intents.solver", "--ws", "ws://127.0.0.1:28821/ws", "--near-rpc", n.rpc, "--verifier", VERIFIER,
                                       "--account", solver, "--key", n.keys[solver], "--wzfx", CONTRACT, "--tusdc", TUSDC, "--price-file", pricef,
                                       "--spread-bps", "50", "--max-trade-fraction", "0.5"],
                                      cwd=os.path.join(ROOT, "bridge", "intents"), stdout=sol, stderr=sol, env=ENVV))
        time.sleep(2)
        ad = IntentsAdapter("http://127.0.0.1:28820/rpc")
        Z, U = P.token_id(CONTRACT), P.token_id(TUSDC)
        salt = n.view(VERIFIER, "current_salt")

        # I3: ZFX -> tUSDC
        q = ad.quote(Z, U, exact_amount_in=REWARD)
        res, _ = ad.submit(q[0], user, n.keys[user], VERIFIER, salt)
        s1 = ad.wait(res["intent_hash"])
        ok &= check("I3 swap ZFX -> tUSDC: quote, NEP-413 token_diff, settled on the Verifier",
                    s1["status"] == "SETTLED" and mt(user, CONTRACT) == z_user - REWARD and mt(user, TUSDC) == int(q[0]["amount_out"]),
                    {"quote": q[0], "status": s1, "user_tusdc": mt(user, TUSDC)})

        # I4: tUSDC -> ZFX
        q2 = ad.quote(U, Z, exact_amount_in=10_000_000)
        res2, _ = ad.submit(q2[0], user, n.keys[user], VERIFIER, salt)
        s2 = ad.wait(res2["intent_hash"])
        ok &= check("I4 swap tUSDC -> ZFX settles", s2["status"] == "SETTLED" and mt(user, CONTRACT) == z_user - REWARD + int(q2[0]["amount_out"]),
                    {"quote": q2[0], "status": s2["status"]})

        # I5: books
        st = n.stats()
        total = sum(n.zfx_balance(a) for a in (solver, user, VERIFIER))
        ok &= check("I5 ZFX supply equals reward x finalized blocks and all ZFX is accounted for (wallets + Verifier)",
                    int(st["total_supply"]) == int(st["expected_supply"]) == total,
                    {"supply": st["total_supply"], "wallets_plus_verifier": total, "verifier_holds": n.zfx_balance(VERIFIER)})
    except Exception as e:
        ok = False
        check("near intents e2e aborted", False, {"exception": repr(e)})
    finally:
        for p in procs:
            p.terminate()
        n.stop_all()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    json.dump(REPORT, open(os.path.join(ROOT, "bench", "results", "e2e_near_intents.json"), "w"), indent=2, default=str)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed")
    sys.exit(0 if ok and REPORT["passed"] == REPORT["total"] else 1)


if __name__ == "__main__":
    main()
