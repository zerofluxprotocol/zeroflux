#!/usr/bin/env python3
"""Build zeroflux.network into site/dist: the landing page, static assets, and
HTML renderings of the public design docs. Private material (the original
specification files, internal status notes) is not published, and vendor or
personal names are replaced.

    .venv/bin/python site/build.py && cf pages deploy site/dist ...
"""
import html
import os
import re
import shutil

import markdown

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SITE = os.path.join(ROOT, "site")
DIST = os.path.join(SITE, "dist")

DOCS = [  # (source, slug, title, one-line summary)
    ("docs/CONSENSUS.md", "consensus", "Consensus specification", "Byte-level protocol: header, proof of work, transactions, state, bridge module"),
    ("docs/ALGORITHM_DECISION.md", "algorithm", "Algorithm decision", "Why this proof of work, the candidates that lost, and the experiments"),
    ("docs/HARDWARE.md", "hardware", "Hardware support", "Which GPU path to use, measured throughput, bring-up checklist"),
    ("docs/BRIDGE_TRUST_MODEL.md", "bridge_trust_model", "Bridge trust model", "Who can do what in the 2-of-3 NEAR bridge"),
    ("docs/TOKEN_ACCOUNTING.md", "token_accounting", "Token accounting", "Supply, bridge books and the identities checked continuously"),
    ("docs/CONTRACT_API.md", "contract_api", "Contract API", "Node RPC, wZFX contract and Intents adapter, with signing bytes"),
    ("docs/INTENTS_INTEGRATION.md", "intents", "NEAR Intents integration", "How wZFX trades through NEAR Intents"),
    ("docs/THREAT_MODEL.md", "threat_model", "Threat model", "Attacks tried, assumptions, open risks"),
    ("docs/OPERATOR_RUNBOOK.md", "runbook", "Operator runbook", "Pause, rotation, upgrade, incident and recovery procedures"),
    ("docs/TEST_REPORT.md", "test_report", "Test report", "What was tested and how"),
    ("ACCEPTANCE_REPORT.md", "acceptance", "Acceptance report", "Every check with its command and result"),
    ("docs/RELEASE_READINESS.md", "release_readiness", "Release readiness", "What blocks a mainnet, and who owns it"),
]
SCRUB = [
    (r"project owner \(owner decision\)", "project owner (decision)"), (r"project owner \(account\)", "project owner (account)"),
    (r"project owner \(key\)", "project owner (key)"), (r"project owner \(partner program\)", "project owner (partner program)"),
    (r"project owner and governance", "project owner and governance"), (r"\bOwner\b", "project owner"),
    (r"/Users/[A-Za-z0-9_.-]+/", "~/"),
]
LINKS = {os.path.basename(src): f"{slug}.html" for src, slug, _, _ in DOCS}


def scrub(t):
    for a, b in SCRUB:
        t = re.sub(a, b, t)
    return t


def relink(h):
    def fix(m):
        target = m.group(1)
        base = os.path.basename(target.split("#")[0])
        return f'href="{LINKS[base]}"' if base in LINKS else m.group(0)
    return re.sub(r'href="([^"]+\.md(?:#[^"]*)?)"', fix, h)


PAGE = """<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · ZeroFlux</title><meta name="description" content="{desc}">
<link rel="icon" type="image/png" sizes="32x32" href="/static/favicon-32.png"><link rel="apple-touch-icon" href="/static/apple-touch-icon.png">
<link rel="preconnect" href="https://fonts.googleapis.com"><link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Inter+Tight:wght@200;300;400&family=Inter:wght@300;400;500&family=JetBrains+Mono:wght@300;400&display=swap" rel="stylesheet">
<link rel="stylesheet" href="/static/docs.css"></head><body>
<header><div class="w"><a class="logo" href="/"><img src="/static/logo-64.png" alt="" width="24" height="24"><span>ZeroFlux</span></a>
<nav><a href="/docs/">Docs</a><a href="/#run">Run it</a><a href="https://testnet.nearblocks.io/address/wzfx.zeroflux.testnet" target="_blank" rel="noopener">wZFX on NEAR</a></nav></div></header>
<div class="status">Testnet · mainnet not launched · testnet ZFX has no monetary value</div>
<main class="w doc">{body}</main>
<footer class="w">Apache-2.0 · <a href="/">zeroflux.network</a></footer></body></html>"""

