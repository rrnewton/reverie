#!/usr/bin/env python3
"""Report health-tick invocations that outlived their complete-run bound."""

from __future__ import annotations

import argparse
import dataclasses
import errno
import os
from pathlib import Path


DEFAULT_MAX_AGE_SECS = 900


class ScanUnavailable(RuntimeError):
    """The process census could not support a clean verdict."""


@dataclasses.dataclass(frozen=True)
class Process:
    pid: int
    ppid: int
    state: str
    start_ticks: int
    uid: int
    argv: tuple[str, ...]
    cgroup: str


@dataclasses.dataclass(frozen=True)
class Scan:
    processes: dict[int, Process]
    unreadable_same_uid: int


def _parse_stat(raw: str) -> tuple[str, int, int]:
    right = raw.rfind(")")
    if right < 0:
        raise ValueError("missing command terminator")
    fields = raw[right + 2 :].split()
    if len(fields) < 20:
        raise ValueError("too few stat fields")
    return fields[0], int(fields[1]), int(fields[19])


def _real_uid(raw: str) -> int:
    for line in raw.splitlines():
        if line.startswith("Uid:"):
            return int(line.split()[1])
    raise ValueError("missing Uid")


def _cgroup(raw: str) -> str:
    rows = [
        line.split(":", 2)[2] for line in raw.splitlines() if line.startswith("0::")
    ]
    return rows[0] if len(rows) == 1 else "unavailable"


def read_processes(proc_root: Path, uid: int) -> Scan:
    try:
        entries = list(proc_root.iterdir())
    except OSError as error:
        raise ScanUnavailable(f"cannot enumerate {proc_root}: {error}") from error

    processes: dict[int, Process] = {}
    unreadable = 0
    for entry in entries:
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        # Directory ownership is readable before any process-owned file and
        # lets the scanner distinguish a foreign process from a same-uid
        # identity it failed to inspect.  Without this boundary, a systematic
        # procfs denial produced an empty census and a false clean verdict.
        try:
            entry_uid = entry.stat().st_uid
        except OSError as error:
            if error.errno in (errno.ENOENT, errno.ESRCH):
                continue
            raise ScanUnavailable(
                f"cannot identify process directory {entry}: {error}"
            ) from error
        if entry_uid != uid:
            continue

        try:
            state, ppid, start_ticks = _parse_stat((entry / "stat").read_text())
            process_uid = _real_uid((entry / "status").read_text())
        except OSError as error:
            if error.errno in (errno.ENOENT, errno.ESRCH):
                continue
            unreadable += 1
            continue
        except ValueError:
            unreadable += 1
            continue
        if process_uid != uid or state == "Z":
            continue
        try:
            raw_argv = (entry / "cmdline").read_bytes()
            argv = tuple(
                field.decode("utf-8", errors="surrogateescape")
                for field in raw_argv.rstrip(b"\0").split(b"\0")
                if field
            )
        except OSError as error:
            if error.errno in (errno.ENOENT, errno.ESRCH):
                continue
            unreadable += 1
            continue
        try:
            cgroup = _cgroup((entry / "cgroup").read_text())
        except OSError:
            cgroup = "unavailable"

        # Bind argv and age to one process incarnation.  A PID can exit and be
        # reused while its files are being read; accepting the first start time
        # with the replacement's argv would splice two identities into one
        # stale finding.  Reparenting alone is harmless, so retain the newest
        # ppid/state after proving the start tick did not change.
        try:
            final_state, final_ppid, final_start_ticks = _parse_stat(
                (entry / "stat").read_text()
            )
        except OSError as error:
            if error.errno in (errno.ENOENT, errno.ESRCH):
                continue
            unreadable += 1
            continue
        except ValueError:
            unreadable += 1
            continue
        if final_start_ticks != start_ticks:
            continue
        if final_state == "Z":
            continue
        processes[pid] = Process(
            pid=pid,
            ppid=final_ppid,
            state=final_state,
            start_ticks=final_start_ticks,
            uid=process_uid,
            argv=argv,
            cgroup=cgroup,
        )
    return Scan(processes=processes, unreadable_same_uid=unreadable)


def _option(argv: tuple[str, ...], name: str) -> str | None:
    for index, token in enumerate(argv):
        if token == name and index + 1 < len(argv):
            return argv[index + 1]
        if token.startswith(name + "="):
            return token.split("=", 1)[1]
    return None


