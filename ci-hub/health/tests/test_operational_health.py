#!/usr/bin/env python3

from __future__ import annotations

import contextlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import operational_health


def pr_status_unavailable(reason: str) -> Exception:
    """A repo-query failure as pr_status raises it (a RuntimeError subclass)."""
    return operational_health.pr_status.RepoUnavailable(reason)


class OperationalHealthTest(unittest.TestCase):
    def capture(
        self, function: object, *args: object, **kwargs: object
    ) -> tuple[int, str]:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            result = function(*args, **kwargs)  # type: ignore[operator]
        return int(result), output.getvalue()

    def test_hosted_diagnostics_still_run_but_emit_notes(self) -> None:
        config = yaml.safe_load(
            (Path(__file__).resolve().parents[1] / "tick-hub.yaml").read_text()
        )
        reminders = {row["name"]: row for row in config["reminders"]}
        for name in ("github_main_health", "pull_request_health", "ci_queue_health"):
            row = reminders[name]
            self.assertEqual(row["cadence_secs"], 3600)
            self.assertEqual(row["gate"]["when"], "failure")
            self.assertTrue(row["gate"]["capture"])
            self.assertEqual(row["emit"]["kind"], "note")
            self.assertNotIn("skill", row["emit"])

    def test_github_main_red_is_a_warning(self) -> None:
        repos = [SimpleNamespace(repo="example/project", state="red", available=True)]
        with mock.patch.object(
            operational_health.github_main_health,
            "collect_health",
            return_value=repos,
        ), mock.patch.object(
            operational_health.github_main_health,
            "overall_state",
            return_value="red",
        ):
            result, output = self.capture(operational_health.github_main_gate)
        self.assertEqual(result, 1)
        self.assertIn("state=red", output)
        self.assertIn("summary=example/project:red", output)

    def test_github_main_pending_is_not_a_hard_warning(self) -> None:
        repos = [
            SimpleNamespace(repo="example/project", state="pending", available=True)
        ]
        with mock.patch.object(
            operational_health.github_main_health,
            "collect_health",
            return_value=repos,
        ), mock.patch.object(
            operational_health.github_main_health,
            "overall_state",
            return_value="pending",
        ):
            result, output = self.capture(operational_health.github_main_gate)
        self.assertEqual(result, 0)
        self.assertIn("state=pending", output)

    def test_pull_request_red_count_is_a_warning(self) -> None:
        statuses = [
            SimpleNamespace(
                repo="a/one",
                all_open=103,
                population_age_secs=30.0,
                open=3,
                red=1,
                green=1,
                pending=1,
                real_reds=1,
                outage_suspected=False,
            ),
            SimpleNamespace(
                repo="b/two",
                all_open=7,
                population_age_secs=45.0,
                open=0,
                red=0,
                green=0,
                pending=0,
                real_reds=0,
                outage_suspected=False,
            ),
        ]
        with mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            side_effect=statuses,
        ):
            result, output = self.capture(operational_health.pull_request_gate)
        self.assertEqual(result, 1)
        self.assertIn("total=110", output)
        self.assertIn("total_scope=cached-all-open-prs-across-2/2-configured-repos", output)
        self.assertIn("census_ttl_secs=900", output)
        self.assertIn("population_repo_coverage=2/2", output)
        self.assertIn("census_stale=no", output)
        self.assertIn("ready_non_draft=3", output)
        self.assertIn("rrnewton/hermit:all_open=103,ready_non_draft=3", output)
        self.assertIn("rrnewton/reverie:all_open=7,ready_non_draft=0", output)
        self.assertIn("red=1", output)
        self.assertIn("pending=1", output)

    def test_fresh_and_stale_census_verdicts_bracket_same_count_consumer(self) -> None:
        def status(repo: str, count: int, age: float) -> SimpleNamespace:
            return SimpleNamespace(
                repo=repo,
                all_open=count,
                population_age_secs=age,
                open=0,
                red=0,
                green=0,
                pending=0,
                real_reds=0,
                outage_suspected=False,
            )

        repos = ["rrnewton/hermit", "rrnewton/reverie", "rrnewton/agent-utils"]
        fresh_statuses = [
            status(repos[0], 83, 30.0),
            status(repos[1], 7, 20.0),
            status(repos[2], 5, 10.0),
        ]
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", repos
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            side_effect=fresh_statuses,
        ):
            fresh_result, fresh_output = self.capture(
                operational_health.pull_request_gate
            )

        self.assertEqual(fresh_result, 0)
        self.assertIn("state=ok", fresh_output)
        self.assertIn("total=95", fresh_output)
        self.assertIn("summary=all_open=95 across 3/3", fresh_output)
        self.assertIn("census_stale=no", fresh_output)
        self.assertNotIn("STALE", fresh_output)
        self.assertNotIn("AS-OF", fresh_output)

        aged_statuses = [
            status(repos[0], 135, 13806.4),
            status(repos[1], 7, 13800.0),
            status(repos[2], 5, 13790.0),
        ]
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", repos
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            side_effect=aged_statuses,
        ):
            stale_result, stale_output = self.capture(
                operational_health.pull_request_gate
            )

        self.assertEqual(stale_result, 1)
        self.assertIn("state=degraded", stale_output)
        self.assertIn("total=147", stale_output)
        self.assertIn(
            "summary=all_open=147 AS-OF UP TO 13806.4s AGO "
            "(STALE; freshness_bound_s=900) across 3/3",
            stale_output,
        )
        self.assertIn("census_stale=yes", stale_output)
        self.assertIn(
            "stale_census_repos=rrnewton/hermit|rrnewton/reverie|rrnewton/agent-utils",
            stale_output,
        )
        self.assertNotIn("PARTIAL SAMPLE", stale_output)
        self.assertIn(
            "STALE CENSUS REPOS=rrnewton/hermit|rrnewton/reverie|rrnewton/agent-utils",
            stale_output,
        )
        # The tick title interpolates this exact summary.  Prove that its
        # consumer cannot recover the old present-tense reading.
        config = yaml.safe_load(
            (Path(__file__).resolve().parents[1] / "tick-hub.yaml").read_text()
        )
        reminder = next(
            row
            for row in config["reminders"]
            if row["name"] == "pull_request_health"
        )
        fields = dict(
            line.split("=", 1)
            for line in stale_output.splitlines()
            if "=" in line
        )
        title = reminder["emit"]["title"].format(**fields)
        self.assertIn("all_open=147 AS-OF UP TO 13806.4s AGO (STALE", title)
        self.assertNotIn("all_open=147 across", title)

    def test_pull_request_gate_exposes_setup_only_no_result(self) -> None:
        status = SimpleNamespace(
            open=1,
            red=0,
            green=0,
            pending=1,
            real_reds=0,
            setup_only_no_result_checks=1,
            outage_suspected=False,
        )
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", ["a/one"]
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            return_value=status,
        ):
            result, output = self.capture(operational_health.pull_request_gate)
        self.assertEqual(result, 0)
        self.assertIn("state=ok", output)
        self.assertIn("setup_only_no_result_checks=1", output)
        self.assertIn("setup_only_no_result=1", output)

    def test_pull_request_gate_exposes_prerequisite_no_result(self) -> None:
        status = SimpleNamespace(
            open=1,
            red=0,
            green=0,
            pending=1,
            real_reds=0,
            setup_only_no_result_checks=1,
            prerequisite_no_result_checks=1,
            outage_suspected=False,
        )
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", ["a/one"]
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            return_value=status,
        ):
            result, output = self.capture(operational_health.pull_request_gate)
        self.assertEqual(result, 0)
        self.assertIn("prerequisite_no_result_checks=1", output)
        self.assertIn("prerequisite_no_result=1", output)

    def test_pull_request_one_repo_unavailable_is_degraded_not_lost(self) -> None:
        # A slow/blocked repo must not discard the other repo's real data: the
        # answer is DEGRADED (partial), distinct from "PRs are red".
        healthy = SimpleNamespace(
            all_open=12, open=2, red=0, green=2, pending=0,
            real_reds=0, outage_suspected=False
        )
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", ["a/one", "b/two"]
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            side_effect=[healthy, pr_status_unavailable("b/two blocked")],
        ):
            result, output = self.capture(operational_health.pull_request_gate)
        self.assertEqual(result, 1)
        self.assertIn("state=degraded", output)
        self.assertIn("degraded=yes", output)
        self.assertIn("total=12", output)  # the healthy repo survived
        self.assertIn("repo_coverage=1/2", output)
        self.assertIn("population_repo_coverage=1/2", output)
        self.assertIn("PARTIAL SAMPLE, NOT A FLEET CENSUS", output)
        self.assertIn("unavailable=b/two", output)

    def test_ledger_unavailable_keeps_population_and_marks_verdicts_unknown(
        self,
    ) -> None:
        population_only = [
            SimpleNamespace(
                repo="rrnewton/hermit",
                available=False,
                reason="local validation ledger unavailable",
                all_open=23,
                open=11,
                drafts=12,
                population_age_secs=2.0,
            ),
            SimpleNamespace(
                repo="rrnewton/reverie",
                available=False,
                reason="local validation ledger unavailable",
                all_open=8,
                open=3,
                drafts=5,
                population_age_secs=3.0,
            ),
        ]
        with mock.patch.object(
            operational_health.pr_status,
            "DEFAULT_REPOS",
            ["rrnewton/hermit", "rrnewton/reverie"],
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            side_effect=population_only,
        ):
            result, output = self.capture(operational_health.pull_request_gate)

        self.assertEqual(result, 1)
        self.assertIn("state=degraded", output)
        self.assertIn("total=31", output)
        self.assertIn("population_repo_coverage=2/2", output)
        self.assertIn("repo_coverage=0/2", output)
        self.assertIn("ready_non_draft=14", output)
        self.assertIn("green=unknown", output)
        self.assertIn("red=unknown", output)
        self.assertIn("pending=unknown", output)
        self.assertIn("POPULATION COMPLETE; VERDICTS UNAVAILABLE OR PARTIAL", output)

    def test_expired_census_refresh_failure_is_unavailable_not_last_value(self) -> None:
        status = SimpleNamespace(
            repo="a/one",
            all_open=None,
            open=2,
            red=0,
            green=2,
            pending=0,
            real_reds=0,
            outage_suspected=False,
            population_age_secs=None,
            population_error="expired census could not refresh",
        )
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", ["a/one"]
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            return_value=status,
        ):
            result, output = self.capture(operational_health.pull_request_gate)
        self.assertEqual(result, 1)
        self.assertIn("state=degraded", output)
        self.assertIn("total=unknown", output)
        self.assertIn("population_repo_coverage=0/1", output)
        self.assertIn("a/one:all_open=unknown,ready_non_draft=2", output)
        self.assertIn("census_max_age_secs=unknown", output)
        self.assertIn("population_unknown=a/one", output)
        self.assertNotIn("all_open=177", output)

    def test_pull_request_all_repos_unavailable_is_not_zero_prs(self) -> None:
        # Every repo failing is "ci-hub/GitHub unavailable", never "no open PRs".
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", ["a/one", "b/two"]
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            side_effect=[
                pr_status_unavailable("a/one blocked"),
                pr_status_unavailable("b/two blocked"),
            ],
        ):
            result, output = self.capture(operational_health.pull_request_gate)
        self.assertEqual(result, 1)
        self.assertIn("state=unavailable", output)
        self.assertIn("population_repo_coverage=0/2", output)
        self.assertIn("this is not zero open PRs", output)
        self.assertNotIn("state=ok", output)

    def test_pull_request_gate_bounds_each_repo_under_the_guillotine(self) -> None:
        # The gate MUST bound each repo so it resolves before tick-hub's 30s
        # SubprocessGateRunner kills it (which would erase the reason).
        #
        # The bound is the SUM over the serial pass, not per-repo * repo count.
        # This assertion used to read `timeout * 2 <= 30`, which silently encoded
        # a FLAT budget -- and a flat budget is what starved the large repo. The
        # invariant being protected (the pass finishes before the guillotine) is
        # unchanged; only the arithmetic that expresses it is corrected.
        healthy = SimpleNamespace(
            open=0, red=0, green=0, pending=0, real_reds=0, outage_suspected=False
        )
        repos = ["a/one", "b/two", "c/three"]
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", repos
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            return_value=healthy,
        ) as fetch:
            self.capture(operational_health.pull_request_gate)
        budgets = [call.kwargs["timeout"] for call in fetch.call_args_list]
        self.assertEqual(len(repos), len(budgets))
        for budget in budgets:
            self.assertGreaterEqual(budget, operational_health.PR_GATE_MIN_REPO_BUDGET_SECS)
        # Every repo returns instantly here, so each sees a near-full envelope;
        # the guarantee is that no single grant can exceed the whole envelope and
        # that the envelope itself clears the guillotine.
        self.assertLessEqual(max(budgets), operational_health.PR_GATE_TOTAL_BUDGET_SECS)
        self.assertLess(operational_health.PR_GATE_TOTAL_BUDGET_SECS, 30.0)

    def test_the_large_repo_is_not_starved_by_a_flat_budget(self) -> None:
        """The defect this allocation exists to remove.

        Measured 2026-08-08: hermit's PR-list query costs 11.1-13.1s against the
        old flat 12s, so it straddled its budget and the tick reported reverie's
        3 PRs alone as though the queue were nearly drained. The first repo must
        now receive materially more than that flat number.
        """

        first = operational_health.pr_gate_repo_budget(0.0, 1)
        self.assertGreaterEqual(first, 20.0)
        self.assertGreater(first, 12.0)  # strictly better than the old flat value

    def test_a_slow_first_repo_cannot_starve_a_later_one(self) -> None:
        # The floor is reserved while the envelope still has time left.
        after_a_greedy_first_repo = operational_health.pr_gate_repo_budget(20.0, 0)
        self.assertEqual(
            operational_health.PR_GATE_MIN_REPO_BUDGET_SECS,
            after_a_greedy_first_repo,
        )
        # Once the shared envelope is gone, no later repo receives a fresh
        # allowance.  Returning the floor here was the deadline escape.
        self.assertEqual(
            0.0,
            operational_health.pr_gate_repo_budget(999.0, 0),
        )

    def test_serial_pass_total_stays_under_the_guillotine(self) -> None:
        # Simulate the worst case: every repo consumes exactly what it is granted.
        for repo_count in (1, 2, 3, 5):
            elapsed = 0.0
            for index in range(repo_count):
                elapsed += operational_health.pr_gate_repo_budget(
                    elapsed, repo_count - index - 1
                )
            self.assertLessEqual(
                elapsed,
                operational_health.PR_GATE_TOTAL_BUDGET_SECS,
                f"{repo_count} repos overran the shared envelope",
            )
            self.assertLess(elapsed, 30.0)

    def test_exhausted_envelope_does_not_reopen_a_repo_floor(self) -> None:
        self.assertEqual(
            0.0,
            operational_health.pr_gate_repo_budget(
                operational_health.PR_GATE_TOTAL_BUDGET_SECS, 0
            ),
        )

    def test_budget_fix_does_not_buy_reachability_by_dropping_the_honesty(self) -> None:
        """A repo that STILL exceeds its (now larger) budget must stay loud.

        The flat-budget defect was only findable because the gate reported
        `degraded=yes` and NAMED the unreachable repo instead of presenting a
        clean count from the repos it could reach. Widening the budget must not
        quietly remove that.
        """

        healthy = SimpleNamespace(
            open=2, red=0, green=2, pending=0, real_reds=0, outage_suspected=False
        )
        with mock.patch.object(
            operational_health.pr_status, "DEFAULT_REPOS", ["small/one", "big/two"]
        ), mock.patch.object(
            operational_health.pr_status,
            "fetch_repo_status",
            side_effect=[healthy, pr_status_unavailable("big/two exceeded 20s")],
        ):
            result, output = self.capture(operational_health.pull_request_gate)
        self.assertEqual(result, 1)
        self.assertIn("state=degraded", output)
        self.assertIn("degraded=yes", output)
        self.assertIn("unavailable=big/two", output)
        self.assertIn("ready_non_draft=2", output)  # reachable data survives
        self.assertNotIn("state=ok", output)

    def test_primary_snapshot_failure_is_a_structured_warning(self) -> None:
        def blocked(*_args: object, **kwargs: object) -> int:
            kwargs["err"].write("dirty Hermit primary\n")  # type: ignore[attr-defined]
            return 1

        with mock.patch.object(
            operational_health.primary_checkout,
            "checkout_fresh",
            side_effect=blocked,
        ):
            result, output = self.capture(operational_health.primary_snapshot_gate)
        self.assertEqual(result, 1)
        self.assertIn("state=blocked", output)
        self.assertIn("summary=dirty Hermit primary", output)

    def test_primary_snapshot_preserves_exception_disclosure_and_later_error(self) -> None:
        def blocked(*_args: object, **kwargs: object) -> int:
            kwargs["out"].write(  # type: ignore[attr-defined]
                "KNOWN DIRTY EXCEPTION APPLIED: product=hermit path=validate; NOT CLEAN\n"
            )
            kwargs["err"].write("parent publication refused\n")  # type: ignore[attr-defined]
            return 1

        with mock.patch.object(
            operational_health.primary_checkout,
            "checkout_fresh",
            side_effect=blocked,
        ):
            result, output = self.capture(operational_health.primary_snapshot_gate)
        self.assertEqual(result, 1)
        self.assertIn("path=validate", output)
        self.assertIn("NOT CLEAN", output)
        self.assertIn("parent publication refused", output)

    def test_broken_and_silent_active_agents_are_stuck(self) -> None:
        agents = [
            {"name": "broken", "status": "crashed", "last_activity": 9_999},
            {"name": "silent", "status": "working", "last_activity": 1_000},
            {"name": "fresh", "status": "working", "last_activity": 9_900},
            {"name": "idle", "status": "idle", "last_activity": 1_000},
        ]
        stuck = operational_health.classify_stuck_agents(
            agents,
            now=10_000,
            stuck_after_secs=3_600,
        )
        self.assertEqual(
            stuck,
            [("broken", "crashed"), ("silent", "working-silent-150m")],
        )

    def test_millisecond_activity_timestamp_is_normalized(self) -> None:
        agents = [
            {
                "name": "fresh",
                "status": "running",
                "last_activity": 1_784_999_900_000,
            }
        ]
        self.assertEqual(
            operational_health.classify_stuck_agents(
                agents,
                now=1_785_000_000,
                stuck_after_secs=3_600,
            ),
            [],
        )

    def test_agent_gate_requires_a_snapshot(self) -> None:
        with mock.patch.dict("os.environ", {}, clear=True):
            result, output = self.capture(operational_health.agent_gate)
        self.assertEqual(result, 1)
        self.assertIn("state=unknown", output)
        self.assertIn("ORC-agent-snapshot-missing", output)

    def test_agent_gate_emits_stuck_names(self) -> None:
        snapshot = '[{"name":"worker","status":"failed"}]'
        result, output = self.capture(
            operational_health.agent_gate,
            snapshot,
            now=10_000,
        )
        self.assertEqual(result, 1)
        self.assertIn("state=stuck", output)
        self.assertIn("names=worker", output)

    def test_active_work_reconciles_all_five_classes(self) -> None:
        task = operational_health.TaskRecord
        agent = operational_health.AgentRecord
        report = operational_health.reconcile_active_work(
            [
                task("active", "Active", "alice", ()),
                task("await", "Awaiting", "alice", ("implemented",)),
                task("stale", "Stale", "", ()),
                task("orphan", "Orphan", "retired-worker", ()),
                task("mismatch", "Mismatch", "bob", ()),
            ],
            [
                agent("alice", "busy", "active"),
                agent("bob", "working", "different-task"),
                agent("offbook", "running", None),
                agent("retired-worker", "retired", "orphan"),
            ],
        )
        self.assertEqual(report.counts()["in_progress"], 5)
        self.assertEqual([item.id for item in report.claimed], ["active"])
        self.assertEqual([item.id for item in report.actually_active], ["active"])
        self.assertEqual([item.id for item in report.awaiting_land], ["await"])
        self.assertEqual([item.id for item in report.stale], ["stale"])
        self.assertEqual([item.id for item in report.orphaned], ["orphan"])
        self.assertEqual([item.name for item in report.off_book], ["offbook"])
        self.assertEqual(len(report.misrouted), 2)
        self.assertGreater(report.actionable_count, 0)

    def test_claim_survives_waiting_but_expires_when_assignment_moves(self) -> None:
        task = operational_health.TaskRecord
        agent = operational_health.AgentRecord

        waiting = operational_health.reconcile_active_work(
            [task("claimed", "Claimed", "worker", ())],
            [agent("worker", "idle", "claimed")],
        )
        self.assertEqual([item.id for item in waiting.claimed], ["claimed"])
        self.assertEqual(waiting.actually_active, ())

        moved = operational_health.reconcile_active_work(
            [
                task("claimed", "Claimed", "worker", ()),
                task("other", "Other", "", ()),
            ],
            [agent("worker", "busy", "other")],
        )
        self.assertEqual(moved.claimed, ())
        self.assertIn(
            operational_health.Misroute(
                "worker",
                "claimed",
                "owner-status=busy,owner-current-task=other",
            ),
            moved.misrouted,
        )

    def test_terminal_owner_does_not_retain_a_claim(self) -> None:
        report = operational_health.reconcile_active_work(
            [
                operational_health.TaskRecord(
                    "claimed", "Claimed", "retired-worker", ()
                )
            ],
            [
                operational_health.AgentRecord(
                    "retired-worker", "retired", "claimed"
                )
            ],
        )
        self.assertEqual(report.claimed, ())
        self.assertEqual([item.id for item in report.orphaned], ["claimed"])

    def test_active_work_resolves_thirteen_orc_titles_without_false_pairs(
        self,
    ) -> None:
        tasks = []
        agents = []
        for index in range(13):
            task_id = f"healthy_task_{index:02d}"
            title = (
                "Healthy short task"
                if index == 0
                else f"Healthy task {index:02d} -- a title longer than forty characters"
            )
            owner = f"worker-{index:02d}"
            tasks.append(operational_health.TaskRecord(task_id, title, owner, ()))
            agents.append(
                operational_health.AgentRecord(
                    owner,
                    "busy",
                    title if len(title) <= 40 else f"{title[:37]}...",
                )
            )

        report = operational_health.reconcile_active_work(tasks, agents)

        # The old two-way local_id/title comparison emitted 2 rows per agent.
        self.assertEqual(report.counts()["actually_active"], 13)
        self.assertEqual(report.counts()["misrouted"], 0)

    def test_active_work_still_reports_a_real_title_resolved_misroute(self) -> None:
        expected_title = "Expected task whose title is longer than forty characters"
        actual_title = "Different task whose title is longer than forty characters"
        report = operational_health.reconcile_active_work(
            [
                operational_health.TaskRecord(
                    "expected_task", expected_title, "worker", ()
                ),
                operational_health.TaskRecord("actual_task", actual_title, "", ()),
            ],
            [
                operational_health.AgentRecord(
                    "worker",
                    "busy",
                    f"{actual_title[:37]}...",
                ),
            ],
        )
        self.assertEqual(report.actually_active, ())
        self.assertEqual(report.counts()["misrouted"], 2)
        self.assertEqual({item.agent for item in report.misrouted}, {"worker"})
        self.assertIn(
            operational_health.Misroute(
                "worker",
                "expected_task",
                "owner-status=busy,owner-current-task=actual_task",
            ),
            report.misrouted,
        )
        self.assertIn(
            operational_health.Misroute(
                "worker",
                "actual_task",
                "task-owner=none",
            ),
            report.misrouted,
        )

    def test_active_work_refuses_an_ambiguous_title_prefix(self) -> None:
        shared_prefix = "A title prefix shared by two active tasks"
        display_title = f"{shared_prefix[:37]}..."
        report = operational_health.reconcile_active_work(
            [
                operational_health.TaskRecord(
                    "first", f"{shared_prefix} first", "worker", ()
                ),
                operational_health.TaskRecord(
                    "second", f"{shared_prefix} second", "other", ()
                ),
            ],
            [
                operational_health.AgentRecord("worker", "busy", display_title),
                operational_health.AgentRecord("other", "idle", None),
            ],
        )
        self.assertEqual(report.actually_active, ())
        self.assertIn(
            operational_health.Misroute(
                "worker",
                display_title,
                "current-task-title-is-ambiguous",
            ),
            report.misrouted,
        )

    def test_awaiting_land_alone_is_not_actionable(self) -> None:
        report = operational_health.reconcile_active_work(
            [
                operational_health.TaskRecord(
                    "await",
                    "Awaiting",
                    "worker",
                    ("implemented",),
                )
            ],
            [operational_health.AgentRecord("worker", "idle", None)],
        )
        self.assertEqual(report.actionable_count, 0)
        self.assertEqual(report.counts()["awaiting_land"], 1)
        self.assertEqual(report.counts()["actually_active"], 0)

    def test_agent_snapshot_cache_is_freshness_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "agents.json"
            # Carries a pane id because the cached-read path now requires one:
            # this test round-trips through the CACHE, and a cached entry with
            # no `%NN` identity is refused as implausible. The subject of this
            # test is freshness bounding, which the assertions below still
            # exercise unchanged.
            agents, captured_at = operational_health.load_agent_snapshot(
                '[{"name":"worker","status":"busy","current_task":"task",'
                '"tmux_pane_id":"%1"}]',
                snapshot_file=path,
                now=1000,
            )
            self.assertEqual(agents[0].current_task, "task")
            self.assertEqual(captured_at, 1000)
            cached, _ = operational_health.load_agent_snapshot(
                None,
                snapshot_file=path,
                max_age_secs=60,
                now=1050,
            )
            self.assertEqual(cached, agents)
            with self.assertRaisesRegex(RuntimeError, "agent-snapshot-stale"):
                operational_health.load_agent_snapshot(
                    None,
                    snapshot_file=path,
                    max_age_secs=60,
                    now=1061,
                )

    def test_cache_agent_snapshot_validates_before_replacing_the_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "agents.json"
            path.write_text("last usable observation\n")
            result, output = self.capture(
                operational_health.cache_agent_snapshot,
                '[{"name":"worker","status":"busy","current_task":"task"}]',
                snapshot_file=path,
                now=1000,
            )
            self.assertEqual(result, 1)
            self.assertIn("no-pane-bound-agents", output)
            self.assertEqual(path.read_text(), "last usable observation\n")

    def test_cache_agent_snapshot_refuses_unknown_status_before_replacing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "agents.json"
            path.write_text("last usable observation\n")
            result, output = self.capture(
                operational_health.cache_agent_snapshot,
                '[{"name":"worker","status":"future-state",'
                '"tmux_pane_id":"%7"}]',
                snapshot_file=path,
                now=1000,
            )
            self.assertEqual(result, 1)
            self.assertIn("future-state", output)
            self.assertEqual(path.read_text(), "last usable observation\n")

    def test_cache_agent_snapshot_persists_a_plausible_full_snapshot(self) -> None:
        snapshot = (
            '[{"name":"coordinator","status":"managed","current_task":null,'
            '"tmux_pane_id":null},'
            '{"name":"worker","status":"busy","current_task":"task",'
            '"tmux_pane_id":"%7"}]'
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "agents.json"
            result, output = self.capture(
                operational_health.cache_agent_snapshot,
                snapshot,
                snapshot_file=path,
                now=1000,
            )
            written = path.read_text()
            envelope = json.loads(written)
        self.assertEqual(result, 0)
        self.assertIn("state=ok", output)
        self.assertEqual(envelope["captured_at"], 1000)
        self.assertEqual(envelope["agents"][1]["current_task"], "task")
        self.assertEqual(
            written,
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": 1000,
                    "agents": json.loads(snapshot),
                },
                sort_keys=True,
            )
            + "\n",
            "shared validation must not rewrite the existing snapshot bytes",
        )

    def test_cache_agents_command_dispatches_to_the_snapshot_writer(self) -> None:
        with mock.patch.object(
            operational_health, "cache_agent_snapshot", return_value=7
        ) as cache:
            result = operational_health.main(["cache-agents"])
        self.assertEqual(result, 7)
        cache.assert_called_once_with()

    def test_taskgraph_query_parses_exact_tag_array(self) -> None:
        output = """task_json
---------------
{"id":"one","title":"One","owner":"worker","tags":["implemented"]}

(1 rows)
"""
        completed = SimpleNamespace(returncode=0, stdout=output, stderr="")
        # The resolver must be stubbed as well as subprocess.run. `resolve()` is
        # called while BUILDING run's `env=` argument, so it executes even though
        # run itself is mocked -- and with no TG_DB_PATH bound it raises
        # TaskGraphUnavailable before the parser under test is ever reached. That
        # is why this passed on an agent box, where a graph is always bound, and
        # failed on hosted CI, where none is. Verified by bracketing rather than
        # inferred from the message: bound it passes, `env -u TG_DB_PATH`
        # reproduces CI's exact RuntimeError.
        #
        # This is a PARSER test. Binding a real graph would make it depend on that
        # graph's contents; stubbing the resolver keeps it testing exactly what it
        # names. The resolver's own refusal is not weakened here -- it keeps its
        # coverage in ci-hub/health/tests/test_orphaned_task_detector_fails_closed.py,
        # where an unresolvable graph must be COULD NOT MEASURE and never a clean zero.
        with mock.patch.object(
            operational_health.taskgraph_db, "resolve", return_value=None
        ), mock.patch.object(
            operational_health.taskgraph_db, "child_env", return_value={}
        ), mock.patch.object(
            operational_health.subprocess,
            "run",
            return_value=completed,
        ):
            tasks = operational_health._taskgraph_in_progress()
        self.assertEqual(len(tasks), 1)
        self.assertTrue(tasks[0].implemented)

    def test_task_reference_hygiene_refuses_a_malformed_task_row_by_name(self) -> None:
        completed = SimpleNamespace(
            returncode=0,
            stdout="task_json\n---------\n{not-json\n\n(1 rows)\n",
            stderr="",
        )
        with mock.patch.object(
            operational_health.taskgraph_db, "resolve", return_value=None
        ), mock.patch.object(
            operational_health.taskgraph_db, "child_env", return_value={}
        ), mock.patch.object(
            operational_health.subprocess, "run", return_value=completed
        ):
            result, output = self.capture(
                operational_health.task_reference_hygiene_gate
            )
        self.assertEqual(result, 1)
        self.assertIn("state=error", output)
        self.assertIn("taskgraph-returned-invalid-json", output)
        self.assertIn("open_tasks=unknown", output)

    def test_task_reference_hygiene_refuses_a_missing_task_row_by_count(self) -> None:
        completed = SimpleNamespace(
            returncode=0,
            stdout="task_json\n---------\n\n(1 rows)\n",
            stderr="",
        )
        with mock.patch.object(
            operational_health.taskgraph_db, "resolve", return_value=None
        ), mock.patch.object(
            operational_health.taskgraph_db, "child_env", return_value={}
        ), mock.patch.object(
            operational_health.subprocess, "run", return_value=completed
        ):
            result, output = self.capture(
                operational_health.task_reference_hygiene_gate
            )
        self.assertEqual(result, 1)
        self.assertIn("state=error", output)
        self.assertIn("taskgraph-row-count-mismatch:parsed=0,reported=1", output)
        self.assertIn("open_tasks=unknown", output)

    def test_task_reference_hygiene_accepts_a_reported_empty_population(self) -> None:
        completed = SimpleNamespace(
            returncode=0,
            stdout="task_json\n---------\n\n(0 rows)\n",
            stderr="",
        )
        with mock.patch.object(
            operational_health.taskgraph_db, "resolve", return_value=None
        ), mock.patch.object(
            operational_health.taskgraph_db, "child_env", return_value={}
        ), mock.patch.object(
            operational_health.subprocess, "run", return_value=completed
        ):
            result, output = self.capture(
                operational_health.task_reference_hygiene_gate
            )
        self.assertEqual(result, 0)
        self.assertIn("state=clean", output)
        self.assertIn("open_tasks=0", output)

    def test_no_explicit_snapshot_uses_a_fresh_full_cache(self) -> None:
        tasks = (operational_health.TaskRecord("held", "Held", "worker", ()),)
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            operational_health, "_taskgraph_in_progress", return_value=tasks
        ), mock.patch.object(
            operational_health,
            "live_agent_names",
            side_effect=AssertionError("a fresh full snapshot must not go name-only"),
        ):
            path = Path(directory) / "agents.json"
            operational_health._persist_agent_snapshot(
                path,
                [
                    {
                        "name": "worker",
                        "status": "busy",
                        "current_task": "held",
                        "tmux_pane_id": "%7",
                    }
                ],
                captured_at=950,
            )
            result, output = self.capture(
                operational_health.active_work_gate,
                snapshot_file=path,
                json_output=True,
                now=1000,
            )
        payload = json.loads(output)
        self.assertNotIn("census_mode", payload)
        self.assertEqual([item["id"] for item in payload["claimed"]], ["held"])
        self.assertEqual([item["id"] for item in payload["actually_active"]], ["held"])
        self.assertEqual(result, 0)

    def test_an_explicit_snapshot_still_takes_the_full_census_path(self) -> None:
        """The escape hatch: real ORC data, supplied deliberately, still works.

        Deleting the cache must not delete the ability to answer the full census
        when someone can actually provide `status` and `current_task`.
        """
        tasks = (operational_health.TaskRecord("active", "Active", "worker", ()),)
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            operational_health, "_taskgraph_in_progress", return_value=tasks
        ), mock.patch.object(
            operational_health,
            "live_agent_names",
            side_effect=AssertionError("explicit snapshot must not go live"),
        ):
            result, output = self.capture(
                operational_health.active_work_gate,
                snapshot='[{"name":"worker","status":"busy","current_task":"active"}]',
                snapshot_file=Path(directory) / "agents.json",
                json_output=True,
                now=1000,
            )
        payload = json.loads(output)
        # No `census_mode` key at all is the marker of the FULL census path; the
        # degraded path always sets it.
        self.assertNotIn("census_mode", payload)
        # A clean full census exits 0 -- one busy agent whose current_task resolves
        # to the one owned task is `actually_active`, not drift. Asserting 1 here
        # was my error, caught by running it: rc=0 is the correct answer and is also
        # what distinguishes this path from the degraded one, which never returns 0.
        self.assertEqual(result, 0)

    def test_a_stale_snapshot_still_expires_claims_from_a_live_name_read(self) -> None:
        """⚠️ THE AUTHORITY MUST NOT GO DARK WHEN ITS CACHE ROTS.

        Measured 2026-08-26: `ignored/ci-hub/agent-snapshot.json` was 356283s --
        4.12 days -- old against a 600s window, so `load_agent_snapshot` raised and
        `active-work` returned `state=unknown` for EVERY query. CLAUDE.md names this
        the machine-readable authority for claim expiry, so no claim on the board
        could be expired by the documented mechanism however dead its owner.
        """
        tasks = (
            operational_health.TaskRecord("held-by-dead", "Held", "ghost", ()),
            operational_health.TaskRecord("held-by-live", "Held", "worker", ()),
        )
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            operational_health, "_taskgraph_in_progress", return_value=tasks
        ), mock.patch.object(
            operational_health, "live_agent_names", return_value=("worker",)
        ):
            result, output = self.capture(
                operational_health.active_work_gate,
                snapshot_file=Path(directory) / "absent.json",
                json_output=True,
                now=1000,
            )
        payload = json.loads(output)
        self.assertEqual(payload["state"], "degraded")
        self.assertEqual(payload["census_mode"], "live-names-only")
        # The dead owner's claim is reclaimable; the live owner's is NOT.
        self.assertEqual(payload["orphaned"], ["held-by-dead"])
        self.assertEqual(payload["live_agents_examined"], 1)
        # ⚠️ AND IT MUST REFUSE THE QUESTIONS IT CANNOT ANSWER BY NAME. A name-only
        # read carries no `current_task`, so computing `claimed`/`misrouted` from it
        # would mark every LIVE agent's claim misrouted. Naming them beats guessing.
        self.assertEqual(
            payload["not_determined"], ["claimed", "actually_active", "misrouted"]
        )
        # Non-zero: a degraded answer to one of three questions is not a clean census.
        self.assertEqual(result, 1)

    def test_live_agent_names_returns_none_not_empty_when_the_read_fails(self) -> None:
        """⚠️ PINS THE HELPER, NOT ONLY ITS CALLER, AND THAT DISTINCTION HAS TEETH.

        The sibling control mocks `live_agent_names`, so it pins what the CALLER
        does with `None` and says nothing about what the helper RETURNS. Mutating
        the helper's failure path from `None` to `()` left that control green --
        caught by running the mutation rather than reasoning about it. An empty
        tuple is the dangerous value: it makes every owned task's owner "absent
        from the live fleet", authorising reclamation of a LIVE agent's work.
        """
        with mock.patch(
            "importlib.util.spec_from_file_location", side_effect=OSError("boom")
        ):
            self.assertIsNone(operational_health.live_agent_names())

    def test_a_failed_live_read_keeps_the_hard_refusal_and_expires_nothing(
        self,
    ) -> None:
        """⚠️ THE CONTROL, AND IT GUARDS THE DANGEROUS DIRECTION.

        An empty fleet would make every owned task look orphaned -- i.e. authorise
        reclaiming a LIVE agent's work. `live_agent_names` returns None rather than
        `()` when the read fails, and this pins that the caller then keeps the
        original refusal instead of inventing a zero-agent census.
        """
        tasks = (operational_health.TaskRecord("held", "Held", "worker", ()),)
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            operational_health, "_taskgraph_in_progress", return_value=tasks
        ), mock.patch.object(
            operational_health, "live_agent_names", return_value=None
        ):
            result, output = self.capture(
                operational_health.active_work_gate,
                snapshot_file=Path(directory) / "absent.json",
                json_output=True,
                now=1000,
            )
        payload = json.loads(output)
        self.assertEqual(payload["state"], "unknown")
        self.assertNotIn("orphaned", payload)
        self.assertEqual(result, 2)

    def test_active_work_gate_emits_counts_and_items(self) -> None:
        tasks = (
            operational_health.TaskRecord("active", "Active", "worker", ()),
            operational_health.TaskRecord("stale", "Stale", "", ()),
        )
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            operational_health,
            "_taskgraph_in_progress",
            return_value=tasks,
        ):
            result, output = self.capture(
                operational_health.active_work_gate,
                snapshot=(
                    '[{"name":"worker","status":"busy",' '"current_task":"active"}]'
                ),
                snapshot_file=Path(directory) / "agents.json",
                gate_output=True,
                now=1000,
            )
        self.assertEqual(result, 1)
        self.assertIn("state=drift", output)
        self.assertIn("claimed=1", output)
        self.assertIn("actually_active=1", output)
        self.assertIn("stale=1", output)
        self.assertIn("detail=STALE stale", output)

    def test_memory_gate_accepts_absent_optional_store_when_skills_are_clean(self) -> None:
        """A stock Codex/CI host has no Claude memory directory by design."""
        lint = (
            0,
            "active skills: 41  mapped memories: 0  in-sync: 0  "
            "problems: 0  warnings: 1\n",
            "",
        )
        scan = (
            0,
            "state=ok\nsummary=41 authoritative repository skills clean; "
            "0 optional local finding(s)\ncontradictions=0\ndrift=0\n",
            "",
        )
        with mock.patch.object(
            operational_health,
            "_run_tool",
            side_effect=[lint, scan],
        ):
            result, output = self.capture(operational_health.memory_skill_sync_gate)
        self.assertEqual(result, 0)
        self.assertIn("state=ok", output)
        self.assertIn("problems=0", output)
        self.assertIn("contradictions=0", output)

    def test_memory_gate_still_refuses_repository_skill_contradictions(self) -> None:
        lint = (0, "problems: 0\n", "")
        scan = (
            1,
            "state=contradiction\nsummary=one repository contradiction\n"
            "contradictions=1\ndrift=0\nACTION: reconcile repository skill\n",
            "",
        )
        with mock.patch.object(
            operational_health,
            "_run_tool",
            side_effect=[lint, scan],
        ):
            result, output = self.capture(operational_health.memory_skill_sync_gate)
        self.assertEqual(result, 1)
        self.assertIn("state=contradiction", output)
        self.assertIn("contradictions=1", output)
        self.assertIn("ACTION: reconcile repository skill", output)


