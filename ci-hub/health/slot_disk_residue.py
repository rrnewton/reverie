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
and unknown is never converted to dead.  Unknown ALSO suppresses reclaim: a held
slot the authority cannot assess used to fall through both the dead finding and
the alive suppression and reach the candidate list on idleness alone, which is
how a live owner's slot could be named reclaimable.  It is now named as its own
finding instead, because an absent signal reads exactly like a healthy one.

The blast radii are not symmetric.  Every finding therefore authorizes nothing:

* cap breach: active registry rows exceed the hard count, independent of age;
* dead owner: exact guarded identity is dead and no live process uses the slot;
* unassessable owner: a held slot whose guarded identity answers neither dead
  nor alive, so it can be neither reclaimed here nor released by the authority;
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

3. SIZE IS MEASURED NOW, ONCE FOR THE COMPLETE CANDIDATE SET. Per-slot tree
   sizes cannot be added on btrfs: compression makes `du` count the wrong bytes,
   `--one-file-system` skips nested subvolumes, and reflinks make the same extent
   appear under several slots. `compsize` receives every candidate path in one
   invocation, so shared extents inside that set are counted once. Its disk-usage
   result remains an upper bound because protected paths outside the set may
   still reference an extent. The report says that and refuses any measurement
   larger than the filesystem instead of printing an impossible number.

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

import btrfs_headroom
import residue_sweep

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from worktree_registry import (  # noqa: E402
    RegistrySchemaError,
    read_registry as read_typed_registry,
)

SLOTS_DIR = Path("worktrees/slots")
DEFAULT_STATE = Path(".tick-hub") / "slot-disk-residue-state.json"

# Defaults. A slot must be sustained-idle, and the complete unprotected set must
# be materially large: a few 40 MiB directories are not a disk problem and
# paging about them is noise.
DEFAULT_IDLE_HOURS = 24.0
DEFAULT_MIN_GIB = 1.0
DEFAULT_SIZE_TIMEOUT = 5
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
        if (cand / SLOTS_DIR).is_dir():
            return cand
    raise ResidueUnavailable(f"no {SLOTS_DIR}/ directory found above {here}")


def slots_on_disk(root: Path) -> set[str]:
    """Slot directories that actually exist and therefore actually cost space.

    The FILESYSTEM is the authority here, not `worktree-state.json`. A registry
    can claim a slot that was deleted (costing nothing) and can omit a slot that
    exists (costing plenty); only one of those two is a disk problem, and it is
    the one the registry cannot see.
    """
    wt = root / SLOTS_DIR
    try:
        return {p.name for p in wt.iterdir() if p.is_dir() and not p.is_symlink()}
    except OSError as exc:
        raise ResidueUnavailable(f"cannot enumerate {wt}: {exc}") from exc


def read_registry(path: Path) -> dict[str, object]:
    try:
        value = read_typed_registry(path)
    except RegistrySchemaError as exc:
        raise ResidueUnavailable(f"cannot read slot registry {path}: {exc}") from exc
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


