from __future__ import annotations

import contextlib
import io
import json
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import pane_watch  # noqa: E402
import run_registry  # noqa: E402
import service_result  # noqa: E402


def current_validation_handle(log: Path) -> dict[str, object]:
    return {
        "schema_version": run_registry.SCHEMA_VERSION,
        "kind": "validate",
        "state": "running",
        "unit": "validate-test.service",
        "target": "a" * 40,
        "repo": "rrnewton/hermit",
        "checkout": "/tmp/checkout",
        "source_checkout": "/tmp/source",
        "temporary_checkout": True,
        "cargo_home": "/tmp/cargo",
        "log": str(log),
        "agent": "type-audit",
        "pr": None,
        "started_at": "2026-08-28T07:00:00Z",
        "parent_checkout_head": "b" * 40,
        "producer": run_registry.PRODUCER,
        "admission": "ci-hub validate-lock",
        "pane_role": "observer-only",
        "validate_lock_child_deadline_seconds": 3600,
        "e2e_result_root": "/tmp/e2e",
        "safe_ci_dag_runner_log_dir": "/tmp/dag",
        "hermit_run_timeout_seconds": 3300,
    }


class PaneWatchTest(unittest.TestCase):
    def run_observer(self, root: Path) -> int:
        return pane_watch.main(
            [
                "--unit", "validate-test.service",
                "--target", "a" * 40,
                "--checkout", str(root),
                "--log", str(root / "validate.log"),
                "--record", str(root / "runs/validate-test.json"),
                "--appearance-seconds", "0",
                "--poll-seconds", "0",
            ]
        )

    def test_live_unknown_live_waits_for_real_terminal_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            record = root / "runs/validate-test.json"
            log.write_text("")
            run_registry.write_record(record, current_validation_handle(log))
            observed = 0

            def properties(_unit: str) -> dict[str, str] | None:
                nonlocal observed
                observed += 1
                durable = run_registry.read_record(record)
                self.assertEqual("running", durable["state"])
                self.assertIsNone(durable.get("finished_at"))
                self.assertIsNone(durable.get("exit_code"))
                if observed == 2:
                    return None
                if observed in (1, 3):
                    return {"ActiveState": "active", "MainPID": "0"}
                self.assertEqual(4, observed)
                log.write_text(
                    "validate-lock: supervisor received signal 15\n"
                    "# ci-hub/validate-lock tool COST ACTUAL wall=1s cpu=1s "
                    "cpu_user=1s cpu_system=0s exit=143\n"
                )
                return {"ActiveState": "inactive", "MainPID": "0"}

            output = io.StringIO()
            with mock.patch.object(
                pane_watch, "service_properties", side_effect=properties
            ), contextlib.redirect_stdout(output):
                rc = self.run_observer(root)

            self.assertEqual(4, observed)
            self.assertEqual(143, rc)
            durable = run_registry.read_record(record)
            self.assertEqual("killed", durable["state"])
            self.assertEqual("terminated", durable["result"])
            self.assertEqual(15, durable["signal"])
            self.assertTrue(durable["finished_at"])
            self.assertIn("OBSERVATION UNCERTAIN", output.getvalue())
            self.assertIn("OBSERVATION RECOVERED", output.getvalue())
            self.assertEqual(1, output.getvalue().count("VALIDATE FINISHED"))

    def test_exhausted_observation_preserves_unfinished_run(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            record = root / "runs/validate-test.json"
            log.write_text("")
            original = current_validation_handle(log) | {
                "service_result_schema": service_result.SCHEMA_VERSION,
                "detail": "original producer detail",
            }
            run_registry.write_record(record, original)
            output = io.StringIO()
            with mock.patch.object(
                pane_watch, "service_properties",
                side_effect=[{"ActiveState": "active", "MainPID": "0"}] + [None] * 5,
            ) as query, contextlib.redirect_stdout(output):
                rc = self.run_observer(root)

            self.assertEqual(6, query.call_count)
            self.assertEqual(1, rc)
            durable = run_registry.read_record(record)
            for key, value in original.items():
                self.assertEqual(value, durable[key], key)
            self.assertIsNone(durable.get("finished_at"))
            self.assertIsNone(durable.get("result"))
            self.assertIsNone(durable.get("exit_code"))
            self.assertIn("OBSERVATION UNCERTAIN", output.getvalue())
            self.assertIn("service-result.json", output.getvalue())
            self.assertNotIn("VALIDATE FINISHED", output.getvalue())

    def test_service_query_rejects_malformed_or_unavailable_state(self) -> None:
        for returncode, stdout in (
            (1, ""),
            (0, "LoadState=loaded\nInvocationID=abc\nMainPID=0\n"),
            (0, "LoadState=loaded\nInvocationID=abc\nActiveState=garbled\nMainPID=0\n"),
            (0, "LoadState=loaded\nInvocationID=abc\nActiveState=active\nMainPID=bad\n"),
        ):
            with self.subTest(returncode=returncode, stdout=stdout), mock.patch.object(
                pane_watch.bounded_capture, "capture",
                return_value=subprocess.CompletedProcess([], returncode, stdout, "unavailable"),
            ):
                self.assertIsNone(pane_watch.service_properties("validate-test.service"))
        with mock.patch.object(
            pane_watch.bounded_capture, "capture", side_effect=OSError("bus unavailable")
        ):
            self.assertIsNone(pane_watch.service_properties("validate-test.service"))
        for state in ("active", "inactive", "failed"):
            with self.subTest(state=state), mock.patch.object(
                pane_watch.bounded_capture, "capture",
                return_value=subprocess.CompletedProcess(
                    [], 0, f"LoadState=loaded\nInvocationID=abc\nActiveState={state}\nMainPID=0\n", ""
                ),
            ):
                self.assertEqual(state, pane_watch.service_properties("validate-test.service")["ActiveState"])

    def test_observer_preserves_concurrent_terminal_cause_and_time(self) -> None:
        for terminal in (
            {"state": "killed", "result": "terminated", "exit_code": 143, "signal": 15},
            {
                "state": "completed", "result": "exit-code", "exit_code": 1,
                "final_validate_status": "FAILED",
            },
        ):
            with self.subTest(state=terminal["state"]), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                log = root / "validate.log"
                log.write_text("")
                record = root / "runs/validate-test.json"
                original = {
                    "schema_version": 1,
                    **terminal,
                    "finished_at": "2026-09-15T20:00:00+00:00",
                    "detail": "original terminal cause",
                }
                run_registry.write_record(record, current_validation_handle(log))

                def stale_classification(*_args: object, **_kwargs: object) -> dict[str, object]:
                    run_registry.write_record(record, original)
                    return {"state": "unknown", "result": "unknown", "exit_code": None}

                output = io.StringIO()
                with mock.patch.object(
                    pane_watch, "service_properties", return_value=None
                ), mock.patch.object(
                    service_result, "from_run_record", side_effect=stale_classification
                ), contextlib.redirect_stdout(output):
                    rc = self.run_observer(root)

                self.assertEqual(terminal["exit_code"], rc)
                self.assertIn(f"state={terminal['state']}", output.getvalue())
                self.assertEqual(
                    original | {"observed_safe_ci_cgroups": []},
                    run_registry.read_record(record),
                )

    def test_terminal_guard_keeps_boxing_evidence_and_allows_unknown_repair(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            record = Path(temporary) / "runs/validate-test.json"
            run_registry.write_record(record, {
                "schema_version": 1, "state": "unknown", "result": "unknown",
                "finished_at": "2026-09-15T19:00:00+00:00",
            })
            terminal = run_registry.update_from_observer(
                record, state="killed", result="terminated", exit_code=143, signal=15,
                finished_at="2026-09-15T20:00:00+00:00",
                observed_safe_ci_cgroups=["/safe-ci-first.scope"],
            )
            self.assertEqual("killed", terminal["state"])
            self.assertEqual("2026-09-15T20:00:00+00:00", terminal["finished_at"])
            updated = run_registry.update_from_observer(
                record, state="unknown", result="unknown", exit_code=None,
                observed_safe_ci_cgroups=["/safe-ci-second.scope"],
            )
            self.assertEqual(
                ["/safe-ci-first.scope", "/safe-ci-second.scope"],
                updated["observed_safe_ci_cgroups"],
            )
            for key, value in terminal.items():
                if key != "observed_safe_ci_cgroups":
                    self.assertEqual(value, updated[key], key)

    def test_unfinished_observer_accumulates_boxing_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            record = Path(temporary) / "runs/validate-test.json"
            run_registry.write_record(record, {
                "schema_version": 1, "state": "running",
                "observed_safe_ci_cgroups": ["/safe-ci-first.scope"],
            })
            updated = run_registry.update_from_observer(
                record, observed_safe_ci_cgroups=[]
            )
            self.assertEqual(["/safe-ci-first.scope"], updated["observed_safe_ci_cgroups"])
            updated = run_registry.update_from_observer(
                record, observed_safe_ci_cgroups=["/safe-ci-second.scope"]
            )
            self.assertEqual(
                ["/safe-ci-first.scope", "/safe-ci-second.scope"],
                updated["observed_safe_ci_cgroups"],
            )
            self.assertEqual("running", updated["state"])
            self.assertNotIn("finished_at", updated)

    def test_uncertainty_preserves_concurrent_producer_detail(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            log.write_text("")
            record = root / "runs/validate-test.json"
            run_registry.write_record(record, current_validation_handle(log))

            def stale_classification(*_args: object, **_kwargs: object) -> dict[str, object]:
                run_registry.update_record(record, detail="first producer cause")
                return {
                    "state": "unknown", "result": "unknown", "exit_code": None,
                    "detail": "observer could not read the result",
                }

            with mock.patch.object(
                pane_watch, "service_properties", return_value=None
            ), mock.patch.object(
                service_result, "from_run_record", side_effect=stale_classification
            ), contextlib.redirect_stdout(io.StringIO()):
                rc = self.run_observer(root)
            durable = run_registry.read_record(record)
            self.assertEqual(1, rc)
            self.assertEqual("first producer cause", durable["detail"])
            self.assertEqual("running", durable["state"])
            self.assertNotIn("result", durable)
            self.assertNotIn("finished_at", durable)

    def test_observed_terminal_with_missing_result_keeps_outcome_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            log.write_text("")
            record = root / "runs/validate-test.json"
            run_registry.write_record(record, current_validation_handle(log) | {
                "service_result_schema": service_result.SCHEMA_VERSION,
            })
            output = io.StringIO()
            with mock.patch.object(
                pane_watch, "service_properties", return_value={"ActiveState": "inactive", "MainPID": "0"}
            ), contextlib.redirect_stdout(output):
                rc = self.run_observer(root)
            durable = run_registry.read_record(record)
            self.assertEqual(1, rc)
            self.assertEqual("unknown", durable["state"])
            self.assertEqual("unknown", durable["result"])
            self.assertIsNone(durable["exit_code"])
            self.assertTrue(durable["finished_at"])
            self.assertIn("service-result.json", durable["detail"])
            self.assertIn("VALIDATE FINISHED state=unknown", output.getvalue())

    def test_collected_killed_unit_uses_durable_inner_exit_not_success_placeholder(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            record = root / "runs/validate-test.json"
            log.write_text(
                "validate-lock: supervisor received signal 15\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=50.197s cpu=0.2s "
                "cpu_user=0.1s cpu_system=0.1s exit=143\n"
            )
            run_registry.write_record(
                record,
                {
                    "schema_version": 1,
                    "state": "running",
                    "target": "a" * 40,
                    "log": str(log),
                },
            )
            output = io.StringIO()
            with mock.patch.object(pane_watch, "service_properties", return_value=None):
                with contextlib.redirect_stdout(output):
                    rc = pane_watch.main(
                        [
                            "--unit",
                            "validate-test.service",
                            "--target",
                            "a" * 40,
                            "--checkout",
                            str(root),
                            "--log",
                            str(log),
                            "--record",
                            str(record),
                            "--appearance-seconds",
                            "0",
                        ]
                    )

            self.assertEqual(143, rc)
            durable = run_registry.read_record(record)
            self.assertEqual("killed", durable["state"])
            self.assertEqual("terminated", durable["result"])
            self.assertEqual(143, durable["exit_code"])
            self.assertEqual(15, durable["signal"])
            self.assertIn("state=killed result=terminated exit=143", output.getvalue())

    def test_current_handle_keeps_attempt_result_fields_typed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            record = root / "runs/validate-test.json"
            log.write_text(
                "# ci-hub/validate-lock tool COST ACTUAL wall=50.197s cpu=0.2s "
                "cpu_user=0.1s cpu_system=0.1s exit=143\n"
            )
            run_registry.write_record(record, current_validation_handle(log))
            with mock.patch.object(pane_watch, "service_properties", return_value=None):
                rc = pane_watch.main(
                    [
                        "--unit",
                        "validate-test.service",
                        "--target",
                        "a" * 40,
                        "--checkout",
                        str(root),
                        "--log",
                        str(log),
                        "--record",
                        str(record),
                        "--appearance-seconds",
                        "0",
                    ]
                )
            self.assertEqual(143, rc)
            durable = run_registry.read_record(record)
            self.assertEqual(1, durable["schema_version"])
            self.assertEqual("validate", durable["kind"])
            self.assertEqual("killed", durable["state"])

    def test_current_handle_uses_framework_result_instead_of_log(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            record = root / "runs/validate-test.json"
            log.write_text(
                "FINAL_VALIDATE_STATUS: FAILED\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=1s cpu=1s "
                "cpu_user=1s cpu_system=0s exit=1\n"
            )
            run_registry.write_record(
                record,
                current_validation_handle(log)
                | {"service_result_schema": service_result.SCHEMA_VERSION},
            )
            service_result.result_path(record).write_text(
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

            with mock.patch.object(pane_watch, "service_properties", return_value=None):
                rc = pane_watch.main(
                    [
                        "--unit",
                        "validate-test.service",
                        "--target",
                        "a" * 40,
                        "--checkout",
                        str(root),
                        "--log",
                        str(log),
                        "--record",
                        str(record),
                        "--appearance-seconds",
                        "0",
                    ]
                )

            self.assertEqual(0, rc)
            durable = run_registry.read_record(record)
            self.assertEqual("completed", durable["state"])
            self.assertEqual("success", durable["result"])
            self.assertEqual("validation-service-result", durable["result_source"])
            self.assertEqual(76, durable["executed_nodes"])
            self.assertEqual(2129, durable["executed_tests"])
            self.assertEqual(2129, durable["passed_tests"])
            self.assertEqual({"status": "completed"}, durable["scorecard_writeback"])

    def test_current_handle_refuses_result_missing_passed_count(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            record = root / "runs/validate-test.json"
            log.write_text(
                "FINAL_VALIDATE_STATUS: PASSED\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=1s cpu=1s "
                "cpu_user=1s cpu_system=0s exit=0\n"
            )
            run_registry.write_record(
                record,
                current_validation_handle(log)
                | {"service_result_schema": service_result.SCHEMA_VERSION},
            )
            service_result.result_path(record).write_text(
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

            output = io.StringIO()
            with mock.patch.object(
                pane_watch, "service_properties", return_value=None
            ), contextlib.redirect_stdout(output):
                rc = pane_watch.main(
                    [
                        "--unit",
                        "validate-test.service",
                        "--target",
                        "a" * 40,
                        "--checkout",
                        str(root),
                        "--log",
                        str(log),
                        "--record",
                        str(record),
                        "--appearance-seconds",
                        "0",
                    ]
                )

            self.assertEqual(1, rc)
            durable = run_registry.read_record(record)
            self.assertEqual("running", durable["state"])
            self.assertNotIn("passed_tests", durable)
            self.assertIsNone(durable.get("finished_at"))
            self.assertIsNone(durable.get("result"))
            self.assertIn("schema 5 expected", output.getvalue())
            self.assertIn("schema 5 expected", durable["detail"])
            self.assertIn("observer uncertainty", durable["detail"])
            self.assertIn("OBSERVATION UNCERTAIN", output.getvalue())
            self.assertNotIn("VALIDATE FINISHED", output.getvalue())

    def test_historical_results_keep_passed_count_unknown(self) -> None:
        for version in (1, 2, 3):
            with self.subTest(version=version), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                log = root / "validate.log"
                record = root / "runs/validate-test.json"
                log.write_text(
                    "FINAL_VALIDATE_STATUS: PASSED\n"
                    "# ci-hub/validate-lock tool COST ACTUAL wall=1s cpu=1s "
                    "cpu_user=1s cpu_system=0s exit=0\n"
                )
                run_registry.write_record(
                    record,
                    current_validation_handle(log) | {"service_result_schema": version},
                )
                result = {
                    "schema_version": version,
                    "commit": "a" * 40,
                    "profile": "full",
                    "final_validate_status": "PASSED",
                    "exit_code": 0,
                    "executed_nodes": 76,
                    "executed_tests": 2129,
                }
                if version in (2, 3):
                    result["scorecard_writeback"] = {"status": "completed"}
                if version == 3:
                    result["selection_mode"] = "full"
                service_result.result_path(record).write_text(json.dumps(result))

                output = io.StringIO()
                with mock.patch.object(
                    pane_watch, "service_properties", return_value=None
                ), contextlib.redirect_stdout(output):
                    rc = pane_watch.main(
                        [
                            "--unit",
                            "validate-test.service",
                            "--target",
                            "a" * 40,
                            "--checkout",
                            str(root),
                            "--log",
                            str(log),
                            "--record",
                            str(record),
                            "--appearance-seconds",
                            "0",
                        ]
                    )

                self.assertEqual(1, rc)
                durable = run_registry.read_record(record)
                self.assertEqual("running", durable["state"])
                self.assertNotIn("passed_tests", durable)
                self.assertIsNone(durable.get("finished_at"))
                self.assertIsNone(durable.get("result"))
                self.assertIn(f"historical schema {version}", output.getvalue())
                self.assertIn(f"historical schema {version}", durable["detail"])
                self.assertIn("observer uncertainty", durable["detail"])
                self.assertIn("OBSERVATION UNCERTAIN", output.getvalue())
                self.assertNotIn("VALIDATE FINISHED", output.getvalue())

    def test_boxing_proof_is_bound_to_descendant_process_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            proc = Path(temporary)
            fixtures = {
                10: (1, "/user.slice/3pai_sandbox.slice/validate.service"),
                11: (10, "/user.slice/safe-ci-dag-runner/safe-ci-node.scope"),
                12: (1, "/user.slice/safe-ci-unrelated.scope"),
            }
            for pid, (ppid, cgroup) in fixtures.items():
                directory = proc / str(pid)
                directory.mkdir()
                (directory / "stat").write_text(
                    f"{pid} (fixture) S {ppid} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
                )
                (directory / "cgroup").write_text(f"0::{cgroup}\n")

            self.assertEqual(
                {"/user.slice/safe-ci-dag-runner/safe-ci-node.scope"},
                pane_watch.safe_ci_cgroups(10, proc),
            )

    def test_durable_handle_round_trip_preserves_observer_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "runs/validate-test.json"
            run_registry.write_record(
                path,
                {
                    "schema_version": 1,
                    "state": "running",
                    "target": "a" * 40,
                },
            )
            updated = run_registry.update_record(
                path,
                state="completed",
                observed_safe_ci_cgroups=["/safe-ci-test.scope"],
            )

            self.assertEqual(updated, run_registry.read_record(path))
            self.assertEqual("completed", updated["state"])
            self.assertEqual(["/safe-ci-test.scope"], updated["observed_safe_ci_cgroups"])

    def test_concurrent_final_updates_do_not_erase_each_other(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "runs/validate-test.json"
            run_registry.write_record(path, {"schema_version": 1, "state": "running"})
            barrier = threading.Barrier(3)

            def update(**fields: object) -> None:
                barrier.wait()
                run_registry.update_record(path, **fields)

            caller = threading.Thread(target=update, kwargs={"exit_code": 0})
            observer = threading.Thread(
                target=update,
                kwargs={"observed_safe_ci_cgroups": ["/safe-ci-test.scope"]},
            )
            caller.start()
            observer.start()
            barrier.wait()
            caller.join()
            observer.join()

            durable = run_registry.read_record(path)
            self.assertEqual(0, durable["exit_code"])
            self.assertEqual(["/safe-ci-test.scope"], durable["observed_safe_ci_cgroups"])

    def test_observer_cannot_overwrite_concurrent_prelaunch_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log = root / "validate.log"
            record = root / "runs/validate-test.json"
            log.write_text("")
            handle = current_validation_handle(log)
            handle["state"] = "launching"
            run_registry.write_record(record, handle)

            def stale_classification(*_args: object, **_kwargs: object) -> dict[str, object]:
                run_registry.update_record(
                    record,
                    state="refused",
                    result="systemd-launch-refused",
                    exit_code=2,
                    detail="validation log already exists: /fixture/validate.log",
                    finished_at="2026-08-30T12:00:00+00:00",
                )
                return {
                    "state": "unknown",
                    "result": "unknown",
                    "exit_code": None,
                    "detail": "no unit was observed",
                }

            output = io.StringIO()
            with mock.patch.object(
                pane_watch, "service_properties", return_value=None
            ), mock.patch.object(
                pane_watch.service_result,
                "from_run_record",
                side_effect=stale_classification,
            ), contextlib.redirect_stdout(output):
                rc = pane_watch.main(
                    [
                        "--unit",
                        "validate-test.service",
                        "--target",
                        "a" * 40,
                        "--checkout",
                        str(root),
                        "--log",
                        str(log),
                        "--record",
                        str(record),
                        "--appearance-seconds",
                        "0",
                    ]
                )

            self.assertEqual(2, rc)
            durable = run_registry.read_record(record)
            self.assertEqual("refused", durable["state"])
            self.assertEqual("systemd-launch-refused", durable["result"])
            self.assertIn("state=refused result=systemd-launch-refused exit=2", output.getvalue())


if __name__ == "__main__":
    unittest.main()
