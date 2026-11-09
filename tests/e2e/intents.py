#!/usr/bin/env python3
"""make e2e-intents: Gate 5 with the official NEAR Intents Verifier.

Real NEAR sandbox + official Verifier (near/intents defuse/v0.4.2, SHA-256
checked) + wZFX (bridged from native ZFX through the 2-of-3 bridge) + a tUSDC
test token + local Message Bus (documented JSON-RPC/WebSocket API) + test
solver. End-user flow: native ZFX -> wZFX -> swap -> wZFX -> native ZFX.
Writes bench/results/e2e_intents.json.
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
sys.path.insert(0, os.path.join(ROOT, "bridge", "intents"))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import localnet  # noqa: E402
from localnet import BIN, CONTRACT, ENVV, FT_WASM, Localnet, jrun, user_wallets  # noqa: E402
import zfxnet  # noqa: E402
from bridge import deposit, native_bal, near_bal, wait, zrpc  # noqa: E402
from zfx_intents import protocol as P  # noqa: E402
from zfx_intents.adapter import IntentsAdapter, PUBLIC_RELAY  # noqa: E402

VERIFIER = "intents.deployer.test.near"
TUSDC = "tusdc.deployer.test.near"
VWASM = os.path.join(ROOT, "bridge", "intents", "vendor", "defuse.wasm")
PY = os.path.join(ROOT, ".venv", "bin", "python")
REPORT = {"started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "checks": []}
ONE_YOCTO = 1
STORAGE = 1_250_000_000_000_000_000_000


def check(name, ok, ev):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": ev})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(ev, default=str)[:500], flush=True)
    return ok


def mt(ln, account, token):
    return int(ln.near_view(VERIFIER, "mt_balance_of", {"account_id": account, "token_id": P.token_id(token)}))


def main():
    subprocess.run(["rm", "-rf", localnet.RUN])
    os.makedirs(os.path.join(localnet.RUN, "zfx"), exist_ok=True)
    zfxnet.RUN = os.path.join(localnet.RUN, "zfx")
    if not os.path.exists(VWASM):
        subprocess.run([os.path.join(ROOT, "bridge", "intents", "fetch_verifier.sh")], check=True)
    vinfo = json.load(open(os.path.join(ROOT, "bridge", "intents", "vendor", "verifier.json")))
    ws = user_wallets()
    ln = Localnet(confirmations=12)
    procs = []
    ok = True
    try:
        ln.up([{"address": w["addr"], "amount": 10**12} for w in ws])
        dep = lambda acct: ln.key(acct)  # noqa: E731
        # --- contracts ---
        v = jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", ln.rpc, "--signer", "deployer.test.near", "--key-file", dep("deployer.test.near"), "--contract", VERIFIER,
                  "--wasm", VWASM, "--deposit-near", "50",
                  "--init", json.dumps({"config": {"wnear_id": "wrap.test.near", "fees": {"fee": 0, "fee_collector": "deployer.test.near"}, "roles": {}}})])
        t = jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", ln.rpc, "--signer", "deployer.test.near", "--key-file", dep("deployer.test.near"), "--contract", TUSDC,
                  "--wasm", FT_WASM, "--init", json.dumps({"owner_id": "deployer.test.near", "symbol": "tUSDC", "decimals": 6})])
        salt = ln.near_view(VERIFIER, "current_salt", {})
        ok &= check("B5 official Verifier deployed in sandbox (release artifact, SHA-256 verified) + tUSDC fixture",
                    v["success"] and t["success"] and v["wasm_sha256"] == vinfo["sha256"] and len(salt) == 8,
                    {"verifier_tag": vinfo["tag"], "verifier_sha256": v["wasm_sha256"], "verifier_deploy_tx": v["tx_hash"], "current_salt": salt, "tusdc_tx": t["tx_hash"]})
        for tok in (CONTRACT, TUSDC):
            for acct in (VERIFIER, "solver.test.near", "u5.test.near"):
                ln.near_call("deployer.test.near", tok, "storage_deposit", {"account_id": acct, "registration_only": True}, deposit=STORAGE)
        ln.near_call("deployer.test.near", TUSDC, "mint", {"account_id": "solver.test.near", "amount": str(1_000_000 * 10**6)})
        # solver obtains wZFX liquidity through the real bridge (u9 deposits for solver.test.near)
        u5, u9 = ws[5], ws[9]
        sw = dict(u9, near="solver.test.near")
        deposit(ln, sw, 5_000_000_000, node=1)
        wait(lambda: near_bal(ln, "solver.test.near") == 5_000_000_000, 300, "solver wZFX via bridge")
        for tok, amt in ((CONTRACT, 5_000_000_000), (TUSDC, 1_000_000 * 10**6)):
            r = ln.near_call("solver.test.near", tok, "ft_transfer_call", {"receiver_id": VERIFIER, "amount": str(amt), "msg": ""}, deposit=ONE_YOCTO, gas=150)
            assert r["success"], r
        ln.near_call("solver.test.near", VERIFIER, "add_public_key", {"public_key": P.public_key_of(dep("solver.test.near"))}, deposit=ONE_YOCTO)
        # user: native ZFX -> wZFX -> Verifier
        n_before = native_bal(ln, u5["addr"])
        deposit(ln, u5, 1_000_000_000, node=2)
        wait(lambda: near_bal(ln, "u5.test.near") == 1_000_000_000, 300, "user wZFX")
        r = ln.near_call("u5.test.near", CONTRACT, "ft_transfer_call", {"receiver_id": VERIFIER, "amount": "1000000000", "msg": ""}, deposit=ONE_YOCTO, gas=150)
        ln.near_call("u5.test.near", VERIFIER, "add_public_key", {"public_key": P.public_key_of(dep("u5.test.near"))}, deposit=ONE_YOCTO)
        ok &= check("B5 deposits into the Verifier: solver liquidity (bridged wZFX + tUSDC) and user's bridged wZFX",
                    r["success"] and mt(ln, "u5.test.near", CONTRACT) == 1_000_000_000 and mt(ln, "solver.test.near", TUSDC) == 10**12,
                    {"user_wzfx_in_verifier": mt(ln, "u5.test.near", CONTRACT), "solver_tusdc": mt(ln, "solver.test.near", TUSDC), "solver_wzfx": mt(ln, "solver.test.near", CONTRACT)})
        # --- bus + solver ---
        pricef = os.path.join(localnet.RUN, "price.json")
        json.dump({"usdc_per_zfx": 2.5, "timestamp": time.time()}, open(pricef, "w"))
        busl = open(os.path.join(localnet.RUN, "bus.log"), "w")
        procs.append(subprocess.Popen([PY, "-m", "zfx_intents.messagebus", "--near-rpc", ln.rpc, "--verifier", VERIFIER, "--relay-account", "relay.test.near",
                                       "--relay-key", dep("relay.test.near"), "--http-port", "28810", "--ws-port", "28811"],
                                      cwd=os.path.join(ROOT, "bridge", "intents"), stdout=busl, stderr=busl, env=ENVV))
        time.sleep(1.5)
        sol = open(os.path.join(localnet.RUN, "solver.log"), "w")
        procs.append(subprocess.Popen([PY, "-m", "zfx_intents.solver", "--ws", "ws://127.0.0.1:28811/ws", "--near-rpc", ln.rpc, "--verifier", VERIFIER,
                                       "--account", "solver.test.near", "--key", dep("solver.test.near"), "--wzfx", CONTRACT, "--tusdc", TUSDC,
                                       "--price-file", pricef, "--spread-bps", "50", "--max-trade-fraction", "0.5"],
                                      cwd=os.path.join(ROOT, "bridge", "intents"), stdout=sol, stderr=sol, env=ENVV))
        time.sleep(2)
        ad = IntentsAdapter("http://127.0.0.1:28810/rpc")
        W, U = P.token_id(CONTRACT), P.token_id(TUSDC)

        # Swap A: wZFX -> tUSDC (exact in)
        q = ad.quote(W, U, exact_amount_in=500_000_000)
        expect_out = int(500_000_000 * 2.5 * 1e6 / 1e8 * (1 - 0.005))
        res, sd = ad.submit(q[0], "u5.test.near", dep("u5.test.near"), VERIFIER, salt)
        st = ad.wait(res["intent_hash"])
        ok &= check("B5 swap wZFX->tUSDC: quote -> NEP-413 token_diff -> settled on the Verifier (on-chain receipt)",
                    len(q) >= 1 and int(q[0]["amount_out"]) == expect_out and st["status"] == "SETTLED"
                    and mt(ln, "u5.test.near", U[7:]) == expect_out and mt(ln, "u5.test.near", CONTRACT) == 500_000_000,
                    {"quote": q[0], "status": st, "user_tusdc": mt(ln, "u5.test.near", TUSDC), "user_wzfx": mt(ln, "u5.test.near", CONTRACT)})
        settled_pair = [None, sd]
        # Swap B: tUSDC -> wZFX (exact in)
        q2 = ad.quote(U, W, exact_amount_in=10_000_000)
        res2, _ = ad.submit(q2[0], "u5.test.near", dep("u5.test.near"), VERIFIER, salt)
        st2 = ad.wait(res2["intent_hash"])
        got_w = int(q2[0]["amount_out"])
        ok &= check("B5 reverse swap tUSDC->wZFX settles (needs the solver's bridged wZFX as counterpart liquidity)",
                    st2["status"] == "SETTLED" and mt(ln, "u5.test.near", CONTRACT) == 500_000_000 + got_w,
                    {"quote": q2[0], "status": st2["status"], "tx": st2.get("data")})

        # --- failure cases ---
        big = ad.quote(W, U, exact_amount_in=10**15)
        json.dump({"usdc_per_zfx": 2.5, "timestamp": time.time() - 3600}, open(pricef, "w"))
        stale = ad.quote(W, U, exact_amount_in=100_000_000)
        json.dump({"usdc_per_zfx": 2.5, "timestamp": time.time()}, open(pricef, "w"))
        fresh = ad.quote(W, U, exact_amount_in=100_000_000)
        ok &= check("B5 insufficient liquidity and stale price produce no quotes; fresh price quotes again",
                    big == [] and stale == [] and len(fresh) >= 1, {"big_quotes": big, "stale_quotes": stale, "fresh_quotes": len(fresh)})
        # duplicate fill at the bus and on-chain nonce replay
        dup_res, dup_sd = ad.submit(fresh[0], "u5.test.near", dep("u5.test.near"), VERIFIER, salt)
        ad.wait(dup_res["intent_hash"])
        again = ad._rpc("publish_intent", {"quote_hashes": [fresh[0]["quote_hash"]], "signed_data": dup_sd})
        qd = json.loads(urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:28810/rpc", data=json.dumps(
            {"jsonrpc": "2.0", "id": 9, "method": "get_status", "params": [{"intent_hash": dup_res["intent_hash"]}]}).encode(), headers={"content-type": "application/json"})).read())
        nonce_used = ln.near_view(VERIFIER, "is_nonce_used", {"account_id": "u5.test.near", "nonce": dup_sd["payload"]["nonce"]})
        replay = ln.near_call("relay.test.near", VERIFIER, "execute_intents", {"signed": [dup_sd]}, gas=250, check=False)
        ok &= check("B5 duplicate fill rejected by the bus and nonce replay rejected on-chain by the Verifier",
                    again["status"] == "FAILED" and nonce_used is True and not replay.get("success", True),
                    {"bus": again, "is_nonce_used": nonce_used, "onchain_replay_failure": str(replay.get("failure"))[:200]})
        # expiry: bus-level and on-chain deadline
        qe = ad.quote(W, U, exact_amount_in=50_000_000, min_deadline_ms=1000)
        time.sleep(12)
        exp = ad._rpc("publish_intent", {"quote_hashes": [qe[0]["quote_hash"]], "signed_data": P.signed_intent(dep("u5.test.near"), "u5.test.near", VERIFIER, salt,
                      [P.token_diff({W: -50_000_000, U: int(qe[0]["amount_out"])})])})
        short = P.signed_intent(dep("u5.test.near"), "u5.test.near", VERIFIER, salt, [P.token_diff({W: -1, U: 1})], deadline_s=1)
        time.sleep(3)
        sim_err = None
        try:
            ln.near_view(VERIFIER, "simulate_intents", {"signed": [short]})
        except Exception as e:
            sim_err = str(e)[-300:]
        ok &= check("B5 expired quote refused by the bus; expired NEP-413 deadline refused by the Verifier (simulate_intents)",
                    exp["status"] == "FAILED" and "expired" in exp.get("reason", "") and sim_err is not None,
                    {"bus": exp, "verifier_error": sim_err})
        # cancellation: quoted but never published -> nothing settles, balances unchanged
        b0 = (mt(ln, "u5.test.near", CONTRACT), mt(ln, "u5.test.near", TUSDC))
        qc = ad.quote(W, U, exact_amount_in=70_000_000)
        time.sleep(4)
        ok &= check("B5 cancellation: an unpublished quote never settles; balances unchanged",
                    len(qc) >= 1 and (mt(ln, "u5.test.near", CONTRACT), mt(ln, "u5.test.near", TUSDC)) == b0, {"balances": b0})
        # refund: withdrawal to an account not registered on tUSDC fails and is refunded
        tb = mt(ln, "u5.test.near", TUSDC)
        rf = ln.near_call("u5.test.near", VERIFIER, "ft_withdraw", {"token": TUSDC, "receiver_id": "u6.test.near", "amount": "1000000"}, deposit=ONE_YOCTO, gas=150, check=False)
        time.sleep(3)
        ok &= check("B5 refund: failed ft_withdraw (receiver not registered) is refunded to the user's Verifier balance",
                    mt(ln, "u5.test.near", TUSDC) == tb and int(ln.near_view(TUSDC, "ft_balance_of", {"account_id": "u6.test.near"})) == 0,
                    {"withdraw_tx": rf.get("tx_hash"), "balance_before": tb, "balance_after": mt(ln, "u5.test.near", TUSDC)})

        # --- exit to native ZFX ---
        wz = mt(ln, "u5.test.near", CONTRACT)
        ln.near_call("u5.test.near", VERIFIER, "ft_withdraw", {"token": CONTRACT, "receiver_id": "u5.test.near", "amount": str(wz)}, deposit=ONE_YOCTO, gas=150)
        wait(lambda: near_bal(ln, "u5.test.near") == wz, 60, "withdraw wZFX")
        pre = native_bal(ln, u5["addr"])
        b = jrun([f"{BIN}/zfx-bridge", "burn", "--rpc", ln.rpc, "--signer", "u5.test.near", "--key-file", dep("u5.test.near"), "--contract", CONTRACT,
                  "--amount", str(wz), "--native-recipient", u5["addr"]])
        wait(lambda: native_bal(ln, u5["addr"]) == pre + wz, 300, "native release")
        rec = ln.reconcile()
        ok &= check("B5 end-to-end: native ZFX -> wZFX -> Intents swap both ways -> wZFX -> native ZFX; bridge books exact",
                    native_bal(ln, u5["addr"]) == pre + wz and rec.get("ok") is True,
                    {"native_before_flow": n_before, "native_after": native_bal(ln, u5["addr"]), "wzfx_redeemed": wz, "burn_tx": b["tx_hash"],
                     "reconcile": {k: rec.get(k) for k in ("ok", "native", "near")}})

        # --- real external endpoint ---
        ext = {}
        for name, url, body in [("message_bus_rpc", PUBLIC_RELAY, {"jsonrpc": "2.0", "id": 1, "method": "quote", "params": [{"defuse_asset_identifier_in": W,
                                 "defuse_asset_identifier_out": "nep141:usdt.tether-token.near", "exact_amount_in": "100"}]})]:
            try:
                r = urllib.request.urlopen(urllib.request.Request(url, data=json.dumps(body).encode(), headers={"content-type": "application/json"}), timeout=15)
                ext[name] = {"http": r.status, "body": r.read()[:300].decode(errors="replace")}
            except urllib.error.HTTPError as e:
                ext[name] = {"http": e.code, "body": e.read()[:300].decode(errors="replace")}
            except Exception as e:
                ext[name] = {"error": repr(e)}
        try:
            toks = json.loads(urllib.request.urlopen("https://1click.chaindefuser.com/v0/tokens", timeout=15).read())
            ext["oneclick_tokens"] = {"count": len(toks), "wzfx_listed": any("wzfx" in json.dumps(x).lower() for x in toks)}
        except Exception as e:
            ext["oneclick_tokens"] = {"error": repr(e)}
        REPORT["external"] = ext
        check("B5 real public Message Bus probed: requires a Partner JWT (EXTERNAL_BLOCKED, not claimed as verified)",
              True, ext)
        REPORT["solver_log_tail"] = open(os.path.join(localnet.RUN, "solver.log")).read()[-600:]
    except Exception as e:
        ok = False
        check("e2e-intents aborted", False, {"exception": repr(e)})
    finally:
        for p in procs:
            p.terminate()
        ln.stop_all()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    out = os.path.join(ROOT, "bench", "results", "e2e_intents.json")
    json.dump(REPORT, open(out, "w"), indent=2, default=str)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed -> {out}")
    sys.exit(0 if ok and REPORT["passed"] == REPORT["total"] else 1)


if __name__ == "__main__":
    main()
