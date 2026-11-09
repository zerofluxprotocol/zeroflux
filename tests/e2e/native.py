#!/usr/bin/env python3
"""make e2e-native: ZeroFlux L1 acceptance (Gate 1 / A1-A3) with real processes.

Three independently launched nodes (separate processes and data dirs), three
miner implementations (Rust, C++, Python), wallet flows, receipts with
Merkle proofs, 1000+ blocks with an exact supply audit, peer discovery,
encrypted-transport capture, adversarial frames, partition/reorg, 10
restart/recovery runs, corrupted-database detection and repair.

Writes bench/results/e2e_native.json; exits 1 if any check fails.
"""
import hashlib
import json
import os
import signal
import struct
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "testnet"))
import zfxnet  # noqa: E402
from zfxnet import BIN, RUN, Net, http, ports, status, wait_for, wallet  # noqa: E402

TARGET_BLOCKS = int(os.environ.get("ZFX_E2E_BLOCKS", "1000"))
FAUCET_SEED = "fa" * 32
REPORT = {"started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "checks": []}


def check(name, ok, evidence):
    REPORT["checks"].append({"check": name, "result": "PASS" if ok else "FAIL", "evidence": evidence})
    print(("PASS " if ok else "FAIL ") + name + " :: " + json.dumps(evidence)[:400], flush=True)
    return ok


def rpc(i, method, params=None):
    r = http(ports(i)[1], "/rpc", {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}})
    return r


def wallet_cli(*args):
    r = subprocess.run([f"{BIN}/zfx-wallet", *args], capture_output=True, text=True, timeout=120)
    return r.returncode, r.stdout.strip(), r.stderr.strip()


def merkle_ok(receipt):
    """Independently verify a receipt's Merkle proof against the header tx_root."""
    h = bytes.fromhex(receipt["leaf"])
    for step in receipt["merkle_proof"]:
        sib = bytes.fromhex(step["hash"])
        h = hashlib.sha256(b"ZFX-merkle-v0.1" + (h + sib if step["right"] else sib + h)).digest()
    root = hashlib.sha256(b"ZFX-txroot-v0.1" + struct.pack("<I", receipt["leaf_count"]) + h).digest()
    header = bytes.fromhex(receipt["header_hex"])
    return root == header[52:84] and root.hex() == receipt["tx_root"]


def make_config(faucet_addr):
    cfg = json.loads(subprocess.run([f"{BIN}/zfx-node", "print-config"], check=True, capture_output=True, text=True).stdout)
    cfg["genesis_timestamp"] = int(time.time()) - 7200
    cfg["genesis_target"] = "00" + "f" * 62  # 2^-8: fast blocks; fixed so 1000 blocks fit in minutes
    cfg["fixed_difficulty"] = True
    cfg["target_spacing_secs"] = 1
    cfg["coinbase_maturity"] = 5
    cfg["allocations"] = [{"address": faucet_addr, "amount": 10**13}]
    cfg["label"] = "zeroflux-e2e-native"
    path = os.path.join(RUN, "chain.json")
    json.dump(cfg, open(path, "w"), indent=2)
    return path, cfg


