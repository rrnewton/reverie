#!/usr/bin/env python3
"""How long does the Demo 7 guest take to come up, with and without the debugger?

The execution-control run timed out with no guest console output at all, which
could mean either a deadlock or merely a very slow startup. This separates the
two by timing each phase of start() in both modes.

Usage: drgn -q -p $$ startup-timing.py [--gdbserver] [--timeout S]
"""

import argparse
import os
from pathlib import Path
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE / "lib"))

import drgn_hermit as dh  # noqa: E402

ASSETS = Path(os.environ.get("QEMU_ASSETS", ROOT / "ignored/qemu-linux"))
ARTIFACTS = HERE / "artifacts"


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--gdbserver", action="store_true")
    p.add_argument("--port", type=int, default=1270)
    p.add_argument("--timeout", type=float, default=1200.0)
    p.add_argument("--flags", default="default",
                   help="default | none | nostrict")
    args = p.parse_args()

    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    par = ARTIFACTS / "drgn-par"
    par.mkdir(parents=True, exist_ok=True)
    os.environ.setdefault("FB_PAR_UNPACK_BASEDIR", str(par))

    config = dh.GuestConfig(
        root=ROOT,
        hermit=ROOT / "hermit/target/release/hermit",
        qemu=Path(os.environ.get("QEMU_BIN", "/usr/bin/qemu-system-x86_64")),
        kernel=ASSETS / "bzImage",
        initrd=ASSETS / "initramfs.cpio.gz",
        vmlinux=ASSETS / "vmlinux",
        snapshot_disk=ASSETS / "hermit-boot.qcow2",
        snapshot_name="hermit-boot",
        artifact_dir=ARTIFACTS,
        timeout=args.timeout,
        gdbserver_port=args.port if args.gdbserver else None,
        gdb_binary="/usr/bin/gdb",
        stop_at_writev=0,          # no mid-execution stop; just measure startup
        scheduler_flags={
            "default": ("--strict", "--no-rcb-time",
                        "--target-timeslice", "100000",
                        "--max-timeslice", "disabled"),
            "none": (),
            "nostrict": ("--no-rcb-time", "--target-timeslice", "100000",
                         "--max-timeslice", "disabled"),
        }[args.flags],
    )

    label = "gdbserver" if args.gdbserver else "plain"
    print("mode={} flags={} timeout={}s".format(label, args.flags, args.timeout), flush=True)
    guest = dh.HermitGuestProgram(config)
    started = time.monotonic()

    # Report progress from a watcher thread so a hang is visible as it happens.
    import threading
    stop_watch = threading.Event()

    def watch():
        while not stop_watch.wait(15.0):
            elapsed = time.monotonic() - started
            pid = guest._qemu_pid
            state = ""
            if pid:
                try:
                    state = dh._proc_state(pid)
                except OSError:
                    state = "gone"
            serial = 0
            if guest.serial_log and guest.serial_log.exists():
                serial = guest.serial_log.stat().st_size
            print("  t={:6.1f}s qemu_pid={} state={} serial={}B".format(
                elapsed, pid, state, serial), flush=True)

    threading.Thread(target=watch, daemon=True).start()
    try:
        guest.start()
        elapsed = time.monotonic() - started
        print("START OK in {:.1f}s  qemu_pid={} state={}".format(
            elapsed, guest._qemu_pid, dh._proc_state(guest._qemu_pid)), flush=True)
        t0 = time.monotonic()
        program = guest._program()
        init_task = program["init_task"]
        print("drgn read OK in {:.1f}s: init_task comm={!r} reads={}".format(
            time.monotonic() - t0,
            init_task.comm.string_().decode(), guest._reads), flush=True)
        print("RESULT {} startup={:.1f}s".format(label, elapsed))
    finally:
        stop_watch.set()
        guest.close()


if __name__ == "__main__":
    main()
