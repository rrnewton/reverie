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

WHAT THIS REPORTS, AND THE DISTINCTION THAT MATTERS. Three outcomes, never two:

  EVALUATED_PASS      >=1 run at the exact head, none failed
  EVALUATED_FAIL      >=1 run at the exact head, at least one failed
  NOT_EVALUATED       ZERO runs at the exact head  <-- the silent state

A failing gate is a RESULT. No gate at all is not a result, and collapsing the
two is the defect. Compare `slot_disk_residue.py` / `holder_liveness.py`, which
draw the same distinction for slots.

Runs are matched on the EXACT head SHA. A run at an older head is evidence about
that older head and nothing else -- the same binding rule the approval labels
fail to apply.

Usage:
  unevaluated_head.py --repo rrnewton/hermit [--limit N] [--gate] [--json]
  unevaluated_head.py --from-json FIXTURE [--gate]   # offline, for the bracket

--gate exits 1 when any inspected head is NOT_EVALUATED, 0 otherwise. It is
deliberately silent about EVALUATED_FAIL: a red gate is another check's job and
double-reporting it here would make this detector noisy enough to be ignored.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from typing import Any

NOT_EVALUATED = "NOT_EVALUATED"
EVALUATED_PASS = "EVALUATED_PASS"
EVALUATED_FAIL = "EVALUATED_FAIL"

# Conclusions that mean the run produced a negative verdict. `cancelled` counts
# as evaluated-but-failed rather than unevaluated: something ran and did not
# finish clean, which is a result. Only the total ABSENCE of runs is
# NOT_EVALUATED.
FAILING = {"failure", "cancelled", "timed_out", "startup_failure", "action_required"}


def classify(runs: list[dict[str, Any]]) -> str:
    """Pure classifier: the seam the bracket drives.

    `runs` are the workflow runs recorded AT THE EXACT HEAD SHA. Empty means no
    workflow was ever created for this commit.
    """
    if not runs:
        return NOT_EVALUATED
    for run in runs:
        if (run.get("conclusion") or "").strip() in FAILING:
            return EVALUATED_FAIL
    return EVALUATED_PASS


def likely_reason(pr: dict[str, Any], state: str) -> str:
    """Why a head went unevaluated, when we can say. Never guessed silently."""
    if state != NOT_EVALUATED:
        return ""
    if (pr.get("mergeable") or "").upper() == "CONFLICTING":
        return (
            "PR conflicts with its base, so GitHub cannot build refs/pull/N/merge "
            "and creates no pull_request run"
        )
    return "cause not established for this PR; conflict is the known cause but does not apply here"


def gh_json(args: list[str]) -> Any:
    out = subprocess.run(
        ["with-proxy", "gh", *args], capture_output=True, text=True, timeout=300
    )
    if out.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)} failed: {out.stderr.strip()[:300]}")
    return json.loads(out.stdout or "null")


def collect_live(repo: str, limit: int) -> list[dict[str, Any]]:
    prs = gh_json(
        [
            "pr", "list", "--repo", repo, "--state", "open", "--limit", str(limit),
            "--json", "number,headRefOid,mergeable,isDraft,title",
        ]
    ) or []
    rows = []
    for pr in prs:
        head = pr["headRefOid"]
        runs = (
            gh_json(["api", f"repos/{repo}/actions/runs?head_sha={head}"]) or {}
        ).get("workflow_runs", [])
        rows.append(
            {
                "number": pr["number"],
                "head": head,
                "mergeable": pr.get("mergeable"),
                "draft": pr.get("isDraft"),
                "run_count": len(runs),
                "state": classify(runs),
            }
        )
        rows[-1]["reason"] = likely_reason(pr, rows[-1]["state"])
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
            "state": classify(runs),
        }
        row["reason"] = likely_reason(pr, row["state"])
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
    unevaluated = [r for r in rows if r["state"] == NOT_EVALUATED]

    if a.as_json:
        print(json.dumps({"inspected": len(rows), "not_evaluated": len(unevaluated),
                          "rows": rows}, indent=2))
    else:
        # Both terms, always: a bare count of unevaluated heads is not a
        # measurement without the population it came from.
        print(f"inspected {len(rows)} open PR head(s); NOT_EVALUATED: {len(unevaluated)}")
        for r in rows:
            if r["state"] == NOT_EVALUATED:
                print(
                    f"  NOT_EVALUATED  PR {r['number']}  head {r['head'][:12]}  "
                    f"runs=0  mergeable={r['mergeable']}  -- {r['reason']}"
                )
        if not unevaluated:
            print("  every inspected head has at least one run recorded against it")

    if a.gate and unevaluated:
        print(
            f"GATE FAIL: {len(unevaluated)} of {len(rows)} open PR head(s) were never "
            "evaluated. This is NOT the same as passing, and nothing else reports it.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
