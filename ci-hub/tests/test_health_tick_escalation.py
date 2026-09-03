"""Brackets on the tick wrapper's escalation wiring.

The wrapper decides three things that nothing else does: which of its report
lines count as findings, whether a clean tick closes the issue a dirty one
opened, and whether it can be run twice without saying everything twice. Those
decisions live in one shell function, so the function is extracted by name and
exercised against a stub. Nothing here reaches GitHub or Orc.

Extraction is by exact function name and it asserts that it found something, so
a rename produces a loud failure here rather than a silent loss of coverage.
"""

from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
WRAPPER = ROOT / "scripts/hermit-health-tick.sh"
FUNCTION = "escalate_live_report_findings"

STUB = """#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$STUB_CALLS"
echo "escalate: outcome=published"
exit 0
"""


def extract_function() -> str:
    text = WRAPPER.read_text()
    match = re.search(rf"^{FUNCTION}\(\) \{{$.*?^\}}$", text, re.MULTILINE | re.DOTALL)
    assert match, f"{FUNCTION} is no longer defined in {WRAPPER}; this bracket is stale"
    return match.group(0)


@pytest.fixture()
def bench(tmp_path: Path):
    function = extract_function()
    calls = tmp_path / "calls"
    invlog = tmp_path / "invlog"
    report = tmp_path / "report"
    stub = tmp_path / "escalate"
    stub.write_text(STUB)
    stub.chmod(0o755)

    def run(report_lines: str, *, escalate: Path | None = None, twice: bool = False) -> dict:
        report.write_text(report_lines)
        calls.write_text("")
        invlog.write_text("")
        preamble = "\n".join(
            [
                "set -uo pipefail",
                f'INVLOG={invlog}',
                f'LIVE_REPORT={report}',
                'LIVE_REPORT_STAMP=2026-09-03T00:00:00Z',
                'LIVE_REPORT_ESCALATED=0',
                'ESCALATION_KEY=operational-tick-findings',
                'ESCALATION_AGENT=health-tick',
                'ESCALATION_TEAM=hermit2',
                f'ESCALATE={escalate if escalate is not None else stub}',
            ]
        )
        body = f"{FUNCTION}\n" * (2 if twice else 1)
        completed = subprocess.run(
            ["bash", "-c", f"{preamble}\n{function}\n{body}"],
            capture_output=True,
            text=True,
            env={**os.environ, "STUB_CALLS": str(calls)},
            timeout=120,
        )
        return {
            "returncode": completed.returncode,
            "stderr": completed.stderr,
            "calls": [line for line in calls.read_text().splitlines() if line.strip()],
            "invlog": invlog.read_text(),
        }

    return run


def test_action_and_error_lines_are_findings_and_notes_are_not(bench) -> None:
    """A NOTE is the tick describing itself; an ACTION or ERROR is the tick
    reporting something. Counting notes would page on every clean cycle."""
    result = bench(
        "ACTION: hard-warn-coordinator component=a\n"
        "NOTE: ops-state enabled=true\n"
        "ERROR: reminder b: gate command could not run\n"
        "NOTE: emitted 1 instruction(s) this tick\n"
    )
    assert result["returncode"] == 0
    assert "findings=2" in result["invlog"]
    assert any("--title" in call for call in result["calls"])
    assert any("reports 2 finding(s)" in call for call in result["calls"])


def test_a_clean_tick_closes_what_a_dirty_one_opened(bench) -> None:
    """Otherwise last week's alarm sits open beside this week's and a reader
    cannot tell which is true now."""
    result = bench("NOTE: ops-state enabled=true\nNOTE: emitted 0 instruction(s) this tick\n")
    assert "findings=0" in result["invlog"]
    assert any("--resolve" in call for call in result["calls"])
    assert not any("--title" in call for call in result["calls"])


def test_the_normal_path_and_the_exit_trap_do_not_both_report(bench) -> None:
    """Both call this; the tick must not publish the same findings twice."""
    result = bench("ACTION: hard-warn-coordinator component=a\n", twice=True)
    assert len(result["calls"]) == 1, result["calls"]


def test_a_missing_escalate_does_not_fail_the_tick(bench, tmp_path: Path) -> None:
    result = bench(
        "ACTION: hard-warn-coordinator component=a\n", escalate=tmp_path / "absent"
    )
    assert result["returncode"] == 0
    assert result["calls"] == []


def test_the_publication_goes_through_the_proxy_wrapper() -> None:
    """A scheduled service inherits no proxy variables and this host has no
    direct external route. Measured 2026-09-03 from the real unit without it:
    `dial tcp 140.82.112.5:443: connect: network is unreachable`. The tool
    reported that honestly rather than claiming delivery, which is the design
    working -- but needing that refusal every hour is not."""
    function = extract_function()
    invocations = [
        line for line in function.splitlines() if "$ESCALATE" in line and "=" in line
    ]
    assert invocations, "the function no longer invokes $ESCALATE; this bracket is stale"
    for line in invocations:
        assert "with-proxy" in line, f"unproxied publication: {line.strip()}"


def test_the_outcome_is_recorded_whatever_it_was(bench) -> None:
    """A tick that could not publish must say so in the same durable record that
    carries the findings, not only on a stream nobody kept."""
    result = bench("ACTION: hard-warn-coordinator component=a\n")
    assert "escalation_rc=0" in result["invlog"]
    assert "detail=" in result["invlog"]
