#!/usr/bin/env python3
"""Read BOTH of the guest's clocks at the same instant, across a pause.

The claim under test is that a guest can observe a large time step while no
watchdog fires, because the watchdogs and the guest's own clock are different
clocks. Testing that needs both read at the same moment, not in separate runs.

The trick is one line carrying both:

    echo "T $(cut -d' ' -f1 /proc/uptime)" > /dev/kmsg

comes back on the console as

    [   42.123456] T 42.12

The bracketed stamp is applied by printk inside the kernel; the value after
`T` was read by userspace from /proc/uptime just before the write. So each
line is a simultaneous sample of:

  printk stamp   <- local_clock(), i.e. sched_clock: the clock the soft-lockup
                    detector's "stuck for Ns" arithmetic is computed from
  /proc/uptime   <- CLOCK_MONOTONIC via timekeeping, i.e. the current
                    clocksource: what the guest's userspace sees

If the two diverge across a pause, the guest observed a step that the
watchdog's own clock did not -- which is the question.
"""

import argparse
import json
import os
import re
import signal
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import guest_smp
from guest_smp import Guest, complaints_in  # noqa: E402

WORKLOAD = (
    "n=$(nproc); i=0; while [ $i -lt $n ]; do (while :; do :; done)& i=$((i+1)); done; "
    "echo CS=$(cat /sys/devices/system/clocksource/clocksource0/current_clocksource); "
    "echo HB_START; "
    "while :; do echo \"T $(cut -d' ' -f1 /proc/uptime)\" > /dev/kmsg; usleep 20000; done"
)

# [   42.123456] T 42.12   ->  (printk stamp, /proc/uptime)
BOTH = re.compile(r"^\[\s*(\d+\.\d+)\]\s+T (\d+\.\d+)\s*$", re.M)

MODES = {
    "kvm": ["-accel", "kvm", "-cpu", "host"],
    "tcg": ["-accel", "tcg", "-cpu", "max"],
    "icount": ["-accel", "tcg", "-cpu", "max",
               "-icount", "shift=3,sleep=off",
               "-rtc", "base=2022-01-01T00:00:00,clock=vm"],
}


def samples(text):
    return [(float(a), float(b)) for a, b in BOTH.findall(text)]


def locate_pause(rows):
    """(sched_clock step, clocksource step, index, count of large steps).

    The pause is by far the largest discontinuity in the run, so find it
    rather than trusting the console to have been fully drained at the moment
    it began.
    """
    steps = [(b[0] - a[0], b[1] - a[1], i)
             for i, (a, b) in enumerate(zip(rows, rows[1:]))]
    by_sched = max(steps, key=lambda s: s[0])
    by_source = max(steps, key=lambda s: s[1])
    pick = by_sched if by_sched[0] >= by_source[1] else by_source
    large = sum(1 for s in steps if s[0] > 1.0 or s[1] > 1.0)
    return pick[0], pick[1], pick[2], large


def trial(mode, clocksource, hold, pause_mode, run_dir, cpus, numa):
    guest_smp.ARMS[mode] = MODES[mode]
    guest = Guest(mode, run_dir, WORKLOAD, cpus=cpus, numa_nodes=numa,
                  append_extra="clocksource=%s" % clocksource if clocksource else "")
    record = {"mode": mode, "requested_clocksource": clocksource or "(default)",
              "hold_seconds": hold, "pause_mode": pause_mode,
              "cpus": cpus, "numa_nodes": numa}
    try:
        guest.start(timeout=420)
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            guest.serial.pump(1.0)
            if len(samples(guest.serial.text())) >= 40:
                break
        else:
            raise TimeoutError("guest produced fewer than 40 paired samples")
        guest.serial.pump(4.0)
        chosen = re.search(r"CS=(\S+)", guest.serial.text())
        record["clocksource_in_use"] = chosen.group(1) if chosen else "?"

        if pause_mode == "qmp":
            guest.pause()
        else:
            os.killpg(os.getpgid(guest.process.pid), signal.SIGSTOP)
        # Drain first: with the guest stopped, everything still in the socket
        # was written BEFORE the pause. Without this the "first sample after
        # resume" is a backlog sample and the measurement is meaningless.
        guest.serial.drain(limit=90.0)
        before = samples(guest.serial.text())
        # Count console bytes DURING the pause only: a guest that can still
        # talk was not paused. Measured before the resume, not after.
        marker = len(guest.serial.buffer)
        time.sleep(hold)
        record["bytes_emitted_while_paused"] = len(guest.serial.buffer) - marker
        if pause_mode == "qmp":
            guest.resume()
        else:
            os.killpg(os.getpgid(guest.process.pid), signal.SIGCONT)
        guest.serial.pump(max(8.0, hold / 6))

        after = samples(guest.serial.text())
        if len(after) < 3:
            raise RuntimeError("too few samples to locate the pause")
        sched, source, index, large = locate_pause(after)
        record.update({
            "samples_total": len(after),
            "samples_before_drain": len(before),
            "pause_at_sample": index,
            "large_steps_in_run": large,
            "sched_clock_across_pause": round(sched, 6),
            "clocksource_across_pause": round(source, 6),
        })
        periods = sorted(b[0] - a[0] for a, b in zip(before, before[1:]))
        record["sampling_period_median_seconds"] = (
            round(periods[len(periods) // 2], 6) if periods else None)
        found = complaints_in(guest.serial.text())
        record["mechanisms"] = sorted({m for m, _ in found})
        record["complaints"] = [{"mechanism": m, "line": l} for m, l in found][:4]
        record["ok"] = True
    except Exception as error:
        record["ok"] = False
        record["error"] = "%s: %s" % (type(error).__name__, error)
    finally:
        guest.close()
    return record


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--modes", default="kvm,tcg,icount")
    parser.add_argument("--clocksources", default="",
                        help="comma-separated; empty entry means guest default")
    parser.add_argument("--holds", default="30")
    parser.add_argument("--pause-mode", choices=("qmp", "sigstop"), default="qmp")
    parser.add_argument("--cpus", type=int, default=4)
    parser.add_argument("--numa", type=int, default=2)
    parser.add_argument("--label", default="twoclocks")
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    here = Path(__file__).resolve().parent
    sources = args.clocksources.split(",") if args.clocksources else [""]
    out = []
    for mode in args.modes.split(","):
        for source in sources:
            for hold in [float(h) for h in args.holds.split(",")]:
                tag = "%s-%s-%s-%gs" % (mode, source or "default", args.pause_mode, hold)
                record = trial(mode, source, hold, args.pause_mode,
                               here / "runs" / args.label / tag, args.cpus, args.numa)
                out.append(record)
                print("%-7s cs=%-8s->%-9s %s %5.0fs   sched_clock=%-10s "
                      "clocksource=%-10s  %s" % (
                          mode, source or "default",
                          record.get("clocksource_in_use", "?"), args.pause_mode, hold,
                          record.get("sched_clock_across_pause",
                                     record.get("error", "?")),
                          record.get("clocksource_across_pause", "?"),
                          ",".join(record.get("mechanisms", [])) or "no complaint"),
                      flush=True)
                Path(args.out).parent.mkdir(parents=True, exist_ok=True)
                Path(args.out).write_text(json.dumps(out, indent=2))


main()