def is_health_tick(process: Process, root: Path) -> bool:
    """Match only the flushing production config/state pair for this checkout."""
    argv = process.argv
    if "tick" not in argv or "--flush" not in argv:
        return False
    if not any(
        Path(token).as_posix().endswith("/agent-utils/py/bin/tick-hub")
        for token in argv
    ):
        return False
    config = _option(argv, "--config")
    fired_state = _option(argv, "--fired-state")
    if config is None or fired_state is None:
        return False
    if not Path(config).as_posix().endswith("/ci-hub/health/tick-hub.yaml"):
        return False
    return Path(fired_state).resolve() == (root / ".tick-hub" / "fired-state").resolve()


def ancestor_pids(processes: dict[int, Process], self_pid: int) -> set[int]:
    ancestors: set[int] = set()
    current = self_pid
    while current > 1 and current not in ancestors:
        ancestors.add(current)
        process = processes.get(current)
        if process is None:
            break
        current = process.ppid
    return ancestors


def stale_invocations(
    scan: Scan,
    *,
    root: Path,
    uptime_seconds: float,
    clock_ticks: int,
    max_age_secs: int,
    self_pid: int,
) -> list[tuple[Process, float]]:
    matches = {
        pid for pid, process in scan.processes.items() if is_health_tick(process, root)
    }
    ancestors = ancestor_pids(scan.processes, self_pid)
    findings: list[tuple[Process, float]] = []
    for pid in sorted(matches):
        process = scan.processes[pid]
        # The Python launcher and its selected interpreter carry the same argv.
        # Name the outer invocation once instead of reporting both processes.
        if process.ppid in matches:
            continue
        if pid in ancestors:
            # This probe is a child of the current scheduled tick. Excluding its
            # exact ancestor chain cannot hide an older, unrelated invocation.
            continue
        age = uptime_seconds - process.start_ticks / clock_ticks
        if age >= max_age_secs:
            findings.append((process, age))
    return findings


def _field(name: str, value: object) -> None:
    if name == "summary":
        value = str(value).replace("{", "(").replace("}", ")")
    print(f"{name}={value}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--gate", action="store_true", help="emit tick-hub key/value fields"
    )
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[2]
    )
    parser.add_argument(
        "--proc-root", type=Path, default=Path("/proc"), help=argparse.SUPPRESS
    )
    parser.add_argument("--max-age-secs", type=int, default=DEFAULT_MAX_AGE_SECS)
    args = parser.parse_args(argv)
    if args.max_age_secs <= 0:
        parser.error("--max-age-secs must be positive")

    try:
        uptime = float((args.proc_root / "uptime").read_text().split()[0])
        ticks = os.sysconf("SC_CLK_TCK")
        scan = read_processes(args.proc_root, os.getuid())
        findings = stale_invocations(
            scan,
            root=args.root.resolve(),
            uptime_seconds=uptime,
            clock_ticks=ticks,
            max_age_secs=args.max_age_secs,
            self_pid=os.getpid(),
        )
    except (OSError, ValueError, ScanUnavailable) as error:
        _field("state", "unknown")
        _field(
            "summary",
            f"stale tick process scan unavailable: {' '.join(str(error).split())}",
        )
        return 75 if args.gate else 2

    if findings:
        details = ", ".join(
            f"pid={process.pid} age_secs={int(age)} ppid={process.ppid} cgroup={process.cgroup}"
            for process, age in findings
        )
        state = "actionable"
        summary = (
            f"{len(findings)} health-tick invocation(s) exceeded the "
            f"{args.max_age_secs}s complete-run bound: {details}"
        )
        code = 1
    elif scan.unreadable_same_uid:
        state = "unknown"
        summary = (
            "no stale health-tick invocation was readable, but "
            f"{scan.unreadable_same_uid} same-uid process identity/identities were unreadable"
        )
        code = 75 if args.gate else 2
    else:
        state = "ok"
        summary = f"no health-tick invocation exceeded {args.max_age_secs}s"
        code = 0

    _field("state", state)
    _field("summary", summary)
    _field("stale_tick_invocations", len(findings))
    _field("process_scan_unreadable_same_uid", scan.unreadable_same_uid)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
