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


def from_resolution(
    resolution: Any,
    *,
    team: str,
    role: str,
    claimed_agent: str | None = None,
    machine: str | None = None,
    degraded: bool = False,
) -> Disclosure:
    """Validate process identity and build the only canonical disclosure.

    ``claimed_agent`` is used by boundaries such as the GitHub wrapper that
    must compare a caller assertion with process context.  The standalone
    printer omits it and prints the process-resolved agent directly.
    """

    if resolution.state == "AGREEMENT" and resolution.agent:
        agent = resolution.agent
    elif degraded:
        agent = _best_degraded_agent(resolution)
    else:
        raise DisclosureError(
            f"owner unresolved: identity resolver returned {resolution.state}; "
            f"signals={_signal_evidence(resolution)}"
        )
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
