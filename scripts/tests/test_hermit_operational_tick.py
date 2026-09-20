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
    def test_help_from_scripts_directory_needs_no_git_or_home(self) -> None:
        for flag in ("-h", "--help"):
            with self.subTest(flag=flag):
                result = subprocess.run(
                    ["/bin/bash", SCRIPT.name, flag], cwd=SCRIPT.parent,
                    env={"PATH": "/nonexistent"}, capture_output=True,
                    text=True, timeout=5, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("--print-bounds", result.stdout)
                self.assertEqual(result.stderr, "")

    def test_help_and_refusal_precede_home_git_and_state_creation(self) -> None:
        for arguments, expected in ((["--help"], 0), (["-h"], 0),
                                    (["--run-current-tool", "--help"], 0),
                                    (["--run-current-tool", "-h"], 0),
                                    (["--print-bounds"], 0),
                                    (["--unknown"], 2), (["unexpected"], 2),
                                    (["--emit-output", "unexpected"], 2),
                                    (["--print-bounds", "unexpected"], 2)):
            for with_home, empty_home in ((False, False), (True, False), (True, True)):
                with self.subTest(arguments=arguments, with_home=with_home, empty_home=empty_home):
                    with tempfile.TemporaryDirectory() as directory:
                        root = Path(directory)
                        marker = root / "effects"
                        fake_bin = root / "bin"
                        fake_bin.mkdir()
                        for name in ("git", "dirname", "mkdir", "mktemp", "date", "tee"):
                            program = fake_bin / name
                            program.write_text(
                                '#!/bin/bash\nprintf "called:%s\\n" "$0" >>"$ENTRY_EFFECTS"\nexit 93\n'
                            )
                            program.chmod(0o755)
                        environment = {"PATH": str(fake_bin), "ENTRY_EFFECTS": str(marker)}
                        if with_home:
                            environment["HOME"] = "" if empty_home else str(root / "absent-home")
                        result = subprocess.run(
                            ["/bin/bash", str(SCRIPT), *arguments], env=environment,
                            capture_output=True, text=True, timeout=5, check=False,
                        )
                        self.assertEqual(result.returncode, expected, result.stderr)
                        if expected == 0:
                            if arguments == ["--print-bounds"]:
                                self.assertEqual(result.stdout,
                                    "cpu_budget_seconds=1200 wall_backstop_seconds=619\n")
                            else:
                                self.assertIn("--emit-output", result.stdout)
                                if len(arguments) == 1:
                                    self.assertIn("--print-bounds", result.stdout)
                            self.assertEqual(result.stderr, "")
                        else:
                            self.assertIn("--help", result.stderr)
                            self.assertEqual(result.stdout, "")
                        self.assertFalse(marker.exists())
                        self.assertFalse((root / "absent-home").exists())

    def test_delegated_arguments_and_execution_status_survive_bounded_entry(self) -> None:
        arguments = ["--run-current-tool", "--tool-relative", "payload", "--",
                     "--help", "two words", "", "$(not-a-command)", "--unknown"]
        for status in (0, 126, 127):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                box = root / "box"
                tick = root / "tick"
                argv_file = root / "arguments"
                box.write_text(
                    '#!/bin/bash\n'
                    'while [[ $# -gt 0 ]]; do\n'
                    '  if [[ $1 == -- ]]; then shift; exec "$@"; fi\n'
                    '  shift\n'
                    'done\nexit 94\n'
                )
                tick.write_text(
                    '#!/bin/bash\nprintf \'%s\\0\' "$@" >"$ENTRY_ARGUMENTS"\n'
                    'exit "$ENTRY_EXIT"\n'
                )
                box.chmod(0o755)
                tick.chmod(0o755)
                result = subprocess.run(
                    [str(SCRIPT), *arguments], cwd=ROOT,
                    env=os.environ | {
                        "HERMIT_HEALTH_TICK_BOX_RUN": str(box),
                        "HERMIT_HEALTH_TICK_COMMAND": str(tick),
                        "HERMIT_HEALTH_TICK_STATE_DIR": str(root),
                        "ENTRY_ARGUMENTS": str(argv_file), "ENTRY_EXIT": str(status),
                    }, capture_output=True, text=True, timeout=10, check=False,
                )
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertEqual(argv_file.read_bytes(),
                                 b"".join(arg.encode() + b"\0" for arg in arguments))
                self.assertIn(f"exit={status}\n",
                              (root / "hermit-health-tick-invocations.log").read_text())

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

    def test_systemd_is_the_only_operational_driver(self) -> None:
        plugin = PLUGIN.read_text()
        service = SERVICE.read_text()
        self.assertNotIn("./scripts/hermit-operational-tick", plugin)
        self.assertNotIn("hermitOperationalTick", plugin)
        self.assertNotIn("operationalHealthHeartbeat", plugin)
        self.assertIn('operationalHealthOwner: "hermit-health-tick.timer"', plugin)
        self.assertIn("--tool-relative scripts/hermit-operational-tick", service)
        self.assertIn("TimeoutStartSec=650", service)


if __name__ == "__main__":
    unittest.main()
