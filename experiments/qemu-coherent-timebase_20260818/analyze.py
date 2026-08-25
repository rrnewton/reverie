#!/usr/bin/env python3
"""Summarise the two guest-visible time domains from twodomain output."""
import re
import sys

rows = []
for line in open(sys.argv[1]):
    m = re.match(r"SAMPLE thread=(\d+) seq=(\d+) tsc=(\d+) mono=(\d+)", line)
    if m:
        rows.append(tuple(int(x) for x in m.groups()))

if not rows:
    print("no samples")
    sys.exit(1)

label = sys.argv[2] if len(sys.argv) > 2 else sys.argv[1]

# Per-thread spans.
print(f"== {label} ==  samples={len(rows)}")
for tid in sorted({r[0] for r in rows}):
    tr = [r for r in rows if r[0] == tid]
    tr.sort(key=lambda r: r[1])
    d_tsc = tr[-1][2] - tr[0][2]
    d_mono = tr[-1][3] - tr[0][3]
    ratio = (d_mono / d_tsc) if d_tsc else float("inf")
    print(f"  thread {tid}: tsc span {d_tsc:>15,}  mono span {d_mono:>15,}  "
          f"mono/tsc rate ratio {ratio:8.3f}")

# Global interleaved order: sort by mono (the coordinator clock is the shared
# order), then check whether tsc is monotone in that order.
order = sorted(rows, key=lambda r: (r[3], r[0], r[1]))
back = []
for a, b in zip(order, order[1:]):
    if b[2] < a[2]:
        back.append((a, b, a[2] - b[2]))
print(f"  in coordinator-clock order: tsc goes BACKWARDS {len(back)} times"
      f" of {len(order)-1} steps")
if back:
    mags = sorted(x[2] for x in back)
    print(f"    backward jump magnitudes (ns): min {mags[0]:,}  median "
          f"{mags[len(mags)//2]:,}  max {mags[-1]:,}")
    a, b, d = back[0]
    print(f"    first: thread {a[0]} seq {a[1]} tsc {a[2]:,} -> "
          f"thread {b[0]} seq {b[1]} tsc {b[2]:,}  (drop {d:,})")

# Coordinator clock monotone?
monoback = sum(1 for a, b in zip(order, order[1:]) if b[3] < a[3])
print(f"  in coordinator-clock order: mono goes backwards {monoback} times")

# Absolute divergence between the two domains at the last sample of each thread.
for tid in sorted({r[0] for r in rows}):
    tr = sorted([r for r in rows if r[0] == tid], key=lambda r: r[1])
    first, last = tr[0], tr[-1]
    print(f"  thread {tid}: (mono - tsc) grew from {first[3]-first[2]:,} to "
          f"{last[3]-last[2]:,} ns")
