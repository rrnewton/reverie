#!/usr/bin/env python3
"""Both-direction integration tests for the parent-main writer authority."""

from __future__ import annotations

import fcntl
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parent.parent
TRAILER = "[coordinator, Test] [parent-writer-test, devbig999]"


def tagged(subject: str) -> str:
    return f"{subject}\n\n{TRAILER}"


def run(*args: str, cwd: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    merged = dict(os.environ, **(env or {}))
    return subprocess.run(args, cwd=cwd, env=merged, text=True, capture_output=True, check=False)


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = run("git", *args, cwd=repo)
    if check and result.returncode:
        raise AssertionError(result.stdout + result.stderr)
    return result


class ParentMainWriteTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        base = Path(self.temp.name)
        self.remote = base / "origin.git"
        self.repo = base / "repo"
        self.lock = base / "parent-main.lock"
        git(base, "init", "--bare", "--initial-branch=main", str(self.remote))
        git(base, "init", "--initial-branch=main", str(self.repo))
        git(self.repo, "config", "user.name", "Test Writer")
        git(self.repo, "config", "user.email", "writer@example.invalid")
        (self.repo / "notes.md").write_text("seed\n")
        git(self.repo, "add", "notes.md")
        git(self.repo, "commit", "-m", "seed")
        git(self.repo, "remote", "add", "origin", str(self.remote))
        git(self.repo, "push", "-u", "origin", "main")

        (self.repo / "scripts").mkdir()
        shutil.copy2(SOURCE / "scripts/parent-main-write", self.repo / "scripts/parent-main-write")
        shutil.copy2(
            SOURCE / "scripts/check_commit_attribution.py",
            self.repo / "scripts/check_commit_attribution.py",
        )
        hooks = self.repo / ".githooks"
        hooks.mkdir()
        for name in ("pre-commit", "pre-push", "reference-transaction"):
            shutil.copy2(SOURCE / ".githooks" / name, hooks / name)
        git(self.repo, "config", "core.hooksPath", ".githooks")
        self.env = {
            "HERMIT_PARENT_MAIN_LOCK_PATH": str(self.lock),
            "HERMIT_PARENT_MAIN_NO_PROXY": "1",
            "HERMIT_COMMIT_MACHINE": "devbig999",
        }
        self.seed = git(self.repo, "rev-parse", "HEAD").stdout.strip()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def writer(self, *args: str) -> subprocess.CompletedProcess[str]:
        return run(str(self.repo / "scripts/parent-main-write"), *args, cwd=self.repo, env=self.env)

    def change(self, path: str = "notes.md", text: str = "changed\n") -> None:
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        git(self.repo, "add", path)

    def test_direct_main_commit_is_refused_before_ref_update(self) -> None:
        self.change()
        result = run("git", "commit", "-m", "bypass", "--", "notes.md", cwd=self.repo, env=self.env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing serialized-writer lock receipt", result.stderr)
        self.assertEqual(git(self.repo, "rev-parse", "HEAD").stdout.strip(), self.seed)

    def test_no_verify_cannot_bypass_reference_transaction(self) -> None:
        self.change()
        result = run(
            "git", "commit", "--no-verify", "-m", "bypass", "--", "notes.md",
            cwd=self.repo, env=self.env,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing serialized-writer lock receipt", result.stderr)
        self.assertEqual(git(self.repo, "rev-parse", "HEAD").stdout.strip(), self.seed)

    def test_ordinary_writer_commits_pushes_and_proves_ancestry(self) -> None:
        self.change()
        result = self.writer("commit", "-m", tagged("update notes"), "--", "notes.md")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("ancestry=1/1 mode=ordinary", result.stdout)
        head = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        self.assertEqual(head, git(self.repo, "rev-parse", "origin/main").stdout.strip())

    def test_stale_local_main_is_refused_before_commit(self) -> None:
        other = Path(self.temp.name) / "other"
        git(Path(self.temp.name), "clone", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        (other / "other.md").write_text("remote move\n")
        git(other, "add", "other.md")
        git(other, "commit", "-m", "remote move")
        git(other, "push", "origin", "main")
        self.change()

        result = self.writer("commit", "-m", tagged("stale"), "--", "notes.md")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("local main is not the freshly fetched origin/main", result.stderr)
        self.assertEqual(git(self.repo, "rev-parse", "HEAD").stdout.strip(), self.seed)

    def test_second_writer_is_refused_by_host_mutex(self) -> None:
        with self.lock.open("w") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.writer("sync")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("another parent-main writer owns", result.stderr)

    def test_sensitive_commit_requires_audit_receipt(self) -> None:
        self.change(".githooks/probe", "#!/bin/sh\n")
        refused = self.writer("commit", "-m", tagged("hook repair"), "--", ".githooks/probe")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("retained --audit-reason path", refused.stderr)
        self.assertEqual(git(self.repo, "rev-parse", "HEAD").stdout.strip(), self.seed)

    def test_sensitive_commit_retains_audit_receipt(self) -> None:
        self.change(".githooks/probe", "#!/bin/sh\n")
        reason = "repair hook enforcement"
        allowed = self.writer(
            "commit", "-m", tagged("hook repair"), "--audit-reason", reason, "--", ".githooks/probe"
        )
        self.assertEqual(allowed.returncode, 0, allowed.stdout + allowed.stderr)
        message = git(self.repo, "log", "-1", "--format=%B").stdout
        self.assertIn("Parent-Main-Write-Mode: audited", message)
        self.assertIn(f"Parent-Main-Write-Reason: {reason}", message)
        self.assertIn(f"Parent-Main-Write-Base: {self.seed}", message)
        self.assertEqual(TRAILER, message.rstrip().splitlines()[-1])

    def test_writer_refuses_missing_machine_attribution(self) -> None:
        self.change()
        refused = self.writer("commit", "-m", "untagged", "--", "notes.md")
        self.assertNotEqual(0, refused.returncode)
        self.assertIn("canonical final attribution", refused.stderr)
        self.assertEqual(self.seed, git(self.repo, "rev-parse", "HEAD").stdout.strip())

    def test_clean_slot_direct_push_is_refused_but_wrapper_publish_passes(self) -> None:
        git(self.repo, "switch", "-c", "feature")
        self.change("feature.md", "feature\n")
        git(self.repo, "commit", "-m", tagged("feature"), "--", "feature.md")
        direct = run("git", "push", "origin", "HEAD:refs/heads/main", cwd=self.repo, env=self.env)
        self.assertNotEqual(direct.returncode, 0)
        self.assertIn("missing serialized-writer lock receipt", direct.stderr)

        published = self.writer("publish")
        self.assertEqual(published.returncode, 0, published.stdout + published.stderr)
        self.assertIn("ancestry=1/1 mode=ordinary", published.stdout)

    def test_sensitive_feature_publish_without_commit_receipt_is_refused(self) -> None:
        git(self.repo, "switch", "-c", "feature")
        self.change(".githooks/probe", "#!/bin/sh\n")
        git(self.repo, "commit", "-m", tagged("unaudited hook"), "--", ".githooks/probe")
        result = self.writer("publish", "--audit-reason", "claimed after the fact")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("lacks Parent-Main-Write-Mode trailer", result.stderr)


class UnauthoredReversionTests(ParentMainWriteTests):
    """The near-miss of 2026-08-08, both directions.

    An agent read a shared file, edited one field, and its CONTENT was later
    committed onto a newer base. The commit carried the authored hunk plus a
    silent reversion of everything that had landed in that file meanwhile. The
    second hunk is invisible in a diff against the base the author read -- the
    diff every review habit inspects -- and only appears against origin/main.
    """

    def advance_main(self, text: str, message: str) -> str:
        """Land a commit on main WITHOUT the writer, so each test takes the host
        mutex exactly once (two writer calls in one test contend on it)."""
        self.change("shared.txt", text)
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "commit", "-m", tagged(message), "--", "shared.txt",
        )
        git(self.repo, "-c", "core.hooksPath=/dev/null", "push", "origin", "HEAD:main")
        return git(self.repo, "rev-parse", "HEAD").stdout.strip()

    def land_colleague_change(self) -> None:
        self.advance_main("alpha\nBETA_original\ngamma\n", "seed shared")
        self.advance_main("alpha\nBETA_LANDED_BY_COLLEAGUE\ngamma\n", "colleague lands BETA")

    def test_negative_transplanted_stale_content_is_refused_and_names_the_lost_line(self) -> None:
        self.land_colleague_change()
        # The author's content was read BEFORE the colleague's line landed, then
        # written out whole onto the current base. Their own diff shows only the
        # added line; the reversion is invisible to them.
        self.change("shared.txt", "alpha\nBETA_original\ngamma\nDELTA_authored\n")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("add DELTA"), "--", "shared.txt")
        result = self.writer("publish")
        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("would revert landed work", combined)
        # It must name WHAT is lost, not merely that something is.
        self.assertIn("BETA_LANDED_BY_COLLEAGUE", combined)

    def test_positive_current_tree_whole_file_commit_still_publishes(self) -> None:
        self.land_colleague_change()
        self.change("shared.txt", "alpha\nBETA_LANDED_BY_COLLEAGUE\ngamma\nDELTA_authored\n")
        result = self.writer("commit", "-m", tagged("add DELTA from a current tree"), "--", "shared.txt")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("PARENT_MAIN_WRITE published=", result.stdout)

    def test_positive_stale_base_then_rebase_does_not_cry_wolf(self) -> None:
        """THE test that decides whether this guard survives contact.

        Committing on a stale base and REBASING is safe -- git replays only the
        authored diff -- and it is the common case. A guard that keys on tree
        staleness rather than on content lineage would fire here, be recognised
        as noise, and be switched off within a day.
        """
        stale_base = self.advance_main("alpha\nBETA_original\ngamma\n", "seed shared")
        # Colleague lands on main while the author works from the older base.
        tip = self.advance_main("alpha\nBETA_LANDED_BY_COLLEAGUE\ngamma\n", "colleague lands BETA")
        git(self.repo, "checkout", "-q", "-B", "author", stale_base)
        self.change("shared.txt", "alpha\nBETA_original\ngamma\nDELTA_authored\n")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("add DELTA on a stale base"), "--", "shared.txt")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "rebase", tip)
        rebased = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        result = self.writer("publish", rebased)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("PARENT_MAIN_WRITE published=", result.stdout)

    def test_author_editing_the_same_region_is_a_real_edit_not_a_reversion(self) -> None:
        self.land_colleague_change()
        self.change("shared.txt", "alpha\nBETA_rewritten_by_the_author\ngamma\n")
        result = self.writer("commit", "-m", tagged("rewrite the line main touched"), "--", "shared.txt")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_documented_opt_out_allows_a_deliberate_reversion(self) -> None:
        """One writer call per test, deliberately.

        Two invocations in a single test contend on the host mutex: a backgrounded
        hook inherits the writer's lock file descriptor and keeps the flock after
        the writer itself has exited. That is a real property of this harness, not
        of the guard, and the refusal side is already proven by
        test_negative_transplanted_stale_content_is_refused_and_names_the_lost_line
        against byte-identical inputs.
        """
        self.land_colleague_change()
        self.change("shared.txt", "alpha\nBETA_original\ngamma\nDELTA_authored\n")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("add DELTA"), "--", "shared.txt")
        self.env["HERMIT_PARENT_MAIN_ALLOW_REVERT"] = "1"
        result = self.writer("publish")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("reversion guard DISABLED", result.stdout + result.stderr)


