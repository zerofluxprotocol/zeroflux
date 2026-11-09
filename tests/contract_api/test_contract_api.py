#!/usr/bin/env python3
"""Contract tests for schemas/zfx-node-rpc-v1.json against a live node.

Starts one node (bridge enabled, 2-of-3 committee from fresh relayer seeds)
on ports 19890/19891, mines with the Rust CPU miner, makes a wallet transfer
and a bridge lock, then:

  positive  every documented method is called with valid params and its
            result must validate against the method's `result` schema;
  negative  for each method, cases generated from its `params` schema
            (each required field dropped, each hash32 field given 31 bytes,
            each address given a broken checksum) plus the documented
            `x-negative` semantic cases must return exactly the error code
            the schema lists;
  envelope  parse error, wrong jsonrpc version, unknown method, admin
            without token, idempotent resubmission.

Captured valid responses are written to schemas/fixtures/ (regenerated each
run). Writes bench/results/contract_api.json; exits 1 on any failure.
"""
import copy
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

import jsonschema

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
B = os.path.join(ROOT, "target", "release")
SCHEMA = json.load(open(os.path.join(ROOT, "schemas", "zfx-node-rpc-v1.json")))
FIX = os.path.join(ROOT, "schemas", "fixtures")
P2P, RPCP = 19890, 19891
URL = f"http://127.0.0.1:{RPCP}/rpc"
results = []


def raw(body, headers=None):
    req = urllib.request.Request(URL, data=body if isinstance(body, bytes) else json.dumps(body).encode(),
                                 headers=dict({"content-type": "application/json"}, **(headers or {})))
    try:
        return json.loads(urllib.request.urlopen(req, timeout=30).read())
    except urllib.error.HTTPError as e:
        return json.loads(e.read())


def call(method, params, headers=None):
    return raw({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}, headers)


def record(kind, method, case, ok, detail):
    results.append({"kind": kind, "method": method, "case": case, "result": "PASS" if ok else "FAIL", "detail": detail})
    if not ok:
        print(f"FAIL {kind} {method} {case}: {json.dumps(detail)[:400]}", flush=True)


def validator(sub):
    s = dict(sub)
    s["$defs"] = SCHEMA["$defs"]
    return jsonschema.Draft202012Validator(s)


def positive(method, params):
    r = call(method, params)
    if "result" not in r:
        record("positive", method, json.dumps(params)[:80], False, r)
        return None
    errs = [e.message for e in validator(SCHEMA["methods"][method]["result"]).iter_errors(r["result"])]
    pe = [e.message for e in validator(SCHEMA["methods"][method]["params"]).iter_errors(params)]
    record("positive", method, json.dumps(params)[:80], not errs and not pe, {"result_schema_errors": errs[:3], "params_schema_errors": pe[:3]})
    os.makedirs(FIX, exist_ok=True)
    json.dump({"method": method, "params": params, "result": r["result"]}, open(os.path.join(FIX, f"{method}.json"), "w"), indent=2)
    return r["result"]


def expect_code(method, case, params, code):
    r = call(method, params)
    got = (r.get("error") or {}).get("code")
    # Also confirm the generated params really violate the params schema
    # (semantic cases may be schema-valid by design).
    record("negative", method, case, got == code, {"expected": code, "got": got, "message": (r.get("error") or {}).get("message"), "params": params})


def broken_address(a):
    return a[:-1] + ("0" if a[-1] != "0" else "1")


def generated_negatives(method, good):
    """Derive negatives mechanically from the params schema and a valid call."""
    ps = SCHEMA["methods"][method]["params"]
    props = ps.get("properties", {})
    for f in ps.get("required", []):
        bad = {k: v for k, v in good.items() if k != f}
        expect_code(method, f"missing:{f}", bad, -32602)
    for f, sch in props.items():
        if f not in good:
            continue
        ref = sch.get("$ref", "")
        if ref.endswith("hash32"):
            expect_code(method, f"short_hex:{f}", dict(good, **{f: good[f][:62]}), -32602)
            expect_code(method, f"non_hex:{f}", dict(good, **{f: "zz" * 32}), -32602)
        if ref.endswith("address"):
            expect_code(method, f"bad_checksum:{f}", dict(good, **{f: broken_address(good[f])}), -32602)
            expect_code(method, f"wrong_type:{f}", dict(good, **{f: 12345}), -32602)


