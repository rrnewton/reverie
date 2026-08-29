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

MALFORMED SOURCE LINES ARE ORTHOGONAL TO THOSE STATES. A comment may contain an
incomplete ``APPROVED-AT: <lane>`` assertion and a valid canonical binding at the
same head. The incomplete line remains syntax debt worth reporting, but it cannot turn the
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
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402
import ops_state  # noqa: E402

SHA40 = re.compile(r"\b[0-9a-f]{40}\b")
RETIRES = re.compile(r"\bRETIRES\s+#?(\d{6,})\b", re.IGNORECASE)
LANES = ("claude", "codex")
CRITICAL_CLASSES = frozenset(
    {"hermit-syscall", "detcore-scheduling", "reverie-api"}
)
POST_FACTO_LABEL = "post-facto-human-review"
LOCALLY_VALIDATED_LABEL = "locally-validated"

BOUND_CURRENT = "BOUND_CURRENT"
SUPERSEDED = "SUPERSEDED"
UNDETERMINED = "UNDETERMINED"
REFUSED = "REFUSED"
REFUSED_STALE = "REFUSED_STALE"


class GitHubUnavailable(RuntimeError):
    """The live GitHub census did not complete; no binding verdict exists."""


def _refusal_is_stale(head: str, refused_at: str | None) -> bool:
    """Did the refusal name a SHA that the head has since moved past?

    Conservative on purpose: an unknown or unparsed `refused_at` is NOT stale, so
    a caller that cannot say which SHA was refused gets the blocking state rather
    than the softer one. Absence of evidence must not read as evidence of
    staleness in the direction that unblocks.
    """
    return refused_at is not None and refused_at.lower() != head.lower()


