from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from collections.abc import Callable
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


DEFAULT_PASSED = object()


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
        legacy_v3: bool = False,
        maturity_v5: bool = False,
        regular_executed: int = 12,
        aggregate_executed: int | None = None,
        regular_passed: object = DEFAULT_PASSED,
        aggregate_passed: object = DEFAULT_PASSED,
        regular_result: str = "pass",
        mutate_source: Callable[[dict], None] | None = None,
        expect_record: bool = True,
    ) -> tuple[int, str, dict]:
        target = "b" * 40
        log = root / "reverie.log"
        status_file = root / "final-status"
        log.write_text(
            "== Test all owned public instruction cases ==\n"
            "test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n"
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

        expected_gates = (
            reverie_node.V3_GATES if legacy_v3 else reverie_node.EXPECTED_GATES
        )
        if maturity_v5:
            expected_gates = reverie_node.V5_GATES
        gates = []
        for name in expected_gates:
            gate = {"name": name}
            if name == "Test all owned public instruction cases":
                gate.update(
                    {
                        "result": "pass",
                        "executed_tests": 17,
                        "passed_tests": 17,
                        "filtered_tests": 0,
                    }
                )
            elif name == "Test regular workspace cases":
                gate.update(
                    {
                        "result": regular_result,
                        "executed_tests": regular_executed,
                        "passed_tests": (
                            regular_executed
                            if regular_passed is DEFAULT_PASSED
                            else regular_passed
                        ),
                        "filtered_tests": 3,
                    }
                )
            elif name == "Documentation tests":
                gate.update(
                    {
                        "result": "pass",
                        "executed_tests": 5,
                        "passed_tests": 5,
                        "filtered_tests": 1,
                    }
                )
            gates.append(gate)
        executed_tests = (
            aggregate_executed
            if aggregate_executed is not None
            else regular_executed + 5 + (0 if legacy_v3 or maturity_v5 else 17)
        )
        source = {
            "result": "pass" if validate_exit == 0 else "fail",
            "checks": len(expected_gates),
            "executed_tests": executed_tests,
            "passed_tests": (
                executed_tests
                if aggregate_passed is DEFAULT_PASSED
                else aggregate_passed
            ),
            "filtered_tests": 4,
            "gates": gates,
        }
        if mutate_source is not None:
            mutate_source(source)
        recorded: list[dict] = []
        with (
            mock.patch.object(reverie_node, "git", side_effect=fake_git),
            mock.patch.object(reverie_node, "require_kvm_environment"),
            mock.patch.object(
                reverie_node,
                "safe_ci_cgroup",
                return_value=(
                    "/user.slice/user-212630.slice/user@212630.service/dagrun.slice/"
                    "dagrun-153463.scope/step-validate.reverie-full"
                ),
            ),
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
        if expect_record:
            assert len(recorded) == 1
            row = recorded[0]
        else:
            assert recorded == []
            row = {}
        return rc, status_file.read_text(), row

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
                self.assertEqual(row["executed_tests"], 34)
                self.assertEqual(row["passed_tests"], 34)
                self.assertEqual(row["filtered_tests"], 4)
                self.assertEqual(
                    row["reverie_validation_policy"], "reverie-local-validation/v4"
                )
                self.assertEqual(
                    row["coverage"],
                    {
                        "planned_test_nodes": 3,
                        "executed_test_nodes": 3,
                        "zero_executed_nodes": [],
                        "absent_nodes": [],
                    },
                )
                self.assertEqual(row["safe_ci"]["step"], reverie_node.SAFE_CI_STEP)

    def test_reverie_node_preserves_the_v3_source_contract(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, status, row = self.run_reverie_node(
                root, checkout, validate_exit=0, legacy_v3=True
            )
        self.assertEqual(rc, 0)
        self.assertEqual(status, "FINAL_VALIDATE_STATUS: PASSED\n")
        self.assertEqual(
            row["reverie_validation_policy"], "reverie-local-validation/v3"
        )
        self.assertEqual(row["gates_expected"], 7)
        self.assertEqual(row["executed_tests"], 17)
        self.assertEqual(row["passed_tests"], 17)
        self.assertEqual(row["coverage"]["planned_test_nodes"], 2)
        self.assertEqual(row["coverage"]["executed_test_nodes"], 2)

    def test_reverie_node_refuses_v4_count_and_coverage_gaps(self) -> None:
        for label, options, detail in (
            (
                "aggregate mismatch",
                {"aggregate_executed": 33},
                "aggregate test counts disagree",
            ),
            (
                "two of three test gates",
                {"regular_executed": 0},
                "requires all three counted test gates",
            ),
            (
                "passing aggregate count disagrees",
                {"aggregate_passed": 33},
                "requires passed_tests == executed_tests",
            ),
        ):
            with self.subTest(label=label), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                checkout = root / "reverie"
                checkout.mkdir()
                stderr = StringIO()
                with redirect_stderr(stderr):
                    rc, status, _row = self.run_reverie_node(
                        root,
                        checkout,
                        validate_exit=0,
                        expect_record=False,
                        **options,
                    )
                self.assertEqual(rc, 75)
                self.assertEqual(status, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
                self.assertIn(detail, stderr.getvalue())

    def test_reverie_node_records_v5_without_inventing_maturity_test_counts(self) -> None:
        for validate_exit, expected in ((0, "PASSED"), (1, "FAILED"), (75, "COULD_NOT_RUN")):
            with self.subTest(validate_exit=validate_exit), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                checkout = root / "reverie"
                checkout.mkdir()
                rc, status, row = self.run_reverie_node(
                    root, checkout, validate_exit=validate_exit, maturity_v5=True
                )
            self.assertEqual(rc, validate_exit)
            self.assertEqual(status, f"FINAL_VALIDATE_STATUS: {expected}\n")
            self.assertEqual(row["reverie_validation_policy"], "reverie-local-validation/v5")
            self.assertEqual(row["gates_expected"], 8)
            self.assertEqual(row["executed_tests"], 17)
            self.assertEqual(row["passed_tests"], 17)
            self.assertEqual(row["filtered_tests"], 4)
            self.assertEqual(row["coverage"]["planned_test_nodes"], 2)
            self.assertEqual(row["coverage"]["executed_test_nodes"], 2)
            self.assertEqual(row["gates"][3], {"name": "Backend maturity"})

    def test_reverie_node_refuses_v5_count_and_coverage_gaps(self) -> None:
        for options, detail in (
            ({"aggregate_executed": 18}, "aggregate test counts disagree"),
            ({"regular_executed": 0}, "requires both counted test gates"),
            (
                {"aggregate_passed": 16},
                "requires passed_tests == executed_tests",
            ),
        ):
            with self.subTest(options=options), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                checkout = root / "reverie"
                checkout.mkdir()
                stderr = StringIO()
                with redirect_stderr(stderr):
                    rc, status, _ = self.run_reverie_node(
                        root, checkout, validate_exit=0, maturity_v5=True,
                        expect_record=False, **options,
                    )
                self.assertEqual(rc, 75)
                self.assertEqual(status, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
                self.assertIn(detail, stderr.getvalue())

    def test_reverie_node_refuses_missing_or_malformed_passed_counts(self) -> None:
        def remove_aggregate(source: dict) -> None:
            del source["passed_tests"]

        def remove_regular_gate(source: dict) -> None:
            regular = next(
                gate
                for gate in source["gates"]
                if gate["name"] == "Test regular workspace cases"
            )
            del regular["passed_tests"]

        for maturity_v5 in (False, True):
            for label, options, detail in (
                (
                    "missing aggregate",
                    {"mutate_source": remove_aggregate},
                    "source row passed_tests",
                ),
                (
                    "null aggregate",
                    {"aggregate_passed": None},
                    "source row passed_tests",
                ),
                (
                    "malformed aggregate",
                    {"aggregate_passed": "17"},
                    "source row passed_tests",
                ),
                (
                    "missing per-gate",
                    {"mutate_source": remove_regular_gate},
                    "Test regular workspace cases.*passed_tests",
                ),
            ):
                with (
                    self.subTest(maturity_v5=maturity_v5, label=label),
                    tempfile.TemporaryDirectory() as raw,
                ):
                    root = Path(raw)
                    checkout = root / "reverie"
                    checkout.mkdir()
                    stderr = StringIO()
                    with redirect_stderr(stderr):
                        rc, status, _ = self.run_reverie_node(
                            root,
                            checkout,
                            validate_exit=0,
                            maturity_v5=maturity_v5,
                            expect_record=False,
                            **options,
                        )
                    self.assertEqual(rc, 75)
                    self.assertEqual(
                        status, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
                    )
                    self.assertRegex(stderr.getvalue(), detail)

    def test_reverie_node_compares_aggregate_and_per_gate_passed_counts(self) -> None:
        for maturity_v5, passed in ((False, 33), (True, 16)):
            with (
                self.subTest(maturity_v5=maturity_v5),
                tempfile.TemporaryDirectory() as raw,
            ):
                root = Path(raw)
                checkout = root / "reverie"
                checkout.mkdir()
                stderr = StringIO()
                with redirect_stderr(stderr):
                    rc, status, _ = self.run_reverie_node(
                        root,
                        checkout,
                        validate_exit=1,
                        maturity_v5=maturity_v5,
                        aggregate_passed=passed,
                        expect_record=False,
                    )
                self.assertEqual(rc, 75)
                self.assertEqual(status, "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n")
                self.assertIn("aggregate test counts disagree", stderr.getvalue())

    def test_reverie_node_records_failed_test_counts_without_invention(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            checkout = root / "reverie"
            checkout.mkdir()
            rc, status, row = self.run_reverie_node(
                root,
                checkout,
                validate_exit=1,
                regular_passed=11,
                aggregate_passed=33,
                regular_result="fail",
            )
        self.assertEqual(rc, 1)
        self.assertEqual(status, "FINAL_VALIDATE_STATUS: FAILED\n")
        self.assertEqual(row["result"], "fail")
        self.assertEqual(row["executed_tests"], 34)
        self.assertEqual(row["passed_tests"], 33)
        regular = next(
            gate
            for gate in row["gates"]
            if gate["name"] == "Test regular workspace cases"
        )
        self.assertEqual(regular["executed_tests"], 12)
        self.assertEqual(regular["passed_tests"], 11)
        self.assertEqual(regular["result"], "fail")

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
                    "result": "pass",
                    "executed_tests": 10,
                    "passed_tests": 10,
                    "filtered_tests": 3,
                },
                {
                    "name": "Documentation tests",
                    "result": "pass",
                    "executed_tests": 4,
                    "passed_tests": 4,
                    "filtered_tests": 0,
                },
            ]
        }
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                complete, reverie_node.V3_TEST_GATES
            ),
            (14, 14, 3, [], []),
        )
        complete["gates"][1]["executed_tests"] = 0
        complete["gates"][1]["passed_tests"] = 0
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                complete, reverie_node.V3_TEST_GATES
            ),
            (10, 10, 3, ["Documentation tests"], []),
        )
        missing = {"gates": complete["gates"][:1]}
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                missing, reverie_node.V3_TEST_GATES
            ),
            (10, 10, 3, [], ["Documentation tests"]),
        )

    def test_missing_per_gate_count_refuses_by_field_name(self) -> None:
        source = {
            "gates": [
                {
                    "name": "Test regular workspace cases",
                    "result": "pass",
                    "executed_tests": 10,
                    "passed_tests": 10,
                    "filtered_tests": 3,
                },
                {
                    "name": "Documentation tests",
                    "result": "pass",
                    "passed_tests": 0,
                    "filtered_tests": 0,
                },
            ]
        }
        with self.assertRaisesRegex(
            reverie_node.Refusal, "Documentation tests.*executed_tests"
        ):
            reverie_node.recorded_gate_coverage(
                source, reverie_node.V3_TEST_GATES
            )

    def test_per_gate_passed_counts_are_typed_and_consistent(self) -> None:
        def source(*, result: str = "pass", passed: object = 10) -> dict:
            return {
                "gates": [
                    {
                        "name": "Test regular workspace cases",
                        "result": result,
                        "executed_tests": 10,
                        "passed_tests": passed,
                        "filtered_tests": 3,
                    },
                    {
                        "name": "Documentation tests",
                        "result": "pass",
                        "executed_tests": 4,
                        "passed_tests": 4,
                        "filtered_tests": 0,
                    },
                ]
            }

        for value in (None, "10", True, -1):
            with self.subTest(value=value):
                with self.assertRaisesRegex(
                    reverie_node.Refusal,
                    "Test regular workspace cases.*passed_tests",
                ):
                    reverie_node.recorded_gate_coverage(
                        source(passed=value), reverie_node.V3_TEST_GATES
                    )
        missing = source()
        del missing["gates"][0]["passed_tests"]
        with self.assertRaisesRegex(
            reverie_node.Refusal, "Test regular workspace cases.*passed_tests"
        ):
            reverie_node.recorded_gate_coverage(
                missing, reverie_node.V3_TEST_GATES
            )
        with self.assertRaisesRegex(reverie_node.Refusal, "must not exceed"):
            reverie_node.recorded_gate_coverage(
                source(result="fail", passed=11), reverie_node.V3_TEST_GATES
            )
        with self.assertRaisesRegex(
            reverie_node.Refusal, "requires passed_tests == executed_tests"
        ):
            reverie_node.recorded_gate_coverage(
                source(passed=9), reverie_node.V3_TEST_GATES
            )
        self.assertEqual(
            reverie_node.recorded_gate_coverage(
                source(result="fail", passed=9), reverie_node.V3_TEST_GATES
            ),
            (14, 13, 3, [], []),
        )

    def test_v4_owned_public_gate_requires_exact_counts(self) -> None:
        def source(executed: int, passed: int, filtered: int) -> dict:
            return {
                "gates": [
                    {
                        "name": "Test all owned public instruction cases",
                        "result": "pass",
                        "executed_tests": executed,
                        "passed_tests": passed,
                        "filtered_tests": filtered,
                    },
                    {
                        "name": "Test regular workspace cases",
                        "result": "pass",
                        "executed_tests": 12,
                        "passed_tests": 12,
                        "filtered_tests": 3,
                    },
                    {
                        "name": "Documentation tests",
                        "result": "pass",
                        "executed_tests": 5,
                        "passed_tests": 5,
                        "filtered_tests": 1,
                    },
                ]
            }

        self.assertEqual(
            reverie_node.recorded_gate_coverage(source(17, 17, 0)),
            (34, 34, 4, [], []),
        )
        for executed, filtered in ((16, 0), (18, 0), (17, 1), (0, 0)):
            with self.subTest(executed=executed, filtered=filtered):
                with self.assertRaisesRegex(
                    reverie_node.Refusal, "must record exactly executed_tests=17"
                ):
                    reverie_node.recorded_gate_coverage(
                        source(executed, executed, filtered)
                    )
        with self.assertRaisesRegex(
            reverie_node.Refusal, "requires passed_tests == executed_tests"
        ):
            reverie_node.recorded_gate_coverage(source(17, 16, 0))

    def test_aggregate_counts_come_from_the_framework_row(self) -> None:
        self.assertEqual(
            reverie_node.recorded_test_counts(
                {
                    "result": "pass",
                    "executed_tests": 17,
                    "passed_tests": 17,
                    "filtered_tests": 4,
                }
            ),
            (17, 17, 4),
        )
        for field, value in (
            ("executed_tests", None),
            ("executed_tests", "17"),
            ("executed_tests", True),
            ("executed_tests", -1),
            ("passed_tests", None),
            ("passed_tests", "17"),
            ("passed_tests", True),
            ("passed_tests", -1),
            ("filtered_tests", None),
        ):
            row = {
                "result": "pass",
                "executed_tests": 17,
                "passed_tests": 17,
                "filtered_tests": 4,
                field: value,
            }
            with self.assertRaisesRegex(reverie_node.Refusal, field):
                reverie_node.recorded_test_counts(row)

        missing = {
            "result": "pass",
            "executed_tests": 17,
            "filtered_tests": 4,
        }
        with self.assertRaisesRegex(reverie_node.Refusal, "passed_tests"):
            reverie_node.recorded_test_counts(missing)
        with self.assertRaisesRegex(reverie_node.Refusal, "must not exceed"):
            reverie_node.recorded_test_counts(
                {
                    "result": "fail",
                    "executed_tests": 17,
                    "passed_tests": 18,
                    "filtered_tests": 4,
                }
            )
        with self.assertRaisesRegex(
            reverie_node.Refusal, "requires passed_tests == executed_tests"
        ):
            reverie_node.recorded_test_counts(
                {
                    "result": "pass",
                    "executed_tests": 17,
                    "passed_tests": 16,
                    "filtered_tests": 4,
                }
            )
        self.assertEqual(
            reverie_node.recorded_test_counts(
                {
                    "result": "fail",
                    "executed_tests": 17,
                    "passed_tests": 16,
                    "filtered_tests": 4,
                }
            ),
            (17, 16, 4),
        )

    def test_versioned_gate_contracts_are_exact(self) -> None:
        self.assertEqual(
            reverie_node.EXPECTED_GATES,
            (
                "Cross-client skill discovery",
                "Build workspace",
                "DBT virtual identity and pidfd_open policy",
                "Compile owned public instruction target",
                "Inventory owned public instruction target",
                "Test all owned public instruction cases",
                "Test regular workspace cases",
                "Documentation tests",
                "Clippy",
                "Rustfmt",
            ),
        )
        current = [{"name": name} for name in reverie_node.EXPECTED_GATES]
        policy, gates, test_gates = reverie_node.require_expected_gate_set(current)
        self.assertEqual(policy, "reverie-local-validation/v4")
        self.assertEqual(gates, reverie_node.V4_GATES)
        self.assertEqual(test_gates, reverie_node.V4_TEST_GATES)
        old = [{"name": name} for name in reverie_node.V3_GATES]
        self.assertEqual(
            reverie_node.require_expected_gate_set(old),
            (
                "reverie-local-validation/v3",
                reverie_node.V3_GATES,
                reverie_node.V3_TEST_GATES,
            ),
        )
        invalid_sets = {
            "retired merge gate": current[:1]
            + [{"name": "Merge-gate policy"}]
            + current[1:],
            "missing gate": current[:-1],
            "reordered gates": [current[1], current[0], *current[2:]],
            "v3 plus one v4 gate": old[:3] + current[3:4] + old[3:],
        }
        for index in range(3, 6):
            invalid_sets[f"missing added gate {index}"] = (
                current[:index] + current[index + 1 :]
            )
            reordered = list(current)
            reordered[index - 1], reordered[index] = (
                reordered[index],
                reordered[index - 1],
            )
            invalid_sets[f"reordered added gate {index}"] = reordered
        for case, gates in invalid_sets.items():
            with self.subTest(case=case):
                with self.assertRaisesRegex(reverie_node.Refusal, "gate set drifted"):
                    reverie_node.require_expected_gate_set(gates)

    def test_v5_gate_contract_requires_exact_maturity_placement(self) -> None:
        names = (
            "Cross-client skill discovery", "Build workspace",
            "DBT virtual identity and pidfd_open policy", "Backend maturity",
            "Test regular workspace cases", "Documentation tests", "Clippy", "Rustfmt",
        )
        self.assertEqual(reverie_node.V5_GATES, names)
        gates = [{"name": name} for name in names]
        self.assertEqual(
            reverie_node.require_expected_gate_set(gates),
            ("reverie-local-validation/v5", names, reverie_node.V3_TEST_GATES),
        )
        invalid = {
            "duplicate maturity": gates[:4] + [gates[3]] + gates[4:],
            "late maturity": gates[:3] + gates[4:] + [gates[3]],
            "missing existing gate": gates[:4] + gates[5:],
            "v4 plus maturity": [{"name": name} for name in reverie_node.V4_GATES]
            + [gates[3]],
            "unknown gate": gates[:3] + [{"name": "Other maturity"}] + gates[4:],
        }
        for label, rows in invalid.items():
            with self.subTest(label=label), self.assertRaisesRegex(
                reverie_node.Refusal, "gate set drifted"
            ):
                reverie_node.require_expected_gate_set(rows)

    def test_safe_ci_cgroup_refuses_unboxed_and_accepts_step_scope(self) -> None:
        with mock.patch.object(Path, "read_text", return_value="0::/user.slice/agent.scope\n"):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node.safe_ci_cgroup()
        boxed_paths = [
            (
                "/user.slice/user-212630.slice/user@212630.service/safe.slice/"
                "safe-ci.slice/safe-ci-123.scope/step-validate.reverie-full"
            ),
            (
                "/user.slice/user-212630.slice/user@212630.service/dagrun.slice/"
                "dagrun-153463.scope/step-validate.reverie-full"
            ),
        ]
        for path in boxed_paths:
            with self.subTest(path=path):
                with mock.patch.object(Path, "read_text", return_value=f"0::{path}\n"):
                    self.assertEqual(reverie_node.safe_ci_cgroup(), path)

        refused_paths = [
            "/user.slice/safe.slice/safe-ci.slice/unsafe-ci-123.scope/step-validate.reverie-full",
            "/user.slice/safe.slice/safe-ci.slice/safe-ci-worker.scope/step-validate.reverie-full",
            "/user.slice/safe.slice/safe-ci.slice/safe-ci-123.scope/nested/step-validate.reverie-full",
            "/user.slice/dagrun.slice/worker-153463.scope/step-validate.reverie-full",
            "/user.slice/dagrun-153463.scope/step-validate.reverie-full",
            "/user.slice/dagrun.slice/dagrun-153463.scope/nested/step-validate.reverie-full",
            "/user.slice/dagrun.slice/dagrun-153463.scope/step-validate.other",
            "/user.slice//dagrun.slice/dagrun-153463.scope/step-validate.reverie-full",
        ]
        for path in refused_paths:
            with self.subTest(path=path):
                with mock.patch.object(Path, "read_text", return_value=f"0::{path}\n"):
                    with self.assertRaises(reverie_node.Refusal):
                        reverie_node.safe_ci_cgroup()

    def test_kvm_requirement_refuses_absence_and_accepts_exact_value(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(reverie_node.Refusal):
                reverie_node.require_kvm_environment()
        with mock.patch.dict(os.environ, {"REVERIE_REQUIRE_KVM": "1"}, clear=True):
            reverie_node.require_kvm_environment()


if __name__ == "__main__":
    unittest.main()
