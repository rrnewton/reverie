#!/usr/bin/env python3
"""Focused non-Hermit tests for the AsyncSSL sched-slowdown run boundary."""

from __future__ import annotations

import copy
import io
import json
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import admit_asyncssl_sched_slowdown_seed as admit
from admit_asyncssl_sched_slowdown_seed import (
    ACK_FAILURE_LINES,
    EVENT_FAILURE_PREFIX,
    FAILURE_CLASS,
    FAILED_LIST_HEADER,
    FAILED_LIST_LINE,
    FAILED_TESTS,
    PASSED_ONE,
    PASSED_ZERO,
    STRICT_COMPARISON,
    STRICT_SUCCESS,
    Refusal,
    assert_admission_boundary,
    authenticate_stdout_cap,
    authenticate_qgroups,
    authenticate_safehermit,
    build_cell,
    classify,
    validate_run_dir,
)
from create_asyncssl_sched_slowdown_plan import HERMIT_BINARY, HERMIT_SHA256, TEST_DISPLAY
from run_asyncssl_sched_slowdown_seed import (
    RECORD,
    RECORD_REL,
    REPO_ROOT,
    STDOUT_CAP_METHOD,
    STDOUT_CHUNK,
    STDOUT_LIMIT,
    assert_live_helper_blobs,
    evidence_report_locator,
    capture_stdout,
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
EVENT_FAILURE_BLOCK = (
    "\n".join(EVENT_FAILURE_PREFIX)
    + "\n  Actual: { event-0, event-1, event-2 }, whose size 3 doesn't match\n"
)
ACK_FAILURE_BLOCK = "\n".join(ACK_FAILURE_LINES) + "\n"
FAIL_SUMMARY = (
    f"[  FAILED  ] {TEST_DISPLAY} (17 ms)\n"
    + "[----------] 1 test from AsyncSSLSocketByteEventTest (17 ms total)\n\n"
    + "[----------] Global test environment tear-down\n"
    + "[==========] 1 test from 1 test suite ran. (18 ms total)\n"
    + PASSED_ZERO
    + "\n"
    + FAILED_LIST_HEADER
    + "\n"
    + FAILED_LIST_LINE
    + "\n\n"
    + FAILED_TESTS
    + "\n"
)
EXACT_FAIL = START + EVENT_FAILURE_BLOCK + "\n" + ACK_FAILURE_BLOCK + "\n" + FAIL_SUMMARY


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

    def test_exact_paired_ordinary_exit_one_fail(self) -> None:
        result = classify(
            EXACT_FAIL,
            STRICT,
            termination(exit_code=1, unit_result="exit-code"),
        )
        self.assertEqual(result.status, "VALID")
        self.assertEqual(result.target_outcome, "FAIL")
        self.assertEqual(result.failure_class_id, FAILURE_CLASS)

    def test_each_single_failure_block_is_censored(self) -> None:
        for stdout in (
            START + EVENT_FAILURE_BLOCK + FAIL_SUMMARY,
            START + ACK_FAILURE_BLOCK + FAIL_SUMMARY,
        ):
            with self.subTest(block=stdout[stdout.index("Failure") - 16 :]):
                result = classify(
                    stdout,
                    STRICT,
                    termination(exit_code=1, unit_result="exit-code"),
                )
                self.assertEqual(result.status, "CENSORED")
                self.assertIsNone(result.failure_class_id)

    def test_sanitizer_on_otherwise_pass_is_censored(self) -> None:
        result = classify(
            PASS,
            STRICT + "ERROR: AddressSanitizer: heap-use-after-free\n",
            termination(),
        )
        self.assertEqual(result.status, "CENSORED")

    def test_contradictory_zero_pass_summary_is_censored(self) -> None:
        result = classify(PASS + PASSED_ZERO + "\n", STRICT, termination())
        self.assertEqual(result.status, "CENSORED")

    def test_wrong_source_line_size_or_ack_value_is_censored(self) -> None:
        mutations = (
            EXACT_FAIL.replace(".cpp:3797:", ".cpp:3798:"),
            EXACT_FAIL.replace("whose size 3 doesn't match", "whose size 4 doesn't match"),
            EXACT_FAIL.replace("Which is: 501", "Which is: 500"),
            EXACT_FAIL.replace(
                "16-byte object <00-00 00-00 00-00 00-00 00-00 00-00 00-00 00-00>",
                "16-byte object <01-00 00-00 00-00 00-00 00-00 00-00 00-00 00-00>",
            ),
        )
        for stdout in mutations:
            with self.subTest(change=stdout):
                result = classify(
                    stdout,
                    STRICT,
                    termination(exit_code=1, unit_result="exit-code"),
                )
                self.assertEqual(result.status, "CENSORED")

    def test_wrong_signal_is_censored(self) -> None:
        result = classify(
            EXACT_FAIL,
            STRICT,
            termination(
                exit_code=139,
                signal_number=signal.SIGSEGV,
                unit_result="core-dump",
            ),
        )
        self.assertEqual(result.status, "CENSORED")

    def test_selected_ok_line_prevents_fail_class(self) -> None:
        result = classify(
            EXACT_FAIL + f"[       OK ] {TEST_DISPLAY} (97 ms)\n",
            STRICT,
            termination(exit_code=1, unit_result="exit-code"),
        )
        self.assertEqual(result.status, "CENSORED")

    def test_sanitizer_with_exact_blocks_is_censored_in_either_stream(self) -> None:
        contaminated = (
            (
                EXACT_FAIL,
                STRICT + "ERROR: LeakSanitizer: detected memory leaks\n",
            ),
            (
                EXACT_FAIL + "ERROR: AddressSanitizer: heap-use-after-free\n",
                STRICT,
            ),
            (
                EXACT_FAIL,
                STRICT + "FATAL: ThreadSanitizer: unexpected memory mapping\n",
            ),
            (
                EXACT_FAIL,
                STRICT + "LeakSanitizer has encountered a fatal error.\n",
            ),
        )
        for stdout, stderr in contaminated:
            with self.subTest(stderr=stderr[-80:], stdout=stdout[-80:]):
                result = classify(
                    stdout,
                    stderr,
                    termination(exit_code=1, unit_result="exit-code"),
                )
                self.assertEqual(result.status, "CENSORED")
                self.assertIsNone(result.failure_class_id)

    def test_missing_timed_failure_or_summary_is_censored(self) -> None:
        mutations = (
            EXACT_FAIL.replace(f"[  FAILED  ] {TEST_DISPLAY} (17 ms)\n", "", 1),
            EXACT_FAIL.replace(FAILED_LIST_HEADER + "\n", ""),
            EXACT_FAIL.replace(FAILED_LIST_LINE + "\n\n", ""),
            EXACT_FAIL.replace(FAILED_TESTS + "\n", ""),
        )
        for stdout in mutations:
            with self.subTest(stdout=stdout[-160:]):
                result = classify(
                    stdout,
                    STRICT,
                    termination(exit_code=1, unit_result="exit-code"),
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
            EXACT_FAIL,
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
            EXACT_FAIL,
            STRICT + "Sandbox container exited unexpectedly\n",
            termination(exit_code=1, unit_result="exit-code"),
        )
        self.assertEqual(pass_result.status, "CENSORED")
        self.assertEqual(fail_result.status, "CENSORED")


class BoundaryAuthenticationTest(unittest.TestCase):
    def setUp(self) -> None:
        self.record = json.loads(RECORD.read_text())

    def test_sealed_prefix_starts_at_zero_and_cannot_skip(self) -> None:
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
        self.assertEqual(next_seed(pristine), (0, 0))
        material = copy.deepcopy(pristine)
        material.update(
            attempt_state="MATERIAL",
            samples=[
                {
                    "selection_index": 0,
                    "seed": {"flag": "--sched-seed", "value": 0},
                }
            ],
            current_outcome={
                "kind": "NOT-DETERMINABLE",
                "measurement_budget_exhausted": False,
            },
        )
        self.assertEqual(next_seed(material), (1, 1))
        material["samples"][0]["seed"]["value"] = 1
        with self.assertRaisesRegex(Refusal, "exact sealed prefix"):
            next_seed(material)

    def test_argv_is_literal_and_only_replaces_two_placeholders(self) -> None:
        locator = evidence_report_locator(0, 0)
        argv = materialize_argv(self.record, 0, locator)
        self.assertEqual(argv[0], "bin/safehermit")
        self.assertEqual(argv[argv.index("--sched-seed") + 1], "0")
        self.assertEqual(argv[argv.index("--fuzz-seed") + 1], "0")
        self.assertEqual(argv[argv.index("--sh-report") + 1], locator.as_posix())
        self.assertNotIn("{SWEEP}", argv)
        self.assertNotIn("{SAFEHERMIT_REPORT}", argv)
        self.assertIn("--epoch=2026-09-21T22:13:13Z", argv)
        self.assertIn("--chaos-per-thread-slowdown", argv)
        self.assertIn("--chaos-slowdown-max-factor=10.0", argv)
        self.assertIn("--chaos-epoch-length-ns=0", argv)
        self.assertEqual(argv.count("--target-timeslice"), 1)
        self.assertEqual(argv[argv.index("--target-timeslice") + 1], "10000000")
        self.assertIn("--clock-multiplier=1.0", argv)
        self.assertNotIn("--chaos-target-races", argv)
        self.assertIn("--gtest_also_run_disabled_tests", argv)
        self.assertNotIn("--clock-multiplier=0.125", argv)

    def test_helper_receipt_rejects_execution_head_drift(self) -> None:
        helpers = [
            {"path": "a", "blob": "1" * 40},
            {"path": "b", "blob": "2" * 40},
        ]
        receipt = {"commit": "3" * 40, "helper_blobs": helpers}
        with mock.patch(
            "run_asyncssl_sched_slowdown_seed.helper_blobs_at",
            side_effect=[helpers, helpers],
        ):
            assert_live_helper_blobs(receipt)
        drifted = [helpers[0], {"path": "b", "blob": "4" * 40}]
        with mock.patch(
            "run_asyncssl_sched_slowdown_seed.helper_blobs_at",
            side_effect=[helpers, drifted],
        ):
            with self.assertRaisesRegex(Refusal, "drifted"):
                assert_live_helper_blobs(receipt)

    def test_stdout_cap_is_live_and_constant_space(self) -> None:
        class Process:
            def poll(self) -> None:
                return None

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stdout = root / "stdout.txt"
            report = root / "report.txt"
            report.write_text("safehermit: unit=safehermit-test\n")
            completed = subprocess.CompletedProcess([], 0)
            with mock.patch(
                "run_asyncssl_sched_slowdown_seed.subprocess.run", return_value=completed
            ):
                cap = capture_stdout(
                    io.BytesIO(b"x" * 9),
                    stdout,
                    report,
                    Process(),  # type: ignore[arg-type]
                    limit=8,
                )
            captured = stdout.read_bytes()
        self.assertEqual(captured, b"x" * 8)
        self.assertEqual(cap["limit_bytes"], 8)
        self.assertEqual(cap["observed_bytes"], 9)
        self.assertEqual(cap["written_bytes"], 8)
        self.assertTrue(cap["truncated"])
        self.assertEqual(cap["kill"]["returncode"], 0)

    def test_stdout_cap_evidence_is_authenticated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            stdout = Path(temporary) / "stdout.txt"
            stdout.write_bytes(b"pass\n")
            manifest = {
                "stdout_cap_policy": {
                    "method": STDOUT_CAP_METHOD,
                    "limit_bytes": STDOUT_LIMIT,
                    "chunk_bytes": STDOUT_CHUNK,
                    "overflow_action": "systemctl --user kill --signal=SIGKILL UNIT",
                },
                "stdout_cap": {
                    "method": STDOUT_CAP_METHOD,
                    "limit_bytes": STDOUT_LIMIT,
                    "chunk_bytes": STDOUT_CHUNK,
                    "observed_bytes": 5,
                    "written_bytes": 5,
                    "truncated": False,
                    "kill": {
                        "unit": None,
                        "command": None,
                        "returncode": None,
                        "error": None,
                    },
                },
            }
            self.assertFalse(authenticate_stdout_cap(manifest, stdout))
            manifest["stdout_cap"]["observed_bytes"] = 6
            with self.assertRaises(Refusal):
                authenticate_stdout_cap(manifest, stdout)

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

    def test_cell_keeps_strict_target_races_configuration(self) -> None:
        # The sealed production cwd names the designated campaign slot; make
        # only this pure cell-construction unit portable to an exact-tree
        # validation worktree.
        with mock.patch.object(admit, "CAMPAIGN_CWD", str(admit.ROOT)):
            cell = build_cell(self.record)
        self.assertEqual(cell["hermit"]["swept_coordinate"], "--sched-seed")
        self.assertEqual(cell["hermit"]["relaxations"], [])
        self.assertTrue(cell["hermit"]["strict"])
        self.assertEqual(cell["timing"]["effective_max_timeslice"], 200_000_000)
        self.assertEqual(cell["resource_bounds"], self.record["plan"]["resource_bounds"])

    def test_prepared_boundary_allows_only_actual_report_dirty_path(self) -> None:
        report_path = REPO_ROOT / evidence_report_locator(0, 0)
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
            run_dir = scratch / "selection-000-sched-seed-00000000000000000000"
            run_dir.mkdir(parents=True)
            report_locator = evidence_report_locator(0, 0)
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
                "seed": {"flag": "--sched-seed", "value": 0},
                "cwd": str(campaign),
                "host_environment": {
                    "SAFEHERMIT_LOG_ROOT": str(admit.SAFEHERMIT_LOG_ROOT),
                    "RUST_LOG": None,
                    "HERMIT_BIN": None,
                },
                "paths": {key: str(path) for key, path in paths.items()},
                "argv": materialize_argv(self.record, 0, report_locator),
                "stdout_cap": {
                    "method": STDOUT_CAP_METHOD,
                    "limit_bytes": STDOUT_LIMIT,
                    "chunk_bytes": STDOUT_CHUNK,
                    "observed_bytes": len("fixture\n"),
                    "written_bytes": len("fixture\n"),
                    "truncated": False,
                    "kill": {
                        "unit": None,
                        "command": None,
                        "returncode": None,
                        "error": None,
                    },
                },
                "stdout_cap_policy": {
                    "method": STDOUT_CAP_METHOD,
                    "limit_bytes": STDOUT_LIMIT,
                    "chunk_bytes": STDOUT_CHUNK,
                    "overflow_action": "systemctl --user kill --signal=SIGKILL UNIT",
                },
            }
            (run_dir / "invocation.json").write_text(json.dumps(manifest))
            with mock.patch.object(admit, "SCRATCH_ROOT", str(scratch)), mock.patch.object(
                admit, "ROOT", campaign
            ):
                observed, observed_paths = validate_run_dir(run_dir, self.record, 0, 0)
                self.assertEqual(observed, manifest)
                self.assertEqual(observed_paths["safehermit_report"], report)

                wrong = run_dir / "safehermit-report.txt"
                wrong.write_text("wrong\n")
                manifest["paths"]["safehermit_report"] = str(wrong)
                (run_dir / "invocation.json").write_text(json.dumps(manifest))
                with self.assertRaisesRegex(Refusal, "paths drifted"):
                    validate_run_dir(run_dir, self.record, 0, 0)


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
                    STRICT.encode(),
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
