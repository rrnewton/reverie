#!/usr/bin/env python3

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
GUARD = HERE / "producer_transition_guard.py"
VERIFIER = ROOT / "ci-hub" / "validation" / "verify_receipt.sh"


def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        **kwargs,
    )


class ProducerTransitionGuardTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.fixture_root = Path(self.temporary.name) / "parent"
        landing = self.fixture_root / "ci-hub" / "landing"
        validation = self.fixture_root / "ci-hub" / "validation"
        validate = self.fixture_root / "ci-hub" / "validate"
        landing.mkdir(parents=True)
        validation.mkdir(parents=True)
        validate.mkdir(parents=True)
        shutil.copy2(GUARD, landing / GUARD.name)
        shutil.copy2(VERIFIER, validation / VERIFIER.name)
        self.guard = landing / GUARD.name
        self.registry = validate / "producer-definition.json"

        self.repo = Path(self.temporary.name) / "hermit"
        self.repo.mkdir()
        run(["git", "init", "-q", str(self.repo)])
        run(
            [
                "git",
                "-C",
                str(self.repo),
                "config",
                "user.email",
                "fixture@example.invalid",
            ]
        )
        run(["git", "-C", str(self.repo), "config", "user.name", "Fixture"])
        producer = self.repo / "scripts" / "validate.rs"
        producer.parent.mkdir()
        producer.write_text("primary\n", encoding="utf-8")
        (self.repo / "README.md").write_text("base\n", encoding="utf-8")
        run(["git", "-C", str(self.repo), "add", "scripts/validate.rs", "README.md"])
        run(["git", "-C", str(self.repo), "commit", "-q", "-m", "base"])
        self.base = run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"]
        ).stdout.strip()
        self.primary_blob = run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD:scripts/validate.rs"]
        ).stdout.strip()

        run(["git", "-C", str(self.repo), "checkout", "-q", "-b", "producer"])
        producer.write_text("candidate\n", encoding="utf-8")
        run(["git", "-C", str(self.repo), "add", "scripts/validate.rs"])
        run(["git", "-C", str(self.repo), "commit", "-q", "-m", "producer change"])
        self.producer_head = run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"]
        ).stdout.strip()
        self.candidate_blob = run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD:scripts/validate.rs"]
        ).stdout.strip()

        run(
            ["git", "-C", str(self.repo), "checkout", "-q", "-b", "ordinary", self.base]
        )
        (self.repo / "README.md").write_text("ordinary\n", encoding="utf-8")
        run(["git", "-C", str(self.repo), "add", "README.md"])
        run(["git", "-C", str(self.repo), "commit", "-q", "-m", "ordinary change"])
        self.ordinary_head = run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"]
        ).stdout.strip()

    def registry_value(self, *, staged: bool) -> dict[str, object]:
        value: dict[str, object] = {
            "registered_at": self.base,
            "registered_coverage_status": "complete",
            "registered": {"scripts/validate.rs": self.primary_blob},
        }
        if staged:
            value["transition"] = {
                "id": "rrnewton-hermit-pr-123",
                "registered_at": self.producer_head,
                "provenance": {
                    "repository": "rrnewton/hermit",
                    "pull_request": 123,
                    "head": self.producer_head,
                },
                "candidate_coverage_status": "complete",
                "candidate": {"scripts/validate.rs": self.candidate_blob},
                "added_paths": [],
                "finalize_after": "2020-01-01T00:00:00Z",
                "expires_at": "2099-01-01T00:00:00Z",
            }
        return value

    def write_registry(self, *, staged: bool) -> None:
        self.registry.write_text(
            json.dumps(self.registry_value(staged=staged), sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def invoke(
        self, head: str, *, pr: int = 123, environment: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "python3",
                str(self.guard),
                "--repo",
                "rrnewton/hermit",
                "--pr",
                str(pr),
                "--head",
                head,
                "--checkout",
                str(self.repo),
            ],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
        )

    def test_staged_producer_change_is_accepted(self) -> None:
        self.write_registry(staged=True)
        result = self.invoke(self.producer_head)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ACCEPTED: staged producer transition", result.stdout)
        self.assertIn("changed_registered_paths=1", result.stdout)

    def test_unstaged_producer_change_is_refused(self) -> None:
        self.write_registry(staged=False)
        result = self.invoke(self.producer_head)
        self.assertEqual(result.returncode, 1)
        self.assertIn("REFUSED:", result.stderr)
        self.assertIn("changed_registered_paths=1", result.stderr)
        self.assertIn("scripts/validate.rs", result.stderr)

    def test_transition_for_another_pr_does_not_authorize_change(self) -> None:
        self.write_registry(staged=True)
        result = self.invoke(self.producer_head, pr=124)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "without an active transition bound to this exact head", result.stderr
        )

    def test_ordinary_change_is_accepted(self) -> None:
        self.write_registry(staged=False)
        result = self.invoke(self.ordinary_head)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("changed_registered_paths=0", result.stdout)

    def test_environment_cannot_replace_installed_registry(self) -> None:
        self.write_registry(staged=False)
        replacement = Path(self.temporary.name) / "replacement.json"
        replacement.write_text(
            json.dumps(self.registry_value(staged=True), sort_keys=True) + "\n",
            encoding="utf-8",
        )
        environment = {**os.environ, "PRODUCER_DEFINITION_REGISTRY": str(replacement)}
        result = self.invoke(self.producer_head, environment=environment)
        self.assertEqual(result.returncode, 1)
        self.assertIn("REFUSED:", result.stderr)

    def test_script_dir_cannot_replace_installed_registry(self) -> None:
        self.write_registry(staged=False)
        replacement_root = Path(self.temporary.name) / "replacement-root"
        replacement_validation = replacement_root / "validation"
        replacement_validation.mkdir(parents=True)
        replacement_registry = replacement_root / "validate" / "producer-definition.json"
        replacement_registry.parent.mkdir()
        replacement_registry.write_text(
            json.dumps(self.registry_value(staged=True), sort_keys=True) + "\n",
            encoding="utf-8",
        )
        environment = {**os.environ, "script_dir": str(replacement_validation)}
        result = self.invoke(self.producer_head, environment=environment)
        self.assertEqual(result.returncode, 1)
        self.assertIn("REFUSED:", result.stderr)

    def test_lander_runs_guard_before_merge(self) -> None:
        lander = (HERE / "land-pr.sh").read_text(encoding="utf-8")
        self.assertLess(
            lander.index("producer_transition_guard.py"),
            lander.index('gh pr merge "$PR"'),
        )


if __name__ == "__main__":
    unittest.main()
