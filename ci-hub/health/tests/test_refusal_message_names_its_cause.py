#!/usr/bin/env python3
"""The BLOCKED message must name the cause that applies, not every cause there is.

TWO DEFECTS, PINNED SEPARATELY so a regression in either fails on its own rather
than one carrying the other.

FIRST. The message asserted unconditionally that "a withdrawal from a DIFFERENT
lane does not discharge it", including when every withdrawal present sat in the
SAME lane as the refusal it failed to clear. Measured 2026-09-05 on a bare
same-lane pair: it sends the reader hunting a cross-lane problem that is not
there. This repository has lost time twice to a refusal that misidentified its own
cause -- a hook whose message pointed at a lint that was not the problem, and a
KVM guest error that was really a worktree-location constraint.

SECOND, AND IT IS THE ONE THAT COST THIS INVESTIGATION. `records` is an optional
argument, and without it `retired()` can credit nothing: `_retire_claims` and
`records_by_id` are both built from it. So a body carrying a valid
`RETIRES <comment-id>` is silently ignored and the call returns BLOCKED where the
same input WITH records returns OK. An optional argument that changes the verdict
has to announce itself. Both production callers do pass it --
ci-hub/health/lane_shortfall.py:149 and the gate's own main() -- so only a caller
that forgets reaches this, and a caller that forgets is exactly who needs telling.

⚠️ WHAT IS NOT WRONG HERE, recorded because it was reported as a defect and is
not one. A bare refusal is NOT unretirable. On the real hermit#2625 pair a
withdrawal quoting its comment id clears it -- "issued by <unsigned> and retired
by hermit-012". And an UNQUOTED withdrawal correctly fails to clear it, because
an unsigned refusal has no issuer to match and clearing it would be a guess about
who spoke. The bare/tagged difference is that rule working, not an inversion.

These cases assert MESSAGES ONLY. The verdicts are unchanged by the commit that
added this file, and test_gate_variant_space.py plus the gate's own --self-test
cover the verdict space.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib
import unittest

GATE = pathlib.Path(__file__).resolve().parents[2] / "bin" / "changes-requested-gate"
HEAD = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"
TAG = "[hermit2, hermit-006, unresolved, devbig014, role=reviewer]\n"
CROSS_LANE_SENTENCE = "a withdrawal from a DIFFERENT lane does not discharge it"
MISSING_RECORDS = "NO REVIEW RECORDS WERE SUPPLIED"


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
        self.assertEqual(rc, self.gate.BLOCKED, msg)
        self.assertNotIn(CROSS_LANE_SENTENCE, msg)

    def test_a_same_lane_refusal_is_told_what_is_actually_missing(self) -> None:
        """Not naming the wrong cause is half of it; naming the right one is the rest."""
        _rc, msg = self.gate.evaluate([refuse("claude"), drop("claude")], HEAD)
        self.assertIn("Every withdrawal present is in the refusal's own lane", msg)
        self.assertIn("no readable issuer", msg)

    def test_a_genuine_cross_lane_withdrawal_is_still_named_as_one(self) -> None:
        """⚠️ THE CONTROL. Removing the sentence entirely would also pass the
        first case, and would lose a true explanation for the case it describes."""
        _rc, msg = self.gate.evaluate([refuse("claude"), drop("codex")], HEAD)
        self.assertIn(CROSS_LANE_SENTENCE, msg)

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
    """A message change must not move a single verdict on a landing gate."""

    def setUp(self) -> None:
        self.gate = _gate()

    def test_the_shapes_that_must_block_still_block(self) -> None:
        for name, bodies in [
            ("unsigned same-lane pair", [refuse("claude"), drop("claude")]),
            ("cross-lane withdrawal", [refuse("claude"), drop("codex")]),
            ("refusal alone", [refuse("claude")]),
            ("unsigned refusal, signed withdrawal",
             [refuse("claude"), drop("claude", signed=True)]),
        ]:
            with self.subTest(name=name):
                rc, msg = self.gate.evaluate(bodies, HEAD)
                self.assertEqual(rc, self.gate.BLOCKED, msg)

    def test_the_shape_that_must_clear_still_clears(self) -> None:
        """Without this the block assertions above would pass against a gate
        that blocks everything."""
        rc, msg = self.gate.evaluate(
            [refuse("claude", signed=True), drop("claude", signed=True)], HEAD
        )
        self.assertEqual(rc, self.gate.OK, msg)


if __name__ == "__main__":
    unittest.main()
