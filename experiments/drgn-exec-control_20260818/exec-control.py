#!/usr/bin/env python3
"""Stop Linux under Hermit mid-execution, read kernel state, resume.

Demo 7 reads guest kernel state only where QEMU has cooperatively parked
itself.  This adds the other half: Hermit's own debugger stops the guest
mid-execution, drgn reads real kernel state at that stop, and the guest
resumes.

Each run performs one restore of the Demo 5 boot snapshot and then:

  1. reads the guest task list with the vCPUs paused (the Demo 7 read);
  2. resumes the guest running a fixed command;
  3. lets Hermit's breakpoint stop QEMU at the Nth guest console byte,
     mid-execution, and reads the task list again there;
  4. resumes and runs the command to its completion marker;
  5. reads the task list a third time.

`--dwell` holds the mid-execution stop for a fixed number of wall-clock
seconds.  Comparing a dwelling run against a non-dwelling one is the test that
matters: under Hermit's virtual time the pause is supposed to cost the guest
nothing, so both runs must produce the same guest console transcript and the
same task lists.

Usage:
  drgn -q -p $$ exec-control.py [--dwell SECONDS] [--stop-at N] [--label NAME]
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
# fixed 1000us guest timer.  The marker is split so the shell's echo of the
# command line cannot satisfy the wait.
COMMAND = (
    "for n in 1 2; do sleep 1000 & done; "
    'usleep 1000; echo __HERMIT_EXEC_CONTROL_"DONE__"'
)


def task_rows(program):
    init_task = program["init_task"]
    rows = [(init_task.pid.value_(), init_task.comm.string_().decode("utf-8", "replace"))]
    rows.extend(
        (t.pid.value_(), t.comm.string_().decode("utf-8", "replace"))
        for t in list_for_each_entry(
            "struct task_struct", init_task.tasks.address_of_(), "tasks"
        )
    )
    return rows


def canonical(rows):
    return "\n".join("{} {}".format(pid, comm) for pid, comm in rows)


def digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dwell", type=float, default=0.0,
                        help="wall-clock seconds to hold the mid-execution stop")
    parser.add_argument("--stop-at", type=int, default=1,
                        help="stop at the Nth guest console byte after resume")
    parser.add_argument("--label", default="run")
    parser.add_argument("--port", type=int,
                        default=int(os.environ.get("EXEC_PORT", "1260")))
    parser.add_argument("--timeout", type=float, default=600.0)
    args = parser.parse_args()

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
        gdbserver_port=args.port,
        gdb_binary=os.environ.get("GDB", "/usr/bin/gdb"),
        stop_at_writev=args.stop_at,
    )

    result = {
        "label": args.label,
        "dwell_seconds": args.dwell,
        "stop_at_console_byte": args.stop_at,
    }
    guest = dh.HermitGuestProgram(config)
    started = time.monotonic()
    try:
        guest.start()
        result["startup_seconds"] = round(time.monotonic() - started, 1)
        result["qemu_pid"] = guest._qemu_pid
        result["hermit_gdbserver_port"] = args.port

        # (1) Demo 7's read: vCPUs paused right after the restore.
        with guest.paused_observation() as program:
            before = task_rows(program)
        result["before_tasks"] = len(before)

        # (2) Resume the guest running the fixed command.
        guest.begin_advance(COMMAND)

        # (3) Hermit's breakpoint stops QEMU mid-execution.
        waited = guest.wait_for_execution_stop()
        result["seconds_until_stop"] = round(waited, 2)
        qemu_state = dh._proc_state(guest._qemu_pid)
        result["qemu_state_at_stop"] = qemu_state
        print("  stopped mid-execution after {:.2f}s, QEMU state={}".format(
            waited, qemu_state))

        with guest.stopped_observation() as program:
            at_stop = task_rows(program)
        result["at_stop_tasks"] = len(at_stop)
        result["at_stop_reads"] = guest.metrics[-1].physical_reads
        result["at_stop_bytes"] = guest.metrics[-1].physical_bytes
        print("  drgn read {} tasks at the stop ({} physical reads)".format(
            len(at_stop), guest.metrics[-1].physical_reads))

        if args.dwell > 0:
            print("  holding the stop for {}s of wall clock...".format(args.dwell))
            time.sleep(args.dwell)
            # The guest must not have moved while we sat here.
            after_dwell = dh._proc_state(guest._qemu_pid)
            result["qemu_state_after_dwell"] = after_dwell
            if after_dwell != "t":
                raise RuntimeError(
                    "QEMU left its ptrace-stop during the dwell: {}".format(
                        after_dwell))

        # (4) Resume and run to the completion marker.
        guest.release_execution_stop()
        guest.finish_advance(MARKER)

        # (5) Read the task list again.
        with guest.paused_observation() as program:
            after = task_rows(program)
        result["after_tasks"] = len(after)

        result["before_digest"] = digest(canonical(before))
        result["at_stop_digest"] = digest(canonical(at_stop))
        result["after_digest"] = digest(canonical(after))
        serial = guest.serial_log.read_bytes()
        result["serial_bytes"] = len(serial)
        result["serial_digest"] = hashlib.sha256(serial).hexdigest()[:16]
        result["total_seconds"] = round(time.monotonic() - started, 1)
        result["ok"] = True
    finally:
        guest.close()

    out = ARTIFACTS / "{}.json".format(args.label)
    out.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
