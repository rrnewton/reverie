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
import signal
import stat
import subprocess
import sys
import tempfile
import textwrap
import time


ROOT = Path(__file__).resolve().parent.parent
SAFEHERMIT = ROOT / "bin" / "safehermit"
CASE_TIMEOUT_SECONDS = 20.0
PROCESS_TIMEOUT_SECONDS = 15.0
CLEANUP_RESERVE_SECONDS = 3.0
_ACTIVE_RUNS: dict[int, Path] = {}


def _handle_suite_signal(signum: int, _frame: object) -> None:
    cleanup_deadline = time.monotonic() + 4.5
    for process_id, report in tuple(_ACTIVE_RUNS.items()):
        try:
            os.killpg(process_id, signal.SIGKILL)
        except ProcessLookupError:
            pass
        _cleanup_owned_unit(report, cleanup_deadline)
    raise SystemExit(128 + signum)


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
        import time

        args = sys.argv[1:]
        with Path(os.environ["FAKE_CALL_LOG"]).open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(["systemd-run", *args]) + "\n")
        unit = next((arg.split("=", 1)[1] for arg in args if arg.startswith("--unit=")), "")
        if unit.startswith("safehermit-probe-"):
            raise SystemExit(0)
        replacement_source = os.environ.get("FAKE_REPLACEMENT_SOURCE")
        replacement_target = os.environ.get("FAKE_REPLACEMENT_TARGET")
        if replacement_source and replacement_target:
            os.replace(replacement_source, replacement_target)
        report = Path(os.environ["EXPECT_RESOURCE_REPORT"])
        initial = report.read_text(encoding="utf-8")
        if initial.count("safehermit: unit=") != 1 or "memory_peak_bytes" in initial:
            raise SystemExit(90)
        if f"safehermit: unit={unit}\n" not in initial:
            raise SystemExit(91)
        Path(os.environ["INITIAL_REPORT_SEEN"]).write_text(initial, encoding="utf-8")
        applied_args = list(args)
        mismatch = os.environ.get("FAKE_APPLIED_MISMATCH")
        replacements = {
            "memory": ("--property=MemoryMax=", "--property=MemoryMax=134217728"),
            "swap": ("--property=MemorySwapMax=", "--property=MemorySwapMax=4096"),
            "tasks": ("--property=TasksMax=", "--property=TasksMax=63"),
            "cpu": ("--property=CPUQuota=", "--property=CPUQuota=200%"),
        }
        if mismatch in replacements:
            prefix, replacement = replacements[mismatch]
            applied_args = [replacement if arg.startswith(prefix) else arg for arg in applied_args]
        completed = subprocess.run(["/usr/bin/systemd-run", *applied_args], check=False)
        time.sleep(float(os.environ.get("FAKE_POST_RUN_SLEEP", "0")))
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
        if os.environ.get("FAKE_METRICS_MODE") == "real":
            completed = subprocess.run(
                ["/usr/bin/systemctl", *args], check=False, capture_output=True, text=True,
            )
            Path(os.environ["REAL_FINAL_PROPERTIES"]).write_text(completed.stdout, encoding="utf-8")
            if completed.returncode == 0 and {"MemoryPeak", "CPUUsageNSec"} <= set(requested):
                Path(os.environ["REAL_FINAL_QUERY_COMPLETE"]).touch()
            sys.stdout.write(completed.stdout)
            sys.stderr.write(completed.stderr)
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
        elif os.environ.get("FAKE_METRICS_MODE") == "oom-hit":
            values["Result"] = "oom-kill"
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
        if [ "${1:-}" = - ]; then
            script=$(command cat)
            if [ "${FAKE_SAMPLE_FAILURE:-}" = after-preflight ] \
                && [[ "$script" == *'cpu.stat'* ]]; then
                if [ -e "$FAKE_SAMPLE_STATE" ]; then
                    exit 88
                fi
                : >"$FAKE_SAMPLE_STATE"
            fi
            output=$(printf '%s\n' "$script" | /usr/local/bin/python3 - "${@:2}")
            rc=$?
            if [ "$rc" -eq 0 ] && [[ "$output" =~ ^[0-9]+\ [0-9]+\ [0-9]+\ [0-9]+$ ]]; then
                read -r usage oom_hits pids_hits tasks_peak <<<"$output"
                case "${FAKE_SAMPLE_COUNTER:-}" in
                    oom-hit) oom_hits=1 ;;
                    pids-hit) pids_hits=1 ;;
                esac
                printf '%s %s %s %s\n' "$usage" "$oom_hits" "$pids_hits" "$tasks_peak"
            else
                [ -z "$output" ] || printf '%s\n' "$output"
            fi
            exit "$rc"
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
    sample_counter: str = "",
    reference_failure: str = "",
    post_run_sleep: float = 0,
    replacement_source: Path | None = None,
    replacement_target: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    overall_deadline = time.monotonic() + CASE_TIMEOUT_SECONDS
    operation_deadline = overall_deadline - CLEANUP_RESERVE_SECONDS
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
            "REAL_FINAL_PROPERTIES": str(temporary / "real-final-properties.txt"),
            "REAL_FINAL_QUERY_COMPLETE": str(temporary / "real-final-query-complete"),
            "REFERENCE_LOSS_MARKER": str(temporary / "payload-started"),
            "REFERENCE_PROXY_EVENTS": str(temporary / "reference-proxy-events.jsonl"),
            "FAKE_APPLIED_MISMATCH": applied_mismatch,
            "FAKE_SAMPLE_FAILURE": sample_failure,
            "FAKE_SAMPLE_STATE": str(temporary / "sample-state"),
            "FAKE_SAMPLE_COUNTER": sample_counter,
            "FAKE_POST_RUN_SLEEP": str(post_run_sleep),
            "FAKE_REPLACEMENT_SOURCE": str(replacement_source or ""),
            "FAKE_REPLACEMENT_TARGET": str(replacement_target or ""),
        }
    )
    safehermit = SAFEHERMIT
    if reference_failure:
        # Fault injection is confined to a test-only helper copy. Production
        # deliberately has no environment switch for bypassing unit retention.
        local_bin = temporary / "bin"
        local_bin.mkdir()
        safehermit = local_bin / "safehermit"
        safehermit.write_bytes(SAFEHERMIT.read_bytes())
        safehermit.chmod(0o755)
        helper = local_bin / "safehermit-unit-reference.py"
        if reference_failure == "acquire":
            helper.write_text('raise RuntimeError("injected reference startup failure")\n')
        elif reference_failure == "loss":
            helper.write_text(textwrap.dedent("""\
                import os, pathlib, subprocess, sys, time
                child = subprocess.Popen(["/usr/bin/python3", ORIGINAL, *sys.argv[1:]])
                try:
                    deadline = time.monotonic() + 5
                    while not pathlib.Path(os.environ["REFERENCE_LOSS_MARKER"]).exists():
                        if child.poll() is not None:
                            raise SystemExit(child.returncode)
                        if time.monotonic() >= deadline:
                            raise RuntimeError("payload did not reach reference-loss control")
                        time.sleep(0.01)
                    child.kill()
                    raise SystemExit(child.wait())
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait()
                """).replace("ORIGINAL", repr(str(ROOT / "bin/safehermit-unit-reference.py"))))
        else:
            assert reference_failure in {"publication-loss", "release"}
            helper.write_text(textwrap.dedent("""\
                import json, os, pathlib, subprocess, sys
                child = subprocess.Popen(
                    ["/usr/bin/python3", ORIGINAL, *sys.argv[1:]],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                )
                def event(value):
                    with pathlib.Path(os.environ["REFERENCE_PROXY_EVENTS"]).open("a") as stream:
                        stream.write(json.dumps(value) + "\\n")
                def response():
                    line = child.stdout.readline()
                    if not line:
                        raise RuntimeError("real reference helper stopped responding")
                    return line
                try:
                    sys.stdout.buffer.write(response())
                    sys.stdout.buffer.flush()
                    while True:
                        command = sys.stdin.buffer.read(1)
                        if not command:
                            raise RuntimeError("reference proxy owner closed its pipe")
                        if MODE == "publication-loss" and command == b"c" \\
                                and pathlib.Path(os.environ["REAL_FINAL_QUERY_COMPLETE"]).exists():
                            child.kill()
                            child.wait()
                            event({"injection": "loss-after-real-final-query", "helper_exit": child.returncode})
                            raise SystemExit(89)
                        child.stdin.write(command)
                        child.stdin.flush()
                        reply = response()
                        if MODE == "release" and command == b"r":
                            assert reply == b"released\\n", reply
                            assert child.wait() == 0
                            event({"injection": "release-acknowledgement-failure", "real_reply": reply.decode().strip()})
                            sys.stdout.buffer.write(b"injected-release-failure\\n")
                            sys.stdout.buffer.flush()
                            break
                        sys.stdout.buffer.write(reply)
                        sys.stdout.buffer.flush()
                        if command == b"r":
                            raise SystemExit(child.wait())
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait()
                """).replace("ORIGINAL", repr(str(ROOT / "bin/safehermit-unit-reference.py")))
                    .replace("MODE", repr(reference_failure)))
    argv = [
        str(safehermit),
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
    process = subprocess.Popen(
        argv,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        start_new_session=True,
    )
    _ACTIVE_RUNS[process.pid] = report
    try:
        try:
            stdout, stderr = process.communicate(
                timeout=min(PROCESS_TIMEOUT_SECONDS, _remaining(operation_deadline))
            )
        except subprocess.TimeoutExpired as error:
            os.killpg(process.pid, signal.SIGKILL)
            _cleanup_owned_unit(report, overall_deadline)
            stdout, stderr = process.communicate(timeout=_remaining(overall_deadline))
            raise AssertionError(
                f"safehermit resource fixture exceeded independent 15s test bound: {argv}"
            ) from error
        completed = subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)
        (temporary / "wrapper.stdout").write_text(stdout, encoding="utf-8")
        (temporary / "wrapper.stderr").write_text(stderr, encoding="utf-8")
        (temporary / "wrapper-result.json").write_text(
            json.dumps({"argv": argv, "returncode": process.returncode}, indent=2) + "\n",
            encoding="utf-8",
        )
        try:
            _assert_owned_unit_gone(report, operation_deadline)
        except BaseException:
            _cleanup_owned_unit(report, overall_deadline)
            raise
        return completed
    finally:
        _ACTIVE_RUNS.pop(process.pid, None)


