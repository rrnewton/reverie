"""Structured publication records, written by the act that publishes.

WHY THIS EXISTS
---------------
`drain_publication.py` enforces "a drain task must not close with its verdict
unpublished" by PATTERN-MATCHING PROSE. That was the right thing to build first
-- it needed no migration and it works on today's corpus -- and its author named
it a heuristic that will drift. This is the durable half.

THE DIFFERENCE IS NOT TIDINESS. A regex over prose can only ask "does this text
LOOK LIKE someone published something". A record written at post time answers
"this publication happened, here is its id". The first is reconstructed after the
fact from a description of the act; the second is PRODUCED BY THE ACT. When the
two can come apart, they eventually do, and they come apart quietly.

WHY A NOTE GRAMMAR AND NOT `task_attachments`
---------------------------------------------
The `task_attachments` table exists and is empty, and it is the better SHAPE --
real columns, a UNIQUE constraint, a `kind` generated from the URI scheme. It was
rejected for one reason: nothing can write it except
`tg sql --write-i-got-approval-from-user`.

That flag is a HUMAN APPROVAL GATE. A tool that passes it on every call converts
the gate into a no-op for every future caller, which is precisely the class of
defect this repository keeps removing. `tg note` is a sanctioned agent write
path and needs no such assertion, so the record lives there instead.

If a `tg attach` subcommand ever exists, this module should move to it, and
`parse_records` should read both for a transition. The grammar below is
deliberately independent of where it is stored.

HOW THIS AVOIDS BEING THE REGEX AGAIN
-------------------------------------
The stated hazard with `NO-PUBLICATION:` is that "nothing stops that string
appearing inside a quoted example". A marker matched ANYWHERE in prose has that
problem no matter how baroque the marker.

So a record is only recognised when the ENTIRE note body is the record --
first line, field lines, nothing else. Quoting one inside a longer note produces
prose that contains a record and IS NOT one. You cannot write this by accident,
and you cannot write it by describing it.

THE THREE FALSE-POSITIVE CLASSES ARE PRESERVED, because they were paid for by
enumeration and a structured field must not reintroduce them:

  1. THE ROLLUP -- a task naming many artifacts has no single one to satisfy a
     per-artifact rule (144 of 226 drain-tagged tasks). A record names its own
     repo and pr explicitly, so a rollup simply carries several records and the
     caller decides; this module never infers "the" subject.
  2. THE ALREADY-SUPERSEDED OUTCOME -- 11 closed drain tasks merged their pull
     request and 7 correctly recorded no comment, because a landing publishes
     itself. This module reports what was recorded and takes no view on whether
     a record was owed.
  3. THE FOREIGN IDENTIFIER -- a bare `#N`, or a pull-request number arriving
     from another agent's coordination notes, read as this task's subject. A
     record carries `repo` and `pr` as its own fields, written by the poster from
     what it actually posted, so a number appearing in adjacent prose can never
     become the subject. THIS IS THE CLASS THE STRUCTURE ACTUALLY FIXES.
"""

from __future__ import annotations

import re
from typing import NamedTuple, Optional

RECORD_VERSION = "v1"
PUBLISHED_HEADER = f"PUBLISHED-COMMENT {RECORD_VERSION}"
EXEMPT_HEADER = f"PUBLICATION-EXEMPT {RECORD_VERSION}"

_FIELD = re.compile(r"^(?P<key>[a-z_]+):[ \t]*(?P<value>.*)$")
_REPO = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
_COMMENT_URL = re.compile(
    r"^https://github\.com/(?P<repo>[^/]+/[^/]+)/pull/(?P<pr>\d+)#issuecomment-(?P<cid>\d+)$"
)


class Publication(NamedTuple):
    """One comment this task's own posting path put on a pull request."""

    repo: str
    pr: int
    comment_id: int
    url: str
    actor: str


class Exemption(NamedTuple):
    """An attributed statement that nothing was owed. Not a string in prose."""

    repo: str
    pr: int
    actor: str
    reason: str


