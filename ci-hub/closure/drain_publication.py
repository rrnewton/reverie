#!/usr/bin/env python3
"""A drain task must not close with its verdict unpublished.

WHAT WENT WRONG, MEASURED
=========================
`agent(hermit-004)` read hermit#2304, reached a HOLD verdict, wrote the whole
analysis into its TaskGraph task, and closed the task WITHOUT POSTING. The pull
request's newest comment was from the previous day; the next one arrived at
11:19 the following morning from a DIFFERENT agent who happened to re-open the
same PR and notice. A complete, correct review existed and the artifact showed
nothing.

That is the third of four stranding shapes recorded on 2026-08-24/25, and they
end identically: work done, record says finished, the pull request shows
nothing. THE COMMON CAUSE IS THAT CLOSING A TASK AND PUBLISHING A VERDICT ARE
SEPARATE ACTS AND ONLY ONE OF THEM WAS EVER CHECKED.

THE PRECEDENT THIS COPIES
=========================
`verified_close.py` already refuses to close an unlanded task that is not
tagged `implemented`, and refuses a bare PR number with no repository, because
a closure whose reference cannot be resolved proves nothing. This module
applies the same rule one artifact over: when a task's deliverable is a REVIEW
rather than a LANDING, the durable reference is the comment permalink, not the
SHA.

FOUR NARROWINGS, EACH ONE PAID FOR BY A MEASUREMENT
===================================================
The population was enumerated BEFORE the rule was enforced (2026-08-25, 1905
tasks, 70 closed drain tasks). Each narrowing below removed a class of finding
that would have been a red with no correct remedy -- the shape that gets a gate
switched off on its first day, and the shape (an unsatisfiable precondition
with no written exemption) that this project has already had to remove.

1. ONE PULL REQUEST IN THE SUBJECT. A drain ROLLUP (`drain-open-prs-to-zero-
   overnight`) has no single artifact to post on. 144 of 221 drain-tagged tasks
   are rollups or name no PR at all; all are out of scope. Only the task's
   TITLE and ID are read for the subject -- a rollup's NOTES enumerate dozens
   of PRs and scanning them would drag every rollup back in.

2. THE PULL REQUEST IS NOT MERGED. 10 of the 70 closed drain tasks ended by
   MERGING their PR, and 7 of those recorded no comment -- correctly, because
   a landing publishes itself. Demanding a review comment on merged work is the
   wrong remedy for a non-problem.

3. A WRITTEN EXEMPTION EXISTS. A duplicate task, or one that correctly decided
   nothing was owed, publishes nothing on purpose (`drain_hermit_pr_2399_2`:
   "I did not fetch, read, comment on, rebase, validate, or mutate PR #2399").
   A rule with no exemption for that randomises behaviour -- conservative
   agents strand, permissive ones close anyway. So `NO-PUBLICATION: <reason>`
   in the task record satisfies the rule, and every use of it is NAMED in the
   output so the escape hatch is audited rather than silent.

4. A CUTOVER, NOT A RETROACTIVE RED. Of the 60 closed-and-unmerged in-scope
   tasks, 25 carry no comment link -- but GitHub says 16 of those 25 DID post,
   and only failed to record the permalink. A retroactive gate would emit 25
   findings of which 16 (64%) tell an agent to go re-post a verdict that is
   already posted. That is the same ordering failure that nearly made deleting
   contributors' names look correct. So the gate scores only tasks closed at or
   after `--since`, and ALWAYS prints `pre_cutover_no_link_recorded` so the
   backlog stays on screen. That field is a cheap LINK-PRESENCE count and says
   so: `suppressed_pre_cutover` can only count tasks that were fully evaluated,
   and the gated path asks GitHub only about tasks closed since the cutover, so
   it reads 0 in normal operation. A cutover that silently zeroes the number
   which justified it is the same defect as a gate going quiet.

   The 9 genuinely-silent pre-cutover tasks are real stranded work. They are
   listed by `--census --pre-cutover` for recovery; they are not a gate red.

⚠️ THIS IS A HEURISTIC AND IT WILL DRIFT. THE DURABLE FIX IS A STRUCTURED FIELD
==============================================================================
Everything above rests on PATTERN-MATCHING PROSE. The subject PR is recovered
by regexing a human-written title; the publication is recovered by regexing a
URL out of free-text notes. That is the correct thing to build FIRST, because
it works on the corpus as it exists today and needed no migration -- but do not
trust it past its design. Every one of these degrades silently:

  * A title phrased a new way ("drain the SaBRe shutdown PR") matches nothing,
    the task leaves the population, and the gate goes quieter, not louder.
  * A title that names TWO pull requests is dropped entirely by the
    exactly-one rule. Measured 2026-08-25: 0 such titles today, so this is a
    LATENT gap, which is the worst kind -- nothing will announce its arrival.
  * `NO-PUBLICATION:` is a string in prose. Nothing stops it appearing inside a
    quoted example, and nothing records WHO wrote it.

The durable version is a first-class field on the task -- `published_comment`
alongside the PR reference -- written by the tool that posts the comment, so
the evidence is produced by the act instead of reconstructed from a description
of the act. Filed as its own task rather than left as a comment here.

THE REUSABLE PART IS THE THREE FALSE-POSITIVE CLASSES
=====================================================
Any future checker over this corpus will meet the same three, and each one was
found by enumerating rather than by reasoning:

  1. THE ROLLUP -- a task naming many artifacts has no single one to satisfy a
     per-artifact rule.
  2. THE ALREADY-SUPERSEDED OUTCOME -- the merged PR here; the obligation was
     discharged by a stronger act than the one being checked for.
  3. THE FOREIGN IDENTIFIER -- a bare `#N`, or a PR number arriving from
     another agent's coordination notes, read as this task's subject. This is
     the bare-integer collision this project has now hit twice in different
     tools, and it is why the subject is taken ONLY from the title and id (never
     the notes), and why every pattern here requires a repository name adjacent
     to the number. A bare `#2304` matches nothing in this module on purpose.

WHY THIS IS NOT A TASKGRAPH CENSUS (the tick-hub admission test)
================================================================
tick-hub permanently excludes anything ORC can observe directly, and TaskGraph
census is named in that exclusion. This gate is not one. The fact it measures
is WHETHER THE PULL REQUEST CARRIES THE VERDICT -- GitHub state, which ORC
cannot see. The TaskGraph is read only to know which PRs to ask about and which
permalink was claimed. Consistent with that, GitHub being unreachable is NO
RESULT (exit 75), never a pass or a publication gap: the recorded permalink
must appear in the PR's actual comment set, so pasting a plausible URL does not
satisfy the rule.

EXIT CODES
  0  every in-scope drain task published its verdict (or is exempt, by name)
  1  at least one in-scope drain task closed with its verdict unpublished
 75  NO RESULT -- the TaskGraph or GitHub could not be read. Never 0 or 1.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime as dt
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

CI_HUB_ROOT = Path(__file__).resolve().parents[1]
# One resolver for the TaskGraph, shared with every other ci-hub consumer.
sys.path.insert(0, str(CI_HUB_ROOT / "lib"))
import taskgraph_db  # noqa: E402

DEFAULT_OWNER = "rrnewton"
# Repositories a drain task can be about. Same spelling as
# ci-hub/health/drain_reconcile.py; a new repo is added in both.
DEFAULT_REPOS = ("hermit", "reverie", "agent-utils", "dev-hermit", "liteinst2")

# A task is a drain task if it carries one of these tags or its id starts with
# `drain`. Both, because the two conventions are live at once: measured on
# 2026-08-25, 70 tasks carry both, 14 only the id prefix, 137 only a tag.
DRAIN_TAGS = frozenset({"drain", "pr-drain"})
DRAIN_ID_PREFIX = "drain"

# Statuses that mean the task has stopped tracking work. RESOLVED is included:
# it is terminal, and a verdict stranded behind it is stranded just the same.
TERMINAL = frozenset({"CLOSED", "RESOLVED"})

# The written exemption. Deliberately a marker a human wrote, not a tag: tags
# are added in bulk, and the thing being recorded here is a DECISION.
EXEMPTION = re.compile(r"^\s*NO-PUBLICATION:\s*(\S.*)$", re.MULTILINE)

# ⚠️ THE DISPATCH TEXT QUOTES THE RULE, AND THE QUOTE WAS OPENING THE HATCH.
# ORC's drain dispatch tells the agent what is required, in these words:
#
#   "it must not close without a link to the comment published on hermit#N, or a
#    written NO-PUBLICATION: <reason>."
#
# That sentence lands in the task DESCRIPTION, `task_text` concatenates
# description and notes, and `EXEMPTION.search` returns the FIRST match -- so the
# instruction telling an agent how to claim an exemption WAS ITSELF READ AS ONE.
#
# Measured on the live corpus 2026-08-25, 236 drain-tagged tasks: 2 hit.
#   drain_hermit_pr_2448 -- CLOSED, no comment link, admitted on the placeholder.
#     A genuine exemption did exist in its notes, so the verdict was right by
#     luck, but the reason shown to a reviewer was boilerplate rather than the
#     real justification sitting three lines below it.
#   drain_hermit_pr_2255 -- IN_PROGRESS, so not yet gated, but ARMED: no genuine
#     exemption anywhere on the task. It would have closed exempt with nobody
#     having decided anything.
#
# This is the hazard the structured record closes by construction -- a record
# must be a whole note, so quoting one cannot produce one. This guard is for the
# FALLBACK, which stays until record coverage reaches 1 and therefore stays
# exploitable until then. It only ever rejects; no exemption that named a real
# reason stops being honoured.
PLACEHOLDER_REASON = re.compile(r"^<[^>]*>")

# ⚠️ THE REGEX ABOVE AND EVERY PATTERN IN THIS FILE ARE NOW THE FALLBACK, NOT THE
# RULE. `publication_record` holds the structured records that the posting path
# writes AT POST TIME, and those are preferred wherever they exist.
#
# The fallback is kept deliberately. Coverage of the structured record starts at
# zero -- nothing that has already been posted can grow one retroactively, and
# anything written retroactively would be inference, which is the regex again
# with better manners. Deleting the prose path today would turn every historical
# task into an unexplained failure. It comes out when `--census` reports coverage
# at or near 1, and not before.
#
# Every verdict therefore carries WHICH SOURCE ESTABLISHED IT, so the changeover
# is a measurement rather than a belief.
# Imported by path rather than as a package member: the test suite loads this
# file directly with importlib, and `ci-hub/bin/gh` loads it from a sibling
# directory, so a relative import would work in neither.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import publication_record  # noqa: E402

SOURCE_RECORD = "record"
SOURCE_REGEX = "regex"
# tick-hub reserves EX_TEMPFAIL for a gate that ran but could not determine its
# condition. Exit 2 is an ordinary failure to `when: failure` and would announce
# a publication gap even though no verdict was measured.
NO_RESULT_EXIT = 75

_REPO_ALT = "|".join(re.escape(r) for r in DEFAULT_REPOS)

# --- subject: which ONE pull request is this task's job? ---------------------
# Every spelling below appears in live titles and ids:
#   "Drain Hermit PR 2304 end to end"   "Drain Hermit draft PR #2415 end to end"
#   "Drain SALVAGE draft https://github.com/rrnewton/hermit/pull/2407 end to end"
#   local id `drain_hermit_pr_2370`
_SUBJECT_URL = re.compile(
    r"github\.com/[\w.-]+/(" + _REPO_ALT + r")/pull/(\d+)", re.IGNORECASE
)
_SUBJECT_SHORT = re.compile(r"\b(" + _REPO_ALT + r")#(\d+)\b", re.IGNORECASE)
_SUBJECT_PROSE = re.compile(
    r"\b(" + _REPO_ALT + r")\b[\w\s]{0,24}?\bpr\s*#?\s*(\d+)\b", re.IGNORECASE
)
_SUBJECT_ID = re.compile(
    r"^drain[-_]?(" + _REPO_ALT + r")[-_](?:pr[-_])?(\d+)", re.IGNORECASE
)

# --- evidence: a permalink to a comment ON THAT pull request -----------------
# GitHub's three durable comment anchors. `#issuecomment-` is what a verdict is
# normally posted as; the other two are review anchors, which are just as
# published and must not be rejected as "not a comment".
_COMMENT_LINK = re.compile(
    r"https?://github\.com/([\w.-]+)/([\w.-]+)/pull/(\d+)"
    r"#(?:issuecomment-\d+|pullrequestreview-\d+|discussion_r\d+)",
    re.IGNORECASE,
)


class Unavailable(Exception):
    """A required authority could not be read. Never downgraded to 'no gaps'."""


@dataclass
class DrainTask:
    task_id: str
    title: str
    status: str
    closed_at: str
    repo: str
    number: int
    links: list[str] = field(default_factory=list)
    exemption: str = ""
    # Which mechanism established the two facts above: SOURCE_RECORD when the
    # posting path wrote a structured record, SOURCE_REGEX when they were
    # reconstructed from prose. Reported per task by --census; this is the number
    # that decides when the prose path can be deleted.
    link_source: str = SOURCE_REGEX
    exemption_source: str = SOURCE_REGEX
    exemption_actor: str = ""
    # Filled from GitHub. None until asked -- never defaulted to a passing value.
    merged: bool | None = None
    confirmed: list[str] = field(default_factory=list)
    forged: list[str] = field(default_factory=list)

    @property
    def pr(self) -> str:
        return f"{self.repo}#{self.number}"

    @property
    def terminal(self) -> bool:
        return self.status in TERMINAL


# ---------------------------------------------------------------------------
# reading the TaskGraph


def _tg_sql(query: str, db: Path | None, timeout: int = 60) -> list[dict]:
    """One read-only `tg sql` returning one json_object per line."""
    try:
        resolution = taskgraph_db.resolve(db)
        env = taskgraph_db.child_env(resolution)
    except taskgraph_db.TaskGraphUnavailable as exc:
        raise Unavailable(str(exc)) from exc
    try:
        proc = subprocess.run(
            ["tg", "sql", query], capture_output=True, text=True, timeout=timeout, env=env
        )
    except subprocess.TimeoutExpired as exc:
        raise Unavailable(f"tg sql timed out after {timeout}s") from exc
    except OSError as exc:
        raise Unavailable(f"could not execute tg: {exc}") from exc
    if proc.returncode != 0:
        raise Unavailable(f"tg sql exit {proc.returncode}: {proc.stderr.strip()[:200]}")
    try:
        return taskgraph_db.parse_json_rows(proc.stdout)
    except taskgraph_db.TaskGraphUnavailable as exc:
        raise Unavailable(str(exc)) from exc


def load_tasks(db: Path | None = None) -> list[dict]:
    """Every task with its text and its notes attached."""
    tasks = _tg_sql(
        "SELECT json_object('id',local_id,'title',title,"
        "'desc',coalesce(description,''),'status',status,'tags',tags,"
        "'modified',coalesce(local_modified_at,created_at)) FROM tasks",
        db,
    )
    if not tasks:
        # An empty population from a bound, validated database is a real
        # measured zero -- and it is also exactly what a misbinding looks like.
        # This module's whole subject is checks that pass because they see
        # nothing, so it refuses rather than reporting a clean fleet.
        raise Unavailable(
            "the bound TaskGraph returned zero tasks; refusing to report "
            "'every drain task published' from an empty population"
        )
    notes = _tg_sql(
        "SELECT json_object('task_id',task_id,'content',content,"
        "'created',created_at) FROM task_notes",
        db,
    )
    by_id: dict[str, dict] = {}
    for task in tasks:
        task["notes"] = []
        by_id[task["id"]] = task
    for note in notes:
        row = by_id.get(note.get("task_id"))
        if row is not None:
            row["notes"].append(note)
    return tasks


# ---------------------------------------------------------------------------
# population and evidence


def is_drain_task(task: dict) -> bool:
    try:
        tags = json.loads(task.get("tags") or "[]")
    except (json.JSONDecodeError, TypeError) as exc:
        raise Unavailable(
            f"task {task.get('id', '<unknown>')} has malformed tags"
        ) from exc
    if not isinstance(tags, list):
        raise Unavailable(
            f"task {task.get('id', '<unknown>')} tags are not a list"
        )
    if not all(isinstance(tag, str) for tag in tags):
        raise Unavailable(
            f"task {task.get('id', '<unknown>')} tags are not all strings"
        )
    if any(str(t).lower() in DRAIN_TAGS for t in tags):
        return True
    return str(task.get("id", "")).lower().startswith(DRAIN_ID_PREFIX)


def subject_pr(task: dict) -> tuple[str, int] | None:
    """The ONE pull request this task's subject names, or None.

    None deliberately covers two different cases -- a rollup naming many PRs,
    and a task naming none -- because this rule has nothing to say about
    either. `--census` counts them so the exclusion stays visible.
    """
    subject = f"{task.get('title', '')}\n{task.get('id', '')}"
    found: set[tuple[str, int]] = set()
    for pattern in (_SUBJECT_URL, _SUBJECT_SHORT, _SUBJECT_PROSE, _SUBJECT_ID):
        for match in pattern.finditer(subject):
            found.add((match.group(1).lower(), int(match.group(2))))
    return next(iter(found)) if len(found) == 1 else None


def task_text(task: dict) -> str:
    return task.get("desc", "") + "\n" + "\n".join(
        n.get("content", "") for n in task.get("notes", [])
    )


def claimed_links(text: str, repo: str, number: int) -> list[str]:
    """Comment permalinks recorded on the task FOR THIS pull request.

    Repository and number must match the subject. A link to some other PR's
    comment is evidence of some other publication; accepting it would make the
    rule satisfiable by pasting any URL, which is the rule-that-only-passes
    defect this project has removed nine times.
    """
    return [
        m.group(0)
        for m in _COMMENT_LINK.finditer(text)
        if m.group(2).lower() == repo and int(m.group(3)) == number
    ]


def closed_at(task: dict) -> str:
    """Best available closure timestamp.

    `tasks` carries no closed_at column, so this is the latest of the task's
    note timestamps and its last local modification. Named honestly because the
    cutover rests on it: it can only be LATER than the true close, so a task can
    drift INTO scope, never silently out of it.
    """
    stamps = [n.get("created", "") or "" for n in task.get("notes", [])]
    stamps.append(task.get("modified", "") or "")
    return max(stamps) if stamps else ""


def collect(tasks: list[dict]) -> tuple[list[DrainTask], int]:
    """(drain tasks naming exactly one PR, drain tasks excluded for not doing so)."""
    scoped: list[DrainTask] = []
    excluded = 0
    for task in tasks:
        if not is_drain_task(task):
            continue
        bodies = [n.get("content", "") or "" for n in task.get("notes", [])]
        bodies.append(task.get("desc", "") or "")
        records, exemptions = publication_record.parse_records(bodies)

        # THE SUBJECT COMES FROM THE RECORD WHEN THERE IS ONE. A record was
        # written by the poster from the permalink GitHub returned, so it cannot
        # name a pull request other than the one actually commented on. That
        # closes THE FOREIGN IDENTIFIER class by construction: a bare `#N`, or a
        # number arriving from another agent's coordination notes pasted into
        # this task, can no longer be read as this task's subject.
        #
        # Records disagreeing about the subject means a rollup, which this rule
        # still has nothing to say about -- so it falls through to the same
        # exclusion as before rather than picking one arbitrarily.
        subjects = {(r.repo.split("/")[-1], r.pr) for r in records}
        if len(subjects) == 1:
            repo, number = next(iter(subjects))
            source = SOURCE_RECORD
        else:
            subject = subject_pr(task)
            if subject is None:
                excluded += 1
                continue
            repo, number = subject
            source = SOURCE_REGEX

        text = task_text(task)
        own = [r for r in records if r.repo.split("/")[-1] == repo and r.pr == number]
        if own:
            links, link_source = [r.url for r in own], SOURCE_RECORD
        else:
            links, link_source = claimed_links(text, repo, number), SOURCE_REGEX

        # The exemption gets the same treatment, for the same reason: a written
        # `NO-PUBLICATION:` line is prose, and nothing stops that string appearing
        # inside a quoted example. A structured exemption is ATTRIBUTED -- it
        # names the agent who decided nothing was owed -- which is the property
        # that makes it reviewable rather than merely present.
        own_exempt = [
            e for e in exemptions if e.repo.split("/")[-1] == repo and e.pr == number
        ]
        if own_exempt:
            exemption = own_exempt[0].reason
            exemption_source = SOURCE_RECORD
            exemption_actor = own_exempt[0].actor
        else:
            # Every match is considered, not just the first, so that a genuine
            # exemption in a note is still found when the dispatch template sits
            # above it in the description. See PLACEHOLDER_REASON.
            reasons = [
                m.group(1).strip()
                for m in EXEMPTION.finditer(text)
                if not PLACEHOLDER_REASON.match(m.group(1).strip())
            ]
            exemption = reasons[0] if reasons else ""
            exemption_source = SOURCE_REGEX
            exemption_actor = ""

        scoped.append(
            DrainTask(
                task_id=task["id"],
                title=task.get("title", ""),
                status=(task.get("status") or "").upper(),
                closed_at=closed_at(task),
                repo=repo,
                number=number,
                links=links,
                exemption=exemption,
                link_source=link_source,
                exemption_source=exemption_source,
                exemption_actor=exemption_actor,
            )
        )
    return scoped, excluded


# ---------------------------------------------------------------------------
# GitHub: the authority for "did the verdict actually reach the artifact"


def _gh(endpoint: str, jq: str, timeout: int) -> list[str]:
    proc = subprocess.run(
        ["with-proxy", "gh", "api", "--paginate", endpoint, "--jq", jq],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if proc.returncode != 0:
        raise Unavailable(f"gh api {endpoint}: {(proc.stderr or '').strip()[:160]}")
    return [line for line in proc.stdout.splitlines() if line.strip()]


def pr_facts(repo: str, number: int, *, owner: str = DEFAULT_OWNER, timeout: int = 120) -> dict:
    """Merged state and every published comment URL for one pull request."""
    slug = f"{owner}/{repo}"
    merged = _gh(f"repos/{slug}/pulls/{number}", ".merged", timeout)
    urls: list[str] = []
    for endpoint in (
        f"repos/{slug}/issues/{number}/comments",
        f"repos/{slug}/pulls/{number}/reviews",
        f"repos/{slug}/pulls/{number}/comments",
    ):
        urls += _gh(endpoint, ".[].html_url", timeout)
    return {"merged": merged and merged[0].strip() == "true", "comment_urls": urls}


def gather_facts(
    prs: list[tuple[str, int]],
    *,
    cached: dict | None = None,
    workers: int = 6,
) -> dict:
    """Facts for each PR, fetched concurrently. Any failure is Unavailable."""
    facts = dict(cached or {})
    todo = [pr for pr in prs if f"{pr[0]}#{pr[1]}" not in facts]
    if not todo:
        return facts
    errors: list[str] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {pool.submit(pr_facts, repo, num): (repo, num) for repo, num in todo}
        for future in concurrent.futures.as_completed(futures):
            repo, num = futures[future]
            try:
                facts[f"{repo}#{num}"] = future.result()
            except (Unavailable, subprocess.SubprocessError, OSError) as exc:
                errors.append(f"{repo}#{num}: {exc}")
    if errors:
        # "I could not look" and "I looked and all is well" must never share an
        # exit code -- the discipline pr_status.py and drain_reconcile.py record.
        raise Unavailable(
            f"{len(errors)} of {len(todo)} pull request(s) could not be read: "
            + "; ".join(errors[:3])
        )
    return facts


def apply_facts(scoped: list[DrainTask], facts: dict) -> None:
    """Bind GitHub's answer onto each task. Unknown stays None, never False."""
    for task in scoped:
        fact = facts.get(task.pr)
        if fact is None:
            continue
        task.merged = bool(fact.get("merged"))
        published = {u.lower() for u in fact.get("comment_urls", [])}
        for link in task.links:
            (task.confirmed if link.lower() in published else task.forged).append(link)


