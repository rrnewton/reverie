#!/usr/bin/env python3

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path


HEALTH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HEALTH))
import wrkslots_audit_result


SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_C = "c" * 40


def observer_cost(*, exit_code: int = 0, wall: float = 115.7) -> dict[str, object]:
    return {
        "schema_version": 1,
        "tool": "ci-hub/wrkslots-audit",
        "estimate": {
            "kind": "derived",
            "wall_seconds": 190.0,
            "cpu_seconds": 179.0,
            "basis": "derived from one completed contention audit",
        },
        "actual": {
            "wall_seconds": wall,
            "cpu_seconds": 3.6,
            "cpu_user_seconds": 1.2,
            "cpu_system_seconds": 2.4,
            "exit": str(exit_code),
        },
    }


def audit_cost(
    *,
    exit_code: int = 0,
    wall: float = 115.7,
    cpu_budget: int = 240,
    wall_backstop: int = 1800,
    usage_usec: int = 85_200_000,
    reaped: bool = False,
    termination: str = "completed",
) -> dict[str, object]:
    return {
        "schema": "wrkslots-audit-cost/v1",
        "wall_seconds": wall,
        "cpu_seconds": usage_usec / 1_000_000,
        "wall_source": "outer monotonic observer",
        "cpu_source": "cgroup-v2 cpu.stat usage_usec",
        "cpu_budget_seconds": cpu_budget,
        "wall_backstop_seconds": wall_backstop,
        "cgroup_usage_usec": usage_usec,
        "scope": "hermit-box-run-123-456.scope",
        "reaped": reaped,
        "termination": termination,
        "exit_code": exit_code,
    }


class WrkslotsAuditResultTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name) / "latest.json"

    def write(self, **overrides: object) -> None:
        payload: dict[str, object] = {
            "schema": wrkslots_audit_result.SCHEMA,
            "started_at": "1970-01-01T00:16:00Z",
            "started_at_epoch_seconds": 960.0,
            "observed_at": "1970-01-01T00:16:40Z",
            "observed_at_epoch_seconds": 1_000.0,
            "producer": {
                "parent_sha": SHA_A,
                "hermit_sha": SHA_B,
                "agent_utils_sha": SHA_C,
            },
            "limits": {
                "cpu_budget_seconds": 240,
                "wall_backstop_seconds": 1800,
            },
            "verdict": "OK",
            "summary": "no worktree slot currently needs coordinator attention",
            "exit_code": 0,
            "audit": {
                "schema": 2,
                "state": "ok",
                "summary": "no worktree slot currently needs coordinator attention",
                "worktree_count": 32,
                "running_agent_count": 20,
                "attention_count": 0,
                "attention_slots": [],
                "unknown_count": 0,
                "unknown_slots": [],
            },
            "cost": audit_cost(),
            "observer_cost": observer_cost(),
            "stderr_sha256": "0" * 64,
        }
        payload.update(overrides)
        self.path.write_text(json.dumps(payload), encoding="utf-8")

    def evaluate(self, **overrides: object) -> tuple[str, str, dict[str, object]]:
        arguments: dict[str, object] = {
            "max_age_seconds": 7200,
            "expected_parent_sha": SHA_A,
            "expected_hermit_sha": SHA_B,
            "expected_agent_utils_sha": SHA_C,
            "now": 1100.0,
        }
        arguments.update(overrides)
        return wrkslots_audit_result.evaluate(self.path, **arguments)  # type: ignore[arg-type]

    def test_fresh_matching_result_preserves_ok_and_actionable(self) -> None:
        self.write()
        state, summary, fields = self.evaluate()
        self.assertEqual("ok", state)
        self.assertIn("no worktree slot", summary)
        self.assertEqual(100, fields["age_secs"])

        self.write(
            verdict="ACTIONABLE",
            summary="slot01 needs attention",
            exit_code=1,
            audit={
                "schema": 2,
                "state": "actionable",
                "summary": "slot01 needs attention",
                "worktree_count": 1,
                "running_agent_count": 0,
                "attention_count": 1,
                "attention_slots": ["slot01"],
                "unknown_count": 0,
                "unknown_slots": [],
            },
            cost=audit_cost(exit_code=1, wall=1.0, usage_usec=500_000),
            observer_cost=observer_cost(exit_code=1, wall=1.0),
        )
        state, summary, _fields = self.evaluate()
        self.assertEqual("actionable", state)
        self.assertEqual("slot01 needs attention", summary)

    def test_missing_stale_corrupt_or_future_result_is_no_result(self) -> None:
        for setup, fragment, now in (
            (lambda: None, "never completed", 1100.0),
            (lambda: self.path.write_text("not-json"), "unreadable", 1100.0),
            (lambda: self.write(), "freshness", 10_000.0),
            (lambda: self.write(), "future", 900.0),
        ):
            self.path.unlink(missing_ok=True)
            setup()
            state, summary, _fields = self.evaluate(now=now)
            self.assertEqual("NO_RESULT", state)
            self.assertIn(fragment, summary)

    def test_wrong_provenance_or_incomplete_cost_is_no_result(self) -> None:
        self.write()
        state, summary, _fields = self.evaluate(expected_parent_sha="d" * 40)
        self.assertEqual("NO_RESULT", state)
        self.assertIn("parent_sha", summary)

        self.write(cost=None)
        state, summary, _fields = self.evaluate()
        self.assertEqual("NO_RESULT", state)
        self.assertIn("cost actual", summary)

    def test_contradictory_or_semantically_incomplete_receipts_are_no_result(
        self,
    ) -> None:
        cases = (
            {"audit": None, "exit_code": 0},
            {"exit_code": 1},
            {
                "audit": {
                    "schema": 2,
                    "state": "actionable",
                    "summary": "claims healthy",
                    "worktree_count": 9,
                    "running_agent_count": 0,
                    "attention_count": 9,
                    "attention_slots": [f"slot{i}" for i in range(9)],
                    "unknown_count": 0,
                    "unknown_slots": [],
                }
            },
            {"cost": audit_cost(wall=-1.0)},
            {"stderr_sha256": int("1" * 64)},
        )
        for overrides in cases:
            with self.subTest(overrides=overrides):
                self.write(**overrides)
                state, summary, _fields = self.evaluate()
                self.assertEqual("NO_RESULT", state)
                self.assertIn("contract", summary)

    def test_boxed_and_observer_cost_mismatches_are_no_result(self) -> None:
        cases = (
            {"cost": audit_cost(usage_usec=85_200_000) | {"cpu_seconds": 1.0}},
            {"cost": audit_cost(wall=99.0)},
            {"cost": audit_cost(cpu_budget=241)},
            {"cost": audit_cost(wall_backstop=1801)},
            {"cost": audit_cost(exit_code=1)},
            {"cost": audit_cost(reaped=True)},
            {
                "cost": audit_cost(
                    exit_code=137,
                    usage_usec=240_000_000,
                    reaped=True,
                    termination="cpu_budget",
                ),
                "exit_code": 137,
                "verdict": "NO_RESULT",
                "summary": "CPU budget exhausted",
                "audit": None,
                "observer_cost": observer_cost(exit_code=137),
            },
            {
                "cost": audit_cost(
                    exit_code=137,
                    wall=1.0,
                    reaped=True,
                    termination="wall_backstop",
                ),
                "exit_code": 137,
                "verdict": "NO_RESULT",
                "summary": "wall backstop expired",
                "audit": None,
                "observer_cost": observer_cost(exit_code=137, wall=1.0),
            },
        )
        for overrides in cases:
            with self.subTest(overrides=overrides):
                self.write(**overrides)
                state, summary, _fields = self.evaluate()
                self.assertEqual("NO_RESULT", state)
                self.assertIn("contract", summary)

    def test_producer_no_result_remains_no_result(self) -> None:
        self.write(
            verdict="NO_RESULT",
            summary="CPU budget exhausted",
            exit_code=137,
            audit=None,
            cost=None,
            observer_cost=None,
        )
        state, summary, _fields = self.evaluate()
        self.assertEqual("NO_RESULT", state)
        self.assertEqual("CPU budget exhausted", summary)


if __name__ == "__main__":
    unittest.main()
