#!/usr/bin/env python3
"""BRACKETS for check_freshness.py — both sides of every gate.

The gate that matters most is the one that says a lane which FIRES but does not
MEASURE is stale. It is bracketed twice on purpose: a measuring run inside the
bound must read FRESH (so the alarm is not simply always-on), and a
firing-but-not-measuring lane must read STALE (so the fault cannot silence the
alarm that watches for it).

No network, no store mutation: every case builds its rows in memory.
"""
from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(os.path.abspath(__file__)).parent
sys.path.insert(0, str(HERE))

import check_freshness as F  # noqa: E402
from stress_rows import BurstRow  # noqa: E402
from stress_rows import BurstStatus  # noqa: E402
from stress_rows import StoreError  # noqa: E402
from stress_rows import StressRunRecord  # noqa: E402

NOW = datetime(2026, 8, 6, 12, 0, 0, tzinfo=timezone.utc)
SHA = "0" * 40


def run_row(hours_ago: float, *, verdict="CLEAN") -> StressRunRecord:
    stamp = (NOW - timedelta(hours=hours_ago)).isoformat().replace("+00:00", "Z")
    if verdict == "CLEAN":
        row = BurstRow(SHA, "000000000000", "1", 64, 0, 64, 0, "0.0", BurstStatus.OK)
    elif verdict == "FLAKY":
        row = BurstRow(SHA, "000000000000", "1", 64, 1, 63, 0, "0.02", BurstStatus.OK)
    elif verdict == "ERROR":
        row = BurstRow(
            SHA,
            "000000000000",
            "1",
            64,
            None,
            None,
            None,
            "",
            BurstStatus.CALIB_UNDERPOWERED,
        )
    else:  # pragma: no cover - test helper misuse
        raise AssertionError(f"unsupported fixture verdict {verdict}")
    return StressRunRecord.build(
        rows=[row],
        repo="rrnewton/hermit",
        git_sha=SHA,
        workload="tests_misc:vfork::vfork_parent_resumes_after_child_exec",
        width=64,
        timeout_s=20,
        backend="ptrace",
        host="fixture-host",
        agent="fixture-agent",
        started_at=stamp,
        finished_at=stamp,
        source_tool="fixture",
        trigger="nightly",
    )


def no_result_row(hours_ago: float) -> StressRunRecord:
    """The 2026-08-04 shape: the run fired and recorded, but measured nothing."""
    return run_row(hours_ago, verdict="ERROR")


def assess(rows, cadence=24.0, grace=24.0):
    return F.assess(rows, NOW, cadence, grace)


class FreshIsReachable(unittest.TestCase):
    """Positive side: the alarm is not inert."""

    def test_a_recent_measuring_run_is_fresh(self):
        r = assess([run_row(6)])
        self.assertEqual(r["verdict"], F.FRESH)
        self.assertFalse(r["alarm"])
        self.assertEqual(r["measuring_runs"], 1)

    def test_a_run_just_inside_the_bound_is_fresh(self):
        r = assess([run_row(47.9)])          # bound = 24 + 24 = 48h
        self.assertEqual(r["verdict"], F.FRESH)

    def test_a_flaky_but_measuring_run_still_counts_as_fresh(self):
        """Freshness is about PRODUCING a result, not about the result being
        green. A red nightly is a working nightly."""
        r = assess([run_row(3, verdict="FLAKY")])
        self.assertEqual(r["verdict"], F.FRESH)


class StaleIsDetected(unittest.TestCase):
    def test_a_run_past_the_bound_is_stale(self):
        r = assess([run_row(49)])
        self.assertEqual(r["verdict"], F.STALE)
        self.assertTrue(r["alarm"])
        self.assertIn("not firing", r["reason"])

    def test_the_live_two_night_gap_is_stale(self):
        """The actual situation on 2026-08-06: last real activity 2026-08-04."""
        r = assess([run_row(48 + 7.5)])
        self.assertEqual(r["verdict"], F.STALE)
        self.assertGreater(r["age_hours"], 48)


