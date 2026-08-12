#!/usr/bin/env python3
"""One writer per results ledger — and a DEAD writer must never block a live one.

WHY
---
The corpus runners append to a shared `results.tsv` and dedupe with
`grep -q "^$label\\t" results.tsv && continue`. Two drivers against one ledger
corrupt it in a way that leaves no trace:

  * TOCTOU on the dedupe. Both greps miss the same label, both measure it, both
    append. The file still parses, every row is well-formed, and
    `wc -l - 1` now OVERSTATES coverage. A `133/171` derived from it is wrong
    with no symptom.
  * Two verdicts for one guest, possibly disagreeing. Whichever a reader hits
    first wins, and which that is depends on append order.

That is worse than a crash: a crash stops and says so; this publishes.

Observed 2026-08-07: `ignored/w7-corpus-run/results.tsv` is written by
`runner/runner.sh`, `runner/batch.sh`, and `runner/wait_then_batch.sh`, and a
FOURTH driver (`scratch/w27-corpus-fix/wait_then_batch.fixed.sh`, a different
agent) appends to the same file. None of the four takes any lock.

THE MECHANISM, AND WHY IT IS flock AND NOT A PID FILE
------------------------------------------------------
A PID/heartbeat file has to answer "is the owner still alive?", and getting that
wrong in the safe direction produces a guard that cannot be released when its
owner dies. That is the trap `ci-hub validate-lock` is in: an owner proven dead,
a quarantine that `reclaim-dead` refuses by design, and every agent's run
refused until a human intervenes. Do not build a second one.

`flock(2)` has no such state. The lock lives on the OPEN FILE DESCRIPTION, so the
kernel drops it when the holder exits, is killed -9, segfaults, or has its
terminal torn away. There is nothing to time out, nothing to reclaim, and no
stale PID can hold it: a dead owner is indistinguishable from an owner that
never existed. The dead-owner trap is not mitigated here, it is unrepresentable.

The guard therefore `exec`s the driver while holding the fd, so the lock's
lifetime is exactly the driver's lifetime.

The PID/start-time/boot-id sidecar this writes is DIAGNOSTIC ONLY -- it exists so
a human can see who holds the lock. It is never consulted to decide whether to
refuse. Keeping it strictly non-authoritative is what stops it growing into the
thing it replaced.

USAGE
-----
    scripts/single-driver.py run --ledger PATH [--name NAME] -- CMD [ARG...]
    scripts/single-driver.py status --ledger PATH
    scripts/single-driver.py check  --ledger PATH     # exit 0 free, 3 held

EXIT
----
0  the driver ran (its own status is propagated)
3  REFUSED: another driver holds the ledger
4  the lock file cannot be created, or the filesystem cannot flock
2  usage
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import json
import os
import socket
import sys
import time
from pathlib import Path
from typing import Optional

LOCK_SUFFIX = ".driver-lock"
OWNER_SUFFIX = ".driver-owner.json"


def lock_path(ledger: Path) -> Path:
    """Lock a SIDECAR, not the ledger itself.

    Locking the ledger would collide with anything that legitimately opens it
    for reading or appending, and would make the guard's presence change the
    behaviour of unrelated tools.
    """
    return ledger.with_name(ledger.name + LOCK_SUFFIX)


def owner_path(ledger: Path) -> Path:
    return ledger.with_name(ledger.name + OWNER_SUFFIX)


def boot_id() -> str:
    try:
        return Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    except OSError:
        return "unknown"


def start_ticks(pid: int) -> Optional[int]:
    """Field 22 of /proc/<pid>/stat: process start time in clock ticks.

    PIDs are reused. `(pid, start_ticks, boot_id)` is an identity; a bare pid is
    a guess that grows more wrong the longer the box has been up. Only used for
    the human-readable report.
    """
    try:
        raw = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    # The comm field is parenthesised and may contain spaces or ')'.
    try:
        after = raw[raw.rindex(")") + 2:]
        return int(after.split()[19])
    except (ValueError, IndexError):
        return None


def read_owner(ledger: Path) -> Optional[dict]:
    try:
        return json.loads(owner_path(ledger).read_text())
    except (OSError, ValueError):
        return None


def describe_owner(rec: Optional[dict]) -> str:
    if not rec:
        return ("held, but the owner sidecar is missing or unreadable. The LOCK "
                "is still authoritative: something live holds it.")
    pid = rec.get("pid")
    live = "unknown"
    if rec.get("boot_id") == boot_id() and isinstance(pid, int):
        st = start_ticks(pid)
        if st is None:
            live = "that pid is GONE (but see below)"
        elif rec.get("start_ticks") is not None and st != rec["start_ticks"]:
            live = "that pid was REUSED by a different process (but see below)"
        else:
            live = "alive"
    held = time.time() - rec.get("since_epoch", time.time())
    return (f"agent/name={rec.get('name')} pid={pid} ({live}) "
            f"host={rec.get('host')} held_for={held / 60:.1f} min\n"
            f"    command: {rec.get('cmd')}")


def acquire_or_refuse(ledger: Path, name: str, cmd: list[str]) -> int:
    lp = lock_path(ledger)
    try:
        lp.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(lp, os.O_CREAT | os.O_RDWR, 0o644)
    except OSError as e:
        print(f"single-driver: cannot create lock file {lp}: {e}", file=sys.stderr)
        return 4

    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as e:
        if e.errno in (errno.EWOULDBLOCK, errno.EAGAIN, errno.EACCES):
            rec = read_owner(ledger)
            print(
                f"single-driver: REFUSED — another driver is writing {ledger}.\n"
                f"    {describe_owner(rec)}\n"
                f"    Two drivers on one ledger interleave the "
                f"grep-then-append dedupe: both miss a label, both measure it, "
                f"both append. The file stays well-formed and the row count "
                f"silently overstates coverage.\n"
                f"    Nothing to clean up: this lock is an flock, so the kernel "
                f"releases it the instant the holder exits or is killed. If that "
                f"process is gone, retry and you will get the lock.",
                file=sys.stderr)
            return 3
        if e.errno in (errno.ENOLCK, errno.EOPNOTSUPP, errno.ENOSYS):
            # FAIL CLOSED. Degrading to "no lock" here would silently restore
            # exactly the unguarded behaviour this tool exists to remove, and it
            # would do so on the filesystems where concurrent writers are most
            # likely (shared/network storage).
            print(
                f"single-driver: cannot lock on this filesystem ({e}). Refusing "
                f"rather than running unguarded — put the ledger on a filesystem "
                f"that supports flock, or accept the corruption risk explicitly "
                f"by not using this wrapper.", file=sys.stderr)
            return 4
        raise

    # Held. Record WHO for humans; this is never read to make a decision.
    try:
        owner_path(ledger).write_text(json.dumps({
            "name": name,
            "pid": os.getpid(),
            "start_ticks": start_ticks(os.getpid()),
            "boot_id": boot_id(),
            "host": socket.gethostname(),
            "ledger": str(ledger),
            "cmd": " ".join(cmd),
            "since_epoch": time.time(),
            "since_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "authority": "the flock on "
                         f"{lock_path(ledger).name}; this file is diagnostic only",
        }, indent=2) + "\n")
    except OSError:
        pass  # Diagnostics are best-effort; the lock is the guarantee.

    # Keep the fd open across exec so the lock's lifetime IS the driver's.
    os.set_inheritable(fd, True)
    try:
        os.execvp(cmd[0], cmd)
    except OSError as e:
        print(f"single-driver: cannot exec {cmd[0]}: {e}", file=sys.stderr)
        return 2


def cmd_status(ledger: Path) -> int:
    lp = lock_path(ledger)
    if not lp.exists():
        print(f"single-driver: FREE — no lock file at {lp}")
        return 0
    fd = os.open(lp, os.O_CREAT | os.O_RDWR, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print(f"single-driver: HELD — {ledger}\n    {describe_owner(read_owner(ledger))}")
        return 3
    finally_free = True
    if finally_free:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)
        rec = read_owner(ledger)
        extra = ""
        if rec:
            extra = (f"\n    (a stale owner sidecar from pid {rec.get('pid')} is "
                     f"present and is IGNORED — the lock is free, so a new driver "
                     f"may run. A stale record never blocks.)")
        print(f"single-driver: FREE — {ledger}{extra}")
        return 0
    return 0


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="verb", required=True)

    r = sub.add_parser("run", description="run CMD as the sole driver of --ledger", help="run CMD as the sole driver of --ledger")
    r.add_argument("--ledger", required=True)
    r.add_argument("--name", default=os.environ.get("HERMIT_AGENT", "unnamed-driver"))
    r.add_argument("cmd", nargs=argparse.REMAINDER)

    for verb in ("status", "check"):
        s = sub.add_parser(verb, description="report whether a driver holds --ledger", help="report whether a driver holds --ledger")
        s.add_argument("--ledger", required=True)

    a = ap.parse_args(argv)
    ledger = Path(a.ledger).resolve()

    if a.verb in ("status", "check"):
        return cmd_status(ledger)

    cmd = [c for c in a.cmd if c != "--"] if a.cmd and a.cmd[0] == "--" else a.cmd
    if not cmd:
        print("single-driver: run needs a command after `--`", file=sys.stderr)
        return 2
    return acquire_or_refuse(ledger, a.name, cmd)


if __name__ == "__main__":
    raise SystemExit(main())
