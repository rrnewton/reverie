#!/usr/bin/env python3
"""Virtual time GRANTED per dettid, from the COMMIT records inside a window.

Each COMMIT carries hermit's own logical clock:

  2026-08-20T10:03:26.903575Z  INFO detcore::scheduler: [sched-step5] >>>>>>>
   COMMIT turn 5, dettid 3 ... on previously committed 1_767_225_600.003_412_930s

The virtual time a thread was granted is the advance of that clock across the
commits where it held the turn: for consecutive commits at virtual times v[i-1]
and v[i], the interval (v[i-1], v[i]) was run by the thread named at commit i.
Summing those intervals per dettid gives virtual seconds granted.

This is continuous where a turn count is an event count, which is why it is worth
trying after turn counts ran out of resolution.

The host wall-clock timestamp on the [sched-step5] line selects the window; the
harness stamps the workload window in UTC off the same host clock.

Usage: vtime.py <log> <start-iso> <end-iso>
"""
import re, sys
from datetime import datetime

TS = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+Z)\s")
CM = re.compile(r"^ COMMIT turn [\d_]+, dettid (\d+).*?committed ([\d_]+\.[\d_]+)s")

log, start, end = sys.argv[1], sys.argv[2], sys.argv[3]
t0 = datetime.fromisoformat(start)
t1 = datetime.fromisoformat(end)

cur_wall = None
prev_v = None
granted, turns = {}, {}
for line in open(log, errors="replace"):
    m = TS.match(line)
    if m:
        cur_wall = datetime.fromisoformat(m.group(1).replace("Z", "+00:00"))
        continue
    c = CM.match(line)
    if not c:
        continue
    d = int(c.group(1))
    v = float(c.group(2).replace("_", ""))
    if prev_v is not None and cur_wall is not None and t0 <= cur_wall <= t1:
        dv = v - prev_v
        # A backwards or absurd jump means the clock was reset rather than advanced
        # by this thread; count the turn but not the interval, instead of letting
        # one bad delta dominate the sum.
        if 0 <= dv < 60:
            granted[d] = granted.get(d, 0.0) + dv
        turns[d] = turns.get(d, 0) + 1
    prev_v = v

keys = sorted(set(granted) | set(turns))
print("granted_s=%s turns=%s" % (
    {k: round(granted.get(k, 0.0), 6) for k in keys},
    {k: turns.get(k, 0) for k in keys}))
