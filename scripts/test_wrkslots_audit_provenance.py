#!/usr/bin/env python3
"""Whose wrkslots audit receipt the health tick is willing to vouch for.

THE DEFECT THIS GUARDS. The reader required the receipt's producer shas to EQUAL
its own. But the producer is ``hermit-wrkslots-audit.service`` on an hourly timer
and the reader is the health tick every thirty minutes, and each materialises its
own tool root from origin/main at its own moment -- so the two are equal only
while origin/main has not moved in between. Measured 2026-09-05: six distinct
tool-root revisions inside three hours, and the audit returned NO_RESULT with
"producer provenance mismatch" on every cycle where a landing fell between the
two runs. Slot health had no signal for hours.

⚠️ THE REFUSAL WAS THE RIGHT DIRECTION AND IS NOT SOFTENED HERE. An audit that
vouches for an unverifiable producer is worse than one that says nothing, because
its numbers would then read as measured. What changed is the property: from "this
receipt came from the identical revision I am running" to "this receipt came from
a real landed revision of this repository, whose recorded submodule pins are the
ones it claims". The second is strictly checkable and does not depend on two
independently scheduled units happening to share a moment.

These cases build a real throwaway repository with real commits and real
submodule gitlinks, because the ancestry and gitlink lookups are the substance --
a fixture of invented shas could only ever exercise the refusal paths. The
in-tree suite at ci-hub/health/tests/test_wrkslots_audit_result.py covers the
receipt's shape and freshness with invented shas and deliberately keeps those as
refusals.

WHY THIS FILE IS IN ``scripts/``. The Makefile's ``unittest discover -s scripts``
is the only scheduled Python discovery, and it reaches neither
``ci-hub/health/tests/`` nor ``scripts/tests/``. Measured 2026-09-05: the ci-hub
health tests are referenced only from ai_docs write-ups, so they run by hand.
"""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
READER = ROOT / "ci-hub/health/wrkslots_audit_result.py"


def _reader():
    sys.path.insert(0, str(ROOT / "ci-hub/health"))
    spec = importlib.util.spec_from_file_location("wrkslots_audit_provenance", READER)
    module = importlib.util.module_from_spec(spec)
    sys.modules["wrkslots_audit_provenance"] = module
    spec.loader.exec_module(module)
    return module


reader = _reader()


def git(repo: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, text=True, check=True,
    )
    return done.stdout.strip()


@unittest.skipIf(subprocess.run(["git", "--version"], capture_output=True).returncode,
                 "git is not available")