def quarantined_slot_names(registry: dict[str, object]) -> set[str]:
    """Rows retired from allocation but retained visibly and non-destructively.

    Quarantine is deliberately not active-cap occupancy, but it is never hidden
    from the report. The exact values are owned by the shared registry type;
    `lease-quarantined` is the current releaser state and
    `quarantined-retired` is retained for historical rows.
    """

    raw_slots = registry.get("slots")
    if not isinstance(raw_slots, dict):
        raise ResidueUnavailable("slot registry slots is not an object")
    return {
        str(name)
        for name, raw in raw_slots.items()
        if isinstance(raw, dict)
        and raw.get("status") in {"lease-quarantined", "quarantined-retired"}
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
    prefix = str(root / SLOTS_DIR) + os.sep
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


@dataclass(frozen=True)
class CandidateSetMeasurement:
    size_upper_bound_bytes: int | None
    device_total_bytes: int | None
    data_headroom_bytes: int | None
    error: str | None = None


def measure_candidate_set(
    root: Path, slots: list[str], timeout: int = DEFAULT_SIZE_TIMEOUT
) -> CandidateSetMeasurement:
    """Measure all candidate paths together, never by adding per-slot sizes.

    ``compsize`` counts a reflink-shared extent once when all paths are supplied
    in one invocation. Its Disk Usage column is post-compression, unlike
    ``du``'s value on this filesystem. It is still an upper bound on bytes freed:
    an extent can also be referenced by a protected path outside this set.

    Filesystem capacity and data headroom come from btrfs's allocator counters.
    If either source is unavailable, or the path-set result exceeds the entire
    filesystem, the byte figure is refused rather than guessed.
    """
    try:
        headroom = btrfs_headroom.measure(str(root))
    except (btrfs_headroom.NotBtrfs, OSError, ValueError) as exc:
        return CandidateSetMeasurement(
            None, None, None, f"btrfs allocator accounting unavailable: {exc}"
        )

    if not slots:
        return CandidateSetMeasurement(
            0,
            headroom.device_total,
            headroom.pool_headroom("data"),
        )

    paths = [str(root / SLOTS_DIR / slot) for slot in sorted(slots)]
    try:
        out = subprocess.run(
            ["compsize", "--bytes", *paths],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except FileNotFoundError:
        return CandidateSetMeasurement(
            None,
            headroom.device_total,
            headroom.pool_headroom("data"),
            "compsize is not installed",
        )
    except subprocess.TimeoutExpired:
        return CandidateSetMeasurement(
            None,
            headroom.device_total,
            headroom.pool_headroom("data"),
            f"compsize exceeded its {timeout}s limit",
        )
    except OSError as exc:
        return CandidateSetMeasurement(
            None,
            headroom.device_total,
            headroom.pool_headroom("data"),
            f"compsize could not run: {exc}",
        )

    blob = out.stdout + out.stderr
    if out.returncode != 0:
        if "No files." in blob or "All empty or still-delalloced files" in blob:
            size = 0
        else:
            detail = " ".join(blob.split()) or f"exit {out.returncode}"
            return CandidateSetMeasurement(
                None,
                headroom.device_total,
                headroom.pool_headroom("data"),
                f"compsize failed: {detail}",
            )
    else:
        totals = [line.split() for line in out.stdout.splitlines()
                  if line.split() and line.split()[0] == "TOTAL"]
        if len(totals) != 1 or len(totals[0]) < 3:
            return CandidateSetMeasurement(
                None,
                headroom.device_total,
                headroom.pool_headroom("data"),
                "compsize did not emit exactly one TOTAL row",
            )
        try:
            size = int(totals[0][2])
        except ValueError:
            return CandidateSetMeasurement(
                None,
                headroom.device_total,
                headroom.pool_headroom("data"),
                "compsize TOTAL disk-usage value is not an integer",
            )

    if size < 0:
        return CandidateSetMeasurement(
            None,
            headroom.device_total,
            headroom.pool_headroom("data"),
            "compsize reported a negative candidate-set size",
        )
    if size > headroom.device_total:
        return CandidateSetMeasurement(
            None,
            headroom.device_total,
            headroom.pool_headroom("data"),
            "candidate-set size exceeds the filesystem capacity",
        )
    return CandidateSetMeasurement(
        size,
        headroom.device_total,
        headroom.pool_headroom("data"),
    )


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
    reclaimable: bool
    reason: str

    def to_dict(self) -> dict:
        return dict(self.__dict__)


def evaluate(
    root: Path,
    prior: dict,
    now: datetime,
    idle_hours: float,
) -> tuple[list[SlotResidue], dict, Occupancy]:
    """Classify every on-disk slot from current process facts and idle history."""
    on_disk = slots_on_disk(root)
    occ = occupancy(root)
    prior_slots = prior.get("slots") if isinstance(prior.get("slots"), dict) else {}

    next_slots: dict[str, dict] = {}
    for slot in sorted(on_disk):
        procs = occ.counts.get(slot, 0)
        rec = prior_slots.get(slot) if isinstance(prior_slots.get(slot), dict) else {}
        if procs > 0:
            # Any live process resets the clock outright. Positive evidence of use
            # always wins; the cost of a false "idle" is somebody's work listed
            # for deletion, the cost of a false "busy" is one more cycle of disk.
            next_slots[slot] = {}
            continue
        since = parse_ts(rec.get("idle_since")) or now
        # Deliberately discard legacy size_bytes/sized_at state. A cached byte
        # count survived the files it described and was repeatedly presented as
        # current. Byte accounting is now one fresh measurement over the final
        # candidate set, after owner suppression.
        next_slots[slot] = {"idle_since": fmt_ts(since)}

    results: list[SlotResidue] = []
    for slot in sorted(on_disk):
        rec = next_slots[slot]
        procs = occ.counts.get(slot, 0)
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
        else:
            reclaimable = True
            reason = f"idle {age_h:.1f}h, no process inside"

        results.append(SlotResidue(
            slot=slot, procs=procs,
            idle_since=rec.get("idle_since"),
            idle_hours=round(age_h, 2) if age_h is not None else None,
            reclaimable=reclaimable, reason=reason,
        ))

    next_state = {"slots": next_slots}
    return results, next_state, occ


def suppress_undecided_owners(
    results: list[SlotResidue],
    live_owner_slots: set[str],
    unassessable_owner_slots: set[str],
) -> None:
    """Take slots OFF the candidate list when owner liveness is not "proven dead".

    SHRINK-ONLY BY CONSTRUCTION, and that is the entire safety argument.  Every
    branch below assigns ``reclaimable = False``; none can assign ``True``.  So
    for any input, the candidate set afterwards is a subset of the candidate set
    before, whatever the two predicates contain -- including if they are wrong.

    That property is worth more than the predicates being correct.  A change that
    is safe only when it is right has to be right.  If either predicate
    over-matches here the cost is disk left unreclaimed, reported as a named
    finding; there is no input for which it costs somebody's uncommitted work.
    The reverse design -- inferring reclaimability from an absent verdict -- has
    no such floor, and is the behaviour this replaces.

    It is a separate function, not an inline loop, so the property can be
    asserted directly rather than argued for in a comment.
    """

    for result in results:
        if result.slot in live_owner_slots:
            result.reclaimable = False
            result.reason = (
                "slot-owner-sidecar registered owner is alive; "
                "idle time cannot qualify this slot"
            )
        elif result.slot in unassessable_owner_slots:
            result.reclaimable = False
            result.reason = (
                "owner liveness is INDETERMINATE and cannot be assessed; "
                "idle time cannot qualify this slot"
            )


def apply_size_threshold(
    results: list[SlotResidue],
    measurement: CandidateSetMeasurement,
    min_gib: float,
) -> int:
    """Apply one current size threshold to the complete unprotected set.

    Returns the number of slots in the set before the size decision. A missing
    or impossible measurement removes every slot from the reclaim list: unknown
    size must never be converted into a positive reclamation claim.
    """
    candidates = [r for r in results if r.reclaimable]
    if not candidates or min_gib <= 0:
        return len(candidates)

    if measurement.size_upper_bound_bytes is None:
        for result in candidates:
            result.reclaimable = False
            result.reason += "; current candidate-set size unavailable; not flagged"
        return len(candidates)

    measured_gib = measurement.size_upper_bound_bytes / GIB
    if measurement.size_upper_bound_bytes < min_gib * GIB:
        for result in candidates:
            result.reclaimable = False
            result.reason += (
                f"; complete candidate set is up to {measured_gib:.2f} GiB "
                f"(< {min_gib} GiB threshold)"
            )
    else:
        for result in candidates:
            result.reason += "; included in the current candidate-set measurement"
    return len(candidates)


def summarize(
    results: list[SlotResidue],
    occ: Occupancy,
    fresh: set[str],
    min_gib: float,
    measured_candidates: int,
    measurement: CandidateSetMeasurement,
    measurement_required: bool,
) -> tuple[str, str, dict]:
    flagged = [r for r in results if r.reclaimable]
    fields = {
        "slots_on_disk": len(results),
        "slots_occupied": sum(1 for r in results if r.procs > 0),
        "slots_idle": sum(1 for r in results if r.procs == 0),
        "idle_unprotected_slots": measured_candidates,
        "reclaimable_slots": len(flagged),
        "new_reclaimable_slots": len(fresh),
        "blind_pids": occ.blind_pids,
        "zombie_pids": occ.zombie_pids,
        "deleted_cwd_pids": occ.deleted_cwd_pids,
    }
    if measurement.device_total_bytes is not None:
        fields["filesystem_device_gib"] = round(measurement.device_total_bytes / GIB, 2)
    if measurement.data_headroom_bytes is not None:
        fields["filesystem_data_headroom_gib"] = round(
            measurement.data_headroom_bytes / GIB, 2
        )
    if measurement.size_upper_bound_bytes is not None:
        fields["idle_unprotected_gib_upper_bound"] = round(
            measurement.size_upper_bound_bytes / GIB, 2
        )
        fields["idle_unprotected_size_status"] = "current"
    elif measurement_required:
        fields["idle_unprotected_size_status"] = UNKNOWN
        fields["idle_unprotected_size_error"] = (
            measurement.error or "measurement unavailable"
        )
    else:
        fields["idle_unprotected_size_status"] = "not-requested"

    measurement_unknown = (
        measurement_required
        and measured_candidates > 0
        and measurement.size_upper_bound_bytes is None
    )
    if measurement_unknown:
        state = UNKNOWN
    elif fresh:
        state = RECLAIMABLE
    else:
        state = OK
    headroom = (
        f" Current data headroom is {measurement.data_headroom_bytes / GIB:.1f} GiB."
        if measurement.data_headroom_bytes is not None
        else ""
    )

    if measurement_unknown:
        summary = (
            f"{measured_candidates} idle unprotected slot(s), but their current set size "
            f"is unavailable: {measurement.error or 'measurement unavailable'}. "
            "No reclaimable byte figure is reported."
            + headroom
        )
    elif fresh:
        named = ", ".join(r.slot for r in flagged if r.slot in fresh)
        size = measurement.size_upper_bound_bytes
        measured = (
            f"currently reference up to {size / GIB:.2f} GiB as one measured set. "
            "This is an upper bound: extents shared with protected paths may not be freed. "
            if size is not None
            else "were not measured because the byte threshold is disabled. "
        )
        summary = (
            f"{len(fresh)} slot(s) newly idle >= threshold: {named}; "
            f"{len(flagged)} reclaim candidate(s) {measured}"
            f"DETECT ONLY -- release-worktree.rs is the authority and may still "
            f"refuse; reclaim needs a recovery SHA."
            + headroom
        )
    elif flagged:
        size = measurement.size_upper_bound_bytes
        measured = (
            f"currently reference up to {size / GIB:.2f} GiB as one measured set. "
            "This is an upper bound: extents shared with protected paths may not be freed. "
            if size is not None
            else "were not measured because the byte threshold is disabled. "
        )
        summary = (
            f"no newly reclaimable slot; {len(flagged)} standing reclaim CANDIDATE(s) "
            f"{measured}Standing backlog, not paged."
            + headroom
        )
    elif measured_candidates and measurement.size_upper_bound_bytes is not None:
        summary = (
            f"no reclaim candidate: {measured_candidates} idle unprotected slot(s) "
            f"currently reference up to {measurement.size_upper_bound_bytes / GIB:.2f} GiB "
            f"as one measured set (< {min_gib} GiB threshold)."
            + headroom
        )
    else:
        summary = (
            f"no reclaim candidate among {fields['slots_on_disk']} slot(s) on disk "
            f"({fields['slots_occupied']} occupied)."
            + headroom
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
    p.add_argument(
        "--min-gib",
        type=float,
        default=DEFAULT_MIN_GIB,
        help="minimum current upper bound for the complete idle unprotected set",
    )
    p.add_argument(
        "--size-timeout",
        "--du-timeout",
        dest="size_timeout",
        type=int,
        default=DEFAULT_SIZE_TIMEOUT,
        help=(
            "seconds allowed for the one current compsize measurement "
            f"(default {DEFAULT_SIZE_TIMEOUT}; --du-timeout is a compatibility alias)"
        ),
    )
    # Accepted so an older installed tick configuration fails safe during a
    # rolling update. They no longer alter behavior: cached sizes and partial
    # refreshes are the defect this version removes.
    p.add_argument("--refresh-hours", help=argparse.SUPPRESS)
    p.add_argument("--max-measure", help=argparse.SUPPRESS)
    p.add_argument("--no-measure", action="store_true",
                   help="skip size and filesystem-headroom measurement")
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
        help="test-only fixture for guarded per-slot owner-authority JSON",
    )
    p.add_argument("--dry-run", action="store_true",
                   help="do not persist state (idle clocks and baseline stay unchanged)")
    args = p.parse_args(argv)

    if args.active_slot_cap < 0:
        p.error("--active-slot-cap must be non-negative")
    if args.idle_hours < 0:
        p.error("--idle-hours must be non-negative")
    if args.min_gib < 0:
        p.error("--min-gib must be non-negative")
    if args.size_timeout <= 0:
        p.error("--size-timeout must be positive")

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
        quarantined_slots = quarantined_slot_names(registry)
        active_count = len(active_slots)
        active_excess = max(0, active_count - args.active_slot_cap)
        try:
            if disk_only_fixture and args.test_owner_authority_json is None:
                authorities: dict[str, dict[str, object]] = {}
            else:
                authorities = residue_sweep.read_guarded_owner_authorities(
                    registry,
                    root,
                    Path(args.test_owner_authority_json)
                    if args.test_owner_authority_json
                    else None,
                )
        except residue_sweep.SweepUnavailable as exc:
            # Owner liveness is independent of aggregate capacity. If its
            # guarded snapshot cannot complete within its internal bound, keep
            # the known cap breach visible rather than letting the outer 30s
            # runner erase every field. Never infer an owner verdict here.
            summary_parts = []
            if active_excess:
                summary_parts.append(
                    f"ACTIVE SLOT CAP BREACH: {active_count} active slot(s) against "
                    f"cap {args.active_slot_cap} (excess {active_excess})"
                )
            summary_parts.append(
                "guarded owner census unavailable; owner state remains INDETERMINATE: "
                + " ".join(str(exc).split())
            )
            state = "actionable" if active_excess else UNKNOWN
            fields = {
                "active_slots": active_count,
                "active_slot_cap": args.active_slot_cap,
                "active_slot_excess": active_excess,
                "quarantined_slots": len(quarantined_slots),
                "owner_census_status": "indeterminate",
                "dead_owner_slots": 0,
                "guarded_live_owner_slots": 0,
                "unassessable_owner_slots": 0,
            }
            summary = "; ".join(summary_parts)
            if args.json:
                print(json.dumps({"state": state, "summary": summary, **fields},
                                 indent=2, sort_keys=True))
            else:
                print(f"state={state}")
                print(f"summary={summary}")
                for key, value in fields.items():
                    print(f"{key}={value}")
            return 1 if active_excess else 2
        prior = read_state(state_path)
        now = datetime.now(timezone.utc)
        results, next_state, occ = evaluate(root, prior, now, idle_hours=args.idle_hours)
        dead_owner = residue_sweep.guarded_dead_owner_slots(
            registry, authorities, set(occ.counts)
        )
        live_owner_slots = residue_sweep.guarded_live_owner_slots(
            registry, authorities
        )
        unassessable_owner_slots = residue_sweep.guarded_unassessable_owner_slots(
            registry, authorities
        )
    except ResidueUnavailable as exc:
        # Never silent: an unreadable input is `unknown`, which is not `ok`.
        print(f"state={UNKNOWN}")
        print(f"summary=slot disk residue unavailable: {exc}")
        return 2

    suppress_undecided_owners(results, live_owner_slots, unassessable_owner_slots)

    candidate_names = [r.slot for r in results if r.reclaimable]
    measurement_required = bool(candidate_names) and args.min_gib > 0
    if args.no_measure:
        measurement = CandidateSetMeasurement(
            None,
            None,
            None,
            "measurement disabled by --no-measure" if measurement_required else None,
        )
    else:
        measurement = measure_candidate_set(root, candidate_names, args.size_timeout)
    measured_candidates = apply_size_threshold(results, measurement, args.min_gib)

    current = {r.slot for r in results if r.reclaimable}
    reported = prior.get("reported")
    if measurement_required and measurement.size_upper_bound_bytes is None:
        # Do not acknowledge an unmeasured candidate set. If measurement works
        # next tick, every newly qualifying slot must still be able to page. Do
        # forget previously reported slots that are no longer candidates, so a
        # later idle period is a new event rather than hidden by old state.
        fresh = set()
        previous = set(reported) if isinstance(reported, list) else set()
        next_state["reported"] = sorted(previous & set(candidate_names))
    elif isinstance(reported, list):
        # Only slots that were ALREADY reclaimable are suppressed. A slot that
        # gets used again and later goes idle again must page again, so this is
        # the current set, never a cumulative union.
        fresh = current - set(reported)
        next_state["reported"] = sorted(current)
    else:
        # First run adopts the baseline WITHOUT paging: the standing backlog did
        # not happen at this tick and must not be reported as if it had.
        fresh = set()
        next_state["reported"] = sorted(current)

    if not args.dry_run:
        write_state(state_path, next_state)

    state, summary, fields = summarize(
        results,
        occ,
        fresh,
        args.min_gib,
        measured_candidates,
        measurement,
        measurement_required,
    )

    fields.update({
        "active_slots": active_count,
        "active_slot_cap": args.active_slot_cap,
        "active_slot_excess": active_excess,
        "quarantined_slots": len(quarantined_slots),
        "owner_census_status": "complete",
        "dead_owner_slots": len(dead_owner),
        "guarded_live_owner_slots": len(live_owner_slots),
        "unassessable_owner_slots": len(unassessable_owner_slots),
    })
    findings: list[str] = []
    if active_excess:
        findings.append(
            f"ACTIVE SLOT CAP BREACH: {active_count} active slot(s) against cap "
            f"{args.active_slot_cap} (excess {active_excess})"
        )
    if dead_owner:
        findings.append(
            f"{len(dead_owner)} slot(s) have a sidecar-bound owner proven dead "
            f"and no live process using the slot: "
            + ", ".join(item.key for item in dead_owner)
        )
    if unassessable_owner_slots:
        # NOT A SILENCE. A held slot the authority cannot assess is invisible to
        # both the dead-owner finding and the alive-owner suppression, so before
        # this it left no trace at all -- and an absent signal reads exactly like
        # a healthy one. It is reported as a finding for the same reason a cap
        # breach is: the condition holds until somebody changes something.
        findings.append(
            f"{len(unassessable_owner_slots)} held slot(s) CANNOT BE ASSESSED: "
            f"the guarded authority answers indeterminate, so this gate cannot "
            f"name them reclaimable and release-worktree.rs will refuse them; "
            f"they are neither usable nor recoverable until their owner evidence "
            f"is restored by the allocator: "
            + ", ".join(sorted(unassessable_owner_slots))
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

    # An unassessable held slot fails the gate for the same reason a cap breach
    # does: `state` is already `actionable` once it is a finding, and a gate whose
    # exit status disagrees with its own printed state is worse than either
    # answer alone. It stays failed while the condition holds -- the dial that
    # clears it is restoring owner evidence in the allocator, not reclassifying
    # the finding here.
    if not args.gate:
        return 0
    if fresh or active_excess or dead_owner or unassessable_owner_slots:
        return 1
    return 2 if state == UNKNOWN else 0


if __name__ == "__main__":
    raise SystemExit(main())
