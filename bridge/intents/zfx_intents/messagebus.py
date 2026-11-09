"""Local NEAR Intents Message Bus for sandbox settlement.

Implements the documented interfaces (docs.near-intents.org, 2026-11-09):
  JSON-RPC  POST /rpc   quote, publish_intent, publish_intents, get_status
  WebSocket /ws         subscribe("quote" | "quote_status", filters), quote_response
Matching and settlement: the bus pairs the user's signed token_diff with the
solver's signed quote and submits both to the real Verifier contract
(near/intents defuse/v0.4.2) via simulate_intents then execute_intents,
signed by a relay account. Statuses: PENDING, TX_BROADCASTED, SETTLED,
NOT_FOUND_OR_NOT_VALID. Not a production service: one process, in memory.

    python -m zfx_intents.messagebus --near-rpc URL --verifier intents.test.near \
        --relay-account relay.test.near --relay-key FILE --http-port 28810 --ws-port 28811
"""
import argparse
import asyncio
import hashlib
import http.server
import json
import threading
import time
import uuid

import websockets
from websockets.asyncio.server import serve

from .near import Near
from .protocol import parse_nonce

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58(b):
    n = int.from_bytes(b, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s


class Bus:
    def __init__(self, near, verifier, relay_account, relay_key):
        self.near, self.verifier = near, verifier
        self.relay_account, self.relay_key = relay_account, relay_key
        self.loop = None
        self.subs = {}  # ws -> {sub_id: (kind, filters)}
        self.auctions = {}  # quote_id -> {"req":..., "responses": [...], "open": bool}
        self.quotes = {}  # quote_hash -> {"quote_id", "req", "signed_data", "amount_in", "amount_out", "expires", "used"}
        self.intents = {}  # intent_hash -> status dict
        self.lock = threading.Lock()
        self.log = []

    # ---------------- WebSocket side (solvers) ----------------
    async def ws_handler(self, ws):
        self.subs[ws] = {}
        try:
            async for raw in ws:
                try:
                    m = json.loads(raw)
                except Exception:
                    await ws.send(json.dumps({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}}))
                    continue
                mid, method, params = m.get("id"), m.get("method"), m.get("params") or []
                if method == "subscribe":
                    kind = params[0] if params else None
                    if kind not in ("quote", "quote_status"):
                        await ws.send(json.dumps({"jsonrpc": "2.0", "id": mid, "error": {"code": -32602, "message": "unknown subscription"}}))
                        continue
                    sid = str(uuid.uuid4())
                    self.subs[ws][sid] = (kind, (params[1] if len(params) > 1 and params[1] else {}))
                    await ws.send(json.dumps({"jsonrpc": "2.0", "id": mid, "result": sid}))
                elif method == "unsubscribe":
                    self.subs[ws].pop(params[0] if params else None, None)
                    await ws.send(json.dumps({"jsonrpc": "2.0", "id": mid, "result": "OK"}))
                elif method == "quote_response":
                    p = params[0] if params else {}
                    err = self.accept_quote_response(p)
                    if err:
                        await ws.send(json.dumps({"jsonrpc": "2.0", "id": mid, "error": {"code": -32000, "message": err}}))
                    else:
                        await ws.send(json.dumps({"jsonrpc": "2.0", "id": mid, "result": "OK"}))
                else:
                    await ws.send(json.dumps({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}}))
        except websockets.ConnectionClosed:
            pass
        finally:
            self.subs.pop(ws, None)

    async def broadcast(self, kind, params, token_in=None, token_out=None):
        for ws, subs in list(self.subs.items()):
            for sid, (k, f) in subs.items():
                if k != kind:
                    continue
                if kind == "quote":
                    if f.get("tokens_in") and token_in not in f["tokens_in"]:
                        continue
                    if f.get("tokens_out") and token_out not in f["tokens_out"]:
                        continue
                try:
                    await ws.send(json.dumps({"jsonrpc": "2.0", "method": "subscribe", "params": dict(params, subscription=sid)}))
                except Exception:
                    pass

    def accept_quote_response(self, p):
        qid = p.get("quote_id")
        with self.lock:
            a = self.auctions.get(qid)
            if not a or not a["open"]:
                return "quote not found or already finished"
            sd = p.get("signed_data")
            if not sd or sd.get("standard") != "nep413":
                return "signed_data must be a nep413 MultiPayload"
            out = p.get("quote_output") or {}
            a["responses"].append({"signed_data": sd, "amount_out": out.get("amount_out"), "amount_in": out.get("amount_in")})
        return None

    # ---------------- JSON-RPC side (users / distribution channels) ----------------
    def rpc(self, req):
        mid, method, params = req.get("id"), req.get("method"), (req.get("params") or [{}])
        p = params[0] if isinstance(params, list) and params else (params if isinstance(params, dict) else {})
        try:
            if method == "quote":
                return {"jsonrpc": "2.0", "id": mid, "result": self.quote(p)}
            if method == "publish_intent":
                return {"jsonrpc": "2.0", "id": mid, "result": self.publish(p.get("quote_hashes", []), p.get("signed_data"))}
            if method == "publish_intents":
                res = [self.publish([qh], sd) for qh, sd in zip(p.get("quote_hashes", []), p.get("signed_datas", []))]
                return {"jsonrpc": "2.0", "id": mid, "result": res[0] if len(res) == 1 else res}
            if method == "get_status":
                st = self.intents.get(p.get("intent_hash"))
                if not st:
                    st = {"intent_hash": p.get("intent_hash"), "status": "NOT_FOUND_OR_NOT_VALID", "status_details": "unknown intent hash"}
                return {"jsonrpc": "2.0", "id": mid, "result": st}
            return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}}
        except Exception as e:
            return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32000, "message": str(e)}}

    def quote(self, p):
        if bool(p.get("exact_amount_in")) == bool(p.get("exact_amount_out")):
            raise ValueError("exactly one of exact_amount_in / exact_amount_out is required")
        qid = str(uuid.uuid4())
        min_wait = max(int(p.get("min_wait_ms", 300)), 300)
        max_wait = int(p.get("max_wait_ms", 3000))
        ev = {"quote_id": qid, "defuse_asset_identifier_in": p["defuse_asset_identifier_in"], "defuse_asset_identifier_out": p["defuse_asset_identifier_out"],
              "min_deadline_ms": int(p.get("min_deadline_ms", 60000)), "min_wait_ms": min_wait, "max_wait_ms": max_wait, "protocol_fee_included": False}
        if p.get("exact_amount_in"):
            ev["exact_amount_in"] = str(p["exact_amount_in"])
        else:
            ev["exact_amount_out"] = str(p["exact_amount_out"])
        with self.lock:
            self.auctions[qid] = {"req": ev, "responses": [], "open": True}
        asyncio.run_coroutine_threadsafe(self.broadcast("quote", ev, ev["defuse_asset_identifier_in"], ev["defuse_asset_identifier_out"]), self.loop)
        t0 = time.time()
        while time.time() - t0 < max_wait / 1000:
            time.sleep(0.02)
            elapsed = (time.time() - t0) * 1000
            if elapsed >= min_wait and self.auctions[qid]["responses"] and elapsed >= min_wait + 300:
                break
        with self.lock:
            a = self.auctions[qid]
            a["open"] = False
            out = []
            for r in a["responses"]:
                qh = hashlib.sha256(json.dumps(r["signed_data"], sort_keys=True).encode()).hexdigest()[:32]
                n = parse_nonce(r["signed_data"]["payload"]["nonce"])
                exp = n.get("deadline_ns", 0) / 1e9 if n.get("versioned") else time.time() + ev["min_deadline_ms"] / 1000
                amount_in = ev.get("exact_amount_in") or r["amount_in"]
                amount_out = ev.get("exact_amount_out") or r["amount_out"]
                self.quotes[qh] = {"quote_id": qid, "req": ev, "signed_data": r["signed_data"], "amount_in": str(amount_in), "amount_out": str(amount_out),
                                   "expires": exp, "used": False}
                out.append({"quote_hash": qh, "defuse_asset_identifier_in": ev["defuse_asset_identifier_in"], "defuse_asset_identifier_out": ev["defuse_asset_identifier_out"],
                            "amount_in": str(amount_in), "amount_out": str(amount_out),
                            "expiration_time": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(exp))})
        # best price first
        out.sort(key=lambda q: -int(q["amount_out"]) if ev.get("exact_amount_in") else int(q["amount_in"]))
        return out

    def publish(self, quote_hashes, signed):
        if not signed or not quote_hashes:
            return {"status": "FAILED", "reason": "quote_hashes and signed_data are required"}
        with self.lock:
            q = self.quotes.get(quote_hashes[0])
            if not q:
                return {"status": "FAILED", "reason": "quote not found"}
            if q["used"]:
                return {"status": "FAILED", "reason": "quote already used (duplicate fill)"}
            if time.time() > q["expires"]:
                return {"status": "FAILED", "reason": "quote expired"}
            q["used"] = True
        # The user's diff must be the exact mirror of the solver's.
        try:
            u = json.loads(signed["payload"]["message"])["intents"][0]["diff"]
            s = json.loads(q["signed_data"]["payload"]["message"])["intents"][0]["diff"]
        except Exception:
            return {"status": "FAILED", "reason": "malformed token_diff"}
        if set(u) != set(s) or any(int(u[k]) != -int(s[k]) for k in u):
            with self.lock:
                q["used"] = False
            return {"status": "FAILED", "reason": "user diff does not match the quote"}
        ih = b58(hashlib.sha256(json.dumps(signed, sort_keys=True).encode()).digest())
        self.intents[ih] = {"intent_hash": ih, "status": "PENDING", "status_details": "received", "data": {}, "filled_amounts": []}
        threading.Thread(target=self.settle, args=(ih, [q["signed_data"], signed], quote_hashes[0]), daemon=True).start()
        return {"status": "OK", "intent_hash": ih}

    def settle(self, ih, signed_list, qh):
        st = self.intents[ih]
        try:
            sim = self.near.view(self.verifier, "simulate_intents", {"signed": signed_list})
            st["simulation"] = {"intents_executed": len(sim.get("intents_executed", []))}
        except Exception as e:
            st.update(status="NOT_FOUND_OR_NOT_VALID", status_details=f"simulation failed: {e}")
            return
        st.update(status="TX_BROADCASTED", status_details="execute_intents submitted")
        r = self.near.call(self.relay_account, self.relay_key, self.verifier, "execute_intents", {"signed": signed_list}, gas=250)
        if r.get("success"):
            msg = json.loads(signed_list[1]["payload"]["message"])
            diff = msg["intents"][0]["diff"]
            st.update(status="SETTLED", status_details="settled on the Verifier", data={"hash": r["tx_hash"]},
                      filled_amounts=[str(abs(int(v))) for v in diff.values()], logs=[l for l in r.get("logs", []) if "token_diff" in l][:2])
            asyncio.run_coroutine_threadsafe(self.broadcast("quote_status", {"quote_hash": qh, "intent_hash": ih, "tx_hash": r["tx_hash"]}), self.loop)
        else:
            st.update(status="NOT_FOUND_OR_NOT_VALID", status_details=f"execution error: {r.get('failure')}"[:500], data={"hash": r.get("tx_hash")})


