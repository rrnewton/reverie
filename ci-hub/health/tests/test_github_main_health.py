#!/usr/bin/env python3
"""Tests for ci-hub/health/github_main_health.py."""

from __future__ import annotations

import json
import os
import tempfile
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import github_main_health


def run(
    *,
    workflow: str,
    conclusion: str,
    created_at: str = "2026-08-02T00:00:00Z",
    run_id: int = 1,
) -> dict[str, str]:
    return {
        "workflowName": workflow,
        "headSha": "a" * 40,
        "status": "completed",
        "conclusion": conclusion,
        "url": f"https://example.test/{workflow}",
        "createdAt": created_at,
        "databaseId": str(run_id),
    }


class MainHealthTests(unittest.TestCase):
    def setUp(self) -> None:
        selected = mock.patch.dict(os.environ, FLEET_REAL_GH=sys.executable)
        selected.start()
        self.addCleanup(selected.stop)

    def test_actual_selected_proxy_route_and_refusals(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            selected = root / "selected gh"
            selected.write_text('#!/bin/sh\nprintf \'%s\\n\' "$@"\n')
            selected.chmod(0o755)
            proxy = root / "with-proxy"
            proxy.write_text('#!/bin/sh\nprintf \'proxied\\n\'\nexec "$@"\n')
            proxy.chmod(0o755)
            decoy = root / "gh"
            decoy.write_text('#!/bin/sh\nexit 99\n')
            decoy.chmod(0o755)
            with mock.patch.dict(os.environ, PATH=str(root), FLEET_REAL_GH=str(selected)):
                self.assertEqual(github_main_health._run_gh(
                    ["api", "operand with spaces"], timeout=2),
                    "proxied\napi\noperand with spaces\n")
                for invalid in ("", "gh", str(root / "absent"), str(root)):
                    with self.subTest(invalid=invalid), mock.patch.dict(
                        os.environ, FLEET_REAL_GH=invalid
                    ), mock.patch("github_main_health.subprocess.run") as child:
                        with self.assertRaisesRegex(RuntimeError, "FLEET_REAL_GH"):
                            github_main_health.fetch_main_sha("fixture/repo", timeout=2)
                        child.assert_not_called()
                proxy.unlink()
                with self.assertRaisesRegex(RuntimeError, "with-proxy"):
                    github_main_health._run_gh(["api", "user"], timeout=2)

    def test_red_wins(self) -> None:
        runs = (
            github_main_health.MainRun(
                "docs", "a" * 40, "completed", "success", "u1", "1"
            ),
            github_main_health.MainRun(
                "ci", "a" * 40, "completed", "failure", "u2", "1"
            ),
        )
        self.assertEqual(github_main_health.classify_current_runs(runs), "red")

    def test_pending_is_not_green(self) -> None:
        runs = (
            github_main_health.MainRun("ci", "a" * 40, "in_progress", "", "u", "1"),
        )
        self.assertEqual(github_main_health.classify_current_runs(runs), "pending")

    def test_cancelled_is_not_red(self) -> None:
        # Regression: a cancelled run is a HOLE, not a failure. It must never make
        # main "red" (task cancelled-run-classified-as-red) — a cancelled run
        # misread as red nearly reverted a healthy main.
        runs = (
            github_main_health.MainRun("ci", "a" * 40, "completed", "cancelled", "u", "1"),
        )
        self.assertEqual(github_main_health.classify_current_runs(runs), "pending")

    def test_no_result_conclusions_are_neither_red_nor_green(self) -> None:
        for conclusion in (
            "cancelled",
            "action_required",
            "stale",
            "skipped",
            "neutral",
            "",
        ):
            runs = (
                github_main_health.MainRun(
                    "ci", "a" * 40, "completed", conclusion, "u", "1"
                ),
            )
            state = github_main_health.classify_current_runs(runs)
            self.assertEqual(state, "pending")
            self.assertNotEqual(state, "red")
            self.assertNotEqual(state, "green")

    def test_unknown_conclusion_is_not_red(self) -> None:
        # A conclusion GitHub adds later must not manufacture a false red (the
        # hardcoded-list-of-a-growing-set trap).
        runs = (
            github_main_health.MainRun(
                "ci", "a" * 40, "completed", "brand_new_state", "u", "1"
            ),
        )
        self.assertNotEqual(
            github_main_health.classify_current_runs(runs), "red"
        )

    def test_render_marks_cancelled_as_no_result_not_red(self) -> None:
        health = (
            github_main_health.RepoMainHealth(
                repo="rrnewton/hermit",
                main_sha="a" * 40,
                state="pending",
                runs=(
                    github_main_health.MainRun(
                        "ci", "a" * 40, "completed", "cancelled", "u", "1"
                    ),
                ),
            ),
        )
        report = github_main_health.render_report(health)
        self.assertIn("NO-RESULT", report)
        self.assertNotIn("HARD WARNING: GITHUB MAIN IS RED", report)

    # --- cancellation sub-taxonomy: three flavors, one conclusion -------------
    # `cancelled` cannot be split by conclusion or duration (a supersede was
    # observed cancelled 4s under a 300s cap). The ONLY reliable discriminator is
    # the check annotation, and it is a single POSITIVE red signal in the safe
    # direction: only the self-timeout notice promotes cancelled -> red; every
    # other annotation (or none) leaves cancelled a no_result, so a supersede /
    # manual / queue cancel can never manufacture a false red.

    def test_self_timeout_annotation_detected(self) -> None:
        # A `timeout-minutes` kill (our box firing on a hang) — a REAL signal.
        messages = [
            "The job running on runner X has exceeded the maximum execution time of 10 minutes.",
        ]
        self.assertTrue(github_main_health.is_self_timeout(messages))

    def test_supersede_and_manual_annotations_are_not_self_timeout(self) -> None:
        # A superseding push and a manual/queue cancel are the ABSENCE of a
        # result, not a hang: neither may read as a self-timeout.
        supersede = [
            "Canceling since a higher priority waiting request for 'main' exists",
        ]
        manual = ["The run was canceled by @someone"]
        self.assertFalse(github_main_health.is_self_timeout(supersede))
        self.assertFalse(github_main_health.is_self_timeout(manual))
        self.assertFalse(github_main_health.is_self_timeout([]))

    def test_self_timeout_cancel_classifies_red(self) -> None:
        # A confirmed self-timeout kill is a genuine bad answer (a hang): it
        # alarms on the dashboard so the box is never silent.
        runs = (
            github_main_health.MainRun(
                "ci", "a" * 40, "completed", "cancelled", "u", "1",
                run_id="99", self_timeout=True,
            ),
        )
        self.assertEqual(github_main_health.classify_current_runs(runs), "red")

    def test_supersede_cancel_without_self_timeout_is_not_red(self) -> None:
        runs = (
            github_main_health.MainRun(
                "ci", "a" * 40, "completed", "cancelled", "u", "1",
                run_id="99", self_timeout=False,
            ),
        )
        self.assertNotEqual(github_main_health.classify_current_runs(runs), "red")

    def test_render_marks_self_timeout_run(self) -> None:
        health = (
            github_main_health.RepoMainHealth(
                repo="rrnewton/hermit",
                main_sha="a" * 40,
                state="red",
                runs=(
                    github_main_health.MainRun(
                        "ci", "a" * 40, "completed", "cancelled", "u", "1",
                        run_id="99", self_timeout=True,
                    ),
                ),
            ),
        )
        report = github_main_health.render_report(health)
        self.assertIn("SELF-TIMEOUT", report)

    @mock.patch("github_main_health.subprocess.run")
    def test_evaluate_uses_proxy_and_latest_attempt(
        self, run_command: mock.Mock
    ) -> None:
        payload = [
            run(workflow="ci", conclusion="success", created_at="2026-08-02T00:01:00Z"),
            run(workflow="ci", conclusion="failure", created_at="2026-08-02T00:00:00Z"),
        ]
        run_command.side_effect = (
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout="a" * 40 + "\n", stderr=""
            ),
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps(payload), stderr=""
            ),
        )
        health = github_main_health.evaluate_repo("rrnewton/hermit")
        self.assertEqual(health.state, "green")
        self.assertEqual(len(health.runs), 1)
        for call in run_command.call_args_list:
            self.assertEqual(call.args[0][:2], ["with-proxy", sys.executable])
        list_command = run_command.call_args_list[1].args[0]
        self.assertIn("--branch", list_command)
        self.assertIn("--event", list_command)

    @mock.patch("github_main_health.subprocess.run")
    def test_same_timestamp_uses_newer_run_id(self, run_command: mock.Mock) -> None:
        timestamp = "2026-08-02T00:01:00Z"
        payload = [
            run(workflow="ci", conclusion="failure", created_at=timestamp, run_id=10),
            run(workflow="ci", conclusion="success", created_at=timestamp, run_id=11),
        ]
        run_command.side_effect = (
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout="a" * 40 + "\n", stderr=""
            ),
            subprocess.CompletedProcess(
                args=[], returncode=0, stdout=json.dumps(payload), stderr=""
            ),
        )
        health = github_main_health.evaluate_repo("rrnewton/hermit")
        self.assertEqual(health.state, "green")
        self.assertEqual(health.runs[0].run_id, "11")

    def test_render_red_hard_warning(self) -> None:
        health = (
            github_main_health.RepoMainHealth(
                repo="rrnewton/hermit",
                main_sha="a" * 40,
                state="red",
                runs=(
                    github_main_health.MainRun(
                        "ci", "a" * 40, "completed", "failure", "https://run", "1"
                    ),
                ),
            ),
        )
        report = github_main_health.render_report(health)
        self.assertIn("HARD WARNING: GITHUB MAIN IS RED", report)
        self.assertIn("RED", report)

    @mock.patch("github_main_health.subprocess.run")
    def test_stalling_service_becomes_partial_not_hang(
        self, run_command: mock.Mock
    ) -> None:
        run_command.side_effect = subprocess.TimeoutExpired(cmd="gh", timeout=0.01)
        health = github_main_health.collect_health(
            ["rrnewton/hermit"],
            10,
            per_call_timeout=0.01,
            overall_deadline=0.1,
        )
        self.assertEqual(len(health), 1)
        self.assertFalse(health[0].available)
        self.assertEqual(github_main_health.overall_state(health), "degraded")
        report = github_main_health.render_report(health)
        self.assertIn("DEGRADED", report)
        self.assertIn("UNAVAILABLE", report)
        self.assertIn("PARTIAL RESULT", report)
        run_command.assert_called_once()

    def test_exhausted_deadline_marks_every_remaining_repo_unavailable(self) -> None:
        with mock.patch(
            "github_main_health.time.monotonic", side_effect=[100.0, 101.0, 101.0]
        ):
            health = github_main_health.collect_health(
                ["rrnewton/hermit", "rrnewton/reverie"],
                10,
                per_call_timeout=1.0,
                overall_deadline=0.0,
            )
        self.assertTrue(all(not repo.available for repo in health))
        self.assertTrue(all("deadline" in repo.reason for repo in health))


if __name__ == "__main__":
    unittest.main()
