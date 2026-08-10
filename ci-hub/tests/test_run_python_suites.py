#!/usr/bin/env python3
"""Bracket counted pytest discovery with inert positive and negative fixtures."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import textwrap
import unittest
from importlib import util
from pathlib import Path
from unittest import mock


RUNNER = Path(__file__).with_name("run_python_suites.py")


class RunPythonSuitesTest(unittest.TestCase):
    def _run(self, suite: Path, floor: int) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(RUNNER), "--suite", f"{suite}={floor}"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
            timeout=30,
        )

    def test_wrong_pytest_version_is_refused_before_collection(self) -> None:
        spec = util.spec_from_file_location("run_python_suites_under_test", RUNNER)
        assert spec is not None and spec.loader is not None
        module = util.module_from_spec(spec)
        sys.modules[spec.name] = module
        self.addCleanup(sys.modules.pop, spec.name, None)
        spec.loader.exec_module(module)
        with mock.patch.object(module.importlib.metadata, "version", return_value="0.0"):
            self.assertFalse(module._verify_pytest_version())

    def test_zero_discovered_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            result = self._run(Path(raw), 1)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("floor=1 discovered=0 executed=0", result.stdout)
        self.assertIn("SUITE REFUSED", result.stdout)

    def test_under_floor_is_refused_even_when_the_test_passes(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            suite = Path(raw)
            (suite / "test_one.py").write_text("def test_one():\n    pass\n")
            result = self._run(suite, 2)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("floor=2 discovered=1 executed=1", result.stdout)
        self.assertIn("SUITE REFUSED", result.stdout)

    def test_importlib_mode_survives_a_legacy_basename_collision(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            component = Path(raw) / "component"
            suite = component / "tests"
            suite.mkdir(parents=True)
            (component / "test_collision.py").write_text("ORIGIN = 'parent'\n")
            (suite / "test_collision.py").write_text(
                textwrap.dedent(
                    f"""\
                    import sys
                    sys.path.insert(0, {str(component)!r})

                    def test_child_is_collected():
                        pass
                    """
                )
            )

            legacy = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "unittest",
                    "discover",
                    "-s",
                    str(suite),
                    "-p",
                    "test_*.py",
                ],
                cwd=raw,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
                timeout=30,
            )
            result = self._run(suite, 1)

        # Legacy discovery resolves the parent collision and silently executes
        # zero child tests. Importlib mode binds collection to the child path.
        self.assertNotEqual(legacy.returncode, 0, legacy.stdout)
        self.assertIn("Ran 0 tests", legacy.stdout)
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("floor=1 discovered=1 executed=1", result.stdout)
        self.assertIn("SUITE PASS", result.stdout)

    def test_validate_default_suite_covers_root_and_nested_failure_evidence(self) -> None:
        spec = util.spec_from_file_location("run_python_suites_coverage", RUNNER)
        assert spec is not None and spec.loader is not None
        module = util.module_from_spec(spec)
        sys.modules[spec.name] = module
        self.addCleanup(sys.modules.pop, spec.name, None)
        spec.loader.exec_module(module)

        validate_root = RUNNER.parents[1] / "validate"
        suites = {suite.path: suite.floor for suite in module._default_suites()}
        self.assertEqual(suites.get(validate_root.resolve()), 123)

        root_test = validate_root / "test_failure_evidence_solo_rerun.py"
        nested_test = validate_root / "tests" / "test_failure_evidence.py"
        self.assertTrue(root_test.is_file())
        self.assertTrue(nested_test.is_file())
        self.assertNotEqual(root_test.name, nested_test.name)


if __name__ == "__main__":
    unittest.main()
