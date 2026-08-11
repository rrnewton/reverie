#!/usr/bin/env python3
"""Mutation-sensitive brackets for the fleet GitHub write boundary."""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SOURCE_SHIM = Path(os.environ.get("FLEET_GH_UNDER_TEST", ROOT / "ci-hub/bin/gh"))
TEST_AGENT = os.environ.get("DG_AGENT_NAME", "codex-boundary")


def owner_row(agent: str = TEST_AGENT, *, sidecar: bool = True) -> dict:
    cgroup = "/user.slice/test-boundary.service"
    task = "test-who-is-talking"
    row = {
        "status": "active",
        "task": task,
        "agents": [{"name": agent, "read_only": False, "task": task}],
        "coordinator_lease": {
            "schema_version": 1,
            "source": "codex-systemd-sentinel-v1",
            "main_pid": 4242,
            "main_pid_starttime": 101,
            "cgroup_path": cgroup,
        },
    }
    if sidecar:
        row["owner_sidecar"] = {
            "schema_version": 1,
            "source": "codex-systemd-sentinel-v1",
            "slot": "slot01",
            "agent": agent,
            "task": task,
            "recorded_at": "2026-08-11T00:00:00Z",
            "generation": "boot:4242:101",
            "supervisor_pid": 4242,
            "boot_id": "boot",
            "start_ticks": 101,
            "tmux_pane_id": None,
            "cgroup_path": cgroup,
        }
    return row


