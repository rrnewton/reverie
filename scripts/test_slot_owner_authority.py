#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("slot-owner-authority.py")
SPEC = importlib.util.spec_from_file_location("slot_owner_authority", MODULE_PATH)
assert SPEC and SPEC.loader
authority = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = authority
SPEC.loader.exec_module(authority)


def process_stat(state: str, start_ticks: int) -> str:
    fields = [state, "0", "0", "0", "0", "0", "0", "0", "0", "0", "0", "0",
              "0", "0", "0", "0", "0", "0", "0", str(start_ticks)]
    return "123 (fixture supervisor) " + " ".join(fields) + "\n"


def state(sidecar: dict[str, object] | None) -> dict[str, object]:
    row: dict[str, object] = {
        "status": "active",
        "task": "task-a",
        "agents": [
            {
                "name": "agent-a",
                "task": "task-a",
                "read_only": False,
                "tmux_pane_id": "%7",
                "cgroup_path": "/agent.slice/a.scope",
            }
        ],
    }
    if sidecar is not None:
        row["owner_sidecar"] = sidecar
    return {"slots": {"slot-a": row}}


def sidecar(**updates: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": 1,
        "source": "orc-tmux-pane-supervisor-v1",
        "slot": "slot-a",
        "agent": "agent-a",
        "task": "task-a",
        "recorded_at": "2026-08-11T00:00:00Z",
        "generation": "boot-a:123:456",
        "supervisor_pid": 123,
        "boot_id": "boot-a",
        "start_ticks": 456,
        "tmux_pane_id": "%7",
        "cgroup_path": "/agent.slice/a.scope",
        "lease": {
            "schema_version": 1,
            "resource": "slot/slot-a",
            "generation": "boot-a:123:456",
            "issued_at": 100,
            "renewed_at": 100,
            "ttl_seconds": 30,
            "expires_at": 130,
            "state": "active",
            "last_transition_actor": "agent-a",
            "automatic_handoff": None,
        },
    }
    value.update(updates)
    return value


