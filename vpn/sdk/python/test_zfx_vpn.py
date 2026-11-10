"""Tests for zfx_vpn.py against a tiny in-process server (no network, stdlib only).
    python3 -I sdk/python/test_zfx_vpn.py
"""
import http.server
import json
import os
import sys
import threading

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import zfx_vpn  # noqa: E402

seen = []


class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        seen.append((dict(self.headers), body))
        if "card" in body["messages"][0]["content"]:
            data = json.dumps({"error": {"code": "ZFX_POLICY_BLOCKED", "legacy_code": "PV_POLICY_BLOCKED", "message": "Request blocked by policy", "categories": ["CARD_NUMBER"], "event_id": "e2"}}).encode()
            self.send_response(403)
        else:
            data = json.dumps({"choices": [{"message": {"content": "hi"}}]}).encode()
            self.send_response(200)
            self.send_header("x-zfx-privacy-route", "tier=TRANSFORMED_PUBLIC; action=TRANSFORM; dest=frontier; verification=NOT_APPLICABLE; stream=none")
            self.send_header("x-zfx-request-id", "e1")
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass


srv = http.server.HTTPServer(("127.0.0.1", 0), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
c = zfx_vpn.ZfxClient(f"http://127.0.0.1:{srv.server_port}/v1", "zfx_sk_test")
r = c.chat([{"role": "user", "content": "hello"}], "frontier/x", privacy=zfx_vpn.PrivacyOptions(mode="transform_only"), idempotency_key="k1")
assert r.text == "hi" and r.request_id == "e1" and r.route["tier"] == "TRANSFORMED_PUBLIC", r
hdrs, body = seen[-1]
assert body["zfx_privacy"]["mode"] == "transform_only" and hdrs["Idempotency-Key"] == "k1"
try:
    c.chat([{"role": "user", "content": "my card"}], "frontier/x")
    raise AssertionError("expected error")
except zfx_vpn.ZfxError as e:
    assert e.code == "ZFX_POLICY_BLOCKED" and e.legacy_code == "PV_POLICY_BLOCKED" and e.status == 403
try:
    zfx_vpn.PrivacyOptions(mode="everything").to_json()
    raise AssertionError("expected ValueError")
except ValueError:
    pass
print("sdk tests: 3 passed")
