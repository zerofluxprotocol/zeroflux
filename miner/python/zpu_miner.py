#!/usr/bin/env python3
"""zpu_miner.py: third independent ZFX-FP8C implementation (Python).

Backends (all compute the same canonical integers):
  numpy        CPU, float32 BLAS GEMM (exact: every |sum| < 2^24)
  torch        --device cuda (NVIDIA CUDA or AMD ROCm builds of PyTorch) | mps | cpu
               --gemm fp32 (TF32 disabled) | int8 (torch._int_mm) | fp8 (torch._scaled_mm, E4M3 / E4M3FNUZ)
  mlx          Apple Silicon GPU via MLX/Metal, float32 GEMM

Usage:
  zpu_miner.py [--backend numpy|torch|mlx] [...]        ZPU line protocol on stdin/stdout (driven by zpu-cli)
  zpu_miner.py selftest VECTORS.json [...]              bit-exactness vs published vectors
  zpu_miner.py bench [--profile v01|dev] [--count N] [...]

Hashing (SHA-256, SHAKE256) is hashlib; nothing is shared with the Rust or
C++ implementations except the specification.
"""
import argparse
import hashlib
import json
import struct
import sys
import time

import numpy as np

SBOX = bytes.fromhex(
    "637c777bf26b6fc53001672bfed7ab76ca82c97dfa5947f0add4a2af9ca472c0b7fd9326363ff7cc34a5e5f171d8311504c723c31896059a071280e2eb27b27509832c1a1b6e5aa0523bd6b329e32f8453d100ed20fcb15b6acbbe394a4c58cfd0efaafb434d338545f9027f503c9fa851a3408f929d38f5bcb6da2110fff3d2"
    "cd0c13ec5f974417c4a77e3d645d197360814fdc222a908846eeb814de5e0bdbe0323a0a4906245cc2d3ac629195e479e7c8376d8dd54ea96c56f4ea657aae08ba78252e1ca6b4c6e8dd741f4bbd8b8a703eb5664803f60e613557b986c11d9ee1f8981169d98e949b1e87e9ce5528df8ca1890dbfe6426841992d0fb054bb16"
)
VALS = np.array([u + 1 if u < 16 else 15 - u for u in range(32)], dtype=np.int8)  # 5-bit idx -> value
BYTE_TO_VAL = VALS[np.arange(256) & 31]
# requant LUT over the selected byte: value = VALS[SBOX[b] >> 3]
RQ_LUT = VALS[np.frombuffer(SBOX, dtype=np.uint8) >> 3]

PROFILES = {
    1: dict(id=1, name="zfx-fp8c-v0.1", d_in=64, hidden=4096, n_hidden=3, d_out=256, shift_in=0, shift_hidden=2),
    2: dict(id=2, name="zfx-fp8c-v0.1-dev", d_in=64, hidden=256, n_hidden=3, d_out=64, shift_in=0, shift_hidden=0),
    # NEAR-verifiable profile: ChaCha8 weights (see crates/zfx-verify/src/pow.rs).
    3: dict(id=3, name="zfx-near-v1", d_in=64, hidden=1024, n_hidden=3, d_out=256, shift_in=0, shift_hidden=1),
}


def layers(p):
    ls = [(p["d_in"], p["hidden"], p["shift_in"])]
    ls += [(p["hidden"], p["hidden"], p["shift_hidden"])] * p["n_hidden"]
    ls.append((p["hidden"], p["d_out"], None))
    return ls


def attempt_id(challenge: bytes, nonce: int) -> bytes:
    return hashlib.sha256(b"ZFX-attempt-v0.1" + challenge + struct.pack("<Q", nonce)).digest()


def x0(attempt: bytes, d_in: int) -> np.ndarray:
    b = hashlib.shake_256(b"ZFX-FP8C-X0-v0.1" + attempt).digest(d_in)
    return BYTE_TO_VAL[np.frombuffer(b, dtype=np.uint8)]


