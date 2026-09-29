#!/usr/bin/env python3
"""Every active slot must have a live agent behind it.

WHY THIS EXISTS.  On 2026-08-24 the fleet held 170 slots on disk against 13 live
agents.  `slot_disk_residue.py` was, and is, correct about the aggregate: it was
emitting "ACTIVE SLOT CAP BREACH: 77 active slot(s) against cap 12" every 900
seconds and had been for a long time.  Nobody knew, and the audit that named the
missing piece is fifteen days old:
`ai_docs/2026-08-09-worktree-discipline-audit.md` section C4, "Nothing checks
worktree/agent parity" -- "Rule 6 is a parity predicate (|worktrees| == |active
agents|) and nothing computes it."  This computes it.

WHAT IS DIFFERENT FROM THE CAP CHECK.  The cap is an absolute number (12) that
someone has to keep true by hand as the fleet grows and shrinks.  Parity has no
number in it: the denominator is however many agents are alive right now, so it
stays right without maintenance and it names the specific slots that have
nobody behind them rather than reporting an excess count.

THE FAILURE MODE THIS FILE REFUSES TO HAVE.  A parity check whose denominator
comes up empty computes 0 orphans and reports health.  That is how a monitor
becomes worse than no monitor.  So:

  * the agent census is READ LIVE by default, through
    `residue_sweep.read_live_fleet()`, which already raises rather than return
    an empty identity set;
  * a zero-agent census is a REFUSAL (state=unknown, exit 2), never a pass;
  * `--agent-source snapshot` REFUSES a snapshot older than one tick.  This is
    not hypothetical: `ignored/ci-hub/agent-snapshot.json` was 65.8 hours -- 790
    ticks -- old when this was written, and it is the input the sibling
    `agent_container_lifecycle` reminder consumes;
  * every report, pass or fail, prints `live_agents_examined`, so a reader can
    see the denominator that produced the verdict instead of trusting it;
  * `--agent-source herdr`, which is what tick-hub runs, takes its census from
    `herdr agent list` plus every Claude or Codex process in /proc and its
    working directory.  herdr absent, failing, slow, or answering in a shape
    it cannot classify is a REFUSAL, not zero agents.  An agent process whose
    working directory cannot be read, or that sits in a pane herdr does not
    report live, is counted as UNSEEN, and while anything is unseen no slot is
    called ownerless (it is reported unverified instead).  The comment "THE
    HERDR SOURCE, AND WHY A THIRD ONE EXISTS" below says why this source
    exists and the order in which a slot is judged.

Exit codes follow `slot_disk_residue.py`: 0 clean, 1 actionable, 2 could not be
determined.  A could-not-determine is never a pass.
"""

from __future__ import annotations

import argparse
import datetime as dt
import errno
import json
import os
import shutil
import subprocess
import sys
import time
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))

import residue_sweep  # noqa: E402
import agent_snapshot as agent_snapshot_contract  # noqa: E402
import bounded_capture  # noqa: E402
from herdr_agent_snapshot import STATUS_MAP as HERDR_STATUS_MAP  # noqa: E402
from worktree_registry import (  # noqa: E402
    RegistrySchemaError,
    read_active_wrkslots_registry,
)

ROOT = Path(__file__).resolve().parents[2]
STATE_ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", ROOT)).resolve()
ACTIONABLE = "actionable"
CLEAN = "ok"
UNKNOWN = "unknown"

# One tick.  The staleness bound is deliberately this tight: a census older than
# the interval that produces it describes a fleet that has already moved on.
DEFAULT_MAX_SOURCE_AGE_SECS = 300

# Repositories whose git worktree registrations are counted.  These are counted
# because NOTHING ELSE COUNTS THEM: the slot registry sees
# `worktrees/slots/<slot>`,
# not the git worktrees created inside slots.  Measured 2026-08-24: parent 104,
# hermit 310, reverie 53, liteinst2 19, agent-utils 33.  The 310 in hermit is
# 1.8x the slot count everyone was discussing and was invisible to every gate.
COUNTED_REPOS = (".", "hermit", "reverie", "liteinst2", "agent-utils")
ORC_DB_PATH_HELPER = ROOT / "scripts" / "orc-hermit-msg.py"


class ParityUnavailable(Exception):
    """The verdict could not be determined.  Never rendered as a pass."""


# THE REGISTRY THIS GATE READS, AND WHY IT CHANGED.
#
# ⚠️ THIS GATE USED TO READ worktree-state.json AND REPORTED A FOSSIL. That file
# is written by scripts/release-worktree.rs, which is a LEGACY INTERFACE that
# refuses every invocation in a project configured with .wrkslots.yml -- as this
# one is. The producer was retired by the migration to wrkslots and the readers
# were not moved with it, so the file froze at its last legacy write, 2026-08-28
# 07:40, and every gate reading it has reported that day's fleet ever since.
#
# It was not detectable by validating the bytes: the fossil passes
# scripts/check-worktree-registry.rs perfectly, because it is a well-formed
# document of a schema nobody writes any more. Valid and current are different
# questions and only the first was being asked.
#
# MEASURED 2026-09-03, which is what makes this a defect and not a preference: a
# real reclaim moved the wrkslots registry from 100 rows to 93 and the git
# worktree census from 181 to 178, and this gate's `active_slots` stayed at
# exactly 45 across the whole operation. A number that does not move when the
# thing it names is removed is not measuring that thing.
#
# The live authority is the wrkslots registry, one file per machine shard. Its
# ACTIVE file holds exactly the live rows -- finished ones move to ARCHIVED -- so
# presence in it IS the active status the old schema spelled out in a field.
def read_slot_registry(root: Path) -> dict[str, dict]:
    """Live slot rows from the wrkslots registry, keyed by slot name."""
    try:
        return read_active_wrkslots_registry(root)
    except RegistrySchemaError as exc:
        raise ParityUnavailable(str(exc)) from exc


def slot_directories(root: Path, active: dict[str, dict]) -> dict[str, Path]:
    """Where each slot's checkouts actually are.

    Derived from the row rather than assumed to be worktrees/slots/<name>:
    validation slots live under worktrees/validate/ and would otherwise never be
    found to be occupied, so a live validation run would read as an orphan.
    """
    directories: dict[str, Path] = {}
    for name, row in active.items():
        checkouts = row.get("checkouts")
        paths = []
        if isinstance(checkouts, list):
            for checkout in checkouts:
                if isinstance(checkout, dict) and isinstance(checkout.get("path"), str):
                    paths.append((root / checkout["path"]).resolve())
        if paths:
            # Every checkout of one slot sits under the slot directory.
            directories[name] = Path(os.path.commonpath([str(p) for p in paths]))
        else:
            subdir = "validate" if row.get("slot_type") == "validate" else "slots"
            directories[name] = (root / "worktrees" / subdir / name).resolve()
    return directories


