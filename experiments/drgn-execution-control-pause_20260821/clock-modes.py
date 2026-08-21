#!/usr/bin/env python3
"""Clock-mode matrix: every QEMU clock mode, by backend, and what each does to
guest-visible time across a `stop`/`cont`.

Two axes QEMU controls -- the accelerator and the clock flags -- and one the
GUEST controls, its choice of clocksource.  The third axis matters: the first
sweep found that forcing a kvm guest onto tsc changes the answer completely,
so a table indexed only by QEMU flags would be wrong.

Time is measured with the kernel's own printk clock at 1 us resolution, which
is 2500x finer than the 1 Hz heartbeat the first sweep used.
"""

import argparse
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import guest_smp
from guest_smp import Guest, complaints_in  # noqa: E402

# Report the clocksource the GUEST actually selected, read from sysfs.  The
# previous attempt parsed it out of "Switched to clocksource X" in dmesg and
# got it wrong: a cmdline override can be applied and then superseded, so the
# last dmesg line is not necessarily what is in force.
WORKLOAD = (
    "n=$(nproc); i=0; while [ $i -lt $n ]; do (while :; do :; done)& i=$((i+1)); done; "
    "echo CS=$(cat /sys/devices/system/clocksource/clocksource0/current_clocksource)"
    " AVAIL=$(cat /sys/devices/system/clocksource/clocksource0/available_clocksource)"
    " CPUS=$n; "
    # 1 ms between kernel-log writes.  An unthrottled loop emits ~20k
    # lines/s, which under TCG produced 1.7 MB of backlog in the settle
    # window, overflowed the console (a CS= line came through truncated) and
    # outran the drain.  1 ms still resolves "0 vs 30 s" by four orders of
    # magnitude, which is the question this table asks.
    "echo HB_START; while :; do echo KM > /dev/kmsg; usleep 1000; done"
)
KM = re.compile(r"^\[\s*(\d+\.\d+)\] KM\s*$", re.M)

# accelerator + clock flags.  `-icount` is rejected outright under kvm
# ("-icount is not allowed with hardware virtualization"), so the icount rows
# are TCG-only by construction rather than by choice.
MODES = {
    "kvm-default":       ["-accel", "kvm", "-cpu", "host"],
    "kvm-rtc-clock-rt":  ["-accel", "kvm", "-cpu", "host", "-rtc", "clock=rt"],
    "kvm-rtc-clock-vm":  ["-accel", "kvm", "-cpu", "host", "-rtc", "clock=vm"],
    "kvm-rtc-driftfix-slew": ["-accel", "kvm", "-cpu", "host",
                              "-rtc", "clock=host,driftfix=slew"],
    "tcg-default":       ["-accel", "tcg", "-cpu", "max"],
    "tcg-mttcg":         ["-accel", "tcg,thread=multi", "-cpu", "max"],
    "tcg-rtc-clock-vm":  ["-accel", "tcg", "-cpu", "max", "-rtc", "clock=vm"],
    "tcg-icount-sleep-off": ["-accel", "tcg", "-cpu", "max",
                             "-icount", "shift=3,sleep=off",
                             "-rtc", "base=2022-01-01T00:00:00,clock=vm"],
    "tcg-icount-sleep-on": ["-accel", "tcg", "-cpu", "max",
                            "-icount", "shift=3,sleep=on",
                            "-rtc", "base=2022-01-01T00:00:00,clock=vm"],
    "tcg-icount-align":  ["-accel", "tcg", "-cpu", "max",
                          "-icount", "shift=3,sleep=off,align=on",
                          "-rtc", "base=2022-01-01T00:00:00,clock=vm"],
    "tcg-icount-auto":   ["-accel", "tcg", "-cpu", "max",
                          "-icount", "shift=auto,sleep=off",
                          "-rtc", "base=2022-01-01T00:00:00,clock=vm"],
    "tcg-icount-rtc-host": ["-accel", "tcg", "-cpu", "max",
                            "-icount", "shift=3,sleep=off",
                            "-rtc", "clock=host"],
}