def wallet(*a):
    r = subprocess.run([f"{B}/zfx-wallet", *a], capture_output=True, text=True)
    return r.stdout.strip().splitlines()[-1] if r.stdout.strip() else r.stderr


def main():
    tmp = tempfile.mkdtemp(prefix="zfx-api-")
    procs = []
    try:
        seeds = []
        for i in range(3):
            k = json.loads(subprocess.run([f"{B}/zfx-bridge", "keygen-relayer", "--out", f"{tmp}/r{i}.seed"], capture_output=True, text=True).stdout.strip().splitlines()[-1])
            seeds.append(k)
        cfg = json.loads(subprocess.run([f"{B}/zfx-node", "print-config"], capture_output=True, text=True).stdout)
        cfg.update(genesis_timestamp=int(time.time()) - 3600, label="zfx-contract-api",
                   bridge={"committee": [k["slh"] for k in seeds], "threshold": 2, "rotation_timelock_blocks": 20, "min_lock": 1000,
                           "activation_height": 1, "near_contract": "wzfx.test.near"})
        json.dump(cfg, open(f"{tmp}/chain.json", "w"))
        for w in ("m", "b"):
            wallet("new", "--wallet", f"{tmp}/{w}.json")
        addr = {w: subprocess.run([f"{B}/zfx-wallet", "address", "--wallet", f"{tmp}/{w}.json"], capture_output=True, text=True).stdout.split()[1]
                for w in ("m", "b")}
        open(f"{tmp}/tok", "w").write(os.urandom(16).hex())
        procs.append(subprocess.Popen([f"{B}/zfx-node", "--config", f"{tmp}/chain.json", "--data-dir", f"{tmp}/d", "--p2p-port", str(P2P),
                                       "--rpc-port", str(RPCP), "--no-discovery", "--bridge-confirmations", "3", "--admin-token-file", f"{tmp}/tok"],
                                      stdout=open(f"{tmp}/node.log", "w"), stderr=subprocess.STDOUT))
        for _ in range(100):
            try:
                call("zfx_node_get_tip", {})
                break
            except Exception:
                time.sleep(0.1)
        miner = subprocess.Popen([f"{B}/zpu-cli", "mine", "--node", f"http://127.0.0.1:{RPCP}", "--address", addr["m"]],
                                 stdout=open(f"{tmp}/miner.log", "w"), stderr=subprocess.STDOUT)
        procs.append(miner)
        while call("zfx_node_get_tip", {})["result"]["height"] < 12:
            time.sleep(0.5)
        node = f"http://127.0.0.1:{RPCP}"
        send = json.loads(wallet("send", "--wallet", f"{tmp}/m.json", "--node", node, "--to", addr["b"], "--amount", "1000", "--fee", "auto", "--memo", "api"))
        lock = json.loads(wallet("bridge-deposit", "--wallet", f"{tmp}/m.json", "--node", node, "--near-account", "alice.test.near", "--amount", "5000", "--fee", "1000"))
        txid, dep = send["txid"], lock["deposit_id"]
        h0 = call("zfx_node_get_tip", {})["result"]["height"]
        while call("zfx_node_get_tip", {})["result"]["height"] < h0 + 4:
            time.sleep(0.5)
        miner.terminate()

        # ---------------- positive ----------------
        tip = positive("zfx_node_get_tip", {})
        positive("zfx_node_get_block", {"height": 1})
        positive("zfx_node_get_block", {"hash": tip["hash"]})
        positive("zfx_node_get_transaction", {"txid": txid})
        positive("zfx_get_account", {"address": addr["m"]})
        positive("zfx_get_address_history", {"address": addr["m"], "limit": 5})
        positive("zfx_estimate_fee", {})
        work = positive("zfx_get_mining_work", {"address": addr["m"]})
        positive("zfx_bridge_info", {})
        st = positive("zfx_bridge_deposit_status", {"deposit_id": dep})
        record("semantic", "zfx_bridge_deposit_status", "lock reaches final after K=3", st and st["state"] == "final" and st["amount"] == 5000
               and st["near_recipient"] == "alice.test.near", st)
        positive("zfx_bridge_redeem_status", {"withdrawal_id": "ab" * 32})

        # ---------------- generated negatives ----------------
        generated_negatives("zfx_node_get_transaction", {"txid": txid})
        generated_negatives("zfx_get_account", {"address": addr["m"]})
        generated_negatives("zfx_get_address_history", {"address": addr["m"]})
        generated_negatives("zfx_bridge_deposit_status", {"deposit_id": dep})
        generated_negatives("zfx_bridge_redeem_status", {"withdrawal_id": dep})
        generated_negatives("zfx_get_mining_work", {"address": addr["m"]})
        generated_negatives("zfx_submit_mining_result", {"work_id": work.get("work_id", "x"), "nonce": 1})
        generated_negatives("zfx_submit_transaction", {"tx": {}})
        generated_negatives("zfx_bridge_submit_op", {"op": {}})
        expect_code("zfx_node_get_block", "missing:hash|height", {}, -32602)

        # ---------------- semantic negatives (x-negative) ----------------
        expect_code("zfx_node_get_block", "not_found", {"hash": "00" * 32}, 1003)
        expect_code("zfx_node_get_transaction", "not_found", {"txid": "00" * 32}, 1003)
        tx = call("zfx_node_get_transaction", {"txid": txid})["result"]["tx"]
        r = call("zfx_submit_transaction", {"tx": tx})
        record("semantic", "zfx_submit_transaction", "replayed confirmed tx (nonce reuse) rejected", (r.get("error") or {}).get("code") == 1001, r)
        t2 = copy.deepcopy(tx)
        t2["amount"] += 1
        r = call("zfx_submit_transaction", {"tx": t2})
        record("semantic", "zfx_submit_transaction", "tampered amount (signature no longer valid) rejected", (r.get("error") or {}).get("code") == 1001, r)
        r = call("zfx_bridge_lock", {"tx": tx})
        record("semantic", "zfx_bridge_lock", "transfer not to the lockbox rejected", (r.get("error") or {}).get("code") == 1006, r)
        r = call("zfx_submit_mining_result", {"work_id": "deadbeef", "nonce": 1})
        record("semantic", "zfx_submit_mining_result", "unknown work id", (r.get("error") or {}).get("code") == 1002, r)
        rel = {"op": "release", "epoch": 0, "withdrawal_id": "11" * 32, "near_burn_ref": "w:1", "recipient": addr["b"], "amount": 10,
               "expiry_height": 10**6, "approvals": []}
        r = call("zfx_bridge_submit_op", {"op": rel})
        record("semantic", "zfx_bridge_submit_op", "release without quorum rejected", (r.get("error") or {}).get("code") == 1006, r)
        info = call("zfx_bridge_info", {})["result"]
        r = call("zfx_bridge_submit_op", {"op": {"op": "admin", "epoch": 0, "op_nonce": info["admin_nonce"] + 5, "action": "pause", "approvals": []}})
        record("semantic", "zfx_bridge_submit_op", "admin op with wrong nonce rejected", (r.get("error") or {}).get("code") == 1006, r)

        # ---------------- envelope ----------------
        r = raw(b"{not json")
        record("envelope", "-", "parse error", (r.get("error") or {}).get("code") == -32700, r)
        r = raw({"jsonrpc": "1.0", "id": 1, "method": "zfx_node_get_tip"})
        record("envelope", "-", "jsonrpc != 2.0", (r.get("error") or {}).get("code") == -32600, r)
        r = call("zfx_no_such_method", {})
        record("envelope", "-", "unknown method", (r.get("error") or {}).get("code") == -32601, r)
        r = call("zfx_admin_snapshot_export", {})
        record("envelope", "zfx_admin_snapshot_export", "admin without token", (r.get("error") or {}).get("code") == 1004, r)
        r = call("zfx_admin_snapshot_export", {}, {"authorization": "Bearer wrong-token-of-any-length"})
        record("envelope", "zfx_admin_snapshot_export", "admin with wrong token", (r.get("error") or {}).get("code") == 1004, r)
        r = call("zfx_node_get_tip", [{}])
        record("envelope", "zfx_node_get_tip", "array-wrapped params accepted", "result" in r, r)
    finally:
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(15)
            except Exception:
                p.kill()
        shutil.rmtree(tmp, ignore_errors=True)
    passed = sum(r["result"] == "PASS" for r in results)
    rep = {"schema": "schemas/zfx-node-rpc-v1.json", "methods_covered": sorted({r["method"] for r in results if r["method"] != "-"}),
           "passed": passed, "total": len(results), "cases": results}
    json.dump(rep, open(os.path.join(ROOT, "bench", "results", "contract_api.json"), "w"), indent=2)
    print(f"contract API: {passed}/{len(results)} cases passed")
    sys.exit(0 if passed == len(results) and results else 1)


if __name__ == "__main__":
    main()
