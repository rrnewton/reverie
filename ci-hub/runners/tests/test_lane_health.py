#!/usr/bin/env python3
"""Tests for lane_health: durable saturation record + self-bounding throttle."""

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve()
_MODPATH = _HERE.parents[1] / "lane_health.py"
_spec = importlib.util.spec_from_file_location("lane_health", str(_MODPATH))
lh = importlib.util.module_from_spec(_spec)
sys.modules["lane_health"] = lh  # needed for dataclass under future-annotations
_spec.loader.exec_module(lh)


def _rec(path, repo, *, saturated, epoch, iso=None, reason="lane full",
         host_suitable=True, host_note="ok", green_pct=6.5, ticks=3,
         green_hours=6.5, green_authoritative_run_hours=100.0,
         green_window_start="window-start", green_window_end="window-end"):
    return lh.record_observation(
        path, repo, saturated=saturated, reason=reason,
        host_suitable=host_suitable, host_note=host_note, green_pct=green_pct,
        now_epoch=epoch, now_iso=iso or f"iso-{epoch}", sustained_ticks=ticks,
        green_hours=green_hours,
        green_authoritative_run_hours=green_authoritative_run_hours,
        green_window_start=green_window_start,
        green_window_end=green_window_end)


class StoreTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / "lane-health.jsonl"

    def tearDown(self):
        self.tmp.cleanup()

    def test_single_saturated_not_yet_sustained(self):
        obs = _rec(self.path, "r/h", saturated=True, epoch=100.0)
        self.assertEqual(obs.consecutive_saturated, 1)
        self.assertFalse(obs.sustained)
        self.assertEqual(obs.streak_since, "iso-100.0")

    def test_three_consecutive_become_sustained_with_stable_streak(self):
        _rec(self.path, "r/h", saturated=True, epoch=1.0, iso="t1")
        _rec(self.path, "r/h", saturated=True, epoch=2.0, iso="t2")
        third = _rec(self.path, "r/h", saturated=True, epoch=3.0, iso="t3")
        self.assertEqual(third.consecutive_saturated, 3)
        self.assertTrue(third.sustained)
        self.assertEqual(third.streak_since, "t1")  # streak start is stable

    def test_clear_resets_streak(self):
        _rec(self.path, "r/h", saturated=True, epoch=1.0)
        _rec(self.path, "r/h", saturated=True, epoch=2.0)
        clear = _rec(self.path, "r/h", saturated=False, epoch=3.0)
        self.assertEqual(clear.consecutive_saturated, 0)
        self.assertFalse(clear.sustained)
        self.assertIsNone(clear.streak_since)
        # a new saturation after a clear starts a fresh streak
        again = _rec(self.path, "r/h", saturated=True, epoch=4.0, iso="t4")
        self.assertEqual(again.consecutive_saturated, 1)
        self.assertEqual(again.streak_since, "t4")

    def test_survives_recycling_fresh_read(self):
        # Write, then read via a code path that reopens the file from scratch --
        # this is exactly the recycled-agent (new process) read.
        _rec(self.path, "r/h", saturated=True, epoch=1.0)
        _rec(self.path, "r/h", saturated=True, epoch=2.0)
        latest = lh.latest_observation(self.path, "r/h")
        self.assertIsNotNone(latest)
        self.assertEqual(latest.consecutive_saturated, 2)
        self.assertEqual(latest.observed_epoch, 2.0)

    def test_records_host_verdict_and_qualified_green_metric(self):
        obs = _rec(self.path, "r/h", saturated=True, epoch=1.0,
                   host_suitable=False, host_note="cpu hot", green_pct=3.14,
                   green_hours=3.14, green_authoritative_run_hours=10.0,
                   green_window_start="s", green_window_end="e")
        latest = lh.latest_observation(self.path, "r/h")
        self.assertIs(latest.host_suitable, False)
        self.assertEqual(latest.host_note, "cpu hot")
        self.assertEqual(latest.green_pct, 3.14)
        self.assertEqual(latest.green_hours, 3.14)
        self.assertEqual(latest.green_authoritative_run_hours, 10.0)
        self.assertEqual(latest.green_window_start, "s")
        self.assertEqual(latest.green_window_end, "e")

    def test_malformed_lines_ignored(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with open(self.path, "w", encoding="utf-8") as fh:
            fh.write("not json\n")
            fh.write("{partial\n")
        _rec(self.path, "r/h", saturated=True, epoch=5.0)
        latest = lh.latest_observation(self.path, "r/h")
        self.assertEqual(latest.consecutive_saturated, 1)

    def test_latest_by_repo_isolates_repos(self):
        _rec(self.path, "r/h", saturated=True, epoch=1.0)
        _rec(self.path, "r/rev", saturated=False, epoch=2.0)
        _rec(self.path, "r/h", saturated=True, epoch=3.0)
        self.assertEqual(lh.latest_observation(self.path, "r/h").consecutive_saturated, 2)
        self.assertFalse(lh.latest_observation(self.path, "r/rev").saturated)


class ThrottleTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / "lane-health.jsonl"

    def tearDown(self):
        self.tmp.cleanup()

    def test_no_data_is_clear(self):
        st = lh.throttle_status(self.path, "r/h", now_epoch=10.0)
        self.assertFalse(st.engaged)
        self.assertEqual(st.code, lh.EXIT_CLEAR)
        self.assertIn("no lane observation", st.reason)

    def test_engaged_when_sustained(self):
        for e in (1.0, 2.0, 3.0):
            _rec(self.path, "r/h", saturated=True, epoch=e, ticks=3)
        st = lh.throttle_status(self.path, "r/h", now_epoch=3.0,
                                stale_after=1800, sustained_ticks=3)
        self.assertTrue(st.engaged)
        self.assertEqual(st.code, lh.EXIT_THROTTLED)
        self.assertIn("ENGAGED", st.reason)
        # OBSERVABILITY: must state what it suppresses AND that landings are not.
        self.assertIn("SUPPRESSES", st.detail)
        self.assertIn("LANDINGS", st.detail)
        # REVERSIBILITY: must state what un-throttles it.
        self.assertIn("Un-throttles", st.detail)

    def test_fail_open_on_stale_signal(self):
        for e in (1.0, 2.0, 3.0):
            _rec(self.path, "r/h", saturated=True, epoch=e, ticks=3)
        # 'now' is far past the stale bound -> must FAIL OPEN despite sustained.
        st = lh.throttle_status(self.path, "r/h", now_epoch=3.0 + 5000,
                                stale_after=1800, sustained_ticks=3)
        self.assertFalse(st.engaged)
        self.assertEqual(st.code, lh.EXIT_CLEAR)
        self.assertIn("FAIL-OPEN", st.reason)
        self.assertIn("stale", st.reason)

    def test_saturating_but_not_sustained_is_clear(self):
        _rec(self.path, "r/h", saturated=True, epoch=1.0, ticks=3)
        st = lh.throttle_status(self.path, "r/h", now_epoch=1.0,
                                stale_after=1800, sustained_ticks=3)
        self.assertFalse(st.engaged)
        self.assertIn("not yet sustained", st.reason)

    def test_clear_lane_is_clear(self):
        _rec(self.path, "r/h", saturated=False, epoch=1.0)
        st = lh.throttle_status(self.path, "r/h", now_epoch=1.0)
        self.assertFalse(st.engaged)
        self.assertIn("not saturated", st.reason)

    def test_host_phrase_discriminates_capacity_vs_contention(self):
        for e in (1.0, 2.0, 3.0):
            _rec(self.path, "r/h", saturated=True, epoch=e, host_suitable=True,
                 ticks=3)
        st = lh.throttle_status(self.path, "r/h", now_epoch=3.0)
        self.assertIn("single PMU runner", st.detail)  # capacity, not contention


class TickTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / "lane-health.jsonl"
        self._qh = lh._queue_health
        self._probe = lh._probe_host
        self._green = lh._green_metric
        lh._probe_host = lambda s: (True, "cpu 14% <= 50%")
        lh._green_metric = lambda r: {
            "green_pct": 6.48,
            "green_hours": 6.48,
            "authoritative_run_hours": 100.0,
            "window_start": "s",
            "window_end_utc": "e",
        }

    def tearDown(self):
        lh._queue_health = self._qh
        lh._probe_host = self._probe
        lh._green_metric = self._green
        self.tmp.cleanup()

    def _fake_qh(self, state, bc):
        class _Fake:
            @staticmethod
            def compute_gate(repo, gh, limit, per_call_timeout=10):
                return (1 if bc != "none" else 0), {"state": state,
                                                    "binding_constraint": bc,
                                                    "summary": "s"}
        lh._queue_health = lambda: _Fake

    def test_degraded_fetch_does_not_write_and_returns_2(self):
        self._fake_qh("unknown", "none")
        rc = lh.do_tick("r/h", "gh", 100, path=self.path,
                        now=(1.0, "t1"))
        self.assertEqual(rc, 2)
        self.assertIsNone(lh.latest_observation(self.path, "r/h"))

    def test_saturated_records_and_sustains_after_threshold(self):
        self._fake_qh(
            "red",
            "all online self-hosted runners busy; binding label not established",
        )
        rc1 = lh.do_tick("r/h", "gh", 100, path=self.path, sustained_ticks=3,
                         now=(1.0, "t1"))
        rc2 = lh.do_tick("r/h", "gh", 100, path=self.path, sustained_ticks=3,
                         now=(2.0, "t2"))
        rc3 = lh.do_tick("r/h", "gh", 100, path=self.path, sustained_ticks=3,
                         now=(3.0, "t3"))
        self.assertEqual((rc1, rc2, rc3), (0, 0, 1))  # exit 1 == fire hard-warn
        latest = lh.latest_observation(self.path, "r/h")
        self.assertTrue(latest.sustained)
        self.assertTrue(latest.host_suitable)
        self.assertEqual(latest.green_pct, 6.48)
        self.assertEqual(latest.green_authoritative_run_hours, 100.0)
        self.assertEqual(latest.green_window_start, "s")
        self.assertEqual(latest.green_window_end, "e")

    def test_clear_tick_records_and_returns_0(self):
        self._fake_qh("ok", "none")
        rc = lh.do_tick("r/h", "gh", 100, path=self.path, now=(1.0, "t1"))
        self.assertEqual(rc, 0)
        self.assertFalse(lh.latest_observation(self.path, "r/h").saturated)


if __name__ == "__main__":
    unittest.main()
