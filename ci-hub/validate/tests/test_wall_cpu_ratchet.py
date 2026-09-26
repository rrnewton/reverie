#!/usr/bin/env python3
"""Offline tests for wall_cpu_ratchet.py.

Bracket BOTH sides of every gate (plant the violating case, confirm the alarm;
plant the qualifying case, confirm it stays clean), per the Proxy Binding
review axis: a ratchet that never fires and one that always fires are equally
useless.

Run:  python3 -m unittest ci-hub/validate/tests/test_wall_cpu_ratchet.py
"""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import wall_cpu_ratchet as R  # noqa: E402


def _apply_case(row: dict, case: dict) -> dict:
    """Apply a release-payload-cases.json case: drop the pair and every absent
    key, then set exactly the case's fields."""
    for key in ("release_builder", "e2e_payload", *case.get("absent", ())):
        row.pop(key, None)
    row.update(case["fields"])
    return row


def _row(commit, started, finished, wall, cpu, *, cache="warm", host="h1", profile="full", result="pass"):
    return {
        "schema_version": 3,
        "started_at": started,
        "finished_at": finished,
        "host": host,
        "profile": profile,
        "cache_state": cache,
        "commit": commit,
        "result": result,
        "real_seconds": wall,
        "user_seconds": cpu * 0.7,
        "sys_seconds": cpu * 0.3,
    }


CASES = json.loads(open(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                               "fixtures", "release-payload-cases.json")).read())["cases"]
BUCK = next(case["fields"] for case in CASES if case["label"] == "consistent buck")


def _ts(day, hh, mm=0):
    return f"2026-08-{day:02d}T{hh:02d}:{mm:02d}:00Z"


def _write(rows):
    fh = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
    for r in rows:
        fh.write(json.dumps(r) + "\n")
    fh.close()
    return fh.name


class BaselineAndBucketing(unittest.TestCase):
    def test_insufficient_baseline_is_no_result_not_pass(self):
        # 3 priors < min_baseline 8 -> status insufficient, exit 2.
        rows = [_row(f"c{i}", _ts(3, i), _ts(3, i, 30), 480, 1100) for i in range(3)]
        rows.append(_row("target", _ts(3, 20), _ts(3, 20, 30), 5000, 9000))
        runs = R.load_runs(_write(rows))
        v = R.evaluate(runs, runs[-1])
        self.assertEqual(v.status, "insufficient")
        self.assertEqual(v.exit_code(), 2)

    def test_cold_target_not_compared_to_warm_baseline(self):
        # 10 warm ~480s priors; a cold 730s target must NOT alarm (different
        # bucket -> its own baseline is empty -> insufficient, not regression).
        rows = [_row(f"w{i}", _ts(3, i % 24), _ts(3, (i % 24)), 480, 1100) for i in range(10)]
        rows.append(_row("coldtarget", _ts(4, 2), _ts(4, 2, 12), 730, 5800, cache="cold"))
        runs = R.load_runs(_write(rows))
        v = R.evaluate(runs, [r for r in runs if r.commit == "coldtarget"][0])
        self.assertNotEqual(v.status, "regression")


class BracketWall(unittest.TestCase):
    def _stable_warm(self, n=12):
        # Serial, non-overlapping warm priors ~480s so concurrency == 1.
        rows = []
        for i in range(n):
            rows.append(_row(f"base{i}", _ts(3, i, 0), _ts(3, i, 8), 480 + (i % 3) * 5, 1100))
        return rows

    def test_clean_when_within_band(self):
        rows = self._stable_warm()
        rows.append(_row("good", _ts(4, 1, 0), _ts(4, 1, 8), 495, 1120))
        runs = R.load_runs(_write(rows))
        v = R.evaluate(runs, [r for r in runs if r.commit == "good"][0])
        self.assertEqual(v.status, "clean", v.message)
        self.assertEqual(v.exit_code(), 0)

    def test_wall_regression_fires_when_concurrency_normal(self):
        rows = self._stable_warm()
        # target runs ALONE (conc 1, same as baseline) but wall doubled -> real.
        rows.append(_row("slowcommit", _ts(4, 1, 0), _ts(4, 1, 20), 980, 1150))
        runs = R.load_runs(_write(rows))
        v = R.evaluate(runs, [r for r in runs if r.commit == "slowcommit"][0])
        self.assertEqual(v.status, "regression")
        self.assertEqual(v.exit_code(), 3)
        wall = [m for m in v.metrics if m.metric == "wall"][0]
        self.assertTrue(wall.crossed and not wall.confounded)

    def test_wall_crossing_confounded_by_concurrency_not_blamed(self):
        # Baseline is serial (conc 1). Target's wall is high AND many other
        # validates overlap it -> CONFOUNDED, exit 0, commit not blamed.
        rows = self._stable_warm()
        # 8 overlapping validates around the target window.
        for i in range(8):
            rows.append(_row(f"noise{i}", _ts(4, 1, 0), _ts(4, 1, 25), 500, 1100))
        rows.append(_row("victim", _ts(4, 1, 5), _ts(4, 1, 22), 900, 1160))
        runs = R.load_runs(_write(rows))
        v = R.evaluate(runs, [r for r in runs if r.commit == "victim"][0])
        wall = [m for m in v.metrics if m.metric == "wall"][0]
        self.assertTrue(wall.crossed)
        self.assertTrue(wall.confounded)
        self.assertIn(v.status, ("confounded", "clean"))
        self.assertEqual(v.exit_code(), 0)


