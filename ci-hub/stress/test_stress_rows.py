#!/usr/bin/env python3
"""Contract tests for nightly stress burst and stored run rows."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import stress_store  # noqa: E402
from stress_rows import BurstStatus  # noqa: E402
from stress_rows import StoreError  # noqa: E402
from stress_rows import StressRunRecord  # noqa: E402
from stress_rows import StressVerdict  # noqa: E402


SHA = "0123456789abcdef0123456789abcdef01234567"


def build(csv_text: str) -> StressRunRecord:
    return stress_store.build_record(
        csv_text=csv_text,
        repo="rrnewton/hermit",
        git_sha=SHA,
        workload="tests_misc:vfork::vfork_parent_resumes_after_child_exec",
        width=64,
        timeout_s=20,
        backend="ptrace",
        host="fixture-host",
        agent="fixture-agent",
        started_at="2026-08-27T00:00:00Z",
        finished_at="2026-08-27T00:01:00Z",
        source_tool="fixture",
        trigger="nightly",
    )


class BurstCsvContract(unittest.TestCase):
    def test_documented_header_is_accepted(self):
        text = (
            "sha,short,build_s,burst_N,hangs,passes,other,hang_rate,STATUS\n"
            f"{SHA},short,1,64,0,64,0,0.000,OK"
        )
        self.assertEqual(build(text).summary.verdict, StressVerdict.CLEAN)

    def test_complete_clean_row_is_green(self):
        record = build(f"{SHA},short,1,64,0,64,0,0.000,OK")
        self.assertEqual(record.summary.verdict, StressVerdict.CLEAN)
        self.assertFalse(record.summary.alarm)
        self.assertEqual(record.rows[0].status, BurstStatus.OK)

    def test_unaccounted_instance_is_refused_by_name(self):
        with self.assertRaisesRegex(StoreError, "hangs \\+ passes \\+ other"):
            build(f"{SHA},short,1,64,0,63,0,0.000,OK")

    def test_ok_row_without_burst_n_is_refused_by_name(self):
        with self.assertRaisesRegex(StoreError, "burst_N"):
            build(f"{SHA},short,1,,0,64,0,0.000,OK")

    def test_extra_column_is_refused_instead_of_truncated(self):
        with self.assertRaisesRegex(StoreError, "expected 9 columns, got 10"):
            build(f"{SHA},short,1,64,0,64,0,0.000,OK,ignored")

    def test_unknown_status_is_refused_by_name(self):
        with self.assertRaisesRegex(StoreError, "unknown status 'MAYBE'"):
            build(f"{SHA},short,1,64,0,64,0,0.000,MAYBE")


class StoredRecordContract(unittest.TestCase):
    def test_current_record_round_trips_without_changing_values(self):
        record = build(f"{SHA},short,1,64,1,63,0,0.016,OK")
        value = record.to_dict()
        parsed = StressRunRecord.from_dict(json.loads(json.dumps(value)))
        self.assertEqual(parsed.to_dict(), value)

    def test_stored_summary_cannot_disagree_with_rows(self):
        value = build(f"{SHA},short,1,64,0,64,0,0.000,OK").to_dict()
        value["passes"] = 63
        with self.assertRaisesRegex(StoreError, "passes"):
            StressRunRecord.from_dict(value)

    def test_stored_unknown_field_is_refused(self):
        value = build(f"{SHA},short,1,64,0,64,0,0.000,OK").to_dict()
        value["new_claim"] = True
        with self.assertRaisesRegex(StoreError, "unknown field 'new_claim'"):
            StressRunRecord.from_dict(value)

    def test_newer_schema_is_refused_by_version_before_its_fields(self):
        value = build(f"{SHA},short,1,64,0,64,0,0.000,OK").to_dict()
        value["schema_version"] = 2
        value["new_claim"] = True
        with self.assertRaisesRegex(StoreError, "schema_version.*unsupported value 2"):
            StressRunRecord.from_dict(value)


if __name__ == "__main__":
    unittest.main(verbosity=2)
