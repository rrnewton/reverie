#!/usr/bin/env python3
"""Count backwards jumps in the guest kernel's own printk timestamps.

printk timestamps come from the guest's sched_clock, which rides the TSC once
tsc-early is selected. A backwards jump means the guest observed the TSC move
backwards -- the per-thread rdtsc offset showing through.
"""
import re
import sys

for path in sys.argv[1:]:
    ts = []
    for line in open(path, errors="replace"):
        m = re.match(r"\[\s*(\d+\.\d+)\]", line)
        if m:
            ts.append(float(m.group(1)))
    back = [(a, b) for a, b in zip(ts, ts[1:]) if b < a - 1e-9]
    worst = min((b - a for a, b in back), default=0.0)
    print(f"{path.split('/')[-1]:<42} lines={len(ts):<6} "
          f"backwards={len(back):<5} largest_rewind={worst*1000:9.3f} ms")
