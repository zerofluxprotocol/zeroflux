#!/usr/bin/env python3
"""zfxnet: local multi-node ZeroFlux private testnet launcher and
integration scenario (stdlib only).

  python3 testnet/zfxnet.py up [--nodes 3] [--profile dev|v01]   start nodes + miners, print status
  python3 testnet/zfxnet.py status
  python3 testnet/zfxnet.py down
  python3 testnet/zfxnet.py scenario [--out bench/results/testnet_integration.json]

The scenario exercises spec section 8 "Network" end to end with real
processes: three Rust nodes, three independent miner implementations
(Rust zpu-cli, C++ zpu-miner, Python zpu_miner), wallet transfers,
a network partition healed by restart (reorg), an invalid block pushed over
P2P (ban), and a SIGKILL crash/restart recovery.
"""
import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUN = os.path.join(ROOT, "testnet", "run")
BIN = os.path.join(ROOT, "target", "release")
CPP = os.path.join(ROOT, "miner", "cuda", "build", "zpu-miner")
PY = os.path.join(ROOT, ".venv", "bin", "python")
if not os.path.exists(PY):
    PY = sys.executable
SEED_A = "a1" * 32
SEED_B = "b2" * 32


def http(port, path, body=None, timeout=10):
    url = f"http://127.0.0.1:{port}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        return json.loads(e.read() or b"null")


def ports(i):
    return 19440 + 2 * i, 19441 + 2 * i


def wallet(name, seed):
    path = os.path.join(RUN, f"{name}.wallet.json")
    if not os.path.exists(path):
        subprocess.run([f"{BIN}/zfx-wallet", "new", "--wallet", path, "--seed-hex", seed], check=True, capture_output=True)
    return path, json.load(open(path))["address"]


def chain_config(profile, alloc_addr):
    cfg = json.loads(subprocess.run([f"{BIN}/zfx-node", "print-config"] + (["--testnet"] if profile == "v01" else []),
                                    check=True, capture_output=True, text=True).stdout)
    cfg["genesis_timestamp"] = int(time.time()) - 3600
    cfg["target_spacing_secs"] = 2 if profile == "dev" else 10
    cfg["genesis_target"] = "0000" + "f" * 60 if profile == "dev" else "00" + "f" * 62
    cfg["allocations"] = [{"address": alloc_addr, "amount": 10**12}]
    path = os.path.join(RUN, "chain.json")
    json.dump(cfg, open(path, "w"), indent=2)
    return path


class Net:
    def __init__(self):
        self.procs = {}
        os.makedirs(RUN, exist_ok=True)

    def start_node(self, i, peers, cfg, extra=None, expect_fail=False, peer_ports=None):
        p2p, rpc = ports(i)
        log = open(os.path.join(RUN, f"node{i}.log"), "a")
        args = [f"{BIN}/zfx-node", "--config", cfg, "--data-dir", os.path.join(RUN, f"node{i}"), "--p2p-port", str(p2p),
                "--rpc-port", str(rpc), "--rpc-bind", "127.0.0.1", "--explorer-dir", os.path.join(ROOT, "explorer", "dist")] + (extra or [])
        for j in peers:
            args += ["--peer", f"127.0.0.1:{ports(j)[0]}"]
        for pp in peer_ports or []:
            args += ["--peer", f"127.0.0.1:{pp}"]
        proc = subprocess.Popen(args, stdout=log, stderr=log)
        self.procs[f"node{i}"] = proc
        for _ in range(300):
            if proc.poll() is not None:
                if expect_fail:
                    return proc.returncode
                raise RuntimeError(f"node{i} exited with {proc.returncode}")
            try:
                if http(rpc, "/v1/status"):
                    return 0
            except Exception:
                time.sleep(0.1)
        raise RuntimeError(f"node{i} did not start")

    def start_miner(self, name, node_i, address, kind, threads=2):
        rpc = ports(node_i)[1]
        base = [f"{BIN}/zpu-cli", "mine", "--node", f"http://127.0.0.1:{rpc}", "--address", address, "--spot-check", "2"]
        if kind == "rust":
            args = base + ["--backend", "cpu", "--threads", str(threads), "--batch", "20000"]
        elif kind == "cpp":
            args = base + ["--backend", "exec", "--exec", f"{CPP} --backend cpu --threads {threads}", "--batch", "20000"]
        elif kind.startswith("python"):
            be = kind.split(":", 1)[1] if ":" in kind else "numpy"
            args = base + ["--backend", "exec", "--exec", f"{PY} {ROOT}/miner/python/zpu_miner.py --backend {be} --batch 512", "--batch", "4096"]
        else:
            raise ValueError(kind)
        log = open(os.path.join(RUN, f"{name}.log"), "a")
        self.procs[name] = subprocess.Popen(args, stdout=log, stderr=log)

    def stop(self, name, sig=signal.SIGTERM):
        p = self.procs.pop(name, None)
        if p and p.poll() is None:
            p.send_signal(sig)
            try:
                p.wait(10)
            except subprocess.TimeoutExpired:
                p.kill()

    def stop_all(self):
        for n in list(self.procs):
            self.stop(n)


