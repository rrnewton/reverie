#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import residue_sweep


class ResidueSweepTest(unittest.TestCase):
    def test_live_fleet_passes_the_explicit_database_to_the_shared_resolver(self) -> None:
        database = Path("/tmp/explicit-taskgraph.db")
        completed = mock.Mock(returncode=0, stdout="live agents: worker\n", stderr="")
        resolution = mock.sentinel.resolution
        with mock.patch.object(
            residue_sweep.taskgraph_db, "resolve", return_value=resolution
        ) as resolve, mock.patch.object(
            residue_sweep.taskgraph_db, "child_env", return_value={}
        ), mock.patch.object(
            residue_sweep.bounded_capture, "capture", return_value=completed
        ):
            live, _ = residue_sweep.read_live_fleet(database)
        self.assertEqual(live, {"worker"})
        resolve.assert_called_once_with(database)

    def test_live_fleet_without_a_database_still_refuses(self) -> None:
        with mock.patch.dict(residue_sweep.os.environ, {}, clear=True):
            with self.assertRaisesRegex(
                residue_sweep.SweepUnavailable, "no TaskGraph database bound"
            ):
                residue_sweep.read_live_fleet()

    def _owner_registry(self) -> dict[str, object]:
        return {
            "slots": {
                "owned": {
                    "status": "active",
                    "task": "task-owner",
                    "agents": [
                        {"name": "agent-owner", "task": "task-owner", "read_only": False}
                    ],
                }
            }
        }

    def _owner_authority(self, state: str) -> dict[str, dict[str, object]]:
        identity = None
        if state != "indeterminate":
            identity = {
                "source": "orc-tmux-pane-supervisor-v1",
                "generation": "boot-a:123:456",
                "supervisor_pid": 123,
                "boot_id": "boot-a",
                "start_ticks": 456,
            }
        return {
            "owned": {
                "schema_version": 1,
                "state": state,
                "slot": "owned",
                "agent": "agent-owner" if state != "indeterminate" else None,
                "owner_identity": identity,
            }
        }

    def test_sidecar_dead_owner_is_counted_once(self) -> None:
        result = residue_sweep.guarded_dead_owner_slots(
            self._owner_registry(), self._owner_authority("dead"), set()
        )
        self.assertEqual([item.key for item in result], ["owned"])
        self.assertIn("authority=slot-owner-sidecar", result[0].evidence)

    def test_sidecar_live_owner_is_never_counted_dead(self) -> None:
        registry = self._owner_registry()
        authority = self._owner_authority("alive")
        self.assertEqual(
            residue_sweep.guarded_dead_owner_slots(registry, authority, set()), []
        )
        self.assertEqual(
            residue_sweep.guarded_live_owner_slots(registry, authority), {"owned"}
        )

    def test_sidecar_indeterminate_owner_is_neither_dead_nor_live(self) -> None:
        registry = self._owner_registry()
        authority = self._owner_authority("indeterminate")
        self.assertEqual(
            residue_sweep.guarded_dead_owner_slots(registry, authority, set()), []
        )
        self.assertEqual(
            residue_sweep.guarded_live_owner_slots(registry, authority), set()
        )

    def test_sidecar_indeterminate_owner_is_named_unassessable(self) -> None:
        """The third outcome is now reported instead of leaving no trace."""
        registry = self._owner_registry()
        authority = self._owner_authority("indeterminate")
        self.assertEqual(
            residue_sweep.guarded_unassessable_owner_slots(registry, authority),
            {"owned"},
        )

    def test_a_decided_owner_is_never_also_unassessable(self) -> None:
        """Both halves of the bracket: a decided verdict stays out of the set.

        The failure this pins is a predicate that matched everything, which would
        suppress the reclaim list wholesale and be indistinguishable from the gate
        having stopped classifying.
        """
        registry = self._owner_registry()
        for decided in ("dead", "alive"):
            with self.subTest(state=decided):
                self.assertEqual(
                    residue_sweep.guarded_unassessable_owner_slots(
                        registry, self._owner_authority(decided)
                    ),
                    set(),
                )

    def test_a_released_row_is_never_unassessable(self) -> None:
        """Only a HELD row can be unassessable; a released row is free by status."""
        registry = self._owner_registry()
        registry["slots"]["owned"]["status"] = "released"
        self.assertEqual(
            residue_sweep.guarded_unassessable_owner_slots(
                registry, self._owner_authority("indeterminate")
            ),
            set(),
        )

    def test_a_row_with_only_read_only_owners_is_never_unassessable(self) -> None:
        """An observer is not the incarnation whose death would free the slot."""
        registry = self._owner_registry()
        registry["slots"]["owned"]["agents"] = [
            {"name": "watcher", "task": "task-owner", "read_only": True}
        ]
        self.assertEqual(
            residue_sweep.guarded_unassessable_owner_slots(
                registry, self._owner_authority("indeterminate")
            ),
            set(),
        )

    def _read_authority_fixture(
        self, *, state: str, exit_code: int, generation: str = "boot-a:123:456"
    ) -> dict[str, dict[str, object]]:
        registry = self._owner_registry()
        registry["slots"]["owned"]["owner_sidecar"] = {
            "agent": "agent-owner",
            "generation": "boot-a:123:456",
        }
        payload = self._owner_authority(state)["owned"]
        if isinstance(payload.get("owner_identity"), dict):
            payload["owner_identity"]["generation"] = generation
        digest = hashlib.sha256(
            json.dumps(
                registry, sort_keys=True, separators=(",", ":"), ensure_ascii=True
            ).encode("utf-8")
        ).hexdigest()
        envelope = {
            "schema_version": 1,
            "registry_sha256": digest,
            "authorities": {"owned": payload},
        }
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            script = root / "scripts" / "slot-owner-authority.py"
            script.parent.mkdir()
            script.write_text(
                "import json, sys\n"
                f"print(json.dumps({envelope!r}))\n"
                f"raise SystemExit({exit_code})\n",
                encoding="utf-8",
            )
            return residue_sweep.read_guarded_owner_authorities(registry, root)

    def test_bulk_authority_transport_failure_is_refused(self) -> None:
        with self.assertRaisesRegex(residue_sweep.SweepUnavailable, "rc=2"):
            self._read_authority_fixture(state="dead", exit_code=2)

    def test_authority_generation_must_match_registry_snapshot(self) -> None:
        with self.assertRaisesRegex(residue_sweep.SweepUnavailable, "registry snapshot"):
            self._read_authority_fixture(
                state="alive", exit_code=0, generation="boot-a:123:999"
            )

    def test_authority_state_exit_and_generation_match(self) -> None:
        result = self._read_authority_fixture(state="alive", exit_code=0)
        self.assertEqual(result["owned"]["state"], "alive")

    def test_exact_decline_marker_routes_to_coordinator(self) -> None:
        notes = [residue_sweep.NoteRow(7, "task-a", "worker", "file left untracked")]
        result = residue_sweep.classify_notes(notes, set())
        self.assertEqual(len(result), 1)
        self.assertTrue(result[0].actionable)
        self.assertEqual(result[0].route_authority, "coordinator")
        self.assertEqual(result[0].evidence, "source_note_ids=7")

    def test_owner_authorization_routes_to_owner_queue(self) -> None:
        notes = [residue_sweep.NoteRow(8, "task-b", "worker", "needs authorization")]
        result = residue_sweep.classify_notes(notes, set())
        self.assertEqual(result[0].route_task, residue_sweep.OWNER_QUEUE)
        self.assertEqual(result[0].route_authority, "owner")

    def test_explicit_nothing_left_is_no_action(self) -> None:
        notes = [
            residue_sweep.NoteRow(
                9, "task-c", "worker", "Nothing uncommitted; only build output existed."
            )
        ]
        result = residue_sweep.classify_notes(notes, set())
        self.assertEqual(result[0].disposition, "no-action")
        self.assertFalse(result[0].actionable)

    def test_previous_disposition_suppresses_source_note(self) -> None:
        notes = [residue_sweep.NoteRow(10, "task-d", "worker", "not committed")]
        self.assertEqual(residue_sweep.classify_notes(notes, {10}), [])

    def test_same_task_notes_are_aggregated(self) -> None:
        notes = [
            residue_sweep.NoteRow(11, "task-e", "worker", "not committed"),
            residue_sweep.NoteRow(12, "task-e", "worker", "untracked artifact"),
        ]
        result = residue_sweep.classify_notes(notes, set())
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0].evidence, "source_note_ids=11,12")

    def test_live_owner_prevents_slot_hit(self) -> None:
        registry = {
            "slots": {
                "one": {
                    "status": "active",
                    "agents": [{"name": "live", "task": "task"}],
                }
            }
        }
        self.assertEqual(residue_sweep.classify_slots(registry, {"live"}, set()), [])

    def test_process_cwd_prevents_dead_owner_slot_hit(self) -> None:
        registry = {
            "slots": {
                "two": {
                    "status": "active",
                    "agents": [{"name": "dead", "task": "task"}],
                }
            }
        }
        slot = (residue_sweep.ROOT / "worktrees" / "two").resolve()
        self.assertEqual(
            residue_sweep.classify_slots(registry, set(), {slot / "hermit"}), []
        )

    def test_dead_owner_and_no_process_routes_slot_without_cleaning(self) -> None:
        registry = {
            "slots": {
                "three": {
                    "status": "held",
                    "agents": [{"name": "dead", "task": "task-z"}],
                }
            }
        }
        result = residue_sweep.classify_slots(registry, set(), set())
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0].kind, "held-slot-dead-owner")
        self.assertEqual(result[0].route_authority, "coordinator-slot-lifecycle")
        self.assertIn("live_owner=0 process_cwd_under_slot=0", result[0].evidence)

    def test_released_slot_is_not_residue(self) -> None:
        registry = {
            "slots": {
                "four": {
                    "status": "released",
                    "agents": [{"name": "dead", "task": "task"}],
                }
            }
        }
        self.assertEqual(residue_sweep.classify_slots(registry, set(), set()), [])

    def test_every_item_has_typed_disposition(self) -> None:
        notes = [
            residue_sweep.NoteRow(13, "task-f", "worker", "untracked"),
            residue_sweep.NoteRow(14, "task-g", "worker", "nothing to commit"),
        ]
        for item in residue_sweep.classify_notes(notes, set()):
            self.assertIn(item.disposition, {"route", "no-action"})
            if item.actionable:
                self.assertNotEqual(item.route_authority, "none")
                self.assertNotEqual(item.route_task, "none")


if __name__ == "__main__":
    unittest.main()
