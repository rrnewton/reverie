#!/usr/bin/env python3
"""BRACKETS for the green-CLASS clause wired into the SHARED PREDICATE consumer.

Scope note: a concurrent agent wired the same `green_class.derive_class` into a
DIFFERENT consumer -- `qualified_rows.is_qualified` (reached via `ci-hub.rs ->
qualified-rows`), bracketed in `tests/test_green_class_wiring.py`. These two are
complementary, not duplicates: this file covers
`qualifying_receipt.row_qualifies` (reached via `history/query.py` and
`validation/publish_receipt.py`). Both delegate to the one derivation; neither
restates it. This file was renamed off the shared basename because two
`test_green_class_wiring.py` in different dirs without `__init__.py` collide
during pytest collection.

The review that prompted this recorded `green_class.py` as REAL logic with ZERO
consumers — "a bracketed classifier that no decision depends on cannot fail
closed on anything". These tests exist to prove that is no longer true, and they
deliberately go through `qualifying_receipt.row_qualifies` (the shared predicate
that `history/query.py` and `validation/publish_receipt.py` actually call) rather
than through `green_class` directly. Testing the classifier in isolation is
exactly the weakness the review named.

Every gate is bracketed both ways: the planted soft/refused row must be REFUSED,
and the legitimate hard population must still QUALIFY and be counted.
"""
from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path

HERE = Path(os.path.abspath(__file__)).parent
CI_HUB = HERE.parent
sys.path.insert(0, str(CI_HUB))
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(CI_HUB / "ledger"))

import green_class as G  # noqa: E402
import qualifying_receipt as Q  # noqa: E402
import validate_rows as V  # noqa: E402

SHA = "a" * 40


def qualifying_row(**over) -> dict:
    """A row that satisfies every VALUE clause of the shared predicate."""
    row = {
        "commit": SHA,
        "base_sha": SHA,
        "base_tree": "b" * 40,
        "reverie_base_sha": "c" * 40,
        "reverie_base_tree": "d" * 40,
        "commit_anchored": True,
        "tree_dirty": False,
        "selection_mode": "full",
        "profile": "full",
        "result": "pass",
        "failures": 0,
        "checks": 1,
        "gates_run": 1,
        "gates_expected": 1,
        "gates": [{"name": "test", "result": "pass", "exit_code": 0}],
        "executed_tests": 412,
        "passed_tests": 412,
        "schema_version": 5,
        "producer": "hermit-validate-sh",
        "admission": "ci-hub-validate-lock",
        "concurrent_validates": 0,
        "concurrency_proof": "validate_lock_owner_ancestry",
        "coverage": {"planned_test_nodes": 19, "zero_executed_nodes": [], "absent_nodes": []},
    }
    row.update(over)
    return row


def inherited(**over) -> dict:
    base = {
        "delta_kind": G.DELTA_REBASE_ONLY,
        "upstream_commits": 0,
        "branch_commits": 0,
        "patch_identical": True,
        "force_full_paths": [],
        "recorded_by": "test",
    }
    base.update(over)
    return base


class TheClauseIsWired(unittest.TestCase):
    """The point of the whole exercise: the predicate now consults the class."""

    def setUp(self):
        self.pred = Q.active()

    def test_the_policy_key_is_present_and_defaults_to_hard(self):
        self.assertEqual(self.pred.get("accepts_green_class"), ["hard"])

    def test_positive_control_a_hard_row_still_qualifies(self):
        """If this fails, the wiring broke landing for everyone."""
        self.assertTrue(Q.row_qualifies(qualifying_row(), SHA, self.pred))

    def test_planted_soft_row_is_REFUSED_by_the_shared_predicate(self):
        """Before the wiring this row qualified: it satisfies every value clause
        and differs only in provenance. That is the fake-green this closes."""
        soft = qualifying_row(validated_head_sha="b" * 40, inherited_from=inherited())
        self.assertEqual(Q.green_class_of(soft), G.SOFT_REBASE_ONLY)
        # the value clauses alone still accept it -- proving the class clause is
        # what refuses it, not some incidental field difference.
        self.assertTrue(Q._row_qualifies_without_class(soft, SHA, self.pred))
        self.assertFalse(Q.row_qualifies(soft, SHA, self.pred))

    def test_planted_refused_row_is_refused(self):
        """Soft claimed with no provenance to justify it."""
        bad = qualifying_row(validated_head_sha="b" * 40)
        self.assertEqual(Q.green_class_of(bad), G.REFUSED)
        self.assertTrue(Q._row_qualifies_without_class(bad, SHA, self.pred))
        self.assertFalse(Q.row_qualifies(bad, SHA, self.pred))

    def test_planted_label_forgery_is_refused(self):
        """A carry-forward writer stamping green_class=hard: the label is a cache,
        the provenance is the authority."""
        forged = qualifying_row(validated_head_sha="b" * 40,
                                inherited_from=inherited(), green_class="hard")
        self.assertFalse(Q.row_qualifies(forged, SHA, self.pred))

    def test_the_clause_only_NARROWS(self):
        """A row that fails a VALUE clause must stay refused and must not be
        rescued by being class-hard. Ordering: class is applied last."""
        not_full = qualifying_row(profile="portable-strict-compat-only")
        self.assertEqual(Q.green_class_of(not_full), G.HARD)
        self.assertFalse(Q.row_qualifies(not_full, SHA, self.pred))

    def test_widening_the_policy_admits_exactly_the_named_class(self):
        widened = dict(self.pred, accepts_green_class=["hard", G.SOFT_REBASE_ONLY])
        ok = qualifying_row(validated_head_sha="b" * 40, inherited_from=inherited())
        weak = qualifying_row(
            validated_head_sha="b" * 40,
            inherited_from=inherited(delta_kind=G.DELTA_REBASE_PLUS_UPSTREAM,
                                     upstream_commits=3))
        self.assertTrue(Q.row_qualifies(ok, SHA, widened))
        self.assertFalse(Q.row_qualifies(weak, SHA, widened),
                         "widening to rebase-only must NOT admit upstream-delta")


class LegitimatePopulationNotFlagged(unittest.TestCase):
    """The other half of the bracket: the real ledger must be untouched."""

    def test_every_live_row_classifies_hard_and_none_is_newly_refused(self):
        rows = V.rows()
        if not rows:
            self.skipTest("canonical ledger union is empty")
        self.assertGreater(len(rows), 100, "expected a populated ledger")

        classes = {}
        for row in rows:
            klass = Q.green_class_of(row)
            classes[klass] = classes.get(klass, 0) + 1

        # Counted, not asserted vacuously: state the denominator.
        self.assertEqual(
            classes, {G.HARD: len(rows)},
            f"expected every live row to classify hard; got {classes} over {len(rows)} rows")


if __name__ == "__main__":
    unittest.main(verbosity=2)