def run(mode, clocksource, hold, run_dir, cpus, numa, pause_mode="qmp"):
    guest_smp.ARMS[mode] = MODES[mode]
    append = "clocksource=%s" % clocksource if clocksource else ""
    guest = Guest(mode, run_dir, WORKLOAD, cpus=cpus, numa_nodes=numa,
                  append_extra=append)
    record = {"mode": mode, "flags": " ".join(MODES[mode]),
              "requested_clocksource": clocksource or "(guest default)",
              "hold_seconds": hold, "pause_mode": pause_mode,
              "cpus": cpus, "numa_nodes": numa}
    try:
        guest.start(timeout=420)
        if not guest.serial.wait_for(b"HB_START", 180):
            raise TimeoutError("workload never started")
        guest.serial.pump(6.0)

        text = guest.serial.text()
        record["settle_bytes"] = len(guest.serial.buffer)
        chosen = re.search(r"CS=(\S+) AVAIL=([^\r\n]*?) CPUS=(\d+)", text)
        record["clocksource_in_use"] = chosen.group(1) if chosen else "?"
        record["clocksources_available"] = chosen.group(2) if chosen else "?"
        record["cpus_seen_by_guest"] = int(chosen.group(3)) if chosen else None

        if pause_mode == "qmp":
            guest.pause()
        else:
            import os, signal
            os.killpg(os.getpgid(guest.process.pid), signal.SIGSTOP)
        # The guest is stopped, so everything still in the socket was written
        # BEFORE the pause. Drain it, and only then take the boundary --
        # otherwise the "first sample after resume" is really a sample from
        # the pre-pause backlog and the measurement reports the ordinary
        # sampling period instead of the pause.
        guest.serial.drain(limit=90.0)
        before = [float(v) for v in KM.findall(guest.serial.text())]
        marker = len(guest.serial.buffer)
        time.sleep(hold)
        if pause_mode == "qmp":
            guest.resume()
        else:
            import os, signal
            os.killpg(os.getpgid(guest.process.pid), signal.SIGCONT)
        guest.serial.pump(max(6.0, hold / 8))

        after = [float(v) for v in KM.findall(guest.serial.text())]
        if not before or len(after) <= len(before):
            raise RuntimeError("no guest sample after resume")
        periods = sorted(b - a for a, b in zip(before, before[1:]))
        record.update({
            "bytes_emitted_while_paused": len(guest.serial.buffer) - marker,
            "sampling_period_median_seconds": round(periods[len(periods) // 2], 6)
            if periods else None,
            "guest_seconds_across_pause": round(after[len(before)] - before[-1], 6),
        })
        found = complaints_in(guest.serial.text())
        record["mechanisms"] = sorted({m for m, _ in found})
        record["complaints"] = [{"mechanism": m, "line": l} for m, l in found][:6]
        record["ok"] = True
    except Exception as error:
        record["ok"] = False
        record["error"] = "%s: %s" % (type(error).__name__, error)
    finally:
        guest.close()
    return record


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--modes", default=",".join(MODES))
    parser.add_argument("--clocksources", default="",
                        help="comma-separated; empty entry = guest default")
    parser.add_argument("--hold", type=float, default=30.0)
    parser.add_argument("--pause-mode", choices=("qmp", "sigstop"), default="qmp")
    parser.add_argument("--cpus", type=int, default=4)
    parser.add_argument("--numa", type=int, default=2)
    parser.add_argument("--out", default="results/clock-modes.json")
    args = parser.parse_args()

    here = Path(__file__).resolve().parent
    sources = args.clocksources.split(",") if args.clocksources else [""]
    out = []
    for mode in args.modes.split(","):
        for source in sources:
            tag = "%s-%s-%s" % (mode, source or "default", args.pause_mode)
            record = run(mode, source, args.hold,
                         here / "runs" / "clockmodes" / tag,
                         args.cpus, args.numa, args.pause_mode)
            out.append(record)
            print("%-24s cs=%-10s -> in_use=%-10s across_pause=%-12s %s" % (
                mode, source or "default", record.get("clocksource_in_use", "-"),
                record.get("guest_seconds_across_pause",
                           record.get("error", "?")),
                ",".join(record.get("mechanisms", [])) or ""), flush=True)
            Path(args.out).parent.mkdir(parents=True, exist_ok=True)
            Path(args.out).write_text(json.dumps(out, indent=2))


main()
