#!/usr/bin/env python3
"""Focused non-Hermit tests for the HTTPCoro time-input run boundary."""

from __future__ import annotations

import copy
import json
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import admit_httpcoro_time_seed as admit
from admit_httpcoro_time_seed import (
    FAILURE_CLASS,
    FATAL_TEXT,
    PASSED_ONE,
    SELECTED_STACK,
    STRICT_COMPARISON,
    STRICT_SUCCESS,
    Refusal,
    assert_admission_boundary,
    authenticate_qgroups,
    authenticate_safehermit,
    build_cell,
    classify,
    validate_run_dir,
)
from create_httpcoro_time_plan import HERMIT_BINARY, HERMIT_SHA256, TEST_DISPLAY
from run_httpcoro_time_seed import (
    RECORD,
    RECORD_REL,
    REPO_ROOT,
    evidence_report_locator,
    materialize_argv,
    next_seed,
    parse_qgroup_report,
)


STRICT = STRICT_COMPARISON + "\n" + STRICT_SUCCESS + "\n"
START = (
    "Note: Google Test filter = "
    + TEST_DISPLAY
    + "\n[==========] Running 1 test from 1 test suite.\n"
    + "[----------] Global test environment set-up.\n"
    + f"[ RUN      ] {TEST_DISPLAY}\n"
)
PASS = (
    START
    + f"[       OK ] {TEST_DISPLAY} (97 ms)\n"
    + "[==========] 1 test from 1 test suite ran. (98 ms total)\n"
    + PASSED_ONE
    + "\n"
)
BENIGN_ASAN_WARNING = (
    "==3==WARNING: ASan doesn't fully support makecontext/swapcontext functions "
    "and may produce false positives in some cases!\n"
)
TARGET_FAIL = (
    STRICT
    + "F0101 00:00:02.614037     3 "
    + FATAL_TEXT
    + " \n*** Check failure stack trace: ***\n"
    + BENIGN_ASAN_WARNING
    + f"    @ 0000000000eb36c2 proxygen::coro::test::{SELECTED_STACK}::co_TestBody()\n"
)


def termination(
    *,
    exit_code: int = 0,
    signal_number: int | None = None,
    unit_result: str = "success",
    timed_out: bool = False,
) -> dict[str, object]:
    return {
        "exit_code": exit_code,
        "signal_number": signal_number,
        "unit_result": unit_result,
        "timed_out": timed_out,
    }


