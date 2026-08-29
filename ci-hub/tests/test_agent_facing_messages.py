#!/usr/bin/env python3
"""Execution tests for hand-written ci-hub help and usage messages."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "ci-hub" / "bin"


def run(name: str, *arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(BIN / name), *arguments],
        cwd="/tmp",
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=20,
    )


class AgentFacingMessageTests(unittest.TestCase):
    def test_hand_written_help_is_visible_on_stdout_and_succeeds(self) -> None:
        for name in (
            "agent-tool",
            "commit-verdict",
            "gh-api",
            "gh-merge-verified",
            "git-push-verified",
            "tg-note-verified",
            "wrkslots",
        ):
            with self.subTest(name=name):
                result = run(name, "--help")
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertTrue(result.stdout.strip(), f"{name} printed no help on stdout")
                self.assertIn("usage", result.stdout.casefold())

    def test_hand_written_usage_errors_name_cause_and_next_action(self) -> None:
        cases = (
            ("agent-tool", ("not-a-tool",), "unknown tool", "choose one"),
            ("commit-verdict", (), "missing commit SHA", "Pass one or more"),
            ("gh-api", (), "missing API path", "Pass /repos/OWNER/REPO"),
            ("gh-merge-verified", ("not-a-pr",), "pull request number", "Pass the numeric"),
            ("git-push-verified", ("origin",), "missing remote or", "Pass both values"),
            ("tg-note-verified", (), "missing TASK_ID", "Pass the task id"),
        )
        for name, arguments, cause, action in cases:
            with self.subTest(name=name):
                result = run(name, *arguments)
                self.assertNotEqual(0, result.returncode)
                self.assertIn(cause, result.stderr)
                self.assertIn(action, result.stderr)

    def test_git_push_refspec_error_names_required_shape(self) -> None:
        result = run("git-push-verified", "origin", "main")
        self.assertEqual(1, result.returncode)
        self.assertIn("has no ':'", result.stderr)
        self.assertIn("Pass <src>:<dst>", result.stderr)

    def test_cap_log_distinguishes_not_found_from_not_executable(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ghchecks-cap-log-") as directory:
            missing = run(
                "cap-log",
                "--max-bytes",
                "1K",
                "--stderr",
                str(Path(directory) / "missing-stderr"),
                "--",
                "definitely-no-such-command",
            )
            self.assertEqual(127, missing.returncode)
            self.assertIn("NEVER RAN", missing.stderr)
            self.assertIn("Install it or pass the correct executable path", missing.stderr)

            command = Path(directory) / "not-executable"
            command.write_text("#!/bin/sh\nexit 0\n")
            refused = run(
                "cap-log",
                "--max-bytes",
                "1K",
                "--stderr",
                str(Path(directory) / "stderr"),
                "--",
                str(command),
            )
        self.assertEqual(126, refused.returncode)
        self.assertIn("NEVER RAN", refused.stderr)
        self.assertIn("Check its executable bit and interpreter", refused.stderr)

    def test_gh_merge_owned_option_missing_value_is_not_forwarded(self) -> None:
        result = run("gh-merge-verified", "123", "--expect-head")
        self.assertEqual(1, result.returncode)
        self.assertIn("--expect-head needs the exact 40-hex PR head", result.stderr)
        self.assertIn("run --help", result.stderr)

    def test_edit_owners_bad_root_is_a_message_not_a_traceback(self) -> None:
        result = run("edit-owners", "--root", "/definitely/not/a/repository")
        self.assertEqual(2, result.returncode)
        self.assertIn("is not a directory", result.stderr)
        self.assertIn("Pass --root PATH", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_ancestry_audit_bad_root_stops_before_network_and_names_remedy(self) -> None:
        result = run(
            "ancestry-audit",
            "--root",
            "/definitely/not/a/repository",
            "--no-fetch",
        )
        self.assertEqual(2, result.returncode)
        self.assertIn("no checkout", result.stderr)
        self.assertIn("Pass --root PATH", result.stderr)
        self.assertNotIn("api.github.com", result.stderr)

    def test_task_scan_without_a_query_says_which_flag_to_add(self) -> None:
        result = run("tg-dup-scan")
        self.assertEqual(1, result.returncode)
        self.assertIn("no query selected", result.stderr)
        self.assertIn("--owner AGENT", result.stderr)

    def test_task_scan_bad_database_says_how_to_select_one(self) -> None:
        result = run(
            "tg-dup-scan",
            "--topic",
            "anything",
            "--db",
            "/definitely/not/a/database",
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("cannot open the database", result.stderr)
        self.assertIn("Pass --db PATH", result.stderr)

    def test_refusal_probe_bad_checkout_says_what_path_is_required(self) -> None:
        result = run("refusal-predicate-gate-probes", "/definitely/not/a/repository")
        self.assertEqual(2, result.returncode)
        self.assertIn("no such Hermit repository", result.stderr)
        self.assertIn("Pass the path to an existing Hermit checkout", result.stderr)

    def test_reconcile_bad_offline_files_name_the_inputs_and_remedy(self) -> None:
        result = run(
            "reconcile-receipts",
            "--prs-json",
            "/definitely/not/a/file",
            "--history-json",
            "/definitely/not/another",
            "--no-fetch",
        )
        self.assertEqual(3, result.returncode)
        self.assertIn("COULD NOT READ --prs-json", result.stderr)
        self.assertIn("Pass readable JSON files", result.stderr)

    def test_publication_reason_file_error_says_how_to_correct_it(self) -> None:
        result = run(
            "publication-exempt",
            "fixture-task",
            "--repo",
            "rrnewton/hermit",
            "--pr",
            "1",
            "--reason-file",
            "/definitely/not/a/file",
        )
        self.assertEqual(2, result.returncode)
        self.assertIn("cannot read reason file", result.stderr)
        self.assertIn("pass a readable --reason-file", result.stderr)

    def test_comment_fixture_error_says_how_to_correct_it(self) -> None:
        result = run(
            "changes-requested-gate",
            "--repo",
            "rrnewton/hermit",
            "--pr",
            "1",
            "--from-json",
            "/definitely/not/a/file",
        )
        self.assertEqual(2, result.returncode)
        self.assertIn("could not read comment records", result.stdout)
        self.assertIn("Pass a readable JSON file to --from-json", result.stdout)

    def test_close_task_bad_artifact_names_a_valid_alternative(self) -> None:
        result = run(
            "close-task",
            "fixture-task",
            "--artifact",
            "/etc/hostname",
            "--check-only",
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("outside the versioned workspace", result.stderr)
        self.assertIn("ACTION: pass a URL", result.stderr)


if __name__ == "__main__":
    unittest.main()