def _fields(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        payload = line.rsplit(": ", 1)[-1]
        key, separator, value = payload.partition("=")
        if separator:
            assert key not in result, f"duplicate {key} in {path}"
            result[key] = value
    return result


def _owned_unit(report: Path) -> str | None:
    if not report.is_file():
        return None
    fields = _fields(report)
    run_id = fields.get("run_id", "")
    unit = fields.get("unit", "")
    if unit != f"safehermit-{run_id}" or not unit.startswith("safehermit-"):
        return None
    if any(character not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-" for character in unit):
        return None
    return unit


def _remaining(deadline: float, ceiling: float | None = None) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("safehermit resource test exhausted its overall deadline")
    return remaining if ceiling is None else min(remaining, ceiling)


def _cleanup_owned_unit(report: Path, deadline: float | None = None) -> None:
    unit = _owned_unit(report)
    if unit is None:
        return
    if deadline is None:
        deadline = time.monotonic() + 3.0
    service = unit + ".service"
    actions = (
        ("kill", "--kill-whom=all", "--signal=KILL", service),
        ("stop", service),
        ("reset-failed", service),
    )
    for index, arguments in enumerate(actions):
        remaining_actions = len(actions) - index
        try:
            action_timeout = _remaining(deadline) / remaining_actions
            subprocess.run(
                ["/usr/bin/systemctl", "--user", *arguments],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=action_timeout,
            )
        except (OSError, subprocess.TimeoutExpired, TimeoutError):
            # Each exact-unit action is independent. A hung kill must not skip
            # stop/reset-failed; the remaining deadline is divided among them.
            continue


def _owned_unit_state(report: Path, deadline: float) -> tuple[str, str, str]:
    unit = _owned_unit(report)
    if unit is None:
        return "not-found", "inactive", ""
    completed = subprocess.run(
        [
            "/usr/bin/systemctl",
            "--user",
            "show",
            unit + ".service",
            "-p",
            "LoadState",
            "-p",
            "ActiveState",
            "-p",
            "ControlGroup",
        ],
        check=False,
        text=True,
        capture_output=True,
        timeout=_remaining(deadline, 0.25),
    )
    values = {}
    for line in completed.stdout.splitlines():
        key, separator, value = line.partition("=")
        if separator:
            values[key] = value
    return (
        values.get("LoadState", ""),
        values.get("ActiveState", ""),
        values.get("ControlGroup", ""),
    )


def _assert_owned_unit_gone(report: Path, deadline: float) -> None:
    unit = _owned_unit(report)
    if unit is None:
        return
    final = ("", "", "")
    while time.monotonic() < deadline:
        final = _owned_unit_state(report, deadline)
        load, active, control_group = final
        group_exists = bool(control_group) and Path("/sys/fs/cgroup" + control_group).exists()
        if load == "not-found" and active != "active" and not group_exists:
            return
        time.sleep(min(0.02, _remaining(deadline)))
    raise AssertionError(f"owned unit {unit} was not reaped: state={final}")


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
        assert "--property=KillSignal=SIGKILL" in main
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
        assert len(cleanup_units) == 2, cleanup_units
        assert all(arg == fields["unit"] + ".service" for arg in cleanup_units)
        assert (
            temporary.joinpath("cleanup-observations.txt")
            .read_text(encoding="ascii")
            .splitlines()
            == ["complete", "complete"]
        )


def test_pinned_binary_preserves_argv0_and_reports_executed_inode() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(
            temporary,
            report,
            command='printf "%s\\n" "$0"',
        )
        assert completed.returncode == 0, completed
        assert completed.stdout == "/bin/sh\n"

    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        selected = temporary / "selected"
        replacement = temporary / "replacement"
        _write_executable(selected, "#!/bin/sh\nprintf 'old\\n'\n")
        replacement_bytes = b"#!/bin/sh\nprintf 'new\\n'\n"
        replacement.write_bytes(replacement_bytes)
        replacement.chmod(0o755)
        tools = _fake_tools(temporary)
        env = {
            **os.environ,
            "PATH": f"{tools}:{os.environ['PATH']}",
            "SAFEHERMIT_LOG_ROOT": str(temporary / "logs"),
            "EXPECT_RESOURCE_REPORT": str(report),
            "INITIAL_REPORT_SEEN": str(temporary / "initial.txt"),
            "FAKE_RUNNER_RC": str(temporary / "runner-rc.txt"),
            "FAKE_CALL_LOG": str(temporary / "calls.jsonl"),
            "CLEANUP_OBSERVATIONS": str(temporary / "cleanup-observations.txt"),
            "FAKE_METRICS_MODE": "complete",
            "FAKE_APPLIED_MISMATCH": "",
            "FAKE_SAMPLE_FAILURE": "",
            "FAKE_SAMPLE_STATE": str(temporary / "sample-state"),
            "FAKE_SAMPLE_COUNTER": "",
            "FAKE_POST_RUN_SLEEP": "0",
            "FAKE_REPLACEMENT_SOURCE": str(replacement),
            "FAKE_REPLACEMENT_TARGET": str(selected),
        }
        argv = [
            str(SAFEHERMIT),
            "--sh-resource-report",
            str(report),
            "--sh-deadline",
            "10",
            "--sh-memory-max-bytes",
            str(256 * 1024 * 1024),
            "--sh-tasks-max",
            "64",
            "--sh-cpu-cores",
            "1",
            "--sh-cpu-timeout",
            "5",
            str(selected),
        ]
        overall_deadline = time.monotonic() + CASE_TIMEOUT_SECONDS
        operation_deadline = overall_deadline - CLEANUP_RESERVE_SECONDS
        process = subprocess.Popen(
            argv,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            start_new_session=True,
        )
        _ACTIVE_RUNS[process.pid] = report
        try:
            stdout, stderr = process.communicate(timeout=_remaining(operation_deadline))
            completed = subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)
            assert completed.returncode == 0, completed
            assert completed.stdout == "new\n"
            assert _fields(report)["binary_sha256"] == hashlib.sha256(
                replacement_bytes
            ).hexdigest()
            _assert_owned_unit_gone(report, operation_deadline)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=_remaining(overall_deadline))
            _cleanup_owned_unit(report, overall_deadline)
            _ACTIVE_RUNS.pop(process.pid, None)


