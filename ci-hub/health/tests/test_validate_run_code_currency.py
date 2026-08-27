#!/usr/bin/env python3
"""Both directions for the validate-run tooling-currency check.

A check that fires on everything is ignored within a week, and this project has
one that passed 7 of 7 with zero call sites. So every case that MUST fire is
asserted alongside a case that MUST stay silent, and the silent cases are the
ones that keep this usable: a finished run, and a run whose tooling matches.
"""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "validate_run_code_currency.py"
SPEC = importlib.util.spec_from_file_location("validate_run_code_currency", MODULE_PATH)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)

OLD = "a" * 40
NEW = "b" * 40


def record(**over) -> dict:
    base = {
        "schema_version": 1,
        "state": "running",
        "agent": "hermit-138",
        "target": "c" * 40,
        "parent_checkout_head": OLD,
    }
    base.update(over)
    return base


class ValidateRunCodeCurrencyTest(unittest.TestCase):
    def test_run_started_on_superseded_tooling_is_named(self) -> None:
        v = mod.assess_records([("run-1.json", record())], NEW)
        self.assertEqual(len(v), 1)
        self.assertEqual(v[0]["verdict"], "drifted")
        self.assertEqual(v[0]["run"], "run-1.json")
        self.assertEqual(v[0]["agent"], "hermit-138")
        # The detail must carry BOTH commits: a reader has to be able to tell
        # what it started with and what the tree holds now, without re-deriving.
        self.assertIn(OLD[:12], v[0]["detail"])
        self.assertIn(NEW[:12], v[0]["detail"])

    def test_matching_tooling_is_silent(self) -> None:
        """The case that keeps this from becoming noise."""

        v = mod.assess_records([("run-1.json", record())], OLD)
        self.assertEqual(len(v), 1)
        self.assertEqual(v[0]["verdict"], "current")

    def test_finished_runs_are_not_reported_however_old_their_tooling(self) -> None:
        """History is not exposure. Without this, every past run fires forever."""

        for state in ("succeeded", "failed", "inactive", "released", ""):
            with self.subTest(state=state):
                v = mod.assess_records([("r.json", record(state=state))], NEW)
                self.assertEqual(v, [])

    def test_missing_or_malformed_recorded_commit_is_unknown_never_current(self) -> None:
        for bad in (None, "", "not-a-sha", 7, OLD[:12]):
            with self.subTest(bad=bad):
                v = mod.assess_records(
                    [("r.json", record(parent_checkout_head=bad))], NEW
                )
                self.assertEqual(v[0]["verdict"], "unknown")

    def test_unreadable_parent_head_is_unknown_never_current(self) -> None:
        v = mod.assess_records([("r.json", record())], None)
        self.assertEqual(v[0]["verdict"], "unknown")
        self.assertIn("could not be read", v[0]["detail"])

    def test_unparseable_record_is_surfaced_not_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            d = Path(raw)
            (d / "broken.json").write_text("{not json")
            (d / "good.json").write_text(json.dumps(record()))
            loaded = mod.load_records(d)
        self.assertEqual(len(loaded), 2)
        v = mod.assess_records(loaded, NEW)
        # broken -> unknown (it must not read as nothing-to-see), good -> drifted
        self.assertEqual(
            sorted(x["verdict"] for x in v), ["drifted", "unknown"]
        )

    def test_exit_codes_separate_drift_from_unknown_from_clean(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            d = Path(raw)
            (d / "a.json").write_text(json.dumps(record()))
            # parent HEAD is read from a real repo; point --root at a
            # non-repository so the head is unreadable -> unknown, rc 2.
            rc = mod.main(["--runs-dir", str(d), "--root", str(d), "--json"])
        self.assertEqual(rc, 2)

    def test_clean_tree_with_no_live_runs_exits_zero_and_says_nothing(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            d = Path(raw)
            (d / "done.json").write_text(json.dumps(record(state="succeeded")))
            rc = mod.main(["--runs-dir", str(d), "--root", str(d), "--json"])
        self.assertEqual(rc, 0)


if __name__ == "__main__":
    unittest.main()