def classify_approval(
    head: str,
    comment_shas: list[str],
    refused: bool = False,
    unanswered: bool = False,
    refused_at: str | None = None,
) -> str:
    """Pure classifier: the seam the bracket drives.

    `comment_shas` are the head SHAs named by that lane's role-tagged review
    comments, oldest first. The NEWEST is what the lane last bound itself to.

    `unanswered` says this lane holds a refusal that its OWN ISSUER never
    discharged. It outranks every approval, and it is a STRICTLY STRONGER
    condition than `refused`: `refused` asks whether the refusal was the lane's
    last word in time, `unanswered` asks whether the agent who wrote it ever
    took it back. A colleague approving afterwards satisfies the first and not
    the second. See `lane_shas`'s `unanswered` out-parameter for who is allowed
    to discharge what.

    `refused` says this lane emitted a CHANGES-REQUESTED-AT that was never
    followed by a fresh approval. IT IS NOT DERIVABLE FROM `comment_shas`: a
    refusal contributes no SHA and clears the ones before it, so without this
    flag "a lane looked and said no" and "nobody in this lane has looked" are
    BOTH the empty list. They render identically and need opposite responses --
    an absence invites a fresh reviewer, which is exactly the wrong answer to a
    head two reviewers already blocked. Measured 2026-08-26 on hermit#2655: two
    lanes, two refusals, two blocking findings, and both lanes reported
    UNDETERMINED, indistinguishable from a pull request nobody had opened.

    `refused_at` is the SHA the refusal named. When it is not the current head
    the state is `REFUSED_STALE` instead of `REFUSED`.

    ⚠️ `REFUSED_STALE` STILL BLOCKS, AND THAT IS THE POINT OF SPELLING IT OUT.
    It reports that the head moved after the refusal was written, NOT that the
    refusal stopped applying. Making a stale refusal stop blocking would hand
    every author a one-commit bypass: push anything, and the objection
    evaporates unanswered. That is the "outrun rather than answered" failure,
    and it is worse than the illegibility this replaces. Measured 2026-08-26 on
    hermit#2680: a refusal at `099290c1` still suppressed the lane at
    `642a2990` after the finding had been fixed and pushed -- correct behaviour,
    but indistinguishable from a refusal nobody had addressed, so the head read
    as freshly blocked and was re-dispatched as such.

    WHO CLEARS IT IS UNCHANGED: its own issuer, or a later approval that
    `unanswered` permits. A moved head is a REASON TO GO LOOK AGAIN, not a
    discharge.

    ⚠️ THIS RENAMES A STATE, IT DOES NOT MOVE AN ADMISSION BOUNDARY. Every
    caller that grants authority tests `== BOUND_CURRENT`, and neither REFUSED
    nor REFUSED_STALE is BOUND_CURRENT, so nothing becomes landable that was not
    landable before. `relevant_rejection` still never compares the refused SHA to
    head when deciding whether the lane is cleared; the comparison added here is
    for REPORTING only.
    """
    # ⚠️ CHECKED BEFORE THE SHAs, AND THAT ORDER IS THE WHOLE POINT OF
    # `unanswered`. A refusal nobody discharged outranks any number of
    # approvals, including approvals that name this exact head, because the
    # approvals did not answer it -- they arrived beside it. `refused` cannot
    # carry this: a refusal followed by a fresh approval repopulates
    # `comment_shas`, so by the time the empty-list branch is reached the
    # override has already happened and the lane reads BOUND_CURRENT.
    #
    # Measured 2026-08-26 on hermit#2678, claude lane, one unmoved head:
    #     01:52:02Z  CHANGES-REQUESTED-AT  agent(hermit-007)
    #     01:54:39Z  APPROVED-AT           a different, unsigned reviewer
    # The lane read BOUND_CURRENT and carried `passed-review-claude`, while
    # hermit-007's blocking finding stood unanswered. The approving agent said
    # in their own comment that they saw "zero comments when I started", so
    # nobody overrode anything deliberately -- which is exactly why the grammar
    # has to hold the line instead of the reviewers' attention.
    if unanswered:
        return REFUSED_STALE if _refusal_is_stale(head, refused_at) else REFUSED
    if not comment_shas:
        # Order matters: a refusal followed by a fresh approval repopulates
        # `comment_shas`, so reaching here with `refused` set means the refusal
        # was the lane's LAST word.
        if refused:
            return REFUSED_STALE if _refusal_is_stale(head, refused_at) else REFUSED
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
    the arithmetic testable without a network. The live collector currently
    supplies only PR numbers, so absence of ``approved_at`` is a missing
    measurement, not an observed zero.

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
    associated_prs = [pr for landing in via_pr for pr in landing["prs"]]
    binding_coverage_measured = all(
        isinstance(pr.get("approved_at"), list) for pr in associated_prs
    )
    # "carried a binding" is deliberately NOT "bound to the landed sha": a
    # squash merge gives the commit on main a different sha from the PR head it
    # came from, so requiring equality here would report every squash as
    # unbound and the line would be ignored within a week.
    with_binding = (
        [l for l in via_pr if any(pr["approved_at"] for pr in l["prs"])]
        if binding_coverage_measured
        else []
    )
    binding_count: int | str = (
        len(with_binding) if binding_coverage_measured else "NOT MEASURED"
    )
    if binding_coverage_measured:
        binding_summary = (
            f"{len(via_pr)} took the PR path, of which {len(with_binding)} came "
            "from a PR carrying an APPROVED-AT binding."
        )
    else:
        binding_summary = (
            f"{len(via_pr)} took the PR path; APPROVED-AT binding coverage is "
            "NOT MEASURED because the collector did not populate approved_at."
        )
    return {
        "window_commits": total,
        "landed_via_pr": len(via_pr),
        "landed_without_pr": len(without_pr),
        "pr_landings_carrying_a_binding": binding_count,
        "summary": (
            f"{len(without_pr)} of {total} commits landed without a pull request, so this "
            f"check could not observe them; {binding_summary} "
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


def github_actions_enabled(repo: str) -> bool:
    """Return this host's configured Actions precondition, or refuse.

    ``runs_at_head == 0`` means a head was not evaluated only while Actions are
    enabled. When this team has deliberately disabled Actions, zero is expected
    and cannot support the stronger APPROVED + UNEVALUATED claim. The local
    ops-state is authoritative for that operational choice; GitHub's repository
    permission may remain enabled while every workflow is deliberately dormant.
    """
    del repo
    try:
        enabled, _directive = ops_state.github_actions_policy()
    except ops_state.OpsStateError as error:
        raise GitHubUnavailable(str(error)) from error
    return enabled


# Leading block-level markdown that can decorate an otherwise-whole-line verdict:
# ATX headings and unordered-list bullets. A blockquote is deliberately absent:
# it attributes the line as quoted text, so it is a mention rather than this
# comment's assertion. Bullet forms require the trailing space so that
# `*emphasis*` is left to `undecorate`'s paired handling.
_BLOCK_PREFIX = re.compile(r"^(?:#{1,6}\s+|[-+*]\s+)")


def _undecorate(line: str) -> str:
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

def _undecorate_block(line: str) -> str:
    """`_undecorate`, plus LEADING block-level markdown: ``##``, ``>``, ``-``.

    WHY THIS CANNOT MANUFACTURE A FALSE POSITIVE, which is the only reason
    it is safe to strip more: removing a prefix does not decide anything.
    After stripping, the strict grammar still has to match the whole
    remaining line -- keyword, colon, lane, and a 40-hex, anchored at both
    ends. There is no string that becomes a verdict by having ``##``
    removed from it that was not already a verdict wearing a hat. Stripping
    can only ever move a line from "unparsed" to "parsed as exactly what it
    says"; it can never change what a parsed line says.

    The converse is what made this necessary. `_undecorate` handled paired
    emphasis but not block-level prefixes, so a verdict written as a
    Markdown heading fell through to silence -- and silence is not neutral
    here. A lost APPROVED-AT reads as ABSENT (no authority granted, safe); a
    lost CHANGES-REQUESTED-AT leaves the lane's EARLIER approval standing as
    the newest binding, so the tool reports the lane SATISFIED while a live
    exact-head refusal sits unparsed on the same pull request. Same cause,
    opposite consequences, and only one of them is safe.

    The heading form really did fall through, and closing it is right. But it
    explains NEITHER incident this docstring used to attribute to it, and the
    retracted claim is recorded here rather than deleted, because a wrong
    explanation in the durable record is worse than none: it stops the next
    reader looking.

    ⚠️ RETRACTED, MEASURED 2026-08-25. This docstring previously said that on
    https://github.com/rrnewton/hermit/pull/2176 a heading-form refusal did not
    bind and "the 18:15Z approval stayed authoritative for twelve days", and
    that this was "the mechanism behind pull/2591 merging over a valid block".
    Three separate parts of that are false:

    1. ON 2176 A PLAIN, COLUMN-0 REFUSAL EXISTS 87 SECONDS LATER. The reviewer
       noticed the heading and re-posted it unadorned at 2026-08-13T01:28:43Z,
       naming the same head. That form binds under the PRE-FIX grammar, so the
       heading defect changed nothing about what was detectable there.

    2. THE APPROVAL DID NOT STAY AUTHORITATIVE FOR TWELVE DAYS. From the label
       event log, ``passed-review-claude`` and ``passed-review-codex`` were both
       removed at 2026-08-13T05:17:57Z -- three hours forty-nine minutes after
       the refusal, not twelve days. Detection on 2176 WORKED. The twelve days
       is how long a correctly-blocked head has sat with nobody clearing it: the
       head b3ca4e155098 has not moved since 2026-08-12T18:09:58Z, no fix was
       pushed, and no CHANGES-REQUESTED-WITHDRAWN-AT was ever posted. That is an
       unresolved block, not a missed one, and 2176 is not evidence of any
       detection failure at all.

    3. THE HEADING FORM DOES NOT EXPLAIN 2591 EITHER. Its timeline:
           17:55:19Z  APPROVED-AT: claude 86b76b82        (column 0)
           18:05:19Z  CHANGES-REQUESTED-AT: claude 86b76b82  (COLUMN 0, plain)
           18:09:01Z  merged
           18:19:11Z  a decorated refusal, AFTER the merge
       A plain binding refusal was live 3m42s before the merge. Grammar cannot
       account for a merge over a marker that parses.

    ⚠️ THE ACTUAL MECHANISM: THIS MODULE HAS NO ENFORCING CONSUMER ON THE MERGE
    PATH. ``grep -rn CHANGES-REQUESTED-AT`` across every .py/.sh/.rs in the
    repository returns hits in this file and in its OWN TESTS, and nowhere else.
    No lander, no merge gate and no workflow reads a refusal marker. This module
    is reached only by ``ci-hub/health/pr_status.py``, which reports. The merge
    itself is ``gh pr merge`` run by an agent, so the only thing standing between
    a live refusal and a landing is whether that agent read the comments.

    2591 confirms it from the other side: at merge time it carried NO
    ``passed-review-claude`` label either -- that label was applied at
    18:12:00Z, three minutes AFTER the merge. Neither the marker path nor the
    label path was consulted.

    So the grammar fix removes a real fail-open from the REPORT, and changes
    nothing about what can be merged. Until a pre-merge caller exists, this
    module can only observe.
    """
    prev = None
    while prev != line:
        prev = line
        line = _BLOCK_PREFIX.sub("", line.strip())
        line = _undecorate(line)
    return line


# ---------------------------------------------------------------------------
# THE SHARED GRAMMAR. One definition, several policies.
#
# WHY THIS IS MODULE-LEVEL AND PUBLIC. `ci-hub/bin/gh-merge-verified` had its own
# copy of this parsing, so there were TWO parsers for one grammar and they had
# already drifted. Sweep check 14 is about exactly this shape, and the truncation
# rule existing three ways is the same defect in another file. A landing guard and
# a review board are ENTITLED to different POLICY -- the lander deliberately
# over-blocks, honours CHANGES-REQUESTED-WITHDRAWN-AT, and respects GitHub's
# native CHANGES_REQUESTED review state, none of which this module does -- but
# they must not disagree about what a marker line SAYS. Policy may differ; the
# grammar may not.
#
# So callers get the classification from here and apply their own policy on top.

MARKER_APPROVED = "APPROVED-AT"
MARKER_REFUSED = "CHANGES-REQUESTED-AT"
MARKER_WITHDRAWN = "CHANGES-REQUESTED-WITHDRAWN-AT"

# An agent id, defined ONCE. `_DISCLOSURE` below reads this shape out of a
# comment's `[team, agent, ...]` tag; the marker's `BY` clause reads it out of the
# binding line itself. Those are the two identity sources this module has, and two
# sources that disagreed about what an agent id looks like would resolve the same
# reviewer two ways -- the exact drift this module exists to prevent. One
# definition, so they cannot.
_AGENT_ID = r"[a-z0-9][a-z0-9-]*"

# ⚠️ `BY <agent>` IS OPTIONAL, AND THIS READER LEARNS IT BEFORE ANY WRITER EMITS IT.
#
# THE LANE IS NOT THE REVIEWER. `claude` and `codex` name a HARNESS FAMILY, and a
# fleet of nineteen agents shares two lane names, so "two lanes bound this head"
# has never been evidence that two reviewers looked at it. The same agent can bind
# both lanes and the record reads as independent review. Nothing in the grammar
# could contradict that, because the binding did not carry who issued it.
#
# ⚠️ THE ORDERING IS THE WHOLE SAFETY ARGUMENT, and it is the reason this clause
# arrives in a commit that emits nothing. A marker is a string in a GitHub comment
# read by several independent programs; there is no schema handshake and no way to
# upgrade them together. `$` here meant a marker carrying a trailing clause matched
# NOTHING -- and the failure was not symmetric. An approval falls through to
# the malformed-marker check and fails to bind: loud, and safe. A
# REFUSAL that goes malformed still refuses, so it blocks a head nobody refused.
# Emitting first would have manufactured both. Every reader tolerates the field
# first; only then may a writer produce one.
#
# Group numbering is deliberately undisturbed -- the clause is a NAMED group added
# after group 3 -- so `parse_marker`'s existing 3-tuple contract, and every consumer
# of it, is unchanged by this edit. The issuer is read through `marker_issuer`.
# Longest first: CHANGES-REQUESTED-WITHDRAWN-AT must not be read as a refusal.
_MARKER_PATTERN = (
    r"(CHANGES-REQUESTED-WITHDRAWN-AT|CHANGES-REQUESTED-AT|APPROVED-AT):"
    r"\s*(claude|codex)\s+([0-9a-f]{40})"
    rf"(?:\s+BY\s+(?P<issuer>{_AGENT_ID}))?"
)
_MARKER_RE = re.compile(rf"^{_MARKER_PATTERN}$", re.IGNORECASE)
_MARKER_PREFIX_RE = re.compile(
    rf"{_MARKER_PATTERN}(?=$|[\s.,;!?—-])",
    re.IGNORECASE,
)


def strip_decoration(line: str) -> str:
    """Normalise a candidate marker line: whole-line emphasis AND block prefixes.

    Removing decoration cannot manufacture a false positive, which is the only
    reason it is safe to strip: the anchored grammar still has to match the whole
    remaining line afterwards. Nothing becomes a verdict by losing a ``##`` unless
    it already was one.
    """
    return _undecorate_block(line)


# A malformed marker is still an assertion when the review vocabulary starts the
# normalized line. This is deliberately independent of SHA presence or length:
# ``APPROVED-AT: claude`` is an incomplete assertion, while prose that mentions
# ``APPROVED-AT: claude <sha>`` is not a verdict. Existing lane headings put the
# marker directly after the heading. A writer publishing a verdict with other
# surrounding prose records that choice with the existing ``VERDICT:`` prefix;
# readers do not guess from arbitrary prose around a token.
_MARKER_ASSERTION_RE = re.compile(
    r"(?:APPROVED-AT|APPROVED(?=\s+(?:@|[-—]))|"
    r"APPROVE(?=\s+\S+\s*$)|"
    r"CHANGES-REQUESTED(?:-WITHDRAWN)?(?:-AT)?|"
    r"CHANGES\s+REQUESTED(?=\s*(?::|for\b))|REQUEST\s+CHANGES\s+AT|"
    r"REJECT(?:ED)?(?=\s*:)|LGTM(?=\s+\S+\s*$)|"
    r"SIGN(?:ED)?[-\s]?OFF(?=\s+\S+\s*$)|ACK(?=\s+\S+\s*$))"
    r"(?![-\w])",
    re.IGNORECASE,
)

_LANE_HEADING_RE = re.compile(
    r"^(?:\*\*)?(?:claude|codex)(?:-lane|\s+lane)(?:\*\*)?:?\s+",
    re.IGNORECASE,
)
_VERDICT_LEAD_RE = re.compile(r"^VERDICT:\s*", re.IGNORECASE)


def marker_occurrences(line: str) -> list[int]:
    """Start offsets of review-marker spellings in the normalized line."""
    normalized = strip_decoration(line)
    return [match.start() for match in _MARKER_ASSERTION_RE.finditer(normalized)]


def marker_assertion_starts(line: str) -> list[int]:
    """Every review marker asserted by the line's syntactic context.

    A normal line has at most one such position. ``VERDICT:`` makes marker text
    after it an assertion, so returning every occurrence lets callers refuse an
    ambiguous line instead of silently selecting the first marker.
    """
    normalized = strip_decoration(line)
    if normalized.startswith(">"):
        return []
    match = _MARKER_ASSERTION_RE.match(normalized)
    if match is not None:
        return [match.start()]
    lane_heading = _LANE_HEADING_RE.match(normalized)
    if lane_heading is not None:
        match = _MARKER_ASSERTION_RE.match(normalized, lane_heading.end())
        return [match.start()] if match is not None else []
    verdict = _VERDICT_LEAD_RE.match(normalized)
    if verdict is not None:
        return [
            match.start()
            for match in _MARKER_ASSERTION_RE.finditer(normalized, verdict.end())
        ]
    return []


def marker_assertion_start(line: str) -> "int | None":
    """The sole asserted marker's start, or ``None`` if absent or ambiguous."""
    starts = marker_assertion_starts(line)
    return starts[0] if len(starts) == 1 else None


def marker_assertion(line: str) -> bool:
    """Whether ``line`` asserts a review marker rather than mentioning one."""
    return bool(marker_assertion_starts(line))


# ---------------------------------------------------------------------------
# WHICH LINES OF A COMMENT MAY CARRY A VERDICT AT ALL.
#
# ⚠️ AN ILLUSTRATION IS NOT AN INSTRUCTION, AND TREATING IT AS ONE COST REAL
# LANDINGS. Measured 2026-08-25 across all 28 open pull requests, at each head,
# through both landed lander paths: `gh-merge-verified` refused 12 heads of which
# SIX carried no refusal naming that head, and `changes-requested-gate` refused 7
# of which one did. hermit#2611 was refused by both while approved by both lanes;
# what refused it was a fenced example in a comment TEACHING a reviewer the
# grammar.
#
# ⚠️ AND THE ORDINARY CLEARING PATH DOES NOT WORK ON THESE. A refusal naming a
# sha stops naming it when the author pushes. An example carrying no sha, or one
# quoting some other head's sha, never named this head and so never stops
# applying: the refusal is PERMANENT and no author action removes it. That is why
# this is a correctness fix and not a noise reduction.
#
# ⚠️ THE RULE IS SYMMETRIC, AND THAT IS THE POINT. A fenced APPROVED-AT must not
# bind either. Excluding fenced refusals while still honouring fenced approvals
# would swap one asymmetry for another and would newly PERMIT merges -- a comment
# quoting the approval grammar would grant a lane. Both kinds are read from the
# same lines, so both change together, and the direction stays conservative: this
# makes strictly FEWER lines eligible to say anything.
#
# The discriminator is markdown's own, not a heuristic about wording: a verdict
# must be asserted in prose. Fenced blocks (``` and ~~~) and indented code blocks
# are where people put examples, transcripts and templates, and nobody records a
# real decision inside one.
_FENCE_RE = re.compile(r"^ {0,3}(?P<f>`{3,}|~{3,})\s*(?P<info>.*)$")


def prose_lines(body: str) -> list[str]:
    """The lines of ``body`` that are outside every code block.

    Fenced blocks follow CommonMark closely enough for review comments: an
    opening fence of three or more backticks or tildes, closed by a fence of the
    SAME character and at least the same length. An unclosed fence runs to the
    end of the comment -- deliberately, because that is how the renderer shows it
    to a human, and the gate must agree with what the reader sees.

    ⚠️ INDENTED BLOCKS REQUIRE A BLANK LINE BEFORE THEM, which is what keeps this
    from eating real verdicts. A four-space indent is only a code block when it
    opens one; an indented continuation inside a list item is not, and neither is
    a marker somebody happened to indent under a bullet. Without that condition
    this would silently stop honouring bindings that bind today.
    """
    out: list[str] = []
    fence: str | None = None
    indented = False
    prev_blank = True
    for raw in body.split("\n"):
        blank = not raw.strip()
        if fence is not None:
            m = _FENCE_RE.match(raw)
            if m and m.group("f")[0] == fence[0] and len(m.group("f")) >= len(fence) \
                    and not m.group("info").strip():
                fence = None
            prev_blank = blank
            continue
        m = _FENCE_RE.match(raw)
        if m:
            fence = m.group("f")
            indented = False
            prev_blank = False
            continue
        if indented:
            if blank:
                prev_blank = True
                continue
            if raw.startswith("    ") or raw.startswith("\t"):
                continue
            indented = False
        elif prev_blank and (raw.startswith("    ") or raw.startswith("\t")) and not blank:
            indented = True
            prev_blank = False
            continue
        out.append(raw)
        prev_blank = blank
    return out


def marker_lines(body: str) -> list[str]:
    """Every prose line of ``body`` that may contain review-marker text.

    This excludes fenced and indented code blocks once for every consumer.
    ``marker_assertion`` then distinguishes an asserted marker from a prose
    mention, and ``parse_marker`` decides whether an assertion is canonical.
    `gh-merge-verified`, `changes-requested-gate` and this module's own label
    reader share all three steps.
    """
    return prose_lines(body)


# ⚠️ A TEMPLATE IS NOT A VERDICT EITHER, and this rule belongs beside the fence
# rule rather than in one lander. `agent(hermit-007)` landed it in
# ci-hub/bin/changes-requested-gate at 32faa7e2 after a template line refused
# hermit#2611; measured immediately afterwards, gh-merge-verified still refused
# the same line, because the fix lived in one of the two copies. Same grammar,
# two homes, immediate divergence -- so it moves here and both call it.
#
# The discriminator is narrow on purpose: an ANGLE-BRACKET PLACEHOLDER where the
# sha belongs. A reviewer refusing a head writes a sha, or writes prose; they do
# not write `<sha>`. So `CHANGES-REQUESTED-AT: claude -- see above` is still
# genuinely unbindable and still fails closed.
_PLACEHOLDER_RE = re.compile(r"<[^>\n]{1,24}>")


def is_template(tail: str) -> bool:
    """True when the text after a marker token is a placeholder, not a sha."""
    return bool(_PLACEHOLDER_RE.search(tail))


def parse_canonical_marker(line: str) -> tuple[str, str, str] | None:
    """``(kind, lane, sha)`` when the whole normalized line is canonical."""
    match = _MARKER_RE.match(strip_decoration(line))
    if not match:
        return None
    return match.group(1).upper(), match.group(2).lower(), match.group(3)


def _asserted_marker_match(line: str) -> "re.Match[str] | None":
    """Match a canonical marker in the syntactic context that asserts it.

    A bare marker and one after a lane heading retain the existing exact-line
    grammar.  ``VERDICT:`` is different: the writer puts that prefix on a prose
    line precisely to say which canonical marker in the line carries the
    decision, so surrounding prose is allowed there.
    """
    normalized = strip_decoration(line)
    start = marker_assertion_start(normalized)
    if start is None:
        return None
    if _VERDICT_LEAD_RE.match(normalized) is None:
        return _MARKER_RE.match(normalized[start:])
    match = _MARKER_PREFIX_RE.match(normalized, start)
    if match is None:
        return None
    # ``BY`` starts an issuer clause.  If the optional clause did not consume
    # it, the clause is malformed; it is not ordinary trailing prose.
    if match.group("issuer") is None and re.match(
        r"^\s+BY(?:\s|$)", normalized[match.end():], re.IGNORECASE
    ):
        return None
    return match


def parse_marker(line: str) -> tuple[str, str, str] | None:
    """``(kind, lane, sha)`` for a canonical marker in an assertion, else ``None``.

    `kind` is one of the ``MARKER_*`` constants, upper-cased; `lane` is
    lower-cased. Decoration is stripped first. The assertion boundary decides
    whether a canonical marker at the start, after an existing lane heading, or
    after ``VERDICT:`` is a verdict rather than a mention. A ``VERDICT:`` line
    with more than one marker is deliberately not parsed.
    """
    match = _asserted_marker_match(line)
    if not match:
        return None
    return match.group(1).upper(), match.group(2).lower(), match.group(3)


def marker_issuer(line: str) -> "str | None":
    """The agent named in this marker's own ``BY`` clause, or ``None``.

    ``None`` means the LINE does not say -- not that the issuer is unknown. A
    caller that wants the best available answer should fall back to
    `comment_author`, which every agent comment already supplies; `lane_shas`
    does exactly that. Separating the two is what makes the fallback auditable:
    an issuer read from the binding was asserted BY THE BINDING, one read from
    the disclosure tag was inferred from the comment around it, and only the
    first survives being quoted into someone else's comment.
    """
    match = _asserted_marker_match(line)
    if not match:
        return None
    issuer = match.group("issuer")
    return issuer.lower() if issuer else None



# ⚠️ LINE-INITIAL, BUT NOT ONLY THE CANONICAL TOKEN -- AND I NARROWED THIS TOO FAR
# ONCE ALREADY. `ff2a3d83` (mine) converged this fail-closed net onto
# `startswith(MARKER_REFUSED)`, which fixed prose-about-a-refusal blocking a merge
# and SILENTLY DROPPED the near-miss spellings the previous regex caught. Measured
# 2026-08-25, before and after that change:
#
#     form                                     pre-ff2a3d83   post-ff2a3d83
#     CHANGES-REQUESTED-AT: claude <sha>       catches        catches
#     CHANGES-REQUESTED: claude <sha>          catches        MISSES     <- regression
#     CHANGES REQUESTED: claude <sha>          catches        MISSES     <- regression
#     CHANGES-REQUESTED: claude -- see above   catches        MISSES     <- regression
#     prose "... CHANGES-REQUESTED-AT ..."     catches        MISSES     <- the intended fix
#
# Only the last row was the goal. A reviewer who omits `-AT` writes a line every
# human reads as a block, the grammar reports it MALFORMED, and with the net
# narrowed neither lander refused it -- a real block invisible to both. Measured on
# hermit#2611: `CHANGES-REQUESTED: claude 7a75909c92ae` at the exact head, flagged
# and not honoured.
#
# So the net shares ``marker_assertion_start`` and spans the near-miss family.
# Prose containing the marker is a mention. A marker at the start, directly after
# an existing lane heading, or on a line beginning ``VERDICT:`` is an assertion.
# ⚠️ THE TRAILING GUARD IS NOT COSMETIC: WITHOUT IT THIS FIRES ON THE GATE'S OWN
# NAME. `changes-requested-gate` contains the token, `\b` is satisfied by the
# hyphen, and every sentence naming the tool became an unbindable refusal. Measured
# within minutes of shipping it, on https://github.com/rrnewton/hermit/pull/2624 --
# a head with no refusal on it, carrying the line
#     `changes-requested-gate` **CLEAR**; **no markers of any shape on this PR**
# which drove the gate to INDETERMINATE and blocked the merge. A guard that cannot
# admit a comment SAYING IT IS CLEAR is the same defect as one that blocks a pull
# request for explaining the rule, and it was mine.
#
# `(?![-\w])` requires the token to END there -- a verdict is followed by a colon
# or a space, never by `-gate`. The optional `-AT` keeps the canonical form
# matching, and `CHANGES-REQUESTED-WITHDRAWN-AT` now correctly does NOT match,
# since a withdrawal is not a refusal.
_REFUSAL_NEARMISS = re.compile(
    r"(?:CHANGES[-\s_]?REQUESTED(?:-AT)?|REQUEST(?:ING|ED)?\s+CHANGES|REJECT(?:ED)?)"
    r"(?![-\w])",
    re.IGNORECASE,
)


# ⚠️ A VERDICT IS ISSUED ONCE. EVERY LATER COPY OF IT IS A CITATION, AND CITING A
# REFUSAL MUST NOT CREATE ONE.
#
# Measured 2026-08-25 on hermit#2615: the gate counted FIVE refusal markers where
# only TWO verdicts existed. All three phantoms came from GOOD practice -- sentences
# naming exactly which marker still stood, e.g. "my `CHANGES-REQUESTED-AT: claude
# 072ed7b4...` above still stands" and "... is superseded". The sentence saying a
# refusal was SUPERSEDED manufactured another refusal.
#
# ⚠️ AND IT FALSELY BLOCKS, it does not merely over-count. A reviewer who writes
# "my earlier refusal is answered by the fix below" and then approves BLOCKS THE
# HEAD THEY ARE APPROVING, because their own citation is read as a fresh refusal.
#
# THE RULE, and it is deliberately not a rule about characters: an issuer that is
# not this comment's author is a CITATION, however it is written. Formatting cannot
# express that -- a `>` catches only typographically marked quotation and misses an
# inline paste, which is the case that actually happened; a backtick, an indent and
# a heading are each equally silent about who is speaking. What CAN express it is
# document order, which every consumer already has: THE FIRST OCCURRENCE OF A
# VERDICT IS THE ISSUANCE AND EVERY LATER COPY IS A CITATION. The issuer's own
# earlier line is what the citation points AT, so the verdict it names is still
# counted -- from its issuance.
#
# ⚠️ WHY THIS CANNOT LOSE A REFUSAL, which is the only direction that matters here:
# suppression requires an EARLIER occurrence that was itself counted. If the earlier
# copy was invisible for any reason -- fenced, unparsed, in a container this gate
# does not read -- then the later one is the first SEEN occurrence and remains an
# issuance. A refusal can therefore be duplicated away only into itself.
#
# A shortened sha is prefix-compatible on purpose: citations abbreviate, and
# `06c3f3735fbf` naming `06c3f3735fbf...` is the same verdict, not a second one.
# The author is read from the disclosure tag every agent comment opens with,
# `[team, agent, ...]`. It is the only authorship signal available: `user.login` is
# the SHARED account on every one of these comments -- measured on hermit#2625, all
# 24 comments are `rrnewton` -- so GitHub's own author field cannot distinguish two
# reviewers. An `agent(name)` mention in prose is NOT the author; on #2625 it names
# a DIFFERENT agent from the tag in 3 of 8 tagged comments, because agents refer to
# each other constantly.
# ⚠️ A ROLE TAG IN FRONT OF THE DISCLOSURE TAG USED TO LOSE THE AUTHOR ENTIRELY.
# The team field is `[a-z0-9]+`, so `[review agent, claude-opus-5]` cannot match it
# -- a space is not in the class -- and because the tag must start the line, a
# comment opening `[review agent, claude-opus-5] [hermit2, hermit-016, ...]` read
# as having NO author at all. Measured 2026-08-26: three refusals holding three
# open pull requests (#2692, #2670 comment 5420455389, #2680 comment 5420278252)
# were classified UNATTRIBUTED on exactly this, while naming their author in
# plain sight. Under issuer-bound withdrawal an unattributed refusal is
# undischargeable BY ANYONE, so a parser miss becomes a permanent block.
#
# ⚠️ THE LEADING GROUPS ARE SEPARATED BY `[ \t]*`, NOT `\s*`, AND THAT IS THE
# WHOLE SAFETY ARGUMENT. `\s` matches newlines, so `\s*` would let the prefix
# leap from a role tag on line 0 to a tag several lines below it -- including an
# indented paste of SOMEBODY ELSE'S comment. That turns a parser fix into an
# attribution transfer, which is the defect this file exists to prevent.
# Restricting to same-line whitespace keeps the whole match on one line.
#
# ⚠️ AND THE PREFIX IS NON-GREEDY so the FIRST disclosure tag on the line wins.
# Greedy, `[hermit2, hermit-004, ...] [hermit2, hermit-016, ...]` would attribute
# to hermit-016 -- the last tag rather than the writer's own.
#
# A blockquoted or indented tag still does not match, because the line must BEGIN
# with a bracket: `> [hermit2, ...]` and `    [hermit2, ...]` are both rejected.
# Bracketed both ways in tests/test_disclosure_behind_role_tag.py.
_DISCLOSURE = re.compile(
    rf"^(?:\[[^\]\n]*\][ \t]*)*?\[\s*[a-z0-9]+\s*,\s*({_AGENT_ID})\s*,", re.I | re.M
)


def comment_author(body: str) -> "str | None":
    """The agent that WROTE this comment, or None when it does not say."""
    m = _DISCLOSURE.search(body or "")
    return m.group(1).lower() if m else None


def comment_issuer(body: str) -> "str | None":
    """Who issued a comment's marker, or None when the comment does not say.

    A marker's own ``BY`` clause wins over the surrounding disclosure tag. This
    is the same ownership rule used for approvals, withdrawals and refusals; it
    lives here so the two refusal readers cannot disagree about who may retire a
    refusal.
    """
    for line in (body or "").split("\n"):
        named = marker_issuer(line)
        if named:
            return named
    return comment_author(body or "")


def retirement_targets(body: str) -> list[str]:
    """Comment ids named by ``RETIRES`` in ``body``, in document order."""
    return [match.group(1) for match in RETIRES.finditer(body or "")]


def retirement_allowed(
    refusal_issuer: "str | None", claimants: "Sequence[str | None]"
) -> bool:
    """Whether any claimant may retire a refusal with this issuer.

    A refusal with a readable issuer may be retired only by that issuer. A
    refusal with no readable issuer keeps the existing escape hatch: any
    claimant may retire it by naming its GitHub comment id.
    """
    if not claimants:
        return False
    if refusal_issuer is None:
        return True
    return refusal_issuer in claimants


def sha_names_same_head(a: "str | None", b: "str | None") -> bool:
    """True when two shas name one head; a citation may abbreviate the issuance."""
    if not a or not b:
        return False
    return a.startswith(b) or b.startswith(a)


def _lane_agrees(a: "str | None", b: "str | None") -> bool:
    """A citation may drop the lane; it may not CHANGE it."""
    return a is None or b is None or a == b


def is_citation(
    seen: "Sequence[tuple[str | None, str, str | None, str | None]]",
    author: "str | None",
    kind: str,
    lane: "str | None",
    sha: "str | None",
) -> bool:
    """True when THIS AUTHOR already issued this same verdict earlier.

    `seen` is every (author, kind, lane, sha) already accepted, oldest first.

    ⚠️ SAME AUTHOR IS THE WHOLE RULE, AND AN EARLIER VERSION OF THIS OMITTED IT.
    Two DIFFERENT reviewers refusing the same head in the same lane are TWO
    verdicts, not a repeat. Dropping the author collapsed them, and the regression
    was caught by test_withdrawal_quotes_refusal -- hermit-012 and hermit-015 both
    refused hermit#2625 at one head, and retiring hermit-012's must leave
    hermit-015's standing. Collapsing them let ONE withdrawal clear BOTH, which is
    the dangerous direction and is the very defect that test exists to prevent.

    Two guards keep this from ever discarding a verdict:
      * an UNKNOWN author never matches, so a comment with no disclosure tag is
        always an issuance. Under-suppressing costs a duplicate; over-suppressing
        loses a refusal.
      * a marker with NO readable sha is never a citation, since it names no head
        and so cannot be shown to repeat one.
    """
    if sha is None or author is None:
        return False
    return any(
        prev_author == author
        and kind == prev_kind
        and _lane_agrees(lane, prev_lane)
        and sha_names_same_head(sha, prev_sha)
        for prev_author, prev_kind, prev_lane, prev_sha in seen
    )


def near_miss_refusal(line: str, start: int = 0) -> "re.Match | None":
    """Where a NON-CANONICAL refusal token starts in `line`, if it does.

    ⚠️ SHARED SO THE TWO LANDERS CANNOT DIVERGE ON IT AGAIN -- which they already
    did once, for eleven minutes, because I fixed only one of them. `ff2a3d83`
    narrowed the near-miss net and `7db64676` restored it, but the restoration
    reached `refusal_shaped()` (which `gh-merge-verified` calls) and NOT
    `changes-requested-gate`'s own `markers()`, which matches the literal
    `CHANGES-REQUESTED-AT` and so never saw a missing-`-AT` line at all. Measured
    on main at `82ce3ac2`:

        form                          gh-merge-verified   changes-requested-gate
        CHANGES-REQUESTED-AT: <lane>  REFUSE              REFUSE
        CHANGES-REQUESTED: <lane>     REFUSE              allow      <- diverged
        CHANGES REQUESTED: <lane>     REFUSE              allow      <- diverged
        REJECTED: <lane>              REFUSE              allow      <- diverged

    The permissive side was `land-pr.sh`'s, which is the path landings take.

    Returns the match so a caller can apply its own fail-closed policy to what
    follows. ``marker_assertion`` decides whether the line is asserting this
    vocabulary or merely mentioning it; this function identifies the refusal
    spelling within an asserted line.
    """
    return _REFUSAL_NEARMISS.search(line, start)


def refusal_shaped(line: str) -> bool:
    """A line that is TRYING to be a refusal, for a lander's fail-closed branch.

    ⚠️ LINE-INITIAL AFTER DECORATION IS STRIPPED -- the discriminator
    `changes-requested-gate` shipped, measured by `agent(hermit-007)` as "the
    widest net measured to cost nothing". `gh-merge-verified` instead matched the
    token ANYWHERE in the line, and that difference is the whole divergence: a
    sentence of prose ABOUT a refusal on some other pull request refused a merge
    here and not there. Measured 2026-08-25 on hermit#2602, whose only remaining
    block was a sentence adjudicating hermit#2591.

    A verdict is asserted, not mentioned. The shared assertion boundary accepts a
    marker at the start or after a lead-in when its own fields finish the line;
    prose on both sides is a mention.
    """
    normalized = strip_decoration(line)
    for start in marker_assertion_starts(normalized):
        match = near_miss_refusal(normalized, start)
        if match is not None and match.start() == start:
            return True
    return False


#: Inline code with prose around it is a citation. This is retained for the
#: citation count only; whether the line is malformed is decided by the shared
#: assertion boundary.
_INLINE_CODE_MARKER = re.compile(
    r"`[^`\n]*(?:APPROVED-AT|CHANGES-REQUESTED(?:-AT)?)(?![-\w])[^`\n]*`",
    re.IGNORECASE,
)


def _is_quoted_mention(raw_line: str) -> bool:
    """Whether ``raw_line`` quotes a marker inside code with prose around it."""
    m = _INLINE_CODE_MARKER.search(raw_line)
    if m is None:
        return False
    return bool((raw_line[: m.start()] + raw_line[m.end() :]).strip())


def lane_shas(
    comments: list[dict[str, Any]],
    lane: str,
    malformed: list[tuple[str, str]] | None = None,
    pr_number: int | None = None,
    malformed_prs: dict[tuple[str, str], int] | None = None,
    citations: list[tuple[str, str]] | None = None,
    refusals: list[str] | None = None,
    issuers: list[tuple[str, "str | None"]] | None = None,
    unanswered: list[tuple[str, "str | None"]] | None = None,
) -> list[str]:
    """SHAs a lane EXPLICITLY bound itself to, oldest first. Never inferred.

    `unanswered` is an OUT-PARAMETER carrying the refusals in this lane that
    their OWN ISSUER never took back and no entitled withdrawal retired by
    comment id, as (sha, issuer) pairs, oldest first.

    ⚠️ AN APPROVAL FROM SOMEBODY ELSE DOES NOT DISCHARGE A REFUSAL, and until
    this parameter existed it did. The clearing rule below is chronological --
    a later approval repopulates `found` -- and its comment says it revokes
    "this lane's earlier approval", written when a lane meant ONE REVIEWER.
    A lane is staffed by several agents, so the same rule let agent B's
    approval retire agent A's block with nothing recorded but a timestamp.
    Measured on hermit#2678: hermit-007 refused at 01:52:02Z, an unsigned
    reviewer approved the same head at 01:54:39Z, and the lane read
    BOUND_CURRENT with `passed-review-claude` applied while the blocking
    finding stood. The approving agent had not seen the refusal; their comment
    says "zero comments when I started". Nobody overrode anything on purpose,
    which is why attention cannot be the mechanism that holds this.

    WHO MAY DISCHARGE WHAT, and both halves fail closed:
      * the refusal's OWN issuer, by approving the same lane afterwards. That
        is a reviewer changing their mind, which is what the chronological rule
        was written for and is still honoured exactly.
      * a canonical withdrawal may name exactly one refusing comment with
        `RETIRES <comment-id>`. A signed refusal still requires its own issuer;
        an unsigned refusal keeps the existing gate's exit through any claimant.
      * NOBODY ELSE. Another agent's approval is recorded, binds normally in
        `found`, and leaves the refusal standing -- so the lane reports REFUSED
        while showing an approval, which is the honest description of a lane
        that disagrees with itself.
      * an UNSIGNED approval discharges NOTHING, even if it is in fact the same
        agent. It cannot prove whose refusal it answers, and honouring it would
        hand the unilateral clear straight back through the unsigned spelling.
        This mirrors `gh-merge-verified`'s rule that a withdrawal naming no lane
        clears no lane.

    An UNSIGNED REFUSAL is likewise undischargeable by an approval: nothing can
    prove ownership of it. That fails closed and the escape hatch is the one
    that already exists -- a canonical `CHANGES-REQUESTED-WITHDRAWN-AT` comment
    carrying `RETIRES <comment-id>`, or a push, which moves the head.

    ⚠️ AND THE COST, NAMED RATHER THAN LEFT TO BE DISCOVERED: a refusal naming
    an issuer is un-clearable by anyone but that issuer, so an agent that
    refuses and then goes away parks the head until someone pushes.
    That is the "answered versus abandoned" problem arriving from the other
    side, and it is a real trade, not a free win. It is the safer direction --
    a parked head is visible and a silently-overridden refusal is not -- but a
    reader hitting a stuck head should know this is why.

    `refusals` is an OUT-PARAMETER, in the style of `malformed`: when supplied,
    every SHA this lane refused at is appended, oldest first. It exists because
    the return value CANNOT carry that fact -- a refusal clears `found` and adds
    nothing, so a refused lane and an unreviewed lane both return `[]`. Callers
    that do not pass it are unaffected. Pair it with `classify_approval(...,
    refused=bool(refusals))` to tell the two apart; see that docstring.

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

    # `_undecorate` / `_undecorate_block` are module-level so the lander shares them.
    reject_legacy = re.compile(
        r"^REQUEST\s+CHANGES\s+AT\s+[0-9a-f]{40}$", re.IGNORECASE
    )
    # ⚠️ `-AT` IS OPTIONAL HERE AND DELIBERATELY NOT OPTIONAL FOR `approved_binding`
    # BELOW, which takes the approval grammar from `parse_marker`.
    # A reviewer who wrote `CHANGES-REQUESTED: claude <sha>` -- bare line, correct
    # lane, correct 40-hex, one token wrong -- was flagged malformed and NOT
    # honoured: the state stayed `ok` and the approval kept standing. hermit#2611
    # merged 24 minutes after a block in this form.
    #
    # THE TWO ERRORS ARE NOT SYMMETRIC, so the two matchers are not either. A
    # malformed REFUSAL must still refuse: reading it as "proceed" inverts what its
    # author said. A malformed APPROVAL must still fail to bind: reading it as
    # "approved" invents authority nobody granted. Loosening both would be the
    # obvious edit and would be wrong in one direction.
    # The optional `BY <agent>` clause is accepted here for the same reason it is
    # accepted in `_MARKER_RE`, and the asymmetry above says why it MUST be: an
    # unrecognised refusal does not fall silent, it goes malformed and blocks. A
    # refusal carrying an issuer this matcher could not read would block the head
    # it names AND read as a defect in the reviewer's spelling.
    reject_current = re.compile(
        rf"^CHANGES-REQUESTED(?:-AT)?:\s*(claude|codex)\s+[0-9a-f]{{40}}"
        rf"(?:\s+BY\s+{_AGENT_ID})?$",
        re.IGNORECASE,
    )
    # ⚠️ THE APPROVAL GRAMMAR IS NOT RESTATED HERE, AND MUST NOT BE.
    #
    # This function used to compile its own
    # `^APPROVED-AT:\s*(claude|codex)\s+([0-9a-f]{40})$` -- a second, independent
    # copy of the shape `_MARKER_RE` already defines, inside the very module that
    # exists so that everyone reads one grammar. This module's own header records
    # that `ci-hub/bin/gh-merge-verified` had duplicated this parsing, that there
    # were two parsers for one grammar, and that THEY HAD ALREADY DRIFTED. A
    # docs copy in `docs/PR_SWEEP_VERDICTS.md` had drifted four ways at once --
    # decoration, block prefix, inner whitespace, case -- and missed seven real
    # bindings before hermit#2610 removed it.
    #
    # Two copies fixed independently drift again, so this one is not repaired; it
    # is DELETED, and the single definition is called instead. Every future
    # tightening of what may bind a lane now lands in exactly one place.
    #
    # Canonical refusals use `parse_marker`, exactly as canonical approvals do.
    # The two refusal-only matchers remain solely for their existing fail-closed
    # policy: `reject_current` accepts a missing `-AT`, and `reject_legacy` accepts
    # a lane-less historical form. A malformed REFUSAL must still refuse while a
    # malformed APPROVAL must still fail to bind.
    def approved_binding(line: str) -> tuple[str, str] | None:
        """``(lane, sha)`` when `line` is a canonical approval, else ``None``."""
        parsed = parse_marker(line)
        if parsed is None or parsed[0] != MARKER_APPROVED:
            return None
        return parsed[1], parsed[2]

    def refusal_binding(line: str) -> "tuple[str | None, str] | None":
        """``(lane-or-None, sha)`` when this line is a refusal, else ``None``."""
        parsed = parse_marker(line)
        if parsed is not None and parsed[0] == MARKER_REFUSED:
            return parsed[1], parsed[2]
        if reject_legacy.match(line):
            matched_sha = SHA40.search(line)
            return (None, matched_sha.group(0).lower()) if matched_sha else None
        current = reject_current.match(line)
        if current is None:
            return None
        matched_sha = SHA40.search(line)
        return (
            (current.group(1).lower(), matched_sha.group(0).lower())
            if matched_sha else None
        )

    def refuses_lane(binding: "tuple[str | None, str] | None") -> bool:
        return binding is not None and binding[0] in (None, lane)

    # LOUD ON THE UNKNOWN. The shared grammar decides whether the review
    # vocabulary is an ASSERTION or a MENTION before value parsing. If an
    # assertion does not parse, report it regardless of whether its SHA is full,
    # abbreviated or absent.
    #
    # The old net searched for a keyword plus 7-40 hex characters anywhere in the
    # line. It therefore flagged prose that merely mentioned a full marker and
    # silently dropped an asserted marker with six or zero hex characters. Moving
    # the length floor only moved that boundary. The marker's place in the line
    # supplies the syntactic context the value cannot.
    found: list[str] = []
    # (author, kind, lane, sha) already accepted from THIS comment stream, oldest
    # first. A verdict is issued once; a later copy of the same author's verdict is
    # a CITATION and must neither bind nor be reported as a malformed attempt.
    issued: list[tuple[str | None, str, str | None, str | None]] = []
    # Refusals in THIS lane that their own issuer has not taken back, as
    # (sha, issuer, comment-id), oldest first. The comment id is retained only
    # inside this function so `RETIRES <comment-id>` can address one refusal;
    # the public `unanswered` out-parameter remains (sha, issuer).
    # Maintained unconditionally rather than only
    # when the caller asks: the discharge rule has to see every marker in order,
    # and computing it twice is how two readers of one grammar drift apart.
    outstanding: list[tuple[str, "str | None", object | None]] = []

    # A RETIRES claim has authority only when it accompanies a canonical
    # withdrawal. Collect claims first because the existing gate treats the
    # named refusal as retired regardless of where the withdrawal appears in the
    # comment stream. The entitlement rule is shared with that gate below.
    retire_claims: dict[str, list["str | None"]] = {}
    for c in comments:
        body = c.get("body") or ""
        lines = [_undecorate_block(line) for line in marker_lines(body)]
        if not any(
            parsed is not None and parsed[0] == MARKER_WITHDRAWN
            for parsed in (parse_marker(line) for line in lines)
        ):
            continue
        who = comment_issuer(body)
        for target in retirement_targets(body):
            retire_claims.setdefault(target, []).append(who)

    for c in comments:
        body = c.get("body") or ""
        author = comment_author(body)
        # Match on the block-normalised form. Stripping a leading marker cannot
        # invent a verdict -- the anchored grammar still has to match afterwards --
        # and refusing to strip it is what let a heading-formatted refusal fail open.
        # Fenced and indented code blocks are excluded HERE, once, so an
        # illustration can neither grant a lane nor refuse a merge.
        raw_lines = marker_lines(body)
        lines = [_undecorate_block(l) for l in raw_lines]
        # A rejection contributes NOTHING, even if the same comment also carries
        # an APPROVED-AT-shaped line -- a comment quoting the convention, or
        # quoting the approval it supersedes, must not bind as a positive.
        comment_id = c.get("id")
        refusal_issuer = comment_issuer(body)
        named_retirement = (
            comment_id is not None
            and retirement_allowed(
                refusal_issuer, retire_claims.get(str(comment_id), [])
            )
        )
        comment_refuses_lane = any(
            refuses_lane(refusal_binding(line)) for line in lines
        )
        relevant_rejection = comment_refuses_lane and not named_retirement
        # ⚠️ AN EXPLICIT WITHDRAWAL IS THE ESCAPE HATCH, AND WITHOUT IT THE
        # DISCHARGE RULE HAS NO DOOR. `CHANGES-REQUESTED-WITHDRAWN-AT` is the
        # grammar's way of saying "I take mine back", and `gh-merge-verified`
        # has honoured it for exactly this purpose. It is handled here, before
        # the rejection branch, because the longest-first grammar means a
        # withdrawal is NOT a refusal and must not be counted as one.
        #
        # Same ownership rule as an approval, for the same reason: a withdrawal
        # discharges only its own author's refusal, and an unsigned one
        # discharges nothing. Otherwise the unilateral clear this change removes
        # comes straight back through a different keyword.
        precise_withdrawal = bool(retirement_targets(body))
        for line in lines:
            parsed_withdrawal = parse_marker(line)
            if (
                parsed_withdrawal is not None
                and parsed_withdrawal[0] == MARKER_WITHDRAWN
                and parsed_withdrawal[1] == lane
                and not precise_withdrawal
            ):
                retractor = marker_issuer(line) or author
                if retractor is not None:
                    outstanding[:] = [
                        held for held in outstanding if held[1] != retractor
                    ]

        # A retired refusal no longer clears an earlier approval or contributes
        # an unanswered refusal. Skip the whole comment rather than allowing an
        # APPROVED-AT-shaped quotation beside the retired refusal to bind.
        if comment_refuses_lane and named_retirement:
            continue

        if relevant_rejection:
            # Verdicts are chronological. A later exact-head request for changes
            # revokes this lane's earlier approval; a still-later approval can
            # establish authority again. Clearing rather than merely skipping
            # is load-bearing: otherwise APPROVED then CHANGES-REQUESTED reads
            # as approved forever.
            #
            # ⚠️ RETRACT-AND-REBIND IN ONE COMMENT IS AMBIGUOUS, AND SILENCE WAS
            # THE WORST ANSWER. `continue` skips the WHOLE comment, so a comment
            # carrying a refusal AND a fresh approval for the same lane cleared
            # the accumulated approvals and never reached the approval below --
            # binding nothing, with no malformed flag and no error. The author's
            # text read exactly as they intended. Split across two comments the
            # identical lines bind correctly, which is what made it invisible.
            #
            # It is NOT fixed by processing lines in order. The per-comment clear
            # is deliberate and load-bearing in the other direction: a refusal
            # comment that QUOTES the approval it supersedes must not bind as a
            # positive. Line-order processing would reintroduce exactly that.
            #
            # Both readings are defensible, so this refuses to choose and says
            # so. The comment is reported, which -- since a refusal-shaped
            # malformed line fails the gate -- turns a silent loss into a loud
            # refusal. Fail-closed, and the author is told to split the two
            # verdicts into two comments, where the chronology is unambiguous.
            # Any lane's approval, deliberately: this mirrors the pre-existing
            # behaviour exactly. The finding text says "for this lane" while the
            # test does not filter by lane, so a refusal on one lane beside an
            # approval on the other is reported AMBIGUOUS. That is a policy
            # question, it is not the grammar duplication this change removes,
            # and silently narrowing it here would be a behaviour change smuggled
            # into a refactor. Left exactly as it was and raised separately.
            if any(approved_binding(line) for line in lines):
                finding = (
                    c.get("html_url") or c.get("url") or "?",
                    "AMBIGUOUS: one comment carries both a CHANGES-REQUESTED-AT "
                    "and an APPROVED-AT for this lane; post them as two comments "
                    "so the order is unambiguous",
                )
                if finding not in malformed:
                    malformed.append(finding)
                if pr_number is not None and malformed_prs is not None:
                    malformed_prs[finding] = pr_number
            if refusals is not None:
                # Recorded for REPORTING ONLY. Authority does not depend on this
                # SHA and deliberately still does not: `relevant_rejection` does
                # not compare the refused SHA to head, so a refusal at a stale SHA
                # refuses exactly as it did before. Capturing it only lets a reader
                # see WHICH head a lane blocked.
                for line in lines:
                    refusal = refusal_binding(line)
                    if refuses_lane(refusal):
                        assert refusal is not None
                        refusals.append(refusal[1])
            # WHO refused, so a later approval can be asked whether it is the
            # same agent. Recorded on the same lines the `refusals` block reads,
            # but unconditionally -- authority must not depend on whether the
            # caller happened to pass an out-parameter.
            for line in lines:
                refusal = refusal_binding(line)
                if refuses_lane(refusal):
                    assert refusal is not None
                    outstanding.append((
                        refusal[1],
                        marker_issuer(line) or author,
                        comment_id,
                    ))
            found.clear()
            # ⚠️ AND THE ISSUERS WITH THEM. `issuers` is documented as
            # INDEX-ALIGNED with the returned shas, and clearing one list without
            # the other silently breaks that: the approvals vanish while the
            # names of who made them survive, so a consumer that zips the two
            # mis-pairs, and a consumer that reads only `issuers` sees reviewers
            # for a lane that is not bound at all.
            #
            # This was MINE, shipped in the commit that introduced `issuers`, and
            # the test I wrote for the invariant did not catch it because every
            # case in it was a clean stream with no rejection. Measured on
            # hermit#2637: lane_shas returned `[]` while issuers returned one
            # entry -- an approval that had been revoked, still naming its author.
            if issuers is not None:
                issuers.clear()
            continue
        for _idx, line in enumerate(lines):
            raw_line = raw_lines[_idx] if _idx < len(raw_lines) else line
            binding = approved_binding(line)
            if binding is None:
                asserted = marker_assertion(line)
                if not asserted:
                    if _is_quoted_mention(raw_line) and citations is not None:
                        citations.append((c.get("html_url") or c.get("url") or "?",
                                          line[:120]))
                    continue
                # ⚠️ A WELL-FORMED WITHDRAWAL IS A MARKER, NOT A NEAR MISS, AND
                # COUNTING IT AS MALFORMED PENALISES THE ONE CORRECT BEHAVIOUR.
                # `approved_binding()` returns None here because a withdrawal is
                # not an approval -- so the line fell through to the near-miss
                # branch below, whose `_kind` has no withdrawn case and reads any
                # line containing "CHANGES-REQUESTED" as a REFUSAL. A withdrawal
                # contains that substring by construction, failed the citation
                # test as a refusal, and was reported malformed.
                #
                # MEASURED on hermit#2672, 2026-08-26: `malformed_source_lines=1`,
                # and the single detail was
                #     CHANGES-REQUESTED-WITHDRAWN-AT: claude 6837ed241f8d...
                # posted deliberately by agent(hermit-007) to discharge its own
                # refusal after reading the delta -- correct token, colon, lane,
                # full 40-hex.
                #
                # ⚠️ THE INCENTIVE IS WHAT MAKES THIS WORSE THAN A MISCOUNT.
                # Letting a refusal lapse silently scored clean; withdrawing it
                # explicitly scored as a defect. Withdrawal is the remedy for
                # nearly every way a verdict decays -- orphaned by a new sha,
                # outrun, superseded, approved over -- so a grammar that punishes
                # it makes all of those unfixable.
                #
                # `_MARKER_RE` already lists the withdrawn token FIRST, longest
                # first, precisely so it is not misread as a refusal. This uses
                # that same parser rather than a second copy of the shape.
                _withdrawn = parse_marker(line)
                if _withdrawn is not None and _withdrawn[0] == MARKER_WITHDRAWN:
                    continue
                if (
                    asserted
                    and refusal_binding(line) is None
                ):
                    # A CITATION IS NOT A FAILED ATTEMPT AT A VERDICT, so it is
                    # not malformed either. Without this, widening the net above
                    # to abbreviated shas would report every sentence quoting a
                    # marker -- and a malformed REFUSAL-shaped line BLOCKS, so it
                    # would manufacture blocks out of people explaining a refusal.
                    _sha_m = re.search(r"\b([0-9a-f]{7,40})\b", line)
                    _lane_m = re.search(r"\b(claude|codex)\b", line, re.I)
                    _kind = (MARKER_REFUSED if refusal_shaped(line) or "CHANGES-REQUESTED" in line.upper()
                             else MARKER_APPROVED)
                    if is_citation(
                        issued, author, _kind,
                        _lane_m.group(1).lower() if _lane_m else None,
                        _sha_m.group(1) if _sha_m else None,
                    ):
                        if citations is not None:
                            citations.append((c.get("html_url") or c.get("url") or "?",
                                              line[:120]))
                        continue
                    if _kind == MARKER_REFUSED:
                        # An asserted refusal that cannot be parsed must not leave
                        # an earlier approval standing. This includes a VERDICT
                        # line with more than one marker occurrence: choosing one
                        # would invent which marker the author asserted.
                        found.clear()
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
            explicit_lane, sha = binding
            # ⚠️ QUOTING AN APPROVAL MUST NOT GRANT ONE. A whole-line quotation is
            # undecorated into a bare marker -- a leading backtick is stripped as
            # decoration -- so `APPROVED-AT: claude <sha>` in backticks BOUND a
            # lane nobody had granted at that moment. Formatting cannot say who is
            # speaking; the issuer can.
            if is_citation(issued, author, MARKER_APPROVED, explicit_lane.lower(), sha):
                if citations is not None:
                    citations.append((c.get("html_url") or c.get("url") or "?",
                                      line[:120]))
                continue
            issued.append((author, MARKER_APPROVED, explicit_lane.lower(), sha))
            if explicit_lane.lower() == lane:
                # ⚠️ AN APPROVAL DISCHARGES ONLY ITS OWN AUTHOR'S REFUSAL. An
                # unsigned approval (`approver is None`) discharges nothing: it
                # cannot prove whose block it answers, and an unsigned refusal
                # is likewise unmatchable, so both ends fail closed. Everything
                # else about this line is unchanged -- the approval still binds
                # in `found` exactly as before, so a lane can honestly report an
                # approval AND a standing refusal at once.
                approver = marker_issuer(line) or author
                if approver is not None:
                    outstanding[:] = [
                        held for held in outstanding if held[1] != approver
                    ]
                found.append(sha)
                # WHO BOUND THIS LANE, appended in step with `found` so the two
                # stay index-aligned. This is the entire point of the change: the
                # issuer was already computed here -- `author` is read a few lines
                # up, and the citation logic depends on it -- and then DISCARDED at
                # the return, so no consumer could ask whether two lanes on a head
                # were bound by two agents or by one agent twice.
                #
                # The binding's own `BY` wins over the surrounding comment's
                # disclosure tag. They almost always agree; when they do not, the
                # marker is the more specific claim and the one a reviewer wrote
                # deliberately. `None` means neither source said -- an unsigned
                # comment carrying a bare marker -- and it is reported as None
                # rather than guessed, because a fabricated issuer would make an
                # independence check confidently wrong instead of merely blind.
                if issuers is not None:
                    issuers.append((sha, marker_issuer(line) or author))
    if unanswered is not None:
        unanswered.extend((sha, issuer) for sha, issuer, _cid in outstanding)
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
        for line in marker_lines(comment.get("body") or ""):
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
        lines = [line.strip() for line in marker_lines(comment.get("body") or "")]
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
            refusals: list[str] = []
            unanswered: list[tuple[str, "str | None"]] = []
            if label == LOCALLY_VALIDATED_LABEL:
                lane = "local-validation"
                shas = local_receipt_heads(comments, repo)
            else:
                lane = label.rsplit("-", 1)[-1]
                shas = lane_shas(
                    comments, lane, malformed, pr["number"], malformed_prs,
                    refusals=refusals, unanswered=unanswered,
                )
            rows.append(
                {
                    "number": pr["number"],
                    "label": label,
                    "source": "label",
                    "lane": lane,
                    "head": head,
                    "bound_to": shas[-1] if shas else None,
                    "refused_at": refusals[-1] if refusals else None,
                    "state": classify_approval(
                        head, shas, refused=bool(refusals),
                        refused_at=refusals[-1] if refusals else None,
                        unanswered=bool(unanswered),
                    ),
                    # The ISSUER of every unanswered refusal at this head, in
                    # order, with None preserved for one whose disclosure tag
                    # cannot be read. WHY IT IS PUBLISHED RATHER THAN DERIVED BY
                    # EACH READER: whether a refusal can be retired AT ALL depends
                    # on this value and on nothing else in the row, and the two
                    # cases point opposite ways. A refusal with NO readable issuer
                    # is retired by ANY agent naming it, which
                    # `ci-hub/bin/changes-requested-gate` has done since
                    # 2026-08-25 via `RETIRES #<comment-id>`. A refusal whose
                    # issuer IS named is retired by that issuer alone, so if they
                    # have left the fleet it can be retired by nobody. Measured
                    # 2026-08-26 across 35 open rrnewton/hermit pull requests: 22
                    # unanswered refusals at head, 4 with a live issuer, 13 with a
                    # departed one and 5 with none. Reading the row could not tell
                    # those apart, so the census had to be rebuilt by hand.
                    "refusal_issuers": [w for _s, w in unanswered if _s == head],
                    "runs_at_head": len(runs),
                    "mergeable": pr.get("mergeable"),
                }
            )
        # ⚠️ A LABEL IS A CACHE, AND IT WAS ALSO THE INDEX.
        #
        # Rows were generated ONLY per passed-review-* label, so a PR carrying a
        # correctly-formed `APPROVED-AT: <lane> <head>` and no label produced NO
        # ROW AT ALL -- its comments were fetched above and then never parsed for
        # a binding. The tool reported `approvals: 0, approval_set_missing: true`,
        # which cannot be distinguished from "nobody reviewed it". That inverts
        # the policy stated at the top of this file: comments are authority and
        # labels are caches. With the cache as the index, authority was consulted
        # only for entries the cache already contained, so a cache MISS and an
        # authority ABSENCE produced the same number.
        #
        # Measured 2026-08-25 on rrnewton/hermit: of 28 open PRs, 25 carried no
        # passed-review-* label; FIVE of those carried a well-formed binding that
        # was never read, and TWO were bound at the current head.
        #
        # These rows are tagged `source: "comment"` and carry `label: None`.
        # THEY DO NOT MOVE THE MERGE BOUNDARY -- see `approval_rows` below, which
        # still counts label rows only, and review_rebase.py, which skips them.
        # Whether a comment binding should be sufficient to LAND is a policy
        # question for the owner; this change only stops the tool reporting a
        # review it can see as a review that does not exist.
        labelled_lanes = {
            name.rsplit("-", 1)[-1]
            for name in tracked
            if name != LOCALLY_VALIDATED_LABEL
        }
        for lane in LANES:
            if lane in labelled_lanes:
                continue
            refusals = []
            unanswered: list[tuple[str, "str | None"]] = []
            shas = lane_shas(
                comments, lane, malformed, pr["number"], malformed_prs,
                refusals=refusals, unanswered=unanswered,
            )
            # ⚠️ `if not shas` ALONE DROPPED EVERY UNLABELLED REFUSAL. A refusal
            # contributes no SHA, so the row that would have carried it was
            # skipped here and the blocked head appeared in NO row of the census
            # -- the same cache-miss-reads-as-absence shape documented above, one
            # layer down. Measured 2026-08-26 on hermit#2655: zero labels, two
            # lanes refused with blocking findings, zero rows, and the head kept
            # being re-dispatched as needing a reviewer.
            if not shas and not refusals:
                continue
            rows.append(
                {
                    "number": pr["number"],
                    "label": None,
                    "source": "comment",
                    "lane": lane,
                    "head": head,
                    "bound_to": shas[-1] if shas else None,
                    "refused_at": refusals[-1] if refusals else None,
                    "state": classify_approval(
                        head, shas, refused=bool(refusals),
                        refused_at=refusals[-1] if refusals else None,
                        unanswered=bool(unanswered),
                    ),
                    # The ISSUER of every unanswered refusal at this head, in
                    # order, with None preserved for one whose disclosure tag
                    # cannot be read. WHY IT IS PUBLISHED RATHER THAN DERIVED BY
                    # EACH READER: whether a refusal can be retired AT ALL depends
                    # on this value and on nothing else in the row, and the two
                    # cases point opposite ways. A refusal with NO readable issuer
                    # is retired by ANY agent naming it, which
                    # `ci-hub/bin/changes-requested-gate` has done since
                    # 2026-08-25 via `RETIRES #<comment-id>`. A refusal whose
                    # issuer IS named is retired by that issuer alone, so if they
                    # have left the fleet it can be retired by nobody. Measured
                    # 2026-08-26 across 35 open rrnewton/hermit pull requests: 22
                    # unanswered refusals at head, 4 with a live issuer, 13 with a
                    # departed one and 5 with none. Reading the row could not tell
                    # those apart, so the census had to be rebuilt by hand.
                    "refusal_issuers": [w for _s, w in unanswered if _s == head],
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
) -> tuple[list[dict[str, Any]], dict[str, Any] | None, bool]:
    rows = []
    selected = None
    fixture = json.load(open(path))
    if isinstance(fixture, dict):
        prs = fixture.get("prs")
        actions_enabled = fixture.get("actions_enabled")
        if not isinstance(prs, list) or type(actions_enabled) is not bool:
            raise ValueError(
                "fixture object requires prs list and boolean actions_enabled"
            )
        # ⚠️ FIXTURES SUPPLY THEIR OWN RECEIPT SET AND NEVER READ THE LIVE
        # LEDGER. A fixture that consulted this box's ledger would pass or fail
        # depending on what some other agent validated this afternoon -- a test
        # whose verdict is a function of the day. Absent means "no head has a
        # receipt", which is the historical behaviour of every existing fixture.
        evaluated = fixture.get("evaluated_heads", [])
        if not isinstance(evaluated, list) or not all(
            isinstance(h, str) for h in evaluated
        ):
            raise ValueError("fixture evaluated_heads must be a list of strings")
        fixture_evaluated = frozenset(h.lower() for h in evaluated)
    elif isinstance(fixture, list):
        # Historical fixtures model the ordinary Actions-enabled case.  This
        # preserves their positive zero-run escalation coverage.
        prs = fixture
        actions_enabled = True
        fixture_evaluated = frozenset()
    else:
        raise ValueError("fixture must be a PR list or an object with prs")
    for pr in prs:
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
            refusals = []
            unanswered: list[tuple[str, "str | None"]] = []
            if label == LOCALLY_VALIDATED_LABEL and "comments" in pr:
                shas = local_receipt_heads(comments, pr.get("repo", "rrnewton/hermit"))
            elif "comments" in pr:
                shas = lane_shas(
                    comments,
                    lane,
                    malformed,
                    pr["number"],
                    malformed_prs,
                    refusals=refusals, unanswered=unanswered,
                )
            else:
                shas = pr.get("lane_shas", {}).get(lane, [])
                # Offline twin of the out-parameter. Without it no fixture can
                # express "this lane refused", and the REFUSED state would be
                # unreachable from the suite -- untestable, which for this file
                # means unasserted.
                refusals = list(pr.get("lane_refused", {}).get(lane, []))
            rows.append(
                {
                    "number": pr["number"],
                    "label": label,
                    "source": "label",
                    "lane": lane,
                    "head": pr["head"],
                    "bound_to": shas[-1] if shas else None,
                    "refused_at": refusals[-1] if refusals else None,
                    "state": classify_approval(
                        pr["head"], shas, refused=bool(refusals),
                        refused_at=refusals[-1] if refusals else None,
                        unanswered=bool(unanswered),
                    ),
                    # The ISSUER of every unanswered refusal at this head, in
                    # order, with None preserved for one whose disclosure tag
                    # cannot be read. WHY IT IS PUBLISHED RATHER THAN DERIVED BY
                    # EACH READER: whether a refusal can be retired AT ALL depends
                    # on this value and on nothing else in the row, and the two
                    # cases point opposite ways. A refusal with NO readable issuer
                    # is retired by ANY agent naming it, which
                    # `ci-hub/bin/changes-requested-gate` has done since
                    # 2026-08-25 via `RETIRES #<comment-id>`. A refusal whose
                    # issuer IS named is retired by that issuer alone, so if they
                    # have left the fleet it can be retired by nobody. Measured
                    # 2026-08-26 across 35 open rrnewton/hermit pull requests: 22
                    # unanswered refusals at head, 4 with a live issuer, 13 with a
                    # departed one and 5 with none. Reading the row could not tell
                    # those apart, so the census had to be rebuilt by hand.
                    "refusal_issuers": [w for _s, w in unanswered if _s == pr["head"]],
                    "runs_at_head": pr.get("runs_at_head", 1),
                    "mergeable": pr.get("mergeable"),
                }
            )
        # The offline twin of the comment-discovered rows in collect(). These two
        # builders MUST stay in step: the fixture path is where every assertion in
        # the test suite runs, so a behaviour that exists only in collect() is a
        # behaviour no test can reach. Keeping them together is what makes
        # `an_unlabeled_binding_is_reported_but_still_refuses` mean anything.
        labelled_lanes = {
            name.rsplit("-", 1)[-1]
            for name in pr["labels"]
            if name.startswith("passed-review-")
        }
        for lane in LANES:
            if lane in labelled_lanes:
                continue
            refusals = []
            unanswered: list[tuple[str, "str | None"]] = []
            if "comments" in pr:
                shas = lane_shas(
                    comments, lane, malformed, pr["number"], malformed_prs,
                    refusals=refusals, unanswered=unanswered,
                )
            else:
                shas = pr.get("lane_shas", {}).get(lane, [])
                refusals = list(pr.get("lane_refused", {}).get(lane, []))
            if not shas and not refusals:
                continue
            rows.append(
                {
                    "number": pr["number"],
                    "label": None,
                    "source": "comment",
                    "lane": lane,
                    "head": pr["head"],
                    "bound_to": shas[-1] if shas else None,
                    "refused_at": refusals[-1] if refusals else None,
                    "state": classify_approval(
                        pr["head"], shas, refused=bool(refusals),
                        refused_at=refusals[-1] if refusals else None,
                        unanswered=bool(unanswered),
                    ),
                    # The ISSUER of every unanswered refusal at this head, in
                    # order, with None preserved for one whose disclosure tag
                    # cannot be read. WHY IT IS PUBLISHED RATHER THAN DERIVED BY
                    # EACH READER: whether a refusal can be retired AT ALL depends
                    # on this value and on nothing else in the row, and the two
                    # cases point opposite ways. A refusal with NO readable issuer
                    # is retired by ANY agent naming it, which
                    # `ci-hub/bin/changes-requested-gate` has done since
                    # 2026-08-25 via `RETIRES #<comment-id>`. A refusal whose
                    # issuer IS named is retired by that issuer alone, so if they
                    # have left the fleet it can be retired by nobody. Measured
                    # 2026-08-26 across 35 open rrnewton/hermit pull requests: 22
                    # unanswered refusals at head, 4 with a live issuer, 13 with a
                    # departed one and 5 with none. Reading the row could not tell
                    # those apart, so the census had to be rebuilt by hand.
                    "refusal_issuers": [w for _s, w in unanswered if _s == pr["head"]],
                    "runs_at_head": pr.get("runs_at_head", 1),
                    "mergeable": pr.get("mergeable"),
                }
            )
    return rows, selected, actions_enabled, fixture_evaluated


def locally_evaluated_heads(root: Path) -> frozenset[str]:
    """Every commit the local validate ledger has a record for.

    ⚠️ RECORD EXISTS, NOT RECORD PASSED, AND THE DIFFERENCE IS THE WHOLE POINT.
    The claim this feeds is `APPROVED + UNEVALUATED` -- "a review cache on a head
    NOTHING EVALUATED". A head whose validate ran and went RED has been
    evaluated, loudly; calling it unevaluated is a false statement about the
    record. Whether the head is GREEN is a different question that
    `ci-hub validate-status` already answers and the lander already asks.

    Reads the ledger rather than shelling out to `validate-status`, because that
    tool's exit codes cannot express this predicate: measured 2026-08-26,
    `--sha 135b3a24167f` (a completed FAILING run) and `--sha 91bae53a6f3e`
    (no record at all) BOTH return rc=4, and the distinction survives only in
    the prose. A predicate that has to parse a sentence is not a predicate.
    """

    heads: set[str] = set()
    for path in sorted((root / "ledger").rglob("*.jsonl")):
        try:
            text = path.read_text(errors="replace")
        except OSError:
            continue
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                # An unparseable row is not evidence of evaluation. There are
                # twelve of these on the current ledger; they are somebody
                # else's defect and must not silently mark a head evaluated.
                continue
            commit = row.get("commit") if isinstance(row, dict) else None
            if isinstance(commit, str) and commit:
                heads.add(commit.lower())
    return frozenset(heads)


def unevaluated_binding_rows(
    rows: list[dict[str, Any]],
    actions_enabled: bool,
    evaluated_heads: "frozenset[str]" = frozenset(),
) -> list[dict[str, Any]]:
    """Rows whose head NOTHING evaluated -- no workflow run and no local receipt.

    ⚠️ A ZERO RUN COUNT STOPPED BEING EVIDENCE WHEN ACTIONS STOPPED BEING THE
    GATE. The owner ruled local validate canonical and hosted Actions
    supplemental, so `runs_at_head == 0` on its own says only that the thing we
    do not gate on did not happen. The per-host `.ops-state.yaml` now makes that
    operational choice explicit instead of trusting the repository permission,
    which can remain enabled while every workflow is deliberately dormant.

    THE COST WAS NOT COSMETIC: `--gate-on superseded` admits every non-SUPERSEDED
    bad row that lands in this list, so the headline went 7-of-8 with the run
    count alone and 1-of-8 without it, with EVERY PER-ROW STATE IDENTICAL.
    Reviewers who did exactly what the remedy asked saw no change, which is how
    a gate teaches people to stop supplying approvals.

    A local receipt now counts as evaluation alongside a workflow run. The other
    half of that alarm -- `REFUSED_STALE` on a correct at-head binding, caused by
    an un-discharged refusal at an older sha -- is UNTOUCHED here and is a
    separate, undecided design question. Do not read this as fixing it.
    """
    if not actions_enabled:
        return []
    return [
        row
        for row in rows
        if row["runs_at_head"] == 0
        and (row.get("head") or "").lower() not in evaluated_heads
    ]


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
    remote = bounded_capture.git(["remote", "get-url", "origin"], cwd=checkout)
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
        if a.fixture:
            rows, selected, actions_enabled, evaluated_heads = collect_fixture(
                a.fixture, malformed, malformed_pr_by_source, a.pr
            )
        else:
            rows, selected = collect_live(
                a.repo, malformed, malformed_pr_by_source, a.pr
            )
            actions_enabled = github_actions_enabled(a.repo)
            evaluated_heads = locally_evaluated_heads(Path(a.checkout))
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
    # `or ""` is load-bearing: comment-discovered rows carry label None. Counting
    # them here would make an unlabeled-but-approved PR satisfy the landing
    # boundary, which is the policy change this deliberately does not make.
    approval_rows = [
        row for row in rows if (row["label"] or "").startswith("passed-review-")
    ]
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
    # ⚠️ LABEL ROWS ONLY, and that is a deliberate restraint rather than an
    # oversight. A comment-discovered binding (source "comment", label None) is
    # real authority by this file's stated policy, so counting it here would be
    # defensible -- and it would SHRINK critical_lanes_missing, loosening the
    # critical-review gate for any PR whose reviewer never applied a label. That
    # is a policy change, and the change that made those rows visible is not the
    # place to make it. Deleting the `source` test below is the one-line switch,
    # and `an_unlabeled_binding_does_not_satisfy_the_critical_lane_check` fails
    # if it is deleted by accident.
    authoritative_lanes = {
        str(row["lane"])
        for row in rows
        if row["state"] == BOUND_CURRENT
        and row["lane"] in LANES
        and row.get("source") != "comment"
    }
    critical_lanes_missing = (
        sorted(set(LANES) - authoritative_lanes) if critical_required else []
    )
    # Same population as before this change: comment-discovered rows are reported
    # but are not folded into the headline counts, so `approvals`/`authoritative`
    # stay comparable with every receipt already recorded against them.
    label_rows = [r for r in rows if r.get("source") != "comment"]
    comment_rows = [r for r in rows if r.get("source") == "comment"]
    bad = [r for r in label_rows if r["state"] != BOUND_CURRENT]
    authoritative = len(label_rows) - len(bad)
    malformed_prs = set(malformed_pr_by_source.values())
    malformed_affected_labels = {
        (r["number"], r["label"])
        for r in label_rows
        if r["number"] in malformed_prs and (r["label"] or "").startswith("passed-review-")
    }
    # The composed state: a review/local-validation cache that no longer holds,
    # on a head nothing evaluated.
    combined = unevaluated_binding_rows(bad, actions_enabled, evaluated_heads)
    approved_and_unevaluated = [
        r for r in combined if (r["label"] or "").startswith("passed-review-")
    ]

    if a.as_json:
        print(json.dumps({"approvals": len(approval_rows),
                          "actions_enabled": actions_enabled,
                          "binding_labels": len(label_rows),
                          # Canonical bindings found in comments on a PR carrying no
                          # matching label. Reported, deliberately not counted above.
                          "unlabeled_bindings": len(comment_rows),
                          "unlabeled_bound_current": len(
                              [r for r in comment_rows if r["state"] == BOUND_CURRENT]
                          ),
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
        print(f"actions_enabled={str(actions_enabled).lower()}")
        if not actions_enabled:
            print(
                "runs_at_head_zero=expected while GitHub Actions are disabled; "
                "zero workflow runs are not classified as unevaluated"
            )
        print(f"inspected {len(label_rows)} passed-review-* / locally-validated label(s); bound to current head: "
              f"{authoritative}; authority absent: {len(bad)}; "
              f"passed-review-* / locally-validated AND unevaluated: {len(combined)}")
        for r in label_rows:
            if r["state"] == BOUND_CURRENT:
                continue
            if r["bound_to"]:
                bound = r["bound_to"][:12]
            elif r["label"] == LOCALLY_VALIDATED_LABEL:
                bound = "<no locally-validated receipt marker>"
            elif r.get("refused_at"):
                # NOT "<no APPROVED-AT binding>". Both are the absence of an
                # approval, but only one of them means a reviewer looked and said
                # no, and the remedies are opposites: find a reviewer, versus
                # answer the one you have.
                bound = f"<REFUSED at {r['refused_at'][:12]}, no later approval>"
            else:
                bound = "<no APPROVED-AT binding>"
            if r in combined:
                flag = (
                    "  *** LOCALLY-VALIDATED + UNEVALUATED ***"
                    if r["label"] == LOCALLY_VALIDATED_LABEL
                    else "  *** APPROVED + UNEVALUATED ***"
                )
            else:
                flag = ""
            print(f"  {r['state']:<13} PR {r['number']}  {r['label']}  "
                  f"bound-to {bound}  head {r['head'][:12]}  runs={r['runs_at_head']}{flag}")
        for r in comment_rows:
            # A refused lane binds nothing, so `bound_to` is None here and the
            # refused SHA is what the reader needs instead: WHICH head was
            # blocked. Report that rather than an empty column, and never index
            # `bound_to` unconditionally -- these rows were previously
            # unreachable without a binding.
            if r["bound_to"] is not None:
                where = f"bound-to {r['bound_to'][:12]}"
            elif r["refused_at"] is not None:
                where = f"refused-at {r['refused_at'][:12]}"
            else:
                where = "bound-to none"
            print(
                f"  {r['state']:<13} PR {r['number']}  (no label, lane {r['lane']}, "
                f"from comment)  {where}  head {r['head'][:12]}"
            )
        if approval_set_missing:
            print(f"  REQUIRED-APPROVAL-SET-MISSING  PR {a.pr} has zero passed-review labels")
            # The distinction this tool could not previously draw. Both states
            # print zero approvals; only one of them means nobody looked.
            at_head = [r for r in comment_rows if r["state"] == BOUND_CURRENT]
            if at_head:
                print(
                    f"    NOT UNREVIEWED: {len(at_head)} canonical binding(s) at the "
                    f"current head in comments ("
                    + ", ".join(sorted(r["lane"] for r in at_head))
                    + "), with no passed-review-* label. Apply the label to make "
                    "this landable; the review already exists."
                )
            elif comment_rows:
                print(
                    f"    {len(comment_rows)} canonical binding(s) exist in comments "
                    "but none at the current head -- genuinely superseded."
                )
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
        gating = [r for r in label_rows if r["state"] == SUPERSEDED] + [
            r for r in combined if r["state"] != SUPERSEDED
        ]
    elif a.gate_on == "unverifiable":
        # ⚠️ REFUSED MUST BE LISTED HERE, AND THIS IS THE ONLY PLACE IN THE FILE
        # THAT ENUMERATES A STATE POSITIVELY. Every other reader asks
        # `== BOUND_CURRENT` or `!= BOUND_CURRENT` and is therefore fail-closed
        # under a new value. This one selects what to PAGE about, so omitting
        # REFUSED would have made splitting it out of UNDETERMINED silently stop
        # paging on refused-but-labelled heads: a live block disappearing from
        # the report whose whole job is to surface it, with rc going 1 -> 0 and
        # nothing to show anything had changed. `tick-hub.yaml` runs exactly this
        # selector against the live repo.
        gating = [
            r
            for r in label_rows
            # REFUSED_STALE is here for the same reason REFUSED is: it BLOCKS, so
            # a head carrying one must keep paging. Splitting a state and not
            # updating this line is how the split silently stops the page -- the
            # exact regression the comment above was written about, one state
            # later.
            if r["state"] in (UNDETERMINED, REFUSED, REFUSED_STALE)
        ]
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
            at_head = [r for r in comment_rows if r["state"] == BOUND_CURRENT]
            summary = (
                f"PR #{a.pr} has zero passed-review labels; absence of review "
                "evidence cannot satisfy the final landing boundary"
            )
            if at_head:
                # Same refusal, different cause, and the operator needs the
                # difference: one says find a reviewer, the other says apply a
                # label to a review that already happened.
                summary = (
                    f"PR #{a.pr} has zero passed-review labels, but "
                    f"{len(at_head)} canonical binding(s) at the current head "
                    "exist in comments ("
                    + ", ".join(sorted(r["lane"] for r in at_head))
                    + "); the review exists and the cache label does not"
                )
        else:
            state = "unbound" if gating else "ok"
            remedies = []
            # ⚠️ THE REMEDY IS CHOSEN BY WHY THE ROW GATES, NOT BY WHICH LABEL IT
            # CARRIES, AND GETTING THAT WRONG COST FOUR RE-REVIEWS. A
            # `passed-review-*` row that gates because its head was never
            # evaluated was told to "re-review the exact head and emit
            # APPROVED-AT" -- the action its reviewer had already performed, on a
            # binding that was already correct. Measured 2026-08-26 on hermit#2686,
            # #2683, #2674 and #2602: all four bound to exactly the head they sat
            # on, all four told to re-bind.
            superseded_review = [
                r for r in gating
                if (r["label"] or "").startswith("passed-review-")
                and r["state"] == SUPERSEDED
            ]
            unevaluated_review = [
                r for r in gating
                if (r["label"] or "").startswith("passed-review-")
                and r["state"] != SUPERSEDED
            ]
            if superseded_review:
                remedies.append(
                    "re-review the exact head and emit APPROVED-AT: <lane> <40-hex-head>"
                )
            if unevaluated_review:
                remedies.append(
                    "validate the exact head -- these bindings are AT the head and "
                    "re-reviewing them changes nothing"
                )
            if any(r["label"] == LOCALLY_VALIDATED_LABEL for r in gating):
                remedies.append(
                    "validate the exact head and publish its counted receipt before "
                    "reapplying locally-validated"
                )
            if not remedies:
                remedies.append("no binding repair is required")
            summary = (
                f"{len(gating)}/{len(label_rows)} passed-review-* / locally-validated labels fail the "
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

        # ⚠️ AN UNANSWERED REFUSAL IS NOT ONE CONDITION BUT TWO, AND THEIR
        # REMEDIES POINT OPPOSITE WAYS -- reported here because reading
        # `state=REFUSED` alone gave both the same answer, which is "ask the
        # reviewer", and for one of them there is nobody to ask.
        #
        # A refusal that names NO issuer is the RETIRABLE one. A canonical
        # withdrawal carrying `RETIRES #<comment-id>` retires it for any claimant
        # in both this reader and `ci-hub/bin/changes-requested-gate`; the gate
        # reports that the claimant may not be the author. A comment id is
        # assigned by GitHub and cannot go missing, which is why that identity
        # was chosen over the issuer.
        #
        # A refusal that DOES name an issuer is retirable by that issuer ALONE. So
        # if they have left the fleet it is retirable by nobody, in either
        # consumer, and no amount of re-review moves it. Measured 2026-08-26
        # across 35 open rrnewton/hermit pull requests: 22 unanswered refusals at
        # head over 13 pull requests -- 4 issuers live, 13 departed, 5 absent. TEN
        # of the thirteen pull requests carried at least one refusal no live agent
        # could retire, and nothing in this tool's output said so.
        #
        # This reports the split. It does not retire anything and it does not read
        # the fleet: whether a NAMED issuer is still live is a separate question
        # with its own freshness rules, and answering it here would put a live
        # read on the path of a verdict.
        refusal_rows = [r for r in rows if r.get("refusal_issuers")]
        named = sum(1 for r in refusal_rows for w in r["refusal_issuers"] if w)
        unnamed = sum(1 for r in refusal_rows for w in r["refusal_issuers"] if not w)
        if refusal_rows:
            print("refusal_issuer_state=present")
            print(
                "refusal_issuer_summary="
                f"{named + unnamed} unanswered refusal(s) at head across "
                f"{len({r['number'] for r in refusal_rows})} PR(s); "
                f"{named} name an issuer, {unnamed} name none"
            )
            print(
                "refusal_issuer_remedy=a refusal naming NO issuer is retired by any "
                "agent posting a canonical withdrawal carrying RETIRES #<comment-id>; "
                "approval_binding and changes-requested-gate both honor it. A refusal "
                "naming an issuer is "
                "retired by that issuer alone, so a departed issuer leaves it "
                "retirable by nobody -- escalate rather than re-review"
            )
        else:
            print("refusal_issuer_state=none")

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
            "is absence of authority, not zero review problems."
            + (
                "  NOTE: "
                + str(len([r for r in comment_rows if r["state"] == BOUND_CURRENT]))
                + " canonical binding(s) at the current head were found in comments; "
                "this refuses for a missing LABEL, not a missing REVIEW."
                if any(r["state"] == BOUND_CURRENT for r in comment_rows)
                else ""
            ),
            file=sys.stderr,
        )
        return 1
    if a.gate and gating:
        print(
            f"GATE FAIL ({a.gate_on}): {len(gating)} of {len(label_rows)} passed-review-* / locally-validated label(s) do not bind "
            "to the current head. They are NOT deleted -- they simply must not satisfy the review "
            "or local-validation head-binding gate.",
            file=sys.stderr,
        )
        return 1
    # SPLIT THE MALFORMED BUCKET BY POLARITY, because the two halves fail in
    # opposite directions and only one of them is safe to advise on.
    #
    # An unparseable APPROVAL-shaped line (`APPROVED-AT: <lane>`, `APPROVED @ <sha>`) cannot
    # be a refusal, so it cannot hide one. It stays an ADVISORY: this file's
    # long-standing and separately bracketed policy is that historical syntax
    # debt must not turn a valid canonical BOUND_CURRENT authority into absence,
    # and blocking on it would make every old comment a permanent gate failure.
    #
    # An unparseable REFUSAL-shaped line is the opposite case and must REFUSE.
    # If it does not parse, the lane's newest parsed binding is whatever came
    # before it -- typically an approval -- so the gate reports SATISFIED for a
    # head a reviewer has explicitly refused.
    #
    # ⚠️ THIS IS A REAL HAZARD BUT IT IS NOT WHAT HAPPENED ON pull/2591. That
    # attribution was retracted 2026-08-25 -- see undecorate_block's docstring.
    # 2591's refusal was PLAIN and column-0, it parsed, and the merge happened
    # anyway 3m42s later because nothing on the merge path reads this module.
    # Keep failing closed here on its own merits; do not cite 2591 for it.
    #
    # It is why "not proof that authority is absent" is beside the point: it
    # is not proof that authority is PRESENT either, and the gate must choose.
    # Failing closed here is wrong only in the cheap direction -- a re-post.
    #
    # Most such lines now parse anyway, because `undecorate_block` strips the
    # block-level markdown that hid them. This is the backstop for the forms
    # that convention has not anticipated yet.
    refusalish = re.compile(
        r"CHANGES[-\s]?REQUESTED|REQUEST\s+CHANGES|REJECT", re.IGNORECASE
    )
    malformed_refusals = [f for f in malformed if refusalish.search(f[1])]
    malformed_approvals = [f for f in malformed if not refusalish.search(f[1])]

    # A MARKER-SHAPED LINE THAT DOES NOT PARSE IS AN ERROR, NOT A SILENCE.
    # It used to be an advisory returning 0, on the reasoning that "malformed
    # syntax is not itself proof that label authority is absent". True, and
    # beside the point: it is not proof that authority is PRESENT either, and
    # the gate has to choose. Silence is the one answer that is wrong in both
    # directions at once.
    #
    # THREE CASES, because they are genuinely different and only two are unsafe:
    #
    #  1. REFUSAL-shaped and unparseable -> ALWAYS REFUSE. Unparsed, it leaves
    #     the lane's EARLIER approval standing as the newest binding, so the
    #     gate reports SATISFIED for a head somebody explicitly blocked. Note it
    #     is never redundant with an approval, so the exemption below must not
    #     reach it.
    #     ⚠️ Formerly cited pull/2591 as the instance; retracted 2026-08-25,
    #     because that refusal parsed. See undecorate_block's docstring.
    #
    #  2. APPROVAL-shaped and unparseable, while some lane's authority is
    #     ABSENT -> REFUSE. The unparsed line may BE that missing approval; a
    #     lane lost to formatting rather than to disagreement is the other half
    #     of the same defect. Refusing forces a re-post, which recovers it.
    #
    #  3. APPROVAL-shaped and unparseable, with every inspected label already
    #     bound to the current head -> ADVISORY. Here nothing is lost: the
    #     canonical binding is present and the malformed line is redundant
    #     syntax debt, typically an incomplete `APPROVED-AT: <lane>` sitting in
    #     the same comment as its own canonical form (the #1847 shape). Failing
    #     here would block genuinely-approved heads on an old comment's wording
    #     and make the gate ignorable, which is a worse outcome than the debt.
    if a.gate and malformed_refusals:
        for _where, line in malformed_refusals:
            print(f"  unparseable refusal-shaped line: {line}", file=sys.stderr)
        print(
            f"GATE FAIL: {len(malformed_refusals)} refusal-shaped line(s) could not be parsed. "
            "An unparseable refusal is REFUSED, never ignored: unparsed, it leaves this "
            "lane's earlier approval standing as the newest binding, so the gate would "
            "pass a head somebody has explicitly blocked. Repost in the exact grammar "
            "'CHANGES-REQUESTED-AT: <claude|codex> <40-hex>', then re-run.",
            file=sys.stderr,
        )
        return 1
    if a.gate and malformed_approvals and bad:
        for _where, line in malformed_approvals:
            print(f"  unparseable approval-shaped line: {line}", file=sys.stderr)
        print(
            f"GATE FAIL: {len(malformed_approvals)} approval-shaped line(s) could not be parsed "
            f"while {len(bad)} label(s) lack authority at the current head. The unparsed line "
            "may be the missing approval -- a lane lost to formatting rather than to "
            "disagreement. Repost in the exact grammar "
            "'APPROVED-AT: <claude|codex> <40-hex>', then re-run.",
            file=sys.stderr,
        )
        return 1
    if a.gate and malformed_approvals:
        print(
            f"GATE ADVISORY: {len(malformed_approvals)} malformed source line(s) need syntax cleanup, "
            "but malformed syntax is not itself proof that label authority is absent.",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
