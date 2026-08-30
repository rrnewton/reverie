from __future__ import annotations

import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from typing import Any


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import correct_owner_watch_handles as correction  # noqa: E402


ATTEMPT = "20260830T123456123456Z-4242"
TARGET = "a" * 40


def absent_unit(
    command: list[str], **_kwargs: object
) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(
        command,
        0,
        "LoadState=not-found\nInvocationID=\n",
        "",
    )


class CorrectionTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.state_root = root / "dev-hermit"
        self.owner_state = root / "owner-state"
        (self.state_root / "hermit").mkdir(parents=True)
        (self.state_root / "ignored/validate/artifacts").mkdir(parents=True)
        (self.owner_state / "results").mkdir(parents=True)
        unit, self.handle_path, self.log, _service, artifacts = (
            correction.expected_paths(self.state_root, self.owner_state, ATTEMPT)
        )
        self.unit = unit
        self.log.parent.mkdir(parents=True)
        detail = f"validation log already exists: {self.log}"
        line = f"validate-run: REFUSED: {detail}"
        self.log.write_text(
            "pane-lifecycle: fixture\n"
            f"{line}\n"
            f"{correction.REFUSED_STATE_LINE}\n"
            f"{correction.REFUSED_REMEDY_LINE}\n"
        )
        self.result_path = self.owner_state / "results" / f"{ATTEMPT}.json"
        self.result_path.write_text(
            json.dumps(
                {
                    "attempt_id": ATTEMPT,
                    "sha": TARGET,
                    "validate_log": str(self.log),
                    "validation_run_record": str(self.handle_path),
                    "validate_run_rc": 2,
                    "ledger_row_found": False,
                    "lock_admission": {"state": "UNKNOWN"},
                    "admission_refusal": {
                        "kind": "admission-refusal",
                        "detail": detail,
                        "line": line,
                    },
                    # This is deliberately not a correction predicate. The
                    # historical owner-level outcome varied with older receipts
                    # and post-attempt restoration.
                    "state": "PASS_ALREADY_VALIDATED",
                },
                indent=2,
            )
            + "\n"
        )
        self.handle_path.parent.mkdir(parents=True)
        self.original_detail = "final validate status is absent"
        self.handle = {
            "schema_version": 1,
            "kind": "validate",
            "state": "unknown",
            "unit": f"{unit}.service",
            "target": TARGET,
            "repo": "rrnewton/hermit",
            "checkout": str((self.state_root / "hermit").resolve()),
            "source_checkout": str((self.state_root / "hermit").resolve()),
            "temporary_checkout": False,
            "cargo_home": str(self.state_root / "worktrees/validate/missing-cargo"),
            "log": str(self.log),
            "agent": "owner-checkout-watch",
            "pr": None,
            "started_at": "2026-08-30T12:34:56+00:00",
            "parent_checkout_head": TARGET,
            "producer": correction.run_registry.PRODUCER,
            "admission": "ci-hub validate-lock",
            "pane_role": "observer-only",
            "validate_lock_child_deadline_seconds": 1800,
            "e2e_result_root": str(artifacts / "e2e"),
            "safe_ci_dag_runner_log_dir": str(artifacts / "safe-ci-dag-runner"),
            "hermit_run_timeout_seconds": 1619,
            "workspace_id": "workspace",
            "tab_id": "tab",
            "pane_id": "pane",
            "pane_title": "title",
            "result": "unknown",
            "exit_code": None,
            "detail": self.original_detail,
            "finished_at": "2026-08-30T12:35:00+00:00",
            "executed_nodes": None,
            "executed_tests": None,
            "final_validate_status": None,
            "result_source": "no-exit-evidence",
            "observed_safe_ci_cgroups": [],
        }
        self.handle_path.write_text(json.dumps(self.handle, indent=2) + "\n")
        self.result_bytes = self.result_path.read_bytes()
        self.handle_bytes = self.handle_path.read_bytes()
        ledger = self.state_root / "ledger/hermit/devbig014/2026-08.jsonl"
        ledger.parent.mkdir(parents=True)
        ledger.write_text("ledger sentinel\n")
        self.ledger = ledger

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def plan(self) -> dict[str, Any]:
        return correction.build_plan(
            self.state_root,
            self.owner_state,
            expected_count=1,
            run=absent_unit,
        )

    def add_candidate(self, attempt: str) -> Path:
        unit, handle_path, log, _service, artifacts = correction.expected_paths(
            self.state_root, self.owner_state, attempt
        )
        detail = f"validation log already exists: {log}"
        line = f"validate-run: REFUSED: {detail}"
        log.write_text(
            f"{line}\n{correction.REFUSED_STATE_LINE}\n"
            f"{correction.REFUSED_REMEDY_LINE}\n"
        )
        result = json.loads(self.result_path.read_text())
        result.update(
            attempt_id=attempt,
            validate_log=str(log),
            validation_run_record=str(handle_path),
            admission_refusal={
                "kind": "admission-refusal",
                "detail": detail,
                "line": line,
            },
        )
        (self.owner_state / "results" / f"{attempt}.json").write_text(
            json.dumps(result) + "\n"
        )
        handle = dict(self.handle)
        handle.update(
            unit=f"{unit}.service",
            log=str(log),
            e2e_result_root=str(artifacts / "e2e"),
            safe_ci_dag_runner_log_dir=str(artifacts / "safe-ci-dag-runner"),
        )
        handle_path.write_text(json.dumps(handle) + "\n")
        return handle_path

    def test_dry_run_reports_hashes_and_changes_nothing(self) -> None:
        before = self.handle_path.read_bytes()
        ledger_before = self.ledger.read_bytes()

        plan = self.plan()

        self.assertEqual(1, plan["pending_count"])
        self.assertEqual(0, plan["already_corrected_count"])
        record = plan["records"][0]
        self.assertEqual(correction.sha256(before), record["before_sha256"])
        self.assertNotEqual(record["before_sha256"], record["after_sha256"])
        self.assertEqual(before, self.handle_path.read_bytes())
        self.assertEqual(ledger_before, self.ledger.read_bytes())

    def test_apply_requires_reviewed_plan_and_is_idempotent(self) -> None:
        dry = self.plan()
        report = correction.apply_plan(
            self.state_root,
            self.owner_state,
            expected_count=1,
            expected_plan_sha256=str(dry["plan_sha256"]),
            run=absent_unit,
        )

        self.assertEqual(0, report["pending_count"])
        self.assertEqual(1, report["already_corrected_count"])
        self.assertEqual(1, report["applied_count"])
        self.assertEqual(dry["plan_sha256"], report["applied_plan_sha256"])
        self.assertEqual(dry["records"], report["changes"])
        corrected = json.loads(self.handle_path.read_text())
        self.assertEqual("refused", corrected["state"])
        self.assertEqual("systemd-launch-refused", corrected["result"])
        self.assertEqual(2, corrected["exit_code"])
        self.assertEqual(self.original_detail, corrected["detail"])
        self.assertNotIn("admission_result", corrected)

        second = self.plan()
        before = self.handle_path.read_bytes()
        repeated = correction.apply_plan(
            self.state_root,
            self.owner_state,
            expected_count=1,
            expected_plan_sha256=str(second["plan_sha256"]),
            run=absent_unit,
        )
        self.assertEqual(0, repeated["pending_count"])
        self.assertEqual(before, self.handle_path.read_bytes())

    def test_cli_apply_without_dry_run_hash_refuses(self) -> None:
        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            rc = correction.main(
                [
                    "--state-root",
                    str(self.state_root),
                    "--owner-state-dir",
                    str(self.owner_state),
                    "--expected-count",
                    "1",
                    "--apply",
                ],
                run=absent_unit,
            )
        self.assertEqual(2, rc)
        self.assertIn("requires the --expect-plan-sha256", error.getvalue())

    def test_cli_dry_run_discovers_count_but_apply_requires_it(self) -> None:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            rc = correction.main(
                [
                    "--state-root",
                    str(self.state_root),
                    "--owner-state-dir",
                    str(self.owner_state),
                ],
                run=absent_unit,
            )
        self.assertEqual(0, rc)
        self.assertEqual(1, json.loads(output.getvalue())["expected_count"])

        error = io.StringIO()
        with contextlib.redirect_stderr(error):
            rc = correction.main(
                [
                    "--state-root",
                    str(self.state_root),
                    "--owner-state-dir",
                    str(self.owner_state),
                    "--apply",
                    "--expect-plan-sha256",
                    "0" * 64,
                ],
                run=absent_unit,
            )
        self.assertEqual(2, rc)
        self.assertIn("requires the exact --expected-count", error.getvalue())

    def test_new_candidate_after_dry_run_refuses_the_whole_apply(self) -> None:
        dry = self.plan()
        second_attempt = "20260830T123457123456Z-4343"
        unit, handle_path, log, _service, artifacts = correction.expected_paths(
            self.state_root, self.owner_state, second_attempt
        )
        detail = f"validation log already exists: {log}"
        line = f"validate-run: REFUSED: {detail}"
        log.write_text(
            f"{line}\n{correction.REFUSED_STATE_LINE}\n"
            f"{correction.REFUSED_REMEDY_LINE}\n"
        )
        result = json.loads(self.result_path.read_text())
        result.update(
            attempt_id=second_attempt,
            validate_log=str(log),
            validation_run_record=str(handle_path),
            admission_refusal={
                "kind": "admission-refusal",
                "detail": detail,
                "line": line,
            },
        )
        (self.owner_state / "results" / f"{second_attempt}.json").write_text(
            json.dumps(result) + "\n"
        )
        handle = dict(self.handle)
        handle.update(
            unit=f"{unit}.service",
            log=str(log),
            e2e_result_root=str(artifacts / "e2e"),
            safe_ci_dag_runner_log_dir=str(artifacts / "safe-ci-dag-runner"),
        )
        handle_path.write_text(json.dumps(handle) + "\n")
        before = self.handle_path.read_bytes()

        with self.assertRaisesRegex(correction.CorrectionRefused, "expected exactly 1"):
            correction.apply_plan(
                self.state_root,
                self.owner_state,
                expected_count=1,
                expected_plan_sha256=str(dry["plan_sha256"]),
                run=absent_unit,
            )
        self.assertEqual(before, self.handle_path.read_bytes())

    def test_concurrent_new_handle_before_mutation_changes_nothing(self) -> None:
        dry = self.plan()
        before = self.handle_path.read_bytes()

        def add_unmatched_handle() -> None:
            attempt = "20260830T123459123456Z-4545"
            unit, handle_path, log, _service, artifacts = correction.expected_paths(
                self.state_root, self.owner_state, attempt
            )
            handle = dict(self.handle)
            handle.update(
                unit=f"{unit}.service",
                log=str(log),
                e2e_result_root=str(artifacts / "e2e"),
                safe_ci_dag_runner_log_dir=str(artifacts / "safe-ci-dag-runner"),
            )
            handle_path.write_text(json.dumps(handle) + "\n")

        with self.assertRaisesRegex(
            correction.CorrectionRefused, "no matching owner result"
        ):
            correction.apply_plan(
                self.state_root,
                self.owner_state,
                expected_count=1,
                expected_plan_sha256=str(dry["plan_sha256"]),
                run=absent_unit,
                before_commit=add_unmatched_handle,
            )
        self.assertEqual(before, self.handle_path.read_bytes())

    def test_symlink_swap_before_mutation_does_not_touch_escape_target(self) -> None:
        dry = self.plan()
        outside = Path(self.temporary.name) / "outside-handle.json"
        outside.write_bytes(self.handle_path.read_bytes())

        def swap_handle_for_symlink() -> None:
            self.handle_path.unlink()
            self.handle_path.symlink_to(outside)

        with self.assertRaises(correction.CorrectionRefused):
            correction.apply_plan(
                self.state_root,
                self.owner_state,
                expected_count=1,
                expected_plan_sha256=str(dry["plan_sha256"]),
                run=absent_unit,
                before_commit=swap_handle_for_symlink,
            )
        self.assertEqual(self.handle_bytes, outside.read_bytes())
        self.assertTrue(self.handle_path.is_symlink())

    def test_mid_batch_replace_failure_restores_exact_originals(self) -> None:
        second = self.add_candidate("20260830T123500123456Z-4646")
        dry = correction.build_plan(
            self.state_root,
            self.owner_state,
            expected_count=2,
            run=absent_unit,
        )
        originals = {
            self.handle_path: self.handle_path.read_bytes(),
            second: second.read_bytes(),
        }
        new_replacements = 0

        def fail_second_new(
            source_descriptor: int,
            source_name: str,
            destination_descriptor: int,
            destination_name: str,
        ) -> None:
            nonlocal new_replacements
            if source_name.startswith("replacement-"):
                new_replacements += 1
                if new_replacements == 2:
                    raise OSError("injected second replacement failure")
            correction.replace_child(
                source_descriptor,
                source_name,
                destination_descriptor,
                destination_name,
            )

        with self.assertRaisesRegex(
            correction.CorrectionRefused, "exact originals restored"
        ):
            correction.apply_plan(
                self.state_root,
                self.owner_state,
                expected_count=2,
                expected_plan_sha256=str(dry["plan_sha256"]),
                run=absent_unit,
                replace=fail_second_new,
            )
        for path, original in originals.items():
            self.assertEqual(original, path.read_bytes())
        self.assertFalse(
            correction.transaction_path(
                correction.load_roots(self.state_root, self.owner_state)
            ).exists()
        )

    def test_keyboard_interrupt_restores_exact_originals_and_clears_marker(self) -> None:
        second = self.add_candidate("20260830T123501123456Z-4747")
        dry = correction.build_plan(
            self.state_root,
            self.owner_state,
            expected_count=2,
            run=absent_unit,
        )
        originals = {
            self.handle_path: self.handle_path.read_bytes(),
            second: second.read_bytes(),
        }
        replacements = 0

        def interrupt_after_first_replacement(
            source_descriptor: int,
            source_name: str,
            destination_descriptor: int,
            destination_name: str,
        ) -> None:
            nonlocal replacements
            correction.replace_child(
                source_descriptor,
                source_name,
                destination_descriptor,
                destination_name,
            )
            if source_name.startswith("replacement-"):
                replacements += 1
                if replacements == 1:
                    raise KeyboardInterrupt("injected interrupt")

        with self.assertRaises(KeyboardInterrupt):
            correction.apply_plan(
                self.state_root,
                self.owner_state,
                expected_count=2,
                expected_plan_sha256=str(dry["plan_sha256"]),
                run=absent_unit,
                replace=interrupt_after_first_replacement,
            )
        for path, original in originals.items():
            self.assertEqual(original, path.read_bytes())
        self.assertFalse(
            correction.transaction_path(
                correction.load_roots(self.state_root, self.owner_state)
            ).exists()
        )

    @unittest.skipUnless(hasattr(os, "fork"), "requires process fork")
    def test_process_exit_after_first_replacement_recovers_on_next_invocation(self) -> None:
        second = self.add_candidate("20260830T123502123456Z-4848")
        dry = correction.build_plan(
            self.state_root,
            self.owner_state,
            expected_count=2,
            run=absent_unit,
        )
        originals = {
            self.handle_path: self.handle_path.read_bytes(),
            second: second.read_bytes(),
        }
        pid = os.fork()
        if pid == 0:
            def exit_after_first_replacement(
                source_descriptor: int,
                source_name: str,
                destination_descriptor: int,
                destination_name: str,
            ) -> None:
                correction.replace_child(
                    source_descriptor,
                    source_name,
                    destination_descriptor,
                    destination_name,
                )
                if source_name.startswith("replacement-"):
                    os.fsync(destination_descriptor)
                    os._exit(91)

            correction.apply_plan(
                self.state_root,
                self.owner_state,
                expected_count=2,
                expected_plan_sha256=str(dry["plan_sha256"]),
                run=absent_unit,
                replace=exit_after_first_replacement,
            )
            os._exit(92)
        waited, status = os.waitpid(pid, 0)
        self.assertEqual(pid, waited)
        self.assertEqual(91, os.waitstatus_to_exitcode(status))
        roots = correction.load_roots(self.state_root, self.owner_state)
        marker = correction.transaction_path(roots)
        self.assertTrue(marker.is_dir())
        manifest = json.loads(
            (marker / correction.TRANSACTION_MANIFEST).read_text()
        )
        self.assertEqual(2, manifest["plan"]["pending_count"])
        self.assertEqual(2, len(manifest["records"]))
        for item in manifest["records"]:
            original = correction.decoded(
                item, "original_bytes_base64", item["original_sha256"]
            )
            replacement = correction.decoded(
                item, "replacement_bytes_base64", item["replacement_sha256"]
            )
            self.assertNotEqual(original, replacement)
        self.assertNotEqual(originals[self.handle_path], self.handle_path.read_bytes())
        with self.assertRaisesRegex(RuntimeError, "recovery is pending"):
            correction.run_registry.read_record(self.handle_path)

        recovered = correction.build_plan(
            self.state_root,
            self.owner_state,
            expected_count=2,
            run=absent_unit,
        )

        self.assertEqual(2, recovered["pending_count"])
        for path, original in originals.items():
            self.assertEqual(original, path.read_bytes())
        self.assertFalse(marker.exists())

    def test_rollback_failure_retains_evidence_for_fresh_recovery(self) -> None:
        second = self.add_candidate("20260830T123503123456Z-4949")
        dry = correction.build_plan(
            self.state_root,
            self.owner_state,
            expected_count=2,
            run=absent_unit,
        )
        originals = {
            self.handle_path: self.handle_path.read_bytes(),
            second: second.read_bytes(),
        }
        replacements = 0

        def fail_mutation_and_rollback(
            source_descriptor: int,
            source_name: str,
            destination_descriptor: int,
            destination_name: str,
        ) -> None:
            nonlocal replacements
            if source_name.startswith("replacement-"):
                replacements += 1
                if replacements == 2:
                    raise OSError("injected second replacement failure")
            if source_name.startswith("original-"):
                raise OSError("injected rollback failure")
            correction.replace_child(
                source_descriptor,
                source_name,
                destination_descriptor,
                destination_name,
            )

        with self.assertRaisesRegex(
            correction.CorrectionRefused, "ROLLBACK FAILED.*evidence retained"
        ):
            correction.apply_plan(
                self.state_root,
                self.owner_state,
                expected_count=2,
                expected_plan_sha256=str(dry["plan_sha256"]),
                run=absent_unit,
                replace=fail_mutation_and_rollback,
            )
        roots = correction.load_roots(self.state_root, self.owner_state)
        marker = correction.transaction_path(roots)
        self.assertTrue((marker / correction.TRANSACTION_MANIFEST).is_file())
        self.assertTrue((marker / "original-0.json").is_file())

        recovered = correction.build_plan(
            self.state_root,
            self.owner_state,
            expected_count=2,
            run=absent_unit,
        )

        self.assertEqual(2, recovered["pending_count"])
        for path, original in originals.items():
            self.assertEqual(original, path.read_bytes())
        self.assertFalse(marker.exists())

    def test_every_execution_and_identity_guard_refuses_without_mutation(self) -> None:
        cases = {
            "target mismatch": lambda result, handle: handle.update(target="b" * 40),
            "repository mismatch": lambda result, handle: handle.update(
                repo="rrnewton/reverie"
            ),
            "log mismatch": lambda result, handle: handle.update(log="/tmp/other.log"),
            "recorded process": lambda result, handle: handle.update(
                process_identity={"pid": 1, "start_ticks": 2, "boot_id": "boot"}
            ),
            "recorded admission": lambda result, handle: handle.update(
                admission_result={
                    "state": "admitted",
                    "recorded_at": "2026-08-30T12:34:57+00:00",
                    "run_number": 1,
                }
            ),
            "executed count": lambda result, handle: handle.update(executed_nodes=1),
            "wrong rc": lambda result, handle: result.update(validate_run_rc=3),
        }
        for name, mutate in cases.items():
            with self.subTest(name=name):
                original_result = json.loads(self.result_path.read_text())
                original_handle = json.loads(self.handle_path.read_text())
                mutate(original_result, original_handle)
                self.result_path.write_text(json.dumps(original_result) + "\n")
                self.handle_path.write_text(json.dumps(original_handle) + "\n")
                before = self.handle_path.read_bytes()
                with self.assertRaises(correction.CorrectionRefused):
                    self.plan()
                self.assertEqual(before, self.handle_path.read_bytes())
                self.result_path.write_bytes(self.result_bytes)
                self.handle_path.write_bytes(self.handle_bytes)

    def test_legacy_top_level_run_number_does_not_define_admission(self) -> None:
        handle = json.loads(self.handle_path.read_text())
        handle["run_number"] = 1
        self.handle_path.write_text(json.dumps(handle) + "\n")

        plan = self.plan()
        correction.apply_plan(
            self.state_root,
            self.owner_state,
            expected_count=1,
            expected_plan_sha256=str(plan["plan_sha256"]),
            run=absent_unit,
        )
        corrected = correction.run_registry.read_record(self.handle_path)

        self.assertNotIn("run_number", corrected)
        self.assertEqual("refused", corrected["state"])

    def test_log_execution_evidence_guards_refuse_without_mutation(self) -> None:
        original_log = self.log.read_text()
        cases = {
            "duplicate refusal": original_log
            + f"validate-run: REFUSED: validation log already exists: {self.log}\n",
            "lock acquired": original_log
            + f"validate-lock: ACQUIRED by owner-checkout-watch running validate {TARGET}\n",
            "DAG terminal": original_log + "[check.example] PASS fixture\n",
            "missing standard state": original_log.replace(
                correction.REFUSED_STATE_LINE + "\n", ""
            ),
        }
        for name, content in cases.items():
            with self.subTest(name=name):
                self.log.write_text(content)
                before = self.handle_path.read_bytes()
                with self.assertRaises(correction.CorrectionRefused):
                    self.plan()
                self.assertEqual(before, self.handle_path.read_bytes())
                self.log.write_text(original_log)

    def test_unmatched_new_unexecuted_handle_refuses_apply(self) -> None:
        dry = self.plan()
        second_attempt = "20260830T123458123456Z-4444"
        unit, handle_path, log, _service, artifacts = correction.expected_paths(
            self.state_root, self.owner_state, second_attempt
        )
        handle = dict(self.handle)
        handle.update(
            unit=f"{unit}.service",
            log=str(log),
            e2e_result_root=str(artifacts / "e2e"),
            safe_ci_dag_runner_log_dir=str(artifacts / "safe-ci-dag-runner"),
        )
        handle_path.write_text(json.dumps(handle) + "\n")
        before = self.handle_path.read_bytes()

        with self.assertRaisesRegex(
            correction.CorrectionRefused, "has no matching owner result"
        ):
            correction.apply_plan(
                self.state_root,
                self.owner_state,
                expected_count=1,
                expected_plan_sha256=str(dry["plan_sha256"]),
                run=absent_unit,
            )
        self.assertEqual(before, self.handle_path.read_bytes())

    def test_symlinked_log_or_invalid_target_refuses(self) -> None:
        original_log = self.log.read_text()
        self.log.unlink()
        target = self.owner_state / "other.log"
        target.write_text(original_log)
        self.log.symlink_to(target)
        with self.assertRaises(correction.CorrectionRefused):
            self.plan()
        self.log.unlink()
        self.log.write_text(original_log)

        result = json.loads(self.result_path.read_text())
        result["sha"] = "not-a-commit"
        self.result_path.write_text(json.dumps(result) + "\n")
        with self.assertRaises(correction.CorrectionRefused):
            self.plan()

    def test_symlinked_results_or_runs_root_refuses_escape(self) -> None:
        for name, directory in (
            ("results", self.owner_state / "results"),
            ("runs", self.handle_path.parent),
        ):
            with self.subTest(name=name):
                outside = Path(self.temporary.name) / f"outside-{name}"
                directory.rename(outside)
                directory.symlink_to(outside, target_is_directory=True)
                with self.assertRaisesRegex(correction.CorrectionRefused, "not a real directory"):
                    self.plan()
                directory.unlink()
                outside.rename(directory)

    def test_symlinked_transaction_marker_refuses_recovery(self) -> None:
        roots = correction.load_roots(self.state_root, self.owner_state)
        outside = Path(self.temporary.name) / "outside-transaction"
        outside.mkdir()
        correction.transaction_path(roots).symlink_to(
            outside, target_is_directory=True
        )

        with self.assertRaisesRegex(correction.CorrectionRefused, "not a real directory"):
            self.plan()

    def test_symlinked_result_or_handle_refuses_escape(self) -> None:
        for name, path in (
            ("result", self.result_path),
            ("handle", self.handle_path),
        ):
            with self.subTest(name=name):
                outside = Path(self.temporary.name) / f"outside-{name}.json"
                path.rename(outside)
                path.symlink_to(outside)
                with self.assertRaises(correction.CorrectionRefused):
                    self.plan()
                path.unlink()
                outside.rename(path)

    def test_service_artifact_or_unit_presence_refuses(self) -> None:
        _unit, _handle, _log, service_result, artifacts = correction.expected_paths(
            self.state_root, self.owner_state, ATTEMPT
        )
        for name, plant, runner in (
            (
                "service result",
                lambda: service_result.write_text("{}\n"),
                absent_unit,
            ),
            (
                "artifacts",
                lambda: artifacts.mkdir(parents=True),
                absent_unit,
            ),
            (
                "systemd unit",
                lambda: None,
                lambda command, **kwargs: subprocess.CompletedProcess(
                    command, 0, "LoadState=loaded\nInvocationID=live\n", ""
                ),
            ),
        ):
            with self.subTest(name=name):
                plant()
                before = self.handle_path.read_bytes()
                with self.assertRaises(correction.CorrectionRefused):
                    correction.build_plan(
                        self.state_root,
                        self.owner_state,
                        expected_count=1,
                        run=runner,
                    )
                self.assertEqual(before, self.handle_path.read_bytes())
                if service_result.exists():
                    service_result.unlink()
                if artifacts.exists():
                    artifacts.rmdir()


if __name__ == "__main__":
    unittest.main()
