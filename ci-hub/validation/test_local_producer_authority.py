#!/usr/bin/env python3
"""Validator-file changes are not a validation-status gate.

The fixtures change one or both validator files while keeping the validation
row itself qualifying.  Status and history must follow the completed run; there
is no producer registry (https://github.com/rrnewton/dev-hermit/issues/414).
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
HUB = ROOT / "ci-hub" / "ci-hub"
HERMIT = ROOT / "hermit"
LEDGER_ADAPTER = ROOT / "ci-hub" / "ledger" / "validate_rows.py"
# A fixed Hermit commit the fixture branches from.
PRIMARY_SHA = "ce1aca3aba3a4bb1db244e1ca2ab6903ea4bb7a8"


def run(*args: str, cwd: Path | None = None, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        list(args),
        cwd=cwd,
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


class LocalProducerAuthorityTests(unittest.TestCase):
    def setUp(self) -> None:
        if subprocess.run(
            ["git", "-C", str(HERMIT), "rev-parse", "--git-dir"],
            capture_output=True,
        ).returncode != 0:
            self.skipTest("Hermit submodule is not initialized")

    def fixture(self, root: Path) -> tuple[Path, dict[str, str]]:
        repo = root / "hermit-fixture"
        run("git", "clone", "-q", "--shared", str(HERMIT), str(repo))
        primary_sha = PRIMARY_SHA
        run("git", "checkout", "-q", "--detach", primary_sha, cwd=repo)

        git_env = {
            **os.environ,
            "GIT_AUTHOR_NAME": "producer-authority-test",
            "GIT_AUTHOR_EMAIL": "test@example.com",
            "GIT_COMMITTER_NAME": "producer-authority-test",
            "GIT_COMMITTER_EMAIL": "test@example.com",
        }

        with (repo / "scripts/validate.rs").open("a") as handle:
            handle.write("\n// producer-authority candidate fixture\n")
        (repo / "ci/validate_peer_snapshot.py").write_text(
            "#!/usr/bin/env python3\nprint('candidate helper')\n"
        )
        run(
            "git",
            "add",
            "scripts/validate.rs",
            "ci/validate_peer_snapshot.py",
            cwd=repo,
            env=git_env,
        )
        run("git", "commit", "-qm", "candidate producer", cwd=repo, env=git_env)
        candidate_sha = run("git", "rev-parse", "HEAD", cwd=repo)

        run("git", "checkout", "-q", "--detach", primary_sha, cwd=repo)
        with (repo / "scripts/validate.rs").open("a") as handle:
            handle.write("\n// producer-authority candidate fixture\n")
        run("git", "add", "scripts/validate.rs", cwd=repo, env=git_env)
        run("git", "commit", "-qm", "validate-only producer", cwd=repo, env=git_env)
        crossed_validate_sha = run("git", "rev-parse", "HEAD", cwd=repo)

        run("git", "checkout", "-q", "--detach", primary_sha, cwd=repo)
        (repo / "ci/validate_peer_snapshot.py").write_text(
            "#!/usr/bin/env python3\nprint('candidate helper')\n"
        )
        run("git", "add", "ci/validate_peer_snapshot.py", cwd=repo, env=git_env)
        run("git", "commit", "-qm", "helper-only producer", cwd=repo, env=git_env)
        crossed_helper_sha = run("git", "rev-parse", "HEAD", cwd=repo)

        run("git", "checkout", "-q", "--detach", primary_sha, cwd=repo)
        run("git", "commit", "--allow-empty", "-qm", "later legacy map", cwd=repo, env=git_env)
        legacy_replay_sha = run("git", "rev-parse", "HEAD", cwd=repo)

        return repo, {
            "primary": primary_sha,
            "candidate": candidate_sha,
            "crossed_validate": crossed_validate_sha,
            "crossed_helper": crossed_helper_sha,
            "legacy_replay": legacy_replay_sha,
        }

    @staticmethod
    def row(repo: Path, sha: str, finished: str) -> dict[str, object]:
        return {
            "schema_version": 4,
            "started_at": "2026-08-08T12:00:00Z",
            "finished_at": finished,
            "host": "producer-authority-fixture",
            "slot": "fixture",
            "repo": "hermit",
            "cwd": str(repo),
            "profile": "full",
            "selection_mode": "full",
            "commit": sha,
            "tree": run("git", "rev-parse", f"{sha}^{{tree}}", cwd=repo),
            "commit_anchored": True,
            "tree_dirty": False,
            "result": "pass",
            "raw_result": "pass",
            "exit_code": 0,
            "executed_tests": 12,
            "filtered_tests": 0,
            "checks": 1,
            "gates_run": 1,
            "gates_expected": 1,
            "failures": 0,
            "real_seconds": 1.0,
            "log_file": "/tmp/producer-authority-fixture.log",
            "gates": [{"name": "full", "result": "pass", "exit_code": 0}],
        }

    def hub(self, root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(HUB), *args],
            cwd=ROOT,
            env={
                **os.environ,
                "CI_HUB_TOOL_COST_ACTIVE": "1",
                "HERMIT_VALIDATE_STOP_TEST_MODE": "1",
                "CI_HUB_VALIDATE_LEDGER_TEST_ROOT": str(root),
            },
            capture_output=True,
            text=True,
            timeout=120,
        )

    @staticmethod
    def record(root: Path, row: dict[str, object]) -> None:
        result = subprocess.run(
            [sys.executable, str(LEDGER_ADAPTER), "record"],
            input=json.dumps(row),
            env={
                **os.environ,
                "HERMIT_VALIDATE_STOP_TEST_MODE": "1",
                "CI_HUB_VALIDATE_LEDGER_TEST_ROOT": str(root),
            },
            capture_output=True,
            text=True,
            timeout=30,
        )
        if result.returncode != 0:
            raise AssertionError(result.stderr)

    def test_validate_status_follows_the_run_not_the_file_registry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, shas = self.fixture(root)
            cases = [
                "primary",
                "candidate",
                "crossed_validate",
                "crossed_helper",
                "legacy_replay",
            ]
            for index, name in enumerate(cases):
                with self.subTest(name=name):
                    self.record(
                        root,
                        self.row(repo, shas[name], f"2026-08-08T12:0{index}:00Z"),
                    )
                    result = self.hub(
                        root,
                        "validate-status",
                        "--sha",
                        shas[name],
                        "--repo",
                        "rrnewton/hermit",
                        "--json",
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    report = json.loads(result.stdout)
                    self.assertEqual(report["verdict"], "VALIDATED")
                    self.assertEqual(report["qualifying_count"], 1)
                    self.assertIsNone(
                        report["newest_qualifying"]["producer_definition"]
                    )

    def test_newest_green_follows_the_run_not_the_file_registry(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo, shas = self.fixture(root)
            candidate = shas["candidate"]
            run("git", "update-ref", "refs/remotes/origin/main", candidate, cwd=repo)
            self.record(
                root,
                self.row(repo, candidate, "2026-08-08T12:10:00Z"),
            )
            cache = root / "newest-green-cache.json"
            args = (
                "newest-green",
                "--branch",
                "main",
                "--repo-dir",
                str(repo),
                "--cache",
                str(cache),
                "--no-fetch",
                "--json",
            )
            positive = self.hub(root, *args)
            self.assertEqual(positive.returncode, 0, positive.stderr)
            report = json.loads(positive.stdout)["report"]
            self.assertEqual(report["green"]["sha"], candidate)
            self.assertIsNone(report["green"]["producer_definition"])


if __name__ == "__main__":
    unittest.main()
