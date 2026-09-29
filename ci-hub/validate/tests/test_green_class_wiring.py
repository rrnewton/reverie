#!/usr/bin/env python3
"""Brackets for the green-class clause WIRED into the qualified-rows view.

`green_class.derive_class` had real logic and ZERO consumers -- it could not fire
in production (see ai_docs/phase2-tightening-guards-adversarial-review-20260805.md).
It is now a clause in `qualified_rows.is_qualified`, which IS wired
(`ci-hub/ci-hub.rs` -> `qualified-rows`).

Both directions are bracketed here, because a guard that only ever refuses is as
broken as one that only ever accepts:

  NEGATIVE -- a SOFT (inherited) green, and a row whose `green_class` LABEL has
              been laundered to disagree with its provenance, are refused.
  POSITIVE -- rows that predate the provenance fields (every row in the ledger
              today) still qualify, so wiring the clause rejects no existing
              producer. This is the fleet-flag-day failure mode the schema
              transition rule exists to prevent.
"""

from __future__ import annotations

from pathlib import Path
import sys

CI_HUB = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(CI_HUB / "validate"))
sys.path.insert(0, str(CI_HUB / "ledger"))
import green_class as gc  # noqa: E402
import qualified_rows as qr  # noqa: E402
import validate_rows as canonical_ledger  # noqa: E402

HEAD = "a" * 40
ANCESTOR = "b" * 40


def _row(name: str = HEAD, finished: str = "2026-08-04T10:00:00Z", **over: object) -> dict:
    row: dict[str, object] = {
        "commit": name,
        "finished_at": finished,
        # The oldest ledger schema: pre-count, pre-coverage, and one a writer
        # emitted before the release-builder pair existed.
        "schema_version": 1,
        "result": "pass",
        "executed_tests": 10,
        "gates_run": 5,
        "gates_expected": 5,
    }
    row.update(over)
    return row


# --- POSITIVE CONTROL -------------------------------------------------------


def test_row_predating_the_provenance_fields_still_qualifies() -> None:
    """Version-aware default: absent `validated_head_sha` means "ran here"."""
    row = _row()
    assert "validated_head_sha" not in row
    assert gc.derive_class(row)[0] == gc.HARD
    assert qr.is_qualified(row) is True


def test_explicit_hard_provenance_qualifies() -> None:
    row = _row(validated_head_sha=HEAD)
    assert gc.derive_class(row)[0] == gc.HARD
    assert qr.is_qualified(row) is True


# --- NEGATIVE: the planted violations ---------------------------------------


def test_soft_inherited_green_does_not_qualify() -> None:
    """Validation ran on an ANCESTOR; it is not a measurement at this head."""
    row = _row(
        validated_head_sha=ANCESTOR,
        inherited_from={"delta_kind": gc.DELTA_REBASE_ONLY, "branch_commits": 0},
    )
    derived, _reason = gc.derive_class(row)
    # Assert the SPECIFIC soft class, not merely "not hard": a fixture that is
    # refused as MALFORMED would also satisfy `!= HARD` and would make this
    # bracket vacuous -- it would stop proving that a genuine soft green is
    # excluded.
    assert derived == gc.SOFT_REBASE_ONLY, (derived, _reason)
    assert qr.is_qualified(row) is False


def test_every_soft_class_is_excluded() -> None:
    """Each soft class, and the not-green class, is refused by the wiring."""
    cases = {
        gc.SOFT_REBASE_ONLY: {"delta_kind": gc.DELTA_REBASE_ONLY, "branch_commits": 0},
        gc.SOFT_UPSTREAM_DELTA: {
            "delta_kind": gc.DELTA_REBASE_PLUS_UPSTREAM,
            "branch_commits": 0,
            "upstream_commits": 3,
        },
        gc.SOFT_FORCE_FULL: {
            "delta_kind": gc.DELTA_REBASE_PLUS_UPSTREAM,
            "branch_commits": 0,
            "upstream_commits": 3,
            "force_full_paths": ["validate.sh"],
        },
    }
    for expected_class, inherited in cases.items():
        row = _row(validated_head_sha=ANCESTOR, inherited_from=inherited)
        derived, reason = gc.derive_class(row)
        assert derived == expected_class, (expected_class, derived, reason)
        assert qr.is_qualified(row) is False, expected_class


def test_branch_that_gained_commits_is_excluded() -> None:
    """Owner rule: add a commit and it is NEITHER hard nor soft."""
    row = _row(
        validated_head_sha=ANCESTOR,
        inherited_from={"delta_kind": gc.DELTA_NEW_BRANCH_COMMITS, "branch_commits": 2},
    )
    assert gc.derive_class(row)[0] == gc.NOT_GREEN
    assert qr.is_qualified(row) is False


