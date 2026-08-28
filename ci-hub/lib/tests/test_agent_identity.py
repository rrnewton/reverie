#!/usr/bin/env python3
"""Bracket process-derived agent identity across every known fleet blind spot."""

from __future__ import annotations

import json
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

LIB = Path(__file__).resolve().parents[1]
ROOT = LIB.parents[1]
sys.path.insert(0, str(LIB))

import agent_identity  # noqa: E402


def stat_record(pid: int, ppid: int) -> str:
    return f"{pid} (fixture process) S {ppid} 0 0 0 0\n"


class IdentityFixture(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.proc = self.root / "proc"
        self.proc.mkdir()
        self.repo = self.root / "repo"
        (self.repo / "worktrees" / "slots" / "slot-a" / "hermit").mkdir(parents=True)
        self.state = self.repo / "worktree-state.json"
        self.db = self.root / "tasks.db"
        connection = sqlite3.connect(self.db)
        connection.execute(
            "CREATE TABLE tasks (local_id TEXT PRIMARY KEY, fbid TEXT, status TEXT, owner TEXT)"
        )
        connection.execute(
            "INSERT INTO tasks VALUES ('task-a', NULL, 'IN_PROGRESS', 'agent-a')"
        )
        connection.commit()
        connection.close()
        self.write_state()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def write_state(
        self,
        *,
        owners: tuple[str, ...] = ("agent-a",),
        pane: str | None = "%7",
        cgroup: str | None = "/agent.slice/a.scope",
        task: str = "task-a",
        sidecar_agent: str | None = "agent-a",
    ) -> None:
        row = self.registry_row(
            "slot-a",
            owners=owners,
            pane=pane,
            cgroup=cgroup,
            task=task,
            sidecar_agent=sidecar_agent,
        )
        self.state.write_text(
            json.dumps(
                {
                    "version": 3,
                    "updated": "2026-08-28T00:00:00Z",
                    "slots": {"slot-a": row},
                }
            )
        )

    def registry_row(
        self,
        slot: str,
        *,
        owners: tuple[str, ...],
        pane: str | None,
        cgroup: str | None = None,
        task: str = "task-a",
        status: str = "active",
        sidecar_agent: str | None = None,
    ) -> dict[str, object]:
        agents = []
        for owner in owners:
            entry: dict[str, object] = {"name": owner, "task": task, "read_only": False}
            if pane is not None:
                entry["tmux_pane_id"] = pane
            if cgroup is not None:
                entry["cgroup_path"] = cgroup
            agents.append(entry)
        row: dict[str, object] = {
            "status": status,
            "task": task,
            "allocated": "2026-08-28T00:00:00Z",
            "updated": "2026-08-28T00:00:00Z",
            "hermit_path": f"worktrees/slots/{slot}/hermit",
            "hermit_branch": "detached",
            "reverie_path": f"worktrees/slots/{slot}/reverie",
            "reverie_branch": "-",
            "liteinst2_path": f"worktrees/slots/{slot}/liteinst2",
            "liteinst2_branch": "-",
            "agents": agents,
        }
        if sidecar_agent is not None:
            sidecar: dict[str, object] = {
                "schema_version": 1,
                "agent": sidecar_agent,
                "supervisor_pid": 50,
            }
            if pane is not None:
                sidecar["tmux_pane_id"] = pane
            if cgroup is not None:
                sidecar["cgroup_path"] = cgroup
            row["owner_sidecar"] = sidecar
        return row

    def add_process(
        self,
        pid: int,
        ppid: int,
        *,
        agent: str | None = None,
        pane: str | None = None,
        cwd: Path | str | None = None,
        cgroup: str | None = None,
        environ: bool = True,
        cmdline: tuple[str, ...] | None = ("fixture-process",),
    ) -> None:
        process = self.proc / str(pid)
        process.mkdir()
        (process / "stat").write_text(stat_record(pid, ppid))
        if environ:
            values = []
            if agent is not None:
                values.append(f"DG_AGENT_NAME={agent}")
            if pane is not None:
                values.append(f"TMUX_PANE={pane}")
            (process / "environ").write_bytes(("\0".join(values) + "\0").encode())
        if cwd is not None:
            (process / "cwd").symlink_to(str(cwd))
        if cgroup is not None:
            (process / "cgroup").write_text(f"0::{cgroup}\n")
        if cmdline is not None:
            (process / "cmdline").write_bytes(
                ("\0".join(cmdline) + "\0").encode()
            )

    def standard_processes(
        self,
        *,
        caller_agent: str | None = "agent-a",
        parent_agent: str | None = "agent-a",
        cwd: Path | str | None = None,
        pane: str | None = "%7",
        cgroup: str | None = "/agent.slice/a.scope",
        caller_cmdline: tuple[str, ...] = ("fixture-process",),
        parent_cmdline: tuple[str, ...] = ("fixture-process",),
    ) -> None:
        self.add_process(1, 0, environ=True)
        self.add_process(50, 1, agent=parent_agent, cmdline=parent_cmdline)
        self.add_process(
            100,
            50,
            agent=caller_agent,
            pane=pane,
            cwd=(
                cwd
                if cwd is not None
                else self.repo / "worktrees" / "slots" / "slot-a" / "hermit"
            ),
            cgroup=cgroup,
            cmdline=caller_cmdline,
        )

    def resolve(self, *, task: str | None = "task-a") -> agent_identity.Resolution:
        return agent_identity.resolve_identity(
            100,
            proc_root=self.proc,
            repo_root=self.repo,
            state_path=self.state,
            task_id=task,
            task_db=self.db,
        )


class ResolutionTests(IdentityFixture):
    def test_all_readable_signals_agree(self) -> None:
        self.standard_processes()
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        self.assertEqual("agent-a", result.agent)
        self.assertEqual(0, result.exit_code)
        self.assertEqual("caller-environ", result.primary_source)
        self.assertTrue(
            {"caller-environ", "process-ancestry", "cwd-slot-owner", "pane-owner", "cgroup-owner", "supervisor-owner", "taskgraph-owner"}
            <= set(result.basis)
        )

    def test_relay_disagreement_reports_no_winner(self) -> None:
        self.write_state(owners=("agent-b",), sidecar_agent="agent-b")
        self.standard_processes()
        result = self.resolve(task=None)
        self.assertEqual("DISAGREEMENT", result.state)
        self.assertIsNone(result.agent)
        self.assertEqual(1, result.exit_code)

    def test_slot_claim_without_process_identity_is_unresolved(self) -> None:
        self.standard_processes(caller_agent=None, parent_agent=None)
        result = self.resolve()
        self.assertEqual("UNRESOLVED", result.state)
        self.assertIsNone(result.agent)
        self.assertEqual(2, result.exit_code)
        self.assertIn("cwd-slot-owner", result.basis)

    def test_nearest_ancestor_identity_can_supply_the_inherited_primary(self) -> None:
        self.standard_processes(caller_agent=None, parent_agent="agent-a")
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        self.assertEqual("agent-a", result.agent)
        self.assertEqual("process-ancestry", result.primary_source)

    def test_different_caller_and_ancestor_identities_disagree(self) -> None:
        self.standard_processes(caller_agent="agent-a", parent_agent="agent-b")
        result = self.resolve(task=None)
        self.assertEqual("DISAGREEMENT", result.state)
        self.assertIsNone(result.agent)

    def test_invalid_nearer_identity_does_not_fall_through_to_plausible_ancestor(self) -> None:
        self.standard_processes(caller_agent="not a valid identity", parent_agent="agent-a")
        result = self.resolve()
        self.assertEqual("UNRESOLVED", result.state)
        self.assertIsNone(result.agent)
        self.assertTrue(
            any(signal.state == "error" and "invalid DG_AGENT_NAME" in signal.detail for signal in result.signals)
        )

    def test_nearest_ancestor_can_supply_missing_pane_context(self) -> None:
        self.write_state()
        self.add_process(1, 0, environ=True)
        self.add_process(50, 1, agent="agent-a", pane="%7")
        self.add_process(
            100,
            50,
            agent="agent-a",
            cwd=self.repo / "worktrees" / "slots" / "slot-a" / "hermit",
            cgroup="/agent.slice/a.scope",
        )
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        pane = next(signal for signal in result.signals if signal.source == "pane-owner")
        self.assertEqual("resolved", pane.state)
        self.assertEqual("agent-a", pane.agent)

    def test_unreadable_cwd_is_explicit_but_does_not_erase_proven_identity(self) -> None:
        self.standard_processes(cwd=None)
        (self.proc / "100" / "cwd").unlink(missing_ok=True)
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        cwd = next(signal for signal in result.signals if signal.source == "cwd-slot-owner")
        self.assertEqual("unavailable", cwd.state)
        self.assertIn("unreadable cwd", cwd.detail)

    def test_deleted_cwd_is_explicit_and_never_guessed(self) -> None:
        self.standard_processes(cwd="/gone/slot (deleted)")
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        cwd = next(signal for signal in result.signals if signal.source == "cwd-slot-owner")
        self.assertEqual("unavailable", cwd.state)
        self.assertIn("deleted cwd", cwd.detail)

    def test_unreadable_cwd_without_process_identity_is_unresolved(self) -> None:
        self.standard_processes(caller_agent=None, parent_agent=None)
        (self.proc / "100" / "cwd").unlink()
        result = self.resolve(task=None)
        self.assertEqual("UNRESOLVED", result.state)
        self.assertIsNone(result.agent)
        cwd = next(signal for signal in result.signals if signal.source == "cwd-slot-owner")
        self.assertEqual("unavailable", cwd.state)
        self.assertIn("unreadable cwd", cwd.detail)

    def test_deleted_cwd_without_process_identity_is_unresolved(self) -> None:
        self.standard_processes(
            caller_agent=None,
            parent_agent=None,
            cwd="/gone/slot (deleted)",
        )
        result = self.resolve(task=None)
        self.assertEqual("UNRESOLVED", result.state)
        self.assertIsNone(result.agent)
        cwd = next(signal for signal in result.signals if signal.source == "cwd-slot-owner")
        self.assertEqual("unavailable", cwd.state)
        self.assertIn("deleted cwd", cwd.detail)

    def test_shared_primary_cwd_resolves_from_process_not_cwd(self) -> None:
        self.standard_processes(cwd=self.repo)
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        self.assertEqual("agent-a", result.agent)
        cwd = next(signal for signal in result.signals if signal.source == "cwd-slot-owner")
        self.assertEqual("unavailable", cwd.state)
        self.assertIn("outside registered slots", cwd.detail)

    def test_stale_branch_metadata_cannot_change_physical_slot_owner_binding(self) -> None:
        document = json.loads(self.state.read_text())
        document["slots"]["slot-a"]["hermit_branch"] = "detached:deadbeef"
        self.state.write_text(json.dumps(document))
        self.standard_processes()
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        cwd = next(signal for signal in result.signals if signal.source == "cwd-slot-owner")
        self.assertEqual("agent-a", cwd.agent)
        self.assertIn("branch metadata is not identity authority", cwd.detail)

    def test_missing_pane_metadata_is_visible_without_guessing(self) -> None:
        self.write_state(pane=None)
        self.standard_processes()
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        pane = next(signal for signal in result.signals if signal.source == "pane-owner")
        self.assertEqual("unavailable", pane.state)

    def test_guarded_sidecar_cgroup_corroborates_legacy_agent_row(self) -> None:
        document = json.loads(self.state.read_text())
        del document["slots"]["slot-a"]["agents"][0]["cgroup_path"]
        self.state.write_text(json.dumps(document))
        self.standard_processes()
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        cgroup = next(signal for signal in result.signals if signal.source == "cgroup-owner")
        self.assertEqual("resolved", cgroup.state)
        self.assertEqual("agent-a", cgroup.agent)

    def test_released_slot_pane_claim_is_ignored(self) -> None:
        """A released slot owns nothing, so its stale pane claim must not answer.

        Releasing a slot does not clear `tmux_pane_id`. Measured 2026-08-25: all
        16 pane-carrying records in the live registry sat on released slots, and
        one of them shadowed a pane a different live session held, which blocked
        every guarded write.
        """
        document = json.loads(self.state.read_text())
        document["slots"]["stale-slot"] = self.registry_row(
            "stale-slot",
            owners=("ghost-agent",),
            pane="%7",
            status="released",
        )
        self.state.write_text(json.dumps(document))
        self.standard_processes()
        result = self.resolve()
        pane = next(signal for signal in result.signals if signal.source == "pane-owner")
        self.assertNotIn("ghost-agent", (pane.candidates or ()))
        self.assertEqual(
            "resolved", pane.state, "a released slot still shadowed a live pane"
        )
        self.assertEqual("agent-a", pane.agent)
        self.assertEqual("AGREEMENT", result.state)

    def test_live_slot_pane_disagreement_STILL_REFUSES(self) -> None:
        """The other direction, and the more important one.

        A resolver that stops dissenting is worse than one that over-dissents.
        Skipping released rows must not soften a genuine conflict: an ACTIVE slot
        claiming the caller's pane for a different agent must still refuse.
        """
        document = json.loads(self.state.read_text())
        document["slots"]["live-conflict"] = self.registry_row(
            "live-conflict",
            owners=("other-agent",),
            pane="%7",
        )
        self.state.write_text(json.dumps(document))
        self.standard_processes()
        result = self.resolve()
        pane = next(signal for signal in result.signals if signal.source == "pane-owner")
        # Two LIVE slots claim the caller's pane, so the signal is ambiguous --
        # which is the correct form of dissent here, not a resolved rival.
        self.assertEqual("ambiguous", pane.state)
        self.assertIn("other-agent", pane.candidates)
        self.assertEqual(
            "DISAGREEMENT", result.state, "a live conflicting claim stopped dissenting"
        )
        self.assertIsNone(result.agent)

    def test_release_requested_slot_still_dissents(self) -> None:
        """Only `released` is skipped; a slot mid-release may still be live.

        Suppressing this one would hide a real conflict, so it is asserted
        explicitly rather than left to the reader of the filter.
        """
        document = json.loads(self.state.read_text())
        document["slots"]["mid-release"] = self.registry_row(
            "mid-release",
            owners=("departing-agent",),
            pane="%7",
            status="release-requested",
        )
        self.state.write_text(json.dumps(document))
        self.standard_processes()
        result = self.resolve()
        pane = next(signal for signal in result.signals if signal.source == "pane-owner")
        self.assertEqual("ambiguous", pane.state)
        self.assertIn("departing-agent", pane.candidates)
        self.assertEqual("DISAGREEMENT", result.state)

    def test_taskgraph_owner_mismatch_is_disagreement(self) -> None:
        connection = sqlite3.connect(self.db)
        connection.execute("UPDATE tasks SET owner='agent-b' WHERE local_id='task-a'")
        connection.commit()
        connection.close()
        self.standard_processes()
        result = self.resolve()
        self.assertEqual("DISAGREEMENT", result.state)
        self.assertIsNone(result.agent)

    def test_ambiguous_registered_owners_are_disagreement(self) -> None:
        self.write_state(owners=("agent-a", "agent-b"), sidecar_agent=None)
        self.standard_processes()
        result = self.resolve(task=None)
        self.assertEqual("DISAGREEMENT", result.state)
        slot = next(signal for signal in result.signals if signal.source == "cwd-slot-owner")
        self.assertEqual(("agent-a", "agent-b"), slot.candidates)

    def test_corrupt_registry_is_reported_but_primary_still_resolves(self) -> None:
        self.state.write_text("not json\n")
        self.standard_processes()
        result = self.resolve(task=None)
        self.assertEqual("AGREEMENT", result.state)
        self.assertEqual("agent-a", result.agent)
        self.assertTrue(any("cannot read" in signal.detail for signal in result.signals))

    def test_nonexistent_process_is_unresolved(self) -> None:
        result = agent_identity.resolve_identity(
            999,
            proc_root=self.proc,
            repo_root=self.repo,
            state_path=self.state,
            task_db=self.db,
        )
        self.assertEqual("UNRESOLVED", result.state)
        self.assertEqual(2, result.exit_code)

    def test_schema_two_json_never_asserts_agent_on_disagreement(self) -> None:
        self.write_state(owners=("agent-b",), sidecar_agent="agent-b")
        self.standard_processes()
        payload = json.loads(self.resolve(task=None).to_json())
        self.assertEqual(2, payload["schema"])
        self.assertEqual("DISAGREEMENT", payload["state"])
        self.assertIsNone(payload["agent"])
        self.assertEqual("unresolved", payload["model"])

    def test_explicit_codex_model_is_resolved_from_process_cmdline(self) -> None:
        self.standard_processes(
            caller_cmdline=("codex", "--model", "gpt-5.6-sol", "--sandbox", "workspace-write")
        )
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        self.assertEqual("gpt-5.6-sol", result.model)
        self.assertEqual("process-cmdline", result.model_source)
        signal = next(s for s in result.signals if s.source == "process-model")
        self.assertEqual("resolved", signal.state)

    def test_codex_client_family_is_resolved_without_an_explicit_model(self) -> None:
        self.standard_processes(caller_cmdline=("codex", "exec", "task"))
        result = self.resolve()
        self.assertEqual("codex", result.client_family)
        signal = next(s for s in result.signals if s.source == "client-family")
        self.assertEqual("resolved", signal.state)

    def test_mixed_client_ancestry_has_no_single_review_family(self) -> None:
        self.standard_processes(
            caller_cmdline=("codex", "exec", "task"),
            parent_cmdline=("claude", "--print"),
        )
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        self.assertIsNone(result.client_family)
        signal = next(s for s in result.signals if s.source == "client-family")
        self.assertEqual("ambiguous", signal.state)
        self.assertEqual(("claude", "codex"), signal.candidates)

    def test_cli_model_slug_is_passed_through_verbatim_without_mapping(self) -> None:
        self.standard_processes(
            caller_cmdline=("codex", "--model", "vendor/model:vNext+preview")
        )
        result = self.resolve()
        self.assertEqual("vendor/model:vNext+preview", result.model)

    def test_literal_model_punctuation_is_not_normalized(self) -> None:
        self.standard_processes(
            caller_cmdline=("codex", "--model", "'gpt-5.6-sol'")
        )
        result = self.resolve()
        self.assertEqual("'gpt-5.6-sol'", result.model)

    def test_explicit_claude_model_equals_form_is_resolved(self) -> None:
        self.standard_processes(
            parent_cmdline=("/usr/local/bin/claude", "--model=claude-opus-5")
        )
        result = self.resolve()
        self.assertEqual("claude-opus-5", result.model)

    def test_codex_config_model_is_resolved_but_reasoning_effort_is_not(self) -> None:
        self.standard_processes(
            caller_cmdline=(
                "codex",
                "--config",
                "model_reasoning_effort=high",
                "-c",
                "model=gpt-5.6-sol",
            )
        )
        result = self.resolve()
        self.assertEqual("gpt-5.6-sol", result.model)

    def test_absent_explicit_model_is_literal_unresolved_not_a_guess(self) -> None:
        self.standard_processes(caller_cmdline=("codex", "exec", "task"))
        result = self.resolve()
        self.assertEqual("unresolved", result.model)
        self.assertIsNone(result.model_source)

    def test_model_text_on_non_client_shell_cmdline_is_not_parsed(self) -> None:
        self.standard_processes(
            caller_cmdline=("bash", "-lc", "post body containing --model gpt-forged")
        )
        result = self.resolve()
        self.assertEqual("unresolved", result.model)

    def test_conflicting_explicit_models_are_unresolved_not_nearest_guess(self) -> None:
        self.standard_processes(
            caller_cmdline=("codex", "--model", "gpt-5.6-sol"),
            parent_cmdline=("claude", "--model", "claude-opus-5"),
        )
        result = self.resolve()
        self.assertEqual("AGREEMENT", result.state)
        self.assertEqual("unresolved", result.model)
        signal = next(s for s in result.signals if s.source == "process-model")
        self.assertEqual("ambiguous", signal.state)


class CliTests(IdentityFixture):
    def run_json(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(ROOT / "ci-hub" / "bin" / "who-am-i"),
                "--pid", "100",
                "--proc-root", str(self.proc),
                "--repo-root", str(self.repo),
                "--state", str(self.state),
                "--db", str(self.db),
                "--json",
                *extra,
            ],
            text=True,
            capture_output=True,
            check=False,
        )

    def run_default(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(ROOT / "ci-hub" / "bin" / "who-am-i"),
                "--pid", "100",
                "--proc-root", str(self.proc),
                "--repo-root", str(self.repo),
                "--state", str(self.state),
                "--db", str(self.db),
                *extra,
            ],
            text=True,
            capture_output=True,
            check=False,
        )

    def run_tag(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(ROOT / "ci-hub" / "bin" / "who-am-i"),
                "--pid", "100",
                "--proc-root", str(self.proc),
                "--repo-root", str(self.repo),
                "--state", str(self.state),
                "--db", str(self.db),
                "--tag",
                *extra,
            ],
            text=True,
            capture_output=True,
            check=False,
        )

    def test_cli_exit_codes_are_agreement_disagreement_unresolved(self) -> None:
        self.standard_processes()
        agreed = self.run_json("--task", "task-a")
        self.assertEqual(0, agreed.returncode, agreed.stderr)
        self.assertEqual("agent-a", json.loads(agreed.stdout)["agent"])

        self.write_state(owners=("agent-b",), sidecar_agent="agent-b")
        disagreed = self.run_json()
        self.assertEqual(1, disagreed.returncode)
        self.assertIsNone(json.loads(disagreed.stdout)["agent"])

        self.write_state()
        (self.proc / "100" / "environ").write_bytes(b"TMUX_PANE=%7\0")
        (self.proc / "50" / "environ").write_bytes(b"\0")
        unresolved = self.run_json()
        self.assertEqual(2, unresolved.returncode)
        self.assertEqual("UNRESOLVED", json.loads(unresolved.stdout)["state"])

    def test_default_is_canonical_impl_tag_and_exit_zero(self) -> None:
        self.standard_processes(
            caller_cmdline=("codex", "--model", "gpt-5.6-sol"),
            parent_cmdline=("codex", "--model", "gpt-5.6-sol"),
        )
        result = self.run_default("--task", "task-a")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual(
            "[hermit2, agent-a, gpt-5.6-sol, "
            + __import__("socket").gethostname().split(".", 1)[0]
            + ", role=impl]",
            result.stdout,
        )
        self.assertEqual(
            "who-am-i: fields: team, agent, model, machine, role\n",
            result.stderr,
        )

    def test_default_accepts_role_without_requiring_tag_flag(self) -> None:
        self.standard_processes()
        result = self.run_default("--role", "reviewer")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("role=reviewer]", result.stdout)

    def test_default_degrades_disagreement_without_failing(self) -> None:
        self.standard_processes()
        self.write_state(owners=("agent-b",), sidecar_agent="agent-b")
        result = self.run_default()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("[hermit2, degraded-agent-a, unresolved, ", result.stdout)
        self.assertTrue(result.stdout.endswith(", role=impl]"))
        self.assertIn("fields: team, agent, model, machine, role", result.stderr)
        self.assertIn("DEGRADED", result.stderr)
        self.assertNotIn("signal source=", result.stdout)

    def test_default_unresolved_never_invents_an_identity(self) -> None:
        self.write_state(owners=(), sidecar_agent=None)
        self.standard_processes(
            caller_agent=None,
            parent_agent=None,
            cwd=self.repo,
            pane=None,
            cgroup=None,
            caller_cmdline=("bash", "-c", "./ci-hub/bin/who-am-i"),
            parent_cmdline=("fixture-process",),
        )
        result = self.run_default()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("[hermit2, degraded-unresolved, unresolved, ", result.stdout)
        self.assertTrue(result.stdout.endswith(", role=impl]"))
        self.assertIn("DEGRADED", result.stderr)

    def test_interactive_shell_without_agent_identity_is_human(self) -> None:
        self.write_state(owners=(), sidecar_agent=None)
        self.standard_processes(
            caller_agent=None,
            parent_agent=None,
            cwd=self.repo,
            pane=None,
            cgroup=None,
            caller_cmdline=("bash", "-i"),
            parent_cmdline=("fixture-process",),
        )
        result = self.run_default()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("[hermit2, human, unresolved, ", result.stdout)
        self.assertTrue(result.stdout.endswith(", role=human]"))
        self.assertIn("fields: team, agent, model, machine, role", result.stderr)
        self.assertNotIn("DEGRADED", result.stderr)

        explicit = self.run_default("--role", "reviewer")
        self.assertEqual(0, explicit.returncode, explicit.stderr)
        self.assertIn("[hermit2, human, unresolved, ", explicit.stdout)
        self.assertTrue(explicit.stdout.endswith(", role=reviewer]"))

    def test_interpreter_between_command_and_shell_is_still_human(self) -> None:
        self.write_state(owners=(), sidecar_agent=None)
        self.add_process(1, 0, environ=True)
        self.add_process(50, 1, cmdline=("bash", "-i"))
        self.add_process(
            100,
            50,
            cwd=self.repo,
            cmdline=("python3", "./ci-hub/bin/who-am-i"),
        )
        result = self.run_default()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("[hermit2, human, unresolved, ", result.stdout)
        self.assertTrue(result.stdout.endswith(", role=human]"))

    def test_agent_client_ancestry_is_not_misreported_as_human(self) -> None:
        self.write_state(owners=(), sidecar_agent=None)
        self.standard_processes(
            caller_agent=None,
            parent_agent=None,
            cwd=self.repo,
            pane=None,
            cgroup=None,
            caller_cmdline=("bash", "-i"),
            parent_cmdline=("codex", "exec", "task"),
        )
        result = self.run_default()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("[hermit2, degraded-unresolved, unresolved, ", result.stdout)
        self.assertTrue(result.stdout.endswith(", role=impl]"))
        self.assertIn("DEGRADED", result.stderr)

    def test_diagnostics_are_explicit_and_stay_off_stdout(self) -> None:
        self.standard_processes()
        result = self.run_default("--diagnostics")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("[hermit2, agent-a, unresolved, ", result.stdout)
        self.assertTrue(result.stdout.endswith(", role=impl]"))
        self.assertIn("state=AGREEMENT", result.stderr)
        self.assertIn("signal source=caller-environ", result.stderr)

    def test_help_describes_output_and_shows_a_worked_example(self) -> None:
        result = subprocess.run(
            [sys.executable, str(ROOT / "ci-hub" / "bin" / "who-am-i"), "--help"],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("Print the caller's five-field disclosure tag", result.stdout)
        self.assertIn("./ci-hub/bin/who-am-i", result.stdout)
        self.assertIn(
            "[hermit2, codex-2, gpt-5.6-sol, devbig014, role=impl]",
            result.stdout,
        )
        self.assertIn("default: impl for agents; human for an", result.stdout)
        self.assertIn("interactive shell", result.stdout)

    def test_tag_is_exact_verbatim_model_and_has_no_trailing_newline(self) -> None:
        self.standard_processes(
            caller_cmdline=("codex", "--model", "gpt-5.6-sol"),
            parent_cmdline=("codex", "--model", "gpt-5.6-sol"),
        )
        result = self.run_tag("--role", "impl", "--task", "task-a")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual(
            "[hermit2, agent-a, gpt-5.6-sol, "
            + __import__("socket").gethostname().split(".", 1)[0]
            + ", role=impl]",
            result.stdout,
        )
        self.assertFalse(result.stdout.endswith("\n"))

    def test_tag_defaults_role_and_degrades_disagreement(self) -> None:
        self.standard_processes()
        missing_role = self.run_tag()
        self.assertEqual(0, missing_role.returncode, missing_role.stderr)
        self.assertIn("role=impl]", missing_role.stdout)
        self.assertIn("fields: team, agent, model, machine, role", missing_role.stderr)

        self.write_state(owners=("agent-b",), sidecar_agent="agent-b")
        disagreement = self.run_tag("--role", "impl")
        self.assertEqual(0, disagreement.returncode, disagreement.stderr)
        self.assertIn("degraded-agent-a", disagreement.stdout)
        self.assertIn("DEGRADED", disagreement.stderr)
        self.assertIn("DISAGREEMENT", disagreement.stderr)


if __name__ == "__main__":
    unittest.main()
