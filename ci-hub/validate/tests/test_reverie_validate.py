from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path
from unittest import mock


HERE = Path(__file__).resolve().parent
VALIDATE = HERE.parent
sys.path.insert(0, str(VALIDATE))

import reverie_node
import reverie_safe_ci
import service_result
import start_unit


class ReverieValidateTest(unittest.TestCase):
    @staticmethod
    def run_safe_ci(
        root: Path,
        checkout: Path,
        *,
        node_status: str | None,
        dag_exit: int,
    ) -> tuple[int, str, str, list[str]]:
        observed: list[str] = []

        def fake_run(command, **_kwargs):
            observed.extend(map(str, command))
            dag_path = Path(command[command.index("--dag") + 1])
            dag = json.loads(dag_path.read_text())
            node = shlex.split(dag["steps"][0]["cmd"])
            status_path = Path(node[node.index("--status-file") + 1])
            if node_status is not None:
                status_path.write_text(f"FINAL_VALIDATE_STATUS: {node_status}\n")
            return subprocess.CompletedProcess(command, dag_exit)

        stdout = StringIO()
        stderr = StringIO()
        with mock.patch.object(reverie_safe_ci.subprocess, "run", side_effect=fake_run):
            with redirect_stdout(stdout), redirect_stderr(stderr):
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
        return rc, stdout.getvalue(), stderr.getvalue(), observed

    @staticmethod
    def run_reverie_node(
        root: Path,
        checkout: Path,
        *,
        validate_exit: int,
    ) -> tuple[int, str, dict]:
        target = "b" * 40
        log = root / "reverie.log"
        status_file = root / "final-status"
        log.write_text(
            "== Test regular workspace cases ==\n"
            "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n"
            "== Documentation tests ==\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n"
        )

        def fake_git(_checkout: Path, *args: str) -> str:
            if args == ("rev-parse", "HEAD^{commit}"):
                return target
            if args == ("status", "--porcelain=v1"):
                return ""
            if args == ("rev-parse", "HEAD^{tree}"):
                return "c" * 40
            raise AssertionError(f"unexpected git call: {args!r}")

        gates = []
        for name in reverie_node.EXPECTED_GATES:
            gate = {"name": name}
            if name == "Test regular workspace cases":
                gate.update({"executed_tests": 12, "filtered_tests": 3})
            elif name == "Documentation tests":
                gate.update({"executed_tests": 5, "filtered_tests": 1})
            gates.append(gate)
        source = {
            "result": "pass" if validate_exit == 0 else "fail",
            "checks": len(reverie_node.EXPECTED_GATES),
            "executed_tests": 17,
            "filtered_tests": 4,
            "gates": gates,
        }
        recorded: list[dict] = []
        with (
            mock.patch.object(reverie_node, "git", side_effect=fake_git),
            mock.patch.object(reverie_node, "require_kvm_environment"),
            mock.patch.object(reverie_node, "safe_ci_cgroup", return_value="/safe-ci"),
            mock.patch.object(reverie_node, "lock_authority", return_value={}),
            mock.patch.object(
                reverie_node.subprocess,
                "run",
                return_value=subprocess.CompletedProcess([], validate_exit),
            ),
            mock.patch.object(reverie_node, "load_source_row", return_value=source),
            mock.patch.object(
                reverie_node,
                "record",
                side_effect=lambda _root, row: recorded.append(row),
            ),
        ):
            rc = reverie_node.main(
                [
                    "--root",
                    str(root),
                    "--checkout",
                    str(checkout),
                    "--target",
                    target,
                    "--log",
                    str(log),
                    "--status-file",
                    str(status_file),
                ]
            )
        assert len(recorded) == 1
        return rc, status_file.read_text(), recorded[0]

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
        self.assertNotIn("HERMIT_VALIDATE_RUN_TIMEOUT_SECONDS", joined)
        self.assertNotIn("run_with_validate_deadline.py", joined)
        self.assertNotIn("--allow-cgroup-failure", joined)
        self.assertNotIn("--unsafe-no-cgroups", joined)
        self.assertNotIn("scripts/validate.rs", joined)

    def test_safe_ci_launcher_has_no_unboxed_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, _stderr, observed = self.run_safe_ci(
                root, checkout, node_status="PASSED", dag_exit=0
            )
            self.assertEqual(rc, 0)
            self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: PASSED\n")
            self.assertIn("dagrun", observed)
            self.assertNotIn("safe-ci-dag-runner", observed)
            self.assertEqual(observed[observed.index("--max-steps") + 1], "1")
            self.assertEqual(observed[observed.index("--max-cpus") + 1], "8")
            self.assertNotIn("--jobs", observed)
            self.assertNotIn("--allow-cgroup-failure", observed)
            self.assertNotIn("--unsafe-no-cgroups", observed)
            self.assertEqual(list((root / "ignored/validate").glob("reverie-dag-*")), [])
            self.assertEqual(
                list((root / "ignored/validate").glob("reverie-final-status-*")), []
            )

    def test_safe_ci_reports_product_failure_from_the_main_path(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, _stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="FAILED", dag_exit=1
            )
        self.assertEqual(rc, 1)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: FAILED\n")

    def test_safe_ci_pass_is_accepted_by_the_central_result_reader(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, _stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="PASSED", dag_exit=0
            )
        evidence = service_result.read_output_evidence(
            stdout
            + "# ci-hub/validate-lock tool COST ACTUAL wall=1.0s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=0\n"
        )
        result = service_result.classify(evidence)
        self.assertEqual(rc, 0)
        self.assertEqual(result["state"], "completed")
        self.assertEqual(result["result"], "success")

    def test_safe_ci_reports_could_not_run_from_the_main_path(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, _stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="COULD_NOT_RUN", dag_exit=1
            )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")

    def test_safe_ci_missing_node_status_cannot_be_reported_as_passed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, stderr, _observed = self.run_safe_ci(
                root, checkout, node_status=None, dag_exit=0
            )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
        self.assertIn("final validate status is absent", stderr)
        evidence = service_result.read_output_evidence(
            stdout
            + "# ci-hub/validate-lock tool COST ACTUAL wall=1.0s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=75\n"
        )
        result = service_result.classify(evidence)
        self.assertEqual(result["state"], "completed")
        self.assertEqual(result["result"], "no-result")

    def test_safe_ci_status_and_dagrun_exit_must_agree(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="PASSED", dag_exit=1
            )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
        self.assertIn("status disagrees with dagrun exit", stderr)

    def test_safe_ci_malformed_node_status_cannot_be_reported_as_passed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "ignored/validate").mkdir(parents=True)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, stdout, stderr, _observed = self.run_safe_ci(
                root, checkout, node_status="MAYBE", dag_exit=0
            )
        self.assertEqual(rc, 75)
        self.assertEqual(stdout, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
        self.assertIn("final validate status is malformed", stderr)

    def test_safe_ci_setup_failure_still_reports_could_not_run(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            stdout = StringIO()
            stderr = StringIO()
            with mock.patch.object(
                reverie_safe_ci.tempfile,
                "mkstemp",
                side_effect=OSError("fixture setup failure"),
            ):
                with redirect_stdout(stdout), redirect_stderr(stderr):
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
        self.assertEqual(rc, 75)
        self.assertEqual(stdout.getvalue(), "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
        self.assertIn("could not prepare validation", stderr.getvalue())

    def test_reverie_node_publishes_status_from_the_main_path(self) -> None:
        cases = (
            (0, 0, "FINAL_VALIDATE_STATUS: PASSED\n"),
            (1, 1, "FINAL_VALIDATE_STATUS: FAILED\n"),
            (75, 75, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"),
        )
        for validate_exit, expected_rc, expected_status in cases:
            with self.subTest(validate_exit=validate_exit):
                with tempfile.TemporaryDirectory() as raw:
                    root = Path(raw)
                    checkout = root / "reverie"
                    checkout.mkdir()
                    rc, status, row = self.run_reverie_node(
                        root, checkout, validate_exit=validate_exit
                    )
                self.assertEqual(rc, expected_rc)
                self.assertEqual(status, expected_status)
                self.assertEqual(row["executed_tests"], 17)
                self.assertEqual(row["filtered_tests"], 4)

    def test_reverie_node_refusal_publishes_could_not_run(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            status_file = root / "final-status"
            with mock.patch.object(
                reverie_node, "git", side_effect=reverie_node.Refusal("not admitted")
            ):
                rc = reverie_node.main(
                    [
                        "--root",
                        str(root),
                        "--checkout",
                        str(checkout),
                        "--target",
                        "b" * 40,
                        "--log",
                        str(root / "reverie.log"),
                        "--status-file",
                        str(status_file),
                    ]
                )
            self.assertEqual(rc, 75)
            self.assertEqual(
                status_file.read_text(), "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
            )

    def test_both_test_gates_must_record_nonzero_tests(self) -> None:
        complete = {
            "gates": [
                {
                    "name": "Test regular workspace cases",
                    "executed_tests": 10,
                    "filtered_tests": 3,
                },
                {
                    "name": "Documentation tests",
                    "executed_tests": 4,
                    "filtered_tests": 0,
                },
            ]
        }
        self.assertEqual(
            reverie_node.recorded_gate_coverage(complete), (14, 3, [], [])
        )
        complete["gates"][1]["executed_tests"] = 0
        self.assertEqual(
            reverie_node.recorded_gate_coverage(complete),
            (10, 3, ["Documentation tests"], []),
        )
        missing = {"gates": complete["gates"][:1]}
        self.assertEqual(
            reverie_node.recorded_gate_coverage(missing),
            (10, 3, [], ["Documentation tests"]),
        )

    def test_missing_per_gate_count_refuses_by_field_name(self) -> None:
        source = {
            "gates": [
                {
                    "name": "Test regular workspace cases",
                    "executed_tests": 10,
                    "filtered_tests": 3,
                },
                {"name": "Documentation tests", "filtered_tests": 0},
            ]
        }
        with self.assertRaisesRegex(
            reverie_node.Refusal, "Documentation tests.*executed_tests"
        ):
            reverie_node.recorded_gate_coverage(source)

    def test_aggregate_counts_come_from_the_framework_row(self) -> None:
        self.assertEqual(
            reverie_node.recorded_test_counts(
                {"executed_tests": 17, "filtered_tests": 4}
            ),
            (17, 4),
        )
        for field, value in (
            ("executed_tests", None),
            ("executed_tests", "17"),
            ("executed_tests", -1),
            ("filtered_tests", None),
        ):
            row = {"executed_tests": 17, "filtered_tests": 4, field: value}
            with self.assertRaisesRegex(reverie_node.Refusal, field):
                reverie_node.recorded_test_counts(row)

    def test_current_gate_contract_refuses_retired_merge_gate(self) -> None:
        self.assertEqual(
            reverie_node.EXPECTED_GATES,
            (
                "Cross-client skill discovery",
                "Build workspace",
                "DBT virtual identity and pidfd_open policy",
                "Test regular workspace cases",
                "Documentation tests",
                "Clippy",
                "Rustfmt",
            ),
        )
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
        boxed_paths = [
            "/user.slice/safe-ci-123.scope/step-validate.reverie-full",
            (
                "/user.slice/user-212630.slice/user@212630.service/dagrun.slice/"
                "dagrun-153463.scope/step-validate.reverie-full"
            ),
        ]
        for path in boxed_paths:
            with self.subTest(path=path):
                with mock.patch.object(Path, "read_text", return_value=f"0::{path}\n"):
                    self.assertEqual(reverie_node.safe_ci_cgroup(), path)

    def test_kvm_requirement_refuses_absence_and_accepts_exact_value(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node.require_kvm_environment()
        with mock.patch.dict(os.environ, {"REVERIE_REQUIRE_KVM": "1"}, clear=True):
            reverie_node.require_kvm_environment()


if __name__ == "__main__":
    unittest.main()
