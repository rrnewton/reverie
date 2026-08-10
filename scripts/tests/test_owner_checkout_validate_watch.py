#!/usr/bin/env python3
"""Fail-closed brackets for the owner's canonical-checkout watch."""

from __future__ import annotations

import importlib.util
import json
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
