#!/usr/bin/env python3
"""Build zeroflux.network into site/dist: the pages in site/pages rendered with the
partials in site/partials, static assets, and HTML (plus Markdown) renderings of
the public docs. Local paths are scrubbed.

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

DOCS = [  # (source, slug, title, one-line summary, group)
    ("docs/NEAR_SETTLEMENT.md", "near_settlement", "How ZeroFlux works on NEAR", "Mining, optimistic checking, fraud proofs, measured gas, security", "Earn: ZFX Mining"),
    ("docs/CONTRACT_API.md", "contract_api", "Contract API", "The ZFX contract's methods, byte layouts, events and errors", "Earn: ZFX Mining"),
    ("docs/INTENTS_INTEGRATION.md", "intents", "NEAR Intents", "ZFX on NEAR Intents, protocol mapping, test solver", "Earn: ZFX Mining"),
    ("docs/TOKEN_ACCOUNTING.md", "token_accounting", "Token accounting", "Supply, bonds and what mints when", "Earn: ZFX Mining"),
    ("docs/THREAT_MODEL.md", "threat_model", "Threat model", "What can go wrong, what stops it, open risks", "Earn: ZFX Mining"),
    ("docs/OPERATOR_RUNBOOK.md", "runbook", "Operator runbook", "Local stack, mining, watching, testnet deployment", "Earn: ZFX Mining"),
    ("vpn/README.md", "vpn", "ZFX VPN", "The privacy gateway: modes, tiers, install, hosted gateway, upgrading from Prompt VPN", "Build: ZFX VPN"),
    ("docs/VPN_THREAT_MODEL.md", "vpn_threat_model", "ZFX VPN threat model", "Who sees plaintext in each mode, what a TEE proves, streaming, key custody", "Build: ZFX VPN"),
    ("vpn/docs/evaluation.md", "vpn_evaluation", "ZFX VPN detection evaluation", "Measured detection results, including misses", "Build: ZFX VPN"),
    ("docs/DATA_METHOD.md", "data_method", "ZFX Markets index method", "Source, formula, UTC days, statuses, corrections, manipulation resistance", "Trade: ZFX Markets"),
    ("docs/CONSENSUS.md", "consensus", "Proof-of-work specification", "Profiles, the matrix computation, difficulty", "Proof of work"),
    ("docs/ALGORITHM_DECISION.md", "algorithm", "Algorithm decision", "Why this proof of work, the candidates that lost, the experiments", "Proof of work"),
    ("docs/HARDWARE.md", "hardware", "Hardware support", "Which GPU path to use, measured throughput, bring-up checklist", "Proof of work"),
    ("docs/REPO_AUDIT.md", "repo_audit", "Repository audit", "What existed, what was reused, what changed, and the bugs testnet found", "Evidence"),
    ("docs/TEST_REPORT.md", "test_report", "Test report", "What was tested and how", "Evidence"),
    ("ACCEPTANCE_REPORT.md", "acceptance", "Acceptance report", "Every check with its command and result", "Evidence"),
    ("docs/RELEASE_READINESS.md", "release_readiness", "Release readiness", "What blocks a mainnet, and who owns it", "Evidence"),
    ("docs/BRIDGE_TRUST_MODEL.md", "bridge_trust_model", "Committee bridge trust model", "The own-chain alternative's 2-of-3 bridge", "Alternative architecture"),
]
SCRUB = [
    (r"/Users/[A-Za-z0-9_.-]+/", "~/"),
]
# Mermaid diagrams, themed with the page (dark by default, light on request).
MERMAID = """<script type="module">
import mermaid from "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs";
const r = document.documentElement, t = r.getAttribute("data-theme");
const dark = t ? t === "dark" : !(window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches);
mermaid.initialize({ startOnLoad: false, theme: dark ? "dark" : "neutral", fontFamily: "Inter, system-ui, sans-serif", htmlLabels: false, flowchart: { htmlLabels: false } });  // SVG text: the page zoom distorts HTML label measurement
await document.fonts.ready;  // measure labels with the final font, or they get clipped
await mermaid.run({ querySelector: "pre.mermaid" });
</script>"""
LOGO = open(os.path.join(SITE, "static", "logo-inline.txt")).read()
LINKS = {os.path.basename(src): f"{slug}.html" for src, slug, _, _, _ in DOCS}


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
<link rel="icon" type="image/svg+xml" href="/static/favicon.svg"><link rel="icon" type="image/png" sizes="32x32" href="/static/favicon-32.png"><link rel="apple-touch-icon" href="/static/apple-touch-icon.png">
<link rel="preconnect" href="https://fonts.googleapis.com"><link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Inter+Tight:wght@200;300;400&family=Inter:wght@300;400;500&family=JetBrains+Mono:wght@300;400&display=swap" rel="stylesheet">
<script>(function () {{ try {{ var t = localStorage.getItem('zf-theme'); if (t === 'light' || t === 'dark') document.documentElement.setAttribute('data-theme', t); }} catch (e) {{}} }})();</script>
<link rel="stylesheet" href="/static/docs.css"></head><body>
<header class="top">
<div class="status"><div class="w"><span><span class="dot"></span>Testnet</span><span>Mainnet not launched</span><span>Testnet ZFX has no monetary value</span></div></div>
<div class="hbar"><div class="w">
<a class="brand" href="/" aria-label="ZeroFlux home"><span class="cell">{logo}</span><span class="word">ZeroFlux</span></a>
<nav><a href="/earn">Earn</a><a href="/build">Build</a><a href="/trade">Trade</a><a href="/docs/">Docs</a><a href="https://github.com/zerofluxprotocol/zeroflux" target="_blank" rel="noopener">GitHub</a>
<button class="theme-toggle" type="button" id="themeToggle" aria-label="Switch light or dark mode"><svg class="moon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/></svg><svg class="sun" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/></svg></button>
<a class="cta" href="/earn#run">Start Mining</a></nav>
</div></div>
</header>
<div class="w layout"><aside class="toc" aria-label="Documentation"><details id="tocd" open><summary>Documentation menu</summary>{toc}</details></aside><main class="doc">{body}</main></div>
<footer><div class="w">Apache-2.0 · <a href="/">zeroflux.network</a> · <a href="https://github.com/zerofluxprotocol/zeroflux" target="_blank" rel="noopener">GitHub</a></div></footer>
<script>(function () {{ var r = document.documentElement, b = document.getElementById('themeToggle');
function dark() {{ var t = r.getAttribute('data-theme'); if (t) return t === 'dark'; return !(window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches); }}
b.addEventListener('click', function () {{ var n = dark() ? 'light' : 'dark'; r.setAttribute('data-theme', n); try {{ localStorage.setItem('zf-theme', n); }} catch (e) {{}} }}); }})();</script>
<script>if (window.matchMedia('(max-width:900px)').matches) document.getElementById('tocd').removeAttribute('open');</script>
{copyjs}
</body></html>"""

