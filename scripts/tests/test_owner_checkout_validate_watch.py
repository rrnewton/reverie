#!/usr/bin/env python3
"""Fail-closed brackets for the owner's canonical-checkout watch."""

from __future__ import annotations

import importlib.util
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "owner_checkout_validate_watch",
    ROOT / "scripts/owner-checkout-validate-watch.py",
)
assert SPEC is not None and SPEC.loader is not None
watch = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(watch)


def command(repo: Path, *argv: str) -> str:
    completed = subprocess.run(
        list(argv), cwd=repo, check=True, text=True, capture_output=True
    )
    return completed.stdout


def clean_repository(root: Path) -> Path:
    repo = root / "dev-hermit" / "hermit"
    repo.mkdir(parents=True)
    command(repo, "git", "init", "-q", "-b", "main")
    command(repo, "git", "config", "user.name", "Watch Test")
    command(repo, "git", "config", "user.email", "watch@example.invalid")
    (repo / "tracked.txt").write_text("before\n", encoding="utf-8")
    (repo / "Makefile").write_text(
        ".PHONY: validate other\\\n\n"
        "validate:\n"
        "\t./scripts/validate.rs $(ARGS)\n",
        encoding="utf-8",
    )
    command(repo, "git", "add", "tracked.txt", "Makefile")
    command(repo, "git", "commit", "-q", "-m", "fixture")
    head = command(repo, "git", "rev-parse", "HEAD").strip()
    command(repo, "git", "update-ref", "refs/remotes/origin/main", head)
    return repo


def advance_origin_main(repo: Path, count: int) -> str:
    """Advance only the remote-tracking ref, leaving the checkout untouched."""
    parent = command(repo, "git", "rev-parse", "refs/remotes/origin/main").strip()
    tree = command(repo, "git", "rev-parse", f"{parent}^{{tree}}").strip()
    for index in range(count):
        parent = command(
            repo,
            "git",
            "commit-tree",
            tree,
            "-p",
            parent,
            "-m",
            f"remote fixture {index + 1}",
        ).strip()
    command(repo, "git", "update-ref", "refs/remotes/origin/main", parent)
    return parent


