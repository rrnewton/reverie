#!/usr/bin/env python3
"""Unit tests for prospective target classifiers; no Hermit runs occur here."""

from __future__ import annotations

import copy
import json
import signal
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from admit_observed_target import (
    CONFIGS,
    HERMIT_BINARY,
    HERMIT_SHA256,
    SAFEHERMIT_LOG_ROOT,
    assert_formal_wrapper_boundary,
    assert_first_thrift_admission,
    assert_plan_receipt_preserved,
    assert_prior_prefix_unchanged,
    build_cell,
    classify,
    copy_once_or_verify,
    formal_wrapper_boundary_applied,
    measurement_budget_exhausted,
    preserved_non_hermit_event_prefix,
    supersedes_id,
    thrift_prior_prefix,
    write_json_once_or_verify,
)


CFG = CONFIGS["thrift"]
DISPLAY = CFG["display"]
STRICT = (
    ":: comparison=BitwiseInfoV1 relaxations=none\n"
    ":: Success: deterministic. Determinism verified.\n"
)
START = (
    "Note: Google Test filter = " + DISPLAY + "\n"
    "[==========] Running 1 test from 1 test suite.\n"
    "[----------] Global test environment set-up.\n"
    "[----------] 1 test from RequestInstrumentationTestWithFinishedDebugPayload\n"
    f"[ RUN      ] {DISPLAY}\n"
)
PASS = (
    START
    + f"[       OK ] {DISPLAY} (128 ms)\n"
    + "[----------] 1 test from RequestInstrumentationTestWithFinishedDebugPayload (128 ms total)\n\n"
    + "[----------] Global test environment tear-down\n"
    + "[==========] 1 test from 1 test suite ran. (129 ms total)\n"
    + "[  PASSED  ] 1 test.\n"
)
SNAPSHOT_FAIL = (
    START
    + "fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp:836: Failure\n"
    + "Expected equality of these values:\n"
    + "  reqSnapshots.size()\n"
    + "    Which is: 2\n"
    + "  3\n"
)
TSAN_ONLY = (
    "WARNING: ThreadSanitizer: data race\n"
    "SUMMARY: ThreadSanitizer: data race fbsource/folly/synchronization/Baton.h:206\n"
)


def fields(exit_code: int, unit_result: str, process_signal: int | None = None) -> dict[str, str]:
    result = {
        "binary": HERMIT_BINARY,
        "binary_sha256": HERMIT_SHA256,
        "log_dir": f"{SAFEHERMIT_LOG_ROOT}/brs-safehermit-unit-test",
        "bound.wall": "APPLIED:120s",
        "bound.cgroup": "APPLIED:MemoryMax=16G MemorySwapMax=0",
        "bound.disk": (
            "APPLIED:8G for log_dir only; guest paths and explicit log files "
            "elsewhere are outside this bound"
        ),
        "bound.bytes": (
            "APPLIED:67108864 (LETHAL: the run is cgroup-killed at the cap)"
        ),
        "truncated": "false",
        "exit_code": str(exit_code),
        "unit_result": unit_result,
    }
    if process_signal is not None:
        result["process_signal"] = str(process_signal)
    return result


