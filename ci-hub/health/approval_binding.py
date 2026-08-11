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
  approval_binding.py --from-json FIXTURE [--pr N] [--gate]  # offline bracket
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
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


def lane_shas(comments: list[dict[str, Any]], lane: str,
               malformed: list[tuple[str, str]] | None = None) -> list[str]:
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
    # GRAMMAR, pinned. There is exactly one authority-bearing shape:
    #   APPROVED-AT: <lane> <40-hex>
    # Whole-line Markdown decoration is harmless and removed above, but the
    # colon and explicit lane are mandatory. Historical lane-less variants are
    # intentionally ABSENT authority: inferring a lane from prose recreates the
    # cache-as-source defect this verifier exists to remove.
    # and the rejection mirror introduced on #2026 (comment 5235451519):
    #   REQUEST CHANGES AT <40-hex>
    #
    # Markdown may wrap the canonical line. Decoration is not identity, so
    # `**APPROVED-AT: claude <sha>**` is normalized before the strict grammar is
    # applied. The mandatory colon, lane, and full SHA remain unchanged.
    if malformed is None:
        malformed = []

    def undecorate(line: str) -> str:
        """Remove only markdown that wraps the complete verdict line.

        ``str.strip("*_`")`` also removed a lone opening backtick.  That
        promoted an inline-code quotation followed by prose into a verdict-ish
        line, even though the comment explicitly said it was not a re-approval.
        Whole-line emphasis is legitimate; a code span embedded in prose is
        not a binding.
        """
        line = line.strip()
        while True:
            for wrapper in ("`", "**", "__", "*", "_"):
                if (
                    line.startswith(wrapper)
                    and line.endswith(wrapper)
                    and len(line) > 2 * len(wrapper)
                ):
                    line = line[len(wrapper):-len(wrapper)].strip()
                    break
            else:
                return line

    reject = re.compile(r"^REQUEST\s+CHANGES\s+AT\s+[0-9a-f]{40}$", re.IGNORECASE)
    approve = re.compile(
        r"^APPROVED-AT:\s*(claude|codex)\s+([0-9a-f]{40})$", re.IGNORECASE
    )
    # LOUD ON THE UNKNOWN. A line that is plainly trying to be a verdict binding
    # -- a verdict-ish keyword on the same line as a 40-hex -- but which matches
    # neither grammar is reported, never ignored. Silently skipping an
    # unrecognised variant is how variant three went unnoticed; the next one must
    # announce itself instead.
    suspect = re.compile(
        r"^(?:APPROV|REQUEST\s+CHANGES|REJECT|LGTM|SIGN(?:ED)?[-\s]?OFF|ACK)[^\n]*\b[0-9a-f]{40}\b",
        re.IGNORECASE,
    )

    found: list[str] = []
    for c in comments:
        body = c.get("body") or ""
        lines = [undecorate(l) for l in body.split("\n")]
        # A rejection contributes NOTHING, even if the same comment also carries
        # an APPROVED-AT-shaped line -- a comment quoting the convention, or
        # quoting the approval it supersedes, must not bind as a positive.
        if any(reject.match(l) for l in lines):
            continue
        for line in lines:
            m = approve.match(line)
            if not m:
                if suspect.match(line) and not reject.match(line):
                    finding = (c.get("html_url") or c.get("url") or "?", line[:120])
                    # collect_live checks the same comment once per approval
                    # lane.  Count a malformed source line once, not once per
                    # label, or one defect becomes a confident count of two.
                    if finding not in malformed:
                        malformed.append(finding)
                continue
            explicit_lane, sha = m.group(1), m.group(2)
            if explicit_lane.lower() == lane:
                found.append(sha)
    return found


def collect_live(
    repo: str,
    malformed: list[tuple[str, str]],
    pr_number: int | None = None,
) -> list[dict[str, Any]]:
    if pr_number is None:
        prs = gh_json(
            ["pr", "list", "--repo", repo, "--state", "open", "--limit", "300",
             "--json", "number,headRefOid,labels,mergeable"]
        ) or []
    else:
        pr = gh_json(
            ["pr", "view", str(pr_number), "--repo", repo,
             "--json", "number,headRefOid,labels,mergeable"]
        )
        prs = [pr] if isinstance(pr, dict) else []
    # Same concurrency rationale as unevaluated_head.py: the per-PR comment and
    # run queries are INDEPENDENT and were merely serialised. This skips nothing
    # -- same PRs, same comments, same run counts -- it only stops waiting one at
    # a time. A failed query raises rather than being read as "no comments",
    # because absence of an answer must not read as absence of evidence.
    def fetch(pr: dict[str, Any]) -> dict[str, Any] | None:
        labels = [l["name"] for l in pr.get("labels", [])]
        approvals = [l for l in labels if l.startswith("passed-review-")]
        if not approvals:
            return None
        head = pr["headRefOid"]
        comments = gh_json(
            ["api", f"repos/{repo}/issues/{pr['number']}/comments?per_page=100", "--paginate"]
        ) or []
        runs = (gh_json(["api", f"repos/{repo}/actions/runs?head_sha={head}"]) or {}).get(
            "workflow_runs", []
        )
        return {"pr": pr, "approvals": approvals, "head": head,
                "comments": comments, "runs": runs}

    with ThreadPoolExecutor(max_workers=8) as pool:
        fetched = [f for f in pool.map(fetch, prs) if f is not None]

    rows = []
    for f in sorted(fetched, key=lambda f: f["pr"]["number"], reverse=True):
        pr, approvals, head = f["pr"], f["approvals"], f["head"]
        comments, runs = f["comments"], f["runs"]
        for label in approvals:
            lane = label.rsplit("-", 1)[-1]
            shas = lane_shas(comments, lane, malformed)
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


