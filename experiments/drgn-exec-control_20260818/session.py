#!/usr/bin/env python3
"""One recorded session: stop Linux at a chosen point, inspect it, resume.

Prints a transcript of the whole loop so the result can be read rather than
inferred from JSON. Run the same --stop-at twice and the kernel-state block
must be identical; run a different --stop-at and it must differ.

Usage:
  HERMIT_RELEASE=<hermit> drgn -q -p $$ session.py --stop-at N --port P
"""

import argparse
import hashlib
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
COMMAND = (
    "for n in 1 2; do sleep 1000 & done; "
    'usleep 1000; echo __HERMIT_EXEC_CONTROL_"DONE__"'
)

# Linux task states, enough to label what the task list shows.
# Linux task state bits. 1024 is TASK_NOLOAD, so 1026 is TASK_IDLE -- the
# state most kernel worker threads sit in.
TASK_STATE = {0: "R (running)", 1: "S (sleeping)", 2: "D (uninterruptible)",
              4: "T (stopped)", 8: "t (traced)", 16: "X (dead)",
              32: "Z (zombie)", 1026: "I (idle kthread)"}


def say(text=""):
    print(text, flush=True)


def kernel_state(program):
    """Read real kernel state. Returns (lines, canonical) -- canonical is what
    two runs at the same stop index must match on."""
    lines = []
    canon = []

    init_task = program["init_task"]
    rows = [(init_task.pid.value_(),
             init_task.comm.string_().decode("utf-8", "replace"),
             int(init_task.__state.value_()) if hasattr(init_task, "__state") else -1)]
    for task in list_for_each_entry(
        "struct task_struct", init_task.tasks.address_of_(), "tasks"
    ):
        try:
            state = int(task.__state.value_())
        except Exception:  # noqa: BLE001
            state = -1
        rows.append((task.pid.value_(),
                     task.comm.string_().decode("utf-8", "replace"), state))

    lines.append("  task_struct.tasks walk: {} tasks".format(len(rows)))
    for pid, comm, state in rows[:6]:
        lines.append("    pid {:<5} {:<18} state {}".format(
            pid, comm, TASK_STATE.get(state, state)))
    lines.append("    ... {} more".format(max(0, len(rows) - 6)))
    for pid, comm, state in rows:
        canon.append("{} {} {}".format(pid, comm, state))

    histogram = {}
    for _pid, _comm, state in rows:
        key = TASK_STATE.get(state, "raw {}".format(state))
        histogram[key] = histogram.get(key, 0) + 1
    lines.append("  by state: " + ", ".join(
        "{} x{}".format(k, v) for k, v in sorted(histogram.items())))

    sleepers = [r for r in rows if r[1] == "sleep"]
    lines.append("  tasks named 'sleep': {}".format(len(sleepers)))
    canon.append("sleepers={}".format(len(sleepers)))

    text = "\n".join(canon)
    return lines, text, len(rows), len(sleepers)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--stop-at", type=int, default=1)
    parser.add_argument("--port", type=int, default=33301)
    parser.add_argument("--dwell", type=float, default=0.0)
    parser.add_argument("--timeout", type=float, default=400.0)
    args = parser.parse_args()

    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    par = ARTIFACTS / "drgn-par"
    par.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("FB_PAR_UNPACK_BASEDIR", str(par))

    hermit = Path(os.environ.get("HERMIT_RELEASE",
                                 ROOT / "hermit/target/release/hermit"))
    config = dh.GuestConfig(
        root=ROOT, hermit=hermit,
        qemu=Path(os.environ.get("QEMU_BIN", "/usr/bin/qemu-system-x86_64")),
        kernel=ASSETS / "bzImage", initrd=ASSETS / "initramfs.cpio.gz",
        vmlinux=ASSETS / "vmlinux", snapshot_disk=ASSETS / "hermit-boot.qcow2",
        snapshot_name="hermit-boot", artifact_dir=ARTIFACTS,
        timeout=args.timeout, gdbserver_port=args.port,
        gdb_binary=os.environ.get("GDB", "/usr/bin/gdb"),
        stop_at_writev=args.stop_at,
    )

    say("=" * 72)
    say("Stop Linux under Hermit at a chosen point, inspect it, resume")
    say("=" * 72)
    say("hermit    : {}".format(hermit))
    say("scheduler : {}".format(" ".join(config.scheduler_flags)))
    say("debugger  : hermit run --gdbserver --gdbserver-port {}".format(args.port))
    say("stop point: breakpoint on writev, {}th guest console byte".format(
        args.stop_at))
    say("            (QEMU emits guest console output one byte per writev)")
    say()

    guest = dh.HermitGuestProgram(config)
    started = time.monotonic()
    try:
        guest.start()
        say("[1] Linux running under Hermit")
        say("    demo-5 boot snapshot restored in {:.1f}s, QEMU pid {}".format(
            time.monotonic() - started, guest._qemu_pid))
        say()

        say("[2] guest resumed, running: {}".format(COMMAND))
        guest.begin_advance(COMMAND)
        waited = guest.wait_for_execution_stop()
        say("    Hermit's breakpoint stopped it after {:.2f}s".format(waited))
        say("    QEMU process state = {!r} (t = ptrace-stop; executes nothing)".format(
            dh._proc_state(guest._qemu_pid)))
        say()

        say("[3] drgn reads real kernel state at that stop")
        with guest.stopped_observation() as program:
            lines, canon, ntasks, nsleep = kernel_state(program)
        metrics = guest.metrics[-1]
        for line in lines:
            say(line)
        say("  served by {} physical reads ({} bytes) of /proc/{}/mem".format(
            metrics.physical_reads, metrics.physical_bytes, guest._qemu_pid))
        say("  guest console bytes emitted during the read: {}".format(
            metrics.serial_bytes_delta))
        say("  STATE DIGEST: {}".format(
            hashlib.sha256(canon.encode()).hexdigest()[:16]))
        say()

        if args.dwell > 0:
            say("    holding the stop for {}s of wall clock...".format(args.dwell))
            time.sleep(args.dwell)
            say("    QEMU state after the dwell = {!r}".format(
                dh._proc_state(guest._qemu_pid)))
            say()

        say("[4] resumed")
        guest.release_execution_stop()
        guest.finish_advance(MARKER)
        say("    guest ran on and printed its completion marker")

        with guest.paused_observation() as program:
            _, after_canon, after_tasks, after_sleep = kernel_state(program)
        say("    kernel state after: {} tasks, {} named 'sleep'".format(
            after_tasks, after_sleep))
        say()
        say("SUMMARY  stop_at={}  tasks_at_stop={}  sleepers_at_stop={}"
            "  state_digest={}  total={:.1f}s".format(
                args.stop_at, ntasks, nsleep,
                hashlib.sha256(canon.encode()).hexdigest()[:16],
                time.monotonic() - started))
    finally:
        guest.close()


if __name__ == "__main__":
    main()
