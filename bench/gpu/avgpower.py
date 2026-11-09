#!/usr/bin/env python3
"""Average board power (W) from nvidia-smi or amd-smi/rocm-smi CSV samples."""
import re
import sys

vals = []
for line in open(sys.argv[1], errors="ignore"):
    nums = re.findall(r"\d+\.\d+|\d+", line)
    if not nums:
        continue
    # nvidia-smi: one number per line; amd-smi/rocm-smi: take the largest plausible watt value on the line
    cand = [float(n) for n in nums if 20 <= float(n) <= 2000]
    if cand:
        vals.append(cand[0] if len(nums) == 1 else max(cand))
print(round(sum(vals) / len(vals), 1) if vals else "null")
