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


def validated_status(sha: str) -> dict[str, object]:
    """An INERT canonical-authority answer: a report, not an authorization.

    It cannot validate anything or make any gate pass; it only stands in for what
    `ci-hub validate-status --json` would say about a SHA that already carries a
    qualifying receipt.
    """
    return {
        "sha": sha,
        "available": True,
        "verdict": "VALIDATED",
        "exit_code": 0,
        "reason": f"canonical validate-status verdict=VALIDATED for {sha}",
        "receipt": {
            "validated_at": "2026-08-11T03:15:07Z",
            "profile": "full",
            "selection_mode": "full",
            "coverage_satisfied": True,
            "coverage_status": "satisfied",
            "discovered_tests": 1223,
            "executed_tests": 873,
            "filtered_tests": 350,
            "failures": 0,
            "checks": 56,
            "log_file": "/fixture/validate.log",
            "receipt_digest": "0" * 64,
        },
    }


def unvalidated_status(sha: str) -> dict[str, object]:
    return {
        "sha": sha,
        "available": True,
        "verdict": "NOT-VALIDATED",
        "exit_code": 1,
        "reason": f"canonical validate-status verdict=NOT-VALIDATED for {sha}",
        "receipt": None,
    }


def readmission_refusal_log(log: Path, sha: str, verdict: str = "VALIDATED") -> None:
    """Write the exact durable line the de-duplication guard emits."""
    log.parent.mkdir(parents=True, exist_ok=True)
    log.write_text(
        "validate-run: REFUSED: canonical validate-status already has terminal "
        f"verdict {verdict} for unchanged SHA {sha}; refusing blind re-admission. "
        "Advance HEAD for a new product state.\n",
        encoding="utf-8",
    )


def admitted_result() -> dict[str, object]:
    return {"state": "admitted", "recorded_at": "2026-08-28T07:00:01+00:00"}


def refused_result(reason: str) -> dict[str, object]:
    return {
        "state": "refused",
        "recorded_at": "2026-08-28T07:00:01+00:00",
        "reason": reason,
        "exit_code": 3,
    }


def write_validation_record(
    path: Path,
    *,
    target: str,
    checkout: Path,
    log: Path,
    admission_result: dict[str, object] | None,
) -> None:
    root = checkout.parent.parent
    record = {
        "schema_version": 1,
        "kind": "validate",
        "state": "running",
        "unit": f"{path.stem}.service",
        "target": target,
        "repo": "rrnewton/hermit",
        "checkout": str(checkout),
        "source_checkout": str(checkout),
        "temporary_checkout": False,
        "cargo_home": str(root / "cargo"),
        "log": str(log),
        "agent": "owner-checkout-watch",
        "pr": None,
        "started_at": "2026-08-28T07:00:00+00:00",
        "parent_checkout_head": target,
        "producer": watch.run_registry.PRODUCER,
        "admission": "ci-hub validate-lock",
        "pane_role": "observer-only",
        "validate_lock_child_deadline_seconds": 1800,
        "e2e_result_root": str(root / "e2e"),
        "safe_ci_dag_runner_log_dir": str(root / "dag"),
        "hermit_run_timeout_seconds": 1620,
        **(
            {"admission_result": admission_result}
            if admission_result is not None
            else {}
        ),
    }
    watch.run_registry.write_record(path, record)


