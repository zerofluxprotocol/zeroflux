"""Write one MarketParams file per tradable dataset-7D market (params_from_models.py rules:
dataset 7D index OK, open-interest cap >= $10,000) into v2-markets/, plus v2-markets.json
mapping market ids to permaslugs, sorted by OI cap.

Market ids are derived from the permaslug: the model part upper-cased with every character
outside [A-Z0-9] turned into '-', the date suffix shortened to MMDD, at most 32 characters.
Venues should match markets by `params.index_id` = `or-model-spend-lic-7d/<permaslug>`.

Usage: python3 -I gen_v2_markets.py <models.json> <out dir>
"""
import json, os, re, subprocess, sys

models, out = sys.argv[1], sys.argv[2]
here = os.path.dirname(os.path.abspath(__file__))
m = json.load(open(models))
os.makedirs(out, exist_ok=True)

def market_id(slug, used):
    model = slug.split("/", 1)[1]
    d = re.search(r"-(\d{8})$", model)
    base = model[: d.start()] if d else model
    mid = re.sub(r"[^A-Z0-9]+", "-", base.upper()).strip("-")
    suffix = "-" + d.group(1)[4:] if d else ""
    mid = (mid[: 32 - len(suffix)] + suffix).strip("-")
    if mid in used:
        mid = (re.sub(r"[^A-Z0-9]+", "-", slug.split("/")[0].upper())[:4] + "-" + mid)[:32]
    used.add(mid)
    return mid

rows, used, skipped = [], set(), []
for q in m["markets"]:
    mid = market_id(q["permaslug"], used)
    r = subprocess.run([sys.executable, "-I", os.path.join(here, "params_from_models.py"), models, q["permaslug"], mid], capture_output=True, text=True)
    if r.returncode != 0:
        used.discard(mid)
        skipped.append({"permaslug": q["permaslug"], "reason": r.stderr.strip()})
        continue
    p = json.loads(r.stdout)
    lic = q["licensed"]["index_7d"]["latest"]
    rows.append({"market_id": mid, "permaslug": q["permaslug"], "index_id": p["index_id"], "max_open_interest_usd": p["max_open_interest_notional"] / 1e6,
                 "index_lifecycle": lic.get("lifecycle"), "clamped": lic.get("clamped"), "change_vs_prev": lic.get("change_vs_prev")})
    open(os.path.join(out, mid + ".json"), "w").write(r.stdout)
rows.sort(key=lambda x: -x["max_open_interest_usd"])
json.dump({"source": os.path.abspath(models), "generated_at": m["generated_at"], "tradable": rows, "not_tradable": skipped}, open(out.rstrip("/") + ".json", "w"), indent=1)
print(len(rows), "tradable,", len(skipped), "not")
