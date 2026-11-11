"""Derive model-perp risk parameters from the published ZFX daily model index history.

Input: the index workstream's public series files (read-only), one per model:
  <index>/public/v1/indices/or-model-spend/<author>/<model>/series.json
Output: research/data/index_daily_moves.json with per-market and pooled statistics of the
day-over-day change of the index, which is the jump a position takes when the next print lands.

Usage: python3 -I research/risk/derive_params.py <path to markets/index> <out.json>
"""
import json, sys, glob, os
from fractions import Fraction

root, out = sys.argv[1], sys.argv[2]
files = sorted(glob.glob(os.path.join(root, "public/v1/indices/or-model-spend/*/*/series.json")))
pooled = []
markets = {}
for f in files:
    s = json.load(open(f))
    pts = [p for p in s["points"] if p["data_status"] == "OK"]
    pts.sort(key=lambda p: p["utc_date"])
    moves = []
    for a, b in zip(pts, pts[1:]):
        va, vb = Fraction(a["value_usd"]), Fraction(b["value_usd"])
        if va > 0:
            moves.append(float((vb - va) / va))
    if not moves:
        continue
    ab = sorted(abs(m) for m in moves)
    markets[s["index_id"]] = {
        "days": len(pts), "moves": len(moves),
        "max_up": max(moves), "max_down": min(moves),
        "p95_abs": ab[int(0.95 * (len(ab) - 1))], "max_abs": ab[-1],
        "last_value_usd": pts[-1]["value_usd"],
        "not_ok_days": sum(1 for p in s["points"] if p["data_status"] != "OK"),
    }
    pooled += moves

def q(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(p * (len(xs) - 1)))]

ab = [abs(m) for m in pooled]
down = [-m for m in pooled if m < 0]
up = [m for m in pooled if m > 0]
summary = {
    "markets": len(markets), "daily_moves": len(pooled),
    "abs_move": {k: q(ab, v) for k, v in [("p50", .5), ("p90", .9), ("p95", .95), ("p99", .99), ("p99_5", .995)]} | {"max": max(ab)},
    "down_move": {k: q(down, v) for k, v in [("p95", .95), ("p99", .99)]} | {"max": max(down)},
    "up_move": {k: q(up, v) for k, v in [("p95", .95), ("p99", .99)]} | {"max": max(up)},
    "share_moves_over": {f"{t}%": sum(1 for x in ab if x > t / 100) / len(ab) for t in [10, 20, 30, 40, 50, 75, 100]},
    "markets_with_any_move_over_40pct": sum(1 for m in markets.values() if m["max_abs"] > 0.4),
}
json.dump({"source": "markets/index public series (ZFX-OR-MODEL-SPEND-USD-v1, backfill 2026-10-11 to 2026-11-09)",
           "summary": summary, "per_market": markets}, open(out, "w"), indent=1, sort_keys=True)
print(json.dumps(summary, indent=1))
