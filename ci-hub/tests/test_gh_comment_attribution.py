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
    cgroup = "/user.slice/test-boundary.service"
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

    def test_gh_and_printer_call_shared_formatter_instead_of_defining_one(self) -> None:
        gh_source = SOURCE_SHIM.read_text()
        printer_source = (ROOT / "ci-hub/bin/who-am-i").read_text()
        for source in (gh_source, printer_source):
            self.assertIn("disclosure_tag.from_resolution", source)
            self.assertNotIn("class Disclosure", source)
        self.assertIn("class Disclosure", SOURCE_DISCLOSURE.read_text())

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "ci-hub/bin").mkdir(parents=True)
        (self.root / "ci-hub/closure").mkdir(parents=True)
        (self.root / "ci-hub/health").mkdir(parents=True)
        (self.root / "ci-hub/lib").mkdir(parents=True)
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
        shutil.copy2(SOURCE_DISCLOSURE, self.root / "ci-hub/lib/disclosure_tag.py")

        self.slot = self.root / "worktrees/slots/slot01"
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
            "sys.stdout.write(os.environ.get('GH_STDOUT', ''))\n"
        )
        self.fake.chmod(0o755)
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
            NOTE_CAPTURE=str(self.note_capture),
        )
        self.env["DG_AGENT_NAME"] = TEST_AGENT

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

    def test_pr_close_comment_without_identity_is_refused_before_write(self) -> None:
        result = self.run_shim(
            "pr", "close", "42", "--comment", "closing disposition"
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("missing required --who", result.stderr)
        self.assertFalse(self.capture.exists())

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
            f"[hermit2, {TEST_AGENT}, unresolved, {machine}, "
            "role=coordinator]\nclosing disposition",
        )

    def test_pr_close_attached_short_comment_is_canonicalized_and_prefixed(self) -> None:
        result = self.run_shim(
            *self.identity(), "pr", "close", "42", "-cclosing disposition"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        captured = self.captured()
        self.assertNotIn("-cclosing disposition", captured)
        body = captured[captured.index("--comment") + 1]
        self.assertTrue(body.endswith("role=impl]\nclosing disposition"))

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
        self.assertTrue(body.endswith("role=impl]\n--comment=words"))

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
                    f"[hermit2, {TEST_AGENT}, unresolved, {machine}, role=impl]\nprose",
                    "A CREATED ARTIFACT WENT OUT UNATTRIBUTED",
                )

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
            f"[hermit2, {TEST_AGENT}, unresolved, {machine}, role=impl]\nfiled from a file",
        )

    def test_create_without_identity_is_refused_before_the_artifact_exists(self) -> None:
        result = self.run_shim("issue", "create", "--title", "t", "--body", "anonymous")
        self.assertEqual(result.returncode, 2)
        self.assertIn("missing required --who", result.stderr)
        self.assertFalse(self.capture.exists(), "AN UNATTRIBUTED ISSUE WAS CREATED")

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
            f"[hermit2, {TEST_AGENT}, unresolved, {machine}, role=impl]\nreplacement prose",
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
            f"body=[hermit2, {TEST_AGENT}, unresolved, {machine}, role=impl]\nvia api",
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
            f"[hermit2, {TEST_AGENT}, gpt-5.6-sol, {machine}, role=impl]\nevidence",
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
        self.assertIn("fields: team, agent, model, machine, role", missing.stderr)

        machine = socket.gethostname().split(".", 1)[0]
        self.install_identity_stub(state="UNRESOLVED", agent=None)
        unresolved = self.run_printer("--tag", "--role", "impl")
        self.assertEqual(0, unresolved.returncode, unresolved.stderr)
        self.assertEqual(
            f"[hermit2, degraded-unresolved, unresolved, {machine}, role=impl]",
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
            f"[hermit2, degraded-best-visible-agent, unresolved, {machine}, role=impl]",
            disagreement.stdout,
        )
        self.assertIn("DEGRADED", disagreement.stderr)
        self.assertIn("DISAGREEMENT", disagreement.stderr)

    def test_shared_github_boundary_still_refuses_unresolved_identity(self) -> None:
        self.install_identity_stub(state="UNRESOLVED", agent=None)
        result = self.run_shim(
            *self.identity(),
            "pr", "comment", "42", "--body", "nobody established",
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("owner unresolved", result.stderr)
        self.assertFalse(self.capture.exists())

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
        mutant = mutant_dir / "disclosure_tag.py"
        source = SOURCE_DISCLOSURE.read_text()
        needle = "if claimed_agent is not None and claimed_agent != agent:"
        replacement = (
            "if False and claimed_agent is not None and claimed_agent != agent:  "
            "# MUTANT: resolved-owner comparison removed"
        )
        self.assertIn(needle, source, "mutation could not locate owner comparison")
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
        mutant = mutant_dir / "disclosure_tag.py"
        source = SOURCE_DISCLOSURE.read_text()
        needle = "model = resolution.model"
        replacement = 'model = "unresolved"  # MUTANT: resolved model discarded'
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
