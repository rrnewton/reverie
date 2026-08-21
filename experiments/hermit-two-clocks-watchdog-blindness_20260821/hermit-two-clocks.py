#!/usr/bin/env python3
"""The same two-clock probe, with hermit wrapping QEMU.

The guest inside QEMU has the two clocks the task is about: `sched_clock`,
which the soft-lockup detector computes from, and the current clocksource,
which userspace reads. This asks what hermit's time virtualization does to
each of them, and whether hermit can move one without the other.

Hermit flags follow the demo path (demos/lib/drgn_hermit.py) so the
configuration is the project's own, not one invented here. The guest is
cold-booted rather than restored from the demo snapshot, so `-smp` is free.
"""

import argparse
import json
import os
import re
import select
import signal
import socket
import subprocess
import time
from pathlib import Path

ROOT = Path("/home/newton/work/dev-hermit")
HERMIT = ROOT / "hermit/target/release/hermit"
QEMU = "/usr/bin/qemu-system-x86_64"
ASSETS = ROOT / "ignored/qemu-linux"
KERNEL = ASSETS / "bzImage"
INITRD = ASSETS / "initramfs.cpio.gz"

COMMAND_BEGIN = b"__HERMIT_COMMAND_BEGIN__"
BOTH = re.compile(r"^\[\s*(\d+\.\d+)\]\s+T (\d+\.\d+)\s*$", re.M)

WORKLOAD = (
    "n=$(nproc); i=0; while [ $i -lt $n ]; do (while :; do :; done)& i=$((i+1)); done; "
    "echo CS=$(cat /sys/devices/system/clocksource/clocksource0/current_clocksource)"
    " AVAIL=$(cat /sys/devices/system/clocksource/clocksource0/available_clocksource); "
    "while :; do echo \"T $(cut -d' ' -f1 /proc/uptime)\" > /dev/kmsg; usleep 20000; done"
)


def make_command_image(path, command):
    payload = command.encode()
    if len(payload) > 500:
        raise ValueError("command does not fit in one sector: %d" % len(payload))
    with open(str(path), "wb") as image:
        image.write(payload + b"\n")
        image.write(b"\0" * ((1 << 20) - len(payload) - 1))


class Serial:
    """Read the guest console off a FIFO pair.

    A unix-socket chardev starves the -icount vCPU under hermit (the demo-5
    boot bug, recorded in demos/lib/drgn_hermit.py), so the demos use
    `-serial pipe:` and so does this.
    """

    def __init__(self, base, transcript):
        self.read_fd = os.open(str(base) + ".out", os.O_RDONLY | os.O_NONBLOCK)
        self.write_fd = os.open(str(base) + ".in", os.O_RDWR | os.O_NONBLOCK)
        self.buffer = b""
        self.transcript = open(str(transcript), "wb")

    def pump(self, seconds):
        end = time.monotonic() + seconds
        while True:
            remaining = end - time.monotonic()
            if remaining <= 0:
                return
            ready, _, _ = select.select([self.read_fd], [], [], min(0.2, remaining))
            if not ready:
                continue
            try:
                chunk = os.read(self.read_fd, 1 << 16)
            except BlockingIOError:
                continue
            if not chunk:
                time.sleep(0.05)
                continue
            self.buffer += chunk
            self.transcript.write(chunk)
            self.transcript.flush()

    def drain(self, quiet_for=0.5, limit=60.0):
        deadline = time.monotonic() + limit
        last_change = time.monotonic()
        size = len(self.buffer)
        while time.monotonic() < deadline:
            self.pump(0.1)
            if len(self.buffer) != size:
                size = len(self.buffer)
                last_change = time.monotonic()
            elif time.monotonic() - last_change >= quiet_for:
                return size
        return size

    def text(self):
        return self.buffer.decode("utf-8", "replace")

    def close(self):
        for fd in (self.read_fd, self.write_fd):
            try:
                os.close(fd)
            except OSError:
                pass
        self.transcript.close()


