#!/usr/bin/env python3
"""Tests for detached operational-health payload publication."""

from __future__ import annotations

import os
from pathlib import Path
import json
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "ci-hub/bin/run-health-escalation"
CIRCUIT = ROOT / "ci-hub/bin/health-escalation-circuit"
sys.path.insert(0, str(ROOT / "ci-hub/lib"))
from health_escalation_event import make_event, write_event  # noqa: E402


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
            "if [[ ${ESCALATE_PERMANENT_SINK:-} == \"$sink\" ]]; then\n"
            "  printf 'diagnostic-start:%09000d:diagnostic-end\\n' 0 >&2\n"
            "  exit 78\n"
            "fi\n"
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
        self, *args: str, fail_sink: str = "", permanent_sink: str = ""
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(RUNNER), *args],
            cwd=ROOT,
            env=self.environment | {
                "ESCALATE_FAIL_SINK": fail_sink,
                "ESCALATE_PERMANENT_SINK": permanent_sink,
            },
            text=True,
            capture_output=True,
            check=False,
        )

    def _event(self, queue: Path, sequence: int, kind: str, payload: str = "") -> Path:
        event_id = f"event-20260903T000000{sequence:06d}Z-1-{sequence:08x}"
        return write_event(
            queue,
            make_event(
                kind=kind,
                payload=payload,
                origin_host="devbig014.example.com",
                findings=1 if kind == "alarm" else 0,
                event_id=event_id,
                created_at=f"2026-09-03T00:00:{sequence:02d}Z",
            ),
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
        alarm = self._event(queue, 1, "alarm", "ACTION: fixture\n")
        resolve = self._event(queue, 2, "resolve")
        result = self._run("--queue", str(queue))

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual([], list(queue.glob("event-*")))
        arguments = self.record.read_text().splitlines()
        calls = "\n".join(arguments).split("CALL\n")[1:]
        self.assertEqual(4, len(calls))
        self.assertIn("taskgraph", calls[0])
        self.assertIn(str(alarm), calls[0])
        self.assertIn(str(resolve), calls[1])
        self.assertIn("taskgraph", calls[1])
        self.assertIn("github", calls[2])
        self.assertIn(str(alarm), calls[2])
        self.assertIn(str(resolve), calls[3])
        self.assertIn("github", calls[3])

    def test_github_failure_does_not_block_later_taskgraph_resolve(self) -> None:
        queue = self.root / "queue"
        queue.mkdir()
        alarm = self._event(queue, 1, "alarm", "ERROR: fixture\n")
        resolve = self._event(queue, 2, "resolve")
        result = self._run("--queue", str(queue), fail_sink="github")

        self.assertEqual(1, result.returncode)
        self.assertTrue(alarm.exists())
        self.assertTrue(resolve.exists())
        calls = "\n".join(self.record.read_text().splitlines()).split("CALL\n")[1:]
        self.assertEqual(3, len(calls))
        self.assertIn("taskgraph", calls[0])
        self.assertIn(str(resolve), calls[1])
        self.assertIn("taskgraph", calls[1])
        self.assertIn("github", calls[2])
        self.assertIn(str(alarm), calls[2])
        self.assertTrue(Path(f"{alarm}.taskgraph.done").exists())
        self.assertTrue(Path(f"{resolve}.taskgraph.done").exists())
        self.assertFalse(Path(f"{alarm}.github.done").exists())
        log = self.home / ".local/state/hermit-health-escalation.log"
        log_records = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertTrue(any(
            record["sink"] == "github" and record["returncode"] == 1
            for record in log_records
        ))

        self.record.write_text("")
        retry = self._run("--queue", str(queue))
        self.assertEqual(0, retry.returncode, retry.stderr)
        retry_calls = "\n".join(self.record.read_text().splitlines()).split("CALL\n")[1:]
        self.assertEqual(2, len(retry_calls))
        self.assertTrue(all("github" in call for call in retry_calls))
        self.assertIn(str(alarm), retry_calls[0])
        self.assertIn(str(resolve), retry_calls[1])
        self.assertEqual([], list(queue.glob("event-*")))

    def test_missing_queue_redirects_to_the_required_argument(self) -> None:
        result = self._run()

        self.assertEqual(2, result.returncode)
        self.assertIn("--queue DIR is required", result.stderr)
        self.assertIn("--help", result.stderr)

    def test_second_worker_waits_and_drains_event_added_during_first_worker(self) -> None:
        queue = self.root / "queue"
        queue.mkdir()
        first = self._event(queue, 1, "alarm", "ACTION: fixture\n")
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
        second = self._event(queue, 2, "resolve")
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
        self.assertEqual(4, len(calls))
        self.assertIn("taskgraph", calls[0])
        self.assertIn(str(first), calls[0])
        self.assertIn("github", calls[1])
        self.assertIn(str(first), calls[1])
        self.assertIn("taskgraph", calls[2])
        self.assertIn(str(second), calls[2])
        self.assertIn("github", calls[3])
        self.assertIn(str(second), calls[3])
        resolve_calls = [call for call in calls if str(second) in call]
        self.assertEqual(2, len(resolve_calls))
        self.assertEqual(
            {"taskgraph", "github"},
            {
                sink
                for sink in ("taskgraph", "github")
                if any(sink in call for call in resolve_calls)
            },
        )

    def test_legacy_payload_is_refused_without_publisher_invocation(self) -> None:
        queue = self.root / "queue"
        queue.mkdir()
        legacy = queue / "event-001.alarm"
        legacy.write_text("ACTION: cannot attribute host\n")

        result = self._run("--queue", str(queue))

        self.assertEqual(2, result.returncode)
        self.assertIn("legacy payload-only", result.stderr)
        self.assertTrue(legacy.exists())
        self.assertFalse(self.record.exists())

    def test_event_like_json_with_no_durable_identity_is_not_silently_skipped(self) -> None:
        queue = self.root / "queue"
        queue.mkdir()
        unknown = queue / "event-no-identity.json"
        unknown.write_text("{}")

        result = self._run("--queue", str(queue))

        self.assertEqual(2, result.returncode)
        self.assertIn("do not match the schema-v1 event identity", result.stderr)
        self.assertTrue(unknown.exists())
        self.assertFalse(self.record.exists())

    def test_permanent_circuit_suppresses_retry_but_not_other_sink(self) -> None:
        queue = self.root / "queue"
        queue.mkdir()
        event = self._event(queue, 1, "alarm", "ACTION: permanent fixture\n")

        first = self._run("--queue", str(queue), permanent_sink="taskgraph")
        self.assertEqual(1, first.returncode)
        circuit_path = Path(f"{event}.taskgraph.circuit.json")
        self.assertTrue(circuit_path.exists())
        circuit = json.loads(circuit_path.read_text())
        self.assertIn("diagnostic-start", circuit["stderr"]["head"])
        self.assertIn("diagnostic-end", circuit["stderr"]["tail"])
        self.assertEqual([circuit_path], list(queue.glob("*.circuit.json")))
        self.assertTrue(Path(f"{event}.github.done").exists())
        first_call_count = self.record.read_text().count("CALL\n")
        self.assertEqual(2, first_call_count, "the independent GitHub sink must still run")

        second = self._run("--queue", str(queue))
        self.assertEqual(1, second.returncode)
        self.assertEqual(first_call_count, self.record.read_text().count("CALL\n"))
        self.assertFalse(Path(f"{event}.taskgraph.done").exists())

        shown = subprocess.run(
            [str(CIRCUIT), "show", "--event", str(event), "--sink", "taskgraph"],
            text=True, capture_output=True, check=False,
        )
        self.assertEqual(0, shown.returncode, shown.stderr)
        record_sha = json.loads(shown.stdout)["circuit_record_sha256"]
        stale_clear = subprocess.run(
            [
                str(CIRCUIT), "clear", "--event", str(event), "--sink", "taskgraph",
                "--expected-circuit-sha256", "0" * 64,
            ],
            text=True, capture_output=True, check=False,
        )
        self.assertEqual(2, stale_clear.returncode)
        self.assertTrue(circuit_path.exists())
        cleared = subprocess.run(
            [
                str(CIRCUIT), "clear", "--event", str(event), "--sink", "taskgraph",
                "--expected-circuit-sha256", record_sha,
            ],
            text=True, capture_output=True, check=False,
        )
        self.assertEqual(0, cleared.returncode, cleared.stderr)
        self.assertFalse(circuit_path.exists())
        self.assertFalse(Path(f"{event}.taskgraph.done").exists())
        self.assertEqual(first_call_count, self.record.read_text().count("CALL\n"))


if __name__ == "__main__":
    unittest.main()
