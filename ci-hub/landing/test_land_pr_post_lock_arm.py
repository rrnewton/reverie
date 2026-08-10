#!/usr/bin/env python3
"""Regression bracket for the land-lock/post-land obligation boundary."""

from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "ci-hub/landing/land-pr.sh"
INNER_MARKER = "# --- inner: we now hold the land-lock"
COMPLETE = '"$ROOT/ci-hub/remediation/land_and_arm.py" complete'


class PostLockArmTest(unittest.TestCase):
    def test_obligation_arming_occurs_only_after_lock_run_returns(self) -> None:
        script = SCRIPT.read_text(encoding="utf-8")
        outer, inner = script.split(INNER_MARKER, 1)

        lock_run = outer.index('"$ROOT/ci-hub/ci-hub" land-lock run')
        lock_result = outer.index("rc=$?", lock_run)
        completion = outer.index(COMPLETE)

        self.assertLess(lock_run, lock_result)
        self.assertLess(lock_result, completion)
        self.assertNotIn(COMPLETE, inner)
        self.assertIn("MERGED UNDER LOCK:", inner)

    def test_unregistered_setsid_escape_guard_is_not_weakened(self) -> None:
        source = (ROOT / "ci-hub/lib/landing_lock.rs").read_text(encoding="utf-8")

        self.assertIn(
            "normal_leader_exit_cannot_clear_an_escaped_payload_barrier", source
        )
        self.assertIn(
            "escaped_payload_quarantine_refuses_consumers_until_exact_recovery",
            source,
        )
        self.assertIn("capture_and_freeze_residuals(std::process::id())", source)


if __name__ == "__main__":
    unittest.main()
