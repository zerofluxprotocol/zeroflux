"""ZFX VPN client helper (standard library only).

    from zfx_vpn import ZfxClient, PrivacyOptions
    c = ZfxClient("http://127.0.0.1:8787/v1", api_key)
    out = c.chat([{"role": "user", "content": "..."}], model="frontier/gpt-x",
                 privacy=PrivacyOptions(mode="transform_only"))
    out.text, out.route["tier"], c.privacy_record(out.request_id)
"""
import json
import urllib.error
import urllib.request
from dataclasses import dataclass, field

MODES = ("auto", "transform_only", "confidential", "local_only")


class ZfxError(Exception):
    """An error envelope from ZFX VPN. `code` is ZFX_*, `legacy_code` the Prompt VPN 0.1 name."""

    def __init__(self, status, error):
        self.status = status
        self.code = error.get("code")
        self.legacy_code = error.get("legacy_code")
        self.message = error.get("message", "")
        self.categories = error.get("categories", [])
        self.request_id = error.get("event_id")
        super().__init__(f"{status} {self.code}: {self.message}")


@dataclass
class PrivacyOptions:
    mode: str = "auto"
    fallback_model: str = None
    on_unavailable: str = "block"
    stream_buffering: str = "allow"
    require_receipt: bool = False
    conversation_id: str = None

    def to_json(self):
        if self.mode not in MODES:
            raise ValueError(f"mode must be one of {MODES}")
        if self.on_unavailable not in ("block", "review"):
            raise ValueError("on_unavailable must be block or review")
        if self.stream_buffering not in ("allow", "reject"):
            raise ValueError("stream_buffering must be allow or reject")
        d = {"mode": self.mode, "on_unavailable": self.on_unavailable, "stream_buffering": self.stream_buffering, "require_receipt": self.require_receipt}
        if self.fallback_model:
            d["fallback_model"] = self.fallback_model
        if self.conversation_id:
            d["conversation_id"] = self.conversation_id
        return d


def parse_route(header):
    """`tier=X; action=Y; dest=Z; ...` -> dict. Never contains user content."""
    out = {}
    for part in (header or "").split(";"):
        if "=" in part:
            k, v = part.strip().split("=", 1)
            out[k] = v
    return out


@dataclass
class ChatResult:
    text: str
    request_id: str
    route: dict = field(default_factory=dict)
    notices: list = field(default_factory=list)
    trust: str = None
    raw: object = None


class ZfxClient:
    def __init__(self, base_url, api_key, timeout=60):
        self.base = base_url.rstrip("/")
        self.key = api_key
        self.timeout = timeout

    def _req(self, method, path, body=None, headers=None):
        h = {"Authorization": "Bearer " + self.key}
        if body is not None:
            h["Content-Type"] = "application/json"
        h.update(headers or {})
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.base + path, data=data, headers=h, method=method)
        try:
            r = urllib.request.urlopen(req, timeout=self.timeout)
        except urllib.error.HTTPError as e:
            try:
                err = json.loads(e.read().decode()).get("error", {})
            except ValueError:
                err = {"message": "non-JSON error"}
            raise ZfxError(e.code, err) from None
        return r

    def chat(self, messages, model, privacy=None, stream=False, idempotency_key=None, **extra):
        body = dict(extra, model=model, messages=messages, stream=stream)
        if privacy is not None:
            body["zfx_privacy"] = privacy.to_json()
        headers = {"Idempotency-Key": idempotency_key} if idempotency_key else {}
        with self._req("POST", "/chat/completions", body, headers) as r:
            raw = r.read().decode()
            route = parse_route(r.headers.get("x-zfx-privacy-route"))
            notices = [n.strip() for n in (r.headers.get("x-zfx-notice") or "").split(",") if n.strip()]
            rid = r.headers.get("x-zfx-request-id")
            trust = r.headers.get("x-zfx-trust")
        if stream:
            text = ""
            for line in raw.splitlines():
                if not line.startswith("data: ") or line == "data: [DONE]":
                    continue
                ev = json.loads(line[6:])
                if "error" in ev:
                    raise ZfxError(200, ev["error"])
                for ch in ev.get("choices", []):
                    text += (ch.get("delta") or {}).get("content") or ""
            return ChatResult(text, rid, route, notices, trust, raw)
        v = json.loads(raw)
        return ChatResult(v["choices"][0]["message"].get("content") or "", rid, route, notices, trust, v)

    def privacy_record(self, request_id):
        with self._req("GET", f"/zfx/requests/{request_id}/privacy") as r:
            return json.loads(r.read().decode())

    def models(self):
        with self._req("GET", "/models") as r:
            return json.loads(r.read().decode())["data"]
