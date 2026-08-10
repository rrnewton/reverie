#!/usr/bin/env python3
from __future__ import annotations

import csv
import importlib.util
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("publisher", HERE / "publish-scorecard.py")
assert SPEC and SPEC.loader
publisher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publisher)

HEADER = ["run_id", "test_id", "backend", "outcome", "comparison_tier"]


def write(path: Path, rows: list[dict[str, str]], header=HEADER):
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=header, lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)


def read(path: Path):
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


class CurrentScorecardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="scorecard-publish-")
        root = Path(self.temp.name)
        self.current = root / "scorecard.csv"
        self.history = root / "history" / "scorecard-observations.csv"
        self.history.parent.mkdir()
        self.original = [
            {"run_id": "old", "test_id": "a", "backend": "ptrace", "outcome": "fail", "comparison_tier": "legacy-unqualified"},
            {"run_id": "once", "test_id": "b", "backend": "dbi", "outcome": "pass", "comparison_tier": "legacy-unqualified"},
            {"run_id": "new", "test_id": "a", "backend": "ptrace", "outcome": "pass", "comparison_tier": "legacy-unqualified"},
        ]
        write(self.current, self.original)

    def tearDown(self):
        self.temp.cleanup()

    def test_initialise_preserves_history_and_rewrites_current(self):
        self.assertEqual((3, 2), publisher.initialise(self.current, self.history))
        self.assertEqual((3, 2), publisher.check_projection(self.current, self.history))
        history = read(self.history)
        current = read(self.current)
        self.assertEqual(3, len(history))
        self.assertEqual(2, len(current))
        self.assertEqual("once", next(row for row in current if row["test_id"] == "b")["run_id"])
        self.assertEqual("new", next(row for row in current if row["test_id"] == "a")["run_id"])
        self.assertTrue(all(row["stack_parity"] == "" and row["heap_parity"] == "" for row in history))
        self.assertTrue(all(row["stack_parity"] == "" and row["heap_parity"] == "" for row in current))

    def test_new_rows_carry_modes_and_refold_is_byte_identical(self):
        publisher.initialise(self.current, self.history)
        observation = self.current.parent / "observation.csv"
        header = HEADER + ["stack_parity", "heap_parity"]
        write(
            observation,
            [
                {"run_id": "measured", "test_id": "a", "backend": "ptrace", "outcome": "pass", "comparison_tier": "legacy-unqualified", "stack_parity": "1", "heap_parity": "0"},
                {"run_id": "measured", "test_id": "c", "backend": "dbi", "outcome": "pass", "comparison_tier": "legacy-unqualified", "stack_parity": "0", "heap_parity": "0"},
            ],
            header,
        )
        self.assertEqual((5, 3, True), publisher.publish(observation, self.current, self.history))
        rows = {row["test_id"]: row for row in read(self.current)}
        self.assertEqual(("1", "0"), (rows["a"]["stack_parity"], rows["a"]["heap_parity"]))
        self.assertEqual(("0", "0"), (rows["c"]["stack_parity"], rows["c"]["heap_parity"]))
        current_bytes = self.current.read_bytes()
        history_bytes = self.history.read_bytes()
        self.assertEqual((5, 3, False), publisher.publish(observation, self.current, self.history))
        self.assertEqual(current_bytes, self.current.read_bytes())
        self.assertEqual(history_bytes, self.history.read_bytes())

    def test_blank_mode_in_a_new_observation_refuses(self):
        publisher.initialise(self.current, self.history)
        observation = self.current.parent / "observation.csv"
        header = HEADER + ["stack_parity", "heap_parity"]
        write(
            observation,
            [{"run_id": "new", "test_id": "c", "backend": "dbi", "outcome": "pass", "comparison_tier": "legacy-unqualified", "stack_parity": "", "heap_parity": "0"}],
            header,
        )
        with self.assertRaisesRegex(publisher.PublishError, "blank stack_parity"):
            publisher.publish(observation, self.current, self.history)

    def test_green_query_requires_both_stronger_modes(self):
        rows = [
            {"outcome": "pass", "comparison_tier": "full-stdout-info-stack-heap", "stdout_parity": "1", "compared_log_messages": "2|2", "stack_parity": "", "heap_parity": ""},
            {"outcome": "pass", "comparison_tier": "full-stdout-info-stack-heap", "stdout_parity": "true", "compared_log_messages": "2/2", "stack_parity": "1", "heap_parity": "1"},
            {"outcome": "pass", "comparison_tier": "legacy-unqualified", "stdout_parity": "1", "compared_log_messages": "2|2", "stack_parity": "1", "heap_parity": "1"},
        ]
        self.assertEqual((2, 1), publisher.green_counts(rows))

    def test_green_query_refuses_tier_labels_without_real_operands(self):
        rows = [
            {"outcome": "pass", "comparison_tier": "full-stdout-info-stack-heap", "stdout_parity": "", "compared_log_messages": "2|2", "stack_parity": "1", "heap_parity": "1"},
            {"outcome": "pass", "comparison_tier": "full-stdout-info-stack-heap", "stdout_parity": "1", "compared_log_messages": "", "stack_parity": "1", "heap_parity": "1"},
            {"outcome": "pass", "comparison_tier": "full-stdout-info-stack-heap", "stdout_parity": "1", "compared_log_messages": "0|0", "stack_parity": "1", "heap_parity": "1"},
            {"outcome": "fail", "comparison_tier": "full-stdout-info-stack-heap", "stdout_parity": "1", "compared_log_messages": "2|2", "stack_parity": "1", "heap_parity": "1"},
        ]
        self.assertEqual((0, 0), publisher.green_counts(rows))

    def test_reused_run_id_with_different_rows_refuses(self):
        publisher.initialise(self.current, self.history)
        observation = self.current.parent / "observation.csv"
        header = HEADER + ["stack_parity", "heap_parity"]
        write(
            observation,
            [{"run_id": "old", "test_id": "z", "backend": "dbi", "outcome": "pass", "comparison_tier": "legacy-unqualified", "stack_parity": "0", "heap_parity": "0"}],
            header,
        )
        with self.assertRaisesRegex(publisher.PublishError, "different rows"):
            publisher.publish(observation, self.current, self.history)

    def test_projection_check_refuses_a_stale_current_view(self):
        publisher.initialise(self.current, self.history)
        rows = read(self.current)
        rows[0]["outcome"] = "corrupted"
        write(self.current, rows, list(rows[0]))
        with self.assertRaisesRegex(publisher.PublishError, "not newest"):
            publisher.check_projection(self.current, self.history)


if __name__ == "__main__":
    unittest.main(verbosity=2)
