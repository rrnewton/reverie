#!/usr/bin/env python3
"""Independent, fail-closed liveness verdict for ONE named agent.

WHY THIS EXISTS. The standing mandate lets the coordinator replace a broken or
stuck agent "immediately and autonomously (close+respawn, no permission
needed)". That pairs an UNVERIFIED signal with an IRREVERSIBLE action. On
2026-08-08 health-tick reported a slot owner dead and a task ORPHANED with
`live_agents=6`, while `orc.listAgents()` showed 7 agents all busy and the
supposedly-dead `fleet-forensics` held the single most recent activity
timestamp on the box. Obeying the mandate literally would have destroyed a live
agent mid-P0.

An alarm is a TRIGGER TO CHECK, never a WARRANT TO ACT. This module is the
check, and it is deliberately built on a source INDEPENDENT of every gate that
can raise the alarm.

## The independent source

Every agent process carries `DG_AGENT_NAME` in its environment. Reading
`/proc/<pid>/environ` binds a LIVE PROCESS to an AGENT NAME without consulting
ORC, `ignored/ci-hub/agent-snapshot.json`, tick-hub, tmux, or the taskgraph.
The process either exists or it does not.

That independence is not cosmetic. Measured 2026-08-08: a `/proc` scan found
**29 distinct live agents while the snapshot health-tick reads listed 13** --
it was missing 16 of 29 (55%), because the snapshot is a cache of ONE orc
session's fleet and is structurally unable to see agents from another session.
Repairing its CONTENTS cannot fix that; only asking a different source can.

## Fail-closed, and specifically closed against its own breakage

DEAD is a POSITIVE observation of absence, and absence can only be claimed over
a scan whose coverage is known. So:

- No usable agent name at all               -> UNVERIFIABLE, refuse (rc 2).
- Any live process for the name             -> ALIVE, refuse (rc 1).
- Zero matches, coverage complete, probe
  demonstrably working                      -> VERIFIED_DEAD, permit (rc 0).
- Anything else                             -> UNVERIFIABLE, refuse (rc 2).

The first branch exists because the ONE thing this tool must never do is answer
a question it was not asked. Measured 2026-08-10: the ORC caller passed the
agent name in a shape the dispatcher did not bind, the probe was invoked with an
empty `$1`, and it answered `{"agent": "", "state": "verified-dead",
"may_replace": true}` -- for EVERY query, including agents that were
demonstrably alive. Nothing was wrong with the scan: it swept 265 processes,
carried `blind_spot: 0`, and saw another agent alive, so every self-check above
was satisfied. The verdict was simply about the empty name rather than about the
subject, and an empty name matches nothing, so absence was "established"
vacuously.

That is the Proxy Binding failure the policy names: a verdict CORRELATED with
the subject (it was produced by asking about it) but not BOUND to it. A caller
cannot detect it either, because `may_replace: true` looks identical whether it
was earned or vacuous. So the name is validated HERE, at the authority, and not
left to the plumbing -- fixing the one caller that got the shape wrong would
leave the next one able to reintroduce it silently.

"Anything else" includes the case that matters most: **if the scan cannot see
ANY agent at all, the probe cannot tell "this agent is dead" from "my detector
is broken", so it returns UNVERIFIABLE rather than 0.** Rename `DG_AGENT_NAME`,
run this in a container without host `/proc`, or break the environ read, and
this tool refuses every replacement instead of authorizing all of them. A
liveness check whose failure mode is "everyone looks dead" is worse than no
check at all when it is wired to a kill.

That zero-agent self-check is necessary but not sufficient.  Measured
2026-08-12: an agent sandbox exposed 4 same-uid processes and 1 healthy agent,
omitted the live queried agent, and therefore passed the zero-agent guard.  The
probe returned rc 0 VERIFIED-DEAD even though a full process view simultaneously
found 481 same-uid processes, 25 agents, and 5 live processes for the queried
name.  Hard Invariant 16 already assigns the right polarity: an incomplete
view is rc 2 UNVERIFIABLE and refuses replacement.  The check below closes a
hole in that existing zero-agent guard; it does not create a new authority or
change policy.

Blind spot is measured, not assumed: own-uid processes whose `environ` is
unreadable and which are not zombies (a zombie cannot be a running agent). On
2026-08-08 that was 5 of 1510 own-uid processes (0.3%). A nonzero blind spot
downgrades a would-be DEAD verdict to UNVERIFIABLE, because the agent could be
hiding in exactly the part we could not read.

THIS TOOL NEVER KILLS ANYTHING. It reads `/proc` and prints a verdict. It is
safe to run against any name at any time, which is what makes it usable as an
inert fixture in its own tests.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Sequence


IDENTITY_VAR = "DG_AGENT_NAME"

# Linux assigns this fixed inode to the initial PID namespace.  A process in a
# nested PID namespace can read a perfectly healthy /proc containing only its
# sandbox siblings; absence there says nothing about agents in the host
# namespace.  The 2026-08-12 incident was exactly that shape: 4 visible
# same-uid processes and 1 visible agent were incorrectly called a complete
# fleet scan while the host view contained 481 processes and 25 agents.
INITIAL_PID_NAMESPACE_LINK = "pid:[4026531836]"
HOST_INIT_COMM = "systemd"

# Exit codes deliberately mirror ci-hub/bin/close-task so the polarity is
# familiar across destructive-action gates: 0 authorizes, nonzero refuses.
# Every unexpected exception therefore also refuses.
VERIFIED_DEAD = 0
REFUSED_ALIVE = 1
UNVERIFIABLE = 2

# Process states that mean "this is not a running agent". Z is a reaped-but-not
# -waited corpse; X is dead. Everything else (R, S, D, T, t, I) is a process
# that exists and could be mid-work -- notably T/t (stopped), which a land-lock
# census can produce and which must NEVER read as dead.
NOT_RUNNING_STATES = frozenset("ZX")

# An unreadable `environ` only threatens the verdict if the process could be an
# agent CLI. `comm` stays readable when `environ` does not (the kernel gates
# environ on ptrace_may_access, not file mode), so it is a checkable
# discriminator rather than an assumption.
#
# Measured 2026-08-08: all 5 own-uid non-zombie unreadable processes were
# session infrastructure -- `systemd` (user manager), `(sd-pam)`, `sshd` x2,
# `gnome-keyring-d`. None can host an agent. Counting them would have forced
# UNVERIFIABLE on every genuine death and DISABLED SELF-HEALING ENTIRELY, which
# is its own failure mode.
#
# Keep this set NARROW. It is an exclusion from a safety check, so widening it
# widens the hole; `test_unreadable_agent_comm_still_blocks` pins the direction
# by proving an unreadable `claude` DOES block.
AGENT_COMMS = frozenset({"claude", "codex", "node"})

# An empty/whitespace `DG_AGENT_NAME` is never a real agent identity, so a query
# carrying one is a BROKEN CALLER, not a dead agent. Kept as a constant so the
# test asserts the same string the tool emits.
NO_NAME_REASON = (
    "no agent name was supplied, so there is no subject to verify; a blank name "
    "matches no process and would make 'absence' vacuously true. This is a "
    "broken caller, not a dead agent -- refusing rather than authorizing a "
    "replacement nobody named."
)


def _usable_name(name: str | None) -> bool:
    """A name that can actually bind a verdict to a subject."""
    return bool(name and name.strip())


@dataclass(frozen=True)
class Match:
    pid: int
    state: str

    @property
    def running(self) -> bool:
        return self.state not in NOT_RUNNING_STATES


@dataclass(frozen=True)
class Scan:
    """What the /proc sweep actually saw, with its denominator."""

    matches: tuple[Match, ...] = ()
    own_uid_total: int = 0
    zombies: int = 0
    blind_spot: int = 0
    """Unreadable-environ processes that COULD be an agent CLI (comm in AGENT_COMMS)."""
    unreadable_non_agent: int = 0
    """Unreadable-environ processes excluded because their comm cannot host an agent."""
    distinct_agents_seen: frozenset[str] = field(default_factory=frozenset)
    process_view: str = "unverifiable"
    """Whether /proc is proven to be the host PID namespace."""
    process_view_reason: str = "process view was not inspected"
    scan_error: str | None = None

    @property
    def live_matches(self) -> tuple[Match, ...]:
        return tuple(m for m in self.matches if m.running)


def _proc_state(proc: Path) -> str:
    """Second field of /proc/<pid>/stat, after the (possibly paren-laden) comm."""
    try:
        raw = (proc / "stat").read_text()
    except OSError:
        return "?"
    _, _, rest = raw.partition(") ")
    return rest[:1] or "?"


def _proc_comm(proc: Path) -> str:
    try:
        return (proc / "comm").read_text().strip()
    except OSError:
        # Unknown comm is treated as agent-capable: an unreadable name must not
        # buy an exclusion from the safety check.
        return "?"


def _process_view(root: Path) -> tuple[str, str]:
    """Classify whether ``root`` exposes the full host PID namespace.

    Seeing one healthy bystander proves only that the probe can see *itself*.
    It does not prove that another agent is absent.  Absence is authoritative
    only from Linux's initial PID namespace with the real host init visible.
    Any missing or contradictory marker is therefore UNVERIFIABLE rather than
    a reason to guess.
    """

    namespace_path = root / "self" / "ns" / "pid"
    try:
        namespace = os.readlink(namespace_path)
    except OSError as error:
        return (
            "unverifiable",
            f"cannot establish PID namespace from {namespace_path}: {error}",
        )

    if namespace != INITIAL_PID_NAMESPACE_LINK:
        return (
            "restricted",
            f"PID namespace is {namespace}, not host {INITIAL_PID_NAMESPACE_LINK}",
        )

    init_path = root / "1" / "comm"
    try:
        init_comm = init_path.read_text().strip()
    except OSError as error:
        return (
            "unverifiable",
            f"cannot establish host init from {init_path}: {error}",
        )

    if init_comm != HOST_INIT_COMM:
        return (
            "restricted",
            f"PID 1 is {init_comm!r}, not host init {HOST_INIT_COMM!r}",
        )

    return (
        "full",
        f"initial PID namespace {namespace} with PID 1 {init_comm}",
    )


def scan_proc(name: str, *, root: Path = Path("/proc")) -> Scan:
    """Sweep /proc once, collecting both the matches AND the coverage facts."""
    me = os.getuid()
    process_view, process_view_reason = _process_view(root)
    matches: list[Match] = []
    seen: set[str] = set()
    own = zombies = blind = excluded = 0

    try:
        entries = [p for p in root.iterdir() if p.name.isdigit()]
    except OSError as error:
        return Scan(scan_error=f"cannot enumerate {root}: {error}")

    for proc in entries:
        try:
            if proc.stat().st_uid != me:
                # Another user's process cannot be one of our agents, and is
                # not a blind spot -- excluding it keeps the denominator honest
                # rather than inflating it with 3000+ irrelevant processes.
                continue
        except OSError:
            continue
        own += 1
        state = _proc_state(proc)
        try:
            raw = (proc / "environ").read_bytes()
        except OSError:
            comm = _proc_comm(proc)
            if state == "Z":
                zombies += 1
            elif comm == "?" or comm in AGENT_COMMS:
                # Unknown comm counts as agent-capable: failing to read the
                # name must not buy an exclusion from the safety check.
                blind += 1
            else:
                excluded += 1
            continue
        for item in raw.split(b"\0"):
            if not item.startswith(IDENTITY_VAR.encode() + b"="):
                continue
            value = item.split(b"=", 1)[1].decode("utf-8", "replace")
            seen.add(value)
            if value == name:
                matches.append(Match(pid=int(proc.name), state=state))
            break

    return Scan(
        matches=tuple(matches),
        own_uid_total=own,
        zombies=zombies,
        blind_spot=blind,
        unreadable_non_agent=excluded,
        distinct_agents_seen=frozenset(seen),
        process_view=process_view,
        process_view_reason=process_view_reason,
    )


@dataclass(frozen=True)
class Verdict:
    state: str
    reason: str
    scan: Scan

    @property
    def rc(self) -> int:
        return {
            "alive": REFUSED_ALIVE,
            "verified-dead": VERIFIED_DEAD,
        }.get(self.state, UNVERIFIABLE)

    @property
    def may_replace(self) -> bool:
        return self.state == "verified-dead"


def judge(name: str, scan: Scan) -> Verdict:
    """Pure verdict over already-collected evidence. No I/O."""
    # FIRST, before any evidence is consulted. Every branch below reasons about
    # what the scan saw for `name`; if `name` is blank they all reason about
    # nothing and the most permissive of them (a complete scan with no match)
    # wins. Guarding here rather than only in `verify` keeps the rule on the
    # pure authority, so a caller that assembles its own Scan cannot bypass it.
    if not _usable_name(name):
        return Verdict("unverifiable", NO_NAME_REASON, scan)

    if scan.scan_error:
        return Verdict("unverifiable", scan.scan_error, scan)

    live = scan.live_matches
    if live:
        pids = ",".join(str(m.pid) for m in live)
        return Verdict(
            "alive",
            f"{len(live)} live process(es) carry {IDENTITY_VAR}={name}: pid {pids}",
            scan,
        )

    # A positive live match is safe even through a restricted view.  Absence
    # is not: a sandbox can omit the live subject while still exposing itself
    # as a healthy bystander.  This check must therefore precede every DEAD
    # branch, including the zombie-only branch.
    if scan.process_view != "full":
        return Verdict(
            "unverifiable",
            f"RESTRICTED PROCESS VIEW ({scan.process_view}): "
            f"{scan.process_view_reason}; cannot establish that {name} is absent "
            "from the fleet. Run the registered liveness authority with a full "
            "process view; refusing replacement.",
            scan,
        )

    # SECOND SELF-CHECK.  Host-view proof above closes the partial-view hole;
    # this branch retains the original zero-agent guard.  If the sweep found
    # no agent identity whatsoever, the probe is not working -- and a broken
    # probe must not authorize a kill by reporting "nobody is there".
    if not scan.distinct_agents_seen:
        return Verdict(
            "unverifiable",
            f"probe saw ZERO agents of any name across {scan.own_uid_total} own-uid "
            f"process(es); cannot distinguish '{name} is dead' from 'this probe is "
            f"broken'. Refusing rather than authorizing every replacement at once.",
            scan,
        )

    if scan.blind_spot:
        return Verdict(
            "unverifiable",
            f"{name} not found, but {scan.blind_spot} unreadable-environ process(es) "
            f"have an agent-capable comm; absence is not established over an "
            f"incomplete scan.",
            scan,
        )

    if scan.matches:
        # Present but every match is a corpse. Real death, and worth its own
        # message so the reader knows it was observed rather than inferred.
        return Verdict(
            "verified-dead",
            f"{len(scan.matches)} process(es) carried {IDENTITY_VAR}={name} and all "
            f"are zombie/dead; {len(scan.distinct_agents_seen)} other agent(s) "
            f"observed alive, so the probe is working.",
            scan,
        )

    return Verdict(
        "verified-dead",
        f"no process carries {IDENTITY_VAR}={name} across a complete scan of "
        f"{scan.own_uid_total} own-uid process(es) (blind spot 0); "
        f"{len(scan.distinct_agents_seen)} other agent(s) observed alive, so the "
        f"probe is working.",
        scan,
    )


def verify(name: str, *, root: Path = Path("/proc")) -> Verdict:
    # Short-circuit before the sweep: with no subject there is nothing a scan
    # could establish, and an empty Scan keeps the reported coverage honest
    # (own_uid_total 0 = "did not look") instead of implying a complete sweep.
    if not _usable_name(name):
        return judge(name, Scan())
    return judge(name, scan_proc(name, root=root))


def _render(name: str, verdict: Verdict, *, as_json: bool) -> str:
    scan = verdict.scan
    if as_json:
        return json.dumps(
            {
                "agent": name,
                "state": verdict.state,
                "may_replace": verdict.may_replace,
                "rc": verdict.rc,
                "reason": verdict.reason,
                "live_pids": [m.pid for m in scan.live_matches],
                "own_uid_total": scan.own_uid_total,
                "zombies": scan.zombies,
                "blind_spot": scan.blind_spot,
                "unreadable_non_agent": scan.unreadable_non_agent,
                "distinct_agents_seen": len(scan.distinct_agents_seen),
                "process_view": scan.process_view,
                "process_view_reason": scan.process_view_reason,
            },
            sort_keys=True,
        )
    banner = {
        "alive": "REFUSED",
        "verified-dead": "VERIFIED-DEAD",
    }.get(verdict.state, "UNVERIFIABLE")
    return (
        f"{banner} agent={name} may_replace={str(verdict.may_replace).lower()} "
        f"reason={verdict.reason} "
        f"coverage=own_uid:{scan.own_uid_total},zombies:{scan.zombies},"
        f"blind_spot:{scan.blind_spot},"
        f"unreadable_non_agent:{scan.unreadable_non_agent},"
        f"agents_seen:{len(scan.distinct_agents_seen)},"
        f"process_view:{scan.process_view} "
        f"rc={verdict.rc}"
    )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Verify, from process evidence independent of any health gate, whether a "
            "named agent is genuinely dead. Required immediately before any "
            "close+respawn. Never kills anything."
        )
    )
    # Required positional, so a caller that passes NOTHING is rejected by
    # argparse with SystemExit(2) -- which is UNVERIFIABLE, i.e. it refuses.
    # That polarity is load-bearing rather than lucky, and
    # `test_missing_argument_exits_unverifiable` pins it: argparse's usage-error
    # code and this module's refusal code must not be allowed to drift apart.
    #
    # A caller that passes an EMPTY string gets past argparse (it is a present
    # argument), which is the shape actually observed in the wild, and is caught
    # by the `_usable_name` guard in `judge`.
    parser.add_argument("agent", help="the DG_AGENT_NAME to verify")
    parser.add_argument("--json", action="store_true", dest="as_json")
    parser.add_argument(
        "--proc-root",
        type=Path,
        default=Path("/proc"),
        help="override the /proc root (tests only)",
    )
    args = parser.parse_args(argv)

    verdict = verify(args.agent, root=args.proc_root)
    stream = sys.stdout if verdict.may_replace else sys.stderr
    print(_render(args.agent, verdict, as_json=args.as_json), file=stream)
    return verdict.rc


if __name__ == "__main__":
    raise SystemExit(main())