def run(args):
    near = Near(args.near_rpc)
    bus = Bus(near, args.verifier, args.relay_account, args.relay_key)

    class H(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_POST(self):
            n = int(self.headers.get("content-length", 0))
            if n > 1 << 20:
                self.send_response(413)
                self.end_headers()
                return
            try:
                req = json.loads(self.rfile.read(n))
                out = [bus.rpc(r) for r in req] if isinstance(req, list) else bus.rpc(req)
            except Exception as e:
                out = {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": str(e)}}
            b = json.dumps(out).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)

        def log_message(self, *a):
            pass

    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", args.http_port), H)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    async def main():
        bus.loop = asyncio.get_running_loop()
        async with serve(bus.ws_handler, "127.0.0.1", args.ws_port):
            print(json.dumps({"rpc": f"http://127.0.0.1:{args.http_port}/rpc", "ws": f"ws://127.0.0.1:{args.ws_port}/ws"}), flush=True)
            await asyncio.Future()

    asyncio.run(main())


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--near-rpc", required=True)
    ap.add_argument("--verifier", required=True)
    ap.add_argument("--relay-account", required=True)
    ap.add_argument("--relay-key", required=True)
    ap.add_argument("--http-port", type=int, default=28810)
    ap.add_argument("--ws-port", type=int, default=28811)
    run(ap.parse_args())