class ReadmissionRefusalClassificationTest(unittest.TestCase):
    """Typed admission and canonical status, independent of presentation text."""

    def _run(
        self,
        *,
        returncode: int,
        canonical: dict[str, object],
        write_log,
        row: dict[str, object] | None = None,
        qualified: tuple[bool, str] = (False, "no exact-attempt ledger row"),
        admission_result: dict[str, object] | None = None,
    ) -> tuple[int, dict, mock.Mock]:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            state = root / "state"
            head = command(repo, "git", "rev-parse", "HEAD").strip()

            def launch(
                command_argv, *, cwd, log, run_record, state_dir, result, target
            ):
                write_log(Path(log), head)
                write_validation_record(
                    Path(run_record),
                    target=head,
                    checkout=repo,
                    log=Path(log),
                    admission_result=admission_result,
                )
                return subprocess.CompletedProcess([], returncode, "", "")

            with mock.patch.object(
                watch, "run_validate_observing_admission", side_effect=launch
            ) as launched, mock.patch.object(
                watch, "canonical_ledger_rows", return_value=[]
            ), mock.patch.object(
                watch, "latest_attempt_row", return_value=row
            ), mock.patch.object(
                watch, "qualify_row", return_value=qualified
            ), mock.patch.object(
                watch, "canonical_validate_status", return_value=canonical(head)
            ):
                rc = watch.main(
                    [
                        "--checkout", str(repo),
                        "--state-dir", str(state),
                        "--no-fetch",
                        "--no-notify",
                    ]
                )
            result = json.loads(
                next((state / "results").glob("*.json")).read_text()
            )
            return rc, result, launched

    def test_unchanged_already_green_sha_reports_green_citing_the_prior_receipt(self) -> None:
        rc, result, launched = self._run(
            returncode=2,
            canonical=validated_status,
            write_log=lambda log, sha: readmission_refusal_log(log, sha),
        )

        self.assertEqual(rc, watch.PASS)
        self.assertEqual(result["state"], "PASS_ALREADY_VALIDATED")
        # It reports the existing green without promoting this attempt: the
        # count of executed tests for THIS attempt is honestly null.
        self.assertIsNone(result["executed_tests"])
        self.assertEqual(result["validate_run_rc"], 2)
        self.assertEqual(result["ledger_row_found"], False)
        # ...and it cites the receipt it is standing on.
        self.assertIn("GREEN", result["reason"])
        self.assertIn("validated_at=2026-08-11T03:15:07Z", result["reason"])
        self.assertIn("profile=full", result["reason"])
        self.assertIn("selection=full", result["reason"])
        self.assertIn("coverage_satisfied=True", result["reason"])
        self.assertIn("executed_tests=873", result["reason"])
        self.assertEqual(
            result["admission_refusal"]["kind"], "canonical-terminal-readmission"
        )
        self.assertEqual(
            result["commit_validation_state"],
            watch.COMMIT_VALIDATION_HARD_GREEN,
        )
        self.assertEqual(result["commit_validation_sha"], result["sha"])
        self.assertIn("VALIDATED", result["commit_validation_reason"])
        self.assertNotIn("landing_authority", result)
        launched.assert_called_once()

    def test_terminal_red_readmission_refusal_is_still_fail(self) -> None:
        """The SAME guard refuses a terminally RED SHA. That is not green."""
        rc, result, _ = self._run(
            returncode=2,
            canonical=lambda sha: {
                "sha": sha,
                "available": True,
                "verdict": "FAILED",
                "exit_code": 3,
                "reason": "canonical validate-status verdict=FAILED",
                "receipt": None,
            },
            write_log=lambda log, sha: readmission_refusal_log(log, sha, "FAILED"),
        )

        self.assertEqual(rc, watch.FAIL)
        self.assertEqual(result["state"], "FAIL")
        self.assertIn("FAILED", result["reason"])
        self.assertEqual(
            result["commit_validation_state"],
            watch.COMMIT_VALIDATION_NOT_HARD_GREEN,
        )

    def test_other_exit_two_refusal_is_unvalidated_not_green_and_not_fail(self) -> None:
        """A head that cannot be validated at all is a THIRD, distinct outcome."""
        rc, result, _ = self._run(
            returncode=2,
            canonical=unvalidated_status,
            write_log=lambda log, sha: log.write_text(
                "validate-run: REFUSED: validation admission refused target: "
                "head predates producer floor bfb0a9ef\n",
                encoding="utf-8",
            ),
            admission_result=refused_result("stale-base"),
        )

        self.assertEqual(rc, watch.NO_RESULT)
        self.assertEqual(result["state"], "NO_RESULT")
        self.assertIn("UNVALIDATED rather than failing", result["reason"])
        self.assertIn("stale-base", result["reason"])
        self.assertEqual(result["admission_refusal"]["kind"], "admission-refusal")
        self.assertNotIn("GREEN", result["reason"])
        self.assertEqual(
            result["commit_validation_state"],
            watch.COMMIT_VALIDATION_NOT_HARD_GREEN,
        )

    def test_post_receipt_nonzero_does_not_replace_canonical_green_verdict(self) -> None:
        rc, result, _ = self._run(
            returncode=75,
            canonical=validated_status,
            write_log=lambda log, _sha: log.write_text("post-receipt failure\n"),
            row={"result": "pass", "executed_tests": 862},
            qualified=(True, "fixture qualifies"),
        )

        self.assertEqual(rc, watch.FAIL)
        self.assertEqual(result["state"], "FAIL")
        self.assertIn("canonical validate-status is VALIDATED", result["reason"])
        self.assertIn("validate-run returned 75", result["reason"])
        self.assertIn("verdict remains VALIDATED", result["reason"])

    def test_queue_refusal_before_acquisition_is_blocked_not_fail(self) -> None:
        """An exact lock refusal cannot become a product failure via wrapper rc 2."""
        rc, result, _ = self._run(
            returncode=2,
            canonical=unvalidated_status,
            write_log=lambda log, sha: log.write_text(
                "validate-lock: waiting in declared-priority queue; live holder retained\n"
                "REFUSED: declared-priority queue did not grant "
                f"agent=owner-checkout-watch target={sha} within 3s\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=5.2s exit=3\n"
                "validate-run: CANONICAL-VERDICT-UNAVAILABLE: no receipt\n",
                encoding="utf-8",
            ),
            admission_result=refused_result("validation-lock-refusal"),
        )

        self.assertEqual(rc, watch.BLOCKED_ON_LOCK)
        self.assertEqual(result["state"], "BLOCKED_ON_LOCK")
        self.assertEqual(result["lock_admission"]["state"], "NOT_ADMITTED")
        self.assertIsNone(result["executed_tests"])
        self.assertFalse(result["ledger_row_found"])
        self.assertIn("before product work", result["reason"])
        self.assertNotIn("make validate exit=2", result["reason"])

    def test_exit_two_after_acquisition_remains_fail_without_a_row(self) -> None:
        """Do not turn a post-admission producer failure into lock contention."""
        rc, result, _ = self._run(
            returncode=2,
            canonical=unvalidated_status,
            write_log=lambda log, sha: log.write_text(
                "validate-lock: ACQUIRED by owner-checkout-watch running validate "
                f"{sha} (lease 1200s)\n"
                "REFUSED: producer setup failed after acquisition\n"
                "# ci-hub/validate-lock tool COST ACTUAL wall=5.2s exit=3\n",
                encoding="utf-8",
            ),
            admission_result=admitted_result(),
        )

        self.assertEqual(rc, watch.FAIL)
        self.assertEqual(result["state"], "FAIL")
        self.assertIsNone(result.get("lock_block_evidence"))
        self.assertIn("make validate exit=2", result["reason"])

    def test_historical_handle_does_not_promote_an_acquired_log_line(self) -> None:
        rc, result, _ = self._run(
            returncode=2,
            canonical=unvalidated_status,
            write_log=lambda log, sha: log.write_text(
                "validate-lock: ACQUIRED by owner-checkout-watch running validate "
                f"{sha} (lease 1200s)\n",
                encoding="utf-8",
            ),
        )

        self.assertEqual(watch.NO_RESULT, rc)
        self.assertEqual("NO_RESULT", result["state"])
        self.assertIn("admission is UNKNOWN", result["reason"])
        self.assertEqual("UNKNOWN", result["lock_admission"]["state"])

    def test_a_real_failing_node_is_still_fail(self) -> None:
        """A genuine product failure keeps reporting FAIL, counts and all."""
        rc, result, _ = self._run(
            returncode=2,
            canonical=unvalidated_status,
            write_log=lambda log, sha: log.write_text(
                "[test.strict_compat] [compat.yes] ✗ FAIL   "
                "L2 compatibility: yes (0s, exit 1)\n",
                encoding="utf-8",
            ),
            row={"result": "fail", "executed_tests": 814, "cwd": "", "commit": ""},
            admission_result=admitted_result(),
        )

        self.assertEqual(rc, watch.FAIL)
        self.assertEqual(result["state"], "FAIL")
        self.assertEqual(result["executed_tests"], 814)
        self.assertIn("result=fail", result["reason"])
        self.assertIsNone(result["admission_refusal"])
        self.assertEqual(
            result["commit_validation_state"],
            watch.COMMIT_VALIDATION_NOT_HARD_GREEN,
        )

    def test_readmission_refusal_the_authority_no_longer_backs_is_not_green(self) -> None:
        """Fail closed when the durable line and the live authority disagree."""
        rc, result, _ = self._run(
            returncode=2,
            canonical=unvalidated_status,
            write_log=lambda log, sha: readmission_refusal_log(log, sha),
        )

        self.assertEqual(rc, watch.NO_RESULT)
        self.assertEqual(result["state"], "NO_RESULT")
        self.assertIn("admission is UNKNOWN", result["reason"])
        self.assertEqual(
            result["commit_validation_state"],
            watch.COMMIT_VALIDATION_NOT_HARD_GREEN,
        )

    def test_commit_validation_follows_the_authority_not_the_local_attempt(self) -> None:
        """A qualifying LOCAL row cannot mint authority the canonical source denies."""
        rc, result, _ = self._run(
            returncode=0,
            canonical=unvalidated_status,
            write_log=lambda log, sha: log.write_text("", encoding="utf-8"),
            row={"result": "pass", "executed_tests": 873, "cwd": "", "commit": ""},
            qualified=(True, "local row qualifies"),
        )

        self.assertTrue(result["canonical_qualified"])
        self.assertEqual(
            result["commit_validation_state"],
            watch.COMMIT_VALIDATION_NOT_HARD_GREEN,
        )
        self.assertIn("NOT-VALIDATED", result["commit_validation_reason"])
        self.assertEqual(rc, watch.PASS)

    def test_unreadable_canonical_authority_never_reports_green(self) -> None:
        rc, result, _ = self._run(
            returncode=2,
            canonical=lambda sha: {
                "sha": sha,
                "available": False,
                "verdict": None,
                "exit_code": 70,
                "reason": "canonical validate-status unreadable (exit 70)",
                "receipt": None,
            },
            write_log=lambda log, sha: readmission_refusal_log(log, sha),
        )

        self.assertEqual(rc, watch.NO_RESULT)
        self.assertEqual(result["state"], "NO_RESULT")
        self.assertEqual(
            result["commit_validation_state"],
            watch.COMMIT_VALIDATION_UNKNOWN,
        )
        self.assertIn("unreadable", result["commit_validation_reason"])
        self.assertNotIn("landing_authority", result)