class PublishedResidueTests(ParentMainWriteTests):
    """The 2026-08-08 freeze, both directions.

    The shared parent sat pinned while origin/main advanced six commits, so the
    fleet's only serialized landing path was shut and the blockage belonged to
    nobody. The cause was residue left by agents whose tasks had already CLOSED,
    whose bytes ALREADY matched origin/main. Git blocks the fast-forward anyway
    because it compares the working tree against the INDEX, not the target.
    """

    def freeze(self, planted: str | None = None) -> str:
        """Leave local main behind origin/main with residue on disk.

        Returns the fetched target sha. `planted` is written to work.txt and is
        the only thing that should ever change the verdict.
        """
        self.change("shared.txt", "v1\n")
        self.change("work.txt", "w1\n")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("seed shared"))
        git(self.repo, "-c", "core.hooksPath=/dev/null", "push", "origin", "HEAD:main")
        base = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        self.change("shared.txt", "v2\n")
        self.change("work.txt", "w2\n")
        self.change("added.md", "new\n")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("origin advances"))
        git(self.repo, "-c", "core.hooksPath=/dev/null", "push", "origin", "HEAD:main")
        target = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        # Local main falls behind, exactly as the shared parent did.
        git(self.repo, "-c", "core.hooksPath=/dev/null", "reset", "--hard", base)
        # Residue: bytes already identical to what is published.
        (self.repo / "shared.txt").write_text("v2\n")
        (self.repo / "added.md").write_text("new\n")
        (self.repo / "work.txt").write_text(planted if planted is not None else "w2\n")
        return target

    def test_positive_byte_identical_residue_does_not_block_a_sync(self) -> None:
        target = self.freeze()
        result = self.writer("sync")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn("residue adopted=3", result.stdout)
        # Not one byte on disk may have moved.
        self.assertEqual("v2\n", (self.repo / "shared.txt").read_text())
        self.assertEqual("w2\n", (self.repo / "work.txt").read_text())
        self.assertEqual("new\n", (self.repo / "added.md").read_text())

    def test_negative_a_genuinely_modified_file_still_blocks_and_nothing_moves(self) -> None:
        self.freeze(planted="GENUINE UNCOMMITTED WORK\n")
        head_before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        index_before = git(self.repo, "write-tree").stdout.strip()
        result = self.writer("sync")
        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("work.txt", combined)
        self.assertIn("real", combined)
        # The refusal must be TOTAL: a first pass that stages as it goes would
        # leave the index half-adopted here, which is the residue class this
        # function exists to remove, created on the failure path.
        self.assertEqual(head_before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(index_before, git(self.repo, "write-tree").stdout.strip())
        self.assertEqual("", git(self.repo, "diff", "--cached", "--name-only").stdout.strip())
        self.assertEqual("GENUINE UNCOMMITTED WORK\n", (self.repo / "work.txt").read_text())

    def test_negative_no_automated_path_refreshes_an_entry_whose_bytes_differ(self) -> None:
        # The same planted file, asserted from the other side: the differing path
        # must never appear in the adoption log, and adoption must not be claimed.
        self.freeze(planted="DIFFERENT\n")
        result = self.writer("sync")
        combined = result.stdout + result.stderr
        self.assertNotIn("adopted published residue (bytes identical to", combined.split("REFUSED")[0].replace("shared.txt", ""))
        self.assertNotIn("residue adopted=", result.stdout)

    def test_neutral_a_clean_current_tree_reports_zero_and_still_syncs(self) -> None:
        # No cry-wolf: the common case must be silent about residue, not noisy.
        self.change("notes.md", "changed\n")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("ordinary"))
        git(self.repo, "-c", "core.hooksPath=/dev/null", "push", "origin", "HEAD:main")
        result = self.writer("sync")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("already-current=", result.stdout)

if __name__ == "__main__":
    unittest.main()
