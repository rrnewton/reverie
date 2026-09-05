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

    an approval holds only if a canonical review artifact names the EXACT
    current head SHA for that lane and its immutable GitHub actor has current
    repository review permission.

That cannot fail open. There is no event to miss and no runner to be offline; if
the head moved, the comparison simply fails. It is also what policy already says
these artifacts mean -- "role-tagged review comments carrying the full head SHA
are authority; numbered review and `passed-review-*` labels are caches."

*** APPROVAL COMMENTS ARE NEVER DELETED. *** A superseded approval stops
satisfying the gate and stays visible forever as the record of what was reviewed
and when. With ``--reconcile-review-labels``, only the stale ``passed-review-*``
cache label is removed. The exact-head comment remains the authority and the
history. This distinction matters: PR #1874's review history was permanently
lost on 2026-08-09 by deleting review evidence, while removing a label whose own
description says "at current PR head" makes the visible cache agree with that
evidence.

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
import datetime
import json
import os
import re
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from typing import Any
from typing import Callable, Sequence
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402
import ops_state  # noqa: E402

SHA40 = re.compile(r"\b[0-9a-f]{40}\b")
# An exact retirement is an assertion, not a token that prose may happen to
# quote. Keep this as one complete prose line outside fenced/indented code.
RETIRES = re.compile(r"^\s*RETIRES\s+#?(\d{6,})\s*$", re.IGNORECASE)
_RETIRES_ASSERTION = re.compile(r"^RETIRES(?:\s|$)", re.IGNORECASE)
_COMMENT_URL_ID = re.compile(r"#(?:issuecomment|pullrequestreview)-(\d+)\b")
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


class ReviewPermissionUnavailable(GitHubUnavailable):
    """Current GitHub actor permission could not be established."""


class GitHubDeadlineExceeded(GitHubUnavailable):
    """The caller's shared GitHub read deadline expired."""


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

    `unanswered` says this lane holds a refusal that was never discharged by a
    canonical withdrawal in the same lane, by a later-head approval, or by an
    exact authorized ``RETIRES <comment-id>`` record. It outranks every approval
    and is therefore a strictly stronger condition than `refused`, which only
    describes the lane's last chronological marker. An approval at the same head
    is conflicting evidence, not a withdrawal, and leaves the refusal unanswered.

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

    WHAT CLEARS IT: a canonical withdrawal in the same lane at the exact current
    head, a canonical current-head approval when the refusal names a different
    head, or a precise retirement of the named refusal by a currently authorized
    GitHub actor. A moved head alone and a same-head approval are not discharges.

    ⚠️ THIS RENAMES A STATE, IT DOES NOT MOVE AN ADMISSION BOUNDARY. Every
    caller that grants authority tests `== BOUND_CURRENT`, and neither REFUSED
    nor REFUSED_STALE is BOUND_CURRENT, so nothing becomes landable that was not
    landable before. `relevant_rejection` still never compares the refused SHA to
    head when deciding whether the lane is cleared; the comparison added here is
    for REPORTING only.
    """
    # ⚠️ CHECKED BEFORE THE SHAs, AND THAT ORDER IS THE WHOLE POINT OF
    # `unanswered`. A refusal with no canonical withdrawal, later-head approval,
    # or authorized exact retirement outranks earlier approvals. `refused` cannot
    # carry this distinction by itself because a refusal contributes no SHA.
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


def _github_read_timeout(deadline: float | None) -> tuple[float, bool]:
    """Return this read's allowance and whether the caller deadline binds it."""
    if deadline is None:
        return 300.0, False
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise GitHubDeadlineExceeded(
            "time budget exhausted before the next GitHub API read"
        )
    return min(300.0, remaining), remaining <= 300.0


def _require_github_time(deadline: float | None) -> None:
    """Refuse evidence returned after the caller's deadline."""
    if deadline is not None and deadline - time.monotonic() <= 0:
        raise GitHubDeadlineExceeded(
            "time budget exhausted during a GitHub API read"
        )


