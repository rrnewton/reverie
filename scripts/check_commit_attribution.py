#!/usr/bin/env python3
"""Validate the final, machine-bearing role/team line of commit messages.

The canonical line is intentionally both human-readable and grep-able:

    [<role>, <model>] [<team-slug>, <machine>]

where ``machine`` is an exact ``devbigNNN`` token.  A comma-delimited final
field is required; merely mentioning ``devbig030`` somewhere in a free-form
team name would recreate the ambiguous attribution this checker replaces.

Agent identity is a separate, derived ``Agent:`` trailer.  It is NOT the team
slug: teams are shared between agents and one agent may legitimately change
team, so a team label cannot answer "were these two commits written by two
different agents?".  See :func:`reviewers_are_disjoint`, the single verifier
any author-disjointness consumer should call.
"""

from __future__ import annotations

import argparse
from collections.abc import Iterable, Sequence
import os
from pathlib import Path
import re
import socket
import subprocess
import sys


MACHINE_RE = re.compile(r"^devbig[0-9]+$")
HOST_MACHINE_RE = re.compile(r"(?:^|[^a-z0-9])(devbig[0-9]+)(?:[^a-z0-9]|$)")
TRAILER_RE = re.compile(
    r"^\[(?P<role>impl agent|adversarial-reviewer agent|coordinator|Human), "
    r"(?P<model>[^\[\]\r\n]+)\] "
    r"\[(?P<team>[A-Za-z0-9][A-Za-z0-9._/@+-]*), "
    r"(?P<machine>devbig[0-9]+)\]$"
)
IDENT_RE = re.compile(
    r"^(?P<name>[^<>\r\n]+) <(?P<email>[^<>\r\n]+)> [0-9]+ [+-][0-9]{4}$"
)
# The role/team line carries no agent identity: ``team`` is a TEAM, which is
# neither unique per agent nor stable across one agent's session, and ``model``
# is unvalidated free text.  Agent identity therefore rides in its own derived
# trailer, sourced from the process environment the way ``machine`` is sourced
# from the hostname -- never typed from context by the committing model.
AGENT_TRAILER_RE = re.compile(r"^Agent:[ \t]*(?P<agent>[A-Za-z0-9][A-Za-z0-9._/@+-]*)$")
AGENT_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/@+-]*$")
ALLOWED_OSS_AUTHOR_EMAILS = frozenset({"rrnewton@gmail.com"})
RESERVED_FIXTURE_DOMAINS = frozenset(
    {"example.com", "example.net", "example.org", "example.invalid"}
)
OWNER_AUTHOR_NAMES = frozenset({"ryan newton"})


class AttributionError(ValueError):
    """A proposed or recorded commit lacks canonical attribution."""


def detect_machine() -> str:
    """Return this host's stable devbig token, with an explicit test override."""

    override = os.environ.get("HERMIT_COMMIT_MACHINE")
    if override is not None:
        if not MACHINE_RE.fullmatch(override):
            raise AttributionError(
                "HERMIT_COMMIT_MACHINE must match devbigNNN, "
                f"got {override!r}"
            )
        return override

    host = socket.gethostname().lower()
    match = HOST_MACHINE_RE.search(host)
    if match is None:
        raise AttributionError(
            f"cannot derive a devbigNNN machine token from hostname {host!r}; "
            "set HERMIT_COMMIT_MACHINE explicitly"
        )
    return match.group(1)


def detect_agent() -> str | None:
    """Return this process's agent id from the environment, or None if absent.

    Mirrors :func:`detect_machine`.  ``DG_AGENT_NAME`` is the same value the
    liveness probe reads out of ``/proc``, so it is set by whatever launched the
    agent rather than chosen by the agent mid-session.  Absence returns None
    rather than raising: not every writer is an agent, and a missing id must not
    block a commit -- it only means identity cannot be asserted for it.
    """

    raw = os.environ.get("DG_AGENT_NAME")
    if raw is None or not raw.strip():
        return None
    agent = raw.strip()
    if not AGENT_NAME_RE.fullmatch(agent):
        raise AttributionError(
            f"DG_AGENT_NAME must match {AGENT_NAME_RE.pattern}, got {agent!r}"
        )
    return agent