def test_payload_cannot_forge_executed_binary_digest() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(
            temporary,
            report,
            command=(
                'directory=$(dirname "$(find "$SAFEHERMIT_LOG_ROOT" '
                '-name .resource-payload-start -print -quit)"); '
                'printf "%064d\\n" 0 >"$directory/.resource-binary-sha256"'
            ),
        )
        assert completed.returncode == 0, completed
        expected = hashlib.sha256(Path("/bin/sh").read_bytes()).hexdigest()
        assert _fields(report)["binary_sha256"] == expected


def test_short_success_keeps_real_terminal_accounting() -> None:
    # Do not manufacture loaded/active properties: a short exit0 must retain
    # genuine post-termination counters, even after systemd-run has returned.
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(temporary, report, metrics_mode="real", command="exit 0")
        assert completed.returncode == 0, completed
        assert completed.stdout == ""
        assert _fields(report)["cpu_usage_nsec"].isdigit()
        raw_properties = (temporary / "real-final-properties.txt").read_text()
        properties = dict(line.split("=", 1) for line in raw_properties.splitlines())
        fields = _fields(report)
        assert properties["Id"] == fields["unit"] + ".service"
        assert properties["LoadState"] == "loaded"
        assert properties["ActiveState"] == "inactive"
        assert properties["SubState"] == "dead"
        assert properties["Result"] == "success"
        assert 0 < int(properties["MemoryPeak"]) < 2**64 - 1
        assert 0 < int(properties["CPUUsageNSec"]) < 2**64 - 1
        assert fields["memory_peak_bytes"] == properties["MemoryPeak"]
        assert fields["cpu_usage_nsec"] == properties["CPUUsageNSec"]
        assert int(fields["tasks_peak"]) > 0
        assert (temporary / "runner-rc.txt").read_text() == "0"
        # _run also requires the exact owned unit and cgroup to be gone.


