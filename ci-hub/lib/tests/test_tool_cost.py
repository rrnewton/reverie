from __future__ import annotations

import json
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
TOOL = ROOT / "ci-hub/bin/tool-cost"


def process_active(pid: int) -> bool:
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().split()
    except FileNotFoundError:
        return False
    return len(fields) > 2 and fields[2] != "Z"


class ToolCostTest(unittest.TestCase):
    def run_tool(
        self,
        command: list[str],
        *,
        actual_json: Path | None = None,
        estimate_unknown: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        options = ["--actual-json", str(actual_json)] if actual_json else []
        estimate = (
            ["--estimate-unknown", "--basis", "not measured: test has no history"]
            if estimate_unknown
            else [
                "--estimate-wall-seconds",
                "12",
                "--estimate-cpu-seconds",
                "3",
                "--basis",
                "derived from 3 fixture items x 4 measured seconds/item",
            ]
        )
        return subprocess.run(
            [
                str(TOOL),
                "--tool",
                "test/tool",
                *estimate,
                *options,
                "--",
                *command,
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_success_reports_estimate_and_actual(self) -> None:
        result = self.run_tool([sys.executable, "-c", "print('payload')"])
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "payload\n")
        self.assertIn(
            "# test/tool tool COST ESTIMATE wall=12.000s cpu=3.000s", result.stderr
        )
        self.assertIn(
            "basis='derived from 3 fixture items x 4 measured seconds/item'",
            result.stderr,
        )
        self.assertRegex(
            result.stderr,
            r"# test/tool tool COST ACTUAL wall=[0-9.]+s cpu=[0-9.]+s .* exit=0",
        )

    def test_failure_preserves_exit_and_reports_actual(self) -> None:
        result = self.run_tool([sys.executable, "-c", "raise SystemExit(7)"])
        self.assertEqual(result.returncode, 7)
        self.assertIn("# test/tool tool COST ESTIMATE", result.stderr)
        self.assertIn("# test/tool tool COST ACTUAL", result.stderr)
        self.assertIn("exit=7", result.stderr)

    def test_launch_failure_still_reports_actual(self) -> None:
        result = self.run_tool(["/definitely/not/a/command"])
        self.assertEqual(result.returncode, 127)
        self.assertIn("cannot launch", result.stderr)
        self.assertIn("# test/tool tool COST ACTUAL", result.stderr)
        self.assertIn("exit=127", result.stderr)

    def test_actual_json_is_atomic_structured_cost_record(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            actual_path = Path(directory) / "nested" / "cost.json"
            result = self.run_tool(
                [sys.executable, "-c", "raise SystemExit(3)"],
                actual_json=actual_path,
            )
            self.assertEqual(result.returncode, 3)
            payload = json.loads(actual_path.read_text())
            self.assertEqual(payload["schema_version"], 1)
            self.assertEqual(payload["tool"], "test/tool")
            self.assertEqual(payload["estimate"]["kind"], "derived")
            self.assertEqual(payload["estimate"]["wall_seconds"], 12.0)
            self.assertEqual(payload["estimate"]["cpu_seconds"], 3.0)
            self.assertEqual(payload["actual"]["exit"], "3")
            self.assertGreaterEqual(payload["actual"]["wall_seconds"], 0)
            self.assertGreaterEqual(payload["actual"]["cpu_seconds"], 0)

    def test_term_is_forwarded_and_still_publishes_actual(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            leader_ready = temporary / "leader.pid"
            descendant_ready = temporary / "descendant.pid"
            delayed = temporary / "descendant-survived"
            actual = temporary / "cost.json"
            process = subprocess.Popen(
                [
                    str(TOOL),
                    "--tool",
                    "test/tool",
                    "--estimate-wall-seconds",
                    "12",
                    "--estimate-cpu-seconds",
                    "3",
                    "--basis",
                    "derived from the TERM regression fixture",
                    "--actual-json",
                    str(actual),
                    "--",
                    sys.executable,
                    "-c",
                    textwrap.dedent(
                        f"""\
                        import os
                        from pathlib import Path
                        import time

                        if os.fork() == 0:
                            Path({str(descendant_ready)!r}).write_text(str(os.getpid()))
                            for descriptor in (0, 1, 2):
                                try:
                                    os.close(descriptor)
                                except OSError:
                                    pass
                            time.sleep(0.5)
                            Path({str(delayed)!r}).write_text("survived")
                            os._exit(0)
                        Path({str(leader_ready)!r}).write_text(str(os.getpid()))
                        time.sleep(60)
                        """
                    ),
                ],
                cwd=ROOT,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            deadline = time.monotonic() + 5
            while (
                not leader_ready.exists() or not descendant_ready.exists()
            ) and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(leader_ready.exists(), "tool-cost group leader did not start")
            self.assertTrue(
                descendant_ready.exists(), "same-group descendant did not start"
            )
            leader_pid = int(leader_ready.read_text())
            descendant_pid = int(descendant_ready.read_text())

            process.send_signal(signal.SIGTERM)
            _stdout, stderr = process.communicate(timeout=5)

            self.assertEqual(143, process.returncode, stderr)
            self.assertIn("COST ACTUAL", stderr)
            self.assertIn("exit=signal:15", stderr)
            self.assertEqual("signal:15", json.loads(actual.read_text())["actual"]["exit"])
            deadline = time.monotonic() + 1
            while (
                process_active(leader_pid) or process_active(descendant_pid)
            ) and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertFalse(process_active(leader_pid))
            self.assertFalse(process_active(descendant_pid))
            time.sleep(0.55)
            self.assertFalse(delayed.exists(), "same-group descendant survived TERM")

    def test_unknown_estimate_prints_nothing_and_is_persisted_as_null(self) -> None:
        """An unknown estimate is RECORDED and not PRINTED.

        ⚠️ THIS ASSERTION WAS INVERTED DELIBERATELY, ON THE OWNER'S INSTRUCTION, and it
        is the one place to look if the line ever comes back. It previously required
        `# ... COST ESTIMATE wall=unknown cpu=unknown basis='not measured: ...'` on
        stderr -- a line that printed on every tick and carried no information, which is
        the defect class this repository spent the night removing from its own gates.

        ⚠️ WHAT IS KEPT IS THE FACT, AND THE JSON ASSERTIONS BELOW ARE UNCHANGED. The
        receipt still records `kind = "unknown"` with null wall and cpu, so "we had no
        estimate" stays machine-readable. Deleting those would be losing the record
        rather than the noise, and is the mistake this cell now guards against.
        """
        with tempfile.TemporaryDirectory() as directory:
            actual_path = Path(directory) / "unknown-cost.json"
            result = self.run_tool(
                [sys.executable, "-c", "pass"],
                actual_json=actual_path,
                estimate_unknown=True,
            )
            self.assertEqual(result.returncode, 0)
            self.assertNotIn("COST ESTIMATE", result.stderr)
            self.assertNotIn("not measured", result.stderr)
            # ⚠️ AND THE CONTROL: suppressing the estimate must not suppress the line
            # that carries real numbers. `test_numeric_estimate_*` above covers the
            # other half -- a NUMERIC estimate still prints.
            self.assertIn("# test/tool tool COST ACTUAL ", result.stderr)
            payload = json.loads(actual_path.read_text())
            self.assertEqual(payload["estimate"]["kind"], "unknown")
            self.assertIsNone(payload["estimate"]["wall_seconds"])
            self.assertIsNone(payload["estimate"]["cpu_seconds"])
            self.assertGreaterEqual(payload["actual"]["wall_seconds"], 0)

    def test_missing_numeric_pair_requires_explicit_unknown(self) -> None:
        result = subprocess.run(
            [
                str(TOOL),
                "--tool",
                "test/tool",
                "--basis",
                "not measured: omitted on purpose",
                "--",
                sys.executable,
                "-c",
                "pass",
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("never invent a fallback", result.stderr)


if __name__ == "__main__":
    unittest.main()