class OwnerCheckoutValidateWatchTest(unittest.TestCase):
    def test_dirty_checkout_is_a_durable_cannot_start_without_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            marker = repo / "owner-untracked.txt"
            marker.write_text("must survive\n", encoding="utf-8")
            before = watch.tree_state(repo)

            rc = watch.main(
                [
                    "--checkout", str(repo),
                    "--state-dir", str(state),
                    "--no-fetch",
                    "--no-notify",
                ]
            )

            self.assertEqual(rc, watch.CANNOT_START)
            self.assertEqual(marker.read_text(encoding="utf-8"), "must survive\n")
            self.assertEqual(watch.tree_state(repo), before)
            results = list((state / "results").glob("*.json"))
            self.assertEqual(len(results), 1)
            result = json.loads(results[0].read_text(encoding="utf-8"))
            self.assertEqual(result["state"], "CANNOT_START")
            self.assertIn("dirty before launch", result["reason"])
            self.assertIsNone(result["executed_tests"])
            events = [
                json.loads(line)["event"]
                for line in (state / "invocations.jsonl").read_text().splitlines()
            ]
            self.assertEqual(events, ["attempt-start", "attempt-finish"])

    def test_clean_dry_run_is_no_result_never_pass(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            before = watch.tree_state(repo)

            rc = watch.main(
                [
                    "--checkout", str(repo),
                    "--state-dir", str(state),
                    "--no-fetch",
                    "--no-notify",
                    "--dry-run",
                ]
            )

            self.assertEqual(rc, watch.NO_RESULT)
            self.assertEqual(watch.tree_state(repo), before)
            result_path = next((state / "results").glob("*.json"))
            result = json.loads(result_path.read_text(encoding="utf-8"))
            self.assertEqual(result["state"], "NO_RESULT")
            self.assertIsNone(result["executed_tests"])
            self.assertEqual(result["dirty_paths_after"], [])
            self.assertEqual(result["pre_tree_hash"], result["post_tree_hash"])
            self.assertFalse(result["known_dirty_exception"]["applied"])
            self.assertEqual(result["checkout_relation"], "current")
            self.assertEqual(result["checkout_behind"], 0)
            self.assertEqual(result["checkout_ahead"], 0)
            self.assertIn("current main", result["verdict_qualifier"])

    def test_stale_main_runs_and_discloses_exact_distance_without_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            head = command(repo, "git", "rev-parse", "HEAD").strip()
            origin = advance_origin_main(repo, 2)
            before = watch.tree_state(repo)
            row = {
                "commit": head,
                "cwd": str(repo),
                "result": "pass",
                "executed_tests": 862,
                "coverage": {"satisfied": True},
            }

            with mock.patch.object(
                watch,
                "run_validate_observing_admission",
                return_value=subprocess.CompletedProcess([], 0, "", ""),
            ) as launched, mock.patch.object(
                watch, "canonical_ledger_rows", return_value=[row]
            ), mock.patch.object(
                watch, "latest_attempt_row", return_value=row
            ), mock.patch.object(
                watch, "qualify_row", return_value=(True, "fixture qualifies")
            ):
                rc = watch.main(
                    [
                        "--checkout", str(repo),
                        "--state-dir", str(state),
                        "--no-fetch",
                        "--no-notify",
                    ]
                )

            self.assertEqual(rc, watch.PASS)
            launched.assert_called_once()
            self.assertEqual(watch.tree_state(repo), before)
            result = json.loads(
                next((state / "results").glob("*.json")).read_text()
            )
            self.assertEqual(result["state"], "PASS")
            self.assertEqual(result["sha"], head)
            self.assertEqual(result["origin_main_sha"], origin)
            self.assertEqual(result["checkout_relation"], "stale")
            self.assertEqual(result["checkout_behind"], 2)
            self.assertEqual(result["checkout_ahead"], 0)
            self.assertIn("does not describe current main", result["verdict_qualifier"])
            self.assertIn("2 commit(s) behind", result["reason"])
            self.assertFalse(result["landing_authority"])
            self.assertIn("withheld", result["landing_authority_reason"])

    def test_current_main_runs_and_can_carry_exact_head_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            head = command(repo, "git", "rev-parse", "HEAD").strip()
            row = {
                "commit": head,
                "cwd": str(repo),
                "result": "pass",
                "executed_tests": 862,
                "coverage": {"satisfied": True},
            }

            with mock.patch.object(
                watch,
                "run_validate_observing_admission",
                return_value=subprocess.CompletedProcess([], 0, "", ""),
            ) as launched, mock.patch.object(
                watch, "canonical_ledger_rows", return_value=[row]
            ), mock.patch.object(
                watch, "latest_attempt_row", return_value=row
            ), mock.patch.object(
                watch, "qualify_row", return_value=(True, "fixture qualifies")
            ):
                rc = watch.main(
                    [
                        "--checkout", str(repo),
                        "--state-dir", str(state),
                        "--no-fetch",
                        "--no-notify",
                    ]
                )

            self.assertEqual(rc, watch.PASS)
            launched.assert_called_once()
            result = json.loads(
                next((state / "results").glob("*.json")).read_text()
            )
            self.assertEqual(result["checkout_relation"], "current")
            self.assertEqual(result["checkout_behind"], 0)
            self.assertEqual(result["checkout_ahead"], 0)
            self.assertTrue(result["landing_authority"])
            self.assertIn("current main", result["verdict_qualifier"])

    def test_exact_known_symlink_runs_policy_and_discloses_exception(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            (repo / "validate").symlink_to("scripts/validate.rs")
            before = watch.tree_state(repo)

            rc = watch.main(
                [
                    "--checkout", str(repo),
                    "--state-dir", str(state),
                    "--no-fetch",
                    "--no-notify",
                    "--dry-run",
                ]
            )

            self.assertEqual(rc, watch.NO_RESULT)
            self.assertEqual(watch.tree_state(repo), before)
            result = json.loads(
                next((state / "results").glob("*.json")).read_text()
            )
            exception = result["known_dirty_exception"]
            self.assertTrue(exception["applied"])
            self.assertEqual(exception["paths"], ["validate"])
            self.assertEqual(exception["observed_symlink_target"], "scripts/validate.rs")
            self.assertFalse(exception["could_affect_make_validate"])
            self.assertEqual(result["dirty_paths_after"], ["?? validate"])
            self.assertEqual(result["pre_tree_hash"], result["post_tree_hash"])

    def test_modified_tracked_file_refuses_even_with_known_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            (repo / "validate").symlink_to("scripts/validate.rs")
            (repo / "tracked.txt").write_text("modified\n")

            rc = watch.main(
                [
                    "--checkout", str(repo),
                    "--state-dir", str(state),
                    "--no-fetch",
                    "--no-notify",
                ]
            )

            self.assertEqual(rc, watch.CANNOT_START)
            result = json.loads(
                next((state / "results").glob("*.json")).read_text()
            )
            self.assertFalse(result["known_dirty_exception"]["applied"])
            self.assertIn(" M tracked.txt", result["dirty_paths_before"])

    def test_wrong_branch_refuses_even_when_tree_is_pristine(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            command(repo, "git", "checkout", "-q", "-b", "feature")

            rc = watch.main(
                [
                    "--checkout", str(repo),
                    "--state-dir", str(state),
                    "--no-fetch",
                    "--no-notify",
                ]
            )

            self.assertEqual(rc, watch.CANNOT_START)
            result = json.loads(
                next((state / "results").glob("*.json")).read_text()
            )
            self.assertIn("expected 'main'", result["reason"])

    def test_notification_failure_is_a_distinct_loud_exit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            (repo / "owner-untracked.txt").write_text("must survive\n")

            with mock.patch.object(watch, "notify", return_value=(1, "wake refused")):
                rc = watch.main(
                    [
                        "--checkout", str(repo),
                        "--state-dir", str(state),
                        "--no-fetch",
                    ]
                )

            self.assertEqual(rc, watch.NOTIFY_FAILED)
            result = json.loads(
                next((state / "results").glob("*.json")).read_text()
            )
            self.assertEqual(result["state"], "CANNOT_START")
            self.assertEqual(result["notification_rc"], 1)
            self.assertEqual(result["notification_detail"], "wake refused")

    def test_systemd_does_not_launder_notification_failure_as_success(self) -> None:
        service = (
            ROOT / "scripts/systemd/hermit-owner-checkout-validate.service"
        ).read_text()
        self.assertIn("SuccessExitStatus=0 1 2 3 4 6\n", service)
        self.assertNotIn("SuccessExitStatus=0 1 2 3 4 5", service)

    def test_lock_quarantine_is_distinct_from_product_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "payload quarantine: ACTIVE: payload identities remain active\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=1.1s exit=3\n"
            )
            evidence = watch.lock_block_evidence(log)
            self.assertIsNotNone(evidence)
            self.assertEqual(evidence["validate_lock_exit"], 3)
            self.assertIn("before product work", evidence["interpretation"])

    def test_exit_three_without_quarantine_is_not_called_lock_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text("# ci-hub/validate-lock tool COST ACTUAL exit=3\n")
            self.assertIsNone(watch.lock_block_evidence(log))

    def test_box_exclusive_anchor_refusal_is_lock_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "REFUSED: box-exclusive anchor held by pid=123 host=fixture\n"
                "# ci-hub/validate-lock tool COST ACTUAL exit=3\n"
            )
            evidence = watch.lock_block_evidence(log)
            self.assertIsNotNone(evidence)
            self.assertEqual(evidence["kind"], "validate-lock-refusal")
            self.assertEqual(evidence["refusal_pid"], 123)

    def test_acquired_marker_binds_agent_kind_and_exact_target(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            target = "a" * 40
            log.write_text(
                "validate-lock: ACQUIRED by another-agent running validate "
                + target
                + " (lease 1200s)\n"
                "validate-lock: ACQUIRED by owner-checkout-watch running benchmark "
                + target
                + " (lease 1200s)\n"
                "validate-lock: ACQUIRED by owner-checkout-watch running validate "
                + ("b" * 40)
                + " (lease 1200s)\n"
                "validate-lock: ACQUIRED by owner-checkout-watch running validate "
                + target
                + " (lease 1200s)\n"
            )

            self.assertIn(target, watch.acquired_line(log, target=target))
            self.assertIsNone(watch.acquired_line(log, target="c" * 40))

    def test_admission_status_brackets_admitted_not_admitted_and_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            state = Path(temporary)

            rc, status = watch.admission_status_since(state, "100")
            self.assertEqual((rc, status["state"]), (2, "UNKNOWN"))

            with mock.patch.object(watch, "epoch_millis", return_value=200_000):
                watch.ensure_admission_cursor(state)
            rc, status = watch.admission_status_since(state, "200")
            self.assertEqual((rc, status["state"]), (1, "NOT_ADMITTED"))

            result = {
                "attempt_id": "fixture-attempt",
                "result_json": str(state / "results/fixture.json"),
            }
            with mock.patch.object(watch, "epoch_millis", return_value=201_250), mock.patch.object(
                watch, "utc_now", return_value="1970-01-01T00:03:21.250Z"
            ):
                admission = watch.record_admission(
                    state,
                    result,
                    target="d" * 40,
                    evidence_line=(
                        "validate-lock: ACQUIRED by owner-checkout-watch running validate "
                        + "d" * 40
                        + " (lease 1200s)"
                    ),
                )
            self.assertEqual(admission["state"], "ADMITTED")
            rc, status = watch.admission_status_since(state, "200")
            self.assertEqual((rc, status["state"]), (0, "ADMITTED"))
            self.assertEqual(status["admission"]["attempt_id"], "fixture-attempt")
            rc, status = watch.admission_status_since(state, "202")
            self.assertEqual((rc, status["state"]), (1, "NOT_ADMITTED"))

    def test_boundary_before_observation_is_unknown_not_permission(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            state = Path(temporary)
            with mock.patch.object(watch, "epoch_millis", return_value=200_000):
                watch.ensure_admission_cursor(state)
            rc, status = watch.admission_status_since(state, "199")
            self.assertEqual((rc, status["state"]), (2, "UNKNOWN"))
            self.assertFalse(status["admitted"])

    def test_running_producer_publishes_admission_before_verdict(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            state = Path(temporary) / "state"
            log = state / "logs/attempt.log"
            log.parent.mkdir(parents=True)
            target = "e" * 40
            result_path = state / "results/attempt.json"
            result = {
                "schema_version": 1,
                "attempt_id": "attempt",
                "result_json": str(result_path),
                "state": "CANNOT_START",
                "lock_admission": {"state": "UNKNOWN"},
            }
            marker = (
                "validate-lock: ACQUIRED by owner-checkout-watch running validate "
                + target
                + " (lease 1200s)"
            )

            completed = watch.run_validate_observing_admission(
                ["bash", "-c", f"printf '%s\\n' '{marker}'; sleep 0.4"],
                cwd=Path(temporary),
                log=log,
                state_dir=state,
                result=result,
                target=target,
            )

            self.assertEqual(completed.returncode, 0)
            in_flight = json.loads(result_path.read_text())
            self.assertEqual(in_flight["lock_admission"]["state"], "ADMITTED")
            self.assertEqual(in_flight["lock_admission"]["target"], target)
            boundary = in_flight["lock_admission"]["admitted_at_epoch_ms"] / 1000 - 1
            rc, status = watch.admission_status_since(state, str(boundary))
            self.assertEqual((rc, status["state"]), (0, "ADMITTED"))

    def test_invalid_boundary_is_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            rc, status = watch.admission_status_since(Path(temporary), "infinity")
            self.assertEqual((rc, status["state"]), (2, "UNKNOWN"))

    def test_launcher_capture_cannot_overwrite_concurrent_lock_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            with watch.append_launcher_output(log) as launcher:
                with log.open("a", encoding="utf-8") as service:
                    service.write(
                        "REFUSED: box-exclusive anchor held by pid=123 host=fixture\n"
                        "# ci-hub/validate-lock tool COST ACTUAL exit=3\n"
                    )
                launcher.write("validate-run: FINISHED fixture.service\n")

            text = log.read_text(encoding="utf-8")
            self.assertTrue(text.startswith("REFUSED: box-exclusive anchor held"))
            self.assertIn("validate-run: FINISHED", text)
            self.assertIsNotNone(watch.lock_block_evidence(log))

    def test_lock_holder_attribution_binds_live_identity_and_reserved_pr(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            pid = 123
            ticks = 456
            boot_id = "boot-fixture"
            target = "d" * 40
            unit = "validate-helper-fixture.service"
            (parent / ".validate-lock").write_text(
                f"agent=helper-validator2\nkind=validate\ntarget={target}\n"
            )
            (parent / ".validate-lock.owner").write_text(
                f"host=fixture\nboot_id={boot_id}\npid={pid}\nstart_ticks={ticks}\n"
            )
            (parent / ".validate-lock.cleanup-required").write_text(
                "version=3\nagent=helper-validator2\n"
                f"operation=validate:{target}\n"
                f"cgroup=/fixture/{unit}\nphase=published\n"
            )
            runs = parent / "ignored/validate/runs"
            runs.mkdir(parents=True)
            (runs / "run.json").write_text(
                json.dumps(
                    {
                        "agent": "helper-validator2",
                        "target": target,
                        "unit": unit,
                        "pr": 2012,
                        "state": "running",
                    }
                )
            )
            proc_root = parent / "proc"
            (proc_root / str(pid)).mkdir(parents=True)
            fields_after_comm = ["S", *(["0"] * 18), str(ticks), "0"]
            (proc_root / str(pid) / "stat").write_text(
                f"{pid} (fixture) " + " ".join(fields_after_comm) + "\n"
            )
            boot = parent / "boot_id"
            boot.write_text(boot_id + "\n")

            holder = watch.lock_holder_attribution(
                parent,
                {"refusal_pid": pid},
                proc_root=proc_root,
                boot_id_path=boot,
            )

            self.assertEqual(holder["status"], "attributed")
            self.assertEqual(holder["pr"], 2012)
            self.assertEqual(holder["contention_class"], "reserved-publication-window")
            self.assertTrue(holder["process_identity"]["live_match"])

            record_path = runs / "run.json"
            ordinary = json.loads(record_path.read_text())
            ordinary["pr"] = 2041
            record_path.write_text(json.dumps(ordinary))
            holder = watch.lock_holder_attribution(
                parent,
                {"refusal_pid": pid},
                proc_root=proc_root,
                boot_id_path=boot,
            )
            self.assertEqual(holder["contention_class"], "ordinary-contention")
            self.assertIn("not the reserved", holder["interpretation"])

    def test_lock_holder_attribution_refuses_pid_only_guess(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            holder = watch.lock_holder_attribution(
                Path(temporary), {"refusal_pid": 123}
            )
            self.assertEqual(holder["status"], "unattributed")

    def test_privileged_node_records_prior_empty_detail_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "[privileged-build.privileged_tests] ✗ FAIL   Build focused binaries (0s, exit 1)\n"
                "[privileged-build.privileged_tests] ----- detail -----\n"
                "[privileged-build.privileged_tests] ----- end detail -----\n"
            )
            evidence = watch.privileged_tests_evidence(log)
            self.assertEqual(evidence["status"], "FAIL")
            self.assertEqual(evidence["duration"], "0s")
            self.assertEqual(evidence["exit_code"], 1)
            self.assertTrue(evidence["detail_empty"])
            self.assertTrue(evidence["prior_refusal_signature"])
            self.assertIn("✗ FAIL", evidence["raw_failure_line"])

    def test_privileged_node_pass_carries_duration_and_no_failure_line(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "[privileged-build.privileged_tests] ✓ PASS   Build focused binaries (17s)\n"
            )
            evidence = watch.privileged_tests_evidence(log)
            self.assertEqual(evidence["status"], "PASS")
            self.assertEqual(evidence["duration"], "17s")
            self.assertNotIn("raw_failure_line", evidence)

    def test_relink_detector_captures_four_simultaneous_consumers_and_compile(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            binary = checkout / "target/debug/hermit"
            binary.parent.mkdir(parents=True)
            binary.write_text("fixture", encoding="utf-8")
            binary.chmod(0o755)
            launched = dt.datetime(2026, 8, 10, 7, 0, tzinfo=dt.timezone.utc)
            observed = launched + dt.timedelta(minutes=10)
            os.utime(
                binary, (launched.timestamp() + 300, launched.timestamp() + 300)
            )
            log = checkout / "validate.log"
            log.write_text(
                "[test.app_strict_verify]    Compiling hermit v0.2.0 (/fixture/hermit-cli)\n"
                f"[e2e.manifest_chaos_c] test_harness.sh: Hermit binary is not executable: {binary}\n"
                f"[e2e.manifest_debugger_c] exec format error: {binary}\n"
                f"[privileged-e2e.manifest_backend_parity_c] Text file busy: {binary}\n"
                f"[e2e.manifest_applications] No such file or directory: {binary}\n"
                "[test.app_strict_verify]     Finished `test` profile [unoptimized] target(s) in 5.92s\n",
                encoding="utf-8",
            )

            evidence = watch.relink_race_recurrence_evidence(
                log, checkout, launched_at=launched, observed_at=observed
            )

            self.assertTrue(evidence["triggered"])
            self.assertEqual(evidence["suspect_count"], 4)
            self.assertEqual(evidence["simultaneous_consumer_count"], 4)
            self.assertEqual(evidence["simultaneous_line_range"], [2, 5])
            self.assertTrue(evidence["compile_in_flight_at_any_suspect"])
            self.assertTrue(
                all(item["compile_in_flight"] for item in evidence["suspects"])
            )
            self.assertTrue(evidence["binary_snapshot"]["exists_after_run"])
            self.assertTrue(evidence["binary_snapshot"]["executable_after_run"])
            self.assertEqual(
                evidence["binary_snapshot"]["mtime_relation_to_observed_run"],
                "inside-observed-run",
            )
            self.assertIn(
                "no-such-file-path-exists-afterwards",
                evidence["suspects"][3]["signatures"],
            )

    def test_relink_detector_captures_empty_detail_timeout(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            log = checkout / "validate.log"
            log.write_text(
                "[privileged-e2e.manifest_backend_parity_c] ▶ START  Privileged manifest bucket\n"
                "[privileged-e2e.manifest_backend_parity_c] ✗ FAIL   Privileged manifest bucket (120.03s, exit 124)\n"
                "[privileged-e2e.manifest_backend_parity_c] ----- detail -----\n"
                "[privileged-e2e.manifest_backend_parity_c] ----- end detail -----\n",
                encoding="utf-8",
            )
            now = dt.datetime(2026, 8, 10, 7, 0, tzinfo=dt.timezone.utc)

            evidence = watch.relink_race_recurrence_evidence(
                log, checkout, launched_at=now, observed_at=now
            )

            self.assertTrue(evidence["triggered"])
            self.assertEqual(
                evidence["suspects"][0]["signatures"],
                ["unexplained-empty-detail-timeout"],
            )
            self.assertEqual(evidence["suspects"][0]["detail_lines"], [])

    def test_relink_detector_does_not_launder_detailed_product_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            log = checkout / "validate.log"
            log.write_text(
                "[e2e.manifest_chaos_c] ✗ FAIL   Portable manifest bucket (17s, exit 1)\n"
                "[e2e.manifest_chaos_c] ----- detail -----\n"
                "[e2e.manifest_chaos_c] assertion failed: schedules differ\n"
                "[e2e.manifest_chaos_c] ----- end detail -----\n"
                "[test.app_strict_verify]    Compiling hermit v0.2.0 (/fixture/hermit-cli)\n",
                encoding="utf-8",
            )
            now = dt.datetime(2026, 8, 10, 7, 0, tzinfo=dt.timezone.utc)

            evidence = watch.relink_race_recurrence_evidence(
                log, checkout, launched_at=now, observed_at=now
            )

            self.assertFalse(evidence["triggered"])
            self.assertEqual(evidence["suspect_count"], 0)
            self.assertEqual(evidence["verdict_effect"], "none-evidence-only")

    def test_exact_ledger_failure_outranks_wrapper_success(self) -> None:
        source = (ROOT / "scripts/owner-checkout-validate-watch.py").read_text()
        ledger_clause = 'row is not None and row.get("result") == "fail"'
        wrapper_clause = "elif completed.returncode != 0:"
        self.assertIn(ledger_clause, source)
        self.assertLess(source.index(ledger_clause), source.index(wrapper_clause))

    def test_validation_side_effects_are_preserved_then_fully_restored(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            before = watch.tree_state(repo)
            (repo / "tracked.txt").write_text("after\n", encoding="utf-8")
            generated = repo / "build" / "generated.log"
            generated.parent.mkdir()
            generated.write_text("evidence\n", encoding="utf-8")
            recovery = root / "recovery"

            restored, after = watch.preserve_and_restore(repo, before, recovery)

            self.assertTrue(restored)
            self.assertEqual(after, before)
            self.assertEqual(command(repo, "git", "status", "--porcelain"), "")
            self.assertEqual(
                (recovery / "untracked/build/generated.log").read_text(),
                "evidence\n",
            )
            self.assertIn("+after", (recovery / "tracked.patch").read_text())

    def test_ledger_selection_requires_exact_checkout_sha_and_attempt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            sha = command(repo, "git", "rev-parse", "HEAD").strip()
            rows = [
                {
                    "commit": sha,
                    "cwd": str(repo),
                    "started_at": "2026-08-09T01:00:00Z",
                    "executed_tests": 99,
                },
                {
                    "commit": "0" * 40,
                    "cwd": str(repo),
                    "started_at": "2026-08-09T03:00:00Z",
                    "executed_tests": 88,
                },
                {
                    "commit": sha,
                    "cwd": str(root),
                    "started_at": "2026-08-09T03:00:00Z",
                    "executed_tests": 77,
                },
                {
                    "commit": sha,
                    "cwd": str(repo),
                    "started_at": "2026-08-09T03:00:01Z",
                    "finished_at": "2026-08-09T03:01:00Z",
                    "executed_tests": None,
                },
            ]
            launched = watch.parse_time("2026-08-09T03:00:00Z")
            assert launched is not None

            selected = watch.latest_attempt_row(
                rows, checkout=repo, sha=sha, launched_at=launched
            )

            self.assertIsNotNone(selected)
            self.assertIsNone(selected["executed_tests"])

    def test_canonical_rows_come_from_the_single_adapter(self) -> None:
        parent = Path("/fixture/dev-hermit")
        rows = [{"commit": "1" * 40}, {"commit": "2" * 40}]
        completed = subprocess.CompletedProcess(
            [], 0, stdout="".join(json.dumps(row) + "\n" for row in rows), stderr=""
        )
        with mock.patch.object(watch, "run", return_value=completed) as invoked:
            self.assertEqual(watch.canonical_ledger_rows(parent), rows)
        invoked.assert_called_once_with(
            [sys.executable, parent / "ci-hub/ledger/validate_rows.py", "rows"],
            cwd=parent,
        )


if __name__ == "__main__":
    unittest.main()
