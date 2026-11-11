"""Market parameters for a model perp on the LICENSED 7-day index (documented OpenRouter sources
only, `markets[].licensed.index_7d`), from the index publisher's models.json.

Listing rules: the licensed 7D index must be data_status OK, and the open-interest cap must be at
least $10,000; otherwise the script refuses (the market is not tradable).

The open-interest cap comes from the index side's manipulation-cost bound:
`max_open_interest_usd` = 25% of the 7-day spend window. It is written in collateral atoms
(6-decimal USD stable) as `max_open_interest_notional`; the contract checks it against open
interest valued at the mark. The owner re-runs this when the window moves and submits the result
with `update_market` (a lower cap applies at once, a higher one waits out the timelock).

Usage: python3 -I params_from_models.py <models.json> <permaslug> <MARKET-ID> > params.json
"""
import json, sys

models, slug, market_id = sys.argv[1], sys.argv[2], sys.argv[3]
m = json.load(open(models))
x = next(q for q in m["markets"] if q["permaslug"] == slug)
i7 = x["licensed"]["index_7d"]
latest = i7["latest"]
if latest["data_status"] != "OK":
    sys.exit(f"{slug}: licensed 7D index is {latest['data_status']} ({latest.get('reason')}); not tradable")
whole, _, frac = latest["max_open_interest_usd"].partition(".")
cap_atoms = int(whole) * 10**6 + int((frac + "000000")[:6])
if cap_atoms < 10_000 * 10**6:
    sys.exit(f"{slug}: open-interest cap ${latest['max_open_interest_usd']} is below $10,000; not tradable")
params = {
    "market_id": market_id, "market_type": "Perpetual",
    "index_id": i7["index_id"], "index_version": m["formula_licensed"]["version_7d"], "value_scale": 6,
    "index_units_per_tick": 1000000, "tick_value": 1,
    "min_qty": 1, "max_qty": 10000000, "max_price": 100000000,
    # Tier from docs/MARKET_RISK.md: 2x with a 20% mark limit (solvency rule 2000 + 300 + 75 <= 2500).
    # The index side's jump clamp for the licensed 7D index is 25% (p99 of 22,654 observed 7D
    # moves): a print moving it further keeps the market CLOSE_ONLY until the next clean print.
    "initial_margin_bps": 5000, "maintenance_margin_bps": 2500, "max_leverage": 2,
    "max_open_interest": 100000000000, "max_open_interest_notional": cap_atoms, "max_position": 1000000,
    "maker_fee_bps": 2, "taker_fee_bps": 5, "liq_fee_bps": 100, "liq_slippage_bps": 300, "price_band_bps": 5000,
    "oracle_max_age_sec": 108000, "funding_interval_sec": 28800, "funding_divisor": 24, "max_funding_ppm": 2500,
    "print_close_only_from_sec": 79200, "print_lag_days": 1,
    "mark_limit_bps": 2000, "halt_jump_bps": 100000, "print_move_close_only_bps": 2500,
}
json.dump(params, sys.stdout, indent=2)
print()
