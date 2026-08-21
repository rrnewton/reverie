#!/usr/bin/env python3
"""Pause-duration sweep: how long can a Linux guest be paused at the QEMU
level before something in the kernel complains, and WHICH mechanism complains.

One trial = boot, let a heartbeat workload settle, `stop` at a RANDOM instant,
hold for D wall seconds, `cont`, then watch the console.

The pause length is the only variable inside an arm; the time source is the
only variable between arms.  No drgn in this file on purpose -- a drgn read
would add its own wall time to the pause and blur the axis being measured.
Reading state while paused is demonstrated separately, in pause-and-read.py.
"""

import argparse
import json
import os
import random
import signal
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from guest_smp import Guest, complaints_in  # noqa: E402

# A spinning task keeps the tick and the watchdog hrtimer armed -- on a fully
# idle NO_HZ CPU the soft-lockup detector is touched on idle exit and would
# have nothing to say.  The heartbeat line is the guest's OWN view of time:
# uptime (CLOCK_MONOTONIC) and wall seconds, once a guest-second.
HEARTBEAT_TAIL = (
    "echo HB_START; "
    "while :; do echo \"HB $(cut -d' ' -f1 /proc/uptime) $(date -u +%s)"
    " $(cut -d' ' -f1,4 /proc/loadavg)\"; sleep 1; done"
)

WORKLOADS = {
    # A spinning task keeps the tick and the watchdog hrtimer armed -- on a
    # fully idle NO_HZ CPU the soft-lockup detector is touched on idle exit
    # and would have nothing to say.
    "spin": ("n=$(nproc); i=0; while [ $i -lt $n ]; do (while :; do :; done)& i=$((i+1)); done; "
             "sleep 2; echo CPUS=$n NODES=$(ls -d /sys/devices/system/node/node*|wc -l)"
             " LOAD=$(cut -d' ' -f1-3 /proc/loadavg) RUNNABLE=$(cut -d/ -f1 /proc/loadavg|cut -d' ' -f4); "
             + HEARTBEAT_TAIL),
    # Same, plus two syscall-heavy loops.  An RCU stall can only be reported
    # if a grace period is actually in flight across the pause, so a guest
    # that never enters the kernel cannot exercise that detector -- and
    # "no RCU stall" would then say more about the workload than about Linux.
    "rcu": ("n=$(nproc); i=0; while [ $i -lt $n ]; do (while :; do :; done)& i=$((i+1)); done; "
            "(while :; do cat /proc/self/status>/dev/null; done)& "
            "(while :; do ls /proc>/dev/null; done)& "
            "sleep 2; echo CPUS=$n NODES=$(ls -d /sys/devices/system/node/node*|wc -l)"
            " LOAD=$(cut -d' ' -f1-3 /proc/loadavg); "
            + HEARTBEAT_TAIL),
}

HEARTBEAT = re.compile(r"^HB (\d+\.\d+) (\d+)(?: (\S+) (\S+))?\s*$", re.M)


def _proc_state(pid):
    try:
        with open("/proc/%d/stat" % pid) as handle:
            return handle.read().rsplit(")", 1)[1].split()[0]
    except OSError:
        return "?"


def heartbeats(text):
    """(guest uptime, guest wall, 1-min loadavg, runnable/total) per beat."""
    return [(float(m[0]), int(m[1]), m[2], m[3]) for m in HEARTBEAT.findall(text)]


