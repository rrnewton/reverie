#!/usr/bin/env python3
"""THE PLANTED CASE: a reconcile that drops one hunk while keeping the SHA an ancestor.

The old check (`git merge-base --is-ancestor`) MUST PASS it. The new check MUST CATCH it.
If both agree, the new check adds nothing; if the old one already failed, the defect was
never real. So each test that matters asserts BOTH verdicts on the same planted tree.

Everything runs in a throwaway repo under mktemp. Nothing touches real state.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import content_presence as cp  # noqa: E402


def git(repo: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=False
    )
    if proc.returncode != 0:
        raise AssertionError(f"git {args}: {proc.stderr or proc.stdout}")
    return proc.stdout.strip()


def is_ancestor(repo: Path, sha: str, target: str) -> bool:
    """THE OLD CHECK, verbatim — the one the whole drain currently rests on."""
    return (
        subprocess.run(
            ["git", "-C", str(repo), "merge-base", "--is-ancestor", sha, target],
            capture_output=True,
            check=False,
        ).returncode
        == 0
    )


class ContentPresenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.r = Path(self.tmp.name) / "repo"
        self.r.mkdir()
        git(self.r, "init", "-q", "-b", "main")
        git(self.r, "config", "user.email", "t@t")
        git(self.r, "config", "user.name", "t")
        # A file with two well-separated regions, so a commit touching both yields TWO
        # hunks and a reconcile can drop exactly one of them.
        body = ["top"] + [f"pad{i}" for i in range(20)] + ["bottom"]
        (self.r / "gate.py").write_text("\n".join(body) + "\n")
        git(self.r, "add", "gate.py")
        git(self.r, "commit", "-qm", "base")
        self.base = git(self.r, "rev-parse", "HEAD")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def _two_hunk_commit(self) -> str:
        body = (self.r / "gate.py").read_text().splitlines()
        body[0] = "top CHANGED-A"
        body[-1] = "bottom CHANGED-B"
        (self.r / "gate.py").write_text("\n".join(body) + "\n")
        git(self.r, "add", "gate.py")
        git(self.r, "commit", "-qm", "feature: change both ends")
        return git(self.r, "rev-parse", "HEAD")

    # ---------------- the planted defect -------------------------------------------

    def test_PLANTED_lossy_reconcile_old_check_PASSES_new_check_CATCHES(self) -> None:
        """The whole point. Ancestry says landed; the content is half gone."""
        feature = self._two_hunk_commit()
        git(self.r, "branch", "-q", "feature-ref", feature)

        # Reconcile: a merge that KEEPS the feature commit reachable but restores one
        # region to its pre-feature content — exactly what `checkout --ours` on a
        # conflicted region does.
        git(self.r, "checkout", "-q", "-b", "reconciled", self.base)
        git(self.r, "merge", "-q", "--no-commit", "--no-ff", feature)
        body = (self.r / "gate.py").read_text().splitlines()
        body[-1] = "bottom"  # DROP the second hunk, keep the first
        (self.r / "gate.py").write_text("\n".join(body) + "\n")
        git(self.r, "add", "gate.py")
        git(self.r, "commit", "-qm", "reconcile: resolve toward base for the tail")
        target = git(self.r, "rev-parse", "HEAD")

        # THE OLD CHECK PASSES — the SHA is genuinely reachable.
        self.assertTrue(
            is_ancestor(self.r, feature, target),
            "precondition: the planted case must keep the SHA an ancestor, otherwise "
            "ancestry would already have caught it and there is no defect to fix",
        )

        # THE NEW CHECK CATCHES IT.
        res = cp.check(self.r, feature, target)
        self.assertEqual(res.verdict, cp.CONTENT_LOST, res.render())
        self.assertTrue(res.is_ancestor)
        self.assertEqual(res.hunks_total, 2, res.render())
        self.assertEqual(res.hunks_present, 1, res.render())
        self.assertEqual(len(res.missing), 1)
        self.assertEqual(res.exit_code(), cp.EXIT_CONTENT_LOST)

    # ---------------- the positive control: it must not cry wolf --------------------

    def test_intact_merge_is_PRESENT_not_a_false_alarm(self) -> None:
        """A guard that flags everything is useless. An honest merge must read PRESENT."""
        feature = self._two_hunk_commit()
        git(self.r, "checkout", "-q", "-b", "clean-reconcile", self.base)
        git(self.r, "merge", "-q", "--no-ff", "--no-edit", feature)
        target = git(self.r, "rev-parse", "HEAD")

        self.assertTrue(is_ancestor(self.r, feature, target))
        res = cp.check(self.r, feature, target)
        self.assertEqual(res.verdict, cp.PRESENT, res.render())
        self.assertEqual(res.hunks_present, res.hunks_total)
        self.assertEqual(res.hunks_total, 2)
        self.assertEqual(res.exit_code(), cp.EXIT_OK)

    def test_ambient_git_dir_cannot_redirect_content_check(self) -> None:
        feature = self._two_hunk_commit()
        git(self.r, "checkout", "-q", "-b", "ambient-clean", self.base)
        git(self.r, "merge", "-q", "--no-ff", "--no-edit", feature)
        target = git(self.r, "rev-parse", "HEAD")

        wrong = Path(self.tmp.name) / "wrong"
        subprocess.run(["git", "init", "-q", str(wrong)], check=True)
        git(wrong, "config", "user.name", "Wrong Object Store")
        git(wrong, "config", "user.email", "wrong@example.invalid")
        (wrong / "wrong.txt").write_text("wrong\n")
        git(wrong, "add", "wrong.txt")
        git(wrong, "commit", "-qm", "wrong")
        wrong_head = git(wrong, "rev-parse", "HEAD")

        env = dict(os.environ)
        env["GIT_DIR"] = str(wrong / ".git")
        unsafe = subprocess.run(
            ["git", "-C", str(self.r), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
            env=env,
        )
        self.assertEqual(wrong_head, unsafe.stdout.strip())

        with mock.patch.dict(os.environ, env, clear=True):
            res = cp.check(self.r, feature, target)
        self.assertEqual(cp.PRESENT, res.verdict, res.render())

    def test_unrelated_later_commit_does_not_trip_it(self) -> None:
        """Churn elsewhere in the repo must not read as loss."""
        feature = self._two_hunk_commit()
        (self.r / "other.txt").write_text("unrelated\n")
        git(self.r, "add", "other.txt")
        git(self.r, "commit", "-qm", "unrelated later work")
        target = git(self.r, "rev-parse", "HEAD")
        res = cp.check(self.r, feature, target)
        self.assertEqual(res.verdict, cp.PRESENT, res.render())

    # ---------------- the other two verdicts ----------------------------------------

    def test_non_ancestor_is_ABSENT_and_agrees_with_the_old_check(self) -> None:
        git(self.r, "checkout", "-q", "-b", "side", self.base)
        (self.r / "side.txt").write_text("side\n")
        git(self.r, "add", "side.txt")
        git(self.r, "commit", "-qm", "side work")
        side = git(self.r, "rev-parse", "HEAD")

        self.assertFalse(is_ancestor(self.r, side, self.base))
        res = cp.check(self.r, side, self.base)
        self.assertEqual(res.verdict, cp.ABSENT)
        self.assertFalse(res.is_ancestor)
        # ABSENT is not an error: ancestry already answered it correctly.
        self.assertEqual(res.exit_code(), cp.EXIT_OK)

    def test_zero_hunk_commit_is_INDETERMINATE_not_a_vacuous_PASS(self) -> None:
        """A 0/0 content check is a no-result. It must never read as PRESENT."""
        git(self.r, "commit", "-q", "--allow-empty", "-m", "empty bookkeeping")
        empty = git(self.r, "rev-parse", "HEAD")
        res = cp.check(self.r, empty, "HEAD")
        self.assertEqual(res.verdict, cp.INDETERMINATE, res.render())
        self.assertEqual(res.hunks_total, 0)
        self.assertEqual(res.exit_code(), cp.EXIT_INDETERMINATE)
        self.assertNotEqual(res.verdict, cp.PRESENT)

    # ---------------- the two checks must actually disagree somewhere ---------------

    def test_the_new_check_is_NOT_merely_ancestry_rebadged(self) -> None:
        """If the two never disagree, the new one adds nothing.

        Builds both cases and asserts ancestry gives the SAME answer for both while
        content_presence SEPARATES them. That is the whole value proposition, so it is
        asserted rather than assumed.
        """
        feature = self._two_hunk_commit()

        git(self.r, "checkout", "-q", "-b", "good", self.base)
        git(self.r, "merge", "-q", "--no-ff", "--no-edit", feature)
        good = git(self.r, "rev-parse", "HEAD")

        git(self.r, "checkout", "-q", "-b", "bad", self.base)
        git(self.r, "merge", "-q", "--no-commit", "--no-ff", feature)
        body = (self.r / "gate.py").read_text().splitlines()
        body[-1] = "bottom"
        (self.r / "gate.py").write_text("\n".join(body) + "\n")
        git(self.r, "add", "gate.py")
        git(self.r, "commit", "-qm", "lossy reconcile")
        bad = git(self.r, "rev-parse", "HEAD")

        # OLD CHECK: identical verdict on both. It cannot tell them apart.
        self.assertTrue(is_ancestor(self.r, feature, good))
        self.assertTrue(is_ancestor(self.r, feature, bad))

        # NEW CHECK: separates them.
        self.assertEqual(cp.check(self.r, feature, good).verdict, cp.PRESENT)
        self.assertEqual(cp.check(self.r, feature, bad).verdict, cp.CONTENT_LOST)


if __name__ == "__main__":
    unittest.main(verbosity=2)
