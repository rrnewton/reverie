#!/usr/bin/env python3
"""Bind each `passed-review-*` approval to an exact head, and report the ones that no longer hold.

WHY. `scripts/core-review-protocol-lint.sh` (hermit) gates a
`post-facto-human-review` merge on `has_label passed-review-codex` and
`has_label passed-review-claude` and NOTHING ELSE -- no SHA anywhere in its 109
lines. Its own header comment says the labels "are invalidated on every new
push", so its correctness rests on a producer it never verifies, and its failure
text asserts "has not approved the current revision", a claim it has no means to
evaluate.

THE PRODUCER IT TRUSTS FAILS OPEN. `merge-gate.yml`'s
`invalidate-local-validation` job strips those labels on `synchronize` -- but it
is itself a `pull_request` job, and a CONFLICTING PR produces NO `pull_request`
runs at all (GitHub cannot build `refs/pull/N/merge`). So the same conflict that
stops the new head being evaluated also stops the stale approval being retired.
One cause, both guards. Measured 2026-08-10: #1864 carried `passed-review-codex`
granted at `87f62930` while its head was `b2e44120` with ZERO runs -- approved,
unevaluated, and green-looking at once. See `unevaluated_head.py` for the other
half.

THE FIX SHAPE IS DERIVATION, NOT DELETION. Validity is computed HERE, at the
moment of use, from evidence that is already required to exist:

    an approval holds only if a role-tagged review comment names the EXACT
    current head SHA for that lane.

That cannot fail open. There is no event to miss and no runner to be offline; if
the head moved, the comparison simply fails. It is also what policy already says
these artifacts mean -- "role-tagged review comments carrying the full head SHA
are authority; numbered review and `passed-review-*` labels are caches."

*** NOTHING IS DELETED. *** A superseded approval stops satisfying the gate and
stays visible forever as the record of what was reviewed and when. That is
deliberate: PR #1874's review history was permanently lost on 2026-08-09 by
stripping labels, and the existing merge-gate job DELETES, which is the same
mistake automated. Invalidation and erasure are different operations and only
the first is wanted.

States, three not two -- the same discipline as `unevaluated_head.py`:

  BOUND_CURRENT   a role-tagged comment names the current head. Holds.
  SUPERSEDED      the newest SHA a comment names is not the current head.
  UNDETERMINED    the label exists but no reviewer emitted a machine-readable
                  `APPROVED-AT: <lane> <sha>` binding, so what was reviewed
                  CANNOT BE ESTABLISHED. Flagged, never passed. This is today's
                  normal case: the existing tag convention does not encode the
                  lane (a codex review may be tagged `gpt-5.6-sol`), so there is
                  no link from label to earning comment. Fixing that convention
                  is the prerequisite for this gate to do real work; until then
                  it correctly reports that these approvals are unverifiable
                  rather than pretending to verify them.

Usage:
  approval_binding.py --repo rrnewton/hermit [--gate] [--json]
  approval_binding.py --from-json FIXTURE [--gate]     # offline, for the bracket
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from typing import Any

SHA40 = re.compile(r"\b[0-9a-f]{40}\b")
LANES = ("claude", "codex")

BOUND_CURRENT = "BOUND_CURRENT"
SUPERSEDED = "SUPERSEDED"
UNDETERMINED = "UNDETERMINED"


def classify_approval(head: str, comment_shas: list[str]) -> str:
    """Pure classifier: the seam the bracket drives.

    `comment_shas` are the head SHAs named by that lane's role-tagged review
    comments, oldest first. The NEWEST is what the lane last bound itself to.
    """
    if not comment_shas:
        return UNDETERMINED
    if comment_shas[-1] == head:
        return BOUND_CURRENT
    return SUPERSEDED


def gh_json(args: list[str]) -> Any:
    out = subprocess.run(
        ["with-proxy", "gh", *args], capture_output=True, text=True, timeout=300
    )
    if out.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)} failed: {out.stderr.strip()[:300]}")
    return json.loads(out.stdout or "null")


def lane_shas(comments: list[dict[str, Any]], lane: str) -> list[str]:
    """SHAs a lane EXPLICITLY bound itself to, oldest first. Never inferred.

    *** THIS DELIBERATELY REFUSES TO GUESS, AND THE FIRST VERSION DID GUESS. ***
    Measured 2026-08-10 against live data, a heuristic version of this function
    produced confidently wrong verdicts on all three approval-bearing PRs, for
    two reasons worth keeping written down:

    1. It searched the comment PROSE for the lane name, so a comment *about*
       codex's approval was attributed *to* codex. My own #1864 comment, tagged
       `[impl agent, claude-opus-5]`, contains "passed-review-codex is now
       STALE" -- and was read as a codex review, binding codex to the current
       head and hiding the very staleness it was reporting.

    2. THE ROLE TAG DOES NOT ENCODE THE LANE. #1864's codex review is tagged
       `[adversarial-reviewer agent, GPT-5 Codex]`, but #1727's is
       `[adversarial-reviewer agent, gpt-5.6-sol]` -- neither "codex" nor
       "claude" appears. A Codex-family reviewer may carry any GPT model name,
       and the `passed-review-*` label is applied separately by a coordinator.
       There is no reliable machine link from label to earning comment.

    So the ONLY thing trusted here is an explicit, machine-readable binding that
    a reviewer emits deliberately:

        APPROVED-AT: <lane> <40-hex>

    Absent that, the caller gets an empty list and the approval is reported
    UNDETERMINED -- flagged, not passed. A gate that guesses is worse than no
    gate, which is exactly what the heuristic version demonstrated.
    """
    pattern = re.compile(
        r"^\s*APPROVED-AT:\s*" + re.escape(lane) + r"\s+([0-9a-f]{40})\s*$",
        re.IGNORECASE | re.MULTILINE,
    )
    found: list[str] = []
    for c in comments:
        found.extend(pattern.findall(c.get("body") or ""))
    return found


def collect_live(repo: str) -> list[dict[str, Any]]:
    prs = gh_json(
        ["pr", "list", "--repo", repo, "--state", "open", "--limit", "300",
         "--json", "number,headRefOid,labels,mergeable"]
    ) or []
    rows = []
    for pr in prs:
        labels = [l["name"] for l in pr.get("labels", [])]
        approvals = [l for l in labels if l.startswith("passed-review-")]
        if not approvals:
            continue
        head = pr["headRefOid"]
        comments = gh_json(
            ["api", f"repos/{repo}/issues/{pr['number']}/comments?per_page=100", "--paginate"]
        ) or []
        runs = (gh_json(["api", f"repos/{repo}/actions/runs?head_sha={head}"]) or {}).get(
            "workflow_runs", []
        )
        for label in approvals:
            lane = label.rsplit("-", 1)[-1]
            shas = lane_shas(comments, lane)
            rows.append(
                {
                    "number": pr["number"],
                    "label": label,
                    "lane": lane,
                    "head": head,
                    "bound_to": shas[-1] if shas else None,
                    "state": classify_approval(head, shas),
                    "runs_at_head": len(runs),
                    "mergeable": pr.get("mergeable"),
                }
            )
    return rows


def collect_fixture(path: str) -> list[dict[str, Any]]:
    rows = []
    for pr in json.load(open(path)):
        for label in pr["labels"]:
            lane = label.rsplit("-", 1)[-1]
            shas = pr.get("lane_shas", {}).get(lane, [])
            rows.append(
                {
                    "number": pr["number"],
                    "label": label,
                    "lane": lane,
                    "head": pr["head"],
                    "bound_to": shas[-1] if shas else None,
                    "state": classify_approval(pr["head"], shas),
                    "runs_at_head": pr.get("runs_at_head", 1),
                    "mergeable": pr.get("mergeable"),
                }
            )
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", default="rrnewton/hermit")
    ap.add_argument("--from-json", dest="fixture")
    ap.add_argument("--gate", action="store_true")
    ap.add_argument(
        "--gate-on",
        default="all",
        choices=["all", "superseded", "unverifiable"],
        help=(
            "Which states make --gate exit non-zero. Default 'all' (anything not "
            "BOUND_CURRENT) is the standalone contract and is unchanged. "
            "'superseded' gates only on approvals we can prove were earned "
            "against a different head, plus the combined approved-and-unevaluated "
            "state -- both per-PR and actionable today. 'unverifiable' gates only "
            "on UNDETERMINED. The split exists because UNDETERMINED is currently a "
            "STANDING condition, not an event: no reviewer emits APPROVED-AT yet, "
            "so every approval reads unverifiable and a per-tick page would be "
            "pure noise that trains people to ignore the check. tick-hub.yaml "
            "already draws exactly this distinction for validate_wall_regression "
            "(gates the event) versus validate_wall_series_readiness (surfaces the "
            "standing blocker daily so it cannot go silently inert)."
        ),
    )
    ap.add_argument("--json", dest="as_json", action="store_true")
    a = ap.parse_args()
    a.gate_on = getattr(a, "gate_on", "all")

    rows = collect_fixture(a.fixture) if a.fixture else collect_live(a.repo)
    bad = [r for r in rows if r["state"] != BOUND_CURRENT]
    # The composed state: an approval that no longer holds, on a head nothing
    # evaluated. Approved, unevaluated, and indistinguishable from ready.
    combined = [r for r in bad if r["runs_at_head"] == 0]

    if a.as_json:
        print(json.dumps({"approvals": len(rows), "not_holding": len(bad),
                          "approved_and_unevaluated": len(combined), "rows": rows}, indent=2))
    else:
        print(f"inspected {len(rows)} approval label(s); not holding: {len(bad)}; "
              f"approved AND unevaluated: {len(combined)}")
        for r in rows:
            if r["state"] == BOUND_CURRENT:
                continue
            bound = r["bound_to"][:12] if r["bound_to"] else "<no APPROVED-AT binding>"
            flag = "  *** APPROVED + UNEVALUATED ***" if r["runs_at_head"] == 0 else ""
            print(f"  {r['state']:<13} PR {r['number']}  {r['label']}  "
                  f"bound-to {bound}  head {r['head'][:12]}  runs={r['runs_at_head']}{flag}")
        if not bad:
            print("  every approval label is bound to the current head")

    if a.gate_on == "superseded":
        gating = [r for r in rows if r["state"] == SUPERSEDED] + [
            r for r in combined if r["state"] != SUPERSEDED
        ]
    elif a.gate_on == "unverifiable":
        gating = [r for r in rows if r["state"] == UNDETERMINED]
    else:
        gating = bad

    if a.gate and gating:
        print(
            f"GATE FAIL ({a.gate_on}): {len(gating)} of {len(rows)} approval label(s) do not bind "
            "to the current head. They are NOT deleted -- they simply must not satisfy the review "
            "gate.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