def gh_json(
    args: list[str],
    *,
    net_wrapper: Sequence[str] = ("with-proxy",),
    gh_cmd: str = "gh",
    deadline: float | None = None,
) -> Any:
    timeout, deadline_limited = _github_read_timeout(deadline)
    try:
        out = subprocess.run(
            [*net_wrapper, gh_cmd, *args],
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        if deadline_limited:
            raise GitHubDeadlineExceeded(
                "time budget exhausted during a GitHub API read"
            ) from error
        raise GitHubUnavailable(f"gh {' '.join(args)} timed out") from error
    _require_github_time(deadline)
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


def gh_mutate(args: list[str]) -> None:
    """Run one GitHub label mutation through the repository's gh boundary."""
    try:
        out = subprocess.run(
            [
                "with-proxy",
                str(Path(__file__).resolve().parents[1] / "bin" / "gh"),
                *args,
            ],
            capture_output=True,
            text=True,
            timeout=300,
        )
    except subprocess.TimeoutExpired as error:
        raise GitHubUnavailable(f"gh {' '.join(args)} timed out") from error
    if out.returncode != 0:
        detail = (out.stderr or out.stdout).strip()[:300]
        raise GitHubUnavailable(
            f"gh {' '.join(args)} failed with exit {out.returncode}: {detail}"
        )


def _gh_pages(
    path: str,
    surface: str,
    *,
    net_wrapper: Sequence[str] = ("with-proxy",),
    gh_cmd: str = "gh",
    deadline: float | None = None,
) -> list[dict[str, Any]]:
    pages = gh_json(
        ["api", path, "--paginate", "--slurp"],
        net_wrapper=net_wrapper,
        gh_cmd=gh_cmd,
        deadline=deadline,
    )
    try:
        return review_records_from_rest_pages(pages, surface)
    except ReviewRecordError as error:
        raise GitHubUnavailable(
            f"{path.split('?')[0]} returned malformed review records: {error}"
        ) from error


def github_review_records(
    repo: str,
    pr: object,
    *,
    net_wrapper: Sequence[str] = ("with-proxy",),
    gh_cmd: str = "gh",
    deadline: float | None = None,
) -> list[dict[str, Any]]:
    """Read and canonically order every review-bearing GitHub surface."""
    parts = repo.split("/")
    if len(parts) != 2 or not all(parts):
        raise GitHubUnavailable(f"repository name {repo!r} is not owner/name")
    owner, name = parts
    records = _gh_pages(
        f"repos/{repo}/issues/{pr}/comments?per_page=100",
        "issue-comment",
        net_wrapper=net_wrapper,
        gh_cmd=gh_cmd,
        deadline=deadline,
    )
    records.extend(
        _gh_pages(
            f"repos/{repo}/pulls/{pr}/comments?per_page=100",
            "pull-review-comment",
            net_wrapper=net_wrapper,
            gh_cmd=gh_cmd,
            deadline=deadline,
        )
    )
    pages = gh_json(
        [
            "api",
            "graphql",
            "--paginate",
            "--slurp",
            "-f",
            f"owner={owner}",
            "-f",
            f"name={name}",
            "-F",
            f"number={pr}",
            "-f",
            f"query={NATIVE_REVIEWS_QUERY}",
        ],
        net_wrapper=net_wrapper,
        gh_cmd=gh_cmd,
        deadline=deadline,
    )
    try:
        records.extend(native_review_records_from_graphql(pages))
    except ReviewRecordError as error:
        raise GitHubUnavailable(str(error)) from error
    try:
        records = annotate_current_review_authority(
            records,
            repo,
            lambda path: gh_json(
                ["api", path],
                net_wrapper=net_wrapper,
                gh_cmd=gh_cmd,
                deadline=deadline,
            ),
        )
    except GitHubDeadlineExceeded:
        raise
    except ReviewRecordError as error:
        raise ReviewPermissionUnavailable(str(error)) from error
    try:
        ordered = canonical_review_records(records)
    except ReviewRecordError as error:
        raise GitHubUnavailable(str(error)) from error

    _require_github_time(deadline)
    return ordered


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

    ⚠️ THE ACTUAL MECHANISM AT THE TIME: THIS MODULE HAD NO ENFORCING CONSUMER
    ON THE MERGE PATH. No lander or merge gate read a refusal marker; this module
    was reached only by ``ci-hub/health/pr_status.py``, which reported. The merge
    command therefore depended on its operator having read the comments.

    2591 confirms it from the other side: at merge time it carried NO
    ``passed-review-claude`` label either -- that label was applied at
    18:12:00Z, three minutes AFTER the merge. Neither the marker path nor the
    label path was consulted.

    The current lander and merge wrapper consume this shared parser before
    mutation. Keeping the historical gap here explains why that wiring is
    required; it is not a claim about the current path.
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

# One token shape covers optional fleet-name metadata in a disclosure or ``BY``
# clause. These fields are parsed for compatibility and reporting only.
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

# A permissive near-miss is enough to BLOCK, because an unreadable refusal must
# never become permission to merge.  It is not, by itself, enough to prove that
# the whole COMMENT is a refusal that another comment may retire.  In particular,
# a prose continuation beginning ``CHANGES-REQUESTED and ...`` uses the review
# state as a noun and can carry an incidental commit id later in the sentence.
#
# Keep the retirement boundary grammatical and small: canonical markers are
# accepted by ``parse_marker``; non-canonical forms must end at the token or use
# punctuation / the ordinary reason introducers already used by real refusals.
# The no-sha ``CHANGES-REQUESTED because ...`` form remains eligible, preserving
# the only precise exit for an otherwise permanent unbindable refusal.
_REFUSAL_STATEMENT_TAIL = re.compile(
    r"^\s*(?::|$|[-\N{EM DASH}]|(?:because|for|at|claude|codex)\b|[0-9a-f]{7,40}\b)",
    re.IGNORECASE,
)


# A leading disclosure is retained as audit metadata for old comments. It is not
# used to authorize, suppress, merge, retire, or otherwise classify a verdict.
#
# Parse only a leading, same-line disclosure. This keeps optional metadata from
# being taken from quoted prose or a later pasted comment. The first disclosure
# wins when more than one is present. No authority decision consumes this value.
_DISCLOSURE = re.compile(
    rf"^(?:\[[^\]\n]*\][ \t]*)*?\[\s*[a-z0-9]+\s*,\s*({_AGENT_ID})\s*,", re.I | re.M
)

def comment_author(body: str) -> "str | None":
    """Optional fleet-name metadata from a comment disclosure."""
    m = _DISCLOSURE.search(body or "")
    return m.group(1).lower() if m else None


def comment_issuer(body: str) -> "str | None":
    """Optional fleet-name metadata associated with a comment's marker.

    A marker's own ``BY`` clause wins over the surrounding disclosure tag. No
    approval, refusal, withdrawal, or retirement decision compares this value.
    """
    for line in (body or "").split("\n"):
        named = marker_issuer(line)
        if named:
            return named
    return comment_author(body or "")


def retirement_targets(body: str) -> list[str]:
    """Comment ids asserted by canonical ``RETIRES`` lines, in document order."""
    targets = []
    for line in marker_lines(body or ""):
        match = RETIRES.match(line)
        if match is not None:
            targets.append(match.group(1))
    return targets


def contains_retires_assertion(body: str) -> bool:
    """Whether prose contains a standalone ``RETIRES`` assertion of any shape.

    A malformed precise-retirement attempt must remain inert rather than become
    a generic withdrawal. Code blocks and blockquotes remain citations, while
    the same heading/list/emphasis decoration accepted for verdicts is harmless.
    """
    for raw in marker_lines(body or ""):
        stripped = raw.strip()
        if stripped.startswith(">") or (
            stripped.startswith("`") and stripped.endswith("`")
        ):
            continue
        normalized = strip_decoration(raw)
        if _RETIRES_ASSERTION.match(normalized) is not None:
            return True
    return False


def comment_database_id(record: dict[str, Any]) -> "str | None":
    """Numeric GitHub comment/review id, including GraphQL-record URL fallback."""
    for field in ("fullDatabaseId", "databaseId", "database_id", "id"):
        value = record.get(field)
        if isinstance(value, int) and not isinstance(value, bool):
            return str(value)
        if isinstance(value, str) and value.isdigit():
            return value
    for field in ("url", "html_url"):
        match = _COMMENT_URL_ID.search(str(record.get(field) or ""))
        if match is not None:
            return match.group(1)
    return None


class ReviewRecordError(ValueError):
    """A review record cannot support an authority or chronology decision."""


AUTHORIZED_REVIEW_PERMISSIONS = frozenset({"admin", "maintain", "write", "triage"})


def review_record_actor(record: dict[str, Any]) -> "str | None":
    """The immutable GitHub login that owns a review record."""
    for field in ("user", "author"):
        value = record.get(field)
        if not isinstance(value, dict):
            continue
        login = value.get("login")
        if isinstance(login, str) and login.strip():
            return login.strip().casefold()
    return None


def review_record_is_attestation(record: dict[str, Any]) -> bool:
    """Whether a record configures critical review requirements at one head."""
    return any(
        line.strip() == "REVIEW-DISPATCH-ATTESTATION"
        for line in marker_lines(str(record.get("body") or ""))
    )


def annotate_current_review_authority(
    records: "Sequence[dict[str, Any]]",
    repo: str,
    read_json: "Callable[[str], object]",
) -> list[dict[str, Any]]:
    """Bind review authority to the immutable GitHub actor's current permission.

    For every record that can approve, refuse, retire, or configure critical
    review requirements, independently read the actor's current repository
    permission and retain the result beside the
    immutable ``user.login``/``author.login`` field. A disclosure and marker
    ``BY`` value are optional metadata only. Missing actors, malformed permission
    replies and API failures refuse the whole review read; none may degrade to
    a caller-supplied fleet name.
    """
    candidates: dict[int, str] = {}
    actors: set[str] = set()
    for position, record in enumerate(records):
        if not (
            review_record_has_authority(record)
            or review_record_is_attestation(record)
        ):
            continue
        actor = review_record_actor(record)
        if actor is None:
            continue
        candidates[position] = actor
        actors.add(actor)

    authorized: dict[str, str] = {}
    observed_permissions: dict[str, str] = {}
    for actor in sorted(actors):
        try:
            payload = read_json(f"repos/{repo}/collaborators/{actor}/permission")
        except GitHubDeadlineExceeded:
            raise
        except Exception as error:
            raise ReviewRecordError(
                f"cannot determine current review authorization for GitHub actor {actor}: {error}"
            ) from error
        if not isinstance(payload, dict):
            raise ReviewRecordError(
                f"current review authorization for GitHub actor {actor} is not an object"
            )
        payload_user = payload.get("user")
        payload_login = (
            payload_user.get("login")
            if isinstance(payload_user, dict)
            else None
        )
        role_name = payload.get("role_name")
        permission = payload.get("permission")
        if (
            not isinstance(payload_login, str)
            or payload_login.casefold() != actor
        ):
            raise ReviewRecordError(
                f"current review authorization for GitHub actor {actor} is malformed"
            )
        permission_candidates = [
            value.casefold()
            for value in (role_name, permission)
            if isinstance(value, str) and value
        ]
        observed_permissions[actor] = "/".join(permission_candidates)
        if not permission_candidates:
            raise ReviewRecordError(
                f"current review authorization for GitHub actor {actor} is malformed"
            )
        authorized_permission = next(
            (
                candidate
                for candidate in permission_candidates
                if candidate in AUTHORIZED_REVIEW_PERMISSIONS
            ),
            None,
        )
        if authorized_permission is not None:
            authorized[actor] = authorized_permission

    annotated: list[dict[str, Any]] = []
    for position, record in enumerate(records):
        preserved = dict(record)
        if (
            review_record_has_authority(record)
            or review_record_is_attestation(record)
        ):
            actor = candidates.get(position)
            preserved["_current_review_actor_authorized"] = (
                actor is not None and actor in authorized
            )
            preserved["_current_review_actor_login"] = review_record_actor(record)
            preserved["_current_review_actor_permission"] = (
                (authorized.get(actor) or observed_permissions.get(actor))
                if actor is not None
                else None
            )
        annotated.append(preserved)
    return annotated


NATIVE_REVIEWS_QUERY = """
query($owner: String!, $name: String!, $number: Int!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviews(first: 100, after: $endCursor) {
        nodes {
          fullDatabaseId
          id
          url
          author { login }
          body
          state
          submittedAt
          updatedAt
          lastEditedAt
          includesCreatedEdit
          commit { oid }
        }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
""".strip()


def native_review_records_from_graphql(pages: object) -> list[dict[str, Any]]:
    """Flatten the paginated GraphQL native-review projection or refuse it."""
    if not isinstance(pages, list):
        raise ReviewRecordError("native-review query did not return page objects")
    if not pages:
        raise ReviewRecordError("native-review query returned no page")
    records: list[dict[str, Any]] = []
    cursors: set[str] = set()
    for page_number, page in enumerate(pages, 1):
        if not isinstance(page, dict):
            raise ReviewRecordError(
                f"native-review page {page_number} is not an object"
            )
        if "errors" in page:
            errors = page["errors"]
            if not isinstance(errors, list) or errors:
                raise ReviewRecordError(
                    f"native-review page {page_number} contains GraphQL errors"
                )
        try:
            reviews = page["data"]["repository"]["pullRequest"]["reviews"]
            nodes = reviews["nodes"]
            page_info = reviews["pageInfo"]
        except (KeyError, TypeError):
            raise ReviewRecordError(
                f"native-review page {page_number} has no complete reviews connection"
            ) from None
        if not isinstance(nodes, list) or any(not isinstance(node, dict) for node in nodes):
            raise ReviewRecordError(
                f"native-review page {page_number} has a malformed reviews.nodes array"
            )
        if not isinstance(page_info, dict) or type(page_info.get("hasNextPage")) is not bool:
            raise ReviewRecordError(
                f"native-review page {page_number} has malformed pageInfo"
            )
        has_next = page_info["hasNextPage"]
        cursor = page_info.get("endCursor")
        if has_next:
            if not isinstance(cursor, str) or not cursor or cursor in cursors:
                raise ReviewRecordError(
                    f"native-review page {page_number} has no new endCursor"
                )
            if page_number == len(pages):
                raise ReviewRecordError(
                    "native-review query ended while pageInfo.hasNextPage is true"
                )
            cursors.add(cursor)
        elif page_number != len(pages):
            raise ReviewRecordError(
                f"native-review page {page_number} says it is final but more pages follow"
            )
        for node in nodes:
            state = node.get("state")
            if not isinstance(state, str) or state.upper() not in {
                "APPROVED",
                "CHANGES_REQUESTED",
                "COMMENTED",
                "DISMISSED",
                "PENDING",
            }:
                raise ReviewRecordError(
                    f"native-review page {page_number} has a review with malformed state"
                )
            preserved = dict(node)
            preserved["_review_surface"] = "pull-request-review"
            records.append(preserved)
    return records


def review_records_from_rest_pages(
    pages: object, surface: str
) -> list[dict[str, Any]]:
    """Flatten REST pages while retaining their authoritative source type.

    REST records remain otherwise unchanged, including ``user.login``, numeric
    id, URL, body, and creation/update timestamps.  The source discriminator is
    assigned by the caller from the endpoint it actually queried rather than
    trusted from returned or fixture data.
    """
    if (
        not isinstance(pages, list)
        or not pages
        or any(not isinstance(page, list) for page in pages)
    ):
        raise ReviewRecordError("REST review query did not return page arrays")
    if any(not isinstance(record, dict) for page in pages for record in page):
        raise ReviewRecordError("REST review query returned a non-object record")
    records: list[dict[str, Any]] = []
    for page in pages:
        for record in page:
            preserved = dict(record)
            preserved["_review_surface"] = surface
            records.append(preserved)
    return records


def _github_time(value: object) -> "datetime.datetime | None":
    if not isinstance(value, str) or not value.strip():
        return None
    try:
        parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed.astimezone(datetime.timezone.utc)


def review_record_event_time(record: dict[str, Any]) -> "datetime.datetime | None":
    """Return the record's real creation/submission event time."""
    submitted = _github_time(record.get("submitted_at") or record.get("submittedAt"))
    if submitted is not None:
        return submitted
    return _github_time(record.get("created_at") or record.get("createdAt"))


def review_record_version_time(record: dict[str, Any]) -> "datetime.datetime | None":
    """Return when the currently-read body became authoritative.

    Issue comments and inline review comments are editable, so their creation
    time is not the identity of the body being evaluated.  Their latest update
    time is.  A GraphQL issue-comment record that explicitly says it was never
    edited may use its creation time when that projection omits ``updatedAt``.
    Submitted native reviews expose their original ``submittedAt`` plus the
    current object's ``updatedAt`` and, for a body edit, ``lastEditedAt``.  A
    native review may also be dismissed after submission, so submission time
    alone is never enough to identify the version being evaluated.

    Missing or contradictory metadata returns ``None``.  Exact retirement must
    then fail closed rather than infer chronology from endpoint concatenation.
    """
    submitted_value = record.get("submitted_at") or record.get("submittedAt")
    submitted = _github_time(submitted_value)
    if submitted is not None:
        updated_value = record.get("updated_at") or record.get("updatedAt")
        updated = _github_time(updated_value)
        if updated is None:
            # REST supplies a stable numeric id and submitted_at but no current
            # version timestamp.  `includesCreatedEdit == false` rules out one
            # body-edit shape, but does not rule out a later review dismissal.
            # Production readers therefore combine the numeric GraphQL database
            # id with GraphQL updatedAt and refuse a submittedAt-only record.
            return None
        if updated < submitted:
            return None
        last_edited_value = record.get("last_edited_at") or record.get("lastEditedAt")
        last_edited = _github_time(last_edited_value)
        if last_edited_value is not None and last_edited is None:
            return None
        if last_edited is not None and last_edited < submitted:
            return None
        return max(updated, last_edited or updated)

    created = _github_time(record.get("created_at") or record.get("createdAt"))
    if created is None:
        return None
    updated_value = record.get("updated_at") or record.get("updatedAt")
    updated = _github_time(updated_value)
    if updated is None:
        # `gh pr view --json comments` exposes this boolean even on projections
        # that omit updatedAt.  False proves the current body is the created body;
        # missing/true does not prove which version was reviewed.
        if record.get("includesCreatedEdit") is False:
            updated = created
        else:
            return None
    if updated < created:
        return None
    return updated


def review_record_head(record: dict[str, Any]) -> "str | None":
    """Exact commit reviewed by a native GitHub review, when present."""
    commit = record.get("commit")
    oid = commit.get("oid") if isinstance(commit, dict) else None
    if isinstance(oid, str) and SHA40.fullmatch(oid.lower()):
        return oid.lower()
    commit_id = record.get("commit_id") or record.get("commitId")
    if isinstance(commit_id, str) and SHA40.fullmatch(commit_id.lower()):
        return commit_id.lower()
    return None


def review_record_has_authority(record: dict[str, Any]) -> bool:
    """Whether ordering this record can change review authority."""
    if str(record.get("state") or "").upper() in {
        "APPROVED",
        "CHANGES_REQUESTED",
        "DISMISSED",
    }:
        return True
    body = str(record.get("body") or "")
    return any(marker_assertion(line) for line in marker_lines(body))


def review_record_decisions(
    record: dict[str, Any],
) -> set[tuple[str, str | None, str | None]]:
    """Canonical authority decisions, excluding optional fleet-name metadata."""
    decisions: set[tuple[str, str | None, str | None]] = set()
    for line in marker_lines(str(record.get("body") or "")):
        parsed = parse_marker(line)
        if parsed is not None:
            decisions.add((parsed[0], parsed[1], parsed[2]))
    state = str(record.get("state") or "").upper()
    if state == "APPROVED":
        decisions.add((MARKER_APPROVED, None, review_record_head(record)))
    elif state == "CHANGES_REQUESTED":
        decisions.add((MARKER_REFUSED, None, review_record_head(record)))
    elif state == "DISMISSED":
        decisions.add((MARKER_WITHDRAWN, None, review_record_head(record)))
    return decisions


def _decisions_conflict(
    left: set[tuple[str, str | None, str | None]],
    right: set[tuple[str, str | None, str | None]],
) -> bool:
    """Whether ordering two simultaneous records could change review authority."""
    for left_kind, left_lane, left_head in left:
        for right_kind, right_lane, right_head in right:
            same_lane = (
                left_lane is None
                or right_lane is None
                or left_lane == right_lane
            )
            if same_lane and (left_kind, left_head) != (right_kind, right_head):
                return True
    return False


def canonical_review_records(
    records: "Sequence[dict[str, Any]]",
) -> list[dict[str, Any]]:
    """Validate and order records from all GitHub review surfaces.

    The three REST/GraphQL containers are independently ordered. Concatenating
    them makes endpoint choice decide whether an approval, refusal, withdrawal,
    or edited objection appears later. Mutable comments are interpreted from
    their current bodies, so order those bodies by the version/update time that
    made the currently-read text authoritative. A stable numeric GitHub id is a
    deterministic tie-breaker only when tied records cannot change the decision.
    """
    ordered: list[tuple[datetime.datetime, int, dict[str, Any]]] = []
    seen: set[str] = set()
    for position, record in enumerate(records):
        if not isinstance(record, dict):
            raise ReviewRecordError(f"review record {position} is not an object")
        record_id = comment_database_id(record)
        if record_id is None:
            raise ReviewRecordError(
                f"review record {position} has no stable numeric GitHub id"
            )
        if record_id in seen:
            raise ReviewRecordError(
                f"review record id {record_id} appears more than once"
            )
        seen.add(record_id)
        event_time = review_record_event_time(record)
        version_time = review_record_version_time(record)
        if event_time is None or version_time is None:
            raise ReviewRecordError(
                f"review record {record_id} has no readable immutable event/version time"
            )
        if (
            record.get("_review_surface") == "pull-request-review"
            and str(record.get("state") or "").upper()
            in {"APPROVED", "CHANGES_REQUESTED"}
            and review_record_head(record) is None
        ):
            raise ReviewRecordError(
                f"native review record {record_id} has no exact reviewed commit"
            )
        surface = record.get("_review_surface")
        if surface is not None and surface not in {
            "issue-comment",
            "pull-review-comment",
            "pull-request-review",
        }:
            raise ReviewRecordError(
                f"review record {record_id} has no recognized review surface"
            )
        ordered.append((version_time, int(record_id), record))

    # Numeric database ids are stable only within their GitHub object type.  If
    # two authority-bearing records from different surfaces have the same
    # timestamp, sorting by those unrelated ids invents chronology.  Refuse the
    # ambiguity instead of deciding which verdict happened later.
    by_time: dict[datetime.datetime, list[dict[str, Any]]] = {}
    for version_time, _record_id, record in ordered:
        if review_record_has_authority(record):
            by_time.setdefault(version_time, []).append(record)
    for version_time, tied in by_time.items():
        if len({str(record.get("_review_surface")) for record in tied}) > 1:
            raise ReviewRecordError(
                "review authority chronology is ambiguous across GitHub surfaces at "
                + version_time.isoformat()
            )
        decisions = [review_record_decisions(record) for record in tied]
        if any(
            _decisions_conflict(left, right)
            for index, left in enumerate(decisions)
            for right in decisions[index + 1 :]
        ):
            raise ReviewRecordError(
                "opposing review authority has the same version time at "
                + version_time.isoformat()
            )
    ordered.sort(key=lambda item: (item[0], item[1]))
    return [record for _time, _id, record in ordered]


def retirement_claim(
    body: str,
) -> "tuple[str, str, str, str | None] | None":
    """One exact objection retirement as ``(comment, lane, sha, metadata)``.

    The comment carrying the claim supplies repository and pull-request context.
    Authority additionally requires exactly one canonical withdrawal, exactly
    one ``RETIRES`` target, and an exact head binding. Current repository
    permission for the immutable GitHub actor supplies authority. A disclosure
    or marker ``BY`` value is retained only as optional audit metadata; missing
    or conflicting fleet-agent names cannot grant or deny the retirement.
    Multiple targets or withdrawal markers are ambiguous and carry no authority.
    """
    targets = retirement_targets(body)
    withdrawals: list[tuple[str, str, str | None]] = []
    for raw in marker_lines(body or ""):
        parsed = parse_marker(raw)
        if parsed is None or parsed[0] != MARKER_WITHDRAWN:
            continue
        withdrawals.append((parsed[1], parsed[2], marker_issuer(raw)))
    if len(targets) != 1 or len(withdrawals) != 1:
        return None
    lane, sha, marker_author = withdrawals[0]
    return targets[0], lane, sha, marker_author or comment_author(body)


def retirement_allowed(
    refusal_lane: "str | None",
    current_head: "str | None",
    claims: "Sequence[tuple[str, str, str, str | None]]",
) -> bool:
    """Whether an exact authorized claim resolves this refusal at current head.

    The original issuer is deliberately irrelevant.  The claim is evidence that
    a currently authorized GitHub actor examined the named objection and
    recorded that it is resolved. Exact comment identity links the claim to the old
    refusal. The withdrawal lane must match that refusal, while the withdrawal
    head must match the current repaired head; requiring the old refused head
    here would make every fix commit impossible to retire. A stale-head record
    carries no authority for the current head.
    """
    if current_head is None:
        return False
    for _target, claim_lane, claim_sha, _author in claims:
        # A readable refusal lane must agree. A genuinely unbindable refusal has
        # no lane to compare; its exact comment id remains its only supported
        # exit, as it was before this issuer-policy change.
        if refusal_lane is not None and claim_lane != refusal_lane:
            continue
        if not sha_names_same_head(claim_sha, current_head):
            continue
        return True
    return False


def retirement_record_allowed(
    refusal_record: dict[str, Any],
    refusal_lane: "str | None",
    current_head: "str | None",
    claim_records: "Sequence[dict[str, Any]]",
) -> bool:
    """Whether a later record precisely retires this current refusal version.

    A numeric comment id identifies a GitHub object, not an immutable body: an
    issue or inline comment can be edited after somebody retired its earlier
    text.  Therefore the current refusal body is retired only by an authorized
    exact claim whose own version time is strictly later than the refusal's
    current version time.  Missing timestamps, equal timestamps, an edit after
    the withdrawal, and a guessed future id all remain blocking.
    """
    if refusal_record.get("_retirement_targetable") is False:
        return False
    refusal_id = comment_database_id(refusal_record)
    refusal_time = review_record_version_time(refusal_record)
    if refusal_id is None or refusal_time is None:
        return False
    for claim_record in claim_records:
        if claim_record.get("_current_review_actor_authorized") is not True:
            continue
        claim = retirement_claim(str(claim_record.get("body") or ""))
        if claim is None or claim[0] != refusal_id:
            continue
        actor = review_record_actor(claim_record)
        if actor is None:
            continue
        if claim_record.get("_current_review_actor_login") != actor:
            continue
        if claim_record.get("_current_review_actor_permission") not in (
            AUTHORIZED_REVIEW_PERMISSIONS
        ):
            continue
        claim_time = review_record_version_time(claim_record)
        if claim_time is None or claim_time <= refusal_time:
            continue
        if retirement_allowed(refusal_lane, current_head, [claim]):
            return True
    return False


def sha_names_same_head(a: "str | None", b: "str | None") -> bool:
    """True when two shas name one head; a citation may abbreviate the issuance."""
    if not a or not b:
        return False
    return a.startswith(b) or b.startswith(a)


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


def comment_contains_refusal(body: str) -> bool:
    """Whether ``body`` contains a refusal that an explicit retirement may name.

    Marker discovery remains deliberately permissive and fail-closed.  Retirement
    is an affirmative claim that a specific comment issued a refusal, so it uses
    this additional boundary rather than turning every refusal-shaped prose line
    into authority to clear itself.
    """
    for raw in marker_lines(body or ""):
        line = strip_decoration(raw)
        starts = marker_assertion_starts(line)
        if len(starts) != 1:
            continue
        parsed = parse_marker(line)
        if parsed is not None and parsed[0] == MARKER_REFUSED:
            return True
        match = near_miss_refusal(line, starts[0])
        if (
            match is not None
            and match.start() == starts[0]
            and _REFUSAL_STATEMENT_TAIL.match(line[match.end() :]) is not None
        ):
            return True
    return False


def review_record_refusals(
    record: dict[str, Any],
) -> list[tuple["str | None", "str | None", str]]:
    """Refusal assertions carried by one normalized GitHub review record."""
    found: list[tuple[str | None, str | None, str]] = []
    for raw in marker_lines(str(record.get("body") or "")):
        line = strip_decoration(raw)
        parsed = parse_marker(line)
        if parsed is not None and parsed[0] == MARKER_REFUSED:
            found.append((parsed[1], parsed[2], line.strip()))
            continue
        starts = marker_assertion_starts(line)
        if len(starts) != 1:
            continue
        near = near_miss_refusal(line, starts[0])
        if near is None or near.start() != starts[0]:
            continue
        tail = line[near.end() :]
        if is_template(tail) or _REFUSAL_STATEMENT_TAIL.match(tail) is None:
            continue
        lane_match = re.search(r"\b(claude|codex)\b", tail[:40], re.IGNORECASE)
        sha_match = SHA40.search(tail)
        found.append(
            (
                lane_match.group(1).lower() if lane_match else None,
                sha_match.group(0).lower() if sha_match else None,
                line.strip(),
            )
        )

    if not found and str(record.get("state") or "").upper() == "CHANGES_REQUESTED":
        found.append(
            (
                None,
                review_record_head(record),
                "(GitHub review state CHANGES_REQUESTED)",
            )
        )
    return found


def accepted_exact_retirements(
    records: "Sequence[dict[str, Any]]", current_head: str
) -> list[dict[str, Any]]:
    """Exact, current-authorized RETIRES records accepted for this head."""
    ordered = canonical_review_records(records)
    by_id = {
        record_id: record
        for record in ordered
        if (record_id := comment_database_id(record)) is not None
    }
    claims: dict[str, list[dict[str, Any]]] = {}
    for record in ordered:
        claim = retirement_claim(str(record.get("body") or ""))
        if claim is not None:
            claims.setdefault(claim[0], []).append(record)

    accepted: list[dict[str, Any]] = []
    for target, claim_records in claims.items():
        refusal = by_id.get(target)
        if refusal is None:
            continue
        contexts = review_record_refusals(refusal)
        if len(contexts) != 1:
            continue
        lane, _sha, _message = contexts[0]
        if not retirement_record_allowed(
            refusal, lane, current_head, claim_records
        ):
            continue
        for claim_record in claim_records:
            claim = retirement_claim(str(claim_record.get("body") or ""))
            if (
                claim is not None
                and claim[0] == target
                and claim_record.get("_current_review_actor_authorized") is True
                and retirement_record_allowed(
                    refusal, lane, current_head, [claim_record]
                )
            ):
                accepted.append(
                    {
                        "target": target,
                        "issuer": comment_issuer(str(refusal.get("body") or "")),
                        "retired_by": review_record_actor(claim_record),
                        "record": claim_record,
                    }
                )
                break
    return accepted


def unavailable_approval_evidence(
    records: "Sequence[dict[str, Any]]", lane: str
) -> list[dict[str, Any]]:
    """Canonical approval artifacts that lack current GitHub actor authority."""
    unavailable: list[dict[str, Any]] = []
    for record in records:
        if record.get("_current_review_actor_authorized") is not False:
            continue
        for line in marker_lines(str(record.get("body") or "")):
            parsed = parse_marker(line)
            if (
                parsed is not None
                and parsed[0] == MARKER_APPROVED
                and parsed[1] == lane
            ):
                unavailable.append(
                    {
                        "id": comment_database_id(record),
                        "url": record.get("html_url") or record.get("url"),
                        "head": parsed[2],
                        "github_actor": record.get("_current_review_actor_login"),
                        "permission": record.get("_current_review_actor_permission"),
                    }
                )
    return unavailable


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
    unanswered_records: list[dict[str, Any]] | None = None,
    current_head: str | None = None,
) -> list[str]:
    """SHAs a lane EXPLICITLY bound itself to, oldest first. Never inferred.

    `unanswered` is an OUT-PARAMETER carrying refusals that have not been
    discharged, as (sha, issuer-metadata) pairs, oldest first. A later canonical
    approval or withdrawal in the same lane at the exact current head discharges
    earlier lane objections only when the approval binds a different, later head.
    A same-head approval conflicts with the objection and does not discharge it.
    ``RETIRES <comment-id>`` remains the stronger path
    for resolving one named objection. A departed, missing, conflicting, or
    host-valued fleet name neither blocks nor authorizes either path; live records
    use the immutable GitHub actor and its current repository permission.

    `refusals` is an OUT-PARAMETER, in the style of `malformed`: when supplied,
    every currently active refusal SHA is appended, oldest first. It exists because
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

    # Live GitHub records carry event/version metadata. Once any record carries
    # it, require the complete stream to identify every object and its current
    # version, then establish one cross-surface order here before applying the
    # chronological approval/refusal rules below. Body-only legacy fixtures may
    # retain explicit list order, but they cannot authorize exact retirement:
    # `retirement_record_allowed` requires the real metadata on both records.
    if any(
        any(
            field in comment
            for field in (
                "created_at",
                "createdAt",
                "updated_at",
                "updatedAt",
                "submitted_at",
                "submittedAt",
            )
        )
        for comment in comments
    ):
        try:
            comments = canonical_review_records(comments)
        except ReviewRecordError as error:
            finding = ("?", f"UNREADABLE REVIEW EVENT ORDER: {error}")
            if finding not in malformed:
                malformed.append(finding)
            if pr_number is not None and malformed_prs is not None:
                malformed_prs[finding] = pr_number
            return []

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
    # Refusals in THIS lane that have not been reversed or precisely retired, as
    # (sha, issuer, comment-id), oldest first. The comment id is retained only
    # inside this function so `RETIRES <comment-id>` can address one refusal;
    # the public `unanswered` out-parameter remains (sha, issuer).
    # Maintained unconditionally rather than only
    # when the caller asks: the discharge rule has to see every marker in order,
    # and computing it twice is how two readers of one grammar drift apart.
    outstanding: list[
        tuple[
            str,
            "str | None",
            object | None,
            dict[str, Any],
            str,
            "str | None",
        ]
    ] = []

    # A RETIRES claim has authority only when it is the one exact,
    # current-head-bound target of a canonical withdrawal from a GitHub actor
    # whose current repository permission was independently fetched.
    # Collect claims first because a resolution can appear after the refusal.
    retire_claims: dict[str, list[dict[str, Any]]] = {}
    for c in comments:
        body = c.get("body") or ""
        claim = retirement_claim(body)
        if claim is not None:
            retire_claims.setdefault(claim[0], []).append(c)

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
        comment_id = comment_database_id(c)
        refusal_issuer = comment_issuer(body)
        refusal_entries = review_record_refusals(c)
        all_refusal_bindings = [
            (refusal_lane, refusal_sha)
            for refusal_lane, refusal_sha, _message in refusal_entries
        ]
        refusal_bindings = [
            binding for binding in all_refusal_bindings if refuses_lane(binding)
        ]
        named_retirement = (
            comment_id is not None
            and len(all_refusal_bindings) == 1
            and len(refusal_bindings) == 1
            and retirement_record_allowed(
                c,
                refusal_bindings[0][0],
                current_head,
                retire_claims.get(str(comment_id), []),
            )
        )
        comment_refuses_lane = bool(refusal_bindings)
        relevant_rejection = comment_refuses_lane and not named_retirement
        precise_withdrawal = contains_retires_assertion(body)
        record_authorized = c.get("_current_review_actor_authorized") is not False
        current_head_withdrawal = any(
            parsed is not None
            and parsed[0] == MARKER_WITHDRAWN
            and parsed[1] == lane
            and current_head is not None
            and parsed[2].casefold() == current_head.casefold()
            for parsed in (parse_marker(line) for line in lines)
        )
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
                refusals.extend(
                    refusal_sha or ""
                    for _refusal_lane, refusal_sha in refusal_bindings
                )
            # Retain optional issuer metadata on the same records used for the
            # refusal report. Authority does not compare this value.
            for refusal_lane, refusal_sha in refusal_bindings:
                matching_line = next(
                    (
                        message
                        for entry_lane, entry_sha, message in refusal_entries
                        if (entry_lane, entry_sha) == (refusal_lane, refusal_sha)
                    ),
                    "(GitHub review state CHANGES_REQUESTED)",
                )
                outstanding.append(
                    (
                        refusal_sha or "",
                        refusal_issuer,
                        comment_id,
                        c,
                        matching_line,
                        refusal_lane,
                    )
                )
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
        # A canonical current-head withdrawal is a later substantive lane
        # decision. An exact RETIRES assertion uses the narrower comment-id path
        # above and must not also clear unrelated objections in the lane.
        if current_head_withdrawal and not precise_withdrawal and record_authorized:
            outstanding.clear()
            if refusals is not None:
                refusals.clear()
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
                    if (
                        _kind == MARKER_REFUSED
                        and _lane_m is not None
                        and _lane_m.group(1).lower() != lane
                    ):
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
            if explicit_lane.lower() == lane:
                # Live GitHub records are annotated before evaluation. A record
                # whose immutable actor lacks current review permission cannot
                # grant a lane. Legacy in-memory fixtures have no annotation and
                # retain their syntax-only behavior.
                if c.get("_current_review_actor_authorized") is False:
                    continue
                if (
                    current_head is not None
                    and sha.casefold() == current_head.casefold()
                ):
                    # A later-head approval answers objections to older code. At
                    # one head, approval plus refusal is conflicting evidence;
                    # only a canonical withdrawal or exact RETIRES can clear it.
                    outstanding[:] = [
                        held
                        for held in outstanding
                        if sha_names_same_head(held[0], current_head)
                    ]
                    if refusals is not None:
                        refusals[:] = [held[0] for held in outstanding]
                    if outstanding:
                        # Preserve the refusal in every caller, including older
                        # consumers that ask only for `refusals` and do not pass
                        # the richer `unanswered` out-parameter.
                        continue
                found.append(sha)
                # Optional issuer metadata stays index-aligned with the binding.
                # No admission or retirement decision compares this value.
                if issuers is not None:
                    issuers.append((sha, marker_issuer(line) or author))
    if unanswered is not None:
        unanswered.extend(
            (sha, issuer)
            for sha, issuer, _cid, _record, _line, _refusal_lane in outstanding
        )
    if unanswered_records is not None:
        unanswered_records.extend(
            {
                "id": str(comment_id) if comment_id is not None else None,
                "lane": refusal_lane,
                "sha": sha,
                "issuer": issuer,
                "message": line,
                "record": record,
            }
            for sha, issuer, comment_id, record, line, refusal_lane in outstanding
        )
    return found


def canonical_review_lane_rows(
    records: "Sequence[dict[str, Any]]",
    current_head: str,
    *,
    pr_number: int | None = None,
    malformed: list[tuple[str, str]] | None = None,
    malformed_prs: dict[tuple[str, str], int] | None = None,
) -> list[dict[str, Any]]:
    """Return the shared per-lane authority rows for canonical review records."""
    ordered = canonical_review_records(records)
    findings = malformed if malformed is not None else []
    rows: list[dict[str, Any]] = []
    for lane in LANES:
        refusals: list[str] = []
        unanswered: list[tuple[str, str | None]] = []
        issuers: list[tuple[str, str | None]] = []
        unavailable = unavailable_approval_evidence(ordered, lane)
        shas = lane_shas(
            ordered,
            lane,
            findings,
            pr_number,
            malformed_prs,
            refusals=refusals,
            unanswered=unanswered,
            issuers=issuers,
            current_head=current_head,
        )
        if not shas and not refusals and not unavailable:
            continue
        rows.append(
            {
                "number": pr_number,
                "label": None,
                "source": "comment",
                "lane": lane,
                "review_authority_unavailable": unavailable,
                "head": current_head,
                "bound_to": shas[-1] if shas else None,
                "binding_issuer": issuers[-1][1] if issuers else None,
                "refused_at": refusals[-1] if refusals else None,
                "state": classify_approval(
                    current_head,
                    shas,
                    refused=bool(refusals),
                    refused_at=refusals[-1] if refusals else None,
                    unanswered=bool(unanswered),
                ),
                "refusal_issuers": [
                    issuer for sha, issuer in unanswered if sha == current_head
                ],
            }
        )
    return rows


def unresolved_review_objections(
    records: "Sequence[dict[str, Any]]", current_head: str
) -> list[dict[str, Any]]:
    """Return every unresolved objection from all supported GitHub surfaces.

    This is the shared decision used by approval reporting and both merge
    boundaries. The lane evaluator owns chronology and the exact-retirement
    rule; this wrapper merely runs it for the two existing
    review lanes and removes the duplicate produced by a lane-less native
    GitHub refusal.
    """
    ordered = canonical_review_records(records)
    malformed: list[tuple[str, str]] = []
    unresolved: list[dict[str, Any]] = []
    for lane in LANES:
        lane_unresolved: list[dict[str, Any]] = []
        lane_shas(
            ordered,
            lane,
            malformed,
            unanswered_records=lane_unresolved,
            current_head=current_head,
        )
        unresolved.extend(lane_unresolved)
    if malformed:
        detail = "; ".join(sorted({message for _where, message in malformed}))
        raise ReviewRecordError(detail)

    unique: dict[tuple[object, ...], dict[str, Any]] = {}
    for item in unresolved:
        lane = item["lane"]
        if item["message"] == "(GitHub review state CHANGES_REQUESTED)":
            lane = None
            item = {**item, "lane": None}
        key = (
            item.get("id"),
            lane,
            item.get("sha"),
            item.get("message"),
        )
        unique.setdefault(key, item)
    return list(unique.values())


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
    """Return canonical critical classes bound to ``head``.

    ``review-attest --critical`` is the existing closed-enum classification
    source.  Its durable comment carries the class and exact head together.
    This parser deliberately does not infer criticality from paths or prose.
    """
    classes: set[str] = set()
    for comment in comments:
        if comment.get("_current_review_actor_authorized") is False:
            continue
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
             "--json", "number,headRefOid,labels,mergeable,body,state,isDraft"]
        ) or []
    else:
        pr = gh_json(
            ["pr", "view", str(pr_number), "--repo", repo,
             "--json", "number,headRefOid,labels,mergeable,body,state,isDraft"]
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
        # All-open mode deliberately pays the three review-surface reads, plus
        # permission reads for actors carrying authority, for every PR. A label
        # may describe cache state after this read; it cannot be the precondition
        # for discovering an unlabeled approval or objection. Measured 2026-09-05:
        # Hermit 46 PRs -> 198 HTTP requests / 11.3s; Reverie 7 PRs -> 30 / 2.52s,
        # with every paginated response fitting one page. The configured hourly
        # runs plus the daily Hermit run peak at 426 requests in one hour (324
        # REST, 102 GraphQL) before retries or reconciliation, against each
        # API's observed 5,000/hour limit. Re-measure if population or cadence grows.
        head = pr["headRefOid"]
        comments = github_review_records(repo, pr["number"])
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
            issuers: list[tuple[str, "str | None"]] = []
            unavailable: list[dict[str, Any]] = []
            if label == LOCALLY_VALIDATED_LABEL:
                lane = "local-validation"
                shas = local_receipt_heads(comments, repo)
            else:
                lane = label.rsplit("-", 1)[-1]
                unavailable = unavailable_approval_evidence(comments, lane)
                shas = lane_shas(
                    comments, lane, malformed, pr["number"], malformed_prs,
                    refusals=refusals,
                    unanswered=unanswered,
                    issuers=issuers,
                    current_head=head,
                )
            rows.append(
                {
                    "number": pr["number"],
                    "label": label,
                    "source": "label",
                    "lane": lane,
                    "review_authority_unavailable": unavailable,
                    "head": head,
                    "bound_to": shas[-1] if shas else None,
                    "binding_issuer": issuers[-1][1] if issuers else None,
                    "refused_at": refusals[-1] if refusals else None,
                    "state": classify_approval(
                        head, shas, refused=bool(refusals),
                        refused_at=refusals[-1] if refusals else None,
                        unanswered=bool(unanswered),
                    ),
                    # The ISSUER of every unanswered refusal at this head, in
                    # order, with None preserved for one whose disclosure tag
                    # cannot be read. This is descriptive provenance, not a
                    # retirement authority: an exact authorized RETIRES record
                    # may resolve the objection regardless of whether this issuer
                    # is still present.
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
        # They carry the same exact-head authority as a label-indexed row; the
        # missing label is a cache miss, not missing review evidence.
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
            issuers: list[tuple[str, "str | None"]] = []
            unavailable = unavailable_approval_evidence(comments, lane)
            shas = lane_shas(
                comments, lane, malformed, pr["number"], malformed_prs,
                refusals=refusals,
                unanswered=unanswered,
                issuers=issuers,
                current_head=head,
            )
            # ⚠️ `if not shas` ALONE DROPPED EVERY UNLABELLED REFUSAL. A refusal
            # contributes no SHA, so the row that would have carried it was
            # skipped here and the blocked head appeared in NO row of the census
            # -- the same cache-miss-reads-as-absence shape documented above, one
            # layer down. Measured 2026-08-26 on hermit#2655: zero labels, two
            # lanes refused with blocking findings, zero rows, and the head kept
            # being re-dispatched as needing a reviewer.
            if not shas and not refusals and not unavailable:
                continue
            rows.append(
                {
                    "number": pr["number"],
                    "label": None,
                    "source": "comment",
                    "lane": lane,
                    "review_authority_unavailable": unavailable,
                    "head": head,
                    "bound_to": shas[-1] if shas else None,
                    "binding_issuer": issuers[-1][1] if issuers else None,
                    "refused_at": refusals[-1] if refusals else None,
                    "state": classify_approval(
                        head, shas, refused=bool(refusals),
                        refused_at=refusals[-1] if refusals else None,
                        unanswered=bool(unanswered),
                    ),
                    # The ISSUER of every unanswered refusal at this head, in
                    # order, with None preserved for one whose disclosure tag
                    # cannot be read. This is descriptive provenance, not a
                    # retirement authority: an exact authorized RETIRES record
                    # may resolve the objection regardless of whether this issuer
                    # is still present.
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
            "state": item["pr"].get("state"),
            "draft": item["pr"].get("isDraft") is True,
            "change_author": comment_author(item["pr"].get("body") or ""),
            "comments": item["comments"],
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
        comments = []
        for index, item in enumerate(pr.get("comments", [])):
            if isinstance(item, str):
                record = {"body": item}
            elif isinstance(item, dict):
                record = dict(item)
            else:
                raise ValueError(
                    f"fixture PR #{pr['number']} comment {index} must be a string or object"
                )
            record.setdefault(
                "url", f"fixture://pr/{pr['number']}/comment/{index}"
            )
            comments.append(record)
        if pr_number is not None:
            selected = {
                "number": pr["number"],
                "head": pr["head"],
                "labels": list(pr["labels"]),
                "state": pr.get("state", "OPEN"),
                "draft": pr.get("draft", False) is True,
                "change_author": comment_author(pr.get("body") or ""),
                "comments": comments,
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
            issuers: list[tuple[str, "str | None"]] = []
            unavailable: list[dict[str, Any]] = []
            if label == LOCALLY_VALIDATED_LABEL and "comments" in pr:
                shas = local_receipt_heads(comments, pr.get("repo", "rrnewton/hermit"))
            elif "comments" in pr:
                unavailable = unavailable_approval_evidence(comments, lane)
                shas = lane_shas(
                    comments,
                    lane,
                    malformed,
                    pr["number"],
                    malformed_prs,
                    refusals=refusals,
                    unanswered=unanswered,
                    issuers=issuers,
                    current_head=pr["head"],
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
                    "review_authority_unavailable": unavailable,
                    "head": pr["head"],
                    "bound_to": shas[-1] if shas else None,
                    "binding_issuer": issuers[-1][1] if issuers else None,
                    "refused_at": refusals[-1] if refusals else None,
                    "state": classify_approval(
                        pr["head"], shas, refused=bool(refusals),
                        refused_at=refusals[-1] if refusals else None,
                        unanswered=bool(unanswered),
                    ),
                    # The ISSUER of every unanswered refusal at this head, in
                    # order, with None preserved for one whose disclosure tag
                    # cannot be read. This is descriptive provenance, not a
                    # retirement authority: an exact authorized RETIRES record
                    # may resolve the objection regardless of whether this issuer
                    # is still present.
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
            issuers: list[tuple[str, "str | None"]] = []
            unavailable = unavailable_approval_evidence(comments, lane)
            if "comments" in pr:
                shas = lane_shas(
                    comments, lane, malformed, pr["number"], malformed_prs,
                    refusals=refusals,
                    unanswered=unanswered,
                    issuers=issuers,
                    current_head=pr["head"],
                )
            else:
                shas = pr.get("lane_shas", {}).get(lane, [])
                refusals = list(pr.get("lane_refused", {}).get(lane, []))
            if not shas and not refusals and not unavailable:
                continue
            rows.append(
                {
                    "number": pr["number"],
                    "label": None,
                    "source": "comment",
                    "lane": lane,
                    "review_authority_unavailable": unavailable,
                    "head": pr["head"],
                    "bound_to": shas[-1] if shas else None,
                    "binding_issuer": issuers[-1][1] if issuers else None,
                    "refused_at": refusals[-1] if refusals else None,
                    "state": classify_approval(
                        pr["head"], shas, refused=bool(refusals),
                        refused_at=refusals[-1] if refusals else None,
                        unanswered=bool(unanswered),
                    ),
                    # The ISSUER of every unanswered refusal at this head, in
                    # order, with None preserved for one whose disclosure tag
                    # cannot be read. This is descriptive provenance, not a
                    # retirement authority: an exact authorized RETIRES record
                    # may resolve the objection regardless of whether this issuer
                    # is still present.
                    "refusal_issuers": [w for _s, w in unanswered if _s == pr["head"]],
                    "runs_at_head": pr.get("runs_at_head", 1),
                    "mergeable": pr.get("mergeable"),
                }
            )
    return rows, selected, actions_enabled, fixture_evaluated


def stale_review_label_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Review cache labels proven to name a head other than the current one."""
    return [
        row
        for row in rows
        if row.get("source") == "label"
        and row.get("label") in {"passed-review-claude", "passed-review-codex"}
        and row.get("state") == SUPERSEDED
    ]


