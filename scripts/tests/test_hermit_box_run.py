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
import threading
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

    def assert_fast_exit_series_is_measured(
        self, *, command: list[str], exit_code: int, attempts: int = 10
    ) -> None:
        measured = 0
        for attempt in range(attempts):
            with self.subTest(exit_code=exit_code, attempt=attempt):
                completed, actual, diag = self.run_actual_probe(
                    name=f"fast-{exit_code}-{attempt}",
                    command=command,
                )
                diagnostic = diag.read_text()
                measured += 1
                self.assertEqual(
                    exit_code, completed.returncode, completed.stderr + diagnostic
                )
                self.assertRegex(diagnostic, r"(?m)^\[hermit-box-run\] cgroup=/")
                boxed = json.loads(actual.read_text())
                self.assertEqual("hermit-box-run-actual/v1", boxed["schema"])
                self.assertEqual(
                    "systemd CPUUsageNSec after scope exit", boxed["cpu_source"]
                )
                self.assertIs(type(boxed["cgroup_usage_usec"]), int)
                self.assertGreaterEqual(boxed["cgroup_usage_usec"], 0)
                self.assertIs(type(boxed["cpu_usage_nsec"]), int)
                self.assertGreaterEqual(boxed["cpu_usage_nsec"], 0)
                self.assertEqual(
                    (boxed["cpu_usage_nsec"] + 999) // 1000,
                    boxed["cgroup_usage_usec"],
                )
        self.assertEqual(attempts, measured)

    def test_repeated_fast_success_never_synthesizes_cgroup_cpu(self) -> None:
        self.assert_fast_exit_series_is_measured(
            command=["/bin/true"], exit_code=0
        )

    def test_repeated_fast_failure_never_synthesizes_cgroup_cpu(self) -> None:
        self.assert_fast_exit_series_is_measured(
            command=["/bin/false"], exit_code=1
        )

    def test_repeated_fast_exit_seven_preserves_nondomain_status(self) -> None:
        self.assert_fast_exit_series_is_measured(
            command=["/bin/sh", "-c", "exit 7"], exit_code=7
        )

    def test_completed_cpu_reference_is_released(self) -> None:
        completed, actual, diag = self.run_actual_probe(
            name="completed-reference", command=["/bin/true"]
        )
        self.assertEqual(0, completed.returncode, completed.stderr + diag.read_text())
        measured = json.loads(actual.read_text())
        self.assertEqual("systemd CPUUsageNSec after scope exit", measured["cpu_source"])
        self.assertGreater(measured["cpu_usage_nsec"], 0)
        terminal = subprocess.run(
            ["systemctl", "--user", "show", measured["scope"], "-p", "LoadState"],
            capture_output=True, text=True, timeout=5, check=False,
        )
        self.assertEqual(0, terminal.returncode, terminal.stderr)
        self.assertEqual("LoadState=not-found\n", terminal.stdout)

    def completed_counter_environment(self, replacement: str) -> tuple[dict[str, str], Path]:
        injected = self.base / ("final-counter-" + replacement)
        injected.mkdir()
        readback = injected / "actual-final-query.json"
        adapter = injected / "sitecustomize.py"
        # The private adapter reads the actual sd-bus property first. It then
        # changes only that reply, or blocks at that exact boundary, to exercise
        # refusal and cancellation through the real wrapper and unit reference.
        adapter.write_text(
            "from pathlib import Path\n"
            + f"test_readback = Path({str(readback)!r})\n"
            + f"mode = {replacement!r}\n"
            + textwrap.dedent(
                """\
                import ctypes
                import json
                import os
                import time
                original_cdll = ctypes.CDLL

                class Function:
                    def __init__(self, original, invoke):
                        object.__setattr__(self, "original", original)
                        object.__setattr__(self, "invoke", invoke)
                    def __call__(self, *args):
                        return self.invoke(*args)
                    def __getattr__(self, name):
                        return getattr(self.original, name)
                    def __setattr__(self, name, value):
                        setattr(self.original, name, value)

                def load_library(name, *args, **kwargs):
                    library = original_cdll(name, *args, **kwargs)
                    if name != "libsystemd.so.0":
                        return library
                    get_property = library.sd_bus_get_property
                    read_message = library.sd_bus_message_read
                    cpu_replies = {}
                    def get(*args):
                        status = get_property(*args)
                        if status >= 0 and args[4] == b"CPUUsageNSec":
                            assert args[3] == b"org.freedesktop.systemd1.Scope"
                            reply = ctypes.cast(args[6], ctypes.POINTER(ctypes.c_void_p)).contents.value
                            cpu_replies[reply] = {
                                "path": args[2].decode(),
                                "interface": args[3].decode(),
                                "property": args[4].decode(),
                                "property_status": status,
                            }
                        return status
                    def read(*args):
                        status = read_message(*args)
                        reply = args[0].value
                        if status <= 0 or args[1] != b"t" or reply not in cpu_replies:
                            return status
                        slot = ctypes.cast(args[2], ctypes.POINTER(ctypes.c_uint64))
                        original_nsec = slot.contents.value
                        owner_argv = Path("/proc/self/cmdline").read_bytes().split(b"\\0")
                        assert owner_argv[1] == b"-", owner_argv
                        unit = owner_argv[2].decode()
                        hold = Path(os.fsdecode(owner_argv[4]))
                        minimum_usage = int((hold / "cpu.finish").read_text())
                        assert minimum_usage > 0, minimum_usage
                        altered_nsec = original_nsec
                        if mode == "one-nanosecond-below":
                            altered_nsec = minimum_usage * 1000 - 1
                        elif mode == "exact-boundary":
                            altered_nsec = minimum_usage * 1000
                        elif mode not in ("missing", "blocked"):
                            altered_nsec = int(mode)
                        children_path = Path("/proc/self/task", str(os.getpid()), "children")
                        try:
                            children_interface = {"available": True, "text": children_path.read_text()}
                        except FileNotFoundError as error:
                            children_interface = {"available": False, "error": repr(error)}
                        client_stat = Path("/proc/self/stat").read_text()
                        record = cpu_replies[reply] | {
                            "client_pid": os.getpid(), "unit": unit,
                            "mode": mode, "read_status": status,
                            "original_cpu_usage_nsec": original_nsec,
                            "minimum_usage_usec": minimum_usage,
                            "altered_cpu_usage_nsec": altered_nsec,
                            "children_interface": children_interface,
                            "client_stat": client_stat,
                            "client_status": Path("/proc/self/status").read_text(),
                        }
                        temporary = test_readback.with_suffix(".tmp")
                        temporary.write_text(json.dumps(record))
                        temporary.replace(test_readback)
                        if mode == "blocked":
                            while True:
                                time.sleep(1)
                        deadline = time.monotonic() + 5
                        while not test_readback.with_suffix(".observed").exists():
                            assert time.monotonic() < deadline, "parent child observation did not finish"
                            time.sleep(0.01)
                        if mode == "missing":
                            return -2
                        slot.contents.value = altered_nsec
                        return status
                    library.sd_bus_get_property = Function(get_property, get)
                    library.sd_bus_message_read = Function(read_message, read)
                    return library
                ctypes.CDLL = load_library
                """
            ),
            encoding="utf-8",
        )
        return os.environ | {"PYTHONPATH": str(injected)}, readback

    def assert_reference_has_no_children(self, query: dict[str, object], readback: Path) -> None:
        pid = query["client_pid"]
        def identity() -> dict[str, object]:
            proc = Path("/proc") / str(pid)
            raw = (proc / "stat").read_text()
            return {"stat": raw, "pid": int(raw.split()[0]), "parent_pid": int(raw[raw.rindex(")") + 2:].split()[1]),
                    "start_ticks": int(raw[raw.rindex(")") + 2:].split()[19]),
                    "status": (proc / "status").read_text()}
        before = identity()
        original = query["client_stat"]
        self.assertEqual(int(original[original.rindex(")") + 2:].split()[19]), before["start_ticks"])
        # This process runs in the test parent, outside the reference client.
        # The kernel's optional task/children file is not required for ps's
        # supported PID/PPID observation. Retain its raw output and identities.
        observed = subprocess.run(
            ["/usr/bin/ps", "-eo", "pid=,ppid="],
            capture_output=True, text=True, timeout=2, check=False,
        )
        after = identity()
        relationships = [tuple(map(int, row.split())) for row in observed.stdout.splitlines()]
        self.assertTrue(all(len(row) == 2 for row in relationships))
        self.assertEqual(pid, before["pid"])
        self.assertEqual(pid, after["pid"])
        self.assertIn((pid, before["parent_pid"]), relationships)
        children = [child for child, parent in relationships if parent == pid]
        retained = {"before": before, "after": after, "argv": observed.args,
                    "status": observed.returncode, "stdout": observed.stdout,
                    "stderr": observed.stderr, "children": children}
        readback.with_suffix(".children.json").write_text(json.dumps(retained))
        self.assertEqual(0, observed.returncode, observed.stderr)
        self.assertEqual(before["start_ticks"], after["start_ticks"])
        self.assertEqual([], children, retained)

    def run_completed_counter_probe(
        self, replacement: str
    ) -> tuple[subprocess.CompletedProcess[str], Path, Path, dict[str, object]]:
        environment, readback = self.completed_counter_environment(replacement)
        observation_errors = []
        def observe() -> None:
            try:
                deadline = time.monotonic() + 7
                while not readback.exists():
                    if time.monotonic() >= deadline:
                        raise AssertionError("reference client did not publish its query")
                    time.sleep(0.01)
                self.assert_reference_has_no_children(json.loads(readback.read_text()), readback)
            except BaseException as error:
                observation_errors.append(repr(error))
            finally:
                readback.with_suffix(".observed").touch()
        observer = threading.Thread(target=observe)
        observer.start()
        try:
            completed, actual, diag = self.run_actual_probe(
                name="completed-" + replacement,
                command=["/bin/true"],
                environment=environment,
            )
        finally:
            observer.join(timeout=2)
        self.assertFalse(observer.is_alive())
        self.assertEqual([], observation_errors)
        self.assertTrue(readback.exists(), completed.stderr + diag.read_text())
        original = json.loads(readback.read_text())
        self.assertGreaterEqual(original["property_status"], 0)
        self.assertGreater(original["read_status"], 0)
        self.assertGreaterEqual(original["original_cpu_usage_nsec"], original["minimum_usage_usec"] * 1000)
        self.assertEqual([], json.loads(readback.with_suffix(".children.json").read_text())["children"])
        return completed, actual, diag, original

    def test_missing_or_decreasing_completed_counter_refuses_actual(self) -> None:
        for replacement in ("missing", "0", str(2**64 - 1), "one-nanosecond-below"):
            with self.subTest(replacement=replacement):
                completed, actual, diag, original = self.run_completed_counter_probe(replacement)
                self.assertEqual(3, completed.returncode, completed.stderr + diag.read_text())
                self.assertFalse(actual.exists())
                self.assertIn(
                    "completed scope CPU is missing or below an earlier observation",
                    diag.read_text(),
                )
                self.assertIn("authoritative cgroup CPU unavailable", diag.read_text())
                if replacement == "one-nanosecond-below":
                    self.assertEqual(
                        original["minimum_usage_usec"] * 1000 - 1,
                        original["altered_cpu_usage_nsec"],
                    )

    def test_completed_cpu_at_observed_boundary(self) -> None:
        completed, actual, diag, original = self.run_completed_counter_probe("exact-boundary")
        self.assertEqual(0, completed.returncode, completed.stderr + diag.read_text())
        measured = json.loads(actual.read_text())
        self.assertEqual("systemd CPUUsageNSec after scope exit", measured["cpu_source"])
        self.assertEqual(original["minimum_usage_usec"] * 1000, measured["cpu_usage_nsec"])
        self.assertEqual(original["minimum_usage_usec"], measured["cgroup_usage_usec"])

    def test_final_cpu_query_cancellation_releases_client_and_unit(self) -> None:
        environment, readback = self.completed_counter_environment("blocked")
        actual = self.base / "cancelled-final.json"
        diag = self.base / "cancelled-final.diag"
        bystander = subprocess.Popen(["sleep", "30"])
        self.addCleanup(self.terminate, bystander)
        process = subprocess.Popen(
            [str(self.box), "--passthrough", "--cpu-budget", "30", "--wall", "10",
             "--actual-json", str(actual), "--diag-log", str(diag), "--", "/bin/true"],
            env=environment, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 8
            while not readback.exists() and time.monotonic() < deadline:
                self.assertIsNone(process.poll(), diag.read_text() if diag.exists() else "")
                time.sleep(0.01)
            self.assertTrue(readback.exists(), "final CPU query did not start")
            query = json.loads(readback.read_text())
            self.assert_reference_has_no_children(query, readback)
            self.assertTrue(process_active(query["client_pid"]))
            started = time.monotonic()
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=2)
            self.assertLess(time.monotonic() - started, 2)
            self.assertEqual(143, process.returncode, stdout + stderr + diag.read_text())
            self.assertFalse(process_active(query["client_pid"]))
            self.assertFalse(actual.exists())
            self.assertIsNone(bystander.poll())
            terminal = subprocess.run(
                ["systemctl", "--user", "show", query["unit"], "-p", "LoadState"],
                capture_output=True, text=True, timeout=5, check=False,
            )
            self.assertEqual(0, terminal.returncode, terminal.stderr)
            self.assertEqual("LoadState=not-found\n", terminal.stdout)
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
                process.communicate(timeout=2)

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
        self.assertEqual("systemd CPUUsageNSec after scope exit", boxed["cpu_source"])
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
        self.assertEqual("systemd CPUUsageNSec after scope exit", boxed["cpu_source"])
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