if __name__ == "__main__":
    unittest.main()


class CloseOnImplementedLifecycleTest(unittest.TestCase):
    """The close-on-implemented lifecycle, asserted as code behaviour.

    Under this lifecycle an implemented task is CLOSED immediately and the
    landing debt is enumerated from CLOSED+implemented records by
    `drain-implemented-to-landed`. Before this, `awaiting_land` was derived
    from the IN_PROGRESS set, which reports ~zero once the lifecycle is
    followed -- the landing debt became invisible exactly when the policy
    started working.
    """

    @staticmethod
    def _task(tid, *, status="IN_PROGRESS", owner="", tags=(), landed=False):
        return operational_health.TaskRecord(
            id=tid, title=tid, owner=owner, tags=tuple(tags), status=status,
            landed=landed,
        )

    def fixture(self):
        """active-ready, implemented-unlanded, backlog, blocked, stale."""
        return [
            self._task("active-ready", owner="agent-a"),
            self._task("stale-unowned", owner=""),
            self._task("implemented-unlanded", status="CLOSED",
                       tags=("implemented",)),
            self._task("implemented-unlanded-2", status="CLOSED",
                       tags=("determinism", "implemented")),
            self._task("backlog-item", status="BACKLOG"),
            self._task("blocked-item", status="OPEN"),
        ]

    def report(self):
        agents = [operational_health.AgentRecord(
            name="agent-a", status="running", current_task="active-ready")]
        return operational_health.reconcile_active_work(self.fixture(), agents)

    def test_only_live_in_progress_work_reaches_the_actionable_queue(self):
        r = self.report()
        # BACKLOG/OPEN/CLOSED rows must not be treated as the live queue.
        self.assertEqual([t.id for t in r.in_progress],
                         ["active-ready", "stale-unowned"])
        self.assertEqual([t.id for t in r.owned_active], ["active-ready"])
        self.assertEqual([t.id for t in r.stale], ["stale-unowned"])

    def test_landing_monitor_still_sees_the_implemented_set(self):
        r = self.report()
        # THE REGRESSION THIS GUARDS: keyed off status, this would be empty.
        self.assertEqual(
            [t.id for t in r.awaiting_land],
            ["implemented-unlanded", "implemented-unlanded-2"],
        )
        self.assertEqual(r.counts()["awaiting_land"], 2)

    def test_awaiting_land_moves_in_BOTH_directions(self):
        """A queue depth that cannot fall is not a queue depth.

        The defect: `awaiting_landing` was a bare `return self.implemented`,
        with an entry condition and NO exit. Nothing removes the tag and no
        `landed` tag exists, so every landed task incremented the count
        forever — it grew monotonically with success and read 1718 while the
        real backlog was ~47. This asserts the property that was missing, in
        both directions, because only rising is the whole bug.
        """
        base = self.fixture()
        agents = [operational_health.AgentRecord(
            name="agent-a", status="running", current_task="active-ready")]

        def depth(tasks):
            return operational_health.reconcile_active_work(
                tasks, agents).counts()["awaiting_land"]

        start = depth(base)
        self.assertEqual(start, 2)

        # UP: tag one more task implemented -> the debt RISES.
        rose = base + [self._task("newly-implemented", status="CLOSED",
                                  tags=("implemented",))]
        self.assertEqual(depth(rose), start + 1,
                         "tagging implemented must increase the debt")

        # DOWN: that same task lands (gateway records CLOSURE-VERIFIED)
        # -> the debt FALLS back. This is the direction the old code could
        # never express.
        landed = base + [self._task("newly-implemented", status="CLOSED",
                                    tags=("implemented",), landed=True)]
        self.assertEqual(depth(landed), start,
                         "a proven-landed task must leave the debt")

        # And landing the whole original set drains it to zero.
        all_landed = [
            self._task(t.id, status=t.status, owner=t.owner, tags=t.tags,
                       landed=t.implemented)
            for t in base
        ]
        self.assertEqual(depth(all_landed), 0,
                         "the debt must be able to reach zero")

    def test_closed_status_alone_does_not_discharge_the_debt(self):
        """CLOSED is not landed, and must not be read as landed.

        Measured 2026-08-07: only 291 of 1718 implemented tasks carried a
        CLOSURE-VERIFIED proof, so 1427 were closed with no landing evidence.
        Exiting on status instead of proof would discharge the debt by
        assertion — the phantom-closure mode the gateway exists to prevent.
        """
        closed_unproven = [self._task("closed-no-proof", status="CLOSED",
                                      tags=("implemented",), landed=False)]
        r = operational_health.reconcile_active_work(closed_unproven, [])
        self.assertEqual([t.id for t in r.awaiting_land], ["closed-no-proof"])

    def test_lifecycle_violation_stays_a_distinct_signal(self):
        """The two metrics must not collapse into near-duplicates.

        Scoping awaiting_land to IN_PROGRESS (the other candidate fix) would
        have made it `implemented AND NOT closed`, which is exactly
        `lifecycle_violation`. Here an implemented+landed+open task is a
        lifecycle deviation but NOT landing debt, so the sets differ.
        """
        tasks = [self._task("open-implemented-landed", status="IN_PROGRESS",
                            owner="a", tags=("implemented",), landed=True)]
        r = operational_health.reconcile_active_work(tasks, [])
        self.assertEqual([t.id for t in r.lifecycle_violations],
                         ["open-implemented-landed"])
        self.assertEqual([t.id for t in r.awaiting_land], [])

    def test_closed_implemented_work_is_never_dispatchable(self):
        """Negative mutation: the landing debt must not leak into any set an
        idle agent could be assigned from, nor into the closure-candidate
        sets."""
        r = self.report()
        debt = {"implemented-unlanded", "implemented-unlanded-2"}
        for name in ("in_progress", "owned_active", "claimed", "actually_active",
                     "stale", "orphaned"):
            leaked = debt & {t.id for t in getattr(r, name)}
            self.assertEqual(leaked, set(), f"landing debt leaked into {name}")

    def test_implemented_but_not_closed_is_reported_as_a_deviation(self):
        """An implemented row left nonterminal is invisible to the drain
        tracker while still occupying the live queue, so it is surfaced as a
        violation rather than silently absorbed."""
        tasks = self.fixture() + [
            self._task("implemented-but-open", status="IN_PROGRESS",
                       tags=("implemented",))
        ]
        r = operational_health.reconcile_active_work(tasks, [])
        self.assertEqual([t.id for t in r.lifecycle_violations],
                         ["implemented-but-open"])
        # It still counts as landing debt (tag-first), and still must not be
        # dispatchable as active work.
        self.assertIn("implemented-but-open", {t.id for t in r.awaiting_land})
        self.assertNotIn("implemented-but-open", {t.id for t in r.owned_active})
        self.assertNotIn("implemented-but-open", {t.id for t in r.stale})

    def test_conformant_graph_reports_zero_violations(self):
        """Positive control: the violation detector is not inert -- it fires
        above and reports zero here, on the same code path."""
        r = self.report()
        self.assertEqual(r.counts()["lifecycle_violations"], 0)

    def test_query_selects_both_populations(self):
        """The SQL must fetch IN_PROGRESS *and* implemented-at-any-status;
        fetching only IN_PROGRESS is what made the debt invisible."""
        src = Path(operational_health.__file__).read_text()
        self.assertIn("WHERE status = 'IN_PROGRESS'", src)
        self.assertIn("json_each(tasks.tags)", src)
        self.assertIn("json_each.value = 'implemented'", src)

    def test_awaiting_land_detail_is_bounded_and_states_its_residue(self):
        """The debt is now the full CLOSED+implemented population, so the
        detail line must not enumerate it -- and must not silently truncate."""
        tasks = [
            self._task(f"impl-{i:02d}", status="CLOSED", tags=("implemented",))
            for i in range(12)
        ]
        r = operational_health.reconcile_active_work(tasks, [])
        detail = operational_health._active_work_detail(r)
        shown = [d for d in detail if d.startswith("AWAITING-LAND impl-")]
        self.assertEqual(len(shown), 5)
        self.assertTrue(
            any("+7 more" in d for d in detail),
            f"residue must be stated explicitly, got: {detail}",
        )