CSS = """
:root{--bg:#000;--surface:#0B0B0B;--line:rgba(255,255,255,.13);--line-soft:rgba(255,255,255,.07);--ink:#fff;--ink-2:#D4D4D4;--muted:#A3A3A3;--faint:#8A8A8A;
--btn-bg:#fff;--btn-ink:#000;--btn-hover:#D9D9D9;--th-bg:#F2F2F2;--th-ink:#0A0A0A;--header-bg:rgba(0,0,0,.88);--status-ink:#CFCFCF;--pop:#5CF0AE;color-scheme:dark;
--display:'Inter Tight',system-ui,sans-serif;--body:'Inter',system-ui,sans-serif;--mono:'JetBrains Mono',ui-monospace,monospace}
@media (prefers-color-scheme:light){:root:not([data-theme="dark"]){--bg:#FAFAFA;--surface:#fff;--line:rgba(0,0,0,.13);--line-soft:rgba(0,0,0,.07);--ink:#0A0A0A;--ink-2:#2E2E2E;--muted:#555;--faint:#6B6B6B;
--btn-bg:#0A0A0A;--btn-ink:#fff;--btn-hover:#2A2A2A;--th-bg:#0A0A0A;--th-ink:#fff;--header-bg:rgba(250,250,250,.94);--status-ink:#3A3A3A;--pop:#0B7547;color-scheme:light}}
:root[data-theme="light"]{--bg:#FAFAFA;--surface:#fff;--line:rgba(0,0,0,.13);--line-soft:rgba(0,0,0,.07);--ink:#0A0A0A;--ink-2:#2E2E2E;--muted:#555;--faint:#6B6B6B;
--btn-bg:#0A0A0A;--btn-ink:#fff;--btn-hover:#2A2A2A;--th-bg:#0A0A0A;--th-ink:#fff;--header-bg:rgba(250,250,250,.94);--status-ink:#3A3A3A;--pop:#0B7547;color-scheme:light}
*{box-sizing:border-box}@media (min-width:761px){html{zoom:.9}}
body{margin:0;background:var(--bg);color:var(--ink);font:300 16px/1.7 var(--body);-webkit-font-smoothing:antialiased}
a{color:var(--ink);text-decoration:none;border-bottom:1px solid var(--line)}a:hover{border-color:var(--ink)}
.w{max-width:920px;margin:0 auto;padding:0 24px}
header.top{position:sticky;top:0;z-index:10;background:var(--header-bg);backdrop-filter:blur(10px);-webkit-backdrop-filter:blur(10px)}
header a,footer a{border-bottom:0}
.status{border-bottom:1px solid var(--line);font:300 11px var(--mono);letter-spacing:.06em;color:var(--status-ink)}
.status .w{min-height:34px;display:flex;flex-wrap:wrap;align-items:center;justify-content:center;gap:4px 22px;text-align:center;max-width:1280px}
.status .dot{width:7px;height:7px;background:var(--ink);display:inline-block;margin-right:8px;vertical-align:middle}
.hbar{border-bottom:1px solid var(--line)}.hbar .w{height:54px;display:flex;align-items:stretch;justify-content:space-between;max-width:1280px}
.brand{display:flex;align-items:stretch;color:var(--ink)}.brand .cell{display:flex;align-items:center;padding-right:20px;border-right:1px solid var(--line)}
.brand .cell svg{height:18px;width:auto;color:var(--ink);display:block}.brand .word{display:flex;align-items:center;padding-left:16px;font:300 19px var(--display);letter-spacing:-.03em}
nav{display:flex;align-items:stretch;gap:26px;font-size:14px}nav a{display:flex;align-items:center;color:var(--ink-2)}nav a:hover{color:var(--ink)}
.theme-toggle{width:54px;display:flex;align-items:center;justify-content:center;background:transparent;color:var(--ink);border:0;border-left:1px solid var(--line);cursor:pointer;margin:0 -26px 0 0;padding:0}
.theme-toggle:hover{background:var(--line-soft)}.theme-toggle .sun{display:none}
:root[data-theme="dark"] .theme-toggle .sun{display:block}:root[data-theme="dark"] .theme-toggle .moon{display:none}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]) .theme-toggle .sun{display:block}:root:not([data-theme="light"]) .theme-toggle .moon{display:none}}
nav a.cta{padding:0 24px;background:var(--btn-bg);color:var(--btn-ink);font-weight:400}nav a.cta:hover{background:var(--btn-hover)}
@media (max-width:720px){nav a:not(.cta){display:none}}
.layout{max-width:1280px;display:grid;grid-template-columns:232px minmax(0,1fr);gap:56px;align-items:start}
.doc{padding-top:56px;padding-bottom:96px;min-width:0;max-width:880px}
.toc{position:sticky;top:89px;max-height:calc(100vh - 89px);overflow-y:auto;padding:40px 4px 40px 0;font-size:13.5px;scrollbar-width:thin}
.toc a{border:0;color:var(--muted);display:block;padding:4px 0 4px 12px;border-left:1px solid var(--line-soft);line-height:1.4}
.toc a:hover{color:var(--ink)}.toc a.on{color:var(--ink);border-left-color:var(--ink)}
.toc a.home{padding-left:0;border:0;color:var(--ink-2);font-weight:400;margin-bottom:6px}.toc a.home.on{color:var(--ink)}
.toc .g{font:300 11px var(--mono);letter-spacing:.08em;text-transform:uppercase;color:var(--faint);margin:22px 0 8px}
.toc summary{display:none}.toc a{overflow-wrap:anywhere}
.toc ul{list-style:none;margin:0;padding:0}.toc ul.sec a{padding-left:24px;font-size:12.5px;color:var(--faint)}.toc ul.sec a:hover{color:var(--ink)}
.doc-actions{display:flex;justify-content:flex-end;gap:10px;margin:-28px 0 20px}
.doc-actions button,.doc-actions a{font:300 12px var(--mono);letter-spacing:.04em;color:var(--ink-2);background:transparent;border:1px solid var(--line);padding:6px 12px;cursor:pointer}
.doc-actions button:hover,.doc-actions a:hover{color:var(--ink);border-color:var(--ink)}
h2[id],h3[id]{scroll-margin-top:100px}
@media (max-width:900px){.layout{grid-template-columns:minmax(0,1fr);gap:0}.toc{position:static;max-height:none;padding:24px 0 8px;border-bottom:1px solid var(--line)}.doc-actions{margin-top:0}
.toc summary{display:block;cursor:pointer;font:300 12px var(--mono);letter-spacing:.06em;text-transform:uppercase;color:var(--ink-2);padding:4px 0 12px}.toc{padding-bottom:12px}}
h1{font:200 clamp(36px,5vw,56px)/1.04 var(--display);letter-spacing:-.045em;margin:0 0 28px}
h2{font:300 28px/1.2 var(--display);letter-spacing:-.03em;margin:56px 0 14px;padding-top:24px;border-top:1px solid var(--line)}
h3{font:400 19px/1.3 var(--display);margin:32px 0 8px}
p,li{color:var(--ink-2)}strong{color:var(--ink);font-weight:500}
code{font:300 .86em var(--mono);background:var(--surface);border:1px solid var(--line);padding:1px 5px;color:var(--ink)}
pre{background:var(--surface);border:1px solid var(--line);padding:18px;overflow-x:auto}pre code{border:0;padding:0;background:none;font-size:13px;line-height:1.6}
table{border-collapse:collapse;width:100%;font-size:14px;display:block;overflow-x:auto;margin:18px 0;border:1px solid var(--line)}
th,td{text-align:left;padding:11px 14px;border-bottom:1px solid var(--line);vertical-align:top}
th{background:var(--th-bg);color:var(--th-ink);font:300 11.5px var(--mono);letter-spacing:.06em;text-transform:uppercase}td{color:var(--ink-2)}
tr:last-child td{border-bottom:0}
pre.mermaid{background:transparent;border:1px solid var(--line);text-align:center}
blockquote{margin:18px 0;padding:14px 18px;border:1px solid var(--line);border-left:3px solid var(--ink)}blockquote p{margin:0}
.idx{border-top:1px solid var(--line)}.idx a{display:block;padding:20px 0;border-bottom:1px solid var(--line);color:var(--ink)}
.idx a:hover{padding-left:6px;transition:padding .15s ease}
.idx .t{font:300 24px var(--display);letter-spacing:-.02em}.idx .s{font-size:14px;color:var(--muted)}
.doc h2 + .idx{margin-top:4px;border-top:0}
footer{border-top:1px solid var(--line);padding:32px 0 48px;font-size:13px;color:var(--faint)}footer a{color:var(--muted)}
"""