def main():
    subprocess.run(["rm", "-rf", RUN])
    os.makedirs(RUN, exist_ok=True)
    net = Net()
    ok_all = True
    try:
        _, faucet_addr = wallet("faucet", FAUCET_SEED)
        miners = {k: wallet(f"miner_{k}", s)[1] for k, s in [("rust", "11" * 32), ("cpp", "22" * 32), ("py", "33" * 32)]}
        cfg_path, cfg = make_config(faucet_addr)
        faucet = ["--faucet-seed-hex", FAUCET_SEED, "--faucet-amount", "5000000", "--faucet-cooldown", "1"]
        net.start_node(1, [], cfg_path, extra=faucet)
        net.start_node(2, [1], cfg_path)
        net.start_node(3, [2], cfg_path)  # learns node1 via Addr gossip
        net.start_miner("miner_rust", 1, miners["rust"], "rust")
        net.start_miner("miner_cpp", 2, miners["cpp"], "cpp")
        net.start_miner("miner_py", 3, miners["py"], "python:numpy")

        # discovery: node3 was only given node2, must end up peered with node1 too
        try:
            wait_for(lambda: status(3)["peer_count"] >= 2, 60, "node3 discovers node1")
        except AssertionError:
            pass
        n1id = status(1)["peer_id"]
        p3 = http(ports(3)[1], "/v1/peers")
        ok_all &= check("A2 peer discovery via Addr gossip (node3 configured only with node2 reaches node1)",
                        any(p["peer_id"] == n1id for p in p3), {"node3_peers": [p["peer_id"][:12] for p in p3]})

        # wallet flows on a clean wallet
        wa = os.path.join(RUN, "alice.wallet.json")
        wb = os.path.join(RUN, "bob.wallet.json")
        _, alice, _ = wallet_cli("new", "--wallet", wa)
        _, bob, _ = wallet_cli("new", "--wallet", wb)
        node1 = f"http://127.0.0.1:{ports(1)[1]}"
        node3 = f"http://127.0.0.1:{ports(3)[1]}"
        rc, out, err = wallet_cli("faucet", "--wallet", wa, "--node", node1)
        fx = json.loads(out)
        ok_all &= check("A3 faucet pays a fresh wallet", rc == 0 and fx.get("amount") == 5_000_000, fx)
        wait_for(lambda: rpc(3, "zfx_node_get_transaction", {"txid": fx["txid"]}).get("result", {}).get("status") == "confirmed", 120, "faucet tx")
        rc, out, err = wallet_cli("send", "--wallet", wa, "--node", node1, "--to", bob, "--amount", "1234567", "--fee", "2000")
        sent = json.loads(out)
        ok_all &= check("A3 SLH-DSA transfer accepted", rc == 0 and sent["accepted"], {"stderr": err, "out": sent})
        wait_for(lambda: rpc(3, "zfx_node_get_transaction", {"txid": sent["txid"]}).get("result", {}).get("status") == "confirmed", 120, "transfer")
        a_bal = rpc(3, "zfx_get_account", {"address": alice})["result"]["balance"]
        b_bal = rpc(3, "zfx_get_account", {"address": bob})["result"]["balance"]
        ok_all &= check("A3 balances and fee conservation on a different node", a_bal == 5_000_000 - 1_234_567 - 2000 and b_bal == 1_234_567,
                        {"alice": a_bal, "bob": b_bal})
        rec = rpc(3, "zfx_node_get_transaction", {"txid": sent["txid"]})["result"]
        ok_all &= check("A3 receipt Merkle proof verifies independently (Python) against header tx_root", merkle_ok(rec["receipt"]),
                        {"height": rec["height"], "leaf_count": rec["receipt"]["leaf_count"]})
        hist = rpc(1, "zfx_get_address_history", {"address": alice})["result"]
        ok_all &= check("A3 address history shows in/out", {h["direction"] for h in hist} >= {"in", "out"}, hist[:2])
        # invalid signature rejected
        tx = rec["tx"]
        tx["witness"]["signature"] = ("00" if tx["witness"]["signature"][:2] != "00" else "11") + tx["witness"]["signature"][2:]
        tx["nonce"] = 1
        r = rpc(1, "zfx_submit_transaction", {"tx": tx})
        ok_all &= check("A1 invalid signature rejected with structured error", r.get("error", {}).get("code") == 1001, r.get("error"))
        # backup / restore
        bk = os.path.join(RUN, "alice.backup")
        wallet_cli("backup", "--wallet", wa, "--out", bk)
        rc, restored, _ = wallet_cli("restore", "--backup", bk, "--wallet", os.path.join(RUN, "alice2.wallet.json"))
        ok_all &= check("A3 backup/restore reproduces the address", restored == alice, {"restored": restored == alice})
        # explorer
        import urllib.request
        html = urllib.request.urlopen(f"{node1}/").read()
        ok_all &= check("A3 explorer served by node", b"ZeroFlux explorer" in html, {"bytes": len(html)})

        # adversarial P2P (encrypted transport)
        h_before = status(1)["height"]
        inj = zfxnet.push_invalid_block_v2(1, cfg_path)
        garb = json.loads(subprocess.run([f"{BIN}/zfx-p2ptool", "garbage", "--peer", f"127.0.0.1:{ports(1)[0]}", "--count", "60"],
                                         capture_output=True, text=True, timeout=120).stdout)
        time.sleep(2)
        s1 = status(1)
        ok_all &= check("A2 invalid-PoW block over encrypted P2P rejected and peer disconnected; 60 malformed connections survived",
                        inj["disconnected_by_node"] and s1["height"] >= h_before and net.procs["node1"].poll() is None,
                        {"inject": inj, "garbage": garb, "handshakes_failed": s1["stats"]["handshakes_failed"], "blocks_rejected": s1["stats"]["blocks_rejected"]})

        # encryption evidence: a 4th node syncs from genesis through a recording proxy
        dump = os.path.join(RUN, "wire.bin")
        proxy_port = 19599
        prox = subprocess.Popen([f"{BIN}/zfx-p2ptool", "proxy", "--listen", str(proxy_port), "--to", f"127.0.0.1:{ports(1)[0]}", "--dump", dump])
        net.procs["proxy"] = prox
        time.sleep(0.5)
        net.start_node(4, [], cfg_path, extra=["--no-discovery"], peer_ports=[proxy_port])
        wait_for(lambda: status(4)["height"] >= status(1)["height"] - 2, 120, "node4 sync from genesis via proxy")
        import glob
        wire = b""
        ct = bytearray()
        frames = 0
        for f in sorted(glob.glob(dump + ".*.*")):
            data = open(f, "rb").read()
            wire += data
            # parse u32-BE length-prefixed frames; skip handshake frames
            # (initiator: H1, H3; responder: H2), keep AEAD ciphertexts
            skip = 2 if f.endswith(".i2r") else 1
            pos = 0
            idx = 0
            while pos + 4 <= len(data):
                n = struct.unpack(">I", data[pos:pos + 4])[0]
                body = data[pos + 4:pos + 4 + n]
                if len(body) < n:
                    break
                if idx >= skip:
                    ct += body
                    frames += 1
                idx += 1
                pos += 4 + n
        genesis = status(1)["genesis"]
        markers = [b'"type"', b"hello", b'"header"', b"zfx1", genesis.encode(), b"get_blocks", b"coinbase"]
        found = [m.decode(errors="replace") for m in markers if m in wire]
        sample = bytes(ct[: 1 << 21])
        hist_ = [0] * 256
        for byte in sample:
            hist_[byte] += 1
        n = len(sample)
        chi2 = sum((c - n / 256) ** 2 / (n / 256) for c in hist_) if n else 0
        ok_all &= check("A2 transport encryption: no plaintext markers on the wire; AEAD frame bodies are uniformly distributed",
                        len(wire) > 100_000 and not found and frames > 100 and chi2 < 330,
                        {"wire_bytes": len(wire), "encrypted_frames": frames, "plaintext_markers_found": found,
                         "chi2_255dof_over_2MiB_ciphertext": round(chi2, 1), "p_0.001_critical_value": 330.5,
                         "new_node_synced_from_genesis_to": status(4)["height"]})
        net.stop("node4")
        net.stop("proxy")

        # 1000+ blocks, convergence and exact supply audit
        t0 = time.time()
        wait_for(lambda: status(1)["height"] >= TARGET_BLOCKS, 1800, f"height {TARGET_BLOCKS}")
        # Stop two miners; keep one so equal-work ties break, then stop it.
        net.stop("miner_cpp")
        net.stop("miner_py")
        wait_for(lambda: len({status(i)["best_hash"] for i in (1, 2, 3)}) == 1, 120, "convergence with one miner")
        net.stop("miner_rust")
        time.sleep(2)
        wait_for(lambda: len({status(i)["best_hash"] for i in (1, 2, 3)}) == 1, 60, "convergence")
        s = [status(i) for i in (1, 2, 3)]
        h = s[0]["height"]
        healths = [http(ports(i)[1], "/health") for i in (1, 2, 3)]
        expect_supply = 10**13 + h * cfg["block_subsidy"]
        ok_all &= check(f"A1 {TARGET_BLOCKS}+ blocks: 3 nodes converge; supply == allocations + height*subsidy exactly; audits pass",
                        all(x["best_hash"] == s[0]["best_hash"] for x in s) and all(int(x["supply"]) == expect_supply for x in s) and all(hh["ok"] for hh in healths),
                        {"height": h, "supply": s[0]["supply"], "expected": expect_supply, "state_root": s[0]["state_root"],
                         "reached_in_s": round(time.time() - t0, 1), "health": [hh["supply_audit"] for hh in healths]})
        blocks = http(ports(1)[1], f"/v1/blocks?count=200")
        by = {k: sum(1 for b in blocks if b["miner"] == a) for k, a in miners.items()}
        ok_all &= check("Gate 2: blocks from all three independent miner implementations accepted by independent nodes",
                        all(v > 0 for v in by.values()), {"last_200_blocks_by_miner": by})
        imm = {k: rpc(1, "zfx_get_account", {"address": a})["result"]["immature"] for k, a in miners.items()}
        last5 = http(ports(1)[1], f"/v1/blocks?count=5")
        expect_imm = sum(b["reward"] for b in last5)
        ok_all &= check("A1 coinbase maturity: exactly the last 5 blocks' rewards are immature (maturity=5)",
                        sum(imm.values()) == expect_imm and len(last5) == 5, {"immature_by_miner": imm, "expected_total": expect_imm})

        # partition + reorg: node3 mines alone, then rejoins
        net.stop("node3")
        net.start_node(3, [], cfg_path, extra=["--no-discovery"])
        net.start_miner("miner_py", 3, miners["py"], "python:numpy")
        net.start_miner("miner_rust", 1, miners["rust"], "rust")
        net.start_miner("miner_cpp", 2, miners["cpp"], "cpp")
        wait_for(lambda: status(3)["height"] >= h + 5 and status(1)["height"] >= h + 15, 300, "partitioned mining")
        net.stop("miner_py")
        diverged = status(3)["best_hash"] != status(1)["best_hash"]
        net.stop("node3")
        net.start_node(3, [1], cfg_path)
        net.stop("miner_cpp")
        wait_for(lambda: status(3)["best_hash"] == status(1)["best_hash"], 180, "rejoin convergence")
        s3 = status(3)
        ok_all &= check("A2 partition then rejoin: converge on most cumulative work via reorg",
                        diverged and s3["reorgs"] >= 1, {"node3_reorgs": s3["reorgs"], "max_depth": s3["max_reorg_depth"], "height": s3["height"]})

        net.stop("miner_rust")
        time.sleep(2)
        wait_for(lambda: len({status(i)["best_hash"] for i in (1, 2, 3)}) == 1, 60, "quiet network")
        # 10 restart / recovery runs on node2 (5 clean SIGTERM, 5 SIGKILL)
        runs = []
        for k in range(10):
            sig = signal.SIGTERM if k % 2 == 0 else signal.SIGKILL
            hb = status(2)["height"]
            net.stop("node2", sig)
            net.start_node(2, [1], cfg_path)
            lr = status(2)["load_report"]
            wait_for(lambda: status(2)["best_hash"] == status(1)["best_hash"], 120, "node2 resync")
            runs.append({"signal": "TERM" if sig == signal.SIGTERM else "KILL", "height_before": hb, "reloaded": lr["revalidated"],
                         "previous_clean": lr["previous_shutdown_clean"], "resynced": status(2)["height"]})
        ok_all &= check("A2 10 restart/recovery runs (5 SIGTERM clean, 5 SIGKILL crash) reload, revalidate and resync",
                        len(runs) == 10 and all(r["reloaded"] >= r["height_before"] - 1 for r in runs)
                        and all(r["previous_clean"] == (r["signal"] == "TERM") for r in runs), runs)

        # corrupted database detection and repair
        net.stop("node3", signal.SIGTERM)
        log = os.path.join(RUN, "node3", "blocks.zfxlog")
        data = bytearray(open(log, "rb").read())
        mid = len(data) // 2
        nl = data.index(b"\n", mid) + 1
        data[nl + 120] ^= 0x01
        open(log, "wb").write(bytes(data))
        rc = net.start_node(3, [1], cfg_path, expect_fail=True)
        logtxt = open(os.path.join(RUN, "node3.log")).read()
        detected = rc != 0 and "corrupt at line" in logtxt
        net.start_node(3, [1], cfg_path, extra=["--repair"])
        wait_for(lambda: status(3)["best_hash"] == status(1)["best_hash"], 180, "repaired node resync")
        ok_all &= check("A2 corrupted block log detected (refuses to start) and --repair truncates, revalidates, resyncs",
                        detected and status(3)["load_report"]["repaired_from_line"] is not None,
                        {"exit_code_on_corruption": rc, "load_report": status(3)["load_report"]})

        # admin RPC needs auth
        r = rpc(1, "zfx_admin_snapshot_export", {"path": os.path.join(RUN, "snap.json")})
        ok_all &= check("A5 admin RPC refused without bearer token", r.get("error", {}).get("code") == 1004, r.get("error"))
        REPORT["final"] = {f"node{i}": status(i) for i in (1, 2, 3)}
    except Exception as e:  # record, never hide
        ok_all = False
        check("e2e-native aborted", False, {"exception": repr(e)})
    finally:
        net.stop_all()
    REPORT["passed"] = sum(c["result"] == "PASS" for c in REPORT["checks"])
    REPORT["total"] = len(REPORT["checks"])
    out = os.path.join(ROOT, "bench", "results", "e2e_native.json")
    json.dump(REPORT, open(out, "w"), indent=2)
    print(f"{REPORT['passed']}/{REPORT['total']} checks passed -> {out}")
    sys.exit(0 if ok_all and REPORT["passed"] == REPORT["total"] else 1)


if __name__ == "__main__":
    main()