def _chacha8_layer(key: bytes, n: int, k: int) -> np.ndarray:
    """ChaCha8 keystream for rows 0..n-1 (nonce = row u32le || 0^8), k bytes
    per row, all blocks of the layer computed at once with uint32 arrays."""
    bpr = (k + 63) // 64
    rows = np.repeat(np.arange(n, dtype=np.uint32), bpr)
    ctr = np.tile(np.arange(bpr, dtype=np.uint32), n)
    kw = np.frombuffer(key, dtype="<u4")
    st = np.zeros((n * bpr, 16), dtype=np.uint32)
    st[:, 0:4] = [0x61707865, 0x3320646E, 0x79622D32, 0x6B206574]
    st[:, 4:12] = kw
    st[:, 12] = ctr
    st[:, 13] = rows
    x = st.copy()

    def rotl(v, c):
        return (v << np.uint32(c)) | (v >> np.uint32(32 - c))

    def qr(a, b, c, d):
        x[:, a] += x[:, b]; x[:, d] = rotl(x[:, d] ^ x[:, a], 16)
        x[:, c] += x[:, d]; x[:, b] = rotl(x[:, b] ^ x[:, c], 12)
        x[:, a] += x[:, b]; x[:, d] = rotl(x[:, d] ^ x[:, a], 8)
        x[:, c] += x[:, d]; x[:, b] = rotl(x[:, b] ^ x[:, c], 7)

    for _ in range(4):
        qr(0, 4, 8, 12); qr(1, 5, 9, 13); qr(2, 6, 10, 14); qr(3, 7, 11, 15)
        qr(0, 5, 10, 15); qr(1, 6, 11, 12); qr(2, 7, 8, 13); qr(3, 4, 9, 14)
    ks = (x + st).astype("<u4").view(np.uint8).reshape(n, bpr * 64)[:, :k]
    return BYTE_TO_VAL[ks]


def gen_weights(p, seed: bytes):
    out = []
    if p["id"] == 3:
        for l, (k, n, _) in enumerate(layers(p)):
            key = hashlib.sha256(b"ZFX-near-weights-v1" + seed + struct.pack("<I", l)).digest()
            out.append(_chacha8_layer(key, n, k))
        return out
    for l, (k, n, _) in enumerate(layers(p)):
        rows = [hashlib.shake_256(b"ZFX-FP8C-W-v0.1" + seed + struct.pack("<II", l, j)).digest(k) for j in range(n)]
        out.append(BYTE_TO_VAL[np.frombuffer(b"".join(rows), dtype=np.uint8)].reshape(n, k))
    return out


def requant_np(y: np.ndarray, s: int) -> np.ndarray:
    return RQ_LUT[(y.astype(np.int32) >> s) & 0xFF]


def digest(challenge: bytes, nonce: int, out_row: np.ndarray) -> bytes:
    return hashlib.sha256(b"ZFX-result-v0.1" + challenge + struct.pack("<Q", nonce) + out_row.astype("<i4").tobytes()).digest()


class Backend:
    """Base: x0 + hashing on CPU, layers delegated to `forward`."""

    name = "base"

    def __init__(self):
        self.key = None

    def load(self, p, seed):
        if self.key != (p["id"], seed):
            self.p = p
            self.prepare(gen_weights(p, seed))
            self.key = (p["id"], seed)

    def digests(self, challenge, nonces):
        X = np.stack([x0(attempt_id(challenge, n), self.p["d_in"]) for n in nonces])
        Y = self.forward(X)
        return [digest(challenge, n, Y[i]) for i, n in enumerate(nonces)]


class NumpyBackend(Backend):
    name = "numpy-f32blas"

    def prepare(self, ws):
        self.W = [w.astype(np.float32).T.copy() for w in ws]

    def raw_mm(self, X, W):
        return (X.astype(np.float32) @ W.astype(np.float32).T).astype(np.int32)

    def forward(self, X):
        x = X.astype(np.float32)
        for (k, n, s), w in zip(layers(self.p), self.W):
            y = (x @ w).astype(np.int32)  # exact integers in float32
            x = requant_np(y, s).astype(np.float32) if s is not None else None
        return y


