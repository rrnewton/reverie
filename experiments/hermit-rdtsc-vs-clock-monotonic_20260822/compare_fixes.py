#!/usr/bin/env python3
"""Compare baseline against both candidate fixes on the same invariant.

Invariant: tsc_delta / monotonic_delta. Under hermit, rdtsc returns virtual
NANOSECONDS, so a faithful pair of clocks gives exactly 1.000000 GHz.

Read order is a variable, not a detail. One probe reads rdtsc BEFORE
clock_gettime and the other reads it after. Candidate B only pulls the
thread's local clock up during a scheduler round-trip, so it is expected to
be exact in one order and not the other; candidate A reads one clock and
should be exact in both. Measuring both orders is what distinguishes them.
"""

import argparse
import json
import re
import subprocess
import time
from pathlib import Path

BINARIES = {
    "baseline": "/home/newton/work/dev-hermit/hermit/target/release/hermit",
    "fix-a-rdtsc-reads-global": "/tmp/hermit-fix-a",
    "fix-b-thread-absorbs-sched": "/tmp/hermit-fix-b",
}
PROBES = {
    "rdtsc-first": "/home/newton/scratch-relaxcheck/clockprobe-free",
    "rdtsc-after-syscall": "/home/newton/scratch-relaxcheck/clockprobe-free-swapped",
}
ROW = re.compile(r"^(\d+) (\d+) (\d+)\s*$", re.M)


def measure(command, timeout=2400):
    started = time.monotonic()
    try:
        done = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "timeout after %ds" % timeout}
    rows = ROW.findall(done.stdout)
    wall = round(time.monotonic() - started, 2)
    # Guard against the short-capture failure that silently reports a fraction
    # of the interval as though it were the whole run.
    if len(rows) != 4 or "END" not in done.stdout:
        return {"ok": False, "wall_seconds": wall,
                "error": "short capture: %d rows, END=%s, rc=%d"
                         % (len(rows), "END" in done.stdout, done.returncode)}
    first, last = rows[0], rows[-1]
    mono = int(last[1]) - int(first[1])
    tsc = int(last[2]) - int(first[2])
    return {"ok": True, "wall_seconds": wall,
            "monotonic_seconds": mono / 1e9, "tsc_seconds": tsc / 1e9,
            "implied_ghz": (tsc / (mono / 1e9)) / 1e9 if mono else None,
            "error_parts_per_million": abs((tsc / mono) - 1.0) * 1e6 if mono else None}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--multipliers", default="1,10,100")
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    out = []
    print("%-28s %-20s %5s %12s %12s %12s" % (
        "build", "read order", "mult", "monotonic", "tsc", "implied GHz"))
    print("-" * 96)
    for name, binary in BINARIES.items():
        for order, probe in PROBES.items():
            for m in [float(x) for x in args.multipliers.split(",")]:
                record = measure([binary, "run", "--clock-multiplier", repr(m),
                                  "--", probe])
                record.update({"build": name, "read_order": order, "multiplier": m})
                out.append(record)
                if record["ok"]:
                    print("%-28s %-20s %5g %12.6f %12.6f %12.6f" % (
                        name, order, m, record["monotonic_seconds"],
                        record["tsc_seconds"], record["implied_ghz"]))
                else:
                    print("%-28s %-20s %5g  FAILED %s" % (
                        name, order, m, record["error"][:40]))
                Path(args.out).parent.mkdir(parents=True, exist_ok=True)
                Path(args.out).write_text(json.dumps(out, indent=2))


main()
