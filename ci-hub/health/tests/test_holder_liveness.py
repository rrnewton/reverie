#!/usr/bin/env python3
"""Both-direction brackets for holder liveness and fail-closed release.

A reaper that never releases is useless; one that always releases destroys
work. So every refusal here is paired with a case proving the same clause can
permit a release.

The refusals are sized on measured reality, not imagination: of the 60 slots
the residue sweep flagged dead-owner on 2026-08-07, 21 held uncommitted changes
and 3 carried abandoned merge conflicts.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from holder_liveness import Liveness, Release, holder_liveness, release_verdict

NOW = 1_000_000
LEASED_LIVE = {
    "agents": ["hermit-w10"],
    "owner_sidecar": {"lease": {"expires_at": NOW + 600}},
}
LEASED_DEAD = {
    "agents": ["hermit-gone"],
    "owner_sidecar": {"lease": {"expires_at": NOW - 600}},
}
UNLEASED = {"agents": ["hermit-gone"]}
COORDINATOR_ONLY = {
    "agents": ["hermit-gone"],
    "coordinator_lease": {"expires_at": NOW - 600},
}

CLEAN = dict(now=NOW, live_agents=[], processes_under_slot=0,
             dirty_files=0, unpublished_commits=0)


# ------------------------------ liveness ---------------------------------- #

def test_unexpired_lease_is_alive():
    assert holder_liveness(LEASED_LIVE, now=NOW, live_agents=[],
                           processes_under_slot=0) is Liveness.ALIVE


def test_expired_lease_is_the_only_way_to_prove_death():
    assert holder_liveness(LEASED_DEAD, now=NOW, live_agents=[],
                           processes_under_slot=0) is Liveness.DEAD


def test_no_lease_and_no_signal_is_UNKNOWN_not_dead():
    """The whole point. Absence of evidence is not evidence of absence."""
    assert holder_liveness(UNLEASED, now=NOW, live_agents=[],
                           processes_under_slot=0) is Liveness.UNKNOWN


def test_coordinator_lease_is_not_mistaken_for_the_ttl_owner_lease():
    assert holder_liveness(
        COORDINATOR_ONLY,
        now=NOW,
        live_agents=[],
        processes_under_slot=0,
    ) is Liveness.UNKNOWN


def test_live_process_under_slot_keeps_it_alive_without_a_lease():
    """Guards the known 'detached+clean slot can still be busy' failure mode."""
    assert holder_liveness(UNLEASED, now=NOW, live_agents=[],
                           processes_under_slot=1) is Liveness.ALIVE


def test_fleet_membership_keeps_it_alive_without_a_lease():
    assert holder_liveness(UNLEASED, now=NOW, live_agents=["hermit-gone"],
                           processes_under_slot=0) is Liveness.ALIVE


# --------------------------- NEGATIVE: refusals ---------------------------- #

def test_alive_holder_is_never_released():
    v = release_verdict(LEASED_LIVE, **CLEAN)
    assert v.release is Release.HOLD_ALIVE and not v.may_release


def test_unknown_holder_is_never_released():
    v = release_verdict(UNLEASED, **CLEAN)
    assert v.liveness is Liveness.UNKNOWN
    assert v.release is Release.REFUSE_UNKNOWN and not v.may_release


def test_dirty_slot_is_refused_even_with_a_provably_dead_holder():
    """The measured hazard: 21 of 60 flagged slots were dirty."""
    v = release_verdict(LEASED_DEAD, **{**CLEAN, "dirty_files": 3})
    assert v.liveness is Liveness.DEAD
    assert v.release is Release.REFUSE_DIRTY and not v.may_release
    assert "Invariant 14" in " ".join(v.reasons)


def test_abandoned_merge_is_named_in_the_refusal():
    """3 of 60 carried unmerged paths with no merge in progress."""
    v = release_verdict(LEASED_DEAD,
                        **{**CLEAN, "dirty_files": 3, "unmerged_files": 3})
    assert v.release is Release.REFUSE_DIRTY
    assert "abandoned merge" in " ".join(v.reasons)


def test_unpublished_commits_refuse_even_when_clean():
    v = release_verdict(LEASED_DEAD, **{**CLEAN, "unpublished_commits": 40})
    assert v.release is Release.REFUSE_UNPUBLISHED and not v.may_release


# --------------------------- POSITIVE: it fires ---------------------------- #

def test_expired_lease_clean_slot_IS_released():
    """Not inert: the gate must be able to say yes."""
    v = release_verdict(LEASED_DEAD, **CLEAN)
    assert v.liveness is Liveness.DEAD
    assert v.release is Release.RELEASE and v.may_release


def test_a_recorded_recovery_sha_unblocks_a_dirty_slot():
    """Invariant 14 asks for a documented recovery SHA, not for never."""
    v = release_verdict(LEASED_DEAD,
                        **{**CLEAN, "dirty_files": 3, "recovery_sha": "a" * 40})
    assert v.release is Release.RELEASE and v.may_release


def test_each_blocker_alone_prevents_release():
    """Flip one condition at a time; each alone must stop the release."""
    assert release_verdict(LEASED_DEAD, **CLEAN).may_release is True
    for override in ({"dirty_files": 1}, {"unpublished_commits": 1},
                     {"processes_under_slot": 1}, {"live_agents": ["hermit-gone"]}):
        assert not release_verdict(LEASED_DEAD, **{**CLEAN, **override}).may_release, (
            f"{override} alone must prevent release"
        )
    # and an unleased record cannot be released at all, however clean
    assert not release_verdict(UNLEASED, **CLEAN).may_release


def test_the_lease_is_what_makes_the_fact_decidable():
    """Same slot, same evidence; only the lease differs. That is the fix."""
    assert not release_verdict(UNLEASED, **CLEAN).may_release       # UNKNOWN
    assert release_verdict(LEASED_DEAD, **CLEAN).may_release        # DEAD
