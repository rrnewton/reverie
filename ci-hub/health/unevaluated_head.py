#!/usr/bin/env python3
"""Detect PR heads that NO workflow ever evaluated.

WHY THIS EXISTS. Measured 2026-08-10 on rrnewton/hermit: 6 of the 25 most
recently updated open PRs (24%) had **zero** workflow runs at their current
head, and every one of them was CONFLICTING. GitHub cannot compute
``refs/pull/N/merge`` for a PR that conflicts with its base, and ``pull_request``
workflows check out that merge commit, so **no run is created at all**. The head
is silently never evaluated.

THE DANGER IS NOT THE ABSENCE, IT IS THAT THE ABSENCE LOOKS LIKE HEALTH. With no
runs there are no failing checks, so every surface reads clean: no red X, no
"checks pending", nothing. "Evaluated and clean" and "never evaluated" are
indistinguishable from the outside, and only the second is dangerous.

IT COMPOUNDS, AND NOT INDEPENDENTLY. ``merge-gate.yml``'s
``invalidate-local-validation`` job -- the thing that strips a ``passed-review-*``
label when the head moves -- is ITSELF a ``pull_request`` job. So the same
conflict that prevents evaluation also prevents invalidation. One cause disables
both guards, which is why a PR can end up APPROVED, UNEVALUATED, and green-looking
at the same time. Measured live on #1864.

WHAT THIS REPORTS. SIX states, and only ONE is good news:

  EVALUATED_PASS        every run at the exact head concluded successfully
  EVALUATED_FAIL        a real bad answer, including a cancel that killed
                        RUNNING jobs (a hang or an external kill IS a result)
  PENDING               a run exists and has not concluded; carries `stale`
  SUPERSEDED            cancelled AND a successor run is VISIBLE -- the only
                        case where supersession is asserted as fact
  CANCELLED_UNVERIFIED  cancelled with no verdict and NO successor evidence.
                        A hole whose CAUSE IS NOT ESTABLISHED.
  NEVER_RAN             no run object exists at all

A failing gate is a RESULT. No gate at all is not a result, and collapsing the
two is the defect. Compare `slot_disk_residue.py` / `holder_liveness.py`, which
draw the same distinction for slots.

Runs are matched on the EXACT head SHA. A run at an older head is evidence about
that older head and nothing else -- the same binding rule the approval labels
fail to apply.

Usage:
  unevaluated_head.py --repo rrnewton/hermit [--limit N] [--gate] [--json]
  unevaluated_head.py --from-json FIXTURE [--gate]   # offline, for the bracket

--gate exits 1 when any inspected head carries NO VERDICT -- NEVER_RAN,
SUPERSEDED, CANCELLED_UNVERIFIED, or a PENDING past the stale window. It is
deliberately silent about EVALUATED_FAIL (a red gate is another check's job) and
about a FRESH pending (alarming on every push is an alarm no coordinator can
satisfy).

A CAUSE IS ONLY EVER STATED AS FACT WHEN IT WAS OBSERVED. `SUPERSEDED` requires
a visible successor run; everything else cancelled is `CANCELLED_UNVERIFIED`
with the cause explicitly not established. An earlier version inferred
supersession from zero jobs OR from a FAILED jobs query, which turned missing
data into a causal claim -- the same absence-as-verdict defect one layer down,
in the CAUSE rather than the STATE.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import time
import sys
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from typing import Any

# FIVE STATES, AND ONLY ONE IS GOOD NEWS. Three was not enough, and the two it
# was missing were both being reported as the good one or as the wrong bad one:
#
#   NEVER_RAN       no run object exists at all -- the head was never evaluated
#   SUPERSEDED      a run was created and cancelled BEFORE ANY JOB STARTED,
#                   i.e. a newer push took the concurrency group. A HOLE, not a
#                   verdict. Superseding an obsolete run is CORRECT behaviour;
#                   only rendering it as an absent verdict was wrong.
#   PENDING         a run exists and has not concluded. Carries `stale` once it
#                   outlives the fresh window.
#   EVALUATED_FAIL  a real bad answer, including a cancel that killed RUNNING
#                   jobs (a hang or an external kill is a result, not a hole)
#   EVALUATED_PASS  every run at this head concluded successfully
#
# MEASURED DEFECT THIS REPLACES, in this very file: a run that was still QUEUED
# or IN PROGRESS has no failing conclusion, so the old three-state classifier
# fell through and returned EVALUATED_PASS. The detector built to stop "absence
# reads as success" was reporting an unfinished run as passed. And every
# `cancelled` was bucketed as EVALUATED_FAIL, which conflated supersession (a
# hole) with a genuine failure.
#
# THE cancelled SPLIT IS EVIDENCE-BASED, not a guess. Measured 2026-08-10:
#   dev-hermit 31349210392  cancelled, total_jobs=0   -> superseded; the newer
#                           push that took the group is visible one second
#                           before the cancellation
#   hermit     31352719284  cancelled, total_jobs=36 with jobs started and
#                           succeeded -> NOT supersession; its workflow is
#                           cancel-in-progress:false, so this was an external
#                           kill of an in-progress run
# So: cancelled with zero jobs is a hole; cancelled with jobs is a bad answer.
#
# The fresh/stale split follows github_main_health.classify_current_runs and is
# deliberate: alarming on every pending run fires after every push and is an
# alarm no coordinator can satisfy -- the anti-pattern already removed from
# other gates. A fresh pending is silent; only one that outlives the window is
# a finding.
NEVER_RAN = "NEVER_RAN"
NOT_EVALUATED = NEVER_RAN  # backward-compatible alias for existing consumers
SUPERSEDED = "SUPERSEDED"            # cancelled AND a successor is visible
CANCELLED_UNVERIFIED = "CANCELLED_UNVERIFIED"  # cancelled, cause NOT established
PENDING = "PENDING"
EVALUATED_PASS = "EVALUATED_PASS"
EVALUATED_FAIL = "EVALUATED_FAIL"

DEFAULT_PENDING_STALE_SECS = 900

# Conclusions that are a genuine negative VERDICT. `cancelled` is deliberately
# NOT here: it is split by whether any job ran (see above).
FAILING = {"failure", "timed_out", "startup_failure", "action_required"}


def _resolve(fn, run):
    """Call an optional resolver, returning None on ANY failure.

    None means UNKNOWN and must never be promoted into a causal claim.
    """
    if fn is None:
        return None
    try:
        return fn(run)
    except Exception:
        return None


def _age_secs(run: dict[str, Any], now: float | None) -> float | None:
    created = (run.get("created_at") or "").replace("Z", "+00:00")
    if not created or now is None:
        return None
    try:
        return now - datetime.fromisoformat(created).timestamp()
    except ValueError:
        return None


def classify(
    runs: list[dict[str, Any]],
    now: float | None = None,
    jobs_of=None,
    successor_of=None,
    stale_after: int = DEFAULT_PENDING_STALE_SECS,
) -> tuple[str, bool, dict[str, Any]]:
    """Pure classifier: the seam the bracket drives. Returns (state, stale).

    `jobs_of(run) -> int` resolves how many jobs a run created; it is only
    consulted for `cancelled` runs, so the cost is bounded and rare. When it is
    unavailable the cancel is treated as SUPERSEDED-unknown rather than silently
    promoted to a verdict -- refusing to answer beats guessing a good answer.
    """
    if not runs:
        return NEVER_RAN, False, {}

    for run in runs:
        if (run.get("conclusion") or "").strip() in FAILING:
            return EVALUATED_FAIL, False, {}

    pending = [r for r in runs if not (r.get("conclusion") or "").strip()]
    if pending:
        oldest = max((_age_secs(r, now) or 0.0) for r in pending)
        return PENDING, oldest > stale_after, {}

    for run in runs:
        if (run.get("conclusion") or "").strip() != "cancelled":
            continue
        # A cancel that killed RUNNING jobs is a RESULT (a hang, or an external
        # kill), not a hole. Measured: hermit 31352719284, cancelled with 36 jobs
        # started, under a cancel-in-progress:false workflow.
        jobs = _resolve(jobs_of, run)
        if jobs is not None and jobs > 0:
            return EVALUATED_FAIL, False, {"jobs": jobs}

        # *** SUPERSESSION IS ASSERTED ONLY ON VISIBLE SUCCESSOR EVIDENCE. ***
        # The previous version returned SUPERSEDED whenever jobs==0 OR the jobs
        # query FAILED, while the reason text claimed "superseded by a newer
        # push". That reintroduced absence-as-verdict one layer down: it moved
        # the defect from the STATE to the CAUSE. Two concrete false positives it
        # admitted -- zero jobs also covers a MANUAL or PRE-START cancellation,
        # and a FAILED QUERY became a causal claim about the world.
        successor = _resolve(successor_of, run)
        if successor:
            return SUPERSEDED, False, {"successor": successor, "jobs": jobs}
        return CANCELLED_UNVERIFIED, False, {
            "jobs": jobs,
            "why_unverified": ("job data unavailable" if jobs is None
                               else "zero jobs, but no successor run is visible"),
        }

    return EVALUATED_PASS, False, {}


def likely_reason(pr: dict[str, Any], state: str, stale: bool,
                  ev: dict[str, Any] | None = None) -> str:
    """Named state -> cause -> remedy, with the CAUSE GRADED BY EVIDENCE.

    Shape copied from github_main_health's `stale-pending` emitter. The grading
    is the correction: a cause may only be stated as fact when it was observed.
    """
    ev = ev or {}
    if state == NEVER_RAN:
        if (pr.get("mergeable") or "").upper() == "CONFLICTING":
            return ("no run was ever created; the PR conflicts with its base so GitHub cannot "
                    "build refs/pull/N/merge. REMEDY: resolve the conflict; a dispatch also works")
        return ("no run was ever created and the cause is NOT established for a mergeable PR. "
                "REMEDY: dispatch the gate at this head -- proven to work on #1918 -- and report "
                "that triggering, not the workflow, is what failed")
    if state == SUPERSEDED:
        return (f"THIS IS A HOLE, NOT A GREEN: cancelled before any job started, and a successor "
                f"run IS VISIBLE ({ev.get('successor')}), so supersession is OBSERVED here rather "
                f"than inferred. Superseding an obsolete run is CORRECT. "
                f"REMEDY: re-dispatch the authority for the current head")
    if state == CANCELLED_UNVERIFIED:
        return ("THIS IS A HOLE, NOT A GREEN: the run was cancelled and produced no verdict, and "
                f"THE CAUSE IS NOT ESTABLISHED ({ev.get('why_unverified')}). It may have been "
                "superseded by a newer push, cancelled manually, or cancelled before start -- "
                "this tool has not seen which, and will not guess. "
                "REMEDY: re-dispatch the authority for the current head; if the cause matters, "
                "look for a successor run in the same concurrency group")
    if state == PENDING and stale:
        return ("THIS IS A HOLE, NOT A GREEN: a run has not resolved within the stale window. "
                "REMEDY: re-dispatch the authority for the current head")
    return ""


def gh_json(args: list[str]) -> Any:
    out = subprocess.run(
        ["with-proxy", "gh", *args], capture_output=True, text=True, timeout=300
    )
    if out.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)} failed: {out.stderr.strip()[:300]}")
    return json.loads(out.stdout or "null")


def canonical_head_sha(repo: str, pr: dict[str, Any]) -> str:
    """Return a full exact PR-head SHA, resolving abbreviated reader output.

    The Actions runs endpoint treats ``head_sha`` as an exact 40-hex filter: a
    displayed 12-hex prefix returns an empty list rather than an error. Empty is
    therefore not trustworthy until the input has been canonicalised.
    """
    claimed = str(pr.get("headRefOid") or "").strip().lower()
    if re.fullmatch(r"[0-9a-f]{40}", claimed):
        return claimed

    live = gh_json(["api", f"repos/{repo}/pulls/{pr['number']}"]) or {}
    resolved = str(((live.get("head") or {}).get("sha") or "")).strip().lower()
    if not re.fullmatch(r"[0-9a-f]{40}", resolved):
        raise RuntimeError(
            f"PR #{pr['number']} head is not a full SHA: "
            f"listed={claimed or 'missing'} resolved={resolved or 'missing'}"
        )
    if claimed and not resolved.startswith(claimed):
        raise RuntimeError(
            f"PR #{pr['number']} head changed while collecting: "
            f"listed={claimed} resolved={resolved}"
        )
    return resolved


def exact_head_check_runs(repo: str, head: str) -> list[dict[str, Any]]:
    """Read independent exact-commit evidence when run-listing is empty."""
    payload = gh_json([
        "api",
        f"repos/{repo}/commits/{head}/check-runs?per_page=100&filter=all",
    ]) or {}
    checks = [
        check for check in payload.get("check_runs", [])
        if ((check.get("app") or {}).get("slug") == "github-actions")
    ]
    # Check-runs call this timestamp ``started_at`` while workflow runs call it
    # ``created_at``. Normalise it so a fallback PENDING can still become stale.
    for check in checks:
        check.setdefault("created_at", check.get("started_at"))
    return checks


def collect_live(repo: str, limit: int) -> list[dict[str, Any]]:
    prs = gh_json(
        [
            "pr", "list", "--repo", repo, "--state", "open", "--limit", str(limit),
            "--json", "number,headRefOid,mergeable,isDraft,title",
        ]
    ) or []
    # ONE `gh` SUBPROCESS PER PR, RUN CONCURRENTLY RATHER THAN SERIALLY.
    #
    # Measured 2026-08-10: serially this was 18.11s wall for --limit 40 with only
    # 4.24s of user CPU and 57,446 context switches -- the signature of 41 short
    # subprocesses waiting on the network one at a time, not of heavy work. That
    # is 60% of the tick's 30s gate cap, warm, on a quiet box; a cold or contended
    # tail would push this single gate over and it would report NOTHING, which is
    # the detector going blind in the same way as the thing it monitors.
    #
    # *** THIS SKIPS NOTHING. *** Same calls, same population, same per-head SHA
    # matching -- the queries are independent and were merely serialised. An
    # optimisation that decided some heads were "probably unchanged" and skipped
    # them would be the correct-but-unscheduled defect wearing a performance hat,
    # and is exactly what is NOT done here. If a query fails it raises, rather
    # than being treated as "no runs" -- absence of an answer must never read as
    # absence of runs.
    def one(pr: dict[str, Any]) -> dict[str, Any]:
        head = canonical_head_sha(repo, pr)
        runs = (
            gh_json(["api", f"repos/{repo}/actions/runs?head_sha={head}&per_page=100"]) or {}
        ).get("workflow_runs", [])
        # Two independent readers must agree on absence. A full-SHA Actions run
        # query returning zero is cross-checked against exact-commit check-runs;
        # dispatching another workflow would merely hide this reader bug.
        check_runs = exact_head_check_runs(repo, head) if not runs else []

        def jobs_of(run: dict[str, Any]) -> int:
            # Only ever called for a `cancelled` run, so this stays rare.
            return int((gh_json(["api",
                f"repos/{repo}/actions/runs/{run['id']}/jobs"]) or {}).get("total_count", 0))

        row = {
            "number": pr["number"],
            "head": head,
            "mergeable": pr.get("mergeable"),
            "draft": pr.get("isDraft"),
            "run_count": len(runs),
            "check_run_count": len(check_runs),
        }
        def successor_of(run: dict[str, Any]):
            """Visible successor evidence: a LATER run of the SAME workflow on the
            SAME branch. Absence of evidence is returned as None, never as a
            cause."""
            same = (gh_json(["api", f"repos/{repo}/actions/runs"
                             f"?branch={run.get('head_branch','')}&per_page=20"]) or {}
                    ).get("workflow_runs", [])
            for other in same:
                if (other.get("workflow_id") == run.get("workflow_id")
                        and other.get("id") != run.get("id")
                        and (other.get("created_at") or "") > (run.get("created_at") or "")):
                    return f"run {other.get('id')} created {other.get('created_at')}"
            return None

        if runs:
            st, stale, ev = classify(runs, now=time.time(), jobs_of=jobs_of,
                                     successor_of=successor_of)
        else:
            # Check-runs expose the status/conclusion/created_at fields consumed
            # by the pure classifier. Cancellation remains UNVERIFIED because
            # this fallback has no workflow-successor evidence.
            st, stale, ev = classify(check_runs, now=time.time())
            if check_runs:
                ev = {
                    **ev,
                    "reader": "exact-commit-check-runs",
                    "workflow_runs_endpoint_count": 0,
                    "github_actions_check_runs": len(check_runs),
                }
        row["state"], row["stale"], row["evidence"] = st, stale, ev
        row["reason"] = likely_reason(pr, st, stale, ev)
        return row

    with ThreadPoolExecutor(max_workers=8) as pool:
        rows = list(pool.map(one, prs))
    # Deterministic order regardless of completion order, so tick output is
    # stable and diffable across cycles.
    rows.sort(key=lambda r: r["number"], reverse=True)
    return rows


def collect_fixture(path: str) -> list[dict[str, Any]]:
    data = json.load(open(path))
    rows = []
    for pr in data:
        runs = pr.get("runs", [])
        row = {
            "number": pr["number"],
            "head": pr["head"],
            "mergeable": pr.get("mergeable"),
            "draft": pr.get("draft", False),
            "run_count": len(runs),
        }
        st, stale, ev = classify(runs, now=pr.get("now"), jobs_of=pr.get("jobs_of"),
                                 successor_of=pr.get("successor_of"))
        row["state"], row["stale"], row["evidence"] = st, stale, ev
        row["reason"] = likely_reason(pr, st, stale, ev)
        rows.append(row)
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", default="rrnewton/hermit")
    ap.add_argument("--limit", type=int, default=40)
    ap.add_argument("--from-json", dest="fixture")
    ap.add_argument("--gate", action="store_true")
    ap.add_argument("--json", dest="as_json", action="store_true")
    a = ap.parse_args()

    rows = collect_fixture(a.fixture) if a.fixture else collect_live(a.repo, a.limit)
    # HOLES: no verdict exists. A fresh PENDING is deliberately NOT a hole --
    # alarming on it fires after every push and is an alarm no coordinator can
    # satisfy. EVALUATED_FAIL is deliberately not here either: a red gate is a
    # RESULT and another check's job.
    holes = [r for r in rows
             if r["state"] in (NEVER_RAN, SUPERSEDED, CANCELLED_UNVERIFIED)
             or (r["state"] == PENDING and r.get("stale"))]
    unevaluated = holes

    # tick-hub capture consumes key=value fields. Keep the human table below,
    # but also give the scheduled emitter a fully rendered one-line summary so
    # a finding can never degrade into a literal ``{summary}`` placeholder.
    hole_prs = ",".join(f"#{r['number']}" for r in holes[:8]) or "none"
    tick_summary = (
        f"{len(holes)}/{len(rows)} inspected open PR heads carry NO VERDICT; "
        f"affected={hole_prs}; this is a hole, not green; remedy: dispatch the "
        "authority at each current head and resolve base conflicts where named"
    )

    if a.as_json:
        print(json.dumps({"inspected": len(rows), "not_evaluated": len(unevaluated),
                          "rows": rows}, indent=2))
    else:
        # Both terms, always: a bare count of unevaluated heads is not a
        # measurement without the population it came from.
        import collections as _c
        tally = _c.Counter(r["state"] for r in rows)
        print(f"inspected {len(rows)} open PR head(s); NO VERDICT: {len(holes)}  "
              + "  ".join(f"{k}={v}" for k, v in sorted(tally.items())))
        for r in rows:
            if r in holes:
                label = r["state"] + ("(stale)" if r.get("stale") else "")
                print(
                    f"  {label:<16} PR {r['number']}  head {r['head'][:12]}  "
                    f"runs={r['run_count']}  checks={r.get('check_run_count', 0)}  "
                    f"mergeable={r['mergeable']}  -- {r['reason']}"
                )
        if not holes:
            print("  every inspected head carries a verdict")

        if a.gate:
            print(f"state={'hole' if holes else 'ok'}")
            print(f"inspected={len(rows)}")
            print(f"not_evaluated={len(holes)}")
            print(f"summary={tick_summary}")

    if a.gate and unevaluated:
        print(
            f"GATE FAIL: {len(holes)} of {len(rows)} open PR head(s) carry NO VERDICT "
            "(never ran, superseded, or pending past the stale window). None of these is "
            "passing, and only one of the five states is good news.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
