"""NEAR access for the intents components: views over a keep-alive JSON-RPC
connection, signed calls through the zfx-bridge CLI (Rust NEAR client)."""
import base64
import http.client
import json
import os
import subprocess
import threading
from urllib.parse import urlparse

from .protocol import BIN

ENV = dict(os.environ, DYLD_FALLBACK_LIBRARY_PATH=os.path.expanduser("~/.cache/zeroflux/xz/lib"))
_tls = threading.local()


class Near:
    def __init__(self, rpc):
        self.rpc = rpc
        u = urlparse(rpc)
        self.host, self.port = u.hostname, u.port

    def _post(self, body):
        for attempt in range(2):
            c = getattr(_tls, "c", None)
            if c is None or getattr(_tls, "hp", None) != (self.host, self.port):
                c = _tls.c = http.client.HTTPConnection(self.host, self.port, timeout=60)
                _tls.hp = (self.host, self.port)
            try:
                c.request("POST", "/", body=json.dumps(body), headers={"content-type": "application/json"})
                return json.loads(c.getresponse().read())
            except Exception:
                c.close()
                _tls.c = None
                if attempt:
                    raise

    def view(self, contract, method, args=None, finality="final"):
        r = self._post({"jsonrpc": "2.0", "id": 1, "method": "query", "params": {"request_type": "call_function", "finality": finality,
                        "account_id": contract, "method_name": method, "args_base64": base64.b64encode(json.dumps(args or {}).encode()).decode()}})
        if "error" in r or "error" in r.get("result", {}):
            raise RuntimeError(f"view {method}: {r.get('error') or r['result'].get('error')}")
        return json.loads(bytes(r["result"]["result"]))

    def call(self, signer, key_file, contract, method, args, deposit=0, gas=100):
        r = subprocess.run([f"{BIN}/zfx-bridge", "call", "--rpc", self.rpc, "--signer", signer, "--key-file", key_file, "--contract", contract,
                            "--method", method, "--args", json.dumps(args), "--deposit", str(deposit), "--gas", str(gas)],
                           capture_output=True, text=True, env=ENV)
        lines = [l for l in r.stdout.strip().splitlines() if l.startswith("{")]
        if not lines:
            return {"success": False, "failure": r.stderr[-500:]}
        return json.loads(lines[-1])