class OracleClassifierTest(unittest.TestCase):
    def test_exact_pass(self) -> None:
        result = classify(PASS, STRICT, termination())
        self.assertEqual(result.status, "VALID")
        self.assertEqual(result.target_outcome, "PASS")
        self.assertIsNone(result.failure_class_id)
        self.assertTrue(result.strict_match)

    def test_exact_line_1685_sigabrt_fail_accepts_known_asan_warning(self) -> None:
        result = classify(
            START,
            TARGET_FAIL,
            termination(
                exit_code=134,
                signal_number=signal.SIGABRT,
                unit_result="core-dump",
            ),
        )
        self.assertEqual(result.status, "VALID")
        self.assertEqual(result.target_outcome, "FAIL")
        self.assertEqual(result.failure_class_id, FAILURE_CLASS)

    def test_actual_sanitizer_verdict_is_censored(self) -> None:
        result = classify(
            START,
            TARGET_FAIL + "ERROR: AddressSanitizer: heap-use-after-free\n",
            termination(
                exit_code=134,
                signal_number=signal.SIGABRT,
                unit_result="core-dump",
            ),
        )
        self.assertEqual(result.status, "CENSORED")
        self.assertIsNone(result.target_outcome)

    def test_wrong_line_is_censored(self) -> None:
        result = classify(
            START,
            TARGET_FAIL.replace("cpp:1685]", "cpp:1686]"),
            termination(
                exit_code=134,
                signal_number=signal.SIGABRT,
                unit_result="core-dump",
            ),
        )
        self.assertEqual(result.status, "CENSORED")

    def test_missing_selected_stack_is_censored(self) -> None:
        result = classify(
            START,
            TARGET_FAIL.replace(SELECTED_STACK, "Different_Test"),
            termination(
                exit_code=134,
                signal_number=signal.SIGABRT,
                unit_result="core-dump",
            ),
        )
        self.assertEqual(result.status, "CENSORED")

    def test_wrong_signal_is_censored(self) -> None:
        result = classify(
            START,
            TARGET_FAIL,
            termination(
                exit_code=139,
                signal_number=signal.SIGSEGV,
                unit_result="core-dump",
            ),
        )
        self.assertEqual(result.status, "CENSORED")

    def test_selected_ok_line_prevents_fail_class(self) -> None:
        result = classify(
            START + f"[       OK ] {TEST_DISPLAY} (97 ms)\n",
            TARGET_FAIL,
            termination(
                exit_code=134,
                signal_number=signal.SIGABRT,
                unit_result="core-dump",
            ),
        )
        self.assertEqual(result.status, "CENSORED")

    def test_extra_selected_or_unrelated_run_is_censored(self) -> None:
        for stdout in (
            START + f"[ RUN      ] {TEST_DISPLAY}\n",
            START + "[ RUN      ] OtherSuite.OtherTest\n",
        ):
            with self.subTest(stdout=stdout[-80:]):
                result = classify(stdout, STRICT, termination())
                self.assertEqual(result.status, "CENSORED")

    def test_missing_strict_comparison_is_censored(self) -> None:
        result = classify(PASS, STRICT_SUCCESS + "\n", termination())
        self.assertEqual(result.status, "CENSORED")
        self.assertFalse(result.strict_match)

    def test_outer_timeout_is_censored(self) -> None:
        result = classify(
            START,
            "",
            termination(exit_code=124, unit_result="timeout", timed_out=True),
        )
        self.assertEqual(result.status, "CENSORED")
        self.assertEqual(result.harness_status, "TIMEOUT")

    def test_internal_failure_cannot_be_pass_or_fail(self) -> None:
        pass_result = classify(
            PASS, STRICT + "HERMIT_INTERNAL_FAILURE\n", termination()
        )
        fail_result = classify(
            START,
            TARGET_FAIL + "Sandbox container exited unexpectedly\n",
            termination(
                exit_code=134,
                signal_number=signal.SIGABRT,
                unit_result="core-dump",
            ),
        )
        self.assertEqual(pass_result.status, "CENSORED")
        self.assertEqual(fail_result.status, "CENSORED")


