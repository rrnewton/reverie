#!/usr/bin/env python3
"""End-to-end contract tests for the landing-observability reminder."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
TOOL = ROOT / "ci-hub" / "health" / "approval_binding.py"
CONFIG = ROOT / "ci-hub" / "health" / "tick-hub.yaml"
AB_SPEC = importlib.util.spec_from_file_location("approval_binding", TOOL)
assert AB_SPEC is not None and AB_SPEC.loader is not None
AB = importlib.util.module_from_spec(AB_SPEC)
AB_SPEC.loader.exec_module(AB)
sys.path.insert(0, str(ROOT / "agent-utils" / "py"))

from tick_hub.cadence import unresolved_render_state_keys  # noqa: E402
from tick_hub.engine import parse_kv_lines, run_tick  # noqa: E402
from tick_hub.io import config_from_yaml  # noqa: E402
from tick_hub.model import GateWhen, TickConfig  # noqa: E402
from tick_hub.probes import GlobFileAgeProbe, SubprocessGateRunner  # noqa: E402
from tick_hub.state import OpsState  # noqa: E402


def git(repo: Path, *args: str, env: dict[str, str] | None = None) -> None:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        text=True,
        capture_output=True,
        check=False,
        env=env,
    )
    if result.returncode != 0:
        raise AssertionError(
            f"git {' '.join(args)} failed with {result.returncode}: {result.stderr}"
        )


class LandingObservabilityTickTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="landing-observability-empty-"
        )
        self.checkout = Path(self.temporary.name)
        git(self.checkout, "init", "--initial-branch=main")
        git(self.checkout, "config", "user.name", "tick-hub-test")
        git(self.checkout, "config", "user.email", "tick-hub-test@example.invalid")
        (self.checkout / "README").write_text("old landing\n", encoding="utf-8")
        git(self.checkout, "add", "README")
        old_date = "2000-01-01T00:00:00Z"
        git(
            self.checkout,
            "commit",
            "-m",
            "old landing",
            env=os.environ
            | {"GIT_AUTHOR_DATE": old_date, "GIT_COMMITTER_DATE": old_date},
        )
        git(
            self.checkout,
            "remote",
            "add",
            "origin",
            "https://github.com/rrnewton/hermit.git",
        )
        git(self.checkout, "update-ref", "refs/remotes/origin/main", "HEAD")

        config = config_from_yaml(CONFIG.read_text(encoding="utf-8"))
        self.reminder = config.by_name()["landing_observability"]
        self.assertIsNotNone(self.reminder.gate)
        assert self.reminder.gate is not None
        self.assertTrue(self.reminder.gate.capture)
        self.assertIs(self.reminder.gate.when, GateWhen.ALWAYS)
        self.assertIn("--landing-observability", self.reminder.gate.cmd)
        self.assertIn("--since \"1 day ago\"", self.reminder.gate.cmd)

        self.command = [
            sys.executable,
            str(TOOL),
            "--repo",
            "rrnewton/hermit",
            "--checkout",
            str(self.checkout),
            "--landing-observability",
            "--since",
            "1 day ago",
        ]
        malformed = self.checkout / "malformed_landing_observability.py"
        malformed.write_text(
            "\n".join(
                (
                    "import importlib.util",
                    f"spec = importlib.util.spec_from_file_location('ab', {str(TOOL)!r})",
                    "ab = importlib.util.module_from_spec(spec)",
                    "spec.loader.exec_module(ab)",
                    "ab.emit_landing_observability({'state': 'NO_RESULT'}, False)",
                )
            )
            + "\n",
            encoding="utf-8",
        )
        self.malformed_command = [sys.executable, str(malformed)]
        gate = replace(
            self.reminder.gate,
            cmd=shlex.join(self.command),
            timeout_secs=10,
        )
        self.reminder = replace(self.reminder, gate=gate)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_empty_window_is_renderable_no_result_and_clears_failures(self) -> None:
        producer = subprocess.run(
            self.command,
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
            timeout=10,
        )
        self.assertEqual(75, producer.returncode, producer)
        fields = parse_kv_lines(producer.stdout)
        self.assertEqual("NO_RESULT", fields.get("state"))
        self.assertEqual("empty_window", fields.get("reason"))
        self.assertNotIn("detail", fields)
        self.assertEqual(
            "landing observability could not be determined: landing observability "
            "refuses an empty window: zero commits measured is not zero unobserved "
            "landings, it is no measurement",
            fields["summary"],
        )
        with self.assertRaises(TypeError):
            AB.landing_observability_no_result(
                "empty_window", detail="NO_RESULT"
            )
        self.assertNotIn("{summary}", producer.stdout)
        self.assertIn("NO RESULT: landing observability refuses an empty window", producer.stderr)

        count_key, first_key = unresolved_render_state_keys(
            "landing_observability"
        )
        result = run_tick(
            TickConfig(reminders=(self.reminder,)),
            OpsState.default(),
            now=1789160818,
            fired={count_key: 5, first_key: 1789159018},
            gate_runner=SubprocessGateRunner(),
            age_probe=GlobFileAgeProbe(),
        )
        output = "\n".join(result.lines)
        self.assertIn("NO_RESULT: landing_observability", output)
        self.assertIn(fields["summary"], output)
        self.assertNotIn(
            "NOTE: review-binding coverage of yesterday's landings", output
        )
        self.assertNotIn("unresolved placeholder", output)
        self.assertNotIn("{summary}", output)
        self.assertEqual(1, result.actions_emitted)
        self.assertEqual(0, result.fired.get(count_key, 0))
        self.assertNotIn(count_key, result.fired)
        self.assertNotIn(first_key, result.fired)
        self.assertNotIn("landing_observability", result.fired)

    def test_git_log_failure_without_stderr_gets_explicit_detail(self) -> None:
        stdout = io.StringIO()
        stderr = io.StringIO()
        remote = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="https://github.com/rrnewton/hermit.git\n",
            stderr="",
        )
        failed_log = subprocess.CompletedProcess(
            args=[], returncode=9, stdout="", stderr=""
        )
        with (
            patch.object(AB.bounded_capture, "git", return_value=remote),
            patch.object(AB.subprocess, "run", return_value=failed_log),
            contextlib.redirect_stdout(stdout),
            contextlib.redirect_stderr(stderr),
        ):
            returncode = AB.report_landing_observability(
                "rrnewton/hermit", "1 day ago", False, str(self.checkout)
            )
        fields = parse_kv_lines(stdout.getvalue())
        self.assertEqual(75, returncode)
        self.assertEqual("git_log_failed", fields.get("reason"))
        self.assertEqual("1 day ago", fields.get("since"))
        self.assertEqual(
            "git exited 9 without an error message",
            fields.get("diagnostic"),
        )
        self.assertEqual(
            "landing observability could not be determined: git log failed for "
            "window=1 day ago: git exited 9 without an error message",
            fields.get("summary"),
        )
        self.assertIn("NO RESULT: git log failed for window=", stderr.getvalue())

    def test_missing_summary_remains_a_render_refusal(self) -> None:
        count_key, first_key = unresolved_render_state_keys(
            "landing_observability"
        )
        malformed = subprocess.run(
            self.malformed_command,
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
            timeout=10,
        )
        self.assertNotEqual(0, malformed.returncode)
        self.assertNotEqual(75, malformed.returncode)
        self.assertEqual("", malformed.stdout)
        self.assertIn(
            "landing observability report requires a nonempty summary",
            malformed.stderr,
        )
        assert self.reminder.gate is not None
        malformed_gate = replace(
            self.reminder.gate,
            cmd=shlex.join(self.malformed_command),
        )
        result = run_tick(
            TickConfig(reminders=(replace(self.reminder, gate=malformed_gate),)),
            OpsState.default(),
            now=1789160818,
            fired={count_key: 5, first_key: 1789159018},
            gate_runner=SubprocessGateRunner(),
            age_probe=GlobFileAgeProbe(),
        )
        output = "\n".join(result.lines)
        self.assertIn(
            "refusing emission with unresolved placeholder(s): {summary}", output
        )
        self.assertNotIn(
            "NOTE: review-binding coverage of yesterday's landings", output
        )
        self.assertNotIn("NO_RESULT: landing_observability", output)
        self.assertEqual(6, result.fired[count_key])
        self.assertEqual(1789159018, result.fired[first_key])
        self.assertNotIn("landing_observability", result.fired)

    def test_generic_no_result_summary_cannot_leave_the_producer(self) -> None:
        generic = self.checkout / "generic_landing_observability.py"
        generic.write_text(
            "\n".join(
                (
                    "import importlib.util",
                    f"spec = importlib.util.spec_from_file_location('ab', {str(TOOL)!r})",
                    "ab = importlib.util.module_from_spec(spec)",
                    "spec.loader.exec_module(ab)",
                    "ab.emit_landing_observability("
                    "{'state': 'NO_RESULT', 'reason': 'empty_window', "
                    "'summary': 'landing observability could not be determined: "
                    "NO_RESULT'}, False)",
                )
            )
            + "\n",
            encoding="utf-8",
        )
        malformed = subprocess.run(
            [sys.executable, str(generic)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
            timeout=10,
        )
        self.assertNotEqual(0, malformed.returncode)
        self.assertNotEqual(75, malformed.returncode)
        self.assertEqual("", malformed.stdout)
        self.assertIn(
            "landing observability report does not match its structured fields",
            malformed.stderr,
        )


if __name__ == "__main__":
    unittest.main()
