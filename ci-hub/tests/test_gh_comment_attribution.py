#!/usr/bin/env python3
"""Mutation-sensitive brackets for the fleet GitHub write boundary."""

from __future__ import annotations

import json
import importlib.machinery
import importlib.util
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub"))
from closure import publication_record  # noqa: E402
sys.path.insert(0, str(ROOT / "ci-hub/landing"))
import task_pr_record  # noqa: E402

SOURCE_SHIM = Path(os.environ.get("FLEET_GH_UNDER_TEST", ROOT / "ci-hub/bin/gh"))
SOURCE_DISCLOSURE = Path(
    os.environ.get(
        "FLEET_DISCLOSURE_TAG_UNDER_TEST",
        ROOT / "ci-hub/lib/disclosure_tag.py",
    )
)
TEST_AGENT = os.environ.get("DG_AGENT_NAME", "codex-boundary")
HEAD = "b3ca4e155098ad6e6f32f507ed72855da0638010"


def load_changes_requested_gate():
    path = ROOT / "ci-hub/bin/changes-requested-gate"
    loader = importlib.machinery.SourceFileLoader("gh_boundary_gate", str(path))
    spec = importlib.util.spec_from_loader("gh_boundary_gate", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def owner_row(agent: str = TEST_AGENT, *, sidecar: bool = True) -> dict:
    generation = "0123456789abcdef"
    unit = f"dev-hermit-slot-slot01-{generation}.service"
    cgroup = f"/user.slice/{unit}"
    task = "test-who-is-talking"
    row = {
        "status": "active",
        "task": task,
        "allocated": "2026-08-28T00:00:00Z",
        "updated": "2026-08-28T00:00:00Z",
        "hermit_path": "worktrees/slots/slot01/hermit",
        "hermit_branch": "detached",
        "reverie_path": "worktrees/slots/slot01/reverie",
        "reverie_branch": "-",
        "liteinst2_path": "worktrees/slots/slot01/liteinst2",
        "liteinst2_branch": "-",
        "agents": [{"name": agent, "read_only": False, "task": task}],
        "coordinator_lease": {
            "schema_version": 1,
            "source": "codex-systemd-sentinel-v1",
            "slot": "slot01",
            "generation": generation,
            "nonce": "0123456789abcdef0123456789abcdef",
            "unit": unit,
            "invocation_id": "abcdefabcdefabcdefabcdefabcdefab",
            "main_pid": 4242,
            "main_pid_starttime": 101,
            "cgroup_path": cgroup,
            "working_directory": "/tmp/worktrees/slots/slot01",
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

    def test_gh_and_printer_call_shared_formatter_instead_of_defining_one(self) -> None:
        gh_source = SOURCE_SHIM.read_text()
        printer_source = (ROOT / "ci-hub/bin/who-am-i").read_text()
        for source in (gh_source, printer_source):
            self.assertIn("disclosure_tag.from_resolution", source)
            self.assertNotIn("class Disclosure", source)
        self.assertIn("class Disclosure", SOURCE_DISCLOSURE.read_text())

    def test_module_contract_states_the_no_metadata_validation_boundary(self) -> None:
        source = SOURCE_SHIM.read_text()
        self.assertIn("With no\ndisclosure metadata", source)
        self.assertIn("body and verdict grammar are still validated", source)
        self.assertIn("process\nidentity is intentionally not resolved", source)
        self.assertNotIn("is still checked against the ``codex``", source)

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "ci-hub/bin").mkdir(parents=True)
        (self.root / "ci-hub/closure").mkdir(parents=True)
        (self.root / "ci-hub/health").mkdir(parents=True)
        (self.root / "ci-hub/lib").mkdir(parents=True)
        (self.root / "ci-hub/validate").mkdir(parents=True)
        shutil.copy2(
            ROOT / "ci-hub/validate/gh_route.py",
            self.root / "ci-hub/validate/gh_route.py",
        )
        self.shim = self.root / "ci-hub/bin/gh"
        self.printer = self.root / "ci-hub/bin/who-am-i"
        shutil.copy2(SOURCE_SHIM, self.shim)
        shutil.copy2(ROOT / "ci-hub/bin/who-am-i", self.printer)
        self.shim.chmod(0o755)
        self.printer.chmod(0o755)
        for dependency in (
            "agent_identity.py",
            "ops_state.py",
            "taskgraph_db.py",
            "worktree_registry.py",
        ):
            shutil.copy2(ROOT / "ci-hub/lib" / dependency, self.root / "ci-hub/lib" / dependency)
        (self.root / "scripts").mkdir()
        shutil.copy2(
            ROOT / "scripts/check-worktree-registry.rs",
            self.root / "scripts/check-worktree-registry.rs",
        )
        shutil.copy2(
            ROOT / "scripts/worktree_registry.rs",
            self.root / "scripts/worktree_registry.rs",
        )
        shutil.copy2(
            ROOT / "ci-hub/lib/bounded_capture.py",
            self.root / "ci-hub/lib/bounded_capture.py",
        )
        shutil.copy2(
            ROOT / "ci-hub/health/approval_binding.py",
            self.root / "ci-hub/health/approval_binding.py",
        )
        shutil.copy2(
            ROOT / "ci-hub/closure/publication_record.py",
            self.root / "ci-hub/closure/publication_record.py",
        )
        (self.root / "ci-hub/landing").mkdir(parents=True)
        shutil.copy2(
            ROOT / "ci-hub/landing/task_pr_record.py",
            self.root / "ci-hub/landing/task_pr_record.py",
        )
        shutil.copy2(SOURCE_DISCLOSURE, self.root / "ci-hub/lib/disclosure_tag.py")

        self.slot = self.root / "worktrees/slots/slot01"
        self.cwd = self.slot / "hermit"
        self.cwd.mkdir(parents=True)
        (self.root / "worktree-state.lock").touch()
        self.write_registry(owner_row())

        self.capture = self.root / "capture.json"
        self.body_capture = self.root / "capture-body.txt"
        self.fake = self.root / "real-gh"
        self.fake.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['GH_CAPTURE']).write_text(json.dumps(sys.argv[1:]))\n"
            "if '--body-file' in sys.argv:\n"
            "    source = Path(sys.argv[sys.argv.index('--body-file') + 1])\n"
            "    Path(os.environ['GH_CAPTURE_BODY']).write_text(source.read_text())\n"
            "sys.stdout.write(os.environ.get('GH_STDOUT', ''))\n"
        )
        self.fake.chmod(0o755)
        self.proxy_capture = self.root / "proxy-capture.txt"
        self.proxy = self.root / "with-proxy"
        self.proxy.write_text(
            '#!/bin/sh\nprintf "%s\\n" "$1" >> "$GH_PROXY_CAPTURE"\nexec "$@"\n'
        )
        self.proxy.chmod(0o755)
        decoy = self.root / "gh"
        decoy.write_text("#!/bin/sh\nexit 99\n")
        decoy.chmod(0o755)
        self.note_capture = self.root / "note-capture.json"
        self.note_writer = self.root / "ci-hub/bin/tg-note-verified"
        self.note_writer.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "note = Path(sys.argv[3])\n"
            "Path(os.environ['NOTE_CAPTURE']).write_text(json.dumps({\n"
            "    'args': sys.argv[1:],\n"
            "    'body': note.read_text(),\n"
            "    'private': os.environ.get('TG_NOTE_PRIVATE_TEMP'),\n"
            "    'note_path': str(note),\n"
            "}))\n"
            "raise SystemExit(int(os.environ.get('NOTE_WRITER_RC', '0')))\n"
        )
        self.note_writer.chmod(0o755)
        self.env = os.environ.copy()
        self.env.update(
            FLEET_REAL_GH=str(self.fake),
            GH_CAPTURE=str(self.capture),
            GH_CAPTURE_BODY=str(self.body_capture),
            NOTE_CAPTURE=str(self.note_capture),
            GH_PROXY_CAPTURE=str(self.proxy_capture),
            PATH=str(self.root) + os.pathsep + os.environ.get("PATH", ""),
        )
        self.env["DG_AGENT_NAME"] = TEST_AGENT
        # The test process may itself run below Codex, Claude, or another live
        # coordinator whose ancestor model is legitimately resolvable. Default
        # fixtures assert an unresolved model and must not inherit that external
        # process identity; tests for resolved/disagreeing models replace this
        # stub explicitly below.
        self.install_identity_stub()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def write_registry(self, row: dict) -> None:
        (self.root / "worktree-state.json").write_text(
            json.dumps(
                {
                    "version": 3,
                    "updated": "2026-08-28T00:00:00Z",
                    "slots": {"slot01": row},
                }
            )
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
        model_source: str | None = None,
        client_family: str | None = "codex",
        signal_state: str = "unavailable",
        signal_agent: str | None = None,
        signal_candidates: tuple[str, ...] = (),
    ) -> None:
        (self.root / "ci-hub/lib/agent_identity.py").write_text(
            "MODEL_UNRESOLVED='unresolved'\n"
            "class Signal:\n"
            "    source='caller-environ'; "
            f"state={signal_state!r}; agent={signal_agent!r}; "
            f"candidates={signal_candidates!r}; detail='fixture'\n"
            "class Resolution:\n"
            f"    state={state!r}; agent={agent!r}; model={model!r}; "
            f"model_source={model_source!r}; primary_source=None; "
            f"client_family={client_family!r}; "
            "basis=('fixture',); signals=(Signal(),)\n"
            "    exit_code={'AGREEMENT':0,'DISAGREEMENT':1}.get(state,2)\n"
            "def resolve_identity(pid, **kwargs): return Resolution()\n"
        )

    def run_printer(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.printer), *args],
            env=self.env,
            cwd=self.cwd,
            text=True,
            capture_output=True,
        )

    def install_failing_identity_stub(self) -> None:
        (self.root / "ci-hub/lib/agent_identity.py").write_text(
            "def resolve_identity(*args, **kwargs): "
            "raise AssertionError('identity resolution must not run')\n"
        )

    def test_missing_who_uses_resolved_metadata_and_posts(self) -> None:
        result = self.run_shim(
            "--team", "hermit2", "--role", "impl",
            "pr", "comment", "42", "--body", "anonymous",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertIn(f"[hermit2, {TEST_AGENT},", body)
        self.assertNotIn("Unverified --who metadata:", body)

    def test_missing_team_defaults_but_missing_role_is_refused(self) -> None:
        defaulted = self.run_shim(
            "--who", TEST_AGENT, "--role", "impl",
            "pr", "comment", "42", "--body", "default team",
        )
        self.assertEqual(defaulted.returncode, 0, defaulted.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertTrue(body.startswith(f"[hermit2, {TEST_AGENT},"))

        self.capture.unlink(missing_ok=True)
        refused = self.run_shim(
            "--who", TEST_AGENT, "--team", "hermit2",
            "pr", "comment", "42", "--body", "missing role",
        )
        self.assertEqual(refused.returncode, 2)
        self.assertIn("--role is required when disclosure metadata is supplied", refused.stderr)
        self.assertFalse(self.capture.exists())

    def test_mismatched_who_is_unverified_metadata_not_authority(self) -> None:
        result = self.run_shim(
            *self.identity(who="someone-else"),
            "pr", "comment", "42", "--body", "false claim",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertTrue(body.startswith(f"[hermit2, {TEST_AGENT},"))
        self.assertIn("Unverified --who metadata: someone-else", body)

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
            f"[hermit2, {TEST_AGENT}, unknown-model, {machine}, role=reviewer]\n"
            f"Unverified --who metadata: {TEST_AGENT}\n"
            "evidence",
        )
        self.assertEqual(self.proxy_capture.read_text().splitlines(), [str(self.fake)])

    def test_invalid_explicit_route_never_uses_an_installed_fallback(self) -> None:
        nonexecutable = self.root / "not-executable"
        nonexecutable.write_text("no execution\n")
        for selected in (None, "gh", str(self.root / "absent"), str(nonexecutable), str(self.shim)):
            with self.subTest(selected=selected):
                environment = dict(self.env)
                if selected is None:
                    environment.pop("FLEET_REAL_GH")
                else:
                    environment["FLEET_REAL_GH"] = selected
                result = self.run_shim("api", "user", env=environment)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("FLEET_REAL_GH", result.stderr)
                self.assertFalse(self.capture.exists())
                self.assertFalse(self.proxy_capture.exists())

    def test_missing_proxy_refuses_instead_of_invoking_selected_cli_directly(self) -> None:
        self.proxy.unlink()
        # Invoke the script with an absolute interpreter so PATH can be empty
        # without testing the script's /usr/bin/env shebang instead of its route.
        result = subprocess.run(
            [sys.executable, str(self.shim), "api", "user"],
            env=dict(self.env, PATH=str(self.root / "absent")),
            cwd=self.cwd, text=True, capture_output=True, timeout=5,
        )
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("with-proxy", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_post_write_output_failure_is_not_a_route_refusal(self) -> None:
        driver = (
            "import runpy,sys\n"
            "class BrokenOutput:\n"
            " def write(self,value): raise BrokenPipeError('fixture post-write pipe')\n"
            " def flush(self): pass\n"
            "sys.stdout=BrokenOutput()\n"
            "sys.argv=sys.argv[1:]\n"
            "runpy.run_path(sys.argv[0],run_name='__main__')\n"
        )
        for operation in (["pr", "comment", "42"], ["pr", "create", "--title", "fixture"]):
            with self.subTest(operation=operation):
                result = subprocess.run(
                    [sys.executable, "-c", driver, str(self.shim), *self.identity(),
                     "--task", "fixture-task", *operation, "--body", "fixture"],
                    cwd=self.cwd, env=dict(self.env, GH_STDOUT="https://example.test/posted\n"),
                    text=True, capture_output=True, timeout=5,
                )
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertNotEqual(result.returncode, 2, result.stderr)
                self.assertIn("BrokenPipeError", result.stderr)
                self.assertNotIn("fleet-gh: REFUSE", result.stderr)
                self.assertEqual(self.captured()[:2], operation[:2])
                self.assertFalse(self.note_capture.exists())

    def test_full_sha_marker_in_prose_is_a_mention(self) -> None:
        body = (
            "The migration note reproduces the retired review text exactly: "
            f"CHANGES-REQUESTED-AT: codex {HEAD}"
        )
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "pr", "comment", "42", "--body", body,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        posted = self.captured()[self.captured().index("--body") + 1]
        gate = load_changes_requested_gate()
        approval = f"APPROVED-AT: codex {HEAD}"
        self.assertEqual(gate.evaluate([approval, posted], HEAD)[0], gate.OK)

    def test_standalone_sha_less_assertion_is_refused(self) -> None:
        gate = load_changes_requested_gate()
        approval = f"APPROVED-AT: codex {HEAD}"
        refusal = "CHANGES-REQUESTED-AT: codex"
        self.assertEqual(
            gate.evaluate([approval, refusal], HEAD)[0],
            gate.INDETERMINATE,
        )

    def test_verdict_with_lead_in_marks_that_line_and_blocks(self) -> None:
        self.install_identity_stub(client_family="codex")
        marker = f"CHANGES-REQUESTED-AT: codex {HEAD}"
        body = f"For this head, {marker} because the assertion still accepts prose."
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "--verdict", marker,
            "pr", "comment", "42", "--body", body,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        posted = self.captured()[self.captured().index("--body") + 1]
        self.assertEqual(posted.splitlines()[-1], "VERDICT: " + body)
        self.assertEqual(posted.count(marker), 1)
        self.assertNotIn("\n" + marker, posted)
        gate = load_changes_requested_gate()
        approval = f"APPROVED-AT: codex {HEAD}"
        self.assertEqual(gate.markers([posted]), [(gate.REFUSAL, "codex", HEAD)])
        self.assertEqual(gate.evaluate([approval, posted], HEAD)[0], gate.BLOCKED)

    def test_verdict_line_with_another_marker_is_refused(self) -> None:
        self.install_identity_stub(client_family="codex")
        approval = f"APPROVED-AT: codex {HEAD}"
        refusal = f"CHANGES-REQUESTED-AT: codex {HEAD}"
        body = (
            f"The previous {approval} is no longer my verdict; "
            f"{refusal} because this head is blocked."
        )
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "--verdict", refusal,
            "pr", "comment", "42", "--body", body,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("target line must contain exactly one review marker", result.stderr)
        self.assertFalse(self.capture.exists())

        # A body produced by the previous writer must also fail closed when a
        # reader encounters it in existing comment history.
        gate = load_changes_requested_gate()
        ambiguous = "VERDICT: " + body
        self.assertEqual(gate.evaluate([approval, ambiguous], HEAD)[0], gate.INDETERMINATE)

    def test_verdict_requires_one_canonical_marker_line(self) -> None:
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "--verdict", "CHANGES-REQUESTED-AT: codex short",
            "pr", "comment", "42", "--body", "changes requested",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("--verdict must be one canonical marker line", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_verdict_already_on_its_own_line_is_not_duplicated(self) -> None:
        self.install_identity_stub(client_family="codex")
        marker = f"APPROVED-AT: codex {HEAD}"
        result = self.run_shim(
            *self.identity(role="coordinator"),
            "--verdict", marker,
            "pr", "comment", "42", "--body", f"dispatch evidence\n{marker}",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        posted = self.captured()[self.captured().index("--body") + 1]
        self.assertEqual(posted.splitlines().count(marker), 1)

    def test_verdict_inside_a_fence_is_refused_instead_of_moved(self) -> None:
        self.install_identity_stub(client_family="codex")
        marker = f"CHANGES-REQUESTED-AT: codex {HEAD}"
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "--verdict", marker,
            "pr", "comment", "42", "--body", f"Example:\n```\n{marker}\n```",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("exactly once in prose", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_verdict_family_must_match_process_client(self) -> None:
        self.install_identity_stub(client_family="codex")
        marker = f"APPROVED-AT: claude {HEAD}"
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "--verdict", marker,
            "pr", "comment", "42", "--body", marker,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("review-family mismatch", result.stderr)
        self.assertFalse(
            self.capture.exists(),
            "CODEX PROCESS INVOKED THE WRITER FOR A CLAUDE VERDICT",
        )

    def test_verdict_family_must_be_proven(self) -> None:
        self.install_identity_stub(client_family=None)
        marker = f"APPROVED-AT: codex {HEAD}"
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "--verdict", marker,
            "pr", "comment", "42", "--body", marker,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("review family unresolved", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_successful_comment_records_permalink_through_verified_note(self) -> None:
        url = "https://github.com/rrnewton/reverie/pull/447#issuecomment-123"
        env = dict(self.env, GH_STDOUT=url + "\n")
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "--task", "review-task",
            "pr", "comment", "447", "-R", "rrnewton/reverie", "--body", "review",
            env=env,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads(self.note_capture.read_text())
        self.assertEqual(record["args"][:3], ["review-task", "--file", record["note_path"]])
        self.assertEqual(record["private"], "1")
        self.assertEqual(
            record["body"],
            publication_record.render_publication(
                "rrnewton/reverie", 447, url, TEST_AGENT
            ),
        )
        self.assertFalse(Path(record["note_path"]).exists())
        self.assertIn("recorded publication", result.stderr)

    def test_verified_note_failure_warns_without_claiming_the_post_failed(self) -> None:
        url = "https://github.com/rrnewton/reverie/pull/447#issuecomment-124"
        env = dict(self.env, GH_STDOUT=url + "\n", NOTE_WRITER_RC="2")
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "--task", "review-task",
            "pr", "comment", "447", "-R", "rrnewton/reverie", "--body", "review",
            env=env,
        )
        self.assertEqual(result.returncode, 0)
        record = json.loads(self.note_capture.read_text())
        self.assertFalse(Path(record["note_path"]).exists())
        self.assertIn("could not record", result.stderr)
        self.assertIn("tg-note-verified", result.stderr)

    def test_pr_close_comment_without_metadata_preserves_exact_body(self) -> None:
        result = self.run_shim(
            "pr", "close", "42", "--comment", "closing disposition"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        captured = self.captured()
        self.assertEqual(captured[captured.index("--comment") + 1], "closing disposition")

    def test_pr_close_comment_gets_mechanical_five_field_prefix(self) -> None:
        result = self.run_shim(
            *self.identity(role="coordinator"),
            "pr", "close", "42", "--comment", "closing disposition",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        captured = self.captured()
        body = captured[captured.index("--comment") + 1]
        machine = socket.gethostname().split(".", 1)[0]
        self.assertEqual(
            body,
            f"[hermit2, {TEST_AGENT}, unknown-model, {machine}, "
            "role=coordinator]\n"
            f"Unverified --who metadata: {TEST_AGENT}\n"
            "closing disposition",
        )

    def test_pr_close_attached_short_comment_is_canonicalized_and_prefixed(self) -> None:
        result = self.run_shim(
            *self.identity(), "pr", "close", "42", "-cclosing disposition"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        captured = self.captured()
        self.assertNotIn("-cclosing disposition", captured)
        body = captured[captured.index("--comment") + 1]
        self.assertTrue(
            body.endswith(f"Unverified --who metadata: {TEST_AGENT}\nclosing disposition")
        )

    def test_pr_close_duplicate_comment_options_are_refused_before_write(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "pr", "close", "42",
            "--comment", "first",
            "--comment=second",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("must supply --comment exactly once", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_pr_close_comment_body_that_looks_like_an_option_is_not_reparsed(self) -> None:
        result = self.run_shim(
            *self.identity(), "pr", "close", "42", "--comment", "--comment=words"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        captured = self.captured()
        body = captured[captured.index("--comment") + 1]
        self.assertTrue(
            body.endswith(f"Unverified --who metadata: {TEST_AGENT}\n--comment=words")
        )

    def test_created_issue_and_pr_bodies_are_prefixed_without_the_author_doing_it(
        self,
    ) -> None:
        """The author is a constant on this fork, so an unattributed issue reads
        as the owner's own. Creation must stamp without anyone remembering to."""
        machine = socket.gethostname().split(".", 1)[0]
        for command in (["issue", "create"], ["pr", "create"]):
            with self.subTest(command=" ".join(command)):
                self.capture.unlink(missing_ok=True)
                result = self.run_shim(
                    *self.identity(),
                    *command, "--title", "a title", "--body", "prose",
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                body = self.captured()[self.captured().index("--body") + 1]
                self.assertEqual(
                    body,
                    f"[hermit2, {TEST_AGENT}, unknown-model, {machine}, role=impl]\n"
                    f"Unverified --who metadata: {TEST_AGENT}\n"
                    "prose",
                    "A CREATED ARTIFACT WENT OUT UNATTRIBUTED",
                )

    def test_successful_pr_create_records_exact_task_ownership(self) -> None:
        url = "https://github.com/rrnewton/hermit/pull/2811"
        env = dict(self.env, GH_STDOUT=url + "\n")
        result = self.run_shim(
            *self.identity(),
            "--task", "type-census",
            "pr", "create", "--title", "typed result", "--body", "prose",
            env=env,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        record = json.loads(self.note_capture.read_text())
        self.assertEqual(record["args"][:3], ["type-census", "--file", record["note_path"]])
        self.assertEqual(record["private"], "1")
        self.assertEqual(
            record["body"],
            task_pr_record.render(
                "type-census", "rrnewton/hermit", 2811, url, TEST_AGENT
            ),
        )
        self.assertFalse(Path(record["note_path"]).exists())
        self.assertIn("recorded task/PR ownership", result.stderr)

    def test_task_on_issue_create_is_refused_before_write(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "--task", "not-a-pr",
            "issue", "create", "--title", "t", "--body", "prose",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("--task applies only", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_created_body_from_a_file_is_prefixed_too(self) -> None:
        """`--body-file` is how a long issue is actually filed; it must not be
        the way around the boundary."""
        source = self.root / "body.md"
        source.write_text("filed from a file")
        result = self.run_shim(
            *self.identity(), "issue", "create", "--title", "t", "--body-file", str(source)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        machine = socket.gethostname().split(".", 1)[0]
        self.assertEqual(
            body,
            f"[hermit2, {TEST_AGENT}, unknown-model, {machine}, role=impl]\n"
            f"Unverified --who metadata: {TEST_AGENT}\n"
            "filed from a file",
        )

    def test_227307_byte_body_stays_off_exec_argv_and_is_not_truncated(self) -> None:
        payload = "z" * 227_307
        source = self.root / "large-body.md"
        source.write_text(payload)

        result = self.run_shim(
            "issue", "create", "--title", "large", "--body-file", str(source)
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        captured = self.captured()
        self.assertNotIn("--body", captured)
        self.assertIn("--body-file", captured)
        staged = Path(captured[captured.index("--body-file") + 1])
        self.assertEqual(self.body_capture.read_text(), payload)
        self.assertFalse(staged.exists(), "the private staging file must be cleaned up")

    def test_create_without_metadata_preserves_exact_body(self) -> None:
        result = self.run_shim("issue", "create", "--title", "t", "--body", "anonymous")
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertEqual(body, "anonymous")

    def test_no_metadata_exact_head_verdict_skips_identity_resolution(self) -> None:
        self.install_failing_identity_stub()
        marker = f"APPROVED-AT: codex {HEAD}"
        result = self.run_shim(
            "--verdict", marker,
            "pr", "comment", "42", "--body", marker,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertEqual(body, marker)
        gate = load_changes_requested_gate()
        self.assertEqual(gate.evaluate([body], HEAD)[0], gate.OK)

    def test_no_metadata_malformed_approval_is_refused_before_write(self) -> None:
        self.install_failing_identity_stub()
        result = self.run_shim(
            "pr", "comment", "42", "--body", "APPROVED-AT: codex short",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("verdict-shaped line(s) that will NOT bind", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_no_metadata_bodyless_pr_create_is_refused_before_write(self) -> None:
        self.install_failing_identity_stub()
        result = self.run_shim("pr", "create", "--title", "body required")
        self.assertEqual(result.returncode, 2)
        self.assertIn("no supported --body/--body-file", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_create_without_any_body_is_refused_rather_than_passed_through(self) -> None:
        """Fail closed: a body-less create would otherwise be the hole this
        boundary exists to close."""
        result = self.run_shim(*self.identity(), "issue", "create", "--title", "t")
        self.assertEqual(result.returncode, 2)
        self.assertIn("no supported --body/--body-file", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_edit_with_a_body_is_prefixed(self) -> None:
        result = self.run_shim(
            *self.identity(), "issue", "edit", "7", "--body", "replacement prose"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        machine = socket.gethostname().split(".", 1)[0]
        self.assertEqual(
            body,
            f"[hermit2, {TEST_AGENT}, unknown-model, {machine}, role=impl]\n"
            f"Unverified --who metadata: {TEST_AGENT}\n"
            "replacement prose",
        )

    def test_edit_without_a_body_needs_no_identity_and_passes_through(self) -> None:
        """A title-only edit writes no prose. Demanding identity for it would be
        a regression, not extra safety."""
        result = self.run_shim("issue", "edit", "7", "--title", "renamed")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.captured(), ["issue", "edit", "7", "--title", "renamed"])

    def test_creating_an_issue_through_the_raw_api_is_not_a_way_around_it(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "api", "-X", "POST", "repos/o/r/issues", "-f", "title=t", "-f", "body=via api",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        captured = self.captured()
        machine = socket.gethostname().split(".", 1)[0]
        self.assertIn(
            f"body=[hermit2, {TEST_AGENT}, unknown-model, {machine}, role=impl]\n"
            f"Unverified --who metadata: {TEST_AGENT}\n"
            "via api",
            captured,
        )

    def test_reads_and_non_prose_commands_still_pass_through_untouched(self) -> None:
        for command in (["issue", "list"], ["pr", "view", "42"], ["issue", "close", "7"]):
            with self.subTest(command=" ".join(command)):
                self.capture.unlink(missing_ok=True)
                result = self.run_shim(*command)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.captured(), command)

    def test_pr_close_without_comment_needs_no_identity_and_passes_through(self) -> None:
        result = self.run_shim("pr", "close", "42")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.captured(), ["pr", "close", "42"])

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
            f"[hermit2, {TEST_AGENT}, gpt-5.6-sol, {machine}, role=impl]\n"
            f"Unverified --who metadata: {TEST_AGENT}\n"
            "evidence",
            "RESOLVED MODEL WAS LOST OR REPLACED BY A GUESS",
        )

    def test_gh_and_who_am_i_use_one_byte_identical_tag_formatter(self) -> None:
        self.install_identity_stub(model="gpt-5.6-sol")
        posted = self.run_shim(
            *self.identity(role="impl"),
            "pr", "comment", "42", "--body", "evidence",
        )
        self.assertEqual(0, posted.returncode, posted.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        gh_tag = body.split("\n", 1)[0]

        printed = self.run_printer("--tag", "--role", "impl")
        self.assertEqual(0, printed.returncode, printed.stderr)
        self.assertEqual(gh_tag.encode(), printed.stdout.encode())
        self.assertFalse(
            printed.stdout.endswith("\n"),
            "tag printer must not append a newline that changes command substitution",
        )

    def test_tag_printer_defaults_role_and_degrades_unresolved_or_disagreement(self) -> None:
        missing = self.run_printer("--tag")
        self.assertEqual(0, missing.returncode, missing.stderr)
        self.assertIn("role=impl]", missing.stdout)
        self.assertIn("fields: team, agent_name, model, machine, role", missing.stderr)

        machine = socket.gethostname().split(".", 1)[0]
        self.install_identity_stub(state="UNRESOLVED", agent=None)
        unresolved = self.run_printer("--tag", "--role", "impl")
        self.assertEqual(0, unresolved.returncode, unresolved.stderr)
        self.assertEqual(
            f"[hermit2, unknown-agentname, unknown-model, {machine}, role=impl]",
            unresolved.stdout,
        )
        self.assertIn("DEGRADED", unresolved.stderr)
        self.assertIn("UNRESOLVED", unresolved.stderr)

        self.install_identity_stub(
            state="DISAGREEMENT",
            agent=None,
            signal_state="resolved",
            signal_agent="best-visible-agent",
            signal_candidates=("other-visible-agent",),
        )
        disagreement = self.run_printer("--tag", "--role", "impl")
        self.assertEqual(0, disagreement.returncode, disagreement.stderr)
        self.assertEqual(
            f"[hermit2, degraded-best-visible-agent, unknown-model, {machine}, role=impl]",
            disagreement.stdout,
        )
        self.assertIn("DEGRADED", disagreement.stderr)
        self.assertIn("DISAGREEMENT", disagreement.stderr)

    def test_shared_github_boundary_marks_unresolved_identity_as_metadata(self) -> None:
        self.install_identity_stub(state="UNRESOLVED", agent=None)
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body", "nobody established",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertIn("unknown-agentname", body)
        self.assertIn(f"Unverified --who metadata: {TEST_AGENT}", body)

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

    def test_truly_unresolved_identity_does_not_block_a_write(self) -> None:
        self.install_identity_stub(state="UNRESOLVED", agent=None)
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body", "nobody established",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertIn("unknown-agentname", body)
        self.assertIn(f"Unverified --who metadata: {TEST_AGENT}", body)

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
                f"[hermit2, {TEST_AGENT}, unknown-model, {machine}, role=reviewer]\n"
                f"Unverified --who metadata: {TEST_AGENT}\n",
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
            f"body=[hermit2, {TEST_AGENT}, unknown-model, {machine}, role=impl]\n"
            f"Unverified --who metadata: {TEST_AGENT}\n"
            "evidence",
            self.captured(),
        )

    def test_raw_api_review_body_is_attributed(self) -> None:
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "api", "-X", "POST", "repos/o/r/pulls/42/reviews", "-f", "body=approved",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = next(value for value in self.captured() if value.startswith("body="))
        self.assertIn(
            f"role=reviewer]\nUnverified --who metadata: {TEST_AGENT}\napproved",
            body,
        )

    def test_raw_graphql_comment_mutation_is_refused(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "api", "graphql", "-f",
            "query=mutation { addDiscussionComment(input: {}) }",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("cannot be attributed safely", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_foreign_existing_disclosure_is_preserved_as_metadata(self) -> None:
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body",
            "[other, somebody, gpt-5.6-sol, devbig999, role=impl]\nwords",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        posted = self.captured()[self.captured().index("--body") + 1]
        self.assertTrue(posted.startswith(
            "[other, somebody, gpt-5.6-sol, devbig999, role=impl]\n"
        ))
        self.assertIn(f"Unverified --who metadata: {TEST_AGENT}", posted)

    def test_new_and_historical_existing_disclosures_are_not_duplicated(self) -> None:
        machine = socket.gethostname().split(".", 1)[0]
        for model in ("unknown-model", "unresolved"):
            with self.subTest(model=model):
                self.capture.unlink(missing_ok=True)
                body = (
                    f"[hermit2, {TEST_AGENT}, {model}, {machine}, role=impl]\n"
                    "already attributed"
                )
                result = self.run_shim(
                    *self.identity(), "pr", "comment", "42", "--body", body
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                posted = self.captured()[self.captured().index("--body") + 1]
                first, rest = body.split("\n", 1)
                self.assertEqual(
                    posted,
                    first + f"\nUnverified --who metadata: {TEST_AGENT}\n" + rest,
                )
                self.assertEqual(posted.count("[hermit2,"), 1)

    def test_legacy_four_field_prefix_is_refused_not_nested(self) -> None:
        result = self.run_shim(
            *self.identity(), "pr", "comment", "42", "--body",
            f"[hermit2, {TEST_AGENT}, devbig014, role=impl] old body",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("legacy who-is-talking prefix", result.stderr)
        self.assertFalse(self.capture.exists())

    def test_legacy_role_model_prefix_is_refused_not_nested(self) -> None:
        result = self.run_shim(
            *self.identity(role="reviewer"),
            "pr", "comment", "42", "--body",
            "[adversarial-reviewer agent, gpt-5.6-sol]\nold review",
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

    def test_environment_and_ancestry_disagreement_is_visible_metadata(self) -> None:
        self.install_identity_stub(
            state="DISAGREEMENT",
            agent=None,
            signal_state="resolved",
            signal_agent="different-process-agent",
            signal_candidates=(TEST_AGENT,),
        )
        env = dict(self.env, DG_AGENT_NAME="different-process-agent")
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body", "conflicting process context",
            env=env,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        body = self.captured()[self.captured().index("--body") + 1]
        self.assertIn("degraded-", body)
        self.assertIn(f"Unverified --who metadata: {TEST_AGENT}", body)

    def test_non_comment_command_needs_no_identity_and_passes_through(self) -> None:
        result = self.run_shim("pr", "view", "42", "--json", "headRefOid")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.captured(), ["pr", "view", "42", "--json", "headRefOid"])

    @unittest.skipIf(
        bool(os.environ.get("FLEET_ATTRIBUTION_MUTATION_CHILD")),
        "mutation child must not recurse",
    )
    def test_false_who_does_not_change_the_structured_publication_record(self) -> None:
        url = "https://github.com/rrnewton/hermit/pull/42#issuecomment-124"
        env = dict(self.env, GH_STDOUT=url + "\n")
        base = [
            "--team", "hermit2", "--role", "reviewer", "--task", "review-task",
            "pr", "comment", "42", "--body", f"APPROVED-AT: codex {HEAD}",
        ]
        without_name = self.run_shim(*base, env=env)
        self.assertEqual(without_name.returncode, 0, without_name.stderr)
        no_name_record = json.loads(self.note_capture.read_text())["body"]
        no_name_body = self.captured()[self.captured().index("--body") + 1]

        with_false_name = self.run_shim(
            "--who", "false-reviewer", *base, env=env
        )
        self.assertEqual(with_false_name.returncode, 0, with_false_name.stderr)
        false_name_record = json.loads(self.note_capture.read_text())["body"]
        false_name_body = self.captured()[self.captured().index("--body") + 1]

        self.assertEqual(no_name_record, false_name_record)
        self.assertIn("Unverified --who metadata: false-reviewer", false_name_body)
        self.assertNotIn("false-reviewer", no_name_body)
        gate = load_changes_requested_gate()
        self.assertEqual(gate.evaluate([no_name_body], HEAD)[0], gate.OK)
        self.assertEqual(gate.evaluate([false_name_body], HEAD)[0], gate.OK)

    @unittest.skipIf(
        bool(os.environ.get("FLEET_ATTRIBUTION_MUTATION_CHILD")),
        "mutation child must not recurse",
    )
    def test_mutation_discarding_resolved_model_is_killed(self) -> None:
        mutant_dir = self.root / "model-mutant"
        mutant_dir.mkdir()
        mutant = mutant_dir / "disclosure_tag.py"
        source = SOURCE_DISCLOSURE.read_text()
        needle = (
            "model = model_for_disclosure(\n"
            "        resolution.model,\n"
            "        getattr(resolution, \"model_source\", None),\n"
            "    )"
        )
        replacement = 'model = UNKNOWN_MODEL  # MUTANT: resolved model discarded'
        self.assertIn(needle, source, "mutation could not locate resolved-model binding")
        mutant.write_text(source.replace(needle, replacement, 1))
        mutant.chmod(0o755)
        env = dict(
            os.environ,
            FLEET_DISCLOSURE_TAG_UNDER_TEST=str(mutant),
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
