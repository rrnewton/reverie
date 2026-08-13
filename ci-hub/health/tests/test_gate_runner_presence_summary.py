#!/usr/bin/env python3
"""Bracket gate_runner_presence's captured tick-hub summary field."""

from __future__ import annotations

import importlib.util
import io
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

MODULE = Path(__file__).resolve().parents[1] / "gate_runner_presence.py"
SPEC = importlib.util.spec_from_file_location("gate_runner_presence", MODULE)
assert SPEC and SPEC.loader
grp = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = grp
SPEC.loader.exec_module(grp)


class SummaryContract(unittest.TestCase):
    def _run(self, aggregate: str, surfaces: list[grp.Surface]) -> tuple[int, str]:
        output = io.StringIO()
        with (
            patch.object(grp, "evaluate", return_value=(aggregate, surfaces)),
            patch.object(sys, "argv", [str(MODULE)]),
            redirect_stdout(output),
        ):
            rc = grp.main()
        return rc, output.getvalue()

    def test_present_result_is_clean_and_publishes_a_populated_summary(self) -> None:
        surfaces = [
            grp.Surface(name, grp.PRESENT, "confirmed")
            for name in ("installation", "credentials", "service", "registration")
        ]
        rc, output = self._run(grp.PRESENT, surfaces)
        self.assertEqual(rc, 0)
        self.assertIn(
            "summary=PRESENT: installation, credentials, service, and registration confirmed",
            output,
        )
        self.assertNotIn("{summary}", output)

    def test_fault_result_names_each_failed_surface_in_one_summary_line(self) -> None:
        surfaces = [
            grp.Surface("installation", grp.ABSENT, "distribution missing"),
            grp.Surface("credentials", grp.PRESENT, "confirmed"),
            grp.Surface("service", grp.NO_RESULT, "could not query\nuser service"),
            grp.Surface("registration", grp.PRESENT, "confirmed"),
        ]
        rc, output = self._run(grp.NO_RESULT, surfaces)
        self.assertEqual(rc, 2)
        summary = [line for line in output.splitlines() if line.startswith("summary=")]
        self.assertEqual(len(summary), 1, output)
        self.assertIn("installation=ABSENT: distribution missing", summary[0])
        self.assertIn("service=NO_RESULT: could not query user service", summary[0])

    def test_inconsistent_result_refuses_instead_of_emitting_an_empty_finding(self) -> None:
        surfaces = [
            grp.Surface(name, grp.PRESENT, "confirmed")
            for name in ("installation", "credentials", "service", "registration")
        ]
        with self.assertRaisesRegex(ValueError, "no non-PRESENT surface"):
            grp.render_summary(grp.NO_RESULT, surfaces)


if __name__ == "__main__":
    unittest.main()