class TorchBackend(Backend):
    def __init__(self, device, gemm):
        super().__init__()
        import torch

        self.t = torch
        self.dev = torch.device(device)
        self.gemm = gemm
        if self.dev.type == "cuda":
            torch.backends.cuda.matmul.allow_tf32 = False  # TF32 has a 10-bit mantissa: inexact
            torch.backends.cudnn.allow_tf32 = False
            torch.backends.cuda.matmul.allow_fp16_reduced_precision_reduction = False
            props = torch.cuda.get_device_properties(self.dev)
            arch = getattr(props, "gcnArchName", f"sm_{props.major}{props.minor}")
            vendor = "rocm" if torch.version.hip else "cuda"
            self.fnuz = "gfx94" in arch
            self.name = f"torch-{vendor}:{props.name}:{arch}:{gemm}"
        else:
            self.fnuz = False
            self.name = f"torch-{self.dev.type}:{gemm}"
        torch.set_float32_matmul_precision("highest")
        self.lut = torch.tensor(RQ_LUT, dtype=torch.int32, device=self.dev)

    def _op(self, x):
        t = self.t
        if self.gemm == "fp32":
            return x.to(t.float32)
        if self.gemm == "int8":
            return x.to(t.int8)
        dt = t.float8_e4m3fnuz if self.fnuz else t.float8_e4m3fn
        return x.to(t.float32).to(dt)

    def prepare(self, ws):
        t = self.t
        # Weights kept as [N][K]; GEMM is X @ W^T.
        self.W = [self._op(t.tensor(w, dtype=t.int32, device=self.dev)) for w in ws]

    def _mm(self, x, w):
        t = self.t
        if self.gemm == "fp32":
            return t.matmul(x, w.t()).round().to(t.int32)
        if self.gemm == "int8":
            m = x.shape[0]
            pad = (-m) % 32 if m <= 16 or m % 8 else 0  # _int_mm needs m > 16 and m % 8 == 0
            if pad:
                x = t.cat([x, t.ones((pad, x.shape[1]), dtype=x.dtype, device=x.device)])
            y = t._int_mm(x, w.t())  # w is [N][K]; w.t() is the [K][N] column-major operand
            return y[:m]
        one = t.ones((), device=self.dev, dtype=t.float32)
        m = x.shape[0]
        pad = (-m) % 16
        if pad:
            x = t.cat([x, t.zeros((pad, x.shape[1]), dtype=x.dtype, device=x.device)])
        y = t._scaled_mm(x, w.t(), scale_a=one, scale_b=one, out_dtype=t.float32)
        return y[:m].round().to(t.int32)

    def raw_mm(self, X, W):
        t = self.t
        x = self._op(t.tensor(X, dtype=t.int32, device=self.dev))
        w = self._op(t.tensor(W, dtype=t.int32, device=self.dev))
        return self._mm(x, w).cpu().numpy()

    def forward(self, X):
        t = self.t
        x = self._op(t.tensor(X, dtype=t.int32, device=self.dev))
        for (k, n, s), w in zip(layers(self.p), self.W):
            y = self._mm(x, w)
            if s is not None:
                x = self._op(self.lut[t.bitwise_and(t.bitwise_right_shift(y, s), 0xFF).long()])
        return y.cpu().numpy()


class MlxBackend(Backend):
    def __init__(self):
        super().__init__()
        import mlx.core as mx

        self.mx = mx
        self.name = f"mlx-metal:{mx.default_device()}:fp32"
        self.lut = mx.array(RQ_LUT.astype(np.int32))

    def prepare(self, ws):
        mx = self.mx
        self.W = [mx.array(w.astype(np.float32).T.copy()) for w in ws]

    def raw_mm(self, X, W):
        mx = self.mx
        y = mx.round(mx.matmul(mx.array(X.astype(np.float32)), mx.array(W.astype(np.float32).T.copy()))).astype(mx.int32)
        mx.eval(y)
        return np.array(y)

    def forward(self, X):
        mx = self.mx
        x = mx.array(X.astype(np.float32))
        for (k, n, s), w in zip(layers(self.p), self.W):
            y = mx.round(mx.matmul(x, w)).astype(mx.int32)
            if s is not None:
                idx = mx.bitwise_and(mx.right_shift(y, s), 0xFF)
                x = mx.take(self.lut, idx).astype(mx.float32)
        mx.eval(y)
        return np.array(y)


def make(args) -> Backend:
    if args.backend == "numpy":
        return NumpyBackend()
    if args.backend == "torch":
        return TorchBackend(args.device, args.gemm)
    if args.backend == "mlx":
        return MlxBackend()
    raise SystemExit(f"unknown backend {args.backend}")


def run_job(b: Backend, p, seed, challenge, target: int, start, count, batch, stop_on_found=False):
    b.load(p, seed)
    found, done = [], 0
    for off in range(0, count, batch):
        ns = list(range(start + off, start + min(count, off + batch)))
        for n, d in zip(ns, b.digests(challenge, ns)):
            if int.from_bytes(d, "big") < target:
                found.append((n, d))
        done += len(ns)
        if stop_on_found and found:
            break
    return found, done


def serve(b, batch):
    out = sys.stdout
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            v = json.loads(line)
            cmd = v.get("cmd")
            if cmd == "info":
                print(json.dumps({"event": "info", "backend": b.name, "impl": "zpu-miner-python", "protocol": 1}), file=out, flush=True)
            elif cmd in ("job", "digest"):
                p = dict(v["profile"])
                p.setdefault("id", 0)
                seed, ch = bytes.fromhex(v["epoch_seed"]), bytes.fromhex(v["challenge"])
                if cmd == "job":
                    t0 = time.time()
                    found, done = run_job(b, p, seed, ch, int(v["target"], 16), int(v["nonce_start"]), int(v["nonce_count"]), batch,
                                          bool(v.get("stop_on_found", False)))
                    for n, d in found:
                        print(json.dumps({"event": "found", "nonce": str(n), "digest": d.hex()}), file=out)
                    print(json.dumps({"event": "done", "attempts": str(done), "elapsed_s": time.time() - t0}), file=out, flush=True)
                else:
                    b.load(p, seed)
                    ds = b.digests(ch, [int(n) for n in v["nonces"]])
                    print(json.dumps({"event": "digests", "digests": [d.hex() for d in ds]}), file=out, flush=True)
            elif cmd == "quit":
                return
        except Exception as e:  # report, keep serving
            print(json.dumps({"event": "error", "message": repr(e)}), file=out, flush=True)


