"""Unit tests for the standing receipts x fresh-heads reconciliation.

The join is a PURE function with injectable certifier + floor check, so every
state (VALID / FLOOR-BLOCKED / NOT-CERTIFIED / ORPHANED) is bracketed offline
with no gh / validate-status / preflight subprocess. Both sides are exercised:
a qualifying commit lands in each positive bucket AND a disqualifying variant is
refused, so no classifier is silently inert.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import reconcile_receipts as rr  # noqa: E402

A = "a" * 40  # clean, matched, floor-clear   -> VALID
B = "b" * 40  # clean, matched, floor-blocked -> FLOOR-BLOCKED
C = "c" * 40  # matched but cert refuses       -> NOT-CERTIFIED
D = "d" * 40  # no open head                   -> ORPHANED


def _prs(*pairs):
    return [{"number": n, "headRefOid": oid, "isDraft": draft, "title": "t"}
            for n, oid, draft in pairs]


def _certify(validated_set):
    return lambda sha: sha in validated_set


def _floor(blocked_set):
    def f(sha, repo):
        if sha in blocked_set:
            return {"ok": False, "reason": f"REFUSE: {sha[:12]} predates floor"}
        return {"ok": True, "reason": ""}
    return f


def test_four_states_bracketed():
    open_prs = _prs((1, A, False), (2, B, True), (3, C, False))
    commits = [A, B, C, D]
    rep = rr.reconcile(
        open_prs, commits,
        certify=_certify({A, B}),        # C is NOT authoritatively validated
        floor=_floor({B}),               # B predates a floor
        repo="rrnewton/hermit")
    c = rep["counts"]
    assert rep["total_receipt_commits"] == 4
    assert c == {"valid": 1, "floor_blocked": 1, "not_certified": 1,
                 "orphaned": 1}
    assert rep["buckets"]["valid"][0]["commit"] == A
    assert rep["buckets"]["valid"][0]["prs"] == [1]
    assert rep["buckets"]["floor_blocked"][0]["commit"] == B
    assert rep["buckets"]["floor_blocked"][0]["drafts"] == [2]
    assert "predates floor" in rep["buckets"]["floor_blocked"][0]["reason"]
    assert rep["buckets"]["not_certified"][0]["commit"] == C
    assert rep["buckets"]["orphaned"][0]["commit"] == D


def test_certifier_gates_floor_check():
    # NOT-CERTIFIED must short-circuit BEFORE the floor check: a commit that
    # fails is_clean_full_pass is never labelled VALID even if it clears floors.
    calls = []

    def floor(sha, repo):
        calls.append(sha)
        return {"ok": True, "reason": ""}

    rep = rr.reconcile(_prs((9, C, False)), [C],
                       certify=lambda s: False, floor=floor,
                       repo="r")
    assert rep["counts"]["not_certified"] == 1
    assert rep["counts"]["valid"] == 0
    assert calls == []  # floor never consulted for an uncertified commit


def test_orphan_denominator_is_all_receipts():
    # All receipts orphaned when no open head matches -> orphaned N of N.
    commits = [A, B, C]
    rep = rr.reconcile([], commits, certify=lambda s: True,
                       floor=_floor(set()), repo="r")
    assert rep["counts"]["orphaned"] == 3
    assert rep["total_receipt_commits"] == 3
    assert rep["counts"]["valid"] == 0


def test_candidate_commits_filters_and_dedups():
    rows = [
        {"profile": "full", "result": "pass", "checks": 5, "commit": A,
         "finished_at": "2026-08-04T01:00:00Z"},
        {"profile": "full", "result": "pass", "checks": 5, "commit": A,
         "finished_at": "2026-08-04T09:00:00Z"},   # dup commit, newer
        {"profile": "full", "result": "pass", "checks": 5, "commit": B,
         "finished_at": "2026-08-04T05:00:00Z"},
        {"profile": "portable-only", "result": "pass", "checks": 2,
         "commit": C, "finished_at": "2026-08-04T06:00:00Z"},  # not full
        {"profile": "full", "result": "fail", "checks": 5, "commit": D,
         "finished_at": "2026-08-04T06:00:00Z"},               # not pass
        {"profile": "full", "result": "pass", "checks": 5, "commit": "unknown",
         "finished_at": "2026-08-04T06:00:00Z"},               # not 40-hex
    ]
    out = rr.candidate_commits(rows)
    assert out == [A, B]  # deduped, newest-first (A 09:00 > B 05:00)


def test_render_states_denominators():
    open_prs = _prs((1, A, False))
    rep = rr.reconcile(open_prs, [A, D], certify=lambda s: True,
                       floor=_floor(set()), repo="rrnewton/hermit")
    text = rr.render(rep)
    # ⚠️ ASSERT THE INVARIANT THIS TEST IS NAMED FOR, NOT AN INCIDENTAL LABEL.
    # It previously pinned the exact string "VALID (landable NOW, no new
    # validate): 1 of 2". The label was later reworded to "VALID (fixed floors;
    # final base check required)" -- an honest weakening of the CLAIM, since a
    # final base check is in fact required -- and this test went red. It went red
    # unnoticed because nothing ran it: `ci-hub/landing` is not in
    # `run_python_suites.py`'s registry.
    #
    # The invariant is the module's stated rule at reconcile_receipts.py:39,
    # "Every count is stated WITH ITS DENOMINATOR". That still holds. Pinning the
    # denominator rather than the prose keeps the guard and stops a rewording from
    # reading as a lost invariant.
    for state in ("VALID", "FLOOR-BLOCKED", "NOT-CERTIFIED", "ORPHANED"):
        line = next((ln for ln in text.splitlines()
                     if ln.strip().startswith(state) and " of " in ln), None)
        assert line is not None, f"{state} count is missing its denominator:\n{text}"
        assert "of 2" in line, f"{state} carries the wrong denominator: {line!r}"
    assert "50%" in text  # orphan ratio carries a denominator-derived pct
