#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path


MODULE = Path(__file__).with_name("pr_head_ci.py")
SPEC = importlib.util.spec_from_file_location("pr_head_ci", MODULE)
assert SPEC and SPEC.loader
P = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = P
SPEC.loader.exec_module(P)

OLD = "1" * 40
NEW = "2" * 40


class FakeRun:
    def __init__(self, *, heads=None, verdicts=None, approval_rc=0):
        self.heads = list(heads or [OLD])
        self.verdicts = list(verdicts or ["NOT-VALIDATED"])
        self.approval_rc = approval_rc
        self.commands = []

    def __call__(self, command, **_kwargs):
        command = list(command)
        self.commands.append(command)
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
            return subprocess.CompletedProcess(command, 0, "", "")
        raise AssertionError(command)


class PrHeadCiTest(unittest.TestCase):
    def args(self, *extra):
        return ["--repo", "rrnewton/hermit", "--pr", "7", "--expected-head", OLD,
                "--checkout", "/checkout", "--agent", "agent", *extra]

    def test_absence_never_reads_as_pass(self):
        run = FakeRun()
        self.assertEqual(P.main(self.args(), run=run), 4)
        self.assertFalse(any("validate-run" in command for command in run.commands))

    def test_noncanonical_head_is_refused_before_any_authority_read(self):
        run = FakeRun()
        args = self.args()
        args[args.index(OLD)] = "A" * 40
        self.assertEqual(P.main(args, run=run), 2)
        self.assertEqual(run.commands, [])

    def test_exact_head_late_receipt_is_validated(self):
        run = FakeRun(heads=[OLD, OLD], verdicts=["NOT-VALIDATED", "VALIDATED"])
        self.assertEqual(P.main(self.args("--execute"), run=run), 0)
        self.assertEqual(sum("validate-run" in command for command in run.commands), 1)

    def test_moved_head_expires_old_receipt(self):
        run = FakeRun(heads=[OLD, NEW], verdicts=["NOT-VALIDATED", "VALIDATED"])
        self.assertEqual(P.main(self.args("--execute"), run=run), 4)
        self.assertEqual(sum("validate-status" in command for command in run.commands), 1)

    def test_exact_head_failure_remains_red_without_rerun(self):
        run = FakeRun(verdicts=["FAILED"])
        self.assertEqual(P.main(self.args("--execute"), run=run), 3)
        self.assertFalse(any("validate-run" in command for command in run.commands))

    def test_unapproved_head_does_not_consume_validator(self):
        run = FakeRun(approval_rc=1)
        self.assertEqual(P.main(self.args("--execute"), run=run), 4)
        self.assertFalse(any("validate-run" in command for command in run.commands))

    def test_reverie_uses_same_front_door_without_hermit_profile_argument(self):
        command = P.validate_command("rrnewton/reverie", 441, OLD, Path("/reverie"), "agent")
        self.assertIn("--repo", command)
        self.assertIn("rrnewton/reverie", command)
        self.assertNotIn("full", command)

        run = FakeRun(heads=[OLD, OLD], verdicts=["NOT-VALIDATED", "VALIDATED"])
        args = ["--repo", "rrnewton/reverie", "--pr", "441", "--expected-head", OLD,
                "--checkout", "/reverie", "--agent", "agent", "--execute"]
        self.assertEqual(P.main(args, run=run), 0)
        status_commands = [command for command in run.commands if "validate-status" in command]
        self.assertEqual(len(status_commands), 2)
        self.assertTrue(all(command[command.index("--repo") + 1] == "rrnewton/reverie"
                            for command in status_commands))


if __name__ == "__main__":
    unittest.main()
