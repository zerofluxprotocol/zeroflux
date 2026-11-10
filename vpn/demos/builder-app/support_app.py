"""A tiny support-assistant backend, standing in for a builder's product.

End users send messages that contain their own personal data. The app calls
an OpenAI-compatible endpoint (here: ZFX VPN hosted) instead of the model
provider directly, so the model only sees placeholders and the end user gets
a reply with their details restored. Standard library only.

  ZFX_BASE_URL=http://127.0.0.1:<port>/v1 ZFX_API_KEY=zfx_sk_... python3 -I support_app.py <port>
"""
import http.server
import json
import os
import sys
import urllib.request

BASE = os.environ["ZFX_BASE_URL"].rstrip("/")
KEY = os.environ["ZFX_API_KEY"]


def ask_model(user_message, stream):
    body = {
        "model": "frontier/mock-echo",
        "messages": [
            {"role": "system", "content": "You are the Acme support assistant."},
            {"role": "user", "content": user_message},
        ],
        "stream": stream,
        # Strict, optional ZFX extension: never send this to a confidential
        # fallback; only transformed sends to the selected model.
        "zfx_privacy": {"mode": "transform_only", "on_unavailable": "block"},
    }
    req = urllib.request.Request(BASE + "/chat/completions", data=json.dumps(body).encode(), headers={"Authorization": "Bearer " + KEY, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            route = r.headers.get("x-zfx-privacy-route", "")
            trust = r.headers.get("x-zfx-trust", "")
            raw = r.read().decode()
    except urllib.error.HTTPError as e:
        err = json.loads(e.read().decode())["error"]
        return {"blocked": True, "code": err["code"], "message": err["message"]}
    if stream:
        text = ""
        for line in raw.splitlines():
            if line.startswith("data: ") and line != "data: [DONE]":
                chunk = json.loads(line[6:])
                for c in chunk.get("choices", []):
                    text += c.get("delta", {}).get("content") or ""
    else:
        text = json.loads(raw)["choices"][0]["message"]["content"]
    return {"reply": text, "privacy_route": route, "trust": trust}


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", "0"))
        msg = json.loads(self.rfile.read(n))
        out = ask_model(msg["message"], bool(msg.get("stream")))
        data = json.dumps(out).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
