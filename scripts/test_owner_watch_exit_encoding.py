#!/usr/bin/env python3
"""One exit code carrying both the validation verdict and the delivery outcome.

THE DEFECT THIS GUARDS. ``owner-checkout-validate-watch.py`` returned
``NOTIFY_FAILED`` from an early return placed BEFORE the verdict was computed, so
a run whose report could not be delivered said nothing about the tree it had just
validated. Measured 2026-09-05 on two consecutive firings an hour apart: firing 1
was state FAIL and firing 2 was state PASS_ALREADY_VALIDATED, and both exited 5.
The owner's checkout had passed and the exit code was indistinguishable from a
failure.

The fix cannot be "return the verdict instead". A report that reached nobody has
to stay visible, which is what the unit's ``SuccessExitStatus`` list is for. So
the verdict keeps 0..6 and an undelivered run adds ``UNDELIVERED_OFFSET``: both
facts survive, and neither is silent.

WHY THIS FILE IS IN ``scripts/``. The full watch suite lives in
``scripts/tests/test_owner_checkout_validate_watch.py`` and covers this in more
depth, but the Makefile's ``unittest discover -s scripts`` cannot descend into
``scripts/tests/`` -- it has no ``__init__.py``, so discovery reports "Start
directory is not importable". Measured 2026-09-05: adding those cases did not
change the scheduled test count at all. This file puts the encoding's load-bearing
properties where the counter already looks. The directory gap is filed separately.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
WATCH = ROOT / "scripts/owner-checkout-validate-watch.py"
UNIT = ROOT / "scripts/systemd/hermit-owner-checkout-validate.service"


def _watch():
    spec = importlib.util.spec_from_file_location("owner_watch_exit_encoding", WATCH)
    module = importlib.util.module_from_spec(spec)
    sys.modules["owner_watch_exit_encoding"] = module
    spec.loader.exec_module(module)
    return module


watch = _watch()


def success_exit_statuses() -> list[str]:
    line = next(
        text for text in UNIT.read_text(encoding="utf-8").splitlines()
        if text.startswith("SuccessExitStatus=")
    )
    return line.split("=", 1)[1].split()


class BothFactsSurviveOneExitCode(unittest.TestCase):
    def test_a_pass_that_could_not_be_delivered_still_reports_the_pass(self) -> None:
        """THE REGRESSION, in the exact shape firing 2 produced."""
        code = watch.exit_code_for("PASS_ALREADY_VALIDATED", delivered=False)
        self.assertEqual(watch.PASS, watch.verdict_from_exit_code(code))
        self.assertFalse(watch.report_reached_its_reader(code))

    def test_an_undelivered_pass_and_an_undelivered_failure_differ(self) -> None:
        """They shared exit 5 before, which is the whole defect."""
        self.assertNotEqual(
            watch.exit_code_for("PASS_ALREADY_VALIDATED", delivered=False),
            watch.exit_code_for("FAIL", delivered=False),
        )

    def test_a_failure_that_was_delivered_still_reports_the_failure(self) -> None:
        """The direction the obvious fix breaks."""
        code = watch.exit_code_for("FAIL", delivered=True)
        self.assertEqual(watch.FAIL, code)
        self.assertEqual(watch.FAIL, watch.verdict_from_exit_code(code))
        self.assertTrue(watch.report_reached_its_reader(code))

    def test_every_verdict_survives_an_undelivered_report(self) -> None:
        """Checked over the whole table, not one example."""
        self.assertTrue(watch.VERDICT_BY_STATE, "the verdict table must not be empty")
        for state, verdict in sorted(watch.VERDICT_BY_STATE.items()):
            with self.subTest(state=state):
                delivered = watch.exit_code_for(state, delivered=True)
                undelivered = watch.exit_code_for(state, delivered=False)
                self.assertEqual(verdict, delivered)
                self.assertEqual(verdict, watch.verdict_from_exit_code(undelivered))
                self.assertTrue(watch.report_reached_its_reader(delivered))
                self.assertFalse(watch.report_reached_its_reader(undelivered))

    def test_an_unknown_state_does_not_read_as_a_pass(self) -> None:
        for delivered in (True, False):
            with self.subTest(delivered=delivered):
                code = watch.exit_code_for("A_STATE_ADDED_LATER", delivered=delivered)
                self.assertEqual(
                    watch.CANNOT_START, watch.verdict_from_exit_code(code)
                )


class SystemdStillSeesAMissingReport(unittest.TestCase):
    """Widening the unit's list would restore the laundering it prevents."""

    def test_no_undelivered_code_is_listed_as_success(self) -> None:
        success = success_exit_statuses()
        self.assertTrue(success, "the unit must declare SuccessExitStatus")
        for state in sorted(watch.VERDICT_BY_STATE):
            with self.subTest(state=state):
                undelivered = watch.exit_code_for(state, delivered=False)
                self.assertNotIn(str(undelivered), success)

    def test_every_delivered_verdict_is_listed_as_success(self) -> None:
        """The other half: a delivered result is a watch result, not a dead unit."""
        success = success_exit_statuses()
        for state in sorted(watch.VERDICT_BY_STATE):
            with self.subTest(state=state):
                self.assertIn(str(watch.exit_code_for(state, delivered=True)), success)


class TheCodesStayInsideTheUsableRange(unittest.TestCase):
    def test_undelivered_codes_avoid_the_shell_and_signal_ranges(self) -> None:
        """126, 127 and 128+N already mean something to every reader."""
        for state in watch.VERDICT_BY_STATE:
            with self.subTest(state=state):
                self.assertLess(watch.exit_code_for(state, delivered=False), 126)

    def test_the_offset_clears_every_verdict(self) -> None:
        """An offset smaller than the largest verdict would alias two states."""
        self.assertGreater(
            watch.UNDELIVERED_OFFSET, max(watch.VERDICT_BY_STATE.values())
        )

    def test_the_retired_notification_code_is_gone(self) -> None:
        """5 was returned instead of the verdict; leaving it invites reuse."""
        self.assertFalse(hasattr(watch, "NOTIFY_FAILED"))


if __name__ == "__main__":
    unittest.main()
