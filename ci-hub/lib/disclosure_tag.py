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


def _remediation(resolution: Any) -> str:
    """Name the RECORD that is wrong, not the component that noticed.

    ⚠️ WHY THIS EXISTS. The bare message says "identity resolver returned
    DISAGREEMENT", so every reader goes and reads the resolver. The resolver is
    almost never at fault: it is reporting that two STALE ENTRIES in
    worktree-state.json name different agents. Measured 2026-08-25 -- the text
    sent one agent into agent_identity.py, and the actual fault was a slot
    pointing at a task closed days earlier plus a RELEASED slot still claiming a
    tmux pane that a different, live agent now occupies. An error that names the
    layer that detected the problem instead of the layer that holds it costs
    every future reader the same wrong search.

    Both known causes have TWO places to fix, and correcting one only changes
    the message -- which reads like a new problem and is the same one. That is
    why the hints below name both.
    """

    problems = {
        signal.source
        for signal in resolution.signals
        if signal.state != "resolved"
    }
    hints: list[str] = []
    if "taskgraph-owner" in problems:
        hints.append(
            "taskgraph-owner: set BOTH `slots.<N>.task` AND "
            "`slots.<N>.agents[<you>].task` to a live IN_PROGRESS task -- "
            "slot_owner_signal unions the slot-level key with every agent entry, "
            "so fixing one turns 'is status=CLOSED' into 'multiple candidate tasks'"
        )
    if "pane-owner" in problems or any(
        signal.source == "pane-owner" and signal.state == "resolved"
        for signal in resolution.signals
    ):
        hints.append(
            "pane-owner: another slot may claim your $TMUX_PANE. Clear it from BOTH "
            "`agents[].tmux_pane_id` AND `owner_sidecar.tmux_pane_id` -- "
            "pane_owner_signal matches either. Only do this for a slot whose "
            "status is `released`; confirm with `tmux list-panes -a` that the "
            "pane's live pid is one of the ancestors listed above"
        )
    if not hints:
        return ""
    return (
        "\n  ⚠️ THIS IS USUALLY A STALE RECORD IN worktree-state.json, NOT A BROKEN "
        "RESOLVER. The resolver is reporting that readable signals name different "
        "agents; the wrong data is in the state file.\n  "
        + "\n  ".join(f"- {hint}" for hint in hints)
        + "\n  The file is gitignored (not a parent-repo change). Edit it under "
        "`flock worktree-state.lock` and back it up first.\n"
        "  Verify with `who-am-i --tag`: `degraded-<agent>` in the tag means the "
        "resolver still cannot agree."
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
            + _remediation(resolution)
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
