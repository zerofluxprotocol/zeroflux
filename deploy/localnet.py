#!/usr/bin/env python3
"""Local ZeroFlux + NEAR sandbox topology for the bridge (and Intents).

    python3 deploy/localnet.py up      # start everything, print endpoints, Ctrl-C to stop
    python3 deploy/localnet.py down

Components (each a separate process with its own data dir under
deploy/run/):
  - NEAR sandbox (real nearcore in sandbox mode) with test accounts
  - 3 ZeroFlux nodes with the bridge module enabled (consensus v2)
  - 1 miner
  - wZFX contract deployed and initialised with the committee's ed25519 keys
  - 3 relayers, relayer i reading ZeroFlux node i (independent evidence)
  - 1 coordinator (holds no committee key)
Key files live in deploy/run/ (gitignored, 0600), never in the repository.
"""
import json
import os
import signal
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "testnet"))
import zfxnet  # noqa: E402

BIN = os.path.join(ROOT, "target", "release")
RUN = os.path.join(ROOT, "deploy", "run")
WASM = os.path.join(ROOT, "bridge", "near", "target", "near", "wzfx", "wzfx.wasm")
FT_WASM = os.path.join(ROOT, "bridge", "near", "target", "near", "test_ft", "test_ft.wasm")
XZ = os.path.expanduser("~/.cache/zeroflux/xz/lib")
ENVV = dict(os.environ, DYLD_FALLBACK_LIBRARY_PATH=XZ)
CONTRACT = "wzfx.deployer.test.near"
USERS = [f"u{i}" for i in range(10)]


def sh(args, check=True, **kw):
    r = subprocess.run(args, capture_output=True, text=True, env=ENVV, **kw)
    if check and r.returncode != 0:
        raise RuntimeError(f"{args[0]} {args[1]} failed: {r.stdout[-500:]} {r.stderr[-800:]}")
    return r


def jrun(args, check=True):
    r = sh(args, check=check)
    lines = [l for l in r.stdout.strip().splitlines() if l.startswith("{")]
    return json.loads(lines[-1]) if lines else {"_rc": r.returncode, "_err": r.stderr[-400:]}