def canonical_identity(value: str) -> str:
    """Fold one identity token for comparison.

    Case folding ONLY, and deliberately nothing else.  ``claude`` and ``CLAUDE``
    must not be two identities, but ``codex-1635`` and ``codex-2`` are two real
    agents that differ only by a trailing ordinal -- any normaliser that strips
    ordinals, prefixes, or punctuation merges them and silently defeats
    two-reviewer enforcement in the opposite direction.
    """

    return value.strip().casefold()


def agent_from_message(message: str) -> str | None:
    """Return the ``Agent:`` trailer of one message, or None when absent."""

    for line in reversed(message.splitlines()):
        match = AGENT_TRAILER_RE.fullmatch(line.strip())
        if match is not None:
            return match.group("agent")
    return None


def distinct_identities(identities: Iterable[str | None]) -> set[str]:
    """Canonical identities present, ignoring unknowns."""

    return {canonical_identity(i) for i in identities if i is not None and i.strip()}


def reviewers_are_disjoint(identities: Sequence[str | None], required: int = 2) -> bool:
    """Whether these identities are ``required`` genuinely distinct agents.

    The single verifier for author-disjointness.  An unknown identity is never
    counted, so a missing ``Agent:`` trailer fails closed instead of passing as
    an anonymous extra reviewer.
    """

    if any(i is None or not i.strip() for i in identities):
        return False
    return len(distinct_identities(identities)) >= required


def final_message_line(message: str) -> str:
    """Return the final stored line, ignoring Git's trailing editor comments."""

    lines = message.splitlines()
    while lines and (not lines[-1].strip() or lines[-1].lstrip().startswith("#")):
        lines.pop()
    if not lines:
        raise AttributionError("commit message is empty")
    return lines[-1]


def validate_message(
    message: str,
    expected_machine: str | None = None,
    expected_agent: str | None = None,
) -> str:
    """Validate one message and return its exact machine token.

    ``expected_agent`` cross-checks the ``Agent:`` trailer against the writer's
    own environment, exactly as ``expected_machine`` does for the hostname.  A
    message with no ``Agent:`` trailer is accepted: adoption is incremental and
    a missing id is an absence of a claim, not a false one.  A present trailer
    that disagrees with the writer IS rejected -- that is the spoof this closes.
    """

    if expected_agent is not None:
        claimed = agent_from_message(message)
        if claimed is not None and canonical_identity(claimed) != canonical_identity(
            expected_agent
        ):
            raise AttributionError(
                f"commit claims agent {claimed!r}, but this writer is "
                f"{expected_agent!r}; the Agent trailer is derived from "
                "DG_AGENT_NAME and must not be retyped"
            )
    line = final_message_line(message)
    match = TRAILER_RE.fullmatch(line)
    if match is None:
        raise AttributionError(
            "final commit line must be "
            "[<role>, <model>] [<team-slug>, devbigNNN]; "
            f"got {line!r}"
        )
    machine = match.group("machine")
    if expected_machine is not None and machine != expected_machine:
        raise AttributionError(
            f"commit claims machine {machine}, but this writer is {expected_machine}"
        )
    return machine


def validate_author_identity(name: str, email: str) -> None:
    """Refuse fabricated owner identities while permitting reserved fixtures."""

    if email in ALLOWED_OSS_AUTHOR_EMAILS:
        return
    domain = email.rpartition("@")[2].casefold()
    if domain in RESERVED_FIXTURE_DOMAINS and name.strip().casefold() not in OWNER_AUTHOR_NAMES:
        return
    allowed = ",".join(sorted(ALLOWED_OSS_AUTHOR_EMAILS))
    raise AttributionError(
        f"author identity {name!r} <{email}> is not owner-approved for these OSS repositories; "
        f"allowed={allowed}; reserved example addresses are fixture-only and cannot claim the owner name"
    )


def git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ("git", "-C", str(repo), *args),
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise AttributionError(f"git {' '.join(args)} failed: {detail}")
    return completed.stdout


