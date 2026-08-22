#!/usr/bin/env python3
"""Sweep the clock multiplier against the maximum timeslice.

The question is why CLOCK_MONOTONIC and rdtsc disagree under `hermit run`, and
why the gap grows faster than the multiplier.

The candidate cause, read out of the source, predicts a specific shape:

  * `clock_gettime` returns GLOBAL virtual time
    (detcore/src/syscalls/time.rs:96 `guest_clock_time` -> `thread_observe_time`
    -> `global_time_lower_bound`), while `rdtsc` returns THREAD-LOCAL virtual
    time (detcore/src/lib.rs:1229, under a comment reading
    "TODO: use global time for rdtsc").
  * Only global time is charged for scheduler turns:
    `GlobalTime::add_scheduler_time` adds `NANOS_PER_SCHED * multiplier`
    = 500_000 ns x m per turn (detcore-model/src/time.rs:824).
  * The maximum timeslice is expressed in VIRTUAL nanoseconds and converted to
    an RCB budget by dividing by the multiplier
    (`into_rcbs_with_multiplier`, detcore-model/src/time.rs:233), so the work a
    thread may do per timeslice shrinks as 1/m and the number of preemptions
    for a fixed workload grows as m.

Together those give, for a workload retiring a fixed R conditional branches:

    excess(m) = R * NANOS_PER_RCB * NANOS_PER_SCHED * m^2 / max_timeslice

which is quadratic in m and INVERSELY PROPORTIONAL to the maximum timeslice.
That second half is the falsifiable part and is what this sweep varies: if the
excess does not fall as the timeslice grows, the explanation is wrong.
"""

import argparse
import json
import re
import subprocess
import time
from pathlib import Path

HERMIT = "/home/newton/work/dev-hermit/hermit/target/release/hermit"
PROBE = "/home/newton/scratch-relaxcheck/clockprobe"

ROW = re.compile(r"^\d+ ")


def parse(output):
    rows = [line.split() for line in output.splitlines() if ROW.match(line)]
    if len(rows) < 2:
        return None
    first, last = rows[0], rows[-1]
    return {
        "monotonic": float(last[1]) - float(first[1]),
        "realtime": float(last[3]) - float(first[3]),
        "process_cpu": float(last[4]) - float(first[4]),
        "tsc_raw": int(last[6]) - int(first[6]),
        "rusage": float(last[7]) - float(first[7]),
        "samples": len(rows),
    }


def run(command, timeout=1800):
    started = time.monotonic()
    done = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    return done.stdout, done.stderr, round(time.monotonic() - started, 2), done.returncode


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--multipliers", default="1,10,100,1000")
    parser.add_argument("--max-timeslices", default="",
                        help="comma-separated; empty means hermit's default")
    parser.add_argument("--samples", type=int, default=4)
    parser.add_argument("--work", type=int, default=5000000)
    parser.add_argument("--scale-timeslice-with-multiplier", action="store_true",
                        help="multiply the timeslice by m, which should hold the "
                             "preemption count constant and remove the quadratic term")
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    results = []
    header = ("%-6s %-14s %14s %14s %12s %10s %12s"
              % ("mult", "max_timeslice", "monotonic", "tsc(ns->s)", "excess",
                 "ratio", "wall"))
    print(header)
    print("-" * len(header))
    slices = args.max_timeslices.split(",") if args.max_timeslices else [""]
    for slice_value in slices:
        for m in [float(x) for x in args.multipliers.split(",")]:
            command = [HERMIT, "run"]
            if m != 1.0 or True:
                command += ["--clock-multiplier", repr(m)]
            effective = slice_value
            if slice_value:
                if args.scale_timeslice_with_multiplier:
                    effective = str(int(float(slice_value) * m))
                command += ["--max-timeslice", effective]
            command += ["--", PROBE, str(args.samples), str(args.work)]
            out, err, wall, code = run(command)
            parsed = parse(out)
            # A short read is the failure mode this harness must not paper over:
            # taking first-and-last of a truncated capture silently reports a
            # shorter interval as though it were the whole run.
            if parsed is not None and parsed["samples"] != args.samples:
                parsed = None
                err = ("captured %d of %d expected samples (rc=%d)"
                       % (len((out or "").splitlines()), args.samples, code))
            record = {"multiplier": m, "max_timeslice": effective or "(default)",
                      "wall_seconds": wall, "exit_code": code,
                      "command": " ".join(command)}
            if parsed is None:
                record["ok"] = False
                record["error"] = err.strip()[-300:]
                print("%-6g %-14s FAILED: %s" % (m, effective or "default",
                                                 record["error"][:60]))
            else:
                # Under hermit, rdtsc returns virtual NANOSECONDS: the handler
                # comments "we treat virtual cycles as equivalent to virtual
                # nanoseconds". So dividing by 1e9 gives seconds directly, and
                # a faithful pair of clocks would make this ratio exactly 1.
                tsc_seconds = parsed["tsc_raw"] / 1e9
                record.update(parsed)
                record["ok"] = True
                record["tsc_seconds"] = tsc_seconds
                record["excess_seconds"] = parsed["monotonic"] - tsc_seconds
                record["ratio"] = (parsed["monotonic"] / tsc_seconds
                                   if tsc_seconds else None)
                print("%-6g %-14s %14.6f %14.6f %12.6f %10.4f %12.1f" % (
                    m, effective or "default", parsed["monotonic"], tsc_seconds,
                    record["excess_seconds"], record["ratio"], wall))
            results.append(record)
            Path(args.out).parent.mkdir(parents=True, exist_ok=True)
            Path(args.out).write_text(json.dumps(results, indent=2))


main()
