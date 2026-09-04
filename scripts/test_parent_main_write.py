#!/usr/bin/env python3
"""Both-direction integration tests for the parent-main writer authority."""

from __future__ import annotations

import fcntl
import json
import os
from pathlib import Path
import select
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


SOURCE = Path(__file__).resolve().parent.parent
# Captured BEFORE any test prepends a shim directory to PATH.
REAL_GIT = shutil.which("git") or "/usr/bin/git"
REAL_PYTHON = shutil.which("python3") or "/usr/bin/python3"
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
        # Harness-only executables are tracked in the real parent but copied
        # into this minimal repository after its seed commit. Keep them out of
        # the fixture's dirty-path census so automatic mode is tested against
        # planted repository work, not its own test machinery.
        with (self.repo / ".git/info/exclude").open("a") as exclude:
            exclude.write(
                "scripts/\n"
                ".githooks/pre-commit\n"
                ".githooks/pre-push\n"
                ".githooks/reference-transaction\n"
            )
        self.env = {
            "HERMIT_PARENT_MAIN_LOCK_PATH": str(self.lock),
            "HERMIT_PARENT_MAIN_NO_PROXY": "1",
            "HERMIT_COMMIT_MACHINE": "devbig999",
        }
        self.seed = git(self.repo, "rev-parse", "HEAD").stdout.strip()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def writer(self, *args: str, lock: str | Path | None = None) -> subprocess.CompletedProcess[str]:
        """Run the writer with a FRESH host-mutex lock per invocation by default.

        The mutex is deliberately non-reentrant, and a hook can inherit the lock
        file descriptor and hold the flock past the writer's own exit -- so a
        second invocation in the same test may or may not contend depending on
        timing. That made multi-invocation tests pass in suite order and fail in
        isolation, which misleads whoever runs one by name. Isolation is the
        default; the one test that is ABOUT contention asks for the shared lock
        explicitly.
        """
        if lock is None:
            self._lock_seq = getattr(self, "_lock_seq", 0) + 1
            lock = f"{self.lock}.w{self._lock_seq}"
        env = dict(self.env, HERMIT_PARENT_MAIN_LOCK_PATH=str(lock))
        return run(str(self.repo / "scripts/parent-main-write"), *args, cwd=self.repo, env=env)

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
            # THE one test about contention: it must use the shared lock.
            result = self.writer("sync", lock=self.lock)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("another parent-main writer owns", result.stderr)

    def test_sensitive_commit_requires_audit_receipt(self) -> None:
        self.change(".githooks/probe", "#!/bin/sh\n")
        refused = self.writer("commit", "-m", tagged("hook repair"), "--", ".githooks/probe")
        self.assertNotEqual(refused.returncode, 0)
        # ASSERT THE REFUSAL, NOT THE SENTENCE -- AND THE SENTENCE WAS HIDING
        # THE REAL FAILURE. This line used to pin
        # "retained --audit-reason path" (parent-main-write:284). A cheaper
        # check at :173 now short-circuits with different words, so the pinned
        # assertion failed FIRST and the test read as wording drift. It is not:
        # relax the wording and the failure moves to the HEAD check below,
        # where the ref HAS MOVED. The commit is not actually prevented.
        #
        # Cause: verify_audit_receipt is reached only through verify_ref, and
        # the only caller of `parent-main-write verify-ref` was the forward-
        # update block of .githooks/reference-transaction, deleted by 4acd425b.
        # Nothing aborts the ref transaction now, so the refusal arrives after
        # the fact -- nonzero exit and a message, over a write that happened.
        #
        # Left failing deliberately. Restoring that block reverts an owner
        # decision; this assertion is only made to fail on the behaviour rather
        # than on the wording, so nobody "fixes" the string and turns a live
        # unenforced-write defect green.
        self.assertIn("--audit-reason", refused.stderr)
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

    def advance_remote_only(
        self,
        path: str = "remote-only.md",
        text: str = "landed elsewhere\n",
        *,
        force: bool = False,
    ) -> str:
        """Advance origin/main while leaving this checkout clean and behind."""
        other = Path(self.temp.name) / "automatic-sync-publisher"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        target = other / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        if force:
            git(other, "add", "-f", path)
        else:
            git(other, "add", path)
        git(other, "commit", "-m", tagged("remote advances"))
        git(other, "push", "-q", "origin", "HEAD:main")
        return git(other, "rev-parse", "HEAD").stdout.strip()

    def test_automatic_sync_advances_a_clean_ahead_zero_parent(self) -> None:
        target = self.advance_remote_only()
        self.assertEqual("", git(self.repo, "status", "--short").stdout)

        result = self.writer("sync", "--automatic")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn("automatic-boundary dirty=0", result.stdout)

    def test_preserve_dirty_sync_advances_and_reports_both_shas(self) -> None:
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        target = self.advance_remote_only()

        result = self.writer("sync", "--preserve-dirty")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn("state=fast-forwarded", result.stdout)
        self.assertIn(f"before={before}", result.stdout)
        self.assertIn(f"after={target}", result.stdout)
        self.assertIn("overlap=0", result.stdout)
        self.assertNotIn("residue adopted=", result.stdout + result.stderr)

    def test_preserve_dirty_sync_reports_an_already_current_no_op(self) -> None:
        (self.repo / "HANDOFF.md").write_text("unread handoff\n")

        result = self.writer("sync", "--preserve-dirty")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("state=already-current", result.stdout)
        self.assertIn(f"before={self.seed}", result.stdout)
        self.assertIn(f"after={self.seed}", result.stdout)
        self.assertEqual("unread handoff\n", (self.repo / "HANDOFF.md").read_text())

    def test_preserve_dirty_sync_names_a_fetch_failure_and_does_not_move(self) -> None:
        git(self.repo, "remote", "set-url", "origin", str(Path(self.temp.name) / "missing.git"))

        result = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, result.returncode)
        self.assertIn("could not fetch origin/main", result.stderr)
        self.assertIn("nothing was modified", result.stderr)
        self.assertEqual(self.seed, git(self.repo, "rev-parse", "HEAD").stdout.strip())

    def test_preserve_dirty_sync_advances_around_unrelated_dirty_paths(self) -> None:
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        target = self.advance_remote_only()
        (self.repo / "notes.md").write_text("unstaged local work\n")
        (self.repo / "staged.md").write_text("staged local work\n")
        git(self.repo, "add", "staged.md")
        (self.repo / "HANDOFF.md").write_text("unread handoff\n")
        (self.repo / "wpanic").mkdir()
        (self.repo / "wpanic" / "probe.txt").write_text("untracked work\n")
        status_before = git(
            self.repo, "status", "--porcelain=v1", "--untracked-files=all"
        ).stdout

        result = self.writer("sync", "--preserve-dirty")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertNotEqual(before, target)
        self.assertEqual(
            status_before,
            git(self.repo, "status", "--porcelain=v1", "--untracked-files=all").stdout,
        )
        self.assertEqual("unstaged local work\n", (self.repo / "notes.md").read_text())
        self.assertEqual("staged local work\n", (self.repo / "staged.md").read_text())
        self.assertEqual("unread handoff\n", (self.repo / "HANDOFF.md").read_text())
        self.assertEqual("untracked work\n", (self.repo / "wpanic" / "probe.txt").read_text())
        self.assertIn("state=fast-forwarded", result.stdout)
        self.assertIn("overlap=0", result.stdout)
        self.assertNotIn("residue adopted=", result.stdout + result.stderr)

    def test_preserve_dirty_sync_refuses_exact_published_residue_without_adopting_it(self) -> None:
        target = self.freeze()
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        index_before = git(self.repo, "write-tree").stdout.strip()

        result = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("state=skipped reason=path-overlap", combined)
        self.assertIn(f"before={before}", combined)
        self.assertIn(f"after={before}", combined)
        self.assertIn(f"target={target}", combined)
        self.assertIn("shared.txt", combined)
        self.assertIn("work.txt", combined)
        self.assertIn("added.md", combined)
        self.assertNotIn("adopted published residue", combined)
        self.assertEqual(before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(index_before, git(self.repo, "write-tree").stdout.strip())

    def test_preserve_dirty_sync_refuses_an_untracked_ancestor_of_an_incoming_path(self) -> None:
        target = self.advance_remote_only("wpanic", "landed file\n")
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        (self.repo / "wpanic").mkdir()
        (self.repo / "wpanic" / "local.txt").write_text("local work\n")

        result = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("state=skipped reason=path-overlap", combined)
        self.assertIn("incoming=wpanic", combined)
        self.assertIn("dirty=wpanic/local.txt", combined)
        self.assertEqual(before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual("local work\n", (self.repo / "wpanic" / "local.txt").read_text())
        self.assertNotEqual(target, before)

    def test_preserve_dirty_sync_refuses_an_incoming_child_of_an_untracked_repository(self) -> None:
        target = self.advance_remote_only("wpanic/incoming.txt", "landed file\n")
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        nested = self.repo / "wpanic"
        git(self.repo, "init", "--initial-branch=main", str(nested))
        git(nested, "config", "user.name", "Nested")
        git(nested, "config", "user.email", "nested@example.invalid")
        (nested / "local.txt").write_text("local repository work\n")
        git(nested, "add", "local.txt")
        git(nested, "commit", "-m", "local nested work")
        self.assertEqual("wpanic/", git(
            self.repo, "ls-files", "--others", "--exclude-standard"
        ).stdout.strip())

        result = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("state=skipped reason=path-overlap", combined)
        self.assertIn("incoming=wpanic/incoming.txt", combined)
        self.assertIn("dirty=wpanic/", combined)
        self.assertEqual(before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual("local repository work\n", (nested / "local.txt").read_text())
        self.assertNotEqual(target, before)

    def test_preserve_dirty_sync_refuses_an_incoming_path_over_ignored_work(self) -> None:
        (self.repo / ".gitignore").write_text("ignored/\n")
        git(self.repo, "add", ".gitignore")
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "commit", "-m", tagged("ignore local output"),
        )
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "push", "-q", "origin", "HEAD:main",
        )
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        target = self.advance_remote_only("ignored/output.txt", "landed file\n", force=True)
        ignored = self.repo / "ignored" / "output.txt"
        ignored.parent.mkdir()
        ignored.write_text("ignored local work\n")

        result = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("state=skipped reason=path-overlap", combined)
        self.assertIn("incoming=ignored/output.txt", combined)
        self.assertIn("dirty=ignored/", combined)
        self.assertEqual(before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual("ignored local work\n", ignored.read_text())
        self.assertNotEqual(target, before)

    def test_preserve_dirty_sync_refuses_ignored_work_created_after_its_check(self) -> None:
        (self.repo / ".gitignore").write_text("ignored/\n")
        git(self.repo, "add", ".gitignore")
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "commit", "-m", tagged("ignore local output"),
        )
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "push", "-q", "origin", "HEAD:main",
        )
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        target = self.advance_remote_only("ignored/race.txt", "landed file\n", force=True)

        shim_dir = Path(self.temp.name) / "race-bin"
        shim_dir.mkdir()
        shim = shim_dir / "git"
        shim.write_text(
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "prefix=()\n"
            "if [ \"${1:-}\" = -C ]; then prefix=(-C \"$2\"); shift 2; fi\n"
            "if [ \"${1:-}\" = merge ]; then\n"
            "  mkdir -p \"$HERMIT_RACE_ROOT/ignored\"\n"
            "  printf 'ignored local work\\n' >\"$HERMIT_RACE_ROOT/ignored/race.txt\"\n"
            "fi\n"
            "exec \"$HERMIT_REAL_GIT\" \"${prefix[@]}\" \"$@\"\n"
        )
        shim.chmod(0o755)
        self.env.update(
            PATH=f"{shim_dir}:{os.environ.get('PATH', '')}",
            HERMIT_RACE_ROOT=str(self.repo),
            HERMIT_REAL_GIT=REAL_GIT,
        )

        result = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("fast-forward failed", combined)
        self.assertIn("ignored/race.txt", combined)
        self.assertEqual(before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(
            "ignored local work\n",
            (self.repo / "ignored" / "race.txt").read_text(),
        )
        self.assertNotEqual(target, before)

    def test_preserve_dirty_sync_advances_when_the_dirty_gitlink_already_matches_target(self) -> None:
        submodule = Path(self.temp.name) / "hermit-origin"
        git(Path(self.temp.name), "init", "--initial-branch=main", str(submodule))
        git(submodule, "config", "user.name", "Nested")
        git(submodule, "config", "user.email", "nested@example.invalid")
        (submodule / "version.txt").write_text("old\n")
        git(submodule, "add", "version.txt")
        git(submodule, "commit", "-m", "old nested head")
        old_gitlink = git(submodule, "rev-parse", "HEAD").stdout.strip()
        (submodule / "version.txt").write_text("new\n")
        git(submodule, "commit", "-am", "new nested head")
        new_gitlink = git(submodule, "rev-parse", "HEAD").stdout.strip()

        git(self.repo, "clone", "-q", str(submodule), "hermit")
        git(self.repo / "hermit", "checkout", "-q", old_gitlink)
        git(self.repo, "update-index", "--add", "--cacheinfo", f"160000,{old_gitlink},hermit")
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "commit", "-m", tagged("record nested checkout"),
        )
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "push", "-q", "origin", "HEAD:main",
        )
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()

        publisher = Path(self.temp.name) / "gitlink-publisher"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(publisher))
        git(publisher, "config", "user.name", "Other")
        git(publisher, "config", "user.email", "other@example.invalid")
        git(publisher, "update-index", "--cacheinfo", f"160000,{new_gitlink},hermit")
        git(publisher, "commit", "-m", tagged("advance nested checkout"))
        git(publisher, "push", "-q", "origin", "HEAD:main")
        target = git(publisher, "rev-parse", "HEAD").stdout.strip()

        git(self.repo / "hermit", "checkout", "-q", new_gitlink)
        (self.repo / "hermit" / "version.txt").write_text("uncommitted nested work\n")
        (self.repo / "hermit" / "HANDOFF.md").write_text("unread nested handoff\n")
        self.assertIn("hermit", git(self.repo, "status", "--short").stdout)

        result = self.writer("sync", "--preserve-dirty")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        combined = result.stdout + result.stderr
        self.assertIn("gitlink-drift path=hermit", combined)
        self.assertIn("state=already-at-target", combined)
        self.assertIn("gitlink-drift=1", combined)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(new_gitlink, git(self.repo / "hermit", "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(
            "uncommitted nested work\n", (self.repo / "hermit" / "version.txt").read_text()
        )
        self.assertEqual(
            "unread nested handoff\n", (self.repo / "hermit" / "HANDOFF.md").read_text()
        )
        self.assertNotEqual(target, before)

        # The same identity may already be staged in the superproject index.
        # That is still the target's value, and advancing HEAD must preserve the
        # nested checkout and its untracked handoff exactly as above.
        git(self.repo, "-c", "core.hooksPath=/dev/null", "reset", "--hard", before)
        git(self.repo, "update-index", "--cacheinfo", f"160000,{new_gitlink},hermit")
        staged = self.writer("sync", "--preserve-dirty")

        self.assertEqual(0, staged.returncode, staged.stdout + staged.stderr)
        self.assertIn("gitlink-drift path=hermit", staged.stdout + staged.stderr)
        self.assertIn("state=already-at-target", staged.stdout + staged.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(new_gitlink, git(self.repo / "hermit", "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(
            "uncommitted nested work\n", (self.repo / "hermit" / "version.txt").read_text()
        )
        self.assertEqual(
            "unread nested handoff\n", (self.repo / "hermit" / "HANDOFF.md").read_text()
        )

    def test_preserve_dirty_sync_refuses_an_incoming_gitlink_at_a_different_checkout(self) -> None:
        submodule = Path(self.temp.name) / "hermit-origin-mismatch"
        git(Path(self.temp.name), "init", "--initial-branch=main", str(submodule))
        git(submodule, "config", "user.name", "Nested")
        git(submodule, "config", "user.email", "nested@example.invalid")
        (submodule / "version.txt").write_text("old\n")
        git(submodule, "add", "version.txt")
        git(submodule, "commit", "-m", "old nested head")
        old_gitlink = git(submodule, "rev-parse", "HEAD").stdout.strip()
        (submodule / "version.txt").write_text("target\n")
        git(submodule, "commit", "-am", "target nested head")
        target_gitlink = git(submodule, "rev-parse", "HEAD").stdout.strip()
        (submodule / "version.txt").write_text("other\n")
        git(submodule, "commit", "-am", "different nested head")
        different_gitlink = git(submodule, "rev-parse", "HEAD").stdout.strip()

        git(self.repo, "clone", "-q", str(submodule), "hermit")
        git(self.repo / "hermit", "checkout", "-q", old_gitlink)
        git(self.repo, "update-index", "--add", "--cacheinfo", f"160000,{old_gitlink},hermit")
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "commit", "-m", tagged("record nested checkout"),
        )
        git(
            self.repo,
            "-c", "core.hooksPath=/dev/null",
            "push", "-q", "origin", "HEAD:main",
        )
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()

        publisher = Path(self.temp.name) / "gitlink-mismatch-publisher"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(publisher))
        git(publisher, "config", "user.name", "Other")
        git(publisher, "config", "user.email", "other@example.invalid")
        git(publisher, "update-index", "--cacheinfo", f"160000,{target_gitlink},hermit")
        git(publisher, "commit", "-m", tagged("advance nested checkout"))
        git(publisher, "push", "-q", "origin", "HEAD:main")
        target = git(publisher, "rev-parse", "HEAD").stdout.strip()

        git(self.repo / "hermit", "checkout", "-q", different_gitlink)
        (self.repo / "hermit" / "HANDOFF.md").write_text("unread nested handoff\n")

        result = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("state=skipped reason=path-overlap", combined)
        self.assertIn("incoming=hermit dirty=hermit", combined)
        self.assertEqual(before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(different_gitlink, git(self.repo / "hermit", "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(
            "unread nested handoff\n", (self.repo / "hermit" / "HANDOFF.md").read_text()
        )
        self.assertNotEqual(target_gitlink, different_gitlink)
        self.assertNotEqual(target, before)

        # Matching the target checkout is insufficient when the index carries
        # a third, explicitly staged gitlink value. Preserve and report it.
        git(self.repo / "hermit", "checkout", "-q", target_gitlink)
        git(self.repo, "update-index", "--cacheinfo", f"160000,{different_gitlink},hermit")
        staged = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, staged.returncode)
        self.assertIn("incoming=hermit dirty=hermit", staged.stdout + staged.stderr)
        self.assertEqual(before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn(
            different_gitlink,
            git(self.repo, "ls-files", "--stage", "--", "hermit").stdout,
        )
        self.assertEqual(
            "unread nested handoff\n", (self.repo / "hermit" / "HANDOFF.md").read_text()
        )

    def test_preserve_dirty_sync_refuses_divergence_without_reconciling(self) -> None:
        (self.repo / "local.md").write_text("local\n")
        git(self.repo, "add", "local.md")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("local move"))
        before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        target = self.advance_remote_only()

        result = self.writer("sync", "--preserve-dirty")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("requires a fast-forward", combined)
        self.assertIn(f"before={before}", combined)
        self.assertIn(f"after={before}", combined)
        self.assertIn(f"target={target}", combined)
        self.assertEqual(before, git(self.repo, "rev-parse", "HEAD").stdout.strip())


    def test_automatic_sync_accepts_only_exact_target_residue(self) -> None:
        target = self.freeze()

        result = self.writer("sync", "--automatic")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn("automatic-boundary dirty=3 exact-target=3", result.stdout)

    def test_automatic_sync_refuses_nonexact_target_path_byte_identically(self) -> None:
        self.freeze(planted="GENUINE UNCOMMITTED WORK\n")
        head_before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        index_before = git(self.repo, "write-tree").stdout.strip()

        result = self.writer("sync", "--automatic")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("work.txt", combined)
        self.assertIn("MODIFIED", combined)
        self.assertEqual(head_before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(index_before, git(self.repo, "write-tree").stdout.strip())
        self.assertEqual("GENUINE UNCOMMITTED WORK\n", (self.repo / "work.txt").read_text())

    def test_automatic_sync_refuses_unrelated_dirty_path_byte_identically(self) -> None:
        self.advance_remote_only()
        private = self.repo / "private-unpublished.txt"
        private.write_text("only copy\n")
        head_before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        index_before = git(self.repo, "write-tree").stdout.strip()

        result = self.writer("sync", "--automatic")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("private-unpublished.txt", combined)
        self.assertIn("DIRTY OUTSIDE EXACT FETCHED TARGET", combined)
        self.assertIn("human", combined.lower())
        self.assertEqual(head_before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(index_before, git(self.repo, "write-tree").stdout.strip())
        self.assertEqual("only copy\n", private.read_text())

    def test_automatic_sync_refuses_local_commits_byte_identically(self) -> None:
        (self.repo / "local-only.md").write_text("unpublished\n")
        git(self.repo, "add", "local-only.md")
        git(
            self.repo, "-c", "core.hooksPath=/dev/null",
            "commit", "-m", tagged("local only"), "--", "local-only.md",
        )
        head_before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        index_before = git(self.repo, "write-tree").stdout.strip()

        result = self.writer("sync", "--automatic")

        self.assertNotEqual(0, result.returncode)
        self.assertIn("local commit(s)", result.stdout + result.stderr)
        self.assertIn("human", (result.stdout + result.stderr).lower())
        self.assertEqual(head_before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(index_before, git(self.repo, "write-tree").stdout.strip())
        self.assertEqual("unpublished\n", (self.repo / "local-only.md").read_text())

    def test_mutation_control_exact_target_predicate_is_load_bearing(self) -> None:
        """Removing disk==target changes shared state around a planted edit.

        This is deliberately not a generic "mutant exited nonzero" assertion.
        The copied writer loses exactly the hash-equality predicate, then moves
        HEAD/the index around work.txt even though its bytes are not the target.
        The production negative above proves the same input stays byte-identical.
        """
        self.freeze(planted="GENUINE UNCOMMITTED WORK\n")
        writer = self.repo / "scripts/parent-main-write"
        source = writer.read_text()
        predicate = (
            'if git -C "$root" hash-object -- "$root/$path" 2>/dev/null '
            '| grep -Fqx "$blob"; then'
        )
        self.assertEqual(1, source.count(predicate), "mutation target must be exact")
        writer.write_text(source.replace(predicate, "if true; then", 1))
        writer.chmod(0o755)
        head_before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        index_before = git(self.repo, "write-tree").stdout.strip()

        mutant = self.writer("sync", "--automatic")

        head_after = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        index_after = git(self.repo, "write-tree").stdout.strip()
        self.assertTrue(
            head_after != head_before or index_after != index_before,
            "removing the exact-target predicate must demonstrably touch shared state",
        )
        self.assertEqual("GENUINE UNCOMMITTED WORK\n", (self.repo / "work.txt").read_text())
        self.assertEqual(0, mutant.returncode, mutant.stdout + mutant.stderr)

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
        # Bind to the CLASSIFICATION, not to a stray adjective. The previous
        # assertion looked for the word "real" in "they are real uncommitted
        # work"; the refusal was later reworded and this test went red on main
        # while the guard itself was fine. A test that can be broken by a
        # rewrite of prose it does not care about is a test about prose.
        self.assertIn("MODIFIED", combined)
        self.assertIn("uncommitted work", combined)
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


class ReconcileTests(ParentMainWriteTests):
    """The 2026-08-09 divergence, both directions.

    Local main held one commit whose PATCH was already upstream under a
    different sha, because `commit` built it and its own `publish_sha` lost a
    push race. Every sanctioned route was shut -- `sync` and `verify_ref`/sync
    both require HEAD to be an ancestor of origin/main, ordinary/audited requires
    old == fetched-origin, and `git rebase origin/main` gets the right answer but
    has its own ref update refused by the hook. `documented_commands.py
    --closeout` then refused for every agent in the workspace over a commit none
    of them made.
    """

    def publish_from_a_second_clone(self, changes: dict[str, str], message: str) -> str:
        """Land content on origin/main WITHOUT touching self.repo's worktree."""
        other = Path(self.temp.name) / f"other-{abs(hash(message)) % 100000}"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        for name, text in changes.items():
            (other / name).parent.mkdir(parents=True, exist_ok=True)
            (other / name).write_text(text)
            git(other, "add", name)
        git(other, "commit", "-m", tagged(message))
        git(other, "push", "-q", "origin", "HEAD:main")
        # Make the objects visible here without touching a single file, so a
        # test can assert about the target before the writer runs.
        git(self.repo, "fetch", "-q", "origin", "main")
        return git(other, "rev-parse", "HEAD").stdout.strip()

    def local_commit(self, changes: dict[str, str], message: str) -> str:
        for name, text in changes.items():
            (self.repo / name).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / name).write_text(text)
            git(self.repo, "add", name)
        git(
            self.repo, "-c", "core.hooksPath=/dev/null",
            "commit", "-m", tagged(message), "--", *changes,
        )
        return git(self.repo, "rev-parse", "HEAD").stdout.strip()

    def strand_a_patch_equivalent_commit(self) -> tuple[str, str]:
        """The live shape: same patch, different sha, published by someone else."""
        stranded = self.local_commit({"shared.txt": "alpha\nBETA\n"}, "promote beta")
        # The identical patch reaches origin/main under a different sha, then
        # main moves on -- so local main is diverged, not merely behind.
        self.publish_from_a_second_clone({"shared.txt": "alpha\nBETA\n"}, "promote beta (rebuilt)")
        target = self.publish_from_a_second_clone({"notes.md": "moved on\n"}, "main moves on")
        return stranded, target

    # ---- positive: the exact live state reconciles and loses nothing --------

    def test_positive_a_stranded_patch_equivalent_commit_reconciles(self) -> None:
        stranded, target = self.strand_a_patch_equivalent_commit()
        # Precondition: genuinely DIVERGED, and `git cherry` calls it a duplicate.
        self.assertEqual(
            "1", git(self.repo, "rev-list", "--count", f"{target}..HEAD").stdout.strip()
        )
        self.assertTrue(
            git(self.repo, "cherry", "origin/main", "HEAD").stdout.strip().startswith("- ")
        )

        result = self.writer("reconcile")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(
            "0", git(self.repo, "rev-list", "--count", f"{target}..HEAD").stdout.strip()
        )
        # Counted, and bound to the NAMED upstream commit that carries the patch.
        self.assertIn("dropped=1 proven-upstream=1 divergence=0", result.stdout)
        self.assertIn(stranded[:12], result.stdout)
        self.assertIn("is already upstream as", result.stdout)
        self.assertIn(f"recovery: the previous tip {stranded}", result.stdout)
        # The content the dropped commit carried is still here, and the rest of
        # main's advance was checked out.
        self.assertEqual("alpha\nBETA\n", (self.repo / "shared.txt").read_text())
        self.assertEqual("moved on\n", (self.repo / "notes.md").read_text())
        # And it is genuinely unreferenced-but-recoverable, as claimed.
        self.assertEqual(0, git(self.repo, "cat-file", "-e", stranded).returncode)

    def test_positive_a_plain_fast_forward_is_allowed_and_named_as_one(self) -> None:
        target = self.publish_from_a_second_clone({"notes.md": "ahead\n"}, "remote advances")
        result = self.writer("reconcile")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn("fast-forward: NEW..OLD is empty", result.stdout)
        self.assertIn("dropped=0 proven-upstream=0", result.stdout)

    def test_neutral_already_current_is_a_no_op(self) -> None:
        result = self.writer("reconcile")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn(f"reconcile already-current={self.seed}", result.stdout)

    # ---- negative: a commit carrying genuinely new content -----------------

    def test_negative_a_genuinely_new_local_commit_is_refused_and_named(self) -> None:
        kept = self.local_commit({"mine.txt": "WORK NOBODY ELSE HAS\n"}, "add work only I have")
        target = self.publish_from_a_second_clone({"notes.md": "moved on\n"}, "main moves on")

        result = self.writer("reconcile")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("would DROP the commit(s) above", combined)
        self.assertIn(kept[:12], combined)
        self.assertIn("add work only I have", combined)
        self.assertIn("carries a patch no upstream commit has", combined)
        # Nothing moved, and the work is still on the branch.
        self.assertEqual(kept, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())
        self.assertNotEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual("WORK NOBODY ELSE HAS\n", (self.repo / "mine.txt").read_text())

    def test_negative_a_merge_commit_is_refused_though_git_cherry_omits_it(self) -> None:
        """THE test that decides whether this predicate is worth anything.

        `git cherry` SILENTLY OMITS MERGE COMMITS, so "no line starts with +" can
        be true while an evil merge -- whose resolution exists in neither parent
        -- is dropped. Here every non-merge commit really is a duplicate, so the
        obvious predicate reports a clean bill of health and is WRONG.
        """
        first = self.local_commit({"alpha.txt": "A\n"}, "add alpha")
        git(self.repo, "checkout", "-q", "-b", "side", self.seed)
        side = self.local_commit({"side.txt": "S\n"}, "add side")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "merge", "-q", "--no-ff", "--no-commit", "side")
        # The evil part: content that exists in NEITHER parent.
        (self.repo / "evil.txt").write_text("RESOLVED BY HAND, IN THE MERGE ONLY\n")
        git(self.repo, "add", "evil.txt")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("merge side"))
        merge = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        # Both ordinary commits are published upstream under different shas.
        self.publish_from_a_second_clone({"alpha.txt": "A\n"}, "add alpha (rebuilt)")
        self.publish_from_a_second_clone({"side.txt": "S\n"}, "add side (rebuilt)")
        target = self.publish_from_a_second_clone({"notes.md": "moved on\n"}, "main moves on")

        # THE WEAK PREDICATE SAYS SAFE: rev-list sees the merge, cherry does not.
        cherry = git(self.repo, "cherry", "origin/main", "HEAD").stdout.splitlines()
        self.assertEqual([], [line for line in cherry if line.startswith("+")], cherry)
        self.assertNotIn(merge, "\n".join(cherry))
        self.assertIn(merge, git(self.repo, "rev-list", f"{target}..HEAD").stdout)

        result = self.writer("reconcile")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn(merge[:12], combined)
        self.assertIn("NO comparable patch", combined)
        self.assertEqual(merge, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())
        # The merge-only content is still on disk and still nowhere upstream.
        self.assertEqual(
            "RESOLVED BY HAND, IN THE MERGE ONLY\n", (self.repo / "evil.txt").read_text()
        )
        self.assertNotEqual(0, git(self.repo, "cat-file", "-e", f"{target}:evil.txt", check=False).returncode)
        # PRECISE, not merely loud: the refusal names the merge and ONLY the
        # merge. The two ordinary commits really are proven duplicates, and
        # listing them would teach the reader to skim the list.
        self.assertNotIn(first[:12], combined)
        self.assertNotIn(side[:12], combined)

    def test_negative_an_empty_commit_has_no_patch_and_is_refused(self) -> None:
        self.local_commit({"shared.txt": "alpha\nBETA\n"}, "promote beta")
        git(
            self.repo, "-c", "core.hooksPath=/dev/null",
            "commit", "--allow-empty", "-m", tagged("empty bookkeeping commit"),
        )
        empty = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        self.publish_from_a_second_clone({"shared.txt": "alpha\nBETA\n"}, "promote beta (rebuilt)")
        self.publish_from_a_second_clone({"notes.md": "moved on\n"}, "main moves on")

        result = self.writer("reconcile")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn(empty[:12], combined)
        self.assertIn("NO comparable patch", combined)
        self.assertEqual(empty, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())

    # ---- negative: uncommitted work in the shared working tree -------------

    def test_negative_uncommitted_work_that_would_be_lost_is_refused(self) -> None:
        stranded, target = self.strand_a_patch_equivalent_commit()
        # Somebody else is mid-edit on a path main also moved. Not published
        # anywhere, at any commit: losing it would be losing work.
        (self.repo / "notes.md").write_text("SOMEBODY ELSE IS EDITING THIS\n")
        head_before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        index_before = git(self.repo, "write-tree").stdout.strip()

        result = self.writer("reconcile")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("notes.md", combined)
        self.assertIn("MODIFIED", combined)
        self.assertIn("Nothing was staged or modified", combined)
        self.assertEqual("SOMEBODY ELSE IS EDITING THIS\n", (self.repo / "notes.md").read_text())
        self.assertEqual(head_before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(index_before, git(self.repo, "write-tree").stdout.strip())
        self.assertNotEqual(target, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())
        self.assertEqual(stranded, head_before)

    def test_positive_byte_identical_residue_is_adopted_during_reconcile(self) -> None:
        self.strand_a_patch_equivalent_commit()
        target = git(self.repo, "rev-parse", "origin/main").stdout.strip()
        # Residue: an agent wrote the published bytes straight to disk (the
        # plumbing publish path never touches the worktree), so the index is
        # stale for content that is already on origin/main.
        (self.repo / "notes.md").write_text("moved on\n")
        result = self.writer("reconcile")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("residue adopted=", result.stdout)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual("moved on\n", (self.repo / "notes.md").read_text())

    def test_positive_a_bystanders_staged_work_survives_reconcile(self) -> None:
        """The parent index is SHARED machine state; reconcile rewrites it."""
        self.strand_a_patch_equivalent_commit()
        target = git(self.repo, "rev-parse", "origin/main").stdout.strip()
        (self.repo / "bystander.txt").write_text("ANOTHER AGENT STAGED THIS\n")
        git(self.repo, "add", "bystander.txt")
        result = self.writer("reconcile")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn(
            "bystander.txt", git(self.repo, "diff", "--cached", "--name-only").stdout
        )
        self.assertEqual("ANOTHER AGENT STAGED THIS\n", (self.repo / "bystander.txt").read_text())

    # ---- superseded copies: REPORTED, never replaced, in EITHER mode -------

    def stale_published_copy(self) -> tuple[str, str]:
        """Leave on disk a copy that is published, but not published LAST."""
        first = self.publish_from_a_second_clone({"drift.txt": "V1\n"}, "publish V1")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "merge", "-q", "--ff-only", "origin/main")
        target = self.publish_from_a_second_clone({"drift.txt": "V2\n"}, "publish V2")
        # local main falls back behind V1 so drift.txt is in the HEAD..target diff
        git(self.repo, "-c", "core.hooksPath=/dev/null", "reset", "--hard", self.seed)
        (self.repo / "drift.txt").write_text("V1\n")
        return first, target

    def uncommitted_edit_that_looks_published(self) -> str:
        """THE D1 SHAPE, exactly as the cross-model review reproduced it.

        A TRACKED file with an uncommitted edit in progress whose bytes happen to
        equal a version published earlier. Byte-coincidence with history is not
        consent to discard it, and no test can tell this apart from a stale
        leftover -- which is precisely why neither mode may act on it.
        """
        self.publish_from_a_second_clone({"drift.txt": "V1\n"}, "publish V1")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "merge", "-q", "--ff-only", "origin/main")
        v2 = self.publish_from_a_second_clone({"drift.txt": "V2\n"}, "publish V2")
        self.publish_from_a_second_clone({"drift.txt": "V3\n"}, "publish V3")
        self.assertEqual(
            0, git(self.repo, "ls-files", "--error-unmatch", "drift.txt").returncode
        )
        (self.repo / "drift.txt").write_text("V2\n")
        return v2

    def assert_edit_survived(self, result: subprocess.CompletedProcess[str]) -> None:
        combined = result.stdout + result.stderr
        self.assertNotEqual(0, result.returncode, combined)
        self.assertEqual("V2\n", (self.repo / "drift.txt").read_text())
        self.assertIn("SUPERSEDED", combined)
        self.assertNotIn("refreshed", combined)
        self.assertIn("Nothing was staged or modified", combined)
        self.assertEqual("", git(self.repo, "diff", "--cached", "--name-only").stdout.strip())

    def test_negative_sync_never_replaces_an_uncommitted_edit(self) -> None:
        self.uncommitted_edit_that_looks_published()
        self.assert_edit_survived(self.writer("sync"))

    def test_negative_reconcile_never_replaces_an_uncommitted_edit(self) -> None:
        """reconcile must never be HARSHER than sync. It was: it overwrote V2
        with V3 and reported a clean tree."""
        self.uncommitted_edit_that_looks_published()
        self.assert_edit_survived(self.writer("reconcile"))

    def test_negative_the_refusal_names_the_commit_and_the_owner_only_remedy(self) -> None:
        first, target = self.stale_published_copy()
        result = self.writer("reconcile")
        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn(f"[SUPERSEDED — these exact bytes are this path at {first[:12]}", combined)
        # Actionable, but the ACTION belongs to the owner, not to this tool.
        self.assertIn(f"checkout {target} -- <path>", combined)
        self.assertIn("Only the path's OWNER can tell those apart", combined)
        self.assertEqual("V1\n", (self.repo / "drift.txt").read_text())

    def test_negative_reconcile_refuses_a_copy_published_nowhere(self) -> None:
        self.stale_published_copy()
        (self.repo / "drift.txt").write_text("NEVER PUBLISHED ANYWHERE\n")
        result = self.writer("reconcile")
        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("drift.txt", combined)
        self.assertIn("differ from BOTH your HEAD", combined)
        self.assertNotIn("SUPERSEDED", combined)
        self.assertEqual("NEVER PUBLISHED ANYWHERE\n", (self.repo / "drift.txt").read_text())

    # ---- nothing mutates until every gate has passed -----------------------

    def moving_git_shim(self, trigger: str, moves_to: str) -> Path:
        """A `git` wrapper that moves the tracking ref ONCE, mid-operation.

        A rehearsal proves the operation computes; it cannot prove the world
        holds still afterwards. This is the only way to land inside the
        time-of-check-to-time-of-use window deterministically, and it is the
        harness the cross-model review used to find the defect.
        """
        marker = Path(self.temp.name) / f"moved.{trigger}"
        shim_dir = Path(self.temp.name) / f"shim.{trigger}"
        shim_dir.mkdir()
        shim = shim_dir / "git"
        shim.write_text(
            f'#!/bin/bash\n'
            f'if [ "$1" = "-C" ] && [ "$3" = "{trigger}" ] && [ ! -e "{marker}" ]; then\n'
            f'  : > "{marker}"\n'
            f'  "{REAL_GIT}" -C "$2" update-ref refs/remotes/origin/main {moves_to}\n'
            f'fi\n'
            f'exec "{REAL_GIT}" "$@"\n'
        )
        shim.chmod(0o755)
        return shim_dir

    def test_negative_a_refusal_at_the_ref_transaction_leaves_the_index_untouched(self) -> None:
        """C1: the TOCTOU half. The hook refuses AFTER the rehearsal passed.

        Ordering, not the rehearsal, is what makes "Nothing was staged, moved,
        or modified" true here: the ref transaction is the one step that is
        all-or-nothing, so it must run before anything touches shared state.
        """
        self.strand_a_patch_equivalent_commit()
        # Residue that WOULD be adopted, so there is something to leave behind.
        (self.repo / "notes.md").write_text("moved on\n")
        shim_dir = self.moving_git_shim("update-ref", self.seed)
        index_before = git(self.repo, "ls-files", "--stage").stdout
        head_before = git(self.repo, "rev-parse", "refs/heads/main").stdout.strip()

        result = run(
            str(self.repo / "scripts/parent-main-write"), "reconcile", cwd=self.repo,
            env=dict(self.env, PATH=f"{shim_dir}:{os.environ['PATH']}",
                     HERMIT_PARENT_MAIN_LOCK_PATH=f"{self.lock}.shim"),
        )

        combined = result.stdout + result.stderr
        self.assertNotEqual(0, result.returncode, combined)
        self.assertIn("does not match refs/remotes/origin/main", combined)
        # THE POINT: the shared index of a fifteen-agent tree is byte-identical.
        self.assertEqual(index_before, git(self.repo, "ls-files", "--stage").stdout)
        self.assertEqual("", git(self.repo, "diff", "--cached", "--name-only").stdout.strip())
        self.assertNotIn("adopted published residue", combined)
        self.assertEqual(head_before, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())
        self.assertEqual("moved on\n", (self.repo / "notes.md").read_text())

    # ---- C3: an interrupted update must not report success ------------------

    def interruptible_reconcile(self) -> tuple[str, str, Path]:
        """Set up a move whose tree update cannot be skipped, and a way to fail it.

        The target both ADDS a path that is not on disk and MODIFIES a clean
        one, so neither is adoptable residue and both depend on the post-CAS
        `read-tree`. A thinner fixture hides the defect: if every changed path
        happens to be residue, the index is already correct and the interrupted
        state looks clean.
        """
        stranded = self.local_commit({"shared.txt": "alpha\nBETA\n"}, "promote beta")
        self.publish_from_a_second_clone({"shared.txt": "alpha\nBETA\n"}, "promote beta (rebuilt)")
        target = self.publish_from_a_second_clone(
            {"remote-only.txt": "only upstream has this\n", "notes.md": "upstream rewrote this\n"},
            "main moves on",
        )
        marker = Path(self.temp.name) / "failed.once"
        shim_dir = Path(self.temp.name) / "shim.readtree"
        shim_dir.mkdir()
        shim = shim_dir / "git"
        shim.write_text(
            f'#!/bin/bash\n'
            f'if [ "$3" = "read-tree" ] && [[ " $* " == *" -u "* ]] && [ ! -e "{marker}" ]; then\n'
            f'  : > "{marker}"; echo "simulated post-CAS failure" >&2; exit 1\n'
            f'fi\n'
            f'exec "{REAL_GIT}" "$@"\n'
        )
        shim.chmod(0o755)
        return stranded, target, shim_dir

    def journal(self) -> Path:
        # `--git-path` answers relative to the repository, not to this process.
        path = Path(
            git(self.repo, "rev-parse", "--git-path", "parent-main-write-reconcile").stdout.strip()
        )
        return path if path.is_absolute() else self.repo / path

    def test_negative_a_post_cas_failure_is_resumed_not_reported_as_current(self) -> None:
        """C3: the retry must REPAIR. `already-current` off the ref alone said
        success over `M notes.md / D remote-only.txt`."""
        stranded, target, shim_dir = self.interruptible_reconcile()
        env = dict(self.env, PATH=f"{shim_dir}:{os.environ['PATH']}")

        first = run(str(self.repo / "scripts/parent-main-write"), "reconcile", cwd=self.repo,
                    env=dict(env, HERMIT_PARENT_MAIN_LOCK_PATH=f"{self.lock}.first"))

        self.assertNotEqual(0, first.returncode, first.stdout + first.stderr)
        combined = first.stdout + first.stderr
        # The failure must DISCLOSE that main moved, and point at a recovery
        # that works -- not at a bare read-tree that fails on 'not uptodate'.
        self.assertIn("main HAS ALREADY MOVED", combined)
        self.assertIn("INTERRUPTED", combined)
        self.assertIn("scripts/parent-main-write reconcile", combined)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertTrue(self.journal().exists(), "the journal must record the interruption")
        self.assertIn("remote-only.txt", git(self.repo, "diff", "--cached", "--name-only").stdout)

        second = self.writer("reconcile")

        self.assertEqual(0, second.returncode, second.stdout + second.stderr)
        self.assertIn("RESUMING an interrupted update", second.stdout)
        self.assertIn("resumed=1", second.stdout)
        self.assertIn("tree-completed=1 journal-cleared=1", second.stdout)
        self.assertNotIn("already-current", second.stdout)
        # THE POINT: the retry repaired the tree rather than blessing it.
        self.assertEqual("", git(self.repo, "diff", "--cached", "--name-only").stdout.strip())
        self.assertEqual(
            "only upstream has this\n", (self.repo / "remote-only.txt").read_text()
        )
        self.assertEqual("upstream rewrote this\n", (self.repo / "notes.md").read_text())
        self.assertFalse(self.journal().exists(), "a completed resume clears the journal")
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertNotEqual(stranded, target)

    def test_neutral_already_current_counts_untracked_it_cannot_see_with_diff(self) -> None:
        """C5: a field must not claim more than the command behind it measures.

        `dirty=` was fed by `git diff`, which does not see untracked files, so
        it printed `dirty=0` with `?? untracked.txt` on disk. Each count is now
        named for exactly one command.
        """
        (self.repo / "untracked.txt").write_text("nobody has committed this\n")
        (self.repo / "notes.md").write_text("an unstaged tracked edit\n")
        (self.repo / "staged.txt").write_text("staged by a bystander\n")
        git(self.repo, "add", "staged.txt")

        # Bind each field to the command it claims to report, rather than to a
        # number the fixture happens to produce.
        untracked = len(
            git(self.repo, "ls-files", "--others", "--exclude-standard").stdout.split()
        )
        self.assertGreater(untracked, 0)

        result = self.writer("reconcile")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn(f"already-current={self.seed}", result.stdout)
        self.assertIn("interrupted=0", result.stdout)
        self.assertIn("staged=1", result.stdout)
        self.assertIn("unstaged-tracked=1", result.stdout)
        self.assertIn(f"untracked={untracked}", result.stdout)
        # The field that over-claimed is gone, not merely supplemented.
        self.assertNotIn("dirty=", result.stdout)

    def test_positive_a_crash_before_the_cas_clears_the_journal_and_retries(self) -> None:
        """C4: the journal must survive a crash on EITHER side of the CAS.

        Only the after-case was implemented, so killing the process between the
        journal write and the ref transaction left `head_is_old=True
        journal=True` and every later run refused with "this needs a human" --
        wedging reconcile, sync AND commit until the file was deleted by hand.
        That is the fleet wedge the journal exists to prevent, produced by the
        journal itself.
        """
        stranded, target = self.strand_a_patch_equivalent_commit()
        shim_dir = Path(self.temp.name) / "shim.kill"
        shim_dir.mkdir()
        shim = shim_dir / "git"
        shim.write_text(
            f'#!/bin/bash\n'
            f'if [ "$3" = "update-ref" ] && [[ " $* " == *" refs/heads/main "* ]]; then\n'
            f'  kill -9 $PPID 2>/dev/null; exit 97\n'
            f'fi\n'
            f'exec "{REAL_GIT}" "$@"\n'
        )
        shim.chmod(0o755)

        run(str(self.repo / "scripts/parent-main-write"), "reconcile", cwd=self.repo,
            env=dict(self.env, PATH=f"{shim_dir}:{os.environ['PATH']}",
                     HERMIT_PARENT_MAIN_LOCK_PATH=f"{self.lock}.killed"))

        # Precondition: the CAS did not happen, and the journal is stranded.
        self.assertEqual(stranded, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertTrue(self.journal().exists())

        result = self.writer("reconcile")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("stale-journal-cleared=1", result.stdout)
        self.assertIn("reason=ref-never-moved", result.stdout)
        # It did not merely clear the record: it went on and did the work.
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn("dropped=1 proven-upstream=1", result.stdout)
        self.assertFalse(self.journal().exists())

    def test_positive_a_cleared_stale_journal_unblocks_sync_and_commit(self) -> None:
        """The wedge is only fixed if the OTHER modes come back too."""
        self.strand_a_patch_equivalent_commit()
        shim_dir = Path(self.temp.name) / "shim.kill2"
        shim_dir.mkdir()
        shim = shim_dir / "git"
        shim.write_text(
            f'#!/bin/bash\n'
            f'if [ "$3" = "update-ref" ] && [[ " $* " == *" refs/heads/main "* ]]; then\n'
            f'  kill -9 $PPID 2>/dev/null; exit 97\n'
            f'fi\n'
            f'exec "{REAL_GIT}" "$@"\n'
        )
        shim.chmod(0o755)
        run(str(self.repo / "scripts/parent-main-write"), "reconcile", cwd=self.repo,
            env=dict(self.env, PATH=f"{shim_dir}:{os.environ['PATH']}",
                     HERMIT_PARENT_MAIN_LOCK_PATH=f"{self.lock}.killed2"))
        # A PRE-CAS journal must NOT block sync: main never moved, so nothing is
        # half-applied. sync still refuses -- but for the ordinary reason that
        # local main diverges -- and it says the journal is there.
        during = self.writer("sync")
        self.assertIn("stale reconcile journal is present", during.stderr)
        self.assertNotIn("a reconcile was INTERRUPTED here", during.stderr)
        self.assertIn("diverges from fetched origin", during.stderr)

        self.assertEqual(0, self.writer("reconcile").returncode)

        recovered = self.writer("sync")
        self.assertEqual(0, recovered.returncode, recovered.stdout + recovered.stderr)
        self.assertIn("already-current", recovered.stdout)
        self.assertFalse(self.journal().exists())

    def test_positive_a_stale_pre_cas_journal_does_not_block_sync_or_commit(self) -> None:
        """Found while re-reading the C4 fix, not reported by review.

        Once a pre-CAS crash became recoverable, the sync/commit guard was left
        asserting "INTERRUPTED here after main had already moved" for a case
        where main had NOT moved -- and blocking every agent over a tree that is
        untouched by construction (everything before the CAS is read-only). A
        false claim plus an unjustified fleet block, which is the pair this file
        exists to refuse.
        """
        self.strand_a_patch_equivalent_commit()
        head_before = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        # Exactly what a run that died before its transaction leaves behind.
        self.journal().write_text(f"old={head_before}\ntarget={'c' * 40}\n")
        try:
            (self.repo / "notes.md").write_text("a real edit by somebody else\n")
            result = self.writer(
                "commit", "-m", tagged("ordinary work during a stale journal"),
                "--", "notes.md",
            )
            combined = result.stdout + result.stderr
            # It must SAY the journal is there -- silence would be its own defect.
            self.assertIn("stale reconcile journal is present", combined)
            self.assertIn("main never moved", combined)
            self.assertNotIn("INTERRUPTED here", combined)
            # And it must not have blocked on it.
            self.assertNotIn("would build on that", combined)
        finally:
            self.journal().unlink(missing_ok=True)

    def test_negative_a_journal_at_neither_end_still_needs_a_human(self) -> None:
        """The third case must stay a refusal: clearing it would be guessing."""
        self.strand_a_patch_equivalent_commit()
        self.journal().write_text(f"old={'a' * 40}\ntarget={'b' * 40}\n")
        try:
            result = self.writer("reconcile")
            self.assertNotEqual(0, result.returncode)
            self.assertIn("neither end", result.stderr)
            self.assertIn("This needs a human", result.stderr)
            self.assertTrue(self.journal().exists(), "a refusal must not destroy the record")
        finally:
            self.journal().unlink(missing_ok=True)

    def test_positive_the_journal_is_installed_atomically(self) -> None:
        """A half-written journal lands in the unreadable branch: worst of both.

        Observed through the rename: the temporary never occupies the journal's
        own name, so a concurrent reader sees either no journal or a complete
        one -- never a prefix.
        """
        self.strand_a_patch_equivalent_commit()
        shim_dir = Path(self.temp.name) / "shim.capture"
        shim_dir.mkdir()
        seen = Path(self.temp.name) / "seen-journal"
        shim = shim_dir / "git"
        # At the CAS the journal must already be complete and durable on disk.
        shim.write_text(
            f'#!/bin/bash\n'
            f'if [ "$3" = "update-ref" ] && [[ " $* " == *" refs/heads/main "* ]]; then\n'
            f'  cp "$2/.git/parent-main-write-reconcile" "{seen}" 2>/dev/null\n'
            f'  ls "$2/.git/" | grep -c "parent-main-write-reconcile\\." > "{seen}.partials" || true\n'
            f'fi\n'
            f'exec "{REAL_GIT}" "$@"\n'
        )
        shim.chmod(0o755)
        result = run(str(self.repo / "scripts/parent-main-write"), "reconcile", cwd=self.repo,
                     env=dict(self.env, PATH=f"{shim_dir}:{os.environ['PATH']}",
                              HERMIT_PARENT_MAIN_LOCK_PATH=f"{self.lock}.atomic"))
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        body = seen.read_text()
        lines = body.strip().splitlines()
        self.assertEqual(4, len(lines), body)
        self.assertRegex(lines[0], r"^old=[0-9a-f]{40}$")
        self.assertRegex(lines[1], r"^target=[0-9a-f]{40}$")
        # No temporary left under the journal's directory once the run is done.
        self.assertEqual(
            [], list(Path(self.repo / ".git").glob("parent-main-write-reconcile.*"))
        )

    def test_negative_an_interrupted_tree_blocks_sync_and_commit(self) -> None:
        """Nothing may build on a half-applied tree while the journal stands."""
        _, _, shim_dir = self.interruptible_reconcile()
        run(str(self.repo / "scripts/parent-main-write"), "reconcile", cwd=self.repo,
            env=dict(self.env, PATH=f"{shim_dir}:{os.environ['PATH']}",
                     HERMIT_PARENT_MAIN_LOCK_PATH=f"{self.lock}.setup"))
        self.assertTrue(self.journal().exists())

        for mode in ("sync", "commit"):
            args = ["sync"] if mode == "sync" else [
                "commit", "-m", tagged("should not get this far"), "--", "notes.md"
            ]
            # A FRESH lock per invocation: the host mutex is non-reentrant across
            # invocations (a backgrounded hook inherits the fd and holds the
            # flock past the writer's exit), which is a property of the harness,
            # not of the guard under test.
            result = run(
                str(self.repo / "scripts/parent-main-write"), *args, cwd=self.repo,
                env=dict(self.env, HERMIT_PARENT_MAIN_LOCK_PATH=f"{self.lock}.{mode}"),
            )
            self.assertNotEqual(0, result.returncode, f"{mode}: {result.stdout}{result.stderr}")
            self.assertIn("a reconcile was INTERRUPTED here", result.stderr, mode)
            self.assertIn("main HAS already moved to", result.stderr, mode)
            self.assertIn("scripts/parent-main-write reconcile", result.stderr, mode)
        # And the interruption is still recorded -- refusing must not clear it.
        self.assertTrue(self.journal().exists())

    def test_positive_a_clean_run_leaves_no_journal_behind(self) -> None:
        """The journal must be an exception marker, not a permanent fixture."""
        self.strand_a_patch_equivalent_commit()
        result = self.writer("reconcile")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertFalse(self.journal().exists())
        self.assertIn("interrupted=0", self.writer_after_success())

    def writer_after_success(self) -> str:
        # A second, independent invocation would contend on the host mutex, so
        # read the end state directly instead of shelling out again.
        return (
            "interrupted=0"
            if not self.journal().exists()
            else f"interrupted journal present at {self.journal()}"
        )

    def test_negative_a_refused_transaction_leaves_no_journal_behind(self) -> None:
        """A journal outliving a refusal would wedge sync and commit for everyone."""
        self.strand_a_patch_equivalent_commit()
        (self.repo / "notes.md").write_text("moved on\n")
        shim_dir = self.moving_git_shim("update-ref", self.seed)
        result = run(
            str(self.repo / "scripts/parent-main-write"), "reconcile", cwd=self.repo,
            env=dict(self.env, PATH=f"{shim_dir}:{os.environ['PATH']}",
                     HERMIT_PARENT_MAIN_LOCK_PATH=f"{self.lock}.refused"),
        )
        self.assertNotEqual(0, result.returncode)
        self.assertIn("the journal has been removed", result.stdout + result.stderr)
        self.assertFalse(self.journal().exists())
        self.assertEqual("", git(self.repo, "diff", "--cached", "--name-only").stdout.strip())

    def test_negative_a_stale_tracking_ref_is_not_a_fresh_target(self) -> None:
        """C2: a tracking ref records what the LAST fetch saw. No rewrite needed.

        reconcile is the only mode that moves main to a non-descendant, so it is
        the only one that pays a network round trip to prove its target is what
        origin actually has right now.
        """
        stranded, tracking = self.strand_a_patch_equivalent_commit()
        # The remote moves on; this checkout simply does not fetch again.
        other = Path(self.temp.name) / "mover"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        (other / "later.md").write_text("the remote moved on\n")
        git(other, "add", "later.md")
        git(other, "commit", "-m", tagged("remote moves again"))
        git(other, "push", "-q", "origin", "HEAD:main")
        actual = git(other, "rev-parse", "HEAD").stdout.strip()
        self.assertEqual(
            tracking, git(self.repo, "rev-parse", "refs/remotes/origin/main").stdout.strip()
        )
        self.assertNotEqual(tracking, actual, "the tracking ref must actually be stale")

        result = self.verify_ref_directly(stranded, tracking)

        self.assertNotEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("is NOT origin's current main", result.stderr)
        self.assertIn(actual[:12], result.stderr)
        self.assertIn("records what the last fetch saw", result.stderr)

    def test_negative_a_stale_base_is_caught_at_push_time_in_ordinary_mode(self) -> None:
        """Why ordinary/audited do NOT pay for the remote probe.

        The boundary statement above verify_context claims a stale base is
        caught by the push CAS instead. That is the justification for a
        deliberate residual, so it is evidenced here rather than asserted:
        verify_push compares against the value the REMOTE itself advertises, so
        a stale receipt cannot survive publication.
        """
        _, tracking = self.strand_a_patch_equivalent_commit()
        other = Path(self.temp.name) / "mover-push"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        (other / "later.md").write_text("the remote moved on\n")
        git(other, "add", "later.md")
        git(other, "commit", "-m", tagged("remote moves again"))
        git(other, "push", "-q", "origin", "HEAD:main")
        remote_now = git(other, "rev-parse", "HEAD").stdout.strip()

        # The push hook is handed the remote's OWN advertised old value.
        lock = f"{self.lock}.push"
        script = (
            f'exec 9>"{lock}"; flock -n 9 || exit 90; '
            f'export HERMIT_PARENT_MAIN_LOCK_FD=9 HERMIT_PARENT_MAIN_LOCK_PATH="{lock}"; '
            f'export HERMIT_PARENT_MAIN_MODE=ordinary HERMIT_PARENT_MAIN_EXPECT="{tracking}"; '
            f'exec "{self.repo}/scripts/parent-main-write" verify-push origin '
            f'"{remote_now}" "{tracking}"'
        )
        result = run("bash", "-c", script, cwd=self.repo, env=self.env)
        self.assertNotEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("remote main moved after fetch", result.stderr)
        self.assertIn(remote_now, result.stderr)

    def test_positive_a_freshly_fetched_target_passes_the_remote_probe(self) -> None:
        """The freshness gate must not be inert: the fresh case still fires."""
        stranded, target = self.strand_a_patch_equivalent_commit()
        self.assertEqual(
            target, git(self.repo, "rev-parse", "refs/remotes/origin/main").stdout.strip()
        )
        result = self.verify_ref_directly(stranded, target)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_negative_an_unreadable_remote_refuses_rather_than_proceeds(self) -> None:
        """Fail-closed: an unprovable target is not a fresh target."""
        stranded, target = self.strand_a_patch_equivalent_commit()
        git(self.repo, "remote", "set-url", "origin", str(Path(self.temp.name) / "does-not-exist.git"))
        result = self.verify_ref_directly(stranded, target)
        self.assertNotEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("cannot read origin's current main", result.stderr)
        self.assertIn("Nothing was moved", result.stderr)

    def test_negative_a_rehearsal_failure_leaves_the_shared_index_untouched(self) -> None:
        """The dry run must run BEFORE the index is written, not after.

        An unmerged index entry is something only another agent's abandoned
        conflict can produce, and classification cannot see it -- it inspects
        paths, not index stages. So `read-tree -m` is the gate that catches it,
        and it must catch it while the refusal's promise is still true.
        """
        self.strand_a_patch_equivalent_commit()
        # Residue that WOULD be adopted, so there is something to leave behind.
        (self.repo / "notes.md").write_text("moved on\n")
        # Another agent left a conflicted entry for an unrelated path.
        blob = subprocess.run(
            ["git", "hash-object", "-w", "--stdin"], cwd=self.repo, input="conflicted\n",
            text=True, capture_output=True, check=True,
        ).stdout.strip()
        subprocess.run(
            ["git", "update-index", "--index-info"], cwd=self.repo,
            input=f"100644 {blob} 1\tconflict.txt\n100644 {blob} 2\tconflict.txt\n",
            text=True, capture_output=True, check=True,
        )
        index_before = git(self.repo, "ls-files", "--stage").stdout
        head_before = git(self.repo, "rev-parse", "refs/heads/main").stdout.strip()

        result = self.writer("reconcile")

        combined = result.stdout + result.stderr
        self.assertNotEqual(0, result.returncode, combined)
        self.assertIn("REHEARSAL against a copy of the index", combined)
        self.assertIn("nothing was staged, moved, or modified", combined)
        # The proof: not one index entry moved, and the ref did not move.
        self.assertEqual(index_before, git(self.repo, "ls-files", "--stage").stdout)
        self.assertEqual(head_before, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())
        self.assertNotIn("adopted published residue", combined)

    # ---- the hook re-derives the predicate itself --------------------------

    def verify_ref_directly(self, old: str, new: str) -> subprocess.CompletedProcess[str]:
        """Call the hook entry point with a real lock receipt and nothing else.

        The command path never runs here, so whatever it computed cannot be what
        authorises this: `verify-ref` has to derive the predicate from the
        transaction's own old/new or it has derived nothing.
        """
        script = (
            f'exec 9>"{self.lock}"; flock -n 9 || exit 90; '
            f'export HERMIT_PARENT_MAIN_LOCK_FD=9 HERMIT_PARENT_MAIN_LOCK_PATH="{self.lock}"; '
            f'export HERMIT_PARENT_MAIN_MODE=reconcile HERMIT_PARENT_MAIN_EXPECT="{new}"; '
            f'exec "{self.repo}/scripts/parent-main-write" verify-ref "{old}" "{new}"'
        )
        return run("bash", "-c", script, cwd=self.repo, env=self.env)

    def test_positive_verify_ref_reconcile_accepts_a_proven_range_on_its_own(self) -> None:
        stranded, target = self.strand_a_patch_equivalent_commit()
        result = self.verify_ref_directly(stranded, target)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_negative_verify_ref_reconcile_refuses_an_unproven_range_on_its_own(self) -> None:
        kept = self.local_commit({"mine.txt": "WORK NOBODY ELSE HAS\n"}, "add work only I have")
        target = self.publish_from_a_second_clone({"notes.md": "moved on\n"}, "main moves on")
        result = self.verify_ref_directly(kept, target)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("would DROP the commit(s) above", result.stderr)
        self.assertIn(kept[:12], result.stderr)

    def test_negative_a_forged_expect_cannot_name_the_target(self) -> None:
        """D2: the CAS receipt is a ref, not an environment variable.

        Holding the writer lock, export an arbitrary LOCAL commit as the
        "freshly fetched origin/main" and drive a hooked `update-ref`. The
        predicate was already re-derived from the transaction's own old/new --
        but the TARGET it was derived against was whatever the caller said.
        """
        stranded, target = self.strand_a_patch_equivalent_commit()
        arbitrary = stranded  # any local commit that is not origin/main
        self.assertNotEqual(arbitrary, git(self.repo, "rev-parse", "origin/main").stdout.strip())
        script = (
            f'exec 9>"{self.lock}"; flock -n 9 || exit 90; '
            f'export HERMIT_PARENT_MAIN_LOCK_FD=9 HERMIT_PARENT_MAIN_LOCK_PATH="{self.lock}"; '
            f'export HERMIT_PARENT_MAIN_MODE=reconcile HERMIT_PARENT_MAIN_EXPECT="{arbitrary}"; '
            f'exec "{self.repo}/scripts/parent-main-write" verify-ref "{target}" "{arbitrary}"'
        )
        result = run("bash", "-c", script, cwd=self.repo, env=self.env)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("does not match refs/remotes/origin/main", result.stderr)
        self.assertIn("cannot establish what origin/main is", result.stderr)

    def test_negative_a_forged_expect_is_refused_in_every_mode(self) -> None:
        """A forged base is a forged base whichever arm consumes it."""
        stranded, _ = self.strand_a_patch_equivalent_commit()
        for mode in ("sync", "ordinary", "audited", "reconcile"):
            # A FRESH lock file per mode. The host mutex is deliberately
            # non-reentrant across invocations (a backgrounded hook inherits the
            # fd and holds the flock past the writer's own exit), which is a
            # property of the harness, not of the gate under test.
            lock = f"{self.lock}.{mode}"
            script = (
                f'exec 9>"{lock}"; flock -n 9 || exit 90; '
                f'export HERMIT_PARENT_MAIN_LOCK_FD=9 HERMIT_PARENT_MAIN_LOCK_PATH="{lock}"; '
                f'export HERMIT_PARENT_MAIN_MODE={mode} HERMIT_PARENT_MAIN_EXPECT="{stranded}"; '
                f'exec "{self.repo}/scripts/parent-main-write" verify-ref "{self.seed}" "{stranded}"'
            )
            result = run("bash", "-c", script, cwd=self.repo, env=self.env)
            self.assertNotEqual(0, result.returncode, f"{mode}: {result.stdout}{result.stderr}")
            self.assertIn("does not match refs/remotes/origin/main", result.stderr, mode)

    # ---- D3: patch-id is a prefilter, the strict digest is the proof -------

    def test_negative_a_whitespace_only_difference_is_not_a_duplicate(self) -> None:
        """patch-id normalises whitespace away; the strict digest does not."""
        kept = self.local_commit({"code.py": "def f():\n    return 1\n"}, "add f")
        # Upstream has the SAME change with different indentation: same patch-id,
        # genuinely different content.
        self.publish_from_a_second_clone(
            {"code.py": "def f():\n\treturn 1\n"}, "add f (tabs)"
        )
        self.publish_from_a_second_clone({"notes.md": "moved on\n"}, "main moves on")
        local_id = subprocess.run(
            f"git diff-tree -p --no-commit-id {kept} | git patch-id --stable",
            shell=True, cwd=self.repo, text=True, capture_output=True,
        ).stdout.split()
        upstream_id = subprocess.run(
            "git rev-list origin/main~1..origin/main~0 >/dev/null; "
            f"git diff-tree -p --no-commit-id origin/main~1 | git patch-id --stable",
            shell=True, cwd=self.repo, text=True, capture_output=True,
        ).stdout.split()
        # Precondition: patch-id ALONE would have called these the same change.
        self.assertEqual(local_id[:1], upstream_id[:1], f"{local_id} {upstream_id}")

        result = self.writer("reconcile")

        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("by patch-id but NOT byte-for-byte", combined)
        self.assertIn(kept[:12], combined)
        self.assertEqual(kept, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())

    def test_negative_two_local_commits_cannot_share_one_upstream_commit(self) -> None:
        """Multiplicity, ISOLATED: injectivity is the only thing that refuses.

        Every one of the three local commits has a byte-exact upstream match, so
        the digest is fully satisfied. But the flip A and the flip C are the SAME
        diff and there is only ONE upstream commit carrying it -- and upstream
        flipped back afterwards, so local's net state (`2`) is upstream nowhere.
        Without the consumed-set this reports three proven commits and silently
        moves the tree to `1`.
        """
        self.local_commit({"flip.txt": "1\n"}, "seed flip")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "push", "-q", "origin", "HEAD:main")
        git(self.repo, "fetch", "-q", "origin", "main")
        base = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        a = self.local_commit({"flip.txt": "2\n"}, "flip to 2")
        self.local_commit({"flip.txt": "1\n"}, "flip back to 1")
        c = self.local_commit({"flip.txt": "2\n"}, "flip to 2 again")

        other = Path(self.temp.name) / "other-flip"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        for text, msg in (("2\n", "flip to 2 (rebuilt)"), ("1\n", "flip back to 1 (rebuilt)")):
            (other / "flip.txt").write_text(text)
            git(other, "add", "flip.txt")
            git(other, "commit", "-m", tagged(msg))
        (other / "notes.md").write_text("moved on\n")
        git(other, "add", "notes.md")
        git(other, "commit", "-m", tagged("main moves on"))
        git(other, "push", "-q", "origin", "HEAD:main")
        git(self.repo, "fetch", "-q", "origin", "main")
        self.assertEqual(base, git(self.repo, "merge-base", "HEAD", "origin/main").stdout.strip())

        result = self.writer("reconcile")

        self.assertNotEqual(0, result.returncode, result.stdout + result.stderr)
        combined = result.stdout + result.stderr
        self.assertIn(c[:12], combined)
        self.assertIn("an EARLIER local commit already", combined)
        self.assertIn("discharges at most ONE local commit", combined)
        self.assertEqual(c, git(self.repo, "rev-parse", "refs/heads/main").stdout.strip())
        # Local's net state survives: it is upstream nowhere.
        self.assertEqual("2\n", (self.repo / "flip.txt").read_text())
        self.assertEqual("1\n", git(self.repo, "show", "origin/main:flip.txt").stdout)
        self.assertNotEqual(a, c)

    def test_positive_evidence_reports_the_strict_digest_not_just_a_patch_id(self) -> None:
        stranded, _ = self.strand_a_patch_equivalent_commit()
        result = self.writer("reconcile")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("strict diff digest", result.stdout)
        self.assertIn("byte-exact, one-to-one", result.stdout)
        self.assertIn(stranded[:12], result.stdout)

    def test_negative_verify_ref_reconcile_refuses_a_target_that_is_not_fetched_origin(self) -> None:
        stranded, target = self.strand_a_patch_equivalent_commit()
        script = (
            f'exec 9>"{self.lock}"; flock -n 9 || exit 90; '
            f'export HERMIT_PARENT_MAIN_LOCK_FD=9 HERMIT_PARENT_MAIN_LOCK_PATH="{self.lock}"; '
            f'export HERMIT_PARENT_MAIN_MODE=reconcile HERMIT_PARENT_MAIN_EXPECT="{target}"; '
            f'exec "{self.repo}/scripts/parent-main-write" verify-ref "{stranded}" "{self.seed}"'
        )
        result = run("bash", "-c", script, cwd=self.repo, env=self.env)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("is not fetched origin", result.stderr)


class ReconcileSubmoduleTests(ParentMainWriteTests):
    """A gitlink is not working-tree content, and cannot be hashed like one.

    `git hash-object` refuses a directory (`fatal: Unable to hash ./sub`, rc
    128), so the byte-equality test that authorises residue adoption can never
    be satisfied for a submodule -- every advanced gitlink was landing in the
    blocker list and being reported as somebody's uncommitted work. The live
    parent has exactly that shape on `hermit`, so without the carve-out neither
    sync nor reconcile could ever run there.
    """

    def setUp(self) -> None:
        super().setUp()
        self.sub = Path(self.temp.name) / "sub"
        git(Path(self.temp.name), "init", "-q", "--initial-branch=main", str(self.sub))
        git(self.sub, "config", "user.name", "Sub")
        git(self.sub, "config", "user.email", "sub@example.invalid")
        self.sub_shas = []
        for text in ("s1\n", "s2\n", "s3\n"):
            (self.sub / "s.txt").write_text(text)
            git(self.sub, "add", "s.txt")
            git(self.sub, "commit", "-q", "-m", text.strip())
            self.sub_shas.append(git(self.sub, "rev-parse", "HEAD").stdout.strip())
        git(self.repo, "config", "protocol.file.allow", "always")
        git(
            self.repo, "-c", "core.hooksPath=/dev/null", "-c", "protocol.file.allow=always",
            "submodule", "add", "-q", str(self.sub), "sub",
        )
        git(self.sub_dir(), "checkout", "-q", self.sub_shas[0])
        git(self.repo, "add", ".gitmodules", "sub")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-m", tagged("pin sub at s1"))
        git(self.repo, "-c", "core.hooksPath=/dev/null", "push", "-q", "origin", "HEAD:main")
        self.seed = git(self.repo, "rev-parse", "HEAD").stdout.strip()

    def sub_dir(self) -> Path:
        return self.repo / "sub"

    def advance_gitlink_upstream(self) -> str:
        """origin/main repins the submodule at s2, without this worktree."""
        other = Path(self.temp.name) / "other-sub"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        git(other, "-c", "protocol.file.allow=always", "submodule", "update", "-q", "--init")
        git(other / "sub", "checkout", "-q", self.sub_shas[1])
        (other / "notes.md").write_text("upstream also moves a file\n")
        git(other, "add", "sub", "notes.md")
        git(other, "commit", "-m", tagged("repin sub at s2"))
        git(other, "push", "-q", "origin", "HEAD:main")
        git(self.repo, "fetch", "-q", "origin", "main")
        return git(other, "rev-parse", "HEAD").stdout.strip()

    def test_positive_an_advanced_submodule_checkout_does_not_block_and_is_untouched(self) -> None:
        target = self.advance_gitlink_upstream()
        # The live shape: the primary checkout sits AHEAD of both pins.
        git(self.sub_dir(), "checkout", "-q", self.sub_shas[2])
        status_before = git(self.repo, "status", "--short").stdout

        result = self.writer("sync")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("gitlink sub: index pointer only", result.stdout + result.stderr)
        self.assertIn("gitlinks=1", result.stdout)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        # The superproject advanced; the submodule checkout did not move.
        self.assertEqual(
            self.sub_shas[2], git(self.sub_dir(), "rev-parse", "HEAD").stdout.strip()
        )
        self.assertEqual(status_before, git(self.repo, "status", "--short").stdout)
        self.assertEqual("s3\n", (self.sub_dir() / "s.txt").read_text())

    def test_positive_a_gitlink_is_skipped_by_mode_not_by_a_missing_object(self) -> None:
        """The arm has to be REACHABLE, or it is protection-shaped decoration.

        Ordinarily a gitlink exits through the "absent from the target tree"
        arm, because the submodule's commit object is not in the superproject's
        store (rc 1, measured on the live parent for both `hermit` and
        `agent-utils`) -- right answer, wrong reason. Import that object and the
        wrong reason stops applying: classification then reaches `hash-object`,
        which CANNOT hash a directory, and an advanced gitlink is reported as
        somebody's uncommitted work. Deciding by MODE first is what prevents it.
        """
        target = self.advance_gitlink_upstream()
        git(self.sub_dir(), "checkout", "-q", self.sub_shas[2])
        # The object is now genuinely present in the SUPERPROJECT's store.
        git(self.repo, "fetch", "-q", str(self.sub), "main")
        self.assertEqual(0, git(self.repo, "cat-file", "-e", f"{target}:sub").returncode)

        result = self.writer("sync")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("gitlinks=1", result.stdout)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(
            self.sub_shas[2], git(self.sub_dir(), "rev-parse", "HEAD").stdout.strip()
        )

    def test_positive_reconcile_moves_a_diverged_main_past_an_advanced_gitlink(self) -> None:
        # Strand a patch-equivalent commit AND leave the submodule ahead.
        (self.repo / "shared.txt").write_text("alpha\nBETA\n")
        git(self.repo, "add", "shared.txt")
        git(
            self.repo, "-c", "core.hooksPath=/dev/null",
            "commit", "-m", tagged("promote beta"), "--", "shared.txt",
        )
        stranded = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        other = Path(self.temp.name) / "other-dup"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        (other / "shared.txt").write_text("alpha\nBETA\n")
        git(other, "add", "shared.txt")
        git(other, "commit", "-m", tagged("promote beta (rebuilt)"))
        git(other, "push", "-q", "origin", "HEAD:main")
        target = self.advance_gitlink_upstream()
        git(self.sub_dir(), "checkout", "-q", self.sub_shas[2])

        result = self.writer("reconcile")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertIn("dropped=1 proven-upstream=1", result.stdout)
        self.assertIn(stranded[:12], result.stdout)
        self.assertEqual(
            self.sub_shas[2], git(self.sub_dir(), "rev-parse", "HEAD").stdout.strip()
        )
        self.assertEqual(
            self.sub_shas[1], git(self.repo, "rev-parse", "HEAD:sub").stdout.strip()
        )


class LiveLedgerWriterSyncTests(unittest.TestCase):
    """A parent sync succeeds while the canonical adapter keeps recording."""

    def setUp(self) -> None:
        ParentMainWriteTests.setUp(self)
        ledger_dir = self.repo / "ci-hub/ledger"
        ledger_dir.mkdir(parents=True)
        for name in ("ledger.py", "publisher.py", "validate_rows.py"):
            shutil.copy2(SOURCE / "ci-hub/ledger" / name, ledger_dir / name)
        (self.repo / ".gitignore").write_text("/ignored/\n")
        git(
            self.repo,
            "add",
            ".gitignore",
            "ci-hub/ledger/ledger.py",
            "ci-hub/ledger/publisher.py",
            "ci-hub/ledger/validate_rows.py",
        )
        git(
            self.repo,
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-m",
            tagged("install ledger adapter fixture"),
        )
        git(self.repo, "-c", "core.hooksPath=/dev/null", "push", "origin", "main")

    def tearDown(self) -> None:
        ParentMainWriteTests.tearDown(self)

    def writer(self, *args: str, **kwargs) -> subprocess.CompletedProcess[str]:
        return ParentMainWriteTests.writer(self, *args, **kwargs)

    def test_sync_fast_forwards_while_unique_events_arrive_before_and_after(self) -> None:
        base = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        other = Path(self.temp.name) / "other-live"
        git(Path(self.temp.name), "clone", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        (other / "notes.md").write_text("remote advanced while validates run\n")
        git(other, "add", "notes.md")
        git(other, "commit", "-m", tagged("advance remote during live writes"))
        git(other, "push", "origin", "main")
        target = git(other, "rev-parse", "HEAD").stdout.strip()
        self.assertNotEqual(base, target, "remote main must actually advance")

        ids_path = self.repo / "ignored/writer-event-ids.jsonl"
        script = r'''
import json, sys, time
from pathlib import Path
root, ids_path = Path(sys.argv[1]), Path(sys.argv[2])
real_commit, real_checkout = sys.argv[3], sys.argv[4]
sys.path.insert(0, str(root / "ci-hub/ledger"))
# THE FIXTURE COPIES THREE LEDGER FILES; validate_rows NEEDS MORE THAN THAT.
# It does `sys.path.insert(0, parents[1]); import qualifying_receipt`, added
# by e9d72c83, and qualifying_receipt imports green_class, which imports
# further still. Copying that closure by hand is a treadmill: it breaks again
# the next time anything under ci-hub grows an import. Appending the real
# ci-hub resolves the whole chain, while validate_rows itself still comes from
# the temp repo's copy, inserted ahead of it. Writes stay scoped by the
# explicit root= handed to record().
sys.path.append(sys.argv[5])
import validate_rows
ids_path.parent.mkdir(parents=True, exist_ok=True)
for index in range(300):
    event_id = f"live-sync-{index:04d}"
    row = {
        "schema_version": 5,
        "record_id": event_id,
        "repo": "hermit",
        # NOT one of validate_rows' ACCEPTANCE_FIXTURE_PRODUCERS, and it
        # cannot be: those are refused whenever root == production_root, and
        # repository_root() is just "the repo you are standing in", which for
        # a fixture in a temp clone is always true. A quarantined producer
        # raises on the first row. Tried it; that is why this note exists.
        #
        # SO THE ONLY THING KEEPING THESE 300 SYNTHETIC PASS ROWS OUT OF THE
        # REAL SHARD IS THE EXPLICIT root= BELOW. validate_rows records an
        # incident where a live-sync probe put exactly this many rows into the
        # production Hermit shard, append-only, still there today. This
        # producer sits outside the quarantine set, so that guard would not
        # catch a repeat. Do not drop the root= argument, and do not point
        # this fixture at a real checkout.
        "producer": "parent-main-write-integration",
        "host": "devbig999",
        "started_at": "2026-08-09T12:00:00Z",
        "finished_at": "2026-08-09T12:01:00Z",
        "result": "pass",
        # A REAL COMMIT IN A REAL CHECKOUT, PASSED IN BY THE TEST. This row
        # carried "a" * 40 and a worktrees/slot99/hermit path that never
        # existed. Harmless until e9d72c83 added _require_local_commit, which
        # resolves the cwd and runs `git cat-file -t` on the sha -- after that
        # the synthetic row raised and killed this writer at its first
        # record(). The subject here is sync-versus-concurrent-writes; the row
        # only has to be valid, so it borrows the fixture's own repo.
        "commit": real_commit,
        "cwd": real_checkout,
    }
    validate_rows.record(row, root=root)
    with ids_path.open("a") as stream:
        stream.write(json.dumps({"event_id": event_id}) + "\n")
    time.sleep(0.02)
'''
        process = subprocess.Popen(
            [sys.executable, "-c", script, str(self.repo), str(ids_path),
             base, str(self.repo), str(SOURCE / "ci-hub")],
            cwd=self.repo,
            env=dict(os.environ),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if ids_path.is_file() and len(ids_path.read_text().splitlines()) >= 5:
                    break
                if process.poll() is not None:
                    break
                time.sleep(0.02)
            # REPORT THE WRITER'S OWN ERROR, NOT THE ABSENCE OF ITS OUTPUT.
            # When the writer died at startup, this loop simply spun out its
            # ten seconds and the next line raised FileNotFoundError on the
            # file the writer never reached. That names the symptom and throws
            # away the cause: the traceback explaining why it died was sitting
            # unread in the child's stderr the whole time. Recovering it cost a
            # real diagnosis for something the test already had in hand.
            if process.poll() is not None:
                stdout, stderr = process.communicate()
                self.fail(
                    "the live writer exited before the sync boundary "
                    f"(rc={process.returncode}); its stderr follows:\n"
                    f"{stderr}\n{stdout}"
                )
            before = len(ids_path.read_text().splitlines())
            self.assertGreaterEqual(before, 5)
            self.assertIsNone(process.poll(), "writer must be active before sync")

            result = self.writer("sync")

            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
            self.assertEqual(target, git(self.repo, "rev-parse", "origin/main").stdout.strip())
            self.assertIsNone(process.poll(), "writer must remain active after sync")
            # SIZED FROM A MEASUREMENT, NOT A GUESS. This waits for all 300
            # rows. e9d72c83 gave record() a `git cat-file` per row, so the
            # loop went from roughly 20ms an iteration to a measured 35.2ms
            # plus the 0.02s sleep -- about 16.5s for the run, against a 15s
            # budget set when the check did not exist. It failed by a second,
            # which reads as flake and is not: it is a dependent the new check
            # did not sweep. 60s is over 3x the measured cost, which this box
            # needs with a dozen agents building on it.
            stdout, stderr = process.communicate(timeout=60)
            self.assertEqual(0, process.returncode, stdout + stderr)
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)

        written_ids = [json.loads(line)["event_id"] for line in ids_path.read_text().splitlines()]
        self.assertGreater(len(written_ids), before, "rows must land after the sync boundary")
        union = run(
            sys.executable,
            "-c",
            (
                "import json,sys; from pathlib import Path; "
                "root=Path(sys.argv[1]); sys.path.insert(0,str(root/'ci-hub/ledger')); "
                # Same sibling-import chain as the writer above; the reader
                # needs the real ci-hub for exactly the same reason.
                "sys.path.append(sys.argv[2]); "
                "import validate_rows; "
                "print(json.dumps([e['event_id'] for e in validate_rows.events(root=root)]))"
            ),
            str(self.repo),
            str(SOURCE / "ci-hub"),
            cwd=self.repo,
        )
        self.assertEqual(0, union.returncode, union.stdout + union.stderr)
        union_ids = json.loads(union.stdout)
        self.assertEqual(set(written_ids), set(union_ids))
        self.assertEqual(len(written_ids), len(union_ids), "event-id union must lose nothing")
        self.assertEqual("", git(self.repo, "status", "--short", "--untracked-files=no").stdout)


class PrePushMainHealthBudgetTests(unittest.TestCase):
    """The warning-only health query must fit inside parent publication."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name) / "repo"
        git(Path(self.temp.name), "init", "--initial-branch=main", str(self.repo))
        git(self.repo, "config", "user.name", "Pre-push Test")
        git(self.repo, "config", "user.email", "pre-push@example.invalid")
        (self.repo / "seed").write_text("seed\n")
        git(self.repo, "add", "seed")
        git(self.repo, "commit", "-m", "seed")
        (self.repo / ".githooks").mkdir()
        (self.repo / "ci-hub/bin").mkdir(parents=True)
        shutil.copy2(SOURCE / ".githooks/pre-push", self.repo / ".githooks/pre-push")
        self.hook = self.repo / ".githooks/pre-push"
        self.checker = self.repo / "ci-hub/bin/main-health"

    def tearDown(self) -> None:
        self.temp.cleanup()

    def invoke(
        self, body: str, *, timeout: float = 15.0
    ) -> tuple[subprocess.CompletedProcess[str], float]:
        self.checker.write_text("#!/usr/bin/env bash\nset -u\n" + body)
        self.checker.chmod(0o755)
        started = time.monotonic()
        result = subprocess.run(
            [str(self.hook), "origin", "fixture://origin"],
            cwd=self.repo,
            input="",
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        return result, time.monotonic() - started

    def assert_bound_arguments(self, output: str) -> None:
        self.assertIn("--per-call-timeout 4 --overall-deadline 8", output)

    def test_fast_green_preserves_output_and_arguments(self) -> None:
        result, _ = self.invoke('printf "GREEN %s\\n" "$*"\nexit 0\n')
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("GREEN", result.stderr)
        self.assert_bound_arguments(result.stderr)
        self.assertNotIn("Pre-push warning", result.stderr)

    def test_red_remains_loud_but_does_not_block_repair_push(self) -> None:
        result, _ = self.invoke('printf "RED %s\\n" "$*"\nexit 1\n')
        self.assertEqual(0, result.returncode, result.stderr)
        self.assert_bound_arguments(result.stderr)
        self.assertIn("GitHub reports RED main", result.stderr)

    def test_unavailable_remains_unknown_not_green(self) -> None:
        result, _ = self.invoke('printf "UNKNOWN %s\\n" "$*"\nexit 2\n')
        self.assertEqual(0, result.returncode, result.stderr)
        self.assert_bound_arguments(result.stderr)
        self.assertIn("could not be verified", result.stderr)
        self.assertIn("Do not claim green", result.stderr)

    def test_blocking_checker_is_stopped_by_ten_second_ceiling(self) -> None:
        result, elapsed = self.invoke(
            'echo checker-started\nexec python3 -c "import time; time.sleep(60)"\n'
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertGreaterEqual(elapsed, 9.0)
        self.assertLess(elapsed, 13.0)
        self.assertIn("checker-started", result.stderr)
        self.assertIn("exceeded 10s pre-push budget", result.stderr)
        self.assertIn("green is not established", result.stderr)


class HealthTickParentSyncTests(unittest.TestCase):
    """The durable tick updates the parent before it loads versioned tick code."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        base = Path(self.temp.name)
        self.base = base
        self.home = base / "home"
        self.repo = self.home / "work" / "dev-hermit"
        self.remote = base / "origin.git"
        self.hermit_remote = base / "hermit-origin.git"
        self.agent_utils_remote = base / "agent-utils-origin.git"
        self.bin = base / "bin"
        self.log = self.home / ".local" / "state" / "hermit-health-tick-invocations.log"
        self.lock = base / "parent-main.lock"

        agent_utils = base / "agent-utils-source"
        git(base, "init", "--initial-branch=main", str(agent_utils))
        git(agent_utils, "config", "user.name", "Tick Test")
        git(agent_utils, "config", "user.email", "tick@example.invalid")
        (agent_utils / "bin").mkdir()
        (agent_utils / "bin" / "tick-hub").write_text("pinned agent-utils\n")
        shutil.copytree(
            SOURCE / "hermit" / "agent-utils" / "py" / "tick_hub",
            agent_utils / "py" / "tick_hub",
        )
        git(agent_utils, "add", ".")
        git(agent_utils, "commit", "-m", "seed agent-utils")
        self.agent_utils_head = git(agent_utils, "rev-parse", "HEAD").stdout.strip()
        git(base, "clone", "-q", "--bare", str(agent_utils), str(self.agent_utils_remote))

        hermit = base / "hermit-source"
        git(base, "init", "--initial-branch=main", str(hermit))
        git(hermit, "config", "user.name", "Tick Test")
        git(hermit, "config", "user.email", "tick@example.invalid")
        (hermit / "README.md").write_text("pinned Hermit\n")
        git(hermit, "add", "README.md")
        git(hermit, "commit", "-m", "seed Hermit")
        git(
            hermit,
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            str(self.agent_utils_remote),
            "agent-utils",
        )
        git(hermit, "commit", "-am", "pin agent-utils")
        self.hermit_head = git(hermit, "rev-parse", "HEAD").stdout.strip()
        git(base, "clone", "-q", "--bare", str(hermit), str(self.hermit_remote))

        self.repo.mkdir(parents=True)
        self.bin.mkdir()
        self.log.parent.mkdir(parents=True)
        git(base, "init", "--bare", "--initial-branch=main", str(self.remote))
        git(self.repo, "init", "--initial-branch=main")
        git(self.repo, "config", "user.name", "Tick Test")
        git(self.repo, "config", "user.email", "tick@example.invalid")
        (self.repo / "scripts").mkdir()
        (self.repo / "ci-hub" / "bin").mkdir(parents=True)
        shutil.copy2(SOURCE / "scripts/parent-main-write", self.repo / "scripts/parent-main-write")
        shutil.copy2(SOURCE / "scripts/hermit-health-tick.sh", self.repo / "scripts/hermit-health-tick.sh")
        wrapper = self.repo / "scripts/hermit-health-tick.sh"
        wrapper.write_text(
            wrapper.read_text().replace(
                'CANONICAL_REMOTE_URL="https://github.com/rrnewton/dev-hermit.git"',
                f'CANONICAL_REMOTE_URL="{self.remote}"',
            ).replace('GIT_TRANSPORT_ALLOWLIST="https"', 'GIT_TRANSPORT_ALLOWLIST="file"')
        )
        self.write_health_tick("old")
        git(
            self.repo,
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            str(self.hermit_remote),
            "hermit",
        )
        (self.repo / "agent-utils").symlink_to("hermit/agent-utils")
        git(self.repo, "add", "scripts", "ci-hub", ".gitmodules", "hermit", "agent-utils")
        git(self.repo, "commit", "-m", "seed tick")
        git(self.repo, "remote", "add", "origin", str(self.remote))
        git(self.repo, "push", "-u", "origin", "main")
        self.before = git(self.repo, "rev-parse", "HEAD").stdout.strip()

        proxy = self.bin / "with-proxy"
        proxy.write_text('#!/bin/sh\nexec "$@"\n')
        proxy.chmod(0o755)
        self.env = dict(
            os.environ,
            HOME=str(self.home),
            PATH=f"{self.bin}:{os.environ.get('PATH', '')}",
            GIT_ALLOW_PROTOCOL="file",
            HERMIT_HEALTH_TOOL_CACHE=str(base / "health-tool-cache"),
            HERMIT_PARENT_MAIN_LOCK_PATH=str(self.lock),
            HERMIT_PARENT_MAIN_NO_PROXY="1",
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def write_health_tick(self, version: str, repo: Path | None = None) -> None:
        target = repo or self.repo
        script = target / "ci-hub" / "bin" / "health-tick"
        script.write_text(
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "mkdir -p \"$DEV_HERMIT_PARENT/.tick-hub\"\n"
            "printf '%s\\n' tick > \"$DEV_HERMIT_PARENT/.tick-hub/fired-state\"\n"
            f"printf 'NOTE: health-version={version} tool_head=%s state_head=%s tool_root=%s state_root=%s\\n' "
            "\"$(git -C \"$DEV_HERMIT_TOOL_ROOT\" rev-parse HEAD)\" "
            "\"$(git -C \"$DEV_HERMIT_PARENT\" rev-parse HEAD)\" "
            "\"$DEV_HERMIT_TOOL_ROOT\" \"$DEV_HERMIT_PARENT\"\n"
        )
        script.chmod(0o755)

    def advance_remote(
        self,
        *,
        update_wrapper: bool = False,
        health_script: str | None = None,
        extra_executables: dict[str, str] | None = None,
    ) -> str:
        other = Path(self.temp.name) / "publisher"
        git(Path(self.temp.name), "clone", "-q", str(self.remote), str(other))
        git(other, "config", "user.name", "Other")
        git(other, "config", "user.email", "other@example.invalid")
        if health_script is None:
            self.write_health_tick("new", other)
        else:
            health_tick = other / "ci-hub" / "bin" / "health-tick"
            health_tick.write_text(health_script)
            health_tick.chmod(0o755)
        paths = ["ci-hub/bin/health-tick"]
        for relative, content in (extra_executables or {}).items():
            executable = other / relative
            executable.parent.mkdir(parents=True, exist_ok=True)
            executable.write_text(content)
            executable.chmod(0o755)
            paths.append(relative)
        if update_wrapper:
            wrapper = other / "scripts" / "hermit-health-tick.sh"
            wrapper.write_text(wrapper.read_text() + "# fetched wrapper version\n")
            paths.append("scripts/hermit-health-tick.sh")
        git(other, "add", *paths)
        git(other, "commit", "-m", "land new health tick")
        git(other, "push", "-q", "origin", "HEAD:main")
        return git(other, "rev-parse", "HEAD").stdout.strip()

    def run_tick(self) -> subprocess.CompletedProcess[str]:
        return run(str(self.repo / "scripts/hermit-health-tick.sh"), cwd=self.repo, env=self.env)

    def run_current_tool(
        self, bootstrap: Path, relative: str, *args: str
    ) -> subprocess.CompletedProcess[str]:
        return run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--tool-relative",
            relative,
            "--",
            *args,
            cwd=self.home,
            env=self.env,
        )

    def installed_bootstrap(self) -> Path:
        bootstrap = self.home / ".local/libexec/dev-hermit/operational-tool"
        bootstrap.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(self.repo / "scripts/hermit-health-tick.sh", bootstrap)
        bootstrap.chmod(0o555)
        return bootstrap

    def prime_cached_tool_root(self) -> tuple[str, Path, Path]:
        target = self.advance_remote()
        tick = self.repo / "ci-hub" / "bin" / "health-tick"
        tick.write_text(tick.read_text() + "# local work\n")
        result = self.run_tick()
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        cached = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees" / target
        self.assertTrue(cached.is_dir())
        return target, cached, tick

    def advance_remote_version(
        self,
        version: int,
        *,
        extra_executables: dict[str, str] | None = None,
    ) -> str:
        publisher = self.base / "publisher"
        if publisher.exists():
            shutil.rmtree(publisher)
        return self.advance_remote(
            health_script=(
                "#!/usr/bin/env bash\n"
                "set -eu\n"
                f"printf 'health-version={version}\\n'\n"
            ),
            extra_executables=extra_executables,
        )

    def cached_tool_roots(self) -> set[str]:
        trees = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees"
        return {
            path.name
            for path in trees.iterdir()
            if path.is_dir() and len(path.name) == 40
        }

    def make_cache_writable(self, cached: Path) -> None:
        changed = run("chmod", "-R", "u+w", str(cached), cwd=self.repo)
        self.assertEqual(0, changed.returncode, changed.stdout + changed.stderr)

    def poison_same_size(self, path: Path, marker: Path) -> None:
        original = path.read_bytes()
        metadata = path.stat()
        poison = (
            "#!/usr/bin/env bash\n"
            f"printf poison > {marker}\n"
            "exit 0\n"
        ).encode()
        self.assertLessEqual(len(poison), len(original))
        path.write_bytes(poison + b"#" * (len(original) - len(poison)))
        os.utime(path, ns=(metadata.st_atime_ns, metadata.st_mtime_ns))

    def assert_private_run_pool_is_reusable(self) -> None:
        entries = list(Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]).glob(".run.*"))
        self.assertEqual(1, len(entries), "normal runs must reuse one private root")
        self.assertTrue(entries[0].is_dir())
        self.assertEqual([], list(entries[0].iterdir()))

    def assert_cached_tool_refused(self, marker: Path) -> None:
        result = self.run_tick()
        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("cached tool root", result.stderr)
        self.assertIn("incomplete or dirty; refusing to repair it in place", result.stderr)
        self.assertFalse(marker.exists())
        self.assert_private_run_pool_is_reusable()

    def test_clean_parent_fast_forwards_before_loading_health_tick(self) -> None:
        target = self.advance_remote(update_wrapper=True)

        result = self.run_tick()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        log = self.log.read_text()
        self.assertIn(f"parent_sync_rc=0 before={self.before} after={target}", log)
        self.assertIn("state=fast-forwarded", log)
        self.assertIn(f"health-version=new tool_head={target} state_head={target}", log)
        self.assertNotIn("health-version=old", log)
        self.assertTrue(
            (self.repo / "scripts" / "hermit-health-tick.sh")
            .read_text()
            .endswith("# fetched wrapper version\n")
        )

    def test_empty_private_run_root_is_reused_without_accumulation(self) -> None:
        self.advance_remote()

        first = self.run_tick()
        self.assertEqual(0, first.returncode, first.stdout + first.stderr)
        entries = list(Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]).glob(".run.*"))
        self.assertEqual(1, len(entries))
        first_identity = (entries[0].stat().st_dev, entries[0].stat().st_ino)
        self.assertEqual([], list(entries[0].iterdir()))

        second = self.run_tick()
        self.assertEqual(0, second.returncode, second.stdout + second.stderr)
        entries = list(Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]).glob(".run.*"))
        self.assertEqual(1, len(entries))
        self.assertEqual(
            first_identity,
            (entries[0].stat().st_dev, entries[0].stat().st_ino),
        )
        self.assertEqual([], list(entries[0].iterdir()))

    def test_cached_tool_roots_stay_bounded_when_main_advances(self) -> None:
        bootstrap = self.installed_bootstrap()
        first = self.advance_remote_version(1)
        first_run = self.run_current_tool(bootstrap, "ci-hub/bin/health-tick")
        self.assertEqual(
            0, first_run.returncode, first_run.stdout + first_run.stderr
        )
        second = self.advance_remote_version(2)
        second_run = self.run_current_tool(bootstrap, "ci-hub/bin/health-tick")
        self.assertEqual(
            0, second_run.returncode, second_run.stdout + second_run.stderr
        )
        third = self.advance_remote_version(3)

        third_run = self.run_current_tool(bootstrap, "ci-hub/bin/health-tick")

        self.assertEqual(
            0, third_run.returncode, third_run.stdout + third_run.stderr
        )
        self.assertEqual({second, third}, self.cached_tool_roots())
        self.assertIn(f"retired cached tool root {first}", third_run.stderr)

        predecessor = run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--tool-parent-sha",
            second,
            "--tool-relative",
            "ci-hub/bin/health-tick",
            cwd=self.home,
            env=self.env,
        )
        self.assertEqual(
            0, predecessor.returncode, predecessor.stdout + predecessor.stderr
        )
        retired = run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--tool-parent-sha",
            first,
            "--tool-relative",
            "ci-hub/bin/health-tick",
            cwd=self.home,
            env=self.env,
        )
        self.assertEqual(1, retired.returncode, retired.stdout + retired.stderr)
        self.assertIn(
            f"exact cached tool root {first} is absent", retired.stderr
        )

    def test_live_cached_tool_root_is_preserved_then_retired(self) -> None:
        bootstrap = self.installed_bootstrap()
        started = self.base / "cached-tool-started"
        proceed = self.base / "cached-tool-proceed"
        blocking_probe = (
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            f': > "{started}"\n'
            f'while [[ ! -e "{proceed}" ]]; do sleep 0.02; done\n'
        )
        first = self.advance_remote_version(
            1, extra_executables={"scripts/blocking-probe": blocking_probe}
        )
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.repo),
                "--tool-relative",
                "scripts/blocking-probe",
            ],
            cwd=self.home,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 15
            while not started.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("tool did not reach the cached-root liveness barrier")
                time.sleep(0.01)
            self.assertIsNone(process.poll())

            second = self.advance_remote_version(2)
            second_run = self.run_current_tool(
                bootstrap, "ci-hub/bin/health-tick"
            )
            self.assertEqual(
                0, second_run.returncode, second_run.stdout + second_run.stderr
            )
            third = self.advance_remote_version(3)
            third_run = self.run_current_tool(
                bootstrap, "ci-hub/bin/health-tick"
            )
            self.assertEqual(
                0, third_run.returncode, third_run.stdout + third_run.stderr
            )
            self.assertEqual({first, second, third}, self.cached_tool_roots())
            self.assertIn(
                f"cached tool root {first} is in use; preserving it",
                third_run.stderr,
            )

            proceed.touch()
            stdout, stderr = process.communicate(timeout=15)
            self.assertEqual(0, process.returncode, stdout + stderr)

            fourth = self.advance_remote_version(4)
            fourth_run = self.run_current_tool(
                bootstrap, "ci-hub/bin/health-tick"
            )
            self.assertEqual(
                0, fourth_run.returncode, fourth_run.stdout + fourth_run.stderr
            )
            self.assertEqual({third, fourth}, self.cached_tool_roots())
            self.assertIn(f"retired cached tool root {first}", fourth_run.stderr)
            self.assertIn(f"retired cached tool root {second}", fourth_run.stderr)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

    def test_unrelated_dirty_paths_are_preserved_while_parent_advances(self) -> None:
        target = self.advance_remote()
        (self.repo / "HANDOFF.md").write_text("unread handoff\n")
        (self.repo / "wpanic").mkdir()
        (self.repo / "wpanic" / "probe.txt").write_text("untracked work\n")
        status_before = git(
            self.repo,
            "status", "--porcelain=v1", "--untracked-files=all", "--",
            "HANDOFF.md", "wpanic",
        ).stdout

        result = self.run_tick()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertNotEqual(target, self.before)
        self.assertEqual(
            status_before,
            git(
                self.repo,
                "status", "--porcelain=v1", "--untracked-files=all", "--",
                "HANDOFF.md", "wpanic",
            ).stdout,
        )
        self.assertEqual("unread handoff\n", (self.repo / "HANDOFF.md").read_text())
        self.assertEqual("untracked work\n", (self.repo / "wpanic" / "probe.txt").read_text())
        log = self.log.read_text()
        self.assertIn(f"parent_sync_rc=0 before={self.before} after={target}", log)
        self.assertIn("state=fast-forwarded", log)
        self.assertIn("overlap=0", log)
        self.assertIn(f"health-version=new tool_head={target} state_head={target}", log)
        self.assertNotIn("component=parent-checkout-sync state=blocked", log)

    def test_overlapping_dirty_path_is_named_and_not_advanced(self) -> None:
        target = self.advance_remote()
        tick = self.repo / "ci-hub" / "bin" / "health-tick"
        tick.write_text(tick.read_text() + "# local work\n")

        result = self.run_tick()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(self.before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertNotEqual(target, self.before)
        self.assertTrue(tick.read_text().endswith("# local work\n"))
        log = self.log.read_text()
        self.assertIn("component=parent-checkout-sync state=blocked", log)
        self.assertIn(f"before={self.before} after={self.before}", log)
        self.assertIn(
            "incoming=ci-hub/bin/health-tick dirty=ci-hub/bin/health-tick",
            log,
        )
        self.assertIn(
            f"health-version=new tool_head={target} state_head={self.before}", log
        )
        self.assertNotIn("health-version=old", log)
        tool_root = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees" / target
        self.assertEqual(target, git(tool_root, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(
            self.hermit_head,
            git(tool_root / "hermit", "rev-parse", "HEAD").stdout.strip(),
        )
        self.assertEqual(
            self.agent_utils_head,
            git(
                tool_root / "hermit" / "agent-utils", "rev-parse", "HEAD"
            ).stdout.strip(),
        )
        self.assertEqual("", git(tool_root, "status", "--porcelain=v1").stdout)
        self.assertRegex(log, r"tool_root=/proc/[0-9]+/fd/[0-9]+")
        self.assert_private_run_pool_is_reusable()

    def test_installed_bootstrap_runs_current_tool_without_touching_stale_primary(
        self,
    ) -> None:
        bootstrap = self.installed_bootstrap()
        payload = (
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "cat \"$DEV_HERMIT_TOOL_AUTHORITY\"\n"
            "printf 'payload=landed parent=%s hermit=%s agent_utils=%s tool_root=%s state_root=%s authority=%s content=%s operational_tool=%s cache=%s args=%s\\n' "
            '"$DEV_HERMIT_TOOL_PARENT_SHA" "$DEV_HERMIT_TOOL_HERMIT_SHA" '
            '"$DEV_HERMIT_TOOL_AGENT_UTILS_SHA" "$DEV_HERMIT_TOOL_ROOT" '
            '"$DEV_HERMIT_PARENT" "$DEV_HERMIT_TOOL_AUTHORITY" '
            '"$DEV_HERMIT_TOOL_CONTENT_SHA256" "$DEV_HERMIT_OPERATIONAL_TOOL" '
            '"$DEV_HERMIT_TOOL_CACHE" "$*"\n'
        )
        target = self.advance_remote(
            extra_executables={"scripts/owner-probe": payload}
        )
        stale = self.repo / "scripts/owner-probe"
        stale.write_text("#!/usr/bin/env bash\nprintf 'payload=STALE\\n'\n")
        stale.chmod(0o755)
        before_head = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        before_status = git(
            self.repo, "status", "--porcelain=v1", "--untracked-files=all"
        ).stdout

        result = self.run_current_tool(bootstrap, "scripts/owner-probe", "one", "two")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn(f"payload=landed parent={target}", result.stdout)
        self.assertIn(f"hermit={self.hermit_head}", result.stdout)
        self.assertIn(f"agent_utils={self.agent_utils_head}", result.stdout)
        self.assertIn(f"state_root={self.repo}", result.stdout)
        self.assertRegex(result.stdout, r"authority=/proc/[0-9]+/fd/[0-9]+")
        self.assertRegex(result.stdout, r"content=[0-9a-f]{64}")
        self.assertIn(f"operational_tool={bootstrap}", result.stdout)
        self.assertIn(
            f"cache={Path(self.env['HERMIT_HEALTH_TOOL_CACHE'])}", result.stdout
        )
        self.assertIn("args=one two", result.stdout)
        self.assertNotIn("payload=STALE", result.stdout)
        self.assertIn(f"parent_sha={target}", result.stderr)
        self.assertIn("entrypoint=scripts/owner-probe", result.stderr)
        authority = json.loads(
            next(line for line in result.stdout.splitlines() if line.startswith("{"))
        )
        tool_pid, tool_fd = result.stdout.split("tool_root=/proc/", 1)[1].split(
            " ", 1
        )[0].split("/fd/")
        authority_pid, authority_fd = result.stdout.split(
            "authority=/proc/", 1
        )[1].split(" ", 1)[0].split("/fd/")
        self.assertEqual(tool_pid, authority_pid)
        self.assertEqual(int(tool_fd), authority["root_fd"])
        self.assertEqual(int(authority_fd), authority["authority_fd"])
        cached = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees" / target
        self.assertEqual(str(cached), authority["target_root"])
        cached_metadata = cached.stat()
        self.assertEqual(cached_metadata.st_dev, authority["target_dev"])
        self.assertEqual(cached_metadata.st_ino, authority["target_ino"])
        self.assertEqual(
            3,
            len(
                {
                    (authority["target_dev"], authority["target_ino"]),
                    (authority["root_dev"], authority["root_ino"]),
                    (authority["state_dev"], authority["state_ino"]),
                }
            ),
        )
        self.assertEqual(
            4,
            len(
                {
                    authority["authority_fd"],
                    authority["target_fd"],
                    authority["root_fd"],
                    authority["state_fd"],
                }
            ),
        )
        self.assertEqual(target, authority["parent_sha"])
        self.assertEqual(self.hermit_head, authority["hermit_sha"])
        self.assertEqual(self.agent_utils_head, authority["agent_utils_sha"])
        self.assertEqual(str(self.repo), authority["state_root"])
        self.assertEqual(
            authority["content_sha256"],
            result.stdout.split("content=", 1)[1].split(" ", 1)[0],
        )
        self.assertEqual(
            before_head, git(self.repo, "rev-parse", "HEAD").stdout.strip()
        )
        self.assertEqual(
            before_status,
            git(self.repo, "status", "--porcelain=v1", "--untracked-files=all").stdout,
        )
        self.assertTrue((cached / ".git").is_dir())
        self.assertFalse((cached / ".git" / "commondir").exists())
        self.assert_private_run_pool_is_reusable()

    def test_installed_bootstrap_runs_entire_wrkslots_audit_chain_from_current_tool(
        self,
    ) -> None:
        bootstrap = self.installed_bootstrap()
        tool_cost = """#!/usr/bin/env python3
import json
from pathlib import Path
import subprocess
import sys

args = sys.argv[1:]
separator = args.index("--")
actual_path = Path(args[args.index("--actual-json") + 1])
completed = subprocess.run(args[separator + 1 :], check=False)
actual_path.write_text(json.dumps({
    "schema_version": 1,
    "tool": "ci-hub/wrkslots-audit",
    "estimate": {
        "kind": "derived",
        "wall_seconds": 190.0,
        "cpu_seconds": 179.0,
        "basis": "derived from one completed contention audit",
    },
    "actual": {
        "wall_seconds": 1.0,
        "cpu_seconds": 0.0,
        "cpu_user_seconds": 0.0,
        "cpu_system_seconds": 0.0,
        "exit": str(completed.returncode),
    },
}))
raise SystemExit(completed.returncode)
"""
        box = """#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
separator = args.index("--")
actual_path = Path(args[args.index("--actual-json") + 1])
usage_usec = 1_000_000
actual_path.write_text(json.dumps({
    "schema": "hermit-box-run-actual/v1",
    "label": "wrkslots-audit",
    "scope": "hermit-box-run-123-456.scope",
    "cpu_source": "cgroup-v2 cpu.stat usage_usec",
    "cgroup_usage_usec": usage_usec,
    "cpu_seconds": usage_usec / 1_000_000,
    "cpu_budget_seconds": int(args[args.index("--cpu-budget") + 1]),
    "wall_backstop_seconds": int(args[args.index("--wall") + 1]),
    "reaped": False,
    "termination": "completed",
    "exit_code": 0,
}))
Path(os.environ["DEV_HERMIT_PARENT"], ".tick-hub").mkdir(exist_ok=True)
Path(os.environ["DEV_HERMIT_PARENT"], ".tick-hub", "audit-chain.json").write_text(
    json.dumps({
        "tool_root": os.environ["DEV_HERMIT_TOOL_ROOT"],
        "entrypoint": str(Path(__file__)),
        "inner": args[separator + 1 :],
    })
)
os.execv(args[separator + 1], args[separator + 1 :])
"""
        wrkslots = """#!/usr/bin/env python3
import json
print(json.dumps({
    "schema": 2,
    "state": "ok",
    "summary": "no worktree slot currently needs coordinator attention",
    "worktree_count": 0,
    "running_agent_count": 0,
    "attention_count": 0,
    "attention_slots": [],
    "unknown_count": 0,
    "unknown_slots": [],
}))
"""
        target = self.advance_remote(
            extra_executables={
                "ci-hub/bin/wrkslots-audit": (
                    SOURCE / "ci-hub/bin/wrkslots-audit"
                ).read_text(),
                "ci-hub/bin/tool-cost": tool_cost,
                "ci-hub/health/wrkslots_audit_contract.py": (
                    SOURCE / "ci-hub/health/wrkslots_audit_contract.py"
                ).read_text(),
                "scripts/hermit-box-run": box,
                "ci-hub/bin/wrkslots": wrkslots,
            }
        )
        stale_marker = self.repo / ".tick-hub/stale-audit-ran"
        stale = self.repo / "ci-hub/bin/wrkslots-audit"
        stale.write_text(
            "#!/usr/bin/env bash\n"
            f": > {stale_marker}\n"
            "exit 97\n"
        )
        stale.chmod(0o755)
        result_path = self.repo / "ignored/ci-hub/wrkslots-audit/latest.json"
        stale_before = stale.read_bytes()

        completed = self.run_current_tool(
            bootstrap,
            "ci-hub/bin/wrkslots-audit",
            "--result",
            str(result_path),
        )

        self.assertEqual(0, completed.returncode, completed.stdout + completed.stderr)
        self.assertFalse(stale_marker.exists())
        receipt = json.loads(result_path.read_text())
        self.assertEqual(target, receipt["producer"]["parent_sha"])
        self.assertEqual(self.hermit_head, receipt["producer"]["hermit_sha"])
        self.assertEqual(
            self.agent_utils_head, receipt["producer"]["agent_utils_sha"]
        )
        chain = json.loads((self.repo / ".tick-hub/audit-chain.json").read_text())
        self.assertRegex(chain["tool_root"], r"^/proc/[0-9]+/fd/[0-9]+$")
        self.assertEqual(
            f"{chain['tool_root']}/scripts/hermit-box-run", chain["entrypoint"]
        )
        self.assertEqual(
            f"{chain['tool_root']}/ci-hub/bin/wrkslots", chain["inner"][0]
        )
        self.assertEqual(stale_before, stale.read_bytes())
        self.assert_private_run_pool_is_reusable()

    def test_primary_executable_git_config_is_never_consulted_by_immutable_tool(
        self,
    ) -> None:
        bootstrap = self.installed_bootstrap()
        marker = self.base / "hostile-primary-config-ran"
        hostile = self.base / "hostile-primary-config"
        hostile.write_text(
            "#!/usr/bin/env bash\n"
            f"printf consulted > {marker}\n"
            "exit 1\n"
        )
        hostile.chmod(0o755)
        target = self.advance_remote(
            extra_executables={
                "scripts/owner-probe": "#!/usr/bin/env bash\nprintf 'SAFE\\n'\n"
            }
        )
        git(self.repo, "config", "core.fsmonitor", str(hostile))
        config_before = (self.repo / ".git/config").read_bytes()

        result = self.run_current_tool(bootstrap, "scripts/owner-probe")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("SAFE", result.stdout)
        self.assertFalse(marker.exists())
        self.assertEqual(config_before, (self.repo / ".git/config").read_bytes())
        cached = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees" / target
        self.assertTrue((cached / ".git").is_dir())
        self.assertFalse((cached / ".git" / "commondir").exists())

    def test_state_path_replacement_refuses_before_authority_is_published(self) -> None:
        bootstrap = self.installed_bootstrap()
        marker = self.base / "state-replacement-payload-ran"
        target = self.advance_remote(
            extra_executables={
                "scripts/owner-probe": (
                    "#!/usr/bin/env bash\n"
                    f"printf poison > {marker}\n"
                )
            }
        )
        primed = self.run_current_tool(bootstrap, "scripts/owner-probe")
        self.assertEqual(0, primed.returncode, primed.stdout + primed.stderr)
        marker.unlink()

        state_root = self.base / "replaceable-state"
        state_root.mkdir()
        retained_state = self.base / "retained-state"
        ready = self.base / "authority-state-ready"
        proceed = self.base / "authority-state-proceed"
        shim_dir = self.base / "authority-state-shim"
        shim_dir.mkdir()
        sha256sum = shutil.which("sha256sum") or "/usr/bin/sha256sum"
        shim = shim_dir / "sha256sum"
        shim.write_text(
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            ": > \"$AUTHORITY_STATE_READY\"\n"
            "while [[ ! -e \"$AUTHORITY_STATE_PROCEED\" ]]; do sleep 0.01; done\n"
            f'exec "{sha256sum}" "$@"\n'
        )
        shim.chmod(0o755)
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(state_root),
                "--cache-root",
                str(cache),
                "--tool-parent-sha",
                target,
                "--tool-relative",
                "scripts/owner-probe",
            ],
            cwd=self.home,
            env=dict(
                self.env,
                PATH=f"{shim_dir}:{self.env['PATH']}",
                AUTHORITY_STATE_READY=str(ready),
                AUTHORITY_STATE_PROCEED=str(proceed),
            ),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 15
            while not ready.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("authority creation did not reach the state-path barrier")
                time.sleep(0.01)
            self.assertIsNone(process.poll())
            state_root.rename(retained_state)
            state_root.mkdir()
            proceed.touch()
            stdout, stderr = process.communicate(timeout=20)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertEqual(1, process.returncode, stdout + stderr)
        self.assertIn("state root changed before authority was sealed", stderr)
        self.assertFalse(marker.exists())

    def test_target_path_replacement_refuses_before_authority_is_published(self) -> None:
        bootstrap = self.installed_bootstrap()
        marker = self.base / "target-replacement-payload-ran"
        target = self.advance_remote(
            extra_executables={
                "scripts/owner-probe": (
                    "#!/usr/bin/env bash\n"
                    f"printf poison > {marker}\n"
                )
            }
        )
        primed = self.run_current_tool(bootstrap, "scripts/owner-probe")
        self.assertEqual(0, primed.returncode, primed.stdout + primed.stderr)
        marker.unlink()

        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        cached = cache / "trees" / target
        retained_target = cache / "trees" / "retained-target"
        ready = self.base / "authority-target-ready"
        proceed = self.base / "authority-target-proceed"
        shim_dir = self.base / "authority-target-shim"
        shim_dir.mkdir()
        sha256sum = shutil.which("sha256sum") or "/usr/bin/sha256sum"
        shim = shim_dir / "sha256sum"
        shim.write_text(
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            ": > \"$AUTHORITY_TARGET_READY\"\n"
            "while [[ ! -e \"$AUTHORITY_TARGET_PROCEED\" ]]; do sleep 0.01; done\n"
            f'exec "{sha256sum}" "$@"\n'
        )
        shim.chmod(0o755)
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.repo),
                "--cache-root",
                str(cache),
                "--tool-parent-sha",
                target,
                "--tool-relative",
                "scripts/owner-probe",
            ],
            cwd=self.home,
            env=dict(
                self.env,
                PATH=f"{shim_dir}:{self.env['PATH']}",
                AUTHORITY_TARGET_READY=str(ready),
                AUTHORITY_TARGET_PROCEED=str(proceed),
            ),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 15
            while not ready.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("authority creation did not reach the target-path barrier")
                time.sleep(0.01)
            self.assertIsNone(process.poll())
            cached.rename(retained_target)
            cached.mkdir()
            cached.chmod(0o555)
            proceed.touch()
            stdout, stderr = process.communicate(timeout=20)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertEqual(1, process.returncode, stdout + stderr)
        self.assertIn("cached target changed before authority was sealed", stderr)
        self.assertFalse(marker.exists())

    def test_current_tool_refuses_cache_beneath_mutable_state_root(self) -> None:
        bootstrap = self.installed_bootstrap()
        before_head = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        before_status = git(
            self.repo, "status", "--porcelain=v1", "--untracked-files=all"
        ).stdout

        result = run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--cache-root",
            str(self.repo / "ignored/tool-cache"),
            "--tool-relative",
            "scripts/owner-checkout-validate-watch.py",
            cwd=self.home,
            env=self.env,
        )

        self.assertEqual(2, result.returncode, result.stdout + result.stderr)
        self.assertIn(
            "cache root must be outside mutable state root", result.stderr
        )
        self.assertFalse((self.repo / "ignored/tool-cache").exists())
        self.assertEqual(
            before_head, git(self.repo, "rev-parse", "HEAD").stdout.strip()
        )
        self.assertEqual(
            before_status,
            git(self.repo, "status", "--porcelain=v1", "--untracked-files=all").stdout,
        )

    def test_exact_tool_parent_refuses_absent_cache_without_following_main(self) -> None:
        bootstrap = self.installed_bootstrap()
        requested = "f" * 40

        result = run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--tool-parent-sha",
            requested,
            "--tool-relative",
            "ci-hub/bin/health-tick",
            cwd=self.home,
            env=self.env,
        )

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn(
            f"exact cached tool root {requested} is absent", result.stderr
        )
        self.assertEqual(
            [],
            list(
                (Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees").iterdir()
            ),
        )

    def test_current_tool_refuses_repointed_primary_origin(self) -> None:
        bootstrap = self.installed_bootstrap()
        malicious = self.base / "malicious.git"
        git(self.base, "init", "--bare", "--initial-branch=main", str(malicious))
        git(self.repo, "remote", "set-url", "origin", str(malicious))

        result = self.run_current_tool(
            bootstrap, "scripts/owner-checkout-validate-watch.py", "--dry-run"
        )

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("refusing mutable remote authority", result.stderr)
        self.assertIn("expected canonical rrnewton/dev-hermit", result.stderr)
        self.assertFalse(
            list(Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]).glob("trees/*"))
        )

    def test_current_tool_scrubs_git_authority_environment_without_outside_writes(
        self,
    ) -> None:
        bootstrap = self.installed_bootstrap()
        outside = self.base / "outside-git-authority"
        objects = outside / "objects"
        alternate = outside / "alternate-objects"
        worktree = outside / "worktree"
        hooks = outside / "hooks"
        for directory in (objects, alternate, worktree, hooks):
            directory.mkdir(parents=True)
        sentinel = outside / "sentinel"
        sentinel.write_text("must remain exact\n")
        before_head = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        before_status = git(
            self.repo, "status", "--porcelain=v1", "--untracked-files=all"
        ).stdout

        def snapshot() -> dict[str, tuple[str, bytes | None]]:
            result: dict[str, tuple[str, bytes | None]] = {}
            for path in sorted(outside.rglob("*")):
                relative = str(path.relative_to(outside))
                if path.is_dir():
                    result[relative] = ("directory", None)
                else:
                    result[relative] = ("file", path.read_bytes())
            return result

        before = snapshot()
        hostile_env = dict(
            self.env,
            GIT_INDEX_FILE=str(outside / "index"),
            GIT_OBJECT_DIRECTORY=str(objects),
            GIT_ALTERNATE_OBJECT_DIRECTORIES=str(alternate),
            GIT_DIR=str(outside / "redirected-git-dir"),
            GIT_WORK_TREE=str(worktree),
            GIT_CONFIG_GLOBAL=str(outside / "global-config"),
            GIT_CONFIG_SYSTEM=str(outside / "system-config"),
            GIT_CONFIG_COUNT="1",
            GIT_CONFIG_KEY_0="core.hooksPath",
            GIT_CONFIG_VALUE_0=str(hooks),
            GIT_ALLOW_PROTOCOL="ext",
        )

        result = run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--tool-relative",
            "scripts/absent-owner-tool",
            cwd=self.home,
            env=hostile_env,
        )

        self.assertEqual(2, result.returncode, result.stdout + result.stderr)
        self.assertIn("immutable tool is absent", result.stderr)
        self.assertEqual(before, snapshot())
        self.assertEqual("must remain exact\n", sentinel.read_text())
        self.assertEqual(
            before_head, git(self.repo, "rev-parse", "HEAD").stdout.strip()
        )
        self.assertEqual(
            before_status,
            git(
                self.repo,
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
            ).stdout,
        )

    def test_current_tool_supplies_fixed_github_auth_with_global_config_disabled(
        self,
    ) -> None:
        bootstrap = self.installed_bootstrap()
        target = self.advance_remote(
            extra_executables={
                "scripts/owner-probe": "#!/usr/bin/env bash\nprintf 'authenticated tool\\n'\n"
            }
        )
        outside = self.base / "outside-auth-config"
        outside.mkdir()
        hostile_marker = outside / "hostile-helper-ran"
        hostile_config = outside / "global-config"
        hostile_config.write_text(
            '[credential "https://github.com"]\n'
            f"\thelper = !printf hostile > {hostile_marker}\n"
        )
        auth_log = outside / "network-git.log"
        shim_dir = outside / "bin"
        shim_dir.mkdir()
        git_shim = shim_dir / "git"
        git_shim.write_text(
            "#!/usr/bin/env python3\n"
            "import os\n"
            "from pathlib import Path\n"
            "import sys\n"
            f"real_git = {str(REAL_GIT)!r}\n"
            "args = sys.argv[1:]\n"
            "network_commands = {'ls-remote', 'clone', 'fetch', 'submodule'}\n"
            "is_wrapper_network = (\n"
            "    '--no-replace-objects' in args\n"
            "    and any(command in args for command in network_commands)\n"
            ")\n"
            "if is_wrapper_network:\n"
            "    configs = [args[index + 1] for index, value in enumerate(args[:-1]) if value == '-c']\n"
            "    expected = 'credential.https://github.com.helper=!/usr/bin/gh auth git-credential'\n"
            "    if 'credential.https://github.com.helper=' not in configs or expected not in configs:\n"
            "        print('fatal: authentication required', file=sys.stderr)\n"
            "        raise SystemExit(128)\n"
            "    if os.environ.get('GIT_CONFIG_GLOBAL') != os.devnull:\n"
            "        print('fatal: ambient global config was not disabled', file=sys.stderr)\n"
            "        raise SystemExit(129)\n"
            "    if os.environ.get('GIT_CONFIG_NOSYSTEM') != '1':\n"
            "        print('fatal: system config was not disabled', file=sys.stderr)\n"
            "        raise SystemExit(130)\n"
            "    with Path(os.environ['AUTH_GIT_LOG']).open('a') as log:\n"
            "        log.write(next(command for command in network_commands if command in args) + '\\n')\n"
            "os.execv(real_git, [real_git, *args])\n"
        )
        git_shim.chmod(0o755)

        result = run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--tool-relative",
            "scripts/owner-probe",
            cwd=self.home,
            env=dict(
                self.env,
                PATH=f"{shim_dir}:{self.env['PATH']}",
                GIT_CONFIG_GLOBAL=str(hostile_config),
                GIT_CONFIG_SYSTEM=str(hostile_config),
                GIT_CONFIG_COUNT="1",
                GIT_CONFIG_KEY_0="credential.https://github.com.helper",
                GIT_CONFIG_VALUE_0=f"!printf hostile > {hostile_marker}",
                AUTH_GIT_LOG=str(auth_log),
            ),
        )

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("authenticated tool", result.stdout)
        self.assertIn(f"parent_sha={target}", result.stderr)
        self.assertFalse(hostile_marker.exists())
        observed = auth_log.read_text().splitlines()
        self.assertIn("ls-remote", observed)
        self.assertIn("clone", observed)
        self.assertIn("submodule", observed)

    def test_current_tool_refuses_when_fixed_github_auth_helper_is_missing(
        self,
    ) -> None:
        bootstrap = self.installed_bootstrap()
        bootstrap.chmod(0o755)
        bootstrap.write_text(
            bootstrap.read_text().replace(
                'GITHUB_CREDENTIAL_HELPER="/usr/bin/gh"',
                f'GITHUB_CREDENTIAL_HELPER="{self.base / "missing-gh"}"',
            )
        )
        bootstrap.chmod(0o555)

        result = self.run_current_tool(
            bootstrap, "scripts/owner-checkout-validate-watch.py", "--dry-run"
        )

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("required GitHub credential helper is unavailable", result.stderr)
        self.assertFalse(
            list(Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]).glob("trees/*"))
        )

    def test_current_tool_accepts_canonical_ssh_origin(self) -> None:
        bootstrap = self.installed_bootstrap()
        target = self.advance_remote(
            extra_executables={
                "scripts/owner-probe": "#!/usr/bin/env bash\nprintf 'canonical ssh origin\\n'\n"
            }
        )
        git(
            self.repo,
            "remote",
            "set-url",
            "origin",
            "git@github.com:rrnewton/dev-hermit.git",
        )

        result = self.run_current_tool(
            bootstrap, "scripts/owner-probe"
        )

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("canonical ssh origin", result.stdout)
        self.assertIn(f"parent_sha={target}", result.stderr)

    def test_current_tool_refuses_trees_symlink_without_writing_through_it(self) -> None:
        bootstrap = self.installed_bootstrap()
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        outside = self.base / "outside-trees"
        cache.mkdir(parents=True)
        outside.mkdir()
        (cache / "trees").symlink_to(outside, target_is_directory=True)

        result = self.run_current_tool(
            bootstrap, "scripts/owner-checkout-validate-watch.py", "--dry-run"
        )

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("cache path component is not a real directory", result.stderr)
        self.assertEqual([], list(outside.iterdir()))

    def test_current_tool_refuses_lock_symlink_without_truncating_target(self) -> None:
        bootstrap = self.installed_bootstrap()
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        (cache / "trees").mkdir(parents=True)
        sentinel = self.base / "lock-target"
        sentinel.write_text("must survive\n")
        (cache / "materialize.lock").symlink_to(sentinel)

        result = self.run_current_tool(
            bootstrap, "scripts/owner-checkout-validate-watch.py", "--dry-run"
        )

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("materialize lock must not be a symlink", result.stderr)
        self.assertEqual("must survive\n", sentinel.read_text())

    def test_current_tool_refuses_lock_replaced_while_waiting(self) -> None:
        bootstrap = self.installed_bootstrap()
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        (cache / "trees").mkdir(parents=True)
        lock_path = cache / "materialize.lock"
        lock_path.write_text("")
        original_stat = lock_path.stat()
        lock_fd = os.open(lock_path, os.O_RDONLY)
        fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.repo),
                "--tool-relative",
                "scripts/owner-checkout-validate-watch.py",
                "--",
                "--dry-run",
            ],
            cwd=self.home,
            env=dict(self.env, HERMIT_HEALTH_TOOL_LOCK_TIMEOUT="10"),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    self.fail(
                        "bootstrap exited before waiting on the held lock: "
                        + process.communicate()[1]
                    )
                try:
                    opened = os.stat(f"/proc/{process.pid}/fd/8")
                    flock_child = subprocess.run(
                        ["pgrep", "-P", str(process.pid), "-x", "flock"],
                        check=False,
                        text=True,
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                    )
                    waiting = flock_child.returncode == 0
                except (FileNotFoundError, ProcessLookupError):
                    waiting = False
                if (
                    waiting
                    and opened.st_dev == original_stat.st_dev
                    and opened.st_ino == original_stat.st_ino
                ):
                    break
                time.sleep(0.01)
            else:
                self.fail("bootstrap did not enter a blocking flock on the original inode")

            replaced = cache / "materialize.lock.replaced"
            lock_path.rename(replaced)
            lock_path.write_text("")
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
            os.close(lock_fd)
            lock_fd = -1
            stdout, stderr = process.communicate(timeout=10)
        finally:
            if lock_fd >= 0:
                fcntl.flock(lock_fd, fcntl.LOCK_UN)
                os.close(lock_fd)
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertEqual(1, process.returncode, stdout + stderr)
        self.assertIn("materialize lock changed after it was locked", stderr)
        self.assertEqual(original_stat.st_ino, replaced.stat().st_ino)
        self.assertNotEqual(original_stat.st_ino, lock_path.stat().st_ino)
        self.assertEqual([], list((cache / "trees").iterdir()))

    def test_cache_publish_collision_cannot_chmod_symlink_target(self) -> None:
        bootstrap = self.installed_bootstrap()
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        outside = self.base / "outside-cache-target"
        outside.mkdir()
        sentinel = outside / "sentinel"
        sentinel.write_text("must remain writable\n")
        sentinel.chmod(0o600)
        ready = self.base / "publish-ready"
        proceed = self.base / "publish-proceed"
        shim_dir = self.base / "publish-python-shim"
        shim_dir.mkdir()
        python_shim = shim_dir / "python3"
        python_shim.write_text(
            "#!/usr/bin/env bash\n"
            "set -u\n"
            "if [[ $# -eq 7 && $1 == - && $5 == .staging.* ]]; then\n"
            "  : > \"$PUBLISH_COLLISION_READY\"\n"
            "  while [[ ! -e \"$PUBLISH_COLLISION_PROCEED\" ]]; do sleep 0.01; done\n"
            "fi\n"
            f'exec "{REAL_PYTHON}" "$@"\n'
        )
        python_shim.chmod(0o755)
        target = git(self.remote, "rev-parse", "refs/heads/main").stdout.strip()
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.repo),
                "--tool-relative",
                "scripts/owner-checkout-validate-watch.py",
                "--",
                "--dry-run",
            ],
            cwd=self.home,
            env=dict(
                self.env,
                PATH=f"{shim_dir}:{self.env['PATH']}",
                PUBLISH_COLLISION_READY=str(ready),
                PUBLISH_COLLISION_PROCEED=str(proceed),
            ),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 15
            while not ready.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("cache publication did not reach the deterministic barrier")
                time.sleep(0.01)
            if process.poll() is not None:
                stdout, stderr = process.communicate()
                self.fail(
                    "cache publication exited before the deterministic barrier: "
                    + stdout
                    + stderr
                )
            target_path = cache / "trees" / target
            target_path.symlink_to(outside, target_is_directory=True)
            proceed.touch()
            stdout, stderr = process.communicate(timeout=15)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertEqual(1, process.returncode, stdout + stderr)
        self.assertIn("cache target collision left staging unconsumed", stderr)
        self.assertTrue(target_path.is_symlink())
        self.assertEqual(0o600, sentinel.stat().st_mode & 0o777)
        self.assertEqual("must remain writable\n", sentinel.read_text())
        staging = list((cache / "trees").glob(".staging.*"))
        self.assertEqual(1, len(staging))
        self.assertEqual([], list(staging[0].iterdir()))

    def test_staging_pool_symlink_is_never_followed_or_cleared(self) -> None:
        bootstrap = self.installed_bootstrap()
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        trees = cache / "trees"
        trees.mkdir(parents=True)
        outside = self.base / "outside-staging-pool"
        outside.mkdir()
        sentinel = outside / "sentinel"
        sentinel.write_text("do not clear\n")
        sentinel.chmod(0o600)
        planted = trees / ".staging.deadbeefdeadbeef"
        planted.symlink_to(outside, target_is_directory=True)
        self.advance_remote()

        result = self.run_current_tool(bootstrap, "ci-hub/bin/health-tick")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertTrue(planted.is_symlink())
        self.assertEqual("do not clear\n", sentinel.read_text())
        self.assertEqual(0o600, sentinel.stat().st_mode & 0o777)
        self.assertEqual(
            [],
            [entry for entry in trees.glob(".staging.*") if not entry.is_symlink()],
        )

    def test_stale_staging_root_is_reused_without_accumulation(self) -> None:
        bootstrap = self.installed_bootstrap()
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        trees = cache / "trees"
        stale = trees / ".staging.0123456789abcdef"
        (stale / "nested").mkdir(parents=True)
        (stale / "nested" / "junk").write_text("discard me\n")
        changed = run("chmod", "-R", "a-w", str(stale), cwd=self.repo)
        self.assertEqual(0, changed.returncode, changed.stdout + changed.stderr)
        stale_identity = (stale.stat().st_dev, stale.stat().st_ino)
        target = self.advance_remote()

        result = self.run_current_tool(bootstrap, "ci-hub/bin/health-tick")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        published = trees / target
        self.assertTrue(published.is_dir())
        self.assertEqual(
            stale_identity,
            (published.stat().st_dev, published.stat().st_ino),
        )
        self.assertFalse((published / "nested" / "junk").exists())
        self.assertEqual([], list(trees.glob(".staging.*")))

    def test_verified_staging_swap_cannot_publish_or_chmod_external_hardlink(
        self,
    ) -> None:
        bootstrap = self.installed_bootstrap()
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        outside = self.base / "outside-staging-swap"
        outside.mkdir()
        sentinel = outside / "sentinel"
        sentinel.write_text("must remain writable\n")
        sentinel.chmod(0o600)
        ready = self.base / "staging-swap-ready"
        proceed = self.base / "staging-swap-proceed"
        shim_dir = self.base / "staging-swap-python-shim"
        shim_dir.mkdir()
        python_shim = shim_dir / "python3"
        python_shim.write_text(
            "#!/usr/bin/env bash\n"
            "set -u\n"
            "if [[ $# -eq 7 && $1 == - && $5 == .staging.* ]]; then\n"
            "  : > \"$STAGING_SWAP_READY\"\n"
            "  while [[ ! -e \"$STAGING_SWAP_PROCEED\" ]]; do sleep 0.01; done\n"
            "fi\n"
            f'exec "{REAL_PYTHON}" "$@"\n'
        )
        python_shim.chmod(0o755)
        target = git(self.remote, "rev-parse", "refs/heads/main").stdout.strip()
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.repo),
                "--tool-relative",
                "scripts/owner-checkout-validate-watch.py",
                "--",
                "--dry-run",
            ],
            cwd=self.home,
            env=dict(
                self.env,
                PATH=f"{shim_dir}:{self.env['PATH']}",
                STAGING_SWAP_READY=str(ready),
                STAGING_SWAP_PROCEED=str(proceed),
            ),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 15
            while not ready.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("staging verification did not reach the swap barrier")
                time.sleep(0.01)
            if process.poll() is not None:
                stdout, stderr = process.communicate()
                self.fail(
                    "staging verification exited before the swap barrier: "
                    + stdout
                    + stderr
                )
            staging_entries = list((cache / "trees").glob(".staging.*"))
            self.assertEqual(1, len(staging_entries))
            staging = staging_entries[0]
            retained = staging.with_name(f"{staging.name}.verified")
            staging.rename(retained)
            shutil.copytree(retained, staging, symlinks=True, copy_function=os.link)
            changed = run("chmod", "-R", "u+w", str(staging), cwd=self.repo)
            self.assertEqual(0, changed.returncode, changed.stdout + changed.stderr)
            poisoned = staging / "scripts" / "hermit-health-tick.sh"
            poisoned.unlink()
            os.link(sentinel, poisoned)
            proceed.touch()
            stdout, stderr = process.communicate(timeout=15)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertEqual(1, process.returncode, stdout + stderr)
        self.assertIn("staging directory changed before publication", stderr)
        self.assertFalse((cache / "trees" / target).exists())
        self.assertEqual(0o600, sentinel.stat().st_mode & 0o777)
        self.assertEqual("must remain writable\n", sentinel.read_text())
        self.assertTrue(staging.is_dir(), "replacement staging root was deleted")
        self.assertTrue(retained.is_dir(), "retained verified root disappeared")
        self.assertEqual([], list(retained.iterdir()))

    def test_interruption_after_publication_leaves_read_only_reusable_cache(
        self,
    ) -> None:
        target = self.advance_remote()
        bootstrap = self.installed_bootstrap()
        ready = self.base / "post-publication-ready"
        proceed = self.base / "post-publication-proceed"
        shim_dir = self.base / "post-publication-python-shim"
        shim_dir.mkdir()
        python_shim = shim_dir / "python3"
        python_shim.write_text(
            "#!/usr/bin/env bash\n"
            "set -u\n"
            f'"{REAL_PYTHON}" "$@"\n'
            "rc=$?\n"
            "if [[ $rc -eq 0 && $# -eq 7 && $1 == - && $5 == .staging.* ]]; then\n"
            "  : > \"$POST_PUBLISH_READY\"\n"
            "  while [[ ! -e $POST_PUBLISH_PROCEED ]]; do sleep 0.01; done\n"
            "  kill -TERM \"$PPID\"\n"
            "fi\n"
            "exit \"$rc\"\n"
        )
        python_shim.chmod(0o755)
        env = dict(
            self.env,
            PATH=f"{shim_dir}:{self.env['PATH']}",
            POST_PUBLISH_READY=str(ready),
            POST_PUBLISH_PROCEED=str(proceed),
        )
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.repo),
                "--tool-relative",
                "ci-hub/bin/health-tick",
            ],
            cwd=self.home,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 15
            while not ready.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("publication did not reach the interruption barrier")
                time.sleep(0.01)
            if process.poll() is not None:
                stdout, stderr = process.communicate()
                self.fail(
                    "publication process exited before the interruption barrier: "
                    + stdout
                    + stderr
                )
            published = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees" / target
            self.assertTrue(published.is_dir())
            directories = [
                published,
                *[
                    path
                    for path in published.rglob("*")
                    if not path.is_symlink() and path.is_dir()
                ],
            ]
            self.assertTrue(
                all(path.stat().st_mode & 0o777 == 0o555 for path in directories)
            )
            self.assertTrue(
                all(
                    not path.lstat().st_mode & 0o222
                    for path in published.rglob("*")
                    if not path.is_symlink()
                )
            )
            proceed.touch()
            stdout, stderr = process.communicate(timeout=15)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertNotIn("health-version=", stdout)
        self.assertTrue(published.is_dir(), stderr)
        self.assertFalse(
            list(
                (Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees").glob(
                    ".staging.*"
                )
            )
        )

        resumed = self.run_current_tool(bootstrap, "ci-hub/bin/health-tick")
        self.assertEqual(0, resumed.returncode, resumed.stdout + resumed.stderr)
        self.assert_private_run_pool_is_reusable()

    def test_current_tool_refuses_symlink_in_cache_parent_chain(self) -> None:
        bootstrap = self.installed_bootstrap()
        outside = self.base / "outside-parent"
        outside.mkdir()
        linked = self.base / "linked-cache-parent"
        linked.symlink_to(outside, target_is_directory=True)

        result = run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--cache-root",
            str(linked / "cache"),
            "--tool-relative",
            "scripts/owner-checkout-validate-watch.py",
            cwd=self.home,
            env=self.env,
        )

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("cache path component is not a real directory", result.stderr)
        self.assertFalse((outside / "cache").exists())

    def test_diverged_parent_is_reported_without_reconciliation(self) -> None:
        (self.repo / "local.md").write_text("local\n")
        git(self.repo, "add", "local.md")
        git(self.repo, "commit", "-m", "local move")
        local = git(self.repo, "rev-parse", "HEAD").stdout.strip()
        target = self.advance_remote()

        result = self.run_tick()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(local, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        log = self.log.read_text()
        self.assertIn("component=parent-checkout-sync state=blocked", log)
        self.assertIn("requires a fast-forward", log)
        self.assertIn(f"before={local} after={local}", log)
        self.assertIn(f"target={target}", log)
        self.assertIn(f"health-version=new tool_head={target} state_head={local}", log)

    def test_dirty_cached_tool_root_is_refused_without_replacing_it(self) -> None:
        target = self.advance_remote()
        tick = self.repo / "ci-hub" / "bin" / "health-tick"
        tick.write_text(tick.read_text() + "# local work\n")
        cached = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees" / target
        cached.mkdir(parents=True)
        marker = cached / "foreign-marker"
        marker.write_text("do not replace\n")

        result = self.run_tick()

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("cached tool root", result.stderr)
        self.assertIn("incomplete or dirty; refusing to repair it in place", result.stderr)
        self.assertEqual("do not replace\n", marker.read_text())
        self.assertEqual(self.before, git(self.repo, "rev-parse", "HEAD").stdout.strip())
        self.assertTrue(tick.read_text().endswith("# local work\n"))

    def test_corrupt_cached_index_is_refused_without_repair(self) -> None:
        _target, cached, _tick = self.prime_cached_tool_root()
        self.make_cache_writable(cached)
        git_dir = Path(
            git(cached, "rev-parse", "--absolute-git-dir").stdout.strip()
        )
        index = git_dir / "index"
        index.write_bytes(b"corrupt-index\n")
        marker = self.base / "corrupt-index-ran"

        self.assert_cached_tool_refused(marker)

        self.assertEqual(b"corrupt-index\n", index.read_bytes())

    def test_cached_tracked_hardlink_is_refused_before_chmod(self) -> None:
        _target, cached, _tick = self.prime_cached_tool_root()
        self.make_cache_writable(cached)
        executable = cached / "ci-hub" / "bin" / "health-tick"
        original = executable.read_bytes()
        outside = self.base / "outside-hardlinked-tool"
        outside.write_bytes(original)
        outside.chmod(0o755)
        executable.unlink()
        os.link(outside, executable)

        result = self.run_tick()

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("tracked file 'ci-hub/bin/health-tick' has link count 2", result.stderr)
        self.assertEqual(original, outside.read_bytes())
        self.assertEqual(0o755, outside.stat().st_mode & 0o777)

    def test_assume_unchanged_cannot_hide_poisoned_tool_bytes(self) -> None:
        _target, cached, _tick = self.prime_cached_tool_root()
        self.make_cache_writable(cached)
        executable = cached / "ci-hub" / "bin" / "health-tick"
        marker = self.base / "assume-unchanged-poison-ran"
        git(cached, "update-index", "--assume-unchanged", "ci-hub/bin/health-tick")
        self.poison_same_size(executable, marker)
        self.assertEqual("", git(cached, "status", "--porcelain=v1").stdout)

        self.assert_cached_tool_refused(marker)

    def test_skip_worktree_and_stat_cache_cannot_hide_poisoned_tool_bytes(self) -> None:
        _target, cached, _tick = self.prime_cached_tool_root()
        self.make_cache_writable(cached)
        executable = cached / "ci-hub" / "bin" / "health-tick"
        marker = self.base / "skip-worktree-poison-ran"
        git(cached, "status", "--porcelain=v1")
        git(cached, "update-index", "--skip-worktree", "ci-hub/bin/health-tick")
        self.poison_same_size(executable, marker)
        self.assertEqual("", git(cached, "status", "--porcelain=v1").stdout)

        self.assert_cached_tool_refused(marker)

    def test_nested_dirt_and_ignore_config_cannot_hide_agent_utils_poison(self) -> None:
        _target, cached, _tick = self.prime_cached_tool_root()
        self.make_cache_writable(cached)
        nested = cached / "hermit" / "agent-utils"
        (nested / "bin" / "tick-hub").write_text("poisoned agent-utils\n")
        git(cached, "config", "submodule.hermit.ignore", "all")
        git(cached / "hermit", "config", "submodule.agent-utils.ignore", "all")
        self.assertEqual("", git(cached, "status", "--porcelain=v1").stdout)
        marker = self.base / "nested-poison-ran"

        self.assert_cached_tool_refused(marker)

    def test_wrong_nested_head_is_refused_even_when_parent_status_is_hidden(self) -> None:
        _target, cached, _tick = self.prime_cached_tool_root()
        self.make_cache_writable(cached)
        nested = cached / "hermit" / "agent-utils"
        git(nested, "config", "user.name", "Cache Attacker")
        git(nested, "config", "user.email", "cache@example.invalid")
        git(nested, "commit", "--allow-empty", "-m", "wrong nested head")
        git(cached, "config", "submodule.hermit.ignore", "all")
        git(cached / "hermit", "config", "submodule.agent-utils.ignore", "all")
        self.assertEqual("", git(cached, "status", "--porcelain=v1").stdout)
        marker = self.base / "wrong-nested-head-ran"

        self.assert_cached_tool_refused(marker)

    def test_missing_parent_hermit_gitlink_is_refused_loudly(self) -> None:
        publisher = self.base / "missing-hermit-publisher"
        git(self.base, "clone", "-q", str(self.remote), str(publisher))
        git(publisher, "config", "user.name", "Other")
        git(publisher, "config", "user.email", "other@example.invalid")
        git(publisher, "rm", "-f", "agent-utils", "hermit", ".gitmodules")
        git(publisher, "commit", "-m", "remove required Hermit gitlink")
        git(publisher, "push", "-q", "origin", "HEAD:main")
        target = git(publisher, "rev-parse", "HEAD").stdout.strip()
        tick = self.repo / "ci-hub" / "bin" / "health-tick"
        tick.write_text(tick.read_text() + "# local work\n")

        result = self.run_tick()

        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn(
            f"parent commit {target} has no required mode-160000 Hermit gitlink",
            result.stderr,
        )
        staging = list(
            (Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees").glob(
                ".staging.*"
            )
        )
        self.assertEqual(1, len(staging))
        self.assertEqual([], list(staging[0].iterdir()))

    def test_concurrent_cache_replacement_cannot_change_private_run(self) -> None:
        started = self.repo / ".tick-hub" / "private-started"
        proceed = self.repo / ".tick-hub" / "private-continue"
        poison_ran = self.repo / ".tick-hub" / "cache-poison-ran"
        health_script = (
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "mkdir -p \"$DEV_HERMIT_PARENT/.tick-hub\"\n"
            ": > \"$DEV_HERMIT_PARENT/.tick-hub/private-started\"\n"
            "while [[ ! -e \"$DEV_HERMIT_PARENT/.tick-hub/private-continue\" ]]; do sleep 0.02; done\n"
            "\"$DEV_HERMIT_TOOL_ROOT/ci-hub/bin/private-payload\"\n"
        )
        payload = (
            "#!/usr/bin/env bash\n"
            "printf 'NOTE: private-payload=safe tool_root=%s\\n' \"$DEV_HERMIT_TOOL_ROOT\"\n"
        )
        target = self.advance_remote(
            health_script=health_script,
            extra_executables={"ci-hub/bin/private-payload": payload},
        )
        local_tick = self.repo / "ci-hub" / "bin" / "health-tick"
        local_tick.write_text(local_tick.read_text() + "# local work\n")
        process = subprocess.Popen(
            [str(self.repo / "scripts" / "hermit-health-tick.sh"), "--emit-output"],
            cwd=self.repo,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 15
            while not started.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("private tool run did not reach the concurrency barrier")
                time.sleep(0.02)
            self.assertIsNone(process.poll())
            cached = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees" / target
            self.make_cache_writable(cached)
            cached_payload = cached / "ci-hub" / "bin" / "private-payload"
            cached_payload.write_text(
                "#!/usr/bin/env bash\n"
                ": > \"$DEV_HERMIT_PARENT/.tick-hub/cache-poison-ran\"\n"
            )
            cached_payload.chmod(0o755)
            proceed.touch()
            stdout, stderr = process.communicate(timeout=15)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertEqual(0, process.returncode, stdout + stderr)
        self.assertIn("private-payload=safe", stdout)
        self.assertFalse(poison_ran.exists())
        self.assert_private_run_pool_is_reusable()

    def test_process_group_term_keeps_tool_root_until_payload_teardown(self) -> None:
        started = self.repo / ".tick-hub" / "term-payload-started"
        helper_ran = self.repo / ".tick-hub" / "term-helper-ran"
        payload = (
            "#!/usr/bin/env bash\n"
            "set -u\n"
            "on_term() {\n"
            "  \"$DEV_HERMIT_TOOL_ROOT/scripts/term-helper\"\n"
            "  exit 143\n"
            "}\n"
            "trap on_term TERM INT HUP\n"
            "mkdir -p \"$DEV_HERMIT_PARENT/.tick-hub\"\n"
            ": > \"$DEV_HERMIT_PARENT/.tick-hub/term-payload-started\"\n"
            "while :; do sleep 0.05; done\n"
        )
        helper = (
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "git -C \"$DEV_HERMIT_TOOL_ROOT\" rev-parse HEAD > "
            "\"$DEV_HERMIT_PARENT/.tick-hub/term-helper-ran\"\n"
        )
        target = self.advance_remote(
            extra_executables={
                "scripts/term-payload": payload,
                "scripts/term-helper": helper,
            }
        )
        bootstrap = self.installed_bootstrap()
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.repo),
                "--tool-relative",
                "scripts/term-payload",
            ],
            cwd=self.home,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 15
            while not started.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("payload did not reach the termination barrier")
                time.sleep(0.01)
            self.assertIsNone(process.poll())
            os.killpg(process.pid, signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=15)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()

        self.assertEqual(143, process.returncode, stdout + stderr)
        self.assertEqual(target, helper_ran.read_text().strip())
        try:
            self.assert_private_run_pool_is_reusable()
        except AssertionError as error:
            self.fail(f"{error}\nstdout:\n{stdout}\nstderr:\n{stderr}")
        run_root = next(
            Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]).glob(".run.*")
        )
        first_identity = (run_root.stat().st_dev, run_root.stat().st_ino)

        repeated = self.run_current_tool(
            bootstrap, "scripts/term-helper"
        )
        self.assertEqual(0, repeated.returncode, repeated.stdout + repeated.stderr)
        self.assert_private_run_pool_is_reusable()
        reused = next(
            Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]).glob(".run.*")
        )
        self.assertEqual(first_identity, (reused.stat().st_dev, reused.stat().st_ino))

    def test_orc_direct_pid_term_reaches_measured_tick_and_reaps_it(self) -> None:
        ready = self.repo / ".tick-hub/orc-cancel-ready"
        tick_term = self.repo / ".tick-hub/orc-tick-term"
        delayed = self.repo / ".tick-hub/orc-delayed-fired-state"
        taskgraph_delivered = self.repo / ".tick-hub/orc-taskgraph-delivered"
        github_started = self.repo / ".tick-hub/orc-github-started"
        github_finished = self.repo / ".tick-hub/orc-github-finished"
        invocation_log = self.home / ".local/state/hermit-health-tick-invocations.log"
        systemd_run = self.bin / "systemd-run"
        systemd_run.write_text(
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "while (( $# )); do\n"
            "  if [[ $1 == -- ]]; then shift; break; fi\n"
            "  shift\n"
            "done\n"
            "python3 - \"$@\" <<'PY'\n"
            "import subprocess, sys\n"
            "subprocess.Popen(sys.argv[1:], stdin=subprocess.DEVNULL, "
            "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, "
            "start_new_session=True, close_fds=True)\n"
            "PY\n"
        )
        systemd_run.chmod(0o755)
        systemctl = self.bin / "systemctl"
        systemctl.write_text("#!/bin/sh\nexit 1\n")
        systemctl.chmod(0o755)
        payload = (
            "#!/usr/bin/env bash\n"
            "set -u\n"
            "state=$DEV_HERMIT_PARENT/.tick-hub\n"
            "mkdir -p \"$state\"\n"
            "on_term() { printf 'term\\n' > \"$state/orc-tick-term\"; exit 143; }\n"
            "trap on_term TERM INT HUP\n"
            "(\n"
            "  trap '' TERM INT HUP\n"
            "  printf '%s\\n' \"$BASHPID\" > \"$state/orc-descendant-pid\"\n"
            "  sleep 1\n"
            "  printf 'late\\n' > \"$state/orc-delayed-fired-state\"\n"
            ") &\n"
            "descendant=$!\n"
            "printf 'ACTION: fixture component=streamed title=preserve-me\\n'\n"
            "printf 'CLEAN: first gate completed\\n'\n"
            "printf 'SUPPRESSED: second gate disabled\\n'\n"
            "printf 'NO_RESULT: third gate unavailable\\n'\n"
            "printf '%s %s\\n' \"$$\" \"$descendant\" > \"$state/orc-cancel-ready\"\n"
            "wait \"$descendant\"\n"
        )
        escalation = (
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "state=$HOME/work/dev-hermit/.tick-hub\n"
            "while (( $# )); do\n"
            "  if [[ $1 == --sink ]]; then\n"
            "    sink=$2; shift 2\n"
            "    if [[ $sink == taskgraph ]]; then\n"
            "      : > \"$state/orc-taskgraph-delivered\"\n"
            "    elif [[ $sink == github ]]; then\n"
            "      : > \"$state/orc-github-started\"\n"
            "      sleep 2.2\n"
            "      : > \"$state/orc-github-finished\"\n"
            "    fi\n"
            "  else\n"
            "    shift\n"
            "  fi\n"
            "done\n"
        )
        self.advance_remote(
            health_script=payload,
            extra_executables={
                "ci-hub/bin/escalate": escalation,
                "ci-hub/bin/run-health-escalation": (
                    SOURCE / "ci-hub/bin/run-health-escalation"
                ).read_text(),
            },
        )
        command = (
            f'cd "{self.repo}" && '
            'HERMIT_AGENT_SNAPSHOT_JSON="$1" '
            "./scripts/hermit-health-tick.sh --emit-output"
        )
        process = subprocess.Popen(
            ["setsid", "sh", "-c", command, "hermitOperationalTick", "[]"],
            cwd=self.home,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        stdout = stderr = ""
        try:
            deadline = time.monotonic() + 15
            while not ready.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("exact ORC command did not reach the cancellation barrier")
                time.sleep(0.01)
            if process.poll() is not None:
                stdout, stderr = process.communicate()
                self.fail(
                    "exact ORC command exited before the cancellation barrier: "
                    f"rc={process.returncode}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                )
            command_line = Path(f"/proc/{process.pid}/cmdline").read_bytes()
            self.assertEqual(
                "bash",
                Path(f"/proc/{process.pid}/exe").resolve().name,
                "the outer sh -c did not tail-exec the Bash health-tick wrapper",
            )
            self.assertIn(
                b"scripts/hermit-health-tick.sh",
                command_line,
                "the outer sh -c did not tail-exec the health-tick wrapper",
            )
            tick_pid, descendant_pid = map(int, ready.read_text().split())
            for pid in (tick_pid, descendant_pid):
                fields = Path(f"/proc/{pid}/stat").read_text().split()
                self.assertNotEqual("Z", fields[2])
            self.assertEqual(process.pid, os.getpgid(process.pid))
            self.assertEqual(tick_pid, os.getpgid(tick_pid))
            self.assertEqual(tick_pid, os.getpgid(descendant_pid))
            self.assertNotEqual(process.pid, tick_pid)

            started = time.monotonic()
            process.send_signal(signal.SIGTERM)
            try:
                stdout, stderr = process.communicate(timeout=2)
                required_kill = False
            except subprocess.TimeoutExpired:
                required_kill = True
                process.kill()
                stdout, stderr = process.communicate(timeout=2)

            self.assertFalse(
                required_kill,
                "health-tick did not forward and reap before ORC's two-second KILL",
            )
            self.assertLess(time.monotonic() - started, 2)
            self.assertEqual(143, process.returncode, stdout + stderr)
            self.assertTrue(tick_term.is_file(), stdout + stderr)
            durable = invocation_log.read_text()
            for verdict in (
                "ACTION: fixture component=streamed title=preserve-me",
                "CLEAN: first gate completed",
                "SUPPRESSED: second gate disabled",
                "NO_RESULT: third gate unavailable",
            ):
                self.assertIn(f"finding={verdict}", durable)
            delivery_deadline = time.monotonic() + 2
            while not taskgraph_delivered.exists() and time.monotonic() < delivery_deadline:
                time.sleep(0.02)
            self.assertTrue(
                taskgraph_delivered.is_file(),
                "the detached publisher did not reach TaskGraph",
            )
            self.assertTrue(github_started.is_file())
            self.assertFalse(
                github_finished.exists(),
                "the wrapper waited for the slow GitHub sink instead of detaching it",
            )
            completion_deadline = time.monotonic() + 4
            pending = self.repo / "ignored/ci-hub/health-escalation-pending"
            while (
                not github_finished.exists() or list(pending.glob("event-*"))
            ) and time.monotonic() < completion_deadline:
                time.sleep(0.02)
            self.assertTrue(
                github_finished.is_file(),
                "the detached publisher did not outlive the cancelled wrapper",
            )
            self.assertEqual([], list(pending.glob("event-*")))
            time.sleep(1.05)
            self.assertFalse(delayed.exists(), "cancelled tick wrote delayed state")
            for pid in (tick_pid, descendant_pid):
                stat_path = Path(f"/proc/{pid}/stat")
                if stat_path.exists():
                    self.assertEqual("Z", stat_path.read_text().split()[2])
            self.assertEqual(
                [],
                list(Path("/tmp").glob(f"mega-lander-hermit-health-tick.{process.pid}.*")),
            )
            self.assertEqual(
                [], list(Path("/tmp").glob(f"hermit-health-tick.{process.pid}.*"))
            )
            self.assert_private_run_pool_is_reusable()
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

    def test_tick_hub_gate_verdict_reaches_caller_before_later_gate_finishes(
        self,
    ) -> None:
        second_running = self.repo / ".tick-hub/streaming-second-running"
        second_release = self.repo / ".tick-hub/streaming-second-release"
        config = json.dumps(
            {
                "reminders": [
                    {
                        "name": "streaming_first",
                        "cadence_secs": 0,
                        "gate": {
                            "cmd": "printf 'summary=first-finished\\n'; exit 1",
                            "when": "failure",
                            "capture": True,
                        },
                        "emit": {
                            "kind": "action",
                            "skill": "hard-warn-coordinator",
                            "title": "first gate reported: {summary}",
                        },
                    },
                    {
                        "name": "streaming_second",
                        "cadence_secs": 0,
                        "gate": {
                            "cmd": (
                                'mkdir -p "$DEV_HERMIT_PARENT/.tick-hub"; '
                                ': > "$DEV_HERMIT_PARENT/.tick-hub/'
                                'streaming-second-running"; '
                                'while [ ! -e "$DEV_HERMIT_PARENT/.tick-hub/'
                                'streaming-second-release" ]; do sleep 0.02; done'
                            ),
                            "when": "failure",
                            "capture": False,
                        },
                        "emit": {
                            "kind": "action",
                            "skill": "hard-warn-coordinator",
                            "title": "second gate failed",
                        },
                    },
                ]
            }
        )
        health_script = (
            "#!/usr/bin/env bash\n"
            "set -euo pipefail\n"
            'export PYTHONPATH="$DEV_HERMIT_TOOL_ROOT/agent-utils/py"\n'
            'exec python3 -m tick_hub tick --no-header --flush '
            '--report-pending '
            '--config "$DEV_HERMIT_TOOL_ROOT/ci-hub/health/streaming-test.json" '
            '--fired-state "$DEV_HERMIT_PARENT/.tick-hub/streaming-fired-state"\n'
        )
        self.advance_remote(
            health_script=health_script,
            extra_executables={"ci-hub/health/streaming-test.json": config},
        )
        process = subprocess.Popen(
            [str(self.repo / "scripts/hermit-health-tick.sh"), "--emit-output"],
            cwd=self.repo,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        prefix: list[str] = []
        stdout = stderr = ""
        try:
            deadline = time.monotonic() + 15
            while not second_running.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("real tick-hub did not reach its second gate")
                time.sleep(0.01)
            self.assertIsNone(process.poll())
            self.assertIsNotNone(process.stdout)
            stream_fd = process.stdout.fileno()
            os.set_blocking(stream_fd, False)
            stream_deadline = time.monotonic() + 1
            streamed = b""
            while time.monotonic() < stream_deadline:
                readable, _, _ = select.select([stream_fd], [], [], 0.05)
                if not readable:
                    continue
                try:
                    chunk = os.read(stream_fd, 65536)
                except BlockingIOError:
                    continue
                if not chunk:
                    break
                streamed += chunk
                if b"first gate reported: first-finished" in streamed:
                    break
            prefix.append(streamed.decode())
            self.assertIn(
                "first gate reported: first-finished",
                "".join(prefix),
                "the parent wrapper buffered a verdict that tick-hub had already emitted",
            )
            self.assertIsNone(process.poll(), "the later gate was not still running")
            second_release.touch()
            stdout, stderr = process.communicate(timeout=15)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        combined_stdout = "".join(prefix) + stdout
        self.assertEqual(0, process.returncode, combined_stdout + stderr)
        self.assertIn("first gate reported: first-finished", combined_stdout)
        self.assertIn(
            "CLEAN: streaming_second ran and found nothing to report",
            combined_stdout,
        )
        self.assertIn("emitted 1 instruction(s) this tick", combined_stdout)
        fired = (
            self.repo / ".tick-hub/streaming-fired-state"
        ).read_text().splitlines()
        self.assertTrue(any(line.startswith("streaming_first=") for line in fired))
        self.assertTrue(any(line.startswith("streaming_second=") for line in fired))
        self.assertEqual(
            [], list(Path("/tmp").glob(f"hermit-health-tick.{process.pid}.*"))
        )

    def test_private_run_replacement_cannot_execute_or_delete_poison(self) -> None:
        started = self.repo / ".tick-hub" / "held-run-started"
        proceed = self.repo / ".tick-hub" / "held-run-continue"
        poison_ran = self.repo / ".tick-hub" / "held-run-poison-ran"
        launcher = (
            "#!/usr/bin/env bash\n"
            "set -eu\n"
            "mkdir -p \"$DEV_HERMIT_PARENT/.tick-hub\"\n"
            ": > \"$DEV_HERMIT_PARENT/.tick-hub/held-run-started\"\n"
            "while [[ ! -e \"$DEV_HERMIT_PARENT/.tick-hub/held-run-continue\" ]]; do sleep 0.01; done\n"
            "\"$DEV_HERMIT_TOOL_ROOT/scripts/private-payload\"\n"
        )
        safe_payload = "#!/usr/bin/env bash\nprintf 'SAFE\\n'\n"
        target = self.advance_remote(
            extra_executables={
                "scripts/owner-probe": launcher,
                "scripts/private-payload": safe_payload,
            }
        )
        bootstrap = self.installed_bootstrap()
        process = subprocess.Popen(
            [
                str(bootstrap),
                "--run-current-tool",
                "--state-root",
                str(self.repo),
                "--tool-relative",
                "scripts/owner-probe",
            ],
            cwd=self.home,
            env=self.env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        cache = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"])
        try:
            deadline = time.monotonic() + 15
            while not started.exists() and process.poll() is None:
                if time.monotonic() >= deadline:
                    self.fail("private tool did not reach the replacement barrier")
                time.sleep(0.01)
            self.assertIsNone(process.poll())
            run_entries = list(cache.glob(".run.*"))
            self.assertEqual(1, len(run_entries))
            run_entry = run_entries[0]
            held_original = cache / f"{run_entry.name}.verified"
            run_entry.rename(held_original)
            shutil.copytree(cache / "trees" / target, run_entry, symlinks=True)
            self.make_cache_writable(run_entry)
            poison = run_entry / "scripts" / "private-payload"
            poison.write_text(
                "#!/usr/bin/env bash\n"
                ": > \"$DEV_HERMIT_PARENT/.tick-hub/held-run-poison-ran\"\n"
                "printf 'POISON\\n'\n"
            )
            poison.chmod(0o755)
            proceed.touch()
            stdout, stderr = process.communicate(timeout=15)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

        self.assertEqual(1, process.returncode, stdout + stderr)
        self.assertIn("SAFE", stdout)
        self.assertNotIn("POISON", stdout)
        self.assertFalse(poison_ran.exists())
        self.assertIn("private tool root entry changed; preserving it", stderr)
        self.assertTrue(run_entry.is_dir(), "replacement must not be deleted")
        self.assertTrue(held_original.is_dir(), "held original must not be misidentified")

    def test_private_run_replacement_after_final_identity_check_is_preserved(
        self,
    ) -> None:
        poison_ran = self.repo / ".tick-hub" / "cleanup-poison-ran"
        swap_count = self.base / "cleanup-identity-count"
        swap_done = self.base / "cleanup-swap-done"
        swap_name = self.base / "cleanup-swap-name"
        target = self.advance_remote(
            extra_executables={
                "scripts/owner-probe": "#!/usr/bin/env bash\nprintf 'SAFE\\n'\n"
            }
        )
        replacement_source = (
            Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / "trees" / target
        )
        bootstrap = self.installed_bootstrap()
        shim_dir = self.base / "cleanup-python-shim"
        shim_dir.mkdir()
        python_shim = shim_dir / "python3"
        python_shim.write_text(
            "#!/usr/bin/env bash\n"
            "set -u\n"
            f'"{REAL_PYTHON}" "$@"\n'
            "rc=$?\n"
            "if [[ $rc -eq 0 && $# -eq 4 && $1 == - && $3 == .run.* "
            "&& $4 == /proc/*/fd/* ]]; then\n"
            "  count=0\n"
            "  [[ ! -f $CLEANUP_SWAP_COUNT ]] || read -r count < \"$CLEANUP_SWAP_COUNT\"\n"
            "  count=$((count + 1))\n"
            "  printf '%s\\n' \"$count\" > \"$CLEANUP_SWAP_COUNT\"\n"
            "  if [[ $count -eq 2 ]]; then\n"
            "    run_name=$3\n"
            "    printf '%s\\n' \"$run_name\" > \"$CLEANUP_SWAP_NAME\"\n"
            "    mv -- \"$HERMIT_HEALTH_TOOL_CACHE/$run_name\" "
            "\"$HERMIT_HEALTH_TOOL_CACHE/$run_name.cleanup-held\"\n"
            "    cp -a -- \"$CLEANUP_REPLACEMENT_SOURCE\" "
            "\"$HERMIT_HEALTH_TOOL_CACHE/$run_name\"\n"
            "    chmod -R u+w -- \"$HERMIT_HEALTH_TOOL_CACHE/$run_name\"\n"
            "    cat > \"$HERMIT_HEALTH_TOOL_CACHE/$run_name/scripts/owner-probe\" <<'POISON'\n"
            "#!/usr/bin/env bash\n"
            "printf poison > \"$CLEANUP_POISON_MARKER\"\n"
            "printf 'POISON\\n'\n"
            "POISON\n"
            "    chmod +x \"$HERMIT_HEALTH_TOOL_CACHE/$run_name/scripts/owner-probe\"\n"
            "    : > \"$CLEANUP_SWAP_DONE\"\n"
            "  fi\n"
            "fi\n"
            "exit \"$rc\"\n"
        )
        python_shim.chmod(0o755)
        env = dict(
            self.env,
            PATH=f"{shim_dir}:{self.env['PATH']}",
            CLEANUP_SWAP_COUNT=str(swap_count),
            CLEANUP_SWAP_DONE=str(swap_done),
            CLEANUP_SWAP_NAME=str(swap_name),
            CLEANUP_REPLACEMENT_SOURCE=str(replacement_source),
            CLEANUP_POISON_MARKER=str(poison_ran),
        )

        result = run(
            str(bootstrap),
            "--run-current-tool",
            "--state-root",
            str(self.repo),
            "--tool-relative",
            "scripts/owner-probe",
            cwd=self.home,
            env=env,
        )

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("SAFE", result.stdout)
        self.assertNotIn("POISON", result.stdout)
        self.assertFalse(poison_ran.exists())
        self.assertTrue(swap_done.exists(), "final identity barrier did not fire")
        run_name = swap_name.read_text().strip()
        replacement = Path(self.env["HERMIT_HEALTH_TOOL_CACHE"]) / run_name
        held_original = replacement.with_name(f"{run_name}.cleanup-held")
        self.assertTrue(replacement.is_dir(), "same-name replacement was deleted")
        self.assertTrue(
            (replacement / "scripts" / "owner-probe").is_file(),
            "replacement contents were recursively deleted",
        )
        self.assertTrue(held_original.is_dir())
        self.assertEqual([], list(held_original.iterdir()))


if __name__ == "__main__":
    unittest.main()