# ---------------------------------------------------------------------------
# the rule


def in_scope(task: DrainTask) -> bool:
    """Closed, about one unmerged PR -- the case where the comment IS the output."""
    return task.terminal and task.merged is False


def satisfied(task: DrainTask) -> bool:
    return bool(task.confirmed) or bool(task.exemption)


def findings(
    scoped: list[DrainTask],
    since: str | None,
    *,
    grace_before: str | None = None,
) -> tuple[list[DrainTask], list[DrainTask], list[DrainTask]]:
    """(failing now, suppressed as pre-cutover, deferred inside the grace window).

    THE GRACE WINDOW IS A READ-AFTER-WRITE GUARD, not a softening. GitHub does
    not always show a just-written object to the very next read -- the same
    hazard that makes `refs/pull/N/head` report the OLD sha immediately after a
    push, which broke two pull requests on 2026-08-25. A comment posted seconds
    before a tick can therefore be absent from the comment list the tick reads,
    and the gate would name a task that DID publish. Deferring the youngest
    closures costs one tick of latency and removes that whole false-red class;
    the deferred count is printed, so a task cannot sit here unnoticed.
    """
    failing = [t for t in scoped if in_scope(t) and not satisfied(t)]
    deferred: list[DrainTask] = []
    if grace_before is not None:
        deferred = [t for t in failing if t.closed_at > grace_before]
        failing = [t for t in failing if t.closed_at <= grace_before]
    if not since:
        return failing, [], deferred
    return (
        [t for t in failing if t.closed_at >= since],
        [t for t in failing if t.closed_at < since],
        deferred,
    )


