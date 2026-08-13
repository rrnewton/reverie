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

MALFORMED SOURCE LINES ARE ORTHOGONAL TO THOSE STATES. A comment may contain a
historical ``APPROVE <sha>`` synonym and a valid canonical binding at the same
head. The synonym remains syntax debt worth reporting, but it cannot turn the
canonical BOUND_CURRENT authority into absence. Conversely, a malformed line
with no canonical binding remains UNDETERMINED or SUPERSEDED and still fails
the authority gate. Counts and remedies for the two conditions stay separate.

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
CRITICAL_CLASSES = frozenset(
    {"hermit-syscall", "detcore-scheduling", "reverie-api"}
)
POST_FACTO_LABEL = "post-facto-human-review"

BOUND_CURRENT = "BOUND_CURRENT"
SUPERSEDED = "SUPERSEDED"
UNDETERMINED = "UNDETERMINED"

# The two states that were previously unrepresentable. See `classify_lane`.
REJECTED_CURRENT = "REJECTED_CURRENT"
REJECTED_SUPERSEDED = "REJECTED_SUPERSEDED"
NEVER = "NEVER"


def classify_approval(head: str, comment_shas: list[str]) -> str:
    """Pure classifier: the seam the bracket drives.

    `comment_shas` are the head SHAs named by that lane's role-tagged review
    comments, oldest first. The NEWEST is what the lane last bound itself to.

    DELIBERATELY UNCHANGED by the 2026-08-13 three-state work, and the
    unchangedness is the point.  Every consumer of this function is a GATE --
    `land-pr.sh` and `pr_head_ci.py` read only the CLI return code, and
    `pr_status.py` reduces the result to ``binding == BOUND_CURRENT``.  For a
    gate, "this lane rejected" and "this lane never spoke" must BOTH refuse,
    so collapsing them onto UNDETERMINED here is correct behaviour rather
    than the defect.  Making a gate three-state would be a regression wearing
    a fix's clothes.  `classify_lane` below adds the distinction for the
    consumers that need it, without moving this seam.
    """
    if not comment_shas:
        return UNDETERMINED
    if comment_shas[-1] == head:
        return BOUND_CURRENT
    return SUPERSEDED


class LaneVerdict:
    """Everything one lane said, including the part that used to be discarded.

    DELIBERATELY A PLAIN CLASS, NOT A ``@dataclass``.  Several consumers load
    this file through ``importlib.util.module_from_spec`` WITHOUT registering
    it in ``sys.modules`` -- including this module's own grammar test.
    ``dataclasses`` resolves ``sys.modules[cls.__module__].__dict__`` while
    building the class, so a module-level dataclass raises AttributeError on
    import under exactly those loaders.  A first draft here used
    ``@dataclass(frozen=True)`` and broke the test suite at import time.

    ``approved_shas`` is exactly what `lane_shas` has always returned: the
    SHAs this lane bound approvals to, oldest first, cleared by any later
    rejection.  ``rejected_sha`` and ``rejected_idx`` are the newest rejection
    that survived -- that is, one with no later approval after it.  They are
    None when the lane's newest word is an approval or when it never spoke.

    THE SCAN ALREADY COMPUTED THE REJECTION AND THREW IT AWAY.  `lane_shas`
    detected a rejection precisely enough to clear the approval list, then
    returned only the list, so a rejected lane and a silent one both surfaced
    as ``[]``.  hermit-906 rebuilt the review inventory BY HAND against live
    pull requests because the parser underneath it could not express the
    difference.  Nothing new is being detected here; the detection is simply
    no longer discarded.
    """

    __slots__ = ("approved_shas", "rejected_sha", "rejected_idx")

    def __init__(
        self,
        approved_shas: list[str],
        rejected_sha: str | None = None,
        rejected_idx: int = -1,
    ) -> None:
        self.approved_shas = approved_shas
        self.rejected_sha = rejected_sha
        self.rejected_idx = rejected_idx

    @property
    def spoke(self) -> bool:
        return bool(self.approved_shas) or self.rejected_sha is not None

    def __repr__(self) -> str:  # diagnostics only
        return (
            f"LaneVerdict(approved_shas={self.approved_shas!r}, "
            f"rejected_sha={self.rejected_sha!r}, "
            f"rejected_idx={self.rejected_idx!r})"
        )


