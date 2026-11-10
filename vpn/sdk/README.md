# ZFX VPN client helpers

ZFX VPN speaks the OpenAI Chat Completions and Responses APIs, so any OpenAI
SDK works by changing the base URL. This directory has a small helper for the
parts the OpenAI SDKs do not model: the `zfx_privacy` request extension, the
route headers and the route record endpoint.

- `python/zfx_vpn.py`: standard library only. `ZfxClient(base_url, api_key).chat(...)`
  returns the reply plus the parsed `X-ZFX-Privacy-Route`; `privacy_record(request_id)`
  fetches `GET /v1/zfx/requests/{id}/privacy`; `PrivacyOptions` validates the extension
  before sending.

With the official OpenAI Python SDK, pass the extension through `extra_body`:

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:8787/v1", api_key=LOCAL_KEY)
r = client.chat.completions.with_raw_response.create(
    model="frontier/gpt-x",
    messages=[{"role": "user", "content": "..."}],
    extra_body={"zfx_privacy": {"mode": "transform_only"}},
)
print(r.headers["x-zfx-privacy-route"])
```

The extension is stripped before anything is forwarded upstream, so strict
upstream APIs never see it.

Protected transport (local preprocessing) is not an SDK feature: run the local
`zfx-vpn` daemon and add the hosted gateway with
`zfx-vpn destination add-hosted`. Detection and the placeholder map then stay
in the daemon, and applications keep talking plain OpenAI to `127.0.0.1`.
