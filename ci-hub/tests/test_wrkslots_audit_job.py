#!/usr/bin/env python3
"""Contract tests for the independently scheduled wrkslots audit producer."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
PRODUCER = ROOT / "ci-hub/bin/wrkslots-audit"
CONSUMER = ROOT / "ci-hub/health/wrkslots_audit_result.py"
SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_C = "c" * 40


class WrkslotsAuditJobTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        base = Path(self.temporary.name)
        self.tool_root = base / "exact-tool"
        self.state_root = base / "mutable-state"
        (self.tool_root / "ci-hub/bin").mkdir(parents=True)
        (self.tool_root / "ci-hub/health").mkdir()
        (self.tool_root / "scripts").mkdir()
        self.state_root.mkdir()
        shutil.copy2(PRODUCER, self.tool_root / "ci-hub/bin/wrkslots-audit")
        shutil.copy2(
            ROOT / "ci-hub/health/wrkslots_audit_contract.py",
            self.tool_root / "ci-hub/health/wrkslots_audit_contract.py",
        )
        for relative in ("scripts/hermit-box-run", "ci-hub/bin/wrkslots"):
            path = self.tool_root / relative
            path.write_text("#!/bin/sh\nexit 99\n", encoding="utf-8")
            path.chmod(0o755)
        tool_cost = self.tool_root / "ci-hub/bin/tool-cost"
        tool_cost.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env python3
                import json
                import os
                from pathlib import Path
                import sys

                args = sys.argv[1:]
                Path(os.environ["ARGV_RECORD"]).write_text(json.dumps(args))
                if os.environ.get("SELF_SIGNAL"):
                    os.kill(os.getpid(), int(os.environ["SELF_SIGNAL"]))
                separator = args.index("--")
                box_args = args[separator + 1:]
                if os.environ.get("OMIT_BOX_ACTUAL") != "1":
                    box_actual = Path(
                        box_args[box_args.index("--actual-json") + 1]
                    )
                    cpu_budget = int(os.environ.get(
                        "BOX_CPU_BUDGET",
                        box_args[box_args.index("--cpu-budget") + 1],
                    ))
                    wall = int(os.environ.get(
                        "BOX_WALL_BACKSTOP",
                        box_args[box_args.index("--wall") + 1],
                    ))
                    termination = os.environ.get("BOX_TERMINATION", "completed")
                    reaped = os.environ.get(
                        "BOX_REAPED", "1" if termination != "completed" else "0"
                    ) == "1"
                    usage_raw = os.environ.get("BOX_USAGE_USEC", "85200000")
                    usage = int(usage_raw) if usage_raw.isdigit() else usage_raw
                    cpu_seconds = os.environ.get("BOX_CPU_SECONDS")
                    boxed_exit = int(os.environ.get(
                        "BOX_EXIT", os.environ.get("AUDIT_RC", "0")
                    ))
                    payload = {
                        "schema": "hermit-box-run-actual/v1",
                        "label": "wrkslots-audit",
                        "scope": "hermit-box-run-123-456.scope",
                        "cpu_source": "cgroup-v2 cpu.stat usage_usec",
                        "cgroup_usage_usec": usage,
                        "cpu_seconds": (
                            float(cpu_seconds)
                            if cpu_seconds is not None
                            else (
                                usage / 1_000_000
                                if isinstance(usage, int)
                                else 85.2
                            )
                        ),
                        "cpu_budget_seconds": cpu_budget,
                        "wall_backstop_seconds": wall,
                        "reaped": reaped,
                        "termination": termination,
                        "exit_code": boxed_exit,
                    }
                    box_actual.write_text(
                        os.environ.get("BOX_ACTUAL", json.dumps(payload))
                    )
                actual = Path(args[args.index("--actual-json") + 1])
                if os.environ.get("OMIT_COST") != "1":
                    cpu = 3.6
                    actual.write_text(json.dumps({
                        "schema_version": 1,
                        "tool": "ci-hub/wrkslots-audit",
                        "estimate": {
                            "kind": "derived",
                            "wall_seconds": 190.0,
                            "cpu_seconds": 179.0,
                            "basis": "derived from one completed contention audit",
                        },
                        "actual": {
                            "wall_seconds": 115.7,
                            "cpu_seconds": cpu,
                            "cpu_user_seconds": 1.2,
                            "cpu_system_seconds": 2.4,
                            "exit": os.environ.get("AUDIT_RC", "0"),
                        },
                    }))
                state = os.environ.get("AUDIT_STATE", "ok")
                actionable = state == "actionable"
                unknown = state == "unknown"
                print(json.dumps({
                    "schema": 2,
                    "state": state,
                    "summary": os.environ.get("AUDIT_SUMMARY", "healthy"),
                    "worktree_count": 32,
                    "running_agent_count": 20,
                    "attention_count": int(actionable),
                    "attention_slots": ["slot01"] if actionable else [],
                    "unknown_count": int(unknown),
                    "unknown_slots": ["slot02"] if unknown else [],
                }))
                if os.environ.get("AUDIT_DIAGNOSTIC"):
                    print(os.environ["AUDIT_DIAGNOSTIC"], file=sys.stderr)
                raise SystemExit(int(os.environ.get("AUDIT_RC", "0")))
                """
            ),
            encoding="utf-8",
        )
        tool_cost.chmod(0o755)
        self.record = base / "argv.json"
        self.result = self.state_root / "result.json"
        self.environment = os.environ | {
            "DEV_HERMIT_TOOL_ROOT": str(self.tool_root),
            "DEV_HERMIT_PARENT": str(self.state_root),
            "DEV_HERMIT_TOOL_PARENT_SHA": SHA_A,
            "DEV_HERMIT_TOOL_HERMIT_SHA": SHA_B,
            "DEV_HERMIT_TOOL_AGENT_UTILS_SHA": SHA_C,
            "ARGV_RECORD": str(self.record),
        }

    def run_job(self, **environment: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                str(self.tool_root / "ci-hub/bin/wrkslots-audit"),
                "--result",
                str(self.result),
            ],
            cwd=self.tool_root,
            env=self.environment | environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_exact_cost_wraps_exact_box_which_wraps_exact_audit(self) -> None:
        completed = self.run_job(AUDIT_STATE="actionable", AUDIT_RC="1")

        self.assertEqual(0, completed.returncode, completed.stderr)
        arguments = json.loads(self.record.read_text())
        command = arguments[arguments.index("--") + 1 :]
        self.assertEqual(str(self.tool_root / "scripts/hermit-box-run"), command[0])
        self.assertIn("--passthrough", command)
        self.assertIn("--cpu-budget", command)
        self.assertEqual("240", command[command.index("--cpu-budget") + 1])
        self.assertIn("--wall", command)
        self.assertEqual("1800", command[command.index("--wall") + 1])
        self.assertIn("--actual-json", command)
        inner = command[command.index("--") + 1 :]
        self.assertEqual(str(self.tool_root / "ci-hub/bin/wrkslots"), inner[0])
        self.assertEqual(
            [
                "--project-root",
                str(self.state_root),
                "audit",
                "--format",
                "json",
                "--gate",
            ],
            inner[1:],
        )

        receipt = json.loads(self.result.read_text())
        self.assertEqual("wrkslots-audit-result/v1", receipt["schema"])
        self.assertEqual("ACTIONABLE", receipt["verdict"])
        self.assertEqual(SHA_A, receipt["producer"]["parent_sha"])
        self.assertEqual(SHA_B, receipt["producer"]["hermit_sha"])
        self.assertEqual(SHA_C, receipt["producer"]["agent_utils_sha"])
        self.assertEqual(85.2, receipt["cost"]["cpu_seconds"])
        self.assertEqual("cgroup-v2 cpu.stat usage_usec", receipt["cost"]["cpu_source"])
        self.assertEqual(3.6, receipt["observer_cost"]["actual"]["cpu_seconds"])

    def test_box_timeout_still_publishes_outer_cost_as_no_result(self) -> None:
        completed = self.run_job(
            AUDIT_RC="137",
            AUDIT_DIAGNOSTIC="CPU-TIMEOUT: fixture exceeded its CPU budget",
            BOX_USAGE_USEC="240040862",
            BOX_REAPED="1",
            BOX_TERMINATION="cpu_budget",
        )

        self.assertEqual(1, completed.returncode)
        receipt = json.loads(self.result.read_text())
        self.assertEqual("NO_RESULT", receipt["verdict"])
        self.assertIn("CPU budget", receipt["summary"])
        self.assertEqual(240.040862, receipt["cost"]["cpu_seconds"])
        self.assertEqual("cpu_budget", receipt["cost"]["termination"])
        self.assertEqual(3.6, receipt["observer_cost"]["actual"]["cpu_seconds"])

    def test_missing_cost_cannot_publish_a_success(self) -> None:
        previous = self.run_job()
        self.assertEqual(0, previous.returncode, previous.stderr)
        self.assertEqual("OK", json.loads(self.result.read_text())["verdict"])

        completed = self.run_job(OMIT_COST="1")

        self.assertEqual(1, completed.returncode)
        receipt = json.loads(self.result.read_text())
        self.assertEqual("NO_RESULT", receipt["verdict"])
        self.assertIsNone(receipt["cost"])
        self.assertIsNone(receipt["observer_cost"])

    def test_bad_box_cost_replaces_prior_success_with_no_result(self) -> None:
        cases = (
            ({"OMIT_BOX_ACTUAL": "1"}, "absent or unreadable"),
            ({"BOX_ACTUAL": "not-json"}, "absent or unreadable"),
            ({"BOX_USAGE_USEC": "not-a-number"}, "CPU usage"),
            ({"BOX_CPU_SECONDS": "1.0"}, "CPU seconds contradict"),
            ({"BOX_CPU_BUDGET": "241"}, "CPU budget contradicts"),
            ({"BOX_WALL_BACKSTOP": "1799"}, "wall backstop contradicts"),
            ({"BOX_EXIT": "1"}, "exit contradicts"),
        )
        for environment, fragment in cases:
            with self.subTest(environment=environment):
                previous = self.run_job()
                self.assertEqual(0, previous.returncode, previous.stderr)
                self.assertEqual("OK", json.loads(self.result.read_text())["verdict"])

                completed = self.run_job(**environment)

                self.assertEqual(1, completed.returncode)
                receipt = json.loads(self.result.read_text())
                self.assertEqual("NO_RESULT", receipt["verdict"])
                self.assertIn(fragment, receipt["summary"])
                self.assertIsNone(receipt["cost"])
                self.assertIsNotNone(receipt["observer_cost"])

    def test_inner_timeout_like_stderr_cannot_forge_box_termination(self) -> None:
        completed = self.run_job(
            AUDIT_DIAGNOSTIC="CPU-TIMEOUT: untrusted inner audit text"
        )

        self.assertEqual(0, completed.returncode, completed.stderr)
        receipt = json.loads(self.result.read_text())
        self.assertEqual("OK", receipt["verdict"])
        self.assertEqual("completed", receipt["cost"]["termination"])

    def test_signalled_tool_cost_replaces_prior_success_with_no_result(self) -> None:
        previous = self.run_job()
        self.assertEqual(0, previous.returncode, previous.stderr)
        self.assertEqual("OK", json.loads(self.result.read_text())["verdict"])

        completed = self.run_job(SELF_SIGNAL="9")

        self.assertEqual(1, completed.returncode)
        receipt = json.loads(self.result.read_text())
        self.assertEqual("NO_RESULT", receipt["verdict"])
        self.assertEqual(137, receipt["exit_code"])
        self.assertIn("signal 9", receipt["summary"])
        self.assertIsNone(receipt["cost"])
        self.assertIsNone(receipt["observer_cost"])

    def test_exit_three_distinguishes_box_unavailable_from_audit_refusal(self) -> None:
        for diagnostic in (
            "hermit-box-run: cgroup boxing unavailable (exit 3)",
            "hermit-box-run: could not establish reaper cgroup (exit 3)",
        ):
            with self.subTest(diagnostic=diagnostic):
                unavailable = self.run_job(
                    AUDIT_RC="3",
                    AUDIT_DIAGNOSTIC=diagnostic,
                    OMIT_BOX_ACTUAL="1",
                )
                self.assertEqual(1, unavailable.returncode)
                receipt = json.loads(self.result.read_text())
                self.assertIn("cgroup containment", receipt["summary"])

        measurement = self.run_job(
            AUDIT_RC="3",
            AUDIT_DIAGNOSTIC="hermit-box-run: authoritative cgroup CPU "
            "unavailable; refusing --actual-json (exit 3)",
            OMIT_BOX_ACTUAL="1",
        )
        self.assertEqual(1, measurement.returncode)
        receipt = json.loads(self.result.read_text())
        self.assertIn("cgroup CPU measurement", receipt["summary"])
        self.assertNotIn("containment", receipt["summary"])

        refused = self.run_job(
            AUDIT_RC="3",
            AUDIT_DIAGNOSTIC="wrkslots: REFUSED: inconsistent registry",
        )
        self.assertEqual(1, refused.returncode)
        receipt = json.loads(self.result.read_text())
        self.assertIn("wrkslots audit refused", receipt["summary"])
        self.assertNotIn("cgroup", receipt["summary"])

    def test_service_success_does_not_hide_typed_actionable_or_unknown(self) -> None:
        for state, audit_rc, consumer_rc in (
            ("actionable", "1", 1),
            ("unknown", "2", 75),
        ):
            with self.subTest(state=state):
                producer = self.run_job(AUDIT_STATE=state, AUDIT_RC=audit_rc)
                self.assertEqual(0, producer.returncode, producer.stderr)
                consumer = subprocess.run(
                    [
                        str(CONSUMER),
                        "--result",
                        str(self.result),
                        "--expected-parent-sha",
                        SHA_A,
                        "--expected-hermit-sha",
                        SHA_B,
                        "--expected-agent-utils-sha",
                        SHA_C,
                    ],
                    cwd=ROOT,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(consumer_rc, consumer.returncode, consumer.stderr)
                expected_state = "actionable" if state == "actionable" else "NO_RESULT"
                self.assertIn(f"state={expected_state}", consumer.stdout)

    def test_tick_hub_retries_unknown_without_consuming_cadence(self) -> None:
        producer = self.run_job(AUDIT_STATE="unknown", AUDIT_RC="2")
        self.assertEqual(0, producer.returncode, producer.stderr)
        command = shlex.join(
            [
                str(CONSUMER),
                "--result",
                str(self.result),
                "--expected-parent-sha",
                SHA_A,
                "--expected-hermit-sha",
                SHA_B,
                "--expected-agent-utils-sha",
                SHA_C,
            ]
        )
        config = self.state_root / "tick-hub.json"
        config.write_text(
            json.dumps(
                {
                    "reminders": [
                        {
                            "name": "wrkslots_audit",
                            "cadence_secs": 3600,
                            "gate": {
                                "cmd": command,
                                "when": "failure",
                                "capture": True,
                            },
                            "emit": {
                                "kind": "action",
                                "skill": "hard-warn-coordinator",
                                "title": "audit: {summary}",
                            },
                        }
                    ]
                }
            ),
            encoding="utf-8",
        )
        fired = self.state_root / "fired-state"
        tick = subprocess.run(
            [
                sys.executable,
                "-m",
                "tick_hub",
                "tick",
                "--no-header",
                "--report-pending",
                "--flush",
                "--config",
                str(config),
                "--fired-state",
                str(fired),
            ],
            cwd=ROOT,
            env=self.environment
            | {"PYTHONPATH": str(ROOT / "hermit/agent-utils/py")},
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, tick.returncode, tick.stdout + tick.stderr)
        self.assertIn("NO_RESULT: wrkslots_audit", tick.stdout)
        self.assertIn("outcome=NO-SIGNAL gate=wrkslots_audit", tick.stdout)
        self.assertNotIn('title="audit:', tick.stdout)
        self.assertFalse(
            fired.exists() and "wrkslots_audit=" in fired.read_text(),
            "NO_RESULT consumed the reminder cadence",
        )

    def test_mutable_or_wrong_provenance_execution_is_refused(self) -> None:
        mutable = self.run_job(DEV_HERMIT_TOOL_ROOT=str(self.state_root))
        self.assertEqual(3, mutable.returncode)
        self.assertIn("entrypoint root", mutable.stderr)
        self.assertFalse(self.result.exists())

        missing = self.run_job(DEV_HERMIT_TOOL_PARENT_SHA="not-a-sha")
        self.assertEqual(3, missing.returncode)
        self.assertIn("DEV_HERMIT_TOOL_PARENT_SHA", missing.stderr)
        self.assertFalse(self.result.exists())


if __name__ == "__main__":
    unittest.main()
