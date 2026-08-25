#!/usr/bin/env python3
"""Stop Linux under Hermit mid-execution, read it with drgn, resume.

The owner's four steps, on the Demo 7 QEMU/Linux guest:

  1. Linux running under Hermit          -- Demo 5's boot snapshot, restored.
  2. stop it mid-execution               -- SIGSTOP of Hermit's exact tracer
                                            thread group while the guest is
                                            partway through a command.
  3. read real kernel state at the stop  -- drgn walks task_struct.tasks.
  4. resume, and show the pause cost      -- `--dwell N` holds the stop for N
     the guest nothing                     wall-clock seconds; comparing a
                                            dwelling run against a
                                            non-dwelling one is the test.

Step 4 is the one that matters. Under QEMU `-icount` the guest's clock comes
from instructions retired, not host time, so a pause should be invisible to it.
If a dwelling run diverges from a non-dwelling one, the pause is NOT free and
that is the finding.

Usage: drgn -q -p $$ pause-midrun.py [--dwell S] [--pause-after-bytes N]
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE / "lib"))

import drgn_hermit as dh  # noqa: E402
from drgn.helpers.linux.list import list_for_each_entry  # noqa: E402

ASSETS = Path(os.environ.get("QEMU_ASSETS", ROOT / "ignored/qemu-linux"))
ARTIFACTS = Path(os.environ.get("EXEC_ARTIFACTS", HERE / "artifacts"))

MARKER = b"__HERMIT_EXEC_CONTROL_DONE__"
# Same shape as Demo 7's advance: create two persistent tasks, then wait on a
# fixed 1000us guest timer. The marker is split so the shell's echo of the
# command line cannot satisfy the wait.
#
# The two `/proc/uptime` reads bracket the mid-execution pause, so the guest
# itself reports how much time it believes passed across it. Serial output
# alone would only show that state matched; this shows the guest's clock did
# not move. It is the same measurement as the userspace precursor's
# "mark_delta", transposed onto the Linux guest.
# /proc/uptime only resolves 10ms. Writing to /dev/kmsg makes the kernel print
# the line to the console with its own printk timestamp, which resolves 1us --
# about four orders of magnitude tighter, and it is the kernel's clock rather
# than a userspace read of it.
COMMAND = (
    "echo HERMITMARKA > /dev/kmsg; for n in 1 2; do sleep 1000 & done; "
    "usleep 1000; echo HERMITMARKB > /dev/kmsg; cat /proc/uptime; "
    'echo __HERMIT_EXEC_CONTROL_"DONE__"'
)


def task_rows(program):
    init_task = program["init_task"]
    rows = [(init_task.pid.value_(),
             init_task.comm.string_().decode("utf-8", "replace"))]
    rows.extend(
        (t.pid.value_(), t.comm.string_().decode("utf-8", "replace"))
        for t in list_for_each_entry(
            "struct task_struct", init_task.tasks.address_of_(), "tasks"
        )
    )
    return rows


def canonical(rows):
    return "\n".join("{} {}".format(pid, comm) for pid, comm in rows)


def digest(data):
    if isinstance(data, str):
        data = data.encode("utf-8")
    return hashlib.sha256(data).hexdigest()[:16]


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--dwell", type=float, default=0.0,
                   help="wall-clock seconds to hold the mid-execution stop")
    p.add_argument("--pause-after-bytes", type=int, default=8,
                   help="pause once the guest has emitted this many bytes")
    p.add_argument("--no-pause", action="store_true",
                   help="baseline: run the advance with no mid-execution stop")
    p.add_argument("--label", default="run")
    p.add_argument("--timeout", type=float, default=300.0)
    args = p.parse_args()

    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    par = ARTIFACTS / "drgn-par"
    par.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("FB_PAR_UNPACK_BASEDIR", str(par))

    config = dh.GuestConfig(
        root=ROOT,
        hermit=Path(os.environ.get("HERMIT_RELEASE",
                                   ROOT / "hermit/target/release/hermit")),
        qemu=Path(os.environ.get("QEMU_BIN", "/usr/bin/qemu-system-x86_64")),
        kernel=ASSETS / "bzImage",
        initrd=ASSETS / "initramfs.cpio.gz",
        vmlinux=ASSETS / "vmlinux",
        snapshot_disk=ASSETS / "hermit-boot.qcow2",
        snapshot_name=os.environ.get("QEMU_SNAPSHOT_NAME", "hermit-boot"),
        artifact_dir=ARTIFACTS,
        timeout=args.timeout,
    )

    out = {"label": args.label, "dwell_seconds": args.dwell,
           "paused": not args.no_pause,
           "pause_after_bytes": None if args.no_pause else args.pause_after_bytes}
    guest = dh.HermitGuestProgram(config)
    started = time.monotonic()
    try:
        guest.start()
        out["startup_seconds"] = round(time.monotonic() - started, 1)

        # (1) The Demo 7 read, with the vCPUs parked after the restore.
        with guest.observation() as program:
            before = task_rows(program)
        out["before_tasks"] = len(before)
        out["before_digest"] = digest(canonical(before))

        # (2) Let the guest run its command.
        guest.begin_advance(COMMAND)

        if not args.no_pause:
            # Wait until the guest is demonstrably executing, then stop it
            # wherever it happens to be.
            seen = guest.wait_for_serial_bytes(args.pause_after_bytes)
            out["serial_bytes_before_pause"] = seen
            t0 = time.monotonic()

            # (3) Read real kernel state at the stop.
            with guest.paused_mid_execution(dwell=args.dwell) as program:
                at_stop = task_rows(program)
            held = time.monotonic() - t0
            metrics = guest.metrics[-1]
            out["stop_held_seconds"] = round(held, 2)
            out["at_stop_tasks"] = len(at_stop)
            out["at_stop_digest"] = digest(canonical(at_stop))
            out["at_stop_qemu_state"] = metrics.qemu_state
            out["at_stop_tracer_state"] = metrics.tracer_state
            out["at_stop_serial_delta"] = metrics.serial_bytes_delta
            out["at_stop_physical_reads"] = metrics.physical_reads
            print("  paused mid-execution for {:.2f}s: qemu={} tracer={} "
                  "serial_delta={} tasks={}".format(
                      held, metrics.qemu_state, metrics.tracer_state,
                      metrics.serial_bytes_delta, len(at_stop)), flush=True)

        # (4) Resume and run to the completion marker.
        guest.finish_advance(MARKER)

        with guest.observation() as program:
            after = task_rows(program)
        out["after_tasks"] = len(after)
        out["after_digest"] = digest(canonical(after))

        # The guest-visible record of the whole advance.
        transcript = bytes(guest._transcript)
        out["advance_serial_bytes"] = len(transcript)
        out["advance_serial_digest"] = digest(transcript)

        # What the guest itself thinks the pause cost it.
        import re
        uptimes = [float(m) for m in
                   re.findall(rb"^([0-9]+\.[0-9]+) [0-9]+\.[0-9]+",
                              transcript, re.MULTILINE)]
        out["guest_uptimes"] = uptimes

        # Kernel printk timestamps, 1us resolution, bracketing the pause.
        def kmsg(tag):
            m = re.search(rb"\[\s*([0-9]+\.[0-9]{6})\] " + tag, transcript)
            return float(m.group(1)) if m else None

        a, b = kmsg(b"HERMITMARKA"), kmsg(b"HERMITMARKB")
        out["kmsg_a"], out["kmsg_b"] = a, b
        if a is not None and b is not None:
            out["guest_seconds_across_pause"] = round(b - a, 6)
        out["total_seconds"] = round(time.monotonic() - started, 1)
        out["ok"] = True
    finally:
        guest.close()

    (ARTIFACTS / "{}.json".format(args.label)).write_text(
        json.dumps(out, indent=2, sort_keys=True) + "\n")
    (ARTIFACTS / "{}.serial".format(args.label)).write_bytes(transcript)
    print(json.dumps(out, sort_keys=True))


if __name__ == "__main__":
    main()
