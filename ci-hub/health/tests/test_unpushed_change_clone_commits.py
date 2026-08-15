#!/usr/bin/env python3
"""Bracket the change-clone gate's SCOPE and the FRESHNESS of its evidence.

This gate must ask exactly the question `ci-hub-clone-change.py dispose` asks.
When it did not, the two disagreed in public: measured 2026-08-15 over the 28
clones then present, the gate's `--all` enumeration counted 77 commits where the
own-branch count was 4, and all 4 of those were themselves already on their
remotes and appeared only because the clones' remote-tracking refs were stale.
A gate whose every alarm is false trains the reflex to ignore it, which is the
same way a floor stops holding.

Three properties, because a fix to only one of them still leaves a false report:

1.  SCOPE IS OWN-BRANCH. A fetch-destination ref such as
    `refs/pr-landing-planner/pr-<n>` holds commits that are on the remote by
    construction and must not be counted.
2.  EVIDENCE IS FRESH. A commit already on the remote must not be reported just
    because this clone last fetched before it landed.
3.  UNREACHABLE IS NOT CLEAN. If the remote cannot be reached the condition is
    undetermined, and the gate says so rather than printing a count nobody can
    date.

The refusal direction is bracketed against all three: genuinely unpushed
own-branch work is still reported, whether it sits on the checked-out branch, on
a branch that is not HEAD, or on a detached HEAD.

Fixtures are inert: a local bare repo stands in for `origin`, so nothing here
reaches the network or touches a real change clone.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HEALTH = Path(__file__).resolve().parents[1]
GATE = HEALTH / "unpushed_change_clone_commits.py"
MANIFEST = "ci-hub-change-manifest.json"


def run(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, cwd=None if cwd is None else str(cwd),
                          capture_output=True, text=True)


def git(repo: Path, *args: str) -> str:
    out = run("git", "-C", str(repo), *args)
    if out.returncode != 0:
        raise AssertionError(out.stderr)
    return out.stdout.strip()


class Fixture(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.search = self.root / "clones"
        self.search.mkdir()
        self.origin = self.root / "origin.git"
        run("git", "init", "-q", "--bare", str(self.origin))
        seed = self.root / "seed"
        run("git", "init", "-q", "-b", "main", str(seed))
        git(seed, "config", "user.email", "t@example.invalid")
        git(seed, "config", "user.name", "t")
        (seed / "f.txt").write_text("seed\n")
        git(seed, "add", "--", "f.txt")
        git(seed, "commit", "-q", "-m", "seed")
        git(seed, "remote", "add", "origin", str(self.origin))
        git(seed, "push", "-q", "origin", "main")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def clone(self, name: str = "dev-hermit-fixture") -> Path:
        path = self.search / name
        run("git", "clone", "-q", "--single-branch", "--branch", "main",
            str(self.origin), str(path))
        git(path, "config", "user.email", "t@example.invalid")
        git(path, "config", "user.name", "t")
        (path / ".git" / MANIFEST).write_text(json.dumps({"task": "fixture-task"}))
        git(path, "switch", "-q", "-c", "ci-hub/fixture-task")
        return path

    def commit(self, repo: Path, text: str) -> str:
        (repo / "f.txt").write_text(text)
        git(repo, "add", "--", "f.txt")
        git(repo, "commit", "-q", "-m", text)
        return git(repo, "rev-parse", "HEAD")

    def gate(self) -> tuple[int, str]:
        out = run(sys.executable, str(GATE), "--search-root", str(self.search))
        return out.returncode, out.stdout

    def gate_json(self) -> tuple[int, dict]:
        out = run(sys.executable, str(GATE), "--search-root", str(self.search), "--json")
        return out.returncode, json.loads(out.stdout)

    # ---- reported: genuinely unpushed own-branch work, in all three shapes ----

    def test_reports_an_unpushed_commit_on_the_checked_out_branch(self) -> None:
        repo = self.clone()
        head = self.commit(repo, "unpushed\n")
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("holding=1", text)
        self.assertIn(head, text)
        self.assertIn("exists on no remote ref", text)

    def test_reports_unpushed_work_on_a_branch_that_is_not_head(self) -> None:
        repo = self.clone()
        git(repo, "switch", "-q", "-c", "side")
        head = self.commit(repo, "parked on a side branch\n")
        git(repo, "switch", "-q", "ci-hub/fixture-task")
        # Scoping to HEAD alone would miss this entirely.
        self.assertEqual(git(repo, "rev-list", "HEAD", "--not", "--remotes"), "")
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("holding=1", text)
        self.assertIn(head, text)

    def test_reports_unpushed_work_on_a_detached_head(self) -> None:
        repo = self.clone()
        head = self.commit(repo, "left on a detached HEAD\n")
        git(repo, "switch", "-q", "--detach", head)
        git(repo, "branch", "-q", "-D", "ci-hub/fixture-task")
        # Scoping to --branches alone would miss this entirely.
        self.assertEqual(git(repo, "rev-list", "--branches", "--not", "--remotes"), "")
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("holding=1", text)
        self.assertIn(head, text)

    # ---- not reported: on the remote, whatever this clone last saw ----

    def test_a_fetch_destination_ref_is_not_counted(self) -> None:
        repo = self.clone()
        git(repo, "switch", "-q", "-c", "scratch")
        fetched = self.commit(repo, "a fetched pull-request head\n")
        git(repo, "update-ref", "refs/pr-landing-planner/pr-1", fetched)
        git(repo, "switch", "-q", "ci-hub/fixture-task")
        git(repo, "branch", "-q", "-D", "scratch")
        # Precondition: the previous `--all` enumeration counted this and alarmed.
        self.assertIn(fetched, git(repo, "rev-list", "--exclude=refs/tags/*", "--all",
                                   "--not", "--remotes").split())
        code, text = self.gate()
        self.assertEqual(code, 0, text)
        self.assertIn("holding=0", text)
        self.assertIn("no change clone holds unpushed work", text)

    def test_a_commit_already_on_the_remote_is_not_counted_when_the_snapshot_is_stale(self) -> None:
        repo = self.clone()
        snapshot = git(repo, "rev-parse", "refs/remotes/origin/main")
        head = self.commit(repo, "pushed, but not seen yet\n")
        git(repo, "push", "-q", "origin", f"{head}:refs/heads/main")
        # Rewind only the remote-tracking ref, reproducing the measured state of
        # dev-hermit-p1-a-producer-path: on the remote, absent from the snapshot.
        git(repo, "update-ref", "refs/remotes/origin/main", snapshot)
        self.assertEqual(git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes"), head)
        code, text = self.gate()
        self.assertEqual(code, 0, text)
        self.assertIn("holding=0", text)

    # ---- unreachable: undetermined, and never silently clean ----

    def test_an_unreachable_remote_is_undetermined_not_clean(self) -> None:
        repo = self.clone()
        # Nothing is at risk by this clone's own snapshot, which is exactly when
        # an unverifiable answer is tempting to round down to "clean".
        self.assertEqual(git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes"), "")
        git(repo, "remote", "set-url", "origin", str(self.root / "absent.git"))
        code, text = self.gate()
        self.assertEqual(code, 1)
        self.assertIn("undetermined=1", text)
        self.assertIn("state=NO_RESULT", text)
        self.assertIn("NO_RESULT/remote-unreachable", text)
        self.assertNotIn("no change clone holds unpushed work", text)

    def test_json_carries_reachability_per_clone(self) -> None:
        repo = self.clone()
        code, payload = self.gate_json()
        self.assertEqual(code, 0)
        self.assertEqual(payload["undetermined"], 0)
        self.assertTrue(payload["rows"][0]["remote_reachable"])
        git(repo, "remote", "set-url", "origin", str(self.root / "absent.git"))
        code, payload = self.gate_json()
        self.assertEqual(code, 1)
        self.assertEqual(payload["undetermined"], 1)
        self.assertFalse(payload["rows"][0]["remote_reachable"])

    def test_a_clone_with_no_manifest_is_not_inspected(self) -> None:
        repo = self.clone()
        self.commit(repo, "unpushed\n")
        (repo / ".git" / MANIFEST).unlink()
        code, text = self.gate()
        self.assertEqual(code, 0, text)
        self.assertIn("clones=0", text)
        self.assertIn("no change clones present", text)


if __name__ == "__main__":
    unittest.main()
