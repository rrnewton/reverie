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
    see the denominator that produced the verdict instead of trusting it.

Exit codes follow `slot_disk_residue.py`: 0 clean, 1 actionable, 2 could not be
determined.  A could-not-determine is never a pass.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))

import residue_sweep  # noqa: E402
import agent_snapshot as agent_snapshot_contract  # noqa: E402

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
WRKSLOTS_CONFIG = ".wrkslots.yml"
WRKSLOTS_REGISTRY_DIR = "worktrees"


def wrkslots_machine(root: Path) -> str:
    """The configured machine shard, read rather than guessed.

    Refusing here rather than globbing for any ACTIVE.*.json is deliberate: on a
    host that has ever carried two shards, picking one by chance would report a
    number for the wrong fleet, which is the failure this whole change is about.
    """
    override = os.environ.get("WRKSLOTS_MACHINE")
    if override:
        return override
    path = root / WRKSLOTS_CONFIG
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ParityUnavailable(f"cannot read {path}: {exc}") from exc
    machine = document.get("machine") if isinstance(document, dict) else None
    if not isinstance(machine, str) or not machine:
        raise ParityUnavailable(f"{path} records no machine shard")
    return machine


def read_slot_registry(root: Path) -> dict[str, dict]:
    """Live slot rows from the wrkslots registry, keyed by slot name."""

    machine = wrkslots_machine(root)
    path = root / WRKSLOTS_REGISTRY_DIR / f"ACTIVE.{machine}.json"
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ParityUnavailable(f"cannot read slot registry {path}: {exc}") from exc
    if not isinstance(document, dict):
        raise ParityUnavailable(f"slot registry {path} root is not an object")
    rows = document.get("slots")
    if not isinstance(rows, list):
        raise ParityUnavailable(f"slot registry {path} slots is not a list")
    active: dict[str, dict] = {}
    for row in rows:
        if not isinstance(row, dict):
            continue
        name = row.get("slot")
        if isinstance(name, str) and name:
            active[name] = row
    # An EMPTY registry is a refusal, not a clean bill of health. Zero rows and
    # an unreadable registry produce the same verdict here for the same reason a
    # zero-agent census does below: a denominator of zero computes zero orphans.
    if not active:
        raise ParityUnavailable(
            f"slot registry {path} lists no slots; a fleet with no rows at all is "
            "unverifiable rather than clean"
        )
    return active


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


def live_agents_from_fleet() -> set[str]:
    try:
        live, _orphans = residue_sweep.read_live_fleet()
    except residue_sweep.SweepUnavailable as exc:
        raise ParityUnavailable(f"live agent census unavailable: {exc}") from exc
    if not live:
        # read_live_fleet already refuses this; belt and braces, because the
        # whole point of the file is that an empty denominator never passes.
        raise ParityUnavailable("live agent census is empty")
    return live


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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gate", action="store_true", help="emit gate key=value output")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--root", default=None, help="repository root (tests)")
    parser.add_argument(
        "--agent-source",
        choices=("live", "snapshot"),
        default="live",
        help="live: the orphan detector's fleet (default). snapshot: the cached "
        "agent snapshot, which is refused when stale.",
    )
    parser.add_argument("--agent-snapshot", default=None)
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
    try:
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
            live = live_agents_from_snapshot(snapshot, args.max_source_age_secs, time.time())
        else:
            live = live_agents_from_fleet()

        state, summary, fields = evaluate(root, live, read_slot_registry(root))
    except ParityUnavailable as exc:
        state = UNKNOWN
        summary = f"parity could not be determined: {' '.join(str(exc).split())}"
        fields = {"live_agents_examined": 0}

    if args.json:
        print(json.dumps({"state": state, "summary": summary, **fields}, indent=2, sort_keys=True))
    else:
        print(f"state={state}")
        print(f"summary={summary}")
        for key, value in fields.items():
            print(f"{key}={value}")

    if state == ACTIONABLE:
        return 1
    if state == UNKNOWN:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
