#!/usr/bin/env python3
"""Decision-table tests for the stranded-work sweep.

The task's own VERIFY clause is the spec: a closed task whose artifact is absent
from origin MUST be flagged, and the legitimately-landed population MUST NOT be
-- "a checker that flags everything is useless and gets disabled".  Both
directions are bracketed here against real throwaway git repos, not mocks.
"""
from __future__ import annotations

import io
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import stranded_sweep as S


def git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def make_repo(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    git(path, "init", "-q", "-b", "main")
    git(path, "config", "user.email", "t@example.com")
    git(path, "config", "user.name", "T")


class ArtifactClassificationTest(unittest.TestCase):
    """T3: where does a closed task's artifact actually live?"""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "parent"
        self.remote = Path(self.tmp.name) / "remote.git"
        make_repo(self.root)
        subprocess.run(["git", "init", "-q", "--bare", str(self.remote)], check=True)
        (self.root / "ai_docs").mkdir()
        (self.root / "ai_docs" / "landed.md").write_text("landed\n")
        git(self.root, "add", "ai_docs/landed.md")
        git(self.root, "commit", "-qm", "landed artifact")
        git(self.root, "remote", "add", "origin", str(self.remote))
        git(self.root, "push", "-q", "origin", "main")
        git(self.root, "fetch", "-q", "origin")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_landed_artifact_is_not_flagged(self) -> None:
        """The POSITIVE control: the legitimately-landed case must stay quiet."""
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/landed.md"), S.LANDED
        )

    def test_uncommitted_artifact_is_flagged(self) -> None:
        """THE INSTANCE THIS TOOL EXISTS FOR: a file written, a task closed, and
        the file never committed."""
        (self.root / "ai_docs" / "stranded.md").write_text("only on this box\n")
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/stranded.md"), S.UNCOMMITTED
        )

    def test_committed_but_unpushed_is_pending_not_landed(self) -> None:
        (self.root / "ai_docs" / "local.md").write_text("committed only\n")
        git(self.root, "add", "ai_docs/local.md")
        git(self.root, "commit", "-qm", "local only")
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/local.md"), S.PENDING_PUSH
        )

    def test_git_ignored_path_is_local_by_design_not_stranded(self) -> None:
        """Experiment hygiene writes bulky evidence under an ignored dir ON
        PURPOSE.  Calling that 'stranded' trains readers to ignore the tool."""
        (self.root / ".gitignore").write_text("ai_docs/ignored/\n")
        git(self.root, "add", ".gitignore")
        git(self.root, "commit", "-qm", "ignore rule")
        (self.root / "ai_docs" / "ignored").mkdir(parents=True, exist_ok=True)
        (self.root / "ai_docs" / "ignored" / "big.log").write_text("evidence\n")
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/ignored/big.log"), S.IGNORED_LOCAL
        )

    def test_renamed_after_landing_is_not_stranded(self) -> None:
        """Absent from the tip, but it DID reach the remote -- never stranded."""
        git(self.root, "rm", "-q", "ai_docs/landed.md")
        git(self.root, "commit", "-qm", "supersede the doc")
        self.assertEqual(S.classify_artifact(self.root, "ai_docs/landed.md"), S.LANDED)

    def test_absent_everywhere_is_missing(self) -> None:
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/never-existed.md"), S.MISSING
        )

    def test_stale_origin_downgrades_absence_to_UNVERIFIABLE(self) -> None:
        """A checker that shouts STRANDED off a stale ref gets disabled.

        With a stale origin/main an absence is not evidence -- the artifact may
        have landed after the last fetch.  PRESENCE stays definitive either way.
        """
        (self.root / "ai_docs" / "local.md").write_text("committed only\n")
        git(self.root, "add", "ai_docs/local.md")
        git(self.root, "commit", "-qm", "local only")
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/local.md", fresh=False),
            S.UNVERIFIABLE,
        )
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/never-existed.md", fresh=False),
            S.UNVERIFIABLE,
        )
        # Presence is still definitive on a stale ref.
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/landed.md", fresh=False), S.LANDED
        )

    def test_uncommitted_is_flagged_even_on_a_stale_ref(self) -> None:
        """An untracked working-tree file is stranded regardless of ref
        freshness -- no fetch can make a never-committed file exist upstream."""
        (self.root / "ai_docs" / "stranded.md").write_text("x\n")
        self.assertEqual(
            S.classify_artifact(self.root, "ai_docs/stranded.md", fresh=False),
            S.UNCOMMITTED,
        )


class PathExtractionTest(unittest.TestCase):
    """Over-flagging is the failure mode that gets a checker turned off."""

    def test_extracts_real_artifact_paths(self) -> None:
        got = S.extract_artifact_paths(
            "IMPLEMENTED: see `ai_docs/foo_20260805.md` and experiments/bar/README.md"
        )
        self.assertIn("ai_docs/foo_20260805.md", got)
        self.assertIn("experiments/bar/README.md", got)

    def test_ignores_prose_bare_dirs_and_urls(self) -> None:
        got = S.extract_artifact_paths(
            "see ci-hub/ for details, or https://github.com/x/ai_docs/y.md , or ai_docs/"
        )
        self.assertEqual(got, [])

    def test_ignores_unrelated_prefixes(self) -> None:
        self.assertEqual(S.extract_artifact_paths("src/main.rs /etc/passwd"), [])