def test_reference_acquisition_failure_prevents_payload() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        marker = temporary / "payload-started"
        completed = _run(
            temporary, report, reference_failure="acquire",
            command=f'printf started >"{marker}"',
        )
        assert completed.returncode == 2, completed
        assert not marker.exists()
        assert "could not retain the owned unit before payload launch" in completed.stderr
        assert "memory_peak_bytes" not in _fields(report)


def test_reference_loss_refuses_and_retires_payload() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        marker = temporary / "payload-started"
        completed = _run(
            temporary, report, reference_failure="loss",
            command=f'printf started >"{marker}"; while :; do :; done',
        )
        assert completed.returncode == 2, completed
        assert marker.read_text() == "started"
        assert "owned unit reference ended before terminal accounting" in completed.stderr
        assert "memory_peak_bytes" not in _fields(report)


def test_reference_loss_after_real_metrics_refuses_publication() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(
            temporary, report, metrics_mode="real", reference_failure="publication-loss",
            command="exit 0",
        )
        assert completed.returncode == 2, completed
        reason = "owned unit reference was lost before report publication"
        assert f"resource_report_state=INCOMPLETE:{reason}" in completed.stderr
        assert (temporary / "runner-rc.txt").read_text() == "0"
        properties = dict(line.split("=", 1) for line in
                          (temporary / "real-final-properties.txt").read_text().splitlines())
        assert properties["LoadState"] == "loaded"
        assert properties["ActiveState"] == "inactive"
        assert properties["SubState"] == "dead"
        assert properties["Result"] == "success"
        assert 0 < int(properties["MemoryPeak"]) < 2**64 - 1
        assert 0 < int(properties["CPUUsageNSec"]) < 2**64 - 1
        assert json.loads((temporary / "reference-proxy-events.jsonl").read_text()) == {
            "injection": "loss-after-real-final-query", "helper_exit": -signal.SIGKILL,
        }
        assert not {"memory_peak_bytes", "cpu_usage_nsec", "tasks_peak"} & _fields(report).keys()