def classify_lane(head: str, verdict: LaneVerdict) -> str:
    """Five states, where `classify_approval` can express three.

    NEVER                the lane emitted no parseable verdict at all
    BOUND_CURRENT        newest verdict is an approval naming ``head``
    SUPERSEDED           newest verdict is an approval naming another commit
    REJECTED_CURRENT     newest verdict is a rejection naming ``head``
    REJECTED_SUPERSEDED  newest verdict is a rejection naming another commit

    NEVER exists as its own name rather than reusing UNDETERMINED because the
    two answer different questions.  UNDETERMINED means "this gate cannot
    confirm an approval", which is true of a rejected lane and a silent one
    alike.  NEVER means "nobody has looked", which is true of only one of
    them -- and it is the value that routes a dispatch.

    THE ROUTING CONSEQUENCE IS THE REASON TO BE CAREFUL WITH IT.  A lane
    reading REJECTED_CURRENT leaves the "needs a reviewer" queue and joins
    "author must fix".  That is right only when the findings genuinely reached
    the author.  On hermit#2216 the author held every finding while the PR had
    never had an independent look, so recording a rejection there would have
    concealed the absence of review instead of reporting it.  A consumer that
    maps REJECTED_* back onto NEVER, or NEVER onto REJECTED_*, reintroduces
    the collapse this exists to remove.
    """
    if verdict.approved_shas:
        return BOUND_CURRENT if verdict.approved_shas[-1] == head else SUPERSEDED
    if verdict.rejected_sha is not None:
        return (
            REJECTED_CURRENT
            if verdict.rejected_sha == head
            else REJECTED_SUPERSEDED
        )
    return NEVER


def lane_row(
    *,
    number: int,
    label: str,
    lane: str,
    head: str,
    verdict: LaneVerdict,
    runs_at_head: int,
    mergeable: Any,
) -> dict[str, Any]:
    """The ONE row shape, built in one place.

    There were two copies of this dict -- one in `collect_live`, one in
    `collect_fixture` -- and only the fixture path is reachable offline, so
    only the fixture path had a test.  A mutation deleting ``lane_state`` from
    the LIVE copy survived the whole suite: the key existed where it was
    tested and was gone where it was not.  Two constructors of one shape with
    asymmetric coverage is the same divergence that produced this task, so
    they are now a single function that both paths call.

    `state` keeps the three-value GATE vocabulary exactly. Every consumer
    reads that key, and a gate must refuse on a rejection and a silence
    alike. `lane_state` is additive and carries the distinction.
    """

    shas = verdict.approved_shas
    return {
        "number": number,
        "label": label,
        "lane": lane,
        "head": head,
        "bound_to": shas[-1] if shas else None,
        "state": classify_approval(head, shas),
        "lane_state": classify_lane(head, verdict),
        "rejected_at": verdict.rejected_sha,
        "runs_at_head": runs_at_head,
        "mergeable": mergeable,
    }


def gh_json(args: list[str]) -> Any:
    out = subprocess.run(
        ["with-proxy", "gh", *args], capture_output=True, text=True, timeout=300
    )
    if out.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)} failed: {out.stderr.strip()[:300]}")
    return json.loads(out.stdout or "null")


def lane_shas(
    comments: list[dict[str, Any]],
    lane: str,
    malformed: list[tuple[str, str]] | None = None,
    pr_number: int | None = None,
    malformed_prs: dict[tuple[str, str], int] | None = None,
) -> list[str]:
    """The approval SHAs from `lane_verdict`. Signature and result unchanged.

    Kept as the name every existing caller already uses, and kept as a THIN
    WRAPPER rather than a second scan.  Two scans of one grammar is how
    `lane_shas` and the review inventory drifted apart in the first place --
    the parser discarded rejections while the tooling above it tried to reason
    about them.  A copy here would start the next divergence.
    """

    return lane_verdict(comments, lane, malformed, pr_number, malformed_prs).approved_shas


