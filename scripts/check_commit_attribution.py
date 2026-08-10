#!/usr/bin/env python3
"""Validate the final, machine-bearing role/team line of commit messages.

The canonical line is intentionally both human-readable and grep-able:

    [<role>, <model>] [<team-slug>, <machine>]

where ``machine`` is an exact ``devbigNNN`` token.  A comma-delimited final
field is required; merely mentioning ``devbig030`` somewhere in a free-form
team name would recreate the ambiguous attribution this checker replaces.
"""

from __future__ import annotations

import argparse
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
IDENT_EMAIL_RE = re.compile(r"<(?P<email>[^<>\r\n]+)> [0-9]+ [+-][0-9]{4}$")
ALLOWED_OSS_AUTHOR_EMAILS = frozenset({"rrnewton@gmail.com"})


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


def final_message_line(message: str) -> str:
    """Return the final stored line, ignoring Git's trailing editor comments."""

    lines = message.splitlines()
    while lines and (not lines[-1].strip() or lines[-1].lstrip().startswith("#")):
        lines.pop()
    if not lines:
        raise AttributionError("commit message is empty")
    return lines[-1]


def validate_message(message: str, expected_machine: str | None = None) -> str:
    """Validate one message and return its exact machine token."""

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


def validate_author_email(email: str) -> None:
    """Refuse a proposed OSS commit whose author identity is fabricated."""

    if email not in ALLOWED_OSS_AUTHOR_EMAILS:
        allowed = ",".join(sorted(ALLOWED_OSS_AUTHOR_EMAILS))
        raise AttributionError(
            f"author email {email!r} is not owner-approved for these OSS repositories; "
            f"allowed={allowed}"
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


def proposed_author_email(repo: Path) -> str:
    """Return the author Git will use, including --author/GIT_AUTHOR_* overrides."""

    ident = git(repo, "var", "GIT_AUTHOR_IDENT").strip()
    match = IDENT_EMAIL_RE.search(ident)
    if match is None:
        raise AttributionError(f"cannot parse proposed Git author identity {ident!r}")
    return match.group("email")


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
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)
    if args.detect_machine:
        if any((args.message_file, args.stdin, args.commit, args.commit_range)):
            parser.error("--detect-machine does not accept a message source")
    elif not any((args.message_file, args.stdin, args.commit, args.commit_range)):
        parser.error("one message source is required")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if args.detect_machine:
            print(detect_machine())
            return 0

        expected = args.expected_machine
        if expected == "auto":
            expected = detect_machine()
        elif expected is not None and not MACHINE_RE.fullmatch(expected):
            raise AttributionError(
                f"--expected-machine must match devbigNNN, got {expected!r}"
            )

        records: list[tuple[str, str]]
        author_records: list[tuple[str, str]]
        if args.message_file is not None:
            records = [(str(args.message_file), args.message_file.read_text())]
            author_records = [("proposed commit", proposed_author_email(args.repo))]
        elif args.stdin:
            records = [("stdin", sys.stdin.read())]
            author_records = [("proposed commit", proposed_author_email(args.repo))]
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

        for identity, email in author_records:
            try:
                validate_author_email(email)
            except AttributionError as error:
                raise AttributionError(f"{identity}: {error}") from error

        machines: set[str] = set()
        for identity, message in records:
            try:
                machines.add(validate_message(message, expected))
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
