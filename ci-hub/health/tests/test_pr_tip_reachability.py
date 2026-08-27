#!/usr/bin/env python3
"""Offline tests for pr_tip_reachability.

⚠️ EVERY TEST HERE IS OFFLINE. The gate's live probes hit the network, and a test
that needs the network is a test that will be disabled the first time a runner is
offline. What is tested is the pure logic: that a failed query does not render as
a clean zero, that a nonzero count carries its lower-bound qualifier, and that the
reachability rule requires BOTH routes to fail.

The one thing that cannot be tested offline -- that the probe can actually report
an unreachable tip against a real remote -- is what `--self-test` covers, and it
is deliberately not duplicated here as a mock. A mock of "the remote says no"
proves the mock says no.
"""

from __future__ import annotations

import contextlib
import io
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import pr_tip_reachability as m  # noqa: E402


def test_indeterminate_never_renders_as_zero() -> None:
    """A failed enumeration is not 'no unreachable tips'.

    This is the fail-open the gate exists to remove, so it is the first assertion.
    """
    text = m.summarize({"indeterminate": True, "error": "could not enumerate"})
    assert "INDETERMINATE" in text, text
    assert "0 of" not in text, text


def test_nonzero_count_carries_its_qualifier() -> None:
    """A count of unreachable tips must never be reported as complete.

    Tips already garbage-collected cannot be counted, so the number is a lower
    bound. A consumer quoting it without that word makes a stronger claim than the
    data supports.
    """
    text = m.summarize(
        {
            "swept": 27,
            "unreachable": 2,
            "unreachable_prs": [2611, 2589],
            "reachable_via_pull_ref_only": 0,
            "pull_ref_only_prs": [],
        }
    )
    assert "lower bound" in text, text
    assert "#2611" in text and "#2589" in text, text


def test_zero_is_reported_plainly() -> None:
    text = m.summarize(
        {
            "swept": 27,
            "unreachable": 0,
            "unreachable_prs": [],
            "reachable_via_pull_ref_only": 0,
            "pull_ref_only_prs": [],
        }
    )
    assert text.startswith("0 of 27"), text


def test_pull_ref_only_is_a_note_not_an_alarm() -> None:
    """A deleted branch whose object is still served is recoverable, not an incident.

    It must appear in the summary -- it is the state that PRECEDES unreachability
    and is the only warning anyone gets -- but it must not read as the alarm.
    """
    text = m.summarize(
        {
            "swept": 27,
            "unreachable": 0,
            "unreachable_prs": [],
            "reachable_via_pull_ref_only": 1,
            "pull_ref_only_prs": [2611],
        }
    )
    assert text.startswith("0 of 27"), text
    assert "#2611" in text and "recoverable" in text, text


def test_gate_exit_codes_are_three_valued() -> None:
    """clear / found / could-not-determine, never collapsed to two.

    Driven through main() with a stubbed sweep so no network is touched.
    """
    original = m.sweep
    try:
        m.sweep = lambda repo: {
            "swept": 1, "unreachable": 0, "unreachable_prs": [],
            "reachable_via_pull_ref_only": 0, "pull_ref_only_prs": [],
        }
        assert m.main(["--gate"]) == 0

        m.sweep = lambda repo: {
            "swept": 1, "unreachable": 1, "unreachable_prs": [7],
            "reachable_via_pull_ref_only": 0, "pull_ref_only_prs": [],
        }
        assert m.main(["--gate"]) == 1

        m.sweep = lambda repo: {"indeterminate": True, "error": "no gh"}
        assert m.main(["--gate"]) == 2, "an indeterminate must not exit 0"
    finally:
        m.sweep = original


def test_main_emits_the_tick_summary_key_on_both_result_arms() -> None:
    """Exercise the production print, not a copy of it in the self-test."""
    original = m.sweep
    try:
        for report, expected in (
            ({"swept": 3, "unreachable": 1, "unreachable_prs": [7],
              "reachable_via_pull_ref_only": 0, "pull_ref_only_prs": []}, "#7"),
            ({"swept": 3, "unreachable": 0, "unreachable_prs": [],
              "reachable_via_pull_ref_only": 0, "pull_ref_only_prs": []}, "0 of 3"),
        ):
            m.sweep = lambda _repo, value=report: value
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                assert m.main(["--repo", "x"]) == 0
            summary_lines = [
                line for line in output.getvalue().splitlines()
                if line.startswith("summary=")
            ]
            assert len(summary_lines) == 1, output.getvalue()
            assert expected in summary_lines[0], summary_lines[0]
    finally:
        m.sweep = original


def main() -> int:
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    failed = 0
    for t in tests:
        try:
            t()
        except AssertionError as exc:
            print(f"FAIL: {t.__name__}: {exc}", file=sys.stderr)
            failed += 1
    if failed:
        print(f"test_pr_tip_reachability: {failed} of {len(tests)} failed", file=sys.stderr)
        return 1
    print(f"PASS: test_pr_tip_reachability, {len(tests)} tests")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