# ---------------------------------------------------------------------------
# reporting


def render_census(scoped: list[DrainTask], excluded: int, total: int, pre: bool) -> str:
    closed = [t for t in scoped if t.terminal]
    merged = [t for t in closed if t.merged]
    unmerged = [t for t in closed if t.merged is False]
    unpublished = [t for t in unmerged if not satisfied(t)]
    exempt = [t for t in unmerged if not t.confirmed and t.exemption]
    forged = [t for t in scoped if t.forged]
    lines = [
        f"tasks_scanned={total}",
        f"drain_tasks_naming_one_pr={len(scoped)} excluded_rollups_or_no_pr={excluded}",
        f"closed_in_scope={len(closed)} pr_merged={len(merged)} pr_unmerged={len(unmerged)}",
        f"unmerged_published={len(unmerged) - len(unpublished) - len(exempt)} "
        f"unmerged_exempt={len(exempt)} unmerged_UNPUBLISHED={len(unpublished)}",
        f"links_recorded_but_absent_from_github={len(forged)}",
        f"still_open={len([t for t in scoped if not t.terminal])}",
    ]
    if unmerged:
        pct = 100.0 * (len(unmerged) - len(unpublished)) / len(unmerged)
        lines.append(f"coverage_of_closed_unmerged={pct:.1f}%")

    # ⚠️ THIS IS THE LINE THAT RETIRES THE PROSE PATH, and the only honest way to
    # decide when. Every task whose evidence came from a structured record needed
    # no pattern matching at all; every task still on `regex` is one the posting
    # path did not couple, either because it predates the coupling or because
    # something posted without naming its task.
    #
    # It starts at 0/N BY CONSTRUCTION -- nothing already posted can grow a record
    # retroactively without inventing one. A rising number here is the coupling
    # working. Delete the regex when it reaches N, not when it feels safe.
    evidenced = [t for t in scoped if t.links or t.exemption]
    by_record = [
        t
        for t in evidenced
        if SOURCE_RECORD in (t.link_source, t.exemption_source)
    ]
    lines.append(
        f"evidence_from_structured_record={len(by_record)}/{len(evidenced)} "
        f"evidence_from_prose_regex={len(evidenced) - len(by_record)}"
    )
    if pre:
        lines += ["", "CLOSED WITH THE VERDICT UNPUBLISHED (oldest close first):"]
        for task in sorted(unpublished, key=lambda t: t.closed_at):
            lines.append(f"  {task.closed_at[:19]:19s}  {task.pr:16s}  {task.task_id}")
    if exempt:
        # Every exemption is named. An escape hatch nobody can enumerate is
        # indistinguishable from no rule.
        lines += ["", "EXEMPT BY WRITTEN NO-PUBLICATION MARKER:"]
        for task in sorted(exempt, key=lambda t: t.task_id):
            lines.append(f"  {task.pr:16s}  {task.task_id}: {task.exemption[:90]}")
    return "\n".join(lines)


