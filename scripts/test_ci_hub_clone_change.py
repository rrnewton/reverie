#!/usr/bin/env python3
"""Focused refusal/acceptance tests for ci-hub-clone-change.py."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


SOURCE = Path(__file__).with_name("ci-hub-clone-change.py")
SPEC = importlib.util.spec_from_file_location("ci_hub_clone_change", SOURCE)
assert SPEC and SPEC.loader
TOOL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOOL)


def command(*argv: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, cwd=cwd, text=True, capture_output=True, check=False)


def git(repo: Path, *args: str) -> str:
    result = command("git", "-C", str(repo), *args)
    if result.returncode != 0:
        raise AssertionError(result.stderr)
    return result.stdout.strip()


class Fixture(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.remote = self.root / "remote.git"
        self.seed = self.root / "seed"
        command("git", "init", "--bare", str(self.remote))
        command("git", "init", "-b", "main", str(self.seed))
        git(self.seed, "config", "user.name", "Test Agent")
        git(self.seed, "config", "user.email", "test@example.invalid")
        (self.seed / "ci-hub/bin").mkdir(parents=True)
        (self.seed / "ci-hub/bin/tool").write_text("old\n")
        (self.seed / "outside.txt").write_text("old\n")
        (self.seed / "scripts").mkdir()
        (self.seed / "scripts/setup-hooks.sh").write_text("#!/bin/sh\nexit 0\n")
        (self.seed / "scripts/setup-hooks.sh").chmod(0o755)
        git(self.seed, "add", "ci-hub/bin/tool", "outside.txt", "scripts/setup-hooks.sh")
        git(self.seed, "commit", "-m", "seed")
        git(self.seed, "remote", "add", "origin", str(self.remote))
        git(self.seed, "push", "-u", "origin", "main")
        command("git", "--git-dir", str(self.remote), "symbolic-ref", "HEAD", "refs/heads/main")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def clone(self, name: str = "change") -> Path:
        path = self.root / name
        result = command(
            str(SOURCE), "create", "--task", "fixture-change", "--path",
            "ci-hub/bin", "--clone", str(path), "--remote", str(self.remote),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        git(path, "config", "user.name", "Test Agent")
        git(path, "config", "user.email", "test@example.invalid")
        return path

    def commit(self, repo: Path, path: str, text: str) -> str:
        (repo / path).write_text(text)
        git(repo, "add", path)
        git(repo, "commit", "-m", "candidate")
        return git(repo, "rev-parse", "HEAD")

    def test_positive_one_commit_claimed_and_tested_seals(self) -> None:
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "new\n")
        result = command(
            str(SOURCE),
            "seal",
            "--clone",
            str(repo),
            "--test",
            "test -f ci-hub/bin/tool",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        seal = json.loads((repo / ".git" / TOOL.SEAL).read_text())
        self.assertEqual(seal["candidate"], candidate)
        self.assertEqual(seal["changed_paths"], ["ci-hub/bin/tool"])
        self.assertEqual(seal["tests"][0]["exit_code"], 0)

    def test_positive_published_candidate_keeps_valid_activation_seal(self) -> None:
        repo = self.clone()
        candidate = self.commit(repo, "ci-hub/bin/tool", "published\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        git(repo, "push", "origin", "HEAD:main")
        manifest, seal = TOOL.verified_seal(repo, require_unpublished_base=False)
        self.assertEqual(manifest["task"], "fixture-change")
        self.assertEqual(seal["candidate"], candidate)

    def test_negative_published_candidate_is_not_publishable_twice(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "published\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        git(repo, "push", "origin", "HEAD:main")
        with self.assertRaisesRegex(TOOL.Refusal, "sealed base is stale"):
            TOOL.verified_seal(repo, require_unpublished_base=True)

    def test_negative_unclaimed_path_refuses(self) -> None:
        repo = self.clone()
        self.commit(repo, "outside.txt", "not allowed\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unclaimed paths", result.stderr)

    def test_negative_missing_test_refuses(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "new\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo))
        self.assertEqual(result.returncode, 2)
        self.assertIn("at least one --test", result.stderr)

    def test_negative_two_commits_refuse(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "one\n")
        self.commit(repo, "ci-hub/bin/tool", "two\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 2)
        self.assertIn("exactly one commit", result.stderr)

    def test_negative_remote_move_invalidates_candidate(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        (self.seed / "other.txt").write_text("remote moved\n")
        git(self.seed, "add", "other.txt")
        git(self.seed, "commit", "-m", "move remote")
        git(self.seed, "push", "origin", "main")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 2)
        self.assertIn("current origin/main", result.stderr)

    def test_negative_existing_destination_preserved(self) -> None:
        occupied = self.root / "occupied"
        occupied.mkdir()
        marker = occupied / "preserve"
        marker.write_text("owned\n")
        result = command(
            str(SOURCE), "create", "--task", "second-change", "--path",
            "ci-hub/bin", "--clone", str(occupied), "--remote", str(self.remote),
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(marker.read_text(), "owned\n")

    def test_negative_dirty_primary_candidate_path_refuses(self) -> None:
        repo = self.clone()
        self.commit(repo, "ci-hub/bin/tool", "candidate\n")
        result = command(str(SOURCE), "seal", "--clone", str(repo), "--test", "true")
        self.assertEqual(result.returncode, 0, result.stderr)
        primary = self.root / "primary"
        command("git", "clone", str(self.remote), str(primary))
        (primary / "ci-hub/bin/tool").write_text("somebody else\n")
        self.assertEqual(len(TOOL.dirty_candidate_paths(primary, ["ci-hub/bin/tool"])), 1)


if __name__ == "__main__":
    unittest.main()