def test_reference_release_failure_preserves_genuine_metrics() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(
            temporary, report, metrics_mode="real", reference_failure="release",
            command="exit 0",
        )
        assert completed.returncode == 2, completed
        assert "resource_report_state=INCOMPLETE:owned unit reference release did not complete" in completed.stderr
        assert (temporary / "runner-rc.txt").read_text() == "0"
        properties = dict(line.split("=", 1) for line in
                          (temporary / "real-final-properties.txt").read_text().splitlines())
        fields = _fields(report)
        assert properties["Id"] == fields["unit"] + ".service"
        assert properties["LoadState"] == "loaded"
        assert properties["ActiveState"] == "inactive"
        assert properties["SubState"] == "dead"
        assert properties["Result"] == "success"
        assert 0 < int(properties["MemoryPeak"]) < 2**64 - 1
        assert 0 < int(properties["CPUUsageNSec"]) < 2**64 - 1
        assert fields["memory_peak_bytes"] == properties["MemoryPeak"]
        assert fields["cpu_usage_nsec"] == properties["CPUUsageNSec"]
        assert int(fields["tasks_peak"]) > 0
        assert json.loads((temporary / "reference-proxy-events.jsonl").read_text()) == {
            "injection": "release-acknowledgement-failure", "real_reply": "released",
        }


