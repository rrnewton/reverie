#!/usr/bin/env python3
"""Tests for ci-hub/runners/queue_health.py (pure analysis; no network)."""

from __future__ import annotations

import sys
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import queue_health as qh

NOW = datetime(2026, 8, 3, 15, 30, 0, tzinfo=timezone.utc)


def ts(minutes_ago: float) -> str:
    return (NOW - timedelta(minutes=minutes_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")


def run(wf, status, conclusion="", created_min_ago=0.0, rid=1):
    return {
        "databaseId": rid,
        "workflowName": wf,
        "status": status,
        "conclusion": conclusion,
        "createdAt": ts(created_min_ago),
        "updatedAt": ts(created_min_ago),
        "headBranch": "main",
        "event": "push",
    }


class QueueDepthTests(unittest.TestCase):
    def test_queued_vs_running_split_per_workflow(self) -> None:
        runs = [
            run("portable", "queued", created_min_ago=40, rid=1),
            run("portable", "queued", created_min_ago=10, rid=2),
            run("portable", "in_progress", rid=3),
            run("docs", "pending", created_min_ago=5, rid=4),
            run("docs", "completed", "success", rid=5),
        ]
        q = qh.analyze_queue(runs, now=NOW)
        self.assertEqual((q["portable"].queued, q["portable"].running), (2, 1))
        self.assertEqual((q["docs"].queued, q["docs"].running), (1, 0))
        # Queue age is now-createdAt; oldest portable run waited ~40 min.
        self.assertAlmostEqual(q["portable"].max_age, 40 * 60, delta=1)
        self.assertAlmostEqual(q["portable"].median_age, 10 * 60, delta=60 * 30 + 1)


class LastGreenTests(unittest.TestCase):
    def test_runs_back_counts_from_newest(self) -> None:
        # newest-first: red, red, green, red  -> green is 2 runs back.
        runs = [
            run("ci", "completed", "failure", rid=10),
            run("ci", "completed", "failure", rid=11),
            run("ci", "completed", "success", created_min_ago=120, rid=12),
            run("ci", "completed", "failure", rid=13),
        ]
        g = qh.analyze_last_green(runs)["ci"]
        self.assertEqual(g.runs_back, 2)
        self.assertEqual(g.green_id, 12)
        self.assertEqual(g.total_in_window, 4)

    def test_no_green_in_window(self) -> None:
        runs = [run("ci", "completed", "failure", rid=i) for i in range(5)]
        g = qh.analyze_last_green(runs)["ci"]
        self.assertIsNone(g.runs_back)
        self.assertIsNone(g.green_id)


class RunnerAndConstraintTests(unittest.TestCase):
    def _runner(self, name, busy, labels, status="online"):
        return {"name": name, "status": status, "busy": busy,
                "labels": [{"name": l} for l in labels]}

    def test_residual_pmu_serial_label_is_not_a_binding_constraint(self) -> None:
        api = {"total_count": 3, "runners": [
            self._runner("r1", True, ["pmu", "pmu-serial", "gate"]),
            self._runner("r2", True, ["pmu", "gate"]),
            self._runner("gate-only", False, ["gate"]),
        ]}
        rh = qh.analyze_runners(api)
        self.assertEqual((rh.pmu_total, rh.pmu_idle), (2, 0))
        self.assertEqual(rh.serial_runners, ["r1"])
        self.assertTrue(rh.serial_busy)
        queues = qh.analyze_queue([run("ci", "queued", rid=1)], now=NOW)
        bc = qh.binding_constraint(queues, rh)
        self.assertIsNone(bc)

    def test_offline_gate_runner_is_named_without_claiming_causality(self) -> None:
        api = {"total_count": 4, "runners": [
            self._runner("r1", False, ["pmu", "pmu-serial", "gate"]),
            self._runner("r2", False, ["pmu", "gate"]),
            self._runner("r3", False, ["pmu", "gate"]),
            self._runner("hermit-gate-newton", False, ["gate"], status="offline"),
        ]}
        rh = qh.analyze_runners(api)
        queues = qh.analyze_queue([run("Merge Gate", "queued", rid=1)], now=NOW)
        signal = qh.binding_constraint(queues, rh)
        self.assertIsNotNone(signal)
        self.assertIn("hermit-gate-newton[gate] offline", signal)
        self.assertIn("causal binding is not established", signal)
        self.assertNotIn("pmu-serial=", signal)

    def test_all_busy_reports_generic_pressure_not_a_lane(self) -> None:
        api = {"total_count": 2, "runners": [
            self._runner("r1", True, ["pmu", "pmu-serial", "gate"]),
            self._runner("r2", True, ["pmu", "gate"]),
        ]}
        rh = qh.analyze_runners(api)
        queues = qh.analyze_queue([run("ci", "queued", rid=1)], now=NOW)
        signal = qh.binding_constraint(queues, rh)
        self.assertIsNotNone(signal)
        self.assertIn("all online self-hosted runners busy", signal)
        self.assertIn("cannot be named as binding", signal)
        self.assertNotIn("pmu-serial=", signal)

    def test_no_constraint_when_idle_pmu(self) -> None:
        api = {"total_count": 2, "runners": [
            self._runner("r1", False, ["pmu", "pmu-serial"]),
            self._runner("r2", True, ["pmu"]),
        ]}
        rh = qh.analyze_runners(api)
        queues = qh.analyze_queue([run("ci", "queued", rid=1)], now=NOW)
        self.assertIsNone(qh.binding_constraint(queues, rh))


class WaitDistributionTests(unittest.TestCase):
    def test_wait_and_duration_kept_separate(self) -> None:
        jobs = [
            {"run_id": 1, "wait": 600.0, "duration": 30.0},
            {"run_id": 2, "wait": 1200.0, "duration": 45.0},
            {"run_id": 3, "wait": 60.0, "duration": 900.0},
        ]
        run_wf = {1: "ci", 2: "ci", 3: "ci"}
        w = qh.analyze_waits(jobs, run_wf)["ci"]
        self.assertEqual(w.n, 3)
        self.assertEqual(w.wait_max, 1200.0)
        self.assertEqual(w.wait_median, 600.0)
        # Duration distribution is independent of wait.
        self.assertEqual(w.dur_median, 45.0)


def job(runner, started_min_ago, dur_min, rid=1):
    """A jobs-API-shaped record with absolute datetimes, as fetch_job_timings
    now returns. dur_min=None => still running (completed is None)."""
    started = NOW - timedelta(minutes=started_min_ago)
    completed = None if dur_min is None else started + timedelta(minutes=dur_min)
    return {"run_id": rid, "runner": runner, "started": started,
            "completed": completed, "wait": 0.0,
            "duration": None if dur_min is None else dur_min * 60.0}


class ConfiguredVsLiveTests(unittest.TestCase):
    def _api(self, statuses):
        return {"total_count": len(statuses), "runners": [
            {"name": f"r{i}", "status": s, "busy": False, "labels": []}
            for i, s in enumerate(statuses)]}

    def test_offline_is_configured_minus_live(self) -> None:
        # 3 configured, 1 offline => the silently-dead-runner finding.
        rh = qh.analyze_runners(self._api(["online", "online", "offline"]))
        self.assertEqual(rh.total, 3)
        self.assertEqual(rh.online, 2)
        self.assertEqual(rh.offline, 1)

    def test_no_offline_when_all_online(self) -> None:
        rh = qh.analyze_runners(self._api(["online", "online"]))
        self.assertEqual(rh.offline, 0)


class UtilizationTests(unittest.TestCase):
    def test_busy_over_capacity_and_selfhosted_filter(self) -> None:
        selfhosted = {"sh1", "sh2"}
        # Over the last 1h, two self-hosted jobs run 30m each (=1h busy), plus a
        # GitHub-hosted job that must NOT count toward self-hosted capacity.
        jobs = [
            job("sh1", started_min_ago=40, dur_min=30, rid=1),
            job("sh2", started_min_ago=20, dur_min=20, rid=2),
            job("GitHub Actions 3", started_min_ago=50, dur_min=45, rid=3),
        ]
        win_start = NOW - timedelta(hours=1)
        u = qh.analyze_utilization(jobs, selfhosted, n_runners=2,
                                   window_start=win_start, now=NOW,
                                   lower_bound=False, basis="test")
        # busy = 30m + 20m = 50m; capacity = 2 runners * 60m = 120m.
        self.assertAlmostEqual(u.busy_secs, 50 * 60, delta=1)
        self.assertAlmostEqual(u.capacity_secs, 120 * 60, delta=1)
        self.assertAlmostEqual(u.util_pct, 100.0 * 50 / 120, delta=0.1)
        self.assertEqual(u.n_jobs, 2)  # hosted job excluded

    def test_running_job_counts_up_to_now_and_clips_to_window(self) -> None:
        # A job that started 90m ago and is still running: only the last 60m fall
        # in a 1h window, so it contributes 60m of busy time, not 90m.
        jobs = [job("sh1", started_min_ago=90, dur_min=None, rid=1)]
        win_start = NOW - timedelta(hours=1)
        u = qh.analyze_utilization(jobs, {"sh1"}, n_runners=1,
                                   window_start=win_start, now=NOW,
                                   lower_bound=True, basis="test")
        self.assertAlmostEqual(u.busy_secs, 60 * 60, delta=1)
        self.assertTrue(u.lower_bound)


class PeakConcurrencyTests(unittest.TestCase):
    def test_overlap_counts_touching_does_not(self) -> None:
        selfhosted = {"a", "b", "c"}
        # a: [-50,-30), b: [-40,-20) overlap a on [-40,-30) => peak 2.
        # c: [-20,-10) touches b's end at -20 => NOT concurrent with b.
        jobs = [
            job("a", started_min_ago=50, dur_min=20, rid=1),
            job("b", started_min_ago=40, dur_min=20, rid=2),
            job("c", started_min_ago=20, dur_min=10, rid=3),
        ]
        win_start = NOW - timedelta(hours=2)
        p = qh.analyze_peak_concurrency(jobs, selfhosted, n_runners=3,
                                        window_start=win_start, now=NOW,
                                        lower_bound=False, basis="test")
        self.assertEqual(p.peak, 2)
        self.assertEqual(p.n_intervals, 3)


class RunWindowTests(unittest.TestCase):
    def test_merge_gate_separated_and_window_filter(self) -> None:
        runs = [
            run("CI", "completed", "success", created_min_ago=10, rid=1),
            run("CI", "completed", "failure", created_min_ago=20, rid=2),
            run("CI", "completed", "cancelled", created_min_ago=30, rid=3),
            run("Merge Gate", "completed", "failure", created_min_ago=15, rid=4),
            {**run("x", "completed", "success", rid=5), "event": "merge_group"},
            # Older than the 1h window => excluded from counts.
            run("CI", "completed", "failure", created_min_ago=120, rid=6),
        ]
        rw = qh.analyze_run_window(runs, NOW, window_hours=1.0)
        self.assertEqual(rw.started, 3)       # 3 non-gate CI runs in window
        self.assertEqual(rw.success, 1)
        self.assertEqual(rw.failure, 1)       # gate failure NOT counted here
        self.assertEqual(rw.cancelled, 1)
        self.assertEqual(rw.gate_started, 2)  # Merge Gate + merge_group event
        self.assertEqual(rw.gate_failure, 1)
        # Oldest run (120m) predates the 1h window => coverage is satisfied.
        self.assertTrue(rw.covers_window)

    def test_coverage_warning_when_window_not_spanned(self) -> None:
        runs = [run("CI", "completed", "success", created_min_ago=10, rid=1)]
        rw = qh.analyze_run_window(runs, NOW, window_hours=24.0)
        self.assertFalse(rw.covers_window)  # only 10m of history for a 24h window


class MergeGateClassifyTests(unittest.TestCase):
    def test_markers(self) -> None:
        self.assertTrue(qh._is_merge_gate("Merge Gate", "push"))
        self.assertTrue(qh._is_merge_gate("anything", "merge_group"))
        self.assertFalse(qh._is_merge_gate("Rust", "push"))
        self.assertFalse(qh._is_merge_gate("CI (portable)", "push"))


class GateTests(unittest.TestCase):
    def _patch(self, runs, api):
        self.addCleanup(setattr, qh, "fetch_runs", qh.fetch_runs)
        self.addCleanup(setattr, qh, "fetch_runners", qh.fetch_runners)
        qh.fetch_runs = lambda *a, **k: runs
        qh.fetch_runners = lambda *a, **k: api

    def test_deep_queue_trips_gate(self) -> None:
        runs = [run("portable", "queued", created_min_ago=5, rid=i)
                for i in range(qh.QUEUE_DEPTH_WARN + 2)]
        self._patch(runs, None)
        code, fields = qh.compute_gate("r", "gh", 100, now=NOW)
        self.assertEqual(code, 1)
        self.assertEqual(fields["state"], "red")
        self.assertGreaterEqual(fields["max_queue_depth"], qh.QUEUE_DEPTH_WARN)

    def test_stale_green_trips_gate(self) -> None:
        # Enough runs to clear GREEN_GATE_MIN_RUNS, none green.
        runs = [run("ci", "completed", "failure", rid=i)
                for i in range(qh.GREEN_GATE_MIN_RUNS + 1)]
        self._patch(runs, None)
        code, fields = qh.compute_gate("r", "gh", 100, now=NOW)
        self.assertEqual(code, 1)
        self.assertIn("ci", fields["stale_green"])

    def test_healthy_is_green(self) -> None:
        runs = [run("ci", "completed", "success", created_min_ago=1, rid=1),
                run("ci", "completed", "success", created_min_ago=2, rid=2)]
        self._patch(runs, None)
        code, fields = qh.compute_gate("r", "gh", 100, now=NOW)
        self.assertEqual(code, 0)
        self.assertEqual(fields["state"], "ok")

    def test_regular_gate_persists_green_time_observation(self) -> None:
        calls = []
        original = qh.compute_gate

        def fake(repo, gh_cmd, limit, now=None, sink=None,
                 per_call_timeout=qh.DEFAULT_GH_CALL_TIMEOUT,
                 persist_green_time=False):
            calls.append(persist_green_time)
            return 0, {"state": "ok", "summary": "healthy"}

        qh.compute_gate = fake
        self.addCleanup(setattr, qh, "compute_gate", original)
        self.assertEqual(qh.gate(["r/x"], "gh", 10), 0)
        self.assertEqual(calls, [True])

    def test_green_time_still_logs_when_live_queue_fetch_is_unavailable(self):
        self._patch(None, None)
        calls = []
        original = qh.green_time_field

        def fake(repo, since=None, *, persist=False):
            calls.append(persist)
            return "qualified-green-time"

        qh.green_time_field = fake
        self.addCleanup(setattr, qh, "green_time_field", original)
        code, fields = qh.compute_gate(
            "r", "gh", 100, now=NOW, persist_green_time=True)
        self.assertEqual(code, 1)
        self.assertEqual(fields["state"], "unknown")
        self.assertEqual(fields["green_time"], "qualified-green-time")
        self.assertEqual(calls, [True])


class ClassifyFetchFailureTests(unittest.TestCase):
    def test_403_is_ci_hub_broken_auth(self) -> None:
        klass, _ = qh.classify_gh_failure(
            1, "gh: You must have repository read permissions ... (HTTP 403)",
            None)
        self.assertEqual(klass, qh.FETCH_AUTH)
        self.assertIn(klass, qh.CI_HUB_BROKEN_CLASSES)

    def test_401_is_auth(self) -> None:
        klass, _ = qh.classify_gh_failure(1, "HTTP 401 Unauthorized", None)
        self.assertEqual(klass, qh.FETCH_AUTH)

    def test_timeout_is_upstream(self) -> None:
        exc = __import__("subprocess").TimeoutExpired(cmd="gh", timeout=120)
        klass, _ = qh.classify_gh_failure(None, "", exc)
        self.assertEqual(klass, qh.FETCH_TIMEOUT)
        self.assertIn(klass, qh.UPSTREAM_CLASSES)

    def test_missing_gh_is_tooling(self) -> None:
        klass, _ = qh.classify_gh_failure(None, "", FileNotFoundError())
        self.assertEqual(klass, qh.FETCH_TOOLING)
        self.assertIn(klass, qh.CI_HUB_BROKEN_CLASSES)

    def test_5xx_is_upstream(self) -> None:
        klass, _ = qh.classify_gh_failure(1, "HTTP 503 Service Unavailable", None)
        self.assertEqual(klass, qh.FETCH_UPSTREAM)

    def test_rate_limit_is_upstream(self) -> None:
        klass, _ = qh.classify_gh_failure(
            1, "API rate limit exceeded (HTTP 403)", None)
        # Rate-limit is transient upstream pressure, NOT a token/config error,
        # even though GitHub returns it as a 403.
        self.assertEqual(klass, qh.FETCH_RATELIMIT)
        self.assertIn(klass, qh.UPSTREAM_CLASSES)

    def test_404_is_ci_hub_broken(self) -> None:
        klass, _ = qh.classify_gh_failure(1, "HTTP 404 Not Found", None)
        self.assertEqual(klass, qh.FETCH_NOTFOUND)
        self.assertIn(klass, qh.CI_HUB_BROKEN_CLASSES)


class FetchVerdictTests(unittest.TestCase):
    def _f(self, klass):
        return qh.FetchFailure("r", "run-list", klass, "d")

    def test_clean_is_exit_0(self) -> None:
        code, state, _ = qh.fetch_verdict([])
        self.assertEqual(code, qh.EXIT_OK)
        self.assertEqual(state, "ok")

    def test_upstream_only_is_exit_3(self) -> None:
        code, state, _ = qh.fetch_verdict([self._f(qh.FETCH_TIMEOUT)])
        self.assertEqual(code, qh.EXIT_UPSTREAM_DEGRADED)
        self.assertEqual(state, "upstream-degraded")

    def test_ci_hub_broken_dominates(self) -> None:
        # A mix of a timeout and a 403 must report ci-hub-broken (exit 2): the
        # actionable failure wins over the retryable one.
        code, state, _ = qh.fetch_verdict(
            [self._f(qh.FETCH_TIMEOUT), self._f(qh.FETCH_AUTH)])
        self.assertEqual(code, qh.EXIT_CI_HUB_BROKEN)
        self.assertEqual(state, "ci-hub-broken")

    def test_failure_line_labels_bucket(self) -> None:
        self.assertIn("CI-HUB-BROKEN", self._f(qh.FETCH_AUTH).line())
        self.assertIn("UPSTREAM", self._f(qh.FETCH_TIMEOUT).line())


class SelfHostedSkipTests(unittest.TestCase):
    """A non-administered repo must NOT hit the runners API (the old 403 source)."""

    def _patch(self, runs):
        self.addCleanup(setattr, qh, "fetch_runs", qh.fetch_runs)
        self.addCleanup(setattr, qh, "fetch_runners", qh.fetch_runners)
        self.calls = []
        qh.fetch_runs = lambda *a, **k: runs
        qh.fetch_runners = lambda *a, **k: self.calls.append(a) or None

    def test_non_administered_repo_skips_runner_fetch(self) -> None:
        self._patch([run("CI", "completed", "success", rid=1)])
        # facebookexperimental/hermit is not in SELF_HOSTED_REPOS by default.
        self.assertNotIn("facebookexperimental/hermit", qh.SELF_HOSTED_REPOS)
        qh.report_repo("facebookexperimental/hermit", "gh", 10, 0, 24.0, sink=[])
        self.assertEqual(self.calls, [],
                         "runners API must not be queried on a non-admin repo")

    def test_administered_repo_does_fetch_runners(self) -> None:
        self._patch([run("CI", "completed", "success", rid=1)])
        self.assertIn("rrnewton/hermit", qh.SELF_HOSTED_REPOS)
        qh.report_repo("rrnewton/hermit", "gh", 10, 0, 24.0, sink=[])
        self.assertEqual(len(self.calls), 1)

    def test_run_list_403_surfaces_as_ci_hub_broken(self) -> None:
        # Even on a non-admin repo, a run-list auth failure is a real, visible
        # ci-hub-broken failure — it is the core signal, not the admin-only one.
        self.addCleanup(setattr, qh, "fetch_runs", qh.fetch_runs)

        def failing_runs(repo, gh_cmd, limit, sink=None):
            if sink is not None:
                sink.append(qh.FetchFailure(repo, "run-list", qh.FETCH_AUTH,
                                            "HTTP 403"))
            return None
        qh.fetch_runs = failing_runs
        sink: list = []
        qh.report_repo("facebookexperimental/hermit", "gh", 10, 0, 24.0,
                       sink=sink)
        code, state, _ = qh.fetch_verdict(sink)
        self.assertEqual(code, qh.EXIT_CI_HUB_BROKEN)
        self.assertEqual(state, "ci-hub-broken")


class GreenTimeFieldTests(unittest.TestCase):
    """green_time_field formats the derived integral and degrades honestly."""

    def _with_fake_query(self, fake):
        # green_time_field imports the history query module via
        # _load_history_query; swap it for a fake so the test needs no store.
        orig = qh._load_history_query
        qh._load_history_query = lambda: fake
        self.addCleanup(lambda: setattr(qh, "_load_history_query", orig))

    def test_formats_available_integral(self):
        class Q:
            @staticmethod
            def parent_root():
                return "/nonexistent"

            @staticmethod
            def green_time(parent, repo, since, workflows):
                return {"green_pct": 87.5, "green_hours": 21.0,
                        "authoritative_run_hours": 24.0,
                        "window_start": "2026-08-01T00:00:00Z",
                        "window_end_utc": "2026-08-02T06:00:00Z",
                        "window_hours": 30.0, "no_result_hours": 2.0,
                        "gap_hours": 4.0, "samples": 42,
                        "current_state": "success"}
        self._with_fake_query(Q)
        out = qh.green_time_field("rrnewton/hermit")
        self.assertIn("87.5% green = 21.0 green h / 24.0 authoritative-run h",
                      out)
        self.assertIn("window 2026-08-01T00:00:00Z..2026-08-02T06:00:00Z",
                      out)
        self.assertIn("excluded NO-RESULT 2.0h + gap 4.0h", out)
        self.assertIn("n=42 commits", out)
        self.assertIn("current=success", out)
        self.assertNotIn("UNAVAILABLE", out)

    def test_persist_appends_the_same_window_and_denominator_snapshot(self):
        calls = []

        class Q:
            @staticmethod
            def parent_root():
                return "/parent"

            @staticmethod
            def green_time(parent, repo, since, workflows):
                return {"green_pct": 50.0, "green_hours": 1.0,
                        "authoritative_run_hours": 2.0,
                        "window_start": "s", "window_end_utc": "e",
                        "window_hours": 5.0, "no_result_hours": 3.0,
                        "gap_hours": 0.0, "samples": 5,
                        "current_state": "red"}

            @staticmethod
            def append_green_time_log(parent, snapshot, path):
                calls.append((parent, snapshot, path))
                return "/parent/ignored/ci-hub/green-time-log.jsonl"

        self._with_fake_query(Q)
        out = qh.green_time_field("rrnewton/hermit", persist=True)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], "/parent")
        self.assertEqual(calls[0][1]["authoritative_run_hours"], 2.0)
        self.assertIn("window s..e", out)
        self.assertIn("log=/parent/ignored/ci-hub/green-time-log.jsonl", out)

    def test_thin_store_degrades_to_unavailable(self):
        class Q:
            @staticmethod
            def parent_root():
                return "/nonexistent"

            @staticmethod
            def green_time(parent, repo, since, workflows):
                return {"green_pct": None, "note": "no terminal runs in store"}
        self._with_fake_query(Q)
        out = qh.green_time_field("rrnewton/hermit")
        self.assertTrue(out.startswith("UNAVAILABLE"))
        self.assertIn("no terminal runs in store", out)

    def test_import_failure_never_raises(self):
        def boom():
            raise ImportError("history module missing")
        orig = qh._load_history_query
        qh._load_history_query = boom
        self.addCleanup(lambda: setattr(qh, "_load_history_query", orig))
        out = qh.green_time_field("rrnewton/hermit")
        self.assertTrue(out.startswith("UNAVAILABLE"))

    def test_green_time_raising_is_swallowed(self):
        class Q:
            @staticmethod
            def parent_root():
                return "/nonexistent"

            @staticmethod
            def green_time(parent, repo, since, workflows):
                raise RuntimeError("store corrupt")
        self._with_fake_query(Q)
        out = qh.green_time_field("rrnewton/hermit")
        self.assertTrue(out.startswith("UNAVAILABLE"))
        self.assertIn("store corrupt", out)


if __name__ == "__main__":
    unittest.main()
