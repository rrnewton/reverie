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

    def clean_exact_unit(self, unit: str) -> None:
        if not unit:
            return
        subprocess.run(
            ["/usr/bin/systemctl", "--user", "kill", "--kill-whom=all", "--signal=KILL", unit],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=1,
            check=False,
        )
        subprocess.run(
            ["/usr/bin/systemctl", "--user", "stop", unit],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=2,
            check=False,
        )
        subprocess.run(
            ["/usr/bin/systemctl", "--user", "reset-failed", unit],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=1,
            check=False,
        )
        terminal = subprocess.run(
            ["/usr/bin/systemctl", "--user", "show", unit, "-p", "LoadState"],
            text=True,
            capture_output=True,
            timeout=1,
            check=False,
        )
        self.assertEqual("LoadState=not-found\n", terminal.stdout, terminal.stderr)

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

    def test_inherited_deadline_keeps_exact_status_with_optional_cpu(self) -> None:
        for cpu_arguments in ([], ['--cpu-budget', '2']):
            for status in (0, 7, 126, 127):
                with self.subTest(status=status, cpu_arguments=cpu_arguments):
                    completed = subprocess.run([
                        str(self.box), '--passthrough', '--inherited-deadline-ns',
                        str(time.monotonic_ns()+8_000_000_000), *cpu_arguments, '--', sys.executable,
                        '-c', f"import sys; print('out'); print('err', file=sys.stderr); sys.exit({status})"],
                        capture_output=True, text=True, timeout=10)
                    self.assertEqual(status, completed.returncode, completed.stderr)
                    self.assertEqual('out\n', completed.stdout)
                    self.assertEqual('err\n', completed.stderr)

    def test_inherited_deadline_rejects_independent_resource_options(self) -> None:
        for arguments in (['--wall', '8'], ['--mem', '2Gi'], ['--cores', '1'],
                          ['--actual-json', str(self.base / 'actual.json')],
                          ['--perf-dir', str(self.base / 'perf')], ['--allow-cgroup-failure']):
            with self.subTest(arguments=arguments):
                completed = subprocess.run([
                    str(self.box), '--passthrough', '--inherited-deadline-ns',
                    str(time.monotonic_ns()+8_000_000_000), *arguments, '--', '/bin/true'],
                    capture_output=True, text=True, timeout=10)
                self.assertEqual(2, completed.returncode)
                self.assertIn('preserves caller resources', completed.stderr)

    def test_inherited_cpu_reaps_setsid_descendant_in_caller_envelope(self) -> None:
        parent = Path('/sys/fs/cgroup' + Path('/proc/self/cgroup').read_text().strip().split('::', 1)[1])
        names = ('cpu.max', 'memory.max', 'memory.swap.max', 'pids.max', 'cpuset.cpus.effective')
        before = {name: (parent / name).read_text() for name in names if (parent / name).exists()}
        diag = self.base / 'inherited-cpu.diag'
        deadline = time.monotonic_ns() + 8_000_000_000
        completed = subprocess.run([
            str(self.box), '--passthrough', '--inherited-deadline-ns', str(deadline),
            '--cpu-budget', '1', '--diag-log', str(diag), '--', sys.executable, '-c',
            "import json,os,pathlib,signal; child=os.fork(); "
            "os.setsid() if child==0 else None; "
            "print(json.dumps({'pid':os.getpid(),'cgroup':pathlib.Path('/proc/self/cgroup').read_text(),"
            "'deadline':os.environ['HERMIT_BOX_LIFECYCLE_DEADLINE_NS']}),flush=True) if child==0 else None; "
            "signal.pause() if child else exec('while True: pass')"],
            capture_output=True, text=True, timeout=10)
        self.assertEqual(137, completed.returncode, completed.stderr + (diag.read_text() if diag.exists() else ''))
        record = json.loads(completed.stdout)
        group = Path('/sys/fs/cgroup' + record['cgroup'].strip().split('::', 1)[1])
        self.assertEqual(parent, group.parent)
        self.assertEqual(str(deadline), record['deadline'])
        self.assertFalse(group.exists())
        self.assertFalse(process_active(record['pid']))
        self.assertEqual(before, {name: (parent / name).read_text() for name in before})
        self.assertIn('CPU-TIMEOUT:', diag.read_text())
        self.assertIn('termination=cpu_budget exit=137', diag.read_text())
        self.assertNotIn('WALL-TIMEOUT:', diag.read_text())

    def test_inherited_cpu_keeps_absolute_wall_deadline_for_sleep(self) -> None:
        deadline = time.monotonic_ns() + 5_000_000_000
        diag = self.base / 'inherited-sleep.diag'
        completed = subprocess.run([
            str(self.box), '--passthrough', '--inherited-deadline-ns', str(deadline),
            '--cpu-budget=1', '--diag-log', str(diag), '--', sys.executable, '-c',
            "import json,os,pathlib,time; print(json.dumps({'cgroup':pathlib.Path('/proc/self/cgroup').read_text(),"
            "'deadline':os.environ['HERMIT_BOX_LIFECYCLE_DEADLINE_NS']}),flush=True); time.sleep(60)"],
            capture_output=True, text=True, timeout=7)
        self.assertEqual(124, completed.returncode, completed.stderr + (diag.read_text() if diag.exists() else ''))
        record = json.loads(completed.stdout)
        self.assertEqual(str(deadline), record['deadline'])
        self.assertFalse(Path('/sys/fs/cgroup' + record['cgroup'].strip().split('::', 1)[1]).exists())
        self.assertLess(time.monotonic_ns(), deadline)
        self.assertIn('termination=wall_backstop exit=124', diag.read_text())
        self.assertNotIn('CPU-TIMEOUT:', diag.read_text())

    def test_inherited_deadline_charges_setup_before_payload(self) -> None:
        adapters = self.base / 'setup-bin'
        adapters.mkdir()
        stat = adapters / 'stat'
        stat.write_text('#!/bin/sh\nsleep 1\nexec /usr/bin/stat "$@"\n')
        stat.chmod(0o755)
        marker = self.base / 'unexpected-payload'
        deadline = time.monotonic_ns()+2_500_000_000
        completed = subprocess.run([str(self.box), '--passthrough', '--inherited-deadline-ns',
            str(deadline), '--', '/usr/bin/touch', str(marker)],
            env={**os.environ, 'PATH':str(adapters)+os.pathsep+os.environ['PATH']},
            capture_output=True, text=True, timeout=5)
        self.assertEqual(124, completed.returncode, completed.stderr)
        self.assertFalse(marker.exists())
        self.assertIn('deadline expired before setup', completed.stderr)

    def test_inherited_setup_signal_removes_owned_empty_group(self) -> None:
        adapters = self.base / 'signal-bin'
        adapters.mkdir()
        marker = self.base / 'created-group'
        mkdir = adapters / 'mkdir'
        mkdir.write_text('#!/bin/sh\n/bin/mkdir "$@" || exit $?\nprintf "%s\\n" "$2" >"'+str(marker)+'"\nkill -TERM "$PPID"\n')
        mkdir.chmod(0o755)
        completed = subprocess.run([str(self.box), '--passthrough', '--inherited-deadline-ns',
            str(time.monotonic_ns()+8_000_000_000), '--', '/bin/true'],
            env={**os.environ, 'PATH':str(adapters)+os.pathsep+os.environ['PATH']},
            capture_output=True, text=True, timeout=10)
        self.assertEqual(143, completed.returncode, completed.stderr)
        group = Path(marker.read_text().strip())
        self.assertIn('hermit-box-run-', group.name)
        self.assertFalse(group.exists())

    def test_inherited_monitor_sigkill_watchdog_reaps_and_exits(self) -> None:
        diag = self.base / 'watchdog.diag'
        marker = self.base / 'payload.json'
        payload = self.base / 'payload.py'
        payload.write_text("""import json, os, pathlib, signal, sys
os.setsid()
pathlib.Path(sys.argv[1]).write_text(json.dumps({'pid':os.getpid(), 'group':pathlib.Path('/proc/self/cgroup').read_text()}))
signal.pause()
""")
        process = subprocess.Popen([str(self.box), '--passthrough', '--inherited-deadline-ns',
            str(time.monotonic_ns()+8_000_000_000), '--diag-log', str(diag), '--',
            sys.executable, str(payload), str(marker)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(self.terminate, process)
        limit = time.monotonic()+4
        while not marker.exists():
            self.assertIsNone(process.poll())
            self.assertLess(time.monotonic(), limit)
            time.sleep(.01)
        record = json.loads(marker.read_text())
        watchdog = int(re.search(r'lifecycle-watchdog=(\d+)', diag.read_text()).group(1))
        group = Path('/sys/fs/cgroup'+record['group'].strip().split('::',1)[1])
        process.kill()
        process.wait(timeout=2)
        self.assertEqual(-signal.SIGKILL, process.returncode)
        limit = time.monotonic()+2
        while group.exists() or process_active(watchdog) or process_active(record['pid']):
            self.assertLess(time.monotonic(), limit, 'watchdog must promptly reap exact owned subtree and exit')
            time.sleep(.01)
        self.assertFalse(group.exists())
        self.assertFalse(process_active(watchdog))
        self.assertFalse(process_active(record['pid']))

    def test_inherited_deadline_reaps_lock_holding_setsid_descendant(self) -> None:
        lock = self.base / "publication.lock"
        ready = self.base / "holder.json"
        acquired = self.base / "contender.json"
        holder = self.base / "holder.py"
        holder.write_text("""import fcntl, json, os, pathlib, signal, sys
lock=open(sys.argv[1], 'a+b')
fcntl.flock(lock, fcntl.LOCK_EX)
child=os.fork()
if child==0:
 os.setsid()
 pathlib.Path(sys.argv[2]).write_text(json.dumps({'pid':os.getpid(), 'parent':os.getppid(), 'cgroup':pathlib.Path('/proc/self/cgroup').read_text()}))
 signal.pause()
else:
 signal.pause()
""")
        contender = self.base / "contender.py"
        contender.write_text("""import fcntl, json, pathlib, sys
with open(sys.argv[1], 'a+b') as lock:
 fcntl.flock(lock, fcntl.LOCK_EX)
 pathlib.Path(sys.argv[2]).write_text(json.dumps({'cgroup':pathlib.Path('/proc/self/cgroup').read_text()}))
""")
        parent = Path('/sys/fs/cgroup' + Path('/proc/self/cgroup').read_text().strip().split('::',1)[1])
        names = ('cpu.max', 'memory.max', 'memory.swap.max', 'pids.max', 'cpuset.cpus.effective')
        before = {name:(parent/name).read_text() for name in names if (parent/name).exists()}
        def command(deadline, source, output, label):
            return [str(self.box), '--passthrough', '--inherited-deadline-ns', str(deadline),
                    '--diag-log', str(self.base / (label+'.diag')), '--',
                    sys.executable, str(source), str(lock), str(output)]
        deadline = time.monotonic_ns() + 6_000_000_000
        first = subprocess.Popen(command(deadline, holder, ready, 'holder'), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(self.terminate, first)
        while not ready.exists():
            self.assertIsNone(first.poll())
            self.assertLess(time.monotonic_ns(), deadline)
            time.sleep(.01)
        record = json.loads(ready.read_text())
        group = Path('/sys/fs/cgroup' + record['cgroup'].strip().split('::',1)[1])
        self.assertEqual(parent, group.parent)
        second = subprocess.Popen(command(time.monotonic_ns()+10_000_000_000, contender, acquired, 'contender'),
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(self.terminate, second)
        stdout, stderr = first.communicate(timeout=8)
        self.assertEqual(124, first.returncode, stderr.decode()+(self.base/'holder.diag').read_text())
        self.assertLess(time.monotonic_ns(), deadline, 'cleanup must finish inside the original absolute deadline')
        self.assertFalse(process_active(record['pid']))
        self.assertFalse(group.exists())
        second_out, second_err = second.communicate(timeout=10)
        self.assertEqual(0, second.returncode, second_err.decode()+(self.base/'contender.diag').read_text())
        self.assertTrue(acquired.is_file(), 'the exact publication lock must be reusable after descendant cleanup')
        second_group = Path('/sys/fs/cgroup' + json.loads(acquired.read_text())['cgroup'].strip().split('::',1)[1])
        self.assertEqual(parent, second_group.parent)
        self.assertFalse(second_group.exists())
        self.assertEqual(before, {name:(parent/name).read_text() for name in before})
        self.assertIn('cpu-budget=0s', (self.base/'holder.diag').read_text())

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

    def test_repeated_fast_no_receipt_runs_keep_scope_until_measured(self) -> None:
        fake_bin = self.base / "delayed-control-group-bin"
        fake_bin.mkdir()
        marker = self.base / "delayed-control-group.marker"
        systemctl = fake_bin / "systemctl"
        systemctl.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env bash
                set -euo pipefail
                if [[ $* == *"show -p ControlGroup --value hermit-box-run-"* ]] \
                    && [[ ! -e {str(marker)!r} ]]; then
                    : > {str(marker)!r}
                    sleep 0.5
                fi
                exec /usr/bin/systemctl "$@"
                """
            ),
            encoding="utf-8",
        )
        systemctl.chmod(0o755)
        for attempt in range(10):
            with self.subTest(attempt=attempt):
                marker.unlink(missing_ok=True)
                diag = self.base / f"fast-no-receipt-{attempt}.diag"
                completed = subprocess.run(
                    [
                        str(self.box),
                        "--passthrough",
                        "--cpu-budget",
                        "30",
                        "--wall",
                        "10",
                        "--diag-log",
                        str(diag),
                        "--",
                        "/bin/true",
                    ],
                    cwd=self.base,
                    env=os.environ | {"PATH": f"{fake_bin}:{os.environ['PATH']}"},
                    text=True,
                    capture_output=True,
                    timeout=8,
                    check=False,
                )
                diagnostic = diag.read_text()
                self.assertTrue(
                    marker.exists(), "delayed ControlGroup adapter did not run"
                )
                self.assertEqual(0, completed.returncode, completed.stderr + diagnostic)
                self.assertRegex(diagnostic, r"(?m)^\[hermit-box-run\] cgroup=/")
                measurements = re.findall(
                    r"(?m)^\[hermit-box-run\] final usage_usec=([0-9]+) .*$",
                    diagnostic,
                )
                self.assertEqual(1, len(measurements), diagnostic)
                scope_match = re.search(r"scope=(hermit-box-run-[^ ]+\.scope)", diagnostic)
                self.assertIsNotNone(scope_match)
                terminal = subprocess.run(
                    [
                        "/usr/bin/systemctl",
                        "--user",
                        "show",
                        scope_match.group(1),
                        "-p",
                        "LoadState",
                    ],
                    text=True,
                    capture_output=True,
                    timeout=2,
                    check=False,
                )
                self.assertEqual("LoadState=not-found\n", terminal.stdout)

    def test_no_receipt_signal_preserves_exact_streams_and_status(self) -> None:
        diag = self.base / "signal-streams.diag"
        completed = subprocess.run(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "10",
                "--diag-log",
                str(diag),
                "--",
                sys.executable,
                "-c",
                (
                    "import os, signal, sys; "
                    "sys.stdout.write('payload-out'); sys.stdout.flush(); "
                    "sys.stderr.write('payload-err'); sys.stderr.flush(); "
                    "os.kill(os.getpid(), signal.SIGTERM)"
                ),
            ],
            cwd=self.base,
            text=True,
            capture_output=True,
            timeout=8,
            check=False,
        )
        self.assertEqual(143, completed.returncode, diag.read_text())
        self.assertEqual("payload-out", completed.stdout)
        self.assertEqual("payload-err", completed.stderr)

    def test_default_diagnostic_log_is_removed(self) -> None:
        completed = subprocess.run(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "10",
                "--",
                "/bin/true",
            ],
            cwd=self.base,
            env=os.environ | {"TMPDIR": str(self.base)},
            text=True,
            capture_output=True,
            timeout=8,
            check=False,
        )
        self.assertEqual(0, completed.returncode, completed.stderr)
        self.assertEqual([], list(self.base.glob("hermit-box-run.*.diag.log")))

    def test_normal_wall_expiry_has_typed_verdict(self) -> None:
        actual = self.base / "wall-expiry.json"
        diag = self.base / "wall-expiry.diag"
        fake_bin = self.base / "delayed-wall-bin"
        fake_bin.mkdir()
        marker = self.base / "delayed-wall.marker"
        systemctl = fake_bin / "systemctl"
        systemctl.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env bash
                set -euo pipefail
                if [[ $* == *"show -p ControlGroup --value hermit-box-run-"* ]] \
                    && [[ ! -e {str(marker)!r} ]]; then
                    : > {str(marker)!r}
                    sleep 1.8
                fi
                exec /usr/bin/systemctl "$@"
                """
            ),
            encoding="utf-8",
        )
        systemctl.chmod(0o755)
        completed = subprocess.run(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "1",
                "--actual-json",
                str(actual),
                "--diag-log",
                str(diag),
                "--",
                "/bin/sleep",
                "30",
            ],
            cwd=self.base,
            env=os.environ | {"PATH": f"{fake_bin}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
            timeout=8,
            check=False,
        )
        diagnostic = diag.read_text()
        self.assertTrue(marker.exists(), "delayed wall adapter did not run")
        self.assertEqual(137, completed.returncode, completed.stderr + diagnostic)
        self.assertIn("WALL-TIMEOUT:", diagnostic)
        boxed = json.loads(actual.read_text())
        self.assertTrue(boxed["reaped"])
        self.assertEqual("wall_backstop", boxed["termination"])
        self.assertEqual(137, boxed["exit_code"])

    def test_parent_sigkill_leaves_manager_bounded_scope(self) -> None:
        diag = self.base / "parent-sigkill.diag"
        process = subprocess.Popen(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "1",
                "--diag-log",
                str(diag),
                "--",
                "/bin/sh",
                "-c",
                "exec sleep 30",
            ],
            cwd=self.base,
            env=os.environ | {"TMPDIR": str(self.base)},
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        unit = ""
        try:
            deadline = time.monotonic() + 4
            while time.monotonic() < deadline:
                diagnostic = diag.read_text() if diag.exists() else ""
                match = re.search(r"scope=(hermit-box-run-[^ ]+\.scope)", diagnostic)
                if match and "cgroup=" in diagnostic:
                    unit = match.group(1)
                    break
                self.assertIsNone(process.poll(), diagnostic)
                time.sleep(0.02)
            self.assertTrue(unit, "scope did not become active before SIGKILL")
            process.kill()
            process.communicate(timeout=4)
            deadline = time.monotonic() + 4
            load_state = ""
            while time.monotonic() < deadline:
                terminal = subprocess.run(
                    [
                        "/usr/bin/systemctl",
                        "--user",
                        "show",
                        unit,
                        "-p",
                        "LoadState",
                    ],
                    text=True,
                    capture_output=True,
                    timeout=1,
                    check=False,
                )
                load_state = terminal.stdout
                if load_state == "LoadState=not-found\n":
                    break
                time.sleep(0.02)
            self.assertEqual("LoadState=not-found\n", load_state)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=2)
            self.clean_exact_unit(unit)

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

    def test_missing_final_numeric_cpu_usage_refuses_no_receipt_run(self) -> None:
        fake_bin = self.base / "fake-no-receipt-awk"
        fake_bin.mkdir()
        awk = fake_bin / "awk"
        awk.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        awk.chmod(0o755)
        diag = self.base / "missing-no-receipt.diag"
        completed = subprocess.run(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "10",
                "--diag-log",
                str(diag),
                "--",
                "/bin/sh",
                "-c",
                "printf accounted-output; sleep 0.2",
            ],
            cwd=self.base,
            env=os.environ | {"PATH": f"{fake_bin}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
            timeout=8,
            check=False,
        )
        diagnostic = diag.read_text()
        self.assertEqual(3, completed.returncode, completed.stderr + diagnostic)
        self.assertEqual("accounted-output", completed.stdout)
        self.assertIn("final usage_usec=unavailable", diagnostic)
        self.assertIn("refusing boxed run", diagnostic)

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

    def test_term_before_scope_submission_cancels_payload(self) -> None:
        fake_bin = self.base / "pre-submission-bin"
        fake_bin.mkdir()
        marker = self.base / "submission-delayed"
        payload = self.base / "payload-ran"
        diag = self.base / "pre-submission.diag"
        systemd_run = fake_bin / "systemd-run"
        systemd_run.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env bash
                set -euo pipefail
                : > {str(marker)!r}
                sleep 2
                exec /usr/bin/systemd-run "$@"
                """
            ),
            encoding="utf-8",
        )
        systemd_run.chmod(0o755)
        process = subprocess.Popen(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "10",
                "--diag-log",
                str(diag),
                "--",
                "/usr/bin/touch",
                str(payload),
            ],
            cwd=self.base,
            env=os.environ | {"PATH": f"{fake_bin}:{os.environ['PATH']}"},
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        unit = ""
        try:
            deadline = time.monotonic() + 2
            while not marker.exists() and time.monotonic() < deadline:
                self.assertIsNone(process.poll(), diag.read_text() if diag.exists() else "")
                time.sleep(0.01)
            self.assertTrue(marker.exists(), "systemd-run submission was not delayed")
            started = time.monotonic()
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=4)
            self.assertLess(time.monotonic() - started, 3)
            self.assertEqual(143, process.returncode, stdout + stderr + diag.read_text())
            self.assertFalse(payload.exists())
            scope = re.search(r"scope=(hermit-box-run-[^ ]+\.scope)", diag.read_text())
            self.assertIsNotNone(scope)
            unit = scope.group(1)
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=2)
            self.clean_exact_unit(unit)

    def test_hanging_manager_adapter_is_bounded_and_refused(self) -> None:
        fake_bin = self.base / "hanging-manager-bin"
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
        systemctl = fake_bin / "systemctl"
        systemctl.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                if [[ $* == *"show -p Version"* ]]; then
                  exec /usr/bin/systemctl "$@"
                fi
                if [[ $* == *"show -p ControlGroup --value"* ]]; then
                  exit 0
                fi
                sleep 5
                """
            ),
            encoding="utf-8",
        )
        systemctl.chmod(0o755)
        diag = self.base / "hanging-manager.diag"
        started = time.monotonic()
        completed = subprocess.run(
            [
                str(self.box),
                "--passthrough",
                "--cpu-budget",
                "30",
                "--wall",
                "10",
                "--diag-log",
                str(diag),
                "--",
                "/bin/true",
            ],
            cwd=self.base,
            env=os.environ | {"PATH": f"{fake_bin}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
            timeout=8,
            check=False,
        )
        self.assertLess(time.monotonic() - started, 6)
        self.assertEqual(3, completed.returncode, completed.stderr + diag.read_text())
        self.assertIn("could not establish reaper cgroup", completed.stderr)

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

    def test_completed_cpu_burst_is_checked_with_and_without_receipt(self) -> None:
        # Delay only the first scope lookup until the real payload has exited.
        # This deterministically exercises completion before the monitor polls;
        # all CPU values still come from the real cgroup/systemd counters.
        adapters = self.base / 'completed-burst-bin'
        adapters.mkdir()
        systemctl = adapters / 'systemctl'
        systemctl.write_text(textwrap.dedent('''\
            #!/usr/bin/env python3
            import os, pathlib, sys, time
            if 'ControlGroup' in sys.argv:
                deadline = time.monotonic() + 6
                marker = pathlib.Path(os.environ['BURST_COMPLETED'])
                while not marker.exists():
                    if time.monotonic() >= deadline:
                        raise SystemExit('completed burst did not finish')
                    time.sleep(.01)
                time.sleep(.05)
            os.execv('/usr/bin/systemctl', ['/usr/bin/systemctl', *sys.argv[1:]])
        '''))
        systemctl.chmod(0o755)
        payload = self.base / 'completed-burst.py'
        payload.write_text(textwrap.dedent('''\
            import json, pathlib, subprocess, sys
            burn = 'import time; end=time.process_time()+.65\\nwhile time.process_time()<end: pass'
            for _ in range(2):
                subprocess.run([sys.executable, '-c', burn], check=True)
            pathlib.Path(sys.argv[1]).write_text(json.dumps({'completed_children': 2}))
            print('out')
            print('err', file=sys.stderr)
            raise SystemExit(int(sys.argv[2]))
        '''))
        for receipt, native_exit in ((False, 0), (True, 7)):
            with self.subTest(receipt=receipt, native_exit=native_exit):
                marker = self.base / f'completed-{receipt}.json'
                actual = self.base / f'burst-{receipt}.json'
                diag = self.base / f'burst-{receipt}.diag'
                arguments = [str(self.box), '--passthrough', '--cpu-budget', '1', '--wall', '10',
                             '--diag-log', str(diag)]
                if receipt:
                    arguments.extend(['--actual-json', str(actual)])
                arguments.extend(['--', sys.executable, str(payload), str(marker), str(native_exit)])
                completed = subprocess.run(
                    arguments, capture_output=True, text=True, timeout=9,
                    env={**os.environ, 'PATH': str(adapters) + os.pathsep + os.environ['PATH'],
                         'BURST_COMPLETED': str(marker)})
                self.assertEqual({'completed_children': 2}, json.loads(marker.read_text()))
                self.assertEqual('out\n', completed.stdout)
                self.assertEqual('err\n', completed.stderr)
                self.assertEqual(137, completed.returncode, completed.stderr + diag.read_text())
                self.assertIn('CPU-TIMEOUT:', diag.read_text())
                self.assertIn('termination=cpu_budget exit=137', diag.read_text())
                group = re.search(r'(?m)^\[hermit-box-run\] cgroup=(.+)$', diag.read_text())
                self.assertIsNotNone(group)
                self.assertFalse(Path(group[1]).exists())
                if receipt:
                    boxed = json.loads(actual.read_text())
                    self.assertGreater(boxed['cgroup_usage_usec'], 1_000_000)
                    self.assertEqual('cpu_budget', boxed['termination'])
                    self.assertEqual(137, boxed['exit_code'])
                    self.assertFalse(boxed['reaped'], 'completed payload was refused, not killed')

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