class PrimarySnapshotDeferralTest(unittest.TestCase):
    """Time the deferral instead of paging on the first lost race.

    `primary_checkout` decides whether the snapshot CAN be published right now;
    this layer is the only one that can tell a race from a stuck snapshot, because
    only it persists across ticks. The threshold is on TIME, not on commit
    distance -- commit distance is the moving quantity that made this gate
    unsatisfiable in the first place.
    """

    def capture(self, **kwargs: object) -> tuple[int, str]:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            result = operational_health.primary_snapshot_gate(**kwargs)  # type: ignore[arg-type]
        return int(result), output.getvalue()

    @contextlib.contextmanager
    def outcome(self, code: int):
        with mock.patch.object(
            operational_health.primary_checkout, "checkout_fresh", return_value=code
        ):
            yield

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.state = Path(self.temp.name) / "deferral.json"

    # ---- NEGATIVE: a lost race must not page --------------------------------

    def test_first_deferral_starts_the_clock_and_stays_quiet(self) -> None:
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_DEFERRED):
            result, output = self.capture(state_path=self.state, now=1000.0)
        self.assertEqual(result, 0)
        self.assertIn("state=deferred", output)
        self.assertIn("deferred_mins=0", output)
        self.assertTrue(self.state.exists(), "the deferral clock was not persisted")

    def test_deferral_inside_the_budget_still_does_not_page(self) -> None:
        """59 minutes of losing races is still a race on a box this busy."""
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_DEFERRED):
            self.capture(state_path=self.state, now=1000.0)
            result, output = self.capture(state_path=self.state, now=1000.0 + 59 * 60)
        self.assertEqual(result, 0)
        self.assertIn("state=deferred", output)
        self.assertIn("deferred_mins=59.0", output)

    # ---- POSITIVE: a stuck snapshot must page -------------------------------

    def test_deferral_past_the_budget_pages(self) -> None:
        """Twelve consecutive lost ticks is not luck; something is holding it down."""
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_DEFERRED):
            self.capture(state_path=self.state, now=1000.0)
            result, output = self.capture(state_path=self.state, now=1000.0 + 61 * 60)
        self.assertEqual(result, 1)
        self.assertIn("state=blocked", output)
        self.assertIn("no longer a lost race", output)

    def test_a_real_block_pages_immediately_without_waiting_out_the_budget(self) -> None:
        """A dirty primary is not a moving reference and gets no grace period."""
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_BLOCKED):
            result, output = self.capture(state_path=self.state, now=1000.0)
        self.assertEqual(result, 1)
        self.assertIn("state=blocked", output)

    def test_snapshot_uses_inner_deadline_below_tick_guillotine(self) -> None:
        with mock.patch.object(
            operational_health.primary_checkout,
            "checkout_fresh",
            return_value=operational_health.primary_checkout.SNAPSHOT_BLOCKED,
        ) as checkout:
            result, output = self.capture(state_path=self.state, now=1000.0)

        self.assertEqual(result, 1)
        self.assertIn("state=blocked", output)
        self.assertEqual(
            checkout.call_args.kwargs["deadline_secs"],
            operational_health.PRIMARY_SNAPSHOT_INNER_BUDGET_SECS,
        )
        self.assertLess(operational_health.PRIMARY_SNAPSHOT_INNER_BUDGET_SECS, 30.0)

    # ---- the clock must be honest ------------------------------------------

    def test_a_successful_publish_clears_the_clock(self) -> None:
        """Otherwise an old deferral ages into a page after the problem is gone."""
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_DEFERRED):
            self.capture(state_path=self.state, now=1000.0)
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_PUBLISHED):
            result, output = self.capture(state_path=self.state, now=1000.0 + 10 * 60)
        self.assertEqual(result, 0)
        self.assertIn("state=ok", output)
        self.assertFalse(self.state.exists(), "the deferral clock survived a success")

        # And a fresh deferral after that starts from zero, not from the old clock.
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_DEFERRED):
            result, output = self.capture(state_path=self.state, now=1000.0 + 20 * 60)
        self.assertEqual(result, 0)
        self.assertIn("deferred_mins=0", output)

    def test_a_hard_block_also_clears_the_clock(self) -> None:
        """A block is reported on its own terms; it must not inherit an aged clock
        and then page twice for two different reasons."""
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_DEFERRED):
            self.capture(state_path=self.state, now=1000.0)
        with self.outcome(operational_health.primary_checkout.SNAPSHOT_BLOCKED):
            self.capture(state_path=self.state, now=1000.0 + 5 * 60)
        self.assertFalse(self.state.exists())


