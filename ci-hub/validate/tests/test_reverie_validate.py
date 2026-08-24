from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


HERE = Path(__file__).resolve().parent
VALIDATE = HERE.parent
sys.path.insert(0, str(VALIDATE))

import reverie_node
import reverie_safe_ci
import start_unit


class ReverieValidateTest(unittest.TestCase):
    def test_systemd_child_is_safe_ci_wrapper_without_unboxed_flags(self) -> None:
        root = Path("/workspace/dev-hermit")
        checkout = Path("/workspace/reverie")
        command = start_unit.build_systemd_command(
            root=root,
            checkout=checkout,
            target="a" * 40,
            agent="codex-1",
            unit="validate-codex-1-reverie",
            record=Path("/workspace/record.json"),
            log=Path("/workspace/reverie.log"),
            pr=437,
            validate_args=[],
            wait=60,
            hold=60,
            child_deadline=8000,
            environment={"HOME": "/home/test", "PATH": "/usr/bin:/bin"},
            repo="rrnewton/reverie",
        )
        joined = " ".join(map(str, command))
        self.assertIn("ci-hub/validate/reverie_safe_ci.py", joined)
        self.assertIn("--kind reverie-validate", joined)
        self.assertIn("--repo", start_unit.parser().format_help())
        self.assertIn("REVERIE_REQUIRE_KVM=1", joined)
        self.assertNotIn("--allow-cgroup-failure", joined)
        self.assertNotIn("--unsafe-no-cgroups", joined)
        self.assertNotIn("scripts/validate.rs", joined)

    def test_safe_ci_launcher_has_no_unboxed_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            observed: list[str] = []

            def fake_run(command, **_kwargs):
                observed.extend(map(str, command))
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(reverie_safe_ci.subprocess, "run", side_effect=fake_run):
                rc = reverie_safe_ci.main(
                    [
                        "--root",
                        str(root),
                        "--checkout",
                        str(checkout),
                        "--target",
                        "b" * 40,
                        "--log",
                        str(root / "reverie.log"),
                    ]
                )
            self.assertEqual(rc, 0)
            self.assertIn("safe-ci-dag-runner", observed)
            self.assertEqual(observed[observed.index("--max-steps") + 1], "1")
            self.assertEqual(observed[observed.index("--max-cpus") + 1], "8")
            self.assertNotIn("--jobs", observed)
            self.assertNotIn("--allow-cgroup-failure", observed)
            self.assertNotIn("--unsafe-no-cgroups", observed)
            self.assertEqual(list((root / "ignored/validate").glob("reverie-dag-*")), [])

    def test_both_test_gates_must_execute_nonzero_tests(self) -> None:
        complete = """
== Test regular workspace cases ==
test result: ok. 10 passed; 0 failed; 0 ignored; 2 measured; 3 filtered out;
== Documentation tests ==
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
"""
        self.assertEqual(reverie_node.gate_coverage(complete), (14, 3, [], []))
        missing = """
== Test regular workspace cases ==
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out;
"""
        self.assertEqual(
            reverie_node.gate_coverage(missing),
            (10, 3, [], ["Documentation tests"]),
        )

    def test_current_gate_contract_refuses_retired_merge_gate(self) -> None:
        current = [{"name": name} for name in reverie_node.EXPECTED_GATES]
        reverie_node.require_expected_gate_set(current)
        invalid_sets = {
            "retired seventh gate": current[:1]
            + [{"name": "Merge-gate policy"}]
            + current[1:],
            "mixed old and current names": current[:1]
            + [{"name": "Merge-gate policy"}]
            + current[2:],
            "missing gate": current[:-1],
            "reordered gates": [current[1], current[0], *current[2:]],
        }
        for case, gates in invalid_sets.items():
            with self.subTest(case=case):
                with self.assertRaisesRegex(reverie_node.Refusal, "gate set drifted"):
                    reverie_node.require_expected_gate_set(gates)

    def test_safe_ci_cgroup_refuses_unboxed_and_accepts_step_scope(self) -> None:
        with mock.patch.object(Path, "read_text", return_value="0::/user.slice/agent.scope\n"):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node.safe_ci_cgroup()
        boxed = "0::/user.slice/safe-ci-123.scope/step-validate.reverie-full\n"
        with mock.patch.object(Path, "read_text", return_value=boxed):
            self.assertEqual(
                reverie_node.safe_ci_cgroup(),
                "/user.slice/safe-ci-123.scope/step-validate.reverie-full",
            )

    def test_kvm_requirement_refuses_absence_and_accepts_exact_value(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node.require_kvm_environment()
        with mock.patch.dict(os.environ, {"REVERIE_REQUIRE_KVM": "1"}, clear=True):
            reverie_node.require_kvm_environment()


if __name__ == "__main__":
    unittest.main()