def test_first_accounting_refusal_survives_release_failure() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(
            temporary, report, metrics_mode="missing-cpu", reference_failure="release",
            command="exit 0",
        )
        assert completed.returncode == 2, completed
        reason = "required post-termination resource property is missing or malformed"
        assert f"resource_report_state=INCOMPLETE:{reason}" in completed.stderr
        assert "owned unit reference release did not complete" not in completed.stderr
        assert not {"memory_peak_bytes", "cpu_usage_nsec", "tasks_peak"} & _fields(report).keys()
        assert (temporary / "runner-rc.txt").read_text() == "0"
        assert json.loads((temporary / "reference-proxy-events.jsonl").read_text()) == {
            "injection": "release-acknowledgement-failure", "real_reply": "released",
        }
        properties = tuple((temporary / "logs").glob("*/.resource-unit-properties"))
        assert len(properties) == 1
        assert "MemoryPeak=123456789" in properties[0].read_text()
        assert "CPUUsageNSec=" not in properties[0].read_text()


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
        properties = tuple((temporary / "logs").glob("*/.resource-unit-properties"))
        assert len(properties) == 1
        raw_properties = properties[0].read_text()
        assert "MemoryPeak=123456789" in raw_properties
        assert "CPUUsageNSec=" not in raw_properties