def render_publication(repo: str, pr: int, comment_url: str, actor: str) -> str:
    """The note body a poster writes. Derived from the URL it actually got back."""
    match = _COMMENT_URL.match(comment_url.strip())
    if match is None:
        raise ValueError(
            f"not a comment permalink: {comment_url!r}. The record must be built from "
            "the URL the post returned, not from one assembled by hand."
        )
    return "\n".join(
        [
            PUBLISHED_HEADER,
            f"repo: {repo}",
            f"pr: {pr}",
            f"comment: {match.group('cid')}",
            f"url: {comment_url.strip()}",
            f"actor: {actor}",
        ]
    )


def render_exemption(repo: str, pr: int, actor: str, reason: str) -> str:
    reason = " ".join(reason.split())
    if not reason:
        raise ValueError("an exemption must state a reason")
    return "\n".join(
        [EXEMPT_HEADER, f"repo: {repo}", f"pr: {pr}", f"actor: {actor}", f"reason: {reason}"]
    )


def _parse_body(body: str) -> tuple[Optional[str], dict[str, str]]:
    """(header, fields) when the WHOLE body is a record, else (None, {}).

    Whole-body is the load-bearing rule. A record quoted inside a longer note is
    prose that contains a record and is not one.
    """
    lines = [ln.rstrip() for ln in body.strip().splitlines()]
    if not lines:
        return None, {}
    header = lines[0].strip()
    if header not in (PUBLISHED_HEADER, EXEMPT_HEADER):
        return None, {}
    fields: dict[str, str] = {}
    for line in lines[1:]:
        if not line.strip():
            continue
        match = _FIELD.match(line)
        if match is None:
            # Any stray line disqualifies the whole note. Being strict here is
            # what stops a quoted record inside commentary from counting.
            return None, {}
        fields[match.group("key")] = match.group("value").strip()
    return header, fields


def parse_records(notes: list[str]) -> tuple[list[Publication], list[Exemption]]:
    """Every well-formed record among these note bodies.

    Malformed candidates are DROPPED SILENTLY BY DESIGN: this module reports what
    was recorded and never guesses. A caller that wants to know about near-misses
    should compare its count against the regex fallback, which is exactly what
    `drain_publication.py` does when it reports the source per task.
    """
    published: list[Publication] = []
    exempt: list[Exemption] = []
    for body in notes:
        header, fields = _parse_body(body or "")
        if header is None:
            continue
        repo = fields.get("repo", "")
        if not _REPO.match(repo):
            continue
        try:
            pr = int(fields.get("pr", ""))
        except ValueError:
            continue
        actor = fields.get("actor", "")
        if not actor:
            continue
        if header == PUBLISHED_HEADER:
            url = fields.get("url", "")
            match = _COMMENT_URL.match(url)
            if match is None:
                continue
            # The url is authoritative and the loose fields must agree with it.
            # A record whose fields disagree with its own permalink is not a
            # record, it is a claim about one.
            if match.group("repo").lower() != repo.lower() or int(match.group("pr")) != pr:
                continue
            try:
                comment_id = int(fields.get("comment", ""))
            except ValueError:
                continue
            if comment_id != int(match.group("cid")):
                continue
            published.append(Publication(repo.lower(), pr, comment_id, url, actor))
        else:
            reason = fields.get("reason", "")
            if reason:
                exempt.append(Exemption(repo.lower(), pr, actor, reason))
    return published, exempt


def publications_for(
    notes: list[str], repo: str, pr: int
) -> list[Publication]:
    """Records naming THIS pull request.

    The subject is taken from the caller, and the record carries its own repo and
    pr, so a number mentioned in neighbouring prose cannot become the subject.
    That is the foreign-identifier class, closed by construction rather than by
    a better pattern.
    """
    published, _ = parse_records(notes)
    return [p for p in published if p.repo == repo.lower() and p.pr == pr]


def exemptions_for(notes: list[str], repo: str, pr: int) -> list[Exemption]:
    _, exempt = parse_records(notes)
    return [e for e in exempt if e.repo == repo.lower() and e.pr == pr]
