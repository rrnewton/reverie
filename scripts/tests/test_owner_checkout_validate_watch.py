#!/usr/bin/env python3
"""Fail-closed brackets for the owner's canonical-checkout watch."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


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
    command(repo, "git", "add", "tracked.txt")
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
            ledger = root / "ledger.jsonl"
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
            ledger.write_text("".join(json.dumps(row) + "\n" for row in rows))
            launched = watch.parse_time("2026-08-09T03:00:00Z")
            assert launched is not None

            selected = watch.latest_attempt_row(
                ledger, checkout=repo, sha=sha, launched_at=launched
            )

            self.assertIsNotNone(selected)
            self.assertIsNone(selected["executed_tests"])


if __name__ == "__main__":
    unittest.main()
