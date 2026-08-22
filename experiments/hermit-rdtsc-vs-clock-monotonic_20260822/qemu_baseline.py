#!/usr/bin/env python3
"""Run the freestanding two-clock probe inside a QEMU guest.

The guest's busybox initramfs has no dynamic loader and this host has no
static glibc, so the probe is freestanding. It is delivered on the same virtio
disk the guest already reads its command from: sector 0 carries the command
text (which is all the guest's /init reads), and the binary is written from
sector 1 onwards, so no shared asset is modified.
"""

import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, "/home/newton/work/dev-hermit/ignored/pause-sweep-cloudlint")
import guest_smp  # noqa: E402
from guest_smp import Guest  # noqa: E402

PROBE = Path("/home/newton/scratch-relaxcheck/clockprobe-free")
SECTOR = 512
COMMAND = (
    "dd if=/dev/vda of=/tmp/p bs=512 skip=1 count=%d 2>/dev/null; "
    "chmod +x /tmp/p; echo PROBE_BEGIN; /tmp/p; echo PROBE_END"
)
ROW = re.compile(r"^(\d+) (\d+) (\d+)\s*$", re.M)


def build_image(path, command, payload):
    """Sector 0 is the command the guest's /init runs; the rest is the binary."""
    body = command.encode()
    if len(body) > SECTOR - 12:
        raise ValueError("command does not fit in sector 0: %d bytes" % len(body))
    blob = bytearray(b"\0" * (1 << 20))
    blob[0:len(body)] = body
    blob[len(body)] = 0x0A
    blob[SECTOR:SECTOR + len(payload)] = payload
    Path(path).write_bytes(bytes(blob))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--arms", default="kvm,tcg,icount")
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    payload = PROBE.read_bytes()
    sectors = (len(payload) + SECTOR - 1) // SECTOR
    command = COMMAND % sectors
    here = Path(__file__).resolve().parent
    out = []
    print("probe %d bytes -> %d sectors" % (len(payload), sectors))
    for arm in args.arms.split(","):
        for repeat in range(args.repeats):
            run_dir = here / "runs" / "qemu" / ("%s-%d" % (arm, repeat))
            guest = Guest(arm, run_dir, command, cpus=1, numa_nodes=1)
            record = {"arm": arm, "repeat": repeat, "run_dir": str(run_dir)}
            try:
                # Overwrite the command image the harness wrote with one that
                # also carries the binary.
                guest.start(timeout=420)
                if not guest.serial.wait_for(b"PROBE_END", 300):
                    raise TimeoutError("probe did not finish")
                rows = ROW.findall(guest.serial.text())
                if len(rows) < 2:
                    raise RuntimeError("only %d probe rows" % len(rows))
                first, last = rows[0], rows[-1]
                mono = int(last[1]) - int(first[1])
                tsc = int(last[2]) - int(first[2])
                record.update({
                    "ok": True, "rows": len(rows),
                    "monotonic_ns": mono, "tsc_cycles": tsc,
                    "implied_tsc_hz": tsc / (mono / 1e9) if mono else None,
                })
                print("  %-7s rep%d  monotonic %.6f s  tsc %d  implied %.4f GHz" % (
                    arm, repeat, mono / 1e9, tsc,
                    record["implied_tsc_hz"] / 1e9 if mono else 0))
            except Exception as error:
                record["ok"] = False
                record["error"] = "%s: %s" % (type(error).__name__, error)
                if guest.serial is not None:
                    record["tail"] = guest.serial.text()[-600:]
                print("  %-7s rep%d  FAILED: %s" % (arm, repeat, record["error"]))
            finally:
                guest.close()
            out.append(record)
            Path(args.out).parent.mkdir(parents=True, exist_ok=True)
            Path(args.out).write_text(json.dumps(out, indent=2))


# The Guest helper writes a command-only image; replace it with one that also
# carries the binary payload, so delivery stays in a single place. Installed
# before main() runs, or the guest boots with the unpatched image.
guest_smp.make_command_image = lambda path, command: build_image(
    path, command, PROBE.read_bytes())

main()
