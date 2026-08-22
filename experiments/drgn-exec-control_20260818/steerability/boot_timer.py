#!/usr/bin/env python3
"""Measure what a two-CPU guest boot costs in each execution mode.

Reports wall-clock seconds to reach a fixed guest kernel log line, the guest's
own timestamp on that line, and the resulting emulation slowdown.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time

ROOT = Path("/home/newton/work/dev-hermit")
ASSETS = ROOT / "ignored/qemu-linux"
QEMU = os.environ.get("QEMU_BIN", "/usr/bin/qemu-system-x86_64")
HERMIT = ROOT / "hermit/target/release/hermit"
MARKER = b"Disabling unused clocks"


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--smp", type=int, default=2)
    p.add_argument("--accel", default="tcg")
    p.add_argument("--icount", action="store_true")
    p.add_argument("--hermit", action="store_true")
    p.add_argument("--no-virt-time", action="store_true",
                   help="the remedy Hermit's own VMM warning suggests")
    p.add_argument("--chaos", action="store_true")
    p.add_argument("--no-strict", action="store_true")
    p.add_argument("--label", required=True)
    p.add_argument("--timeout", type=float, default=900.0)
    args = p.parse_args()

    work = Path(tempfile.mkdtemp(prefix="boot.", dir=str(ROOT / "ignored/exec-control")))
    disk = work / "disk.qcow2"
    subprocess.run(["cp", str(ASSETS / "hermit-boot.qcow2"), str(disk)], check=True)
    serial = work / "serial.txt"
    serial.touch()

    command = [QEMU, "-machine", "q35", "-accel", args.accel, "-cpu", "max",
               "-smp", str(args.smp), "-m", "512M", "-display", "none",
               "-monitor", "none",
               "-serial", "file:{}".format(serial),
               "-drive", "if=none,id=s,file={},format=qcow2".format(disk)]
    if args.icount:
        command += ["-icount", "shift=0,sleep=off"]
    command += ["-rtc", "base=2022-01-01T00:00:00,clock=vm",
                "-kernel", str(ASSETS / "bzImage"),
                "-initrd", str(ASSETS / "initramfs.cpio.gz"),
                "-append", "console=ttyS0 reboot=t"]
    if args.hermit:
        flags = ([] if args.no_strict else ["--strict"]) + ["--no-rcb-time", "--target-timeslice", "100000",
                 "--max-timeslice", "disabled"]
        if args.no_virt_time:
            flags += ["--no-virtualize-time", "--no-virtualize-metadata"]
        if args.chaos:
            flags += ["--chaos"]
        command = [str(HERMIT), "run"] + flags + ["--"] + command

    result = {"label": args.label, "smp": args.smp, "accel": args.accel,
              "icount": args.icount, "under_hermit": args.hermit,
              "no_virtualize_time": args.no_virt_time, "chaos": args.chaos,
              "strict": not args.no_strict}
    log = (work / "run.log").open("wb")
    started = time.monotonic()
    process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                               start_new_session=True)
    try:
        deadline = started + args.timeout
        hit = None
        while time.monotonic() < deadline:
            if process.poll() is not None:
                result["outcome"] = "EXITED early (status {})".format(
                    process.returncode)
                break
            data = serial.read_bytes()
            if MARKER in data:
                hit = time.monotonic() - started
                break
            time.sleep(0.2)
        data = serial.read_bytes()
        if hit is None and "outcome" not in result:
            result["outcome"] = "TIMEOUT after {}s".format(args.timeout)
        elif hit is not None:
            result["outcome"] = "ok"
            result["wall_seconds_to_marker"] = round(hit, 1)
            line = [l for l in data.split(b"\n") if MARKER in l]
            match = re.search(rb"\[\s*([0-9.]+)\]", line[0]) if line else None
            if match:
                guest = float(match.group(1))
                result["guest_seconds_at_marker"] = guest
                result["slowdown_vs_guest_time"] = round(hit / guest, 1)
        result["serial_bytes"] = len(data)
        result["serial_last_line"] = data.strip().split(b"\n")[-1][-90:].decode(
            "utf-8", "replace") if data.strip() else ""
    finally:
        try:
            os.killpg(os.getpgid(process.pid), 9)
        except ProcessLookupError:
            pass
        log.close()
    result["workdir"] = str(work)
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