COPYJS = """<script>(function () { var b = document.getElementById('copyMd'); if (!b) return;
b.addEventListener('click', function () { var u = b.getAttribute('data-md'), l = b.textContent;
fetch(u).then(function (r) { if (!r.ok) throw 0; return r.text(); })
.then(function (t) { return navigator.clipboard.writeText('<!-- source: https://zeroflux.network' + u + ' -->\\n' + t); })
.then(function () { b.textContent = 'Copied'; }, function () { window.location.href = u; })
.finally(function () { setTimeout(function () { b.textContent = l; }, 1600); }); }); })();</script>"""
MDLINKS = {os.path.basename(src): f"{slug}.md" for src, slug, _, _, _ in DOCS}


def relink_md(t):
    """Point links between docs at their published .md copies."""
    def fix(m):
        target, frag = m.group(1), m.group(2) or ""
        base = os.path.basename(target)
        return f"]({MDLINKS[base]}{frag})" if base in MDLINKS else m.group(0)
    return re.sub(r"\]\(([^)#\s]+\.md)(#[^)\s]*)?\)", fix, t)


def toc_html(current, headings):
    """Left sidebar: every doc grouped; the current page expands to its sections."""
    out, group = [], None
    for _, slug, title, _, g in DOCS:
        if g != group:
            if group is not None:
                out.append("</ul>")
            out.append(f'<div class="g">{html.escape(g)}</div><ul>')
            group = g
        on = slug == current
        out.append(f'<li><a href="{slug}.html"' + (' class="on" aria-current="page"' if on else "") + f">{html.escape(title)}</a>")
        if on and headings:
            out.append('<ul class="sec">' + "".join(f'<li><a href="#{i}">{html.escape(re.sub(r" \(.*\)$", "", n))}</a></li>' for i, n in headings) + "</ul>")
        out.append("</li>")
    out.append("</ul>")
    top = '<a class="home' + (' on' if current == "index" else "") + '" href="/docs/">All documentation</a>'
    return top + "".join(out)


