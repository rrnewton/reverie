#!/usr/bin/env python3
"""One binary, three environments: native, QEMU, and hermit.

Runs the freestanding probe and reports, for each run, the frequency the two
clocks imply for each other:

    implied = tsc_delta / (monotonic_delta in seconds)

On real hardware that is the TSC frequency and must be a constant of the
machine. Under hermit, rdtsc returns virtual NANOSECONDS (the handler comments
"we treat virtual cycles as equivalent to virtual nanoseconds"), so a faithful
pair of clocks implies exactly 1.000 GHz. Either way the test is the same:
the number must not depend on the run, the work size, or the clock multiplier.
"""

import argparse
import json
import re
import subprocess
import time
from pathlib import Path

PROBE = "/home/newton/scratch-relaxcheck/clockprobe-free"
HERMIT = "/home/newton/work/dev-hermit/hermit/target/release/hermit"
ROW = re.compile(r"^(\d+) (\d+) (\d+)\s*$", re.M)


def measure(command, timeout=2400):
    started = time.monotonic()
    done = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    rows = ROW.findall(done.stdout)
    wall = round(time.monotonic() - started, 2)
    if len(rows) < 4 or "END" not in done.stdout:
        return {"ok": False, "wall_seconds": wall, "exit_code": done.returncode,
                "rows": len(rows),
                "error": "short capture: %d rows, END=%s, rc=%d, stderr=%s"
                         % (len(rows), "END" in done.stdout, done.returncode,
                            done.stderr.strip()[-200:])}
    first, last = rows[0], rows[-1]
    mono = int(last[1]) - int(first[1])
    tsc = int(last[2]) - int(first[2])
    return {"ok": True, "wall_seconds": wall, "rows": len(rows),
            "monotonic_ns": mono, "tsc_delta": tsc,
            "monotonic_seconds": mono / 1e9,
            "implied_hz": tsc / (mono / 1e9) if mono else None}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--native-repeats", type=int, default=5)
    parser.add_argument("--multipliers", default="1,2,10,100,1000")
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    out = []
    print("%-28s %14s %16s %14s" % ("arm", "monotonic(s)", "tsc delta",
                                    "implied GHz"))
    print("-" * 76)
    for repeat in range(args.native_repeats):
        record = measure([PROBE])
        record.update({"environment": "native", "repeat": repeat})
        out.append(record)
        print("%-28s %14.6f %16d %14.6f" % (
            "native rep%d" % repeat, record.get("monotonic_seconds", 0),
            record.get("tsc_delta", 0), (record.get("implied_hz") or 0) / 1e9))

    for m in [float(x) for x in args.multipliers.split(",")]:
        record = measure([HERMIT, "run", "--clock-multiplier", repr(m), "--", PROBE])
        record.update({"environment": "hermit", "multiplier": m})
        out.append(record)
        if record["ok"]:
            print("%-28s %14.6f %16d %14.6f" % (
                "hermit m=%g" % m, record["monotonic_seconds"],
                record["tsc_delta"], record["implied_hz"] / 1e9))
        else:
            print("%-28s FAILED %s" % ("hermit m=%g" % m, record["error"][:60]))
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(json.dumps(out, indent=2))

    Path(args.out).write_text(json.dumps(out, indent=2))


main()
