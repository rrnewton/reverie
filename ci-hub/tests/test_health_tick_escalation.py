"""Brackets for the health tick's durable escalation queue."""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[2]
WRAPPER = ROOT / "scripts/hermit-health-tick.sh"


def extract_function(name: str) -> str:
    match = re.search(
        rf"^{name}\(\) \{{$.*?^\}}$", WRAPPER.read_text(), re.MULTILINE | re.DOTALL
    )
    assert match, f"{name} is no longer defined in {WRAPPER}"
    return match.group(0)


@pytest.fixture()
def bench(tmp_path: Path):
    report = tmp_path / "report"
    invlog = tmp_path / "invlog"
    queue = tmp_path / "queue"
    calls = tmp_path / "systemd-run-args"
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    (fake_bin / "systemctl").write_text("#!/bin/sh\nexit 1\n")
    (fake_bin / "systemctl").chmod(0o755)
    (fake_bin / "systemd-run").write_text(
        '#!/bin/sh\nprintf \'%s\\n\' "$@" > "$SYSTEMD_RUN_ARGS"\n'
    )
    (fake_bin / "systemd-run").chmod(0o755)
    operational_tool = tmp_path / "operational-tool"
    operational_tool.write_text("#!/bin/sh\nexit 0\n")
    operational_tool.chmod(0o555)

    functions = "\n".join(
        extract_function(name)
        for name in ("escalate_live_report_findings", "launch_health_escalation")
    )

    def run(report_lines: str, *, complete: bool, twice: bool = False) -> dict[str, object]:
        report.write_text(report_lines)
        invlog.write_text("")
        calls.write_text("")
        preamble = "\n".join(
            [
                "set -uo pipefail",
                f"ROOT={tmp_path}",
                f"INVLOG={invlog}",
                f"LIVE_REPORT={report}",
                "LIVE_REPORT_STAMP=2026-09-03T00:00:00Z",
                "LIVE_REPORT_ESCALATED=0",
                f"TICK_REPORT_COMPLETE={1 if complete else 0}",
                "TICK_CANCEL_STATUS=",
                "ESCALATION_AGENT=health-tick",
                "ESCALATION_TG_DB=hermit2",
                f"ESCALATION_PENDING_DIR={queue}",
                f"ESCALATION_LOG={tmp_path / 'delivery.log'}",
                f"ESCALATE={tmp_path / 'escalate'}",
                f"DEV_HERMIT_OPERATIONAL_TOOL={operational_tool}",
                f"DEV_HERMIT_TOOL_PARENT_SHA={'a' * 40}",
                "ts() { printf '2026-09-03T00:00:00Z\\n'; }",
            ]
        )
        body = "\nescalate_live_report_findings" * (2 if twice else 1)
        completed = subprocess.run(
            ["bash", "-c", f"{preamble}\n{functions}{body}"],
            text=True,
            capture_output=True,
            env=os.environ
            | {
                "PATH": f"{fake_bin}:{os.environ['PATH']}",
                "SYSTEMD_RUN_ARGS": str(calls),
            },
            check=False,
        )
        return {
            "returncode": completed.returncode,
            "stderr": completed.stderr,
            "events": sorted(queue.glob("event-*")) if queue.exists() else [],
            "calls": calls.read_text().splitlines(),
            "invlog": invlog.read_text(),
            "root": tmp_path,
        }

    return run


def test_action_and_error_are_one_alarm_event(bench) -> None:
    result = bench(
        "ACTION: hard-warn-coordinator component=a\n"
        "NOTE: ops-state enabled=true\n"
        "ERROR: reminder b could not run\n",
        complete=True,
    )
    assert result["returncode"] == 0
    events = result["events"]
    assert len(events) == 1 and str(events[0]).endswith(".alarm")
    assert events[0].read_text().splitlines() == [
        "ACTION: hard-warn-coordinator component=a",
        "ERROR: reminder b could not run",
    ]
    assert "kind=alarm findings=2" in result["invlog"]


def test_interrupted_empty_report_does_not_create_retry_event(bench) -> None:
    result = bench("", complete=False)
    assert result["returncode"] == 0
    assert result["events"] == []
    assert result["calls"] == []
    assert "escalation_skipped=incomplete-report" in result["invlog"]


def test_interrupted_findings_are_queued(bench) -> None:
    result = bench("ACTION: hard-warn-coordinator component=a\n", complete=False)
    assert len(result["events"]) == 1
    assert str(result["events"][0]).endswith(".alarm")
    assert any(
        arg.startswith("--unit=hermit-health-escalation-")
        for arg in result["calls"]
    )


def test_complete_clean_report_queues_resolve_after_prior_alarm(bench) -> None:
    first = bench("ACTION: hard-warn-coordinator component=a\n", complete=True)
    assert len(first["events"]) == 1
    second = bench("NOTE: emitted 0 instruction(s) this tick\n", complete=True)
    events = second["events"]
    assert len(events) == 2
    assert str(events[0]).endswith(".alarm")
    assert str(events[1]).endswith(".resolve")


def test_normal_path_and_exit_trap_queue_only_once(bench) -> None:
    result = bench(
        "ACTION: hard-warn-coordinator component=a\n", complete=True, twice=True
    )
    assert len(result["events"]) == 1


def test_detached_worker_has_durable_output_and_exact_queue(bench) -> None:
    result = bench("ACTION: hard-warn-coordinator component=a\n", complete=True)
    arguments = result["calls"]
    assert any(arg.startswith("--property=StandardOutput=append:") for arg in arguments)
    assert any(arg.startswith("--property=StandardError=append:") for arg in arguments)
    command_index = arguments.index("--")
    assert arguments[command_index + 1].endswith("operational-tool")
    assert arguments[command_index + 2 : command_index + 10] == [
        "--run-current-tool",
        "--state-root",
        str(result["root"]),
        "--tool-parent-sha",
        "a" * 40,
        "--tool-relative",
        "ci-hub/bin/run-health-escalation",
        "--",
    ]
    queue_index = arguments.index("--queue")
    assert arguments[queue_index + 1].endswith("queue")