PAGES = {  # page file in site/pages -> (output path, title, description, nav key)
    "index.html": ("index.html", "ZeroFlux · The economic layer for AI compute",
                   "Earn from idle hardware. Build privately with AI. Trade AI demand. ZFX mining, ZFX VPN and ZFX Markets on NEAR. Testnet.", ""),
    "build.html": ("build/index.html", "Build · ZFX VPN · ZeroFlux",
                   "A private AI endpoint for the models you already use. Protect your app's users and your own coding-agent sessions. Local, hosted, or hosted transport with local preprocessing.", "build"),
    "earn.html": ("earn/index.html", "Earn · ZFX Mining · ZeroFlux",
                  "Mine ZFX on NEAR with the GPUs and CPUs you already run. Proof of work made of the integer matrix math AI chips are built for. Live on NEAR testnet.", "earn"),
}


def fill(tpl, **kw):
    """Replace {key} placeholders without touching other braces (CSS and JS live in the templates)."""
    for k, v in kw.items():
        tpl = tpl.replace("{" + k + "}", v)
    return tpl


# Product status lines and the Trade spotlight. Kept here so every page states the same thing.
SITE_VARS = {
    "BUILD_STATUS": "Local app for macOS and Linux",
    "TRADE_STATUS": "Daily index · testnet paper trading",
    "TRADE_BLURB": "ZFX Markets publishes a daily index for each popular AI model: the tokens it served on OpenRouter for one UTC day, priced at what those tokens cost. Trade perpetuals on that index, cleared on NEAR. Testnet only: no real money.",
}