class BracketCpu(unittest.TestCase):
    def test_cpu_regression_fires_even_under_concurrency(self):
        # CPU-seconds is not concurrency-sensitive: a CPU blowup fires even when
        # the target ran amid many concurrent validates.
        rows = []
        for i in range(12):
            rows.append(_row(f"base{i}", _ts(3, i, 0), _ts(3, i, 8), 480, 1100))
        for i in range(8):
            rows.append(_row(f"noise{i}", _ts(4, 1, 0), _ts(4, 1, 25), 500, 1100))
        # wall confounded, but cpu doubled -> real work grew.
        rows.append(_row("expensive", _ts(4, 1, 5), _ts(4, 1, 22), 900, 2400))
        runs = R.load_runs(_write(rows))
        v = R.evaluate(runs, [r for r in runs if r.commit == "expensive"][0])
        self.assertEqual(v.status, "regression")
        cpu = [m for m in v.metrics if m.metric == "cpu"][0]
        self.assertTrue(cpu.crossed and not cpu.confounded)


class BuilderSeparation(unittest.TestCase):
    def test_buck_release_payload_is_never_a_timing_sample(self):
        # Twelve Cargo priors ~485s, then twelve faster Buck release-payload
        # runs. Mixed in, the Buck runs would drag the baseline down and make an
        # ordinary Cargo target read as a regression.
        rows = []
        for i in range(12):
            rows.append(_row(f"base{i}", _ts(3, i, 0), _ts(3, i, 8), 480 + (i % 3) * 5, 1100))
        for i in range(12):
            buck = _row(f"buck{i}", _ts(3, 12 + i, 0), _ts(3, 12 + i, 4), 200, 500)
            buck.update(BUCK)
            rows.append(buck)
        rows.append(_row("good", _ts(4, 1, 0), _ts(4, 1, 8), 495, 1120))
        runs = R.load_runs(_write(rows))
        v = R.evaluate(runs, [r for r in runs if r.commit == "good"][0])
        self.assertEqual(v.status, "clean", v.message)
        self.assertEqual(v.baseline_n, 12)
        buck_runs = [r for r in runs if r.commit.startswith("buck")]
        self.assertIsNone(R._pick_target(buck_runs, "full", None))

    def test_buck_run_still_counts_toward_concurrency(self):
        cargo = _row("cargo", _ts(4, 1, 0), _ts(4, 1, 10), 480, 1100)
        buck = _row("buck", _ts(4, 1, 5), _ts(4, 1, 15), 200, 500)
        buck.update(BUCK)
        runs = R.load_runs(_write([cargo, buck]))
        self.assertEqual([r.concurrency for r in runs], [2, 2])

    def test_only_a_consistent_cargo_row_is_a_timing_sample(self):
        # Neither field, or the exact Cargo builder/payload pair, times the
        # Cargo debug binary; a Buck row or an inconsistent pairing does not.
        self.assertEqual(11, sum(case["expected"] is not None for case in CASES))
        for case in CASES:
            with self.subTest(case=case["label"]):
                row = _apply_case(_row("row", _ts(4, 1, 0), _ts(4, 1, 8), 480, 1100), case)
                (run,) = R.load_runs(_write([row]))
                self.assertEqual(case["expected"] == "cargo", run.timed_pass)


class Concurrency(unittest.TestCase):
    def test_overlap_count_inclusive_of_self(self):
        # a:[1:00,1:06] b:[1:05,1:15] c:[1:08,1:20] — a ends before c starts.
        rows = [
            _row("a", _ts(4, 1, 0), _ts(4, 1, 6), 360, 800),
            _row("b", _ts(4, 1, 5), _ts(4, 1, 15), 600, 1100),
            _row("c", _ts(4, 1, 8), _ts(4, 1, 20), 720, 1300),
        ]
        runs = R.load_runs(_write(rows))
        by = {r.commit: r.concurrency for r in runs}
        self.assertEqual(by["a"], 2)  # a overlaps b only (self+1)
        self.assertEqual(by["b"], 3)  # b overlaps a and c
        self.assertEqual(by["c"], 2)  # c overlaps b only


if __name__ == "__main__":
    unittest.main()
