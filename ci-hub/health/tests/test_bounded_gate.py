#!/usr/bin/env python3
"""Brackets for typed tick-gate timeouts and the two production consumers."""

from __future__ import annotations

import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import unittest
from dataclasses import replace

ROOT = Path(__file__).resolve().parents[3]
ADAPTER = ROOT / "ci-hub" / "health" / "bounded_gate.py"
CONFIG = ROOT / "ci-hub" / "health" / "tick-hub.yaml"
sys.path.insert(0, str(ROOT / "agent-utils" / "py"))

from tick_hub.engine import TickResult, run_tick  # noqa: E402
from tick_hub.io import config_from_yaml  # noqa: E402
from tick_hub.model import Emit, EmitKind, Gate, GateWhen, Reminder, TickConfig  # noqa: E402
from tick_hub.probes import GlobFileAgeProbe, SubprocessGateRunner  # noqa: E402
from tick_hub.state import OpsState  # noqa: E402


def run_adapter(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(ADAPTER), *args],
        cwd=ROOT,
        text=True,
        capture_output=True,
        timeout=5,
        check=False,
    )


class BoundedGateBracket(unittest.TestCase):
    def test_timeout_is_distinct_no_result_with_name_and_bound(self) -> None:
        started = time.monotonic()
        result = run_adapter(
            "--name", "planted_timeout",
            "--timeout-secs", "0.1",
            "--", sys.executable, "-c", "import time; time.sleep(3)",
        )
        elapsed = time.monotonic() - started

        self.assertEqual(result.returncode, 1, result)
        self.assertLess(elapsed, 2.0)
        fields = dict(
            line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
        )
        self.assertEqual(fields["state"], "NO_RESULT")
        self.assertEqual(fields["outcome"], "timeout")
        self.assertEqual(fields["gate"], "planted_timeout")
        self.assertEqual(fields["bound_secs"], "0.1")
        self.assertEqual(
            fields["summary"],
            "NO_RESULT/timeout gate=planted_timeout bound_secs=0.1",
        )

    def test_healthy_gate_preserves_output_and_success(self) -> None:
        result = run_adapter(
            "--name", "healthy",
            "--timeout-secs", "2",
            "--", sys.executable, "-c", "print('state=ok'); print('summary=healthy')",
        )
        self.assertEqual(result.returncode, 0, result)
        self.assertEqual(result.stdout, "state=ok\nsummary=healthy\n")

    def test_ordinary_finding_preserves_output_and_failure(self) -> None:
        result = run_adapter(
            "--name", "finding",
            "--timeout-secs", "2",
            "--", sys.executable, "-c",
            "print('state=finding'); print('summary=needs-attention'); raise SystemExit(1)",
        )
        self.assertEqual(result.returncode, 1, result)
        self.assertEqual(result.stdout, "state=finding\nsummary=needs-attention\n")

    @unittest.skipUnless(os.name == "posix", "process-group cleanup is POSIX-specific")
    def test_timeout_reaps_descendants(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bounded-gate-") as raw:
            pid_file = Path(raw) / "child.pid"
            script = f"sleep 30 & echo $! > {pid_file}; sleep 30"
            result = run_adapter(
                "--name", "tree",
                "--timeout-secs", "0.1",
                "--", "bash", "-c", script,
            )
            self.assertEqual(result.returncode, 1, result)
            child_pid = int(pid_file.read_text(encoding="utf-8"))
            stat_path = Path(f"/proc/{child_pid}/stat")
            deadline = time.monotonic() + 1
            while stat_path.exists() and time.monotonic() < deadline:
                try:
                    state = stat_path.read_text(encoding="utf-8").split(") ", 1)[1][0]
                except OSError:
                    break
                if state == "Z":
                    break
                time.sleep(0.01)
            if stat_path.exists():
                state = stat_path.read_text(encoding="utf-8").split(") ", 1)[1][0]
                self.assertEqual(state, "Z", "timed-out gate left a live descendant")


class ProductionWiring(unittest.TestCase):
    def test_activation_obligations_are_registered_without_age_out(self) -> None:
        config = config_from_yaml(CONFIG.read_text(encoding="utf-8"))
        reminder = {item.name: item for item in config.reminders}[
            "activation_obligations"
        ]
        self.assertIsNotNone(reminder.gate)
        assert reminder.gate is not None
        self.assertEqual(
            reminder.gate.cmd,
            "python3 scripts/ci-hub-clone-change.py activation-obligations "
            "--primary /home/newton/work/dev-hermit",
        )
        self.assertTrue(reminder.gate.capture)
        self.assertNotIn("--max-age", reminder.gate.cmd)
        self.assertNotIn("--older-than", reminder.gate.cmd)
        self.assertEqual(
            reminder.emit.fields["summary"],
            "NO-SIGNAL gate=activation_obligations reason=missing-captured-summary",
        )

    def test_both_slow_gates_have_inner_and_outer_bounds(self) -> None:
        config = config_from_yaml(CONFIG.read_text(encoding="utf-8"))
        reminders = {item.name: item for item in config.reminders}
        expected = {
            "speculative_land_obligations",
            "agent_container_lifecycle",
        }
        checked = 0
        for name in expected:
            gate = reminders[name].gate
            self.assertIsNotNone(gate)
            assert gate is not None
            self.assertEqual(gate.timeout_secs, 55)
            self.assertIn(
                f"bounded_gate.py --name {name} --timeout-secs 45 --",
                gate.cmd,
            )
            checked += 1
        self.assertEqual(checked, 2)

    def test_speculative_land_has_a_no_signal_summary_fallback(self) -> None:
        config = config_from_yaml(CONFIG.read_text(encoding="utf-8"))
        reminder = {item.name: item for item in config.reminders}[
            "speculative_land_obligations"
        ]
        self.assertEqual(
            reminder.emit.fields["summary"],
            "NO-SIGNAL gate=speculative_land_obligations "
            "reason=missing-captured-summary",
        )

    def test_speculative_land_gate_does_not_take_rust_script_build_lock(self) -> None:
        config = config_from_yaml(CONFIG.read_text(encoding="utf-8"))
        gate = {item.name: item for item in config.reminders}[
            "speculative_land_obligations"
        ].gate
        self.assertIsNotNone(gate)
        assert gate is not None
        self.assertNotIn("./ci-hub/ci-hub", gate.cmd)
        self.assertIn("./ci-hub/bin/tool-cost", gate.cmd)
        self.assertIn("--estimate-unknown", gate.cmd)
        self.assertIn(
            "python3 ci-hub/remediation/protocol.py watch --once --gate",
            gate.cmd,
        )

    def _run_speculative_with(self, stdout: str) -> TickResult:
        config = config_from_yaml(CONFIG.read_text(encoding="utf-8"))
        reminder = {item.name: item for item in config.reminders}[
            "speculative_land_obligations"
        ]
        command = shlex.join(
            [
                sys.executable,
                "-c",
                f"print({stdout!r}, end=''); raise SystemExit(1)",
            ]
        )
        planted = replace(
            reminder,
            gate=Gate(
                cmd=command,
                when=GateWhen.FAILURE,
                capture=True,
                timeout_secs=2,
            ),
        )
        return run_tick(
            TickConfig(reminders=(planted,)),
            OpsState.default(),
            now=9,
            fired={},
            gate_runner=SubprocessGateRunner(),
            age_probe=GlobFileAgeProbe(),
        )

    def test_resolvable_speculative_template_emits_one_real_verdict(self) -> None:
        result = self._run_speculative_with("summary=obligation-123-is-red\n")
        actions = [line for line in result.lines if line.startswith("ACTION: ")]
        self.assertEqual(len(actions), 1, result.lines)
        self.assertEqual(result.actions_emitted, 1)
        self.assertIn("obligation-123-is-red", actions[0])
        self.assertNotIn("NO-SIGNAL", actions[0])

    def test_missing_summary_emits_one_visible_no_signal_verdict(self) -> None:
        result = self._run_speculative_with("")
        actions = [line for line in result.lines if line.startswith("ACTION: ")]
        self.assertEqual(len(actions), 1, result.lines)
        self.assertEqual(result.actions_emitted, 1)
        self.assertIn("NO-SIGNAL", actions[0])
        self.assertIn("gate=speculative_land_obligations", actions[0])
        self.assertIn("reason=missing-captured-summary", actions[0])
        self.assertFalse(any("{summary}" in line for line in result.lines))


class TickConsumerBracket(unittest.TestCase):
    def run_one(self, child: list[str]):
        command = shlex.join(
            [
                sys.executable,
                str(ADAPTER),
                "--name",
                "planted_timeout",
                "--timeout-secs",
                "0.1",
                "--",
                *child,
            ]
        )
        reminder = Reminder(
            "bounded",
            Emit(
                EmitKind.ACTION,
                title="bounded gate did not return a verdict: {summary}",
                skill="hard-warn",
                fields={"component": "bounded-gate"},
            ),
            gate=Gate(
                cmd=command,
                when=GateWhen.FAILURE,
                capture=True,
                timeout_secs=2,
            ),
        )
        return run_tick(
            TickConfig(reminders=(reminder,)),
            OpsState.default(),
            now=9,
            fired={},
            gate_runner=SubprocessGateRunner(),
            age_probe=GlobFileAgeProbe(),
        )

    def test_timeout_becomes_one_counted_nonpass_action(self) -> None:
        result = self.run_one(
            [sys.executable, "-c", "import time; time.sleep(3)"]
        )
        actions = [line for line in result.lines if line.startswith("ACTION: ")]
        self.assertEqual(len(actions), 1, result.lines)
        self.assertEqual(result.actions_emitted, 1)
        self.assertIn("NO_RESULT/timeout", actions[0])
        self.assertIn("gate=planted_timeout", actions[0])
        self.assertIn("bound_secs=0.1", actions[0])
        self.assertEqual(result.fired["bounded"], 9)

    def test_healthy_gate_consumes_cadence_without_warning(self) -> None:
        result = self.run_one(
            [sys.executable, "-c", "print('state=ok'); print('summary=healthy')"]
        )
        actions = [line for line in result.lines if line.startswith("ACTION: ")]
        self.assertEqual(len(actions), 0, result.lines)
        self.assertEqual(result.actions_emitted, 0)
        self.assertEqual(result.fired["bounded"], 9)


if __name__ == "__main__":
    unittest.main()