class WorktreeScanTest(unittest.TestCase):
    """T1/T2 detection and the shared-object-store trap."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "parent"
        self.remote = Path(self.tmp.name) / "remote.git"
        make_repo(self.root)
        (self.root / "seed.txt").write_text("seed\n")
        git(self.root, "add", "seed.txt")
        git(self.root, "commit", "-qm", "seed")
        # A REAL remote: without one, `--not --remotes` excludes nothing and the
        # whole history reads as unpushed (see test_no_remote_refs_is_not_T2).
        subprocess.run(["git", "init", "-q", "--bare", str(self.remote)], check=True)
        git(self.root, "remote", "add", "origin", str(self.remote))
        git(self.root, "push", "-q", "origin", "main")
        git(self.root, "fetch", "-q", "origin")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_checkout_discovery_reads_slots_and_remaining_legacy_paths(self) -> None:
        current = self.root / "worktrees/slots/slot01/hermit"
        legacy = self.root / "worktrees/old-slot/hermit"
        direct_legacy = self.root / "worktrees/standalone-review"
        validate = self.root / "worktrees/validate/validate-fresh-test"
        for checkout in (current, legacy, direct_legacy, validate):
            make_repo(checkout)

        found = S.find_checkouts(self.root)

        self.assertIn(self.root, found)
        self.assertIn(current, found)
        self.assertIn(legacy, found)
        self.assertIn(direct_legacy, found)
        self.assertNotIn(validate, found)

    def test_no_remote_refs_is_not_T2(self) -> None:
        """A remoteless checkout must NOT report its whole history as stranded.

        `git log HEAD --not --remotes` with zero remote refs excludes nothing,
        so every commit looks unpushed.  That is a missing denominator, not a
        finding, and emitting thousands of false 'stranded' commits is what gets
        a checker switched off.
        """
        solo = Path(self.tmp.name) / "solo"
        make_repo(solo)
        (solo / "a.txt").write_text("a\n")
        git(solo, "add", "a.txt")
        git(solo, "commit", "-qm", "only commit")
        rep = S.scan_checkout(solo, solo)
        self.assertTrue(rep.no_remote_refs)
        self.assertEqual(rep.unpushed_commits, [])
        self.assertNotIn(S.T2_UNPUSHED, rep.tiers)
        self.assertTrue(rep.unavailable)

    def test_failed_git_reads_are_indeterminate_not_clean(self) -> None:
        with mock.patch.object(S, "_git", return_value=(128, "")):
            rep = S.scan_checkout(self.root, self.root)
        self.assertEqual(rep.tiers, [])
        self.assertIn("cannot read working-tree status", rep.unavailable)
        self.assertIn("cannot enumerate remote-tracking refs", rep.unavailable)

    def test_unstaged_modification_keeps_its_first_character(self) -> None:
        """REGRESSION: porcelain's first column is a SPACE for an unstaged
        modification, so stripping the output eats it and every path loses its
        leading character (` M AGENTS.md` -> `GENTS.md`).  Caught live against
        the real parent tree; invisible in a count, fatal to a rescue.
        """
        (self.root / "AGENTS.md").write_text("v1\n")
        git(self.root, "add", "AGENTS.md")
        git(self.root, "commit", "-qm", "add AGENTS.md")
        (self.root / "AGENTS.md").write_text("v2 modified\n")
        rep = S.scan_checkout(self.root, self.root)
        self.assertIn("AGENTS.md", rep.modified)
        self.assertNotIn("GENTS.md", rep.modified)
        # And the parsed path must actually resolve on disk, which is the
        # property rescue depends on.
        self.assertTrue((self.root / rep.modified[0]).exists())

    def test_untracked_file_is_T1(self) -> None:
        (self.root / "orphan.py").write_text("stranded\n")
        rep = S.scan_checkout(self.root, self.root)
        self.assertIn(S.T1_UNCOMMITTED, rep.tiers)
        self.assertIn("orphan.py", rep.untracked)

    def test_clean_checkout_is_not_flagged(self) -> None:
        """The negative control for the worktree half."""
        rep = S.scan_checkout(self.root, self.root)
        self.assertEqual(rep.tiers, [])

    def test_unpushed_commit_is_T2_and_HEAD_anchored(self) -> None:
        (self.root / "b.txt").write_text("b\n")
        git(self.root, "add", "b.txt")
        git(self.root, "commit", "-qm", "unpushed work")
        rep = S.scan_checkout(self.root, self.root)
        self.assertIn(S.T2_UNPUSHED, rep.tiers)
        # HEAD-anchored: exactly the ONE commit this history has beyond origin.
        self.assertEqual(len(rep.unpushed_commits), 1)
        # Now add unrelated local branches carrying their own unpushed commits.
        # A --branches-based count would absorb them and inflate this checkout's
        # figure -- the shared-object-store trap that produced a ~870x overcount
        # (1050/checkout, 54,706 total) against a true 63.
        git(self.root, "checkout", "-q", "-b", "unrelated-stale-branch-1")
        (self.root / "c.txt").write_text("c\n")
        git(self.root, "add", "c.txt")
        git(self.root, "commit", "-qm", "on another branch")
        git(self.root, "checkout", "-q", "main")
        rep2 = S.scan_checkout(self.root, self.root)
        self.assertEqual(len(rep2.unpushed_commits), 1,
                         "count must follow HEAD, not every local branch")


class RescueTest(unittest.TestCase):
    """Rescue must be additive. The source tree is someone else's property."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "parent"
        make_repo(self.root)
        self.remote = Path(self.tmp.name) / "remote.git"
        (self.root / "seed.txt").write_text("seed\n")
        git(self.root, "add", "seed.txt")
        git(self.root, "commit", "-qm", "seed")
        subprocess.run(["git", "init", "-q", "--bare", str(self.remote)], check=True)
        git(self.root, "remote", "add", "origin", str(self.remote))
        git(self.root, "push", "-q", "origin", "main")
        git(self.root, "fetch", "-q", "origin")
        (self.root / "stranded.py").write_text("valuable\n")
        # A wholly-untracked DIRECTORY -- git status collapses this to one entry
        # `sub/`, so a file-only rescue would silently drop deep.txt.
        (self.root / "sub").mkdir()
        (self.root / "sub" / "deep.txt").write_text("nested\n")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_rescue_copies_and_leaves_source_intact(self) -> None:
        q = Path(self.tmp.name) / "quarantine"
        man = S.rescue(self.root, ".", q)
        # Source still there, byte-identical -- rescue NEVER moves or cleans.
        self.assertTrue((self.root / "stranded.py").is_file())
        self.assertEqual((self.root / "stranded.py").read_text(), "valuable\n")
        self.assertTrue(man["source_left_intact"])
        dest = q / next(p.name for p in q.iterdir())
        self.assertEqual((dest / "files" / "stranded.py").read_text(), "valuable\n")
        self.assertTrue((dest / "MANIFEST.json").is_file())

    def test_rescue_preserves_nested_layout(self) -> None:
        q = Path(self.tmp.name) / "quarantine"
        S.rescue(self.root, ".", q)
        dest = q / next(p.name for p in q.iterdir())
        self.assertEqual((dest / "files" / "sub" / "deep.txt").read_text(), "nested\n")

    def test_rescue_bundles_unpushed_commits(self) -> None:
        git(self.root, "add", "stranded.py")
        git(self.root, "commit", "-qm", "now committed but unpushed")
        q = Path(self.tmp.name) / "quarantine"
        man = S.rescue(self.root, ".", q)
        self.assertEqual(man["bundle"], "unpushed.bundle")
        dest = q / next(p.name for p in q.iterdir())
        bundle = dest / "unpushed.bundle"
        self.assertTrue(bundle.is_file())
        # The bundle must be a REAL, verifiable object pack, not an empty file.
        p = subprocess.run(["git", "-C", str(self.root), "bundle", "verify", str(bundle)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.assertEqual(p.returncode, 0)

    def test_manifest_records_restore_instructions(self) -> None:
        q = Path(self.tmp.name) / "quarantine"
        man = S.rescue(self.root, ".", q)
        self.assertIn("copy back", man["restore"])


class SafetyContractTest(unittest.TestCase):
    """The tool must not contain a destructive git verb at all."""

    def test_no_destructive_git_verbs_in_source(self) -> None:
        src = (Path(__file__).resolve().parents[1] / "stranded_sweep.py").read_text()
        code = "\n".join(
            line for line in src.splitlines()
            if not line.lstrip().startswith("#")
        )
        _, _, body = code.partition('"""')          # drop the module docstring
        _, _, body = body.partition('"""')
        for forbidden in ('"clean"', '"reset"', '"stash"', '"checkout", "--"'):
            self.assertNotIn(forbidden, body,
                             f"destructive git verb {forbidden} must never appear")

    def test_every_git_read_uses_no_optional_locks(self) -> None:
        """A sweep over ~90 checkouts must not contend on a live agent's
        index.lock."""
        src = (Path(__file__).resolve().parents[1] / "stranded_sweep.py").read_text()
        self.assertIn('"--no-optional-locks"', src)


def run_as_selftest() -> int:
    suite = unittest.TestLoader().loadTestsFromModule(sys.modules[__name__])
    buf = io.StringIO()
    with redirect_stdout(buf):
        result = unittest.TextTestRunner(stream=buf, verbosity=2).run(suite)
    print(buf.getvalue())
    ok = result.wasSuccessful()
    print(f"stranded-sweep selftest: {'PASS' if ok else 'FAIL'} ({result.testsRun} tests)")
    return 0 if ok else 1


if __name__ == "__main__":
    unittest.main()