class ProvenanceAgainstARealRepository(unittest.TestCase):
    """A parent repository with a hermit submodule that itself pins agent-utils."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="wrkslots-provenance-"))
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.tmp)]))

        def new_repo(path: Path) -> None:
            path.mkdir(parents=True)
            git(path, "init", "-q", "-b", "main")
            git(path, "config", "user.email", "t@example.invalid")
            git(path, "config", "user.name", "t")
            git(path, "config", "commit.gpgsign", "false")

        self.agent_utils = self.tmp / "agent-utils"
        new_repo(self.agent_utils)
        (self.agent_utils / "a").write_text("1\n")
        git(self.agent_utils, "add", "a")
        git(self.agent_utils, "commit", "-qm", "agent-utils one")
        self.agent_utils_sha = git(self.agent_utils, "rev-parse", "HEAD")

        self.hermit = self.tmp / "hermit-src"
        new_repo(self.hermit)
        (self.hermit / "h").write_text("1\n")
        git(self.hermit, "add", "h")
        git(self.hermit, "-c", "protocol.file.allow=always", "submodule", "--quiet",
            "add", str(self.agent_utils), "agent-utils")
        git(self.hermit, "commit", "-qm", "hermit one")
        self.hermit_sha = git(self.hermit, "rev-parse", "HEAD")

        self.parent = self.tmp / "parent"
        new_repo(self.parent)
        (self.parent / "p").write_text("1\n")
        git(self.parent, "add", "p")
        git(self.parent, "-c", "protocol.file.allow=always", "submodule", "--quiet",
            "add", str(self.hermit), "hermit")
        git(self.parent, "commit", "-qm", "parent one")
        self.old_parent = git(self.parent, "rev-parse", "HEAD")

        (self.parent / "p").write_text("2\n")
        git(self.parent, "commit", "-qam", "parent two")
        (self.parent / "p").write_text("3\n")
        git(self.parent, "commit", "-qam", "parent three")
        self.new_parent = git(self.parent, "rev-parse", "HEAD")

        # The reader resolves the agent-utils gitlink inside <state_root>/hermit.
        git(self.parent, "-c", "protocol.file.allow=always",
            "submodule", "update", "--quiet", "--init", "--recursive")

    def objection(self, producer: dict[str, str], expected: dict[str, str]) -> str | None:
        return reader.provenance_objection(producer, expected, self.parent)

    def producer_at(self, parent_sha: str) -> dict[str, str]:
        return {
            "parent_sha": parent_sha,
            "hermit_sha": self.hermit_sha,
            "agent_utils_sha": self.agent_utils_sha,
        }

    # ---- the direction that had gone dark --------------------------------

    def test_a_producer_two_landings_older_than_the_reader_is_accepted(self) -> None:
        """THE REGRESSION. This is every cycle where a landing fell in between."""
        self.assertIsNone(
            self.objection(
                self.producer_at(self.old_parent), self.producer_at(self.new_parent)
            )
        )

    def test_an_identical_producer_is_accepted_without_touching_git(self) -> None:
        """The quiet-period case, and the fast path."""
        same = self.producer_at(self.new_parent)
        self.assertIsNone(reader.provenance_objection(same, same, Path("/nonexistent")))

    # ---- the directions that must still refuse ---------------------------

    def test_a_producer_commit_this_repository_does_not_have_is_refused(self) -> None:
        objection = self.objection(self.producer_at("0" * 40), self.producer_at(self.new_parent))
        self.assertIsNotNone(objection)
        self.assertIn("not a commit in this repository", objection)

    def test_a_producer_newer_than_the_reader_is_refused(self) -> None:
        """A reader on stale tooling must not vouch for work it cannot place."""
        objection = self.objection(
            self.producer_at(self.new_parent), self.producer_at(self.old_parent)
        )
        self.assertIsNotNone(objection)
        self.assertIn("not an ancestor", objection)

    def test_a_hermit_pin_that_commit_never_had_is_refused(self) -> None:
        """A check the old exact-equality comparison could not make at all."""
        producer = self.producer_at(self.old_parent)
        producer["hermit_sha"] = "1" * 40
        objection = self.objection(producer, self.producer_at(self.new_parent))
        self.assertIsNotNone(objection)
        self.assertIn("hermit_sha", objection)
        self.assertIn("is not the gitlink recorded at", objection)

    def test_an_agent_utils_pin_that_commit_never_had_is_refused(self) -> None:
        producer = self.producer_at(self.old_parent)
        producer["agent_utils_sha"] = "2" * 40
        objection = self.objection(producer, self.producer_at(self.new_parent))
        self.assertIsNotNone(objection)
        self.assertIn("agent_utils_sha", objection)

    def test_a_state_root_that_is_not_a_repository_refuses_rather_than_passes(self) -> None:
        """A provenance check that cannot run must not read as verified."""
        objection = reader.provenance_objection(
            self.producer_at(self.old_parent),
            self.producer_at(self.new_parent),
            self.tmp / "not-a-repo",
        )
        self.assertIsNotNone(objection)


class TheReaderStillRefusesEverythingElseItDidBefore(unittest.TestCase):
    """Freshness and shape are unchanged; provenance is not the only gate."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="wrkslots-freshness-"))
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(self.tmp)]))

    def test_a_missing_result_is_no_result(self) -> None:
        state, summary, _ = reader.evaluate(
            self.tmp / "absent.json",
            max_age_seconds=7200,
            state_root=self.tmp,
            expected_parent_sha="a" * 40,
            expected_hermit_sha="b" * 40,
            expected_agent_utils_sha="c" * 40,
        )
        self.assertEqual("NO_RESULT", state)
        self.assertIn("never completed", summary)

    def test_an_unreadable_result_is_no_result(self) -> None:
        path = self.tmp / "latest.json"
        path.write_text("{not json", encoding="utf-8")
        state, summary, _ = reader.evaluate(
            path,
            max_age_seconds=7200,
            state_root=self.tmp,
            expected_parent_sha="a" * 40,
            expected_hermit_sha="b" * 40,
            expected_agent_utils_sha="c" * 40,
        )
        self.assertEqual("NO_RESULT", state)
        self.assertIn("unreadable", summary)


if __name__ == "__main__":
    unittest.main()