def test_malformed_provenance_is_refused_clause_by_clause() -> None:
    """Brackets for the type-guards the review found UNBRACKETED.

    Each fixture is shaped so it reaches -- and is refused by -- exactly the
    clause named, rather than tripping an earlier one.
    """
    cases = [
        ("inherited_from is not an object", "rebase-only"),
        (
            "inherited_from.branch_commits must be a non-negative int",
            {"delta_kind": gc.DELTA_REBASE_ONLY, "branch_commits": "0"},
        ),
        (
            "inherited_from.branch_commits must be a non-negative int",
            {"delta_kind": gc.DELTA_REBASE_ONLY, "branch_commits": -1},
        ),
        (
            "inherited_from.branch_commits must be a non-negative int",
            {"delta_kind": gc.DELTA_REBASE_ONLY, "branch_commits": False},
        ),
        (
            "inherited_from.upstream_commits must be a non-negative int",
            {
                "delta_kind": gc.DELTA_REBASE_ONLY,
                "branch_commits": 0,
                "upstream_commits": False,
            },
        ),
        (
            "inherited_from.upstream_commits must be a non-negative int",
            {
                "delta_kind": gc.DELTA_REBASE_PLUS_UPSTREAM,
                "branch_commits": 0,
                "upstream_commits": True,
            },
        ),
        (
            "inherited_from.upstream_commits must be a non-negative int",
            {
                "delta_kind": gc.DELTA_REBASE_PLUS_UPSTREAM,
                "branch_commits": 0,
                "upstream_commits": 1 << 64,
            },
        ),
        (
            "inherited_from.force_full_paths must be a list of strings",
            {
                "delta_kind": gc.DELTA_REBASE_ONLY,
                "branch_commits": 0,
                "upstream_commits": 0,
                "force_full_paths": None,
            },
        ),
        (
            "inherited_from.force_full_paths must be a list of strings",
            {
                "delta_kind": gc.DELTA_REBASE_PLUS_UPSTREAM,
                "branch_commits": 0,
                "upstream_commits": 1,
                "force_full_paths": "validate.sh",
            },
        ),
        (
            "inherited_from.force_full_paths must be a list of strings",
            {
                "delta_kind": gc.DELTA_REBASE_PLUS_UPSTREAM,
                "branch_commits": 0,
                "upstream_commits": 1,
                "force_full_paths": ["validate.sh", 7],
            },
        ),
    ]
    for expected_reason, inherited in cases:
        row = _row(validated_head_sha=ANCESTOR, inherited_from=inherited)
        derived, reason = gc.derive_class(row)
        assert derived == gc.REFUSED, (inherited, derived)
        assert reason == expected_reason, (reason, expected_reason)
        assert qr.is_qualified(row) is False


def test_non_string_validated_head_is_refused_without_raising() -> None:
    row = _row(
        validated_head_sha=7,
        inherited_from={"delta_kind": gc.DELTA_REBASE_ONLY, "branch_commits": 0},
    )
    derived, reason = gc.derive_class(row)
    assert derived == gc.REFUSED
    assert reason == "validated_head_sha must be a string"
    assert qr.is_qualified(row) is False


def test_laundered_label_does_not_qualify() -> None:
    """A `green_class` label that disagrees with provenance is REFUSED.

    This is the attack the module exists to stop: stamping `green_class: "hard"`
    on an inherited row so it reads byte-identical to a real one.
    """
    row = _row(
        green_class=gc.HARD,
        validated_head_sha=ANCESTOR,
        inherited_from={"delta_kind": gc.DELTA_REBASE_ONLY, "branch_commits": 0},
    )
    derived, reason = gc.derive_class(row)
    assert derived == gc.REFUSED, (derived, reason)
    assert qr.is_qualified(row) is False


def test_contradictory_provenance_does_not_qualify() -> None:
    """Claims exact-head validation AND inheritance at once."""
    row = _row(validated_head_sha=HEAD, inherited_from={"kind": gc.DELTA_REBASE_ONLY})
    assert gc.derive_class(row)[0] == gc.REFUSED
    assert qr.is_qualified(row) is False


def test_row_without_commit_does_not_qualify() -> None:
    row = _row(commit="unknown")
    assert gc.derive_class(row)[0] == gc.REFUSED
    assert qr.is_qualified(row) is False


# --- THE CLAUSE IS LOAD-BEARING ---------------------------------------------


def test_soft_row_is_refused_ONLY_by_the_green_class_clause() -> None:
    """Guard against a vacuous bracket.

    The soft row above must satisfy EVERY other clause of `is_qualified`, so the
    refusal is attributable to the green-class clause and not to some incidental
    malformation in the fixture.
    """
    soft = _row(
        validated_head_sha=ANCESTOR,
        inherited_from={"delta_kind": gc.DELTA_REBASE_ONLY, "branch_commits": 0},
    )
    hard = dict(soft)
    hard.pop("validated_head_sha")
    hard.pop("inherited_from")
    # Identical in every other respect -- only the provenance differs.
    assert qr.is_qualified(hard) is True
    assert qr.is_qualified(soft) is False


# --- POPULATION INVARIANT (over-broad check) --------------------------------


def test_wiring_does_not_shrink_the_real_qualified_population() -> None:
    """No existing producer is rejected by the new clause.

    Every row in today's ledger predates the provenance fields, so all of them
    derive HARD and the qualified count must be unchanged by the wiring.
    """
    rows = canonical_ledger.rows()
    if not rows:
        return
    non_hard = [r for r in rows if gc.derive_class(r)[0] != gc.HARD]
    assert non_hard == [], f"{len(non_hard)} real rows would be newly refused"
