#!/usr/bin/env python3
"""Tests for the runnable refusal-predicate gate cases."""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "bin" / "refusal-predicate-gate-probes"


def load_script():
    loader = importlib.machinery.SourceFileLoader("refusal_predicate_gate_probes", str(SCRIPT))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[loader.name] = module
    loader.exec_module(module)
    return module


class RefusalPredicateGateProbesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.module = load_script()

    def test_names_are_unique_and_required_cases_are_present(self) -> None:
        names = [probe.name for probe in self.module.PROBES]
        self.assertEqual(len(names), len(set(names)))
        self.assertGreaterEqual(len(self.module.MUST_REFUSE), 10)
        self.assertEqual(len(self.module.MUST_PASS), 4)
        for required in (
            "map-bound-contains",
            "operator-contains",
            "imported-search",
            "unbound-find",
            "refusal-marker-character",
            "validate-no-result",
            "declined-by",
            "assigned-splitlines",
            "split-newline",
            "spaces-before-marker-regex",
        ):
            self.assertIn(required, names)

    def test_every_fixture_has_the_behavior_it_claims(self) -> None:
        with tempfile.TemporaryDirectory(prefix="refusal-probe-test-") as raw:
            root = Path(raw)
            for probe in self.module.PROBES:
                path = root / probe.filename
                path.write_text(self.module.normalized_source(probe.source), encoding="utf-8")
                self.assertEqual([], self.module.behavior_errors(probe, path), probe.name)

    def test_per_probe_output_names_every_case_and_predicate(self) -> None:
        with tempfile.TemporaryDirectory(prefix="refusal-probe-checker-") as raw:
            checker = Path(raw) / "checker.py"
            checker.write_text(
                """#!/usr/bin/env python3
import pathlib
import sys
target = pathlib.Path(sys.argv[1])
if target.is_dir() or target.name.startswith('must-refuse-'):
    raise SystemExit(1)
raise SystemExit(0)
""",
                encoding="utf-8",
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                rc = self.module.run_probes(checker)
            rendered = output.getvalue()
            self.assertEqual(0, rc, rendered)
            for probe in self.module.PROBES:
                self.assertIn(f"PASS {probe.name}:", rendered)
                self.assertIn(f"predicate: {probe.predicate}", rendered)
            self.assertIn("PASS wrong-nonempty-directory:", rendered)
            self.assertIn("RESULT: PASS", rendered)


if __name__ == "__main__":
    unittest.main()
