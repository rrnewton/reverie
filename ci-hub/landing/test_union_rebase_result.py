#!/usr/bin/env python3
"""Regression tests for the serialized union-rebase result."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import union_rebase_result as subject  # noqa: E402


SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_C = "c" * 40
SHA_D = "d" * 40


def current(**changes: object) -> dict[str, object]:
    row: dict[str, object] = {
        "schema_version": 1,
        "branch": "type-audit/example",
        "outcome": "unioned",
        "detail": None,
        "base_sha": SHA_A,
        "input_sha": SHA_B,
        "fork_sha": SHA_C,
        "result_sha": SHA_D,
        "pushed": True,
        "exit_code": 0,
    }
    row.update(changes)
    return row


class UnionRebaseResultTest(unittest.TestCase):
    def test_producer_and_consumer_use_the_serialized_contract(self) -> None:
        producer = (HERE / "union-rebase.sh").read_text(encoding="utf-8")
        consumer = (HERE / "land-pr.sh").read_text(encoding="utf-8")
        self.assertIn('--result-json "$uresult"', consumer)
        self.assertIn('union_rebase_result.py" require-success', consumer)
        self.assertNotIn("grep -E '^RESULT'", consumer)
        self.assertIn('python3 "$RESULT_HELPER" write', producer)

    def test_current_success_round_trips(self) -> None:
        result = subject.UnionRebaseResult.from_mapping(current())
        self.assertTrue(result.successful)
        self.assertEqual(result.display(), "UNIONED")
        self.assertEqual(result.to_mapping(), current())

    def test_producer_writes_a_typed_fetch_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            remote = root / "remote.git"
            checkout = root / "checkout"
            result_path = root / "result.json"
            subprocess.run(["git", "init", "--bare", "-q", remote], check=True)
            subprocess.run(["git", "init", "-q", checkout], check=True)
            subprocess.run(
                ["git", "-C", checkout, "config", "user.email", "test@example.com"],
                check=True,
            )
            subprocess.run(
                ["git", "-C", checkout, "config", "user.name", "Test"], check=True
            )
            (checkout / "README").write_text("fixture\n", encoding="utf-8")
            subprocess.run(["git", "-C", checkout, "add", "README"], check=True)
            subprocess.run(
                ["git", "-C", checkout, "commit", "-q", "-m", "fixture"], check=True
            )
            subprocess.run(
                ["git", "-C", checkout, "branch", "-M", "main"], check=True
            )
            subprocess.run(
                ["git", "-C", checkout, "remote", "add", "origin", str(remote)],
                check=True,
            )
            subprocess.run(
                ["git", "-C", checkout, "push", "-q", "origin", "main"], check=True
            )
            env = dict(os.environ)
            run = subprocess.run(
                [
                    str(HERE / "union-rebase.sh"),
                    str(checkout),
                    "missing-branch",
                    "--result-json",
                    str(result_path),
                ],
                text=True,
                capture_output=True,
                timeout=20,
                env=env,
            )
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("RESULT missing-branch REBASE-FAIL:fetch", run.stdout)
            result = subject.read_result(result_path)
            self.assertEqual(result.outcome, subject.Outcome.REBASE_FAIL)
            self.assertEqual(result.detail, "fetch")
            self.assertFalse(result.successful)
            self.assertFalse(result.pushed)

    def test_missing_unknown_and_future_outcome_fail_by_name(self) -> None:
        missing = current()
        del missing["result_sha"]
        with self.assertRaisesRegex(
            subject.UnionRebaseResultError,
            "union-rebase-result-missing-field: result_sha",
        ):
            subject.UnionRebaseResult.from_mapping(missing)

        unknown = current(future="value")
        with self.assertRaisesRegex(
            subject.UnionRebaseResultError,
            "union-rebase-result-unknown-field: future",
        ):
            subject.UnionRebaseResult.from_mapping(unknown)

        future = current(outcome="partly-unioned")
        with self.assertRaisesRegex(
            subject.UnionRebaseResultError,
            "union-rebase-result-outcome: 'partly-unioned'",
        ):
            subject.UnionRebaseResult.from_mapping(future)

    def test_success_requires_every_identity_and_zero_exit(self) -> None:
        for field in ("base_sha", "input_sha", "fork_sha", "result_sha"):
            with self.subTest(field=field), self.assertRaisesRegex(
                subject.UnionRebaseResultError,
                f"union-rebase-result-{field}",
            ):
                subject.UnionRebaseResult.from_mapping(current(**{field: None}))
        with self.assertRaisesRegex(
            subject.UnionRebaseResultError,
            "union-rebase-result-exit_code",
        ):
            subject.UnionRebaseResult.from_mapping(current(exit_code=1))

    def test_refusal_requires_detail_and_cannot_claim_push(self) -> None:
        with self.assertRaisesRegex(
            subject.UnionRebaseResultError,
            "union-rebase-result-detail",
        ):
            subject.UnionRebaseResult.from_mapping(
                current(outcome="human", detail=None, pushed=False, exit_code=1)
            )
        with self.assertRaisesRegex(
            subject.UnionRebaseResultError,
            "union-rebase-result-pushed",
        ):
            subject.UnionRebaseResult.from_mapping(
                current(outcome="human", detail="conflict", exit_code=1)
            )

    def test_require_success_uses_typed_value_not_display_text(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            path.write_text(json.dumps(current()) + "\n", encoding="utf-8")
            accepted = subprocess.run(
                [
                    sys.executable,
                    str(HERE / "union_rebase_result.py"),
                    "require-success",
                    "--path",
                    str(path),
                    "--expect-branch",
                    "type-audit/example",
                ],
                text=True,
                capture_output=True,
                timeout=10,
            )
            self.assertEqual(accepted.returncode, 0, accepted.stderr)
            self.assertEqual(accepted.stdout.strip(), "UNIONED")

            refused = current(
                outcome="human",
                detail="unmanaged-conflict:file",
                result_sha=None,
                pushed=False,
                exit_code=1,
            )
            path.write_text(json.dumps(refused) + "\n", encoding="utf-8")
            rejected = subprocess.run(
                [
                    sys.executable,
                    str(HERE / "union_rebase_result.py"),
                    "require-success",
                    "--path",
                    str(path),
                    "--expect-branch",
                    "type-audit/example",
                ],
                text=True,
                capture_output=True,
                timeout=10,
            )
            self.assertEqual(rejected.returncode, 3, rejected.stderr)
            self.assertEqual(
                rejected.stdout.strip(), "HUMAN:unmanaged-conflict:file"
            )

    def test_every_outcome_requires_an_explicit_mapping(self) -> None:
        subject.require_complete_outcome_mapping()
        self.assertEqual(set(subject._DISPLAY), set(subject.Outcome))


if __name__ == "__main__":
    unittest.main()