def test_limit_mismatch_is_refused_before_payload_launch() -> None:
    for mismatch in ("memory", "swap", "tasks", "cpu"):
        with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
            temporary = Path(raw)
            report = temporary / "resource.txt"
            marker = temporary / "payload-ran"
            completed = _run(
                temporary,
                report,
                applied_mismatch=mismatch,
                command=f'printf ran >"{marker}"',
            )
            assert completed.returncode == 2, (mismatch, completed)
            assert not marker.exists(), mismatch
            assert "limit was not applied exactly" in completed.stderr, mismatch
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
        ("cpu-hit", 10, 1, 126, "cpu_time_limit_hit", "1", 0),
        # Keep systemd-run alive after RuntimeMaxSec has removed the cgroup.
        # This deterministically exercises the race that used to mistake the
        # client process for a still-live payload and refuse the final report.
        ("wall-timeout", 1, 30, 124, "wall_time_limit_hit", "1", 0.5),
    )
    for mode, deadline, cpu_timeout, expected_rc, field, expected, post_sleep in cases:
        with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
            temporary = Path(raw)
            report = temporary / "resource.txt"
            completed = _run(
                temporary,
                report,
                metrics_mode=mode,
                deadline=deadline,
                cpu_timeout=cpu_timeout,
                post_run_sleep=post_sleep,
                command="while :; do :; done",
            )
            assert completed.returncode == expected_rc, (mode, completed)
            fields = _fields(report)
            assert fields[field] == expected
            assert fields["memory_oom_kills"] == "0"
            assert fields["tasks_limit_hits"] == "0"


def test_finite_payload_crossing_cpu_budget_is_not_missed() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        completed = _run(
            temporary,
            report,
            metrics_mode="cpu-hit",
            cpu_timeout=1,
            command="exit 0",
        )
        assert completed.returncode == 126, completed
        fields = _fields(report)
        assert fields["cpu_time_limit_hit"] == "1"
        assert fields["cpu_usage_nsec"] == "1100000000"


def test_oom_and_pid_limit_events_are_not_hardcoded_zero() -> None:
    cases = (
        ("oom-hit", "", "memory_oom_kills"),
        ("complete", "pids-hit", "tasks_limit_hits"),
    )
    for metrics_mode, sample_counter, field in cases:
        with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
            temporary = Path(raw)
            report = temporary / "resource.txt"
            completed = _run(
                temporary,
                report,
                metrics_mode=metrics_mode,
                sample_counter=sample_counter,
                command="exit 0",
            )
            assert completed.returncode == 0, (field, completed)
            assert int(_fields(report)[field]) >= 1


def test_background_descendants_are_accounted_until_quiescent() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        marker = temporary / "background-finished"
        completed = _run(
            temporary,
            report,
            command=f"(sleep 0.3; printf done >'{marker}') & exit 23",
        )
        assert completed.returncode == 23, completed
        assert marker.read_text(encoding="ascii") == "done"
        assert _fields(report)["wall_time_limit_hit"] == "0"

    startup_misses: list[str] = []
    for attempt in range(3):
        with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
            temporary = Path(raw)
            report = temporary / "resource.txt"
            descendant_pid_file = temporary / "descendant-pid"
            completed = _run(
                temporary,
                report,
                metrics_mode="wall-timeout",
                deadline=1,
                command=(
                    f"(trap '' TERM; exec sleep 30) </dev/null >/dev/null 2>/dev/null & "
                    f"printf '%s' $! >'{descendant_pid_file}'; exit 23"
                ),
            )
            assert completed.returncode == 124, (attempt, completed)
            fields = _fields(report)
            assert fields["wall_time_limit_hit"] == "1", (attempt, fields)
            if not descendant_pid_file.is_file():
                startup_misses.append(
                    f"attempt={attempt + 1} stderr={completed.stderr!r} report={report.read_text()!r}"
                )
                continue
            descendant_pid = int(descendant_pid_file.read_text(encoding="ascii"))
            assert not Path(f"/proc/{descendant_pid}").exists()
            break
    else:
        raise AssertionError(
            "strict 1s wall fixture never scheduled its payload in three clean units: "
            + " | ".join(startup_misses)
        )


