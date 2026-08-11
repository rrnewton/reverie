#!/usr/bin/env python3

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
GATE = ROOT / "ci-hub" / "health" / "ledger_spool_backlog.py"
TICK_CONFIG = ROOT / "ci-hub" / "health" / "tick-hub.yaml"


class LedgerSpoolBacklogTest(unittest.TestCase):
    def run_gate(
        self, spool: Path, bound: int = 60
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(GATE),
                "--gate",
                "--spool-dir",
                str(spool),
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
        self.assertIn("python3 ci-hub/health/ledger_spool_backlog.py --gate", config)


if __name__ == "__main__":
    unittest.main(verbosity=2)
