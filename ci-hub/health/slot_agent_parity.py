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
from worktree_registry import RegistrySchemaError, read_registry  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
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
    try:
        doc = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise ParityUnavailable(f"cannot read agent snapshot {path}: {exc}") from exc
    captured_at = doc.get("captured_at")
    if not isinstance(captured_at, (int, float)):
        raise ParityUnavailable(f"{path} has no numeric captured_at")
    age = now - float(captured_at)
    if age > max_age_secs:
        raise ParityUnavailable(
            f"agent snapshot {path} is {age / 60:.1f} min old, older than the "
            f"{max_age_secs / 60:.1f} min bound ({age / max_age_secs:.0f} ticks); "
            "refusing to judge parity against a fleet that has moved on"
        )
    agents = doc.get("agents")
    if not isinstance(agents, list):
        raise ParityUnavailable(f"{path} agents is not a list")
    names = {
        str(a.get("name") or a.get("agent") or "")
        for a in agents
        if isinstance(a, dict)
    }
    names.discard("")
    if not names:
        raise ParityUnavailable(f"{path} yielded no agent identities")
    return names


def slot_owner_names(record: dict) -> set[str]:
    agents = record.get("agents")
    if not isinstance(agents, list):
        return set()
    names = {
        str(a.get("name") or "") for a in agents if isinstance(a, dict)
    }
    names.discard("")
    return names


def occupied_slots(root: Path, slot_names: set[str]) -> set[str]:
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
    for name in slot_names:
        slot_dir = (root / "worktrees" / "slots" / name).resolve()
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


def evaluate(root: Path, live: set[str], registry: dict) -> tuple[str, str, dict]:
    raw_slots = registry.get("slots")
    if not isinstance(raw_slots, dict):
        raise ParityUnavailable("worktree-state.json slots is not an object")
    active = {
        str(name): raw
        for name, raw in raw_slots.items()
        if isinstance(raw, dict) and raw.get("status") == "active"
    }
    occupied = occupied_slots(root, set(active))

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

    root = Path(args.root).resolve() if args.root else ROOT
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

        registry_path = root / "worktree-state.json"
        try:
            registry = read_registry(registry_path)
        except RegistrySchemaError as exc:
            raise ParityUnavailable(f"cannot read {registry_path}: {exc}") from exc

        state, summary, fields = evaluate(root, live, registry)
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
