#!/usr/bin/env python3
"""Release hygiene checks (spec A5). Writes bench/results/release_check.json.

  - SBOM: CycloneDX JSON for every crate in both Cargo workspaces (dist/sbom/)
  - license inventory: every resolved dependency's SPDX expression, checked
    against an allowlist of permissive licenses; anything else fails
  - reproducible build: zfx-node and zfx-wallet built twice from scratch in
    two different target dirs with path remapping; binaries must be identical
  - release signing: reported BLOCKED unless ZFX_RELEASE_SIGNING_KEY is set
    (no release key exists; none is generated here)

Exit status is non-zero if any required check fails.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ENV = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ.get("PATH", ""))
WORKSPACES = [ROOT, os.path.join(ROOT, "bridge", "near")]
# SPDX identifiers accepted for dependencies of an Apache-2.0 project.
ALLOWED = {"MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unicode-3.0",
           "Unicode-DFS-2016", "CC0-1.0", "0BSD", "Unlicense", "BSL-1.0", "MPL-2.0", "CDLA-Permissive-2.0"}
# Crates whose metadata lacks a license field, reviewed by hand.
REVIEWED = {"ring": "ISC AND MIT AND OpenSSL (BoringSSL-derived, permissive)"}


def run(cmd, cwd=ROOT, **kw):
    return subprocess.run(cmd, cwd=cwd, env=kw.pop("env", ENV), capture_output=True, text=True, **kw)


def spdx_ok(expr):
    """Evaluate an SPDX expression (OR < AND < WITH, parentheses) as
    'satisfiable using only allowlisted licenses'. Legacy '/' means OR."""
    toks = expr.replace("/", " OR ").replace("(", " ( ").replace(")", " ) ").split()
    pos = [0]

    def peek():
        return toks[pos[0]] if pos[0] < len(toks) else None

    def take():
        pos[0] += 1
        return toks[pos[0] - 1]

    def factor():
        if peek() == "(":
            take()
            v = disj()
            if take() != ")":
                raise ValueError(expr)
            return v
        lic = take()
        if peek() == "WITH":
            take()
            lic = f"{lic} WITH {take()}"
        return lic in ALLOWED

    def conj():
        v = factor()
        while peek() == "AND":
            take()
            v = factor() and v
        return v

    def disj():
        v = conj()
        while peek() == "OR":
            take()
            v = conj() or v
        return v

    try:
        v = disj()
        return v and pos[0] == len(toks)
    except (ValueError, IndexError):
        return False


def licenses():
    out, bad = {}, []
    for ws in WORKSPACES:
        md = json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=ws).stdout)
        local = {p["id"] for p in md["packages"] if p["source"] is None}
        for p in md["packages"]:
            if p["id"] in local:
                continue
            lic = p.get("license") or REVIEWED.get(p["name"])
            key = f"{p['name']} {p['version']}"
            out[key] = lic
            if not lic or not (spdx_ok(lic) or p["name"] in REVIEWED):
                bad.append({"crate": key, "license": lic})
    counts = {}
    for v in out.values():
        counts[v] = counts.get(v, 0) + 1
    return {"dependencies": len(out), "by_license": dict(sorted(counts.items(), key=lambda x: -x[1])), "not_allowed": bad,
            "allowlist": sorted(ALLOWED), "result": "PASS" if not bad else "FAIL"}


def sbom():
    dest = os.path.join(ROOT, "dist", "sbom")
    shutil.rmtree(dest, ignore_errors=True)
    os.makedirs(dest)
    files = []
    for ws in WORKSPACES:
        r = run(["cargo", "cyclonedx", "--format", "json", "--quiet"], cwd=ws)
        if r.returncode != 0:
            return {"result": "FAIL", "error": r.stderr[-800:]}
        for dp, _, fs in os.walk(ws):
            if "/target" in dp or "/dist" in dp:
                continue
            for f in fs:
                if f.endswith(".cdx.json"):
                    src = os.path.join(dp, f)
                    shutil.move(src, os.path.join(dest, f))
                    files.append(f)
    comps = 0
    for f in files:
        comps += len(json.load(open(os.path.join(dest, f))).get("components", []))
    return {"result": "PASS" if files else "FAIL", "dir": "dist/sbom", "files": sorted(files), "components_total": comps, "format": "CycloneDX JSON"}


def sha(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()


def repro():
    if os.environ.get("ZFX_SKIP_REPRO") == "1":
        return {"result": "SKIPPED", "reason": "ZFX_SKIP_REPRO=1"}
    hashes = []
    for i in range(2):
        td = tempfile.mkdtemp(prefix=f"zfx-repro-{i}-")
        env = dict(ENV, CARGO_TARGET_DIR=td, RUSTFLAGS=f"--remap-path-prefix={ROOT}=/zfx --remap-path-prefix={td}=/target "
                   f"--remap-path-prefix={os.path.expanduser('~/.cargo')}=/cargo", SOURCE_DATE_EPOCH="1700000000")
        t0 = time.time()
        r = run(["cargo", "build", "--release", "--locked", "-p", "zfx-node", "-p", "zfx-wallet"], env=env)
        if r.returncode != 0:
            return {"result": "FAIL", "error": r.stderr[-800:]}
        hashes.append({"build": i, "seconds": round(time.time() - t0, 1),
                       "zfx-node": sha(os.path.join(td, "release", "zfx-node")), "zfx-wallet": sha(os.path.join(td, "release", "zfx-wallet"))})
        shutil.rmtree(td, ignore_errors=True)
    same = hashes[0]["zfx-node"] == hashes[1]["zfx-node"] and hashes[0]["zfx-wallet"] == hashes[1]["zfx-wallet"]
    rustc = run(["rustc", "-V"]).stdout.strip()
    return {"result": "PASS" if same else "FAIL", "scope": "same host and toolchain, two clean target dirs (not cross-host)", "rustc": rustc,
            "builds": hashes}


def wasm_hashes():
    out = {}
    base = os.path.join(ROOT, "bridge", "near", "target", "near")
    for c in ("wzfx", "test_ft", "pow_verifier"):
        p = os.path.join(base, c, f"{c}.wasm")
        if os.path.exists(p):
            out[c] = {"sha256": sha(p), "bytes": os.path.getsize(p)}
    return {"result": "INFO", "note": "built with `cargo near build non-reproducible-wasm`; the reproducible-wasm mode needs Docker, which is not installed on this host",
            "artifacts": out}


def signing():
    if os.environ.get("ZFX_RELEASE_SIGNING_KEY"):
        return {"result": "BLOCKED", "reason": "signing key present but release signing is an owner action; not performed by automated checks"}
    return {"result": "BLOCKED", "reason": "no release signing key configured (ZFX_RELEASE_SIGNING_KEY unset); none was generated. Releases are unsigned.",
            "prerequisite": "owner-held SLH-DSA or minisign release key, kept outside the repository"}


def main():
    rep = {"started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
           "commit": run(["git", "rev-parse", "HEAD"]).stdout.strip(),
           "licenses": licenses(), "sbom": sbom(), "reproducible_build": repro(), "wasm": wasm_hashes(), "release_signing": signing(),
           "docker_images": {"result": "BLOCKED", "reason": "docker not installed on this host; Dockerfile, Dockerfile.cuda, Dockerfile.rocm are unbuilt here"}}
    required = ["licenses", "sbom", "reproducible_build"]
    rep["required_failed"] = [k for k in required if rep[k]["result"] == "FAIL"]
    json.dump(rep, open(os.path.join(ROOT, "bench", "results", "release_check.json"), "w"), indent=2)
    for k in ("licenses", "sbom", "reproducible_build", "wasm", "release_signing", "docker_images"):
        print(f"{rep[k]['result']:8s} {k}")
    if rep["licenses"]["not_allowed"]:
        print(json.dumps(rep["licenses"]["not_allowed"], indent=1))
    sys.exit(1 if rep["required_failed"] else 0)


if __name__ == "__main__":
    main()
