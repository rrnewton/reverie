#!/usr/bin/env python3
"""Focused acceptance tests for safehermit's detached-unit resource report.

Fake systemctl/systemd-run adapters inject property failures and inspect ordering;
they delegate unit creation and cgroup enforcement to the live user manager.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import textwrap


ROOT = Path(__file__).resolve().parent.parent
SAFEHERMIT = ROOT / "bin" / "safehermit"


def _write_executable(path: Path, body: str) -> None:
    path.write_text(textwrap.dedent(body).lstrip(), encoding="utf-8")
    path.chmod(0o755)


def _fake_tools(directory: Path) -> Path:
    tools = directory / "tools"
    tools.mkdir()
    _write_executable(
        tools / "systemd-run",
        r"""
        #!/usr/bin/env python3
        import json
        import os
        from pathlib import Path
        import subprocess
        import sys

        args = sys.argv[1:]
        with Path(os.environ["FAKE_CALL_LOG"]).open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(["systemd-run", *args]) + "\n")
        unit = next((arg.split("=", 1)[1] for arg in args if arg.startswith("--unit=")), "")
        if unit.startswith("safehermit-probe-"):
            raise SystemExit(0)
        report = Path(os.environ["EXPECT_RESOURCE_REPORT"])
        initial = report.read_text(encoding="utf-8")
        if initial.count("safehermit: unit=") != 1 or "memory_peak_bytes" in initial:
            raise SystemExit(90)
        if f"safehermit: unit={unit}\n" not in initial:
            raise SystemExit(91)
        Path(os.environ["INITIAL_REPORT_SEEN"]).write_text(initial, encoding="utf-8")
        applied_args = list(args)
        if os.environ.get("FAKE_APPLIED_MISMATCH") == "memory":
            applied_args = [
                "--property=MemoryMax=134217728"
                if arg.startswith("--property=MemoryMax=")
                else arg
                for arg in applied_args
            ]
        completed = subprocess.run(["/usr/bin/systemd-run", *applied_args], check=False)
        Path(os.environ["FAKE_RUNNER_RC"]).write_text(str(completed.returncode), encoding="ascii")
        raise SystemExit(completed.returncode)
        """,
    )
    _write_executable(
        tools / "systemctl",
        r"""
        #!/usr/bin/env python3
        import json
        import os
        from pathlib import Path
        import subprocess
        import sys

        args = sys.argv[1:]
        with Path(os.environ["FAKE_CALL_LOG"]).open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(["systemctl", *args]) + "\n")
        if "show" not in args:
            if any(action in args for action in ("stop", "reset-failed")):
                report = Path(os.environ["EXPECT_RESOURCE_REPORT"])
                text = report.read_text(encoding="utf-8") if report.exists() else ""
                complete = all(
                    text.count(f"safehermit: {key}=") == 1
                    for key in (
                        "unit",
                        "memory_peak_bytes",
                        "cpu_usage_nsec",
                        "tasks_peak",
                        "limit_memory_bytes",
                        "limit_swap_bytes",
                        "limit_tasks",
                        "limit_cpu_cores_micros",
                        "limit_cpu_time_nsec",
                        "limit_wall_time_usec",
                        "cpu_time_limit_hit",
                        "memory_oom_kills",
                        "tasks_limit_hits",
                        "wall_time_limit_hit",
                        "log_cap_hit",
                    )
                )
                with Path(os.environ["CLEANUP_OBSERVATIONS"]).open("a", encoding="ascii") as stream:
                    stream.write("complete\n" if complete else "incomplete\n")
            completed = subprocess.run(["/usr/bin/systemctl", *args], check=False)
            raise SystemExit(completed.returncode)
        unit = next((arg for arg in args if arg.endswith(".service")), "")
        requested = []
        for index, arg in enumerate(args):
            if arg == "-p" and index + 1 < len(args):
                requested.append(args[index + 1])
        if "--value" in args or set(requested) == {"Id", "LoadState", "ControlGroup"}:
            completed = subprocess.run(["/usr/bin/systemctl", *args], check=False)
            raise SystemExit(completed.returncode)
        values = {
            "Id": unit,
            "LoadState": "loaded",
            "ActiveState": "active",
            "SubState": "running",
            "ControlGroup": "",
            "MemoryPeak": "123456789",
            "CPUUsageNSec": "987654321",
            "TasksPeak": "17",
            "Result": "success",
        }
        if os.environ.get("FAKE_METRICS_MODE") == "missing-cpu":
            values.pop("CPUUsageNSec")
        elif os.environ.get("FAKE_METRICS_MODE") == "cpu-hit":
            values["CPUUsageNSec"] = "1100000000"
        elif os.environ.get("FAKE_METRICS_MODE") == "wall-timeout":
            values["Result"] = "timeout"
        for key in requested:
            if key in values:
                print(f"{key}={values[key]}")
        """,
    )
    _write_executable(
        tools / "python3",
        r"""
        #!/bin/bash
        set -u
        if [ "${FAKE_SAMPLE_FAILURE:-}" = after-preflight ] && [ "${1:-}" = - ]; then
            script=$(command cat)
            if [[ "$script" == *'cpu.stat'* ]]; then
                if [ -e "$FAKE_SAMPLE_STATE" ]; then
                    exit 88
                fi
                : >"$FAKE_SAMPLE_STATE"
            fi
            printf '%s\n' "$script" | /usr/local/bin/python3 - "${@:2}"
            exit $?
        fi
        exec /usr/local/bin/python3 "$@"
        """,
    )
    return tools


def _run(
    temporary: Path,
    report: Path,
    *,
    command: str,
    metrics_mode: str = "complete",
    applied_mismatch: str = "",
    sh_report: Path | None = None,
    deadline: int = 91,
    cpu_timeout: int = 30,
    max_log_bytes: int | None = None,
    sample_failure: str = "",
) -> subprocess.CompletedProcess[str]:
    tools = _fake_tools(temporary)
    env = os.environ.copy()
    env.update(
        {
            "PATH": f"{tools}:{env['PATH']}",
            "SAFEHERMIT_LOG_ROOT": str(temporary / "logs"),
            "EXPECT_RESOURCE_REPORT": str(report),
            "INITIAL_REPORT_SEEN": str(temporary / "initial.txt"),
            "FAKE_RUNNER_RC": str(temporary / "runner-rc.txt"),
            "FAKE_CALL_LOG": str(temporary / "calls.jsonl"),
            "CLEANUP_OBSERVATIONS": str(temporary / "cleanup-observations.txt"),
            "FAKE_METRICS_MODE": metrics_mode,
            "FAKE_APPLIED_MISMATCH": applied_mismatch,
            "FAKE_SAMPLE_FAILURE": sample_failure,
            "FAKE_SAMPLE_STATE": str(temporary / "sample-state"),
        }
    )
    argv = [
        str(SAFEHERMIT),
        "--sh-resource-report",
        str(report),
        "--sh-deadline",
        str(deadline),
        "--sh-memory-max-bytes",
        str(256 * 1024 * 1024),
        "--sh-tasks-max",
        "64",
        "--sh-cpu-cores",
        "1",
        "--sh-cpu-timeout",
        str(cpu_timeout),
    ]
    if max_log_bytes is not None:
        argv.extend(("--sh-max-log-bytes", str(max_log_bytes)))
    if sh_report is not None:
        argv.extend(("--sh-report", str(sh_report)))
    argv.extend(("/bin/sh", "-c", command))
    return subprocess.run(argv, check=False, text=True, capture_output=True, env=env)


def _fields(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        payload = line.rsplit(": ", 1)[-1]
        key, separator, value = payload.partition("=")
        if separator:
            assert key not in result, f"duplicate {key} in {path}"
            result[key] = value
    return result


def _calls(path: Path) -> list[list[str]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def test_complete_report_is_published_before_cleanup() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        bounds = temporary / "bounds.txt"
        completed = _run(
            temporary,
            report,
            sh_report=bounds,
            command='printf "payload-out\\n"; printf "payload-err\\n" >&2; exit 23',
        )
        assert completed.returncode == 23, completed
        assert completed.stdout == "payload-out\n"
        assert completed.stderr == "payload-err\n"
        fields = _fields(report)
        assert fields["unit"].startswith("safehermit-")
        assert fields["memory_peak_bytes"] == "123456789"
        assert fields["cpu_usage_nsec"] == "987654321"
        assert fields["tasks_peak"] == "17"
        assert fields["limit_memory_bytes"] == str(256 * 1024 * 1024)
        assert fields["limit_swap_bytes"] == "0"
        assert fields["limit_tasks"] == "64"
        assert fields["limit_cpu_cores_micros"] == "1000000"
        assert fields["limit_cpu_time_nsec"] == "30000000000"
        assert fields["limit_wall_time_usec"] == "91000000"
        assert fields["cpu_time_limit_hit"] == "0"
        assert fields["memory_oom_kills"] == "0"
        assert fields["tasks_limit_hits"] == "0"
        assert fields["wall_time_limit_hit"] == "0"
        assert fields["log_cap_hit"] == "0"
        invocation = hashlib.sha256()
        for argument in (
            "/bin/sh",
            "-c",
            'printf "payload-out\\n"; printf "payload-err\\n" >&2; exit 23',
        ):
            invocation.update(os.fsencode(argument))
            invocation.update(b"\0")
        assert fields["invocation_sha256"] == invocation.hexdigest()
        assert stat.S_IMODE(report.stat().st_mode) == 0o600
        initial = (temporary / "initial.txt").read_text(encoding="utf-8")
        assert f"safehermit: unit={fields['unit']}\n" in initial
        assert "memory_peak_bytes" not in initial
        assert not tuple(temporary.glob(".safehermit-resource.*.tmp"))
        bounds_text = bounds.read_text(encoding="utf-8")
        for guard in (
            "bound.wall=APPLIED:91s",
            "bound.cgroup=APPLIED:MemoryMax=268435456 MemorySwapMax=0 "
            "TasksMax=64 CPUQuota=100% CPUTimeMax=30s",
            "bound.bytes=APPLIED:",
            "resource_report_state=COMPLETE",
        ):
            assert guard in bounds_text
        calls = _calls(temporary / "calls.jsonl")
        main = next(
            call
            for call in calls
            if call[0] == "systemd-run"
            and any(
                arg.startswith("--unit=safehermit-") and "probe" not in arg
                for arg in call
            )
        )
        assert "--property=MemoryMax=268435456" in main
        assert "--property=MemorySwapMax=0" in main
        assert "--property=TasksMax=64" in main
        assert "--property=CPUQuota=100%" in main
        assert "--property=RuntimeMaxSec=91" in main
        cleanup_units = [
            arg
            for call in calls
            if call[:3]
            in (
                ["systemctl", "--user", "stop"],
                ["systemctl", "--user", "reset-failed"],
            )
            for arg in call[3:]
        ]
        assert cleanup_units
        assert all(arg == fields["unit"] + ".service" for arg in cleanup_units)
        assert set(
            (temporary / "cleanup-observations.txt")
            .read_text(encoding="ascii")
            .splitlines()
        ) == {"complete"}


def test_missing_property_fails_closed_and_keeps_owned_unit_stub() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(
            temporary,
            report,
            metrics_mode="missing-cpu",
            command="printf ran >/dev/null",
        )
        assert completed.returncode == 2, completed
        fields = _fields(report)
        assert fields["unit"].startswith("safehermit-")
        assert "memory_peak_bytes" not in fields
        assert (temporary / "runner-rc.txt").read_text(encoding="ascii") == "0"
        assert (temporary / "initial.txt").exists()
        assert "RESOURCE REPORT REFUSAL" in completed.stderr


def test_limit_mismatch_is_refused_before_payload_launch() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        marker = temporary / "payload-ran"
        completed = _run(
            temporary,
            report,
            applied_mismatch="memory",
            command=f'printf ran >"{marker}"',
        )
        assert completed.returncode == 2, completed
        assert not marker.exists()
        assert "limit was not applied exactly" in completed.stderr
        fields = _fields(report)
        assert fields["limit_memory_bytes"] == str(256 * 1024 * 1024)
        assert "memory_peak_bytes" not in fields


def test_live_cpu_counter_failure_kills_and_refuses_sample() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        marker = temporary / "payload-started"
        completed = _run(
            temporary,
            report,
            sample_failure="after-preflight",
            command=f'printf started >"{marker}"; while :; do :; done',
        )
        assert completed.returncode == 2, completed
        assert marker.read_text(encoding="ascii") == "started"
        assert "live cgroup resource counters became unreadable" in completed.stderr
        fields = _fields(report)
        assert "memory_peak_bytes" not in fields


def test_cpu_and_wall_limits_are_authoritative_reported_breaches() -> None:
    cases = (
        ("cpu-hit", 10, 1, 126, "cpu_time_limit_hit", "1"),
        ("wall-timeout", 1, 30, 124, "wall_time_limit_hit", "1"),
    )
    for mode, deadline, cpu_timeout, expected_rc, field, expected in cases:
        with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
            temporary = Path(raw)
            report = temporary / "resource.txt"
            completed = _run(
                temporary,
                report,
                metrics_mode=mode,
                deadline=deadline,
                cpu_timeout=cpu_timeout,
                command="while :; do :; done",
            )
            assert completed.returncode == expected_rc, (mode, completed)
            fields = _fields(report)
            assert fields[field] == expected
            assert fields["memory_oom_kills"] == "0"
            assert fields["tasks_limit_hits"] == "0"


def test_lethal_log_cap_keeps_complete_detached_measurement() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(
            temporary,
            report,
            max_log_bytes=64,
            command=(
                "python3 -c 'import os; "
                '[os.write(2,b"x"*1024) for _ in range(128)]\''
            ),
        )
        assert completed.returncode == 125, completed
        fields = _fields(report)
        assert fields["cpu_time_limit_hit"] == "0"
        assert fields["wall_time_limit_hit"] == "0"
        assert fields["log_cap_hit"] == "1"
        assert "resource_report_state=COMPLETE" in completed.stderr
        assert "killed_at_cap=cgroup-killed" in completed.stderr


def test_preexisting_and_symlink_paths_are_refused_before_payload() -> None:
    for path_kind in ("existing", "symlink-parent"):
        with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
            temporary = Path(raw)
            marker = temporary / "payload-ran"
            if path_kind == "existing":
                report = temporary / "resource.txt"
                report.write_text("wrapper: unit=foreign.service\n", encoding="utf-8")
            else:
                actual = temporary / "actual"
                actual.mkdir()
                alias = temporary / "alias"
                alias.symlink_to(actual, target_is_directory=True)
                report = alias / "resource.txt"
            completed = _run(
                temporary,
                report,
                command=f'printf ran >"{marker}"',
            )
            assert completed.returncode == 2, (path_kind, completed)
            assert not marker.exists()
            calls = _calls(temporary / "calls.jsonl")
            assert not any(
                call[0] == "systemd-run"
                and any(
                    arg.startswith("--unit=safehermit-") and "probe" not in arg
                    for arg in call
                )
                for call in calls
            )
            assert not any(
                call[:3] == ["systemctl", "--user", "kill"] for call in calls
            )


def test_resource_and_diagnostic_report_cannot_alias() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "same.txt"
        completed = _run(
            temporary,
            report,
            sh_report=report,
            command="exit 0",
        )
        assert completed.returncode == 2
        assert "must name different files" in completed.stderr
        assert not report.exists()


def test_detached_mode_requires_every_explicit_limit() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = subprocess.run(
            [
                str(SAFEHERMIT),
                "--sh-resource-report",
                str(report),
                "--sh-memory-max-bytes",
                str(256 * 1024 * 1024),
                "--sh-tasks-max",
                "64",
                "--sh-cpu-cores",
                "1",
                # Deliberately omit --sh-cpu-timeout and --sh-deadline.
                "/bin/true",
            ],
            check=False,
            text=True,
            capture_output=True,
            env={**os.environ, "SAFEHERMIT_LOG_ROOT": str(temporary / "logs")},
        )
        assert completed.returncode == 2
        assert "requires --sh-memory-max-bytes" in completed.stderr
        assert not report.exists()


def main() -> int:
    tests = (
        test_complete_report_is_published_before_cleanup,
        test_missing_property_fails_closed_and_keeps_owned_unit_stub,
        test_limit_mismatch_is_refused_before_payload_launch,
        test_live_cpu_counter_failure_kills_and_refuses_sample,
        test_cpu_and_wall_limits_are_authoritative_reported_breaches,
        test_lethal_log_cap_keeps_complete_detached_measurement,
        test_preexisting_and_symlink_paths_are_refused_before_payload,
        test_resource_and_diagnostic_report_cannot_alias,
        test_detached_mode_requires_every_explicit_limit,
    )
    for test in tests:
        test()
        print(f"PASS {test.__name__}")
    print(f"{len(tests)} passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
