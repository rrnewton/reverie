"""A withdrawal is scoped to a LANE, not to the reviewer who issued the refusal.

WHAT THIS PINS, AND WHY IT DOES NOT PIN A BLOCK.

`agent(hermit-triage)` fixed the cross-lane version on 2026-08-25: one lane's
withdrawal used to discharge every lane's refusal. This is the same defect one
level in -- WITHIN a lane, ACROSS reviewers. Two agents both reviewing in the
claude lane share one voice, because the marker grammar carries `claude|codex`
and no reviewer identity, and cannot carry one from GitHub either: every agent
authenticates as the same user, so the API attributes all of them to one login.

⚠️ THE FIX IS VISIBILITY, NOT A REFUSAL, AND THAT IS DELIBERATE. One reviewer who
posts two refusals and then withdraws once is a LEGITIMATE clear. Without issuer
identity that case is indistinguishable from the unsafe one, so blocking would
over-block a correct workflow and guessing would reinstate the original bug. The
gate therefore reports the collapse and says it cannot decide which case it is.
Scoping properly needs the marker to carry its issuer -- `agent(hermit-011)`'s
work -- and this is the reader half, waiting for it in the open rather than
silently assuming it away.

Both directions are covered. A test that only checked the collapse is reported
would pass if the gate reported it unconditionally, which would make the warning
worthless; so the 1:1 and 2:2 cases must stay silent.
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
COLLAPSE = "ONE WITHDRAWAL CLEARED MORE THAN ONE REFUSAL"


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

    def test_one_withdrawal_clearing_two_reviewers_refusals_is_reported(self) -> None:
        """The defect itself: it still CLEARS, but it no longer does so silently."""
        rc, msg = self.gate.evaluate(
            [refusal("hermit-015"), refusal("hermit-006"), withdrawal("hermit-006")],
            HEAD,
        )
        self.assertEqual(rc, self.gate.OK, "the verdict is unchanged; only the report is")
        self.assertIn(COLLAPSE, msg)
        self.assertIn("1 withdrawal(s) cleared 2 refusal(s)", msg)
        self.assertIn("CANNOT BE DECIDED HERE", msg)

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