def messages_from_commits(repo: Path, commits: list[str]) -> list[tuple[str, str]]:
    return [(sha, git(repo, "show", "-s", "--format=%B", sha)) for sha in commits]


def proposed_author_identity(repo: Path) -> tuple[str, str]:
    """Return the author Git will use, including --author/GIT_AUTHOR_* overrides."""

    ident = git(repo, "var", "GIT_AUTHOR_IDENT").strip()
    match = IDENT_RE.fullmatch(ident)
    if match is None:
        raise AttributionError(f"cannot parse proposed Git author identity {ident!r}")
    return match.group("name"), match.group("email")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--message-file", type=Path)
    source.add_argument("--stdin", action="store_true")
    source.add_argument("--commit")
    source.add_argument("--range", dest="commit_range")
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument(
        "--expected-machine",
        help="require this exact devbigNNN token; use 'auto' for the hostname",
    )
    parser.add_argument("--detect-machine", action="store_true")
    parser.add_argument(
        "--expected-agent",
        help=(
            "reject an Agent trailer that disagrees with this id; "
            "use 'auto' for DG_AGENT_NAME"
        ),
    )
    parser.add_argument("--detect-agent", action="store_true")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)
    if args.detect_machine or args.detect_agent:
        if any((args.message_file, args.stdin, args.commit, args.commit_range)):
            parser.error("--detect-machine/--detect-agent do not accept a message source")
    elif not any((args.message_file, args.stdin, args.commit, args.commit_range)):
        parser.error("one message source is required")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if args.detect_machine:
            print(detect_machine())
            return 0
        if args.detect_agent:
            agent = detect_agent()
            if agent is None:
                raise AttributionError("DG_AGENT_NAME is unset; no agent identity")
            print(agent)
            return 0

        expected_agent = args.expected_agent
        if expected_agent == "auto":
            expected_agent = detect_agent()
        elif expected_agent is not None and not AGENT_NAME_RE.fullmatch(expected_agent):
            raise AttributionError(
                f"--expected-agent must match {AGENT_NAME_RE.pattern}, "
                f"got {expected_agent!r}"
            )

        expected = args.expected_machine
        if expected == "auto":
            expected = detect_machine()
        elif expected is not None and not MACHINE_RE.fullmatch(expected):
            raise AttributionError(
                f"--expected-machine must match devbigNNN, got {expected!r}"
            )

        records: list[tuple[str, str]]
        author_records: list[tuple[str, str, str]]
        if args.message_file is not None:
            records = [(str(args.message_file), args.message_file.read_text())]
            name, email = proposed_author_identity(args.repo)
            author_records = [("proposed commit", name, email)]
        elif args.stdin:
            records = [("stdin", sys.stdin.read())]
            name, email = proposed_author_identity(args.repo)
            author_records = [("proposed commit", name, email)]
        elif args.commit is not None:
            records = messages_from_commits(args.repo, [args.commit])
            author_records = []
        else:
            commits = [
                line
                for line in git(args.repo, "rev-list", "--reverse", args.commit_range).splitlines()
                if line
            ]
            records = messages_from_commits(args.repo, commits)
            # Historical commits are immutable evidence, not proposed writes.  Do
            # not retroactively reject legitimate collaborators while checking
            # their machine-bearing trailers; the commit-msg path below guards
            # every new commit at creation time.
            author_records = []

        for identity, name, email in author_records:
            try:
                validate_author_identity(name, email)
            except AttributionError as error:
                raise AttributionError(f"{identity}: {error}") from error

        machines: set[str] = set()
        for identity, message in records:
            try:
                machines.add(validate_message(message, expected, expected_agent))
            except AttributionError as error:
                raise AttributionError(f"{identity}: {error}") from error

        if not args.quiet:
            machine_text = ",".join(sorted(machines)) if machines else "none"
            print(
                "commit-attribution: PASS "
                f"checked={len(records)} machines={machine_text}"
            )
        return 0
    except (AttributionError, OSError) as error:
        print(f"commit-attribution: REFUSED: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
