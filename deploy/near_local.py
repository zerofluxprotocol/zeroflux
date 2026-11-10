#!/usr/bin/env python3
"""Local ZeroFlux stack: ZFX mined on NEAR with optimistic proof-of-work checks.

    python3 deploy/near_local.py up      # start everything, print how to mine; Ctrl-C stops
    python3 deploy/near_local.py down

Components (deploy/run/near-local/):
  - NEAR sandbox (real nearcore) with test accounts
  - zfx-mine-opt: ZFX as a NEP-141 token, minted only for proof of work on the
    production profile, checked optimistically (bonds, one-row fraud proofs)
  - a watcher that re-checks every pending block and challenges fraud
  - one mining proxy per miner account, serving the node mining API so the
    unchanged zpu-cli (CPU, CUDA, ROCm, Metal backends) can mine
Keys stay in deploy/run/ (gitignored, 0600).
"""
import json
import os
import signal
import subprocess
import sys
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "deploy"))
from localnet import BIN, ENVV, jrun, sh  # noqa: E402

RUN = os.path.join(ROOT, "deploy", "run", "near-local")
WASM = os.path.join(ROOT, "bridge", "near", "target", "near", "zfx_mine_opt", "zfx_mine_opt.wasm")
CONTRACT = "zfx.deployer.test.near"
REWARD = 50 * 10**8


class NearLocal:
    def __init__(self, miners=("miner1.test.near",), extra_accounts=(), window_secs=30, genesis_target="001" + "f" * 61, bond_yocto=10**24,
                 run_dir=RUN, proxy_base_port=9561):
        self.miners = list(miners)
        self.accounts = ["deployer.test.near", "watcher.test.near"] + self.miners + list(extra_accounts)
        self.window_secs, self.genesis_target, self.bond = window_secs, genesis_target, bond_yocto
        self.run, self.port0 = run_dir, proxy_base_port
        self.procs = {}

    # ---------------- processes ----------------
    def spawn(self, name, args):
        f = open(os.path.join(self.run, f"{name}.log"), "a")
        self.procs[name] = subprocess.Popen(args, stdout=f, stderr=subprocess.STDOUT, env=ENVV)
        return self.procs[name]

    def stop(self, name):
        p = self.procs.pop(name, None)
        if p and p.poll() is None:
            p.send_signal(signal.SIGTERM)
            try:
                p.wait(20)
            except subprocess.TimeoutExpired:
                p.kill()

    def stop_all(self):
        for n in list(self.procs):
            self.stop(n)

    # ---------------- NEAR ----------------
    def start_near(self):
        os.makedirs(self.run, exist_ok=True)
        self.spawn("near-sandbox", [f"{BIN}/zfx-bridge", "near-sandbox", "--dir", self.run, "--accounts", ",".join(self.accounts)])
        for _ in range(180):
            if os.path.exists(os.path.join(self.run, "sandbox.json")):
                break
            time.sleep(0.5)
        s = json.load(open(os.path.join(self.run, "sandbox.json")))
        self.rpc, self.keys = s["rpc"], s["accounts"]

    def view(self, contract, method, args=None):
        return json.loads(sh([f"{BIN}/zfx-bridge", "view", "--rpc", self.rpc, "--contract", contract, "--method", method, "--args", json.dumps(args or {}),
                              "--finality", "optimistic"]).stdout.strip().splitlines()[-1])

    def call(self, signer, contract, method, args, gas=300, deposit=0):
        return jrun([f"{BIN}/zfx-bridge", "call", "--rpc", self.rpc, "--signer", signer, "--key-file", self.keys[signer], "--contract", contract,
                     "--method", method, "--args", json.dumps(args), "--gas", str(gas), "--deposit", str(deposit)], check=False)

    def near_balance(self, acct):
        body = {"jsonrpc": "2.0", "id": 1, "method": "query", "params": {"request_type": "view_account", "finality": "final", "account_id": acct}}
        req = urllib.request.Request(self.rpc, data=json.dumps(body).encode(), headers={"content-type": "application/json"})
        return int(json.loads(urllib.request.urlopen(req).read())["result"]["amount"]) / 1e24

    # ---------------- ZFX ----------------
    def deploy_zfx(self):
        init = {"genesis_seed": "zfx-local", "genesis_target": self.genesis_target, "spacing_secs": 5, "reward": str(REWARD), "bond": str(self.bond),
                "window_secs": self.window_secs}
        self.deploy_record = jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", self.rpc, "--signer", "deployer.test.near", "--key-file",
                                   self.keys["deployer.test.near"], "--contract", CONTRACT, "--wasm", WASM, "--init", json.dumps(init), "--deposit-near", "20"])
        return self.deploy_record

    def stats(self):
        v = self.view(CONTRACT, "stats")
        return {k: (int(v[k]) if k in ("final_height", "head_height", "frauds") else v[k]) for k in v}

    def zfx_balance(self, acct):
        return int(self.view(CONTRACT, "ft_balance_of", {"account_id": acct}))

    def start_watcher(self, account="watcher.test.near"):
        self.spawn("watcher", [f"{BIN}/zfx-bridge", "near-opt-watch", "--rpc", self.rpc, "--signer", account, "--key-file", self.keys[account],
                               "--contract", CONTRACT])

    def proxy_url(self, i):
        return f"http://127.0.0.1:{self.port0 + i}"

    def start_proxy(self, i):
        m = self.miners[i]
        self.spawn(f"proxy{i}", [f"{BIN}/zfx-bridge", "near-mine-proxy", "--optimistic", "--rpc", self.rpc, "--signer", m, "--key-file", self.keys[m],
                                 "--contract", CONTRACT, "--listen", f"127.0.0.1:{self.port0 + i}"])

    def start_miner(self, i, threads=8, extra=()):
        return self.spawn(f"miner{i}", [f"{BIN}/zpu-cli", "mine", "--node", self.proxy_url(i), "--threads", str(threads), *extra])

    def finalize(self):
        return self.call("deployer.test.near", CONTRACT, "finalize", {"max": 50})

    def up(self):
        self.start_near()
        self.deploy_zfx()
        self.start_watcher()
        for i in range(len(self.miners)):
            self.start_proxy(i)
        time.sleep(1)


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "up"
    if cmd == "down":
        subprocess.run(["pkill", "-f", RUN])
        sys.exit(0)
    subprocess.run(["rm", "-rf", RUN])
    n = NearLocal(miners=("miner1.test.near", "miner2.test.near"))
    try:
        n.up()
        print(json.dumps({
            "near_rpc": n.rpc,
            "zfx_contract": CONTRACT,
            "mine_as_miner1": f"{BIN}/zpu-cli mine --node {n.proxy_url(0)}",
            "mine_as_miner2": f"{BIN}/zpu-cli mine --node {n.proxy_url(1)}",
            "mine_on_apple_gpu": f"{BIN}/zpu-cli mine --node {n.proxy_url(0)} --backend exec --exec \".venv/bin/python miner/python/zpu_miner.py --backend mlx\"",
            "balance": f"{BIN}/zfx-bridge view --rpc {n.rpc} --contract {CONTRACT} --method ft_balance_of --args '{{\"account_id\":\"miner1.test.near\"}}'",
            "finalize": "blocks mint after the challenge window; the watcher log is deploy/run/near-local/watcher.log",
        }, indent=2))
        while True:
            time.sleep(5)
            st = n.stats()
            if st["final_height"] < st["head_height"]:
                n.finalize()
    except KeyboardInterrupt:
        pass
    finally:
        n.stop_all()
