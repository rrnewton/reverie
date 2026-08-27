#!/usr/bin/env python3
"""Bracket the staged reverie-pin lag warning.

THE PROPERTY UNDER TEST IS THE STAGING, not "it detects a lag". The old warning
detected the lag perfectly and still rotted, because it had ONE volume: it fired
~30 times over nine hours on 2026-08-11, was accurate every time, and became
furniture. So the assertions here are about which BAND fires at which lag, and
about the two ways staging could quietly fail:

  * a lag that falls in NO band -- the gap that would hide a real lag; and
  * a fault routed to only ONE band -- whichever band is quiet that cycle would
    then silently own an unmeasurable pin or a rewritten reverie history.

There is deliberately no test for a snooze or a clearing flag, because there is
deliberately no snooze or clearing flag: the only input that quiets this tool is
a smaller lag.

The pin side is the Reverie gitlink published on parent origin/main. The shared
parent index is deliberately varied in both directions below: stale-behind when
the remote is current, and staged-current when the remote genuinely lags.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

MODULE = Path(__file__).resolve().parents[1] / "reverie_pin_lag.py"
SPEC = importlib.util.spec_from_file_location("reverie_pin_lag", MODULE)
assert SPEC and SPEC.loader
rpl = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = rpl
SPEC.loader.exec_module(rpl)

NOTE_BAND = ["--gate", "--min-lag", "1", "--max-lag", "4"]
LOUD_BAND = ["--gate", "--min-lag", "5"]


class Staging(unittest.TestCase):
    """Band routing, driven by a stubbed measurement so the lag is exact."""

    def _at(self, result: dict, argv: list[str]) -> int:
        original = rpl.measure
        rpl.measure = lambda *a, **k: result
        try:
            return rpl.main(argv)
        finally:
            rpl.measure = original

    def test_owner_boundary_is_exactly_five(self) -> None:
        # The owner's line: "<5 -> small note. 5 or more -> louder." 4 and 5 are
        # the two values that decide whether that sentence was implemented.
        for lag, note, loud in ((1, 1, 0), (4, 1, 0), (5, 0, 1), (20, 0, 1)):
            r = {"state": "lagging", "pin": "a" * 40, "main": "b" * 40, "lag": lag}
            self.assertEqual(self._at(r, NOTE_BAND), note, f"note band at lag {lag}")
            self.assertEqual(self._at(r, LOUD_BAND), loud, f"loud band at lag {lag}")

    def test_every_lag_lands_in_exactly_one_band(self) -> None:
        # A lag falling through both bands is a real lag reported by nothing --
        # the failure this staging could most plausibly introduce.
        for lag in range(1, 40):
            r = {"state": "lagging", "pin": "a" * 40, "main": "b" * 40, "lag": lag}
            fired = (self._at(r, NOTE_BAND) != 0) + (self._at(r, LOUD_BAND) != 0)
            self.assertEqual(fired, 1, f"lag {lag} fired {fired} bands, expected exactly 1")

    def test_a_current_pin_fires_nothing(self) -> None:
        r = {"state": "current", "pin": "a" * 40, "main": "a" * 40, "lag": 0}
        self.assertEqual(self._at(r, NOTE_BAND), 0)
        self.assertEqual(self._at(r, LOUD_BAND), 0)

    def test_faults_fire_in_both_bands(self) -> None:
        # Fail closed, everywhere. "I could not measure it" must never read as
        # "the lag is zero", and a rewritten reverie history must never be quiet.
        for state in ("unknown", "non-ancestor"):
            r = {"state": state, "reason": "planted", "pin": "a" * 40, "main": "b" * 40}
            self.assertEqual(self._at(r, NOTE_BAND), rpl.UNKNOWN, state)
            self.assertEqual(self._at(r, LOUD_BAND), rpl.UNKNOWN, state)


def git(*args: str, cwd: Path) -> str:
    out = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True, text=True,
        env={"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@e", "GIT_COMMITTER_NAME": "t",
             "GIT_COMMITTER_EMAIL": "t@e", "PATH": "/usr/bin:/bin", "HOME": str(cwd)},
    )
    assert out.returncode == 0, f"git {args}: {out.stderr}"
    return out.stdout.strip()


class RealRepository(unittest.TestCase):
    """Ancestry and the lag count, against actual git objects."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="pin-lag-")
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name) / "parent"
        self.reverie = self.root / "reverie"
        self.reverie.mkdir(parents=True)
        git("init", "-q", "-b", "main", cwd=self.root)
        self.parent_origin = Path(self._tmp.name) / "parent-origin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(self.parent_origin)], check=True)
        git("remote", "add", "origin", str(self.parent_origin), cwd=self.root)
        git("init", "-q", "-b", "main", cwd=self.reverie)
        # A REAL origin, so measure() exercises its actual ls-remote + fetch path
        # instead of a stub. A fetch stubbed out here would leave the one network
        # step this tool depends on completely unbracketed.
        self.origin = Path(self._tmp.name) / "reverie-origin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(self.origin)], check=True)
        git("remote", "add", "origin", str(self.origin), cwd=self.reverie)
        (self.reverie / "f").write_text("0\n")
        git("add", "-A", cwd=self.reverie)
        git("commit", "-qm", "r0", cwd=self.reverie)
        git("push", "-q", "origin", "main", cwd=self.reverie)
        self.base = git("rev-parse", "HEAD", cwd=self.reverie)

    def _advance(self, n: int) -> None:
        for i in range(n):
            (self.reverie / "f").write_text(f"{i + 1}\n")
            git("add", "-A", cwd=self.reverie)
            git("commit", "-qm", f"r{i + 1}", cwd=self.reverie)
        git("push", "-q", "origin", "main", cwd=self.reverie)

    def _record_pin(self, sha: str) -> None:
        git("update-index", "--add", "--cacheinfo", f"160000,{sha},reverie", cwd=self.root)

    def _publish_pin(self, sha: str) -> None:
        self._record_pin(sha)
        git("commit", "-qm", f"pin reverie at {sha[:12]}", cwd=self.root)
        git("push", "-q", "origin", "main", cwd=self.root)

    def _measure(self) -> dict:
        return rpl.measure(self.root)

    def test_lag_is_counted_along_ancestry(self) -> None:
        self._publish_pin(self.base)
        self._advance(6)
        current = git("rev-parse", "HEAD", cwd=self.reverie)
        # The local index claims currency. The published parent gitlink is still
        # the base, so the warning must fire from origin/main rather than go quiet.
        self._record_pin(current)
        result = self._measure()
        self.assertEqual(result["state"], "lagging", result)
        self.assertEqual(result["lag"], 6)
        self.assertEqual(result["pin"], self.base)

    def test_stale_local_index_cannot_create_a_false_lag(self) -> None:
        self._publish_pin(self.base)
        self._advance(1)
        current = git("rev-parse", "HEAD", cwd=self.reverie)
        self._publish_pin(current)

        # Deliberately make the shared index stale after origin/main is correct.
        self._record_pin(self.base)
        staged = git("ls-files", "--stage", "--", "reverie", cwd=self.root)
        self.assertEqual(staged.split()[1], self.base)

        result = self._measure()
        self.assertEqual(result["state"], "current", result)
        self.assertEqual(result["pin"], current)
        self.assertEqual(result["main"], current)
        self.assertEqual(result["lag"], 0)

    def test_unreadable_parent_origin_does_not_fall_back_to_the_index(self) -> None:
        self._record_pin(self.base)
        result = self._measure()
        self.assertEqual(result["state"], "unknown", result)
        self.assertIn("parent origin/main", result["reason"])

    def test_a_rewritten_history_is_non_ancestor_not_a_lag(self) -> None:
        """The monotonicity trap: never report an N that invites a bad bump.

        If reverie main is rewritten the pin stops being an ancestor. Reporting
        "N behind" there would invite a catch-up job to 'bump to main', which
        moves the pin off its own ancestry and destroys the hermetic link the pin
        exists to provide. There is no honest N, so there must be no N.
        """
        self._advance(2)
        orphan = git("rev-parse", "HEAD", cwd=self.reverie)
        self._publish_pin(orphan)
        git("checkout", "-q", "--orphan", "rewritten", cwd=self.reverie)
        (self.reverie / "f").write_text("rewritten\n")
        git("add", "-A", cwd=self.reverie)
        git("commit", "-qm", "rewritten root", cwd=self.reverie)
        git("branch", "-qM", "main", cwd=self.reverie)
        git("push", "-q", "--force", "origin", "main", cwd=self.reverie)
        result = self._measure()
        self.assertEqual(result["state"], "non-ancestor", result)
        self.assertNotIn("lag", result)

    def test_an_uninitialised_submodule_refuses_rather_than_answering(self) -> None:
        """`git -C reverie` walks up to the parent when reverie is an empty dir.

        Caught live while building this tool: it fetched the PARENT's origin/main
        and resolved a parent sha, then reported the reverie pin missing. Every
        number after that describes the wrong repository, so the checkout must be
        bound to itself before any git answer from it is trusted.
        """
        empty = Path(self._tmp.name) / "bare-parent"
        (empty / "reverie").mkdir(parents=True)
        git("init", "-q", "-b", "main", cwd=empty)
        parent_origin = Path(self._tmp.name) / "bare-parent-origin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(parent_origin)], check=True)
        git("remote", "add", "origin", str(parent_origin), cwd=empty)
        (empty / "x").write_text("x\n")
        git("add", "-A", cwd=empty)
        git("update-index", "--add", "--cacheinfo", f"160000,{self.base},reverie", cwd=empty)
        git("commit", "-qm", "c", cwd=empty)
        git("push", "-q", "origin", "main", cwd=empty)
        result = rpl.measure(empty)
        self.assertEqual(result["state"], "unknown", result)
        self.assertIn("not its own git checkout", result["reason"])


if __name__ == "__main__":
    unittest.main()
