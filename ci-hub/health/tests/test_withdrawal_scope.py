"""Canonical current-head withdrawals discharge earlier lane objections.

Exact ``RETIRES <comment-id>`` remains the stronger single-objection form.
Fleet names in disclosures or ``BY`` clauses are optional metadata and cannot
change either decision.
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

    def test_one_current_head_withdrawal_clears_earlier_lane_refusals(self) -> None:
        rc, msg = self.gate.evaluate(
            [refusal("hermit-015"), refusal("hermit-006"), withdrawal("hermit-006")],
            HEAD,
        )
        self.assertEqual(rc, self.gate.OK, msg)
        self.assertNotIn(COLLAPSE, msg)

    def test_one_refusal_one_current_head_withdrawal_clears(self) -> None:
        rc, msg = self.gate.evaluate(
            [refusal("hermit-006"), withdrawal("hermit-006")], HEAD
        )
        self.assertEqual(rc, self.gate.OK)
        self.assertNotIn(COLLAPSE, msg)

    def test_two_refusals_two_current_head_withdrawals_clear(self) -> None:
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