def status(i):
    return http(ports(i)[1], "/v1/status")


def wait_for(cond, timeout, what):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            if cond():
                return time.time() - t0
        except Exception:
            pass
        time.sleep(0.5)
    raise AssertionError(f"timeout waiting for {what}")


def push_invalid_block_v2(i, cfg):
    """Encrypted-transport adversary: handshake, then an invalid-PoW block."""
    out = subprocess.run([f"{BIN}/zfx-p2ptool", "inject-invalid-block", "--peer", f"127.0.0.1:{ports(i)[0]}",
                          "--rpc", f"http://127.0.0.1:{ports(i)[1]}", "--config", cfg], capture_output=True, text=True, timeout=60)
    return json.loads(out.stdout.strip().splitlines()[-1])


def push_invalid_block(i):
    """Connect as a peer, handshake, send a block with a bad nonce."""
    s0 = status(i)
    blk = http(ports(i)[1], "/v1/block/" + str(s0["height"]))
    work = http(ports(i)[1], "/v1/work?address=" + blk["coinbase"]["to"])
    hdr = bytes.fromhex(work["header_without_nonce"])
    u32 = lambda o: int.from_bytes(hdr[o:o + 4], "little")
    u64 = lambda o: int.from_bytes(hdr[o:o + 8], "little")
    header = {"version": u32(4), "chain_id": u32(8), "height": u64(12), "parent": hdr[20:52].hex(), "tx_root": hdr[52:84].hex(),
              "timestamp": u64(84), "target": "00" * 31 + "01", "nonce": 7}
    # Consistent tx_root (so the block fails on PoW, not structure).
    import hashlib, struct
    to_raw = bytes.fromhex(blk["coinbase"]["to"][4:68])
    cb = struct.pack("<Q", header["height"]) + to_raw + struct.pack("<Q", 1)
    cid = hashlib.sha256(b"ZFX-coinbase-v0.1" + cb).digest()
    header["tx_root"] = hashlib.sha256(b"ZFX-txroot-v0.1" + struct.pack("<I", 1) + cid).hexdigest()
    header["target"] = work["target"]
    block = {"header": header, "coinbase": {"height": header["height"], "to": blk["coinbase"]["to"], "amount": 1}, "txs": []}
    s = socket.create_connection(("127.0.0.1", ports(i)[0]), timeout=5)
    hello = {"type": "hello", "protocol": 1, "chain_id": s0["chain_id"], "genesis": s0["genesis"], "height": 0, "best": s0["genesis"], "listen": None}
    s.sendall((json.dumps(hello) + "\n").encode())
    time.sleep(0.3)
    s.sendall((json.dumps({"type": "block", "block": block}) + "\n").encode())
    closed = False
    s.settimeout(5)
    try:
        while True:
            if not s.recv(65536):
                closed = True
                break
    except (socket.timeout, ConnectionResetError):
        closed = True
    s.close()
    return closed


