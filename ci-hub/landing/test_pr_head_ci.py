#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import io
import json
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

OLD = "1" * 40
NEW = "2" * 40
PARENT = "3" * 40
TEST_ENVIRONMENT = {
    "HOME": "/home/tester",
    "DEV_HERMIT_PARENT": str(P.ROOT),
    "DEV_HERMIT_TOOL_PARENT_SHA": PARENT,
}


class FakeRun:
    def __init__(
        self,
        *,
        heads=None,
        verdicts=None,
        approval_rc=0,
        validation_rc=0,
        validation_stdout="",
        validation_stderr="",
    ):
        self.heads = list(heads or [OLD])
        self.verdicts = list(verdicts or ["NOT-VALIDATED"])
        self.approval_rc = approval_rc
        self.validation_rc = validation_rc
        self.validation_stdout = validation_stdout
        self.validation_stderr = validation_stderr
        self.commands = []

    def __call__(self, command, **_kwargs):
        command = list(command)
        self.commands.append(command)
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
            return subprocess.CompletedProcess(command, rc, json.dumps({"verdict": verdict}), "")
        if "validate-run" in command:
            return subprocess.CompletedProcess(
                command,
                self.validation_rc,
                self.validation_stdout,
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

    def test_unapproved_head_does_not_consume_validator(self):
        run = FakeRun(approval_rc=1)
        self.assertEqual(self.main(self.args("--execute"), run=run), 4)
        self.assertFalse(any("validate-run" in command for command in run.commands))

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
        self.assertEqual(command[11:14], ["validate-run", "--checkout", "/tmp/exact-head"])
        self.assertIn("--materialize-target", command)
        self.assertNotEqual(command[0], str(P.ROOT / "ci-hub/ci-hub"))

    def test_launch_descriptor_binds_canonical_state_identity_and_executing_head(self):
        run = FakeRun()

        launch = P.immutable_tool_launch(environment=TEST_ENVIRONMENT, run=run)

        metadata = P.ROOT.stat()
        self.assertEqual(P.ROOT, launch.state_root)
        self.assertEqual(f"{metadata.st_dev}:{metadata.st_ino}", launch.state_identity)
        self.assertEqual(PARENT, launch.parent_sha)

    def test_launch_descriptor_refuses_noncanonical_state_root(self):
        environment = dict(TEST_ENVIRONMENT, DEV_HERMIT_PARENT="/different/root")

        with self.assertRaisesRegex(RuntimeError, "not canonical Git root"):
            P.immutable_tool_launch(environment=environment, run=FakeRun())

    def test_launch_descriptor_ignores_ambient_git_repository_redirects(self):
        def create_repo(path: Path, marker: str) -> str:
            path.mkdir()
            subprocess.run(
                ["git", "init", "-b", "main", str(path)],
                check=True,
                capture_output=True,
                text=True,
            )
            subprocess.run(
                ["git", "-C", str(path), "config", "user.name", "Test User"],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(path), "config", "user.email", "test@example.com"],
                check=True,
            )
            (path / "marker").write_text(marker)
            subprocess.run(["git", "-C", str(path), "add", "marker"], check=True)
            subprocess.run(
                ["git", "-C", str(path), "commit", "-m", marker],
                check=True,
                capture_output=True,
                text=True,
            )
            return subprocess.run(
                ["git", "-C", str(path), "rev-parse", "HEAD^{commit}"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()

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
