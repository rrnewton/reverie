#!/usr/bin/env python3
"""Both-direction integration tests for the parent-main writer authority."""

from __future__ import annotations

import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
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

    # ---- the superseded-copy asymmetry, both modes -------------------------

    def stale_published_copy(self) -> tuple[str, str]:
        """Leave on disk a copy that is published, but not published LAST."""
        first = self.publish_from_a_second_clone({"drift.txt": "V1\n"}, "publish V1")
        git(self.repo, "-c", "core.hooksPath=/dev/null", "merge", "-q", "--ff-only", "origin/main")
        target = self.publish_from_a_second_clone({"drift.txt": "V2\n"}, "publish V2")
        # local main falls back behind V1 so drift.txt is in the HEAD..target diff
        git(self.repo, "-c", "core.hooksPath=/dev/null", "reset", "--hard", self.seed)
        (self.repo / "drift.txt").write_text("V1\n")
        return first, target

    def test_negative_sync_refuses_a_superseded_copy_and_names_its_commit(self) -> None:
        first, _ = self.stale_published_copy()
        result = self.writer("sync")
        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("drift.txt", combined)
        self.assertIn("SUPERSEDED", combined)
        self.assertIn(first[:12], combined)
        self.assertEqual("V1\n", (self.repo / "drift.txt").read_text())

    def test_positive_reconcile_refreshes_a_superseded_copy_and_names_recovery(self) -> None:
        first, target = self.stale_published_copy()
        result = self.writer("reconcile")
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        combined = result.stdout + result.stderr
        self.assertIn("refreshed superseded copy drift.txt", combined)
        self.assertIn(f"git show {first}:drift.txt", combined)
        self.assertIn("refreshed=1", result.stdout)
        self.assertEqual("V2\n", (self.repo / "drift.txt").read_text())
        self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())

    def test_negative_reconcile_still_refuses_a_copy_published_nowhere(self) -> None:
        """The refresh is authorised by a NAMED commit, not by "looks stale"."""
        self.stale_published_copy()
        (self.repo / "drift.txt").write_text("NEVER PUBLISHED ANYWHERE\n")
        result = self.writer("reconcile")
        self.assertNotEqual(0, result.returncode)
        combined = result.stdout + result.stderr
        self.assertIn("drift.txt", combined)
        self.assertIn("differ from BOTH your HEAD", combined)
        self.assertNotIn("SUPERSEDED", combined)
        self.assertNotIn("refreshed superseded copy", combined)
        self.assertEqual("NEVER PUBLISHED ANYWHERE\n", (self.repo / "drift.txt").read_text())

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

    def writer(self, *args: str) -> subprocess.CompletedProcess[str]:
        return ParentMainWriteTests.writer(self, *args)

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
sys.path.insert(0, str(root / "ci-hub/ledger"))
import validate_rows
ids_path.parent.mkdir(parents=True, exist_ok=True)
for index in range(300):
    event_id = f"live-sync-{index:04d}"
    row = {
        "schema_version": 5,
        "record_id": event_id,
        "repo": "hermit",
        "producer": "parent-main-write-integration",
        "host": "devbig999",
        "started_at": "2026-08-09T12:00:00Z",
        "finished_at": "2026-08-09T12:01:00Z",
        "result": "pass",
        "commit": "a" * 40,
        "cwd": str(root / "worktrees/slot99/hermit"),
    }
    validate_rows.record(row, root=root)
    with ids_path.open("a") as stream:
        stream.write(json.dumps({"event_id": event_id}) + "\n")
    time.sleep(0.02)
'''
        process = subprocess.Popen(
            [sys.executable, "-c", script, str(self.repo), str(ids_path)],
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
                time.sleep(0.02)
            before = len(ids_path.read_text().splitlines())
            self.assertGreaterEqual(before, 5)
            self.assertIsNone(process.poll(), "writer must be active before sync")

            result = self.writer("sync")

            self.assertEqual(0, result.returncode, result.stdout + result.stderr)
            self.assertEqual(target, git(self.repo, "rev-parse", "HEAD").stdout.strip())
            self.assertEqual(target, git(self.repo, "rev-parse", "origin/main").stdout.strip())
            self.assertIsNone(process.poll(), "writer must remain active after sync")
            stdout, stderr = process.communicate(timeout=15)
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
                "import validate_rows; "
                "print(json.dumps([e['event_id'] for e in validate_rows.events(root=root)]))"
            ),
            str(self.repo),
            cwd=self.repo,
        )
        self.assertEqual(0, union.returncode, union.stdout + union.stderr)
        union_ids = json.loads(union.stdout)
        self.assertEqual(set(written_ids), set(union_ids))
        self.assertEqual(len(written_ids), len(union_ids), "event-id union must lose nothing")
        self.assertEqual("", git(self.repo, "status", "--short", "--untracked-files=no").stdout)

if __name__ == "__main__":
    unittest.main()
