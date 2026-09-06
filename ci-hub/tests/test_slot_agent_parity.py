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
import importlib.util
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
GATE = ROOT / "ci-hub" / "health" / "slot_agent_parity.py"


def _load_gate_module():
    spec = importlib.util.spec_from_file_location("slot_agent_parity", GATE)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["slot_agent_parity"] = module
    spec.loader.exec_module(module)
    return module


parity = _load_gate_module()


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
        """The LIVE wrkslots registry, which is what the gate now reads.

        ⚠️ THESE TESTS USED TO WRITE worktree-state.json AND THEY PASSED WHILE THE
        GATE REPORTED A FOSSIL. That file's producer, scripts/release-worktree.rs,
        refuses every invocation in a project configured with .wrkslots.yml, so
        the real one froze on 2026-08-28 and the gate reported that day's fleet
        for six days. A fixture that writes a file nothing writes in production
        cannot notice that; writing the same registry production writes is what
        makes these tests measure the gate rather than themselves.
        """
        (self.root / ".wrkslots.yml").write_text(
            json.dumps({"machine": "testhost", "worktrees_dir": "worktrees/slots"})
        )
        (self.root / "worktrees" / "ACTIVE.testhost.json").write_text(
            json.dumps(
                {
                    "machine": "testhost",
                    "revision": 1,
                    "schema": 2,
                    "slots": list(slots.values()),
                }
            )
        )

    def slot(
        self,
        name: str,
        agent: str | None,
        slot_type: str = "agent",
    ) -> dict:
        """One wrkslots ACTIVE row.

        There is no `status` field and that is the point: the ACTIVE registry
        holds exactly the live rows and finished ones move to ARCHIVED, so
        PRESENCE is the active status the retired schema spelled out in a field.
        """
        subdir = "validate" if slot_type == "validate" else "slots"
        record: dict = {
            "slot": name,
            "slot_type": slot_type,
            "machine": "testhost",
            "generation": 1,
            "created_at": "2026-09-03T00:00:00Z",
            "agent": agent,
            "checkouts": [
                {
                    "name": "hermit",
                    "repository": "hermit",
                    "path": f"worktrees/{subdir}/{name}/hermit",
                    "branch": "detached",
                }
            ],
        }
        return record

    # --- the predicate itself -------------------------------------------------

    def test_slot_without_a_live_agent_reds_and_names_it(self) -> None:
        self.write_registry(
            {
                "alive": self.slot("alive", "hermit-001"),
                "abandoned": self.slot("abandoned", "hermit-999"),
            }
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 1, "a slot with no live agent must be actionable")
        self.assertEqual(fields["state"], "actionable")
        self.assertEqual(fields["active_slots_without_live_agent"], "1")
        self.assertIn("abandoned", fields["summary"])
        self.assertNotIn("alive", fields["summary"].split(": ")[-1].split(", "))

    def test_every_slot_covered_passes_and_reports_the_denominator(self) -> None:
        self.write_registry(
            {
                "one": self.slot("one", "hermit-001"),
                "two": self.slot("two", "hermit-002"),
            }
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001,hermit-002")
        self.assertEqual(code, 0)
        self.assertEqual(fields["state"], "ok")
        self.assertEqual(fields["active_slots_without_live_agent"], "0")
        # The green verdict must carry its denominator.
        self.assertEqual(fields["live_agents_examined"], "2")
        self.assertNotEqual(fields["live_agents_examined"], "0")

    def test_only_rows_in_the_active_registry_are_judged(self) -> None:
        """A finished slot leaves ACTIVE for ARCHIVED, so it stops being judged
        by being absent rather than by carrying a status."""
        self.write_registry({"live": self.slot("live", "hermit-001")})
        (self.root / "worktrees" / "ARCHIVED.testhost.json").write_text(
            json.dumps(
                {
                    "machine": "testhost",
                    "revision": 1,
                    "schema": 2,
                    "records": [self.slot("retired", "hermit-999")],
                }
            )
        )
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 0, fields.get("summary"))
        self.assertEqual(fields["active_slots"], "1")

    # --- the defect this gate had ---------------------------------------------

    def test_the_figure_moves_when_a_slot_is_actually_reclaimed(self) -> None:
        """THE DEFECT, PINNED. Measured on the live fleet 2026-09-03: a real
        reclaim moved the wrkslots registry from 100 rows to 93 and the git
        worktree census from 181 to 178, and this gate's active_slots stayed at
        exactly 45 -- because it was reading a file whose producer had been
        retired six days earlier. A number that does not move when the thing it
        names is removed is not measuring that thing.
        """
        self.write_registry(
            {
                "alive": self.slot("alive", "hermit-001"),
                "doomed": self.slot("doomed", "hermit-999"),
            }
        )
        code, before = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 1)
        self.assertEqual(before["active_slots"], "2")
        self.assertEqual(before["active_slots_without_live_agent"], "1")

        # Reclaim it, exactly as wrkslots does: the row leaves ACTIVE.
        self.write_registry({"alive": self.slot("alive", "hermit-001")})

        code, after = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(after["active_slots"], "1", "the figure must follow the registry")
        self.assertEqual(after["active_slots_without_live_agent"], "0")
        self.assertEqual(code, 0, after.get("summary"))

    def test_a_validation_slot_is_looked_for_where_it_actually_lives(self) -> None:
        """Validation checkouts sit under worktrees/validate/, not
        worktrees/slots/. Assuming the latter would make a live validation run
        look unoccupied and therefore orphaned."""
        row = self.slot("validate-fresh-abc", None, slot_type="validate")
        self.write_registry({"validate-fresh-abc": row})
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(fields["active_slots"], "1")
        # No live agent and no process inside, so it is named -- the point here
        # is that it was searched for under the validate root without error.
        self.assertEqual(code, 1)
        self.assertIn("validate-fresh-abc", fields["summary"])

    def test_an_empty_registry_refuses_rather_than_reporting_parity(self) -> None:
        """Zero rows computes zero orphans and would read as perfect health, the
        same reason a zero-agent census refuses."""
        self.write_registry({})
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")
        self.assertIn("no slots", fields["summary"])

    def test_an_unconfigured_machine_shard_refuses_rather_than_guessing(self) -> None:
        self.write_registry({"alive": self.slot("alive", "hermit-001")})
        (self.root / ".wrkslots.yml").write_text(json.dumps({"worktrees_dir": "x"}))
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 2)
        self.assertIn("machine shard", fields["summary"])

    # --- the refusals ---------------------------------------------------------

    def test_empty_agent_census_refuses_rather_than_passes(self) -> None:
        """The failure mode that makes a monitor worse than none."""
        self.write_registry({"abandoned": self.slot("abandoned", "hermit-999")})
        code, fields = run(self.root, "--test-live-agents", "")
        self.assertEqual(code, 2, "an empty census must be unknown, never clean")
        self.assertEqual(fields["state"], "unknown")
        self.assertNotEqual(code, 0)
        self.assertEqual(fields["live_agents_examined"], "0")

    def test_stale_snapshot_refuses(self) -> None:
        self.write_registry({"one": self.slot("one", "hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": time.time() - 65.8 * 3600,
                    "agents": [{"name": "hermit-001", "status": "busy"}],
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
        self.write_registry({"one": self.slot("one", "hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": time.time() - 30,
                    "agents": [{"name": "hermit-001", "status": "busy"}],
                }
            )
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 0, fields.get("summary"))
        self.assertEqual(fields["live_agents_examined"], "1")

    def test_terminal_snapshot_agent_does_not_cover_an_active_slot(self) -> None:
        self.write_registry({"one": self.slot("one", "hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": time.time() - 30,
                    "agents": [{"name": "hermit-001", "status": "unreachable"}],
                }
            )
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 2, "a terminal-only census must refuse its empty denominator")
        self.assertEqual(fields["state"], "unknown")
        self.assertIn("no live agent identities", fields["summary"])

    def test_terminal_snapshot_agent_is_excluded_from_a_mixed_denominator(self) -> None:
        self.write_registry(
            {
                "live": self.slot("live", "hermit-001"),
                "terminal": self.slot("terminal", "hermit-002"),
            }
        )
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "captured_at": time.time() - 30,
                    "agents": [
                        {"name": "hermit-001", "status": "busy"},
                        {"name": "hermit-002", "status": "unreachable"},
                    ],
                }
            )
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 1)
        self.assertEqual(fields["live_agents_examined"], "1")
        self.assertEqual(fields["active_slots_without_live_agent"], "1")
        self.assertIn("terminal", fields["summary"])

    def test_snapshot_with_no_identities_refuses(self) -> None:
        self.write_registry({"one": self.slot("one", "hermit-001")})
        snapshot = self.root / "snapshot.json"
        snapshot.write_text(
            json.dumps({"schema_version": 1, "captured_at": time.time(), "agents": []})
        )
        code, fields = run(
            self.root, "--agent-source", "snapshot", "--agent-snapshot", str(snapshot)
        )
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")

    def test_missing_registry_refuses(self) -> None:
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")

    def test_unknown_live_verdict_still_names_the_bound_database(self) -> None:
        missing = self.root / "current-session.db"
        code, fields = run(self.root, "--db", str(missing))
        self.assertEqual(code, 2)
        self.assertEqual(fields["state"], "unknown")
        self.assertEqual(fields["taskgraph_db"], str(missing))

    # --- item 4: the counts nothing else was counting -------------------------

    def test_git_worktree_counts_are_always_reported(self) -> None:
        self.write_registry({"one": self.slot("one", "hermit-001")})
        code, fields = run(self.root, "--test-live-agents", "hermit-001")
        self.assertEqual(code, 0)
        self.assertIn("git_worktrees_hermit", fields)
        self.assertIn("git_worktrees_parent", fields)
        self.assertIn("git_worktrees_total", fields)


