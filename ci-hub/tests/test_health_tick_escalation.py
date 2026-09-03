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


def extract_settings() -> list[str]:
    """The wrapper's OWN escalation settings, not a copy of them.

    A fixture that redeclares these would test the fixture: the sink list and the
    database name could be changed in the wrapper and every assertion here would
    still pass. Taking the real assignment lines is what makes a mutation of the
    wrapper show up as a failure.
    """
    text = WRAPPER.read_text()
    wanted = ("ESCALATION_SINKS=", "ESCALATION_TG_DB=", "ESCALATION_KEY=")
    found = [
        line
        for line in text.splitlines()
        if any(line.startswith(name) for name in wanted)
    ]
    assert len(found) == len(wanted), (
        f"expected one assignment for each of {wanted} in {WRAPPER}; got {found}"
    )
    return found


@pytest.fixture()
def bench(tmp_path: Path):
    function = extract_function()
    calls = tmp_path / "calls"
    invlog = tmp_path / "invlog"
    report = tmp_path / "report"
    stub = tmp_path / "escalate"
    stub.write_text(STUB)
    stub.chmod(0o755)

    def run(
        report_lines: str,
        *,
        escalate: Path | None = None,
        twice: bool = False,
        complete: bool = True,
    ) -> dict:
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
                f'TICK_REPORT_COMPLETE={1 if complete else 0}',
                'ESCALATION_AGENT=health-tick',
                'ESCALATION_TEAM=hermit2',
                *extract_settings(),
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


def test_an_interrupted_tick_never_retracts_an_alarm(bench) -> None:
    """The one that bit. Measured 2026-09-03T19:49:45Z on the live unit: a tick
    killed at its 300s bound had produced no findings yet, the resolve path read
    that as "nothing is wrong", and it CLOSED a live alarm as no longer reported.
    Absence is not a clean report, and withdrawing a true warning because the
    check was stopped is worse than saying nothing at all."""
    result = bench("", complete=False)
    assert result["returncode"] == 0
    assert result["calls"] == [], result["calls"]
    assert "escalation_skipped=incomplete-report" in result["invlog"]


def test_an_interrupted_tick_still_publishes_what_it_did_find(bench) -> None:
    """Only the RETRACTION needs a complete report. Findings a killed tick
    already reached are still real and still worth publishing."""
    result = bench("ACTION: hard-warn-coordinator component=a\n", complete=False)
    assert any("--title" in call for call in result["calls"]), result["calls"]
    assert not any("--resolve" in call for call in result["calls"])


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


def test_per_run_provenance_is_excluded_from_change_detection(bench) -> None:
    """health-tick stamps every line with when it was observed and which commit
    produced it. Left in, a condition identical for a week reads as new every
    cycle and the sink becomes hourly noise -- the same "nobody hears it" defect
    arriving from the other side."""
    function = extract_function()
    assert "--digest-ignore 'observed_at=" in function
    assert "--digest-ignore 'input_sha=" in function


def test_both_sinks_are_named_on_every_call(bench) -> None:
    """They cover different outages: the fork survives the coordinator being
    absent, the queue gets the alarm dispatched when it is present and not
    looking. Dropping either leaves one outage uncovered while looking complete."""
    result = bench("ACTION: hard-warn-coordinator component=a\n")
    assert result["calls"], "nothing was published"
    for call in result["calls"]:
        assert "--sink github" in call, call
        assert "--sink taskgraph" in call, call


def test_the_queue_database_is_named_and_not_inherited(bench) -> None:
    """Measured from this unit: a scheduled service resolves a DIFFERENT database
    from the one coordinators read, so an unnamed row is an invisible alarm."""
    assert "--tg-db" in extract_function()
    result = bench("ACTION: hard-warn-coordinator component=a\n")
    assert all("--tg-db hermit2" in call for call in result["calls"]), result["calls"]


def test_a_clean_tick_resolves_in_both_sinks(bench) -> None:
    result = bench("NOTE: nothing to report\n")
    assert result["calls"]
    for call in result["calls"]:
        assert "--resolve" in call
        assert "--sink github" in call and "--sink taskgraph" in call


def test_the_outcome_is_recorded_whatever_it_was(bench) -> None:
    """A tick that could not publish must say so in the same durable record that
    carries the findings, not only on a stream nobody kept."""
    result = bench("ACTION: hard-warn-coordinator component=a\n")
    assert "escalation_rc=0" in result["invlog"]
    assert "detail=" in result["invlog"]
