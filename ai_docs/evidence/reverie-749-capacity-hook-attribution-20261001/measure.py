#!/usr/bin/env python3
# Attribution driver for reverie PR 749 (https://github.com/rrnewton/reverie/pull/749).
# Adapted from the part A driver (measure.py in
# https://github.com/rrnewton/reverie/pull/749#issuecomment-5929836935).
# Runs three prebuilt reverie-ptrace library unit-test binaries; never builds,
# never edits source.
#   A = head 410312f4 unchanged
#   B = head 410312f4 minus CapacityTool::on_exit_thread (fct.rs 231-268)
#   C = main dbf2b5c8 unchanged
# Interleaving: for k in 1..40 the variants run in a rotating order
#   k=1 A,B,C; k=2 B,C,A; k=3 C,A,B; k=4 A,B,C; ...
# Each run is one alone invocation of test (2) with --exact --test-threads=1.
# /proc/loadavg is read immediately before and after every invocation.
# Timing sources: libtest per-test exec_time (-Z unstable-options
# --report-time, emitted in --format json events) and the driver's own
# monotonic wall clock around the invocation.
# Differences from the part A driver: one fixed cwd for all variants (the test
# does not read its source tree), stdout and stderr saved to separate files,
# stdin is /dev/null so fd 0 is identical in every run.
import csv
import datetime
import os
import re
import subprocess
import sys
import time

SCRATCH = "/tmp/cml-attr749-1227"
RUNS = os.path.join(SCRATCH, "runs")
CWD = os.path.join(SCRATCH, "cwd")
TEST = ("tracer::tests::fatal_capacity_tests::"
        "retired_threads_release_descriptors_under_reduced_nofile")
VARIANTS = ["A", "B", "C"]
BINS = {v: os.path.join(SCRATCH, "bin", v) for v in VARIANTS}
K_MAX = 40
COMMON = ["-Z", "unstable-options", "--report-time", "--format", "json", "--show-output"]
EVENT = re.compile(
    r'\{ "type": "test", "name": "([^"]+)", "event": "(ok|failed|ignored|timeout)"'
    r'(?:, "exec_time": ([0-9.eE+-]+))?'
)
TIMEOUT_S = 120


def loadavg():
    with open("/proc/loadavg") as f:
        return f.read().strip()


def order_for(k):
    r = (k - 1) % 3
    return VARIANTS[r:] + VARIANTS[:r]


def run(argv, out_path, err_path):
    full = ["nice", "-n", "19", "timeout", str(TIMEOUT_S)] + argv
    start_utc = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    before = loadavg()
    t0 = time.monotonic()
    with open(out_path, "wb") as out, open(err_path, "wb") as err:
        proc = subprocess.run(full, cwd=CWD, stdin=subprocess.DEVNULL,
                              stdout=out, stderr=err)
    t1 = time.monotonic()
    after = loadavg()
    with open(out_path, "rb") as f:
        text = f.read().decode("utf-8", "replace")
    events = [(m.group(1), m.group(2), m.group(3)) for m in EVENT.finditer(text)]
    return {
        "argv": full,
        "rc": proc.returncode,
        "wall": t1 - t0,
        "before": before,
        "after": after,
        "events": events,
        "start_utc": start_utc,
    }


def main():
    os.makedirs(RUNS, exist_ok=True)
    for v in VARIANTS:
        if not os.access(BINS[v], os.X_OK):
            print(f"missing binary {BINS[v]}", flush=True)
            return 2
    for name in list(os.environ):
        if name.startswith("REVERIE_FATAL_CAPACITY"):
            print(f"refusing to run with {name} set", flush=True)
            return 2
    inv_path = os.path.join(SCRATCH, "invocations.csv")
    icsv = open(inv_path, "w", newline="")
    iw = csv.writer(icsv)
    iw.writerow(["variant", "k", "position", "start_utc", "rc", "result", "exec_time_s",
                 "wall_s", "loadavg_before", "loadavg_after", "events", "stdout", "stderr",
                 "argv"])
    for k in range(1, K_MAX + 1):
        for pos, v in enumerate(order_for(k), start=1):
            out = os.path.join(RUNS, f"{v}-k{k:02d}.out")
            err = os.path.join(RUNS, f"{v}-k{k:02d}.err")
            argv = [BINS[v], "--exact", TEST, "--test-threads=1"] + COMMON
            res = run(argv, out, err)
            by_name = {}
            for name, ev, t in res["events"]:
                by_name[name] = (ev, t)
            if res["rc"] == 124:
                result, secs = "hang", ""
            elif TEST in by_name:
                ev, t = by_name[TEST]
                result = {"ok": "pass", "failed": "fail"}.get(ev, ev)
                secs = t or ""
            else:
                result, secs = "missing", ""
            iw.writerow([v, k, pos, res["start_utc"], res["rc"], result, secs,
                         f"{res['wall']:.6f}", res["before"], res["after"],
                         len(res["events"]), out, err, " ".join(res["argv"])])
            icsv.flush()
            lb = res["before"].split()[0]
            la = res["after"].split()[0]
            print(f"k={k} pos={pos} variant={v} rc={res['rc']} result={result} "
                  f"exec={secs} wall={res['wall']:.3f} load={lb}->{la}", flush=True)
            if res["rc"] == 124:
                print(f"HANG at k={k} variant={v}; stopping (stderr {err})", flush=True)
                return 3
    print("DONE", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
