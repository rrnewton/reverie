#!/usr/bin/env python3

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "ci-hub" / "validate"))

import run_registry  # noqa: E402


SHA = "a" * 40


def validation_handle(**updates: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": run_registry.SCHEMA_VERSION,
        "kind": "validate",
        "state": "preparing",
        "unit": "validate-test.service",
        "target": SHA,
        "repo": "rrnewton/hermit",
        "checkout": "/tmp/checkout",
        "source_checkout": "/tmp/source",
        "temporary_checkout": True,
        "cargo_home": "/tmp/cargo",
        "log": "/tmp/validate.log",
        "agent": "type-audit",
        "pr": None,
        "started_at": "2026-08-28T07:00:00+00:00",
        "parent_checkout_head": SHA,
        "producer": run_registry.PRODUCER,
        "admission": "ci-hub validate-lock",
        "pane_role": "observer-only",
        "validate_lock_child_deadline_seconds": 3600,
        "e2e_result_root": "/tmp/e2e",
        "safe_ci_dag_runner_log_dir": "/tmp/dag",
        "hermit_run_timeout_seconds": 3300,
    }
    value.update(updates)
    return value


def bench_handle(**updates: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": run_registry.SCHEMA_VERSION,
        "kind": "bench",
        "state": "launching",
        "unit": "hermit-pressure-test.service",
        "target": SHA,
        "repo": "rrnewton/hermit",
        "checkout": "/tmp/pressure",
        "log": "/tmp/pressure.log",
        "agent": "tick-pressure-test",
        "started_at": "2026-08-28T07:00:00Z",
        "producer": run_registry.PRODUCER,
        "admission": "ci-hub validate-lock",
    }
    value.update(updates)
    return value


class RunRegistryContractTests(unittest.TestCase):
    def test_validation_handle_has_typed_kind_state_and_projection(self) -> None:
        handle = run_registry.parse_current_record(validation_handle())
        self.assertEqual(1, handle.raw["schema_version"])
        self.assertEqual(run_registry.RunKind.VALIDATE, handle.kind)
        self.assertEqual(run_registry.RunState.PREPARING, handle.state)
        self.assertFalse(handle.lock_admissible)
        self.assertTrue(handle.counts_as_validation)
        self.assertIsNone(handle.admission_result)

    def test_admission_result_is_closed_and_exact(self) -> None:
        handle = run_registry.parse_current_record(
            validation_handle(
                admission_result={
                    "state": "admitted",
                    "recorded_at": "2026-08-28T07:00:01+00:00",
                }
            )
        )
        self.assertEqual(run_registry.AdmissionState.ADMITTED, handle.admission_result.state)

        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-admission_result.state: unsupported value 'waiting'",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    admission_result={
                        "state": "waiting",
                        "recorded_at": "2026-08-28T07:00:01+00:00",
                    }
                )
            )
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-admission_result.recorded_at: must include a UTC offset",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    admission_result={
                        "state": "admitted",
                        "recorded_at": "2026-08-28T07:00:01",
                    }
                )
            )

    def test_refused_admission_requires_reason_and_exit_by_name(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-admission_result: refused must contain exactly",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    admission_result={
                        "state": "refused",
                        "recorded_at": "2026-08-28T07:00:01+00:00",
                    }
                )
            )

    def test_bench_failed_state_is_distinct_and_complete(self) -> None:
        handle = run_registry.parse_current_record(
            bench_handle(
                state="failed",
                result="failed",
                exit_code=1,
                finished_at="2026-08-28T07:01:00Z",
            )
        )
        self.assertEqual(run_registry.RunKind.BENCH, handle.kind)
        self.assertEqual(run_registry.RunState.FAILED, handle.state)
        self.assertFalse(handle.counts_as_validation)

    def test_validation_cannot_silently_adopt_bench_failed_state(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-state: 'failed' is not valid for kind 'validate'"
        ):
            run_registry.parse_current_record(validation_handle(state="failed"))

    def test_new_state_fails_by_name(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-state: unsupported value 'cancelled'"
        ):
            run_registry.parse_current_record(validation_handle(state="cancelled"))

    def test_missing_kind_fails_by_name(self) -> None:
        value = validation_handle()
        del value["kind"]
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-kind: must be a nonempty string"
        ):
            run_registry.parse_current_record(value)

    def test_unknown_field_fails_by_name(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-field: unknown field.*invented"
        ):
            run_registry.parse_current_record(validation_handle(invented=1))

    def test_completed_status_must_agree_with_result_and_exit(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "validation-run-handle-exit_code"):
            run_registry.parse_current_record(
                validation_handle(
                    state="completed",
                    result="success",
                    exit_code=1,
                    finished_at="2026-08-28T07:01:00Z",
                    final_validate_status="PASSED",
                )
            )

    def test_required_validation_path_fails_by_name_when_empty(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-source_checkout"
        ):
            run_registry.parse_current_record(validation_handle(source_checkout=""))

    def test_malformed_process_identity_fails_by_name(self) -> None:
        with self.assertRaisesRegex(
            RuntimeError, "validation-run-handle-process_identity.start_ticks"
        ):
            run_registry.parse_current_record(
                validation_handle(
                    state="running",
                    process_identity={"pid": 1, "start_ticks": 0, "boot_id": "boot"},
                )
            )

    def test_schema_one_remains_readable_without_current_semantics(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "legacy.json"
            legacy = {"schema_version": 1, "state": "cancelled", "old": "field"}
            path.write_text(json.dumps(legacy))
            self.assertEqual(legacy, run_registry.read_record(path))

    def test_historical_schema_one_kind_remains_readable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "manual-bench.json"
            historical = {
                "schema_version": 1,
                "kind": "bench",
                "producer": "systemd-user-v1",
                "state": "completed",
                "purpose": "historical manual benchmark",
            }
            path.write_text(json.dumps(historical))
            self.assertEqual(historical, run_registry.read_record(path))

    def test_multi_record_inspection_returns_current_records(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            current = directory / "validate-test.json"
            run_registry.write_record(current, validation_handle(state="launching"))

            inspected = run_registry.inspect_current_records([current])

            self.assertEqual([str(current)], [row["path"] for row in inspected])
            self.assertEqual("validate", inspected[0]["record"]["kind"])

    def test_process_identity_cannot_be_rebound(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            first = run_registry.ProcessIdentity(1, 2, "boot-a")
            run_registry.bind_process_identity(path, first)
            self.assertEqual(1, run_registry.read_record(path)["process_identity"]["pid"])
            with self.assertRaisesRegex(RuntimeError, "different process identity"):
                run_registry.bind_process_identity(
                    path, run_registry.ProcessIdentity(3, 4, "boot-b")
                )

    def test_admission_result_cannot_be_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            run_registry.publish_admission_result(
                path, state=run_registry.AdmissionState.ADMITTED
            )
            self.assertEqual(
                "admitted", run_registry.read_record(path)["admission_result"]["state"]
            )
            with self.assertRaisesRegex(RuntimeError, "different admission result"):
                run_registry.publish_admission_result(
                    path,
                    state=run_registry.AdmissionState.REFUSED,
                    reason=run_registry.AdmissionRefusalReason.VALIDATION_LOCK_REFUSAL,
                    exit_code=3,
                )


if __name__ == "__main__":
    unittest.main()