def scenario(out_path, profile):
    for f in os.listdir(RUN) if os.path.isdir(RUN) else []:
        subprocess.run(["rm", "-rf", os.path.join(RUN, f)])
    net = Net()
    report = {"profile": profile, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "checks": []}

    def check(name, ok, evidence):
        report["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": evidence})
        print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(evidence)[:300], flush=True)

    try:
        wa, addr_a = wallet("alice", SEED_A)
        wb, addr_b = wallet("bob", SEED_B)
        miners = {k: wallet(f"miner_{k}", s)[1] for k, s in [("rust", "11" * 32), ("cpp", "22" * 32), ("py", "33" * 32)]}
        cfg = chain_config(profile, addr_a)
        net.start_node(1, [], cfg)
        net.start_node(2, [1], cfg)
        net.start_node(3, [], cfg)  # partitioned
        net.start_miner("miner_rust", 1, miners["rust"], "rust")
        net.start_miner("miner_cpp", 2, miners["cpp"], "cpp")
        net.start_miner("miner_py", 3, miners["py"], "python:numpy")
        wait_for(lambda: status(1)["height"] >= 12 and status(3)["height"] >= 3, 300 if profile == "dev" else 900, "initial mining")
        s1, s2 = status(1), status(2)
        h = min(s1["height"], s2["height"]) - 2
        b1, b2 = http(ports(1)[1], f"/v1/block/{h}"), http(ports(2)[1], f"/v1/block/{h}")
        check("nodes 1+2 agree on block hashes", b1["hash"] == b2["hash"], {"height": h, "hash": b1["hash"]})
        def by_miner():
            blocks = http(ports(1)[1], f"/v1/blocks?count=200")
            return {k: sum(1 for b in blocks if b["miner"] == a) for k, a in miners.items()}
        try:
            wait_for(lambda: by_miner()["rust"] > 0 and by_miner()["cpp"] > 0, 180, "blocks from both miners")
        except AssertionError:
            pass
        by = by_miner()
        s1 = status(1)
        check("Gate 2: blocks from Rust and C++ miners both accepted on the shared CPU-verified chain",
              by["rust"] > 0 and by["cpp"] > 0, {"blocks_by_miner": by, "height": s1["height"]})
        # wallet transfer
        t0 = time.time()
        r = subprocess.run([f"{BIN}/zfx-wallet", "send", "--wallet", wa, "--node", f"http://127.0.0.1:{ports(1)[1]}", "--to", addr_b,
                            "--amount", "123456", "--fee", "1000"], capture_output=True, text=True)
        res = json.loads(r.stdout.strip().splitlines()[-1])
        check("SLH-DSA P2MR transaction accepted into mempool", res.get("accepted") is True, {"stderr": r.stderr.strip(), "resp": res})
        txid = res.get("txid")
        wait_for(lambda: http(ports(2)[1], f"/v1/tx/{txid}").get("status") == "confirmed", 120, "tx confirmation on node2")
        bal_b = http(ports(2)[1], f"/v1/account/{addr_b}")["balance"]
        bal_a = http(ports(2)[1], f"/v1/account/{addr_a}")["balance"]
        check("transfer confirmed and balances correct on the peer node", bal_b == 123456 and bal_a == 10**12 - 123456 - 1000,
              {"bob": bal_b, "alice": bal_a, "confirm_s": round(time.time() - t0, 2)})
        # replayed tx rejected
        tx = http(ports(1)[1], f"/v1/tx/{txid}")["tx"]
        rr = http(ports(1)[1], "/v1/tx", tx)
        check("replayed (already mined) tx rejected", rr.get("accepted") is False, rr)
        # invalid block over P2P -> ban
        banned_before = status(1)["stats"]["peers_banned"]
        h_before = status(1)["height"]
        closed = push_invalid_block(1)
        s1 = status(1)
        log1 = open(os.path.join(RUN, "node1.log")).read()
        check("invalid-PoW block over P2P rejected (AboveTarget) and peer disconnected", closed and s1["stats"]["peers_banned"] == banned_before + 1
              and s1["height"] >= h_before and "AboveTarget" in log1,
              {"closed": closed, "peers_banned": s1["stats"]["peers_banned"], "blocks_rejected": s1["stats"]["blocks_rejected"]})
        # partition heal: node3 restarts peered with node1
        s3 = status(3)
        net.stop("miner_py")
        net.stop("node3")
        net.start_node(3, [1], cfg)
        import re
        rec = re.findall(r"recovered (\d+) blocks", open(os.path.join(RUN, "node3.log")).read())
        check("restart recovery: node3 reloads its own chain from disk before syncing", bool(rec) and int(rec[-1]) >= s3["height"],
              {"height_before_stop": s3["height"], "blocks_recovered_from_disk": int(rec[-1]) if rec else None})
        wait_for(lambda: status(3)["best_hash"] == status(1)["best_hash"] or status(3)["height"] > status(1)["height"] + 50, 120, "partition heal")
        s3c, s1c = status(3), status(1)
        check("partition healed: isolated chain reorged onto heavier chain", s3c["best_hash"] == s1c["best_hash"] and s3c["reorgs"] >= 1,
              {"node3_reorgs": s3c["reorgs"], "max_reorg_depth": s3c["max_reorg_depth"], "height": s3c["height"]})
        # crash: SIGKILL node2 + its miner, restart, catch up
        net.stop("miner_cpp")
        h2 = status(2)["height"]
        net.stop("node2", signal.SIGKILL)
        net.start_node(2, [1], cfg)
        s2r = status(2)
        wait_for(lambda: status(2)["best_hash"] == status(1)["best_hash"], 120, "node2 resync after SIGKILL")
        check("SIGKILL crash recovery and resync", s2r["height"] >= h2 - 1, {"height_before_kill": h2, "height_after_restart": s2r["height"], "resynced_to": status(2)["height"]})
        # Python miner (third implementation) on the healed network
        net.start_miner("miner_py", 3, miners["py"], "python:numpy")
        wait_for(lambda: any(b["miner"] == miners["py"] for b in http(ports(1)[1], "/v1/blocks?count=50")), 300, "python-miner block on main chain")
        check("third implementation (Python miner) block accepted on main chain", True, {"miner": miners["py"]})
        # difficulty adjusts (LWMA needs 31 blocks of history)
        wait_for(lambda: status(1)["height"] >= 40, 300 if profile == "dev" else 1500, "height 40 for LWMA")
        s1 = status(1)
        gen_t = json.load(open(cfg))["genesis_target"]
        check("LWMA difficulty moved off genesis target", s1["target"] != gen_t and s1["next_target"] != gen_t, {"target": s1["target"], "difficulty_log2": s1["difficulty_log2"], "height": s1["height"]})
        report["final_status"] = {f"node{i}": status(i) for i in (1, 2, 3)}
    finally:
        net.stop_all()
    report["passed"] = sum(c["result"] == "PASS" for c in report["checks"])
    report["total"] = len(report["checks"])
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    json.dump(report, open(out_path, "w"), indent=2)
    print(f"{report['passed']}/{report['total']} checks passed -> {out_path}")
    return 0 if report["passed"] == report["total"] else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["up", "down", "status", "scenario"])
    ap.add_argument("--nodes", type=int, default=3)
    ap.add_argument("--profile", default="dev", choices=["dev", "v01"])
    ap.add_argument("--out", default=os.path.join(ROOT, "bench", "results", "testnet_integration.json"))
    a = ap.parse_args()
    if a.cmd == "scenario":
        sys.exit(scenario(a.out, a.profile))
    if a.cmd == "status":
        for i in range(1, a.nodes + 1):
            try:
                s = status(i)
                print(f"node{i} h={s['height']} best={s['best_hash'][:16]} peers={s['peer_count']} diff_log2={s['difficulty_log2']:.2f}")
            except Exception as e:
                print(f"node{i} down ({e})")
        return
    if a.cmd == "down":
        subprocess.run(["pkill", "-f", os.path.join(RUN, "")], check=False)
        subprocess.run(["pkill", "-f", "zpu-cli mine --node http://127.0.0.1:194"], check=False)
        return
    net = Net()
    _, addr_a = wallet("alice", SEED_A)
    cfg = chain_config(a.profile, addr_a)
    for i in range(1, a.nodes + 1):
        net.start_node(i, list(range(1, i)), cfg)
        net.start_miner(f"miner{i}", i, addr_a, ["rust", "cpp", "python:numpy"][(i - 1) % 3])
    print(f"testnet up: rpc ports {[ports(i)[1] for i in range(1, a.nodes + 1)]}, explorer http://127.0.0.1:{ports(1)[1]}/ (Ctrl-C to stop)")
    try:
        while True:
            time.sleep(5)
            for i in range(1, a.nodes + 1):
                s = status(i)
                print(f"node{i} h={s['height']} peers={s['peer_count']} diff_log2={s['difficulty_log2']:.2f}", end="  ")
            print(flush=True)
    except KeyboardInterrupt:
        net.stop_all()


if __name__ == "__main__":
    main()
