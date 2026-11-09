"""Test solver / market maker for wZFX <-> one test asset (tUSDC).

Connects to a Message Bus WebSocket (local, or the real one with a Partner
JWT), subscribes to quote requests for its pair, and answers with a signed
NEP-413 token_diff priced from a configured reference price with a spread.
Risk limits: quotes only if its Verifier inventory of the output token
covers the trade and the trade stays under `max_trade_fraction` of that
inventory; refuses when the price feed is older than `max_price_age_s`.

    python -m zfx_intents.solver --ws URL --near-rpc URL --verifier intents.test.near \
        --account solver.test.near --key FILE --wzfx wzfx.deployer.test.near --tusdc tusdc.deployer.test.near \
        --price-usdc-per-zfx 2.5 --spread-bps 50 [--price-file FILE] [--api-key JWT]
"""
import argparse
import asyncio
import json
import os
import time

from websockets.asyncio.client import connect

from .near import Near
from . import protocol as P


class Solver:
    def __init__(self, a):
        self.a = a
        self.near = Near(a.near_rpc)
        self.wzfx, self.tusdc = P.token_id(a.wzfx), P.token_id(a.tusdc)
        self.salt = None
        self.salt_at = 0
        self.stats = {"quotes_seen": 0, "quoted": 0, "skipped_liquidity": 0, "skipped_stale_price": 0, "skipped_pair": 0, "settled": 0}

    def price(self):
        """Reference price: tUSDC base units (6 dp) per 1 ZFX (1e8 base units)."""
        if self.a.price_file and os.path.exists(self.a.price_file):
            j = json.load(open(self.a.price_file))
            return float(j["usdc_per_zfx"]), float(j["timestamp"])
        return self.a.price_usdc_per_zfx, time.time()

    def inventory(self, token):
        bal = self.near.view(self.a.verifier, "mt_balance_of", {"account_id": self.a.account, "token_id": token})
        return int(bal)

    def current_salt(self):
        if self.salt is None or time.time() - self.salt_at > 30:
            self.salt = self.near.view(self.a.verifier, "current_salt", {})
            self.salt_at = time.time()
        return self.salt

    def make_quote(self, ev):
        tin, tout = ev["defuse_asset_identifier_in"], ev["defuse_asset_identifier_out"]
        if {tin, tout} != {self.wzfx, self.tusdc}:
            self.stats["skipped_pair"] += 1
            return None
        px, ts = self.price()
        if time.time() - ts > self.a.max_price_age_s:
            self.stats["skipped_stale_price"] += 1
            return None
        spread = self.a.spread_bps / 10_000
        rate = px * 1e6 / 1e8  # tUSDC units per wZFX unit
        if "exact_amount_in" in ev:
            ain = int(ev["exact_amount_in"])
            aout = int(ain * rate * (1 - spread)) if tin == self.wzfx else int(ain / rate * (1 - spread))
        else:
            aout = int(ev["exact_amount_out"])
            ain = int(aout / rate / (1 - spread)) + 1 if tin == self.wzfx else int(aout * rate / (1 - spread)) + 1
        if aout <= 0 or ain <= 0:
            return None
        inv = self.inventory(tout)
        if aout > inv or aout > inv * self.a.max_trade_fraction:
            self.stats["skipped_liquidity"] += 1
            return None
        deadline_s = max(ev.get("min_deadline_ms", 60000) / 1000, 10)
        intents = [P.token_diff({tin: ain, tout: -aout})]
        sd = P.signed_intent(self.a.key, self.a.account, self.a.verifier, self.current_salt(), intents, deadline_s=deadline_s)
        out = {"amount_out": str(aout)} if "exact_amount_in" in ev else {"amount_in": str(ain)}
        return {"quote_id": ev["quote_id"], "quote_output": out, "signed_data": sd}

    async def run(self):
        headers = {"Authorization": f"Bearer {self.a.api_key}"} if self.a.api_key else None
        async with connect(self.a.ws, additional_headers=headers) as ws:
            await ws.send(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "subscribe", "params": ["quote", {"tokens_in": [self.wzfx, self.tusdc], "tokens_out": [self.wzfx, self.tusdc]}]}))
            await ws.send(json.dumps({"jsonrpc": "2.0", "id": 2, "method": "subscribe", "params": ["quote_status"]}))
            print(json.dumps({"solver": self.a.account, "ws": self.a.ws, "pair": [self.wzfx, self.tusdc]}), flush=True)
            nid = 10
            async for raw in ws:
                m = json.loads(raw)
                if m.get("method") != "subscribe":
                    continue
                p = m["params"]
                if "quote_id" in p and "defuse_asset_identifier_in" in p:
                    self.stats["quotes_seen"] += 1
                    try:
                        q = await asyncio.to_thread(self.make_quote, p)
                    except Exception as e:
                        print(json.dumps({"error": repr(e)}), flush=True)
                        q = None
                    if q:
                        nid += 1
                        await ws.send(json.dumps({"jsonrpc": "2.0", "id": nid, "method": "quote_response", "params": [q]}))
                        self.stats["quoted"] += 1
                elif "intent_hash" in p:
                    self.stats["settled"] += 1
                    print(json.dumps({"settled": p, "stats": self.stats}), flush=True)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--ws", required=True)
    ap.add_argument("--near-rpc", required=True)
    ap.add_argument("--verifier", required=True)
    ap.add_argument("--account", required=True)
    ap.add_argument("--key", required=True)
    ap.add_argument("--wzfx", required=True)
    ap.add_argument("--tusdc", required=True)
    ap.add_argument("--price-usdc-per-zfx", type=float, default=2.5)
    ap.add_argument("--price-file")
    ap.add_argument("--max-price-age-s", type=float, default=30)
    ap.add_argument("--spread-bps", type=float, default=50)
    ap.add_argument("--max-trade-fraction", type=float, default=0.5)
    ap.add_argument("--api-key")
    asyncio.run(Solver(ap.parse_args()).run())
