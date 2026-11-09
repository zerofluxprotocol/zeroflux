#!/usr/bin/env python3
"""Steady-state average board power (W) from 200 ms samples.

Samples below 80% of the run's maximum (ramp-up, ramp-down) are excluded.
Prints `null` when no samples exist (power telemetry unavailable)."""
import re
import sys

vals = []
for line in open(sys.argv[1], errors="ignore"):
    nums = [float(n) for n in re.findall(r"\d+\.\d+|\d+", line)]
    cand = [n for n in nums if 20 <= n <= 2000]
    if cand:
        vals.append(cand[0])
if not vals:
    print("null")
else:
    m = max(vals)
    steady = [v for v in vals if v >= 0.8 * m]
    print(round(sum(steady) / len(steady), 1))
