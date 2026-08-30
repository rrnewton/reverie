#!/usr/bin/env python3

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[3]
GATE = ROOT / "ci-hub" / "health" / "ledger_spool_backlog.py"
TICK_CONFIG = ROOT / "ci-hub" / "health" / "tick-hub.yaml"


class LedgerSpoolBacklogTest(unittest.TestCase):
    def run_gate(
        self, spool: Path, bound: int = 60, store: str = "ledger"
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(GATE),
                "--gate",
                "--spool-dir",
                str(spool),
                "--store",
                store,
                "--max-age-seconds",
                str(bound),
            ],
            text=True,
            capture_output=True,
            check=False,
        )

    def test_drained_then_aged_brackets_clean_and_firing_paths(self) -> None:
        """The empty result is meaningful because the same gate then fires."""
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw)
            drained = self.run_gate(spool)
            self.assertEqual(drained.returncode, 0, drained.stdout + drained.stderr)
            self.assertIn("state=drained", drained.stdout)
            self.assertIn("spool_depth=0", drained.stdout)

            old = spool / "hermit__host__2026-08__old.jsonl"
            old.write_text("{}\n", encoding="utf-8")
            timestamp = time.time() - 120
            os.utime(old, (timestamp, timestamp))
            fired = self.run_gate(spool)
            self.assertEqual(fired.returncode, 1, fired.stdout + fired.stderr)
            self.assertIn("state=stalled", fired.stdout)
            self.assertIn("spool_depth=1", fired.stdout)
            fields = dict(line.split("=", 1) for line in fired.stdout.splitlines())
            self.assertGreaterEqual(float(fields["oldest_age_seconds"]), 115.0)
            self.assertLess(float(fields["oldest_age_seconds"]), 150.0)
            self.assertIn("max_age_seconds=60.0", fired.stdout)

    def test_reports_depth_and_oldest_of_multiple_batches(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw)
            now = time.time()
            entries = (
                ("new.jsonl", 10),
                ("old.jsonl", 90),
                ("ignored.tmp", 300),
            )
            for name, age in entries:
                path = spool / name
                path.write_text("{}\n", encoding="utf-8")
                os.utime(path, (now - age, now - age))
            result = self.run_gate(spool, bound=60)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("spool_depth=2", result.stdout)
            self.assertIn("oldest_entry=old.jsonl", result.stdout)

    def test_fresh_backlog_is_visible_but_not_stalled(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw)
            (spool / "fresh.jsonl").write_text("{}\n", encoding="utf-8")
            result = self.run_gate(spool, bound=60)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("state=pending", result.stdout)
            self.assertIn("spool_depth=1", result.stdout)

    def test_tick_hub_runs_the_gate(self) -> None:
        config = TICK_CONFIG.read_text(encoding="utf-8")
        self.assertIn("name: ledger_spool_backlog", config)
        self.assertIn(
            'python3 ci-hub/health/ledger_spool_backlog.py --root "$DEV_HERMIT_PARENT" --gate',
            config,
        )
        self.assertIn("name: series_spool_backlog", config)
        self.assertIn(
            'python3 ci-hub/health/ledger_spool_backlog.py --root "$DEV_HERMIT_PARENT" --store series --gate',
            config,
        )

    def test_series_backlog_names_the_store_it_measured(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            spool = Path(raw)
            old = spool / "campaign.jsonl"
            old.write_text("{}\n", encoding="utf-8")
            timestamp = time.time() - 120
            os.utime(old, (timestamp, timestamp))
            result = self.run_gate(spool, bound=60, store="series")
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("per-test series spool publication stalled", result.stdout)


class MultiParentDiscoveryTest(unittest.TestCase):
    """The fleet is multi-parent; a root-scoped scan reports the others clean.

    Measured 2026-08-11 on devbig014: three producing parents existed and the
    root-scoped gate covered 1 of 3. Both invisibility incidents that day lived
    in a parent the gate does not live in -- including a 55-minute-old fully
    qualifying green in dev-hermit5 that the gate never saw. These brackets pin
    the fix in both directions: a sibling parent's backlog must FIRE, and a
    genuinely drained fleet must still report clean.
    """

    def _parent(self, base: Path, name: str) -> Path:
        parent = base / name
        (parent / "ci-hub" / "ledger").mkdir(parents=True)
        (parent / "ci-hub" / "ledger" / "validate_rows.py").write_text(
            "", encoding="utf-8"
        )
        spool = parent / "ignored" / "ci-hub" / "validate-ledger-spool"
        spool.mkdir(parents=True)
        return parent

    def _load(self):
        sys.path.insert(0, str(ROOT / "ci-hub" / "health"))
        import ledger_spool_backlog as gate

        return gate

    def test_discovery_enumerates_sibling_parents_without_a_spool_yet(self) -> None:
        gate = self._load()
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            own = self._parent(base, "dev-hermit")
            self._parent(base, "dev-hermit4")
            # A parent that has never spooled must still be enumerated, or the
            # gate goes blind exactly when a new parent starts producing.
            fresh = base / "dev-hermit9"
            (fresh / "ci-hub" / "ledger").mkdir(parents=True)
            (fresh / "ci-hub" / "ledger" / "validate_rows.py").write_text("")
            (base / "not-a-parent").mkdir()
            found = gate.producing_parents(own)
            self.assertEqual(len(found), 3, found)
            self.assertNotIn(base / "not-a-parent", found)

    def test_unreadable_parent_directory_is_unknown_not_local_only(self) -> None:
        gate = self._load()
        with tempfile.TemporaryDirectory() as raw:
            own = self._parent(Path(raw), "dev-hermit")
            with mock.patch.object(Path, "iterdir", side_effect=PermissionError("denied")):
                with self.assertRaisesRegex(gate.SpoolUnavailable, "cannot enumerate"):
                    gate.producing_parents(own)

    def test_a_sibling_parents_stalled_backlog_FIRES(self) -> None:
        """The exact miss: backlog in a parent the gate does not live in."""
        gate = self._load()
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            own = self._parent(base, "dev-hermit")
            other = self._parent(base, "dev-hermit5")
            stale = (
                other / "ignored" / "ci-hub" / "validate-ledger-spool" / "green.jsonl"
            )
            stale.write_text("{}\n", encoding="utf-8")
            old = time.time() - 3300
            os.utime(stale, (old, old))

            dirs = [
                p / "ignored" / "ci-hub" / "validate-ledger-spool"
                for p in gate.producing_parents(own)
            ]
            backlog = gate.measure_parents(dirs, max_age_seconds=60)
            self.assertTrue(backlog.stalled, "sibling backlog must fire")
            self.assertEqual(backlog.depth, 1)
            self.assertEqual(backlog.parents_scanned, 2)
            self.assertIn("dev-hermit5", backlog.oldest_parent or "")

            # NOT INERT: the same fleet with that one batch published is clean.
            stale.unlink()
            drained = gate.measure_parents(dirs, max_age_seconds=60)
            self.assertFalse(drained.stalled)
            self.assertEqual(drained.depth, 0)

    def test_depth_sums_and_worst_parent_owns_the_age(self) -> None:
        gate = self._load()
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            own = self._parent(base, "dev-hermit")
            other = self._parent(base, "dev-hermit4")
            now = time.time()
            for parent, name, age in (
                (own, "mine.jsonl", 100),
                (other, "theirs.jsonl", 900),
            ):
                path = (
                    parent / "ignored" / "ci-hub" / "validate-ledger-spool" / name
                )
                path.write_text("{}\n", encoding="utf-8")
                os.utime(path, (now - age, now - age))
            dirs = [
                p / "ignored" / "ci-hub" / "validate-ledger-spool"
                for p in gate.producing_parents(own)
            ]
            backlog = gate.measure_parents(dirs, max_age_seconds=60)
            self.assertEqual(backlog.depth, 2, "depth must sum across parents")
            self.assertGreaterEqual(backlog.oldest_age_seconds, 890)
            self.assertIn("dev-hermit4", backlog.oldest_parent or "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
