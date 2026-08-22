#!/usr/bin/env python3
"""Sample per-vCPU-thread host CPU time at 20 ms while watching the serial log.

Replaces a grep-and-sleep-0.1 loop that reported impossible windows: 100,000,000
guest loop iterations inside a 0.33 s window, i.e. 300 M/s from an emulator that
does about 100 M/s. A 0.1 s poll cannot bracket a 1 s workload, and both the
window and the tick deltas were truncated by the same late first sample. This
records the whole series instead, so the bracket is chosen after the fact from
data rather than by a race with the log.

Usage: sampler.py <qemu-pid> <tid0> <tid1> <serial-path> <trace-out> <timeout-s>
"""
import os, sys, time

pid, t0, t1, serial, out, timeout = (
    sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], float(sys.argv[6])
)


def ticks(tid):
    try:
        with open(f"/proc/{pid}/task/{tid}/stat") as fh:
            f = fh.read().rsplit(") ", 1)[1].split()
        # After the "(comm) " split, utime/stime are fields 12 and 13 (0-based),
        # because splitting on the comm removes the first two fields.
        return int(f[11]) + int(f[12])
    except (OSError, IndexError):
        return None


rows = []
t_start = t_first = None
begin = time.monotonic()
while time.monotonic() - begin < timeout:
    now = time.monotonic() - begin
    a, b = ticks(t0), ticks(t1)
    if a is None or b is None:
        break
    try:
        text = open(serial, errors="replace").read()
    except OSError:
        text = ""
    if t_start is None and "ADVANCE-START" in text:
        t_start = len(rows)
    if "\nADVANCE R" in text or text.startswith("ADVANCE R"):
        rows.append((now, a, b))
        t_first = len(rows) - 1
        break
    rows.append((now, a, b))
    time.sleep(0.02)

with open(out, "w") as fh:
    fh.write("t\tv0_ticks\tv1_ticks\n")
    for r in rows:
        fh.write("%.3f\t%d\t%d\n" % r)

if t_start is None or t_first is None or t_first <= t_start:
    print("WINDOW none start=%s first=%s samples=%d" % (t_start, t_first, len(rows)))
    sys.exit(0)
s, e = rows[t_start], rows[t_first]
print("WINDOW wall=%.2fs v0_ticks=%d v1_ticks=%d samples=%d"
      % (e[0] - s[0], e[1] - s[1], e[2] - s[2], t_first - t_start))
