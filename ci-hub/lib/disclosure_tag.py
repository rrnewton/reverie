#!/usr/bin/env python3
"""One fail-closed formatter and parser for the fleet who-is-talking tag.

``Disclosure.tag`` writes the tag and ``parse``/``parse_prefix`` read it back.
Both live here ON PURPOSE: the grammar is one thing, and a reader kept in a
different module drifts away from the writer.  ``lane_shas`` and the review
inventory diverged exactly that way, and the round-trip test below is what
holds these two together.

Until 2026-08-13 this module could only WRITE.  Every commit carried its
authoring agent in the tag, four test files parsed the tag ad hoc, and no
production code could answer "which agent wrote this commit?".  That gap is
why PR #2216's independent review lane was dispatched four times to agents
who could not fill it -- twice to a Codex-family process for a Claude lane,
then to the PR's own author.
"""

from __future__ import annotations

import re
import socket
from dataclasses import dataclass
from typing import Any


DEFAULT_TEAM = "hermit2"
ROLES = frozenset({"impl", "debugger", "reviewer", "relay", "coordinator", "human"})
TOKEN_RE = re.compile(r"^[A-Za-z0-9_.-]+$")
MODEL_TOKEN_RE = re.compile(r"^[^,\[\]\r\n]+$")


class DisclosureError(ValueError):
    """The process context cannot support the requested disclosure."""


@dataclass(frozen=True)
class Disclosure:
    team: str
    agent: str
    model: str
    machine: str
    role: str

    @property
    def tag(self) -> str:
        return (
            f"[{self.team}, {self.agent}, {self.model}, {self.machine}, "
            f"role={self.role}]"
        )


def _signal_evidence(resolution: Any) -> str:
    return "; ".join(
        f"{signal.source}={signal.state}"
        + (f":{signal.detail}" if signal.detail else "")
        for signal in resolution.signals
    )


def from_resolution(
    resolution: Any,
    *,
    team: str,
    role: str,
    claimed_agent: str | None = None,
    machine: str | None = None,
) -> Disclosure:
    """Validate process identity and build the only canonical disclosure.

    ``claimed_agent`` is used by boundaries such as the GitHub wrapper that
    must compare a caller assertion with process context.  The standalone
    printer omits it and prints the process-resolved agent directly.
    """

    if resolution.state != "AGREEMENT" or not resolution.agent:
        raise DisclosureError(
            f"owner unresolved: identity resolver returned {resolution.state}; "
            f"signals={_signal_evidence(resolution)}"
        )
    agent = resolution.agent
    if claimed_agent is not None and claimed_agent != agent:
        raise DisclosureError(
            f"--who mismatch: claimed {claimed_agent!r}, but process context "
            f"resolves to {agent!r} via "
            f"{','.join(resolution.basis) or 'primary identity'}"
        )
    if TOKEN_RE.fullmatch(agent) is None:
        raise DisclosureError(f"resolved agent is not a safe token: {agent!r}")
    if claimed_agent is not None and TOKEN_RE.fullmatch(claimed_agent) is None:
        raise DisclosureError(f"--who is not a safe agent token: {claimed_agent!r}")
    if TOKEN_RE.fullmatch(team) is None:
        raise DisclosureError(f"--team is not a safe team token: {team!r}")
    if role not in ROLES:
        raise DisclosureError(
            f"--role must be one of {','.join(sorted(ROLES))}; got {role!r}"
        )
    machine = machine or socket.gethostname().split(".", 1)[0]
    if TOKEN_RE.fullmatch(machine) is None:
        raise DisclosureError(
            f"derived machine shortname is not a safe token: {machine!r}"
        )
    model = resolution.model
    if model != model.strip() or MODEL_TOKEN_RE.fullmatch(model) is None:
        raise DisclosureError(
            f"process resolver returned an unsafe model token: {model!r}"
        )
    return Disclosure(team, agent, model, machine, role)


# The read counterpart to ``Disclosure.tag``.  Field classes are the SAME
# constants the formatter validates against, spliced in rather than retyped,
# so a change to TOKEN_RE cannot leave the reader accepting what the writer
# would refuse.  ``model`` is the only field permitting spaces, and because
# MODEL_TOKEN_RE forbids commas it cannot swallow the following ", machine"
# -- the separators stay unambiguous without backtracking.
_TOKEN = TOKEN_RE.pattern.strip("^$")
_MODEL = MODEL_TOKEN_RE.pattern.strip("^$")
TAG_RE = re.compile(
    rf"\[(?P<team>{_TOKEN}), (?P<agent>{_TOKEN}), (?P<model>{_MODEL}), "
    rf"(?P<machine>{_TOKEN}), role=(?P<role>{_TOKEN})\]"
)


def parse(text: str) -> Disclosure:
    """Read one disclosure tag occupying the WHOLE of ``text``.

    Raises ``DisclosureError`` rather than returning ``None``: a caller that
    cannot read the tag must be forced to decide what that means, because
    "no tag" and "tag I failed to parse" are different facts and only the
    caller knows whether either is safe to treat as absent.
    """

    match = TAG_RE.fullmatch(text.strip())
    if match is None:
        raise DisclosureError(f"not a disclosure tag: {text.strip()[:120]!r}")
    return _from_match(match)


def parse_prefix(subject: str) -> Disclosure:
    """Read the disclosure tag that PREFIXES a commit subject.

    Anchored at the start deliberately.  A tag quoted mid-sentence is a
    citation, not an attribution -- the same distinction the closure
    ancestry audit draws between a claim line and a mention of one.
    """

    match = TAG_RE.match(subject.lstrip())
    if match is None:
        raise DisclosureError(
            f"commit subject carries no leading disclosure tag: {subject[:120]!r}"
        )
    return _from_match(match)


def _from_match(match: re.Match[str]) -> Disclosure:
    role = match.group("role")
    if role not in ROLES:
        raise DisclosureError(
            f"tag role must be one of {','.join(sorted(ROLES))}; got {role!r}"
        )
    return Disclosure(
        match.group("team"),
        match.group("agent"),
        match.group("model"),
        match.group("machine"),
        role,
    )