class Localnet:
    # Account names; testnet runs override these with subaccounts of the
    # operator's configured testnet account.
    owner = "deployer.test.near"
    coordinator_account = "coordinator.test.near"

    def __init__(self, confirmations=12, fault_rate=0.0, mint_validity=600, release_validity=200):
        self.procs = {}
        self.confirmations = confirmations
        self.fault_rate = fault_rate
        self.mint_validity = mint_validity
        self.release_validity = release_validity
        zfxnet.RUN = os.path.join(RUN, "zfx")
        self.net = zfxnet.Net()

    def spawn(self, name, args, log=None):
        f = open(os.path.join(RUN, f"{name}.log"), "a")
        p = subprocess.Popen(args, stdout=f, stderr=f, env=ENVV)
        self.procs[name] = p
        return p

    def stop(self, name, sig=signal.SIGTERM):
        p = self.procs.pop(name, None)
        if p and p.poll() is None:
            p.send_signal(sig)
            try:
                p.wait(15)
            except subprocess.TimeoutExpired:
                p.kill()

    def stop_all(self):
        for n in list(self.procs):
            self.stop(n)
        self.net.stop_all()

    # ---------------- NEAR ----------------
    def start_near(self):
        accounts = ["deployer.test.near", "coordinator.test.near", "solver.test.near", "relay.test.near"] + [f"{u}.test.near" for u in USERS]
        d = os.path.join(RUN, "near")
        self.spawn("near-sandbox", [f"{BIN}/zfx-bridge", "near-sandbox", "--dir", d, "--accounts", ",".join(accounts)])
        for _ in range(180):
            if os.path.exists(os.path.join(d, "sandbox.json")):
                break
            time.sleep(0.5)
        self.near = json.load(open(os.path.join(d, "sandbox.json")))
        self.rpc = self.near["rpc"]
        return self.near

    def key(self, account):
        return self.near["accounts"][account]

    def near_call(self, signer, contract, method, args, deposit=0, gas=100, check=True):
        return jrun([f"{BIN}/zfx-bridge", "call", "--rpc", self.rpc, "--signer", signer, "--key-file", self.key(signer), "--contract", contract,
                     "--method", method, "--args", json.dumps(args), "--deposit", str(deposit), "--gas", str(gas)], check=check)

    def near_view(self, contract, method, args=None):
        r = sh([f"{BIN}/zfx-bridge", "view", "--rpc", self.rpc, "--contract", contract, "--method", method, "--args", json.dumps(args or {})])
        return json.loads(r.stdout.strip().splitlines()[-1])

    # ---------------- relayer keys ----------------
    def make_relayer_keys(self):
        self.relayers = []
        for i in range(3):
            seed = os.path.join(RUN, f"relayer{i}", "seed")
            if os.path.exists(seed):
                os.remove(seed)
            k = jrun([f"{BIN}/zfx-bridge", "keygen-relayer", "--out", seed])
            self.relayers.append({"member": i, "seed": seed, "ed25519": k["ed25519"], "slh": k["slh"], "port": 28700 + i})
        return self.relayers

    # ---------------- native chain ----------------
    def native_config(self, extra_allocs):
        cfg = json.loads(sh([f"{BIN}/zfx-node", "print-config"]).stdout)
        cfg["genesis_timestamp"] = int(time.time()) - 7200
        cfg["genesis_target"] = "00" + "f" * 62
        cfg["fixed_difficulty"] = True
        cfg["target_spacing_secs"] = 1
        cfg["coinbase_maturity"] = 5
        cfg["label"] = "zeroflux-bridge-localnet"
        cfg["allocations"] = extra_allocs
        cfg["bridge"] = {"committee": [r["slh"] for r in self.relayers], "threshold": 2, "rotation_timelock_blocks": 20,
                         "min_lock": 1000, "activation_height": 1, "near_contract": CONTRACT}
        path = os.path.join(RUN, "chain.json")
        json.dump(cfg, open(path, "w"), indent=2)
        self.cfg_path, self.cfg = path, cfg
        self.chain_id = cfg["chain_id"]
        self.asset_id = f"zfx:{cfg['chain_id']:08x}:native"
        return path

    def start_native(self, users_alloc):
        os.makedirs(zfxnet.RUN, exist_ok=True)
        self.native_config(users_alloc)
        k = ["--bridge-confirmations", str(self.confirmations)]
        self.net.start_node(1, [], self.cfg_path, extra=k)
        self.net.start_node(2, [1], self.cfg_path, extra=k)
        self.net.start_node(3, [1, 2], self.cfg_path, extra=k)

    def start_miner(self, node=1):
        _, addr = zfxnet.wallet("bridge_miner", "77" * 32)
        self.net.start_miner(f"miner{node}", node, addr, "rust")

    def zfx_url(self, i):
        return f"http://127.0.0.1:{zfxnet.ports(i)[1]}"

    # ---------------- contracts ----------------
    def deploy_wzfx(self, timelock=5):
        init = {"owner_id": self.owner, "committee": [r["ed25519"] for r in self.relayers], "threshold": 2,
                "native_chain_id": self.chain_id, "native_asset_id": self.asset_id, "timelock_sec": timelock, "metadata": None}
        out = jrun([f"{BIN}/zfx-bridge", "deploy", "--rpc", self.rpc, "--signer", self.owner, "--key-file", self.key(self.owner),
                    "--contract", CONTRACT, "--wasm", WASM, "--init", json.dumps(init)])
        self.deploy_record = out
        return out

    # ---------------- relayers / coordinator ----------------
    def env_file(self):
        env = {"native_chain_id": self.chain_id, "native_asset_id": self.asset_id, "near_rpc": self.rpc, "near_network": "sandbox",
               "wzfx_contract": CONTRACT, "confirmations": self.confirmations, "release_validity_blocks": self.release_validity,
               "mint_validity_secs": self.mint_validity,
               "relayers": [{"member": r["member"], "url": f"http://127.0.0.1:{r['port']}", "ed25519": r["ed25519"], "slh": r["slh"]} for r in self.relayers]}
        p = os.path.join(RUN, "bridge_env.json")
        json.dump(env, open(p, "w"), indent=2)
        self.env_path = p
        return p

    def start_relayer(self, i):
        r = self.relayers[i]
        cfg = {"member": i, "seed_file": r["seed"], "zfx_node": self.zfx_url(i + 1), "near_rpc": self.rpc, "wzfx_contract": CONTRACT,
               "native_chain_id": self.chain_id, "native_asset_id": self.asset_id, "confirmations": self.confirmations,
               "listen": f"127.0.0.1:{r['port']}", "data_dir": os.path.join(RUN, f"relayer{i}")}
        p = os.path.join(RUN, f"relayer{i}", "config.json")
        json.dump(cfg, open(p, "w"), indent=2)
        self.spawn(f"relayer{i}", [f"{BIN}/zfx-relayer", "--config", p])
        import urllib.request
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{r['port']}/info", timeout=1)
                return
            except Exception:
                time.sleep(0.1)
        raise RuntimeError(f"relayer{i} did not start")

    def start_coordinator(self, fault_rate=None):
        fr = self.fault_rate if fault_rate is None else fault_rate
        self.spawn("coordinator", [f"{BIN}/zfx-bridge-coordinator", "--env", self.env_path, "--zfx", self.zfx_url(1),
                                   "--near-account", self.coordinator_account, "--near-key-file", self.key(self.coordinator_account),
                                   "--store", os.path.join(RUN, "jobs.json"), "--incidents", os.path.join(RUN, "incidents"),
                                   "--auto-register", "--status-port", "28790", "--interval-ms", "500", "--fault-rate", str(fr)])

    def coordinator_status(self):
        import urllib.request
        return json.loads(urllib.request.urlopen("http://127.0.0.1:28790/status", timeout=5).read())

    def set_refuse(self, i, on):
        import urllib.request
        req = urllib.request.Request(f"http://127.0.0.1:{self.relayers[i]['port']}/admin/refuse", data=json.dumps({"refuse": on}).encode(),
                                     headers={"content-type": "application/json"})
        return json.loads(urllib.request.urlopen(req, timeout=5).read())

    def reconcile(self):
        r = sh([f"{BIN}/zfx-bridge", "reconcile", "--env", self.env_path, "--zfx", self.zfx_url(1)], check=False)
        return json.loads(r.stdout) if r.stdout.strip().startswith("{") else {"ok": False, "error": r.stderr[-400:]}

    def up(self, users_alloc):
        os.makedirs(RUN, exist_ok=True)
        self.start_near()
        self.make_relayer_keys()
        self.start_native(users_alloc)
        self.start_miner(1)
        self.deploy_wzfx()
        self.env_file()
        for i in range(3):
            self.start_relayer(i)
        self.start_coordinator()


