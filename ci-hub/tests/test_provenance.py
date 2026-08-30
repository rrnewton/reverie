#!/usr/bin/env python3
"""Brackets for the provenance emitters.

Every case below is a REAL incident from 2026-08-07, not a synthetic fixture --
each one actually produced a confident wrong reading before this module existed.
Both directions throughout: a clean artifact must render WITHOUT a warning, or
the warning carries no information.
"""

from __future__ import annotations

import sys
from pathlib import Path
from unittest import mock

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from provenance import (  # noqa: E402
    Provenance, StaleArtifact, query_provenance, require_fresh, stamp,
    tree_provenance,
)


# ---- the cap case: the subtlest, and the one no tool checked ---------------- #

def test_a_query_returning_exactly_its_limit_is_reported_as_truncated():
    """The real incident: --limit 400 returned 400 of 438; 38 issues unsearched."""
    p = query_provenance("gh issue list --state all", limit=400, returned=400)
    assert p.capped is True
    assert p.suspect
    assert "TRUNCATED" in p.render()
    assert "FROM ABSENCE is INVALID" in p.render()


def test_an_uncapped_query_is_clean_so_the_warning_means_something():
    """Positive control: the corrected run, 438 of a 1000 limit."""
    p = query_provenance("gh issue list --state all", limit=1000, returned=438)
    assert p.capped is False
    assert not p.suspect
    assert not p.render().startswith("WARNING")


def test_a_capped_query_is_fatal_to_require_fresh_regardless_of_allowance():
    """A cap has no safe reading, so allow_behind must not excuse it."""
    capped = query_provenance("q", limit=10, returned=10)
    with pytest.raises(StaleArtifact, match="TRUNCATED"):
        require_fresh(capped, allow_behind=10_000)


# ---- the behind/dirty cases ------------------------------------------------ #

def test_a_behind_binary_is_flagged():
    """The real incident: 23 commits behind -> 3 false nondeterminism verdicts."""
    p = Provenance("binary", "gf89c69766371", behind=23, dirty=True)
    assert p.suspect and p.render().startswith("WARNING")


def test_a_current_clean_binary_is_not_flagged():
    """Positive control: the head build that then passed 9/9."""
    p = Provenance("binary", "g294e89bfeeeb", behind=0, dirty=False)
    assert not p.suspect and not p.render().startswith("WARNING")


def test_being_behind_is_only_fatal_when_the_caller_says_so():
    """Behind-ness is usually benign; the caller decides its tolerance."""
    p = Provenance("tree", "543f3ec7f269", behind=317, dirty=False)
    require_fresh(p, allow_behind=1000)                     # tolerated
    with pytest.raises(StaleArtifact, match="317 commits behind"):
        require_fresh(p, allow_behind=0)                    # not tolerated


def test_an_unattributed_artifact_is_always_fatal():
    """UNKNOWN has no safe reading -- it is not 'probably fine'."""
    p = Provenance("binary", "UNKNOWN(unparsable-version)")
    with pytest.raises(StaleArtifact, match="UNATTRIBUTED"):
        require_fresh(p, allow_behind=10_000)


def test_partial_git_failure_is_unknown_not_current():
    """A readable HEAD does not make failed freshness and status reads current."""
    with mock.patch("provenance._git", side_effect=["abc123def456", None, None]):
        p = tree_provenance("/repo")
    assert p.behind is None
    assert p.dirty is None
    assert p.suspect
    assert "behind=unknown" in p.render()
    assert "dirty=unknown" in p.render()
    with pytest.raises(StaleArtifact, match="UNKNOWN"):
        require_fresh(p, allow_behind=10_000)


# ---- the stamp ------------------------------------------------------------- #

def test_stamp_emits_one_line_per_artifact():
    lines = stamp(
        Provenance("binary", "gabc", behind=0, dirty=False),
        query_provenance("q", limit=100, returned=5),
    ).splitlines()
    assert len(lines) == 2
    assert all(line.lstrip("WARNING ").startswith("provenance:") for line in lines)


def test_a_clean_run_emits_no_warning_at_all():
    """If everything is fresh the stamp must be quiet, or it becomes noise
    and the next real warning gets skimmed past."""
    out = stamp(
        Provenance("binary", "gabc", behind=0, dirty=False),
        Provenance("tree", "gdef", behind=0, dirty=False),
        query_provenance("q", limit=100, returned=5),
    )
    assert "WARNING" not in out