class FleetGhTest(unittest.TestCase):
    def test_installed_boundary_is_executable(self) -> None:
        self.assertTrue(SOURCE_SHIM.stat().st_mode & 0o111, SOURCE_SHIM)

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "ci-hub/bin").mkdir(parents=True)
        (self.root / "ci-hub/lib").mkdir(parents=True)
        self.shim = self.root / "ci-hub/bin/gh"
        shutil.copy2(SOURCE_SHIM, self.shim)
        self.shim.chmod(0o755)
        for dependency in ("agent_identity.py", "taskgraph_db.py"):
            shutil.copy2(ROOT / "ci-hub/lib" / dependency, self.root / "ci-hub/lib" / dependency)

        self.slot = self.root / "worktrees/slot01"
        self.cwd = self.slot / "hermit"
        self.cwd.mkdir(parents=True)
        (self.root / "worktree-state.lock").touch()
        self.write_registry(owner_row())

        self.capture = self.root / "capture.json"
        self.fake = self.root / "real-gh"
        self.fake.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['GH_CAPTURE']).write_text(json.dumps(sys.argv[1:]))\n"
        )
        self.fake.chmod(0o755)
        self.env = os.environ.copy()
        self.env.update(FLEET_REAL_GH=str(self.fake), GH_CAPTURE=str(self.capture))
        self.env["DG_AGENT_NAME"] = TEST_AGENT

    def tearDown(self) -> None:
        self.temp.cleanup()

    def write_registry(self, row: dict) -> None:
        (self.root / "worktree-state.json").write_text(
            json.dumps({"slots": {"slot01": row}})
        )

    @staticmethod
    def identity(
        *, who: str = TEST_AGENT, team: str = "hermit2", role: str = "impl"
    ) -> list[str]:
        return ["--who", who, "--team", team, "--role", role]

    def run_shim(
        self,
        *args: str,
        env: dict[str, str] | None = None,
        cwd: Path | None = None,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.shim), *args],
            env=env or self.env,
            cwd=cwd or self.cwd,
            text=True,
            capture_output=True,
        )

    def captured(self) -> list[str]:
        return json.loads(self.capture.read_text())

    def install_identity_stub(
        self,
        *,
        state: str = "AGREEMENT",
        agent: str | None = TEST_AGENT,
        model: str = "unresolved",
    ) -> None:
        (self.root / "ci-hub/lib/agent_identity.py").write_text(
            "MODEL_UNRESOLVED='unresolved'\n"
            "class Signal:\n"
            "    source='caller-environ'; state='unavailable'; detail='fixture'\n"
            "class Resolution:\n"
            f"    state={state!r}; agent={agent!r}; model={model!r}; "
            "basis=('fixture',); signals=(Signal(),)\n"
            "def resolve_identity(pid): return Resolution()\n"
        )

    def test_missing_who_is_distinct_and_refused_before_write(self) -> None:
        result = self.run_shim(
            "--team", "hermit2", "--role", "impl",
            "pr", "comment", "42", "--body", "anonymous",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("missing required --who", result.stderr)
        self.assertNotIn("mismatch", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_missing_team_or_role_is_refused_before_write(self) -> None:
        cases = (
            (["--who", TEST_AGENT, "--role", "impl"], "missing required --team"),
            (["--who", TEST_AGENT, "--team", "hermit2"], "missing required --role"),
        )
        for supplied, expected in cases:
            with self.subTest(expected=expected):
                self.capture.unlink(missing_ok=True)
                result = self.run_shim(
                    *supplied, "pr", "comment", "42", "--body", "incomplete"
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn(expected, result.stderr)
                self.assertFalse(self.capture.exists())

    def test_mismatched_who_is_distinct_and_refused_before_write(self) -> None:
        result = self.run_shim(
            *self.identity(who="someone-else"),
            "pr", "comment", "42", "--body", "false claim",
        )
        self.assertNotEqual(
            result.returncode,
            0,
            "MISMATCHED --who WAS POSTED after worktree-owner validation disappeared",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("--who mismatch", result.stderr)
        self.assertIn(f"process context resolves to '{TEST_AGENT}'", result.stderr)
        self.assertNotIn("missing required --who", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_matching_who_posts_mechanical_five_field_newline_prefix(self) -> None:
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "pr", "comment", "42", "--body", "evidence",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        machine = socket.gethostname().split(".", 1)[0]
        self.assertEqual(
            body,
            f"[hermit2, {TEST_AGENT}, unresolved, {machine}, role=reviewer]\nevidence",
        )

    def test_resolved_process_model_is_emitted_and_not_caller_typed(self) -> None:
        self.install_identity_stub(model="gpt-5.6-sol")
        result = self.run_shim(
            *self.identity(), "pr", "comment", "42", "--body", "evidence"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        machine = socket.gethostname().split(".", 1)[0]
        self.assertEqual(
            body,
            f"[hermit2, {TEST_AGENT}, gpt-5.6-sol, {machine}, role=impl]\nevidence",
            "RESOLVED MODEL WAS LOST OR REPLACED BY A GUESS",
        )

    def test_caller_supplied_model_is_refused(self) -> None:
        result = self.run_shim(
            *self.identity(), "--model", "forged-model",
            "pr", "comment", "42", "--body", "evidence",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("process-resolved", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_model_text_inside_body_is_not_parsed_as_caller_input(self) -> None:
        result = self.run_shim(
            *self.identity(), "pr", "comment", "42", "--body",
            "--model is process context, not a caller field",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertTrue(body.endswith("\n--model is process context, not a caller field"))

    def test_shared_primary_cwd_uses_proven_process_identity(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "issue", "comment", "42", "--body", "from shared primary",
            cwd=self.root,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertIn(f"[hermit2, {TEST_AGENT},", body)

    def test_missing_sidecar_is_unavailable_but_proven_identity_still_posts(self) -> None:
        self.write_registry(owner_row(sidecar=False))
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body", "legacy slot",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.capture.exists())

    def test_missing_tmux_pane_is_unavailable_but_proven_identity_still_posts(self) -> None:
        row = owner_row()
        row["owner_sidecar"]["source"] = "orc-tmux-pane-supervisor-v1"
        row.pop("coordinator_lease")
        self.write_registry(row)
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body", "unadopted ORC slot",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.capture.exists())

    def test_truly_unresolved_identity_is_explicitly_refused(self) -> None:
        self.install_identity_stub(state="UNRESOLVED", agent=None)
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body", "nobody established",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("owner unresolved", result.stderr)
        self.assertIn("UNRESOLVED", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_role_can_disclose_a_relay(self) -> None:
        result = self.run_shim(
            *self.identity(role="relay"),
            "pr", "comment", "42", "--body", "relaying another verdict",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertIn("role=relay]\n", body)

    def test_all_six_roles_are_accepted_and_free_text_is_refused(self) -> None:
        for role in ("impl", "debugger", "reviewer", "relay", "coordinator", "human"):
            with self.subTest(role=role):
                self.capture.unlink(missing_ok=True)
                result = self.run_shim(
                    *self.identity(role=role),
                    "pr", "comment", "42", "--body", "role bracket",
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"role={role}]\n", self.captured()[-1])
        for role in ("auditor", "tester", "relay:reviewer-verdict"):
            with self.subTest(rejected=role):
                self.capture.unlink(missing_ok=True)
                result = self.run_shim(
                    *self.identity(role=role),
                    "pr", "comment", "42", "--body", "role bracket",
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn("--role must be one of", result.stderr)
                self.assertFalse(self.capture.exists())

    def test_review_without_body_gets_attribution_body(self) -> None:
        result = self.run_shim(
            *self.identity(role="reviewer"), "pr", "review", "42", "--approve"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        machine = socket.gethostname().split(".", 1)[0]
        self.assertEqual(
            self.captured()[-2:],
            [
                "--body",
                f"[hermit2, {TEST_AGENT}, unresolved, {machine}, role=reviewer]\n",
            ],
        )

    def test_raw_api_comment_body_is_attributed(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "api", "-X", "POST", "repos/o/r/issues/42/comments", "-f", "body=evidence",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        machine = socket.gethostname().split(".", 1)[0]
        self.assertIn(
            f"body=[hermit2, {TEST_AGENT}, unresolved, {machine}, role=impl]\nevidence",
            self.captured(),
        )

    def test_raw_api_review_body_is_attributed(self) -> None:
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "api", "-X", "POST", "repos/o/r/pulls/42/reviews", "-f", "body=approved",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = next(value for value in self.captured() if value.startswith("body="))
        self.assertIn("role=reviewer]\napproved", body)

    def test_raw_graphql_comment_mutation_is_refused(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "api", "graphql", "-f",
            "query=mutation { addDiscussionComment(input: {}) }",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("cannot be attributed safely", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_foreign_existing_disclosure_is_refused(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body",
            "[other, somebody, gpt-5.6-sol, devbig999, role=impl]\nwords",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("existing who-is-talking prefix conflicts", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_matching_existing_disclosure_is_not_duplicated(self) -> None:
        machine = socket.gethostname().split(".", 1)[0]
        body = (
            f"[hermit2, {TEST_AGENT}, unresolved, {machine}, role=impl]\n"
            "already attributed"
        )
        result = self.run_shim(
            *self.identity(), "pr", "comment", "42", "--body", body
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        posted = self.captured()[self.captured().index("--body") + 1]
        self.assertEqual(posted, body)
        self.assertEqual(posted.count("[hermit2,"), 1)

    def test_legacy_four_field_prefix_is_refused_not_nested(self) -> None:
        result = self.run_shim(
            *self.identity(), "pr", "comment", "42", "--body",
            f"[hermit2, {TEST_AGENT}, devbig014, role=impl] old body",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("legacy who-is-talking prefix", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_malformed_five_field_prefix_is_refused_not_nested(self) -> None:
        result = self.run_shim(
            *self.identity(), "pr", "comment", "42", "--body",
            f"[hermit2, {TEST_AGENT}, guessed-model, devbig014, role=auditor]\nbody",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("malformed five-field", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_environment_and_ancestry_disagreement_is_an_additional_refusal(self) -> None:
        env = dict(self.env, DG_AGENT_NAME="different-process-agent")
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body", "conflicting process context",
            env=env,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("DISAGREEMENT", result.stderr)
        self.assertNotIn("--who mismatch", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_non_comment_command_needs_no_identity_and_passes_through(self) -> None:
        result = self.run_shim("pr", "view", "42", "--json", "headRefOid")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.captured(), ["pr", "view", "42", "--json", "headRefOid"])

    @unittest.skipIf(
        bool(os.environ.get("FLEET_ATTRIBUTION_MUTATION_CHILD")),
        "mutation child must not recurse",
    )
    def test_mutation_removing_owner_comparison_fails_on_mismatch_post(self) -> None:
        mutant_dir = self.root / "mutant"
        mutant_dir.mkdir()
        mutant = mutant_dir / "gh"
        source = SOURCE_SHIM.read_text()
        needle = "if disclosure.agent != resolution.agent:"
        replacement = (
            "if False and disclosure.agent != resolution.agent:  "
            "# MUTANT: resolved-owner comparison removed"
        )
        self.assertIn(needle, source, "mutation could not locate owner comparison")
        mutant.write_text(source.replace(needle, replacement, 1))
        mutant.chmod(0o755)
        env = dict(
            os.environ,
            FLEET_GH_UNDER_TEST=str(mutant),
            FLEET_ATTRIBUTION_MUTATION_CHILD="1",
        )
        child = subprocess.run(
            [
                sys.executable,
                str(Path(__file__).resolve()),
                "FleetGhTest.test_mismatched_who_is_distinct_and_refused_before_write",
            ],
            env=env,
            text=True,
            capture_output=True,
        )
        combined = child.stdout + child.stderr
        self.assertNotEqual(child.returncode, 0, combined)
        self.assertIn("MISMATCHED --who WAS POSTED", combined)

    @unittest.skipIf(
        bool(os.environ.get("FLEET_ATTRIBUTION_MUTATION_CHILD")),
        "mutation child must not recurse",
    )
    def test_mutation_discarding_resolved_model_is_killed(self) -> None:
        mutant_dir = self.root / "model-mutant"
        mutant_dir.mkdir()
        mutant = mutant_dir / "gh"
        source = SOURCE_SHIM.read_text()
        needle = "return replace(disclosure, model=resolution.model)"
        replacement = (
            "return replace(disclosure, model=agent_identity.MODEL_UNRESOLVED)  "
            "# MUTANT: resolved model discarded"
        )
        self.assertIn(needle, source, "mutation could not locate resolved-model binding")
        mutant.write_text(source.replace(needle, replacement, 1))
        mutant.chmod(0o755)
        env = dict(
            os.environ,
            FLEET_GH_UNDER_TEST=str(mutant),
            FLEET_ATTRIBUTION_MUTATION_CHILD="1",
        )
        child = subprocess.run(
            [
                sys.executable,
                str(Path(__file__).resolve()),
                "FleetGhTest.test_resolved_process_model_is_emitted_and_not_caller_typed",
            ],
            env=env,
            text=True,
            capture_output=True,
        )
        combined = child.stdout + child.stderr
        self.assertNotEqual(child.returncode, 0, combined)
        self.assertIn("RESOLVED MODEL WAS LOST OR REPLACED BY A GUESS", combined)


if __name__ == "__main__":
    unittest.main()