def user_wallets():
    out = []
    for i, u in enumerate(USERS):
        path, addr = zfxnet.wallet(f"bridge_{u}", f"{0x40 + i:02x}" * 32)
        out.append({"name": u, "near": f"{u}.test.near", "wallet": path, "addr": addr})
    return out


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "up"
    if cmd == "down":
        subprocess.run(["pkill", "-f", RUN])
        subprocess.run(["pkill", "-f", "zfx-bridge near-sandbox --dir " + os.path.join(RUN, "near")])
        sys.exit(0)
    ln = Localnet()
    os.makedirs(os.path.join(RUN, "zfx"), exist_ok=True)
    zfxnet.RUN = os.path.join(RUN, "zfx")
    ws = None
    try:
        subprocess.run(["rm", "-rf", RUN])
        os.makedirs(zfxnet.RUN, exist_ok=True)
        ws = user_wallets()
        ln.up([{"address": w["addr"], "amount": 10**12} for w in ws])
        print(json.dumps({"near_rpc": ln.rpc, "wzfx": CONTRACT, "zfx_nodes": [ln.zfx_url(i) for i in (1, 2, 3)],
                          "coordinator": "http://127.0.0.1:28790/status", "env": ln.env_path}, indent=2))
        while True:
            time.sleep(5)
    except KeyboardInterrupt:
        pass
    finally:
        ln.stop_all()