class MainAuthorityAbsenceIsNotGreenTest(unittest.TestCase):
    """Silence must stop meaning green, WITHOUT becoming unsatisfiable.

    The gate alarmed on explicit red and stayed quiet otherwise, so an authority
    that never resolved was indistinguishable from a passing one. The naive
    repair -- alarm on `pending` -- fires after every push and is an alarm no
    coordinator can satisfy, which is the anti-pattern removed from three other
    gates today. So the discriminator is TIME, and both sides are planted here.
    """

    def _health(self, repo_states, available=True):
        gh = operational_health.github_main_health
        return [
            gh.RepoMainHealth(repo=r, main_sha="a" * 40, state=s, runs=(),
                              available=available)
            for r, s in repo_states
        ]

    def capture(self, health, age):
        gh = operational_health.github_main_health
        out = io.StringIO()
        with mock.patch.object(gh, "collect_health", return_value=health), \
             mock.patch.object(gh, "pending_age_secs",
                               side_effect=lambda runs, *a, **k: age):
            with contextlib.redirect_stdout(out):
                rc = operational_health.github_main_gate()
        return rc, out.getvalue()

    # ---- must NOT alarm -------------------------------------------------

    def test_a_normal_post_push_pending_window_is_silent(self) -> None:
        """90s of pending after a push is the common case and must stay quiet."""
        rc, out = self.capture(self._health([("r/one", "pending")]), 90.0)
        self.assertEqual(rc, 0, out)
        self.assertIn("state=pending", out)
        self.assertIn("oldest_pending_secs=90", out)

    def test_green_is_still_green(self) -> None:
        rc, out = self.capture(self._health([("r/one", "green")]), None)
        self.assertEqual(rc, 0, out)
        self.assertIn("state=green", out)

    # ---- must alarm -----------------------------------------------------

    def test_pending_past_the_bound_alarms_and_names_the_remedy(self) -> None:
        """The live case: a run superseded under queue-depth-1, silent for hours."""
        rc, out = self.capture(self._health([("r/one", "pending")]), 9198.0)
        self.assertEqual(rc, 1, out)
        self.assertIn("state=stale-pending", out)
        self.assertIn("9198", out)
        self.assertIn("HOLE, not a green", out)
        self.assertIn("Re-dispatch", out, "the alarm must name what clears it")

    def test_an_absent_authority_reports_unmeasured_not_passing(self) -> None:
        """`none` = no authority run at all. Not a green and not a red."""
        rc, out = self.capture(self._health([("r/one", "none")]), None)
        self.assertEqual(rc, 1, out)
        self.assertIn("COULD NOT BE MEASURED", out)
        self.assertNotIn("main is green", out)

    def test_an_unavailable_repo_reports_unmeasured(self) -> None:
        rc, out = self.capture(
            self._health([("r/one", "green")], available=False), None)
        self.assertEqual(rc, 1, out)
        self.assertIn("COULD NOT BE MEASURED", out)

    # ---- the threshold itself -------------------------------------------

    def test_the_bound_is_above_the_slowest_observed_legitimate_run(self) -> None:
        """758s was the slowest real completion measured; the bound must clear it.

        If someone lowers this below observed reality the gate starts paging on
        healthy runs, which is how it becomes unsatisfiable and then muted.
        """
        self.assertGreater(
            operational_health.github_main_health.DEFAULT_PENDING_STALE_SECS, 758.0)
