#!/usr/bin/env python3

from __future__ import annotations

import json
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
        self.assertIn("name: series_quarantine", config)
        self.assertIn(
            'python3 ci-hub/health/ledger_spool_backlog.py --root "$DEV_HERMIT_PARENT" '
            "--store series-quarantine --gate",
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


class SeriesQuarantineGateTest(unittest.TestCase):
    """A validate run exits 0 after quarantining; this gate is its alarm.

    Each firing state is bracketed by the same fixture in its clean state, so
    a gate that never fires cannot pass.
    """

    def setUp(self) -> None:
        sys.path.insert(0, str(ROOT / "ci-hub" / "series"))
        import series

        self.series = series
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name) / "fleet" / "dev-hermit"
        (self.parent / "ci-hub" / "ledger").mkdir(parents=True)
        (self.parent / "ci-hub" / "ledger" / "validate_rows.py").write_text("")
        self.directory = self.parent / "ignored" / "ci-hub" / "series-quarantine"
        self.env = {
            key: value for key, value in os.environ.items()
            if key != series.LEDGER_ROOT_ENV and not key.startswith("GIT_")
        } | {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
             "PYTHONDONTWRITEBYTECODE": "1"}

    def record(self, emitted_at: str = "2026-09-28T00:00:00Z", raw: bytes = b"{}") -> dict:
        return self.series.quarantine_record(
            stage="parse", run_id="unit-1", producer="validate", tree="0" * 40,
            emitted_at=emitted_at, error="JSONDecodeError", reason="not JSON",
            inputs=[self.series.quarantine_input(1, raw)], host="fixture-host",
        )

    def write(self, relative: str, records: list[dict], age: float) -> Path:
        path = self.directory / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("".join(
            json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n"
            for record in records
        ))
        moment = time.time() - age
        os.utime(path, (moment, moment))
        return path

    def publish_ledger(self, records: list[dict]) -> None:
        """A test-ledger checkout whose fetched main holds ``records``."""
        git = ["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid"]
        remote = self.parent.parent / "ledger.git"
        seed = self.parent.parent / "seed"
        subprocess.run([*git, "init", "-q", "--bare", "-b", "main", str(remote)],
                       check=True, env=self.env)
        subprocess.run([*git, "init", "-q", "-b", "main", str(seed)], check=True, env=self.env)
        shard = seed / self.series.quarantine_shard_path("hermit", "fixture-host", "2026-09")
        shard.parent.mkdir(parents=True)
        shard.write_text("".join(
            json.dumps(self.series.portable_quarantine_record(record), sort_keys=True,
                       separators=(",", ":")) + "\n"
            for record in records
        ))
        for command in (["add", "quarantine"], ["commit", "-qm", "seed"],
                        ["push", "-q", str(remote), "main"]):
            subprocess.run([*git, "-C", str(seed), *command], check=True, env=self.env)
        ledger = self.parent / "hermit_test_ledger"
        subprocess.run([*git, "clone", "-q", str(remote), str(ledger)], check=True, env=self.env)
        subprocess.run([*git, "-C", str(ledger), "remote", "set-url", "origin",
                        self.series.LEDGER_REPOSITORY], check=True, env=self.env)
        (self.parent / ".gitmodules").write_text(
            '[submodule "hermit_test_ledger"]\n path = hermit_test_ledger\n'
            f" url = {self.series.LEDGER_REPOSITORY}\n"
        )

    def run_gate(self, *, bound: int = 3600, recent: int = 600) -> tuple[int, dict]:
        result = subprocess.run(
            [sys.executable, str(GATE), "--gate", "--json", "--root", str(self.parent),
             "--store", "series-quarantine", "--max-age-seconds", str(bound),
             "--recent-seconds", str(recent)],
            text=True, capture_output=True, check=False, env=self.env,
        )
        if result.returncode == 2:
            return 2, {"stdout": result.stdout, "stderr": result.stderr}
        return result.returncode, json.loads(result.stdout)

    def test_no_quarantine_is_clean_and_reports_the_unread_ledger(self) -> None:
        rc, report = self.run_gate()
        self.assertEqual(0, rc, report)
        self.assertEqual("drained", report["state"])
        self.assertEqual(0, report["quarantine_recent_records"])
        self.assertEqual(0, report["quarantine_held_records"])
        self.assertEqual(1, report["quarantine_ledger_unread_parents"])

    def test_a_recent_local_refusal_fires_and_ages_out(self) -> None:
        path = self.write("published/unit-1.jsonl",
                          [self.record(raw=b"x"), self.record(raw=b"y")], age=5)
        rc, report = self.run_gate()
        self.assertEqual(1, rc, report)
        self.assertEqual("quarantined", report["state"])
        self.assertEqual(2, report["quarantine_recent_records"])
        self.assertIn("refused 2 input record(s)", report["summary"])
        moment = time.time() - 1200
        os.utime(path, (moment, moment))
        rc, report = self.run_gate()
        self.assertEqual(0, rc, report)
        self.assertEqual(0, report["quarantine_recent_records"])

    def test_a_held_exact_record_fires_until_it_is_gone(self) -> None:
        held = self.write("held/unit-1.jsonl", [self.record()], age=100000)
        rc, report = self.run_gate()
        self.assertEqual(1, rc, report)
        self.assertEqual("withheld", report["state"])
        self.assertEqual(1, report["quarantine_held_records"])
        self.assertEqual(0, report["quarantine_recent_records"])
        held.unlink()
        rc, report = self.run_gate()
        self.assertEqual(0, rc, report)

    def test_a_set_aside_corrupt_file_fires_as_withheld_until_it_is_gone(self) -> None:
        corrupt = self.directory / "corrupt" / "pending--unit-1.jsonl"
        corrupt.parent.mkdir(parents=True)
        corrupt.write_bytes(b'{"schema":"series-quarantine/v2","trunc')  # never parsed
        moment = time.time() - 100000
        os.utime(corrupt, (moment, moment))
        rc, report = self.run_gate()
        self.assertEqual(1, rc, report)
        self.assertEqual("withheld", report["state"])
        self.assertEqual(1, report["quarantine_corrupt_files"])
        self.assertEqual(0, report["quarantine_held_records"])
        self.assertIn("1 unreadable file(s) set aside in corrupt/", report["summary"])
        corrupt.unlink()
        rc, report = self.run_gate()
        self.assertEqual(0, rc, report)
        self.assertEqual(0, report["quarantine_corrupt_files"])

    def test_an_unpublished_refusal_past_the_bound_is_stalled(self) -> None:
        self.write("unit-1.jsonl", [self.record()], age=120)
        rc, report = self.run_gate(bound=60, recent=30)
        self.assertEqual(1, rc, report)
        self.assertEqual("stalled", report["state"])
        self.assertIn("series quarantine spool publication stalled", report["summary"])
        rc, report = self.run_gate(bound=600, recent=30)
        self.assertEqual(0, rc, report)
        self.assertEqual("pending", report["state"])

    def test_a_published_refusal_fires_after_the_local_copy_is_reaped(self) -> None:
        now = time.time()
        stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now - 30))
        self.publish_ledger([self.record(emitted_at=stamp)])
        rc, report = self.run_gate()
        self.assertEqual(1, rc, report)
        self.assertEqual("quarantined", report["state"])
        self.assertEqual(1, report["quarantine_recent_records"])
        self.assertEqual(0, report["quarantine_ledger_unread_parents"])
        # The same identity held locally is one refusal, not two.
        self.write("published/unit-1.jsonl", [self.record(emitted_at=stamp)], age=5)
        rc, report = self.run_gate()
        self.assertEqual(1, report["quarantine_recent_records"], report)

    def test_an_old_published_refusal_is_read_and_clean(self) -> None:
        self.publish_ledger([self.record(emitted_at="2026-01-01T00:00:00Z")])
        rc, report = self.run_gate()
        self.assertEqual(0, rc, report)
        self.assertEqual(0, report["quarantine_ledger_unread_parents"])

    def test_an_invalid_published_record_is_unknown_not_clean(self) -> None:
        record = self.record(emitted_at="2026-01-01T00:00:00Z")
        record["reason"] = "changed after its identity was bound"
        record["event_id"] = "series-quarantine-" + "0" * 64
        with mock.patch.object(self.series, "portable_quarantine_record", lambda value: value):
            self.publish_ledger([record])
        rc, report = self.run_gate()
        self.assertEqual(2, rc, report)
        self.assertIn("state=unknown", report["stdout"])

    def test_spool_dir_is_refused_for_the_quarantine_store(self) -> None:
        result = subprocess.run(
            [sys.executable, str(GATE), "--store", "series-quarantine",
             "--spool-dir", str(self.directory)],
            text=True, capture_output=True, check=False, env=self.env,
        )
        self.assertEqual(2, result.returncode)
        self.assertIn("--spool-dir is not supported", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