def lane_verdict(
    comments: list[dict[str, Any]],
    lane: str,
    malformed: list[tuple[str, str]] | None = None,
    pr_number: int | None = None,
    malformed_prs: dict[tuple[str, str], int] | None = None,
) -> LaneVerdict:
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

    # THE PREFIX CLASS IS ON THE REJECTIONS AND DELIBERATELY NOT ON `approve`.
    # That asymmetry is the whole fix, so it must not be "tidied" into symmetry.
    #
    # `undecorate` above removes markdown that WRAPS a line -- matching opener
    # and closer. A markdown STRUCTURAL marker (`##`, `>`, `-`, `*`, `+`) is a
    # prefix with NO closer, so it survives undecoration and, before this
    # change, matched nothing. A rejection written that way was invisible:
    #
    #     [APPROVED-AT: claude <head>, "## CHANGES-REQUESTED-AT: claude <head>"]
    #       -> lane_shas == ["<head>"]  =>  BOUND_CURRENT, --gate rc=0, state=ok
    #
    # The lane read as currently approved while its newest verdict was a
    # REJECTION AT THE SAME HEAD. That is worse than the stale-label bug this
    # verifier exists to catch: a stale label can be spotted, whereas this
    # returns a correctly-bound approval and looks perfectly healthy. Measured
    # live on hermit#2176, where a heading-prefixed Claude-lane rejection sat
    # underneath two standing approval labels.
    #
    # Each direction is the REFUSING one:
    #   * a prefixed REJECTION is honoured, so it revokes. Being generous about
    #     recognising a rejection can only withdraw authority, never grant it.
    #   * a prefixed APPROVAL still does NOT bind -- `approve` keeps its strict
    #     anchor. Extending the class to it would let `## APPROVED-AT: ...`
    #     start authorising, which is the direction that loses.
    # A prefixed approval instead falls through to `suspect` below and is
    # reported as a malformed source line.
    REJECT_PREFIX = r"[\s#>*+-]*"
    reject_legacy = re.compile(
        rf"^{REJECT_PREFIX}REQUEST\s+CHANGES\s+AT\s+[0-9a-f]{{40}}$", re.IGNORECASE
    )
    reject_current = re.compile(
        rf"^{REJECT_PREFIX}CHANGES-REQUESTED-AT:\s*(claude|codex)\s+[0-9a-f]{{40}}$",
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
    #
    # THE LEADING `[#>*+-]` CLASS CLOSES A FALSE PASS, and it is the reason this
    # pattern is not simply anchored at the keyword. `undecorate` above removes
    # markdown that WRAPS a line -- a matching opener and closer. A markdown
    # STRUCTURAL marker is a prefix with NO closer, so `## CHANGES-REQUESTED-AT:
    # claude <sha>` survives undecoration intact and then matches neither the
    # verdict grammar nor (without this class) the suspect pattern. It was
    # therefore invisible, and the consequence was not a missing approval but a
    # WRONG one:
    #
    #     [APPROVED-AT: claude <head>, "## CHANGES-REQUESTED-AT: claude <head>"]
    #       -> lane_shas == ["<head>"], malformed == []   =>  BOUND_CURRENT
    #
    # The lane read as currently approved while its newest verdict was a
    # rejection AT THE SAME HEAD. That is strictly worse than the stale-label
    # bug this verifier exists to catch: a stale label can be noticed, whereas
    # this returns a correctly-bound approval and looks healthy. Measured live
    # on hermit#2176, where a heading-prefixed Claude-lane rejection sat under
    # two standing approval labels.
    #
    # A prefixed line is REPORTED, never promoted: it must not bind either, or
    # `## APPROVED-AT: ...` would start authorising, which is the wrong
    # direction. hermit's `scripts/core-review-protocol-lint.sh` carries the
    # same class so the two verifiers agree.
    suspect = re.compile(
        r"^[\s#>*+-]*"
        r"(?:APPROV|CHANGES-REQUESTED|REQUEST\s+CHANGES|REJECT|LGTM|SIGN(?:ED)?[-\s]?OFF|ACK)"
        r"[^\n]*\b[0-9a-f]{40}\b",
        re.IGNORECASE,
    )

    # Malformed lines are buffered WITH the index of the comment they came from,
    # because whether one matters depends on where it sits relative to this
    # lane's newest binding. Reported at the end, once that position is known.
    found: list[str] = []
    newest_binding_idx = -1
    rejected_sha: str | None = None
    rejected_idx = -1
    candidates: list[tuple[int, tuple[str, str]]] = []
    for idx, c in enumerate(comments):
        body = c.get("body") or ""
        lines = [undecorate(l) for l in body.split("\n")]
        # A rejection contributes NOTHING, even if the same comment also carries
        # an APPROVED-AT-shaped line -- a comment quoting the convention, or
        # quoting the approval it supersedes, must not bind as a positive.
        #
        # The SHA is captured as well as the fact. It was always available on
        # the matched line and was simply dropped, which is what made a
        # rejected lane indistinguishable from a silent one. The LAST matching
        # line in the comment wins: within one body, later is newer.
        rejection_here: str | None = None
        for line in lines:
            legacy = reject_legacy.match(line)
            if legacy is not None:
                # The historical form names no lane, so it revokes BOTH. That
                # breadth is deliberate and predates this change.
                sha = SHA40.search(line.lower())
                rejection_here = sha.group(0) if sha else ""
                continue
            current = reject_current.match(line)
            if current is not None and current.group(1).lower() == lane:
                sha = SHA40.search(line.lower())
                rejection_here = sha.group(0) if sha else ""
        if rejection_here is not None:
            # Verdicts are chronological. A later exact-head request for changes
            # revokes this lane's earlier approval; a still-later approval can
            # establish authority again. Clearing rather than merely skipping
            # is load-bearing: otherwise APPROVED then CHANGES-REQUESTED reads
            # as approved forever.
            #
            # UNCHANGED by the three-state work, and revocation stays
            # SHA-INDEPENDENT: any rejection clears, whichever commit it names,
            # because ordering is what revokes. Recording which SHA it named
            # adds a report; it does not add a condition.
            found.clear()
            newest_binding_idx = -1
            rejected_sha = rejection_here or None
            rejected_idx = idx
            continue
        for line in lines:
            m = approve.match(line)
            if not m:
                if (
                    suspect.match(line)
                    and not reject_legacy.match(line)
                    and not reject_current.match(line)
                ):
                    candidates.append(
                        (idx, (c.get("html_url") or c.get("url") or "?", line[:120]))
                    )
                continue
            explicit_lane, sha = m.group(1), m.group(2)
            if explicit_lane.lower() == lane:
                found.append(sha)
                newest_binding_idx = idx
                # An approval AFTER a rejection re-establishes authority, so
                # that rejection is no longer this lane's newest word. Mirrors
                # the clearing above in the other direction; without it a lane
                # that was rejected and then correctly re-approved would keep
                # reporting a rejection it has already spoken past.
                rejected_sha = None
                rejected_idx = -1
                rejected_idx = -1

    # SUPERSEDED HISTORY IS NOT AN OPEN QUESTION. An unparseable verdict line
    # matters only if it could express an opinion this lane has not already
    # spoken past. One in a comment OLDER than the lane's newest binding cannot:
    # whatever it was trying to say, the lane said something later and parseable.
    #
    # A blanket "any malformed line gates" rule is what this replaces, and it
    # made the gate PERMANENTLY UNSATISFIABLE for the PRs that need it. #2176
    # and #2172 hold four prose verdict headlines between them, already in their
    # comment history; under a blanket rule no amount of correct re-approval
    # would ever clear them. A gate that cannot be satisfied by doing the right
    # thing gets routed around, and then it protects nothing.
    #
    # AT the newest binding still counts: a bad line in the same comment as the
    # binding has not been spoken past, and could be the rejection the parser
    # failed to read. So does everything after it, and everything when the lane
    # never bound at all.
    for idx, finding in candidates:
        if newest_binding_idx >= 0 and idx < newest_binding_idx:
            continue
        # collect_live checks the same comment once per approval lane. Count a
        # malformed source line once, not once per label, or one defect becomes
        # a confident count of two. A line one lane has superseded is still
        # reported if another lane has not -- the union is what gates.
        if finding not in malformed:
            malformed.append(finding)
        if pr_number is not None and malformed_prs is not None:
            malformed_prs[finding] = pr_number
    return LaneVerdict(found, rejected_sha, rejected_idx)


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
        if not approvals and pr_number is None:
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
            verdict = lane_verdict(
                comments, lane, malformed, pr["number"], malformed_prs
            )
            shas = verdict.approved_shas
            rows.append(
                lane_row(
                    number=pr["number"],
                    label=label,
                    lane=lane,
                    head=head,
                    verdict=verdict,
                    runs_at_head=len(runs),
                    mergeable=pr.get("mergeable"),
                )
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
            if not label.startswith("passed-review-"):
                continue
            lane = label.rsplit("-", 1)[-1]
            if "comments" in pr:
                verdict = lane_verdict(
                    comments,
                    lane,
                    malformed,
                    pr["number"],
                    malformed_prs,
                )
            else:
                # A fixture supplying only `lane_shas` cannot express a
                # rejection, so this reads NEVER rather than REJECTED_*. That
                # is the honest answer for such a fixture -- the input carries
                # no rejection to report -- and a fixture wanting the third
                # state supplies `comments` instead.
                verdict = LaneVerdict(pr.get("lane_shas", {}).get(lane, []))
            rows.append(
                lane_row(
                    number=pr["number"],
                    label=label,
                    lane=lane,
                    head=pr["head"],
                    verdict=verdict,
                    runs_at_head=pr.get("runs_at_head", 1),
                    mergeable=pr.get("mergeable"),
                )
            )
    return rows, selected


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
    malformed_pr_by_source: dict[tuple[str, str], int] = {}
    rows, selected = (
        collect_fixture(a.fixture, malformed, malformed_pr_by_source, a.pr)
        if a.fixture
        else collect_live(a.repo, malformed, malformed_pr_by_source, a.pr)
    )
    # The final landing boundary always selects exactly one PR with --pr.  An
    # empty row set there means the PR supplied no passed-review-* cache label
    # at all.  That is absence of review evidence, not a population with zero
    # defects.  Fleet-wide scans deliberately retain their historical scope:
    # they audit approval labels that exist and do not infer which open PRs
    # require review.
    approval_set_missing = a.pr is not None and not rows
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
        if row["state"] == BOUND_CURRENT
    }
    critical_lanes_missing = (
        sorted(set(LANES) - authoritative_lanes) if critical_required else []
    )
    bad = [r for r in rows if r["state"] != BOUND_CURRENT]
    authoritative = len(rows) - len(bad)
    malformed_prs = set(malformed_pr_by_source.values())
    malformed_affected_labels = {
        (r["number"], r["label"]) for r in rows if r["number"] in malformed_prs
    }
    # The composed state: an approval that no longer holds, on a head nothing
    # evaluated. Approved, unevaluated, and indistinguishable from ready.
    combined = [r for r in bad if r["runs_at_head"] == 0]

    if a.as_json:
        print(json.dumps({"approvals": len(rows), "authoritative": authoritative,
                          "approval_set_missing": approval_set_missing,
                          "critical_required": critical_required,
                          "critical_classes": sorted(selected_critical_classes),
                          "critical_label_missing": critical_label_missing,
                          "critical_lanes_missing": critical_lanes_missing,
                          "required_approval_sets": 1 if a.pr is not None else 0,
                          "approval_sets_missing": int(approval_set_missing),
                          "authority_absent": len(bad), "not_holding": len(bad),
                          "approved_and_unevaluated": len(combined),
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
            print("  every approval label is bound to the current head")
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
            summary = (
                f"{len(gating)}/{len(rows)} approval labels fail the "
                f"{a.gate_on} binding check; affected={affected}; they do not bind "
                "the current head and are not deleted; remedy: re-review the exact "
                "head and emit APPROVED-AT: <lane> <40-hex-head>"
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
            f"GATE FAIL ({a.gate_on}): {len(gating)} of {len(rows)} approval label(s) do not bind "
            "to the current head. They are NOT deleted -- they simply must not satisfy the review "
            "gate.",
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