def trade_table():
    """Top models from the published index snapshot, if one is in the repo."""
    return ""


def render_pages():
    head, header, footer = (open(os.path.join(SITE, "partials", f)).read() for f in ("head.html", "header.html", "footer.html"))
    for src, (out, title, desc, nav) in PAGES.items():
        path = os.path.join(SITE, "pages", src)
        if not os.path.exists(path):
            continue
        body = fill(open(path).read(), TRADE_TABLE=trade_table(), **SITE_VARS)
        extra_scripts = ""
        if 'id="heroCanvas"' in body:
            extra_scripts += '<script src="/static/hero.js"></script>'
        page = (fill(head, title=html.escape(title), desc=html.escape(desc), page=nav or "home", extra_head="")
                + fill(header, **{f"a_{k}": (' aria-current="page"' if k == nav else "") for k in ("earn", "build", "trade")})
                + body
                + fill(footer, extra_scripts=extra_scripts))
        dest = os.path.join(DIST, out)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        open(dest, "w").write(scrub(page))



def main():
    shutil.rmtree(DIST, ignore_errors=True)
    os.makedirs(os.path.join(DIST, "docs"))
    shutil.copytree(os.path.join(SITE, "static"), os.path.join(DIST, "static"), ignore=shutil.ignore_patterns("*.txt"))
    open(os.path.join(DIST, "static", "docs.css"), "w").write(CSS)
    render_pages()
    md = markdown.Markdown(extensions=["tables", "fenced_code", "sane_lists", "toc"])
    llms = ["# ZeroFlux", "", "> ZeroFlux is a proof-of-work network on NEAR. ZFX is its token. Each page below is plain Markdown.", ""]
    cur_group = None
    groups = {}
    for src, slug, title, desc, group in DOCS:
        text = scrub(open(os.path.join(ROOT, src)).read())
        open(os.path.join(DIST, "docs", f"{slug}.md"), "w").write(relink_md(text))
        body = relink(md.reset().convert(text))
        heads = [(t["id"], html.unescape(t["name"])) for top in md.toc_tokens for t in ([top] if top["level"] == 2 else top["children"]) if t["level"] == 2]
        actions = f'<div class="doc-actions"><button type="button" id="copyMd" data-md="/docs/{slug}.md">Copy for agent</button><a href="{slug}.md">View .md</a></div>'
        if group != cur_group:
            llms += ([""] if cur_group else []) + [f"## {group}", ""]
            cur_group = group
        llms.append(f"- [{title}](https://zeroflux.network/docs/{slug}.md): {desc}")
        if 'class="language-mermaid"' in body:
            body = re.sub(r'<pre><code class="language-mermaid">(.*?)</code></pre>', lambda m: '<pre class="mermaid">' + m.group(1) + '</pre>', body, flags=re.S)
            body += MERMAID
        open(os.path.join(DIST, "docs", f"{slug}.html"), "w").write(PAGE.format(logo=LOGO, title=html.escape(title), desc=html.escape(desc), body=actions + body, toc=toc_html(slug, heads), copyjs=COPYJS))
        groups.setdefault(group, []).append(f'<a href="{slug}.html"><div class="t">{html.escape(title)}</div><div class="s">{html.escape(desc)}</div></a>')
    body = "<h1>Documentation</h1><p>ZeroFlux is a proof-of-work network on NEAR. Design, evidence and open risks, generated from the repository at build time.</p>"
    for g, items in groups.items():
        body += f"<h2>{html.escape(g)}</h2><div class=\"idx\">" + "".join(items) + "</div>"
    actions = '<div class="doc-actions"><button type="button" id="copyMd" data-md="/llms.txt">Copy for agent</button><a href="/llms.txt">llms.txt</a></div>'
    open(os.path.join(DIST, "docs", "index.html"), "w").write(PAGE.format(logo=LOGO, title="Documentation", desc="ZeroFlux documentation", body=actions + body, toc=toc_html("index", []), copyjs=COPYJS))
    open(os.path.join(DIST, "llms.txt"), "w").write("\n".join(llms) + "\n")
    open(os.path.join(DIST, "docs", "index.md"), "w").write("\n".join(llms) + "\n")
    body404 = '<h1>Not found</h1><p>That page does not exist. Go to the <a href="/">home page</a> or the <a href="/docs/">documentation</a>.</p>'
    open(os.path.join(DIST, "404.html"), "w").write(PAGE.format(logo=LOGO, title="Not found", desc="Page not found", body=body404, toc=toc_html(None, []), copyjs=""))
    open(os.path.join(DIST, "_headers"), "w").write("/*\n  X-Content-Type-Options: nosniff\n  Referrer-Policy: strict-origin-when-cross-origin\n  X-Frame-Options: DENY\n/docs/*.md\n  Content-Type: text/plain; charset=utf-8\n  Access-Control-Allow-Origin: *\n/llms.txt\n  Content-Type: text/plain; charset=utf-8\n  Access-Control-Allow-Origin: *\n")
    leaks = []
    for dp, _, fs in os.walk(DIST):
        for f in fs:
            if f.endswith((".html", ".md", ".txt")):
                t = open(os.path.join(dp, f)).read()
                for bad in ("/Users/",):
                    if bad in t:
                        leaks.append((f, bad))
    if leaks:
        raise SystemExit(f"refusing to build: private names in output: {leaks}")
    print(f"built {DIST}: index + {len(DOCS)} docs")


if __name__ == "__main__":
    main()
