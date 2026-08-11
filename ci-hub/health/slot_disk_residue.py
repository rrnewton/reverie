#!/usr/bin/env python3
"""Measure worktree slot capacity, identity-bound ownership, and disk residue.

THREE DIFFERENT QUANTITIES.  The active-slot cap is an aggregate registry count
against Hard Invariant 13.  A dead-owner finding is an exact process-identity
verdict from validate-lock's guarded sidecar.  Disk residue is a per-directory
size/occupancy/idleness measurement.  None substitutes for another: ninety-two
fresh slots violate a cap of twelve even though no individual slot is old.

WHAT THIS STILL REFUSES.  Its predecessor guessed agent death from a registry
name missing in an ORC fleet snapshot.  On 2026-08-08 that disagreement falsely
named the agent with the freshest activity timestamp as dead.  This gate never
uses that census or `agent_liveness_probe.py`.  It accepts a dead owner only when
the canonical guarded authority says the recorded PID + boot_id + start_ticks is
dead; `alive` suppresses disk-idleness reclaim for the matching registered slot,
and unknown is never converted to dead.

The blast radii are not symmetric.  Every finding therefore authorizes nothing:

* cap breach: active registry rows exceed the hard count, independent of age;
* dead owner: exact guarded identity is dead and no live process uses the slot;
* disk candidate: it consumes space and has sustained process-free idleness.

DETECT ONLY.  Nothing here removes, releases, resets, cleans, or prunes anything.
A flagged slot is very often holding the only copy of somebody's uncommitted work
-- reclaiming is a coordinator decision made with a recovery SHA in hand, and this
tool deliberately gives the coordinator no mechanism to skip that step.

THREE MEASUREMENT HONESTIES, because a number without them is not evidence:

1. OCCUPANCY IS SIX /proc SIGNALS -- cwd, exe, root, fd, maps, map_files -- the
   exact set `release-worktree.rs::live_process_users` refuses on. None can be
   forged by editing a file and nobody can forget to update them. This gate is
   NOT the authority on release; it mirrors the authority's predicate so the two
   cannot disagree, and it is allowed to be stricter-or-equal, never looser.
   Cwd alone was the original definition and it was too narrow: measured
   2026-08-08 across 77 slots, one slot (`scorecard`) was held by 52 processes
   through exe/fd/maps/map_files with no cwd inside it, and cwd-only called it
   empty. The blind spot is a same-uid process whose links are all unreadable;
   `blind_pids` is reported every run so the count travels with its uncertainty.

2. IDLENESS IS SUSTAINED, NOT INSTANTANEOUS. A slot must read idle continuously
   for `--idle-hours` before it can qualify. This is what makes the 0.6% blind
   spot tolerable: a permission blip does not survive a day of consecutive ticks.

3. SIZE IS ACTUAL DISK BLOCKS, NOT APPARENT SIZE. Slots are seeded with
   `cp -a --reflink=auto`, so extents are shared between them. A slot reported at
   8 GiB may free far less than 8 GiB when removed, because its neighbours hold
   references to the same extents. The number is an upper bound on what reclaim
   yields, and `summary=` says so rather than implying a clean total.

DISK PAGES ON THE DELTA; SAFETY BREACHES STAY FAILED.  A disk-idleness candidate
pages only when newly reclaimable, because a permanent disk page gets muted.  A
cap breach or proven dead owner is different: `--gate` remains failed while the
condition holds and always carries the count/cap or guarded identity in captured
fields.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path

import residue_sweep

WORKTREES_DIR = "worktrees"
DEFAULT_STATE = Path(".tick-hub") / "slot-disk-residue-state.json"

# Defaults. A slot must be BOTH sustained-idle and materially large: a 40 MiB
# idle directory is not a disk problem and paging about it is noise.
DEFAULT_IDLE_HOURS = 24.0
DEFAULT_MIN_GIB = 1.0
DEFAULT_REFRESH_HOURS = 12.0
DEFAULT_DU_TIMEOUT = 120
DEFAULT_MAX_MEASURE = 60
DEFAULT_ACTIVE_SLOT_CAP = 12

GIB = 1024.0 ** 3

OK = "ok"
RECLAIMABLE = "reclaimable"
UNKNOWN = "unknown"


class ResidueUnavailable(RuntimeError):
    """An input could not be read. Silence is not a clean result."""


def repo_root(start: Path | None = None) -> Path:
    here = (start or Path(__file__).resolve().parent).resolve()
    for cand in [here, *here.parents]:
        if (cand / WORKTREES_DIR).is_dir():
            return cand
    raise ResidueUnavailable(f"no {WORKTREES_DIR}/ directory found above {here}")


def slots_on_disk(root: Path) -> set[str]:
    """Slot directories that actually exist and therefore actually cost space.

    The FILESYSTEM is the authority here, not `worktree-state.json`. A registry
    can claim a slot that was deleted (costing nothing) and can omit a slot that
    exists (costing plenty); only one of those two is a disk problem, and it is
    the one the registry cannot see.
    """
    wt = root / WORKTREES_DIR
    try:
        return {p.name for p in wt.iterdir() if p.is_dir() and not p.is_symlink()}
    except OSError as exc:
        raise ResidueUnavailable(f"cannot enumerate {wt}: {exc}") from exc


def read_registry(path: Path) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ResidueUnavailable(f"cannot read slot registry {path}: {exc}") from exc
    if not isinstance(value, dict) or not isinstance(value.get("slots"), dict):
        raise ResidueUnavailable(f"slot registry {path} has no slots object")
    return value


def active_slot_names(registry: dict[str, object]) -> set[str]:
    raw_slots = registry.get("slots")
    if not isinstance(raw_slots, dict):
        raise ResidueUnavailable("slot registry slots is not an object")
    return {
        str(name)
        for name, raw in raw_slots.items()
        if isinstance(raw, dict) and raw.get("status") == "active"
    }


@dataclass
class Occupancy:
    counts: dict[str, int] = field(default_factory=dict)
    blind_pids: int = 0
    zombie_pids: int = 0
    deleted_cwd_pids: int = 0


def _proc_state(pid: str) -> str:
    """Scheduler state letter from /proc/<pid>/stat, tolerant of ')' in comm."""
    try:
        with open(f"/proc/{pid}/stat", encoding="utf-8", errors="replace") as fh:
            return fh.read().rsplit(")", 1)[1].split()[0]
    except (OSError, IndexError):
        return "?"


# The occupancy signals, in the order `release-worktree.rs::live_process_users`
# checks them. THIS LIST IS A MIRROR, NOT A SECOND OPINION -- see `occupancy`.
#   cwd/exe/root : single symlinks under /proc/<pid>/
#   fd/map_files : directories of symlinks
#   maps         : mapped-file paths, matched textually
SINGLE_LINK_SIGNALS = ("cwd", "exe", "root")
DIRECTORY_LINK_SIGNALS = ("fd", "map_files")


def occupancy(root: Path, proc_root: Path = Path("/proc")) -> Occupancy:
    """Slots containing at least one live process, by SIX signals, not one.

    THIS GATE IS NOT THE AUTHORITY AND MUST NOT BEHAVE LIKE ONE.
    `release-worktree.rs::live_process_users` decides whether a slot may be
    released; it refuses on any of cwd, exe, root, fd, maps or map_files. This
    function mirrors that set so the two agree, and the report says "candidate"
    rather than "reclaimable" so a reader is never told a slot is free that the
    release path will then refuse.

    WHY THE MIRROR EXISTS AT ALL, since two implementations of one predicate is
    the thing to avoid: the authority is Rust and refuses as a side effect of
    attempting a removal, so it cannot be consulted cheaply from a read-only
    health tick. The reconciliation is that this side is deliberately allowed to
    be STRICTER-or-equal and never looser, and a test pins the diverging case.

    WHAT CWD-ONLY MISSED, measured 2026-08-08 across 77 slots: exactly one slot
    changed classification -- `scorecard`, held by 52 processes via exe, fd, maps
    and map_files with NO cwd inside it. They were executing binaries out of the
    slot. Cwd-only called it empty. Nothing was cwd-occupied-but-otherwise-free,
    so the wider predicate is a strict superset: it can only ever remove slots
    from the reclaim list, never add them.

    Deliberately NOT a match on process names. Name matching is how sibling
    agents get misidentified and, on this shared box, killed; these links are
    unforgeable statements of what a process is actually using.

    A cwd rendered by the kernel as `<path> (deleted)` COUNTS AS OCCUPANCY. The
    suffix is appended to the whole path and marks the unlinked *leaf*, which is
    usually a child worktree (`<slot>/hermit`) removed out from under a process
    that is still running there. The slot above it still exists and is still in
    use, so discarding those pids would report a busy slot as idle. A slot that
    has genuinely vanished needs no special case: it is absent from
    `slots_on_disk` and drops out on its own. Counted for visibility, never
    subtracted.
    """
    prefix = str(root / WORKTREES_DIR) + os.sep
    occ = Occupancy()
    uid = os.getuid()
    try:
        entries = [e for e in os.listdir(proc_root) if e.isdigit()]
    except OSError as exc:
        raise ResidueUnavailable(f"cannot enumerate {proc_root}: {exc}") from exc

    def slot_for(target: str) -> str | None:
        """The slot a /proc link points into, or None. Handles the deleted suffix."""
        if target.endswith(" (deleted)"):
            target = target[: -len(" (deleted)")]
        if not target.startswith(prefix):
            return None
        return target[len(prefix):].split(os.sep)[0] or None

    for pid in entries:
        proc = proc_root / pid
        try:
            same_uid = proc.stat().st_uid == uid
        except OSError:
            continue
        if not same_uid:
            continue

        hits: set[str] = set()
        readable = False
        for kind in SINGLE_LINK_SIGNALS:
            try:
                target = os.readlink(proc / kind)
            except OSError:
                continue
            readable = True
            if kind == "cwd" and target.endswith(" (deleted)") \
                    and target[: -len(" (deleted)")].startswith(prefix):
                occ.deleted_cwd_pids += 1
            slot = slot_for(target)
            if slot:
                hits.add(slot)
        for kind in DIRECTORY_LINK_SIGNALS:
            try:
                names = os.listdir(proc / kind)
            except OSError:
                continue
            readable = True
            for name in names:
                try:
                    target = os.readlink(proc / kind / name)
                except OSError:
                    continue
                slot = slot_for(target)
                if slot:
                    hits.add(slot)
        # `maps` is text, not links: the path is the last field of each line.
        try:
            with open(proc / "maps", encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    if prefix in line:
                        slot = slot_for(line.rstrip("\n").split(" ")[-1].strip())
                        if slot:
                            hits.add(slot)
        except OSError:
            pass

        # NOTE: `maps` deliberately does NOT set `readable`. It opens successfully
        # and empty for a zombie, which would mask every zombie as a live legible
        # process -- observed as zombie_pids dropping 754 -> 0. Legibility is
        # judged on the LINK signals, which a zombie genuinely lacks.
        if not readable:
            # Nothing about this process was legible. A zombie has no links and
            # cannot be using a slot; anything else is the genuine blind spot.
            if _proc_state(pid) == "Z":
                occ.zombie_pids += 1
            else:
                occ.blind_pids += 1
            continue
        # One process counts ONCE per slot however many signals matched -- the
        # count is "processes using this slot", not "references to it".
        for slot in hits:
            occ.counts[slot] = occ.counts.get(slot, 0) + 1
    return occ


def disk_bytes(path: Path, timeout: int = DEFAULT_DU_TIMEOUT) -> int | None:
    """Actual disk blocks used, not apparent size.

    `--block-size=1` without `--apparent-size` reports allocated blocks, which is
    what "consuming space" means. `--one-file-system` keeps a stray bind mount or
    nested mount from being attributed to the slot. Returns None -- never 0 -- if
    the measurement could not be taken, so an unmeasurable slot cannot silently
    read as empty and get flagged.
    """
    try:
        out = subprocess.run(
            ["du", "-s", "--block-size=1", "--one-file-system", str(path)],
            capture_output=True, text=True, timeout=timeout, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0 and not out.stdout.strip():
        return None
    try:
        return int(out.stdout.split(None, 1)[0])
    except (ValueError, IndexError):
        return None


def parse_ts(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def fmt_ts(when: datetime) -> str:
    return when.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def read_state(path: Path) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def write_state(path: Path, state: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


@dataclass
class SlotResidue:
    slot: str
    procs: int
    idle_since: str | None
    idle_hours: float | None
    size_bytes: int | None
    reclaimable: bool
    reason: str

    def to_dict(self) -> dict:
        return dict(self.__dict__)


def evaluate(
    root: Path,
    prior: dict,
    now: datetime,
    idle_hours: float,
    min_gib: float,
    refresh_hours: float,
    max_measure: int,
    du_timeout: int,
    measure: bool = True,
) -> tuple[list[SlotResidue], dict, Occupancy]:
    """Classify every on-disk slot. Pure disk + process facts, nothing else."""
    on_disk = slots_on_disk(root)
    occ = occupancy(root)
    prior_slots = prior.get("slots") if isinstance(prior.get("slots"), dict) else {}
    min_bytes = min_gib * GIB

    next_slots: dict[str, dict] = {}
    idle_names: list[str] = []
    for slot in sorted(on_disk):
        procs = occ.counts.get(slot, 0)
        rec = prior_slots.get(slot) if isinstance(prior_slots.get(slot), dict) else {}
        if procs > 0:
            # Any live process resets the clock outright. Positive evidence of use
            # always wins; the cost of a false "idle" is somebody's work listed
            # for deletion, the cost of a false "busy" is one more cycle of disk.
            next_slots[slot] = {"size_bytes": rec.get("size_bytes"),
                                "sized_at": rec.get("sized_at")}
            continue
        since = parse_ts(rec.get("idle_since")) or now
        next_slots[slot] = {
            "idle_since": fmt_ts(since),
            "size_bytes": rec.get("size_bytes"),
            "sized_at": rec.get("sized_at"),
        }
        idle_names.append(slot)

    # Measure only idle slots, and only when the cached size is missing or stale.
    # An occupied slot's size is nobody's business: it is not a reclaim candidate.
    if measure:
        stale = []
        for slot in idle_names:
            rec = next_slots[slot]
            sized_at = parse_ts(rec.get("sized_at"))
            age_h = (now - sized_at).total_seconds() / 3600.0 if sized_at else None
            if rec.get("size_bytes") is None or age_h is None or age_h >= refresh_hours:
                stale.append(slot)
        for slot in stale[:max_measure]:
            size = disk_bytes(root / WORKTREES_DIR / slot, timeout=du_timeout)
            if size is not None:
                next_slots[slot]["size_bytes"] = size
                next_slots[slot]["sized_at"] = fmt_ts(now)

    results: list[SlotResidue] = []
    for slot in sorted(on_disk):
        rec = next_slots[slot]
        procs = occ.counts.get(slot, 0)
        size = rec.get("size_bytes")
        size = size if isinstance(size, int) else None
        since = parse_ts(rec.get("idle_since"))
        age_h = (now - since).total_seconds() / 3600.0 if since else None

        if procs > 0:
            reclaimable, reason = False, (
                f"{procs} live process(es) using the slot "
                "(cwd/exe/root/fd/maps/map_files)")
        elif age_h is None or age_h < idle_hours:
            shown = 0.0 if age_h is None else age_h
            reclaimable = False
            reason = f"idle {shown:.1f}h (< {idle_hours}h threshold)"
        elif size is None:
            reclaimable, reason = False, "size could not be measured; not flagged"
        elif size < min_bytes:
            reclaimable = False
            reason = f"idle {age_h:.1f}h but only {size / GIB:.2f} GiB (< {min_gib} GiB)"
        else:
            reclaimable = True
            reason = f"idle {age_h:.1f}h, {size / GIB:.2f} GiB on disk, no process inside"

        results.append(SlotResidue(
            slot=slot, procs=procs,
            idle_since=rec.get("idle_since"),
            idle_hours=round(age_h, 2) if age_h is not None else None,
            size_bytes=size, reclaimable=reclaimable, reason=reason,
        ))

    return results, {"slots": next_slots}, occ


def summarize(
    results: list[SlotResidue], occ: Occupancy, fresh: set[str], min_gib: float
) -> tuple[str, str, dict]:
    flagged = [r for r in results if r.reclaimable]
    total = sum(r.size_bytes or 0 for r in flagged)
    fields = {
        "slots_on_disk": len(results),
        "slots_occupied": sum(1 for r in results if r.procs > 0),
        "slots_idle": sum(1 for r in results if r.procs == 0),
        "reclaimable_slots": len(flagged),
        "reclaimable_gib": round(total / GIB, 2),
        "new_reclaimable_slots": len(fresh),
        "blind_pids": occ.blind_pids,
        "zombie_pids": occ.zombie_pids,
        "deleted_cwd_pids": occ.deleted_cwd_pids,
    }
    state = RECLAIMABLE if fresh else OK
    if fresh:
        named = ", ".join(
            f"{r.slot} ({(r.size_bytes or 0) / GIB:.1f} GiB)"
            for r in flagged if r.slot in fresh
        )
        summary = (
            f"{len(fresh)} slot(s) newly idle >= threshold and >= {min_gib} GiB: {named}; "
            f"{len(flagged)} reclaimable in total holding up to {total / GIB:.1f} GiB "
            f"(upper bound: reflink-shared extents are counted per-slot). "
            f"DETECT ONLY -- release-worktree.rs is the authority and may still "
            f"refuse; reclaim needs a recovery SHA."
        )
    else:
        summary = (
            f"no newly reclaimable slot; {len(flagged)} standing reclaim CANDIDATE(s) "
            f"holding up to {total / GIB:.1f} GiB of {fields['slots_on_disk']} on disk "
            f"({fields['slots_occupied']} occupied). Standing backlog, not paged."
        )
    if occ.blind_pids:
        summary += f" occupancy blind spot: {occ.blind_pids} same-uid pid(s) with unreadable cwd."
    return state, summary, fields


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--root", help="repo root (default: discovered from this script)")
    p.add_argument("--json", action="store_true", help="emit the complete typed report")
    p.add_argument("--gate", action="store_true",
                   help="exit 1 when a slot became reclaimable since the last run")
    p.add_argument("--idle-hours", type=float, default=DEFAULT_IDLE_HOURS)
    p.add_argument("--min-gib", type=float, default=DEFAULT_MIN_GIB)
    p.add_argument("--refresh-hours", type=float, default=DEFAULT_REFRESH_HOURS)
    p.add_argument("--max-measure", type=int, default=DEFAULT_MAX_MEASURE)
    p.add_argument("--du-timeout", type=int, default=DEFAULT_DU_TIMEOUT)
    p.add_argument("--no-measure", action="store_true",
                   help="skip du entirely and use only cached sizes")
    p.add_argument("--state", default=None, help=f"state file (default {DEFAULT_STATE})")
    p.add_argument("--registry", default=None,
                   help="slot registry (default <root>/worktree-state.json)")
    p.add_argument(
        "--active-slot-cap", type=int,
        default=int(os.environ.get("DEV_HERMIT_ACTIVE_SLOT_CAP", DEFAULT_ACTIVE_SLOT_CAP)),
        help=f"hard active-slot cap (default {DEFAULT_ACTIVE_SLOT_CAP})",
    )
    p.add_argument(
        "--test-owner-authority-json", default=None,
        help="test-only fixture for guarded validate-lock authority JSON",
    )
    p.add_argument("--dry-run", action="store_true",
                   help="do not persist state (idle clocks and baseline stay unchanged)")
    args = p.parse_args(argv)

    if args.active_slot_cap < 0:
        p.error("--active-slot-cap must be non-negative")

    try:
        root = repo_root(Path(args.root) if args.root else None)
        state_path = Path(args.state) if args.state else root / DEFAULT_STATE
        registry_path = Path(args.registry) if args.registry else root / "worktree-state.json"
        # ``--root`` is the existing disposable-fixture interface used by the
        # disk-only tests.  Such a fixture has neither the parent registry nor
        # ci-hub; preserve that intentionally narrow mode.  Production has the
        # canonical executable and therefore refuses a missing registry.
        disk_only_fixture = bool(args.root) and not (root / "ci-hub" / "ci-hub").is_file()
        if args.test_owner_authority_json and not disk_only_fixture:
            raise ResidueUnavailable(
                "--test-owner-authority-json is restricted to a disposable --root fixture"
            )
        if disk_only_fixture and args.registry is None and not registry_path.exists():
            registry: dict[str, object] = {"slots": {}}
        else:
            registry = read_registry(registry_path)
        active_slots = active_slot_names(registry)
        prior = read_state(state_path)
        now = datetime.now(timezone.utc)
        results, next_state, occ = evaluate(
            root, prior, now,
            idle_hours=args.idle_hours, min_gib=args.min_gib,
            refresh_hours=args.refresh_hours, max_measure=args.max_measure,
            du_timeout=args.du_timeout, measure=not args.no_measure,
        )
        try:
            if disk_only_fixture and args.test_owner_authority_json is None:
                authority: dict[str, object] = {
                    "schema_version": 1,
                    "state": "free",
                    "holder": None,
                    "owner": None,
                }
            else:
                authority = residue_sweep.read_guarded_owner_authority(
                    root,
                    Path(args.test_owner_authority_json)
                    if args.test_owner_authority_json
                    else None,
                )
            dead_owner = residue_sweep.guarded_dead_owner_slots(
                registry, authority, set(occ.counts)
            )
            live_owner_slots = residue_sweep.guarded_live_owner_slots(
                registry, authority
            )
        except residue_sweep.SweepUnavailable as exc:
            raise ResidueUnavailable(str(exc)) from exc
    except ResidueUnavailable as exc:
        # Never silent: an unreadable input is `unknown`, which is not `ok`.
        print(f"state={UNKNOWN}")
        print(f"summary=slot disk residue unavailable: {exc}")
        return 2

    for result in results:
        if result.slot in live_owner_slots:
            result.reclaimable = False
            result.reason = (
                "validate-lock-guarded registered owner is alive; "
                "idle time cannot qualify this slot"
            )

    current = {r.slot for r in results if r.reclaimable}
    reported = prior.get("reported")
    if isinstance(reported, list):
        # Only slots that were ALREADY reclaimable are suppressed. A slot that
        # gets used again and later goes idle again must page again, so this is
        # the current set, never a cumulative union.
        fresh = current - set(reported)
    else:
        # First run adopts the baseline WITHOUT paging: the standing backlog did
        # not happen at this tick and must not be reported as if it had.
        fresh = set()
    next_state["reported"] = sorted(current)

    if not args.dry_run:
        write_state(state_path, next_state)

    state, summary, fields = summarize(results, occ, fresh, args.min_gib)

    active_count = len(active_slots)
    active_excess = max(0, active_count - args.active_slot_cap)
    fields.update({
        "active_slots": active_count,
        "active_slot_cap": args.active_slot_cap,
        "active_slot_excess": active_excess,
        "dead_owner_slots": len(dead_owner),
        "guarded_live_owner_slots": len(live_owner_slots),
    })
    findings: list[str] = []
    if active_excess:
        findings.append(
            f"ACTIVE SLOT CAP BREACH: {active_count} active slot(s) against cap "
            f"{args.active_slot_cap} (excess {active_excess})"
        )
    if dead_owner:
        findings.append(
            f"{len(dead_owner)} slot(s) have a validate-lock-guarded owner proven dead "
            f"and no live process using the slot: "
            + ", ".join(item.key for item in dead_owner)
        )
    if findings:
        state = "actionable"
        summary = "; ".join([*findings, summary])

    if args.json:
        print(json.dumps({
            "state": state, "summary": summary, **fields,
            "new_reclaimable": sorted(fresh),
            "dead_owner": [asdict(item) for item in dead_owner],
            "slots": [r.to_dict() for r in results],
        }, indent=2, sort_keys=True))
    else:
        print(f"state={state}")
        print(f"summary={summary}")
        for key, value in fields.items():
            print(f"{key}={value}")
        for r in results:
            if r.reclaimable or r.procs == 0:
                mark = "RECLAIMABLE" if r.reclaimable else "idle       "
                new = " NEW" if r.slot in fresh else ""
                print(f"slot={r.slot} {mark}{new} {r.reason}")
        for item in dead_owner:
            print(f"slot={item.key} DEAD_OWNER {item.evidence}")

    return 1 if (args.gate and (fresh or active_excess or dead_owner)) else 0


if __name__ == "__main__":
    raise SystemExit(main())
