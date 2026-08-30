#!/usr/bin/env python3
"""Tests for the commit-bound awaiting-land count.

The load-bearing assertion is the one the task names explicitly: a task whose commit IS
on main must be EXCLUDED even though it is still tagged `implemented`. The old tag-only
predicate counts it; this must not.

Ancestry is exercised against real throwaway git repos, not mocks, because the whole
point of the change is that the number binds to repository state.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import landing_composition as lc  # noqa: E402


def git(repo: Path, *args: str) -> str:
    p = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=False
    )
    if p.returncode:
        raise AssertionError(f"git {args}: {p.stderr}")
    return p.stdout.strip()


class ExtractShaTests(unittest.TestCase):
    def test_taskgraph_row_loss_is_indeterminate(self) -> None:
        completed = subprocess.CompletedProcess(
            ["tg"], 0, 'row_json\n--------\n{"id":"one"}\n\n(2 rows)\n', ""
        )
        with mock.patch.object(lc.taskgraph_db, "resolve"), \
             mock.patch.object(lc.taskgraph_db, "child_env", return_value={}), \
             mock.patch.object(lc.bounded_capture, "capture", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "reported 2 row"):
                lc.load_tagged_tasks(Path("/tmp"))

    def test_complete_taskgraph_rows_still_load(self) -> None:
        completed = subprocess.CompletedProcess(
            ["tg"], 0,
            'row_json\n--------\n'
            '{"id":"one","status":"IN_PROGRESS","evidence":"IMPLEMENTED: '
            + "a" * 40 + '"}\n\n(1 row)\n', ""
        )
        with mock.patch.object(lc.taskgraph_db, "resolve"), \
             mock.patch.object(lc.taskgraph_db, "child_env", return_value={}), \
             mock.patch.object(lc.bounded_capture, "capture", return_value=completed):
            rows = lc.load_tagged_tasks(Path("/tmp"))
        self.assertEqual(["one"], [row.id for row in rows])

    def test_first_40hex_is_taken(self) -> None:
        a, b = "a" * 40, "b" * 40
        self.assertEqual(lc.extract_sha(f"IMPLEMENTED: SHA {a} | later comment {b}"), a)

    def test_no_sha_yields_empty_not_a_guess(self) -> None:
        self.assertEqual(lc.extract_sha("IMPLEMENTED: see the PR"), "")
        self.assertEqual(lc.extract_sha(""), "")

    def test_a_short_sha_is_not_accepted(self) -> None:
        """A 12-hex abbreviation cannot be membership-tested against the index."""
        self.assertEqual(lc.extract_sha("landed at abc123def456"), "")


class ClassificationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.repo = self.root / "r"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "t@t")
        git(self.repo, "config", "user.name", "t")
        (self.repo / "f").write_text("1\n")
        git(self.repo, "add", "f")
        git(self.repo, "commit", "-qm", "landed work")
        self.landed = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "checkout", "-q", "-b", "side")
        (self.repo / "g").write_text("2\n")
        git(self.repo, "add", "g")
        git(self.repo, "commit", "-qm", "unlanded work")
        self.unlanded = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "checkout", "-q", "main")
        self.index = lc.AncestorIndex(self.root, targets=(("r", "main"),))

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_index_brackets_both_directions(self) -> None:
        self.assertTrue(self.index.contains(self.landed))
        self.assertFalse(self.index.contains(self.unlanded))

    def test_LANDED_but_still_tagged_is_EXCLUDED(self) -> None:
        """THE REQUIREMENT. The old count includes this row; the new one must not."""
        t = lc.TaggedTask("done", "CLOSED", self.landed)
        comp = lc.classify([t], self.index)
        self.assertEqual(len(comp.awaiting), 0, comp.render())
        self.assertEqual(len(comp.landed_still_tagged), 1)
        self.assertIn("EXCLUDED", comp.render())

    def test_genuinely_unlanded_IS_counted(self) -> None:
        """The positive half: a guard that excludes everything is useless."""
        t = lc.TaggedTask("owed", "CLOSED", self.unlanded)
        comp = lc.classify([t], self.index)
        self.assertEqual(len(comp.awaiting), 1, comp.render())
        self.assertEqual(len(comp.landed_still_tagged), 0)

    def test_CLOSED_alone_does_not_decide_it(self) -> None:
        """Both rows are CLOSED; only the commit separates them.

        This is the test that would fail if someone 'fixed' the count by dropping CLOSED
        rows -- that shortcut deletes the genuinely-unlanded CLOSED row too.
        """
        comp = lc.classify(
            [
                lc.TaggedTask("closed-and-landed", "CLOSED", self.landed),
                lc.TaggedTask("closed-but-owed", "CLOSED", self.unlanded),
            ],
            self.index,
        )
        self.assertEqual([t.id for t in comp.awaiting], ["closed-but-owed"])
        self.assertEqual(
            [t.id for t in comp.landed_still_tagged], ["closed-and-landed"]
        )

    def test_missing_sha_is_INDETERMINATE_not_awaiting(self) -> None:
        """No SHA is an absence. Counting it as awaiting would manufacture debt."""
        comp = lc.classify([lc.TaggedTask("no-evidence", "CLOSED", "")], self.index)
        self.assertEqual(len(comp.indeterminate), 1)
        self.assertEqual(len(comp.awaiting), 0)
        self.assertEqual(len(comp.landed_still_tagged), 0)

    def test_count_is_never_published_without_composition(self) -> None:
        comp = lc.classify(
            [
                lc.TaggedTask("a", "CLOSED", self.landed),
                lc.TaggedTask("b", "CLOSED", self.unlanded),
                lc.TaggedTask("c", "CLOSED", ""),
            ],
            self.index,
        )
        d = comp.to_dict()
        self.assertEqual(d["awaiting_land"], 1)
        self.assertEqual(d["total_tagged_implemented"], 3)
        self.assertEqual(
            d["composition"],
            {"awaiting": 1, "landed_still_tagged": 1, "indeterminate": 1},
        )
        text = comp.render()
        for token in ("awaiting", "landed_still_tagged", "indeterminate"):
            self.assertIn(token, text)

    def test_an_UNINDEXED_repo_produces_a_false_awaiting(self) -> None:
        """Documents the failure mode that bit this module during development.

        A SHA from a repository the index does not cover is indistinguishable from an
        unlanded one, so it lands in `awaiting`. Two of twelve sampled live rows hit this
        via `agent-utils`. The mitigation is the explicit DEFAULT_TARGETS list, and this
        test pins the behaviour so the hazard stays visible rather than being rediscovered.
        """
        other = self.root / "other"
        other.mkdir()
        git(other, "init", "-q", "-b", "main")
        git(other, "config", "user.email", "o@o")
        git(other, "config", "user.name", "o")
        (other / "x").write_text("x\n")
        git(other, "add", "x")
        git(other, "commit", "-qm", "landed elsewhere")
        elsewhere = git(other, "rev-parse", "HEAD")

        narrow = lc.classify([lc.TaggedTask("t", "CLOSED", elsewhere)], self.index)
        self.assertEqual(len(narrow.awaiting), 1, "unindexed reads as awaiting")

        wide = lc.classify(
            [lc.TaggedTask("t", "CLOSED", elsewhere)],
            lc.AncestorIndex(self.root, targets=(("r", "main"), ("other", "main"))),
        )
        self.assertEqual(len(wide.awaiting), 0, "indexing the repo resolves it")
        self.assertEqual(len(wide.landed_still_tagged), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