def test_term_reaps_owned_unit_and_retains_incomplete_stub() -> None:
    with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
        temporary = Path(raw)
        report = temporary / "resource.txt"
        overall_deadline = time.monotonic() + CASE_TIMEOUT_SECONDS
        operation_deadline = overall_deadline - CLEANUP_RESERVE_SECONDS
        process = subprocess.Popen(
            [
                str(SAFEHERMIT),
                "--sh-resource-report",
                str(report),
                "--sh-deadline",
                "30",
                "--sh-memory-max-bytes",
                str(256 * 1024 * 1024),
                "--sh-tasks-max",
                "64",
                "--sh-cpu-cores",
                "1",
                "--sh-cpu-timeout",
                "20",
                "/bin/sh",
                "-c",
                "exec sleep 30",
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, "SAFEHERMIT_LOG_ROOT": str(temporary / "logs")},
            start_new_session=True,
        )
        _ACTIVE_RUNS[process.pid] = report
        try:
            readiness_deadline = min(operation_deadline, time.monotonic() + 5.0)
            while time.monotonic() < readiness_deadline:
                if (
                    _owned_unit(report) is not None
                    and _owned_unit_state(report, readiness_deadline)[1] == "active"
                ):
                    break
                assert process.poll() is None
                time.sleep(min(0.02, _remaining(readiness_deadline)))
            else:
                raise AssertionError("resource unit did not become active before TERM probe")
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=_remaining(operation_deadline))
            assert process.returncode == 143, (process.returncode, stdout, stderr)
            _assert_owned_unit_gone(report, operation_deadline)
            fields = _fields(report)
            assert fields["unit"].startswith("safehermit-")
            assert "memory_peak_bytes" not in fields
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=_remaining(overall_deadline))
            _cleanup_owned_unit(report, overall_deadline)
            _ACTIVE_RUNS.pop(process.pid, None)


def test_lethal_log_cap_keeps_complete_detached_measurement() -> None:
    commands = (
        ("immediate-small-overflow", "printf '%0100d' 0 >&2"),
        (
            "large-overflow",
            "python3 -c 'import os; "
            '[os.write(2,b"x"*1024) for _ in range(128)]\'',
        ),
    )
    for label, command in commands:
        with tempfile.TemporaryDirectory(prefix="safehermit-resource-test-") as raw:
            temporary = Path(raw)
            report = temporary / "resource.txt"
            completed = _run(
                temporary,
                report,
                max_log_bytes=64,
                command=command,
            )
            assert completed.returncode == 125, (label, completed)
            fields = _fields(report)
            assert fields["cpu_time_limit_hit"] == "0"
            assert fields["wall_time_limit_hit"] == "0"
            assert fields["log_cap_hit"] == "1"
            assert "resource_report_state=COMPLETE" in completed.stderr
            assert "killed_at_cap=parent-cgroup-killed" in completed.stderr


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
    for handled_signal in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(handled_signal, _handle_suite_signal)
    tests = (
        test_short_success_keeps_real_terminal_accounting,
        test_reference_acquisition_failure_prevents_payload,
        test_reference_loss_refuses_and_retires_payload,
        test_reference_loss_after_real_metrics_refuses_publication,
        test_reference_release_failure_preserves_genuine_metrics,
        test_first_accounting_refusal_survives_release_failure,
        test_complete_report_is_published_before_cleanup,
        test_pinned_binary_preserves_argv0_and_reports_executed_inode,
        test_payload_cannot_forge_executed_binary_digest,
        test_missing_property_fails_closed_and_keeps_owned_unit_stub,
        test_limit_mismatch_is_refused_before_payload_launch,
        test_live_cpu_counter_failure_kills_and_refuses_sample,
        test_cpu_and_wall_limits_are_authoritative_reported_breaches,
        test_finite_payload_crossing_cpu_budget_is_not_missed,
        test_oom_and_pid_limit_events_are_not_hardcoded_zero,
        test_background_descendants_are_accounted_until_quiescent,
        test_term_reaps_owned_unit_and_retains_incomplete_stub,
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
