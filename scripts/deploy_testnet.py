#!/usr/bin/env python3
"""Deploy the ZFX mining contract (zfx-mine-opt) to NEAR testnet.

    NEAR_TESTNET_ACCOUNT=zeroflux.testnet .venv/bin/python scripts/deploy_testnet.py [--dry-run]

Creates mine.<account> holding the contract (about 3 NEAR: code storage plus
headroom) and initialises it with testnet parameters. Then:

    zfx-bridge near-opt-watch --rpc https://rpc.testnet.near.org --signer <acct> --key-file <key> --contract mine.<account>
    zfx-bridge near-mine-proxy --optimistic --bond-near 0.1 --rpc https://rpc.testnet.near.org --signer <miner> --key-file <key> --contract mine.<account>
    zpu-cli mine --node http://127.0.0.1:9551

Never run against mainnet; there is no mainnet ZFX.
"""
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release")
RPC = os.environ.get("NEAR_TESTNET_RPC", "https://rpc.testnet.near.org")
PARAMS = {"genesis_seed": "zfx-testnet-1", "genesis_target": "0000" + "f" * 60, "spacing_secs": 60, "reward": str(50 * 10**8),
          "bond": str(10**23), "window_secs": 600}


def main():
    acct = os.environ.get("NEAR_TESTNET_ACCOUNT")
    if not acct or not acct.endswith(".testnet"):
        sys.exit("set NEAR_TESTNET_ACCOUNT=<account>.testnet")
    key = os.path.expanduser(f"~/.near-credentials/testnet/{acct}.json")
    if not os.path.exists(key):
        sys.exit(f"missing {key}")
    wasm = os.path.join(ROOT, "bridge", "near", "target", "near", "zfx_mine_opt", "zfx_mine_opt.wasm")
    cmd = [f"{BIN}/zfx-bridge", "deploy", "--rpc", RPC, "--signer", acct, "--key-file", key, "--contract", f"mine.{acct}", "--wasm", wasm,
           "--init", json.dumps(PARAMS), "--deposit-near", "3"]
    print(" ".join(cmd[:10]) + " ...", flush=True)
    if "--dry-run" in sys.argv:
        return
    r = subprocess.run(cmd, capture_output=True, text=True)
    print(r.stdout.strip().splitlines()[-1] if r.stdout.strip() else r.stderr[-800:])


if __name__ == "__main__":
    main()