CSS = """
:root{--bg:#FAFAFA;--surface:#fff;--line:#ECECEF;--ink:#0B0B0F;--muted:#4A4A55;--faint:#6B6B76;--accent:#1F43D6;color-scheme:light}
@media (prefers-color-scheme:dark){:root{--bg:#09090B;--surface:#121215;--line:#232329;--ink:#F4F4F6;--muted:#A9A9B3;--faint:#8A8A93;--accent:#9DB0FF;color-scheme:dark}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:300 16px/1.7 Inter,system-ui,sans-serif;-webkit-font-smoothing:antialiased}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline;text-underline-offset:3px}
.w{max-width:860px;margin:0 auto;padding:0 24px}
header{border-bottom:1px solid var(--line);position:sticky;top:0;background:var(--bg);z-index:5}
header .w{display:flex;justify-content:space-between;align-items:center;min-height:60px;gap:16px}
.logo{display:flex;gap:10px;align-items:center;color:var(--ink);font:400 17px 'Inter Tight',sans-serif;letter-spacing:-.02em}.logo img{border-radius:6px}
nav{display:flex;gap:22px;font-size:14px}nav a{color:var(--muted)}
.status{text-align:center;font:300 12px 'JetBrains Mono',monospace;letter-spacing:.05em;color:var(--muted);border-bottom:1px solid var(--line);padding:9px 12px}
.doc{padding-top:48px;padding-bottom:80px}
h1{font:200 clamp(36px,5vw,52px)/1.05 'Inter Tight',sans-serif;letter-spacing:-.04em;margin:0 0 28px}
h2{font:300 28px/1.2 'Inter Tight',sans-serif;letter-spacing:-.03em;margin:52px 0 12px}
h3{font:400 19px/1.3 'Inter Tight',sans-serif;margin:32px 0 8px}
p,li{color:var(--muted)}strong{color:var(--ink);font-weight:500}
code{font:300 .86em 'JetBrains Mono',monospace;background:var(--surface);border:1px solid var(--line);border-radius:6px;padding:1px 5px;color:var(--ink)}
pre{background:var(--surface);border:1px solid var(--line);border-radius:14px;padding:18px;overflow-x:auto}pre code{border:0;padding:0;background:none;font-size:13px;line-height:1.6}
table{border-collapse:collapse;width:100%;font-size:14px;display:block;overflow-x:auto;margin:16px 0}
th,td{text-align:left;padding:10px 12px 10px 0;border-bottom:1px solid var(--line);vertical-align:top}
th{font:300 11.5px 'JetBrains Mono',monospace;letter-spacing:.05em;text-transform:uppercase;color:var(--faint)}td{color:var(--muted)}
blockquote{margin:16px 0;padding:14px 18px;border:1px solid var(--accent);border-radius:14px}blockquote p{margin:0}
.idx a{display:block;padding:20px 0;border-bottom:1px solid var(--line);color:var(--ink)}.idx a:hover{text-decoration:none}
.idx .t{font:300 24px 'Inter Tight',sans-serif;letter-spacing:-.02em}.idx .s{font-size:14px;color:var(--muted)}
footer{border-top:1px solid var(--line);padding-top:28px;padding-bottom:40px;font-size:13px;color:var(--faint)}
"""


def main():
    shutil.rmtree(DIST, ignore_errors=True)
    os.makedirs(os.path.join(DIST, "docs"))
    shutil.copytree(os.path.join(SITE, "static"), os.path.join(DIST, "static"))
    open(os.path.join(DIST, "static", "docs.css"), "w").write(CSS)
    shutil.copy(os.path.join(SITE, "index.html"), os.path.join(DIST, "index.html"))
    md = markdown.Markdown(extensions=["tables", "fenced_code", "sane_lists"])
    idx = []
    for src, slug, title, desc in DOCS:
        text = scrub(open(os.path.join(ROOT, src)).read())
        body = relink(md.reset().convert(text))
        open(os.path.join(DIST, "docs", f"{slug}.html"), "w").write(PAGE.format(title=html.escape(title), desc=html.escape(desc), body=body))
        idx.append(f'<a href="{slug}.html"><div class="t">{html.escape(title)}</div><div class="s">{html.escape(desc)}</div></a>')
    body = "<h1>Documentation</h1><p>Design, evidence and open risks for ZeroFlux. Generated from the repository at build time.</p><div class=\"idx\">" + "".join(idx) + "</div>"
    open(os.path.join(DIST, "docs", "index.html"), "w").write(PAGE.format(title="Documentation", desc="ZeroFlux documentation", body=body))
    body404 = '<h1>Not found</h1><p>That page does not exist. Go to the <a href="/">home page</a> or the <a href="/docs/">documentation</a>.</p>'
    open(os.path.join(DIST, "404.html"), "w").write(PAGE.format(title="Not found", desc="Page not found", body=body404))
    open(os.path.join(DIST, "_headers"), "w").write("/*\n  X-Content-Type-Options: nosniff\n  Referrer-Policy: strict-origin-when-cross-origin\n  X-Frame-Options: DENY\n")
    leaks = []
    for dp, _, fs in os.walk(DIST):
        for f in fs:
            if f.endswith(".html"):
                t = open(os.path.join(dp, f)).read()
                for bad in ("project owner", "/Users/", "other*", "other.testnet", "user"):
                    if bad in t:
                        leaks.append((f, bad))
    if leaks:
        raise SystemExit(f"refusing to build: private names in output: {leaks}")
    print(f"built {DIST}: index + {len(DOCS)} docs")


if __name__ == "__main__":
    main()
