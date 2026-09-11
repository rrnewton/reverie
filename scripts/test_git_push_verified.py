#!/usr/bin/env python3
"""Tests for ci-hub/bin/git-push-verified.

The test that matters is REJECTED_PUSH_IS_REPORTED_AS_FAILURE. A fix
demonstrated only against a successful push proves nothing, because the
successful case already worked -- the defect was that a REJECTED push reported
success, since `$?` after `git push ... | tail` reads tail's status.

test_the_old_pipeline_pattern_still_lies is deliberately included. It asserts
the bug, not the fix. If it ever starts failing, the shell has changed under us
and the reason this tool exists needs re-examining.
"""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TOOL = Path(__file__).resolve().parent.parent / "ci-hub" / "bin" / "git-push-verified"


def git(cwd, *args, check=True):
    p = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    if check and p.returncode != 0:
        raise AssertionError(f"git {' '.join(args)} failed in {cwd}: {p.stderr}")
    return p


class Fixture:
    """A bare remote plus two clones, so one can advance the remote behind the other."""

    def __init__(self, root: Path):
        self.root = root
        self.remote = root / "remote.git"
        self.work = root / "work"
        self.other = root / "other"
        git(root, "init", "-q", "--bare", str(self.remote))
        git(root, "clone", "-q", str(self.remote), str(self.work))
        self._ident(self.work)
        (self.work / "f").write_text("one\n")
        git(self.work, "add", "-A")
        git(self.work, "commit", "-qm", "one")
        git(self.work, "push", "-q", "origin", "HEAD:main")

    def _ident(self, repo):
        git(repo, "config", "user.email", "t@example.invalid")
        git(repo, "config", "user.name", "t")

    def advance_remote_behind_us(self):
        git(self.root, "clone", "-q", str(self.remote), str(self.other))
        self._ident(self.other)
        (self.other / "f").write_text("two\n")
        git(self.other, "commit", "-qam", "two")
        git(self.other, "push", "-q", "origin", "HEAD:main")

    def diverge_locally(self):
        (self.work / "f").write_text("local\n")
        git(self.work, "commit", "-qam", "local divergent")

    def remote_main(self) -> str:
        out = git(self.root, "ls-remote", str(self.remote), "refs/heads/main").stdout
        return out.split()[0] if out.strip() else ""

    def local_ref_state(self) -> str:
        return git(self.work, "for-each-ref", "--format=%(refname) %(objectname)").stdout

    def fetch_head_state(self) -> bytes | None:
        path = self.work / ".git" / "FETCH_HEAD"
        return path.read_bytes() if path.exists() else None

    def tool(self, *args):
        return subprocess.run(
            [sys.executable, str(TOOL), *args],
            cwd=self.work, capture_output=True, text=True,
        )


class GitPushVerified(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.fx = Fixture(Path(self._tmp.name))

    def tearDown(self):
        self._tmp.cleanup()

    def test_rejected_push_is_reported_as_failure(self):
        """THE point of the tool: a non-fast-forward rejection must not read as success."""
        self.fx.advance_remote_behind_us()
        self.fx.diverge_locally()
        before = self.fx.remote_main()
        refs_before = self.fx.local_ref_state()
        fetch_head_before = self.fx.fetch_head_state()

        r = self.fx.tool("origin", "HEAD:main")

        self.assertNotEqual(r.returncode, 0, "a rejected push must not exit 0")
        self.assertEqual(r.returncode, 2, "push failure is exit 2")
        self.assertIn("NOTHING LANDED", r.stderr)
        self.assertEqual(self.fx.remote_main(), before, "remote must be unchanged")
        self.assertEqual(
            refs_before,
            self.fx.local_ref_state(),
            "reading a remote commit must not move any local ref",
        )
        self.assertEqual(
            fetch_head_before,
            self.fx.fetch_head_state(),
            "reading a remote commit must not rewrite FETCH_HEAD",
        )

    def test_the_old_pipeline_pattern_still_lies(self):
        """Asserts the BUG, so the tool's reason for existing stays visible.

        `git push ... | tail` then `$?` reads tail's status. This is why the
        rejection above was reported as a landing twice in one hour.
        """
        self.fx.advance_remote_behind_us()
        self.fx.diverge_locally()
        p = subprocess.run(
            "git push origin HEAD:main 2>&1 | tail -2",
            cwd=self.fx.work, shell=True, capture_output=True, text=True,
        )
        self.assertEqual(
            p.returncode, 0,
            "if this ever fails, the pipeline trap is gone and this tool can be revisited",
        )

    def test_successful_push_is_verified_against_the_remote(self):
        (self.fx.work / "f").write_text("three\n")
        git(self.fx.work, "commit", "-qam", "three")
        expected = git(self.fx.work, "rev-parse", "HEAD").stdout.strip()

        r = self.fx.tool("origin", "HEAD:main")

        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("VERIFIED", r.stdout)
        self.assertEqual(self.fx.remote_main(), expected)

    def test_unresolvable_local_ref_is_usage_failure_not_success(self):
        r = self.fx.tool("origin", "no-such-ref-here:main")
        self.assertEqual(r.returncode, 1)
        self.assertIn("cannot resolve local", r.stderr)

    def test_missing_refspec_is_rejected(self):
        r = self.fx.tool("origin")
        self.assertEqual(r.returncode, 1)

    def test_branch_shorthand_is_expanded_to_a_full_ref(self):
        (self.fx.work / "f").write_text("branchy\n")
        git(self.fx.work, "commit", "-qam", "branchy")
        r = self.fx.tool("origin", "HEAD:feature-x")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("refs/heads/feature-x", r.stdout)


if __name__ == "__main__":
    unittest.main()
