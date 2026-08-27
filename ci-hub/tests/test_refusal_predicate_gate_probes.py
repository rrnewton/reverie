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


class ExecutingCheckerProbesTest(unittest.TestCase):
    """The probe set for scripts/check-validate-refusal-predicate.py."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.module = load_script()

    def test_names_are_unique_and_the_measured_cases_are_present(self) -> None:
        names = [name for name, _, _ in self.module.EXECUTING_MUST_REFUSE]
        names += [name for name, _, _ in self.module.EXECUTING_MUST_PASS]
        self.assertEqual(len(names), len(set(names)))
        for required in (
            "marker-character-only",
            "sibling-verdict-words",
            "restated-marker",
            "whole-channel-original-defect",
            "splitlines-bound-to-a-name",
            "split-on-newline",
            "space-star-indent",
        ):
            self.assertIn(required, names)

    def test_every_fixture_has_the_behavior_it_claims(self) -> None:
        with tempfile.TemporaryDirectory(prefix="refusal-exec-probe-") as raw:
            root = Path(raw)
            cases = [(n, b, False) for n, _, b in self.module.EXECUTING_MUST_REFUSE]
            cases += [(n, b, True) for n, _, b in self.module.EXECUTING_MUST_PASS]
            for name, body, correct in cases:
                path = root / f"{name}.py"
                source = self.module.EXECUTING_SHELL.format(body=body)
                path.write_text(source, encoding="utf-8")
                self.assertEqual(
                    [],
                    self.module.executing_behavior_errors(source, path, correct),
                    name,
                )

    def test_an_absent_classification_without_the_marker_is_a_failure(self) -> None:
        """Exit 0 alone must not satisfy the wrong-corpus probe."""
        with tempfile.TemporaryDirectory(prefix="refusal-exec-checker-") as raw:
            root = Path(raw)
            silent = root / "silent.py"
            silent.write_text(
                "#!/usr/bin/env python3\n"
                "import pathlib, sys\n"
                "t = pathlib.Path(sys.argv[1])\n"
                "if 'must-refuse-' in str(t):\n"
                "    raise SystemExit(1)\n"
                "print('PASS: nothing to check')\n"
                "raise SystemExit(0)\n",
                encoding="utf-8",
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                rc = self.module.run_executing_probes(silent)
            rendered = output.getvalue()
            self.assertEqual(1, rc, rendered)
            self.assertIn("FAIL target-without-a-classification:", rendered)
            self.assertIn("got node pass", rendered)

            marking = root / "marking.py"
            marking.write_text(
                "#!/usr/bin/env python3\n"
                "import pathlib, sys\n"
                "t = pathlib.Path(sys.argv[1])\n"
                "if 'must-refuse-' in str(t):\n"
                "    raise SystemExit(1)\n"
                "if 'target-without-a-classification' in str(t):\n"
                "    print('NO-RESULT-CASE: nothing was evaluated')\n"
                "raise SystemExit(0)\n",
                encoding="utf-8",
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                rc = self.module.run_executing_probes(marking)
            rendered = output.getvalue()
            self.assertEqual(0, rc, rendered)
            self.assertIn("PASS target-without-a-classification:", rendered)
            self.assertIn("got node no_result", rendered)


    def test_the_bare_startswith_probe_detects_a_genuine_refusal(self) -> None:
        """It must be unsafe for the reason it names, not by never firing.

        This probe once keyed on "halted:", a string no sample contains, so it
        was refused because it MISSED both genuine refusals rather than because
        matching the start of a line is not the same as matching the whole line.
        Substituting the real marker made it pass. Asserting that the fixture
        still classifies a genuine refusal as refused is what stops it drifting
        back to a string that tests nothing.
        """
        body = next(
            b for n, _, b in self.module.EXECUTING_MUST_REFUSE
            if n == "marker-named-bare-startswith"
        )
        namespace: dict[str, object] = {"__name__": "probe"}
        exec(compile(self.module.EXECUTING_SHELL.format(body=body), "<probe>", "exec"), namespace)
        predicate = namespace["_looks_refused"]
        self.assertTrue(
            predicate(self.module._GENUINE, 2),  # type: ignore[operator]
            "the bare-startswith probe no longer fires on a genuine refusal, so it "
            "would be refused for missing it rather than for the class it names",
        )
        self.assertTrue(
            predicate(self.module._QUOTED_DOC, 2),  # type: ignore[operator]
            "the bare-startswith probe must MISCLASSIFY the quoted-documentation "
            "line; that misclassification is the defect it exists to demonstrate",
        )

    def test_the_quoted_documentation_sample_is_in_the_corpus(self) -> None:
        wanted = [
            expected
            for sample, _, expected in self.module.EXECUTING_SAMPLES
            if sample == self.module._QUOTED_DOC
        ]
        self.assertEqual(1, len(wanted))
        self.assertFalse(
            wanted[0],
            "a line that merely starts with the marker is not a refusal",
        )


if __name__ == "__main__":
    unittest.main()
