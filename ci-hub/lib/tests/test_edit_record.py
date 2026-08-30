#!/usr/bin/env python3
"""The edit-claim record: what it establishes, and what it refuses to guess.

The cases that matter are not "a claim round-trips". They are the three the
without-this-mechanism state cannot distinguish: an unclaimed change, a claim
that explains the change, and a claim that does NOT explain it. The last is the
one a name-only claim would report as cleanly owned.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

LIB = Path(__file__).resolve().parent.parent


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, LIB / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


er = _load("edit_record")


def git(root: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=str(root), check=True, capture_output=True)


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="edit-record-")
        self.root = Path(self.tmp.name)
        git(self.root, "init", "-q", ".")
        git(self.root, "config", "user.email", "t@t")
        git(self.root, "config", "user.name", "t")
        (self.root / "a.txt").write_text("v1\n")
        (self.root / "b.txt").write_text("v1\n")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-qm", "base")

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def claim(self, rel: str, intent: str = "work", agent: str = "hermit-005"):
        return er.record_claim(self.root, [rel], agent, "hermit2", "impl", intent)

    def state_of(self, rel: str) -> str:
        for row in er.attribute(self.root):
            if row.path == rel:
                return row.state
        return "CLEAN"


class WhatItRecords(Base):
    def test_an_unclaimed_dirty_path_is_unattributed(self):
        """The state the whole thing exists to make visible."""
        (self.root / "a.txt").write_text("changed by nobody\n")
        self.assertEqual(self.state_of("a.txt"), "UNATTRIBUTED")

    def test_a_claim_made_before_the_edit_attributes_it(self):
        self.claim("a.txt", "widen the retry bound")
        (self.root / "a.txt").write_text("changed\n")
        self.assertEqual(self.state_of("a.txt"), "CLAIMED")
        row = [r for r in er.attribute(self.root) if r.path == "a.txt"][0]
        self.assertEqual(row.claimants[-1].agent, "hermit-005")
        self.assertEqual(row.claimants[-1].intent, "widen the retry bound")

    def test_the_claim_records_the_content_it_started_from(self):
        """RECORD WHAT YOU MEASURED AGAINST. Without this the claim is a name."""
        before = er.blob_id(self.root, "a.txt")
        claims = self.claim("a.txt")
        self.assertEqual(claims[0].blob_before, before)
        (self.root / "a.txt").write_text("changed\n")
        self.assertNotEqual(er.blob_id(self.root, "a.txt"), before)

    def test_a_claim_on_an_absent_path_records_absence_not_a_fake_hash(self):
        claims = er.record_claim(
            self.root, ["new.txt"], "hermit-005", "hermit2", "impl", "create it"
        )
        self.assertIsNone(claims[0].blob_before)


class WhatItRefusesToGuess(Base):
    def test_failed_git_diff_is_unavailable_not_clean(self):
        failed = subprocess.CompletedProcess(["git"], 128, "", "not a repository")
        with mock.patch.object(er.bounded_capture, "capture", return_value=failed):
            with self.assertRaisesRegex(RuntimeError, "cannot enumerate tracked edits"):
                er.dirty_paths(self.root)

    def test_a_claim_that_does_not_explain_the_change_is_named_as_such(self):
        """⚠️ THE CASE A NAME-ONLY CLAIM GETS WRONG.

        Someone edits without claiming; a different agent then claims the path
        and changes nothing. The file is dirty, a claimant is on record, and the
        claimant is not responsible for the content. A claim that stored only a
        name would report this as cleanly owned.
        """
        (self.root / "a.txt").write_text("edited by nobody\n")
        self.claim("a.txt", "intended to refactor")  # no edit follows
        self.assertEqual(self.state_of("a.txt"), "CLAIMED-BUT-UNCHANGED")

    def test_it_never_invents_a_claimant_for_an_unclaimed_path(self):
        """Attribution is not inference. Discussing a file is not claiming it,
        and mtime proximity is not authorship -- both already failed."""
        (self.root / "a.txt").write_text("x\n")
        self.claim("b.txt", "unrelated work")
        (self.root / "b.txt").write_text("y\n")
        rows = {r.path: r for r in er.attribute(self.root)}
        self.assertEqual(rows["a.txt"].claimants, [])
        self.assertEqual(rows["a.txt"].state, "UNATTRIBUTED")

    def test_a_clean_path_is_not_reported_even_when_claimed(self):
        """A claim is about a working tree, not a permanent assignment."""
        self.claim("a.txt")
        self.assertEqual(self.state_of("a.txt"), "CLEAN")


class NotALock(Base):
    def test_two_agents_may_claim_one_path_and_both_are_named(self):
        """Concurrent editing is allowed. The problem is unrecorded authorship,
        not simultaneous work; serialising the fleet would trade an attribution
        gap for a throughput collapse."""
        self.claim("a.txt", "first pass", agent="hermit-005")
        (self.root / "a.txt").write_text("v2\n")
        self.claim("a.txt", "second pass", agent="hermit-013")
        (self.root / "a.txt").write_text("v3\n")
        row = [r for r in er.attribute(self.root) if r.path == "a.txt"][0]
        self.assertEqual([c.agent for c in row.claimants], ["hermit-005", "hermit-013"])
        self.assertEqual(row.state, "CLAIMED")

    def test_claiming_never_fails_because_someone_else_claimed(self):
        self.claim("a.txt", agent="hermit-005")
        self.claim("a.txt", agent="hermit-013")  # must not raise
        self.assertEqual(len(er.read_claims(self.root)), 2)

    def test_a_superseded_claim_is_kept_not_overwritten(self):
        """The sequence of claimants IS the handover record."""
        self.claim("a.txt", "first", agent="hermit-005")
        self.claim("a.txt", "second", agent="hermit-013")
        intents = [c.intent for c in er.read_claims(self.root)]
        self.assertEqual(intents, ["first", "second"])


class JournalHygiene(Base):
    def test_malformed_lines_are_skipped_and_counted_not_hidden(self):
        """A silently smaller denominator reads as better coverage than measured."""
        self.claim("a.txt")
        with er.journal_path(self.root).open("a", encoding="utf-8") as fh:
            fh.write("not json at all\n")
            fh.write('{"schema":"something-else","path":"a.txt","agent":"x"}\n')
        self.assertEqual(len(er.read_claims(self.root)), 1)
        self.assertEqual(er.malformed_count(self.root), 2)

    def test_a_missing_journal_is_empty_not_an_error(self):
        self.assertEqual(er.read_claims(self.root), [])
        self.assertEqual(er.malformed_count(self.root), 0)

    def test_the_journal_is_not_tracked_by_git(self):
        """If it were tracked it would itself be an uncommitted change with no
        recorded owner -- the exact defect it closes."""
        self.claim("a.txt")
        out = subprocess.run(
            ["git", "status", "--porcelain", "--untracked-files=all"],
            cwd=str(self.root), capture_output=True, text=True,
        ).stdout
        line = [l for l in out.splitlines() if er.JOURNAL_NAME in l]
        self.assertTrue(line, "journal should appear in status")
        self.assertTrue(line[0].startswith("??"),
                        f"journal must be untracked, got {line[0]!r}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