def samples(text):
    return [(float(a), float(b)) for a, b in BOTH.findall(text)]


def locate_pause(rows):
    """See two-clocks.py: find the pause by its size, not by the drain."""
    steps = [(b[0] - a[0], b[1] - a[1], i)
             for i, (a, b) in enumerate(zip(rows, rows[1:]))]
    by_sched = max(steps, key=lambda s: s[0])
    by_source = max(steps, key=lambda s: s[1])
    pick = by_sched if by_sched[0] >= by_source[1] else by_source
    large = sum(1 for s in steps if s[0] > 1.0 or s[1] > 1.0)
    return pick[0], pick[1], pick[2], large


def run_trial(run_dir, hold, cpus, hermit_flags, qemu_icount, pause_mode,
              timeout=900.0):
    run_dir = Path(run_dir).resolve()
    run_dir.mkdir(parents=True, exist_ok=True)
    serial_base = run_dir / "serial"
    for suffix in (".in", ".out"):
        path = str(serial_base) + suffix
        if not os.path.exists(path):
            os.mkfifo(path)
    command_image = run_dir / "command.img"
    make_command_image(command_image, WORKLOAD)

    qemu_command = [
        QEMU,
        "-machine", "q35",
        "-accel", "tcg",
        "-cpu", "max",
        "-smp", str(cpus),
        "-m", "512M",
        "-display", "none",
        "-monitor", "none",
        "-no-reboot",
        "-serial", "pipe:%s" % serial_base,
        "-drive", "if=none,id=cmd,file=%s,format=raw,readonly=on" % command_image,
        "-device", "virtio-blk-pci,drive=cmd",
        "-kernel", str(KERNEL),
        "-initrd", str(INITRD),
        "-append", "console=ttyS0 reboot=t",
    ]
    if qemu_icount:
        qemu_command += ["-icount", qemu_icount,
                         "-rtc", "base=2022-01-01T00:00:00,clock=vm"]

    command = [str(HERMIT), "run"] + hermit_flags + ["--"] + qemu_command
    (run_dir / "command.txt").write_text(" ".join(command) + "\n")

    record = {"run_dir": str(run_dir), "hold_seconds": hold, "cpus": cpus,
              "hermit_flags": " ".join(hermit_flags), "qemu_icount": qemu_icount,
              "pause_mode": pause_mode}
    environment = os.environ.copy()
    environment["LC_ALL"] = "C"
    environment["TZ"] = "UTC"
    serial = None
    with (run_dir / "hermit.log").open("wb") as log:
        process = subprocess.Popen(command, cwd=str(ROOT), env=environment,
                                   start_new_session=True, stdout=log,
                                   stderr=subprocess.STDOUT)
    try:
        serial = Serial(serial_base, run_dir / "serial.log")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            serial.pump(1.0)
            if len(samples(serial.text())) >= 40:
                break
            if process.poll() is not None:
                raise RuntimeError("hermit exited early (%d): %s" % (
                    process.returncode,
                    (run_dir / "hermit.log").read_text()[-400:]))
        else:
            raise TimeoutError("fewer than 40 paired samples in %.0fs" % timeout)
        serial.pump(4.0)

        chosen = re.search(r"CS=(\S+) AVAIL=([^\r\n]*)", serial.text())
        record["clocksource_in_use"] = chosen.group(1) if chosen else "?"
        record["clocksources_available"] = chosen.group(2) if chosen else "?"

        # Pause the WHOLE hermit process group: hermit is the tracer and QEMU
        # its tracee, so stopping QEMU alone would leave the tracer spinning.
        group = os.getpgid(process.pid)
        os.killpg(group, signal.SIGSTOP)
        serial.drain(limit=90.0)
        before = samples(serial.text())
        marker = len(serial.buffer)
        time.sleep(hold)
        quiet = len(serial.buffer) - marker
        os.killpg(group, signal.SIGCONT)
        serial.pump(max(10.0, hold / 4))

        after = samples(serial.text())
        if len(after) < 3:
            raise RuntimeError("too few paired samples to locate the pause")
        sched, source, index, large = locate_pause(after)
        periods = sorted(b[0] - a[0] for a, b in zip(before, before[1:]))
        record.update({
            "bytes_emitted_while_paused": quiet,
            "samples_total": len(after),
            "pause_at_sample": index,
            "large_steps_in_run": large,
            "sched_clock_across_pause": round(sched, 6),
            "clocksource_across_pause": round(source, 6),
            "sampling_period_median_seconds": round(periods[len(periods) // 2], 6)
            if periods else None,
        })
        text = serial.text()
        record["soft_lockup_lines"] = re.findall(
            r"watchdog: BUG: soft lockup[^\r\n]*", text)[:4]
        record["rcu_stall_lines"] = re.findall(
            r"rcu: INFO: rcu_\w+ [^\r\n]*stall[^\r\n]*", text)[:4]
        record["mechanisms"] = sorted(
            (["soft-lockup detector"] if record["soft_lockup_lines"] else [])
            + (["RCU stall detector"] if record["rcu_stall_lines"] else []))
        record["ok"] = True
    except Exception as error:
        record["ok"] = False
        record["error"] = "%s: %s" % (type(error).__name__, error)
        if serial is not None:
            record["tail"] = serial.text()[-800:]
    finally:
        if serial is not None:
            serial.close()
        if process.poll() is None:
            try:
                os.killpg(os.getpgid(process.pid), signal.SIGCONT)
            except OSError:
                pass
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=15)
    return record


# The demo path's hermit flags, plus variants that change only the time model.
PROFILES = {
    "demo": (["--strict", "--no-rcb-time", "--target-timeslice", "100000",
              "--max-timeslice", "disabled"], "shift=0,sleep=off"),
    "demo-multiplier-1000": (["--strict", "--no-rcb-time",
                              "--target-timeslice", "100000",
                              "--max-timeslice", "disabled",
                              "--clock-multiplier", "1000"], "shift=0,sleep=off"),
    "no-virtualize-time": (["--no-virtualize-time", "--no-virtualize-metadata",
                            "--no-rcb-time", "--target-timeslice", "100000",
                            "--max-timeslice", "disabled"], "shift=0,sleep=off"),
    "no-virtualize-time-no-icount": (["--no-virtualize-time",
                                      "--no-virtualize-metadata",
                                      "--no-rcb-time",
                                      "--target-timeslice", "100000",
                                      "--max-timeslice", "disabled"], ""),
    "demo-no-icount": (["--strict", "--no-rcb-time", "--target-timeslice", "100000",
                        "--max-timeslice", "disabled"], ""),
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--profiles", default="demo")
    parser.add_argument("--holds", default="30")
    parser.add_argument("--cpus", type=int, default=1)
    parser.add_argument("--out", required=True)
    parser.add_argument("--label", default="hermit-twoclocks")
    args = parser.parse_args()

    here = Path(__file__).resolve().parent
    out = []
    for name in args.profiles.split(","):
        flags, icount = PROFILES[name]
        for hold in [float(h) for h in args.holds.split(",")]:
            record = run_trial(
                here / "runs" / args.label / ("%s-%gs-smp%d" % (name, hold, args.cpus)),
                hold, args.cpus, flags, icount, "sigstop")
            record["profile"] = name
            out.append(record)
            print("%-22s smp=%d %5.0fs  cs=%-9s  sched_clock=%-11s "
                  "clocksource=%-11s  %s" % (
                      name, args.cpus, hold, record.get("clocksource_in_use", "?"),
                      record.get("sched_clock_across_pause",
                                 record.get("error", "?")),
                      record.get("clocksource_across_pause", "?"),
                      ",".join(record.get("mechanisms", [])) or "no complaint"),
                  flush=True)
            Path(args.out).parent.mkdir(parents=True, exist_ok=True)
            Path(args.out).write_text(json.dumps(out, indent=2))


main()
