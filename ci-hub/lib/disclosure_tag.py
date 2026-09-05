#!/usr/bin/env python3
"""Format the fleet who-is-talking disclosure tag."""

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
    unverified_agent: str | None = None

    @property
    def tag(self) -> str:
        return (
            f"[{self.team}, {self.agent}, {self.model}, {self.machine}, "
            f"role={self.role}]"
        )

    @property
    def unverified_metadata(self) -> str | None:
        if self.unverified_agent is None:
            return None
        return f"Unverified --who metadata: {self.unverified_agent}"


def _best_degraded_agent(resolution: Any) -> str:
    candidates = [resolution.agent]
    candidates.extend(
        signal.agent
        for signal in resolution.signals
        if signal.state == "resolved"
    )
    candidates.extend(
        candidate
        for signal in resolution.signals
        for candidate in signal.candidates
    )
    for candidate in candidates:
        if candidate is not None and TOKEN_RE.fullmatch(candidate) is not None:
            return f"degraded-{candidate}"
    return "degraded-unresolved"


def for_human(
    *,
    team: str,
    role: str = "human",
    machine: str | None = None,
) -> Disclosure:
    """Build the canonical disclosure for an identified interactive human."""

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
    return Disclosure(team, "human", "unresolved", machine, role)


def from_resolution(
    resolution: Any,
    *,
    team: str,
    role: str,
    claimed_agent: str | None = None,
    machine: str | None = None,
    degraded: bool = False,
) -> Disclosure:
    """Build a canonical disclosure without making an agent name authority.

    A caller-supplied name is recorded separately as explicitly unverified audit
    metadata when present. It never occupies the disclosure's agent field. An
    agreed process-context name is useful metadata; unresolved or disagreeing
    process context degrades visibly instead of refusing the write.
    None of these names authorizes the operation that carries the disclosure.

    ``degraded`` remains accepted for callers of the earlier interface. The
    result now always degrades when process identity is unavailable because a
    transient fleet-agent name cannot be a mutation boundary.
    """

    if claimed_agent is not None and TOKEN_RE.fullmatch(claimed_agent) is None:
        raise DisclosureError(f"--who is not a safe metadata token: {claimed_agent!r}")
    if resolution.state == "AGREEMENT" and resolution.agent:
        agent = resolution.agent
    else:
        agent = _best_degraded_agent(resolution)
    if TOKEN_RE.fullmatch(agent) is None:
        raise DisclosureError(f"resolved agent is not a safe token: {agent!r}")
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
    return Disclosure(team, agent, model, machine, role, claimed_agent)
