#!/usr/bin/env python3
"""Bracket root-executable attribution from both sides.

The defect this guards: attribution was decided by comparing argv[0]'s basename
to the corpus LABEL, so any scenario whose name differed from its binary was
filed as unattributable even though argv[0] named a real program. Six stable-GAP
cells were mis-filed. A rule that can only widen attribution is as wrong as the
one it replaced, so the negative side is bracketed too.
"""

from __future__ import annotations

import json
import sys
import unittest
from importlib import util
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
_spec = util.spec_from_file_location(
    "analyze_repeat_stability", ROOT / "tools/analyze_repeat_stability.py"
)
assert _spec is not None and _spec.loader is not None
analyzer = util.module_from_spec(_spec)
sys.modules[_spec.name] = analyzer
_spec.loader.exec_module(analyzer)


def program(label: str, *argv: str) -> dict[str, object]:
    return {"label": label, "argv": list(argv)}


class RootExecutableAttributionTest(unittest.TestCase):
    def test_a_named_binary_is_attributed_even_when_the_label_differs(self) -> None:
        """POSITIVE. The six real cells that were mis-filed."""
        for label, argv0, expected in (
            ("mpstat-softirqs", "/usr/bin/mpstat", "mpstat"),
            ("pidstat-disk", "/usr/bin/pidstat", "pidstat"),
            ("sar-resource-tables", "/usr/bin/sar", "sar"),
            ("sysctl-random-uuid", "/usr/sbin/sysctl", "sysctl"),
            ("vmstat-disk", "/usr/bin/vmstat", "vmstat"),
        ):
            prog = program(label, argv0, "-x")
            self.assertEqual(analyzer.root_executable(prog), expected, label)
            self.assertEqual(analyzer.harness_form(prog), "named-program-direct", label)
            self.assertTrue(analyzer.named_defect_eligible(prog), label)

    def test_a_wrapper_root_stays_unattributable(self) -> None:
        """NEGATIVE. What the wrapper actually exec'd is not in argv."""
        for argv0 in ("/bin/bash", "/usr/bin/sh", "/usr/bin/env"):
            prog = program("anything", argv0, "-c", "some command")
            self.assertIsNone(analyzer.root_executable(prog), argv0)
            self.assertEqual(
                analyzer.harness_form(prog), "shell-or-launcher-wrapped", argv0
            )
            self.assertFalse(analyzer.named_defect_eligible(prog), argv0)

    def test_the_workload_driver_stays_unattributable(self) -> None:
        prog = program("workload", "/bin/bash", "/x/real_compat_workload.sh", "case")
        self.assertIsNone(analyzer.root_executable(prog))
        self.assertEqual(analyzer.harness_form(prog), "workload-script")
        self.assertFalse(analyzer.named_defect_eligible(prog))

    def test_the_test_builtin_is_a_program_not_a_launcher(self) -> None:
        prog = program("bracket", "/usr/bin/[", "1", "=", "1", "]")
        self.assertEqual(analyzer.root_executable(prog), "[")
        self.assertEqual(analyzer.harness_form(prog), "named-program-direct")

    def test_bash_run_as_the_subject_is_still_a_wrapper_root(self) -> None:
        """Deliberate: `bash` cannot be told apart from bash-as-launcher by argv[0].

        The old rule special-cased label == "bash" as direct. Attribution by
        root executable cannot make that distinction and must not pretend to;
        unknown is the honest answer.
        """
        prog = program("bash", "/bin/bash", "--version")
        self.assertIsNone(analyzer.root_executable(prog))

    def test_published_corpus_partition_matches_the_republished_numbers(self) -> None:
        """The corpus-wide oracle, so a future edit cannot drift silently."""
        corpus = json.loads((ROOT / "inputs/corpus.json").read_text())
        forms = [analyzer.harness_form(p) for p in corpus["programs"]]
        self.assertEqual(len(forms), 214)
        self.assertEqual(forms.count("workload-script"), 50)
        # 66 under the old label-equality rule, +7 recovered by argv[0]
        # attribution (mpstat, numactl, pidstat, sar, sysctl, vmstat, wc),
        # -1 because bash-as-subject is now correctly a wrapper root.
        self.assertEqual(forms.count("named-program-direct"), 72)
        self.assertEqual(forms.count("shell-or-launcher-wrapped"), 92)


class ArtifactVerifierIsActuallyInvokedTest(unittest.TestCase):
    """The artifact shipped verify_artifact.py and NOTHING RAN IT.

    That is the failure this correction had to avoid repeating: a tool with no
    caller closes nothing. Registering this suite in the counted runner gives
    the verifier a caller, and this test is that call. It regenerates all three
    result files from the five immutable sweeps and compares byte-for-byte, so
    a drifted classifier or a stale republished CSV fails here.
    """

    def test_verify_artifact_passes_end_to_end(self) -> None:
        import subprocess

        result = subprocess.run(
            [sys.executable, str(ROOT / "verify_artifact.py")],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=600,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("regenerated=3/3 byte-identical", result.stdout)
        self.assertIn(
            "stable_gap named=23 wrapper=215 workload=110 unknown=325 reattributed=6/6",
            result.stdout,
        )


if __name__ == "__main__":
    unittest.main()
