#!/usr/bin/env python3
"""Brackets for the slot/agent parity gate.

A parity check added in response to a missed leak, that has never been seen to
fire, is the same class of defect as the leak.  So every case below asserts a
NAMED outcome, and the negative cases are the point:

  * a slot with no live agent must RED, and must name the slot;
  * an empty agent census must REFUSE (exit 2), never pass -- a denominator of
    zero computes zero orphans and would otherwise read as perfect health;
  * a stale snapshot must REFUSE.  The real one was 65.8 hours -- 790 ticks --
    old on 2026-08-24, which is why staleness is a refusal and not a caveat;
  * a genuine pass must report a NONZERO `live_agents_examined`, so a green
    verdict carries the denominator that produced it.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GATE = ROOT / "ci-hub" / "health" / "slot_agent_parity.py"


def run(root: Path, *args: str) -> tuple[int, dict[str, str]]:
    result = subprocess.run(
        (sys.executable, str(GATE), "--gate", "--root", str(root), *args),
        capture_output=True,
        text=True,
        check=False,
    )
    fields: dict[str, str] = {}
    for line in result.stdout.splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            fields[key] = value
    return result.returncode, fields


class SlotAgentParityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="slot-agent-parity-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "worktrees").mkdir()

    def write_registry(self, slots: dict[str, dict]) -> None:
        (self.root / "worktree-state.json").write_text(json.dumps({"slots": slots}))

    def slot(self, agent: str | None, status: str = "active") -> dict:
        record: dict = {"status": status}
        if agent is not None:
            record["agents"] = [{"name": agent, "read_only": False}]
        return record

    # --- the predicate itself -------------------------------------------------

    def test_slot_without_a_live_agent_reds_and_names_it(self) -> None:
        self.write_registry(
            {"alive": self.slot("hermit-001"), "abandoned": self.slot("hermit-999")}
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 1, "a slot with no live agent must be actionable")
        self.assertEqual(fields["state"], "actionable")
        self.assertEqual(fields["active_slots_without_live_agent"], "1")
        self.assertIn("abandoned", fields["summary"])
        self.assertNotIn("alive", fields["summary"].split(": ")[-1].split(", "))

    def test_every_slot_covered_passes_and_reports_the_denominator(self) -> None:
        self.write_registry(
            {"one": self.slot("hermit-001"), "two": self.slot("hermit-002")}
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001,hermit-002")
        self.assertEqual(code, 0)
        self.assertEqual(fields["state"], "ok")
        self.assertEqual(fields["active_slots_without_live_agent"], "0")
        # The green verdict must carry its denominator.
        self.assertEqual(fields["live_agents_examined"], "2")
        self.assertNotEqual(fields["live_agents_examined"], "0")

    def test_non_active_slots_are_not_judged(self) -> None:
        self.write_registry(
            {
                "live": self.slot("hermit-001"),
                "retired": self.slot("hermit-999", status="quarantined"),
            }
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 0, fields.get("summary"))
        self.assertEqual(fields["active_slots"], "1")

    # --- the refusals ---------------------------------------------------------

    def test_empty_agent_census_refuses_rather_than_passes(self) -> None:
        """The failure mode that makes a monitor worse than none."""
        self.write_registry({"abandoned": self.slot("hermit-999")})
        code, fields = run(self.root, "--test-live-agents", "")
        self.assertEqual(code, 2, "an empty census must be unknown, never clean")
        self.assertEqual(fields["state"], "unknown")
        self.assertNotEqual(code, 0)
        self.assertEqual(fields["live_agents_examined"], "0")

    def test_stale_snapshot_refuses(self) -> None:
        self.write_registry({"one": self.slot("hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "captured_at": time.time() - 65.8 * 3600,
                    "agents": [{"name": "hermit-001"}],
                }
            )
        )
        code, fields = run(
            self.root,
            "--agent-source",
            "snapshot",
            "--agent-snapshot",
            str(snapshot),
        )
        self.assertEqual(code, 2, "a 65.8h-old snapshot must be refused")
        self.assertEqual(fields["state"], "unknown")
        self.assertIn("older than", fields["summary"])

    def test_fresh_snapshot_is_accepted(self) -> None:
        """The staleness bound must not be a blanket refusal of snapshots."""
        self.write_registry({"one": self.slot("hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps({"captured_at": time.time() - 30, "agents": [{"name": "hermit-001"}]})
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 0, fields.get("summary"))
        self.assertEqual(fields["live_agents_examined"], "1")

    def test_snapshot_with_no_identities_refuses(self) -> None:
        self.write_registry({"one": self.slot("hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(json.dumps({"captured_at": time.time(), "agents": []}))
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")

    def test_missing_registry_refuses(self) -> None:
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")

    # --- item 4: the counts nothing else was counting -------------------------

    def test_git_worktree_counts_are_always_reported(self) -> None:
        self.write_registry({"one": self.slot("hermit-001")})
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 0)
        self.assertIn("git_worktrees_hermit", fields)
        self.assertIn("git_worktrees_parent", fields)
        self.assertIn("git_worktrees_total", fields)


if __name__ == "__main__":
    unittest.main()
