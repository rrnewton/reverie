#!/usr/bin/env python3
"""Language-boundary tests for the shared agent snapshot contract."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

LIB = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(LIB))

import agent_snapshot  # noqa: E402


class AgentSnapshotTests(unittest.TestCase):
    def snapshot(self, status: str = "busy") -> bytes:
        return json.dumps(
            {
                "schema_version": 1,
                "captured_at": 1_787_893_230,
                "agents": [
                    {
                        "name": "type-audit",
                        "status": status,
                        "current_task": "twenty-two-remaining-type-families-from-the-published-census",
                        "tmux_pane_id": "%20",
                    }
                ],
            }
        ).encode()

    def test_current_shape_crosses_the_rust_python_boundary(self) -> None:
        document = agent_snapshot.parse_snapshot_bytes(self.snapshot())
        self.assertEqual(document["agents"][0]["name"], "type-audit")

    def test_unreachable_is_not_live(self) -> None:
        agent_snapshot.parse_snapshot_bytes(self.snapshot("unreachable"))
        meaning = agent_snapshot.status_semantics("unreachable")
        self.assertFalse(meaning.live)
        self.assertTrue(meaning.broken)

    def test_spawning_is_live_and_busy(self) -> None:
        document = agent_snapshot.parse_snapshot_bytes(self.snapshot("spawning"))
        self.assertEqual(document["agents"][0]["status"], "spawning")
        meaning = agent_snapshot.status_semantics("spawning")
        self.assertTrue(meaning.live)
        self.assertTrue(meaning.busy)
        self.assertFalse(meaning.broken)

    def test_needs_permission_is_live_but_waiting(self) -> None:
        document = agent_snapshot.parse_snapshot_bytes(
            self.snapshot("needs-permission")
        )
        self.assertEqual(document["agents"][0]["status"], "needs-permission")
        meaning = agent_snapshot.status_semantics("needs-permission")
        self.assertTrue(meaning.live)
        self.assertFalse(meaning.busy)
        self.assertFalse(meaning.broken)

    def test_missing_and_unknown_status_fail_by_name(self) -> None:
        missing = json.loads(self.snapshot())
        del missing["agents"][0]["status"]
        with self.assertRaisesRegex(agent_snapshot.AgentSnapshotSchemaError, "status"):
            agent_snapshot.parse_snapshot_bytes(json.dumps(missing).encode())
        with self.assertRaisesRegex(
            agent_snapshot.AgentSnapshotSchemaError, "future-state"
        ):
            agent_snapshot.parse_snapshot_bytes(self.snapshot("future-state"))

    def test_canonical_binding_refuses_legacy_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "ignored/ci-hub/agent-snapshot.json"
            path.parent.mkdir(parents=True)
            binding = root / ".tick-hub/herdr-agent-bindings.json"
            binding.parent.mkdir()
            binding.write_text(
                json.dumps(
                    {
                        "schema_version": 2,
                        "taskgraph_db": str(root / "tasks.db"),
                    }
                )
            )
            document = agent_snapshot.parse_snapshot_bytes(self.snapshot())
            with self.assertRaisesRegex(
                agent_snapshot.AgentSnapshotSchemaError, "composite authority"
            ):
                agent_snapshot.require_canonical_authority(document, path)


if __name__ == "__main__":
    unittest.main()