class TaskGraphBindingTests(unittest.TestCase):
    def test_explicit_database_wins_without_running_discovery(self) -> None:
        with mock.patch.object(parity.subprocess, "run") as discover:
            path = parity.live_taskgraph_db(Path("/tmp/explicit.db"))
        self.assertEqual(path, Path("/tmp/explicit.db"))
        discover.assert_not_called()

    def test_environment_database_wins_without_running_discovery(self) -> None:
        with mock.patch.dict(
            os.environ, {"TG_DB_PATH": "/tmp/current-session.db"}
        ), mock.patch.object(parity.subprocess, "run") as discover:
            path = parity.live_taskgraph_db()
        self.assertEqual(path, Path("/tmp/current-session.db"))
        discover.assert_not_called()

    def test_unbound_tick_derives_database_from_live_coordinator(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[], returncode=0, stdout="/home/test/.tg/hermit3.db\n", stderr=""
        )
        with mock.patch.dict(os.environ, {}, clear=False), mock.patch.object(
            parity.subprocess, "run", return_value=completed
        ) as discover:
            os.environ.pop("TG_DB_PATH", None)
            path = parity.live_taskgraph_db()
        self.assertEqual(path, Path("/home/test/.tg/hermit3.db"))
        self.assertIn("--print-db-path", discover.call_args.args[0])

    def test_failed_live_discovery_refuses_instead_of_guessing(self) -> None:
        completed = subprocess.CompletedProcess(
            args=[], returncode=1, stdout="", stderr="two Hermit coordinators"
        )
        with mock.patch.dict(os.environ, {}, clear=False), mock.patch.object(
            parity.subprocess, "run", return_value=completed
        ):
            os.environ.pop("TG_DB_PATH", None)
            with self.assertRaises(parity.ParityUnavailable) as caught:
                parity.live_taskgraph_db()
        self.assertIn("two Hermit coordinators", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
