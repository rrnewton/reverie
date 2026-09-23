#!/usr/bin/env python3
"""Tests for ci-hub/health/github_main_health.py."""

from __future__ import annotations

import json
import os
import tempfile
import shutil
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

    def test_shell_default_survives_cost_reentry_and_preserves_explicit_route(self) -> None:
        source = Path(__file__).resolve().parents[3]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("ci-hub/bin/main-health", "ci-hub/health/github_main_health.py",
                         "ci-hub/check_outcome.py", "ci-hub/validate/gh_route.py"):
                target = root / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source / name, target)
            bindir = root / "bin"
            bindir.mkdir()
            (bindir / "python3").symlink_to(sys.executable)
            (bindir / "git").write_text('#!/bin/sh\nprintf \'%s\\n\' "$FIXTURE_ROOT"\n')
            (bindir / "git").chmod(0o755)
            (bindir / "with-proxy").write_text('#!/bin/sh\nexec "$@"\n')
            (bindir / "with-proxy").chmod(0o755)
            cost = root / "ci-hub/bin/tool-cost"
            cost.write_text(
                f"#!{sys.executable}\nimport os,sys,json\n"
                "from pathlib import Path\n"
                "Path(os.environ['FIXTURE_COST']).write_text(json.dumps([os.environ['FLEET_REAL_GH'],os.environ['CI_HUB_TOOL_COST_ACTIVE']]))\n"
                "argv=sys.argv[sys.argv.index('--')+1:];os.execv(argv[0],argv)\n"
            )
            cost.chmod(0o755)
            default = root / "home/bin/gh"
            default.parent.mkdir(parents=True)
            override = root / "explicit gh"
            cli = (f"#!{sys.executable}\nimport os,sys,json\n"
                   "with open(os.environ['FIXTURE_CALLS'],'a') as f: json.dump([sys.argv[0],sys.argv[1:]],f);f.write('\\n')\n"
                   "if sys.argv[1]=='api': print('a'*40)\n"
                   "else: print(json.dumps([{'workflowName':'ci','headSha':'a'*40,'status':'completed','conclusion':'success','databaseId':1,'createdAt':'2026-09-23T00:00:00Z'}]))\n")
            for selected in (default, override):
                selected.write_text(cli)
                selected.chmod(0o755)
            calls = root / "calls"
            cost_record = root / "cost"
            env = dict(os.environ, HOME=str(root / "home"), PATH=f"{bindir}:/usr/bin:/bin",
                       FIXTURE_ROOT=str(root), FIXTURE_CALLS=str(calls), FIXTURE_COST=str(cost_record))
            env.pop("FLEET_REAL_GH", None)
            env.pop("CI_HUB_TOOL_COST_ACTIVE", None)
            argv = [str(root / "ci-hub/bin/main-health"), "--repo", "fixture/repo", "--json"]
            for configured in (None, str(override)):
                current = dict(env)
                if configured is not None:
                    current['FLEET_REAL_GH'] = configured
                result = subprocess.run(argv, env=current, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(json.loads(result.stdout)['state'], 'green')
                expected = configured or str(default)
                self.assertEqual(json.loads(cost_record.read_text()), [expected, '1'])
                recent = [json.loads(line) for line in calls.read_text().splitlines()][-2:]
                self.assertEqual([row[0] for row in recent], [expected, expected])
                self.assertEqual(recent[0][1], ['api', 'repos/fixture/repo/commits/main', '--jq', '.sha'])
            before = calls.read_bytes()
            default.chmod(0o644)
            for configured in (None, '', 'gh', str(root / 'absent')):
                current = dict(env)
                if configured is not None:
                    current['FLEET_REAL_GH'] = configured
                result = subprocess.run(argv, env=current, capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('FLEET_REAL_GH', result.stdout)
                self.assertEqual(calls.read_bytes(), before)

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
