"""One turn of a coding agent, as an agent harness would send it through any
OpenAI-compatible client (OPENAI_BASE_URL / OPENAI_API_KEY). Standard library only.

  OPENAI_BASE_URL=http://127.0.0.1:<port>/v1 OPENAI_API_KEY=<local key> python3 -I agent_turn.py <case>

Cases: leaky (a .env with a cloud key), clean (personal details and a
codename, no credentials), stream (same as clean, streamed).
"""
import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ["OPENAI_BASE_URL"].rstrip("/")
KEY = os.environ["OPENAI_API_KEY"]
CASE = sys.argv[1]

env_file = "DATABASE_URL=postgres://app:hunter2-Strong-Pass@db.internal:5432/app\nAWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n"
notes = "Owner: Sam Okafor <sam.okafor@example.dev>, on-call phone 415-555-0123. Project codename Bluefin-Orchid-42 ships Friday."
messages = [
    {"role": "system", "content": "You are a coding agent working in the user's repository."},
    {"role": "user", "content": "Read the config and write a deploy note for the team. Mention who owns it."},
    {"role": "assistant", "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": json.dumps({"path": ".env" if CASE == "leaky" else "NOTES.md"})}}]},
    # In an agent loop the tool result is the last message; the model continues from it.
    {"role": "tool", "tool_call_id": "call_1", "content": env_file if CASE == "leaky" else notes},
]
body = {"model": "default/mock-echo", "messages": messages, "tools": [{"type": "function", "function": {"name": "read_file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}}], "stream": CASE == "stream"}
req = urllib.request.Request(BASE + "/chat/completions", data=json.dumps(body).encode(), headers={"Authorization": "Bearer " + KEY, "Content-Type": "application/json", "X-ZFX-Conversation": "demo-session-1"})
try:
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = r.read().decode()
        print("route:", r.headers.get("x-zfx-privacy-route"))
        if CASE == "stream":
            text = "".join((json.loads(l[6:])["choices"][0]["delta"].get("content") or "") for l in raw.splitlines() if l.startswith("data: {") and json.loads(l[6:]).get("choices"))
        else:
            text = json.loads(raw)["choices"][0]["message"]["content"]
        print("reply:", text)
except urllib.error.HTTPError as e:
    err = json.loads(e.read().decode())["error"]
    print("refused:", e.code, err["code"], "-", err["message"], "categories:", err.get("categories"))