def _live_pr_snapshot(
    repo: str, number: int
) -> tuple[list[dict[str, Any]], dict[str, Any] | None]:
    malformed: list[tuple[str, str]] = []
    malformed_prs: dict[tuple[str, str], int] = {}
    return collect_live(repo, malformed, malformed_prs, number)


def _edit_review_label(repo: str, number: int, label: str, add: bool) -> None:
    operation = "--add-label" if add else "--remove-label"
    gh_mutate(
        ["pr", "edit", str(number), "--repo", repo, operation, label]
    )


def reconciliation_marker(
    repo: str, number: int, head: str, labels: Sequence[str]
) -> str:
    label_text = ",".join(sorted(labels))
    return (
        f"<!-- review-label-reconciled repo={repo} pr={number} head={head} "
        f"labels={label_text} -->"
    )


def _post_reconciliation_notice(
    repo: str,
    action: dict[str, Any],
    who: str,
    team: str,
) -> None:
    owners = ", ".join(f"agent({owner})" for owner in action["owners"])
    if not owners:
        owners = "unavailable from disclosure tags"
    bindings = ", ".join(head[:12] for head in action["bound_to"])
    labels = ", ".join(action["labels"])
    body = (
        "Review labels no longer bind after the pull request head moved.\n\n"
        f"Current head: `{action['head']}`\n"
        f"Earlier approved head(s): `{bindings}`\n"
        f"Stale label(s): `{labels}`\n"
        f"Affected owners: {owners}\n\n"
        "The exact-head review comments remain as history. The stale labels are "
        "being reconciled; a lane needs a new approval at the current head unless "
        "one arrives while reconciliation is running.\n\n"
        + action["marker"]
    )
    gh_mutate(
        [
            "--who",
            who,
            "--team",
            team,
            "--role",
            "relay",
            "pr",
            "comment",
            str(action["pr"]),
            "--repo",
            repo,
            "--body",
            body,
        ]
    )


