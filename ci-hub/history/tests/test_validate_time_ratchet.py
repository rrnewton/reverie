#!/usr/bin/env python3
"""Both-direction fixtures for the validate CPU ratchet.

Negatives alone would be satisfied by a tool that flags everything, so each is
paired with a positive that must stay quiet. Every fixture is a tmpdir ledger;
the real ledger is never read or written.
"""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from unittest import mock
from pathlib import Path

MODULE = Path(__file__).resolve().parents[1] / "validate_time_ratchet.py"
SPEC = importlib.util.spec_from_file_location("vtr", MODULE)
assert SPEC and SPEC.loader
vtr = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(vtr)


def ledger(tmp: str, cpus: list[float], profile: str = "full") -> Path:
    p = Path(tmp) / "ledger.jsonl"
    with p.open("w") as f:
        for index, c in enumerate(cpus):
            f.write(json.dumps(row(c, profile, commit=f"{index:040x}")) + "\n")
    return p


def row(
    cpu: float,
    profile: str = "full",
    *,
    commit: str = "a" * 40,
    finished_at: str | None = "2026-08-07T00:00:00Z",
    result: str = "pass",
    executed_tests: int = 1,
) -> dict:
    value = {
        "schema_version": 3,
        "commit": commit,
        "profile": profile,
        "result": result,
        "executed_tests": executed_tests,
        "gates_run": 1,
        "gates_expected": 1,
        "user_seconds": cpu * 0.8,
        "sys_seconds": cpu * 0.2,
        "real_seconds": cpu / 4,
    }
    if finished_at is not None:
        value["finished_at"] = finished_at
    return value


class RatchetTest(unittest.TestCase):
    def run_main(self, ledger_path: Path, arguments: list[str]) -> int:
        with mock.patch.object(vtr, "DEFAULT_LEDGER", ledger_path):
            return vtr.main(arguments)

    def test_positive_a_normal_run_is_not_flagged(self):
        """Load-bearing: a tool that alarms on ordinary runs gets muted."""
        with tempfile.TemporaryDirectory() as tmp:
            led = ledger(tmp, [100.0] * 40)
            self.assertEqual(0, self.run_main(led, ["--cpu-seconds", "105", "--gate"]))

    def test_negative_a_regressed_run_is_flagged_and_gates(self):
        with tempfile.TemporaryDirectory() as tmp:
            led = ledger(tmp, [100.0] * 40)
            self.assertEqual(1, self.run_main(led, ["--cpu-seconds", "500", "--gate"]))

    def test_gate_is_opt_in_so_the_default_cannot_block_landing(self):
        """Same regressed input, no --gate: reports but exits 0."""
        with tempfile.TemporaryDirectory() as tmp:
            led = ledger(tmp, [100.0] * 40)
            self.assertEqual(0, self.run_main(led, ["--cpu-seconds", "500"]))

    def test_too_few_samples_refuses_rather_than_inventing_a_threshold(self):
        """A p90 over 3 runs is a number with no authority behind it."""
        with tempfile.TemporaryDirectory() as tmp:
            led = ledger(tmp, [100.0] * 3)
            self.assertEqual(2, self.run_main(led, ["--cpu-seconds", "500", "--gate"]))

    def test_profiles_are_not_mixed_into_one_baseline(self):
        """`quick` and `full` are different workloads; pooling them is meaningless.

        40 cheap `quick` runs plus 40 expensive `full` runs: a 500s full run must
        be judged against the full baseline (ok), not against the pooled one.
        """
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "l.jsonl"
            with p.open("w") as f:
                for index in range(40):
                    f.write(json.dumps(row(10, "quick", commit=f"1{index:039x}")) + "\n")
                for index in range(40):
                    f.write(json.dumps(row(500, "full", commit=f"2{index:039x}")) + "\n")
            self.assertEqual(0, self.run_main(p, ["--profile", "full",
                                                  "--cpu-seconds", "500", "--gate"]))
            # Judged against the cheap quick baseline, the same number IS a regression.
            self.assertEqual(1, self.run_main(p, ["--profile", "quick",
                                                  "--cpu-seconds", "500", "--gate"]))

    def test_rows_without_timing_do_not_enter_the_baseline(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "l.jsonl"
            with p.open("w") as f:
                for index in range(40):
                    f.write(json.dumps(row(100, commit=f"3{index:039x}")) + "\n")
                for index in range(20):
                    f.write(
                        json.dumps(
                            row(100, commit=f"4{index:039x}", finished_at=None)
                        )
                        + "\n"
                    )
            rows = vtr.load(p, "full")
            self.assertEqual(40, len(rows))
            self.assertEqual(40, vtr.baseline(rows)["n"], "untimed rows must be excluded")

    def test_load_uses_canonical_qualification_and_event_order(self):
        """Negative rows are refused while both valid rows remain time-ordered."""
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "l.jsonl"
            values = [
                row(200, commit="b" * 40, finished_at="2026-08-07T02:00:00Z"),
                row(900, commit="c" * 40, result="fail"),
                row(900, commit="d" * 40, executed_tests=0),
                row(100, commit="a" * 40, finished_at="2026-08-07T01:00:00Z"),
            ]
            p.write_text("".join(json.dumps(value) + "\n" for value in values))
            selected = vtr.load(p, "full")
            self.assertEqual(["a" * 40, "b" * 40], [value["commit"] for value in selected])


if __name__ == "__main__":
    unittest.main()
