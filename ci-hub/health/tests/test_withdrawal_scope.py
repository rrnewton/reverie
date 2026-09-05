"""A withdrawal is scoped to a LANE and to the REVIEWER who issued the refusal.

WHAT THIS PINS.

`agent(hermit-triage)` fixed the cross-lane version on 2026-08-25: one lane's
withdrawal used to discharge every lane's refusal. The same defect one level in
-- WITHIN a lane, ACROSS reviewers -- is now also fixed, and this file pins the
fixed behaviour.

⚠️ THIS FILE USED TO ASSERT THE OPPOSITE, AND IT WENT UNNOTICED BECAUSE NOTHING
RAN IT. Until 2026-09-05 nothing scheduled ci-hub/health/tests, so these cases had
never executed. The version before this one said "THE FIX IS VISIBILITY, NOT A
REFUSAL", that a same-lane collapse "still CLEARS, but no longer does so
silently", and that "the verdict is unchanged; only the report is". Its reasoning
was that the marker grammar carried no reviewer identity, so a legitimate clear
-- one reviewer retracting their own two refusals -- was indistinguishable from
the unsafe one, and blocking would over-block.

THAT PREMISE IS OBSOLETE. The markers carry an optional issuer since
dev-hermit#191 and every fleet comment carries a disclosure tag, so the gate reads
who refused and who withdrew and separates the two cases. It blocks the one where
an objection was never retracted and clears the one where a reviewer retracted
their own. The gate says so itself, in changes-requested-gate's `one_for_many`:
"an unquoted withdrawal retires ONE refusal rather than its whole lane, so the
blocking decision has not depended on this text since that change landed."

So the expectation here got STRICTER, not looser: a shape that used to be
asserted as passing is now asserted as blocked.

⚠️ AND BOTH DIRECTIONS ARE COVERED, WHICH IS WHAT MAKES THE STRICTER ASSERTION
MEAN ANYTHING. A file that only asserted the refusal would be satisfied by a gate
that blocks everything, so the legitimate same-reviewer clear -- the very case the
old docstring called indistinguishable -- is asserted to still pass, alongside the
1:1 and 2:2 controls.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib
import unittest

GATE = (
    pathlib.Path(__file__).resolve().parents[2] / "bin" / "changes-requested-gate"
)
HEAD = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"
#: The gate's banner for a lane that withdrew fewer times than it refused.
#: Was "ONE WITHDRAWAL CLEARED MORE THAN ONE REFUSAL" before the issuer-aware
#: rewrite; the wording moved with the behaviour.
COLLAPSE = "A LANE WITHDREW FEWER TIMES THAN IT REFUSED"


def _gate():
    loader = importlib.machinery.SourceFileLoader("gate_under_test", str(GATE))
    spec = importlib.util.spec_from_loader("gate_under_test", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def refusal(agent: str, lane: str = "claude") -> str:
    return f"[hermit2, {agent}, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-AT: {lane} {HEAD}"


def withdrawal(agent: str, lane: str = "claude") -> str:
    return (
        f"[hermit2, {agent}, unresolved, devbig014, role=reviewer]\n"
        f"CHANGES-REQUESTED-WITHDRAWN-AT: {lane} {HEAD}"
    )


class WithinLaneWithdrawal(unittest.TestCase):
    def setUp(self) -> None:
        self.gate = _gate()

    def test_a_withdrawal_does_not_discharge_another_reviewers_refusal(self) -> None:
        """The defect itself, and it now BLOCKS rather than merely reporting.

        hermit-015 and hermit-006 both refuse in the claude lane; hermit-006
        withdraws. hermit-015 never retracted, so the head stays blocked and the
        message names both sides.
        """
        rc, msg = self.gate.evaluate(
            [refusal("hermit-015"), refusal("hermit-006"), withdrawal("hermit-006")],
            HEAD,
        )
        self.assertEqual(rc, self.gate.BLOCKED, msg)
        self.assertIn(COLLAPSE, msg)
        self.assertIn("withdrawn by hermit-006", msg)
        self.assertIn("still objecting: hermit-015", msg)

    def test_one_reviewer_retracting_their_own_two_refusals_still_clears(self) -> None:
        """⚠️ THE CASE THE OLD DOCSTRING CALLED INDISTINGUISHABLE.

        It is distinguishable now, and it must stay a pass: without this, the
        stricter assertion above would be satisfied by a gate that blocks
        everything, which is the over-blocking the old reasoning feared.
        """
        rc, msg = self.gate.evaluate(
            [refusal("hermit-006"), refusal("hermit-006"), withdrawal("hermit-006")],
            HEAD,
        )
        self.assertEqual(rc, self.gate.OK, msg)
        self.assertNotIn("still objecting", msg)

    def test_one_refusal_one_withdrawal_reports_nothing(self) -> None:
        """⚠️ THE CONTROL. A warning that always fires distinguishes nothing."""
        rc, msg = self.gate.evaluate(
            [refusal("hermit-006"), withdrawal("hermit-006")], HEAD
        )
        self.assertEqual(rc, self.gate.OK)
        self.assertNotIn(COLLAPSE, msg)

    def test_two_refusals_two_withdrawals_reports_nothing(self) -> None:
        """Every refusal accounted for by a withdrawal is not a collapse."""
        rc, msg = self.gate.evaluate(
            [
                refusal("hermit-015"),
                refusal("hermit-006"),
                withdrawal("hermit-006"),
                withdrawal("hermit-015"),
            ],
            HEAD,
        )
        self.assertEqual(rc, self.gate.OK)
        self.assertNotIn(COLLAPSE, msg)

    def test_the_cross_lane_fix_is_not_regressed(self) -> None:
        """`agent(hermit-triage)`'s guarantee, pinned here so this change cannot
        quietly undo it: a claude withdrawal does not discharge a codex refusal."""
        rc, msg = self.gate.evaluate(
            [
                refusal("hermit-006", "claude"),
                withdrawal("hermit-006", "claude"),
                refusal("hermit-001", "codex"),
            ],
            HEAD,
        )
        self.assertEqual(rc, self.gate.BLOCKED, msg)
        self.assertNotIn(COLLAPSE, msg)


if __name__ == "__main__":
    unittest.main(verbosity=2)