def reconcile_stale_review_labels(
    repo: str,
    initial_rows: list[dict[str, Any]],
    *,
    snapshot: Callable[
        [str, int], tuple[list[dict[str, Any]], dict[str, Any] | None]
    ] = _live_pr_snapshot,
    edit_label: Callable[[str, int, str, bool], None] = _edit_review_label,
    notify: Callable[[str, dict[str, Any]], None] | None = None,
) -> list[dict[str, Any]]:
    """Remove stale review cache labels after a fresh exact-head check.

    The first fleet scan only selects candidates. Each candidate is read again
    immediately before mutation. After removal it is read once more; if an
    exact-head approval arrived in the interval, its label is restored and the
    final state is verified. A head that never moved has no ``SUPERSEDED`` row,
    so it causes neither a label write nor an owner notification.

    Returned rows are the notification payload for the health tick. The change
    author and the reviewer whose old binding was invalidated are both named
    when their disclosure tags are available.
    """
    candidates = sorted(
        {int(row["number"]) for row in stale_review_label_rows(initial_rows)}
    )
    notifications: list[dict[str, Any]] = []
    for number in candidates:
        before_rows, before = snapshot(repo, number)
        if before is None or str(before.get("state") or "").lower() != "open":
            continue
        stale = stale_review_label_rows(before_rows)
        if not stale:
            continue

        removed_labels = [str(row["label"]) for row in stale]
        owners = {
            str(owner)
            for owner in [
                before.get("change_author"),
                *(row.get("binding_issuer") for row in stale),
            ]
            if owner
        }
        action = {
            "pr": number,
            "head": str(before.get("head") or ""),
            "labels": sorted(set(removed_labels)),
            "owners": sorted(owners),
            "bound_to": sorted(
                {
                    str(row["bound_to"])
                    for row in stale
                    if row.get("bound_to")
                }
            ),
        }
        action["marker"] = reconciliation_marker(
            repo, number, str(action["head"]), action["labels"]
        )
        comments = before.get("comments") or []
        already_notified = any(
            action["marker"] in str(comment.get("body") or "")
            for comment in comments
            if isinstance(comment, dict)
        )
        if not already_notified:
            if notify is None:
                raise GitHubUnavailable(
                    f"PR #{number} stale review labels cannot be removed without "
                    "notifying their owners"
                )
            notify(repo, action)

        for label in removed_labels:
            edit_label(repo, number, label, False)

        after_rows, after = snapshot(repo, number)
        if after is None:
            raise GitHubUnavailable(
                f"PR #{number} disappeared while stale review labels were reconciled"
            )
        after_labels = set(after.get("labels") or [])
        restored: list[str] = []
        for row in stale:
            label = str(row["label"])
            lane = str(row["lane"])
            current_binding = any(
                candidate.get("lane") == lane
                and candidate.get("state") == BOUND_CURRENT
                for candidate in after_rows
            )
            if current_binding and label not in after_labels:
                edit_label(repo, number, label, True)
                restored.append(label)
            elif not current_binding and label in after_labels:
                raise GitHubUnavailable(
                    f"PR #{number} still carries stale {label} after removal"
                )

        if restored:
            _final_rows, final = snapshot(repo, number)
            final_labels = set((final or {}).get("labels") or [])
            missing = sorted(set(restored) - final_labels)
            if missing:
                raise GitHubUnavailable(
                    f"PR #{number} exact-head approval label restore did not hold: "
                    + ",".join(missing)
                )

        removed = sorted(set(removed_labels) - set(restored))
        action["head"] = str(after.get("head") or before.get("head") or "")
        action["removed"] = removed
        action["restored"] = sorted(restored)
        action["notified"] = not already_notified
        notifications.append(action)
    return notifications


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
    ap = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        epilog=(
            "examples:\n"
            "  approval_binding.py --repo rrnewton/hermit --json\n"
            "  approval_binding.py --repo rrnewton/hermit --pr 123 --gate\n"
            "  approval_binding.py --repo rrnewton/hermit --gate "
            "--gate-on superseded --reconcile-review-labels\n\n"
            "Detailed rationale and state definitions are in this file's module docstring."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument(
        "--repo",
        default="rrnewton/hermit",
        help="GitHub owner/name to inspect (default: rrnewton/hermit)",
    )
    ap.add_argument(
        "--pr",
        type=int,
        help="Restrict live verification to one PR (used by the final landing boundary)",
    )
    ap.add_argument(
        "--from-json",
        dest="fixture",
        help="read an offline fixture instead of GitHub",
    )
    ap.add_argument(
        "--gate",
        action="store_true",
        help="exit nonzero when the selected binding check fails",
    )
    ap.add_argument(
        "--gate-on",
        default="all",
        choices=["all", "superseded", "unverifiable"],
        help=(
            "states that make --gate exit nonzero: all non-current labels "
            "(default), only labels bound to an older head, or only labels "
            "whose exact-head authority cannot be established"
        ),
    )
    ap.add_argument(
        "--json",
        dest="as_json",
        action="store_true",
        help="emit the complete report as JSON",
    )
    ap.add_argument(
        "--reconcile-review-labels",
        action="store_true",
        help=(
            "remove passed-review-* labels whose newest approval names a different "
            "head, after a fresh per-PR check; exact-head comments are never changed"
        ),
    )
    ap.add_argument(
        "--notification-team",
        default="hermit2",
        help="team disclosure used for the moved-head notice (default: hermit2)",
    )
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
        help=(
            "window start for --landing-observability, any git --since expression "
            "(default: 1 day ago)"
        ),
    )
    ap.add_argument(
        "--checkout",
        metavar="DIR",
        default=".",
        help=(
            "DIRECTORY of the checkout whose commits form the window. A path, not "
            "a commit. Must be a checkout of --repo; mixing them is refused "
            "(default: current directory)."
        ),
    )
    a = ap.parse_args()
    if a.fixture and a.reconcile_review_labels:
        ap.error("--reconcile-review-labels requires live GitHub state")
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
            if a.reconcile_review_labels:
                print("reconciliation_owners=unavailable")
                print(f"reconciliation_summary={summary}")
        elif a.as_json:
            print(json.dumps({"state": "unavailable", "summary": summary}))
        print(f"NO RESULT: {summary}", file=sys.stderr)
        return 75 if a.gate else 1

    reconciled: list[dict[str, Any]] = []
    if a.reconcile_review_labels and stale_review_label_rows(rows):
        notification_agent = os.environ.get("DG_AGENT_NAME", "").strip()
        if not notification_agent:
            summary = (
                "stale review-label reconciliation refused: DG_AGENT_NAME is "
                "unset, so an owner notice could not be attributed"
            )
            if a.gate and not a.as_json:
                print("state=unavailable")
                print(f"summary={summary}")
                print("reconciliation_owners=unavailable")
                print(f"reconciliation_summary={summary}")
            elif a.as_json:
                print(json.dumps({"state": "unavailable", "summary": summary}))
            print(f"NO RESULT: {summary}", file=sys.stderr)
            return 75 if a.gate else 1
        try:
            reconciled = reconcile_stale_review_labels(
                a.repo,
                rows,
                notify=lambda repo, action: _post_reconciliation_notice(
                    repo,
                    action,
                    notification_agent,
                    a.notification_team,
                ),
            )
        except GitHubUnavailable as error:
            summary = (
                "stale review-label reconciliation could not be determined: "
                + " ".join(str(error).split()).replace("{", "(").replace("}", ")")
            )
            if a.gate and not a.as_json:
                print("state=unavailable")
                print(f"summary={summary}")
                print("reconciliation_owners=unavailable")
                print(f"reconciliation_summary={summary}")
            elif a.as_json:
                print(json.dumps({"state": "unavailable", "summary": summary}))
            print(f"NO RESULT: {summary}", file=sys.stderr)
            return 75 if a.gate else 1
        removed_keys = {
            (action["pr"], label)
            for action in reconciled
            for label in action["removed"]
        }
        rows = [
            row
            for row in rows
            if (row.get("number"), row.get("label")) not in removed_keys
        ]
    # Exact-head review records are authority; labels are caches. A labelled lane
    # and an unlabelled lane are each represented once by the row builders above.
    approval_rows = [
        row for row in rows if row.get("lane") in LANES
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
    # Critical review still requires both lanes, but not their cache labels.
    authoritative_lanes = {
        str(row["lane"])
        for row in rows
        if row["state"] == BOUND_CURRENT
        and row["lane"] in LANES
    }
    critical_lanes_missing = (
        sorted(set(LANES) - authoritative_lanes) if critical_required else []
    )
    label_rows = [r for r in rows if r.get("source") != "comment"]
    comment_rows = [r for r in rows if r.get("source") == "comment"]
    bad = [r for r in rows if r["state"] != BOUND_CURRENT]
    authoritative = len(rows) - len(bad)
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
    permission_unavailable_details: list[dict[str, Any]] = []
    seen_permission_details: set[tuple[object, ...]] = set()
    for row in rows:
        for detail in row.get("review_authority_unavailable") or []:
            key = (
                row.get("number"),
                row.get("lane"),
                detail.get("id"),
                detail.get("head"),
            )
            if key in seen_permission_details:
                continue
            seen_permission_details.add(key)
            permission_unavailable_details.append(
                {"pr": row.get("number"), "lane": row.get("lane"), **detail}
            )

    if a.as_json:
        print(json.dumps({"approvals": len(approval_rows),
                          "actions_enabled": actions_enabled,
                          "binding_labels": len(label_rows),
                          # Canonical bindings found without a matching cache label.
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
                          "review_permission_unavailable": len(
                              permission_unavailable_details
                          ),
                          "review_permission_unavailable_details": permission_unavailable_details,
                          "authority_absent": len(bad), "not_holding": len(bad),
                          "approved_and_unevaluated": len(approved_and_unevaluated),
                          "binding_labels_and_unevaluated": len(combined),
                          "malformed_source_lines": len(malformed),
                          "malformed_affected_labels": len(malformed_affected_labels),
                          "malformed_affected_prs": sorted(malformed_prs),
                          "reconciled_review_labels": sum(
                              len(action["removed"]) for action in reconciled
                          ),
                          "reconciliation_notifications": reconciled,
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
        reconciled_count = sum(len(action["removed"]) for action in reconciled)
        affected_owners = sorted(
            {
                owner
                for action in reconciled
                for owner in action["owners"]
            }
        )
        print(f"reconciled_review_labels={reconciled_count}")
        print(
            "reconciliation_owners="
            + (",".join(affected_owners) if affected_owners else "none")
        )
        if reconciled:
            print(
                "reconciliation_summary="
                + "; ".join(
                    f"PR #{action['pr']} removed {','.join(action['removed'])} "
                    f"after head moved to {action['head'][:12]}; notify "
                    + (
                        ",".join(action["owners"])
                        if action["owners"]
                        else "owner unavailable"
                    )
                    for action in reconciled
                )
            )
        else:
            print("reconciliation_summary=no stale review label was removed")
        print(f"actions_enabled={str(actions_enabled).lower()}")
        if not actions_enabled:
            print(
                "runs_at_head_zero=expected while GitHub Actions are disabled; "
                "zero workflow runs are not classified as unevaluated"
            )
        print(f"inspected {len(rows)} review/local-validation artifact(s); bound to current head: "
              f"{authoritative}; authority absent: {len(bad)}; "
              f"cached review/local-validation artifact(s) AND unevaluated: {len(combined)}")
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
        for detail in permission_unavailable_details:
            print(
                "  REVIEW-PERMISSION-UNAVAILABLE  "
                f"PR {detail['pr']} lane {detail['lane']} comment "
                f"{detail.get('id') or 'unknown'} actor "
                f"{detail.get('github_actor') or 'unknown'} permission "
                f"{detail.get('permission') or 'unavailable'}"
            )
        if approval_set_missing:
            print(f"  REQUIRED-APPROVAL-SET-MISSING  PR {a.pr} has zero review artifacts")
            # The distinction this tool could not previously draw. Both states
            # print zero approvals; only one of them means nobody looked.
            at_head = [r for r in comment_rows if r["state"] == BOUND_CURRENT]
            if at_head:
                print(
                    f"    NOT UNREVIEWED: {len(at_head)} canonical binding(s) at the "
                    f"current head in comments ("
                    + ", ".join(sorted(r["lane"] for r in at_head))
                    + "), with no passed-review-* label. The review is authoritative; "
                    "the absent label is only a cache miss."
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
            print("  every review/local-validation artifact is bound to the current head")
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
            for r in rows
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
                f"PR #{a.pr} has zero review artifacts; absence of review "
                "evidence cannot satisfy the final landing boundary"
            )
        elif permission_unavailable_details:
            state = "unbound"
            summary = (
                f"{len(permission_unavailable_details)} canonical review artifact(s) "
                "cannot establish authority because current GitHub actor permission "
                "is unavailable or insufficient; remedy: obtain a canonical exact-head "
                "review from a GitHub actor with current repository review permission"
            )
        else:
            state = "unbound" if gating else ("reconciled" if reconciled else "ok")
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
            if reconciled:
                removed = sum(len(action["removed"]) for action in reconciled)
                owners = sorted(
                    {
                        owner
                        for action in reconciled
                        for owner in action["owners"]
                    }
                )
                reconciliation_text = (
                    f"removed {removed} stale passed-review label(s) after a head "
                    f"move; affected owners={','.join(owners) if owners else 'unavailable'}"
                )
            else:
                reconciliation_text = ""
            if gating:
                summary = (
                    f"{len(gating)}/{len(rows)} review/local-validation artifacts fail the "
                    f"{a.gate_on} binding check; affected={affected}; they do not bind "
                    f"the current head; remedy: {'; '.join(remedies)}"
                )
                if reconciliation_text:
                    summary = reconciliation_text + "; " + summary
            elif reconciliation_text:
                summary = reconciliation_text
            else:
                summary = "every review/local-validation artifact binds to the current head"
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

        # Preserve the issuer split as provenance, not as a permission boundary.
        # Issuer names remain provenance only. Resolution uses exact head, lane,
        # chronology, canonical markers, and current GitHub permission.
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
                "refusal_issuer_remedy=after verifying the objection is resolved, a "
                "currently authorized GitHub actor may post a canonical withdrawal "
                "in the refusal's lane at the current head, or one exact "
                "withdrawal with RETIRES #<the exact refusing comment id>"
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
            f"GATE FAIL: PR #{a.pr} has zero review artifacts. Zero approvals "
            "is absence of authority, not zero review problems.",
            file=sys.stderr,
        )
        return 1
    if a.gate and (gating or reconciled):
        if reconciled:
            print(
                "GATE NOTICE: stale passed-review label(s) were removed after a "
                "head move; the affected owners are named in reconciliation_owners.",
                file=sys.stderr,
            )
        if not gating:
            return 1
        print(
            f"GATE FAIL ({a.gate_on}): {len(gating)} of {len(rows)} review/local-validation artifact(s) do not bind "
            "to the current head. Stale review labels are reconciled when requested; "
            "other labels do not satisfy the review or local-validation head-binding gate.",
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
