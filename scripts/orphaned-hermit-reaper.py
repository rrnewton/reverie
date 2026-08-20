#!/usr/bin/env python3
"""Find and reap proven runaway Hermit or test orphans.

The default is report-only.  ``--apply`` additionally requires the exact
acknowledgement flag, and signals through a pidfd after re-reading the process
identity.  It never uses a name-, user-, process-group-, or cgroup-wide kill:
an agent scope can contain both a live coordinator and an orphaned Hermit.

A process is eligible only when all of these independently observable facts
hold:

* its real uid is the caller's uid and its current parent is PID 1;
* its cgroup is a 3pai per-agent ``run-p*.scope`` for that same uid;
* its executable resolves below this dev-hermit workspace;
* it is either ``hermit ... run ...`` or a Cargo test executable under
  ``target/{debug,release}/deps``;
* it is older than the age bound; AND
* it is BUSY, by either of two independent measures -- consumed CPU time and
  lifetime CPU/age ratio both above their bounds, OR its process TREE writing to
  disk above the write-rate bound.  The disk measure exists because every other
  gate is CPU-based, so an orphan that writes and sleeps was invisible.

This is cleanup containment.  New ad-hoc runs should instead use
``scripts/hermit-box-run``, whose cgroup owns and reaps the complete process
tree at the resource boundary.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import json
import os
from pathlib import Path
import re
import select
import signal
import sys
import time
import traceback
from collections.abc import Callable, Iterable


DEFAULT_MIN_AGE_SECONDS = 15 * 60.0
DEFAULT_MIN_CPU_SECONDS = 10 * 60.0
DEFAULT_MIN_CORE_RATIO = 0.80
DEFAULT_GRACE_SECONDS = 5.0
CONFIRMATION = "signal-only-the-reported-pids"

MIB = 1 << 20
# THE DISK CRITERION, added 2026-08-20.
#
# Every gate above except age is CPU-based, so an orphan that writes and sleeps
# was invisible BY CONSTRUCTION. Measured on 2026-08-19, three real orphans that
# survived up to 3h50m had core ratios of 0.003, 0.083 and 0.0000 against the
# 0.80 gate; relaxing age+cpu without the ratio still yielded zero candidates.
# A low-CPU steady log writer is precisely what filled 4.5 TB on 2026-08-19.
#
# The answer is NOT a looser CPU threshold -- 0.003 would match every idle
# process on the box. It is a different quantity: bytes actually written.
#
# 1 MiB/s is ~84 GiB/day, far above the ~0 that an idle orphan writes and far
# below a runaway. It is a floor for "this process is doing damage while
# nobody is watching", not an attempt to characterise normal I/O.
DEFAULT_MIN_WRITE_BYTES_PER_SECOND = 1.0 * MIB
# Two samples inside ONE invocation, rather than a state file compared across
# runs. That choice is deliberate and it buys the liveness property for free:
# a bounded emitter that already finished reads zero across the window, so it
# cannot trip the gate. A cross-run state file would instead measure an
# interval that is already history -- the exact false positive that escalated a
# completed 240 MiB write as "23.6 GiB/h" on 2026-08-19.
DEFAULT_DISK_SAMPLE_SECONDS = 3.0


class ProcReadError(RuntimeError):
    """A process could not be read consistently."""


@dataclasses.dataclass(frozen=True)
class ProcessSnapshot:
    pid: int
    start_ticks: int
    ppid: int
    uid: int
    state: str
    cpu_seconds: float
    age_seconds: float
    exe: Path
    argv: tuple[str, ...]
    cgroup: str

    @property
    def core_ratio(self) -> float:
        return self.cpu_seconds / self.age_seconds if self.age_seconds > 0 else 0.0


@dataclasses.dataclass(frozen=True)
class Bounds:
    min_age_seconds: float = DEFAULT_MIN_AGE_SECONDS
    min_cpu_seconds: float = DEFAULT_MIN_CPU_SECONDS
    min_core_ratio: float = DEFAULT_MIN_CORE_RATIO
    min_write_bytes_per_second: float = DEFAULT_MIN_WRITE_BYTES_PER_SECOND


@dataclasses.dataclass(frozen=True)
class Decision:
    snapshot: ProcessSnapshot
    eligible: bool
    reasons: tuple[str, ...]
    kind: str | None = None
    # None means NOT MEASURED, which is not the same as measured-and-zero. Only
    # a measured rate can qualify a process; an unmeasured one falls back to the
    # CPU path exactly as before this criterion existed.
    write_bytes_per_second: float | None = None


@dataclasses.dataclass(frozen=True)
class ReapResult:
    pid: int
    outcome: str
    term_sent: bool
    kill_sent: bool


def _read_text(path: Path) -> str:
    try:
        return path.read_text()
    except (FileNotFoundError, ProcessLookupError) as exc:
        raise ProcReadError(f"process disappeared while reading {path}") from exc
    except OSError as exc:
        raise ProcReadError(f"cannot read {path}: {exc}") from exc


def _parse_stat(raw: str) -> tuple[str, int, int, int, int]:
    """Return state, ppid, utime, stime, starttime from proc(5) stat."""
    right = raw.rfind(")")
    if right < 0:
        raise ProcReadError("malformed /proc stat: missing command terminator")
    fields = raw[right + 2 :].split()
    # fields[0] is field 3 (state); indices below follow proc(5).
    if len(fields) < 20:
        raise ProcReadError("malformed /proc stat: too few fields")
    try:
        return fields[0], int(fields[1]), int(fields[11]), int(fields[12]), int(fields[19])
    except (ValueError, IndexError) as exc:
        raise ProcReadError("malformed numeric /proc stat field") from exc


def _parse_uid(status: str) -> int:
    for line in status.splitlines():
        if line.startswith("Uid:"):
            fields = line.split()
            if len(fields) >= 2:
                try:
                    return int(fields[1])
                except ValueError as exc:
                    raise ProcReadError("malformed real uid in /proc status") from exc
    raise ProcReadError("missing Uid field in /proc status")


def _parse_cgroup(raw: str) -> str:
    unified = [line.split(":", 2)[2] for line in raw.splitlines() if line.startswith("0::")]
    if len(unified) != 1 or not unified[0].startswith("/"):
        raise ProcReadError("missing or ambiguous unified cgroup membership")
    return unified[0]


class ProcReader:
    def __init__(
        self,
        proc_root: Path = Path("/proc"),
        *,
        clock_ticks: int | None = None,
        uptime_seconds: Callable[[], float] | None = None,
    ) -> None:
        self.proc_root = proc_root
        self.clock_ticks = clock_ticks or os.sysconf("SC_CLK_TCK")
        self._uptime_seconds = uptime_seconds or self._read_uptime

    def _read_uptime(self) -> float:
        try:
            return float(_read_text(self.proc_root / "uptime").split()[0])
        except (ValueError, IndexError) as exc:
            raise ProcReadError("malformed /proc/uptime") from exc

    def pids(self) -> Iterable[int]:
        try:
            entries = self.proc_root.iterdir()
        except OSError as exc:
            raise ProcReadError(f"cannot enumerate {self.proc_root}: {exc}") from exc
        for entry in entries:
            if entry.name.isdigit():
                yield int(entry.name)

    def read(self, pid: int) -> ProcessSnapshot:
        base = self.proc_root / str(pid)
        state, ppid, utime, stime, start_ticks = _parse_stat(_read_text(base / "stat"))
        uid = _parse_uid(_read_text(base / "status"))
        cgroup = _parse_cgroup(_read_text(base / "cgroup"))
        try:
            exe_raw = os.readlink(base / "exe")
            cmdline_raw = (base / "cmdline").read_bytes()
        except (FileNotFoundError, ProcessLookupError) as exc:
            raise ProcReadError(f"process {pid} disappeared while reading identity") from exc
        except OSError as exc:
            raise ProcReadError(f"cannot read process {pid} identity: {exc}") from exc
        if exe_raw.endswith(" (deleted)"):
            raise ProcReadError("executable has been deleted")
        argv = tuple(
            part.decode("utf-8", errors="surrogateescape")
            for part in cmdline_raw.rstrip(b"\0").split(b"\0")
            if part
        )
        uptime = self._uptime_seconds()
        age = uptime - (start_ticks / self.clock_ticks)
        if age < 0:
            raise ProcReadError("process start time is newer than system uptime")
        return ProcessSnapshot(
            pid=pid,
            start_ticks=start_ticks,
            ppid=ppid,
            uid=uid,
            state=state,
            cpu_seconds=(utime + stime) / self.clock_ticks,
            age_seconds=age,
            exe=Path(exe_raw),
            argv=argv,
            cgroup=cgroup,
        )


def _inside(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _agent_scope(uid: int, cgroup: str) -> bool:
    prefix = (
        rf"/user\.slice/user-{uid}\.slice/user@{uid}\.service/"
        rf"3pai_sandbox\.slice/run-p[0-9]+(?:-i[A-Za-z0-9_.:-]+)?\.scope"
    )
    return re.fullmatch(prefix + r"(?:/.*)?", cgroup) is not None


def _process_kind(snapshot: ProcessSnapshot, workspace: Path) -> str | None:
    if not _inside(snapshot.exe, workspace):
        return None
    if snapshot.exe.name == "hermit":
        # The known leak is a Hermit run, not a long-lived coordinator/helper.
        return "hermit-run" if "run" in snapshot.argv[1:] else None
    try:
        relative = snapshot.exe.relative_to(workspace)
    except ValueError:
        return None
    parts = relative.parts
    for index, part in enumerate(parts[:-2]):
        if part == "target" and parts[index + 1] in {"debug", "release"} and parts[index + 2] == "deps":
            return "cargo-test"
    return None


def _maybe_in_workspace(proc_root: Path, pid: int, workspace: Path) -> bool:
    """Could this process's executable be under the workspace?

    Used ONLY to exclude a process from the unreadable warning, never to qualify
    one for signalling. `readlink /proc/<pid>/exe` still returns the original
    path when the file has been unlinked -- it just appends " (deleted)" -- so a
    process whose binary was replaced on disk can still be placed. Unknown
    counts as in-scope, so nothing of ours is dropped on a guess.
    """
    try:
        target = os.readlink(proc_root / str(pid) / "exe")
    except OSError:
        return True
    return _inside(Path(target.removesuffix(" (deleted)")), workspace)


def _readable_uid(proc_root: Path, pid: int) -> int | None:
    """Real uid from /proc/<pid>/status, or None if that too is unreadable.

    /proc/<pid>/status is world-readable where /proc/<pid>/exe is not, so this
    answers "is this even mine?" for a process whose identity otherwise cannot
    be read. None means genuinely unknown, and the caller treats unknown as
    in-scope so an unreadable process of ours is never silently dropped.
    """
    try:
        return _parse_uid(_read_text(proc_root / str(pid) / "status"))
    except ProcReadError:
        return None


def _children_map(proc_root: Path) -> dict[int, list[int]]:
    """ppid -> [pid]. One /proc walk, reused for every root in a sample."""
    kids: dict[int, list[int]] = {}
    try:
        entries = list(proc_root.iterdir())
    except OSError:
        return kids
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            _state, ppid, *_rest = _parse_stat(_read_text(entry / "stat"))
        except (ProcReadError, OSError):
            continue  # a process that vanished mid-walk contributes nothing
        kids.setdefault(ppid, []).append(int(entry.name))
    return kids


def tree_write_bytes(root_pid: int, kids: dict[int, list[int]], proc_root: Path) -> int:
    """Sum ``write_bytes`` over the process TREE rooted at ``root_pid``.

    THE TREE, not the process. This is the whole reason the criterion works.
    Measured on 2026-08-19 against an orphaned Hermit tree whose guest was doing
    64 KiB writes in a loop:

        pid 3207650  hermit (ppid=1)        write_bytes 0 -> 0            0.00 MiB/s
        pid 3207653  sh (guest, state 't')  1,115,701,248 -> 1,560,297,472  84.80 MiB/s

    The writes do not appear on the Hermit process AT ALL; they are attributed
    entirely to the guest child. A gate reading /proc/<hermit>/io would always
    see zero and would reproduce the CPU gate's blindness in a new form.

    Note the guest was in state 't' -- ptrace-stopped -- while writing at
    84.8 MiB/s. A stopped process is still a disk writer, which is also how the
    real orphans presented.

    An unreadable child contributes 0 rather than failing the sum: partial
    attribution understates, and understating can only ever make the reaper
    less likely to act.
    """
    total = 0
    seen: set[int] = set()
    stack = [root_pid]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        try:
            for line in _read_text(proc_root / str(pid) / "io").splitlines():
                if line.startswith("write_bytes:"):
                    total += int(line.split()[1])
                    break
        except (ProcReadError, OSError, ValueError, IndexError):
            pass
        stack.extend(kids.get(pid, ()))
    return total


def sample_write_rates(
    pids: Iterable[int],
    *,
    interval_seconds: float,
    proc_root: Path = Path("/proc"),
    sleep: Callable[[float], None] = time.sleep,
) -> dict[int, float]:
    """Tree write rate in bytes/second, from two samples inside ONE invocation.

    Self-contained on purpose. Because both samples are taken here, a rate above
    zero means the tree is writing NOW -- there is no stale interval to
    misinterpret and no state file to go stale. A bounded emitter that has
    already finished reads exactly zero.

    It also sidesteps the fd-holder trap by construction: ``write_bytes`` only
    advances when a process actually writes, so unlike an open-fd size delta
    there is no holder to misattribute and no read-only holder to mistake for a
    writer.
    """
    pids = list(pids)
    if not pids or interval_seconds <= 0:
        return {}
    first_kids = _children_map(proc_root)
    first = {pid: tree_write_bytes(pid, first_kids, proc_root) for pid in pids}
    sleep(interval_seconds)
    second_kids = _children_map(proc_root)
    second = {pid: tree_write_bytes(pid, second_kids, proc_root) for pid in pids}
    return {
        pid: max(0.0, (second[pid] - first[pid]) / interval_seconds) for pid in pids
    }


def decide(
    snapshot: ProcessSnapshot,
    *,
    caller_uid: int,
    workspace: Path,
    bounds: Bounds,
    write_bytes_per_second: float | None = None,
) -> Decision:
    reasons: list[str] = []
    if snapshot.uid != caller_uid:
        reasons.append(f"uid {snapshot.uid} != caller uid {caller_uid}")
    if snapshot.ppid != 1:
        reasons.append(f"ppid {snapshot.ppid} != 1")
    if snapshot.state == "Z":
        reasons.append("zombie has no executable work to reap")
    if not _agent_scope(caller_uid, snapshot.cgroup):
        reasons.append("not in this uid's 3pai per-agent scope")
    kind = _process_kind(snapshot, workspace)
    if kind is None:
        reasons.append("not an allowed workspace Hermit run or Cargo test executable")
    if snapshot.age_seconds < bounds.min_age_seconds:
        reasons.append(
            f"age {snapshot.age_seconds:.1f}s < {bounds.min_age_seconds:.1f}s"
        )
    # BUSY is now a disjunction: burning CPU, or writing to disk. Age still
    # applies to both -- this widens WHAT counts as damage, not how new a
    # process may be.
    #
    # The CPU thresholds are deliberately UNCHANGED. Lowering 0.80 far enough to
    # catch a 0.003 orphan would match every idle process on the box; the fix is
    # a second quantity, not a looser bound on the wrong one.
    cpu_busy = (
        snapshot.cpu_seconds >= bounds.min_cpu_seconds
        and snapshot.core_ratio >= bounds.min_core_ratio
    )
    disk_busy = (
        write_bytes_per_second is not None
        and write_bytes_per_second >= bounds.min_write_bytes_per_second
    )
    if not cpu_busy and not disk_busy:
        # Same two messages as before, so the CPU-only path reads identically.
        if snapshot.cpu_seconds < bounds.min_cpu_seconds:
            reasons.append(
                f"cpu {snapshot.cpu_seconds:.1f}s < {bounds.min_cpu_seconds:.1f}s"
            )
        if snapshot.core_ratio < bounds.min_core_ratio:
            reasons.append(
                f"cpu/age {snapshot.core_ratio:.3f} < {bounds.min_core_ratio:.3f}"
            )
        # Say WHY the disk path did not rescue it, distinguishing "measured and
        # quiet" from "never measured". Silence about an unmeasured criterion is
        # how a gate comes to look like coverage it does not have.
        if write_bytes_per_second is None:
            reasons.append("disk write rate not measured")
        else:
            reasons.append(
                f"write {write_bytes_per_second / MIB:.2f} MiB/s < "
                f"{bounds.min_write_bytes_per_second / MIB:.2f} MiB/s"
            )
    return Decision(
        snapshot=snapshot,
        eligible=not reasons,
        reasons=tuple(reasons),
        kind=kind,
        write_bytes_per_second=write_bytes_per_second,
    )


def same_identity(left: ProcessSnapshot, right: ProcessSnapshot) -> bool:
    """Facts which must not change between report and pidfd signal."""
    return (
        left.pid,
        left.start_ticks,
        left.uid,
        left.ppid,
        left.exe,
        left.cgroup,
        left.argv,
    ) == (
        right.pid,
        right.start_ticks,
        right.uid,
        right.ppid,
        right.exe,
        right.cgroup,
        right.argv,
    )


def _wait_pidfd(pidfd: int, seconds: float) -> bool:
    poller = select.poll()
    poller.register(pidfd, select.POLLIN)
    return bool(poller.poll(max(0, round(seconds * 1000))))


def reap(
    candidate: Decision,
    *,
    reader: ProcReader,
    caller_uid: int,
    workspace: Path,
    bounds: Bounds,
    grace_seconds: float,
    write_bytes_per_second: float | None = None,
    pidfd_open: Callable[[int], int] = os.pidfd_open,
    pidfd_send: Callable[[int, signal.Signals], None] = signal.pidfd_send_signal,
    wait_pidfd: Callable[[int, float], bool] = _wait_pidfd,
) -> ReapResult:
    """TERM then, if necessary, KILL one revalidated process via pidfd."""
    if not candidate.eligible:
        return ReapResult(candidate.snapshot.pid, "refused-ineligible", False, False)
    pid = candidate.snapshot.pid
    try:
        pidfd = pidfd_open(pid)
    except (AttributeError, OSError) as exc:
        return ReapResult(pid, f"refused-pidfd-unavailable:{exc}", False, False)
    try:
        try:
            current = reader.read(pid)
        except ProcReadError:
            return ReapResult(pid, "already-exited", False, False)
        # The ORIGINAL measurement is reused here, deliberately. These
        # revalidations exist to catch an identity change -- uid, ppid, exe,
        # start time -- between report and signal. Re-sampling the disk rate
        # would instead re-litigate the qualifying criterion, and a writer that
        # happened to pause for the sample window would escape mid-reap.
        current_decision = decide(
            current,
            caller_uid=caller_uid,
            workspace=workspace,
            bounds=bounds,
            write_bytes_per_second=write_bytes_per_second,
        )
        if not current_decision.eligible or not same_identity(candidate.snapshot, current):
            return ReapResult(pid, "refused-identity-changed-before-term", False, False)
        try:
            pidfd_send(pidfd, signal.SIGTERM)
        except ProcessLookupError:
            return ReapResult(pid, "already-exited", False, False)
        if wait_pidfd(pidfd, grace_seconds):
            return ReapResult(pid, "terminated", True, False)
        try:
            after_term = reader.read(pid)
        except ProcReadError:
            return ReapResult(pid, "terminated", True, False)
        after_decision = decide(
            after_term,
            caller_uid=caller_uid,
            workspace=workspace,
            bounds=bounds,
            write_bytes_per_second=write_bytes_per_second,
        )
        if not after_decision.eligible or not same_identity(candidate.snapshot, after_term):
            return ReapResult(pid, "refused-identity-changed-before-kill", True, False)
        try:
            pidfd_send(pidfd, signal.SIGKILL)
        except ProcessLookupError:
            return ReapResult(pid, "terminated", True, False)
        return ReapResult(pid, "killed-after-grace", True, True)
    finally:
        os.close(pidfd)


def summary_line(
    candidates: list[Decision],
    *,
    scanned: int,
    disk_sampled: int,
    disk_criterion: str,
    scan_errors: int,
    detection: bool = False,
    vanished: int = 0,
) -> str:
    """The one line tick-hub interpolates into ``{summary}``.

    THIS MUST ALWAYS RENDER, on every path -- zero candidates, refusals, scan
    errors, --no-disk-criterion. tick_hub.engine.parse_kv_lines splits each
    stdout line on its FIRST '=', and render_emit REFUSES the whole emission if
    any {placeholder} is left unresolved. Two gates were silenced that way on
    2026-08-19: the reverie pin gate for a full day, and the headroom monitor.

    So it is emitted as its own line with a bare ``summary=`` prefix. The
    reaper's human header is NOT usable for this -- "orphaned-hermit-reaper:
    mode=dry-run candidates=2" parses to the key "orphaned-hermit-reaper: mode",
    and no ``summary`` key would ever exist.
    """
    # An incomplete list must SAY it is incomplete, in the one field an operator
    # is guaranteed to read. Appended to every branch below, not just the empty
    # one -- "1 orphan found" and "1 orphan found, 1 unreadable" are different
    # statements and only the second is honest.
    incomplete = (
        f" -- WARNING {scan_errors} live process(es) UNREADABLE, list is INCOMPLETE"
        if scan_errors
        else ""
    )
    if not candidates:
        return (
            f"no eligible orphans (scanned={scanned}, disk_sampled={disk_sampled}, "
            f"disk_criterion={disk_criterion}, unreadable={scan_errors}, "
            f"vanished={vanished})" + incomplete
        )
    if detection:
        parts = []
        for decision in candidates[:5]:
            item = decision.snapshot
            rate = decision.write_bytes_per_second
            written = "unmeasured" if rate is None else f"{rate / MIB:.2f}MiB/s"
            parts.append(
                f"pid {item.pid} ({decision.kind or '?'}, orphaned {item.age_seconds / 60:.0f}m, "
                f"ratio {item.core_ratio:.3f}, write {written}, {item.exe.name})"
            )
        more = "" if len(candidates) <= 5 else f" and {len(candidates) - 5} more"
        return (
            f"{len(candidates)} orphaned run(s) reparented to PID 1 in this agent scope: "
            + "; ".join(parts)
            + more
            + incomplete
            # The relaxation is EXPLICIT and must be, because the kill path
            # rightly refuses an orphan with no proof of damage -- which is most
            # of them, and was pid 3569148 at ratio 0.000 and 0.00 MiB/s. An
            # earlier version of this line told the operator to run plain
            # `--apply`, which refuses; following my own instruction is how I
            # found that. Naming the three bounds keeps the act auditable while
            # every identity gate -- uid, ppid==1, agent scope, workspace exe,
            # kind, age -- still applies untouched.
            + (
                " -- reap ONE with: --pid <pid> --apply --confirm "
                + CONFIRMATION
                + " --min-cpu-seconds 0 --min-core-ratio 0 --min-write-mib-per-second 0"
            )
        )
    parts = []
    for decision in candidates[:5]:
        item = decision.snapshot
        rate = decision.write_bytes_per_second
        written = "unmeasured" if rate is None else f"{rate / MIB:.2f}MiB/s"
        parts.append(
            f"pid {item.pid} ({decision.kind or '?'}, age {item.age_seconds / 60:.0f}m, "
            f"ratio {item.core_ratio:.3f}, write {written})"
        )
    more = "" if len(candidates) <= 5 else f" and {len(candidates) - 5} more"
    return f"{len(candidates)} eligible orphan(s): " + "; ".join(parts) + more + incomplete


def _snapshot_record(decision: Decision) -> dict[str, object]:
    item = decision.snapshot
    return {
        "pid": item.pid,
        "eligible": decision.eligible,
        "kind": decision.kind,
        "reasons": decision.reasons,
        "uid": item.uid,
        "ppid": item.ppid,
        "state": item.state,
        "start_ticks": item.start_ticks,
        "age_seconds": round(item.age_seconds, 3),
        "cpu_seconds": round(item.cpu_seconds, 3),
        "cpu_age_ratio": round(item.core_ratio, 6),
        # null distinguishes NOT MEASURED from measured-and-zero.
        "write_bytes_per_second": (
            None
            if decision.write_bytes_per_second is None
            else round(decision.write_bytes_per_second, 1)
        ),
        "exe": str(item.exe),
        "argv": item.argv,
        "cgroup": item.cgroup,
    }


class _SummaryParser(argparse.ArgumentParser):
    """An ArgumentParser that still emits a ``summary=`` line when it refuses.

    tick_hub.probes runs a gate with ``bash -c`` and captures STDOUT ONLY --
    stderr is read into a discarded local. argparse writes its refusal to stderr
    and exits 2, so a malformed invocation would produce an empty stdout, no
    ``summary`` key, an unresolved ``{summary}`` placeholder, and a REFUSED
    emission. The gate would then be silent in exactly the situation where
    something is wrong with it.

    Two gates died that way on 2026-08-19. A detection that cannot report is the
    same defect as no detection, so the summary contract is honoured on the
    refusal path too.
    """

    def error(self, message: str) -> None:  # type: ignore[override]
        print(f"summary=refused: {message}")
        sys.stdout.flush()
        super().error(message)


def _parser() -> argparse.ArgumentParser:
    parser = _SummaryParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="signal eligible PIDs")
    parser.add_argument(
        "--confirm",
        metavar="TEXT",
        help=f"required with --apply; must equal {CONFIRMATION!r}",
    )
    parser.add_argument("--pid", action="append", type=int, help="inspect only this PID; repeatable")
    parser.add_argument("--min-age-seconds", type=float, default=DEFAULT_MIN_AGE_SECONDS)
    parser.add_argument("--min-cpu-seconds", type=float, default=DEFAULT_MIN_CPU_SECONDS)
    parser.add_argument("--min-core-ratio", type=float, default=DEFAULT_MIN_CORE_RATIO)
    parser.add_argument(
        "--min-write-mib-per-second",
        type=float,
        default=DEFAULT_MIN_WRITE_BYTES_PER_SECOND / MIB,
        help="tree write rate that qualifies a low-CPU orphan (default %(default).2f)",
    )
    parser.add_argument(
        "--disk-sample-seconds",
        type=float,
        default=DEFAULT_DISK_SAMPLE_SECONDS,
        help="width of the two-sample write-rate window (default %(default).1f)",
    )
    parser.add_argument(
        "--report-orphans",
        action="store_true",
        help=(
            "DETECTION mode: report every orphaned workspace Hermit/test process "
            "past --min-age-seconds, regardless of how busy it is. Never signals."
        ),
    )
    parser.add_argument(
        "--no-disk-criterion",
        action="store_true",
        help="skip the write-rate sample; CPU gates only, as before this criterion existed",
    )
    parser.add_argument("--grace-seconds", type=float, default=DEFAULT_GRACE_SECONDS)
    parser.add_argument("--json", action="store_true", help="emit one JSON document")
    parser.add_argument(
        "--verbose-refusals", action="store_true", help="include inspected non-candidates"
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    """Entry point. Guarantees a ``summary=`` line on stdout on EVERY exit path.

    See _SummaryParser for why: the tick-hub gate runner captures stdout only,
    and an emission whose {summary} cannot be resolved is refused outright. A
    crash that produced a bare traceback on stderr would therefore read as
    silence, which is indistinguishable from "no orphans found".
    """
    try:
        return _main(argv)
    except SystemExit:
        # argparse and explicit exits already honoured the contract.
        raise
    except BaseException as exc:  # noqa: BLE001 - the whole point is to catch everything
        print(f"summary=crashed: {type(exc).__name__}: {exc}")
        sys.stdout.flush()
        traceback.print_exc(file=sys.stderr)
        return 5


def _main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    numeric = (
        args.min_age_seconds,
        args.min_cpu_seconds,
        args.min_core_ratio,
        args.grace_seconds,
        args.min_write_mib_per_second,
        args.disk_sample_seconds,
    )
    if any(value < 0 for value in numeric) or args.min_core_ratio > 1.0:
        _parser().error("bounds must be nonnegative and --min-core-ratio must be <= 1")
    if args.apply and args.confirm != CONFIRMATION:
        _parser().error(f"--apply requires --confirm {CONFIRMATION}")
    if args.apply and args.report_orphans:
        # These modes disagree about what "eligible" means. Detection reports
        # everything orphaned; the kill path deliberately demands proof of
        # damage. Letting --apply run against the detection set would signal
        # processes the strict gates exist to protect.
        _parser().error("--report-orphans is a reporting mode and cannot be combined with --apply")
    if args.apply and not args.pid:
        _parser().error("--apply requires at least one explicit --pid from a prior dry-run")

    workspace = Path(__file__).resolve().parent.parent
    reader = ProcReader()
    caller_uid = os.getuid()
    bounds = Bounds(
        args.min_age_seconds,
        args.min_cpu_seconds,
        args.min_core_ratio,
        args.min_write_mib_per_second * MIB,
    )
    requested = sorted(set(args.pid)) if args.pid else sorted(reader.pids())
    decisions: list[Decision] = []
    scan_errors: list[dict[str, object]] = []

    # PHASE 1 -- read every requested process once.
    #
    # A FAILED READ IS A THIRD OUTCOME, not a silence. The earlier version
    # recorded scan errors only when a PID was named explicitly, on the reasoning
    # that races are normal in a /proc scan. That reasoning is right about races
    # and wrong about everything else, and it conflated two different things
    # under one silence: "this process went away mid-read" and "this process is
    # ALIVE and I could not classify it".
    #
    # Found on a live box on 2026-08-20: pid 3089765, a hermit run orphaned for
    # 6h51m, appeared in NEITHER the candidate nor the refused list. Its exe was
    # `.../ignored/pdeathsig/target/debug/hermit (deleted)`, and the reader fails
    # closed on a deleted executable because it can no longer verify the path
    # lies under the workspace. The full scan reported `scanned=183 candidates=1`
    # and dropped it without a word. A report-only tool's safety argument rests
    # entirely on the list being COMPLETE, so an incomplete list that cannot say
    # it is incomplete is worse than no list.
    #
    # The discriminator is whether the process still exists after the failure:
    # gone means a benign race, still there means we genuinely could not tell.
    snapshots: dict[int, ProcessSnapshot] = {}
    vanished = 0
    foreign = 0
    for pid in requested:
        try:
            snapshots[pid] = reader.read(pid)
        except ProcReadError as exc:
            if args.pid:
                # An explicitly named PID always reports, whatever went wrong.
                scan_errors.append({"pid": pid, "error": str(exc)})
            elif not (reader.proc_root / str(pid)).exists():
                vanished += 1  # benign race: it went away mid-read
            elif _readable_uid(reader.proc_root, pid) not in (None, caller_uid):
                # Another user's process. /proc/N/exe is Permission denied for
                # these and the uid gate would refuse them anyway, so they are
                # OUT OF SCOPE, not unclassifiable.
                #
                # This distinction is the whole difference between a useful
                # warning and noise. A first cut recorded every failed read and
                # reported 3403 unreadable processes on this box -- almost all
                # of them root-owned kernel threads -- with the summary shouting
                # "list is INCOMPLETE" on every tick. A warning that always
                # fires teaches the reader to skip it, which is the same defect
                # as staying silent, only louder.
                foreign += 1
            elif not _maybe_in_workspace(reader.proc_root, pid, workspace):
                # Ours, unreadable, but its executable path -- still legible even
                # when the file is gone -- lies outside this workspace, so the
                # workspace gate would refuse it regardless. Out of scope.
                #
                # Narrowing ONLY ever excludes here. The path from a deleted exe
                # is never used to qualify anything, and read() still fails
                # closed on it, so the kill path is untouched. Without this the
                # warning listed 53 processes on this box, almost all of them
                # agent binaries replaced on disk by an upgrade, which is noise
                # wearing the costume of a warning.
                foreign += 1
            else:
                # Ours, still alive, plausibly in scope, and we could not
                # classify it. THIS is the case worth a warning: pid 3089765 on
                # 2026-08-20, a hermit run orphaned 6h51m whose exe read
                # `.../ignored/pdeathsig/target/debug/hermit (deleted)`.
                scan_errors.append({"pid": pid, "error": str(exc)})

    # PHASE 2 -- measure the write rate, but ONLY for processes that already
    # clear every other gate.  The sample costs a sleep, and on a box with
    # thousands of PIDs it must not be paid per process.  Zeroing just the two
    # CPU bounds makes "eligible" mean "passes identity, scope, kind and age",
    # which is exactly the set worth sampling.
    # Identity, scope, kind, ppid==1 and age, with the two CPU bounds zeroed.
    # Used twice: to choose what is worth sampling, and -- in --report-orphans --
    # as the judging bounds themselves.
    safety_only = dataclasses.replace(bounds, min_cpu_seconds=0.0, min_core_ratio=0.0)
    rates: dict[int, float] = {}
    if not args.no_disk_criterion:
        worth_sampling = [
            pid
            for pid, snap in snapshots.items()
            if decide(
                snap, caller_uid=caller_uid, workspace=workspace, bounds=safety_only
            ).eligible
        ]
        rates = sample_write_rates(
            worth_sampling, interval_seconds=args.disk_sample_seconds
        )

    # PHASE 3 -- decide for real, with the rate where one was measured.
    #
    # DETECTION MODE uses safety_only, and that is the whole point of it.
    # ORPHANHOOD IS THE SIGNAL. An agent's Hermit run reparented to PID 1 inside
    # that agent's own scope is always wrong, whatever it is doing -- the three
    # orphans that contaminated measurements on 2026-08-19 sat at core ratios of
    # 0.003 to 0.14, and one of them was writing roughly four bytes a second to a
    # log. No busy-ness threshold, CPU or disk, would have reported that one:
    # a 1 MiB/s disk gate misses it by six orders of magnitude. Asking "is this
    # orphaned and old" needs no threshold and would have caught all three.
    #
    # The kill path keeps its strict gates. Detecting is not the same act as
    # signalling, and only one of them has to be sure.
    judging_bounds = safety_only if args.report_orphans else bounds
    for pid in requested:
        snapshot = snapshots.get(pid)
        if snapshot is None:
            continue
        decision = decide(
            snapshot,
            caller_uid=caller_uid,
            workspace=workspace,
            bounds=judging_bounds,
            write_bytes_per_second=rates.get(pid),
        )
        # Explicit PID inspection always reports a refusal.  In apply mode an
        # unreadable or ineligible member of the requested set blocks the
        # entire operation instead of silently producing a partial sweep.
        if decision.eligible or args.verbose_refusals or args.pid:
            decisions.append(decision)

    candidates = [decision for decision in decisions if decision.eligible]
    explicit_refusals = [decision for decision in decisions if not decision.eligible]
    results: list[ReapResult] = []
    if args.apply and not scan_errors and not explicit_refusals:
        for candidate in candidates:
            results.append(
                reap(
                    candidate,
                    reader=reader,
                    caller_uid=caller_uid,
                    workspace=workspace,
                    bounds=bounds,
                    grace_seconds=args.grace_seconds,
                    write_bytes_per_second=rates.get(candidate.snapshot.pid),
                )
            )

    payload = {
        "schema": "orphaned-hermit-reaper/v1",
        "observed_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "mode": (
            "apply" if args.apply else ("report-orphans" if args.report_orphans else "dry-run")
        ),
        "workspace": str(workspace),
        "caller_uid": caller_uid,
        "bounds": dataclasses.asdict(bounds),
        "candidate_count": len(candidates),
        "scanned": len(snapshots),
        "disk_criterion": "off" if args.no_disk_criterion else "on",
        "disk_sampled": len(rates),
        "unreadable": len(scan_errors),
        "vanished_mid_scan": vanished,
        "foreign_uid_skipped": foreign,
        "summary": summary_line(
            candidates,
            scanned=len(snapshots),
            disk_sampled=len(rates),
            disk_criterion="off" if args.no_disk_criterion else "on",
            scan_errors=len(scan_errors),
            detection=args.report_orphans,
            vanished=vanished,
        ),
        "processes": [_snapshot_record(decision) for decision in decisions],
        "scan_errors": scan_errors,
        "results": [dataclasses.asdict(result) for result in results],
    }
    if args.json:
        print(json.dumps(payload, sort_keys=True))
    else:
        # EVERY field here resolves on EVERY path -- zero candidates, refusals,
        # scan errors, --no-disk-criterion. tick-hub refuses to emit a reminder
        # whose {summary} still contains an unresolved placeholder, and that
        # silenced two gates on 2026-08-19 (the reverie pin gate for a full day,
        # and the headroom monitor). A detection that cannot render is a
        # detection that does not exist.
        # Own line, bare key, first. See summary_line().
        print(f"summary={payload['summary']}")
        print(
            f"orphaned-hermit-reaper: mode={payload['mode']} "
            f"candidates={len(candidates)} scanned={payload['scanned']} "
            f"disk_criterion={payload['disk_criterion']} "
            f"disk_sampled={payload['disk_sampled']} "
            f"unreadable={len(scan_errors)} vanished={vanished} "
            f"bounds={dataclasses.asdict(bounds)}"
        )
        for decision in decisions:
            item = decision.snapshot
            status = "CANDIDATE" if decision.eligible else "REFUSED"
            print(
                f"{status} pid={item.pid} kind={decision.kind or '-'} "
                f"age={item.age_seconds:.1f}s cpu={item.cpu_seconds:.1f}s "
                f"ratio={item.core_ratio:.3f} "
                f"write={'unmeasured' if decision.write_bytes_per_second is None else f'{decision.write_bytes_per_second / MIB:.2f}MiB/s'} "
                f"exe={item.exe} cgroup={item.cgroup}"
            )
            if decision.reasons:
                print("  reasons: " + "; ".join(decision.reasons))
        # Third named category, on STDOUT beside CANDIDATE and REFUSED. It used
        # to go only to stderr, which the tick-hub gate runner DISCARDS -- so the
        # one line saying "I could not classify a live process" was invisible to
        # the very consumer that needs it.
        for error in scan_errors:
            print(f"UNREADABLE pid={error['pid']} reason={error['error']}")
        for error in scan_errors:
            print(f"ERROR pid={error['pid']}: {error['error']}", file=sys.stderr)
        for result in results:
            print(f"RESULT pid={result.pid} outcome={result.outcome}")

    if scan_errors:
        return 3
    if args.apply and explicit_refusals:
        return 4
    if args.apply and any(
        result.outcome not in {"terminated", "killed-after-grace", "already-exited"}
        for result in results
    ):
        return 4
    return 1 if candidates and not args.apply else 0


if __name__ == "__main__":
    raise SystemExit(main())