def live_agents_from_fleet(db: Path | None = None) -> set[str]:
    try:
        live, _orphans = residue_sweep.read_live_fleet(db)
    except residue_sweep.SweepUnavailable as exc:
        raise ParityUnavailable(f"live agent census unavailable: {exc}") from exc
    if not live:
        # read_live_fleet already refuses this; belt and braces, because the
        # whole point of the file is that an empty denominator never passes.
        raise ParityUnavailable("live agent census is empty")
    return live


def live_taskgraph_db(explicit: Path | None = None) -> Path:
    """Bind the live census to this running Hermit session's TaskGraph.

    Agent-launched ticks carry ``TG_DB_PATH``.  The durable systemd peer does
    not, so its fallback asks the existing coordinator discovery code which
    database the live Hermit Orc process was started with.  That discovery
    rejects zero or multiple Hermit coordinators instead of choosing one.
    """

    if explicit is not None:
        return explicit
    inherited = (os.environ.get("TG_DB_PATH") or "").strip()
    if inherited:
        return Path(inherited).expanduser()
    try:
        result = subprocess.run(
            (sys.executable, str(ORC_DB_PATH_HELPER), "--print-db-path"),
            capture_output=True,
            text=True,
            check=False,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ParityUnavailable(
            f"cannot derive TaskGraph database from the live Hermit coordinator: {exc}"
        ) from exc
    if result.returncode != 0:
        detail = " ".join((result.stderr or result.stdout).split())
        raise ParityUnavailable(
            "cannot derive TaskGraph database from the live Hermit coordinator: "
            f"{detail or f'helper exited {result.returncode}'}"
        )
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if len(lines) != 1 or not Path(lines[0]).is_absolute():
        raise ParityUnavailable(
            "cannot derive TaskGraph database from the live Hermit coordinator: "
            f"expected one absolute path, got {lines!r}"
        )
    return Path(lines[0])


def live_agents_from_snapshot(path: Path, max_age_secs: int, now: float) -> set[str]:
    """Read the same live/terminal decision as every Rust snapshot consumer.

    This used to count every named row as live. Measured against the current
    snapshot, six ``unreachable`` rows inflated the denominator from 12 to 18
    and hid the unowned ``cloudlint`` and ``cloudrecover`` slots. The shared
    status contract makes those rows terminal here just as they already were
    to allocation, release, container reconciliation, and active-work.
    """
    try:
        doc = agent_snapshot_contract.parse_snapshot_bytes(path.read_bytes())
        agent_snapshot_contract.require_canonical_authority(doc, path)
    except (OSError, agent_snapshot_contract.AgentSnapshotSchemaError) as exc:
        raise ParityUnavailable(f"cannot read agent snapshot {path}: {exc}") from exc
    captured_at = float(doc["captured_at"])
    age = now - float(captured_at)
    if age < 0:
        raise ParityUnavailable(
            f"agent snapshot {path} is from the future by {-age:.1f}s"
        )
    if age > max_age_secs:
        raise ParityUnavailable(
            f"agent snapshot {path} is {age / 60:.1f} min old, older than the "
            f"{max_age_secs / 60:.1f} min bound ({age / max_age_secs:.0f} ticks); "
            "refusing to judge parity against a fleet that has moved on"
        )
    agents = doc["agents"]
    try:
        names = {
            a["name"]
            for a in agents
            if agent_snapshot_contract.status_semantics(a["status"]).live
        }
    except agent_snapshot_contract.AgentSnapshotSchemaError as exc:
        raise ParityUnavailable(
            f"cannot classify agent snapshot status from {path}: {exc}"
        ) from exc
    if not names:
        raise ParityUnavailable(f"{path} yielded no live agent identities")
    return names


# THE HERDR SOURCE, AND WHY A THIRD ONE EXISTS.
#
# Measured on devbig014 on 2026-09-28, both older sources refuse on every tick:
#
#   * `--agent-source live` reads tmux through scripts/orc-hermit-msg.py and
#     stops at "tmux socket does not exist"; the fleet now runs in Herdr.
#   * `--agent-source snapshot` reads ignored/ci-hub/agent-snapshot.json, whose
#     producer (hermit-herdr-agent-snapshot.service) exits 75 because
#     ~/.local/bin/agentctl no longer exists.  The file was 5676 minutes old.
#     The same path also spent the tick's 30 s budget: every call into
#     ci-hub/lib/agent_snapshot.py runs scripts/check-agent-snapshot.rs under
#     rust-script, which compiles it again for each new tool-tree path the
#     health tick materialises (28.3 s cold, 0.12 s warm).  The timeout was the
#     compiler, not the census.
#
# This source reads what exists on the host now: `herdr agent list` for the
# panes and their names, and /proc for every process, which finds the Claude and
# Codex processes started outside Herdr and says where each process is working.
# It never starts rust-script.  The Herdr status meanings are mirrored below and
# a test compares every one of them with the Rust contract.
#
# WHAT IT REFUSES TO DO.  An unavailable Herdr is NOT an empty fleet: a missing
# binary, a nonzero exit, a timeout, output that is not the documented shape,
# and a status nobody has classified all refuse (exit 2).  A process whose
# working directory cannot be read is counted as UNSEEN and named, and while any
# agent is unseen no slot is called ownerless -- it is reported as unverified,
# because the agent we could not see may be the one working in it.
#
# HOW A SLOT IS JUDGED, in this order; the first that holds wins:
#
#   1. owner    -- the registry row's owner process (host, boot, pid, start
#                  time) is alive AND the row's heartbeat is within its TTL.
#                  wrkslots records the owner at allocation, and it is the only
#                  test that also covers a workflow subagent, which has no
#                  process, pane, or working directory of its own.
#   2. agent_cwd   -- a live agent's working directory is inside the slot.
#   3. process_cwd -- any process's working directory is inside the slot.
#   4. name     -- the row's agent is the name of a LIVE Herdr pane, and the
#                  owner is not proven dead (or it is, but the heartbeat is
#                  within its TTL).  A pane Herdr reports dead, exited,
#                  stopped, or errored is not live and its name does not count.
#
# AN OWNER THAT IS ALIVE DOES NOT CLEAR A SLOT BY ITSELF.  Measured 2026-09-29
# on devbig014: 52 slots had a live owner, 27 of them held by one codex process
# and 20 by one claude process; 46 had a heartbeat older than the 3600 s
# wrkslots TTL and 50 had no process inside.  A lane that allocates slots and
# never releases them keeps its pid alive, so "owner alive" alone reported a
# host whose only problem was hoarding as "parity holds".  A slot whose owner
# is alive, whose heartbeat is past its TTL, and with no process inside is
# IDLE PAST TTL: actionable, and reported with the owner's pid, program, and
# herdr pane, plus every live owner's slot count.
#
# A slot none of the tests cover is OWNERLESS only when its owner is PROVEN
# dead: another boot, a pid that no longer exists, or a pid now running a
# different process.  A dead owner is not revived by a live pane that merely
# carries the same agent name: short pane names such as "kvm" and "liteinst"
# are reused by each new lane, and on 2026-09-29 they kept two slots whose
# owners died with the previous boot.  Such a slot is ownerless, and counted
# under ownerless_despite_live_pane_name.
#
# A row with NO owner is judged by its coordinator lease, which wrkslots writes
# on every row (see lease_state): a lease from a previous boot of this host,
# with the last heartbeat also before that boot, and no process or live agent
# name behind the slot, is proof of death.  Any other row this census cannot
# settle -- an owner from another host, an unreadable boot id or machine id, a
# lease from this boot -- is UNVERIFIED, which is exit 2 rather than a pass or a
# breach.
AGENT_PROGRAMS = frozenset({"claude", "codex"})
# Agent programs this census does NOT count, reported so the claude/codex-only
# limit is visible in the output instead of silent.  Both names were running on
# devbig014 on 2026-09-29 (`muse serve`, two `agentterm -s` sessions).  A
# process of any program still counts as "inside" a slot through process_cwd;
# what these miss is the denominator and the agent-cwd match.
UNCOUNTED_AGENT_PROGRAMS = frozenset({"muse", "muse.real", "agentterm"})
AGENT_CENSUS_LIMIT = (
    "counts only processes named claude or codex as agents; any other agent "
    "program is not in live_agents_examined (processes of every program still "
    "count as inside a slot)"
)
HERDR_TIMEOUT_SECS = 10.0
MACHINE_ID_PATH = Path("/etc/machine-id")
# The statuses for which `AgentStatus::is_live` in scripts/agent_snapshot.rs
# returns true.  Mirrored rather than asked for at run time, because asking
# compiles a Rust program inside the tick's budget.
LIVE_SNAPSHOT_STATUSES = frozenset(
    {
        "active",
        "busy",
        "free",
        "idle",
        "in_progress",
        "managed",
        "needs-input",
        "needs-permission",
        "running",
        "spawning",
        "working",
    }
)
OWNER_ALIVE = "alive"
OWNER_DEAD = "dead"
# No owner recorded, but the coordinator lease proves the slot's user is gone.
# Kept apart from OWNER_DEAD because the proof is inferred, so a live pane with
# the row's agent name withholds it (see evaluate_herdr).
OWNER_DEAD_BY_LEASE = "dead-by-lease"
OWNER_UNVERIFIABLE = "unverifiable"
HEARTBEAT_FRESH = "fresh"
HEARTBEAT_STALE = "stale"
HEARTBEAT_UNREADABLE = "unreadable"


@dataclass(frozen=True)
class HerdrPane:
    pane_id: str
    name: str | None
    agent: str
    raw_status: str
    live: bool
    cwds: tuple[Path, ...]


@dataclass(frozen=True)
class ProcessRecord:
    pid: int
    ppid: int
    uid: int | None
    comm: str | None
    argv0: str
    cwd: Path | None
    cwd_error: str | None
    pane: str | None = None
    environ_error: str | None = None

    @property
    def agent_like(self) -> bool:
        return self.comm in AGENT_PROGRAMS or self.argv0 in AGENT_PROGRAMS


@dataclass(frozen=True)
class ProcessCensus:
    processes: dict[int, ProcessRecord]
    scanned: int
    vanished_or_zombie: int
    kernel_threads: int


def _unique_pairs(items: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in items:
        if key in result:
            raise ParityUnavailable(f"herdr agent list repeats the JSON field {key!r}")
        result[key] = value
    return result


def parse_herdr_agent_list(raw: str) -> list[HerdrPane]:
    """The panes in one `herdr agent list` document, or a refusal.

    Every row must carry a pane id, an agent kind, and a status the shared map
    classifies.  An unknown status refuses instead of being guessed, because a
    guess in either direction moves a slot between owned and ownerless.
    """
    try:
        document = json.loads(raw, object_pairs_hook=_unique_pairs)
    except json.JSONDecodeError as exc:
        raise ParityUnavailable(f"herdr agent list did not print JSON: {exc}") from exc
    result = document.get("result") if isinstance(document, dict) else None
    if not isinstance(result, dict) or result.get("type") != "agent_list":
        raise ParityUnavailable(
            "herdr agent list is not an agent_list document (expected "
            "result.type == 'agent_list')"
        )
    rows = result.get("agents")
    if not isinstance(rows, list):
        raise ParityUnavailable("herdr agent list result.agents is not a list")
    panes: list[HerdrPane] = []
    seen: set[str] = set()
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            raise ParityUnavailable(f"herdr agent list row {index} is not an object")
        pane_id = row.get("pane_id")
        agent = row.get("agent")
        raw_status = row.get("agent_status")
        name = row.get("name")
        if not isinstance(pane_id, str) or not pane_id:
            raise ParityUnavailable(f"herdr agent list row {index} has no pane_id")
        if pane_id in seen:
            raise ParityUnavailable(f"herdr agent list names pane {pane_id} twice")
        seen.add(pane_id)
        if not isinstance(agent, str) or not agent:
            raise ParityUnavailable(f"herdr pane {pane_id} has no agent kind")
        if not isinstance(raw_status, str) or raw_status not in HERDR_STATUS_MAP:
            raise ParityUnavailable(
                f"herdr pane {pane_id} has unclassified agent_status {raw_status!r}; "
                "refusing to guess whether it is live"
            )
        if name is not None and (not isinstance(name, str) or not name):
            raise ParityUnavailable(f"herdr pane {pane_id} has a malformed name")
        cwds: list[Path] = []
        for key in ("cwd", "foreground_cwd"):
            value = row.get(key)
            if isinstance(value, str) and value.startswith("/"):
                path = Path(value)
                if path not in cwds:
                    cwds.append(path)
        panes.append(
            HerdrPane(
                pane_id=pane_id,
                name=name,
                agent=agent,
                raw_status=raw_status,
                live=HERDR_STATUS_MAP[raw_status] in LIVE_SNAPSHOT_STATUSES,
                cwds=tuple(cwds),
            )
        )
    return panes


def herdr_census(herdr_bin: str | None, timeout: float) -> list[HerdrPane]:
    """Run `herdr agent list`.  Any failure is NO-SIGNAL, never zero agents."""
    if not herdr_bin:
        raise ParityUnavailable(
            "herdr is not on PATH, so there is no live agent census; an absent "
            "census is not a census of zero agents"
        )
    try:
        result = bounded_capture.capture((herdr_bin, "agent", "list"), timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        raise ParityUnavailable(
            f"herdr agent list did not answer within {timeout:g} s"
        ) from exc
    except OSError as exc:
        raise ParityUnavailable(f"cannot run {herdr_bin} agent list: {exc}") from exc
    if result.returncode != 0:
        detail = " ".join((result.stderr or result.stdout or "").split())[:300]
        raise ParityUnavailable(
            f"herdr agent list exited {result.returncode}: {detail or 'no output'}"
        )
    return parse_herdr_agent_list(result.stdout)


def _errno_name(exc: OSError) -> str:
    return errno.errorcode.get(exc.errno or 0, str(exc.errno))


def _read_status_uid(base: Path) -> int | None:
    for line in (base / "status").read_text(errors="replace").splitlines():
        if line.startswith("Uid:"):
            fields = line.split()
            if len(fields) > 1 and fields[1].isdigit():
                return int(fields[1])
    return None


def _parse_stat(text: str) -> tuple[str, str, int, str]:
    """(comm, state, ppid, start_ticks) from /proc/<pid>/stat."""
    head, sep, tail = text.rpartition(")")
    if not sep or "(" not in head:
        raise ValueError("malformed stat")
    fields = tail.split()
    return head.partition("(")[2], fields[0], int(fields[1]), fields[19]


def _argv0(base: Path) -> str:
    try:
        argv = (base / "cmdline").read_bytes().split(b"\0")
    except OSError:
        return ""
    return os.path.basename(argv[0].decode(errors="replace")) if argv else ""


def process_census(proc_root: Path, exclude: frozenset[int] = frozenset()) -> ProcessCensus:
    """Every process's uid, identity, parent, working directory, and Herdr pane.

    A process that exits during the scan, and a zombie, are counted rather than
    judged.  A working directory that cannot be read is kept with the reason,
    so the caller can decide whether that process could be an agent it missed.
    """
    try:
        names = [entry.name for entry in os.scandir(proc_root) if entry.name.isdigit()]
    except OSError as exc:
        raise ParityUnavailable(f"cannot enumerate {proc_root}: {exc}") from exc
    processes: dict[int, ProcessRecord] = {}
    vanished = 0
    kernel_threads = 0
    for name in names:
        pid = int(name)
        if pid in exclude:
            continue
        base = proc_root / name
        try:
            comm, state, ppid, _start = _parse_stat((base / "stat").read_text(errors="replace"))
            uid = _read_status_uid(base)
        except (FileNotFoundError, ProcessLookupError):
            vanished += 1
            continue
        except (OSError, ValueError, IndexError) as exc:
            # Kept, never skipped: a process this census cannot identify may be
            # the agent working in a slot, so evaluate_herdr reports it unseen.
            reason = _errno_name(exc) if isinstance(exc, OSError) else "malformed stat"
            processes[pid] = ProcessRecord(
                pid, 0, None, None, _argv0(base), None, f"stat {reason}"
            )
            continue
        if state in ("Z", "X"):
            vanished += 1
            continue
        if pid == 2 or ppid == 2:
            kernel_threads += 1
            continue
        argv0 = _argv0(base)
        cwd: Path | None = None
        cwd_error: str | None = None
        try:
            cwd = Path(os.readlink(base / "cwd"))
        except OSError as exc:
            if not (base / "stat").exists():
                vanished += 1
                continue
            cwd_error = _errno_name(exc)
        pane: str | None = None
        environ_error: str | None = None
        if comm in AGENT_PROGRAMS or argv0 in AGENT_PROGRAMS:
            try:
                for entry in (base / "environ").read_bytes().split(b"\0"):
                    if entry.startswith(b"HERDR_PANE_ID="):
                        pane = entry.partition(b"=")[2].decode(errors="replace") or None
                        break
            except OSError as exc:
                environ_error = _errno_name(exc)
        processes[pid] = ProcessRecord(
            pid, ppid, uid, comm, argv0, cwd, cwd_error, pane, environ_error
        )
    return ProcessCensus(processes, len(names), vanished, kernel_threads)


def _read_text_or_none(path: Path) -> str | None:
    try:
        value = path.read_text(errors="replace").strip()
    except OSError:
        return None
    return value or None


def read_boot_time(proc_root: Path) -> float | None:
    """The epoch second this boot started (`btime` in /proc/stat), or None."""
    try:
        text = (proc_root / "stat").read_text(errors="replace")
    except OSError:
        return None
    for line in text.splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[0] == "btime" and fields[1].isdigit():
            return float(fields[1])
    return None


def heartbeat_epoch(row: dict) -> float | None:
    """The row's `heartbeat_at` as an epoch second, or None if absent or malformed."""
    value = row.get("heartbeat_at")
    if not isinstance(value, str) or not value:
        return None
    try:
        stamp = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if stamp.tzinfo is None:
        return None
    return stamp.timestamp()


def heartbeat_state(row: dict, now: float) -> tuple[str, float | None]:
    """Whether the heartbeat is within the row's own `heartbeat_ttl_seconds`.

    Same predicate as wrkslots' `_heartbeat_diagnosis`: expired when the age is
    greater than the TTL.  A missing or malformed stamp or TTL is UNREADABLE,
    never fresh.
    """
    beat = heartbeat_epoch(row)
    ttl = row.get("heartbeat_ttl_seconds")
    if beat is None or type(ttl) is not int or ttl < 1:
        return HEARTBEAT_UNREADABLE, None
    age = max(0.0, now - beat)
    return (HEARTBEAT_STALE if age > ttl else HEARTBEAT_FRESH), age


def lease_state(
    row: dict, boot_id: str | None, machine_id: str | None, boot_time: float | None
) -> tuple[str, str]:
    """A row with no owner: whether its coordinator lease proves the user is gone.

    wrkslots requires a `coordinator_lease` (host, boot, pid, start time of the
    process that allocated the slot) on every row, including the rows it
    writes with `owner: null`.  On devbig014 on 2026-09-29 there were 32 such
    rows, every lease from the boot before the 2026-09-27T00:55Z reboot, every
    heartbeat before it (the latest 2026-09-25T23:42Z).

    WHY THAT IS PROOF.  Every process of a previous boot is gone: the
    coordinator that leased the slot, and whatever worked in it then.  A
    heartbeat older than the boot says nothing in this boot has renewed the
    row.  The caller also requires that no process is inside the slot and that
    no live herdr pane carries the row's agent name.  What remains is an agent
    of this boot that adopted the slot without registering or heartbeating and
    has no process inside it at this instant; that is the same residual the
    recorded-owner rule already accepts for "owner from a previous boot".

    Anything short of that is unverifiable, with the reason named.
    """
    base = "no owner recorded"
    lease = row.get("coordinator_lease")
    if (
        not isinstance(lease, dict)
        or not isinstance(lease.get("host_id"), str)
        or not isinstance(lease.get("boot_id"), str)
    ):
        return OWNER_UNVERIFIABLE, f"{base}; no usable coordinator lease"
    if machine_id is None:
        return OWNER_UNVERIFIABLE, f"{base}; machine id unreadable"
    if lease["host_id"] != machine_id:
        return OWNER_UNVERIFIABLE, f"{base}; coordinator lease from another host"
    if boot_id is None:
        return OWNER_UNVERIFIABLE, f"{base}; boot id unreadable"
    if lease["boot_id"] == boot_id:
        return OWNER_UNVERIFIABLE, f"{base}; coordinator lease from this boot"
    if boot_time is None:
        return OWNER_UNVERIFIABLE, f"{base}; boot time unreadable"
    beat = heartbeat_epoch(row)
    if beat is None:
        return OWNER_UNVERIFIABLE, f"{base}; heartbeat unreadable"
    if beat >= boot_time:
        return OWNER_UNVERIFIABLE, f"{base}; heartbeat renewed during this boot"
    return (
        OWNER_DEAD_BY_LEASE,
        "no owner; coordinator lease and last heartbeat from a previous boot",
    )


def owner_state(
    row: dict,
    proc_root: Path,
    boot_id: str | None,
    machine_id: str | None,
    boot_time: float | None = None,
) -> tuple[str, str]:
    """Whether the slot's recorded owner process is alive, dead, or uncheckable."""
    owner = row.get("owner")
    if owner is None:
        return lease_state(row, boot_id, machine_id, boot_time)
    if not isinstance(owner, dict):
        return OWNER_UNVERIFIABLE, "malformed owner record"
    host = owner.get("host_id")
    boot = owner.get("boot_id")
    pid = owner.get("pid")
    ticks = owner.get("start_ticks")
    if (
        not isinstance(host, str)
        or not isinstance(boot, str)
        or type(pid) is not int
        or pid <= 0
        or type(ticks) not in (int, str)
    ):
        return OWNER_UNVERIFIABLE, "malformed owner record"
    if machine_id is None:
        return OWNER_UNVERIFIABLE, "machine id unreadable"
    if host != machine_id:
        return OWNER_UNVERIFIABLE, "owner on another host"
    if boot_id is None:
        return OWNER_UNVERIFIABLE, "boot id unreadable"
    if boot != boot_id:
        return OWNER_DEAD, "owner from a previous boot"
    try:
        _comm, state, _ppid, start = _parse_stat(
            (proc_root / str(pid) / "stat").read_text(errors="replace")
        )
    except (FileNotFoundError, ProcessLookupError):
        return OWNER_DEAD, "owner pid gone"
    except (OSError, ValueError, IndexError):
        return OWNER_UNVERIFIABLE, "owner stat unreadable"
    if state in ("Z", "X"):
        return OWNER_DEAD, "owner is a zombie"
    if start != str(ticks):
        return OWNER_DEAD, "owner pid reused"
    return OWNER_ALIVE, ""


def _with_ancestors(paths) -> set[Path]:
    covered: set[Path] = set()
    for path in paths:
        covered.add(path)
        covered.update(path.parents)
    return covered


def _by_reason(entries: list[tuple[str, str]]) -> str:
    counts = Counter(reason for _name, reason in entries)
    return ", ".join(f"{reason}: {count}" for reason, count in sorted(counts.items())) or "none"


def _first(names: list[str], limit: int = 20) -> str:
    shown = ", ".join(names[:limit])
    more = f" (+{len(names) - limit} more)" if len(names) > limit else ""
    return f"{shown}{more}"


def _age_text(seconds: float | None) -> str:
    if seconds is None:
        return "unknown"
    if seconds >= 86400:
        return f"{seconds / 86400:.1f} d"
    if seconds >= 3600:
        return f"{seconds / 3600:.1f} h"
    return f"{seconds / 60:.0f} min"


def evaluate_herdr(
    root: Path,
    active: dict[str, dict],
    panes: list[HerdrPane],
    census: ProcessCensus,
    *,
    proc_root: Path,
    boot_id: str | None,
    machine_id: str | None,
    boot_time: float | None = None,
    now: float | None = None,
) -> tuple[str, str, dict]:
    started = time.monotonic()
    now = time.time() if now is None else now
    procs = census.processes
    panes_by_id = {pane.pane_id: pane for pane in panes}
    live_panes = {pane.pane_id: pane for pane in panes if pane.live}
    stale_panes = {pane.pane_id: pane for pane in panes if not pane.live}

    def has_agent_ancestor(record: ProcessRecord) -> bool:
        seen: set[int] = set()
        pid = record.ppid
        while pid > 1 and pid not in seen:
            seen.add(pid)
            parent = procs.get(pid)
            if parent is None:
                return False
            if parent.agent_like:
                return True
            pid = parent.ppid
        return False

    unseen: list[str] = []
    background: list[ProcessRecord] = []
    outside: list[ProcessRecord] = []
    pane_roots: dict[str, list[ProcessRecord]] = {}
    unreadable_other: list[ProcessRecord] = []
    environ_unreadable: list[ProcessRecord] = []
    uncounted_programs: Counter[str] = Counter()
    for record in sorted(procs.values(), key=lambda r: r.pid):
        if record.comm is None:
            program = f"; argv0 {record.argv0}" if record.argv0 else ""
            unseen.append(f"pid {record.pid} (unidentifiable: {record.cwd_error}{program})")
            continue
        if not record.agent_like:
            if record.cwd is None:
                unreadable_other.append(record)
            if record.comm in UNCOUNTED_AGENT_PROGRAMS or record.argv0 in UNCOUNTED_AGENT_PROGRAMS:
                uncounted_programs[
                    record.comm if record.comm in UNCOUNTED_AGENT_PROGRAMS else record.argv0
                ] += 1
            continue
        if record.cwd is None:
            unseen.append(
                f"pid {record.pid} {record.comm} uid {record.uid} "
                f"(working directory {record.cwd_error})"
            )
        if record.environ_error is not None:
            # Without its environment the process's HERDR_PANE_ID is unknown,
            # so it cannot be matched to its pane or told apart from a
            # background agent.  Unseen, like an unreadable working directory.
            environ_unreadable.append(record)
            unseen.append(
                f"pid {record.pid} {record.comm} uid {record.uid} "
                f"(environment {record.environ_error}, so its herdr pane is unknown)"
            )
        if has_agent_ancestor(record):
            continue
        if record.pane is None:
            background.append(record)
        elif record.pane in live_panes:
            pane_roots.setdefault(record.pane, []).append(record)
        else:
            outside.append(record)
            if record.pane in stale_panes:
                why = f"herdr reports that pane {stale_panes[record.pane].raw_status}"
            else:
                why = "herdr does not list that pane"
            unseen.append(f"pid {record.pid} {record.comm} in pane {record.pane} ({why})")
    for pane in live_panes.values():
        if pane.pane_id not in pane_roots and not pane.cwds:
            unseen.append(
                f"herdr pane {pane.pane_id} {pane.name or '(unnamed)'} "
                f"({pane.raw_status}) has no cwd and no process carries its pane id"
            )

    live_count = len(live_panes) + len(background) + len(outside)
    if live_count == 0:
        raise ParityUnavailable(
            f"no live agent found: herdr listed {len(panes)} pane(s), "
            f"{len(stale_panes)} of them not live, and /proc shows no claude or "
            "codex process outside them"
        )

    live_names = {pane.name for pane in live_panes.values() if pane.name}
    agent_cwds: set[Path] = set()
    for pane in live_panes.values():
        agent_cwds.update(pane.cwds)
    for record in [*background, *outside, *(r for rs in pane_roots.values() for r in rs)]:
        if record.cwd is not None:
            agent_cwds.add(record.cwd)
    # Every directory that contains a working directory, so each slot is one
    # set lookup instead of a scan of ~4000 processes (measured 2026-09-28).
    agent_covered = _with_ancestors(agent_cwds)
    process_covered = _with_ancestors(
        record.cwd for record in procs.values() if record.cwd is not None
    )

    def owner_label(pid: int) -> str:
        record = procs.get(pid)
        parts = [f"pid {pid}"]
        if record is not None and record.comm:
            parts.append(record.comm)
        if record is not None and record.pane:
            pane = panes_by_id.get(record.pane)
            name = f" {pane.name}" if pane is not None and pane.name else ""
            parts.append(f"pane {record.pane}{name}")
        return " ".join(parts)

    directories = slot_directories(root, active)
    matched: dict[str, list[str]] = {
        "owner": [],
        "agent_cwd": [],
        "process_cwd": [],
        "name": [],
    }
    ownerless: list[tuple[str, str]] = []
    ownerless_name_only: list[str] = []
    idle_past_ttl: list[tuple[str, str]] = []
    unverified: list[tuple[str, str]] = []
    # pid -> [slots held, idle past TTL, oldest heartbeat age]
    live_owner_slots: dict[int, list] = {}
    for name, row in sorted(active.items()):
        state, reason = owner_state(row, proc_root, boot_id, machine_id, boot_time)
        beat, age = heartbeat_state(row, now)
        directory = directories[name]
        if directory in agent_covered:
            inside: str | None = "agent_cwd"
        elif directory in process_covered:
            inside = "process_cwd"
        else:
            inside = None
        named = bool(slot_owner_names(row) & live_names)
        if state == OWNER_ALIVE:
            pid = row["owner"]["pid"]
            tally = live_owner_slots.setdefault(pid, [0, 0, None])
            tally[0] += 1
            if age is not None and (tally[2] is None or age > tally[2]):
                tally[2] = age
            if beat == HEARTBEAT_FRESH:
                matched["owner"].append(name)
            elif inside is not None:
                matched[inside].append(name)
            elif beat == HEARTBEAT_STALE:
                # Owner alive is NOT enough: see "AN OWNER THAT IS ALIVE" above.
                tally[1] += 1
                idle_past_ttl.append((name, owner_label(pid)))
            else:
                unverified.append(
                    (name, "owner alive, heartbeat unreadable, nothing inside")
                )
        elif inside is not None:
            matched[inside].append(name)
        elif state in (OWNER_DEAD, OWNER_DEAD_BY_LEASE):
            if named and beat == HEARTBEAT_FRESH:
                matched["name"].append(name)
            elif named and state == OWNER_DEAD_BY_LEASE:
                # The lease proof requires that no live pane carries the name
                # (lease_state); a match withholds it without clearing it.
                unverified.append(
                    (
                        name,
                        "no owner recorded; lease from a previous boot, but a live "
                        "herdr pane carries the agent name",
                    )
                )
            else:
                if named:
                    ownerless_name_only.append(name)
                ownerless.append((name, reason))
        elif named:
            matched["name"].append(name)
        else:
            unverified.append((name, reason))

    withheld = 0
    withheld_idle = 0
    if unseen and (ownerless or idle_past_ttl):
        # An agent we could not see may be the one working in these slots.
        withheld = len(ownerless)
        withheld_idle = len(idle_past_ttl)
        unverified.extend(
            (name, f"{reason} but an agent was unseen") for name, reason in ownerless
        )
        unverified.extend(
            (name, "owner alive, heartbeat past TTL, but an agent was unseen")
            for name, _owner in idle_past_ttl
        )
        ownerless = []
        ownerless_name_only = []
        idle_past_ttl = []
    unverified.sort()
    match_ms = round((time.monotonic() - started) * 1000)

    git_started = time.monotonic()
    counts = git_worktree_counts(root)
    git_ms = round((time.monotonic() - git_started) * 1000)

    same_uid_unreadable = Counter(
        record.comm for record in unreadable_other if record.uid == os.getuid()
    )
    owners_text = "; ".join(
        f"{owner_label(pid)}: {held} held, {idle} idle past TTL "
        f"(oldest heartbeat {_age_text(oldest)})"
        for pid, (held, idle, oldest) in sorted(
            live_owner_slots.items(), key=lambda item: (-item[1][0], item[0])
        )
    ) or "none"
    idle_owners = Counter(owner for _name, owner in idle_past_ttl)
    idle_owner_text = ", ".join(
        f"{owner}: {count}" for owner, count in sorted(
            idle_owners.items(), key=lambda item: (-item[1], item[0])
        )
    )
    ratio_text = f"{len(active)} active slot(s) against {live_count} live agent(s)"
    fields: dict[str, object] = {
        "agent_source": "herdr",
        "live_agents_examined": live_count,
        "active_slots_against_live_agents": ratio_text,
        "agent_census_limit": AGENT_CENSUS_LIMIT,
        "uncounted_agent_program_processes": ", ".join(
            f"{program} x{count}" for program, count in sorted(uncounted_programs.items())
        )
        or "none",
        "herdr_panes_listed": len(panes),
        "herdr_live_panes": len(live_panes),
        "herdr_stale_panes": len(stale_panes),
        "herdr_stale_pane_detail": ", ".join(
            f"{pane.pane_id} {pane.name or '(unnamed)'} {pane.raw_status}"
            for pane in stale_panes.values()
        )
        or "none",
        "background_agents": len(background),
        "background_agent_detail": "; ".join(
            f"pid {r.pid} {r.comm} {r.cwd or '(cwd ' + str(r.cwd_error) + ')'}"
            for r in background
        )
        or "none",
        "agent_processes_outside_live_panes": len(outside),
        "agent_processes_environ_unreadable": len(environ_unreadable),
        "unseen_agents": len(unseen),
        "unseen_agent_detail": "; ".join(unseen[:10]) or "none",
        "active_slots": len(active),
        "live_owners": len(live_owner_slots),
        "slots_by_live_owner": owners_text,
        "matched_by_owner": len(matched["owner"]),
        "matched_by_name": len(matched["name"]),
        "matched_by_agent_cwd": len(matched["agent_cwd"]),
        "matched_by_process_cwd": len(matched["process_cwd"]),
        "matched_by_name_detail": _first(matched["name"]) or "none",
        "matched_by_cwd_detail": _first(matched["agent_cwd"] + matched["process_cwd"])
        or "none",
        "idle_past_ttl": len(idle_past_ttl),
        "idle_past_ttl_by_owner": idle_owner_text or "none",
        "idle_past_ttl_detail": _first([n for n, _ in idle_past_ttl]) or "none",
        "ownerless": len(ownerless),
        "ownerless_by_reason": _by_reason(ownerless),
        "ownerless_despite_live_pane_name": len(ownerless_name_only),
        "ownerless_despite_live_pane_name_detail": _first(ownerless_name_only) or "none",
        "unverified": len(unverified),
        "unverified_by_reason": _by_reason(unverified),
        "ownerless_withheld_for_unseen_agents": withheld,
        "idle_past_ttl_withheld_for_unseen_agents": withheld_idle,
        "processes_scanned": census.scanned,
        "processes_vanished_or_zombie": census.vanished_or_zombie,
        "processes_cwd_unreadable_non_agent": len(unreadable_other),
        "processes_cwd_unreadable_same_uid": ", ".join(
            f"{comm} x{count}" for comm, count in sorted(same_uid_unreadable.items())
        )
        or "none",
        "elapsed_ms_match": match_ms,
        "elapsed_ms_git": git_ms,
    }
    for repo, count in counts.items():
        key = "parent" if repo == "." else repo
        fields[f"git_worktrees_{key}"] = "unreadable" if count is None else count
    fields["git_worktrees_total"] = sum(c for c in counts.values() if isinstance(c, int))

    census_text = (
        f"{live_count} live agent(s) examined ({len(live_panes)} live herdr pane(s), "
        f"{len(background)} background process(es))"
    )
    unverified_text = (
        f"; {len(unverified)} more slot(s) unverified ({_by_reason(unverified)})"
        if unverified
        else ""
    )
    if ownerless or idle_past_ttl:
        parts: list[str] = []
        if ownerless:
            name_only = (
                f" ({len(ownerless_name_only)} of them carry the agent name of a live "
                "herdr pane, which does not revive a dead owner)"
                if ownerless_name_only
                else ""
            )
            parts.append(
                f"{len(ownerless)} have a dead owner ({_by_reason(ownerless)}), no "
                f"process inside, and no live agent behind them{name_only}: "
                f"{_first([n for n, _ in ownerless])}"
            )
        if idle_past_ttl:
            parts.append(
                f"{len(idle_past_ttl)} are held by a live owner with a heartbeat "
                f"older than its TTL and no process inside ({idle_owner_text}): "
                f"{_first([n for n, _ in idle_past_ttl], 10)}"
            )
        summary = (
            f"SLOT/AGENT PARITY BREACH: {ratio_text}; of those, "
            + "; ".join(parts)
            + f"; census {census_text}{unverified_text}"
        )
        return ACTIONABLE, summary, fields
    if unverified:
        unseen_text = (
            f"; {len(unseen)} agent process(es) could not be seen, so {withheld} "
            f"slot(s) with a dead owner and {withheld_idle} idle past TTL are "
            "reported unverified instead"
            if unseen
            else ""
        )
        summary = (
            f"slot/agent parity unconfirmed: {len(unverified)} of {len(active)} active "
            f"slot(s) have no live agent, fresh owner heartbeat, or process behind them "
            f"and an owner this census cannot prove dead ({_by_reason(unverified)}), "
            f"against {census_text}{unseen_text}: {_first([n for n, _ in unverified])}"
        )
        return UNKNOWN, summary, fields
    matched_total = sum(len(names) for names in matched.values())
    if matched_total != len(active):
        raise ParityUnavailable(
            f"internal accounting error: {matched_total} of {len(active)} slot(s) "
            "classified; refusing to report parity"
        )
    summary = (
        f"parity holds: all {matched_total} active slot(s) have a live owner with a "
        f"heartbeat within its TTL, a process inside, or a live agent, against "
        f"{census_text}"
    )
    return CLEAN, summary, fields


def slot_owner_names(record: dict) -> set[str]:
    """The agent names a slot row claims are behind it.

    The wrkslots row names one agent in `agent`; the older schema carried a list
    under `agents`. Both are read so a row of either shape answers the same
    question, and neither is invented when absent.
    """
    names: set[str] = set()
    agent = record.get("agent")
    if isinstance(agent, str) and agent:
        names.add(agent)
    agents = record.get("agents")
    if isinstance(agents, list):
        for entry in agents:
            if isinstance(entry, dict):
                name = entry.get("name")
                if isinstance(name, str) and name:
                    names.add(name)
    return names


def occupied_slots(directories: dict[str, Path]) -> set[str]:
    """Slots with a live same-uid process whose cwd is inside them.

    A process inside the slot is proof of use even when the registry's owner
    field has rotted, so it is checked as well as ownership -- the point is to
    name slots nobody is behind, not slots whose bookkeeping is untidy.
    """

    try:
        cwds = residue_sweep.process_cwds()
    except residue_sweep.SweepUnavailable as exc:
        raise ParityUnavailable(f"cannot enumerate process cwds: {exc}") from exc
    occupied: set[str] = set()
    for name, slot_dir in directories.items():
        for cwd in cwds:
            if cwd == slot_dir or slot_dir in cwd.parents:
                occupied.add(name)
                break
    return occupied


def git_worktree_counts(root: Path) -> dict[str, int | None]:
    """Registered git worktrees per repository.  None where it cannot be read."""

    counts: dict[str, int | None] = {}
    for repo in COUNTED_REPOS:
        path = root / repo if repo != "." else root
        try:
            result = subprocess.run(
                ("git", "-C", str(path), "worktree", "list", "--porcelain"),
                capture_output=True,
                text=True,
                check=False,
                timeout=30,
            )
        except (OSError, subprocess.TimeoutExpired):
            counts[repo] = None
            continue
        if result.returncode != 0:
            counts[repo] = None
            continue
        counts[repo] = sum(
            1 for line in result.stdout.splitlines() if line.startswith("worktree ")
        )
    return counts


def evaluate(root: Path, live: set[str], active: dict[str, dict]) -> tuple[str, str, dict]:
    occupied = occupied_slots(slot_directories(root, active))

    orphaned: list[str] = []
    for name, record in sorted(active.items()):
        owners = slot_owner_names(record)
        if owners & live:
            continue
        if name in occupied:
            continue
        orphaned.append(name)

    counts = git_worktree_counts(root)
    fields: dict[str, object] = {
        "live_agents_examined": len(live),
        "active_slots": len(active),
        "active_slots_occupied": len(occupied),
        "active_slots_without_live_agent": len(orphaned),
    }
    for repo, count in counts.items():
        key = "parent" if repo == "." else repo
        fields[f"git_worktrees_{key}"] = "unreadable" if count is None else count
    total = sum(c for c in counts.values() if isinstance(c, int))
    fields["git_worktrees_total"] = total

    if orphaned:
        shown = ", ".join(orphaned[:20])
        more = f" (+{len(orphaned) - 20} more)" if len(orphaned) > 20 else ""
        summary = (
            f"SLOT/AGENT PARITY BREACH: {len(orphaned)} active slot(s) have no live "
            f"agent and no process inside, against {len(live)} live agent(s) "
            f"examined: {shown}{more}"
        )
        return ACTIONABLE, summary, fields

    summary = (
        f"parity holds: every one of {len(active)} active slot(s) has a live agent "
        f"or a live process, against {len(live)} live agent(s) examined"
    )
    return CLEAN, summary, fields


def run_herdr_source(args: argparse.Namespace, root: Path) -> tuple[str, str, dict]:
    started = time.monotonic()
    herdr_bin = args.herdr_bin or shutil.which("herdr")
    panes = herdr_census(herdr_bin, args.herdr_timeout_secs)
    herdr_ms = round((time.monotonic() - started) * 1000)
    proc_started = time.monotonic()
    census = process_census(args.proc_root, exclude=frozenset({os.getpid()}))
    proc_ms = round((time.monotonic() - proc_started) * 1000)
    active = read_slot_registry(root)
    state, summary, fields = evaluate_herdr(
        root,
        active,
        panes,
        census,
        proc_root=args.proc_root,
        boot_id=_read_text_or_none(args.proc_root / "sys" / "kernel" / "random" / "boot_id"),
        machine_id=_read_text_or_none(args.machine_id_file),
        boot_time=read_boot_time(args.proc_root),
    )
    fields["elapsed_ms_herdr"] = herdr_ms
    fields["elapsed_ms_proc"] = proc_ms
    fields["elapsed_ms_total"] = round((time.monotonic() - started) * 1000)
    return state, summary, fields


def captured_value(value: object) -> str:
    """One inert line for tick-hub's key=value capture.

    tick-hub parses every stdout line containing '=' as a field, so a newline
    in a path or pane name would mint a phantom field; and it renders captured
    values as templates, so braces become placeholders.  Whitespace runs
    collapse to one space and braces become parentheses.
    """
    return " ".join(str(value).split()).replace("{", "(").replace("}", ")")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--gate", action="store_true", help="emit gate key=value output")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--root", default=None, help="repository root (tests)")
    parser.add_argument(
        "--agent-source",
        choices=("live", "snapshot", "herdr"),
        default="live",
        help="live: the orphan detector's tmux fleet (default). snapshot: the "
        "cached agent snapshot, which is refused when stale. herdr: `herdr agent "
        "list` plus a /proc census of Claude and Codex processes and every "
        "process's working directory, read now; a failed or absent herdr "
        "refuses (exit 2).",
    )
    parser.add_argument(
        "--herdr-bin",
        default=None,
        help="herdr executable for --agent-source herdr (default: herdr on PATH)",
    )
    parser.add_argument(
        "--herdr-timeout-secs",
        type=float,
        default=HERDR_TIMEOUT_SECS,
        help=f"seconds `herdr agent list` may take (default {HERDR_TIMEOUT_SECS:g})",
    )
    parser.add_argument(
        "--proc-root",
        type=Path,
        default=Path("/proc"),
        help="process table read by --agent-source herdr (default /proc; tests)",
    )
    parser.add_argument(
        "--machine-id-file",
        type=Path,
        default=MACHINE_ID_PATH,
        help="host identity compared with each slot owner's host_id "
        f"(default {MACHINE_ID_PATH}; tests)",
    )
    parser.add_argument("--agent-snapshot", default=None)
    parser.add_argument(
        "--db",
        type=Path,
        default=None,
        help="TaskGraph database used by the live agent census",
    )
    parser.add_argument(
        "--max-source-age-secs",
        type=int,
        default=int(
            os.environ.get("DEV_HERMIT_MAX_AGENT_SOURCE_AGE_SECS", DEFAULT_MAX_SOURCE_AGE_SECS)
        ),
    )
    parser.add_argument("--test-live-agents", default=None, help="comma list (tests)")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve() if args.root else STATE_ROOT
    state, summary, fields = UNKNOWN, "", {}
    database: Path | None = None
    herdr_source = args.test_live_agents is None and args.agent_source == "herdr"
    try:
        if herdr_source:
            state, summary, fields = run_herdr_source(args, root)
        else:
            if args.test_live_agents is not None:
                live = {n for n in args.test_live_agents.split(",") if n}
                if not live:
                    raise ParityUnavailable("live agent census is empty")
            elif args.agent_source == "snapshot":
                snapshot = (
                    Path(args.agent_snapshot)
                    if args.agent_snapshot
                    else root / "ignored" / "ci-hub" / "agent-snapshot.json"
                )
                live = live_agents_from_snapshot(
                    snapshot, args.max_source_age_secs, time.time()
                )
            else:
                database = live_taskgraph_db(args.db)
                live = live_agents_from_fleet(database)

            state, summary, fields = evaluate(root, live, read_slot_registry(root))
            if args.test_live_agents is None and args.agent_source == "live":
                fields["taskgraph_db"] = str(database)
    except ParityUnavailable as exc:
        state = UNKNOWN
        summary = f"parity could not be determined: {' '.join(str(exc).split())}"
        fields = {"live_agents_examined": 0}
        if herdr_source:
            fields["agent_source"] = "herdr"
        if database is not None:
            fields["taskgraph_db"] = str(database)

    # Captured values are rendered as tick-hub templates. The tmux diagnostic
    # format contains fields such as ``#{session_name}``; passing those braces
    # through made the intended refusal become a second, unrelated unresolved-
    # placeholder error. Preserve the evidence as inert text.  Every captured
    # field gets the same treatment, not only the summary: working directories
    # and herdr pane names reach the detail fields verbatim.
    summary = captured_value(summary)

    if args.json:
        print(json.dumps({"state": state, "summary": summary, **fields}, indent=2, sort_keys=True))
    else:
        print(f"state={state}")
        print(f"summary={summary}")
        for key, value in fields.items():
            print(f"{key}={captured_value(value)}")

    if state == ACTIONABLE:
        return 1
    if state == UNKNOWN:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
