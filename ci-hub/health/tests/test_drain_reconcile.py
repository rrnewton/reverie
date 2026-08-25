#!/usr/bin/env python3
"""The drain reconciler must fire on a real gap and stay quiet on a tracked PR.

A detector shown only to fire could be firing unconditionally; one shown only to
stay quiet could be inert. Neither half is evidence alone, so every case here is
bracketed both ways.

These live in ``ci-hub/health/tests`` deliberately: that path is one of
``run_python_suites.py``'s ``DEFAULT_SUITES``, and a guard placed outside those
directories is never executed by CI -- it would be a test that exists but does
not run, which is the same class of defect the reconciler itself detects.
"""

from __future__ import annotations

import importlib.util
import io
import json
import unittest
from unittest import mock
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
_SPEC = importlib.util.spec_from_file_location(
    "drain_reconcile", REPO_ROOT / "ci-hub" / "health" / "drain_reconcile.py"
)
dr = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(dr)


def _result(**over):
    base = {
        "tracker": "fixture", "tracker_status": "IN_PROGRESS", "tracker_healthy": True,
        "tracker_refs": 1, "open_examined": 1, "gaps": [], "unreachable": [],
    }
    base.update(over)
    return base


def _gap(number: int, age: float | None = 1.0):
    return {"repo": "hermit", "number": number, "url": "", "title": "t",
            "draft": False, "age_hours": age}


def _render(result, list_gaps=False):
    buf = io.StringIO()
    rc = dr.report(result, list_gaps=list_gaps, out=buf)
    return rc, buf.getvalue()


def test_parser_recovers_both_notations() -> None:
    """A tracker entry may be a full URL or the short form; missing either
    notation would invent gaps for PRs that ARE tracked."""
    refs = dr.tracked_refs(
        "landed https://github.com/rrnewton/hermit/pull/1840 plus reverie#362"
    )
    assert refs == {("hermit", 1840), ("reverie", 362)}, refs


def test_a_tracked_pr_is_not_flagged_and_an_untracked_one_is() -> None:
    """Both directions on the same fixture, so the comparison is like-for-like."""
    refs = dr.tracked_refs("https://github.com/rrnewton/hermit/pull/100")
    assert ("hermit", 100) in refs, "NEGATIVE: a tracked PR must not become a gap"
    assert ("hermit", 101) not in refs, "POSITIVE: an untracked PR must be a gap"


def test_a_closed_tracker_yields_one_structural_finding_not_n() -> None:
    """The report must not restate a single fault once per PR.

    133 findings on the first run is how a reconciler gets switched off, and the
    one actionable fact -- the tracker is closed -- would be buried in them.
    """
    rc, text = _render(_result(
        tracker_status="CLOSED", tracker_healthy=False,
        gaps=[_gap(n) for n in range(133)], open_examined=133,
    ))
    assert rc == 3, rc
    assert "STRUCTURAL" in text
    assert "hermit#0" not in text, "per-PR findings leaked past the short-circuit"
    assert text.count("hermit#") == 0


def test_a_healthy_tracker_with_gaps_reports_them() -> None:
    """The structural short-circuit must not swallow real gaps."""
    rc, text = _render(_result(gaps=[_gap(7)], open_examined=2))
    assert rc == 1, rc
    assert "hermit#7" in text
    assert "STRUCTURAL" not in text


def test_no_gaps_is_a_clean_zero() -> None:
    rc, text = _render(_result())
    assert rc == 0, rc
    assert "OK:" in text


def test_an_unreadable_repo_is_unavailable_not_clean() -> None:
    """A blind run must never be reported as 'everything is tracked'.

    Zero gaps because a query failed is indistinguishable from zero gaps because
    the backlog is clean, unless the tool says so and exits non-zero.
    """
    rc, text = _render(_result(unreachable=["hermit: exit 1 from gh"]))
    assert rc == 2, rc
    assert "UNAVAILABLE" in text
    assert "REFUSED" in text


def test_unknown_age_sorts_last_never_first() -> None:
    """A missing timestamp must not masquerade as the oldest and jump the queue."""
    ordered = sorted([_gap(1, None), _gap(2, 5.0), _gap(3, 100.0)], key=dr._age_sort_key)
    assert [g["number"] for g in ordered] == [3, 2, 1], ordered


def test_gap_list_truncation_announces_itself() -> None:
    """A capped list that does not say it was capped reads as full coverage."""
    rc, text = _render(_result(gaps=[_gap(n) for n in range(50)], open_examined=50))
    assert rc == 1
    assert "more suppressed" in text
    rc, full = _render(_result(gaps=[_gap(n) for n in range(50)], open_examined=50),
                       list_gaps=True)
    assert "more suppressed" not in full
    assert full.count("hermit#") == 50


def test_terminal_status_set_is_not_inert() -> None:
    """Positive control on the health predicate itself.

    If NONTERMINAL ever grew to include a terminal status, every structural
    check above would pass vacuously.
    """
    assert "CLOSED" not in dr.NONTERMINAL
    assert "IN_PROGRESS" in dr.NONTERMINAL
    assert dr.NONTERMINAL, "an empty set would make every tracker look unhealthy"


# ---------------------------------------------------------------------------
# ⚠️ THE CAP ON open_prs() MUST REFUSE, NOT UNDER-REPORT.
#
# `--limit 500` is an explicit cap, and the comment beside it records why the
# default 30 was rejected: it "would under-report the gap as a clean-looking
# number". 500 fails the same way, just later. `returned == limit` is the only
# signal a cap was hit; nothing compared them until provenance.query_provenance
# was wired in, so at 500 open pull requests this reported a smaller drain gap
# with no error and no warning.
#
# This test fails BY NAME if that call is removed: without it a full page parses
# cleanly and open_prs returns 500 rows instead of raising.
class OpenPrCapRefuses(unittest.TestCase):
    def _load(self):
        import importlib.util
        import sys
        from pathlib import Path

        root = Path(__file__).resolve().parents[3]
        spec = importlib.util.spec_from_file_location(
            "drain_reconcile_cap", root / "ci-hub" / "health" / "drain_reconcile.py"
        )
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        self.addCleanup(sys.modules.pop, spec.name, None)
        spec.loader.exec_module(module)
        return module

    def test_a_full_page_is_refused_as_truncated_rather_than_reported(self):
        module = self._load()
        full_page = json.dumps(
            [{"number": n, "title": "t", "url": "u", "createdAt": None, "isDraft": False}
             for n in range(module.OPEN_PR_LIMIT)]
        )
        with mock.patch.object(module, "_run", return_value=full_page):
            with self.assertRaises(Exception) as caught:
                module.open_prs("rrnewton", "hermit", 30)
        message = str(caught.exception)
        self.assertIn("TRUNCATED", message,
                      f"a full page must be refused as truncated, got: {message}")
        self.assertIn("conclusion FROM ABSENCE is INVALID", message)

    def test_a_short_page_is_returned_normally(self):
        # The control. A guard that refuses everything is not a guard.
        module = self._load()
        short = json.dumps(
            [{"number": 1, "title": "t", "url": "u", "createdAt": None, "isDraft": False}]
        )
        with mock.patch.object(module, "_run", return_value=short):
            rows = module.open_prs("rrnewton", "hermit", 30)
        self.assertEqual(len(rows), 1)
