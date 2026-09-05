#!/usr/bin/env python3
"""The refusal message must name the review evidence that is actually missing.

A later canonical current-head withdrawal answers earlier objections in its
lane. Fleet names are optional metadata, so same-lane resolution does not depend
on who supplied or omitted one. A withdrawal in another lane still cannot answer
the objection, and the message must say which lane lacks a later decision.

Separately, `records` is an optional
argument, and without it `retired()` can credit nothing: `_retire_claims` and
`records_by_id` are both built from it. So a body carrying a valid
`RETIRES <comment-id>` is silently ignored and the call returns BLOCKED where the
same input WITH records returns OK. An optional argument that changes the verdict
has to announce itself.

These cases cover both decisions and messages. `test_gate_variant_space.py` and
the gate's own `--self-test` cover the broader verdict space.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib
import unittest

GATE = pathlib.Path(__file__).resolve().parents[2] / "bin" / "changes-requested-gate"
HEAD = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"
TAG = "[hermit2, hermit-006, unresolved, devbig014, role=reviewer]\n"
CROSS_LANE_SENTENCE = "A withdrawal in a different lane does not answer"
MISSING_RECORDS = "no review records were supplied"


def _gate():
    loader = importlib.machinery.SourceFileLoader("gate_under_test", str(GATE))
    spec = importlib.util.spec_from_loader("gate_under_test", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def refuse(lane: str = "claude", signed: bool = False) -> str:
    return (TAG if signed else "") + f"CHANGES-REQUESTED-AT: {lane} {HEAD}"


def drop(lane: str = "claude", signed: bool = False, retires: int | None = None) -> str:
    body = (TAG if signed else "") + f"CHANGES-REQUESTED-WITHDRAWN-AT: {lane} {HEAD}"
    return body + (f"\n\nRETIRES {retires}\n" if retires is not None else "")


class TheCrossLaneSentenceOnlyAppearsWhenACrossLaneWithdrawalDoes(unittest.TestCase):
    def setUp(self) -> None:
        self.gate = _gate()

    def test_a_same_lane_withdrawal_is_not_blamed_on_the_lane(self) -> None:
        """THE REGRESSION. The only withdrawal here is in the refusal's own lane."""
        rc, msg = self.gate.evaluate([refuse("claude"), drop("claude")], HEAD)
        self.assertEqual(rc, self.gate.OK, msg)
        self.assertNotIn(CROSS_LANE_SENTENCE, msg)

    def test_a_same_lane_withdrawal_names_the_later_decision(self) -> None:
        rc, msg = self.gate.evaluate([refuse("claude"), drop("claude")], HEAD)
        self.assertEqual(rc, self.gate.OK, msg)
        self.assertIn("later canonical current-head lane decision", msg)

    def test_a_genuine_cross_lane_withdrawal_is_still_named_as_one(self) -> None:
        """⚠️ THE CONTROL. Removing the sentence entirely would also pass the
        first case, and would lose a true explanation for the case it describes."""
        _rc, msg = self.gate.evaluate([refuse("claude"), drop("codex")], HEAD)
        self.assertIn(CROSS_LANE_SENTENCE, msg)
        self.assertIn("lane(s) claude that have no later canonical decision", msg)

    def test_no_withdrawal_at_all_is_not_blamed_on_a_lane(self) -> None:
        _rc, msg = self.gate.evaluate([refuse("claude")], HEAD)
        self.assertNotIn(CROSS_LANE_SENTENCE, msg)


class AMissingRecordsArgumentSaysSoRatherThanRejectingSilently(unittest.TestCase):
    def setUp(self) -> None:
        self.gate = _gate()

    def test_a_retires_claim_with_no_records_reports_the_missing_evidence(self) -> None:
        """THE SECOND REGRESSION. The claim is valid; the evidence was never passed."""
        rc, msg = self.gate.evaluate(
            [refuse("claude"), drop("claude", retires=900001)], HEAD
        )
        self.assertEqual(rc, self.gate.BLOCKED, msg)
        self.assertIn(MISSING_RECORDS, msg)
        self.assertIn("missing evidence, not a rejected claim", msg)

    def test_no_retires_claim_does_not_claim_missing_evidence(self) -> None:
        """⚠️ THE CONTROL. A notice that always fires distinguishes nothing, and
        would tell every ordinary refusal to go looking for records it never needed."""
        _rc, msg = self.gate.evaluate([refuse("claude"), drop("claude")], HEAD)
        self.assertNotIn(MISSING_RECORDS, msg)

    def test_the_notice_is_about_the_argument_not_about_the_shape(self) -> None:
        """Supplying records must silence it even when the verdict stays BLOCKED."""
        grammar = self.gate._load_grammar()
        bodies = [refuse("claude"), drop("claude", retires=900099)]
        ids = [900001, 900002]
        records = [
            {
                "id": rid,
                "body": body,
                "user": {"login": grammar.comment_author(body) or "fixture-reviewer"},
                "_review_surface": "issue-comment",
                "created_at": f"2026-09-04T00:00:{pos:02d}Z",
                "updated_at": f"2026-09-04T00:00:{pos:02d}Z",
            }
            for pos, (rid, body) in enumerate(zip(ids, bodies), 1)
        ]
        records = grammar.annotate_current_review_authority(
            records,
            "rrnewton/hermit",
            lambda path: {
                "role_name": "write",
                "permission": "write",
                "user": {"login": path.split("/collaborators/", 1)[1].split("/", 1)[0]},
            },
        )
        _rc, msg = self.gate.evaluate(bodies, HEAD, ids, records=records)
        self.assertNotIn(MISSING_RECORDS, msg)


class TheVerdictsThemselvesAreUnchanged(unittest.TestCase):
    """Lane chronology, not optional fleet names, determines each verdict."""

    def setUp(self) -> None:
        self.gate = _gate()

    def test_the_shapes_that_must_block_still_block(self) -> None:
        for name, bodies in [
            ("cross-lane withdrawal", [refuse("claude"), drop("codex")]),
            ("refusal alone", [refuse("claude")]),
        ]:
            with self.subTest(name=name):
                rc, msg = self.gate.evaluate(bodies, HEAD)
                self.assertEqual(rc, self.gate.BLOCKED, msg)

    def test_same_lane_withdrawal_clears_regardless_of_fleet_name_metadata(self) -> None:
        """Without this the block assertions above would pass against a gate
        that blocks everything."""
        for name, bodies in [
            ("both unsigned", [refuse("claude"), drop("claude")]),
            ("refusal unsigned", [refuse("claude"), drop("claude", signed=True)]),
            ("both signed", [refuse("claude", signed=True), drop("claude", signed=True)]),
        ]:
            with self.subTest(name=name):
                rc, msg = self.gate.evaluate(bodies, HEAD)
                self.assertEqual(rc, self.gate.OK, msg)


if __name__ == "__main__":
    unittest.main()
