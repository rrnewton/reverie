#!/usr/bin/env python3
"""Health command help stays concise while module rationale remains in source."""

from __future__ import annotations

import ast
from pathlib import Path
import subprocess
import sys
import unittest


HEALTH = Path(__file__).resolve().parents[1]
FORMERLY_LONG = (
    "approval_binding.py",
    "pr_status.py",
    "pr_tip_reachability.py",
    "reverie_pin_lag.py",
    "slot_agent_parity.py",
    "stale_tick_processes.py",
    "tick_code_currency.py",
    "unevaluated_head.py",
    "unpushed_change_clone_commits.py",
)


class HealthHelpTests(unittest.TestCase):
    def test_help_keeps_rationale_out_of_the_primary_path(self) -> None:
        for name in FORMERLY_LONG:
            with self.subTest(command=name):
                result = subprocess.run(
                    [sys.executable, str(HEALTH / name), "--help"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stderr, "")
                self.assertIn("usage:", result.stdout)
                self.assertIn("options:", result.stdout)
                self.assertNotIn("WHY THIS EXISTS", result.stdout)
                self.assertNotIn("Measured 2026-", result.stdout)
                self.assertLessEqual(len(result.stdout.splitlines()), 55)

    def test_health_parsers_do_not_use_the_whole_module_docstring(self) -> None:
        offenders: list[str] = []
        for path in sorted(HEALTH.glob("*.py")):
            tree = ast.parse(path.read_text(), filename=str(path))
            for node in ast.walk(tree):
                if not isinstance(node, ast.Call):
                    continue
                if not (
                    isinstance(node.func, ast.Attribute)
                    and node.func.attr == "ArgumentParser"
                ):
                    continue
                for keyword in node.keywords:
                    if (
                        keyword.arg == "description"
                        and isinstance(keyword.value, ast.Name)
                        and keyword.value.id == "__doc__"
                    ):
                        offenders.append(path.name)
        self.assertEqual(
            offenders,
            [],
            "use the first docstring line for --help and keep rationale in source",
        )


if __name__ == "__main__":
    unittest.main()
