#!/usr/bin/env python3
"""Live cgroup teardown regressions for hermit-box-run passthrough mode."""

from __future__ import annotations

import csv
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
BOX = ROOT / "scripts/hermit-box-run"
TOOL_COST = ROOT / "ci-hub/bin/tool-cost"


def process_active(pid: int) -> bool:
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().split()
    except FileNotFoundError:
        return False
    return len(fields) > 2 and fields[2] != "Z"


class HermitBoxRunTests(unittest.TestCase):
    @staticmethod
    def terminate(process: subprocess.Popen[bytes]) -> None:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=2)

    def setUp(self) -> None:
        if subprocess.run(
            ["systemctl", "--user", "show", "-p", "Version"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        ).returncode:
            self.skipTest("systemd --user is unavailable")
        if (
            subprocess.run(
                ["stat", "-fc", "%T", "/sys/fs/cgroup"],
                text=True,
                capture_output=True,
                check=False,
            ).stdout.strip()
            != "cgroup2fs"
        ):
            self.skipTest("cgroup v2 is unavailable")

        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        fixture_root = self.base / "tool"
        (fixture_root / "scripts").mkdir(parents=True)
        (fixture_root / "agent-utils/common/bin").mkdir(parents=True)
        self.box = fixture_root / "scripts/hermit-box-run"
        shutil.copy2(BOX, self.box)
        runner = fixture_root / "agent-utils/common/bin/dagrun"
        runner.write_text("#!/bin/sh\nexit 99\n", encoding="utf-8")
        runner.chmod(0o755)

    def run_actual_probe(
        self,
        *,
        name: str,
        command: list[str],
        environment: dict[str, str] | None = None,
        perf_dir: Path | None = None,
    ) -> tuple[subprocess.CompletedProcess[str], Path, Path]:
        actual = self.base / f"{name}.json"
        diag = self.base / f"{name}.diag"
        arguments = [
            str(self.box),
            "--passthrough",
            "--cpu-budget",
            "30",
            "--wall",
            "10",
        ]
        if perf_dir is not None:
            arguments.extend(("--perf-dir", str(perf_dir)))
        arguments.extend(
            ("--actual-json", str(actual), "--diag-log", str(diag), "--")
        )
        arguments.extend(command)
        completed = subprocess.run(
            arguments,
            cwd=self.base,
            env=environment,
            text=True,
            capture_output=True,
            timeout=8,
            check=False,
        )
        return completed, actual, diag

    def assert_fast_exit_series_is_measured_or_refused(
        self, *, command: list[str], exit_code: int, attempts: int = 10
    ) -> None:
        measured = 0
        refused = 0
        for attempt in range(attempts):
            with self.subTest(exit_code=exit_code, attempt=attempt):
                completed, actual, diag = self.run_actual_probe(
                    name=f"fast-{exit_code}-{attempt}",
                    command=command,
                )
                diagnostic = diag.read_text()
                if completed.returncode == 3:
                    refused += 1
                    self.assertFalse(actual.exists())
                    self.assertRegex(
                        diagnostic,
                        r"authoritative cgroup CPU unavailable|"
                        r"could not establish reaper cgroup",
                    )
                    continue

                measured += 1
                self.assertEqual(
                    exit_code, completed.returncode, completed.stderr + diagnostic
                )
                self.assertRegex(diagnostic, r"(?m)^\[hermit-box-run\] cgroup=/")
                boxed = json.loads(actual.read_text())
                self.assertEqual("hermit-box-run-actual/v1", boxed["schema"])
                self.assertEqual(
                    "cgroup-v2 cpu.stat usage_usec", boxed["cpu_source"]
                )
                self.assertIs(type(boxed["cgroup_usage_usec"]), int)
                self.assertGreaterEqual(boxed["cgroup_usage_usec"], 0)
        self.assertEqual(attempts, measured + refused)

    def test_repeated_fast_success_never_synthesizes_cgroup_cpu(self) -> None:
        self.assert_fast_exit_series_is_measured_or_refused(
            command=["/bin/true"], exit_code=0
        )

    def test_repeated_fast_failure_never_synthesizes_cgroup_cpu(self) -> None:
        self.assert_fast_exit_series_is_measured_or_refused(
            command=["/bin/false"], exit_code=1
        )

    def test_repeated_fast_exit_seven_preserves_nondomain_status(self) -> None:
        self.assert_fast_exit_series_is_measured_or_refused(
            command=["/bin/sh", "-c", "exit 7"], exit_code=7
        )

    def test_unresolved_cgroup_refuses_actual_without_changing_perf_csv(self) -> None:
        fake_bin = self.base / "fake-systemd-run"
        fake_bin.mkdir()
        systemd_run = fake_bin / "systemd-run"
        systemd_run.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                while [[ $# -gt 0 && $1 != -- ]]; do shift; done
                [[ ${1:-} == -- ]] || exit 64
                shift
                exec "$@"
                """
            ),
            encoding="utf-8",
        )
        systemd_run.chmod(0o755)
        metrics = self.base / "unresolved-metrics"

        for exit_code in (0, 1):
            with self.subTest(exit_code=exit_code):
                completed, actual, diag = self.run_actual_probe(
                    name=f"unresolved-{exit_code}",
                    command=["/bin/true" if exit_code == 0 else "/bin/false"],
                    environment=os.environ
                    | {"PATH": f"{fake_bin}:{os.environ['PATH']}"},
                    perf_dir=metrics,
                )

                self.assertEqual(3, completed.returncode, completed.stderr)
                self.assertFalse(actual.exists())
                self.assertIn("could not establish reaper cgroup", completed.stderr)
                self.assertIn(
                    "authoritative cgroup CPU unavailable", diag.read_text()
                )

        metrics_path = metrics / "hermit-box-run-passthrough.csv"
        with metrics_path.open(newline="", encoding="utf-8") as handle:
            rows = list(csv.reader(handle))
        self.assertEqual(
            ["label", "scope", "cpu_budget_s", "usage_usec", "reaped", "exit"],
            rows[0],
        )
        self.assertTrue(all(len(row) == len(rows[0]) for row in rows[1:]))

    def test_missing_final_numeric_cpu_usage_refuses_actual(self) -> None:
        fake_bin = self.base / "fake-awk"
        fake_bin.mkdir()
        awk = fake_bin / "awk"
        awk.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        awk.chmod(0o755)
        completed, actual, diag = self.run_actual_probe(
            name="missing-final-usage",
            command=["/bin/sh", "-c", "sleep 0.2; exit 0"],
            environment=os.environ | {"PATH": f"{fake_bin}:{os.environ['PATH']}"},
        )

        diagnostic = diag.read_text()
        self.assertEqual(3, completed.returncode, completed.stderr + diagnostic)
        self.assertRegex(diagnostic, r"(?m)^\[hermit-box-run\] cgroup=/")
        self.assertIn("final usage_usec=unavailable", diagnostic)
        self.assertIn("authoritative cgroup CPU unavailable", diagnostic)
        self.assertFalse(actual.exists())

    def test_orc_direct_term_during_lookup_reaps_double_fork_scope(self) -> None:
        marker = self.base / "lookup-started"
        escapee = self.base / "escapee.pid"
        diag = self.base / "box.diag"
        actual = self.base / "cost.json"
        fake_bin = self.base / "bin"
        fake_bin.mkdir()
        systemctl = fake_bin / "systemctl"
        systemctl.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env bash
                set -euo pipefail
                if [[ $* == *"show -p ControlGroup --value hermit-box-run-"* ]] \
                    && [[ ! -e {str(marker)!r} ]]; then
                    : > {str(marker)!r}
                    sleep 1
                fi
                exec /usr/bin/systemctl "$@"
                """
            ),
            encoding="utf-8",
        )
        systemctl.chmod(0o755)
        payload = self.base / "double_fork.py"
        payload.write_text(
            textwrap.dedent(
                f"""\
                import os
                from pathlib import Path
                import signal
                import time

                child = os.fork()
                if child == 0:
                    os.setsid()
                    grandchild = os.fork()
                    if grandchild:
                        os._exit(0)
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                    Path({str(escapee)!r}).write_text(str(os.getpid()))
                    while True:
                        time.sleep(1)
                while True:
                    time.sleep(1)
                """
            ),
            encoding="utf-8",
        )
        bystander = subprocess.Popen(["sleep", "30"])
        self.addCleanup(self.terminate, bystander)
        process = subprocess.Popen(
            [
                str(TOOL_COST),
                "--tool",
                "test/wrkslots-audit-composition",
                "--estimate-wall-seconds",
                "2",
                "--estimate-cpu-seconds",
                "1",
                "--basis",
                "derived from the bounded teardown fixture",
                "--actual-json",
                str(actual),
                "--",
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "30",
                "--diag-log",
                str(diag),
                "--",
                sys.executable,
                str(payload),
            ],
            cwd=self.base,
            env=os.environ | {"PATH": f"{fake_bin}:{os.environ['PATH']}"},
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        unit = ""
        try:
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                diagnostic = diag.read_text() if diag.exists() else ""
                match = re.search(r"scope=(hermit-box-run-[^ ]+\.scope)", diagnostic)
                if match:
                    unit = match.group(1)
                if marker.exists() and escapee.exists() and unit:
                    break
                if process.poll() is not None:
                    self.fail(
                        f"box exited before race fixture was ready: {process.returncode} "
                        f"{diagnostic}"
                    )
                time.sleep(0.02)
            self.assertTrue(marker.exists(), "cgroup lookup delay did not fire")
            self.assertTrue(escapee.exists(), "double-fork escapee did not start")
            self.assertTrue(unit, "scope name was not diagnosed")
            escapee_pid = int(escapee.read_text())
            self.assertTrue(process_active(escapee_pid))

            started = time.monotonic()
            process.send_signal(signal.SIGTERM)
            try:
                _stdout, stderr = process.communicate(timeout=2)
                required_kill = False
            except subprocess.TimeoutExpired:
                required_kill = True
                process.kill()
                _stdout, stderr = process.communicate(timeout=2)
            elapsed = time.monotonic() - started

            self.assertFalse(
                required_kill, "wrapper did not finish before ORC's 2s KILL"
            )
            self.assertEqual(143, process.returncode, stderr)
            self.assertLess(elapsed, 2)
            cost = json.loads(actual.read_text())
            self.assertEqual("143", cost["actual"]["exit"])
            self.assertIn("COST ACTUAL", stderr)
            deadline = time.monotonic() + 2
            while process_active(escapee_pid) and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertFalse(
                process_active(escapee_pid), "escapee survived cgroup cleanup"
            )
            self.assertIsNone(bystander.poll(), "cleanup killed an unrelated process")
            active = subprocess.run(
                ["systemctl", "--user", "is-active", "--quiet", unit],
                check=False,
            )
            self.assertNotEqual(0, active.returncode, "transient scope remained active")
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=2)
            if unit:
                subprocess.run(
                    [
                        "systemctl",
                        "--user",
                        "kill",
                        "--kill-whom=all",
                        "--signal=KILL",
                        unit,
                    ],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                )
                subprocess.run(
                    ["systemctl", "--user", "--no-block", "stop", unit],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                )

    def test_normal_main_exit_still_reaps_double_fork_escapee(self) -> None:
        escapee = self.base / "normal-escapee.pid"
        diag = self.base / "normal.diag"
        metrics = self.base / "normal-metrics"
        actual = self.base / "box-actual.json"
        actual.write_text('{"stale": true}\n')
        payload = self.base / "normal_double_fork.py"
        payload.write_text(
            textwrap.dedent(
                f"""\
                import os
                from pathlib import Path
                import signal
                import time

                if os.fork() == 0:
                    os.setsid()
                    if os.fork():
                        os._exit(0)
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                    Path({str(escapee)!r}).write_text(str(os.getpid()))
                    while True:
                        time.sleep(1)
                deadline = time.monotonic() + 5
                while not Path({str(escapee)!r}).exists():
                    if time.monotonic() >= deadline:
                        raise SystemExit(70)
                    time.sleep(0.01)
                """
            ),
            encoding="utf-8",
        )
        bystander = subprocess.Popen(["sleep", "30"])
        self.addCleanup(self.terminate, bystander)

        completed = subprocess.run(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "10",
                "--perf-dir",
                str(metrics),
                "--actual-json",
                str(actual),
                "--diag-log",
                str(diag),
                "--",
                sys.executable,
                str(payload),
            ],
            cwd=self.base,
            text=True,
            capture_output=True,
            timeout=8,
            check=False,
        )

        self.assertEqual(0, completed.returncode, completed.stderr + diag.read_text())
        self.assertTrue(escapee.exists())
        escapee_pid = int(escapee.read_text())
        deadline = time.monotonic() + 2
        while process_active(escapee_pid) and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertFalse(process_active(escapee_pid))
        self.assertIsNone(bystander.poll())
        match = re.search(r"scope=(hermit-box-run-[^ ]+\.scope)", diag.read_text())
        self.assertIsNotNone(match)
        active = subprocess.run(
            ["systemctl", "--user", "is-active", "--quiet", match.group(1)],
            check=False,
        )
        self.assertNotEqual(0, active.returncode)
        metrics_path = metrics / "hermit-box-run-passthrough.csv"
        self.assertEqual(
            "label,scope,cpu_budget_s,usage_usec,reaped,exit",
            metrics_path.read_text().splitlines()[0],
        )
        with metrics_path.open(
            newline="", encoding="utf-8"
        ) as handle:
            rows = list(csv.DictReader(handle))
        self.assertEqual(1, len(rows))
        self.assertEqual("30", rows[0]["cpu_budget_s"])
        self.assertGreaterEqual(int(rows[0]["usage_usec"]), 0)
        self.assertEqual("0", rows[0]["reaped"])
        self.assertEqual("0", rows[0]["exit"])
        boxed = json.loads(actual.read_text())
        self.assertEqual("hermit-box-run-actual/v1", boxed["schema"])
        self.assertEqual("cgroup-v2 cpu.stat usage_usec", boxed["cpu_source"])
        self.assertEqual(30, boxed["cpu_budget_seconds"])
        self.assertEqual(10, boxed["wall_backstop_seconds"])
        self.assertEqual(
            boxed["cgroup_usage_usec"] / 1_000_000, boxed["cpu_seconds"]
        )
        self.assertFalse(boxed["reaped"])
        self.assertEqual("completed", boxed["termination"])
        self.assertEqual(0, boxed["exit_code"])
        self.assertEqual([], list(actual.parent.glob(f".{actual.name}.*.tmp")))

    def test_systemd_service_stop_reaps_complete_audit_composition(self) -> None:
        escapee = self.base / "service-escapee.pid"
        delayed = self.base / "service-delayed-state"
        actual = self.base / "service-cost.json"
        diag = self.base / "service.diag"
        payload = self.base / "service_double_fork.py"
        payload.write_text(
            textwrap.dedent(
                f"""\
                import os
                from pathlib import Path
                import signal
                import time

                if os.fork() == 0:
                    os.setsid()
                    if os.fork():
                        os._exit(0)
                    signal.signal(signal.SIGTERM, signal.SIG_IGN)
                    Path({str(escapee)!r}).write_text(str(os.getpid()))
                    time.sleep(1)
                    Path({str(delayed)!r}).write_text("survived")
                    while True:
                        time.sleep(1)
                while True:
                    time.sleep(1)
                """
            ),
            encoding="utf-8",
        )
        bystander = subprocess.Popen(["sleep", "30"])
        self.addCleanup(self.terminate, bystander)
        unit = f"hermit-wrkslots-audit-test-{os.getpid()}-{time.time_ns()}.service"
        launched = subprocess.run(
            [
                "systemd-run",
                "--user",
                "--quiet",
                "--collect",
                "--no-block",
                f"--unit={unit}",
                "--property=Type=exec",
                "--property=KillMode=control-group",
                "--property=TimeoutStopSec=2s",
                "--",
                str(TOOL_COST),
                "--tool",
                "test/systemd-wrkslots-audit",
                "--estimate-wall-seconds",
                "2",
                "--estimate-cpu-seconds",
                "1",
                "--basis",
                "derived from the bounded systemd cancellation fixture",
                "--actual-json",
                str(actual),
                "--",
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "30",
                "--diag-log",
                str(diag),
                "--",
                sys.executable,
                str(payload),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, launched.returncode, launched.stdout + launched.stderr)
        try:
            deadline = time.monotonic() + 8
            while not escapee.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(escapee.exists(), "systemd audit fixture did not start")
            escapee_pid = int(escapee.read_text())

            stopped = subprocess.run(
                ["systemctl", "--user", "stop", unit],
                text=True,
                capture_output=True,
                timeout=4,
                check=False,
            )

            self.assertEqual(0, stopped.returncode, stopped.stdout + stopped.stderr)
            deadline = time.monotonic() + 2
            while process_active(escapee_pid) and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertFalse(process_active(escapee_pid))
            self.assertFalse(delayed.exists(), "stopped service wrote delayed state")
            self.assertTrue(actual.is_file(), "outer tool-cost did not persist actual")
            self.assertIn(
                json.loads(actual.read_text())["actual"]["exit"], ("143", "signal:15")
            )
            self.assertIsNone(bystander.poll())
            self.assertNotEqual(
                0,
                subprocess.run(
                    ["systemctl", "--user", "is-active", "--quiet", unit],
                    check=False,
                ).returncode,
            )
        finally:
            subprocess.run(
                ["systemctl", "--user", "--no-block", "stop", unit],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )

    def test_cpu_limit_publishes_typed_actual_and_removes_scope(self) -> None:
        actual = self.base / "cpu-limit-actual.json"
        diag = self.base / "cpu-limit.diag"
        completed = subprocess.run(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "1",
                "--wall",
                "10",
                "--actual-json",
                str(actual),
                "--diag-log",
                str(diag),
                "--",
                sys.executable,
                "-c",
                "while True: pass",
            ],
            cwd=self.base,
            text=True,
            capture_output=True,
            timeout=8,
            check=False,
        )

        self.assertEqual(137, completed.returncode, completed.stderr + diag.read_text())
        boxed = json.loads(actual.read_text())
        self.assertEqual("hermit-box-run-actual/v1", boxed["schema"])
        self.assertEqual("cgroup-v2 cpu.stat usage_usec", boxed["cpu_source"])
        self.assertEqual(1, boxed["cpu_budget_seconds"])
        self.assertEqual(10, boxed["wall_backstop_seconds"])
        self.assertGreater(boxed["cgroup_usage_usec"], 1_000_000)
        self.assertEqual(
            boxed["cgroup_usage_usec"] / 1_000_000, boxed["cpu_seconds"]
        )
        self.assertTrue(boxed["reaped"])
        self.assertEqual("cpu_budget", boxed["termination"])
        self.assertEqual(137, boxed["exit_code"])
        self.assertNotEqual(
            0,
            subprocess.run(
                ["systemctl", "--user", "is-active", "--quiet", boxed["scope"]],
                check=False,
            ).returncode,
        )


if __name__ == "__main__":
    unittest.main()
