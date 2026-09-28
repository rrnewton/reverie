#!/usr/bin/env python3
"""Bracketed tests for the rescue-ref reconciler, on throwaway scratch repos.

The scenario under test is the 2026-08-06 incident: on shared parent main,
agent A commits, agent B commits on top, then B runs `git reset HEAD~1` twice
to redo B's own work -- and the second reset silently takes A's commit with it.

Both directions are exercised, because only one of them is interesting on its
own. A guard that fires on every reset is worse than no guard: it gets muted,
and then the real drop is invisible again. So the positive control (a
legitimate reset must stay SILENT) is load-bearing, not decoration.

Every repo here is created in a tmpdir and thrown away. Nothing touches the
real parent, and nothing pushes.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).resolve().parents[1] / "rescue_ref_reconcile.py"
SPEC = importlib.util.spec_from_file_location("rescue_ref_reconcile", MODULE_PATH)
assert SPEC and SPEC.loader
reconcile = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reconcile)


def git(repo: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, text=True, check=True, timeout=120,
    )
    return proc.stdout.strip()


def commit(repo: Path, name: str, body: str, subject: str) -> str:
    (repo / name).write_text(body)
    git(repo, "add", name)
    git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", subject)
    return git(repo, "rev-parse", "HEAD")


class Scratch:
    """A repo with a `main`, plus rescue refs kept as local `rescue/auto-*`.

    The reconciler reads `refs/heads/rescue/*` as well as the remote-tracking
    ones, so a single local repo reproduces the shape without a network.
    """

    def __init__(self, tmp: str):
        self.repo = Path(tmp) / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        self.base = commit(self.repo, "seed.txt", "seed\n", "seed")

    def rescue(self, sha: str) -> None:
        git(self.repo, "branch", f"rescue/auto-{sha[:7]}", sha)

    def run(self, *extra: str):
        argv = ["--repo", str(self.repo), "--main-ref", "main", *extra]
        return reconcile.main(argv)

    def verdicts(self) -> dict[str, str]:
        out = {}
        rc, refs = reconcile.git(
            str(self.repo), "for-each-ref", "--format=%(refname:short) %(objectname)",
            "refs/heads/rescue/auto-*",
        )
        index = reconcile.index_main(str(self.repo), "main", 200)
        for line in refs.splitlines():
            _, sha = line.split()
            row = reconcile.classify(str(self.repo), sha, "main", index)
            out[sha] = row["verdict"]
        return out


class RescueReconcileTest(unittest.TestCase):
    def test_negative_collateral_reset_is_detected_and_names_the_dropped_sha(self):
        """THE INCIDENT. Two resets, and A's commit is collateral."""
        with tempfile.TemporaryDirectory() as tmp:
            s = Scratch(tmp)
            a_sha = commit(s.repo, "a.txt", "A work\n", "agent A: real work")
            b_sha = commit(s.repo, "b.txt", "B work\n", "agent B: own work")
            # Rescue refs exist for both, as the producer would have made them.
            s.rescue(a_sha)
            s.rescue(b_sha)

            git(s.repo, "reset", "-q", "--hard", "HEAD~1")  # drops B's own -- intended
            git(s.repo, "reset", "-q", "--hard", "HEAD~1")  # drops A's -- COLLATERAL
            self.assertEqual(s.base, git(s.repo, "rev-parse", "HEAD"))
            # B recommits only their own work, exactly as in the incident.
            commit(s.repo, "b.txt", "B work\n", "agent B: own work")

            verdicts = s.verdicts()
            self.assertEqual(
                "UNRECONCILED", verdicts[a_sha],
                "agent A's dropped commit must be reported; it is on no branch and "
                "nothing on main carries its content",
            )
            # And the check FAILS, so it can gate rather than merely inform.
            self.assertEqual(1, s.run(), "an unreconciled drop must exit nonzero")

    def test_negative_reports_the_exact_dropped_sha_not_just_a_count(self):
        """A count is not actionable; the operator needs the SHA to cherry-pick."""
        with tempfile.TemporaryDirectory() as tmp:
            s = Scratch(tmp)
            a_sha = commit(s.repo, "a.txt", "A work\n", "agent A: real work")
            s.rescue(a_sha)
            git(s.repo, "reset", "-q", "--hard", "HEAD~1")

            index = reconcile.index_main(str(s.repo), "main", 200)
            row = reconcile.classify(str(s.repo), a_sha, "main", index)
            self.assertEqual("UNRECONCILED", row["verdict"])
            self.assertEqual(a_sha, row["sha"])
            self.assertIn("agent A", row["subject"])

    def test_positive_legitimate_self_reset_stays_silent(self):
        """POSITIVE CONTROL, and the reason this guard is usable.

        B resets away B's OWN commit and never recommits it -- a normal "undo
        my last commit". Nobody else's work was on top. The guard must say
        nothing, because a guard that fires here gets muted and then misses the
        collateral case above.
        """
        with tempfile.TemporaryDirectory() as tmp:
            s = Scratch(tmp)
            b_sha = commit(s.repo, "b.txt", "B work\n", "agent B: own work")
            s.rescue(b_sha)
            git(s.repo, "reset", "-q", "--hard", "HEAD~1")
            # B redoes the work: IDENTICAL DIFF, different subject, so the new
            # commit gets a different SHA. Reworded deliberately -- an identical
            # subject AND tree AND parent within the same second reproduces the
            # original SHA exactly, which made an earlier version of this test
            # pass as LANDED and never exercise the recovery path at all.
            redone = commit(s.repo, "b.txt", "B work\n", "agent B: own work (redo)")
            self.assertNotEqual(b_sha, redone, "fixture must produce a NEW sha")

            verdicts = s.verdicts()
            self.assertEqual(
                "RECOVERED", verdicts[b_sha],
                "same content is back on main under a new SHA, so this is not a loss",
            )
            index = reconcile.index_main(str(s.repo), "main", 200)
            row = reconcile.classify(str(s.repo), b_sha, "main", index)
            self.assertEqual("patch-id", row.get("strength"),
                             "identical content must match by patch-id, not merely by subject")
            self.assertEqual(0, s.run(), "a recovered commit must not fail the check")

    def test_positive_landed_commit_is_silent(self):
        """The ordinary case: the rescue ref is just an ancestor of main."""
        with tempfile.TemporaryDirectory() as tmp:
            s = Scratch(tmp)
            sha = commit(s.repo, "a.txt", "A\n", "agent A: landed normally")
            s.rescue(sha)
            self.assertEqual({sha: "LANDED"}, s.verdicts())
            self.assertEqual(0, s.run())

    def test_stash_and_throwaway_commits_are_not_reported_as_losses(self):
        """Anti-noise: measured on the real repo, these classes were pure noise."""
        with tempfile.TemporaryDirectory() as tmp:
            s = Scratch(tmp)
            stash_sha = commit(s.repo, "s.txt", "x\n", "index on main: deadbeef something")
            canary_sha = commit(s.repo, "c.txt", "y\n", "canary: throwaway commit to verify a check")
            s.rescue(stash_sha)
            s.rescue(canary_sha)
            git(s.repo, "reset", "-q", "--hard", "HEAD~2")

            verdicts = s.verdicts()
            self.assertEqual("NOT-A-LOSS", verdicts[stash_sha])
            self.assertEqual("NOT-A-LOSS", verdicts[canary_sha])
            self.assertEqual(0, s.run(), "stash/throwaway refs must not fail the check")

    def test_baseline_suppresses_known_backlog_but_not_a_new_drop(self):
        """A check that can never go green gets muted, so the baseline matters.

        Both directions: a baselined SHA must not fail, and a NEW drop
        alongside it still must.
        """
        with tempfile.TemporaryDirectory() as tmp:
            s = Scratch(tmp)
            old = commit(s.repo, "old.txt", "old\n", "known backlog item")
            s.rescue(old)
            git(s.repo, "reset", "-q", "--hard", "HEAD~1")

            baseline = Path(tmp) / "baseline.txt"
            baseline.write_text(f"# triaged\n{old}\n")
            self.assertEqual(0, s.run("--baseline", str(baseline)),
                             "a baselined backlog item must not fail the check")

            fresh = commit(s.repo, "new.txt", "new\n", "a NEW dropped commit")
            s.rescue(fresh)
            git(s.repo, "reset", "-q", "--hard", "HEAD~1")
            self.assertEqual(1, s.run("--baseline", str(baseline)),
                             "a new drop must still fail even with a baseline present")

    def test_non_branch_rescue_locations_are_reconciled(self):
        """The producer now writes refs/rescue/ (never a branch) and mirrors a
        verified one under refs/remotes/origin-rescue/. A drop sitting only in
        either location must still be reported, not silently unenumerated."""
        for location in ("refs/remotes/origin-rescue/auto-{short}",
                         "refs/rescue/auto-{short}",
                         "refs/archive/rescue/auto-{short}"):
            with self.subTest(location=location), tempfile.TemporaryDirectory() as tmp:
                s = Scratch(tmp)
                dropped = commit(s.repo, "a.txt", "A work\n", "agent A: real work")
                git(s.repo, "update-ref", location.format(short=dropped[:7]), dropped)
                git(s.repo, "reset", "-q", "--hard", "HEAD~1")
                self.assertEqual(
                    [], git(s.repo, "for-each-ref", "refs/heads/rescue").splitlines(),
                    "the fixture must hold no rescue BRANCH, or this proves nothing",
                )
                self.assertEqual(1, s.run(), f"a drop at {location} must fail the check")
                landed = commit(s.repo, "b.txt", "B\n", "landed work")
                git(s.repo, "update-ref", location.format(short=dropped[:7]), landed)
                self.assertEqual(0, s.run(), "a landed rescue must stay silent")

    def test_one_commit_at_several_locations_is_one_row(self):
        with tempfile.TemporaryDirectory() as tmp:
            s = Scratch(tmp)
            dropped = commit(s.repo, "a.txt", "A work\n", "agent A: real work")
            s.rescue(dropped)
            git(s.repo, "update-ref", f"refs/remotes/origin-rescue/auto-{dropped[:7]}",
                dropped)
            git(s.repo, "reset", "-q", "--hard", "HEAD~1")
            buffer = io.StringIO()
            with contextlib.redirect_stdout(buffer):
                self.assertEqual(1, s.run("--json"))
            report = json.loads(buffer.getvalue())
            self.assertEqual(1, report["total"], report)
            self.assertEqual([dropped], [r["sha"] for r in report["unreconciled"]])


if __name__ == "__main__":
    unittest.main()
