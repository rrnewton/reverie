#!/usr/bin/env python3
"""Contract tests for the CPU-bounded operational tick entry point."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "hermit-operational-tick"
PLUGIN = ROOT / ".orc" / "plugins" / "hermit-dev" / "index.ts"
SERVICE = ROOT / "scripts" / "systemd" / "hermit-health-tick.service"


class OperationalTickBoundTest(unittest.TestCase):
    def test_bound_derivation_is_exposed(self) -> None:
        result = subprocess.run(
            [str(SCRIPT), "--print-bounds"],
            cwd=ROOT,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
        )
        self.assertEqual(
            "cpu_budget_seconds=1200 wall_backstop_seconds=619\n",
            result.stdout,
        )

    def test_full_run_records_cpu_wall_and_instruction_population(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            box = temp / "box"
            tick = temp / "tick"
            box.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                "actual=\n"
                "cpu=\n"
                "wall=\n"
                "while [[ $# -gt 0 ]]; do\n"
                "  case $1 in\n"
                "    --actual-json) actual=$2; shift 2 ;;\n"
                "    --cpu-budget) cpu=$2; shift 2 ;;\n"
                "    --wall) wall=$2; shift 2 ;;\n"
                "    --passthrough) shift ;;\n"
                "    --) shift; break ;;\n"
                "  esac\n"
                "done\n"
                "[[ $cpu == 1200 && $wall == 619 ]]\n"
                '"$@"\n'
                "rc=$?\n"
                "printf '{\"cpu_seconds\":797.755708,\"termination\":\"completed\"}\\n' >\"$actual\"\n"
                "exit $rc\n"
            )
            tick.write_text(
                "#!/usr/bin/env bash\n"
                "printf 'NOTE: emitted 14 instruction(s) this tick observed_at=test\\n'\n"
            )
            box.chmod(0o755)
            tick.chmod(0o755)
            environment = os.environ | {
                "HERMIT_HEALTH_TICK_BOX_RUN": str(box),
                "HERMIT_HEALTH_TICK_COMMAND": str(tick),
                "HERMIT_HEALTH_TICK_STATE_DIR": str(temp),
            }

            result = subprocess.run(
                [str(SCRIPT), "--emit-output"],
                cwd=ROOT,
                env=environment,
                check=True,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

            self.assertIn("emitted 14 instruction(s)", result.stdout)
            history = (temp / "hermit-health-tick-invocations.log").read_text()
            self.assertIn("instructions=14", history)
            self.assertIn("cpu_seconds=797.755708", history)
            self.assertIn("termination=completed", history)
            self.assertIn("exit=0", history)

    def test_missing_measurement_is_not_zero(self) -> None:
        with tempfile.TemporaryDirectory() as raw_temp:
            temp = Path(raw_temp)
            box = temp / "box"
            tick = temp / "tick"
            box.write_text(
                "#!/usr/bin/env bash\n"
                "while [[ $# -gt 0 ]]; do [[ $1 == -- ]] && { shift; break; }; shift; done\n"
                '"$@"\n'
            )
            tick.write_text("#!/usr/bin/env bash\nprintf 'tick skipped\\n'\n")
            box.chmod(0o755)
            tick.chmod(0o755)
            environment = os.environ | {
                "HERMIT_HEALTH_TICK_BOX_RUN": str(box),
                "HERMIT_HEALTH_TICK_COMMAND": str(tick),
                "HERMIT_HEALTH_TICK_STATE_DIR": str(temp),
            }

            subprocess.run([str(SCRIPT)], cwd=ROOT, env=environment, check=True)

            history = (temp / "hermit-health-tick-invocations.log").read_text()
            self.assertIn("instructions=not_available", history)
            self.assertIn("cpu_seconds=not_available", history)
            self.assertNotIn("cpu_seconds=0", history)

    def test_both_operational_drivers_use_the_bounded_entry_point(self) -> None:
        plugin = PLUGIN.read_text()
        service = SERVICE.read_text()
        self.assertIn("./scripts/hermit-operational-tick --emit-output", plugin)
        self.assertIn("OPERATIONAL_TICK_SCRIPT_TIMEOUT_SECONDS = 650", plugin)
        self.assertIn("timeoutSec: OPERATIONAL_TICK_SCRIPT_TIMEOUT_SECONDS", plugin)
        self.assertIn("--tool-relative scripts/hermit-operational-tick", service)
        self.assertIn("TimeoutStartSec=650", service)


if __name__ == "__main__":
    unittest.main()