def collect_fixture(
    path: str,
    malformed: list[tuple[str, str]],
    pr_number: int | None = None,
) -> list[dict[str, Any]]:
    rows = []
    for pr in json.load(open(path)):
        if pr_number is not None and pr["number"] != pr_number:
            continue
        for label in pr["labels"]:
            lane = label.rsplit("-", 1)[-1]
            if "comments" in pr:
                shas = lane_shas([{"body": b} for b in pr["comments"]], lane, malformed)
            else:
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
    ap.add_argument(
        "--pr",
        type=int,
        help="Restrict live verification to one PR (used by the final landing boundary)",
    )
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

    malformed: list[tuple[str, str]] = []
    rows = (
        collect_fixture(a.fixture, malformed, a.pr)
        if a.fixture
        else collect_live(a.repo, malformed, a.pr)
    )
    # The final landing boundary always selects exactly one PR with --pr.  An
    # empty row set there means the PR supplied no passed-review-* cache label
    # at all.  That is absence of review evidence, not a population with zero
    # defects.  Fleet-wide scans deliberately retain their historical scope:
    # they audit approval labels that exist and do not infer which open PRs
    # require review.
    approval_set_missing = a.pr is not None and not rows
    bad = [r for r in rows if r["state"] != BOUND_CURRENT]
    authoritative = len(rows) - len(bad)
    # The composed state: an approval that no longer holds, on a head nothing
    # evaluated. Approved, unevaluated, and indistinguishable from ready.
    combined = [r for r in bad if r["runs_at_head"] == 0]

    if a.as_json:
        print(json.dumps({"approvals": len(rows), "authoritative": authoritative,
                          "approval_set_missing": approval_set_missing,
                          "required_approval_sets": 1 if a.pr is not None else 0,
                          "approval_sets_missing": int(approval_set_missing),
                          "authority_absent": len(bad), "not_holding": len(bad),
                          "approved_and_unevaluated": len(combined), "rows": rows}, indent=2))
    else:
        print(f"inspected {len(rows)} approval label(s); authoritative at current head: "
              f"{authoritative}; authority absent: {len(bad)}; "
              f"approved AND unevaluated: {len(combined)}")
        for r in rows:
            if r["state"] == BOUND_CURRENT:
                continue
            bound = r["bound_to"][:12] if r["bound_to"] else "<no APPROVED-AT binding>"
            flag = "  *** APPROVED + UNEVALUATED ***" if r["runs_at_head"] == 0 else ""
            print(f"  {r['state']:<13} PR {r['number']}  {r['label']}  "
                  f"bound-to {bound}  head {r['head'][:12]}  runs={r['runs_at_head']}{flag}")
        if approval_set_missing:
            print(f"  REQUIRED-APPROVAL-SET-MISSING  PR {a.pr} has zero passed-review labels")
        elif not bad:
            print("  every approval label is bound to the current head")
        for where, line in malformed:
            # LOUD, never ignored: something tried to be a binding and is not one.
            print(f"  MALFORMED-BINDING  {line}   ({where})")

    if a.gate_on == "superseded":
        gating = [r for r in rows if r["state"] == SUPERSEDED] + [
            r for r in combined if r["state"] != SUPERSEDED
        ]
    elif a.gate_on == "unverifiable":
        gating = [r for r in rows if r["state"] == UNDETERMINED]
    else:
        gating = bad

    if a.gate and not a.as_json:
        affected = ",".join(f"#{r['number']}" for r in gating[:8]) or "none"
        if malformed:
            state = "malformed"
            summary = (
                f"{len(malformed)} malformed approval bindings across {len(rows)} "
                "approval labels; authority cannot be established; remedy: emit "
                "APPROVED-AT: <lane> <40-hex-head>"
            )
        elif approval_set_missing:
            state = "absent"
            summary = (
                f"PR #{a.pr} has zero passed-review labels; absence of review "
                "evidence cannot satisfy the final landing boundary"
            )
        else:
            state = "unbound" if gating else "ok"
            summary = (
                f"{len(gating)}/{len(rows)} approval labels fail the "
                f"{a.gate_on} binding check; affected={affected}; they do not bind "
                "the current head and are not deleted; remedy: re-review the exact "
                "head and emit APPROVED-AT: <lane> <40-hex-head>"
            )
        print(f"state={state}")
        print(f"summary={summary}")

    if a.gate and malformed:
        print(
            f"GATE FAIL: {len(malformed)} line(s) look like a verdict binding but match no known "
            "grammar. An unrecognised variant that is silently skipped reads as NO approval, which "
            "retires real authorisation -- so this is reported, not ignored.",
            file=sys.stderr,
        )
        return 1
    if a.gate and approval_set_missing:
        print(
            f"GATE FAIL: PR #{a.pr} has zero passed-review labels. Zero approvals "
            "is absence of authority, not zero review problems.",
            file=sys.stderr,
        )
        return 1
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
