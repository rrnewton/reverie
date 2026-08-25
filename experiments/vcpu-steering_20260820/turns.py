#!/usr/bin/env python3
"""Count scheduler turns per dettid INSIDE a wall-clock window.

Whole-run turn counts cannot answer this task: an idle resume with no workload at
all produced 36-175 turns on dettid 11, the same range as runs that ran a workload,
so the settle phase dominates and a whole-run ratio cannot see the workload.

A COMMIT line is a continuation of the `[sched-step5]` record immediately above it,
and that record carries a host wall-clock timestamp:

  2026-08-20T10:03:26.903575Z  INFO detcore::scheduler: [sched-step5] >>>>>>>
   COMMIT turn 5, dettid 3 ...

Hermit's log clock and the harness clock are the same host clock, so the harness's
UTC stamps for the workload window select the turns that belong to it.

Usage: turns.py <log> <start-iso> <end-iso>
"""
import re, sys
from datetime import datetime

TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+Z)\s")
CM = re.compile(r"^ COMMIT turn [\d_]+, dettid (\d+)")

log, start, end = sys.argv[1], sys.argv[2], sys.argv[3]
t0 = datetime.fromisoformat(start)
t1 = datetime.fromisoformat(end)
cur = None
inside, total = {}, {}
for line in open(log, errors="replace"):
    m = TS.match(line)
    if m:
        cur = datetime.fromisoformat(m.group(1).replace("Z", "+00:00"))
        continue
    c = CM.match(line)
    if c:
        d = int(c.group(1))
        total[d] = total.get(d, 0) + 1
        if cur is not None and t0 <= cur <= t1:
            inside[d] = inside.get(d, 0) + 1
print("window=%.2fs  in_window=%s  whole_run=%s"
      % ((t1 - t0).total_seconds(),
         {k: inside.get(k, 0) for k in sorted(set(total) | set(inside))},
         {k: total[k] for k in sorted(total)}))
