"""NEAR Intents adapter: intents.quote / intents.submit / intents.status.

Speaks the Message Bus JSON-RPC (docs.near-intents.org/integration/market-makers/
message-bus/rpc). Against the public relay it sends the Partner JWT in
`X-API-Key`; against the local bus no key is needed. The user's side of a
swap is a NEP-413 token_diff that mirrors the chosen quote exactly.

    python -m zfx_intents.adapter quote  --bus URL --in nep141:A --out nep141:B --amount-in N
    python -m zfx_intents.adapter status --bus URL --intent-hash H
"""
import argparse
import json
import time
import urllib.request

from . import protocol as P

PUBLIC_RELAY = "https://solver-relay-v2.chaindefuser.com/rpc"
TERMINAL = ("SETTLED", "NOT_FOUND_OR_NOT_VALID")


class IntentsAdapter:
    def __init__(self, bus_url, api_key=None, timeout=15):
        self.url, self.api_key, self.timeout = bus_url, api_key, timeout
        self._id = 0

    def _rpc(self, method, params):
        self._id += 1
        body = json.dumps({"jsonrpc": "2.0", "id": self._id, "method": method, "params": [params]}).encode()
        h = {"content-type": "application/json"}
        if self.api_key:
            h["X-API-Key"] = self.api_key
        req = urllib.request.Request(self.url, data=body, headers=h)
        try:
            r = json.loads(urllib.request.urlopen(req, timeout=self.timeout).read())
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"HTTP {e.code}: {e.read()[:300].decode(errors='replace')}")
        if "error" in r:
            raise RuntimeError(f"{method}: {r['error']}")
        return r["result"]

    # intents.quote
    def quote(self, token_in, token_out, exact_amount_in=None, exact_amount_out=None, min_deadline_ms=60000):
        if (exact_amount_in is None) == (exact_amount_out is None):
            raise ValueError("exactly one of exact_amount_in / exact_amount_out")
        p = {"defuse_asset_identifier_in": token_in, "defuse_asset_identifier_out": token_out, "min_deadline_ms": min_deadline_ms}
        if exact_amount_in is not None:
            p["exact_amount_in"] = str(exact_amount_in)
        else:
            p["exact_amount_out"] = str(exact_amount_out)
        return self._rpc("quote", p)

    # intents.submit
    def submit(self, quote, signer_id, key_file, verifier, salt_hex, deadline_s=60):
        diff = {quote["defuse_asset_identifier_in"]: -int(quote["amount_in"]), quote["defuse_asset_identifier_out"]: int(quote["amount_out"])}
        sd = P.signed_intent(key_file, signer_id, verifier, salt_hex, [P.token_diff(diff)], deadline_s=deadline_s)
        res = self._rpc("publish_intent", {"quote_hashes": [quote["quote_hash"]], "signed_data": sd})
        return res, sd

    # intents.status
    def status(self, intent_hash):
        return self._rpc("get_status", {"intent_hash": intent_hash})

    def wait(self, intent_hash, timeout=120):
        t = time.time()
        while time.time() - t < timeout:
            s = self.status(intent_hash)
            if s["status"] in TERMINAL:
                return s
            time.sleep(1)
        return self.status(intent_hash)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["quote", "status"])
    ap.add_argument("--bus", default=PUBLIC_RELAY)
    ap.add_argument("--api-key")
    ap.add_argument("--in", dest="tin")
    ap.add_argument("--out", dest="tout")
    ap.add_argument("--amount-in")
    ap.add_argument("--intent-hash")
    a = ap.parse_args()
    ad = IntentsAdapter(a.bus, a.api_key)
    print(json.dumps(ad.quote(a.tin, a.tout, exact_amount_in=a.amount_in) if a.cmd == "quote" else ad.status(a.intent_hash), indent=2))