class FiringButNotMeasuring(unittest.TestCase):
    """THE LOAD-BEARING GATE. A lane whose calibrator is permanently
    under-powered fires nightly and measures nothing. Under a run-keyed check it
    would look fresh forever — the fault would silence the alarm watching for
    it."""

    def test_recent_no_result_runs_do_not_confer_freshness(self):
        rows = [run_row(100), no_result_row(2), no_result_row(26)]
        r = assess(rows)
        self.assertEqual(r["verdict"], F.STALE)
        self.assertTrue(r["alarm"])
        self.assertEqual(r["no_result_runs"], 2)
        self.assertIn("FIRING", r["reason"])
        self.assertIn("not MEASURED", r["reason"])

    def test_only_no_result_runs_ever_is_NEVER(self):
        r = assess([no_result_row(2), no_result_row(26)])
        self.assertEqual(r["verdict"], F.NEVER)
        self.assertTrue(r["alarm"])
        self.assertIn("NONE measured", r["reason"])

    def test_a_run_keyed_check_would_have_called_these_fresh(self):
        """Pins the distinction itself: by run time these rows ARE recent; it is
        only the measuring filter that makes them stale. If this ever fails, the
        two notions have been collapsed."""
        rows = [no_result_row(2)]
        newest_any = max(F.row_time(x) for x in rows)
        self.assertLess((NOW - newest_any).total_seconds() / 3600.0, 24)
        self.assertEqual(assess(rows)["verdict"], F.NEVER)

    def test_bursts_ok_without_instances_is_refused_by_name(self):
        value = run_row(2).to_dict()
        value["total_instances"] = 0
        with self.assertRaisesRegex(StoreError, "total_instances"):
            StressRunRecord.from_dict(value)

    def test_instances_without_bursts_ok_is_refused_by_name(self):
        value = run_row(2).to_dict()
        value["bursts_ok"] = 0
        with self.assertRaisesRegex(StoreError, "bursts_ok"):
            StressRunRecord.from_dict(value)

    def test_a_real_measurement_passes_the_filter(self):
        self.assertTrue(F.measured(run_row(2)))


class EmptyAndMalformed(unittest.TestCase):
    def test_empty_store_is_NEVER(self):
        r = assess([])
        self.assertEqual(r["verdict"], F.NEVER)
        self.assertIn("never recorded", r["reason"])

    def test_rows_without_timestamps_are_not_counted_as_measuring(self):
        value = run_row(2).to_dict()
        del value["finished_at"]
        with self.assertRaisesRegex(StoreError, "finished_at"):
            StressRunRecord.from_dict(value)

    def test_missing_store_file_alarms_via_cli(self):
        with tempfile.TemporaryDirectory() as tmp:
            code = F.main(["--store", str(Path(tmp) / "absent.jsonl"), "--json"])
            self.assertEqual(code, F.EXIT_ALARM)


class BoundIsStatedNotImplied(unittest.TestCase):
    def test_the_report_names_the_bound_and_calls_it_a_policy_choice(self):
        r = assess([run_row(1)])
        self.assertEqual(r["bound_hours"], 48.0)
        self.assertIn("POLICY CHOICE", r["bound_basis"])

    def test_a_tighter_grace_moves_the_boundary(self):
        self.assertEqual(assess([run_row(30)], grace=0.0)["verdict"], F.STALE)
        self.assertEqual(assess([run_row(30)], grace=24.0)["verdict"], F.FRESH)


class CliContract(unittest.TestCase):
    def test_exit_codes_and_json_shape(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp) / "s.jsonl"
            store.write_text(json.dumps(run_row(2).to_dict()) + "\n")
            self.assertEqual(
                F.main(["--store", str(store), "--now", NOW.isoformat(), "--json"]),
                F.EXIT_OK)
            store.write_text(json.dumps(no_result_row(2).to_dict()) + "\n")
            self.assertEqual(
                F.main(["--store", str(store), "--now", NOW.isoformat(), "--json"]),
                F.EXIT_ALARM)

    def test_malformed_line_refuses_the_store_by_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp) / "s.jsonl"
            store.write_text(
                "{not json\n" + json.dumps(run_row(2).to_dict()) + "\n"
            )
            with self.assertRaisesRegex(StoreError, "line 1: invalid JSON"):
                F.load_rows(store)
            self.assertEqual(
                F.main(["--store", str(store), "--now", NOW.isoformat(), "--json"]),
                F.EXIT_ERROR,
            )

    def test_semantically_malformed_line_refuses_the_store_by_field_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp) / "s.jsonl"
            value = run_row(2).to_dict()
            value["passes"] = 63
            store.write_text(json.dumps(value) + "\n")
            with self.assertRaisesRegex(StoreError, "passes"):
                F.load_rows(store)


if __name__ == "__main__":
    unittest.main(verbosity=2)
