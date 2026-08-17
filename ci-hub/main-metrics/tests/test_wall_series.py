#!/usr/bin/env python3
"""Both-direction tests for the main-branch wall series.

The alarm this builds is only worth having if it can be WRONG. So every
"regression detected" case is paired with a case that must NOT fire, and the
refusal path (unconditioned inputs) is tested as a first-class outcome rather
than as an error branch.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import wall_series as ws  # noqa: E402


def pt(wall, cpu, conc, commit="c", started="2026-08-07T00:00:00Z"):
    return ws.Point(commit=commit, started_at=started, wall=wall, cpu=cpu,
                    ratio=round(cpu / wall, 3) if wall else None,
                    concurrency=conc, bucket=ws.bucket_of(conc),
                    dag_jobs=None, cache_state=None, peak_memory_kb=None)


def ledger_row(commit, finished, *, executed=1):
    return {
        "commit": commit,
        "profile": "full",
        "result": "pass",
        "executed_tests": executed,
        "gates_run": 1,
        "gates_expected": 1,
        "finished_at": finished,
        "started_at": finished,
        "real_seconds": 10,
        "user_seconds": 5,
        "sys_seconds": 5,
    }


# ---------------------------------------------------------------- buckets

def test_bucket_boundaries_follow_the_measured_knee():
    assert ws.bucket_of(0) == "0-3"
    assert ws.bucket_of(3) == "0-3"
    assert ws.bucket_of(4) == "4-6"
    assert ws.bucket_of(6) == "4-6"      # knee: budget holds up to here
    assert ws.bucket_of(7) == "7-9"      # and breaks above it
    assert ws.bucket_of(14) == "14+"
    assert ws.bucket_of(999) == "14+"


def test_missing_concurrency_is_unknown_not_zero():
    """The majority bucket. Calling it 0 would silently claim an idle box."""
    assert ws.bucket_of(None) == ws.UNKNOWN
    assert pt(500, 1400, None).conditioned is False
    assert pt(500, 1400, 2).conditioned is True


# ------------------------------------------------- regression, both ways

def test_a_real_regression_fires():
    base = [pt(500, 1400, 2) for _ in range(5)]
    cand = [pt(900, 2500, 2) for _ in range(5)]
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "REGRESSION"
    assert v["delta_fraction"] > 0.20


def test_a_flat_series_does_NOT_fire():
    """The control. An alarm that always fires passes the test above."""
    base = [pt(500, 1400, 2) for _ in range(5)]
    cand = [pt(510, 1430, 2) for _ in range(5)]
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "OK", v


def test_an_improvement_does_NOT_fire():
    base = [pt(900, 2500, 2) for _ in range(5)]
    cand = [pt(500, 1400, 2) for _ in range(5)]
    assert ws.compare(base, cand, threshold=0.20)["verdict"] == "OK"


def test_just_under_threshold_does_not_fire_and_just_over_does():
    base = [pt(500, 1400, 2)]
    assert ws.compare(base, [pt(599, 1650, 2)], threshold=0.20)["verdict"] == "OK"
    assert ws.compare(base, [pt(601, 1660, 2)], threshold=0.20)["verdict"] == "REGRESSION"


# --------------------------------------- the ratio separates the causes

def test_contention_is_named_when_cpu_stays_flat():
    base = [pt(500, 1450, 2)]          # ratio 2.9
    cand = [pt(900, 1450, 2)]          # wall up, CPU identical -> we WAITED
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "REGRESSION"
    assert "CONTENTION" in v["cause"]


def test_more_work_is_named_when_cpu_rises_with_wall():
    base = [pt(500, 1450, 2)]          # ratio 2.9
    cand = [pt(900, 2900, 2)]          # ratio 3.2 -> we DID MORE
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "REGRESSION"
    assert "MORE WORK" in v["cause"]


# ------------------------------------------------------- the refusal

def test_unconditioned_inputs_REFUSE_rather_than_guess():
    """A verdict that cannot say what both sides ran under is not a verdict."""
    base = [pt(500, 1400, None) for _ in range(5)]
    cand = [pt(900, 2500, None) for _ in range(5)]
    v = ws.compare(base, cand, threshold=0.20)
    assert v["verdict"] == "INSUFFICIENT"
    assert v["baseline_n"] == 0 and v["candidate_n"] == 0


def test_the_refusal_can_be_overridden_explicitly():
    base = [pt(500, 1400, None) for _ in range(3)]
    cand = [pt(900, 2500, None) for _ in range(3)]
    v = ws.compare(base, cand, threshold=0.20, require_conditioned=False)
    assert v["verdict"] == "REGRESSION"


def test_one_sided_conditioning_still_refuses():
    base = [pt(500, 1400, 2)]
    cand = [pt(900, 2500, None)]
    assert ws.compare(base, cand, threshold=0.20)["verdict"] == "INSUFFICIENT"


# ------------------------------------------------------- summary shape

def test_summary_reports_unconditioned_count_and_missing_memory():
    pts = [pt(500, 1400, 2), pt(700, 1900, None), pt(900, 2500, 14)]
    s = ws.summarize(pts)
    assert s["n"] == 3
    assert s["conditioned"] == 2 and s["unconditioned"] == 1
    assert s["peak_memory_available"] is False
    assert s["over_budget"] == 2          # 700 and 900 exceed 600
    assert set(s["by_concurrency_bucket"]) == {"0-3", "14+", ws.UNKNOWN}


def test_gate_breakdown_attributes_to_a_node_and_ranks_by_median():
    p = pt(500, 1400, 2)
    p.gates = [{"name": "slow.gate", "real_seconds": 300},
               {"name": "fast.gate", "real_seconds": 5}]
    q = pt(520, 1450, 2)
    q.gates = [{"name": "slow.gate", "real_seconds": 320},
               {"name": "fast.gate", "real_seconds": 7}]
    out = ws.gate_breakdown([p, q])
    assert out[0]["gate"] == "slow.gate"
    assert out[0]["n"] == 2
    assert out[0]["median_seconds"] == 310


def test_gate_breakdown_ignores_gates_without_a_duration():
    p = pt(500, 1400, 2)
    p.gates = [{"name": "no-timing"}, {"name": "timed", "real_seconds": 9}]
    out = ws.gate_breakdown([p])
    assert [g["gate"] for g in out] == ["timed"]


# ------------------------------------------------------- loader hygiene

def test_loader_qualifies_rows_and_orders_by_finished_at(tmp_path):
    f = tmp_path / "l.jsonl"
    late = ledger_row("late", "2026-08-07T02:00:00Z")
    early = ledger_row("early", "2026-08-07T01:00:00Z")
    empty = ledger_row("zero-executed", "2026-08-07T00:00:00Z", executed=0)
    f.write_text(
        json.dumps(late) + "\n\nnot json\n[\"a list\"]\n"
        + json.dumps(empty) + "\n" + json.dumps(early) + "\n",
        encoding="utf-8",
    )
    rows = ws.load_rows(f)
    assert [row["commit"] for row in rows] == ["early", "late"]


def test_rows_without_wall_are_dropped_not_zeroed():
    rows = [{"commit": "c", "real_seconds": 0, "user_seconds": 1, "sys_seconds": 1},
            {"commit": "c", "user_seconds": 1, "sys_seconds": 1},
            {"commit": "c", "real_seconds": 100, "user_seconds": 200, "sys_seconds": 90}]
    pts = ws.to_points(rows, None)
    assert len(pts) == 1
    assert pts[0].wall == 100 and pts[0].ratio == 2.9


def test_only_main_filter_drops_non_ancestors():
    rows = [{"commit": "onmain", "real_seconds": 10, "user_seconds": 5, "sys_seconds": 5},
            {"commit": "offmain", "real_seconds": 10, "user_seconds": 5, "sys_seconds": 5}]
    pts = ws.to_points(rows, {"onmain"})
    assert [p.commit for p in pts] == ["onmain"]


def test_main_commits_admits_on_main_and_refuses_off_main(monkeypatch):
    onmain = "1" * 40
    offmain = "2" * 40
    calls = []

    def run(argv, **kwargs):
        calls.append((argv, kwargs))
        if "rev-list" in argv:
            return subprocess.CompletedProcess(
                argv,
                0,
                stdout=onmain + "\n",
                stderr="",
            )
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout="commit\n",
            stderr="",
        )

    monkeypatch.setattr(ws.subprocess, "run", run)

    found = ws.main_commits(
        Path("/repo"),
        {onmain, offmain, ""},
    )

    assert found == {onmain}
    assert len(calls) == 2


def test_main_commits_refuses_malformed_commit_without_running_git(monkeypatch):
    def run(*_args, **_kwargs):
        raise AssertionError("malformed commit must refuse before invoking git")

    monkeypatch.setattr(ws.subprocess, "run", run)

    with pytest.raises(RuntimeError, match="refused malformed commit ID"):
        ws.main_commits(Path("/repo"), {"not-a-commit"})


def test_main_commits_uses_two_subprocesses_for_the_whole_population(monkeypatch):
    calls = []
    commits = {f"{index:040x}" for index in range(1_000)}
    onmain = {f"{0:040x}", f"{999:040x}"}
    offmain = commits - onmain

    def run(argv, **kwargs):
        calls.append((argv, kwargs))
        if "rev-list" in argv:
            return subprocess.CompletedProcess(
                argv,
                0,
                stdout="\n".join(sorted(onmain)) + "\n",
                stderr="",
            )
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout="commit\n" * len(offmain),
            stderr="",
        )

    monkeypatch.setattr(ws.subprocess, "run", run)

    found = ws.main_commits(Path("/repo"), commits)

    assert found == onmain
    assert len(calls) == 2
    assert calls[0][0] == [
        "git", "-C", "/repo", "rev-list", "origin/main",
    ]
    assert calls[1][0] == [
        "git", "-C", "/repo", "cat-file", "--batch-check=%(objecttype)",
    ]
    assert set(calls[1][1]["input"].splitlines()) == offmain


def test_main_commits_refuses_when_the_ancestry_query_fails(monkeypatch):
    def run(argv, **kwargs):
        return subprocess.CompletedProcess(
            argv,
            128,
            stdout="",
            stderr="fatal: bad revision",
        )

    monkeypatch.setattr(ws.subprocess, "run", run)

    claimed_main = "1" * 40
    with pytest.raises(RuntimeError, match="rev-list exited 128"):
        ws.main_commits(Path("/repo"), {claimed_main})


def test_main_commits_refuses_whole_population_when_one_object_is_absent(monkeypatch):
    claimed_main = "1" * 40
    absent = "f" * 40

    def run(argv, **kwargs):
        if "rev-list" in argv:
            return subprocess.CompletedProcess(
                argv,
                0,
                stdout=claimed_main + "\n",
                stderr="",
            )
        return subprocess.CompletedProcess(
            argv,
            0,
            stdout=absent + " missing\n",
            stderr="",
        )

    monkeypatch.setattr(ws.subprocess, "run", run)

    with pytest.raises(RuntimeError, match="refused 1 unavailable"):
        ws.main_commits(Path("/repo"), {claimed_main, absent})


@pytest.mark.parametrize("option", ["--gate-regression", "--readiness"])
def test_both_gate_commands_emit_refusal_when_ancestry_refuses(monkeypatch, capsys,
                                                               option):
    def build(*_args, **_kwargs):
        raise RuntimeError("main ancestry check refused 1 unavailable object")

    monkeypatch.setattr(ws, "build", build)

    assert ws.main([option]) == 1
    output = capsys.readouterr().out
    assert "state=refused" in output
    assert "summary=REFUSED: main ancestry check refused" in output


# ------------------------------------------------- readiness / wireable gate

def _rep(n_cond, n_total, span_days):
    from datetime import datetime, timedelta
    a = datetime(2026, 5, 1)
    b = a + timedelta(days=span_days)
    return {"summary": {"n": n_total, "conditioned": n_cond,
                        "span": [a.isoformat() + "Z", b.isoformat() + "Z"]}}


def test_readiness_needs_BOTH_retention_and_conditioning():
    """Raising either alone fails -- 90 days of unqualified walls is still a refusal."""
    only_days = ws.series_readiness(_rep(15, 100, 120), 90, 0.9)
    assert only_days["ready"] is False
    assert any("conditioning" in s for s in only_days["shortfalls"])
    only_cond = ws.series_readiness(_rep(99, 100, 4), 90, 0.9)
    assert only_cond["ready"] is False
    assert any("retention" in s for s in only_cond["shortfalls"])
    neither = ws.series_readiness(_rep(4, 26, 4), 90, 0.9)
    assert len(neither["shortfalls"]) == 2


def test_readiness_passes_when_both_are_met():
    r = ws.series_readiness(_rep(95, 100, 120), 90, 0.9)
    assert r["ready"] is True and r["shortfalls"] == []


def test_todays_real_shape_is_not_ready_and_names_both_gaps():
    r = ws.series_readiness(_rep(4, 26, 3.91), 90, 0.9)
    assert r["ready"] is False
    assert r["conditioned_frac"] < 0.2
    assert len(r["shortfalls"]) == 2
