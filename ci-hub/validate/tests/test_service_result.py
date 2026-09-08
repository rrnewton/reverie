from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import service_result  # noqa: E402


class ServiceResultTest(unittest.TestCase):
    @staticmethod
    def schema_projection(version: int) -> dict[str, object]:
        fields = {
            service_result.HISTORICAL_SCHEMA_VERSION: service_result.HISTORICAL_FIELD_NAMES,
            service_result.WRITEBACK_SCHEMA_VERSION: service_result.WRITEBACK_FIELD_NAMES,
            service_result.SELECTION_SCHEMA_VERSION: service_result.SELECTION_FIELD_NAMES,
            service_result.TEST_COUNTS_SCHEMA_VERSION: service_result.TEST_COUNTS_FIELD_NAMES,
            service_result.SCHEMA_VERSION: service_result.FIELD_NAMES,
        }[version]
        value: dict[str, object] = {
            "schema_version": version,
            "fields": list(fields),
            "outcomes": (
                service_result.HISTORICAL_EXPECTED_OUTCOMES
                if version == service_result.HISTORICAL_SCHEMA_VERSION
                else service_result.EXPECTED_OUTCOMES
            ),
        }
        if version != service_result.HISTORICAL_SCHEMA_VERSION:
            value["scorecard_writeback"] = service_result.EXPECTED_SCORECARD_WRITEBACK
        return value

    def write_schema(self, root: Path, version: int) -> Path:
        path = root / service_result.SCHEMA_RELATIVE_PATH
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(self.schema_projection(version)))
        return path

    def classify(self, text: str) -> dict[str, object]:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(text)
            return service_result.from_log(log)

    def test_schema_for_checkout_accepts_current_ahead_of_historical_pin(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "target"
            reference = root / "reference"
            self.write_schema(target, service_result.SCHEMA_VERSION)
            self.write_schema(reference, service_result.SELECTION_SCHEMA_VERSION)

            self.assertEqual(
                service_result.SCHEMA_VERSION,
                service_result.schema_for_checkout(
                    target, reference_checkout=reference
                ),
            )

    def test_schema_for_checkout_accepts_exact_historical_versions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            reference = root / "reference"
            self.write_schema(reference, service_result.SELECTION_SCHEMA_VERSION)
            for version in (
                service_result.HISTORICAL_SCHEMA_VERSION,
                service_result.WRITEBACK_SCHEMA_VERSION,
                service_result.SELECTION_SCHEMA_VERSION,
                service_result.TEST_COUNTS_SCHEMA_VERSION,
            ):
                with self.subTest(version=version):
                    target = root / f"target-{version}"
                    self.write_schema(target, version)
                    self.assertEqual(
                        version,
                        service_result.schema_for_checkout(
                            target, reference_checkout=reference
                        ),
                    )

    def test_current_schema_refuses_an_unexpected_top_level_field(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = self.write_schema(Path(temporary), service_result.SCHEMA_VERSION)
            value = json.loads(path.read_text())
            value["future"] = True
            path.write_text(json.dumps(value))

            with self.assertRaisesRegex(
                RuntimeError,
                r"validation-service-result-schema-fields: .*future",
            ):
                service_result.read_schema(path)

    def test_schema_reader_refuses_an_unsupported_version_by_version(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = self.write_schema(Path(temporary), service_result.SCHEMA_VERSION)
            value = json.loads(path.read_text())
            value["schema_version"] = service_result.SCHEMA_VERSION + 1
            path.write_text(json.dumps(value))

            with self.assertRaisesRegex(
                RuntimeError,
                r"validation-service-result-schema_version: expected 1, 2, 3, 4, or 5, got 6",
            ):
                service_result.read_schema(path)

    def test_schema_reader_refuses_bool_and_integral_float_versions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = self.write_schema(Path(temporary), service_result.SCHEMA_VERSION)
            for version in (True, 4.0, 5.0):
                with self.subTest(version=version):
                    value = self.schema_projection(service_result.SCHEMA_VERSION)
                    value["schema_version"] = version
                    path.write_text(json.dumps(value))
                    with self.assertRaisesRegex(
                        RuntimeError,
                        "validation-service-result-schema_version",
                    ):
                        service_result.read_schema(path)

    def test_genuine_pass_requires_status_and_exit_agreement(self) -> None:
        result = self.classify(
            "   862 test(s) executed, 350 filtered\n"
            "   nodes: 55 executed, 0 failed, 0 skipped in 9m12s at -j 16\n"
            "FINAL_VALIDATE_STATUS: PASSED\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=552.0s cpu=1.0s "
            "cpu_user=0.5s cpu_system=0.5s exit=0\n"
        )

        self.assertEqual("completed", result["state"])
        self.assertEqual("success", result["result"])
        self.assertEqual(0, result["exit_code"])
        self.assertEqual(55, result["executed_nodes"])
        self.assertEqual(862, result["executed_tests"])
        self.assertTrue(service_result.is_evidenced_terminal(result))

    def test_sigterm_exit_is_named_killed_and_preserved_as_143(self) -> None:
        result = self.classify(
            "validate-lock: supervisor received signal 15\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=50.197s cpu=0.2s "
            "cpu_user=0.1s cpu_system=0.1s exit=143\n"
        )

        self.assertEqual("killed", result["state"])
        self.assertEqual("terminated", result["result"])
        self.assertEqual(143, result["exit_code"])
        self.assertEqual(15, result["signal"])

    def test_direct_signal_description_is_also_named_killed(self) -> None:
        result = self.classify(
            "# ci-hub/validate-lock tool COST ACTUAL wall=2.0s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=signal:9\n"
        )

        self.assertEqual("killed", result["state"])
        self.assertEqual(137, result["exit_code"])
        self.assertEqual(9, result["signal"])

    def test_exit_zero_without_status_is_unknown(self) -> None:
        result = self.classify(
            "   0 test(s) executed, 0 filtered\n"
            "   nodes: 0 executed, 0 failed, 0 skipped in 0s at -j 16\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=0\n"
        )

        self.assertEqual("unknown", result["state"])
        self.assertEqual("unknown", result["result"])
        self.assertEqual(0, result["exit_code"])
        self.assertEqual(0, result["executed_nodes"])
        self.assertEqual("final validate status is absent", result["detail"])

    def test_collected_units_synthetic_success_is_not_exit_evidence(self) -> None:
        properties = {
            "LoadState": "not-found",
            "ActiveState": "inactive",
            "ExecMainCode": "0",
            "ExecMainStatus": "0",
            "Result": "success",
            "InvocationID": "",
        }

        result = service_result.classify(service_result.RunEvidence(), properties=properties)

        # `unknown`, not `not-run`. This test's whole thesis is that a collected
        # unit's synthetic success IS NOT EVIDENCE -- so the honest answer is
        # that the classifier cannot tell, never a positive claim that the run
        # did not happen.
        self.assertEqual("unknown", result["state"])
        self.assertEqual("unknown", result["result"])
        self.assertIsNone(result["exit_code"])
        self.assertEqual("no-exit-evidence", result["result_source"])

    def test_unknown_is_not_terminal_so_it_is_re_derived_not_frozen(self) -> None:
        """The one state that means "I cannot tell" must never be durable.

        Freezing it is the defect: an ignorant row gets preserved and re-served
        instead of being re-derived once the log or the unit can answer.
        """
        unknown = service_result.classify(service_result.RunEvidence())

        self.assertEqual("unknown", unknown["state"])
        self.assertFalse(service_result.is_evidenced_terminal(unknown))

    def test_absence_is_not_terminal_even_when_exit_is_known(self) -> None:
        result = self.classify(
            "   0 test(s) executed, 0 filtered\n"
            "   nodes: 0 executed, 0 failed, 0 skipped in 0s at -j 16\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=0\n"
        )

        self.assertEqual("unknown", result["state"])
        self.assertFalse(service_result.is_evidenced_terminal(result))

    def test_could_not_run_is_named_and_uses_exit_75(self) -> None:
        result = self.classify(
            "validate refused by: stale base\n"
            "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=75\n"
        )

        self.assertEqual("completed", result["state"])
        self.assertEqual("no-result", result["result"])
        self.assertEqual("COULD_NOT_RUN", result["final_validate_status"])
        self.assertTrue(service_result.is_evidenced_terminal(result))

    def test_earlier_quoted_status_does_not_override_final_status(self) -> None:
        result = self.classify(
            "fixture says FINAL_VALIDATE_STATUS: PASSED in prose\n"
            "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
            "quoted block follows:\n"
            "FINAL_VALIDATE_STATUS: FAILED\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=1\n"
        )

        self.assertEqual("completed", result["state"])
        self.assertEqual("failure", result["result"])
        self.assertEqual("FAILED", result["final_validate_status"])

    def test_status_exit_disagreement_is_unknown(self) -> None:
        result = self.classify(
            "FINAL_VALIDATE_STATUS: PASSED\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=1\n"
        )

        self.assertEqual("unknown", result["state"])
        self.assertEqual("unknown", result["result"])
        self.assertIn("requires exit 0, got 1", result["detail"])

    def test_status_signal_disagreement_is_unknown_not_killed(self) -> None:
        result = self.classify(
            "FINAL_VALIDATE_STATUS: PASSED\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=143\n"
        )

        self.assertEqual("unknown", result["state"])
        self.assertIn("requires exit 0, got 143", result["detail"])

    def test_loaded_systemd_signal_is_fail_closed_fallback(self) -> None:
        properties = {
            "LoadState": "loaded",
            "ActiveState": "failed",
            "ExecMainCode": "killed",
            "ExecMainStatus": "15",
            "Result": "signal",
            "InvocationID": "fixture-invocation",
        }

        result = service_result.classify(service_result.RunEvidence(), properties=properties)

        self.assertEqual("killed", result["state"])
        self.assertEqual(143, result["exit_code"])

    def test_current_run_uses_framework_result_not_conflicting_log(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            log = root / "validate.log"
            log.write_text(
                "   1 test(s) executed, 0 filtered\n"
                "   nodes: 1 executed, 0 failed, 0 skipped in 1s at -j 1\n"
                "FINAL_VALIDATE_STATUS: FAILED\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=1s cpu=1s "
                "cpu_user=1s cpu_system=0s exit=1\n"
            )
            result_path = service_result.result_path(record)
            result_path.write_text(
                json.dumps(
                    {
                        "schema_version": service_result.SCHEMA_VERSION,
                        "commit": "a" * 40,
                        "profile": "full",
                        "selection_mode": "full",
                        "final_validate_status": "PASSED",
                        "detail": None,
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "passed_tests": 2129,
                        "scorecard_writeback": {"status": "completed"},
                    }
                )
            )

            result = service_result.from_run_record(
                record,
                {
                    "target": "a" * 40,
                    "service_result_schema": service_result.SCHEMA_VERSION,
                },
                log,
            )

            self.assertEqual("completed", result["state"])
            self.assertEqual("success", result["result"])
            self.assertEqual(76, result["executed_nodes"])
            self.assertEqual(2129, result["executed_tests"])
            self.assertEqual(2129, result["passed_tests"])
            self.assertEqual("full", result["selection_mode"])
            self.assertEqual("validation-service-result", result["result_source"])

    def test_current_run_missing_result_is_unknown_not_log_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            log = root / "validate.log"
            log.write_text(
                "FINAL_VALIDATE_STATUS: PASSED\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=1s cpu=1s "
                "cpu_user=1s cpu_system=0s exit=0\n"
            )

            result = service_result.from_run_record(
                record,
                {
                    "target": "a" * 40,
                    "service_result_schema": service_result.SCHEMA_VERSION,
                },
                log,
            )

            self.assertEqual("unknown", result["state"])
            self.assertEqual("validation-service-result", result["result_source"])
            self.assertIn("validation-service-result-read", result["detail"])

    def test_passed_validation_with_failed_writeback_preserves_both_facts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            service_result.result_path(record).write_text(
                json.dumps(
                    {
                        "schema_version": service_result.SCHEMA_VERSION,
                        "commit": "a" * 40,
                        "profile": "full",
                        "selection_mode": "full",
                        "final_validate_status": "PASSED",
                        "detail": None,
                        "exit_code": 75,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "passed_tests": 2129,
                        "scorecard_writeback": {
                            "status": "failed",
                            "error": "fixture refusal",
                        },
                    }
                )
            )

            result = service_result.from_run_record(
                record,
                {
                    "target": "a" * 40,
                    "service_result_schema": service_result.SCHEMA_VERSION,
                },
                root / "unused.log",
            )
            self.assertEqual("completed", result["state"])
            self.assertEqual("success", result["result"])
            self.assertEqual("PASSED", result["final_validate_status"])
            self.assertEqual(75, result["exit_code"])
            self.assertEqual("failed", result["scorecard_writeback"]["status"])
            self.assertTrue(service_result.is_evidenced_terminal(result))

            value = json.loads(service_result.result_path(record).read_text())
            value["scorecard_writeback"] = None
            service_result.result_path(record).write_text(json.dumps(value))
            mutated = service_result.from_run_record(
                record,
                {
                    "target": "a" * 40,
                    "service_result_schema": service_result.SCHEMA_VERSION,
                },
                root / "unused.log",
            )
            self.assertEqual("unknown", mutated["state"])
            self.assertIn("requires 0, got 75", mutated["detail"])

    def test_genuine_could_not_run_remains_distinct(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            service_result.result_path(record).write_text(
                json.dumps(
                    {
                        "schema_version": service_result.SCHEMA_VERSION,
                        "commit": "a" * 40,
                        "profile": "full",
                        "selection_mode": "full",
                        "final_validate_status": "COULD_NOT_RUN",
                        "detail": [
                            "refused by: pre.reverie_pin",
                            "recorded pin is not an ancestor",
                        ],
                        "exit_code": 75,
                        "executed_nodes": 0,
                        "executed_tests": None,
                        "passed_tests": None,
                        "scorecard_writeback": None,
                    }
                )
            )

            result = service_result.from_run_record(
                record,
                {
                    "target": "a" * 40,
                    "service_result_schema": service_result.SCHEMA_VERSION,
                },
                root / "unused.log",
            )
            self.assertEqual("completed", result["state"])
            self.assertEqual("no-result", result["result"])
            self.assertEqual("COULD_NOT_RUN", result["final_validate_status"])
            self.assertEqual(
                "refused by: pre.reverie_pin\nrecorded pin is not an ancestor",
                result["detail"],
            )
            self.assertIsNone(result["passed_tests"])
            self.assertIsNone(result["scorecard_writeback"])

    def test_current_run_killed_before_result_uses_loaded_systemd_signal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            properties = {
                "LoadState": "loaded",
                "ExecMainCode": "killed",
                "ExecMainStatus": "15",
                "InvocationID": "fixture-invocation",
            }

            result = service_result.from_run_record(
                record,
                {
                    "target": "a" * 40,
                    "service_result_schema": service_result.SCHEMA_VERSION,
                },
                root / "unused.log",
                properties=properties,
            )

            self.assertEqual("killed", result["state"])
            self.assertEqual(143, result["exit_code"])
            self.assertEqual("loaded-systemd-unit", result["result_source"])
            self.assertIn("validation-service-result-read", result["detail"])

    def test_current_run_unknown_field_fails_by_name(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            result_path = service_result.result_path(record)
            result_path.write_text(
                json.dumps(
                    {
                        "schema_version": service_result.SCHEMA_VERSION,
                        "commit": "a" * 40,
                        "profile": "full",
                        "selection_mode": "full",
                        "final_validate_status": "PASSED",
                        "detail": None,
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "passed_tests": 2129,
                        "scorecard_writeback": None,
                        "future": True,
                    }
                )
            )

            result = service_result.from_run_record(
                record,
                {
                    "target": "a" * 40,
                    "service_result_schema": service_result.SCHEMA_VERSION,
                },
                root / "unused.log",
            )

            self.assertEqual("unknown", result["state"])
            self.assertIn("validation-service-result-fields", result["detail"])
            self.assertIn("future", result["detail"])

    def test_verified_bytes_are_parsed_without_a_second_artifact_read(self) -> None:
        payload = json.dumps(
            {
                "schema_version": service_result.SCHEMA_VERSION,
                "commit": "a" * 40,
                "profile": "full",
                "selection_mode": "full",
                "final_validate_status": "PASSED",
                "detail": None,
                "exit_code": 0,
                "executed_nodes": 76,
                "executed_tests": 2129,
                "passed_tests": 2129,
                "scorecard_writeback": {"status": "completed"},
            }
        ).encode()

        with tempfile.TemporaryDirectory() as temporary:
            historical_schema = self.write_schema(
                Path(temporary), service_result.SELECTION_SCHEMA_VERSION
            )
            evidence = service_result.read_framework_result_bytes(
                payload,
                expected_commit="a" * 40,
                schema_path=historical_schema,
            )

        self.assertEqual("PASSED", evidence.final_validate_status)
        self.assertEqual(2129, evidence.executed_tests)
        self.assertEqual(2129, evidence.passed_tests)

    def test_historical_run_without_marker_keeps_log_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            log = root / "validate.log"
            log.write_text(
                "FINAL_VALIDATE_STATUS: PASSED\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=1s cpu=1s "
                "cpu_user=1s cpu_system=0s exit=0\n"
            )

            result = service_result.from_run_record(
                record, {"target": "a" * 40}, log
            )

            self.assertEqual("completed", result["state"])
            self.assertEqual("durable-log", result["result_source"])

    def test_schema_one_result_is_readable_but_has_no_current_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            service_result.result_path(record).write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "commit": "a" * 40,
                        "profile": "full",
                        "final_validate_status": "PASSED",
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                    }
                )
            )

            evidence = service_result.read_framework_result(
                service_result.result_path(record), expected_commit="a" * 40
            )
            self.assertEqual(1, evidence.service_result_schema)
            self.assertIsNone(evidence.passed_tests)
            evidence = service_result.read_framework_result(
                service_result.result_path(record), expected_commit="a" * 40
            )
            self.assertIsNone(evidence.passed_tests)
            result = service_result.from_run_record(
                record,
                {"target": "a" * 40, "service_result_schema": 1},
                root / "unused.log",
            )
            self.assertEqual("unknown", result["state"])
            self.assertIn("readable but has no current authority", result["detail"])
            self.assertFalse(service_result.is_evidenced_terminal(result))

    def test_schema_two_result_is_readable_but_has_no_current_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            service_result.result_path(record).write_text(
                json.dumps(
                    {
                        "schema_version": 2,
                        "commit": "a" * 40,
                        "profile": "full",
                        "final_validate_status": "PASSED",
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "scorecard_writeback": {"status": "completed"},
                    }
                )
            )

            result = service_result.from_run_record(
                record,
                {"target": "a" * 40, "service_result_schema": 2},
                root / "unused.log",
            )
            self.assertEqual("unknown", result["state"])
            self.assertIn("historical schema 2", result["detail"])
            self.assertFalse(service_result.is_evidenced_terminal(result))

    def test_schema_three_missing_passed_count_remains_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            record = root / "validate-test.json"
            service_result.result_path(record).write_text(
                json.dumps(
                    {
                        "schema_version": service_result.SELECTION_SCHEMA_VERSION,
                        "commit": "a" * 40,
                        "profile": "full",
                        "selection_mode": "full",
                        "final_validate_status": "PASSED",
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "scorecard_writeback": {"status": "completed"},
                    }
                )
            )

            evidence = service_result.read_framework_result(
                service_result.result_path(record), expected_commit="a" * 40
            )
            self.assertEqual(service_result.SELECTION_SCHEMA_VERSION, evidence.service_result_schema)
            self.assertIsNone(evidence.passed_tests)
            result = service_result.from_run_record(
                record,
                {
                    "target": "a" * 40,
                    "service_result_schema": service_result.SELECTION_SCHEMA_VERSION,
                },
                root / "unused.log",
            )
            self.assertEqual("unknown", result["state"])
            self.assertIsNone(result["passed_tests"])
            self.assertIn("historical schema 3", result["detail"])

    def test_current_result_missing_writeback_field_fails_by_name(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "result.json"
            path.write_text(
                json.dumps(
                    {
                        "schema_version": service_result.SCHEMA_VERSION,
                        "commit": "a" * 40,
                        "profile": "full",
                        "selection_mode": "full",
                        "final_validate_status": "PASSED",
                        "detail": None,
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "passed_tests": 2129,
                    }
                )
            )
            with self.assertRaisesRegex(
                RuntimeError,
                "validation-service-result-fields: schema 5 expected",
            ):
                service_result.read_framework_result(path, expected_commit="a" * 40)

    def test_current_result_missing_selection_field_fails_by_name(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "result.json"
            path.write_text(
                json.dumps(
                    {
                        "schema_version": service_result.SCHEMA_VERSION,
                        "commit": "a" * 40,
                        "profile": "full",
                        "final_validate_status": "PASSED",
                        "detail": None,
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "passed_tests": 2129,
                        "scorecard_writeback": {"status": "completed"},
                    }
                )
            )
            with self.assertRaisesRegex(
                RuntimeError,
                "validation-service-result-fields: schema 5 expected",
            ):
                service_result.read_framework_result(path, expected_commit="a" * 40)

    def test_current_result_missing_passed_count_fails_by_name(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "result.json"
            path.write_text(
                json.dumps(
                    {
                        "schema_version": service_result.SCHEMA_VERSION,
                        "commit": "a" * 40,
                        "profile": "full",
                        "selection_mode": "full",
                        "final_validate_status": "PASSED",
                        "detail": None,
                        "exit_code": 0,
                        "executed_nodes": 76,
                        "executed_tests": 2129,
                        "scorecard_writeback": {"status": "completed"},
                    }
                )
            )
            with self.assertRaisesRegex(
                RuntimeError,
                "validation-service-result-fields: schema 5 expected",
            ):
                service_result.read_framework_result(path, expected_commit="a" * 40)

    def test_current_results_require_exact_consistent_passed_counts(self) -> None:
        base = {
            "schema_version": service_result.SCHEMA_VERSION,
            "commit": "a" * 40,
            "profile": "full",
            "selection_mode": "full",
            "final_validate_status": "FAILED",
            "detail": None,
            "exit_code": 1,
            "executed_nodes": 2,
            "executed_tests": 3,
            "passed_tests": 2,
            "scorecard_writeback": None,
        }
        evidence = service_result.read_framework_result_bytes(
            json.dumps(base).encode(), expected_commit="a" * 40
        )
        self.assertEqual(3, evidence.executed_tests)
        self.assertEqual(2, evidence.passed_tests)

        for label, update, message in (
            (
                "missing",
                {"passed_tests": None},
                "current result with executed_tests requires an exact count",
            ),
            ("greater", {"passed_tests": 4}, "exceeds executed_tests 3"),
            (
                "successful mismatch",
                {
                    "final_validate_status": "PASSED",
                    "exit_code": 0,
                    "passed_tests": 2,
                },
                "requires passed_tests == executed_tests",
            ),
        ):
            with self.subTest(label=label):
                value = {**base, **update}
                with self.assertRaisesRegex(RuntimeError, message):
                    service_result.read_framework_result_bytes(
                        json.dumps(value).encode(), expected_commit="a" * 40
                    )


    def test_current_result_requires_explicit_nullable_detail(self) -> None:
        value = {
            "schema_version": service_result.SCHEMA_VERSION,
            "commit": "a" * 40,
            "profile": "full",
            "selection_mode": "full",
            "final_validate_status": "COULD_NOT_RUN",
            "detail": None,
            "exit_code": 75,
            "executed_nodes": 0,
            "executed_tests": None,
            "passed_tests": None,
            "scorecard_writeback": None,
        }
        evidence = service_result.read_framework_result_bytes(
            json.dumps(value).encode(), expected_commit="a" * 40
        )
        self.assertIsNone(evidence.detail)
        floating_version = {**value, "schema_version": 5.0}
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-service-result-schema_version",
        ):
            service_result.read_framework_result_bytes(
                json.dumps(floating_version).encode(), expected_commit="a" * 40
            )
        result = service_result.classify(evidence)
        self.assertEqual(
            "validation reported COULD_NOT_RUN without detail", result["detail"]
        )
        self.assertTrue(service_result.is_evidenced_terminal(result))
        missing_terminal_detail = dict(result)
        del missing_terminal_detail["detail"]
        self.assertFalse(
            service_result.is_evidenced_terminal(missing_terminal_detail)
        )
        result["detail"] = None
        self.assertFalse(service_result.is_evidenced_terminal(result))
        del value["detail"]
        with self.assertRaisesRegex(RuntimeError, "schema 5 expected"):
            service_result.read_framework_result_bytes(
                json.dumps(value).encode(), expected_commit="a" * 40
            )

    def test_schema_four_pass_with_failed_writeback_retains_authority(self) -> None:
        value = {
            "schema_version": service_result.TEST_COUNTS_SCHEMA_VERSION,
            "commit": "a" * 40,
            "profile": "full",
            "selection_mode": "full",
            "final_validate_status": "PASSED",
            "exit_code": 75,
            "executed_nodes": 76,
            "executed_tests": 2129,
            "passed_tests": 2129,
            "scorecard_writeback": {
                "status": "failed",
                "error": "fixture writeback refusal",
            },
        }
        evidence = service_result.read_framework_result_bytes(
            json.dumps(value).encode(), expected_commit="a" * 40
        )
        floating_version = {**value, "schema_version": 4.0}
        with self.assertRaisesRegex(
            RuntimeError,
            "validation-service-result-schema_version",
        ):
            service_result.read_framework_result_bytes(
                json.dumps(floating_version).encode(), expected_commit="a" * 40
            )
        result = service_result.classify(evidence)
        self.assertEqual("validation-service-result", result["result_source"])
        self.assertEqual("completed", result["state"])
        self.assertEqual("success", result["result"])
        self.assertEqual(75, result["exit_code"])
        self.assertTrue(service_result.is_evidenced_terminal(result))

if __name__ == "__main__":
    unittest.main()
