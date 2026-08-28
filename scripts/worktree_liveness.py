#!/usr/bin/env python3
"""Derive worktree-slot liveness from the RUNNING SYSTEM, not from an assertion.

NOT A TICK-HUB GATE. UNWIRED 2026-08-08 -- DO NOT RE-WIRE.
    Owner directive: "tick hub should not be used for interfering with these orc
    internal matters." This tool answers "is the agent owning this slot gone?",
    which is ORC's question and ORC's authority. As a standing gate
    (`worktree_new_dead_owner`, via `--fail-on-new-dead`) it reported an agent as
    dead while that agent held the freshest activity timestamp on the box, and
    the coordinator's mandate to replace dead agents without asking would have
    turned that into a destroyed agent mid-P0.

    The DISK problem it was really hired for -- big slots idling under
    `worktrees/` forever -- was not dropped. It moved to
    `ci-hub/health/slot_disk_residue.py`, which asks the disk question directly
    (on disk, consuming space, no live process inside) and names no agent at all.

    This file survives as an ON-REQUEST audit for a human or coordinator with a
    specific question. `--fail-on-new-dead` exists solely for that manual use and
    prints a warning saying so; a recurring caller is a policy violation, not a
    configuration choice.

`worktree-state.json` records who *claimed* a slot. It has never recorded whether
that claimant still exists, so a dead owner's `status=active` slot is
indistinguishable from a live one and stays protected forever while its contents
rot invisibly. Measured 2026-08-07: 104 slots marked `active` against a policy cap
of 12 active worktrees / 15 agents, and a teardown left 14 commits existing in
exactly one place plus 30 dirty trees with nothing signalling it.

THE DESIGN CHOICE THAT MATTERS. The obvious fix -- add a `heartbeat` field agents
must write -- reproduces the defect it is meant to cure: a registry that agents
must REMEMBER to update goes stale, and a stale heartbeat is indistinguishable
from a dead one. So liveness here is DERIVED, not declared:

    a slot is LIVE iff the operating system says a live process is in it.

Nobody can forget to update that, and nobody can forge it by editing a file. The
recorded `owner_pid`/`heartbeat` fields (written by `touch`) are corroborating
evidence only -- they can promote a slot to LIVE, never demote one to DEAD.

Three independent authorities are reconciled, and they are NOT collapsed into one
check, because each fails differently:

  1. REGISTRY   -- worktree-state.json (what was claimed)
  2. PORCELAIN  -- `git worktree list --porcelain` (what git believes)
  3. PROCESSES  -- /proc/<pid>/cwd (what is actually happening)

A slot present in 2 or 3 but absent from 1 is a BARE-GIT BYPASS: someone ran
`git worktree add` directly, and the registry now looks complete while being wrong.
That is worse than no registry, so it is reported as its own class.

READ-ONLY by default. `report` never writes, never removes, never cleans; 30 slots
currently hold unprotected uncommitted work and reclaiming is explicitly not this
tool's job. Only `touch` writes, and only the two liveness fields of one slot.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ci-hub" / "lib"))
from worktree_registry import read_registry  # noqa: E402

STATE_FILE = "worktree-state.json"
SLOTS_DIR = Path("worktrees/slots")

# A slot is DEAD only if it claims to be active. `released` slots are correctly
# inactive and are not a finding.
ACTIVE_STATUS = "active"

# Classifications. Kept as data so the report cannot drift from the tally.
LIVE = "LIVE"
DEAD_OWNER = "DEAD_OWNER"
RELEASED = "RELEASED"
PHANTOM_PATH = "PHANTOM_PATH"
BARE_GIT_BYPASS = "BARE_GIT_BYPASS"


def repo_root(start: Path | None = None) -> Path:
    here = (start or Path(__file__).resolve().parent).resolve()
    for cand in [here, *here.parents]:
        if (cand / STATE_FILE).exists():
            return cand
    raise SystemExit(f"worktree_liveness: no {STATE_FILE} found above {here}")


def pid_alive(pid: int) -> bool:
    """Signal 0 asks 'may I signal this' without sending anything.

    EPERM means the process EXISTS but is not ours -- that is alive, not dead.
    Treating EPERM as dead would let another user's live agent read as a corpse.
    """
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OverflowError:
        return False


def occupied_slots(root: Path) -> dict[str, int]:
    """Slots containing at least one live process, by /proc/<pid>/cwd.

    This is the load-bearing signal. It is deliberately NOT a pattern match on
    process names: matching names is how sibling agents get misidentified (and,
    on this box, killed). A cwd is an unforgeable statement of where a process is.
    """
    prefix = str(root / SLOTS_DIR) + os.sep
    counts: dict[str, int] = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            cwd = os.readlink(f"/proc/{entry}/cwd")
        except OSError:
            # Process exited between listdir and readlink, or is not ours.
            continue
        if not cwd.startswith(prefix):
            continue
        slot = cwd[len(prefix):].split(os.sep)[0]
        if slot:
            counts[slot] = counts.get(slot, 0) + 1
    return counts


PRODUCTS = ("hermit", "reverie", "liteinst2")


def porcelain_slots(root: Path, products: tuple[str, ...] = PRODUCTS) -> set[str]:
    """Slot names git itself believes are worktrees.

    THE AUTHORITY IS PER-PRODUCT, NOT THE PARENT. A slot is
    `worktrees/slots/<slot>/`, but the things git tracks are its CHILDREN:
    `worktrees/slots/<slot>/hermit` is a
    worktree of the hermit repo, and the parent repo knows nothing about it.
    Asking the parent returns 42 unrelated entries and ZERO slot children, which
    made an earlier version of this function report every active slot as missing
    from porcelain -- a 104/104 "drift" that was entirely an artifact of querying
    the wrong repository. Measured: hermit 101 slot children, reverie 46,
    liteinst2 16. Query each product and union the slot names.
    """
    prefix = str(root / SLOTS_DIR) + os.sep
    found: set[str] = set()
    for product in products:
        if not (root / product / ".git").exists():
            continue
        try:
            out = subprocess.run(
                ["git", "-C", str(root / product), "worktree", "list", "--porcelain"],
                capture_output=True, text=True, timeout=180,
            )
        except (OSError, subprocess.SubprocessError):
            continue
        if out.returncode != 0:
            continue
        for line in out.stdout.splitlines():
            if not line.startswith("worktree "):
                continue
            path = line[len("worktree "):].strip()
            if path.startswith(prefix):
                found.add(path[len(prefix):].split(os.sep)[0])
    return found


def parse_ts(value: str | None) -> datetime | None:
    if not value or not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


@dataclass
class SlotVerdict:
    slot: str
    classification: str
    status: str = ""
    procs: int = 0
    owner_pid: int | None = None
    owner_pid_alive: bool = False
    heartbeat_age_h: float | None = None
    in_porcelain: bool = False
    path_exists: bool = False
    agents: list[str] = field(default_factory=list)
    reason: str = ""

    def to_dict(self) -> dict:
        d = dict(self.__dict__)
        return d


def classify(
    root: Path,
    state: dict,
    occ: dict[str, int],
    porc: set[str],
    now: datetime,
    stale_hours: float,
) -> list[SlotVerdict]:
    slots = state.get("slots") or {}
    verdicts: list[SlotVerdict] = []
    for name in sorted(slots):
        rec = slots[name] or {}
        procs = occ.get(name, 0)
        opid = rec.get("owner_pid")
        opid = int(opid) if isinstance(opid, int) else None
        alive = pid_alive(opid) if opid is not None else False
        hb = parse_ts(rec.get("heartbeat"))
        age = (now - hb).total_seconds() / 3600.0 if hb else None
        agents = [a.get("name", "") for a in (rec.get("agents") or []) if isinstance(a, dict)]
        path = root / SLOTS_DIR / name
        v = SlotVerdict(
            slot=name, classification="", status=str(rec.get("status") or ""),
            procs=procs, owner_pid=opid, owner_pid_alive=alive,
            heartbeat_age_h=round(age, 2) if age is not None else None,
            in_porcelain=name in porc, path_exists=path.exists(), agents=agents,
        )
        # Order matters. Any positive liveness evidence wins: the cost of a false
        # DEAD is someone's work discarded, the cost of a false LIVE is a slot
        # that stays protected one more cycle. Those are not symmetric.
        if procs > 0:
            v.classification, v.reason = LIVE, f"{procs} live process(es) with cwd in slot"
        elif alive:
            v.classification, v.reason = LIVE, f"recorded owner_pid {opid} is alive"
        elif age is not None and age < stale_hours:
            v.classification, v.reason = LIVE, f"heartbeat {age:.1f}h old (< {stale_hours}h)"
        elif v.status != ACTIVE_STATUS:
            v.classification, v.reason = RELEASED, f"status={v.status or 'unset'}, not active"
        elif not v.path_exists:
            v.classification, v.reason = PHANTOM_PATH, "claims active but slot directory is absent"
        else:
            bits = ["no process in slot"]
            bits.append(f"owner_pid {opid} dead" if opid is not None else "no owner_pid recorded")
            bits.append(f"heartbeat {age:.1f}h stale" if age is not None else "no heartbeat recorded")
            v.classification, v.reason = DEAD_OWNER, "; ".join(bits)
        verdicts.append(v)

    # The bypass class: git and/or the filesystem know about a slot the registry
    # does not. This is the failure the owner named -- a CLI agents can bypass
    # produces a registry that LOOKS complete.
    known = set(slots)
    on_disk = set()
    wt = root / SLOTS_DIR
    if wt.is_dir():
        on_disk = {p.name for p in wt.iterdir() if p.is_dir()}
    for name in sorted((porc | on_disk) - known):
        verdicts.append(SlotVerdict(
            slot=name, classification=BARE_GIT_BYPASS,
            procs=occ.get(name, 0), in_porcelain=name in porc,
            path_exists=(wt / name).exists(),
            reason="present in git porcelain and/or on disk but ABSENT from the registry",
        ))
    return verdicts


def build_report(root: Path, stale_hours: float, now: datetime | None = None) -> dict:
    state = read_registry(
        root / STATE_FILE,
        checker=Path(__file__).resolve().with_name("check-worktree-registry.rs"),
    )
    occ = occupied_slots(root)
    porc = porcelain_slots(root)
    now = now or datetime.now(timezone.utc)
    verdicts = classify(root, state, occ, porc, now, stale_hours)
    tally: dict[str, int] = {}
    for v in verdicts:
        tally[v.classification] = tally.get(v.classification, 0) + 1
    return {
        "root": str(root),
        "stale_hours": stale_hours,
        "registry_slots": len(state.get("slots") or {}),
        "slots_with_live_process": len(occ),
        "tally": tally,
        "total_classified": len(verdicts),
        "verdicts": [v.to_dict() for v in verdicts],
    }


def render(report: dict) -> str:
    lines = ["worktree liveness -- derived from the running system, not asserted"]
    lines.append(f"  registry slots            : {report['registry_slots']}")
    lines.append(f"  slots with a live process : {report['slots_with_live_process']}")
    lines.append(f"  stale-heartbeat threshold : {report['stale_hours']}h")
    lines.append("")
    t = report["tally"]
    for k in (LIVE, DEAD_OWNER, PHANTOM_PATH, BARE_GIT_BYPASS, RELEASED):
        if k in t:
            lines.append(f"  {k:<16} {t[k]}")
    lines.append(f"  {'TOTAL':<16} {report['total_classified']}")
    flagged = [v for v in report["verdicts"]
               if v["classification"] in (DEAD_OWNER, PHANTOM_PATH, BARE_GIT_BYPASS)]
    unflagged = [v for v in report["verdicts"] if v["classification"] == LIVE]
    lines.append("")
    lines.append(f"  NOT FLAGGED (live, left alone): {len(unflagged)}")
    if unflagged:
        lines.append("    " + ", ".join(f"{v['slot']}({v['procs']})" for v in unflagged))
    lines.append("")
    lines.append(f"  FLAGGED: {len(flagged)}")
    for v in flagged:
        who = ",".join(a for a in v["agents"] if a) or "-"
        lines.append(f"    {v['classification']:<16} {v['slot']:<18} agents={who:<28} {v['reason']}")
    lines.append("")
    lines.append("  NOTE: flagged != reclaimable. Slots may hold uncommitted work;")
    lines.append("  this tool never removes, resets, or cleans anything.")
    return "\n".join(lines)


def flagged_slots(report: dict) -> set[str]:
    return {v["slot"] for v in report["verdicts"]
            if v["classification"] in (DEAD_OWNER, PHANTOM_PATH, BARE_GIT_BYPASS)}


def new_dead(report: dict, state_path: Path) -> tuple[set[str], set[str]]:
    """Slots that became flagged SINCE THE LAST TICK, and the current flagged set.

    THIS IS WHAT MAKES THE CHECK WIREABLE. `--fail-on-dead` is correct as a
    one-shot audit and useless as a recurring alarm: 70 slots are flagged right
    now and none may be reclaimed (they hold unprotected work), so a standing
    gate would fire on every tick forever and be muted within a day -- the
    "permanently-on" failure mode this repo has already hit once with the
    fired-state staleness alarm.

    A DELTA is the alarmable event: a slot that was live at the last tick and is
    dead at this one is news, and it fires at the tick that caused it. The
    standing 70 are a backlog, reported by `report`, not paged.

    The state is the CURRENT flagged set, not a cumulative union: a slot that
    revives and later dies again must alarm again.
    """
    current = flagged_slots(report)
    try:
        known = set(json.loads(state_path.read_text(encoding="utf-8")).get("flagged", []))
    except (OSError, ValueError, AttributeError):
        # No prior state: adopt the baseline WITHOUT alarming. A first run must
        # not page the whole standing backlog as if it just happened.
        known = current
    return current - known, current


def write_state(state_path: Path, flagged: set[str]) -> None:
    state_path.parent.mkdir(parents=True, exist_ok=True)
    tmp = state_path.with_suffix(state_path.suffix + ".tmp")
    tmp.write_text(json.dumps({"flagged": sorted(flagged)}, indent=2) + "\n",
                   encoding="utf-8")
    tmp.replace(state_path)


def cmd_report(args: argparse.Namespace) -> int:
    root = repo_root(Path(args.root) if args.root else None)
    report = build_report(root, args.stale_hours)

    if args.fail_on_new_dead:
        # Loud on stderr so a rewiring shows up in whatever log captures it. This
        # never changes the exit code: the manual audit must keep working.
        print(
            "worktree_liveness: WARNING -- --fail-on-new-dead asserts AGENT liveness, "
            "which ORC owns. It was unwired from tick-hub on 2026-08-08 after a false "
            "'agent died' alarm. For the disk-reclamation question use "
            "ci-hub/health/slot_disk_residue.py. Manual/audit use only; do not wire "
            "this as a recurring gate.",
            file=sys.stderr,
        )
        state_path = Path(args.state) if args.state else root / ".tick-hub" / "worktree-liveness-state.json"
        fresh, current = new_dead(report, state_path)
        write_state(state_path, current)
        if fresh:
            print(f"NEW dead-owner/bypass slot(s) since last tick: {len(fresh)} "
                  f"({', '.join(sorted(fresh))}); {len(current)} flagged in total")
            return 1
        print(f"no new dead-owner slots; {len(current)} flagged in total (standing backlog, not paged)")
        return 0

    print(json.dumps(report, indent=2) if args.json else render(report))
    if args.fail_on_dead:
        bad = sum(report["tally"].get(k, 0)
                  for k in (DEAD_OWNER, PHANTOM_PATH, BARE_GIT_BYPASS))
        return 1 if bad else 0
    return 0


def cmd_touch(args: argparse.Namespace) -> int:
    """Record owner_pid + heartbeat for one slot. Corroboration, not authority."""
    root = repo_root(Path(args.root) if args.root else None)
    path = root / STATE_FILE
    state = json.loads(path.read_text(encoding="utf-8"))
    slots = state.setdefault("slots", {})
    if args.slot not in slots:
        print(f"worktree_liveness: unknown slot {args.slot!r}", file=sys.stderr)
        return 2
    rec = slots[args.slot]
    rec["owner_pid"] = int(args.pid) if args.pid is not None else os.getppid()
    rec["heartbeat"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)
    print(f"touched {args.slot}: owner_pid={rec['owner_pid']} heartbeat={rec['heartbeat']}")
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--root", help="repo root (default: discovered from this script)")
    sub = p.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("report", description="classify every slot (read-only)", help="classify every slot (read-only)")
    r.add_argument("--json", action="store_true")
    r.add_argument("--stale-hours", type=float, default=6.0)
    r.add_argument("--fail-on-dead", action="store_true",
                   help="exit 1 if ANY dead-owner/phantom/bypass slot is found "
                        "(one-shot audit; unusable as a recurring alarm)")
    r.add_argument("--fail-on-new-dead", action="store_true",
                   help="exit 1 only for slots newly flagged since the last tick "
                        "(the wireable form; standing backlog is not paged)")
    r.add_argument("--state", default=None,
                   help="delta state file (default .tick-hub/worktree-liveness-state.json)")
    r.set_defaults(func=cmd_report)

    t = sub.add_parser("touch", description="record owner_pid+heartbeat for a slot", help="record owner_pid+heartbeat for a slot")
    t.add_argument("--slot", required=True)
    t.add_argument("--pid", type=int, default=None)
    t.set_defaults(func=cmd_touch)

    args = p.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