def selftest(b, path):
    v = json.load(open(path))
    passed = failed = 0
    e = v.get("edge", {"byte_to_val": [], "requant_cases": []})
    for i, want in enumerate(e["byte_to_val"]):
        ok = int(BYTE_TO_VAL[i]) == want
        passed, failed = passed + ok, failed + (not ok)
    for c in e["requant_cases"]:
        y = np.array([c["y"]], dtype=np.int32)
        ok = int(requant_np(y, 0)[0]) == c["s0"] and int(requant_np(y, 2)[0]) == c["s2"]
        passed, failed = passed + ok, failed + (not ok)
    for key in ("dev", "v01", "near_v1"):
        if key not in v:
            continue
        pv = v[key]
        b.load(pv["profile"], bytes.fromhex(pv["epoch_seed"]))
        ch = bytes.fromhex(pv["challenge"])
        ns = [int(a["nonce"]) for a in pv["attempts"]]
        ds = b.digests(ch, ns)
        for a, d in zip(pv["attempts"], ds):
            ok = d.hex() == a["digest"]
            passed, failed = passed + ok, failed + (not ok)
            print(("PASS" if ok else "FAIL"), key, "nonce=" + a["nonce"], b.name)
    print(f"{passed}/{passed + failed} checks bit-exact on {b.name}")
    return 1 if failed else 0


def stress(b, rows=512, k=4096, n=4096, seed=1):
    """Accumulator stress: extreme-magnitude rows (|sum| = 2^20) plus random
    domain GEMMs, compared bit-for-bit with an int64 reference."""
    rng = np.random.default_rng(seed)
    X = VALS[rng.integers(0, 32, size=(rows, k))].astype(np.int32)
    W = VALS[rng.integers(0, 32, size=(n, k))].astype(np.int32)
    X[0, :], W[0, :] = 16, 16          # +2^20
    X[1, :], W[1, :] = 16, -16         # -2^20 (row 1 x col 1 is +2^20, row 1 x col 0 is -2^20)
    X[2, ::2], X[2, 1::2] = 16, -16    # cancellation
    X[3, :k // 2], X[3, k // 2:] = 16, 1  # big then tiny terms (alignment/truncation probe)
    W[2, :k // 2], W[2, k // 2:] = 16, 1
    ref = X.astype(np.int64) @ W.astype(np.int64).T
    t0 = time.time()
    got = b.raw_mm(X, W).astype(np.int64)
    el = time.time() - t0
    bad = int((got != ref).sum())
    res = {"backend": b.name, "rows": rows, "k": k, "n": n, "max_abs_ref": int(np.abs(ref).max()),
           "mismatches": bad, "elapsed_s": el, "verdict": "PASS" if bad == 0 else "FAIL"}
    print(json.dumps(res))
    return 0 if bad == 0 else 1


def bench(b, profile, count, batch):
    p = PROFILES[{"v01": 1, "dev": 2, "near": 3}[profile]]
    seed, ch = b"\x5e" * 32, b"\xc4" * 32
    run_job(b, p, seed, ch, 0, 0, min(batch, 64), batch)  # warm (weights + kernels)
    t0 = time.time()
    _, done = run_job(b, p, seed, ch, 0, 0, count, batch)
    el = time.time() - t0
    macs = sum(k * n for k, n, _ in layers(p))
    print(json.dumps({"backend": b.name, "impl": "zpu-miner-python", "profile": p["name"], "attempts": done, "elapsed_s": el,
                      "attempts_per_s": done / el, "tera_macs_per_s": done / el * macs / 1e12}))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", nargs="?", default="serve")
    ap.add_argument("path", nargs="?")
    ap.add_argument("--backend", default="numpy")
    ap.add_argument("--device", default="cuda")
    ap.add_argument("--gemm", default="fp32", choices=["fp32", "int8", "fp8"])
    ap.add_argument("--batch", type=int, default=1024)
    ap.add_argument("--profile", default="v01")
    ap.add_argument("--count", type=int, default=2048)
    a = ap.parse_args()
    b = make(a)
    if a.cmd == "selftest":
        sys.exit(selftest(b, a.path or "tests/vectors/fp8c_v0.1.json"))
    if a.cmd == "stress":
        sys.exit(stress(b))
    if a.cmd == "bench":
        return bench(b, a.profile, a.count, a.batch)
    serve(b, a.batch)


if __name__ == "__main__":
    main()
