#!/usr/bin/env python3
"""Tests for ci-hub/validate/preflight_anchor.py.

The guard refuses a head that PREDATES a producer anchor before a doomed
~17-minute validate can start. git/gh are mocked at the seam (module functions
_run / _local_contains / _compare_contains) so no test touches the network or a
real checkout. The three required behaviours are bracketed from both sides:
refuse-on-pre-anchor, pass-on-post-anchor, and reason-names-the-anchor.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import preflight_anchor as pa

ANCHOR = "bfb0a9ef1c303d1977f5f02903b70cc93e514cb5"
OTHER = "abcdef0123456789abcdef0123456789abcdef01"
HEAD = "4cdda3921111111111111111111111111111aaaa"


def _anchors_file(entries: list[dict]) -> str:
    """Write a temp anchors file and return its path (caller cleans up)."""
    fd, path = tempfile.mkstemp(suffix=".json")
    with os.fdopen(fd, "w") as fh:
        json.dump({"anchors": entries}, fh)
    return path


def _seed_entry(sha: str = ANCHOR) -> dict:
    return {"sha": sha, "field": "commit_anchored",
            "landed_utc": "2026-08-03T18:43:14Z",
            "reason": "producer predating this emits commit_anchored NULL"}


class LoadAnchorsTest(unittest.TestCase):
    def test_real_anchors_file_parses_and_has_seed(self):
        # The shipped rebase-base-floors.json must parse and carry bfb0a9ef.
        anchors = pa.load_anchors(pa.DEFAULT_ANCHORS)
        shas = {a["sha"] for a in anchors}
        self.assertIn(ANCHOR, shas)
        for a in anchors:                      # every enforced anchor is 40-hex
            self.assertEqual(len(a["sha"]), 40)

    def test_placeholder_sha_is_rejected(self):
        # A non-40-hex sha (a TBD placeholder) must raise, never silently pass:
        # a placeholder anchor would refuse every head.
        path = _anchors_file([{"sha": "TBD-not-a-real-sha", "field": "x"}])
        try:
            with self.assertRaises(pa.PreflightError):
                pa.load_anchors(path)
        finally:
            os.unlink(path)

    def test_missing_file_is_error_not_empty_ok(self):
        with self.assertRaises(pa.PreflightError):
            pa.load_anchors("/nonexistent/anchors.json")


class PreflightVerdictTest(unittest.TestCase):
    def setUp(self):
        self.path = _anchors_file([_seed_entry()])

    def tearDown(self):
        os.unlink(self.path)

    def test_refuse_on_pre_anchor(self):
        # NEGATIVE: head does NOT contain the anchor -> not ok, and main exits 2.
        with mock.patch.object(pa, "head_contains",
                               return_value=(False, "github-compare")):
            res = pa.preflight(HEAD, checkout="", repo="rrnewton/hermit",
                               anchors_path=self.path)
            self.assertFalse(res["ok"])
            self.assertEqual(len(res["missing"]), 1)
            rc = pa.main(["--head", HEAD, "--anchors", self.path])
        self.assertEqual(rc, pa.EXIT_REFUSED)

    def test_pass_on_post_anchor(self):
        # POSITIVE: head contains the anchor -> ok, main exits 0. Proves the
        # guard is not inert (does not refuse everything).
        with mock.patch.object(pa, "head_contains",
                               return_value=(True, "local-merge-base")):
            res = pa.preflight(HEAD, checkout="", repo="rrnewton/hermit",
                               anchors_path=self.path)
            self.assertTrue(res["ok"])
            self.assertEqual(res["missing"], [])
            rc = pa.main(["--head", HEAD, "--anchors", self.path])
        self.assertEqual(rc, pa.EXIT_OK)

    def test_reason_names_the_specific_anchor(self):
        with mock.patch.object(pa, "head_contains",
                               return_value=(False, "github-compare")):
            res = pa.preflight(HEAD, checkout="", repo="rrnewton/hermit",
                               anchors_path=self.path)
        text = pa.render(res)
        self.assertIn("REFUSE:", text)
        self.assertIn(ANCHOR[:12], text)          # names the specific anchor sha
        self.assertIn("commit_anchored", text)    # names the missing field
        self.assertIn(HEAD[:12], text)            # names the refused head
        self.assertIn("Rebase", text)             # states the remedy


class SeamRoutingTest(unittest.TestCase):
    """head_contains prefers the local ancestry check, else the compare API."""

    def test_local_preferred_over_compare(self):
        # When local can decide, the compare API must NOT be consulted.
        with mock.patch.object(pa, "_local_contains", return_value=True), \
             mock.patch.object(pa, "_compare_contains",
                               side_effect=AssertionError("compare not used")):
            contains, how = pa.head_contains("/co", "r/x", ANCHOR, HEAD)
        self.assertTrue(contains)
        self.assertEqual(how, "local-merge-base")

    def test_falls_back_to_compare_when_local_undecided(self):
        with mock.patch.object(pa, "_local_contains", return_value=None), \
             mock.patch.object(pa, "_compare_contains", return_value=False):
            contains, how = pa.head_contains("/co", "r/x", ANCHOR, HEAD)
        self.assertFalse(contains)
        self.assertEqual(how, "github-compare")


class CompareApiSemanticsTest(unittest.TestCase):
    """_compare_contains maps GitHub compare .status correctly, git/gh mocked."""

    def setUp(self):
        patch = mock.patch.dict(os.environ, {"FLEET_REAL_GH": sys.executable})
        patch.start()
        self.addCleanup(patch.stop)

    def _with_status(self, status: str):
        def fake_run(cmd, *, timeout):
            return subprocess.CompletedProcess(cmd, 0, stdout=status + "\n",
                                               stderr="")
        return mock.patch.object(pa, "_run", side_effect=fake_run)

    def test_ahead_and_identical_contain(self):
        for status in ("ahead", "identical"):
            with self._with_status(status):
                self.assertTrue(
                    pa._compare_contains("r/x", ANCHOR, HEAD),
                    f"{status} must mean head contains anchor")

    def test_behind_and_diverged_do_not_contain(self):
        for status in ("behind", "diverged"):
            with self._with_status(status):
                self.assertFalse(
                    pa._compare_contains("r/x", ANCHOR, HEAD),
                    f"{status} must mean head predates anchor")

    def test_unexpected_status_is_error(self):
        with self._with_status("weird"):
            with self.assertRaises(pa.PreflightError):
                pa._compare_contains("r/x", ANCHOR, HEAD)


class LocalContainsTest(unittest.TestCase):
    """_local_contains returns None (undecided) when it cannot trust git."""

    def test_absent_checkout_is_undecided(self):
        self.assertIsNone(pa._local_contains("", ANCHOR, HEAD))
        self.assertIsNone(
            pa._local_contains("/no/such/checkout", ANCHOR, HEAD))


if __name__ == "__main__":
    unittest.main()