class AdmissionRefusalEvidenceTest(unittest.TestCase):
    def test_recognizes_the_readmission_refusal_with_verdict_and_sha(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate-run.log"
            sha = "a4c8aedaf5eaed7dfaafd7148e1acc83aed95348"
            readmission_refusal_log(log, sha)
            evidence = watch.admission_refusal_evidence(log)
            assert evidence is not None
            self.assertEqual(evidence["kind"], "canonical-terminal-readmission")
            self.assertEqual(evidence["refused_verdict"], "VALIDATED")
            self.assertEqual(evidence["refused_sha"], sha)

    def test_a_generic_refusal_is_not_promoted_to_the_readmission_kind(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate-run.log"
            log.write_text(
                "validate-run: REFUSED: cannot start ci-hub Herdr server: "
                "Failed to find executable herdr\n",
                encoding="utf-8",
            )
            evidence = watch.admission_refusal_evidence(log)
            assert evidence is not None
            self.assertEqual(evidence["kind"], "admission-refusal")
            self.assertNotIn("refused_verdict", evidence)

    def test_an_ordinary_producer_log_carries_no_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate-run.log"
            log.write_text(
                "validate-run: HANDLE validate-owner-checkout-x.service state=running\n"
                "[lint.clippy] ✓ PASS   Clippy (78s)\n",
                encoding="utf-8",
            )
            self.assertIsNone(watch.admission_refusal_evidence(log))

    def test_a_missing_log_is_not_a_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            self.assertIsNone(
                watch.admission_refusal_evidence(Path(temporary) / "absent.log")
            )


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
            self.assertEqual(
                result["checkout_freshness"], watch.CHECKOUT_FRESHNESS_UNKNOWN
            )
            self.assertEqual(
                result["commit_validation_state"],
                watch.COMMIT_VALIDATION_UNKNOWN,
            )
            message = next((state / "results").glob("*.md")).read_text()
            self.assertIn("checkout_freshness: UNKNOWN", message)
            self.assertNotIn("checkout_freshness: BEHIND", message)
            self.assertNotIn("landing_authority", message)
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
            self.assertEqual(result["checkout_freshness"], "CURRENT")
            self.assertEqual(result["checkout_behind"], 0)
            self.assertEqual(result["checkout_ahead"], 0)
            self.assertEqual(
                result["commit_validation_state"],
                watch.COMMIT_VALIDATION_UNKNOWN,
            )
            self.assertIn("current main", result["verdict_qualifier"])

    def test_validated_stale_checkout_reports_hard_green_and_behind(self) -> None:
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
            ), mock.patch.object(
                watch,
                "canonical_validate_status",
                return_value=validated_status(head),
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
            self.assertEqual(result["schema_version"], 2)
            self.assertEqual(result["checkout_freshness"], "BEHIND")
            self.assertEqual(result["checkout_behind"], 2)
            self.assertEqual(result["checkout_ahead"], 0)
            self.assertIn("does not describe current main", result["verdict_qualifier"])
            self.assertIn("2 commit(s) behind", result["reason"])
            self.assertEqual(
                result["commit_validation_state"],
                watch.COMMIT_VALIDATION_HARD_GREEN,
            )
            self.assertEqual(result["commit_validation_sha"], head)
            self.assertIn("VALIDATED", result["commit_validation_reason"])
            self.assertNotIn("landing_authority", result)

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
            ), mock.patch.object(
                watch,
                "canonical_validate_status",
                return_value=validated_status(head),
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
            self.assertEqual(result["checkout_freshness"], "CURRENT")
            self.assertEqual(result["checkout_behind"], 0)
            self.assertEqual(result["checkout_ahead"], 0)
            self.assertEqual(
                result["commit_validation_state"],
                watch.COMMIT_VALIDATION_HARD_GREEN,
            )
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
            record = Path(temporary) / "ignored/validate/runs/validate-fixture.json"
            log.write_text(
                "REFUSED: box-exclusive anchor held by pid=123 host=fixture "
                "anchor=box-exclusive-v1 boot_id=boot-fixture start_ticks=456 "
                "agent=helper kind=validate target=" + ("d" * 40) + " "
                f"unit=validate-fixture.service record={record}; this box admits one\n"
                "# ci-hub/validate-lock tool COST ACTUAL exit=3\n"
            )
            evidence = watch.lock_block_evidence(log)
            self.assertIsNotNone(evidence)
            self.assertEqual(evidence["kind"], "validate-lock-refusal")
            self.assertEqual(evidence["refusal_pid"], 123)
            self.assertEqual(evidence["anchor_identity"]["start_ticks"], 456)
            self.assertEqual(evidence["anchor_identity"]["boot_id"], "boot-fixture")
            self.assertEqual(evidence["anchor_identity"]["record"], str(record))

    def test_declared_priority_queue_refusal_is_lock_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "validate-lock: waiting in declared-priority queue; live holder retained\n"
                "REFUSED: declared-priority queue did not grant "
                "agent=owner-checkout-watch target=" + ("d" * 40) + " within 3s\n"
                "# ci-hub/validate-lock tool COST ACTUAL exit=3\n"
            )
            evidence = watch.lock_block_evidence(log)
            self.assertIsNotNone(evidence)
            self.assertEqual(evidence["validate_lock_exit"], 3)
            self.assertIn("declared-priority queue", evidence["refusal_line"])

    def test_direct_refusal_after_acquisition_is_not_called_lock_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            target = "d" * 40
            log.write_text(
                "validate-lock: ACQUIRED by owner-checkout-watch running validate "
                f"{target} (lease 1200s)\n"
                "REFUSED: producer setup failed after acquisition\n"
                "# ci-hub/validate-lock tool COST ACTUAL exit=3\n"
            )
            self.assertIsNone(watch.lock_block_evidence(log))

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
                    run_record=state / "runs/fixture.json",
                    admission_result={
                        "state": "admitted",
                        "recorded_at": "1970-01-01T00:03:21.250Z",
                    },
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
            run_record = Path(temporary) / "ignored/validate/runs/validate-test.json"
            write_validation_record(
                run_record,
                target=target,
                checkout=Path(temporary) / "checkout",
                log=log,
                admission_result=admitted_result(),
            )

            completed = watch.run_validate_observing_admission(
                ["bash", "-c", f"printf '%s\\n' '{marker}'; sleep 0.4"],
                cwd=Path(temporary),
                log=log,
                run_record=run_record,
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
            runs = parent / "ignored/validate/runs"
            runs.mkdir(parents=True)
            record_path = runs / "validate-helper-fixture.json"
            record_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "agent": "helper-validator2",
                        "target": target,
                        "unit": unit,
                        "pr": 2012,
                        "state": "running",
                        "process_identity": {
                            "pid": pid,
                            "start_ticks": ticks,
                            "boot_id": boot_id,
                        },
                    }
                )
            )
            proc_root = parent / "proc"
            (proc_root / str(pid)).mkdir(parents=True)
            fields_after_comm = ["S", *(["0"] * 18), str(ticks), "0"]
            (proc_root / str(pid) / "stat").write_text(
                f"{pid} (fixture) " + " ".join(fields_after_comm) + "\n"
            )
            (proc_root / str(pid) / "cgroup").write_text(
                f"0::/user.slice/fixture.slice/{unit}\n"
            )
            boot = parent / "boot_id"
            boot.write_text(boot_id + "\n")

            holder = watch.lock_holder_attribution(
                parent,
                {
                    "refusal_pid": pid,
                    "anchor_identity": {
                        "pid": pid,
                        "start_ticks": ticks,
                        "boot_id": boot_id,
                        "agent": "helper-validator2",
                        "target": target,
                        "unit": unit,
                        "record": str(record_path),
                    },
                },
                proc_root=proc_root,
                boot_id_path=boot,
            )

            self.assertEqual(holder["status"], "attributed")
            self.assertEqual(holder["pr"], 2012)
            self.assertEqual(holder["contention_class"], "reserved-publication-window")
            self.assertTrue(holder["process_identity"]["live_match"])

            ordinary = json.loads(record_path.read_text())
            ordinary["pr"] = 2041
            record_path.write_text(json.dumps(ordinary))
            holder = watch.lock_holder_attribution(
                parent,
                {
                    "refusal_pid": pid,
                    "anchor_identity": {
                        "pid": pid,
                        "start_ticks": ticks,
                        "boot_id": boot_id,
                        "agent": "helper-validator2",
                        "target": target,
                        "unit": unit,
                        "record": str(record_path),
                    },
                },
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

    def test_lock_holder_attribution_reports_confirmed_dead_without_guessing(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            runs = parent / "ignored/validate/runs"
            runs.mkdir(parents=True)
            pid, ticks, boot_id = 321, 654, "boot-fixture"
            target = "e" * 40
            unit = "validate-dead-fixture.service"
            record_path = runs / "validate-dead-fixture.json"
            record_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "state": "running",
                        "agent": "dead-agent",
                        "target": target,
                        "unit": unit,
                        "process_identity": {
                            "pid": pid,
                            "start_ticks": ticks,
                            "boot_id": boot_id,
                        },
                    }
                )
            )
            boot = parent / "boot_id"
            boot.write_text(boot_id + "\n")
            proc_root = parent / "proc"
            (proc_root / str(pid)).mkdir(parents=True)
            fields_after_comm = ["S", *(["0"] * 18), str(ticks + 1), "0"]
            (proc_root / str(pid) / "stat").write_text(
                f"{pid} (replacement) " + " ".join(fields_after_comm) + "\n"
            )
            holder = watch.lock_holder_attribution(
                parent,
                {
                    "anchor_identity": {
                        "pid": pid,
                        "start_ticks": ticks,
                        "boot_id": boot_id,
                        "agent": "dead-agent",
                        "target": target,
                        "unit": unit,
                        "record": str(record_path),
                    }
                },
                proc_root=proc_root,
                boot_id_path=boot,
            )
            self.assertEqual(holder["status"], "dead")
            self.assertIn("pid has been reused", holder["reason"])
            self.assertFalse(holder["process_identity"]["live_match"])

            (proc_root / str(pid) / "stat").unlink()
            holder = watch.lock_holder_attribution(
                parent,
                {
                    "anchor_identity": {
                        "pid": pid,
                        "start_ticks": ticks,
                        "boot_id": boot_id,
                        "agent": "dead-agent",
                        "target": target,
                        "unit": unit,
                        "record": str(record_path),
                    }
                },
                proc_root=proc_root,
                boot_id_path=boot,
            )
            self.assertEqual(holder["status"], "dead")
            self.assertIn("pid is absent", holder["reason"])

    def test_lock_holder_attribution_keeps_partial_tuple_unattributed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            holder = watch.lock_holder_attribution(
                Path(temporary),
                {"anchor_identity": {"pid": 123, "start_ticks": 456}},
            )
            self.assertEqual(holder["status"], "unattributed")
            self.assertIn("complete anchor identity", holder["reason"])

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

    def test_relink_retirement_requires_overlap_and_all_consumers_pass(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "[e2e.manifest_chaos_c] ▶ START  Portable manifest bucket: chaos-c\n"
                "[e2e.manifest_debugger_c] ▶ START  Portable manifest bucket: debugger-c\n"
                "[test.app_strict_verify]    Compiling hermit v0.2.0 (/fixture/hermit-cli)\n"
                "[e2e.manifest_chaos_c] PASS  portable verify ptrace chaos-c/clock\n"
                "[e2e.manifest_debugger_c] PASS  portable verify ptrace debugger-c/gdb\n"
                "[test.app_strict_verify]     Finished `test` profile [unoptimized] target(s) in 5.92s\n"
                "[e2e.manifest_chaos_c] ✓ PASS   Portable manifest bucket: chaos-c (17s)\n"
                "[e2e.manifest_debugger_c] ✓ PASS   Portable manifest bucket: debugger-c (18s)\n",
                encoding="utf-8",
            )

            evidence = watch.relink_race_retirement_evidence(
                log, recurrence_triggered=False
            )

            self.assertTrue(evidence["confirmed"])
            self.assertEqual(evidence["witnessed_window_count"], 1)
            self.assertEqual(evidence["started_consumer_count"], 2)
            self.assertEqual(evidence["terminal_pass_count"], 2)
            self.assertEqual(
                evidence["compile_windows"][0]["overlap_consumers"],
                ["e2e.manifest_chaos_c", "e2e.manifest_debugger_c"],
            )

    def test_relink_retirement_rejects_quiet_green(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "[test.app_strict_verify]    Compiling hermit v0.2.0 (/fixture/hermit-cli)\n"
                "[test.app_strict_verify]     Finished `test` profile [unoptimized] target(s) in 5.92s\n"
                "[e2e.manifest_chaos_c] ▶ START  Portable manifest bucket: chaos-c\n"
                "[e2e.manifest_chaos_c] ✓ PASS   Portable manifest bucket: chaos-c (17s)\n",
                encoding="utf-8",
            )

            evidence = watch.relink_race_retirement_evidence(
                log, recurrence_triggered=False
            )

            self.assertFalse(evidence["confirmed"])
            self.assertIn("no e2e per-cell PASS activity", evidence["reason"])

    def test_relink_retirement_rejects_overlap_with_nonpassing_consumer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "[e2e.manifest_chaos_c] ▶ START  Portable manifest bucket: chaos-c\n"
                "[e2e.manifest_debugger_c] ▶ START  Portable manifest bucket: debugger-c\n"
                "[test.app_strict_verify]    Compiling hermit v0.2.0 (/fixture/hermit-cli)\n"
                "[e2e.manifest_chaos_c] PASS  portable verify ptrace chaos-c/clock\n"
                "[test.app_strict_verify]     Finished `test` profile [unoptimized] target(s) in 5.92s\n"
                "[e2e.manifest_chaos_c] ✓ PASS   Portable manifest bucket: chaos-c (17s)\n"
                "[e2e.manifest_debugger_c] ✗ FAIL   Portable manifest bucket: debugger-c (18s, exit 1)\n",
                encoding="utf-8",
            )

            evidence = watch.relink_race_retirement_evidence(
                log, recurrence_triggered=False
            )

            self.assertFalse(evidence["confirmed"])
            self.assertEqual(evidence["nonpassing_consumers"][0]["node"], "e2e.manifest_debugger_c")
            self.assertIn("not every started", evidence["reason"])

    def test_relink_retirement_never_confirms_when_recurrence_triggered(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(
                "[e2e.manifest_chaos_c] ▶ START  Portable manifest bucket: chaos-c\n"
                "[test.app_strict_verify]    Compiling hermit v0.2.0 (/fixture/hermit-cli)\n"
                "[e2e.manifest_chaos_c] PASS  portable verify ptrace chaos-c/clock\n"
                "[test.app_strict_verify]     Finished `test` profile [unoptimized] target(s) in 5.92s\n"
                "[e2e.manifest_chaos_c] ✓ PASS   Portable manifest bucket: chaos-c (17s)\n",
                encoding="utf-8",
            )

            evidence = watch.relink_race_retirement_evidence(
                log, recurrence_triggered=True
            )

            self.assertFalse(evidence["confirmed"])
            self.assertIn("recurrence signature", evidence["reason"])

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

            restored, after, _scorecard = watch.preserve_and_restore(
                repo, before, recovery
            )

            self.assertTrue(restored)
            self.assertEqual(after, before)
            self.assertEqual(command(repo, "git", "status", "--porcelain"), "")
            self.assertEqual(
                (recovery / "untracked/build/generated.log").read_text(),
                "evidence\n",
            )
            self.assertIn("+after", (recovery / "tracked.patch").read_text())

    def test_scorecard_updates_and_existing_symlink_survive_other_restoration(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            compat = repo / "ci/compat-envelope"
            compat.mkdir(parents=True)
            (compat / "scorecard.rs").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            (compat / "scorecard.rs").chmod(0o755)
            (repo / "SCORECARD.md").write_text("before scorecard\n", encoding="utf-8")
            (compat / "cells.json").write_text("{\"before\": true}\n", encoding="utf-8")
            command(repo, "git", "add", "SCORECARD.md", "ci/compat-envelope")
            command(repo, "git", "commit", "-q", "-m", "scorecard fixture")
            (repo / "validate").symlink_to("scripts/validate.rs")
            before = watch.tree_state(repo)
            (repo / "SCORECARD.md").write_text("after scorecard\n", encoding="utf-8")
            (compat / "cells.json").write_text("{\"after\": true}\n", encoding="utf-8")
            (repo / "tracked.txt").write_text("validation side effect\n", encoding="utf-8")
            generated = repo / "build/generated.log"
            generated.parent.mkdir()
            generated.write_text("temporary output\n", encoding="utf-8")
            recovery = root / "recovery"

            restored, after, scorecard_refused = watch.preserve_and_restore(
                repo, before, recovery
            )

            self.assertTrue(restored)
            self.assertFalse(scorecard_refused)
            self.assertEqual((repo / "tracked.txt").read_text(), "before\n")
            self.assertTrue((repo / "validate").is_symlink())
            self.assertEqual((repo / "SCORECARD.md").read_text(), "after scorecard\n")
            self.assertEqual((compat / "cells.json").read_text(), '{"after": true}\n')
            self.assertEqual(
                sorted(after["dirty_paths"]),
                [
                    " M SCORECARD.md",
                    " M ci/compat-envelope/cells.json",
                    "?? validate",
                ],
            )
            self.assertEqual(
                (recovery / "untracked/build/generated.log").read_text(),
                "temporary output\n",
            )
            patch = (recovery / "tracked.patch").read_text()
            self.assertIn("validation side effect", patch)
            self.assertNotIn("after scorecard", patch)

    def test_inconsistent_scorecard_changes_are_left_for_inspection(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            compat = repo / "ci/compat-envelope"
            compat.mkdir(parents=True)
            (repo / "SCORECARD.md").write_text("before\n", encoding="utf-8")
            (compat / "cells.json").write_text("{}\n", encoding="utf-8")
            command(repo, "git", "add", "SCORECARD.md", "ci/compat-envelope/cells.json")
            command(repo, "git", "commit", "-q", "-m", "scorecard fixture")
            before = watch.tree_state(repo)
            (repo / "SCORECARD.md").write_text("unconfirmed\n", encoding="utf-8")

            restored, after, scorecard_refused = watch.preserve_and_restore(
                repo, before, root / "recovery"
            )

            self.assertTrue(restored)
            self.assertTrue(scorecard_refused)
            self.assertNotEqual(after, before)
            self.assertEqual((repo / "SCORECARD.md").read_text(), "unconfirmed\n")

    def test_dirty_scorecard_refuses_before_launch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = clean_repository(root)
            compat = repo / "ci/compat-envelope"
            compat.mkdir(parents=True)
            (repo / "SCORECARD.md").write_text("before\n")
            (compat / "cells.json").write_text("{}\n")
            command(repo, "git", "add", "SCORECARD.md", "ci/compat-envelope/cells.json")
            command(repo, "git", "commit", "-q", "-m", "scorecard fixture")
            (repo / "SCORECARD.md").write_text("pending review\n")

            with mock.patch.object(watch, "run_validate_observing_admission") as launched:
                rc = watch.main(
                    [
                        "--checkout", str(repo),
                        "--state-dir", str(root / "state"),
                        "--no-fetch", "--no-notify",
                    ]
                )

            self.assertEqual(rc, watch.CANNOT_START)
            self.assertFalse(launched.called)

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