def trial(arm, duration, run_dir, settle_low, settle_high, observe, rng, mode,
          workload, append, cpus, numa):
    guest = Guest(arm, run_dir, WORKLOADS[workload], append_extra=append,
                  cpus=cpus, numa_nodes=numa)
    record = {"arm": arm, "pause_seconds": duration, "mode": mode,
              "workload": workload, "append": append,
              "cpus": cpus, "numa_nodes": numa, "run_dir": str(run_dir)}
    try:
        boot_started = time.monotonic()
        guest.start(timeout=240)
        if not guest.serial.wait_for(b"HB_START", 120):
            raise TimeoutError("heartbeat workload never started")
        record["boot_seconds"] = round(time.monotonic() - boot_started, 2)

        # Pause at a random instant, not at a marker: the owner asked for a
        # random point, and a marker-aligned stop would also hide any
        # dependence on WHERE in the guest's execution the pause lands.
        settle = rng.uniform(settle_low, settle_high)
        record["settle_seconds"] = round(settle, 2)
        guest.serial.pump(settle)

        before = heartbeats(guest.serial.text())
        if len(before) < 2:
            raise RuntimeError("guest produced no heartbeat to pause between")
        record["heartbeats_before"] = len(before)

        pause_started = time.monotonic()
        if mode == "qmp":
            record["status_at_pause"] = guest.pause()
        elif mode == "sigstop":
            # The negative control, and a genuinely different mechanism.  On a
            # QMP `stop` QEMU is still running and freezes the guest's clocks
            # on the way down.  Under SIGSTOP it cannot: it is not executing,
            # so nothing freezes anything, and the guest meets the whole host
            # pause as elapsed time when it comes back.
            os.killpg(os.getpgid(guest.process.pid), signal.SIGSTOP)
            record["status_at_pause"] = "sigstopped"
        else:
            raise ValueError("unknown pause mode %s" % mode)
        # Prove the guest really is stopped: no console byte may appear while
        # it is paused.  Without this the sweep cannot tell a pause from a
        # slow guest.
        quiet_probe = min(2.0, max(0.5, duration / 4.0))
        bytes_at_pause = len(guest.serial.buffer)
        guest.serial.pump(quiet_probe)
        record["bytes_emitted_while_paused"] = len(guest.serial.buffer) - bytes_at_pause

        remaining = duration - (time.monotonic() - pause_started)
        if remaining > 0:
            time.sleep(remaining)
        record["held_wall_seconds"] = round(time.monotonic() - pause_started, 2)
        if mode == "qmp":
            record["status_before_resume"] = guest.qmp.status()
            record["status_at_resume"] = guest.resume()
        else:
            record["status_before_resume"] = _proc_state(guest.process.pid)
            os.killpg(os.getpgid(guest.process.pid), signal.SIGCONT)
            record["status_at_resume"] = "sigcont"

        guest.serial.pump(observe)
        after = heartbeats(guest.serial.text())
        record["heartbeats_after"] = len(after)
        # The guest's own run-queue at the last beat before the pause: proof
        # the vCPUs were loaded rather than idle when it was stopped.
        if before:
            record["loadavg_at_pause"] = before[-1][2]
            record["runqueue_at_pause"] = before[-1][3]

        # What the guest itself thinks happened across the pause: the largest
        # step in its own uptime and wall clock between consecutive beats.
        gaps_uptime = [b[0] - a[0] for a, b in zip(after, after[1:])]
        gaps_wall = [b[1] - a[1] for a, b in zip(after, after[1:])]
        record["max_guest_uptime_step_seconds"] = round(max(gaps_uptime), 3) if gaps_uptime else None
        record["max_guest_wall_step_seconds"] = max(gaps_wall) if gaps_wall else None
        record["median_guest_uptime_step_seconds"] = (
            round(sorted(gaps_uptime)[len(gaps_uptime) // 2], 3) if gaps_uptime else None
        )

        text = guest.serial.text()
        found = complaints_in(text)
        record["complaints"] = [{"mechanism": m, "line": l} for m, l in found]
        record["mechanisms"] = sorted({m for m, _ in found})
        record["ok"] = True
    except Exception as error:
        record["ok"] = False
        record["error"] = "%s: %s" % (type(error).__name__, error)
        if guest.serial is not None:
            record["tail"] = guest.serial.text()[-1500:]
    finally:
        guest.close()
    return record


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--arm", required=True)
    parser.add_argument("--durations", required=True,
                        help="comma-separated pause lengths in wall seconds")
    parser.add_argument("--observe", type=float, default=30.0,
                        help="wall seconds to watch the console after resume")
    parser.add_argument("--settle-low", type=float, default=4.0)
    parser.add_argument("--settle-high", type=float, default=12.0)
    parser.add_argument("--seed", type=int, default=20260821)
    parser.add_argument("--out", required=True)
    parser.add_argument("--label", default="main")
    parser.add_argument("--mode", choices=("qmp", "sigstop"), default="qmp")
    parser.add_argument("--workload", choices=tuple(WORKLOADS), default="spin")
    parser.add_argument("--cpus", type=int, default=4)
    parser.add_argument("--numa", type=int, default=2)
    parser.add_argument("--append", default="",
                        help="extra kernel command line, e.g. clocksource=tsc")
    args = parser.parse_args()

    rng = random.Random(args.seed)
    root = Path(__file__).resolve().parent / "runs" / args.label / args.arm
    results = []
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    for index, duration in enumerate([float(d) for d in args.durations.split(",")]):
        started = time.monotonic()
        record = trial(args.arm, duration, root / ("t%02d-d%g" % (index, duration)),
                       args.settle_low, args.settle_high, args.observe, rng,
                       args.mode, args.workload, args.append,
                       args.cpus, args.numa)
        record["trial_wall_seconds"] = round(time.monotonic() - started, 1)
        record["trial_index"] = index
        results.append(record)
        out.write_text(json.dumps(results, indent=2))
        print("%-7s %6.1fs  ok=%-5s guest_step=%-8s %s" % (
            args.arm, duration, record.get("ok"),
            record.get("max_guest_uptime_step_seconds"),
            ",".join(record.get("mechanisms", [])) or
            record.get("error", "(no complaint)")), flush=True)


main()