class BoundaryAuthenticationTest(unittest.TestCase):
    def setUp(self) -> None:
        self.record = json.loads(RECORD.read_text())

    def test_sealed_prefix_starts_at_seven_and_cannot_skip(self) -> None:
        pristine = copy.deepcopy(self.record)
        pristine.update(
            plan_receipt=None,
            attempt_state="PLANNED",
            cells=[],
            events=[],
            samples=[],
            matchers=[],
            outcome_history=[],
            current_outcome=None,
        )
        self.assertEqual(next_seed(pristine), (0, 7))
        material = copy.deepcopy(pristine)
        material.update(
            attempt_state="MATERIAL",
            samples=[
                {
                    "selection_index": 0,
                    "seed": {"flag": "--sched-seed", "value": 7},
                }
            ],
            current_outcome={
                "kind": "NOT-DETERMINABLE",
                "measurement_budget_exhausted": False,
            },
        )
        self.assertEqual(next_seed(material), (1, 5))
        material["samples"][0]["seed"]["value"] = 5
        with self.assertRaisesRegex(Refusal, "exact sealed prefix"):
            next_seed(material)

    def test_argv_is_literal_and_only_replaces_two_placeholders(self) -> None:
        locator = evidence_report_locator(0, 7)
        argv = materialize_argv(self.record, 7, locator)
        self.assertEqual(argv[0], "bin/safehermit")
        self.assertEqual(argv[argv.index("--sched-seed") + 1], "7")
        self.assertEqual(argv[argv.index("--sh-report") + 1], locator.as_posix())
        self.assertNotIn("{SWEEP}", argv)
        self.assertNotIn("{SAFEHERMIT_REPORT}", argv)
        self.assertIn("--epoch=2026-09-21T18:14:41Z", argv)
        self.assertIn("--clock-multiplier=0.125", argv)

    def test_qgroup_parser_requires_exact_live_limit(self) -> None:
        self.assertEqual(
            parse_qgroup_report(
                "referenced=16384 exclusive=16384 limit=8000000000\n"
            ),
            (16384, 16384, 8_000_000_000),
        )
        for bad in (
            "referenced=1 exclusive=1 limit=9000000000\n",
            "referenced=8000000000 exclusive=1 limit=8000000000\n",
            "referenced=1 exclusive=2 limit=8000000000\n",
            "garbage\n",
        ):
            with self.subTest(report=bad):
                with self.assertRaises(Refusal):
                    parse_qgroup_report(bad)

    def test_live_qgroup_may_grow_after_relatime_reads_but_not_shrink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            paths = {
                "verifier_qgroup_before": root / "before.txt",
                "verifier_qgroup_after": root / "after.txt",
                "safehermit_qgroup_after": root / "safe.txt",
            }
            before = "referenced=10 exclusive=10 limit=8000000000\n"
            after = "referenced=20 exclusive=20 limit=8000000000\n"
            safe = "referenced=30 exclusive=30 limit=8000000000\n"
            paths["verifier_qgroup_before"].write_text(before)
            paths["verifier_qgroup_after"].write_text(after)
            paths["safehermit_qgroup_after"].write_text(safe)
            manifest = {
                "preflight": {"qgroup": {"stdout": before}},
                "verifier_qgroup_after": {"stdout": after},
                "safehermit_qgroup_after": {"stdout": safe},
            }
            live = [
                {"stdout": "referenced=21 exclusive=20 limit=8000000000\n"},
                {"stdout": "referenced=31 exclusive=30 limit=8000000000\n"},
            ]
            with mock.patch.object(admit, "qgroup_report", side_effect=live):
                authenticate_qgroups(manifest, paths, {"log_dir": root / "log"})
            backwards = [
                {"stdout": "referenced=19 exclusive=19 limit=8000000000\n"},
                {"stdout": "referenced=31 exclusive=30 limit=8000000000\n"},
            ]
            with mock.patch.object(admit, "qgroup_report", side_effect=backwards):
                with self.assertRaisesRegex(Refusal, "moved backwards"):
                    authenticate_qgroups(
                        manifest, paths, {"log_dir": root / "log"}
                    )

    def test_cell_keeps_strict_time_input_configuration(self) -> None:
        cell = build_cell(self.record)
        self.assertEqual(cell["hermit"]["swept_coordinate"], "--sched-seed")
        self.assertEqual(cell["hermit"]["relaxations"], [])
        self.assertTrue(cell["hermit"]["strict"])
        self.assertEqual(cell["timing"]["effective_max_timeslice"], 200_000_000)
        self.assertEqual(cell["resource_bounds"], self.record["plan"]["resource_bounds"])

    def test_prepared_boundary_allows_only_actual_report_dirty_path(self) -> None:
        report_path = REPO_ROOT / evidence_report_locator(0, 7)
        repository_record = RECORD.relative_to(REPO_ROOT).as_posix()
        head = "1" * 40
        blob = "2" * 40
        route = self.record["plan"]["authorized_routes"][0]
        manifest = {
            "execution_boundary": {
                "head_commit": head,
                "record_repository_path": repository_record,
                "record_blob": blob,
                "plan_sha256": self.record["plan"]["plan_sha256"],
                "route_id": route["route_id"],
                "route_sha256": route["route_sha256"],
                "plan_receipt": {
                    "commit": head,
                    "path": RECORD_REL.as_posix(),
                    "blob": blob,
                },
            }
        }

        def fake_git(*args: str) -> subprocess.CompletedProcess[str]:
            if args[0] == "show":
                return subprocess.CompletedProcess(args, 0, RECORD.read_text(), "")
            if args[0] == "status":
                path = report_path.relative_to(REPO_ROOT).as_posix()
                return subprocess.CompletedProcess(args, 0, f"?? {path}\0", "")
            if args == ("rev-parse", "HEAD"):
                return subprocess.CompletedProcess(args, 0, head + "\n", "")
            if args[0] == "rev-parse":
                return subprocess.CompletedProcess(args, 0, blob + "\n", "")
            raise AssertionError(args)

        with mock.patch.object(admit, "git_result", side_effect=fake_git):
            assert_admission_boundary(self.record, manifest, report_path)

        def dirty_git(*args: str) -> subprocess.CompletedProcess[str]:
            result = fake_git(*args)
            if args[0] == "status":
                return subprocess.CompletedProcess(
                    args, 0, result.stdout + "?? unrelated-file\0", ""
                )
            return result

        with mock.patch.object(admit, "git_result", side_effect=dirty_git):
            with self.assertRaisesRegex(Refusal, "more than the exact"):
                assert_admission_boundary(self.record, manifest, report_path)

    def test_run_dir_requires_eventual_replica_one_tracked_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            campaign = base / "campaign"
            scratch = base / "scratch"
            run_dir = scratch / "selection-000-sched-seed-00000000000000000007"
            run_dir.mkdir(parents=True)
            report_locator = evidence_report_locator(0, 7)
            report = campaign / report_locator
            report.parent.mkdir(parents=True)
            report.write_text("report\n")
            paths = {
                "stdout": run_dir / "stdout.txt",
                "stderr": run_dir / "stderr.txt",
                "safehermit_report": report,
                "verifier_qgroup_before": run_dir / "verifier-qgroup-before.txt",
                "verifier_qgroup_after": run_dir / "verifier-qgroup-after.txt",
                "safehermit_qgroup_after": run_dir / "safehermit-qgroup-after.txt",
            }
            for key, path in paths.items():
                if key != "safehermit_report":
                    path.write_text("fixture\n")
            manifest = {
                "schema_version": 1,
                "state": "COMPLETE",
                "postcheck_errors": [],
                "selection_index": 0,
                "seed": {"flag": "--sched-seed", "value": 7},
                "cwd": str(campaign),
                "host_environment": {
                    "SAFEHERMIT_LOG_ROOT": str(admit.SAFEHERMIT_LOG_ROOT),
                    "RUST_LOG": None,
                    "HERMIT_BIN": None,
                },
                "paths": {key: str(path) for key, path in paths.items()},
                "argv": materialize_argv(self.record, 7, report_locator),
            }
            (run_dir / "invocation.json").write_text(json.dumps(manifest))
            with mock.patch.object(admit, "SCRATCH_ROOT", str(scratch)), mock.patch.object(
                admit, "ROOT", campaign
            ):
                observed, observed_paths = validate_run_dir(run_dir, self.record, 0, 7)
                self.assertEqual(observed, manifest)
                self.assertEqual(observed_paths["safehermit_report"], report)

                wrong = run_dir / "safehermit-report.txt"
                wrong.write_text("wrong\n")
                manifest["paths"]["safehermit_report"] = str(wrong)
                (run_dir / "invocation.json").write_text(json.dumps(manifest))
                with self.assertRaisesRegex(Refusal, "paths drifted"):
                    validate_run_dir(run_dir, self.record, 0, 7)


