#!/usr/bin/env python3
"""Cross-consumer contract tests for PASSED / FAILED / NO_RESULT."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

CI_HUB = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(CI_HUB))

from check_outcome import (
    CheckOutcome,
    classify_check,
    select_latest_checks,
    select_latest_workflow_attempts,
    select_latest_workflow_run,
)


class CheckOutcomeContractTests(unittest.TestCase):
    def test_every_admission_consumer_routes_latest_selection_through_authority(self) -> None:
        root = Path(__file__).resolve().parents[2]
        consumers = {
            "ci-hub/health/pr_status.py": "select_latest_checks",
            "ci-hub/health/github_main_health.py": "select_latest_workflow_attempts",
            "ci-hub/remediation/protocol.py": "select_latest_workflow_attempts",
            "ci-hub/history/query.py": "select_latest_workflow_attempts",
            "ci-hub/landing/land-pr.sh": "--select-latest-run",
        }
        for relative, authority_call in consumers.items():
            with self.subTest(consumer=relative):
                source = (root / relative).read_text()
                self.assertIn(authority_call, source)

    def test_legitimate_passes_remain_passed_n2(self) -> None:
        # N=2 positive controls: CheckRun and legacy StatusContext shapes.
        cases = (("completed", "success"), ("", "SUCCESS"))
        self.assertEqual(len(cases), 2)
        for status, conclusion in cases:
            with self.subTest(status=status, conclusion=conclusion):
                self.assertIs(
                    classify_check(status, conclusion), CheckOutcome.PASSED
                )

    def test_genuine_failures_remain_failed_n4(self) -> None:
        cases = ("failure", "timed_out", "error", "startup_failure")
        self.assertEqual(len(cases), 4)
        for conclusion in cases:
            with self.subTest(conclusion=conclusion):
                self.assertIs(
                    classify_check("completed", conclusion), CheckOutcome.FAILED
                )

    def test_no_result_is_neither_passed_nor_failed_n11(self) -> None:
        cases = (
            ("completed", "cancelled"),
            ("completed", "skipped"),
            ("completed", "neutral"),
            ("completed", "action_required"),
            ("completed", "stale"),
            ("completed", ""),
            ("queued", ""),
            ("in_progress", ""),
            ("waiting", ""),
            ("", "brand_new_github_state"),
            ("", ""),
        )
        self.assertEqual(len(cases), 11)
        for status, conclusion in cases:
            with self.subTest(status=status, conclusion=conclusion):
                outcome = classify_check(status, conclusion)
                self.assertIs(outcome, CheckOutcome.NO_RESULT)
                self.assertIsNot(outcome, CheckOutcome.PASSED)
                self.assertIsNot(outcome, CheckOutcome.FAILED)

    def test_proven_self_timeout_is_failed(self) -> None:
        self.assertIs(
            classify_check("completed", "cancelled", self_timeout=True),
            CheckOutcome.FAILED,
        )

    def test_latest_same_head_check_wins_in_both_input_orders(self) -> None:
        sha = "a" * 40
        older = {
            "name": "merge-gate",
            "headSha": sha,
            "status": "COMPLETED",
            "conclusion": "FAILURE",
            "startedAt": "2026-08-04T15:12:05Z",
            "detailsUrl": "https://github.com/o/r/actions/runs/30922888575/job/1",
        }
        newer = {
            "name": "merge-gate",
            "headSha": sha,
            "status": "COMPLETED",
            "conclusion": "SUCCESS",
            "startedAt": "2026-08-04T15:24:36Z",
            "detailsUrl": "https://github.com/o/r/actions/runs/30923975433/job/2",
        }
        wrong_head = {**newer, "headSha": "b" * 40, "conclusion": "FAILURE"}
        for rollup in ([older, newer, wrong_head], [wrong_head, newer, older]):
            with self.subTest(order=[item["conclusion"] for item in rollup]):
                selected = select_latest_checks(rollup, head_sha=sha)
                self.assertEqual(len(selected), 1)
                self.assertEqual(selected[0]["conclusion"], "SUCCESS")

    def test_newer_queued_run_overrides_older_success(self) -> None:
        selected = select_latest_checks(
            [
                {
                    "name": "merge-gate",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-08-04T15:12:05Z",
                    "detailsUrl": "https://github.com/o/r/actions/runs/10/job/1",
                },
                {
                    "name": "merge-gate",
                    "status": "QUEUED",
                    "conclusion": "",
                    "startedAt": "0001-01-01T00:00:00Z",
                    "detailsUrl": "https://github.com/o/r/actions/runs/11/job/2",
                },
            ]
        )
        self.assertEqual(
            classify_check(selected[0]["status"], selected[0]["conclusion"]),
            CheckOutcome.NO_RESULT,
        )

    def test_unorderable_contrary_duplicate_is_no_result(self) -> None:
        selected = select_latest_checks(
            [
                {
                    "name": "merge-gate",
                    "status": "COMPLETED",
                    "conclusion": "FAILURE",
                },
                {
                    "name": "merge-gate",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                },
            ]
        )
        self.assertEqual(len(selected), 1)
        self.assertEqual(selected[0]["status"], "AMBIGUOUS")
        self.assertIs(
            classify_check(selected[0]["status"], selected[0]["conclusion"]),
            CheckOutcome.NO_RESULT,
        )

    def test_equal_nonzero_identity_contrary_duplicate_is_order_independent(self) -> None:
        failure = {
            "name": "merge-gate",
            "status": "COMPLETED",
            "conclusion": "FAILURE",
            "startedAt": "2026-08-04T15:24:36Z",
            "detailsUrl": "https://github.com/o/r/actions/runs/11/job/100",
        }
        success = {
            **failure,
            "conclusion": "SUCCESS",
            "detailsUrl": "https://github.com/o/r/actions/runs/11/job/101",
        }
        for rollup in ([failure, success], [success, failure]):
            selected = select_latest_checks(rollup)
            self.assertEqual(len(selected), 1)
            self.assertEqual(selected[0]["status"], "AMBIGUOUS")
            self.assertIn("equal ordering identity", selected[0]["_selectionError"])
            self.assertIs(
                classify_check(selected[0]["status"], selected[0]["conclusion"]),
                CheckOutcome.NO_RESULT,
            )

    def test_latest_workflow_run_filters_head_event_and_ties_by_id(self) -> None:
        sha = "a" * 40
        payload = {
            "workflow_runs": [
                {
                    "head_sha": sha,
                    "event": "pull_request",
                    "created_at": "2026-08-04T15:12:05Z",
                    "id": 10,
                },
                {
                    "head_sha": sha,
                    "event": "workflow_dispatch",
                    "created_at": "2026-08-04T15:24:36Z",
                    "id": 11,
                },
                {
                    "head_sha": sha,
                    "event": "workflow_run",
                    "created_at": "2026-08-04T15:25:00Z",
                    "id": 12,
                },
                {
                    "head_sha": "b" * 40,
                    "event": "workflow_dispatch",
                    "created_at": "2026-08-04T15:26:00Z",
                    "id": 13,
                },
            ]
        }
        selected = select_latest_workflow_run(
            payload, head_sha=sha, events=("pull_request", "workflow_dispatch")
        )
        self.assertEqual(selected["id"], 11)

    def test_latest_workflow_attempts_groups_by_head_and_workflow(self) -> None:
        payload = [
            {
                "headSha": "a",
                "workflowName": "ci",
                "createdAt": "t",
                "databaseId": 10,
                "status": "completed",
                "conclusion": "failure",
            },
            {
                "headSha": "a",
                "workflowName": "ci",
                "createdAt": "t",
                "databaseId": 11,
                "status": "completed",
                "conclusion": "success",
            },
            {
                "headSha": "a",
                "workflowName": "docs",
                "createdAt": "t",
                "databaseId": 12,
                "status": "completed",
                "conclusion": "success",
            },
            {
                "headSha": "b",
                "workflowName": "ci",
                "createdAt": "t",
                "databaseId": 13,
                "status": "completed",
                "conclusion": "failure",
            },
        ]
        selected = select_latest_workflow_attempts(payload, head_sha="a")
        self.assertEqual(
            {(run["workflowName"], run["databaseId"]) for run in selected},
            {("ci", 11), ("docs", 12)},
        )

    def test_tied_workflow_attempts_with_contrary_verdicts_are_no_result(self) -> None:
        failure = {
            "headSha": "a",
            "workflowName": "ci",
            "createdAt": "t",
            "databaseId": 10,
            "status": "completed",
            "conclusion": "failure",
        }
        success = {**failure, "conclusion": "success"}
        for payload in ([failure, success], [success, failure]):
            selected = select_latest_workflow_attempts(payload, head_sha="a")
            self.assertEqual(len(selected), 1)
            self.assertIs(
                classify_check(selected[0]["status"], selected[0]["conclusion"]),
                CheckOutcome.NO_RESULT,
            )

    def test_run_attempt_breaks_a_same_run_id_tie(self) -> None:
        first = {
            "head_sha": "a" * 40,
            "workflow_name": "ci",
            "created_at": "2026-08-04T15:24:36Z",
            "run_id": "11",
            "run_attempt": "1",
            "status": "completed",
            "conclusion": "failure",
        }
        second = {
            **first,
            "run_attempt": "2",
            "conclusion": "success",
        }
        for attempts in ([first, second], [second, first]):
            selected = select_latest_workflow_attempts(attempts)
            self.assertEqual(len(selected), 1)
            self.assertEqual(selected[0]["run_attempt"], "2")
            self.assertEqual(selected[0]["conclusion"], "success")


if __name__ == "__main__":
    unittest.main()
