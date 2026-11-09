#!/usr/bin/env python3
"""NEAR testnet smoke test (make testnet-smoke). Gate 6.

Runs only with an explicitly configured ZeroFlux testnet account:

  NEAR_TESTNET_ACCOUNT=<account>.testnet      (required, never auto-picked)
  ~/.near-credentials/testnet/<account>.json  (near-cli format: account_id,
                                               public_key, private_key)
  NEAR_TESTNET_RPC (optional, default https://rpc.testnet.near.org)

With those present it: creates subaccounts wzfx.<acct>, coordinator.<acct>
and user0.<acct> (testnet NEAR only), deploys and initialises wZFX, verifies
the on-chain code hash equals the local WASM, starts a local 3-node
ZeroFlux chain plus 3 relayers and the coordinator against testnet, and runs
deposit -> 2-of-3 mint -> burn -> 2-of-3 native release, then reconciles.
Every NEAR transaction hash is recorded with its explorer link.

Without them it writes a BLOCKED record naming the exact prerequisite and
exits 0 (exit 2 if ZFX_REQUIRE_TESTNET=1). It never deploys onto an account
that was not named in NEAR_TESTNET_ACCOUNT, and keys never leave
~/.near-credentials or deploy/run/testnet (gitignored, 0600).

Writes bench/results/testnet_smoke.json.
"""
import hashlib
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
from localnet import BIN, WASM, Localnet, jrun  # noqa: E402
import zfxnet  # noqa: E402

OUT = os.path.join(ROOT, "bench", "results", "testnet_smoke.json")
RPC = os.environ.get("NEAR_TESTNET_RPC", "https://rpc.testnet.near.org")
EXPLORER = "https://testnet.nearblocks.io/txns/"
MIN_BALANCE_NEAR = 15
PREREQ = ("A NEAR testnet account dedicated to ZeroFlux with >= 15 testnet NEAR, its full-access key in "
          "~/.near-credentials/testnet/<account>.json (near-cli format), and NEAR_TESTNET_ACCOUNT=<account> in the environment. "
          "")


