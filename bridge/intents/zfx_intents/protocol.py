"""NEAR Intents protocol encoding (Verifier defuse/v0.4.2).

- Token ids: "nep141:<contract>".
- Nonce (32 bytes, base64): VERSIONED_MAGIC 5628f6c6 | borsh(VersionedNonce::V1(
  SaltedNonce{salt:[u8;4], nonce: ExpirableNonce{deadline: i64 ns, nonce:[u8;15]}}))
  = magic(4) | 0x00 | salt(4) | deadline_ns(8, LE) | random(15).
- NEP-413 message: JSON string {"deadline": ISO-8601, "intents": [...], "signer_id": ...};
  payload.recipient = Verifier account; signature over the NEP-413 hash.
- MultiPayload (nep413): {"standard":"nep413","payload":{"recipient","nonce","message"},
  "public_key":"ed25519:<b58>","signature":"ed25519:<b58>"}.
"""
import base64
import datetime as dt
import json
import os
import struct
import subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
BIN = os.path.join(ROOT, "target", "release")
VERSIONED_MAGIC = bytes.fromhex("5628f6c6")


def token_id(contract):
    return f"nep141:{contract}"


def iso(ts):
    return dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def make_nonce(salt_hex, deadline_ts, rnd=None):
    rnd = rnd if rnd is not None else os.urandom(15)
    assert len(rnd) == 15
    b = VERSIONED_MAGIC + b"\x00" + bytes.fromhex(salt_hex) + struct.pack("<q", int(deadline_ts * 1e9)) + rnd
    assert len(b) == 32
    return base64.b64encode(b).decode()


def parse_nonce(b64):
    b = base64.b64decode(b64)
    if b[:4] != VERSIONED_MAGIC or b[4] != 0:
        return {"versioned": False}
    return {"versioned": True, "salt": b[5:9].hex(), "deadline_ns": struct.unpack("<q", b[9:17])[0], "random": b[17:].hex()}


def token_diff(diff):
    """diff: {token_id: int} (positive = receive, negative = give)."""
    return {"intent": "token_diff", "diff": {k: str(v) for k, v in diff.items()}}


def nep413_message(signer_id, deadline_ts, intents):
    return json.dumps({"deadline": iso(deadline_ts), "intents": intents, "signer_id": signer_id}, separators=(",", ":"))


def sign_nep413(key_file, message, nonce_b64, recipient):
    r = subprocess.run([f"{BIN}/zfx-bridge", "sign-nep413", "--key-file", key_file, "--message", message, "--nonce-b64", nonce_b64,
                        "--recipient", recipient], capture_output=True, text=True, check=True)
    return json.loads(r.stdout)


def signed_intent(key_file, signer_id, verifier, salt_hex, intents, deadline_s=60):
    deadline = dt.datetime.now(tz=dt.timezone.utc).timestamp() + deadline_s
    nonce = make_nonce(salt_hex, deadline)
    msg = nep413_message(signer_id, deadline, intents)
    sig = sign_nep413(key_file, msg, nonce, verifier)
    return {"standard": "nep413", "payload": {"recipient": verifier, "nonce": nonce, "message": msg},
            "public_key": sig["public_key"], "signature": sig["signature"]}


def public_key_of(key_file):
    return json.load(open(key_file))["public_key"]
