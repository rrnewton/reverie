#!/usr/bin/env python3
"""Tests for detached operational-health payload publication."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "ci-hub/bin/run-health-escalation"


class RunHealthEscalationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.assertTrue(os.access(RUNNER, os.X_OK), f"{RUNNER} must be executable")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.bin = self.root / "bin"
        self.home.mkdir()
        self.bin.mkdir()
        proxy = self.bin / "with-proxy"
        proxy.write_text('#!/bin/sh\nexec "$@"\n')
        proxy.chmod(0o755)
        self.record = self.root / "args"
        self.escalate = self.root / "escalate"
        self.escalate.write_text(
            "#!/usr/bin/env bash\n"
            "{ printf 'CALL\\n'; printf '%s\\n' \"$@\"; } >> \"$ESCALATE_ARGV\"\n"
            "sink=\n"
            "while (( $# )); do\n"
            "  if [[ $1 == --sink ]]; then sink=$2; break; fi\n"
            "  shift\n"
            "done\n"
            "sleep \"${ESCALATE_DELAY:-0}\"\n"
            "[[ ${ESCALATE_FAIL_SINK:-} != \"$sink\" ]]\n"
        )
        self.escalate.chmod(0o755)
        self.environment = os.environ | {
            "HOME": str(self.home),
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "HERMIT_HEALTH_ESCALATE": str(self.escalate),
            "ESCALATE_ARGV": str(self.record),
        }

    def _run(
        self, *args: str, fail_sink: str = ""
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(RUNNER), *args],
            cwd=ROOT,
            env=self.environment | {"ESCALATE_FAIL_SINK": fail_sink},
            text=True,
            capture_output=True,
            check=False,
        )

    def test_both_help_forms_succeed(self) -> None:
        for flag in ("-h", "--help"):
            with self.subTest(flag=flag):
                result = self._run(flag)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertIn("--queue DIR", result.stdout)

    def test_success_drains_alarm_then_resolve_in_order(self) -> None:
        queue = self.root / "queue"
        queue.mkdir()
        alarm = queue / "event-001.alarm"
        resolve = queue / "event-002.resolve"
        alarm.write_text("ACTION: fixture\n")
        resolve.write_text("")
        result = self._run("--queue", str(queue))

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual([], list(queue.glob("event-*")))
        arguments = self.record.read_text().splitlines()
        calls = "\n".join(arguments).split("CALL\n")[1:]
        self.assertEqual(4, len(calls))
        self.assertIn("taskgraph", calls[0])
        self.assertIn(str(alarm), calls[0])
        self.assertIn("--resolve", calls[1])
        self.assertIn("taskgraph", calls[1])
        self.assertIn("github", calls[2])
        self.assertIn(str(alarm), calls[2])
        self.assertIn("--resolve", calls[3])
        self.assertIn("github", calls[3])

    def test_github_failure_does_not_block_later_taskgraph_resolve(self) -> None:
        queue = self.root / "queue"
        queue.mkdir()
        alarm = queue / "event-001.alarm"
        resolve = queue / "event-002.resolve"
        alarm.write_text("ERROR: fixture\n")
        resolve.write_text("")
        result = self._run("--queue", str(queue), fail_sink="github")

        self.assertEqual(1, result.returncode)
        self.assertTrue(alarm.exists())
        self.assertTrue(resolve.exists())
        calls = "\n".join(self.record.read_text().splitlines()).split("CALL\n")[1:]
        self.assertEqual(3, len(calls))
        self.assertIn("taskgraph", calls[0])
        self.assertIn("--resolve", calls[1])
        self.assertIn("taskgraph", calls[1])
        self.assertIn("github", calls[2])
        self.assertNotIn("--resolve", calls[2])
        self.assertTrue(Path(f"{alarm}.taskgraph.done").exists())
        self.assertTrue(Path(f"{resolve}.taskgraph.done").exists())
        self.assertFalse(Path(f"{alarm}.github.done").exists())
        log = self.home / ".local/state/hermit-health-escalation.log"
        self.assertIn("sink=github escalation_rc=1", log.read_text())

        self.record.write_text("")
        retry = self._run("--queue", str(queue))
        self.assertEqual(0, retry.returncode, retry.stderr)
        retry_calls = "\n".join(self.record.read_text().splitlines()).split("CALL\n")[1:]
        self.assertEqual(2, len(retry_calls))
        self.assertTrue(all("github" in call for call in retry_calls))
        self.assertNotIn("--resolve", retry_calls[0])
        self.assertIn("--resolve", retry_calls[1])
        self.assertEqual([], list(queue.glob("event-*")))

    def test_missing_queue_redirects_to_the_required_argument(self) -> None:
        result = self._run()

        self.assertEqual(2, result.returncode)
        self.assertIn("--queue DIR is required", result.stderr)
        self.assertIn("--help", result.stderr)

    def test_second_worker_waits_and_drains_event_added_during_first_worker(self) -> None:
        queue = self.root / "queue"
        queue.mkdir()
        first = queue / "event-001.alarm"
        second = queue / "event-002.resolve"
        first.write_text("ACTION: fixture\n")
        first_worker = subprocess.Popen(
            [str(RUNNER), "--queue", str(queue)],
            cwd=ROOT,
            env=self.environment | {"ESCALATE_DELAY": "0.15"},
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        deadline = time.monotonic() + 3
        while not self.record.exists() or "CALL" not in self.record.read_text():
            if first_worker.poll() is not None or time.monotonic() >= deadline:
                stdout, stderr = first_worker.communicate()
                self.fail(f"first worker did not start: {stdout}\n{stderr}")
            time.sleep(0.01)
        second.write_text("")
        second_worker = subprocess.Popen(
            [str(RUNNER), "--queue", str(queue)],
            cwd=ROOT,
            env=self.environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        first_stdout, first_stderr = first_worker.communicate(timeout=5)
        second_stdout, second_stderr = second_worker.communicate(timeout=5)
        self.assertEqual(0, first_worker.returncode, first_stdout + first_stderr)
        self.assertEqual(0, second_worker.returncode, second_stdout + second_stderr)
        self.assertEqual([], list(queue.glob("event-*")))
        calls = "\n".join(self.record.read_text().splitlines()).split("CALL\n")[1:]
        resolve_calls = [call for call in calls if "--resolve" in call]
        self.assertEqual(2, len(resolve_calls))
        self.assertEqual(
            {"taskgraph", "github"},
            {
                sink
                for sink in ("taskgraph", "github")
                if any(sink in call for call in resolve_calls)
            },
        )


if __name__ == "__main__":
    unittest.main()