def write(rep, code):
    rep["finished"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    json.dump(rep, open(OUT, "w"), indent=2, default=str)
    print(json.dumps({k: rep[k] for k in ("result", "reason") if k in rep}))
    sys.exit(code)


PREFLIGHT_ONLY = "--preflight" in sys.argv


def blocked(reason):
    if PREFLIGHT_ONLY:
        print(json.dumps({"ready": False, "reason": reason}))
        sys.exit(3)
    rep = {"gate": 6, "result": "BLOCKED", "reason": reason, "prerequisite": PREREQ, "rpc": RPC}
    write(rep, 2 if os.environ.get("ZFX_REQUIRE_TESTNET") == "1" else 0)


def rpc(method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": "zfx", "method": method, "params": params}).encode()
    req = urllib.request.Request(RPC, data=body, headers={"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(req, timeout=20).read())


def preflight():
    acct = os.environ.get("NEAR_TESTNET_ACCOUNT", "").strip()
    if not acct:
        blocked("NEAR_TESTNET_ACCOUNT is not set")
    if not acct.endswith(".testnet"):
        blocked(f"NEAR_TESTNET_ACCOUNT={acct} is not a .testnet account")
    cred = os.path.expanduser(f"~/.near-credentials/testnet/{acct}.json")
    if not os.path.exists(cred):
        blocked(f"credentials file {cred} not found")
    try:
        c = json.load(open(cred))
        assert c.get("private_key", "").startswith("ed25519:") and c.get("account_id") == acct
    except Exception:
        blocked(f"{cred} is not a near-cli ed25519 credentials file for {acct}")
    try:
        r = rpc("query", {"request_type": "view_account", "finality": "final", "account_id": acct})
    except Exception as e:
        blocked(f"testnet RPC {RPC} unreachable: {e!r}")
    if "error" in r:
        blocked(f"account {acct} not found on testnet: {str(r['error'])[:200]}")
    bal = int(r["result"]["amount"]) / 1e24
    if bal < MIN_BALANCE_NEAR:
        blocked(f"account {acct} holds {bal:.2f} testnet NEAR; needs >= {MIN_BALANCE_NEAR}")
    return acct, cred, bal


def main():
    acct, cred, bal = preflight()
    if PREFLIGHT_ONLY:
        print(json.dumps({"ready": True, "account": acct, "balance_near": bal}))
        return
    rep = {"gate": 6, "result": "RUNNING", "account": acct, "rpc": RPC, "balance_near_before": bal, "txs": [], "checks": []}
    run = os.path.join(localnet.RUN, "testnet")
    subprocess.run(["rm", "-rf", localnet.RUN])
    os.makedirs(os.path.join(localnet.RUN, "zfx"), exist_ok=True)
    os.makedirs(run, exist_ok=True)
    zfxnet.RUN = os.path.join(localnet.RUN, "zfx")

    contract = f"wzfx.{acct}"
    coord = f"coordinator.{acct}"
    user = f"user0.{acct}"
    localnet.CONTRACT = contract

    def tx(label, out):
        if out.get("tx_hash"):
            rep["txs"].append({"step": label, "tx_hash": out["tx_hash"], "explorer": EXPLORER + out["tx_hash"], "success": out.get("success", out.get("created"))})
        return out

    class TestnetNet(Localnet):
        owner = acct
        coordinator_account = coord

        def start_near(self):
            self.rpc = RPC
            self.near = {"rpc": RPC, "accounts": {acct: cred}}
            for a, dep in ((coord, 3), (user, 2)):
                kf = os.path.join(run, f"{a}.key.json")
                r = tx(f"create {a}", jrun([f"{BIN}/zfx-bridge", "create-subaccount", "--rpc", RPC, "--signer", acct, "--key-file", cred,
                                             "--account", a, "--deposit-near", str(dep), "--out", kf], check=False))
                if not r.get("created") and r.get("reason") != "exists":
                    raise RuntimeError(f"create {a}: {r}")
                if not os.path.exists(kf):
                    raise RuntimeError(f"{a} exists on testnet but its key is not in {run}; delete the subaccount or restore the key")
                self.near["accounts"][a] = kf
            return self.near

    ln = TestnetNet(confirmations=6)
    ok = True
    try:
        ln.start_near()
        ln.make_relayer_keys()
        _, uaddr = zfxnet.wallet("testnet_user0", "5a" * 32)
        uwallet = os.path.join(zfxnet.RUN, "testnet_user0.wallet.json")
        ln.start_native([{"address": uaddr, "amount": 10**12}])
        ln.start_miner(1)
        d = tx("deploy wZFX", ln.deploy_wzfx(timelock=3600))
        local_sha = hashlib.sha256(open(WASM, "rb").read()).hexdigest()
        acc = rpc("query", {"request_type": "view_account", "finality": "final", "account_id": contract})["result"]
        ok &= check(rep, "wZFX deployed on testnet; on-chain code hash equals the local WASM sha256",
                    d.get("success") and acc["code_hash"] == d.get("code_hash_b58"),
                    {"contract": contract, "onchain_code_hash_b58": acc["code_hash"], "local_wasm_sha256": local_sha, "deploy_tx": d.get("tx_hash")})
        tx("register user", ln.near_call(user, contract, "storage_deposit", {"account_id": user}, deposit=1_250_000_000_000_000_000_000))
        ln.env_file()
        for i in range(3):
            ln.start_relayer(i)
        ln.start_coordinator(fault_rate=0.0)

        def wz(a):
            return int(ln.near_view(contract, "ft_balance_of", {"account_id": a}))

        amount = 50_000
        r = subprocess.run([f"{BIN}/zfx-wallet", "bridge-deposit", "--wallet", uwallet, "--node", ln.zfx_url(1), "--near-account", user,
                            "--amount", str(amount), "--fee", "1000"], capture_output=True, text=True)
        dep = json.loads(r.stdout.strip().splitlines()[-1])
        t0 = time.time()
        while wz(user) < amount and time.time() - t0 < 900:
            time.sleep(3)
        jobs = json.loads(urllib.request.urlopen("http://127.0.0.1:28790/jobs", timeout=10).read())
        for j in (jobs.values() if isinstance(jobs, dict) else jobs):
            h = (j.get("submission") or {}).get("near_tx")
            if h:
                rep["txs"].append({"step": f"bridge_mint ({j['kind']})", "tx_hash": h, "explorer": EXPLORER + h})
        ok &= check(rep, "deposit on local ZeroFlux -> 2-of-3 mint on NEAR testnet", wz(user) == amount,
                    {"deposit": dep, "wzfx_balance": wz(user), "seconds": round(time.time() - t0, 1)})
        b = tx("bridge_burn", jrun([f"{BIN}/zfx-bridge", "burn", "--rpc", RPC, "--signer", user, "--key-file", ln.key(user), "--contract", contract,
                                    "--amount", str(amount), "--native-recipient", uaddr], check=False))
        t0 = time.time()
        released = lambda: int(json.loads(urllib.request.urlopen(urllib.request.Request(  # noqa: E731
            f"{ln.zfx_url(1)}/rpc", data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "zfx_bridge_info", "params": {}}).encode(),
            headers={"content-type": "application/json"}), timeout=10).read())["result"]["released_total"])
        while released() < amount and time.time() - t0 < 900:
            time.sleep(3)
        rec = ln.reconcile()
        ok &= check(rep, "burn on NEAR testnet (final) -> 2-of-3 SLH-DSA native release; reconciliation exact",
                    b.get("success") and released() == amount and rec.get("ok") is True,
                    {"burn_tx": b.get("tx_hash"), "released_total": released(), "reconcile_ok": rec.get("ok")})
    except SystemExit:
        raise
    except Exception as e:
        ok = False
        check(rep, "testnet smoke aborted", False, {"exception": repr(e)})
    finally:
        ln.stop_all()
    rep["result"] = "PASS" if ok else "FAIL"
    write(rep, 0 if ok else 1)


def check(rep, name, ok, ev):
    rep["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": ev})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(ev, default=str)[:400], flush=True)
    return bool(ok)


if __name__ == "__main__":
    main()
