#!/usr/bin/env python3

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest import mock


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
                    "run_number": 701,
                }
            )
        )
        self.assertEqual(run_registry.AdmissionState.ADMITTED, handle.admission_result.state)
        self.assertEqual(701, handle.admission_result.run_number)

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

    def test_validation_service_result_schema_is_checked_when_present(self) -> None:
        historical = run_registry.parse_current_record(
            validation_handle(service_result_schema=1)
        )
        writeback = run_registry.parse_current_record(
            validation_handle(service_result_schema=2)
        )
        current = run_registry.parse_current_record(
            validation_handle(service_result_schema=3)
        )
        self.assertEqual(1, historical.raw["service_result_schema"])
        self.assertEqual(2, writeback.raw["service_result_schema"])
        self.assertEqual(3, current.raw["service_result_schema"])

        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-service_result_schema: expected 1, 2, or 3, got 4",
        ):
            run_registry.parse_current_record(
                validation_handle(service_result_schema=4)
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

    def test_passed_validation_with_failed_writeback_keeps_both_facts(self) -> None:
        handle = run_registry.parse_current_record(
            validation_handle(
                state="completed",
                result="success",
                exit_code=75,
                finished_at="2026-08-28T07:01:00Z",
                final_validate_status="PASSED",
                service_result_schema=3,
                selection_mode="full",
                scorecard_writeback={
                    "status": "failed",
                    "error": "fixture refusal",
                },
            )
        )
        self.assertEqual("PASSED", handle.raw["final_validate_status"])
        self.assertEqual(75, handle.raw["exit_code"])
        self.assertEqual("failed", handle.raw["scorecard_writeback"]["status"])

    def test_current_completed_result_requires_a_named_writeback_value(self) -> None:
        base = dict(
            state="completed",
            result="success",
            exit_code=0,
            finished_at="2026-08-28T07:01:00Z",
            final_validate_status="PASSED",
            service_result_schema=3,
            selection_mode="full",
        )
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-scorecard_writeback: is required",
        ):
            run_registry.parse_current_record(validation_handle(**base))
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-run-handle-scorecard_writeback: expected null",
        ):
            run_registry.parse_current_record(
                validation_handle(
                    **base,
                    scorecard_writeback={"status": "invented"},
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

    def test_pending_owner_watch_correction_makes_run_handles_unavailable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            transaction = run_registry.owner_watch_correction_transaction(root)
            transaction.mkdir()

            with self.assertRaisesRegex(RuntimeError, "recovery is pending"):
                run_registry.read_record(path)

    def test_symlinked_owner_watch_correction_marker_is_also_pending(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            outside = root / "outside"
            outside.mkdir()
            transaction = run_registry.owner_watch_correction_transaction(root)
            transaction.symlink_to(outside, target_is_directory=True)

            with self.assertRaisesRegex(RuntimeError, "recovery is pending"):
                run_registry.read_record(path)

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
            root = Path(temporary)
            path = root / "ignored/validate/runs/validate-test.json"
            run_registry.write_record(path, validation_handle(state="launching"))
            with (
                mock.patch.object(
                    run_registry.socket,
                    "gethostname",
                    return_value="hosta.example.com",
                ),
                mock.patch.object(
                    run_registry,
                    "_ledger_run_numbers",
                    return_value=(700, 0),
                ),
            ):
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

    def test_run_number_is_assigned_while_record_is_launching_and_never_reused(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runs = root / "ignored/validate/runs"
            first = runs / "validate-first.json"
            second = runs / "validate-second.json"
            run_registry.create_current_record(
                first, validation_handle(state="launching", unit="validate-first.service")
            )
            run_registry.create_current_record(
                second,
                validation_handle(state="launching", unit="validate-second.service"),
            )
            with (
                mock.patch.object(
                    run_registry.socket,
                    "gethostname",
                    return_value="hosta.example.com",
                ),
                mock.patch.object(
                    run_registry,
                    "_ledger_run_numbers",
                    return_value=(700, 0),
                ) as ledger_numbers,
            ):
                assigned = run_registry.publish_admission_result(
                    first, state=run_registry.AdmissionState.ADMITTED
                )
                repeated = run_registry.publish_admission_result(
                    first, state=run_registry.AdmissionState.ADMITTED
                )
                next_assigned = run_registry.publish_admission_result(
                    second, state=run_registry.AdmissionState.ADMITTED
                )

            self.assertEqual("launching", assigned["state"])
            self.assertEqual(701, assigned["admission_result"]["run_number"])
            self.assertEqual(701, repeated["admission_result"]["run_number"])
            self.assertEqual(702, next_assigned["admission_result"]["run_number"])
            ledger_numbers.assert_called_once_with(
                root, team="hermit", host="hosta"
            )
            self.assertEqual(
                701,
                run_registry.read_record(first)["admission_result"]["run_number"],
            )
            projected = run_registry.projection(assigned)
            self.assertNotIn("run_number", projected)
            self.assertEqual(701, projected["admission_result"]["run_number"])

    def test_bench_admission_does_not_consume_a_validation_run_number(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "hermit-pressure-test.json"
            run_registry.write_record(path, bench_handle())

            admitted = run_registry.publish_admission_result(
                path, state=run_registry.AdmissionState.ADMITTED
            )

            self.assertEqual(
                {"state", "recorded_at"}, set(admitted["admission_result"])
            )

    def test_concurrent_admissions_receive_distinct_run_numbers(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runs = root / "ignored/validate/runs"
            paths = [runs / "validate-first.json", runs / "validate-second.json"]
            for index, path in enumerate(paths, 1):
                run_registry.create_current_record(
                    path,
                    validation_handle(
                        state="launching", unit=f"validate-{index}.service"
                    ),
                )
            with (
                mock.patch.object(
                    run_registry.socket,
                    "gethostname",
                    return_value="hosta.example.com",
                ),
                mock.patch.object(
                    run_registry,
                    "_ledger_run_numbers",
                    return_value=(700, 0),
                ) as ledger_numbers,
                ThreadPoolExecutor(max_workers=2) as executor,
            ):
                admitted = list(
                    executor.map(
                        lambda path: run_registry.publish_admission_result(
                            path, state=run_registry.AdmissionState.ADMITTED
                        ),
                        paths,
                    )
                )

            self.assertEqual(
                [701, 702],
                sorted(value["admission_result"]["run_number"] for value in admitted),
            )
            ledger_numbers.assert_called_once_with(
                root, team="hermit", host="hosta"
            )

    def test_admitted_result_accepts_an_absent_run_number_but_checks_one_present(self) -> None:
        handle = run_registry.parse_current_record(
            validation_handle(
                admission_result={
                    "state": "admitted",
                    "recorded_at": "2026-08-28T07:00:01+00:00",
                }
            )
        )
        self.assertIsNotNone(handle.admission_result)
        self.assertIsNone(handle.admission_result.run_number)
        projected = run_registry.projection(handle.raw)
        self.assertNotIn("run_number", projected["admission_result"])

        for value in (0, -1, True, "1"):
            with self.subTest(value=value):
                with self.assertRaisesRegex(
                    RuntimeError, "validation-run-handle-admission_result.run_number"
                ):
                    run_registry.parse_current_record(
                        validation_handle(
                            admission_result={
                                "state": "admitted",
                                "recorded_at": "2026-08-28T07:00:01+00:00",
                                "run_number": value,
                            }
                        )
                    )

    def test_republishing_admitted_result_does_not_backfill_missing_run_number(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = (
                Path(temporary)
                / "ignored/validate/runs/validate-historical.json"
            )
            run_registry.create_current_record(
                path,
                validation_handle(
                    state="launching",
                    unit="validate-historical.service",
                    admission_result={
                        "state": "admitted",
                        "recorded_at": "2026-08-28T07:00:01+00:00",
                    },
                ),
            )

            repeated = run_registry.publish_admission_result(
                path, state=run_registry.AdmissionState.ADMITTED
            )

            self.assertEqual(
                {
                    "state": "admitted",
                    "recorded_at": "2026-08-28T07:00:01+00:00",
                },
                repeated["admission_result"],
            )

    def test_run_number_baseline_survives_an_unreadable_old_ledger_line(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shard = root / "ledger/hermit/hosta/2026-08.jsonl"
            shard.parent.mkdir(parents=True)
            events = [
                {
                    "event_type": "run.result",
                    "event_id": "first-result",
                    "run_id": "first-run",
                    "team": "hermit",
                    "host": "hosta",
                    "legacy_row": {},
                },
                {
                    "event_type": "run.correct",
                    "event_id": "first-correction",
                    "run_id": "first-run",
                    "team": "hermit",
                    "host": "hosta",
                },
                {
                    "event_type": "run.result",
                    "event_id": "duplicate-result",
                    "run_id": "first-run",
                    "team": "hermit",
                    "host": "hosta",
                    "legacy_row": {},
                },
                {
                    "event_type": "run.result",
                    "event_id": "second-result",
                    "run_id": "second-run",
                    "team": "hermit",
                    "host": "hosta.example.com",
                    "run_number": 9,
                    "legacy_row": {},
                },
                {
                    "event_type": "run.result",
                    "event_id": "fixture-result",
                    "run_id": "fixture-run",
                    "team": "hermit",
                    "host": "hosta",
                    "producer": {"tool": "ledger-live-sync-acceptance"},
                    "legacy_row": {},
                },
                {
                    "event_type": "run.result",
                    "event_id": "wrong-host-result",
                    "run_id": "wrong-host-run",
                    "team": "hermit",
                    "host": "wrong-host",
                    "legacy_row": {},
                },
            ]
            shard.write_text(
                "\n".join(json.dumps(event) for event in events[:2])
                + "\nnot-json\n"
                + "\n".join(json.dumps(event) for event in events[2:])
                + "\n"
            )

            self.assertEqual(
                (4, 9),
                run_registry._ledger_run_numbers_without_canonical_view(
                    root, team="hermit", host="hosta"
                ),
            )


if __name__ == "__main__":
    unittest.main()
