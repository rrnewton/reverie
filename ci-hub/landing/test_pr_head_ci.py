#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock


MODULE = Path(__file__).with_name("pr_head_ci.py")
SPEC = importlib.util.spec_from_file_location("pr_head_ci", MODULE)
assert SPEC and SPEC.loader
P = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = P
SPEC.loader.exec_module(P)

sys.path.insert(0, str(P.ROOT / "ci-hub/validate/tests"))
from test_immutable_tool_authority import ExecutingAuthorityFixture

OLD = "1" * 40
NEW = "2" * 40
PARENT = "3" * 40
TEST_ENVIRONMENT = {
    "HOME": "/home/tester",
    "DEV_HERMIT_PARENT": str(P.ROOT),
    "DEV_HERMIT_TOOL_PARENT_SHA": PARENT,
}


def git(
    path: Path, *arguments: object, capture: bool = False
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(path), *(str(argument) for argument in arguments)],
        check=True,
        capture_output=capture,
        text=True,
    )


def create_repo(path: Path, marker: str) -> str:
    path.mkdir()
    subprocess.run(
        ["git", "init", "-b", "main", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    git(path, "config", "user.name", "Test User")
    git(path, "config", "user.email", "test@example.com")
    (path / "marker").write_text(marker)
    git(path, "add", "marker")
    git(path, "commit", "-m", marker, capture=True)
    return git(path, "rev-parse", "HEAD^{commit}", capture=True).stdout.strip()


def validation_event(
    *,
    repo: str = "rrnewton/hermit",
    target: str = OLD,
    status: str = "PASSED",
    canonical_verdict: str = "VALIDATED",
    wrapper_exit_code: int = 0,
    scorecard_writeback_failed: bool = False,
) -> str:
    result = {
        "PASSED": "success",
        "FAILED": "failure",
        "COULD_NOT_RUN": "no-result",
    }[status]
    row = {
        "schema_version": 1,
        "event": "finished",
        "repo": repo,
        "target": target,
        "state": "completed",
        "result": result,
        "final_validate_status": status,
        "canonical_verdict": canonical_verdict,
        "wrapper_exit_code": wrapper_exit_code,
        "commit_status_publication": {"state": "published"},
    }
    if scorecard_writeback_failed:
        row.update(
            service_result_schema=P.service_result.TEST_COUNTS_SCHEMA_VERSION,
            exit_code=75,
            executed_tests=1,
            passed_tests=1,
            scorecard_writeback={
                "status": "failed",
                "error": "fixture scorecard write-back failure",
            },
        )
        row["commit_status_publication"] = {
            "state": "not-attempted",
            "reason": "scorecard write-back did not complete",
        }
    return json.dumps(row) + "\n"


class FakeRun:
    def __init__(
        self,
        *,
        heads=None,
        verdicts=None,
        approval_rc=0,
        validation_rc=0,
        validation_stdout=None,
        validation_stderr="",
        status_extra=None,
    ):
        self.status_extra = dict(status_extra or {})
        self.heads = list(heads or [OLD])
        self.verdicts = list(verdicts or ["NOT-VALIDATED"])
        self.approval_rc = approval_rc
        self.validation_rc = validation_rc
        self.validation_stdout = validation_stdout
        self.validation_stderr = validation_stderr
        self.commands = []
        self.environments = []

    def __call__(self, command, **kwargs):
        command = list(command)
        self.commands.append(command)
        environment = kwargs.get("env")
        self.environments.append(
            dict(environment) if isinstance(environment, dict) else None
        )
        if command[:5] == [
            "git",
            "--no-optional-locks",
            "--no-replace-objects",
            "-C",
            str(P.ROOT),
        ]:
            if command[-1] == "--git-common-dir":
                return subprocess.CompletedProcess(
                    command, 0, f"{P.ROOT / '.git'}\n", ""
                )
            if command[-1] == "HEAD^{commit}":
                return subprocess.CompletedProcess(command, 0, f"{PARENT}\n", "")
        if command[:3] == ["with-proxy", "gh", "pr"]:
            head = self.heads.pop(0) if len(self.heads) > 1 else self.heads[0]
            return subprocess.CompletedProcess(command, 0, json.dumps({
                "headRefOid": head,
                "headRefName": "feature",
                "isDraft": False,
                "state": "OPEN",
            }), "")
        if "approval_binding.py" in " ".join(command):
            return subprocess.CompletedProcess(command, self.approval_rc, "approval", "")
        if "validate-status" in command:
            verdict = self.verdicts.pop(0) if len(self.verdicts) > 1 else self.verdicts[0]
            rc = {"VALIDATED": 0, "FAILED": 3}.get(verdict, 4)
            report = {"verdict": verdict, **self.status_extra}
            return subprocess.CompletedProcess(command, rc, json.dumps(report), "")
        if "validate-run" in command:
            repo = "rrnewton/hermit"
            validate_index = command.index("validate-run")
            if "--repo" in command[validate_index:]:
                repo_index = command.index("--repo", validate_index)
                repo = command[repo_index + 1]
            target = command[command.index("--target", validate_index) + 1]
            stdout = self.validation_stdout
            if stdout is None:
                stdout = validation_event(
                    repo=repo,
                    target=target,
                    status="COULD_NOT_RUN" if self.validation_rc == 75 else "PASSED",
                    canonical_verdict=(
                        "NOT-VALIDATED" if self.validation_rc == 75 else "VALIDATED"
                    ),
                    wrapper_exit_code=4 if self.validation_rc == 75 else 0,
                )
            return subprocess.CompletedProcess(
                command,
                self.validation_rc,
                stdout,
                self.validation_stderr,
            )
        raise AssertionError(command)


class PrHeadCiTest(unittest.TestCase):
    def args(self, *extra):
        return ["--repo", "rrnewton/hermit", "--pr", "7", "--expected-head", OLD,
                "--checkout", "/checkout", "--agent", "agent", *extra]

    def main(self, args, *, run):
        return P.main(args, run=run, environment=TEST_ENVIRONMENT)

    def test_absence_never_reads_as_pass(self):
        run = FakeRun()
        self.assertEqual(self.main(self.args(), run=run), 4)
        self.assertFalse(any("validate-run" in command for command in run.commands))

    def test_noncanonical_head_is_refused_before_any_authority_read(self):
        run = FakeRun()
        args = self.args()
        args[args.index(OLD)] = "A" * 40
        self.assertEqual(self.main(args, run=run), 2)
        self.assertEqual(run.commands, [])

    def test_exact_head_late_receipt_is_validated(self):
        run = FakeRun(heads=[OLD, OLD], verdicts=["NOT-VALIDATED", "VALIDATED"])
        self.assertEqual(self.main(self.args("--execute"), run=run), 0)
        self.assertEqual(sum("validate-run" in command for command in run.commands), 1)

    def test_moved_head_expires_old_receipt(self):
        run = FakeRun(heads=[OLD, NEW], verdicts=["NOT-VALIDATED", "VALIDATED"])
        self.assertEqual(self.main(self.args("--execute"), run=run), 4)
        self.assertEqual(sum("validate-status" in command for command in run.commands), 1)

    def test_exact_head_failure_remains_red_without_rerun(self):
        run = FakeRun(verdicts=["FAILED"])
        self.assertEqual(self.main(self.args("--execute"), run=run), 3)
        self.assertFalse(any("validate-run" in command for command in run.commands))

    FOREIGN_ENTRY = {
        "state": "foreign-host-artifact-absent",
        "commit": "4c91b9527efc" + "0" * 28,
        "finished_at": "2026-09-28T01:02:03Z",
        "run_id": "validate-buck-validate-4c91b9527efc",
        "producing_host": "devbig030",
        "local_host": "devbig014",
        "missing_artifact": "ignored/validate/artifacts/validate-buck-validate-4c91b9527efc/constructed-plan.json",
        "reason": "validation evidence reader fault: cannot open retained artifact",
    }

    def test_unverifiable_evidence_never_reaches_the_validator(self):
        """https://github.com/rrnewton/dev-hermit/issues/395: a rerun cannot
        clear evidence that exists only on another host, so the 46-minute
        validate is never launched, and neither is approval consulted."""
        forbidden = mock.Mock(side_effect=AssertionError("must not launch validation"))
        for extra in ((), ("--execute",), ("--execute", "--json")):
            with self.subTest(args=extra):
                run = FakeRun(
                    verdicts=["NEEDS-RERUN"],
                    status_extra={"unverifiable_evidence": [self.FOREIGN_ENTRY]},
                )
                stdout = io.StringIO()
                with mock.patch.object(P, "validate_command", forbidden), \
                        mock.patch.object(P, "immutable_tool_launch", forbidden), \
                        mock.patch.object(P, "require_current_approval", forbidden), \
                        redirect_stdout(stdout):
                    self.assertEqual(self.main(self.args(*extra), run=run), 2)
                self.assertFalse(any("validate-run" in command for command in run.commands))
                self.assertFalse(
                    any("approval_binding.py" in " ".join(command) for command in run.commands)
                )
                self.assertEqual(sum("validate-status" in command for command in run.commands), 1)
                output = stdout.getvalue()
                if "--json" in extra:
                    report = json.loads(output)
                    self.assertEqual(report["state"], "EVIDENCE_UNAVAILABLE")
                    self.assertIs(report["acquirable"], False)
                    output = report["detail"]
                else:
                    self.assertTrue(output.startswith("PR-HEAD-CI EVIDENCE_UNAVAILABLE "), output)
                self.assertIn("canonical verdict is NEEDS-RERUN", output)
                self.assertIn(
                    "validate-buck-validate-4c91b9527efc on 4c91b9527efc from devbig030", output
                )
                self.assertIn("a rerun here cannot clear them", output)
                self.assertIn("https://github.com/rrnewton/dev-hermit/issues/412", output)
        forbidden.assert_not_called()

    def test_empty_unverifiable_evidence_still_admits_the_validator(self):
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NEEDS-RERUN", "VALIDATED"],
            status_extra={"unverifiable_evidence": []},
        )
        self.assertEqual(self.main(self.args("--execute"), run=run), 0)
        self.assertEqual(sum("validate-run" in command for command in run.commands), 1)

    def test_malformed_or_contradictory_unverifiable_evidence_is_refused(self):
        cases = (
            ("NEEDS-RERUN", "none"),
            ("NEEDS-RERUN", ["not-an-object"]),
            ("NOT-VALIDATED", {"run_id": "x"}),
            ("VALIDATED", [self.FOREIGN_ENTRY]),
        )
        for verdict, entries in cases:
            with self.subTest(verdict=verdict, entries=entries):
                run = FakeRun(
                    verdicts=[verdict], status_extra={"unverifiable_evidence": entries}
                )
                stdout = io.StringIO()
                with redirect_stdout(stdout):
                    self.assertEqual(self.main(self.args("--execute"), run=run), 2)
                self.assertTrue(stdout.getvalue().startswith("PR-HEAD-CI UNKNOWN "))
                self.assertFalse(any("validate-run" in command for command in run.commands))

    def test_unapproved_head_does_not_consume_validator(self):
        run = FakeRun(approval_rc=1)
        self.assertEqual(self.main(self.args("--execute"), run=run), 4)
        self.assertFalse(any("validate-run" in command for command in run.commands))

    def approval_fixture(self, *, critical: bool) -> Path:
        temporary = Path(tempfile.mkdtemp(prefix="pr-head-ci-approval-"))
        self.addCleanup(shutil.rmtree, temporary)
        old = "4" * 40
        comments = [
            f"APPROVED-AT: codex {OLD}",
            f"APPROVED-AT: claude {old}",
        ]
        if critical:
            comments.insert(
                0,
                "REVIEW-DISPATCH-ATTESTATION\n"
                "critical-class: hermit-syscall\n"
                f"head: {OLD}",
            )
        fixture = temporary / "approval.json"
        fixture.write_text(
            json.dumps(
                {
                    "actions_enabled": True,
                    "prs": [
                        {
                            "number": 7,
                            "head": OLD,
                            "labels": [
                                "post-facto-human-review",
                                "passed-review-codex",
                                "passed-review-claude",
                            ],
                            "comments": comments,
                            "runs_at_head": 1,
                            "mergeable": "MERGEABLE",
                        }
                    ],
                }
            )
        )
        return fixture

    def approval_runner(self, fixture: Path):
        def run(command, **kwargs):
            return subprocess.run(
                [*command, "--from-json", str(fixture)],
                text=True,
                capture_output=True,
                check=False,
            )

        return run

    def test_pr_head_gate_accepts_one_current_lane_for_an_ordinary_pr(self):
        fixture = self.approval_fixture(critical=False)
        P.require_current_approval(
            "rrnewton/hermit", 7, run=self.approval_runner(fixture)
        )

    def test_pr_head_gate_requires_both_lanes_for_a_critical_pr(self):
        fixture = self.approval_fixture(critical=True)
        with self.assertRaisesRegex(
            PermissionError, "CRITICAL-REVIEW-MISSING.*claude"
        ):
            P.require_current_approval(
                "rrnewton/hermit", 7, run=self.approval_runner(fixture)
            )

    def test_validation_uses_separate_immutable_tool_root(self):
        launch = P.ImmutableToolLaunch(
            operational_tool=Path("/home/tester/.local/libexec/dev-hermit/operational-tool"),
            state_root=Path("/state/dev-hermit"),
            state_identity="11:22",
            parent_sha=PARENT,
        )
        command = P.validate_command(
            "rrnewton/hermit", 7, OLD, Path("/tmp/exact-head"), "agent", launch=launch
        )
        self.assertEqual(
            command[:11],
            [
                "/home/tester/.local/libexec/dev-hermit/operational-tool",
                "--run-current-tool",
                "--state-root",
                "/state/dev-hermit",
                "--state-identity",
                "11:22",
                "--tool-parent-sha",
                PARENT,
                "--tool-relative",
                "ci-hub/ci-hub",
                "--",
            ],
        )
        self.assertEqual(
            command[11:15],
            ["validate-run", "--json", "--checkout", "/tmp/exact-head"],
        )
        self.assertIn("--materialize-target", command)
        self.assertNotEqual(command[0], str(P.ROOT / "ci-hub/ci-hub"))

    def test_every_child_drops_ambient_git_repository_selectors(self):
        hostile = {
            **TEST_ENVIRONMENT,
            "PATH": "/safe/bin",
            **{
                name: f"hostile-{name}"
                for name in P.git_env.GIT_REPOSITORY_ENV
            },
        }
        run = FakeRun(
            heads=[OLD, OLD], verdicts=["NOT-VALIDATED", "VALIDATED"]
        )

        rc = P.main(self.args("--execute"), run=run, environment=hostile)

        self.assertEqual(0, rc)
        for command, child in zip(run.commands, run.environments, strict=True):
            with self.subTest(command=command):
                self.assertIsNotNone(child)
                assert child is not None
                self.assertEqual("/safe/bin", child["PATH"])
                self.assertTrue(P.git_env.GIT_REPOSITORY_ENV.isdisjoint(child))

    def test_launch_descriptor_binds_canonical_state_identity_and_executing_head(self):
        run = FakeRun()

        launch = P.immutable_tool_launch(environment=TEST_ENVIRONMENT, run=run)

        metadata = P.ROOT.stat()
        self.assertEqual(P.ROOT, launch.state_root)
        self.assertEqual(f"{metadata.st_dev}:{metadata.st_ino}", launch.state_identity)
        self.assertEqual(PARENT, launch.parent_sha)

    def test_installed_launch_uses_sealed_state_and_code_identity(self):
        with ExecutingAuthorityFixture() as fixture, mock.patch.object(P, "ROOT", fixture.code):
            run = mock.Mock(side_effect=AssertionError("FD authority must not infer state from Git"))
            launch = P.immutable_tool_launch(environment=fixture.environment(), run=run)
            self.assertNotEqual(fixture.code_head, fixture.state_head)
            self.assertEqual(fixture.code_head, launch.parent_sha)
            self.assertEqual(fixture.state_root, launch.state_root)
            self.assertEqual(
                f"{fixture.record['state_dev']}:{fixture.record['state_ino']}",
                launch.state_identity,
            )
            command = P.validate_command(
                "rrnewton/reverie", 597, OLD, Path("/checkout"), "agent", launch=launch
            )
            self.assertEqual(str(fixture.state_root), command[command.index("--state-root") + 1])
            self.assertEqual(launch.state_identity, command[command.index("--state-identity") + 1])
            self.assertEqual(fixture.code_head, command[command.index("--tool-parent-sha") + 1])
            self.assertEqual(
                str(fixture.base / "home/.local/libexec/dev-hermit/operational-tool"), command[0]
            )
            self.assertIn("--materialize-target", command)
            run.assert_not_called()

    def test_installed_launch_refuses_partial_or_changed_authority_without_validation(self):
        cases = (
            "foreign-executing-root", "missing-authority", "empty-authority", "malformed-authority",
            "missing-tool-root", "empty-tool-root", "malformed-tool-root", "missing-state",
            "replaced-state", "parent-mismatch", "changed-content", "unsealed",
            "content-only", "root-only", "relative-home",
        )
        for case in cases:
            with self.subTest(case=case), ExecutingAuthorityFixture(sealed=case != "unsealed") as fixture:
                environment = fixture.environment()
                executing = fixture.code
                if case == "foreign-executing-root":
                    executing = fixture.state_root
                elif case == "missing-authority":
                    environment.pop("DEV_HERMIT_TOOL_AUTHORITY")
                elif case == "empty-authority":
                    environment["DEV_HERMIT_TOOL_AUTHORITY"] = ""
                elif case == "malformed-authority":
                    environment["DEV_HERMIT_TOOL_AUTHORITY"] = "/proc/self/fd/nope"
                elif case == "missing-tool-root":
                    environment.pop("DEV_HERMIT_TOOL_ROOT")
                elif case == "empty-tool-root":
                    environment["DEV_HERMIT_TOOL_ROOT"] = ""
                elif case == "malformed-tool-root":
                    environment["DEV_HERMIT_TOOL_ROOT"] = "/proc/self/fd/nope"
                elif case == "missing-state":
                    environment.pop("DEV_HERMIT_PARENT")
                elif case == "replaced-state":
                    fixture.state_root.rename(fixture.base / "old-state")
                    fixture.state_root.mkdir()
                elif case == "parent-mismatch":
                    environment["DEV_HERMIT_TOOL_PARENT_SHA"] = fixture.state_head
                elif case == "changed-content":
                    marker = fixture.code / "marker"
                    marker.chmod(0o644)
                    marker.write_text("different bytes")
                    marker.chmod(0o444)
                elif case == "content-only":
                    environment = {"HOME": str(fixture.base), "DEV_HERMIT_TOOL_CONTENT_SHA256": ""}
                elif case == "root-only":
                    environment = {"HOME": str(fixture.base), "DEV_HERMIT_TOOL_ROOT": str(fixture.tool_root)}
                elif case == "relative-home":
                    environment["HOME"] = "relative"
                run = FakeRun()
                stdout = io.StringIO()
                with mock.patch.object(P, "ROOT", executing), redirect_stdout(stdout):
                    rc = P.main(self.args("--execute", "--json"), run=run, environment=environment)
                self.assertEqual(2, rc)
                row = json.loads(stdout.getvalue())
                self.assertEqual("UNKNOWN", row["state"])
                self.assertFalse(row["acquirable"])
                self.assertFalse(any("validate-run" in cmd for cmd in run.commands))
                self.assertFalse(any(cmd[0] == "git" for cmd in run.commands))

    def test_ordinary_linked_worktree_keeps_common_git_state_and_exact_head(self):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            canonical = base / "canonical"
            head = create_repo(canonical, "canonical")
            linked = base / "linked"
            git(canonical, "worktree", "add", "--detach", linked, head, capture=True)
            environment = {
                "HOME": "/home/tester", "DEV_HERMIT_PARENT": str(canonical),
                "DEV_HERMIT_TOOL_PARENT_SHA": head, "DEV_HERMIT_TOOL_ROOT": str(linked),
            }
            with mock.patch.object(P, "ROOT", linked):
                launch = P.immutable_tool_launch(environment=environment, run=P.run_command)
                self.assertEqual(canonical, launch.state_root)
                self.assertEqual(head, launch.parent_sha)
                with self.assertRaisesRegex(RuntimeError, "does not name the executing helper"):
                    P.immutable_tool_launch(
                        environment=dict(environment, DEV_HERMIT_TOOL_ROOT=str(canonical)),
                        run=P.run_command,
                    )

    def test_launch_descriptor_refuses_noncanonical_state_root(self):
        environment = dict(TEST_ENVIRONMENT, DEV_HERMIT_PARENT="/different/root")

        with self.assertRaisesRegex(RuntimeError, "not canonical Git root"):
            P.immutable_tool_launch(environment=environment, run=FakeRun())

    def test_launch_descriptor_ignores_ambient_git_repository_redirects(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            canonical = root / "canonical"
            wrong = root / "wrong"
            canonical_head = create_repo(canonical, "canonical")
            wrong_head = create_repo(wrong, "wrong")
            environment = {
                "HOME": "/home/tester",
                "DEV_HERMIT_PARENT": str(canonical),
                "DEV_HERMIT_TOOL_PARENT_SHA": canonical_head,
                "GIT_DIR": str(wrong / ".git"),
                "GIT_WORK_TREE": str(wrong),
            }

            with mock.patch.object(P, "ROOT", canonical):
                launch = P.immutable_tool_launch(
                    environment=environment, run=P.run_command
                )

            self.assertEqual(canonical.resolve(), launch.state_root)
            self.assertEqual(canonical_head, launch.parent_sha)
            self.assertNotEqual(wrong_head, launch.parent_sha)

    def test_full_pr_head_authority_ignores_real_repository_redirect(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            canonical = root / "canonical"
            wrong = root / "wrong"
            create_repo(canonical, "canonical")
            wrong_head = create_repo(wrong, "wrong")
            authority = canonical / "ci-hub/ci-hub"
            authority.parent.mkdir()
            authority.write_text(
                """#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parents[1]
marker = subprocess.run(
    ["git", "-C", str(root), "show", "HEAD:marker"],
    check=True,
    capture_output=True,
    text=True,
).stdout.strip()
if marker != "canonical":
    print(f"redirected authority read: {marker}", file=sys.stderr)
    raise SystemExit(2)
validated = (root / "validated").is_file()
print(json.dumps({"verdict": "VALIDATED" if validated else "NOT-VALIDATED"}))
raise SystemExit(0 if validated else 4)
"""
            )
            authority.chmod(0o755)
            git(canonical, "add", "ci-hub/ci-hub")
            git(canonical, "commit", "-m", "authority fixture", capture=True)
            canonical_head = git(
                canonical, "rev-parse", "HEAD^{commit}", capture=True
            ).stdout.strip()
            hostile = {
                **dict(os.environ),
                "HOME": str(root / "home"),
                "DEV_HERMIT_PARENT": str(canonical),
                "DEV_HERMIT_TOOL_PARENT_SHA": canonical_head,
                "GIT_DIR": str(wrong / ".git"),
                "GIT_WORK_TREE": str(wrong),
            }
            seen_environments: list[dict[str, str]] = []
            fake = FakeRun(heads=[OLD, OLD])

            def run(command, **kwargs):
                command = list(command)
                child_environment = kwargs.get("env", hostile)
                seen_environments.append(dict(child_environment))
                if command[0] == "git" or command[0] == str(authority):
                    return subprocess.run(
                        command,
                        check=kwargs.get("check", False),
                        capture_output=True,
                        text=True,
                        env=child_environment,
                    )
                if "validate-run" in command:
                    (canonical / "validated").write_text("yes\n")
                return fake(command, **{**kwargs, "env": child_environment})

            arguments = [
                "--repo",
                "rrnewton/hermit",
                "--pr",
                "7",
                "--expected-head",
                OLD,
                "--checkout",
                str(canonical),
                "--agent",
                "agent",
                "--execute",
            ]
            with mock.patch.object(P, "ROOT", canonical):
                rc = P.main(arguments, run=run, environment=hostile)

            self.assertEqual(0, rc)
            self.assertNotEqual(canonical_head, wrong_head)
            self.assertTrue(seen_environments)
            self.assertTrue(
                all(
                    P.git_env.GIT_REPOSITORY_ENV.isdisjoint(environment)
                    for environment in seen_environments
                )
            )

    def test_shell_git_selector_set_matches_python_authority(self):
        helper = P.ROOT / "ci-hub/landing/worktree-mutation-fence.sh"
        result = subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; printf "%s\\n" "${LANDER_GIT_REPOSITORY_ENV[@]}"',
                "git-env-contract",
                str(helper),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            P.git_env.GIT_REPOSITORY_ENV, frozenset(result.stdout.splitlines())
        )

    def test_land_pr_entry_ignores_real_repository_redirect(self):
        lander = P.ROOT / "ci-hub/landing/land-pr.sh"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            canonical = root / "canonical"
            wrong = root / "wrong"
            create_repo(canonical, "canonical")
            create_repo(wrong, "wrong")
            git(
                canonical,
                "remote",
                "add",
                "origin",
                "https://github.com/rrnewton/hermit.git",
            )
            git(
                wrong,
                "remote",
                "add",
                "origin",
                "https://github.com/attacker/wrong.git",
            )
            commands = root / "bin"
            commands.mkdir()
            with_proxy = commands / "with-proxy"
            proxy_environment = root / "with-proxy-environment"
            checks = "\n".join(
                f'if [ "${{{name}+x}}" = x ]; then state=hostile; fi'
                for name in sorted(P.git_env.GIT_REPOSITORY_ENV)
            )
            with_proxy.write_text(
                "#!/usr/bin/env bash\n"
                "state=clean\n"
                f"{checks}\n"
                'printf "%s\\n" "$state" >"$PROXY_ENV_MARKER"\n'
                "exit 91\n"
            )
            with_proxy.chmod(0o755)
            fence = root / "mutation.state"
            hostile = {
                **dict(os.environ),
                "PATH": f"{commands}:{os.environ['PATH']}",
                "PROXY_ENV_MARKER": str(proxy_environment),
                "GIT_DIR": str(wrong / ".git"),
                "GIT_WORK_TREE": str(wrong),
            }

            result = subprocess.run(
                [
                    str(lander),
                    "7",
                    "feature",
                    "--repo",
                    "rrnewton/hermit",
                    "--checkout",
                    str(canonical),
                    "--foreground",
                    "--_inner",
                    "--_mutation-fence",
                    str(fence),
                    "--_mutation-token",
                    "ab" * 16,
                ],
                text=True,
                capture_output=True,
                timeout=20,
                env=hostile,
            )

            self.assertEqual(2, result.returncode, result.stderr)
            self.assertIn("ABANDON: fetch origin/main failed", result.stdout)
            self.assertNotIn("checkout origin", result.stderr)
            self.assertEqual("clean\n", proxy_environment.read_text())

    def test_lander_does_not_move_caller_checkout_for_jit(self):
        script = (P.ROOT / "ci-hub/landing/land-pr.sh").read_text(encoding="utf-8")
        start = script.index("# JUST-IN-TIME PR-HEAD CI")
        end = script.index("\nfi\n[ \"$VRC\" -eq 0 ]", start)
        jit = script[start:end]
        self.assertNotIn('checkout -q --detach "$ORIG"', jit)
        self.assertNotIn("checkout -q --detach origin/main", jit)
        self.assertNotIn("WT_MUTATED=1", jit)
        parking = script[
            script.index("park_worktree_off_main(){") : script.index(
                "\n}\n\nabandon(){", script.index("park_worktree_off_main(){")
            )
        ]
        self.assertLess(parking.index("WT_MUTATED"), parking.index("git -C"))
        self.assertLess(
            parking.index("WT_LAST_OWNED_STATE"),
            parking.index('checkout -q --detach "$ORIG"'),
        )
        restore = script[
            script.index("restore_entry_checkout(){") : script.index(
                "\n}\ntrap restore_entry_checkout", script.index("restore_entry_checkout(){")
            )
        ]
        self.assertIn("lander_restore_owned_checkout", restore)
        self.assertIn("MUTATION_FENCE_OWNER", restore)
        outer = script[script.index('if [ "$INNER" -eq 0 ]; then') :]
        self.assertIn('--_mutation-fence "$MUTATION_FENCE"', outer)
        self.assertIn('--_mutation-token "$MUTATION_TOKEN"', outer)
        self.assertLess(
            outer.index("restore_entry_checkout", outer.index("rc=$?")),
            outer.index('case "$rc" in'),
        )

    def test_outer_restores_killed_inner_only_while_fence_state_is_owned(self):
        helper = P.ROOT / "ci-hub/landing/worktree-mutation-fence.sh"
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary) / "repo"
            wrong = Path(temporary) / "wrong"
            subprocess.run(
                ["git", "init", "-b", "main", str(repo)],
                check=True,
                capture_output=True,
                text=True,
            )
            subprocess.run(
                ["git", "-C", str(repo), "config", "user.name", "Test User"],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(repo), "config", "user.email", "test@example.com"],
                check=True,
            )
            (repo / "state").write_text("main\n")
            subprocess.run(["git", "-C", str(repo), "add", "state"], check=True)
            subprocess.run(
                ["git", "-C", str(repo), "commit", "-m", "main"],
                check=True,
                capture_output=True,
                text=True,
            )
            for branch in ("lander", "external"):
                subprocess.run(
                    ["git", "-C", str(repo), "checkout", "-q", "-b", branch, "main"],
                    check=True,
                )
                (repo / "state").write_text(f"{branch}\n")
                subprocess.run(["git", "-C", str(repo), "add", "state"], check=True)
                subprocess.run(
                    ["git", "-C", str(repo), "commit", "-m", branch],
                    check=True,
                    capture_output=True,
                    text=True,
                )
            create_repo(wrong, "wrong")
            hostile = {
                **dict(os.environ),
                "GIT_DIR": str(wrong / ".git"),
                "GIT_WORK_TREE": str(wrong),
            }

            harness = r'''
set -uo pipefail
helper=$1
checkout=$2
fence=$3
token=$4
external=$5
source "$helper"
entry=$(lander_checkout_state "$checkout")
: >"$fence"
bash -c '
  set -uo pipefail
  source "$1"
  git -C "$2" checkout -q lander
  lander_record_checkout_mutation "$2" "$3" "$4" "$5" lander
  while :; do sleep 1; done
' land-pr-inner "$helper" "$checkout" "$entry" "$fence" "$token" &
inner=$!
for _attempt in $(seq 1 100); do
  [ -s "$fence" ] && break
  sleep 0.01
done
[ -s "$fence" ] || { kill -KILL "$inner" 2>/dev/null || true; exit 9; }
kill -KILL "$inner"
wait "$inner" 2>/dev/null || true
if [ "$external" -eq 1 ]; then
  git -C "$checkout" checkout -q external
fi
lander_restore_owned_checkout "$checkout" main 0 "$entry" "$fence" "$token" 1
'''
            for external, expected in ((False, "main"), (True, "external")):
                with self.subTest(external=external):
                    subprocess.run(
                        ["git", "-C", str(repo), "checkout", "-q", "main"],
                        check=True,
                    )
                    fence = Path(temporary) / f"mutation-{int(external)}.state"
                    token = ("ab" if external else "cd") * 16
                    result = subprocess.run(
                        [
                            "bash",
                            "-c",
                            harness,
                            "land-pr-outer",
                            str(helper),
                            str(repo),
                            str(fence),
                            token,
                            "1" if external else "0",
                        ],
                        check=True,
                        capture_output=True,
                        text=True,
                        env=hostile,
                    )
                    branch = subprocess.run(
                        ["git", "-C", str(repo), "symbolic-ref", "--short", "HEAD"],
                        check=True,
                        capture_output=True,
                        text=True,
                    ).stdout.strip()
                    self.assertEqual(expected, branch, result.stderr)
                    if external:
                        self.assertTrue(fence.is_file())
                        self.assertIn("current state is not lander-owned", result.stderr)
                    else:
                        self.assertFalse(fence.exists())

    def test_could_not_run_relays_exact_refusal_without_becoming_product_failure(self):
        refusal = (
            "validate-run: REFUSED: explicit DEV_HERMIT_TOOL_ROOT is dirty: M hermit\n"
            "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
        )
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NOT-VALIDATED", "NOT-VALIDATED"],
            validation_rc=75,
            validation_stderr=refusal,
        )
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            rc = self.main(self.args("--execute"), run=run)
        self.assertEqual(rc, 4)
        self.assertIn(refusal, stderr.getvalue())
        self.assertIn("validate-run exited 75", stdout.getvalue())
        self.assertIn("PR-HEAD-CI NO_RESULT", stdout.getvalue())
        self.assertNotIn("PR-HEAD-CI FAILED", stdout.getvalue())

    def test_passed_validation_with_failed_writeback_is_not_called_could_not_run(self):
        for version in P.service_result.AUTHORITATIVE_SCHEMA_VERSIONS:
            with self.subTest(version=version):
                event = json.loads(
                    validation_event(scorecard_writeback_failed=True)
                )
                event["service_result_schema"] = version
                if version == P.service_result.SCHEMA_VERSION:
                    event["detail"] = None
                run = FakeRun(
                    heads=[OLD, OLD],
                    verdicts=["NOT-VALIDATED", "VALIDATED"],
                    validation_rc=75,
                    validation_stdout=json.dumps(event) + "\n",
                    validation_stderr=(
                        "validate-run: scorecard write-back did not complete\n"
                    ),
                )
                stdout = io.StringIO()
                stderr = io.StringIO()
                with redirect_stdout(stdout), redirect_stderr(stderr):
                    rc = self.main(self.args("--execute"), run=run)
                self.assertEqual(0, rc)
                self.assertIn("PR-HEAD-CI VALIDATED", stdout.getvalue())
                self.assertIn(
                    "scorecard write-back did not complete", stdout.getvalue()
                )
                self.assertIn(
                    "scorecard write-back did not complete", stderr.getvalue()
                )

    def test_concurrent_green_cannot_promote_malformed_rc75_evidence(self):
        missing = object()
        cases = (
            ("missing inner exit", "exit_code", missing),
            ("boolean inner exit", "exit_code", True),
            ("floating inner exit", "exit_code", 75.0),
            ("wrong inner exit", "exit_code", 0),
            ("missing service schema", "service_result_schema", missing),
            ("boolean service schema", "service_result_schema", True),
            ("floating service schema", "service_result_schema", 4.0),
            (
                "historical service schema",
                "service_result_schema",
                P.service_result.SELECTION_SCHEMA_VERSION,
            ),
            ("missing writeback", "scorecard_writeback", missing),
            ("null writeback", "scorecard_writeback", None),
            ("non-object writeback", "scorecard_writeback", "failed"),
            ("missing writeback error", "scorecard_writeback", {"status": "failed"}),
            (
                "empty writeback error",
                "scorecard_writeback",
                {"status": "failed", "error": " "},
            ),
        )
        for label, field, malformed in cases:
            with self.subTest(label=label):
                event = json.loads(
                    validation_event(scorecard_writeback_failed=True)
                )
                if malformed is missing:
                    event.pop(field)
                else:
                    event[field] = malformed
                run = FakeRun(
                    heads=[OLD, OLD],
                    verdicts=["NOT-VALIDATED", "VALIDATED"],
                    validation_rc=75,
                    validation_stdout=json.dumps(event) + "\n",
                )
                stdout = io.StringIO()
                with redirect_stdout(stdout):
                    rc = self.main(self.args("--execute"), run=run)
                self.assertEqual(4, rc)
                self.assertIn("PR-HEAD-CI NO_RESULT", stdout.getvalue())
                self.assertNotIn("PR-HEAD-CI VALIDATED", stdout.getvalue())

    def test_concurrent_green_does_not_replace_a_missing_terminal_event(self):
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NOT-VALIDATED", "VALIDATED"],
            validation_rc=75,
            validation_stdout="",
        )
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            rc = self.main(self.args("--execute"), run=run)
        self.assertEqual(4, rc)
        self.assertIn("emitted 0 terminal events", stdout.getvalue())

    def test_terminal_event_for_another_target_is_not_this_runs_evidence(self):
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NOT-VALIDATED", "VALIDATED"],
            validation_stdout=validation_event(target=NEW),
        )
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            rc = self.main(self.args("--execute"), run=run)
        self.assertEqual(4, rc)
        self.assertIn("terminal event names", stdout.getvalue())

    def test_malformed_terminal_output_cannot_inherit_a_concurrent_green(self):
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NOT-VALIDATED", "VALIDATED"],
            validation_stdout="{not-json}\n",
        )
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            rc = self.main(self.args("--execute"), run=run)
        self.assertEqual(4, rc)
        self.assertIn("JSON line 1 is malformed", stdout.getvalue())

    def test_terminal_event_requires_exact_supported_schema_version(self):
        missing = object()
        for schema_version in (missing, True, 1.0, 2):
            with self.subTest(schema_version=schema_version):
                event = json.loads(validation_event())
                if schema_version is missing:
                    event.pop("schema_version")
                else:
                    event["schema_version"] = schema_version
                run = FakeRun(
                    heads=[OLD, OLD],
                    verdicts=["NOT-VALIDATED", "VALIDATED"],
                    validation_stdout=json.dumps(event) + "\n",
                )
                stdout = io.StringIO()
                with redirect_stdout(stdout):
                    rc = self.main(self.args("--execute"), run=run)
                self.assertEqual(4, rc)
                self.assertIn("terminal event schema_version", stdout.getvalue())
                self.assertNotIn("PR-HEAD-CI VALIDATED", stdout.getvalue())

    def test_terminal_event_requires_exact_integer_wrapper_exit_code(self):
        cases = (
            (False, "PASSED", "VALIDATED", 0),
            (3.0, "FAILED", "FAILED", 3),
        )
        for wrapper_exit_code, status, verdict, process_exit in cases:
            with self.subTest(wrapper_exit_code=wrapper_exit_code):
                event = json.loads(
                    validation_event(
                        status=status,
                        canonical_verdict=verdict,
                        wrapper_exit_code=int(wrapper_exit_code),
                    )
                )
                event["wrapper_exit_code"] = wrapper_exit_code
                run = FakeRun(
                    heads=[OLD, OLD],
                    verdicts=["NOT-VALIDATED", verdict],
                    validation_rc=process_exit,
                    validation_stdout=json.dumps(event) + "\n",
                )
                stdout = io.StringIO()
                with redirect_stdout(stdout):
                    rc = self.main(self.args("--execute"), run=run)
                self.assertEqual(4, rc)
                self.assertIn("terminal event wrapper_exit_code", stdout.getvalue())
                self.assertNotIn("PR-HEAD-CI VALIDATED", stdout.getvalue())
                self.assertNotIn("PR-HEAD-CI FAILED", stdout.getvalue())

    def test_unexpected_runner_exit_cannot_inherit_a_concurrent_green(self):
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NOT-VALIDATED", "VALIDATED"],
            validation_rc=1,
            validation_stdout=validation_event(),
            validation_stderr="unexpected wrapper failure\n",
        )
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            rc = self.main(self.args("--execute"), run=run)
        self.assertEqual(4, rc)
        self.assertIn(
            "only exit 75 with a typed scorecard write-back failure",
            stdout.getvalue(),
        )
        self.assertIn("unexpected wrapper failure", stderr.getvalue())

    def test_failed_verdict_requires_the_same_invocations_typed_failure(self):
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NOT-VALIDATED", "FAILED"],
            validation_rc=3,
            validation_stdout=validation_event(
                status="FAILED",
                canonical_verdict="FAILED",
                wrapper_exit_code=3,
            ),
        )
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            rc = self.main(self.args("--execute"), run=run)
        self.assertEqual(3, rc)
        self.assertIn("PR-HEAD-CI FAILED", stdout.getvalue())

    def test_concurrent_failure_cannot_replace_this_invocations_pass(self):
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NOT-VALIDATED", "FAILED"],
            validation_rc=0,
            validation_stdout=validation_event(),
        )
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            rc = self.main(self.args("--execute"), run=run)
        self.assertEqual(4, rc)
        self.assertIn("PR-HEAD-CI NO_RESULT", stdout.getvalue())
        self.assertNotIn("PR-HEAD-CI FAILED", stdout.getvalue())

    def test_could_not_run_cannot_inherit_a_canonical_receipt(self):
        run = FakeRun(
            heads=[OLD, OLD],
            verdicts=["NOT-VALIDATED", "VALIDATED"],
            validation_rc=75,
            validation_stderr="validate-run: REFUSED: late launcher failure\n",
        )
        stdout = io.StringIO()
        stderr = io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            rc = self.main(self.args("--execute"), run=run)
        self.assertEqual(4, rc)
        self.assertIn("canonical reread is VALIDATED", stdout.getvalue())
        self.assertIn("PR-HEAD-CI NO_RESULT", stdout.getvalue())
        self.assertIn("late launcher failure", stderr.getvalue())

    def test_reverie_uses_same_front_door_without_hermit_profile_argument(self):
        launch = P.ImmutableToolLaunch(
            operational_tool=Path("/bootstrap"),
            state_root=Path("/state"),
            state_identity="33:44",
            parent_sha=PARENT,
        )
        command = P.validate_command(
            "rrnewton/reverie", 441, OLD, Path("/reverie"), "agent", launch=launch
        )
        self.assertIn("--repo", command)
        self.assertIn("rrnewton/reverie", command)
        self.assertNotIn("full", command)

        run = FakeRun(heads=[OLD, OLD], verdicts=["NOT-VALIDATED", "VALIDATED"])
        args = ["--repo", "rrnewton/reverie", "--pr", "441", "--expected-head", OLD,
                "--checkout", "/reverie", "--agent", "agent", "--execute"]
        self.assertEqual(self.main(args, run=run), 0)
        status_commands = [command for command in run.commands if "validate-status" in command]
        self.assertEqual(len(status_commands), 2)
        self.assertTrue(all(command[command.index("--repo") + 1] == "rrnewton/reverie"
                            for command in status_commands))


if __name__ == "__main__":
    unittest.main()