def report(
    state: str,
    summary: str,
    *,
    json_output: bool = False,
    **fields: object,
) -> None:
    """Emit a result that both humans and tick-hub can read."""
    if json_output:
        print(json.dumps({"state": state, **fields, "summary": summary}, indent=2))
        return

    # tick_hub.engine.parse_kv_lines partitions each LINE on its first `=`, so
    # packing fields onto one line captures `state` and drops `summary`.
    print(f"state={state}")
    for key, value in fields.items():
        print(f"{key}={value}")
    # Last, and always present: a captured-but-empty summary is what takes the
    # emission down, so it is the one field that must never be omitted. Error
    # strings can contain newlines; collapse them so the line parser retains
    # the complete explanation instead of only its first line.
    print(f"summary={' '.join(summary.split())}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--db", type=Path, default=None)
    parser.add_argument(
        "--since",
        default=None,
        metavar="ISO8601",
        help="cutover: score only tasks closed at or after this. Earlier ones "
        "are counted and reported, never failed.",
    )
    parser.add_argument(
        "--grace-mins",
        type=int,
        default=15,
        metavar="N",
        help="do not score a task closed in the last N minutes: GitHub may not "
        "yet show a comment posted seconds ago, and a false red naming a task "
        "that did publish is worse than one tick of latency (default 15)",
    )
    parser.add_argument("--census", action="store_true", help="enumerate, exit 0")
    parser.add_argument(
        "--pre-cutover", action="store_true", help="with --census, list the backlog by name"
    )
    parser.add_argument(
        "--task", default=None, help="restrict to ONE task id (demonstrates the rule can fail)"
    )
    parser.add_argument(
        "--pr-facts",
        type=Path,
        default=None,
        help="read GitHub facts from this JSON instead of the network",
    )
    parser.add_argument("--save-pr-facts", type=Path, default=None)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    try:
        tasks = load_tasks(args.db)
    except Unavailable as exc:
        report("unavailable", f"cannot read the TaskGraph: {exc}", json_output=args.json)
        return NO_RESULT_EXIT

    try:
        scoped, excluded = collect(tasks)
    except Unavailable as exc:
        report("unavailable", f"cannot classify TaskGraph rows: {exc}", json_output=args.json)
        return NO_RESULT_EXIT
    total = len(tasks)

    if args.task is not None:
        scoped = [t for t in scoped if t.task_id == args.task]
        if not scoped:
            report(
                "unavailable",
                f"task {args.task!r} is not an in-scope drain task: not tagged "
                "drain/pr-drain, or its subject does not name exactly one pull request",
                json_output=args.json,
            )
            return NO_RESULT_EXIT

    cached = None
    if args.pr_facts is not None:
        try:
            cached = json.loads(args.pr_facts.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            report("unavailable", f"cannot read --pr-facts: {exc}", json_output=args.json)
            return NO_RESULT_EXIT

    # Only tasks that could possibly fail need a network round trip. Open tasks
    # and rollups are already out of scope, so the steady-state cost is the
    # handful of PRs drained since the cutover -- not the whole corpus.
    wanted = [t for t in scoped if t.terminal]
    if args.since and not args.census and args.task is None:
        wanted = [t for t in wanted if t.closed_at >= args.since]
    try:
        facts = gather_facts(sorted({(t.repo, t.number) for t in wanted}), cached=cached)
    except Unavailable as exc:
        report("unavailable", f"cannot read GitHub: {exc}", json_output=args.json)
        return NO_RESULT_EXIT
    apply_facts(scoped, facts)
    if args.save_pr_facts is not None:
        args.save_pr_facts.write_text(json.dumps(facts, indent=1), encoding="utf-8")

    if args.census and args.task is None:
        print(render_census(scoped, excluded, total, args.pre_cutover))
        return 0

    grace_before = (
        dt.datetime.now(dt.timezone.utc) - dt.timedelta(minutes=args.grace_mins)
    ).isoformat() if args.grace_mins > 0 else None
    failing, suppressed, deferred = findings(scoped, args.since, grace_before=grace_before)
    checked = [t for t in scoped if in_scope(t) and (not args.since or t.closed_at >= args.since)]

    # THE BACKLOG THE CUTOVER SUPPRESSES MUST STILL BE COUNTED.
    #
    # `suppressed` only ever contains tasks that were fully evaluated, and the
    # gated path deliberately fetches GitHub facts ONLY for tasks closed since
    # the cutover -- so pre-cutover tasks have no merged state, fall out of
    # `in_scope`, and reported as a flat 0. A cutover that silently zeroes the
    # backlog is the same defect as a gate that goes quiet: the number that
    # justified the cutover stops being visible the moment it starts applying.
    #
    # This count is deliberately the CHEAP one -- terminal, in-scope by subject,
    # closed before the cutover, with no comment permalink recorded and no
    # written exemption. It asks GitHub nothing, so it is a LINK-PRESENCE count,
    # not a verified one, and it is named that way. The 2026-08-25 census found
    # 25 such tasks of which GitHub said 9 were genuinely silent; `--census
    # --pre-cutover` is the verified breakdown.
    backlog = 0
    if args.since and args.task is None:
        backlog = sum(
            1
            for t in scoped
            if t.terminal and t.closed_at < args.since and not t.links and not t.exemption
        )
    exempt = [t for t in checked if not t.confirmed and t.exemption]
    forged = [t for t in failing if t.forged]

    if args.json:
        print(json.dumps({
            "state": "gap" if failing else "clean",
            "cutover": args.since,
            "scanned": total,
            "in_scope_closed_unmerged": len(checked),
            "excluded_rollups_or_no_pr": excluded,
            "exempt": [{"task": t.task_id, "pr": t.pr, "reason": t.exemption} for t in exempt],
            "unpublished": [
                {"task": t.task_id, "pr": t.pr, "closed_at": t.closed_at,
                 "links_absent_from_github": t.forged}
                for t in sorted(failing, key=lambda x: x.task_id)
            ],
            "suppressed_pre_cutover": len(suppressed),
            "pre_cutover_no_link_recorded": backlog,
            "deferred_in_grace_window": [t.task_id for t in deferred],
        }, indent=2))
        return 1 if failing else 0

    common = dict(
        checked=len(checked),
        exempt=len(exempt),
        suppressed_pre_cutover=len(suppressed),
        pre_cutover_no_link_recorded=backlog,
        deferred_in_grace=len(deferred),
    )

    if not failing:
        # NAME THE DENOMINATOR, AND NEVER LET AN EMPTY ONE READ AS A PASS.
        # `checked=0` with the prose "every drain task published its verdict"
        # is a true sentence that means nothing was measured -- and tick-hub
        # renders `{summary}` as the human-visible title, so that sentence is
        # what a coordinator would act on. Say which of the two happened.
        if not checked:
            report(
                "clean",
                f"NOTHING MEASURED: no drain task has closed against an unmerged "
                f"pull request since {args.since or 'the beginning'}. This is an "
                f"empty population, not a clean one ({backlog} pre-cutover task(s) "
                f"recorded no comment link; run --census --pre-cutover for the "
                f"GitHub-verified breakdown)",
                **common,
            )
            return 0
        # NEITHER AN EXEMPTION NOR A DEFERRAL IS A PUBLICATION. Folding any of
        # them into one "all published" sentence is how an escape hatch becomes
        # invisible -- and the deferred ones are UNPUBLISHED RIGHT NOW; they are
        # merely too young to accuse. Both are named in the line a coordinator
        # actually reads, and both are subtracted from the published count.
        waived = ""
        if exempt:
            waived += (
                f"; {len(exempt)} did not publish and said why: "
                + ", ".join(f"{t.task_id} ({t.exemption[:60]})" for t in exempt[:3])
            )
        if deferred:
            waived += (
                f"; {len(deferred)} closed unpublished within the last "
                f"{args.grace_mins}min and will be scored next tick: "
                + ", ".join(t.task_id for t in deferred[:3])
            )
        published = len(checked) - len(exempt) - len(deferred)
        report(
            "clean",
            f"{published} of {len(checked)} drain task(s) closed since "
            f"{args.since or 'the beginning'} against an unmerged pull request "
            f"published a verdict comment on it ({excluded} rollups out of "
            f"scope){waived}",
            **common,
        )
        return 0

    named = ", ".join(f"{t.task_id} ({t.pr})" for t in sorted(failing, key=lambda t: t.task_id))
    detail = ""
    if forged:
        detail = (
            f" {len(forged)} recorded a permalink GitHub does not have: "
            + "; ".join(f"{t.task_id} -> {t.forged[0]}" for t in forged[:3])
        )
    report(
        "gap",
        f"{len(failing)} drain task(s) closed with the verdict unpublished: "
        f"{named}. The analysis is in TaskGraph and the pull request shows "
        f"nothing. Post it, then record the comment permalink on the task -- or "
        f"write NO-PUBLICATION: <reason> if nothing was owed.{detail}",
        unpublished=len(failing),
        **common,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
