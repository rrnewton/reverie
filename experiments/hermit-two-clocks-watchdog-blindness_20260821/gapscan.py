#!/usr/bin/env python3
"""Locate the pause in a paired-sample log without trusting the drain.

Each console line carries both clocks. The pause is by far the largest
discontinuity in the run, so find it directly rather than relying on the
console having been fully drained at the moment the pause began -- an
off-by-one there compares two pre-pause samples and reports a sampling
interval as though it were the pause.
"""
import re, sys
from pathlib import Path

BOTH = re.compile(r"^\[\s*(\d+\.\d+)\]\s+T (\d+\.\d+)\s*$", re.M)

for path in sys.argv[1:]:
    text = Path(path).read_text(errors="replace")
    s = [(float(a), float(b)) for a, b in BOTH.findall(text)]
    if len(s) < 3:
        print("%-58s only %d samples" % (path, len(s))); continue
    steps = [(b[0]-a[0], b[1]-a[1], i) for i, (a, b) in enumerate(zip(s, s[1:]))]
    # The pause is the largest step in EITHER clock.
    by_sched = max(steps, key=lambda t: t[0])
    by_cs = max(steps, key=lambda t: t[1])
    pick = by_sched if by_sched[0] >= by_cs[1] else by_cs
    big = [t for t in steps if t[0] > 1.0 or t[1] > 1.0]
    print("%-58s n=%4d  AT THE PAUSE: sched_clock %+.3f  clocksource %+.3f   "
          "(large steps in run: %d)" % (
              path.split("runs/")[-1], len(s), pick[0], pick[1], len(big)))
