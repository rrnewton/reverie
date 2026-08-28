#!/usr/bin/env python3
"""Typed liveness for a held resource, and a fail-closed release verdict.

WHY. Measured 2026-08-07: `worktree-state.json` held 96 slot records without a
TTL owner lease. So a recycled agent released nothing, and a dead owner's
ACTIVE slot was indistinguishable from a live one. The later
`owner_sidecar.lease` supplies that TTL. `coordinator_lease` is a different
artifact: it records one exact systemd sentinel incarnation and has no
`expires_at`; treating it as the TTL lease would manufacture liveness from a
field its producer never writes.

`residue_sweep.py` copes by INFERRING death from two external signals — absence
from the live ORC fleet, and no same-user process cwd under the slot. That
inference is why it needs two signals and why it can only ever say "I cannot
find the owner", never "the owner is gone". This module makes the fact
queryable instead: if a record carries a lease, liveness is read directly; if
not, the two-signal inference is preserved but its result is typed UNKNOWN
rather than silently promoted to DEAD.

RELEASE IS FAIL-CLOSED, AND THAT IS NOT THEORETICAL. Of the 60 slots the sweep
flagged dead-owner, **21 hold uncommitted changes and 3 carry abandoned merge
conflicts** (measured same day). A reaper that acted on "owner not found" alone
would have destroyed work in a third of them. Hard Invariant 14: never remove a
dirty slot until its state has a documented recovery SHA. So every refusal
below is a measured hazard, not a hypothetical one.

Pure functions over already-collected facts: no subprocess, no filesystem, no
ORC call. The caller gathers evidence; this module only judges it.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Mapping, Sequence


class Liveness(Enum):
    ALIVE = "alive"
    """Positively observed: an unexpired lease, or a live process, or fleet membership."""

    DEAD = "dead"
    """Positively observed absent: an EXPIRED lease. Only a lease can prove this."""

    UNKNOWN = "unknown"
    """Cannot find the owner. NOT the same as dead — this is the un-bound case."""


class Release(Enum):
    RELEASE = "release"
    HOLD_ALIVE = "hold-alive"
    REFUSE_UNKNOWN = "refuse-unknown"
    REFUSE_DIRTY = "refuse-dirty"
    REFUSE_UNPUBLISHED = "refuse-unpublished"


@dataclass(frozen=True)
class Verdict:
    liveness: Liveness
    release: Release
    reasons: tuple[str, ...] = field(default_factory=tuple)

    @property
    def may_release(self) -> bool:
        return self.release is Release.RELEASE


def holder_liveness(
    record: Mapping[str, Any],
    *,
    now: int,
    live_agents: Sequence[str],
    processes_under_slot: int,
) -> Liveness:
    """Liveness of the holder of one slot record.

    A LEASE is authoritative in both directions — it is the binding this schema
    lacks today. Without one we fall back to the two external signals, and the
    best they can ever justify is UNKNOWN.
    """
    # POSITIVE EVIDENCE OF LIFE OUTRANKS ANY BOOKKEEPING RECORD, including an
    # expired lease. A lease expiring does not kill the process it describes —
    # it only means nobody renewed a row. The first draft of this function
    # checked the lease first and returned DEAD for an expired-lease slot that
    # still had a running process in it; its own bracket caught that, and the
    # consequence would have been releasing a slot out from under live work.
    # This is also the documented "detached+clean slot can still be busy" mode.
    if processes_under_slot > 0:
        return Liveness.ALIVE
    holders = record.get("agents") or []
    if isinstance(holders, str):
        holders = [holders]
    if any(a in live_agents for a in holders):
        return Liveness.ALIVE

    sidecar = record.get("owner_sidecar")
    lease = sidecar.get("lease") if isinstance(sidecar, Mapping) else None
    if isinstance(lease, Mapping):
        expires = lease.get("expires_at")
        if isinstance(expires, int):
            # No live signal contradicts it, so the lease is decisive either way.
            return Liveness.ALIVE if expires > now else Liveness.DEAD

    # No lease and no signal found the owner. That is not proof of death.
    return Liveness.UNKNOWN


def release_verdict(
    record: Mapping[str, Any],
    *,
    now: int,
    live_agents: Sequence[str],
    processes_under_slot: int,
    dirty_files: int,
    unmerged_files: int = 0,
    unpublished_commits: int = 0,
    recovery_sha: str | None = None,
) -> Verdict:
    """Whether this slot may be released. Every clause must permit it."""
    liveness = holder_liveness(
        record, now=now, live_agents=live_agents,
        processes_under_slot=processes_under_slot,
    )
    reasons: list[str] = []

    if liveness is Liveness.ALIVE:
        return Verdict(liveness, Release.HOLD_ALIVE, ("holder is alive",))

    # Work-preservation clauses are checked BEFORE the unknown-holder clause, so
    # the report names the data hazard even when the holder is merely unknown.
    if dirty_files > 0 and not recovery_sha:
        reasons.append(
            f"{dirty_files} uncommitted file(s) and no recovery SHA (Invariant 14)"
        )
        if unmerged_files:
            reasons.append(f"{unmerged_files} unmerged path(s): an abandoned merge")
        return Verdict(liveness, Release.REFUSE_DIRTY, tuple(reasons))

    if unpublished_commits > 0:
        return Verdict(
            liveness,
            Release.REFUSE_UNPUBLISHED,
            (f"{unpublished_commits} commit(s) not on any remote",),
        )

    if liveness is Liveness.UNKNOWN:
        return Verdict(
            liveness,
            Release.REFUSE_UNKNOWN,
            ("holder not found, but absence of evidence is not proof of death; "
             "a lease would make this decidable",),
        )

    return Verdict(liveness, Release.RELEASE, ("lease expired; nothing to preserve",))