class SafehermitReportTest(unittest.TestCase):
    def make_report(
        self,
        root: Path,
        stderr: bytes,
        *,
        exit_code: int = 0,
        runner_exit: int = 0,
        unit_result: str = "success",
        process_signal: int | None = None,
    ) -> tuple[Path, Path]:
        log_dir = root / "brs-safehermit-test"
        log_dir.mkdir()
        log_file = log_dir / "hermit.log"
        log_file.write_bytes(stderr)
        stderr_path = root / "stderr.txt"
        stderr_path.write_bytes(stderr)
        fields = {
            "run_id": "20260921T190000Z-1234",
            "binary": HERMIT_BINARY,
            "binary_source": "argv:positional-path",
            "binary_sha256": HERMIT_SHA256,
            "log_dir": str(log_dir),
            "log_file": str(log_file),
            "unit": "safehermit-20260921T190000Z-1234",
            "bound.wall": "APPLIED:120s",
            "bound.cgroup": "APPLIED:MemoryMax=16G MemorySwapMax=0",
            "bound.disk": (
                "APPLIED:8G for log_dir only; guest paths and explicit log files "
                "elsewhere are outside this bound"
            ),
            "bound.bytes": (
                "APPLIED:67108864 (LETHAL: the run is cgroup-killed at the cap)"
            ),
            "bound.logfilter": (
                "NOT_APPLIED:by owner ruling -- the emitter is left loud so a stuck "
                "run stays visible; the low byte cap is the mechanism that fires"
            ),
            "env.forwarded": "APPLIED:42 variables forwarded",
            "note.run_record": "test record",
            "elapsed_secs": "7",
            "bytes_written": str(len(stderr)),
            "truncated": "false",
            "runner_exit_code": str(runner_exit),
            "unit_result": unit_result,
            "exit_code": str(exit_code),
        }
        if process_signal is not None:
            fields["process_signal"] = str(process_signal)
        report_path = root / "report.txt"
        report_path.write_text(
            "".join(f"safehermit: {key}={value}\n" for key, value in fields.items())
        )
        return report_path, stderr_path

    def test_exact_success_report_authenticates(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with mock.patch.object(admit, "SAFEHERMIT_LOG_ROOT", root):
                report, stderr = self.make_report(root, STRICT.encode())
                observed = authenticate_safehermit(report, stderr, 0)
        self.assertEqual(observed["exit_code"], 0)
        self.assertIsNone(observed["signal_number"])

    def test_exact_sigabrt_report_authenticates(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with mock.patch.object(admit, "SAFEHERMIT_LOG_ROOT", root):
                report, stderr = self.make_report(
                    root,
                    TARGET_FAIL.encode(),
                    exit_code=134,
                    runner_exit=1,
                    unit_result="core-dump",
                    process_signal=signal.SIGABRT,
                )
                observed = authenticate_safehermit(report, stderr, 134)
        self.assertEqual(observed["signal_number"], signal.SIGABRT)

    def test_bound_drift_or_truncation_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with mock.patch.object(admit, "SAFEHERMIT_LOG_ROOT", root):
                report, stderr = self.make_report(root, STRICT.encode())
                original = report.read_text()
                for old, new in (
                    ("APPLIED:120s", "NOT_APPLIED:no unit"),
                    ("truncated=false", "truncated=true"),
                    (HERMIT_SHA256, "0" * 64),
                ):
                    with self.subTest(new=new):
                        report.write_text(original.replace(old, new))
                        with self.assertRaises(Refusal):
                            authenticate_safehermit(report, stderr, 0)

    def test_stderr_must_equal_retained_capped_log(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with mock.patch.object(admit, "SAFEHERMIT_LOG_ROOT", root):
                report, stderr = self.make_report(root, STRICT.encode())
                stderr.write_text("different\n")
                with self.assertRaisesRegex(Refusal, "differs"):
                    authenticate_safehermit(report, stderr, 0)


if __name__ == "__main__":
    unittest.main()
