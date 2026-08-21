#!/usr/bin/env python3
"""Why 1.01 s?

A 30-second QMP `stop` moved kvm-clock guest uptime by 1.01 s in the first
sweep.  Neither 0 nor 30.  This measures what actually happens, at three
resolutions, instead of arguing about it:

  A. The original 1 Hz heartbeat, reproduced, so the number under test is the
     same number.
  B. A tight loop with NO sleep, printing /proc/uptime as fast as the guest
     can.  Sampling period drops from ~1 s to ~1 ms.
  C. The kernel's own printk clock at 1 us resolution, via two /dev/kmsg
     writes that bracket the pause -- independent of userspace scheduling
     and of the shell entirely.

If the 1.01 s is the SAMPLING INTERVAL, B and C collapse toward zero while A
stays at ~1.01.  If it is a real advance the guest suffers, all three agree.
"""

import argparse
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from guest_smp import Guest  # noqa: E402

# A: the original observable, 1 Hz.
WORKLOAD_1HZ = (
    "(while :; do :; done)& echo HB_START; "
    "while :; do echo \"HB $(cut -d' ' -f1 /proc/uptime)\"; sleep 1; done"
)

# B: same observable, no sleep.  busybox `cut` per iteration is a fork, so the
# period is milliseconds rather than microseconds -- still 1000x finer than A.
WORKLOAD_TIGHT = (
    "(while :; do :; done)& echo HB_START; "
    "while :; do echo \"HB $(cut -d' ' -f1 /proc/uptime)\"; done"
)

# C: the kernel's own clock.  Each line the guest writes to /dev/kmsg comes
# back on the console stamped by printk at microsecond resolution, so the
# stamp is applied inside the kernel, not by the shell.
WORKLOAD_KMSG = (
    "(while :; do :; done)& echo HB_START; "
    "while :; do echo KM > /dev/kmsg; done"
)

HB = re.compile(r"^HB (\d+\.\d+)\s*$", re.M)
KM = re.compile(r"^\[\s*(\d+\.\d+)\] KM\s*$", re.M)


def steps(values):
    return [b - a for a, b in zip(values, values[1:])]


def trial(arm, workload, pattern, run_dir, hold, settle=6.0):
    guest = Guest(arm, run_dir, workload, cpus=4, numa_nodes=2)
    try:
        guest.start(timeout=300)
        if not guest.serial.wait_for(b"HB_START", 120):
            raise TimeoutError("workload never started")
        guest.serial.pump(settle)

        guest.pause()
        # The guest is stopped, so anything still in the socket was written
        # BEFORE the pause. Drain it before taking the boundary: without this
        # the "first sample after resume" is really a backlog sample and the
        # result is an ordinary sampling interval, not the pause.
        guest.serial.drain()
        before = [float(v) for v in pattern.findall(guest.serial.text())]
        marker = len(guest.serial.buffer)
        time.sleep(hold)
        guest.resume()
        guest.serial.pump(max(4.0, hold / 8))

        after_all = [float(v) for v in pattern.findall(guest.serial.text())]
        if not before or len(after_all) <= len(before):
            raise RuntimeError("not enough samples around the pause")
        last_before = before[-1]
        first_after = after_all[len(before)]
        quiet = len(guest.serial.buffer) - marker

        # Free-running sampling period, away from the pause, from the same run.
        baseline = steps(before)
        baseline_median = sorted(baseline)[len(baseline) // 2] if baseline else None
        return {
            "arm": arm, "hold_seconds": hold,
            "samples_before": len(before),
            "sampling_period_median_seconds": round(baseline_median, 6)
            if baseline_median is not None else None,
            "guest_time_last_before_pause": last_before,
            "guest_time_first_after_resume": first_after,
            "guest_seconds_across_pause": round(first_after - last_before, 6),
            "bytes_emitted_while_paused": quiet,
            "max_step_over_whole_run": round(max(steps(after_all)), 6)
            if len(after_all) > 1 else None,
        }
    finally:
        guest.close()


def _unused_main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--arm", default="kvm")
    parser.add_argument("--holds", default="30")
    parser.add_argument("--out", default="results/why-1.01s.json")
    args = parser.parse_args()

    here = Path(__file__).resolve().parent
    observables = [
        ("A_heartbeat_1hz", WORKLOAD_1HZ, HB),
        ("B_tight_loop", WORKLOAD_TIGHT, HB),
        ("C_kernel_printk", WORKLOAD_KMSG, KM),
    ]
    out = []
    for hold in [float(h) for h in args.holds.split(",")]:
        for name, workload, pattern in observables:
            record = trial(args.arm, workload, pattern,
                           here / "runs" / "why" / args.arm / ("%s-%gs" % (name, hold)),
                           hold)
            record["observable"] = name
            out.append(record)
            print("%-16s hold=%5.1fs  sampling_period=%-10s  "
                  "guest_seconds_across_pause=%-10s  (whole-run max step %s)" % (
                      name, hold,
                      record["sampling_period_median_seconds"],
                      record["guest_seconds_across_pause"],
                      record["max_step_over_whole_run"]), flush=True)
            Path(args.out).parent.mkdir(parents=True, exist_ok=True)
            Path(args.out).write_text(json.dumps(out, indent=2))


#main()
