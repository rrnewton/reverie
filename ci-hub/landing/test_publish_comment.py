#!/usr/bin/env python3
"""Success and failure brackets for landing-comment publication."""

from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime, timezone
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

import publish_comment as subject


FIXED_NOW = datetime(2026, 8, 12, 7, 15, tzinfo=timezone.utc)


class CaptureRunner:
    def __init__(self, returncode: int, *, stdout: str = "", stderr: str = "") -> None:
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr
        self.commands: list[list[str]] = []

    def __call__(
        self, command: list[str], *, text: bool, capture_output: bool
    ) -> subprocess.CompletedProcess[str]:
        self.assert_runner_options(text=text, capture_output=capture_output)
        self.commands.append(command)
        return subprocess.CompletedProcess(
            command, self.returncode, stdout=self.stdout, stderr=self.stderr
        )

    @staticmethod
    def assert_runner_options(*, text: bool, capture_output: bool) -> None:
        if not text or not capture_output:
            raise AssertionError("publisher must capture the real comment diagnostic")


def request(record_dir: Path, kind: str = "abandoned") -> subject.Request:
    return subject.Request(
        repo="rrnewton/hermit",
        pr=2144,
        agent="codex-overhaul",
        team="hermit2",
        kind=kind,
        reason="exact-head review authority refused",
        record_dir=record_dir,
    )


class PublishCommentTest(unittest.TestCase):
    def test_success_crosses_registered_boundary_and_writes_no_failure_record(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            record_dir = Path(raw)
            runner = CaptureRunner(0)
            output = io.StringIO()
            with redirect_stdout(output):
                rc = subject.publish(
                    request(record_dir), runner=runner, now=FIXED_NOW, pid=44
                )

            self.assertEqual(rc, 0)
            self.assertEqual(list(record_dir.iterdir()), [])
            command = runner.commands[0]
            self.assertEqual(Path(command[0]), subject.COMMENT_WRAPPER)
            self.assertIn(
                [
                    "--who",
                    "codex-overhaul",
                    "--team",
                    "hermit2",
                    "--role",
                    "reviewer",
                ],
                [command[index : index + 6] for index in range(len(command) - 5)],
            )
            self.assertIn("ABANDONED landing attempt", command[-1])
            self.assertIn("LANDING COMMENT POSTED", output.getvalue())

    def test_failed_write_is_loud_and_durably_records_the_real_error(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            record_dir = Path(raw)
            runner = CaptureRunner(17, stderr="remote write rejected")
            error = io.StringIO()
            with redirect_stderr(error):
                rc = subject.publish(
                    request(record_dir), runner=runner, now=FIXED_NOW, pid=55
                )

            self.assertEqual(rc, 1)
            records = list(record_dir.glob("*-comment-failed.json"))
            self.assertEqual(len(records), 1)
            payload = json.loads(records[0].read_text(encoding="utf-8"))
            self.assertEqual(payload["repository"], "rrnewton/hermit")
            self.assertEqual(payload["pr"], 2144)
            self.assertEqual(payload["kind"], "abandoned")
            self.assertEqual(payload["role"], "reviewer")
            self.assertEqual(payload["comment_exit_code"], 17)
            self.assertEqual(payload["comment_error"], "remote write rejected")
            self.assertIn("LANDING COMMENT PUBLICATION FAILED", error.getvalue())
            self.assertIn(f"durable_record={records[0]}", error.getvalue())

    def test_record_write_failure_is_also_loud(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            record_dir = Path(raw) / "not-a-directory"
            record_dir.write_text("occupied", encoding="utf-8")
            runner = CaptureRunner(19, stderr="remote unavailable")
            error = io.StringIO()
            with redirect_stderr(error):
                rc = subject.publish(
                    request(record_dir), runner=runner, now=FIXED_NOW, pid=66
                )

            self.assertEqual(rc, 2)
            self.assertIn("LANDING COMMENT PUBLICATION FAILED", error.getvalue())
            self.assertIn("could not write the local failure record", error.getvalue())
            self.assertIn("remote unavailable", error.getvalue())

    def test_all_existing_landing_comment_kinds_are_supported(self) -> None:
        for kind in subject.KINDS:
            with self.subTest(kind=kind):
                body = subject.comment_body(kind, "fixture reason")
                self.assertIn("fixture reason", body)

    def test_lander_has_no_raw_comment_write_or_discarded_error(self) -> None:
        lander = (subject.ROOT / "ci-hub/landing/land-pr.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn('python3 "$SCRIPT_DIR/publish_comment.py"', lander)
        self.assertNotIn("with-proxy gh pr comment", lander)
        self.assertNotIn("could not post ABANDON comment", lander)
        self.assertNotIn("[coordinator, $MODEL]", lander)


if __name__ == "__main__":
    unittest.main()