class SlotOwnerAuthorityTest(unittest.TestCase):
    def assess(
        self,
        document: dict[str, object],
        *,
        boot: str = "boot-a",
        stat: str | None = None,
    ) -> authority.Verdict:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            proc = root / "proc"
            (proc / "123").mkdir(parents=True)
            if stat is not None:
                (proc / "123" / "stat").write_text(stat, encoding="utf-8")
            boot_path = root / "boot_id"
            boot_path.write_text(boot, encoding="utf-8")
            cgroup_root = root / "cgroup"
            cgroup = cgroup_root / "agent.slice" / "a.scope"
            cgroup.mkdir(parents=True)
            (cgroup / "cgroup.procs").write_text("", encoding="utf-8")
            return authority.assess_state(
                "slot-a",
                document,
                proc_root=proc,
                boot_id_path=boot_path,
                cgroup_root=cgroup_root,
            )

    def test_live_exact_owner_is_never_reported_dead(self) -> None:
        result = self.assess(state(sidecar()), stat=process_stat("S", 456))
        self.assertEqual(result.state, "alive")
        self.assertEqual(result.exit_code, 1)
        self.assertFalse(result.may_recover)

    def test_absent_exact_owner_is_proven_dead(self) -> None:
        result = self.assess(state(sidecar()))
        self.assertEqual(result.state, "dead")
        self.assertEqual(result.reason_code, "owner-pid-absent")
        self.assertEqual(result.exit_code, 0)
        self.assertTrue(result.may_recover)

    def test_reboot_proves_old_owner_dead(self) -> None:
        result = self.assess(
            state(sidecar()), boot="boot-b", stat=process_stat("S", 456)
        )
        self.assertEqual(result.state, "dead")
        self.assertEqual(result.reason_code, "owner-boot-id-mismatch")

    def test_pid_reuse_proves_old_owner_dead(self) -> None:
        result = self.assess(state(sidecar()), stat=process_stat("S", 999))
        self.assertEqual(result.state, "dead")
        self.assertEqual(result.reason_code, "owner-pid-reused")

    def test_missing_legacy_sidecar_is_indeterminate(self) -> None:
        result = self.assess(state(None), stat=process_stat("S", 456))
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.exit_code, 2)
        self.assertFalse(result.may_recover)

    def test_unsupported_sidecar_schema_is_indeterminate(self) -> None:
        result = self.assess(
            state(sidecar(schema_version=2)), stat=process_stat("S", 456)
        )
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.reason_code, "sidecar-invalid")

    def test_malformed_or_unbound_ttl_lease_is_indeterminate(self) -> None:
        value = sidecar()
        value["lease"] = {**value["lease"], "generation": "another-owner"}
        result = self.assess(state(value), stat=process_stat("S", 456))
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.reason_code, "sidecar-invalid")

    def test_legacy_identity_without_ttl_remains_liveness_readable(self) -> None:
        value = sidecar()
        value.pop("lease")
        result = self.assess(state(value), stat=process_stat("S", 456))
        self.assertEqual(result.state, "alive")
        self.assertIsNone(result.owner_identity["lease"])

    def test_binding_mismatch_is_indeterminate(self) -> None:
        result = self.assess(
            state(sidecar(agent="somebody-else")), stat=process_stat("S", 456)
        )
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.reason_code, "sidecar-invalid")

    def test_codex_sidecar_must_match_current_coordinator_lease(self) -> None:
        document = state(
            sidecar(
                source="codex-systemd-sentinel-v1",
                tmux_pane_id=None,
            )
        )
        document["slots"]["slot-a"]["coordinator_lease"] = {
            "schema_version": 1,
            "source": "codex-systemd-sentinel-v1",
            "main_pid": 123,
            "main_pid_starttime": 999,
            "cgroup_path": "/agent.slice/a.scope",
        }
        result = self.assess(document, stat=process_stat("S", 456))
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.reason_code, "sidecar-invalid")

    def test_unreadable_process_identity_is_indeterminate(self) -> None:
        result = self.assess(state(sidecar()), stat="not a stat record\n")
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.reason_code, "owner-process-unreadable")

    def test_absent_pid_with_nonempty_owner_cgroup_is_indeterminate(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            proc = root / "proc"
            proc.mkdir()
            boot_path = root / "boot_id"
            boot_path.write_text("boot-a", encoding="utf-8")
            cgroup_root = root / "cgroup"
            cgroup = cgroup_root / "agent.slice" / "a.scope"
            cgroup.mkdir(parents=True)
            (cgroup / "cgroup.procs").write_text("0\n", encoding="utf-8")
            result = authority.assess_state(
                "slot-a",
                state(sidecar()),
                proc_root=proc,
                boot_id_path=boot_path,
                cgroup_root=cgroup_root,
            )
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.reason_code, "owner-cgroup-nonempty")
        self.assertEqual(result.exit_code, 2)

    def test_absent_pid_with_unreadable_owner_cgroup_is_indeterminate(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            proc = root / "proc"
            proc.mkdir()
            boot_path = root / "boot_id"
            boot_path.write_text("boot-a", encoding="utf-8")
            cgroup_root = root / "not-a-directory"
            cgroup_root.write_text("fixture", encoding="utf-8")
            result = authority.assess_state(
                "slot-a",
                state(sidecar()),
                proc_root=proc,
                boot_id_path=boot_path,
                cgroup_root=cgroup_root,
            )
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.reason_code, "owner-cgroup-unreadable")
        self.assertEqual(result.exit_code, 2)

    def test_guarded_registry_parse_failure_is_indeterminate(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "worktree-state.lock").touch()
            (root / "worktree-state.json").write_text("not json\n", encoding="utf-8")
            result = authority.assess("slot-a", root)
        self.assertEqual(result.state, "indeterminate")
        self.assertEqual(result.reason_code, "registry-unreadable")
        self.assertEqual(result.exit_code, 2)

    def test_bulk_assessment_binds_all_verdicts_to_one_registry_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            document = state(None)
            (root / "worktree-state.lock").touch()
            (root / "worktree-state.json").write_text(
                json.dumps(document), encoding="utf-8"
            )
            proc = root / "proc"
            proc.mkdir()
            boot_path = root / "boot_id"
            boot_path.write_text("boot-a", encoding="utf-8")
            cgroup_root = root / "cgroup"
            cgroup_root.mkdir()
            payload, error = authority.assess_all(
                root,
                proc_root=proc,
                boot_id_path=boot_path,
                cgroup_root=cgroup_root,
            )
        self.assertIsNone(error)
        self.assertIsNotNone(payload)
        assert payload is not None
        self.assertEqual(payload["schema_version"], 1)
        self.assertEqual(payload["registry_sha256"], authority.registry_digest(document))
        self.assertEqual(
            payload["authorities"]["slot-a"]["state"], "indeterminate"
        )

    def test_bulk_registry_failure_stays_indeterminate(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "worktree-state.lock").touch()
            (root / "worktree-state.json").write_text("not json\n", encoding="utf-8")
            payload, error = authority.assess_all(root)
        self.assertIsNone(payload)
        self.assertIn("cannot read guarded slot registry", error or "")


if __name__ == "__main__":
    unittest.main()
