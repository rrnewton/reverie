#!/usr/bin/env python3
"""Validate the canonical, machine-bearing disclosure in commit messages.

New commits put the exact output of ``ci-hub/bin/who-am-i --tag --role ROLE``
on the first line of the body::

    [<team>, <agent>, <model>, <machine>, role=<role>]

``machine`` remains an exact ``devbigNNN`` token.  The earlier final-line form
``[<role>, <model>] [<team>, <machine>]`` remains an explicit compatibility
path for stored history and producers that have not migrated.  It must not hide
a duplicate, malformed, misplaced, or conflicting five-field disclosure.

The five-field form carries agent identity directly.  The legacy form may also
carry the separately derived ``Agent:`` trailer.  See
:func:`reviewers_are_disjoint`, the single verifier any author-disjointness
consumer should call.
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
# This is the exact field order and delimiter spelling emitted by
# ci-hub/lib/disclosure_tag.py.  The machine is deliberately narrower here than
# disclosure_tag's generic safe-token check: commit provenance has always
# required the host's stable devbigNNN token.
CANONICAL_DISCLOSURE_RE = re.compile(
    r"^\[(?P<team>[A-Za-z0-9_.-]+), "
    r"(?P<agent>[A-Za-z0-9_.-]+), "
    r"(?P<model>[^,\[\]\r\n]+), "
    r"(?P<machine>devbig[0-9]+), "
    r"role=(?P<role>[A-Za-z0-9_.-]+)\]$"
)
CANONICAL_ROLES = frozenset(
    {"impl", "debugger", "reviewer", "relay", "coordinator", "human"}
)
TRAILER_RE = re.compile(
    r"^\[(?P<role>impl agent|adversarial-reviewer agent|coordinator|Human), "
    r"(?P<model>[^\[\]\r\n]+)\] "
    r"\[(?P<team>[A-Za-z0-9][A-Za-z0-9._/@+-]*), "
    r"(?P<machine>devbig[0-9]+)\]$"
)
LEGACY_ROLE_FOR_CANONICAL = {
    "impl": "impl agent",
    "reviewer": "adversarial-reviewer agent",
    "coordinator": "coordinator",
    "human": "Human",
}
IDENT_RE = re.compile(
    r"^(?P<name>[^<>\r\n]+) <(?P<email>[^<>\r\n]+)> [0-9]+ [+-][0-9]{4}$"
)
# The legacy role/team line carries no agent identity: ``team`` is a TEAM,
# which is neither unique per agent nor stable across one agent's session.
# Legacy agent identity therefore rides in its own derived trailer.  The
# canonical five-field disclosure carries the same derived identity directly.
AGENT_TRAILER_RE = re.compile(r"^Agent:[ \t]*(?P<agent>[A-Za-z0-9][A-Za-z0-9._/@+-]*)$")
AGENT_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/@+-]*$")
BOT_OSS_AUTHOR_EMAIL = "330061689+rrnewton-bots@users.noreply.github.com"
BOT_OSS_AUTHOR_NAME = "rrnewton-bots"
HUMAN_OSS_AUTHOR_EMAIL = "rrnewton@gmail.com"
ALLOWED_OSS_AUTHOR_EMAILS = frozenset(
    {BOT_OSS_AUTHOR_EMAIL, HUMAN_OSS_AUTHOR_EMAIL}
)
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
                "HERMIT_COMMIT_MACHINE must match devbigNNN, " f"got {override!r}"
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


# ⚠️ THE MODEL FIELD IS THE ONE LANE INDEPENDENCE RESTS ON, AND IT WAS THE ONE
# FIELD NOTHING DERIVED. `machine` comes from the hostname and the canonical
# agent field (or legacy `Agent:` trailer) comes from DG_AGENT_NAME -- both are
# refused when retyped. `model` was
# `(?P<model>[^\[\]\r\n]+)`, any text without brackets, and this module's own
# docstring said so. `post-facto-review` requires one Claude-family AND one
# Codex-family exact-head approval, so the gate closed the spoof on WHICH AGENT
# and left it open on WHICH FAMILY.
#
# NOT HYPOTHETICAL: agent(hermit-107) was dispatched into the codex lane on
# hermit#2694, found it was claude-opus-5 and declined to emit a codex marker.
# That refusal was the only thing that made the empty lane visible; no tool
# would have caught it. The same agent then typed `claude-opus-5` into its own
# trailers from context -- honestly, and unverifiably.
#
# ⚠️ THE VOCABULARY IS READ OFF THE CORPUS, NOT INVENTED. Measured over the last
# 3000 dev-hermit commits, the distinct model fields actually used were:
#   claude side  claude-opus-5 351, opus-5 94, opus-4.8 35, CLAUDE 26, OPUS 19,
#                claude 9, FABLE 21, claude-sonnet-4.5 2
#   codex side   CODEX 132, gpt-5.6-sol 112, GPT-5.6-SOL 24, codex 17,
#                GPT-5.6 12, GPT-5 12, gpt-5 7, GPT-5.6-Codex 1
#   neither      automation 425, unresolved 114, primary-checkout 44
# That last row is 583 commits which must NOT be refused, and is why an
# unrecognised model is an ABSENCE rather than a violation.
CLAUDE_MODEL_MARKERS = ("claude", "opus", "sonnet", "haiku", "fable")
CODEX_MODEL_MARKERS = ("codex", "gpt")


def declared_model_family(model: str) -> str | None:
    """Which family the trailer's model string names, or None when it names none.

    ⚠️ A STRING NAMING BOTH IS UNRESOLVED, NOT A PICK. `GPT-5.6-Codex` names one
    family twice and resolves; a hypothetical `claude-via-gpt` names two and must
    not be forced into either, because a wrong family here refuses an honest
    commit and that is how a gate gets switched off.
    """

    lowered = model.casefold()
    claude = any(marker in lowered for marker in CLAUDE_MODEL_MARKERS)
    codex = any(marker in lowered for marker in CODEX_MODEL_MARKERS)
    if claude and not codex:
        return "claude"
    if codex and not claude:
        return "codex"
    return None


def detect_model_family() -> str | None:
    """Return the family of the client that launched this process, from /proc.

    Mirrors :func:`detect_machine` and :func:`detect_agent`: sourced from
    something the committing model cannot author. The walk lives in
    ``ci-hub/lib/agent_identity.py`` and is IMPORTED rather than copied -- a
    second extractor of the same fact is what diverged in the marker grammar.

    ⚠️ EVERY FAILURE PATH RETURNS None, AND THAT IS DELIBERATE. This runs in the
    commit hook for every writer in the repository. A gate that refused a commit
    because a library moved, or because a container hides /proc, would stop all
    work to enforce a label -- far worse than the hole it closes. None means the
    family could not be established, and nothing is refused on silence.
    """

    # ⚠️ AN EMPTY OVERRIDE DERIVES; IT DOES NOT DISABLE. `HERMIT_COMMIT_MACHINE`
    # can only ever name a DIFFERENT machine, so an empty value there is
    # meaningless. Here an empty value would switch an identity check off from
    # the environment, which is a bypass wearing the shape of a test hook.
    override = (os.environ.get("HERMIT_COMMIT_MODEL_FAMILY") or "").strip().casefold()
    if override:
        return override
    try:
        import importlib.util

        root = Path(__file__).resolve().parent.parent
        lib = root / "ci-hub" / "lib"
        target = lib / "agent_identity.py"
        if not target.is_file():
            return None
        if str(lib) not in sys.path:
            sys.path.insert(0, str(lib))
        spec = importlib.util.spec_from_file_location("agent_identity", target)
        module = importlib.util.module_from_spec(spec)
        # ⚠️ REGISTERED BEFORE exec_module, AND THIS EXACT OMISSION MADE THE
        # CHECK INERT. `@dataclass` resolves annotations through
        # `sys.modules[cls.__module__]`, so a module executed without being
        # registered raises AttributeError on its first dataclass -- which the
        # total `except` below then swallowed into None, i.e. into "no family
        # could be established", i.e. into silence. Every mislabelled trailer
        # PASSED and the output was indistinguishable from having no violations.
        sys.modules.setdefault("agent_identity", module)
        spec.loader.exec_module(module)
        contexts, _error = module.process_ancestry(os.getpid(), Path("/proc"))
        _signal, family = module.client_family_signal(contexts)
        return family
    except Exception:  # noqa: BLE001 -- deliberately total; see the docstring
        return None


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


def first_body_line(message: str) -> tuple[int, str]:
    """Return the first stored body line and its zero-based line index.

    A blank line must separate the concise subject from the body.  Git editor
    comments are not stored in a commit, so they do not manufacture a body
    line while this function examines ``COMMIT_EDITMSG`` in the hook.
    """

    lines = message.splitlines()
    if not lines or not lines[0].strip():
        raise AttributionError(
            "commit message has no subject; expected first body line "
            "[<team>, <agent>, <model>, devbigNNN, role=<role>]"
        )
    if len(lines) < 2 or lines[1].strip():
        raise AttributionError(
            "commit subject must be followed by a blank line before "
            "[<team>, <agent>, <model>, devbigNNN, role=<role>]"
        )
    for index in range(2, len(lines)):
        line = lines[index]
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        return index, line
    raise AttributionError(
        "commit message has no body disclosure; expected first body line "
        "[<team>, <agent>, <model>, devbigNNN, role=<role>]"
    )


def _looks_like_canonical_disclosure(line: str) -> bool:
    """Whether ``line`` is trying to carry the five-field disclosure."""

    stripped = line.strip()
    return (
        stripped.startswith("[")
        and stripped.endswith("]")
        and (
            "role=" in stripped or ("] [" not in stripped and stripped.count(",") >= 4)
        )
    )


def _validate_machine_and_model(
    match: re.Match[str],
    expected_machine: str | None,
    expected_family: str | None,
) -> str:
    machine = match.group("machine")
    if expected_machine is not None and machine != expected_machine:
        raise AttributionError(
            f"commit claims machine {machine}, but this writer is {expected_machine}"
        )
    if expected_family is not None:
        model = match.group("model")
        claimed_family = declared_model_family(model)
        if claimed_family is not None and claimed_family != expected_family:
            raise AttributionError(
                f"commit claims model {model!r} ({claimed_family} family), but this "
                f"writer was launched by a {expected_family} client; the model field "
                "is derived from the launcher and must not be retyped"
            )
    return machine


def validate_message(
    message: str,
    expected_machine: str | None = None,
    expected_agent: str | None = None,
    expected_family: str | None = None,
) -> str:
    """Validate one message and return its exact machine token.

    ``expected_agent`` cross-checks the five-field agent or legacy ``Agent:``
    trailer against the writer's environment, exactly as ``expected_machine``
    does for the hostname.  A legacy message with no ``Agent:`` trailer remains
    accepted for incremental adoption; a five-field disclosure always makes an
    agent claim and is checked when the process identity is available.
    """

    trailer_agent = agent_from_message(message)
    if expected_agent is not None:
        if trailer_agent is not None and canonical_identity(
            trailer_agent
        ) != canonical_identity(expected_agent):
            raise AttributionError(
                f"commit claims agent {trailer_agent!r}, but this writer is "
                f"{expected_agent!r}; the Agent trailer is derived from "
                "DG_AGENT_NAME and must not be retyped"
            )

    lines = message.splitlines()
    body_error: AttributionError | None = None
    try:
        body_index, body_line = first_body_line(message)
    except AttributionError as error:
        body_error = error
        body_index, body_line = -1, ""
    canonical = CANONICAL_DISCLOSURE_RE.fullmatch(body_line)
    candidate_indexes = [
        index
        for index, line in enumerate(lines)
        if not line.lstrip().startswith("#") and _looks_like_canonical_disclosure(line)
    ]
    try:
        final_line = final_message_line(message)
    except AttributionError:
        final_line = ""
    legacy = TRAILER_RE.fullmatch(final_line)

    if canonical is not None:
        if candidate_indexes != [body_index]:
            raise AttributionError(
                "the canonical five-field disclosure must appear exactly once, "
                "as the first commit-body line"
            )
        role = canonical.group("role")
        if role not in CANONICAL_ROLES:
            raise AttributionError(
                "canonical disclosure role must be one of "
                f"{','.join(sorted(CANONICAL_ROLES))}; got {role!r}"
            )
        model = canonical.group("model")
        if model != model.strip():
            raise AttributionError(
                f"canonical disclosure model must not have edge whitespace: {model!r}"
            )
        claimed_agent = canonical.group("agent")
        if expected_agent is not None and canonical_identity(
            claimed_agent
        ) != canonical_identity(expected_agent):
            raise AttributionError(
                f"commit claims agent {claimed_agent!r}, but this writer is "
                f"{expected_agent!r}; the canonical agent field is derived from "
                "process identity and must not be retyped"
            )
        if trailer_agent is not None and canonical_identity(
            trailer_agent
        ) != canonical_identity(claimed_agent):
            raise AttributionError(
                f"canonical agent {claimed_agent!r} conflicts with legacy Agent "
                f"trailer {trailer_agent!r}"
            )

        machine = _validate_machine_and_model(
            canonical, expected_machine, expected_family
        )
        if legacy is not None and final_line != body_line:
            # Recent history contains this transitional hybrid.  Preserve it,
            # but never let the legacy footer contradict the canonical facts.
            legacy_machine = _validate_machine_and_model(
                legacy, expected_machine, expected_family
            )
            expected_legacy_role = LEGACY_ROLE_FOR_CANONICAL.get(role)
            if (
                legacy_machine != machine
                or legacy.group("role") != expected_legacy_role
            ):
                raise AttributionError(
                    "legacy final-line attribution conflicts with the canonical "
                    "five-field disclosure"
                )
        return machine

    if candidate_indexes:
        if body_index in candidate_indexes:
            detail = "is malformed"
        else:
            detail = "is not the first commit-body line"
        raise AttributionError(
            f"canonical five-field disclosure {detail}; expected exact "
            "[<team>, <agent>, <model>, devbigNNN, role=<role>]"
        )

    if legacy is not None:
        return _validate_machine_and_model(legacy, expected_machine, expected_family)
    if body_error is not None:
        raise body_error
    raise AttributionError(
        "first commit-body line must be the canonical disclosure "
        "[<team>, <agent>, <model>, devbigNNN, role=<role>] "
        "(or the final line must be the explicit legacy form "
        "[<role>, <model>] [<team>, devbigNNN]); "
        f"got first body line {body_line!r}"
    )


def validate_author_identity(name: str, email: str) -> None:
    """Refuse fabricated owner identities while permitting reserved fixtures."""

    if email == BOT_OSS_AUTHOR_EMAIL:
        if name != BOT_OSS_AUTHOR_NAME:
            raise AttributionError(
                f"bot author email <{email}> must use the exact visible name "
                f"{BOT_OSS_AUTHOR_NAME!r}; got {name!r}"
            )
        return
    if email == HUMAN_OSS_AUTHOR_EMAIL:
        return
    domain = email.rpartition("@")[2].casefold()
    if (
        domain in RESERVED_FIXTURE_DOMAINS
        and name.strip().casefold() not in OWNER_AUTHOR_NAMES
    ):
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
            "reject a canonical agent field or legacy Agent trailer that "
            "disagrees with this id; "
            "use 'auto' for DG_AGENT_NAME"
        ),
    )
    parser.add_argument("--detect-agent", action="store_true")
    # ⚠️ EXPLICIT, LIKE ITS TWO SIBLINGS, AND NOT IMPLICIT FROM THE ENVIRONMENT.
    # A family check that switched itself on whenever the process had a
    # recognisable launcher would make every unit test's verdict depend on WHO
    # RAN IT: this file's own fixtures carry both a codex trailer and a claude
    # one, so an implicit check passes for a codex agent and fails for a claude
    # one. Measured before this flag existed: 2 positive cells failed under a
    # claude runner and a third failed once the family was pinned the other way.
    parser.add_argument(
        "--expected-family",
        help="claude|codex, or 'auto' to derive it from this process's launcher",
    )
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)
    if args.detect_machine or args.detect_agent:
        if any((args.message_file, args.stdin, args.commit, args.commit_range)):
            parser.error(
                "--detect-machine/--detect-agent do not accept a message source"
            )
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
                for line in git(
                    args.repo, "rev-list", "--reverse", args.commit_range
                ).splitlines()
                if line
            ]
            records = messages_from_commits(args.repo, commits)
            # Historical commits are immutable evidence, not proposed writes.  Do
            # not retroactively reject legitimate collaborators while checking
            # their machine-bearing disclosures; the commit-msg path below guards
            # every new commit at creation time.
            author_records = []

        for identity, name, email in author_records:
            try:
                validate_author_identity(name, email)
            except AttributionError as error:
                raise AttributionError(f"{identity}: {error}") from error

        # ⚠️ THE FAMILY CHECK APPLIES TO A PROPOSED MESSAGE ONLY, AND SCOPING IT
        # ANYWHERE ELSE WOULD REFUSE HONEST WORK. `expected_family` is derived
        # from THIS process's ancestry, which is the writer only while the commit
        # is being created. Applied to a recorded commit or a range it would
        # compare a claude reviewer's launcher against a codex agent's commit
        # from last week and refuse it -- the same category error as checking a
        # hostname against somebody else's machine. `--expected-machine` and
        # `--expected-agent` are already passed `auto` only on the creation path
        # for exactly this reason; this makes the scoping structural rather than
        # dependent on how a caller spells its flags.
        proposed = args.message_file is not None or args.stdin
        expected_family = args.expected_family
        if expected_family == "auto":
            expected_family = detect_model_family()
        elif expected_family is not None:
            expected_family = expected_family.strip().casefold() or None
        if not proposed:
            expected_family = None

        machines: set[str] = set()
        for identity, message in records:
            try:
                machines.add(
                    validate_message(message, expected, expected_agent, expected_family)
                )
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