class ThriftInstrumentationClassifierTest(unittest.TestCase):
    def test_exact_formal_wrapper_boundary_is_required(self) -> None:
        applied = fields(0, "success")
        self.assertTrue(formal_wrapper_boundary_applied(applied))
        assert_formal_wrapper_boundary(applied)

        mutations = {
            "binary_sha256": "0" * 64,
            "log_dir": "/home/newton/work/dev-hermit/ignored/safehermit/brs-unbounded",
            "bound.wall": "NOT_APPLIED",
            "bound.cgroup": "NOT_APPLIED",
            "bound.disk": "NOT_APPLIED:space exists but is not capped",
            "bound.bytes": "NOT_APPLIED",
            "truncated": "true",
        }
        for key, value in mutations.items():
            with self.subTest(key=key):
                refused = fields(0, "success")
                refused[key] = value
                self.assertFalse(formal_wrapper_boundary_applied(refused))
                with self.assertRaisesRegex(
                    SystemExit, "formal wrapper boundary not applied exactly"
                ):
                    assert_formal_wrapper_boundary(refused)
                self.assertEqual(
                    classify(CFG, PASS, STRICT, refused),
                    ("CENSORED", None, "ERROR", None, 1),
                )

    def test_exact_pass(self) -> None:
        self.assertEqual(
            classify(CFG, PASS, STRICT, fields(0, "success")),
            ("VALID", "PASS", "COMPLETE", None, 1),
        )

    def test_exact_snapshot_failure_wins_over_teardown_noise(self) -> None:
        self.assertEqual(
            classify(
                CFG,
                SNAPSHOT_FAIL,
                TSAN_ONLY + STRICT,
                fields(134, "core-dump", signal.SIGABRT),
            ),
            ("VALID", "FAIL", "COMPLETE", "gtest-snapshot-size-2", 1),
        )

    def test_sanitizer_only_is_censored(self) -> None:
        status = classify(
            CFG,
            START,
            TSAN_ONLY + STRICT,
            fields(134, "core-dump", signal.SIGABRT),
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_wrong_snapshot_value_is_censored(self) -> None:
        wrong = SNAPSHOT_FAIL.replace("Which is: 2", "Which is: 1")
        status = classify(
            CFG, wrong, STRICT, fields(134, "core-dump", signal.SIGABRT)
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_wrong_expected_value_is_censored(self) -> None:
        wrong = SNAPSHOT_FAIL.replace("  3\n", "  30\n")
        status = classify(
            CFG, wrong, STRICT, fields(134, "core-dump", signal.SIGABRT)
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_snapshot_before_selected_run_is_censored(self) -> None:
        assertion = SNAPSHOT_FAIL[len(START) :]
        reordered = assertion + START
        status = classify(
            CFG, reordered, STRICT, fields(134, "core-dump", signal.SIGABRT)
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_extra_unrelated_run_is_censored(self) -> None:
        extra = SNAPSHOT_FAIL + "[ RUN      ] UnrelatedSuite.UnrelatedCase\n"
        status = classify(
            CFG, extra, STRICT, fields(134, "core-dump", signal.SIGABRT)
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_pass_lines_before_selected_run_are_censored(self) -> None:
        premature = (
            f"[       OK ] {DISPLAY} (128 ms)\n"
            "[  PASSED  ] 1 test.\n"
            + START
        )
        status = classify(CFG, premature, STRICT, fields(0, "success"))
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_noncontiguous_snapshot_block_is_censored(self) -> None:
        split = SNAPSHOT_FAIL.replace(
            "  reqSnapshots.size()\n", "  unrelated diagnostic\n  reqSnapshots.size()\n"
        )
        status = classify(
            CFG, split, STRICT, fields(134, "core-dump", signal.SIGABRT)
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_duplicate_failure_heading_is_censored(self) -> None:
        duplicate = (
            SNAPSHOT_FAIL
            + "fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp:836: Failure\n"
        )
        status = classify(
            CFG, duplicate, STRICT, fields(134, "core-dump", signal.SIGABRT)
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_timeout_with_partial_signature_is_censored(self) -> None:
        status = classify(CFG, SNAPSHOT_FAIL, STRICT, fields(124, "timeout"))
        self.assertEqual(status, ("CENSORED", None, "TIMEOUT", None, 1))

    def test_verifier_success_without_strict_comparison_is_censored(self) -> None:
        status = classify(
            CFG,
            PASS,
            ":: Success: deterministic. Determinism verified.\n",
            fields(0, "success"),
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_unrelated_nonzero_is_censored(self) -> None:
        status = classify(
            CFG,
            START,
            STRICT,
            fields(1, "exit-code"),
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_internal_failure_cannot_be_pass(self) -> None:
        status = classify(
            CFG,
            PASS,
            STRICT + "HERMIT_INTERNAL_FAILURE class=container-child-exit\n",
            fields(0, "success"),
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_internal_failure_cannot_be_target_fail(self) -> None:
        status = classify(
            CFG,
            SNAPSHOT_FAIL,
            STRICT + "Sandbox container exited unexpectedly\n",
            fields(125, "exit-code"),
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))

    def test_truncated_output_is_censored(self) -> None:
        status = classify(
            CFG,
            PASS,
            STRICT,
            {"exit_code": "0", "unit_result": "success", "truncated": "true"},
        )
        self.assertEqual(status, ("CENSORED", None, "ERROR", None, 1))


class PrefixIntegrityTest(unittest.TestCase):
    def setUp(self) -> None:
        self.cell = {"cell_id": "cell"}
        self.event = {"event_id": "event-0"}
        self.sample = {
            "sample_id": "sample-0",
            "seed": {"flag": "--fuzz-seed", "value": 0},
            "status": "VALID",
        }
        self.matcher = {"matcher_id": "matcher-0"}
        self.record = {
            "plan_receipt": {"commit": "plan", "path": "record", "blob": "blob"},
            "current_outcome": {
                "classification_id": "pending-0",
                "kind": "NOT-DETERMINABLE",
                "measurement_budget_exhausted": False,
            },
            "cells": [copy.deepcopy(self.cell)],
            "events": [copy.deepcopy(self.event)],
            "samples": [copy.deepcopy(self.sample)],
            "matchers": [copy.deepcopy(self.matcher)],
        }

    def test_exact_one_seed_extension_preserves_prior_prefix(self) -> None:
        self.assertEqual(thrift_prior_prefix(self.record, [0, 1], "plan"), [0])
        assert_prior_prefix_unchanged(
            self.record,
            self.cell,
            [self.event, {"event_id": "event-1"}],
            [self.sample, {"sample_id": "sample-1"}],
            [self.matcher, {"matcher_id": "matcher-1"}],
        )
        self.assertEqual(supersedes_id(self.record, True), "pending-0")

    def test_pre_cell_launch_is_preserved_before_rebuilt_seed_events(self) -> None:
        launch = {"event_id": "rejected-boundary", "kind": "LAUNCH"}
        hermit = {"event_id": "event-0", "kind": "HERMIT_EXECUTION"}
        self.record["events"] = [launch, hermit]
        self.assertEqual(
            preserved_non_hermit_event_prefix(self.record, True), [launch]
        )
        rebuilt = [launch, hermit, {"event_id": "event-1", "kind": "HERMIT_EXECUTION"}]
        assert_prior_prefix_unchanged(
            self.record,
            self.cell,
            rebuilt,
            [self.sample, {"sample_id": "sample-1"}],
            [self.matcher, {"matcher_id": "matcher-1"}],
        )

    def test_interleaved_non_hermit_event_is_refused(self) -> None:
        self.record["events"] = [
            {"event_id": "event-0", "kind": "HERMIT_EXECUTION"},
            {"event_id": "late-launch", "kind": "LAUNCH"},
        ]
        with self.assertRaisesRegex(SystemExit, "non-Hermit event appears after"):
            preserved_non_hermit_event_prefix(self.record, True)

    def test_pristine_record_has_no_preserved_material_event_prefix(self) -> None:
        self.assertEqual(preserved_non_hermit_event_prefix(self.record, False), [])

    def test_prior_object_drift_is_refused(self) -> None:
        changed = copy.deepcopy(self.event)
        changed["event_id"] = "retrospectively-changed"
        with self.assertRaisesRegex(SystemExit, "prior execution events changed"):
            assert_prior_prefix_unchanged(
                self.record,
                self.cell,
                [changed, {"event_id": "event-1"}],
                [self.sample, {"sample_id": "sample-1"}],
                [self.matcher, {"matcher_id": "matcher-1"}],
            )

    def test_receipt_change_is_refused(self) -> None:
        with self.assertRaisesRegex(SystemExit, "plan receipt commit changed"):
            thrift_prior_prefix(self.record, [0, 1], "different-plan")
        changed = copy.deepcopy(self.record["plan_receipt"])
        changed["blob"] = "different-blob"
        with self.assertRaisesRegex(SystemExit, "exact plan receipt changed"):
            assert_plan_receipt_preserved(self.record, changed, True)

    def test_first_admission_must_be_seed_zero_only(self) -> None:
        assert_first_thrift_admission(False, [0])
        with self.assertRaisesRegex(SystemExit, "only sealed seed 0"):
            assert_first_thrift_admission(False, [0, 1])
        assert_first_thrift_admission(True, [0, 1])

    def test_skipped_seed_prefix_is_refused(self) -> None:
        with self.assertRaisesRegex(SystemExit, "expected prior admitted prefix"):
            thrift_prior_prefix(self.record, [0, 1, 2], "plan")

    def test_terminal_exhaustion_is_refused(self) -> None:
        self.record["current_outcome"]["measurement_budget_exhausted"] = True
        with self.assertRaisesRegex(SystemExit, "terminal exhausted"):
            thrift_prior_prefix(self.record, [0, 1], "plan")

    def test_working_bracket_is_refused(self) -> None:
        self.record["current_outcome"]["kind"] = "WORKING"
        with self.assertRaisesRegex(SystemExit, "not NOT-DETERMINABLE"):
            thrift_prior_prefix(self.record, [0, 1], "plan")

    def test_blocked_outcome_is_refused(self) -> None:
        self.record["current_outcome"] = {
            "classification_id": "blocked-0",
            "kind": "BLOCKED-WITH-NAMED-CAUSE",
        }
        with self.assertRaisesRegex(SystemExit, "not NOT-DETERMINABLE"):
            thrift_prior_prefix(self.record, [0, 1], "plan")

    def test_exhaustion_at_29_valid_or_36_attempted(self) -> None:
        design = {
            "required_valid_seed_tuples": 29,
            "max_attempted_seed_tuples": 36,
        }
        valid_28 = [{"status": "VALID"} for _ in range(28)]
        self.assertFalse(measurement_budget_exhausted(design, valid_28))
        self.assertTrue(
            measurement_budget_exhausted(
                design, valid_28 + [{"status": "VALID"}]
            )
        )
        self.assertTrue(
            measurement_budget_exhausted(
                design, valid_28 + [{"status": "CENSORED"} for _ in range(8)]
            )
        )

    def test_prior_artifacts_are_verified_never_overwritten(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source.txt"
            destination = root / "destination.txt"
            source.write_text("same\n")
            copy_once_or_verify(source, destination, False)
            copy_once_or_verify(source, destination, True)
            source.write_text("changed\n")
            with self.assertRaisesRegex(SystemExit, "prior replica artifact changed"):
                copy_once_or_verify(source, destination, True)
            self.assertEqual(destination.read_text(), "same\n")

            result = root / "result.json"
            write_json_once_or_verify(result, {"value": 1}, False)
            write_json_once_or_verify(result, {"value": 1}, True)
            with self.assertRaisesRegex(SystemExit, "prior JSON artifact changed"):
                write_json_once_or_verify(result, {"value": 2}, True)
            self.assertEqual(result.read_text(), '{"value":1}\n')


class FrozenCellTest(unittest.TestCase):
    def test_thrift_cell_is_exactly_the_prospectively_instantiated_cell(self) -> None:
        record_path = (
            Path(__file__).resolve().parents[1]
            / CONFIGS["thrift"]["record"]
        )
        record = json.loads(record_path.read_text())
        self.assertEqual(
            record["cells"], [build_cell(record, CONFIGS["thrift"], "thrift")]
        )


if __name__ == "__main__":
    unittest.main()
