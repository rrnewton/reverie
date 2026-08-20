#!/usr/bin/env python3
"""Bind review and local-validation cache labels to an exact head.

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

MALFORMED SOURCE LINES ARE ORTHOGONAL TO THOSE STATES. A comment may contain a
historical ``APPROVE <sha>`` synonym and a valid canonical binding at the same
head. The synonym remains syntax debt worth reporting, but it cannot turn the
canonical BOUND_CURRENT authority into absence. Conversely, a malformed line
with no canonical binding remains UNDETERMINED or SUPERSEDED and still fails
the authority gate. Counts and remedies for the two conditions stay separate.

`locally-validated` uses the same three head-binding states. Its existing
receipt marker names an immutable receipt-repository commit and a path whose
40-hex directory is the validated Hermit head. This tool compares that head to
the live PR head; it does not replace the immutable receipt verifier.

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
from typing import Sequence

SHA40 = re.compile(r"\b[0-9a-f]{40}\b")
LANES = ("claude", "codex")
CRITICAL_CLASSES = frozenset(
    {"hermit-syscall", "detcore-scheduling", "reverie-api"}
)
POST_FACTO_LABEL = "post-facto-human-review"
LOCALLY_VALIDATED_LABEL = "locally-validated"

BOUND_CURRENT = "BOUND_CURRENT"
SUPERSEDED = "SUPERSEDED"
UNDETERMINED = "UNDETERMINED"


class GitHubUnavailable(RuntimeError):
    """The live GitHub census did not complete; no binding verdict exists."""


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


class EmptyLandingWindow(RuntimeError):
    """A window with no commits is no measurement, and must not read as clean."""


def landing_observability(landings: "Sequence[dict[str, Any]]") -> dict[str, Any]:
    """How much of a landing window this check can see at all.

    REPORTS OBSERVER COVERAGE, NOT REVIEW QUALITY. Review bindings live on pull
    requests, so a commit that reached main without one is UNOBSERVED BY THIS
    CHECK -- which is not the same claim as unreviewed, and the wording here is
    deliberately kept away from that. Measured 2026-08-20 on hermit: 21 of 29
    commits since 2026-08-19 carried no associated PR, so this check could speak
    to 8 of them. Reporting nothing in that situation reads as healthy, which is
    the condition this function exists to end.

    `landings` is a sequence of {"sha": str, "prs": [pr, ...]} where each pr may
    carry "approved_at": [sha, ...]. Taking the association as an argument keeps
    the arithmetic testable without a network.

    REFUSES AN EMPTY WINDOW. Zero commits is not zero unobserved landings; a
    check that returns a clean count over nothing is the failure this repository
    found repeatedly on 2026-08-20.
    """
    if not landings:
        raise EmptyLandingWindow(
            "landing observability refuses an empty window: zero commits measured "
            "is not zero unobserved landings, it is no measurement"
        )
    total = len(landings)
    via_pr = [l for l in landings if l.get("prs")]
    without_pr = [l for l in landings if not l.get("prs")]
    # "carried a binding" is deliberately NOT "bound to the landed sha": a
    # squash merge gives the commit on main a different sha from the PR head it
    # came from, so requiring equality here would report every squash as
    # unbound and the line would be ignored within a week.
    with_binding = [
        l for l in via_pr if any(pr.get("approved_at") for pr in l["prs"])
    ]
    return {
        "window_commits": total,
        "landed_via_pr": len(via_pr),
        "landed_without_pr": len(without_pr),
        "pr_landings_carrying_a_binding": len(with_binding),
        "summary": (
            f"{len(without_pr)} of {total} commits landed without a pull request, so this "
            f"check could not observe them; {len(via_pr)} took the PR path, of which "
            f"{len(with_binding)} came from a PR carrying an APPROVED-AT binding. "
            "This is observer coverage, not a review verdict: a landing with no PR is "
            "unobserved here, not unreviewed."
        ),
    }


def gh_json(args: list[str]) -> Any:
    try:
        out = subprocess.run(
            ["with-proxy", "gh", *args], capture_output=True, text=True, timeout=300
        )
    except subprocess.TimeoutExpired as error:
        raise GitHubUnavailable(f"gh {' '.join(args)} timed out") from error
    if out.returncode != 0:
        raise GitHubUnavailable(
            f"gh {' '.join(args)} failed: {out.stderr.strip()[:300]}"
        )
    try:
        return json.loads(out.stdout or "null")
    except json.JSONDecodeError as error:
        raise GitHubUnavailable(
            f"gh {' '.join(args)} returned malformed JSON"
        ) from error


def lane_shas(
    comments: list[dict[str, Any]],
    lane: str,
    malformed: list[tuple[str, str]] | None = None,
    pr_number: int | None = None,
    malformed_prs: dict[tuple[str, str], int] | None = None,
) -> list[str]:
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
    # and the rejection mirrors:
    #   REQUEST CHANGES AT <40-hex>                 (historical, lane-less)
    #   CHANGES-REQUESTED-AT: <lane> <40-hex>       (current fleet grammar)
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

    reject_legacy = re.compile(
        r"^REQUEST\s+CHANGES\s+AT\s+[0-9a-f]{40}$", re.IGNORECASE
    )
    reject_current = re.compile(
        r"^CHANGES-REQUESTED-AT:\s*(claude|codex)\s+[0-9a-f]{40}$",
        re.IGNORECASE,
    )
    approve = re.compile(
        r"^APPROVED-AT:\s*(claude|codex)\s+([0-9a-f]{40})$", re.IGNORECASE
    )
    # LOUD ON THE UNKNOWN. A line that is plainly trying to be a verdict binding
    # -- a verdict-ish keyword on the same line as a 40-hex -- but which matches
    # neither grammar is reported, never ignored. Silently skipping an
    # unrecognised variant is how variant three went unnoticed; the next one must
    # announce itself instead.
    suspect = re.compile(
        r"^(?:APPROV|CHANGES-REQUESTED|REQUEST\s+CHANGES|REJECT|LGTM|SIGN(?:ED)?[-\s]?OFF|ACK)[^\n]*\b[0-9a-f]{40}\b",
        re.IGNORECASE,
    )

    found: list[str] = []
    for c in comments:
        body = c.get("body") or ""
        lines = [undecorate(l) for l in body.split("\n")]
        # A rejection contributes NOTHING, even if the same comment also carries
        # an APPROVED-AT-shaped line -- a comment quoting the convention, or
        # quoting the approval it supersedes, must not bind as a positive.
        relevant_rejection = any(reject_legacy.match(line) for line in lines)
        relevant_rejection = relevant_rejection or any(
            (match := reject_current.match(line)) is not None
            and match.group(1).lower() == lane
            for line in lines
        )
        if relevant_rejection:
            # Verdicts are chronological. A later exact-head request for changes
            # revokes this lane's earlier approval; a still-later approval can
            # establish authority again. Clearing rather than merely skipping
            # is load-bearing: otherwise APPROVED then CHANGES-REQUESTED reads
            # as approved forever.
            found.clear()
            continue
        for line in lines:
            m = approve.match(line)
            if not m:
                if (
                    suspect.match(line)
                    and not reject_legacy.match(line)
                    and not reject_current.match(line)
                ):
                    finding = (
                        c.get("html_url") or c.get("url") or "?",
                        line[:120],
                    )
                    # collect_live checks the same comment once per approval
                    # lane.  Count a malformed source line once, not once per
                    # label, or one defect becomes a confident count of two.
                    if finding not in malformed:
                        malformed.append(finding)
                    if pr_number is not None and malformed_prs is not None:
                        malformed_prs[finding] = pr_number
                continue
            explicit_lane, sha = m.group(1), m.group(2)
            if explicit_lane.lower() == lane:
                found.append(sha)
    return found


def local_receipt_heads(comments: list[dict[str, Any]], repo: str) -> list[str]:
    """Exact validated heads named by immutable receipt markers, oldest first.

    The marker's ``commit`` is the receipt-repository commit, not the Hermit
    head. The exact validated head is the 40-hex path component under
    ``validation-receipts/<owner>/<repo>/``. This function establishes only the
    mutable label's head binding. ``verify_receipt.sh`` remains the authority for
    the immutable receipt's contents, hashes, producer coverage, and counts.
    """
    marker = re.compile(
        r"^<!-- locally-validated-receipt "
        r"commit=[0-9a-f]{40} "
        r"path=validation-receipts/" + re.escape(repo) +
        r"/(?P<head>[0-9a-f]{40})/[0-9a-f]{64}\.json "
        r"sha256=[0-9a-f]{64} -->$",
        re.IGNORECASE,
    )
    found: list[str] = []
    for comment in comments:
        for line in (comment.get("body") or "").splitlines():
            match = marker.match(line.strip())
            if match:
                found.append(match.group("head").lower())
    return found


def critical_classes_at_head(
    comments: list[dict[str, Any]], head: str
) -> set[str]:
    """Return coordinator-attested critical classes bound to ``head``.

    ``review-attest --critical`` is the existing closed-enum classification
    source.  Its durable comment carries the class and exact head together.
    This parser deliberately does not infer criticality from paths or prose.
    """
    classes: set[str] = set()
    for comment in comments:
        lines = [line.strip() for line in (comment.get("body") or "").splitlines()]
        if "REVIEW-DISPATCH-ATTESTATION" not in lines:
            continue
        fields: dict[str, str] = {}
        for line in lines:
            if ":" not in line:
                continue
            key, value = line.split(":", 1)
            fields[key.strip().lower()] = value.strip().lower()
        critical_class = fields.get("critical-class")
        if fields.get("head") == head and critical_class in CRITICAL_CLASSES:
            classes.add(critical_class)
    return classes


def collect_live(
    repo: str,
    malformed: list[tuple[str, str]],
    malformed_prs: dict[tuple[str, str], int],
    pr_number: int | None = None,
) -> tuple[list[dict[str, Any]], dict[str, Any] | None]:
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
        tracked = approvals + (
            [LOCALLY_VALIDATED_LABEL] if LOCALLY_VALIDATED_LABEL in labels else []
        )
        if not tracked and pr_number is None:
            return None
        head = pr["headRefOid"]
        comments = gh_json(
            ["api", f"repos/{repo}/issues/{pr['number']}/comments?per_page=100", "--paginate"]
        ) or []
        runs = (gh_json(["api", f"repos/{repo}/actions/runs?head_sha={head}"]) or {}).get(
            "workflow_runs", []
        )
        return {"pr": pr, "tracked": tracked, "head": head,
                "comments": comments, "runs": runs}

    with ThreadPoolExecutor(max_workers=8) as pool:
        fetched = [f for f in pool.map(fetch, prs) if f is not None]

    rows = []
    for f in sorted(fetched, key=lambda f: f["pr"]["number"], reverse=True):
        pr, tracked, head = f["pr"], f["tracked"], f["head"]
        comments, runs = f["comments"], f["runs"]
        for label in tracked:
            if label == LOCALLY_VALIDATED_LABEL:
                lane = "local-validation"
                shas = local_receipt_heads(comments, repo)
            else:
                lane = label.rsplit("-", 1)[-1]
                shas = lane_shas(
                    comments, lane, malformed, pr["number"], malformed_prs
                )
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
    selected = None
    if pr_number is not None and fetched:
        item = fetched[0]
        labels = [label["name"] for label in item["pr"].get("labels", [])]
        selected = {
            "number": item["pr"]["number"],
            "head": item["head"],
            "labels": labels,
            "critical_classes": sorted(
                critical_classes_at_head(item["comments"], item["head"])
            ),
        }
    return rows, selected


def collect_fixture(
    path: str,
    malformed: list[tuple[str, str]],
    malformed_prs: dict[tuple[str, str], int],
    pr_number: int | None = None,
) -> tuple[list[dict[str, Any]], dict[str, Any] | None]:
    rows = []
    selected = None
    for pr in json.load(open(path)):
        if pr_number is not None and pr["number"] != pr_number:
            continue
        comments = [
            {
                "body": body,
                "url": f"fixture://pr/{pr['number']}/comment/{index}",
            }
            for index, body in enumerate(pr.get("comments", []))
        ]
        if pr_number is not None:
            selected = {
                "number": pr["number"],
                "head": pr["head"],
                "labels": list(pr["labels"]),
                "critical_classes": sorted(
                    critical_classes_at_head(comments, pr["head"])
                ),
            }
        for label in pr["labels"]:
            if not (
                label.startswith("passed-review-")
                or label == LOCALLY_VALIDATED_LABEL
            ):
                continue
            lane = (
                "local-validation" if label == LOCALLY_VALIDATED_LABEL
                else label.rsplit("-", 1)[-1]
            )
            if label == LOCALLY_VALIDATED_LABEL and "comments" in pr:
                shas = local_receipt_heads(comments, pr.get("repo", "rrnewton/hermit"))
            elif "comments" in pr:
                shas = lane_shas(
                    comments,
                    lane,
                    malformed,
                    pr["number"],
                    malformed_prs,
                )
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
    return rows, selected


def report_landing_observability(
    repo: str, since: str, as_json: bool, checkout: str = "."
) -> int:
    """Collect the window live and print the surfacing line. Never gates.

    REFUSES IF THE CHECKOUT AND --repo DISAGREE. The commit window comes from
    `git log` in the current directory while the PR association comes from the
    GitHub API for --repo. If those name different repositories the output is
    arithmetic about two unrelated populations, reported with a confident label
    -- the exact shape of defect this file exists to surface. Caught while
    demonstrating this function, so the guard is here rather than in a caller.
    """
    remote = subprocess.run(
        ["git", "-C", checkout, "remote", "get-url", "origin"],
        capture_output=True, text=True,
    )
    origin = remote.stdout.strip()
    if remote.returncode != 0 or repo.lower() not in origin.lower():
        print(
            f"NO RESULT: refusing to mix repositories: the checkout's origin is "
            f"{origin or '<unreadable>'} but --repo is {repo}; run this from a "
            f"checkout of {repo}",
            file=sys.stderr,
        )
        return 1
    try:
        log = subprocess.run(
            ["git", "-C", checkout, "log", f"--since={since}", "--format=%H",
             "origin/main"],
            capture_output=True, text=True, timeout=120,
        )
    except subprocess.TimeoutExpired:
        print("NO RESULT: git log timed out", file=sys.stderr)
        return 1
    if log.returncode != 0:
        print(f"NO RESULT: git log failed: {log.stderr.strip()[:200]}", file=sys.stderr)
        return 1
    shas = [line.strip() for line in log.stdout.splitlines() if line.strip()]
    landings = []
    for sha in shas:
        prs = gh_json(["api", f"repos/{repo}/commits/{sha}/pulls", "--jq",
                       "[.[] | {number: .number}]"]) or []
        landings.append({"sha": sha, "prs": prs})
    try:
        report = landing_observability(landings)
    except EmptyLandingWindow as error:
        print(f"NO RESULT: {error}", file=sys.stderr)
        return 1
    emit_landing_observability(report, as_json)
    return 0


def emit_landing_observability(report: dict[str, Any], as_json: bool) -> None:
    """Print the measured report in the format selected by the caller.

    Tick capture is line-oriented ``key=value``. Printing a prose prefix here
    leaves the configured ``{summary}`` unresolved even though the report
    contains a real summary, so every non-JSON field is emitted explicitly.
    """
    if as_json:
        print(json.dumps(report))
    else:
        for key, value in report.items():
            print(f"{key}={value}")


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
    ap.add_argument(
        "--landing-observability",
        action="store_true",
        help=(
            "SURFACING ONLY, never gates: report how many commits on the default "
            "branch since --since landed without a pull request, and are therefore "
            "outside anything this check can observe. Reports observer coverage, "
            "not review quality."
        ),
    )
    ap.add_argument(
        "--since",
        default="1 day ago",
        help="window start for --landing-observability, any git --since expression",
    )
    ap.add_argument(
        "--checkout",
        metavar="DIR",
        default=".",
        help=(
            "DIRECTORY of the checkout whose commits form the window. A path, not "
            "a commit. Must be a checkout of --repo; mixing them is refused."
        ),
    )
    a = ap.parse_args()
    if a.landing_observability:
        return report_landing_observability(a.repo, a.since, a.as_json, a.checkout)
    a.gate_on = getattr(a, "gate_on", "all")

    malformed: list[tuple[str, str]] = []
    malformed_pr_by_source: dict[tuple[str, str], int] = {}
    try:
        rows, selected = (
            collect_fixture(a.fixture, malformed, malformed_pr_by_source, a.pr)
            if a.fixture
            else collect_live(a.repo, malformed, malformed_pr_by_source, a.pr)
        )
    except GitHubUnavailable as error:
        summary = (
            "approval binding could not be determined: "
            + " ".join(str(error).split()).replace("{", "(").replace("}", ")")
        )
        if a.gate and not a.as_json:
            print("state=unavailable")
            print(f"summary={summary}")
        elif a.as_json:
            print(json.dumps({"state": "unavailable", "summary": summary}))
        print(f"NO RESULT: {summary}", file=sys.stderr)
        return 75 if a.gate else 1
    # The final landing boundary always selects exactly one PR with --pr.  An
    # empty row set there means the PR supplied no passed-review-* cache label
    # at all.  That is absence of review evidence, not a population with zero
    # defects.  Fleet-wide scans deliberately retain their historical scope:
    # they audit approval labels that exist and do not infer which open PRs
    # require review.
    approval_rows = [row for row in rows if row["label"].startswith("passed-review-")]
    approval_set_missing = a.pr is not None and not approval_rows
    selected_labels = set(selected["labels"]) if selected is not None else set()
    selected_critical_classes = (
        set(selected["critical_classes"]) if selected is not None else set()
    )
    # The post-facto label is not itself a critical classifier: a new
    # determinization strategy may carry it while remaining single-review.
    # Criticality comes from the existing closed-enum review-attest record.
    critical_required = bool(selected_critical_classes)
    critical_label_missing = critical_required and POST_FACTO_LABEL not in selected_labels
    authoritative_lanes = {
        str(row["lane"])
        for row in rows
        if row["state"] == BOUND_CURRENT and row["lane"] in LANES
    }
    critical_lanes_missing = (
        sorted(set(LANES) - authoritative_lanes) if critical_required else []
    )
    bad = [r for r in rows if r["state"] != BOUND_CURRENT]
    authoritative = len(rows) - len(bad)
    malformed_prs = set(malformed_pr_by_source.values())
    malformed_affected_labels = {
        (r["number"], r["label"])
        for r in rows
        if r["number"] in malformed_prs and r["label"].startswith("passed-review-")
    }
    # The composed state: a review/local-validation cache that no longer holds,
    # on a head nothing evaluated.
    combined = [r for r in bad if r["runs_at_head"] == 0]
    approved_and_unevaluated = [
        r for r in combined if r["label"].startswith("passed-review-")
    ]

    if a.as_json:
        print(json.dumps({"approvals": len(approval_rows),
                          "binding_labels": len(rows),
                          "authoritative": authoritative,
                          "approval_set_missing": approval_set_missing,
                          "critical_required": critical_required,
                          "critical_classes": sorted(selected_critical_classes),
                          "critical_label_missing": critical_label_missing,
                          "critical_lanes_missing": critical_lanes_missing,
                          "required_approval_sets": 1 if a.pr is not None else 0,
                          "approval_sets_missing": int(approval_set_missing),
                          "authority_absent": len(bad), "not_holding": len(bad),
                          "approved_and_unevaluated": len(approved_and_unevaluated),
                          "binding_labels_and_unevaluated": len(combined),
                          "malformed_source_lines": len(malformed),
                          "malformed_affected_labels": len(malformed_affected_labels),
                          "malformed_affected_prs": sorted(malformed_prs),
                          "malformed_details": [
                              {
                                  "pr": malformed_pr_by_source.get((where, line)),
                                  "where": where,
                                  "line": line,
                              }
                              for where, line in malformed
                          ],
                          "rows": rows}, indent=2))
    else:
        print(f"inspected {len(rows)} passed-review-* / locally-validated label(s); bound to current head: "
              f"{authoritative}; authority absent: {len(bad)}; "
              f"passed-review-* / locally-validated AND unevaluated: {len(combined)}")
        for r in rows:
            if r["state"] == BOUND_CURRENT:
                continue
            if r["bound_to"]:
                bound = r["bound_to"][:12]
            elif r["label"] == LOCALLY_VALIDATED_LABEL:
                bound = "<no locally-validated receipt marker>"
            else:
                bound = "<no APPROVED-AT binding>"
            if r["runs_at_head"] == 0:
                flag = (
                    "  *** LOCALLY-VALIDATED + UNEVALUATED ***"
                    if r["label"] == LOCALLY_VALIDATED_LABEL
                    else "  *** APPROVED + UNEVALUATED ***"
                )
            else:
                flag = ""
            print(f"  {r['state']:<13} PR {r['number']}  {r['label']}  "
                  f"bound-to {bound}  head {r['head'][:12]}  runs={r['runs_at_head']}{flag}")
        if approval_set_missing:
            print(f"  REQUIRED-APPROVAL-SET-MISSING  PR {a.pr} has zero passed-review labels")
        if critical_label_missing:
            print(
                f"  CRITICAL-LABEL-MISSING  PR {a.pr} is classified "
                f"{','.join(sorted(selected_critical_classes))}; missing {POST_FACTO_LABEL}"
            )
        if critical_lanes_missing:
            print(
                f"  CRITICAL-REVIEW-MISSING  PR {a.pr} lacks exact-head "
                f"{','.join(critical_lanes_missing)} approval"
            )
        elif not bad:
            print("  every passed-review-* / locally-validated label is bound to the current head")
        for where, line in malformed:
            # LOUD, never ignored: something tried to be a binding and is not one.
            pr = malformed_pr_by_source.get((where, line))
            pr_text = f"PR {pr}  " if pr is not None else ""
            print(f"  MALFORMED-SOURCE-LINE  {pr_text}{line}   ({where})")

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
        if critical_label_missing or critical_lanes_missing:
            state = "critical-review-incomplete"
            problems = []
            if critical_label_missing:
                problems.append(f"missing {POST_FACTO_LABEL}")
            if critical_lanes_missing:
                problems.append(
                    "missing exact-head " + ",".join(critical_lanes_missing) + " approval"
                )
            summary = f"PR #{a.pr} critical review protocol incomplete: {'; '.join(problems)}"
        elif approval_set_missing:
            state = "absent"
            summary = (
                f"PR #{a.pr} has zero passed-review labels; absence of review "
                "evidence cannot satisfy the final landing boundary"
            )
        else:
            state = "unbound" if gating else "ok"
            remedies = []
            if any(r["label"].startswith("passed-review-") for r in gating):
                remedies.append(
                    "re-review the exact head and emit APPROVED-AT: <lane> <40-hex-head>"
                )
            if any(r["label"] == LOCALLY_VALIDATED_LABEL for r in gating):
                remedies.append(
                    "validate the exact head and publish its counted receipt before "
                    "reapplying locally-validated"
                )
            if not remedies:
                remedies.append("no binding repair is required")
            summary = (
                f"{len(gating)}/{len(rows)} passed-review-* / locally-validated labels fail the "
                f"{a.gate_on} binding check; affected={affected}; they do not bind "
                f"the current head and are not deleted; remedy: {'; '.join(remedies)}"
            )
        print(f"state={state}")
        print(f"summary={summary}")
        if malformed:
            print("malformed_state=present")
            print(
                "malformed_summary="
                f"{len(malformed)} malformed source line(s) across "
                f"{len(malformed_affected_labels)} affected approval label(s) "
                f"on {len(malformed_prs)} PR(s); label authority is reported separately"
            )
            print(
                "malformed_remedy=edit or remove the malformed historical source line; "
                "emitting APPROVED-AT repairs missing label authority but does not clear "
                "a malformed line when the same comment is already authoritative"
            )
        else:
            print("malformed_state=none")

    if a.gate and (critical_label_missing or critical_lanes_missing):
        print(
            f"GATE FAIL: PR #{a.pr} is critical but its owner-required review "
            "protocol is incomplete.",
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
            f"GATE FAIL ({a.gate_on}): {len(gating)} of {len(rows)} passed-review-* / locally-validated label(s) do not bind "
            "to the current head. They are NOT deleted -- they simply must not satisfy the review "
            "or local-validation head-binding gate.",
            file=sys.stderr,
        )
        return 1
    if a.gate and malformed:
        print(
            f"GATE ADVISORY: {len(malformed)} malformed source line(s) need syntax cleanup, "
            "but malformed syntax is not itself proof that label authority is absent.",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
