#!/usr/bin/env python3
"""Send a message to the Orc Hermit coordinator TUI.

Delivery is acknowledged, not assumed. ``tmux send-keys`` and ``paste-buffer``
exit 0 for any live pane whether or not the application consumed the keystrokes,
so their exit status is a proxy for delivery, not delivery. This script instead
observes the composer transition it caused:

    empty  --(inject)-->  non-empty  --(Enter)-->  empty

and, when the submitted text can be found again above the composer, the `[user]`
transcript echo that only appears once Orc has accepted the message. Exit status
is 0 only when that transition was observed; every other outcome logs
``status="failed"`` and exits nonzero.

Composer recognition is structural (the region between the two horizontal rules
at the bottom of the pane) rather than textual. Every text probe tried before
has been invalidated by an Orc build: the border title flipped twice, and the
post-1.0 TUI dropped both the ``orc (hermit)`` header and the
``Type / for commands`` placeholder that readiness used to key on, which
silently failed 12 consecutive relays over six hours.
"""

from __future__ import annotations

import argparse
import contextlib
import datetime
import fcntl
import hashlib
import json
import os
import re
import socket as pysocket
import http.client
import secrets
import shlex
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn, TextIO
from zoneinfo import ZoneInfo


UID = os.getuid()
DEFAULT_RUNTIME_DIR = Path(f"/run/user/{UID}")
DEFAULT_SOCKET = Path(f"/run/user/{UID}/orc-tmux/tmux-{UID}/default")
def _default_orc_command() -> Path:
    """Locate the orc executable used for session discovery.

    ⚠️ ONE HARDCODED PATH UNDER $HOME WAS WRONG FOR AT LEAST SIX DAYS. This was
    `Path.home() / "orc-bin/orc"`. Measured 2026-09-05 on devbig014:
    /home/newton/orc-bin/ exists and holds only `tg`; there is no orc in it. The
    running coordinator (pid 3180882) executes /usr/local/bin/orc, confirmed
    through /proc/<pid>/exe. So every invocation printed "orc executable does
    not exist ...; falling back to direct tmux scan" and ran degraded, and
    nothing reported that because a warning is not a failure.

    The local pin is still preferred when it is really there -- that is how this
    fleet pins an orc version, and $HOME/orc-bin is otherwise maintained. It is
    just no longer the only answer: when it is absent we take orc from PATH
    rather than degrading. DEV_HERMIT_ORC_COMMAND overrides both and is honoured
    verbatim, so an explicit choice that is wrong fails where it is named
    instead of silently resolving to something else.
    """
    override = os.environ.get("DEV_HERMIT_ORC_COMMAND")
    if override:
        return Path(override)
    pinned = Path.home() / "orc-bin/orc"
    if os.access(pinned, os.X_OK):
        return pinned
    found = shutil.which("orc")
    if found:
        return Path(found)
    # Nothing resolved. Return the pinned path anyway so the existing
    # "orc executable does not exist: <path>" message names a concrete path a
    # reader can act on, rather than an empty string.
    return pinned


DEFAULT_ORC_COMMAND = _default_orc_command()
# Compatibility fallback only. Live successors are derived from TG_DB_PATH or
# the selected/live Orc session instead of remaining pinned to this name.
DEFAULT_ORC_DB = "hermit"
DEFAULT_LOG_FILE = Path.home() / ".local/state/orc-hermit-msg.log"
TMUX_TIMEOUT_SECONDS = 5
# `orc tmux ls` can be slow under heavy fleet load (many concurrent agents).
# A tight timeout was the dominant dropped-tick cause, so allow more time and
# retry a few times before giving up; final failure falls back to a direct
# tmux-socket scan in main() rather than dropping the tick.
ORC_TIMEOUT_SECONDS = 30
ORC_LS_ATTEMPTS = 3
ORC_LS_RETRY_SLEEP = 2.0
# The composer briefly shows non-empty text while the coordinator is typing or
# a prior message is still rendering; retry instead of dropping the tick.
COMPOSER_ATTEMPTS = 6
COMPOSER_RETRY_SLEEP = 2.0
# The TUI repaints asynchronously: `tmux paste-buffer` returns as soon as the
# bytes are in the pane, and under fleet load the composer took several seconds
# to render them. Capturing once immediately after injecting therefore reads the
# PRE-paste screen and reports a false non-delivery, so both post-write checks
# poll instead of sampling once.
INJECT_ATTEMPTS = 20
INJECT_RETRY_SLEEP = 0.5
DRAIN_ATTEMPTS = 20
DRAIN_RETRY_SLEEP = 0.5
# How much scrollback to search for the submitted message echoed as a `[user]`
# transcript block. A long message can scroll its own opening off the visible
# screen, so the echo check must look above it.
TRANSCRIPT_SCROLLBACK_LINES = 200
# Longest slice of the message used to recognise it on screen. It must be long
# enough to tell THIS relay from the previous one: the hourly reminder sends a
# byte-identical file every tick, and the only thing distinguishing two of them
# is the Eastern-time stamp prefix_eastern_time() embeds ~50 characters in. A
# shorter probe would match the last hour's copy still on screen and report a
# corroborated delivery for a message that never arrived.
PROBE_LENGTH = 120
HTML_COMMENT_OPEN = "<!--"
HTML_COMMENT_CLOSE = "-->"
EASTERN_TIME_ZONE = ZoneInfo("America/New_York")
ACTIVE_SESSION_RE = re.compile(r"^(?P<name>[^:]+):\s+\d+\s+windows?\b")
ACTIVE_SESSION_HEADER_RE = re.compile(
    r"^Active sessions on this socket \((?P<count>\d+)\):$"
)
ACTIVE_SESSION_ENTRY_RE = re.compile(r"^  (?P<name>orc-\S+)\s*$")
# The composer is the region delimited by two horizontal rules at the bottom of
# the Orc TUI. This structural shape is the ONLY part of the composer that has
# survived every Orc build so far, which is why readiness keys on it. Text
# probes did not survive: `orc (hermit)` and the `Type / for commands`
# placeholder both vanished in the post-1.0 TUI (which shows a `Session:`/`DB:`
# footer and a bare prompt instead), and the border title had already flipped
# between builds before that. See the module docstring.
#
# ⚠️ AND THE GLYPHS CHANGED UNDER IT. The shape survived; the character set did
# not. This was `^-{20,}$` -- twenty or more ASCII HYPHENS and nothing else --
# and the shipped build draws the box with U+250C/U+2502/U+2514/U+2500. Captured
# from the live coordinator pane on 2026-09-05 and decoded byte by byte; the
# capture is checked in at scripts/tests/data/captured-orc-composer.txt and is
# what both the fixture and the parser test are built from, so neither can drift
# into agreeing with a rendering we invented.
#
# Not one ASCII hyphen in the box, so parse_composer returned None on every
# attempt and the relay refused with "does not render an Orc composer" --
# accurate about what it saw, misleading about what it meant. The owner-checkout
# validate report was undeliverable on that refusal.
#
# So match the RUN rather than the whole line: twenty or more rule characters in
# either character set, anywhere on the line. That tolerates the corner and box
# title the build now puts before the run and the neighbouring column's border
# after it, without widening what counts as a composer -- discovery has already
# restricted us to panes running `orc`.
COMPOSER_RULE_RE = re.compile(r"[-\u2500]{20,}")
# Body lines carry the box's left border in the current build and the right one
# lands on them too; older builds have neither.
COMPOSER_BORDER_CHARS = "\u2502|"
# U+203A is the post-1.0 empty-input prompt; '>' covers a plain-ASCII build. The
# current build renders no glyph at all, which is why this stays optional.
COMPOSER_PROMPT_CHARS = "›>"
# ⚠️ A TEXT PROBE, AND THIS FILE ALREADY RECORDS THAT TEXT PROBES FLIP BETWEEN
# BUILDS -- `Type / for commands` vanished in the post-1.0 TUI and has since come
# back. It is used anyway because this build renders NO prompt glyph, so there is
# no structural way left to tell an empty box from a draft.
#
# The DIRECTION of the risk is the justification: if this goes stale, an EMPTY
# box stops looking empty and the relay REFUSES rather than typing. Staleness
# costs a delivery, never somebody else's draft. Pinned by a test rather than
# left as a comment. Confirmed against the shipped binary: `strings
# /usr/local/bin/orc` contains this and the `Input (Enter, Esc, ...)` title.
COMPOSER_EMPTY_PLACEHOLDERS = frozenset({"Type / for commands"})
# `[idle]` / `[streaming]` / ... — the post-1.0 readiness marker, rendered on the
# key-hint line just above the composer. Recorded as evidence, never used as the
# delivery test: a state label is a proxy for acceptance, the composer
# transition is the acceptance itself.
COMPOSER_STATE_RE = re.compile(
    r"^\[(?P<state>[a-z][a-z0-9 _-]*)\]\s+Enter:\s*submit\b"
)
COMPOSER_STATE_SEARCH_LINES = 3
# Submitting while a turn is streaming does not drop the message: Orc parks it
# and renders `[queued] N pending` above the composer. Recorded so a reader can
# tell "the coordinator has read this" from "the coordinator will read this".
COMPOSER_QUEUE_RE = re.compile(r"^\[queued\]\s+(?P<pending>\d+)\s+pending\b")


class OrcMessageError(RuntimeError):
    """An expected failure while locating or writing to the Orc TUI."""


@dataclass(frozen=True)
class CoordinatorPane:
    session: str
    window: str
    pane_id: str
    orc_session_id: str | None = None


@dataclass(frozen=True)
class Composer:
    """The Orc input box as rendered on screen, parsed structurally.

    ``text`` is the draft currently in the box with the prompt glyph removed;
    ``state`` is the readiness marker (``idle``, ``streaming``, ...) when the
    build renders one, else None.
    """

    text: str
    state: str | None

    @property
    def is_empty(self) -> bool:
        """Empty means safe to type into, so the placeholder counts as empty.

        The placeholder stays in ``text`` rather than being normalised away, so
        the delivery record still shows what was on screen.
        """
        return not self.text or self.text in COMPOSER_EMPTY_PLACEHOLDERS


@dataclass(frozen=True)
class DeliveryAck:
    """What was actually observed about a submitted message.

    ``echo`` distinguishes the two strengths of evidence: a drained composer
    proves Orc consumed the draft, while finding the message again above the
    composer proves Orc took ownership of it — either rendered as a `[user]`
    transcript turn or parked in the `[queued]` list. ``pending`` is the queue
    depth when Orc reported one, so "delivered" never silently means "will be
    read at some point".

    ``echo`` is POSITIVE-ONLY evidence and is never allowed to gate the exit
    status. Messages observed arriving on the live pane were logged with
    ``echo=False`` because the render had not caught up inside the capture
    window, so treating its absence as non-delivery would invent failures.
    Absence of echo means "not corroborated", not "not delivered".
    """

    composer_state: str | None
    echo: bool
    pending: int | None
    #: Orc consumed the draft BEFORE this relay pressed Enter, so no Enter was
    #: sent. Evidence is the message found in the transcript with an empty
    #: composer -- the same evidence `submit_and_acknowledge` already trusts,
    #: read one step earlier. Recorded distinctly because "we submitted it" and
    #: "it was already taken" are different facts, and a reader must not have to
    #: infer which happened.
    consumed_without_submit: bool = False

    @property
    def evidence(self) -> str:
        if self.consumed_without_submit:
            return "consumed-before-submit+echo"
        return "composer-drained+echo" if self.echo else "composer-drained"


@dataclass(frozen=True)
class LivePane:
    """One live pane on the tmux socket, before any coordinator filtering."""

    session: str
    window: str
    pane_id: str
    command: str
    orc_db: str | None
    orc_session_id: str | None

    @property
    def is_orc(self) -> bool:
        return self.command == "orc"

    def render(self) -> str:
        return f"{self.session}:{self.window} ({self.pane_id}, {self.command})"


def fail(message: str) -> NoReturn:
    print(f"orc-hermit-msg: error: {message}", file=sys.stderr)
    raise SystemExit(1)


def orc_db_from_tg_path(value: str | None) -> str | None:
    """Return the TaskGraph database name carried by ``TG_DB_PATH``."""
    if not value:
        return None
    name = Path(value).name
    if name.endswith(".db"):
        name = name[:-3]
    return name or None


def orc_db_from_session_name(session: str | None) -> str | None:
    """Map the fleet's ``orc-<db>`` session identity back to its database."""
    if not session or not session.startswith("orc-"):
        return None
    name = session.removeprefix("orc-")
    return name or None


def configured_orc_db(
    explicit: str | None,
    tg_db_path: str | None,
    session_hint: str | None,
) -> tuple[str, str] | None:
    """Resolve caller-carried identity, preserving the explicit override."""
    if explicit is not None:
        return explicit, "explicit --orc-db"
    if derived := orc_db_from_tg_path(tg_db_path):
        return derived, "TG_DB_PATH"
    if derived := orc_db_from_session_name(session_hint):
        return derived, "--session"
    return None


def orc_db_from_coordinator_panes(panes: list["LivePane"]) -> tuple[str, str] | None:
    """Derive this Hermit relay's DB from what the coordinator is ACTUALLY running.

    WHY THIS EXISTS, AND WHY IT IS NOT KEYED ON THE SESSION NAME.
    `orc_db_from_active_sessions` below reads the database out of the tmux
    SESSION NAME, which assumes the fleet's `orc-<db>` convention. That
    convention no longer holds: the live coordinator's session is
    `orc-d0f81544`, a hex identity, so `d0f81544` does not start with "hermit",
    no candidate is eligible, and the legacy fallback returns "hermit" -- a
    database that has not existed since the coordinator moved to "hermit2".

    MEASURED 2026-08-24: the last acknowledged delivery on this channel was
    2026-08-12T13:27:03-07:00. Every one of the 279 attempts since then failed,
    181 of them with `no live Orc coordinator is running database 'hermit';
    candidates: orc-d0f81544:orc (%0, orc) db=hermit2`. THE ANSWER WAS INSIDE
    THE ERROR MESSAGE: the pane census already knows `db=hermit2`, because
    `LivePane.orc_db` is parsed from the coordinator's own `--db` argument. It
    was computed, printed, and then discarded in favour of a name from history.

    Deriving from the pane's own `--db` keeps both safety properties the
    session-name version was protecting: only the hermit family is eligible, so
    another product's coordinator on the same socket cannot be misdelivered to;
    and ambiguity is refused rather than guessed.
    """
    candidates = sorted(
        {
            pane.orc_db
            for pane in panes
            if pane.is_orc
            and pane.orc_db
            and pane.orc_db.startswith(DEFAULT_ORC_DB)
        }
    )
    if len(candidates) == 1:
        return candidates[0], "live coordinator pane --db"
    if len(candidates) > 1:
        raise OrcMessageError(
            "cannot derive one Hermit database from live coordinator panes: "
            + ", ".join(candidates)
            + "; pass --orc-db or --session explicitly"
        )
    return None


def orc_db_from_active_sessions(active_sessions: list[str]) -> tuple[str, str]:
    """Derive this Hermit relay's DB from live Orc session metadata.

    Other products may have coordinators on the same tmux socket. Selecting
    one merely because it is the only coordinator would misdeliver Hermit
    status to it, so only the ``orc-hermit*`` family is eligible. Ambiguity is
    refused rather than guessed.

    Retained as the second attempt, after `orc_db_from_coordinator_panes`: it
    is correct whenever the `orc-<db>` session convention does hold, and it
    needs only session names.
    """
    candidates = sorted(
        {
            derived
            for session in active_sessions
            if (derived := orc_db_from_session_name(session)) is not None
            and derived.startswith(DEFAULT_ORC_DB)
        }
    )
    if len(candidates) == 1:
        return candidates[0], "live Orc session metadata"
    if len(candidates) > 1:
        raise OrcMessageError(
            "cannot derive one Hermit database from live Orc sessions: "
            + ", ".join(f"orc-{candidate}" for candidate in candidates)
            + "; pass --orc-db or --session explicitly"
        )
    return DEFAULT_ORC_DB, "legacy fallback"


def run_tmux(
    socket: Path,
    *args: str,
    input_text: str | None = None,
) -> str:
    command = ["tmux", "-S", str(socket), *args]
    try:
        result = subprocess.run(
            command,
            input=input_text,
            capture_output=True,
            text=True,
            timeout=TMUX_TIMEOUT_SECONDS,
        )
    except FileNotFoundError as exc:
        raise OrcMessageError("tmux is not installed or is not on PATH") from exc
    except subprocess.TimeoutExpired as exc:
        raise OrcMessageError(
            f"tmux timed out after {TMUX_TIMEOUT_SECONDS}s: {' '.join(args)}"
        ) from exc

    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "unknown tmux error"
        raise OrcMessageError(f"tmux {' '.join(args)} failed: {detail}")
    return result.stdout


def resolve_socket(socket: Path, *, allow_fallback: bool = True) -> Path:
    """Return a live tmux socket, tolerating a relocated orc-tmux dir.

    The default path is deterministic from the UID, but if orc restarts into a
    differently-named socket the hourly tick must still find it, so a missing
    DEFAULT socket falls back to the most-recently-modified one under the
    orc-tmux runtime dir.

    An EXPLICIT ``--socket`` never falls back. Searching for a substitute is
    only ever right for "wherever orc happens to be listening"; applied to a
    socket the caller named, it silently retargets the message to a different
    server. That is not hypothetical: a test aimed at a deliberately
    nonexistent socket path was rewritten to the live coordinator and delivered
    a self-test message to the owner. A named destination that is absent is a
    refusal, not an invitation to pick another one.
    """
    if socket.exists():
        return socket
    if not allow_fallback:
        raise OrcMessageError(
            f"tmux socket does not exist: {socket}. Refusing to look for another "
            "socket because --socket named this one explicitly; a message must go "
            "to the server the caller asked for or to none at all. Omit --socket "
            "to use the default and allow discovery of a relocated orc socket"
        )
    orc_tmux_dir = DEFAULT_RUNTIME_DIR / "orc-tmux"
    candidates = [
        path
        for path in orc_tmux_dir.glob(f"tmux-{UID}/*")
        if path.is_socket()
    ]
    if not candidates:
        candidates = [
            path for path in orc_tmux_dir.rglob("*") if path.is_socket()
        ]
    if not candidates:
        raise OrcMessageError(f"tmux socket does not exist: {socket}")
    return max(candidates, key=lambda path: path.stat().st_mtime)


def parse_active_orc_sessions(output: str) -> list[str]:
    legacy_sessions = [
        match.group("name")
        for line in output.splitlines()
        if (match := ACTIVE_SESSION_RE.match(line)) is not None
    ]
    if legacy_sessions:
        return legacy_sessions

    lines = output.splitlines()
    for index, line in enumerate(lines):
        header = ACTIVE_SESSION_HEADER_RE.match(line)
        if header is None:
            continue
        sessions = [
            match.group("name")
            for candidate in lines[index + 1 :]
            if (match := ACTIVE_SESSION_ENTRY_RE.match(candidate)) is not None
        ]
        expected_count = int(header.group("count"))
        if len(sessions) != expected_count:
            raise OrcMessageError(
                "orc tmux ls session count mismatch: "
                f"reported {expected_count}, parsed {len(sessions)}"
            )
        return sessions
    return []


def list_active_orc_sessions(orc_command: Path) -> list[str]:
    command = [str(orc_command), "tmux", "ls"]
    environment = os.environ.copy()
    environment["XDG_RUNTIME_DIR"] = str(DEFAULT_RUNTIME_DIR)
    last_error: OrcMessageError | None = None
    for attempt in range(ORC_LS_ATTEMPTS):
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                env=environment,
                text=True,
                timeout=ORC_TIMEOUT_SECONDS,
            )
        except FileNotFoundError as exc:
            # A missing executable will not become present on retry.
            raise OrcMessageError(
                f"orc executable does not exist: {orc_command}"
            ) from exc
        except subprocess.TimeoutExpired as exc:
            last_error = OrcMessageError(
                f"orc tmux ls timed out after {ORC_TIMEOUT_SECONDS}s"
            )
            last_error.__cause__ = exc
        else:
            if result.returncode != 0:
                detail = (
                    result.stderr.strip()
                    or result.stdout.strip()
                    or "unknown orc error"
                )
                last_error = OrcMessageError(f"orc tmux ls failed: {detail}")
            else:
                sessions = parse_active_orc_sessions(result.stdout)
                if not sessions:
                    last_error = OrcMessageError(
                        "orc tmux ls reported no active sessions"
                    )
                elif len(sessions) != len(set(sessions)):
                    raise OrcMessageError(
                        "orc tmux ls reported duplicate session names"
                    )
                else:
                    return sessions
        if attempt + 1 < ORC_LS_ATTEMPTS:
            time.sleep(ORC_LS_RETRY_SLEEP)
    assert last_error is not None
    raise last_error


def scan_orc_sessions_via_tmux(socket: Path) -> list[str]:
    """Locate active orc sessions directly on the tmux socket.

    Fallback for when the `orc` CLI is too slow to answer under fleet load: any
    session with a live pane whose current command is `orc` is an active orc
    session. This keeps the hourly tick flowing even if `orc tmux ls` times out.
    """
    panes = run_tmux(
        socket,
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\t#{pane_dead}\t#{pane_current_command}",
    )
    sessions: list[str] = []
    for line in panes.splitlines():
        fields = line.split("\t", 2)
        if len(fields) != 3:
            continue
        session, pane_dead, current_command = fields
        if pane_dead == "0" and Path(current_command).name == "orc":
            if session not in sessions:
                sessions.append(session)
    if not sessions:
        raise OrcMessageError(
            "no live orc coordinator pane found on the tmux socket"
        )
    return sessions


def orc_db_from_start_command(command: str) -> str | None:
    if len(command) >= 2 and command[0] == command[-1] == '"':
        command = command[1:-1]
    try:
        tokens = shlex.split(command)
    except ValueError:
        return None

    for index, token in enumerate(tokens):
        if token == "--db" and index + 1 < len(tokens):
            return tokens[index + 1]
        if token.startswith("--db="):
            return token.removeprefix("--db=")
    return None


def orc_session_id_from_start_command(command: str) -> str | None:
    """Read Orc's exact API/session identity from the coordinator command."""
    if len(command) >= 2 and command[0] == command[-1] == '"':
        command = command[1:-1]
    try:
        tokens = shlex.split(command)
    except ValueError:
        return None
    for index, token in enumerate(tokens):
        if token == "--session-id" and index + 1 < len(tokens):
            return tokens[index + 1]
        if token.startswith("--session-id="):
            return token.removeprefix("--session-id=")
    return None


def parse_live_panes(panes: str, active_sessions: list[str]) -> list[LivePane]:
    """Every live pane in an active orc session, with no coordinator filtering.

    Kept separate from coordinator selection so a failure can report what was
    actually on the socket. A count reported without the panes it was computed
    from cannot distinguish "no coordinator exists" from "a hint excluded it".
    """
    active = set(active_sessions)
    live: list[LivePane] = []
    for line in panes.splitlines():
        fields = line.split("\t", 5)
        if len(fields) != 6:
            continue
        session, window, pane_id, pane_dead, current_command, start_command = fields
        if session not in active or pane_dead != "0":
            continue
        live.append(
            LivePane(
                session=session,
                window=window,
                pane_id=pane_id,
                command=Path(current_command).name,
                orc_db=orc_db_from_start_command(start_command),
                orc_session_id=orc_session_id_from_start_command(start_command),
            )
        )
    return live


def list_live_panes(socket: Path, active_sessions: list[str]) -> list[LivePane]:
    """The live pane census for a socket. One place, so every caller agrees."""
    panes = run_tmux(
        socket,
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\t#{window_name}\t#{pane_id}\t#{pane_dead}\t"
        "#{pane_current_command}\t#{pane_start_command}",
    )
    return parse_live_panes(panes, active_sessions)


def _render_panes(panes: list[LivePane]) -> str:
    return ", ".join(pane.render() for pane in panes) or "none"


def _hint_mismatch_detail(
    orc_panes: list[LivePane],
    live_panes: list[LivePane],
    session_hint: str | None,
    window_hint: str | None,
) -> str:
    """Explain why the hints excluded every coordinator.

    The common mistake is aiming this coordinator-only relay at an *agent*
    pane's window (`--window hermit-perf`). The window filter and the
    "current command is orc" test then intersect to nothing, which used to
    surface as the badly misleading "found 0 (coordinators: none)".
    """
    detail = (
        f"session/window hints excluded every coordinator "
        f"(session_hint={session_hint!r}, window_hint={window_hint!r}); "
        f"live coordinators on this socket: {_render_panes(orc_panes)}"
    )
    if window_hint is None:
        return detail
    named = [
        pane
        for pane in live_panes
        if pane.window == window_hint
        and (session_hint is None or pane.session == session_hint)
    ]
    if not named:
        return f"{detail}; no live pane has window {window_hint!r}"
    if any(pane.is_orc for pane in named):
        return detail
    return (
        f"{detail}; window {window_hint!r} is {_render_panes(named)}, an agent "
        "pane rather than the coordinator. This script only messages the Orc "
        "coordinator TUI; drop --window (or pass the coordinator's window) and "
        "ask the coordinator to relay, or use a TaskGraph note for agent-to-"
        "agent handoff"
    )


def find_coordinator_pane(
    socket: Path,
    active_sessions: list[str],
    orc_db: str,
    *,
    session_hint: str | None = None,
    window_hint: str | None = None,
) -> CoordinatorPane:
    if session_hint is not None and session_hint not in active_sessions:
        raise OrcMessageError(
            f"requested session {session_hint!r} is not present in orc tmux ls"
        )

    live_panes = list_live_panes(socket, active_sessions)
    orc_panes = [pane for pane in live_panes if pane.is_orc]
    hinted = [
        pane
        for pane in orc_panes
        if (session_hint is None or pane.session == session_hint)
        and (window_hint is None or pane.window == window_hint)
    ]

    matching = [pane for pane in hinted if pane.orc_db == orc_db]
    if not matching:
        expected_session = f"orc-{orc_db}"
        matching = [
            pane
            for pane in hinted
            if pane.orc_db is None and pane.session == expected_session
        ]

    if len(matching) == 1:
        selected = matching[0]
        return CoordinatorPane(
            session=selected.session,
            window=selected.window,
            pane_id=selected.pane_id,
            orc_session_id=selected.orc_session_id,
        )

    # Report the count together with the condition that produced it. Each
    # branch below is a materially different operator action, so they must not
    # collapse into one message.
    if not orc_panes:
        raise OrcMessageError(
            f"no live Orc coordinator pane on {socket}: no pane in "
            f"{active_sessions} is running `orc`. Live panes: "
            f"{_render_panes(live_panes)}. The coordinator is probably "
            "restarting; retry shortly"
        )
    if not hinted:
        raise OrcMessageError(
            _hint_mismatch_detail(orc_panes, live_panes, session_hint, window_hint)
        )
    if not matching:
        raise OrcMessageError(
            f"no live Orc coordinator is running database {orc_db!r}; "
            f"candidates: "
            + ", ".join(
                f"{pane.render()} db={pane.orc_db or 'unknown'}" for pane in hinted
            )
        )
    raise OrcMessageError(
        f"expected exactly one live Orc coordinator for database {orc_db!r}; "
        f"found {len(matching)}: {_render_panes(matching)}. Disambiguate with "
        "--session/--window"
    )


def squash(text: str) -> str:
    """Drop all whitespace so a probe survives the pane's own line wrapping.

    tmux wraps at the pane width with no continuation marker and can split
    mid-word, so neither a raw substring test nor whitespace-collapsing is
    reliable. Removing whitespace entirely from both sides matches either way.
    """
    return re.sub(r"\s+", "", text)


def message_probe(message: str) -> str:
    """A short distinctive slice of the message, used to recognise it on screen."""
    return squash(message)[:PROBE_LENGTH]


def _strip_composer_borders(line: str) -> str:
    """One body line with the box borders and prompt glyph removed.

    Exactly ONE border character comes off each end, and only where one is
    present. Stripping greedily would let a draft made only of border characters
    reduce to the empty string, which is the one way this could turn an occupied
    composer into an empty-looking one and license typing into it.
    """
    text = line.strip()
    if text[:1] in COMPOSER_BORDER_CHARS:
        text = text[1:]
    if text[-1:] in COMPOSER_BORDER_CHARS:
        text = text[:-1]
    return text.strip().lstrip(COMPOSER_PROMPT_CHARS).strip()


def parse_composer(screen: str) -> Composer | None:
    """Parse the input box out of a captured pane, or None if it is not there.

    The composer is the region between the last two horizontal rules; the Orc
    footer (``Session:``/``Agents:``/``DB:``) sits below the closing rule and
    contains no rules of its own. Returning None means "this pane does not look
    like an Orc composer at all", which is a different operator action from
    "the composer is busy", so the two must not collapse into one result.
    """
    lines = [line.rstrip() for line in screen.splitlines()]
    rules = [
        index for index, line in enumerate(lines) if COMPOSER_RULE_RE.search(line)
    ]
    if len(rules) < 2:
        return None
    open_index, close_index = rules[-2], rules[-1]

    body: list[str] = []
    for line in lines[open_index + 1 : close_index]:
        body.append(_strip_composer_borders(line))

    state: str | None = None
    for line in reversed(lines[max(0, open_index - COMPOSER_STATE_SEARCH_LINES) : open_index]):
        match = COMPOSER_STATE_RE.match(line.strip())
        if match is not None:
            state = match.group("state")
            break

    return Composer(text="\n".join(body).strip(), state=state)


def parse_pending(screen: str) -> int | None:
    """Depth of Orc's pending-message queue, when it renders one."""
    for line in reversed(screen.splitlines()):
        match = COMPOSER_QUEUE_RE.match(line.strip())
        if match is not None:
            return int(match.group("pending"))
    return None


def capture_pane(socket: Path, pane_id: str, *, scrollback: int = 0) -> str:
    args = ["capture-pane", "-p"]
    if scrollback:
        args += ["-S", f"-{scrollback}"]
    return run_tmux(socket, *args, "-t", pane_id)


def _pane_tail(screen: str, lines: int = 12) -> str:
    return " / ".join(line.strip() for line in screen.splitlines()[-lines:] if line.strip())


def wait_for_ready_composer(socket: Path, pane_id: str) -> Composer:
    """Block until the composer is present and empty.

    Emptiness is the load-bearing precondition: it is what makes it safe to
    type, because injecting into an occupied box would corrupt somebody else's
    draft and submit the concatenation. Readiness deliberately does NOT require
    the `[idle]` marker — requiring an idle coordinator is what silently dropped
    17 relays in one day on a busy fleet, and the composer accepts input while a
    turn is streaming. The observed state travels with the result instead.
    """
    screen = ""
    composer: Composer | None = None
    for attempt in range(COMPOSER_ATTEMPTS):
        screen = capture_pane(socket, pane_id)
        composer = parse_composer(screen)
        if composer is not None and composer.is_empty:
            return composer
        if attempt + 1 < COMPOSER_ATTEMPTS:
            time.sleep(COMPOSER_RETRY_SLEEP)

    if composer is None:
        raise OrcMessageError(
            f"pane {pane_id} does not render an Orc composer: found no input box "
            f"between horizontal rules after {COMPOSER_ATTEMPTS} attempts. This is "
            "not a busy coordinator — the pane layout is unrecognised, so nothing "
            f"was typed. Pane tail: {_pane_tail(screen)}"
        )
    raise OrcMessageError(
        f"Orc composer on {pane_id} still holds an unsent draft after "
        f"{COMPOSER_ATTEMPTS} attempts (state={composer.state or 'unknown'}); "
        "refusing to type into it and concatenate with somebody else's message. "
        f"Draft: {composer.text[:200]!r}"
    )


def inject_message(socket: Path, pane_id: str, message: str) -> None:
    """Type the message into the Orc composer WITHOUT submitting it.

    The Orc TUI composer does not enable terminal bracketed-paste mode, so
    ``tmux paste-buffer -p`` is silently dropped and never reaches the input box
    — the send appears to succeed while nothing arrives. Deliver by injecting
    the text as typed input instead: paste each line UNBRACKETED via a tmux
    buffer (safe for arbitrary content, including lines that start with '-',
    which ``send-keys -l`` misparses as flags), and separate lines with Ctrl+J,
    which the composer inserts as a newline (Enter submits).
    """
    token = secrets.token_hex(4)
    for index, line in enumerate(message.split("\n")):
        if index > 0:
            # Ctrl+J inserts a newline in the composer without submitting.
            run_tmux(socket, "send-keys", "-t", pane_id, "C-j")
        if not line:
            continue
        buffer_name = f"orc-hermit-msg-{os.getpid()}-{token}-{index}"
        run_tmux(socket, "load-buffer", "-b", buffer_name, "-", input_text=line)
        try:
            # No -p: an unbracketed paste is injected as if typed. -d removes
            # the buffer afterwards.
            run_tmux(
                socket,
                "paste-buffer",
                "-d",
                "-b",
                buffer_name,
                "-t",
                pane_id,
            )
        except OrcMessageError:
            with contextlib.suppress(OrcMessageError):
                run_tmux(socket, "delete-buffer", "-b", buffer_name)
            raise


def clear_composer(socket: Path, pane_id: str) -> bool:
    """Best-effort: wipe a draft we injected but are not going to submit.

    Only ever called on a composer this process found EMPTY beforehand, so
    whatever is in it now is ours to remove. Without this a refused relay wedges
    every later one, because the next run correctly refuses to type into an
    occupied box.

    Ctrl+U (kill-line), verified against the live coordinator, not Escape: the
    key hints bind Esc to "cancel", which aborts the coordinator's in-flight
    turn — a far worse side effect than a leftover draft.
    """
    with contextlib.suppress(OrcMessageError):
        run_tmux(socket, "send-keys", "-t", pane_id, "C-u")
        time.sleep(INJECT_RETRY_SLEEP)
        composer = parse_composer(capture_pane(socket, pane_id))
        return composer is not None and composer.is_empty
    return False


#: What `verify_injected` observed. The distinction is load-bearing: one of
#: these still needs Enter, the other must NOT get one.
INJECTED_IN_COMPOSER = "in-composer"
INJECTED_ALREADY_CONSUMED = "already-consumed"


def verify_injected(socket: Path, pane_id: str, probe: str) -> str:
    """Confirm the keystrokes reached the input box, before pressing Enter.

    This is the check that turns a blind rc=0 into a real result: every tmux
    call above exits 0 for a pane that ignored the input entirely, so without it
    a total non-delivery is indistinguishable from a successful one.

    Either signal is sufficient, because each independently shows the box took
    the text: the composer is no longer empty, or the message is visible on
    screen. Both are needed as alternatives because a draft long enough to
    scroll the box can hide the message's opening, while a draft containing a
    horizontal rule of its own can confuse the structural parse.
    """
    screen = ""
    for attempt in range(INJECT_ATTEMPTS):
        screen = capture_pane(socket, pane_id)
        composer = parse_composer(screen)
        if (composer is not None and not composer.is_empty) or probe in squash(screen):
            return INJECTED_IN_COMPOSER
        if attempt + 1 < INJECT_ATTEMPTS:
            time.sleep(INJECT_RETRY_SLEEP)

    # AN EMPTY COMPOSER IS AMBIGUOUS, and reading it as failure is how a relay
    # delivers TWICE. It means either the keystrokes never landed, or they
    # landed and Orc already consumed the draft before this loop looked --
    # exactly what happens when the coordinator is idle and takes the turn
    # immediately. The visible screen cannot tell those apart once the turn has
    # scrolled the message out of view, so before declaring non-delivery, look
    # where `submit_and_acknowledge` already looks for consumption evidence: the
    # scrollback. Finding the message there with an empty composer is Orc having
    # taken ownership, which is delivery -- and pressing Enter or reporting
    # failure at that point would either inject a stray newline into the
    # coordinator's turn or make the caller send the whole message again.
    transcript = capture_pane(socket, pane_id, scrollback=TRANSCRIPT_SCROLLBACK_LINES)
    if probe in squash(transcript):
        return INJECTED_ALREADY_CONSUMED

    cleared = clear_composer(socket, pane_id)
    raise OrcMessageError(
        f"the message did not reach the Orc composer on {pane_id} within "
        f"{INJECT_ATTEMPTS * INJECT_RETRY_SLEEP:.0f}s: the input box is still empty "
        "after typing it, so tmux accepted the keystrokes but the TUI did not. NOT "
        "submitting — nothing was delivered and no Enter was sent. "
        + (
            "Composer cleared for the next relay."
            if cleared
            else "WARNING: could not clear the composer; a partial draft may remain "
            "and will block the next relay until removed."
        )
        + f" Pane tail: {_pane_tail(screen)}"
    )


#: Where to send the reader when a submission is not consumed. Kept as one
#: constant rather than inlined so it reads contiguously in the source and a test
#: can assert on it -- the previous wording said the composer "did not drain" and
#: told the reader to clear it, which points at a component that is working.
NOT_THE_COMPOSER = (
    "THE COMPOSER IS NOT THE SUSPECT -- it was found, it accepted the keystrokes, "
    "and it echoed them back; what did not happen is Orc consuming them. Check "
    "whether the coordinator is answering at all: `orc session socket <id>` and "
    "`orc curl --list --json` fail the same way when it is saturated or wedged, "
    "and a direct `curl --unix-socket <that socket> http://localhost/api/identity` "
    "times out with zero bytes."
)


def _clear_our_own_draft(
    socket: Path, pane_id: str, probe: str, composer: Composer | None
) -> str:
    """Remove the draft THIS relay typed, once submitting it has failed.

    ⚠️ WHY THIS EXISTS. Landing the composer-detection fix without it did real
    damage on 2026-09-05: the relay found the box for the first time, typed the
    validate report into the owner's live coordinator, failed to submit, and left
    it sitting there. The next firing then refused on the occupied-composer guard
    -- correctly -- so the box stayed blocked until a human cleared it, and each
    time it was cleared the next firing stranded another.

    ⚠️ AND WHY IT CANNOT DESTROY SOMEBODY ELSE'S DRAFT. It clears only when the
    remaining text still contains this relay's own probe, which is a slice of the
    message it just injected and verified. Anything else is left exactly where it
    is and said so in the returned text. The clear is then CHECKED rather than
    assumed, because Ctrl+U is itself a keystroke this build might have
    repurposed, and reporting a cleanup that did not happen is worse than
    reporting none.
    """
    if composer is None or probe not in squash(composer.text):
        return (
            "The input box was left untouched: what it holds is not what this "
            "relay typed, so removing it was not this relay's call."
        )
    with contextlib.suppress(OrcMessageError):
        run_tmux(socket, "send-keys", "-t", pane_id, "C-u")
        time.sleep(DRAIN_RETRY_SLEEP)
        after = parse_composer(capture_pane(socket, pane_id))
        if after is not None and after.is_empty:
            return (
                "The undelivered draft was this relay's own and has been removed, "
                "so the box is free for the next attempt and nothing was stranded."
            )
    return (
        "Tried to remove this relay's own undelivered draft and the box did not "
        "clear, so it is still there and the next relay will refuse on it."
    )


def submit_and_acknowledge(
    socket: Path, pane_id: str, probe: str, state: str | None
) -> DeliveryAck:
    """Press Enter once, then confirm Orc consumed the draft.

    Enter is sent exactly once and never retried: a resend after an ambiguous
    outcome is how a relay delivers twice. If the composer does not drain we
    report failure and leave the draft in place rather than guessing.
    """
    run_tmux(socket, "send-keys", "-t", pane_id, "Enter")

    screen = ""
    composer: Composer | None = None
    for attempt in range(DRAIN_ATTEMPTS):
        screen = capture_pane(socket, pane_id)
        composer = parse_composer(screen)
        if composer is not None and composer.is_empty:
            # The composer is empty, so any remaining match is the message
            # rendered back by Orc — as a `[user]` transcript turn if it was
            # taken up immediately, or as a `[queued]` entry if a turn was still
            # streaming. Either way Orc owns it now, which is the thing a
            # cleared input box alone does not prove.
            transcript = capture_pane(
                socket, pane_id, scrollback=TRANSCRIPT_SCROLLBACK_LINES
            )
            return DeliveryAck(
                composer_state=state,
                echo=probe in squash(transcript),
                pending=parse_pending(screen),
            )
        if attempt + 1 < DRAIN_ATTEMPTS:
            time.sleep(DRAIN_RETRY_SLEEP)

    # ⚠️ NAME THE CONDITION, NOT THE SYMPTOM. This message used to say the
    # composer "did not drain" and to tell the reader to clear it, which sends
    # them after a composer that is working perfectly. Measured 2026-09-05: the
    # box was found, the text went in, Enter was sent, and Orc simply did not
    # act -- the same coordinator was failing an identity probe on its own
    # control socket (`orc session socket` reported "indexed but its Unix socket
    # is not live", a direct curl timed out after 5s with 0 bytes) while running
    # at 441% CPU. Both delivery channels were refused by one condition: Orc was
    # not accepting. The composer was never the suspect.
    remaining = "composer no longer parses" if composer is None else repr(composer.text[:200])
    cleared = _clear_our_own_draft(socket, pane_id, probe, composer)
    raise OrcMessageError(
        f"Orc did not accept the submission on {pane_id}: Enter was sent and the "
        f"input box still held the message {DRAIN_ATTEMPTS * DRAIN_RETRY_SLEEP:.0f}s "
        f"later, so it was NOT delivered ({remaining}). {NOT_THE_COMPOSER} Not "
        f"resending: a retry here is how a relay delivers twice. {cleared} "
        f"Pane tail: {_pane_tail(screen)}"
    )


# ------------------------------------------------- supported API path -----
#
# The tmux path infers delivery from what a TUI drew. Every failure mode in this
# file's history follows from that: keystrokes tmux accepted but the app did
# not, an empty composer that could mean either "never arrived" or "already
# taken", a layout the parser stopped recognising. On 2026-08-07 the coordinator
# pane stopped rendering a composer entirely and the hourly wake could not be
# delivered at all, correctly refusing rather than typing blind.
#
# Orc exposes a supported alternative: an authenticated per-session Unix socket
# speaking HTTP. `POST /api/message` is the ingestion route (verified read-only:
# GET returns 405 with `allow: POST`), and its RESPONSE is the acknowledgement.
# That is a typed reply from the process that will consume the message, not a
# picture of a terminal, so there is no rendering to misread.
API_MESSAGE_PATH = "/api/message"
API_TIMEOUT_SECONDS = 15.0
#: Response fields that carry Orc's own identifier for the accepted message.
API_ACK_ID_KEYS = ("message_id", "id", "request_id", "queue_id")


class _UnixHTTPConnection(http.client.HTTPConnection):
    """HTTPConnection over a Unix socket. stdlib only, so tests can bind a real
    receiver on a temp socket instead of mocking the transport away."""

    def __init__(self, unix_socket: str, timeout: float = API_TIMEOUT_SECONDS):
        super().__init__("localhost", timeout=timeout)
        self._unix_socket = unix_socket

    def connect(self) -> None:
        sock = pysocket.socket(pysocket.AF_UNIX, pysocket.SOCK_STREAM)
        sock.settimeout(self.timeout)
        sock.connect(self._unix_socket)
        self.sock = sock


@dataclass(frozen=True)
class ApiAck:
    """Orc's own acknowledgement of one submitted message."""

    status: int
    ack_id: str | None
    body: str

    @property
    def evidence(self) -> str:
        return f"api-accepted:{self.ack_id}" if self.ack_id else "api-accepted"


def deliver_via_api(api_socket: Path, message: str) -> ApiAck:
    """POST the message once and require an acknowledgement.

    Sent EXACTLY ONCE and never retried. A retry after an ambiguous outcome is
    how a relay delivers twice, and an HTTP request whose response was lost may
    still have been processed -- the same rule the Enter key follows on the tmux
    path, for the same reason.
    """
    if not api_socket.exists():
        raise OrcMessageError(
            f"no Orc receiver at {api_socket}: the session socket does not exist, so there "
            "is nothing to accept the message. NOT falling back to typing -- a missing "
            "receiver is a refusal, not a reason to guess."
        )
    payload = json.dumps({"message": message}).encode("utf-8")
    connection = _UnixHTTPConnection(str(api_socket))
    try:
        connection.request(
            "POST",
            API_MESSAGE_PATH,
            body=payload,
            headers={"Content-Type": "application/json", "Content-Length": str(len(payload))},
        )
        response = connection.getresponse()
        status = response.status
        body = response.read().decode("utf-8", errors="replace")
    except OSError as error:
        raise OrcMessageError(
            f"the Orc receiver at {api_socket} did not answer: {error}. Nothing is resent: "
            "an unanswered POST may still have been processed, and a retry here is how a "
            "relay delivers twice."
        ) from error
    finally:
        with contextlib.suppress(Exception):
            connection.close()

    if not 200 <= status < 300:
        raise OrcMessageError(
            f"the Orc receiver at {api_socket} refused the message with HTTP {status}: "
            f"{body[:300]!r}. Nothing was delivered."
        )
    return ApiAck(status=status, ack_id=_api_ack_id(body), body=body)


def _api_ack_id(body: str) -> str | None:
    """Orc's identifier for the accepted message, when it supplies one.

    Absence is NOT failure: a 2xx is already the acknowledgement. The id is
    recorded when present so an outcome can be dereferenced later rather than
    merely asserted.
    """
    try:
        parsed = json.loads(body)
    except (ValueError, TypeError):
        return None
    if not isinstance(parsed, dict):
        return None
    for container in (parsed, parsed.get("data"), parsed.get("result")):
        if isinstance(container, dict):
            for key in API_ACK_ID_KEYS:
                value = container.get(key)
                if isinstance(value, (str, int)) and str(value):
                    return str(value)
    return None


def resolve_api_socket(
    explicit: Path | None,
    orc_command: Path,
    orc_db: str,
    session_id: str | None = None,
) -> Path | None:
    """Locate the coordinator's exact session socket, or None when absent.

    Current Orc identifies sessions by UUID and exposes ``uds_socket_path``;
    older builds used the database name plus ``uds_socket``. Prefer the exact
    parent-session identity carried by ``DG_TMUX_SESSION_ID`` and retain the
    old schema only as a compatibility fallback.
    """
    if explicit is not None:
        return explicit
    try:
        output = subprocess.run(
            [str(orc_command), "curl", "--list", "--json"],
            capture_output=True, text=True, timeout=API_TIMEOUT_SECONDS, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if output.returncode != 0:
        return None
    try:
        entries = json.loads(output.stdout or "[]")
    except ValueError:
        return None
    if not isinstance(entries, list):
        return None
    if session_id:
        matching = [
            entry
            for entry in entries
            if isinstance(entry, dict) and entry.get("session_id") == session_id
        ]
        if len(matching) == 1:
            candidate = (
                matching[0].get("uds_socket_path")
                or matching[0].get("uds_socket")
                or matching[0].get("UDS_SOCKET")
            )
            return Path(candidate) if candidate else None
        # A current-schema registry that knows session identities must never
        # guess another session. An old registry with no such field may still
        # use the database-name compatibility path below.
        if any(isinstance(entry, dict) and "session_id" in entry for entry in entries):
            return None
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        if orc_db and entry.get("name") not in (orc_db, None):
            continue
        candidate = (
            entry.get("uds_socket_path")
            or entry.get("uds_socket")
            or entry.get("UDS_SOCKET")
        )
        if candidate:
            return Path(candidate)
    return None


#: Outcomes of asking whether the coordinator is answering at all, before any
#: keystroke is sent. Three states, not two: "could not ask" is its own answer
#: and must never be recorded as "asked and it was fine".
COORDINATOR_ANSWERING = "answering"
COORDINATOR_NOT_ANSWERING = "not-answering"
COORDINATOR_NOT_PROBED = "not-probed"
#: `orc session socket` performs Orc's own identity probe and fails when the
#: session is indexed but not live, so its exit status IS the readiness check.
#: Short, because a saturated coordinator answers slowly or not at all and this
#: runs before every delivery.
READINESS_TIMEOUT_SECONDS = 8.0


def coordinator_answer_state(
    orc_command: Path | None, orc_session_id: str | None
) -> tuple[str, str]:
    """Is the coordinator answering its own control socket?

    ⚠️ WHY THIS RUNS BEFORE TYPING. Measured 2026-09-05: the composer was found,
    took the keystrokes and echoed them back, Enter was sent, and Orc never
    consumed it -- the draft was still in the box sixteen minutes later. The same
    coordinator was failing an identity probe on its own socket while running at
    441% CPU for three days: saturated rather than hung, so it kept rendering and
    every visible surface looked healthy. Both delivery channels were refused by
    that one condition.

    Typing into a coordinator in that state cannot succeed and strands the text
    in somebody's input box, which is exactly what happened. So ask first, and
    refuse before touching the pane.

    ⚠️ AND "COULD NOT ASK" IS NOT "ASKED AND IT WAS FINE". Where the probe cannot
    run -- no orc executable, no session id, the probe itself timing out -- this
    returns COORDINATOR_NOT_PROBED and the caller announces it rather than
    proceeding silently. It still proceeds: refusing everywhere orc is not
    installed at a known path would disable the relay far more widely than the
    condition it guards, and the take-back in _clear_our_own_draft already bounds
    the damage. What it must never do is read as a pass.
    """
    if orc_command is None or not os.access(orc_command, os.X_OK):
        return COORDINATOR_NOT_PROBED, f"no usable orc executable at {orc_command}"
    if not orc_session_id:
        return COORDINATOR_NOT_PROBED, "the coordinator's Orc session id is unknown"
    try:
        result = subprocess.run(
            [str(orc_command), "session", "socket", orc_session_id],
            capture_output=True, text=True,
            timeout=READINESS_TIMEOUT_SECONDS, check=False,
        )
    except (OSError, subprocess.SubprocessError) as error:
        return COORDINATOR_NOT_PROBED, f"the readiness check could not run: {error}"
    if result.returncode == 0 and result.stdout.strip():
        return COORDINATOR_ANSWERING, result.stdout.strip()
    detail = (result.stderr.strip() or result.stdout.strip() or "no output")
    return COORDINATOR_NOT_ANSWERING, detail.replace("\n", " ")[:400]


def send_message(
    socket: Path,
    pane_id: str,
    message: str,
    *,
    orc_command: Path | None = None,
    orc_session_id: str | None = None,
) -> DeliveryAck:
    """Deliver the message and return what was observed about its acceptance."""
    readiness, readiness_detail = coordinator_answer_state(orc_command, orc_session_id)
    if readiness == COORDINATOR_NOT_ANSWERING:
        raise OrcMessageError(
            f"the coordinator behind {pane_id} is not answering its own control "
            f"socket, so nothing was typed: {readiness_detail}. A message typed "
            "into a coordinator in this state is not delivered and is left "
            f"sitting in the input box. {NOT_THE_COMPOSER}"
        )
    if readiness == COORDINATOR_NOT_PROBED:
        print(
            "orc-hermit-msg: warning: could not check whether the coordinator is "
            f"answering ({readiness_detail}); proceeding, and this is NOT evidence "
            "that it is healthy.",
            file=sys.stderr,
        )
    composer = wait_for_ready_composer(socket, pane_id)
    probe = message_probe(message)
    inject_message(socket, pane_id, message)
    if verify_injected(socket, pane_id, probe) == INJECTED_ALREADY_CONSUMED:
        # Enter is deliberately NOT sent. There is nothing in the box to submit,
        # and a stray Enter lands in whatever the coordinator is doing next.
        return DeliveryAck(
            composer_state=composer.state,
            echo=True,
            pending=parse_pending(capture_pane(socket, pane_id)),
            consumed_without_submit=True,
        )
    return submit_and_acknowledge(socket, pane_id, probe, composer.state)


def validate_message(message: str) -> None:
    if not message or not message.strip():
        raise OrcMessageError("message must not be empty")
    controls = sorted(
        {
            ord(char)
            for char in message
            if (ord(char) < 32 and char not in "\n\t") or ord(char) == 127
        }
    )
    if controls:
        rendered = ", ".join(f"0x{code:02x}" for code in controls)
        raise OrcMessageError(
            f"message contains unsupported control characters ({rendered})"
        )


def strip_html_comments(text: str) -> str:
    """Remove complete HTML comment blocks while preserving other text."""
    output: list[str] = []
    offset = 0
    while offset < len(text):
        opening = text.find(HTML_COMMENT_OPEN, offset)
        closing = text.find(HTML_COMMENT_CLOSE, offset)
        if closing != -1 and (opening == -1 or closing < opening):
            raise OrcMessageError("message file contains an unmatched '-->'")
        if opening == -1:
            output.append(text[offset:])
            break
        output.append(text[offset:opening])
        closing = text.find(HTML_COMMENT_CLOSE, opening + len(HTML_COMMENT_OPEN))
        if closing == -1:
            raise OrcMessageError("message file contains an unterminated '<!--' comment")
        offset = closing + len(HTML_COMMENT_CLOSE)
    return "".join(output)


def load_message_file(path: Path) -> str:
    try:
        contents = path.read_text(encoding="utf-8")
    except OSError as exc:
        raise OrcMessageError(f"could not read message file {path}: {exc}") from exc
    except UnicodeError as exc:
        raise OrcMessageError(f"message file is not valid UTF-8: {path}") from exc
    return strip_html_comments(contents).strip()


def persist_recoverable_message(log_path: Path, message: str) -> Path:
    """Persist the exact attempted body before any delivery side effect.

    The relay log used to retain only a SHA-256 for direct messages. Once the
    caller transcript disappeared, that proved a refusal happened but could
    not recover what was refused. Every real dispatch now has a content-
    addressed, mode-0600 body beside the log; both success and failure records
    point at it.
    """
    encoded = message.encode("utf-8")
    digest = hashlib.sha256(encoded).hexdigest()
    directory = log_path.parent / f"{log_path.name}.messages"
    target = directory / f"{digest}.txt"
    try:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        if target.exists():
            if target.read_bytes() != encoded:
                raise OrcMessageError(
                    f"recoverable message hash collision or corrupt file: {target}"
                )
            return target

        temporary = directory / f".{digest}.{os.getpid()}.{secrets.token_hex(6)}.tmp"
        try:
            with temporary.open("xb") as output:
                os.fchmod(output.fileno(), 0o600)
                output.write(encoded)
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, target)
            directory_fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)
        finally:
            with contextlib.suppress(FileNotFoundError):
                temporary.unlink()
    except OrcMessageError:
        raise
    except OSError as exc:
        raise OrcMessageError(
            f"could not persist recoverable message before dispatch: {exc}"
        ) from exc
    return target


def prefix_eastern_time(message: str, now: datetime.datetime | None = None) -> str:
    current = now or datetime.datetime.now(tz=EASTERN_TIME_ZONE)
    eastern = current.astimezone(EASTERN_TIME_ZONE)
    timestamp = eastern.strftime("%Y-%m-%d %H:%M:%S %Z")
    return (
        "We're working in eastern time and the current date/time is "
        f"`{timestamp}`, use that to orient yourself around day/night and any "
        f"potential future deadlines.  {message}"
    )


def log_delivery(
    path: Path,
    status: str,
    *,
    session: str,
    window: str,
    pane_id: str | None,
    message: str | None,
    message_file: Path | None,
    recoverable_message_file: Path | None = None,
    database: str | None = None,
    database_source: str | None = None,
    error: str | None = None,
    ack: DeliveryAck | None = None,
    transport: str = "tmux",
    api_ack: "ApiAck | None" = None,
) -> None:
    # A bare "sent" does not say what was verified, and this log is the only
    # durable record of a relay. Every `sent` therefore carries the evidence it
    # rests on and the composer state it was submitted from; every `failed`
    # carries the stage that refused, so a reader can tell "nothing was typed"
    # from "typed but never accepted" without re-deriving it from the message.
    record: dict[str, object] = {
        "timestamp": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
        "status": status,
        "target": f"{session}:{window}",
    }
    record["transport"] = transport
    if api_ack is not None:
        # The endpoint's own reply, so an outcome can be dereferenced later
        # rather than asserted: status is the acknowledgement, ack_id names it.
        record["ack"] = api_ack.evidence
        record["api_status"] = api_ack.status
        if api_ack.ack_id:
            record["ack_id"] = api_ack.ack_id

    if ack is not None:
        record["ack"] = ack.evidence
        record["echo"] = ack.echo
        record["composer_state"] = ack.composer_state or "unknown"
        if ack.pending is not None:
            record["pending"] = ack.pending
    if pane_id is not None:
        record["pane"] = pane_id
    if message_file is not None:
        record["message_file"] = str(message_file)
    if recoverable_message_file is not None:
        record["recoverable_message_file"] = str(recoverable_message_file)
    if database is not None:
        record["database"] = database
    if database_source is not None:
        record["database_source"] = database_source
    if message is not None:
        encoded = message.encode("utf-8")
        record["message_bytes"] = len(encoded)
        record["message_sha256"] = hashlib.sha256(encoded).hexdigest()
    if error is not None:
        record["error"] = error

    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as log_file:
        fcntl.flock(log_file, fcntl.LOCK_EX)
        log_file.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
        log_file.flush()
        os.fsync(log_file.fileno())


def locked(lock_path: Path) -> TextIO:
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    lock_file = lock_path.open("w")
    fcntl.flock(lock_file, fcntl.LOCK_EX)
    return lock_file


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--transport",
        choices=("tmux", "api", "auto"),
        default="tmux",
        help=(
            "How to reach the coordinator. `api` POSTs to Orc's supported "
            "/api/message endpoint over its authenticated session socket and "
            "binds success to the HTTP acknowledgement. `tmux` types into the "
            "composer (the historical path). `auto` prefers api when a receiver "
            "resolves and falls back to tmux otherwise -- never both, so a "
            "message is submitted exactly once. Default stays tmux until the "
            "api path has one live validation; see the task note."
        ),
    )
    parser.add_argument(
        "--api-socket",
        type=Path,
        default=None,
        help="Explicit Orc session socket. Skips discovery; used by the inert tests.",
    )
    parser.add_argument("message", nargs="?", help="message to submit")
    parser.add_argument(
        "--message-file",
        type=Path,
        help="read a UTF-8 message, remove HTML comments, and prefix Eastern time",
    )
    parser.add_argument(
        "--socket",
        type=Path,
        default=None,
        help=f"tmux server socket (default: {DEFAULT_SOCKET}). When given "
        "explicitly it is used verbatim: a missing socket is an error rather "
        "than a cue to search for another server to deliver to",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="resolve the coordinator and check the composer is ready, then "
        "stop. Nothing is typed and no Enter is sent, so detection can be "
        "validated without delivering anything",
    )
    parser.add_argument(
        "--orc-command",
        type=Path,
        default=DEFAULT_ORC_COMMAND,
        help=f"orc executable used for session discovery (default: {DEFAULT_ORC_COMMAND})",
    )
    parser.add_argument(
        "--log-file",
        type=Path,
        default=DEFAULT_LOG_FILE,
        help=f"append delivery results as JSON lines (default: {DEFAULT_LOG_FILE})",
    )
    parser.add_argument("--orc-db", default=None, help=argparse.SUPPRESS)
    # Disambiguators for the rare multi-coordinator case only. They select
    # among panes that are already running `orc`; they cannot address an agent
    # pane, and pointing them at one is the documented way to get a confusing
    # zero-match failure. Kept visible so that misuse is discouraged up front
    # rather than only explained after it fails.
    parser.add_argument(
        "--session",
        help="restrict coordinator discovery to this Orc session (an Orc "
        "coordinator session, not an agent pane)",
    )
    parser.add_argument(
        "--window",
        help="restrict coordinator discovery to this window of the Orc "
        "coordinator session. This is NOT how you message another agent: "
        "agent windows run claude/codex, never `orc`, so naming one matches "
        "no coordinator. Use a TaskGraph note for agent-to-agent handoff",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="mark this invocation as a self-test. Refuses before sending "
        "anything unless --socket names an inert fixture server, so a test "
        "cannot reach a human. Two probes have already leaked to the owner",
    )
    args = parser.parse_args()
    if args.message_file is not None and args.message is not None:
        parser.error("message and --message-file are mutually exclusive")
    if args.message_file is None and args.message is None and not args.dry_run:
        parser.error("provide a message or use --message-file")
    if args.self_test:
        refusal = self_test_destination_refusal(args.socket)
        if refusal:
            parser.error(refusal)
    return args


LIVE_SOCKET_DIR = DEFAULT_RUNTIME_DIR / "orc-tmux"


def self_test_destination_refusal(socket: "Path | None") -> str | None:
    """Reason a self-test must not run against this socket, or None if inert.

    A self-test that can reach a person is a production side effect. This has
    happened twice: `RELAY SELF-TEST from hermit-w12...` and a socket-fallback
    probe both landed in front of the owner. So the destination is checked
    BEFORE anything is composed or typed, and the check is structural rather
    than a convention someone has to remember.

    Inert means: an explicitly named socket that is not the live coordinator
    server and does not live in the runtime directory where real orc servers
    are published. Everything else refuses.

    Note what is deliberately NOT accepted as sufficient: --dry-run. Dry-run
    only promises not to type, and it still resolves and touches the live
    coordinator pane. A self-test should be incapable of delivery by
    construction, not merely well-behaved on the happy path -- the earlier leak
    came precisely from a probe that was expected not to deliver.
    """
    if socket is None:
        return (
            "--self-test requires an explicit --socket naming an inert fixture "
            "server. Without one the default socket is the LIVE coordinator, "
            "and a self-test that reaches a human is a production side effect "
            f"(default: {DEFAULT_SOCKET})"
        )
    resolved = Path(socket)
    if resolved == DEFAULT_SOCKET:
        return (
            f"--self-test refuses the live coordinator socket {resolved}. Name a "
            "fixture server instead; a self-test must be structurally incapable "
            "of delivering to a person"
        )
    try:
        inside_live_dir = LIVE_SOCKET_DIR in resolved.parents
    except (OSError, ValueError):
        inside_live_dir = False
    if inside_live_dir:
        return (
            f"--self-test refuses {resolved}: it is under {LIVE_SOCKET_DIR}, where "
            "real orc servers are published, so it may be a live user-facing "
            "destination. Use a fixture socket outside that directory"
        )
    return None


def main() -> int:
    args = parse_args()
    message: str | None = None
    recoverable_message_file: Path | None = None
    pane_id: str | None = None
    database_source: str | None = None
    active_sessions: list[str] | None = None
    socket: Path | None = None
    session = args.session or "db=unresolved"
    window = args.window or "coordinator"
    submitted = False
    discovered_coordinator: CoordinatorPane | None = None
    try:
        if args.message_file is not None:
            message = prefix_eastern_time(load_message_file(args.message_file))
        else:
            message = args.message
        if message is not None:
            if not args.dry_run:
                recoverable_message_file = persist_recoverable_message(
                    args.log_file, message
                )
            validate_message(message)

        selection = configured_orc_db(
            args.orc_db, os.environ.get("TG_DB_PATH"), args.session
        )
        if selection is None:
            # Scheduled user services do not inherit an agent's TG_DB_PATH.
            # Read the live socket directly instead of spending up to 90s on
            # `orc tmux ls` retries before reaching this same census.
            socket = resolve_socket(
                args.socket if args.socket is not None else DEFAULT_SOCKET,
                allow_fallback=args.socket is None,
            )
            active_sessions = scan_orc_sessions_via_tmux(socket)
            # Ask what the coordinator is RUNNING before asking what its tmux
            # session is CALLED. The session name is a convention that has
            # already drifted once; `--db` on the running process has not.
            selection = orc_db_from_coordinator_panes(
                list_live_panes(socket, active_sessions)
            )
            if selection is None:
                selection = orc_db_from_active_sessions(active_sessions)
        args.orc_db, database_source = selection
        session = args.session or f"db={args.orc_db}"

        # ---- supported API path, chosen before any tmux work ----
        if args.transport in ("api", "auto"):
            api_session_id = os.environ.get("DG_TMUX_SESSION_ID")
            if (
                not api_session_id
                and socket is not None
                and active_sessions is not None
            ):
                discovered_coordinator = find_coordinator_pane(
                    socket,
                    active_sessions,
                    args.orc_db,
                    session_hint=args.session,
                    window_hint=args.window,
                )
                api_session_id = discovered_coordinator.orc_session_id
            api_socket = resolve_api_socket(
                args.api_socket,
                args.orc_command,
                args.orc_db,
                api_session_id,
            )
            if api_socket is None:
                if args.transport == "api":
                    raise OrcMessageError(
                        "no Orc session socket resolved and --transport api was requested; "
                        "nothing was delivered. Pass --api-socket, or use --transport auto "
                        "to fall back to the tmux composer."
                    )
            else:
                if args.dry_run:
                    print(
                        f"orc-hermit-msg: dry-run: receiver={api_socket} "
                        f"transport=api endpoint={API_MESSAGE_PATH} (nothing sent)"
                    )
                    return 0
                if message is None:
                    raise OrcMessageError("no message to deliver")
                ack = deliver_via_api(api_socket, message)
                log_delivery(
                    args.log_file,
                    "sent",
                    session=session,
                    window=window,
                    pane_id=None,
                    message=message,
                    message_file=args.message_file,
                    recoverable_message_file=recoverable_message_file,
                    database=args.orc_db,
                    database_source=database_source,
                    transport="api",
                    api_ack=ack,
                )
                print(
                    f"orc-hermit-msg: delivered via {API_MESSAGE_PATH} to {api_socket} "
                    f"[ack={ack.evidence} http={ack.status}]"
                )
                return 0

        if socket is None:
            socket = resolve_socket(
                args.socket if args.socket is not None else DEFAULT_SOCKET,
                allow_fallback=args.socket is None,
            )
        lock_path = socket.parent / f".{args.orc_db}-msg.lock"
        with locked(lock_path):
            if active_sessions is None:
                try:
                    active_sessions = list_active_orc_sessions(args.orc_command)
                except OrcMessageError as discovery_error:
                    # Do not drop the tick when the orc CLI is merely slow: locate
                    # the coordinator directly on the tmux socket instead.
                    print(
                        f"orc-hermit-msg: warning: {discovery_error}; "
                        "falling back to direct tmux scan",
                        file=sys.stderr,
                    )
                    active_sessions = scan_orc_sessions_via_tmux(socket)
            coordinator = discovered_coordinator or find_coordinator_pane(
                socket,
                active_sessions,
                args.orc_db,
                session_hint=args.session,
                window_hint=args.window,
            )
            session = coordinator.session
            window = coordinator.window
            pane_id = coordinator.pane_id
            if args.dry_run:
                # Detection only. Nothing is typed and no Enter is sent, so a
                # readiness regression can be caught without putting a single
                # character in front of a human.
                composer = wait_for_ready_composer(socket, pane_id)
                log_delivery(
                    args.log_file,
                    "dry-run",
                    session=session,
                    window=window,
                    pane_id=pane_id,
                    message=None,
                    message_file=None,
                    database=args.orc_db,
                    database_source=database_source,
                )
                # ⚠️ A READY COMPOSER IS NOT A DELIVERABLE COORDINATOR, and a
                # dry-run that says only "ready" reads as a pass for something
                # it never checked. Measured 2026-09-05: the composer was ready
                # and Orc was not answering its own control socket, so a real
                # send would refuse. Both facts are reported.
                readiness, readiness_detail = coordinator_answer_state(
                    args.orc_command, coordinator.orc_session_id
                )
                print(
                    f"orc-hermit-msg: dry-run: {session}:{window} ({pane_id}) "
                    f"composer ready [state={composer.state or 'unknown'}], "
                    f"coordinator {readiness} ({readiness_detail[:200]}); "
                    "nothing was typed or sent"
                )
                return 0 if readiness != COORDINATOR_NOT_ANSWERING else 1
            # send_message returns only if the composer was observed to take the
            # message and then drain. Anything else raises, so there is no path
            # that logs "sent" or exits 0 on a message that was not accepted.
            ack = send_message(
                socket,
                pane_id,
                message,
                orc_command=args.orc_command,
                orc_session_id=coordinator.orc_session_id,
            )
            submitted = True
            log_delivery(
                args.log_file,
                "sent",
                session=session,
                window=window,
                pane_id=pane_id,
                message=message,
                message_file=args.message_file,
                recoverable_message_file=recoverable_message_file,
                database=args.orc_db,
                database_source=database_source,
                ack=ack,
            )
    except (OrcMessageError, OSError) as exc:
        if not submitted:
            with contextlib.suppress(OSError):
                log_delivery(
                    args.log_file,
                    "failed",
                    session=session,
                    window=window,
                    pane_id=pane_id,
                    message=message,
                    message_file=args.message_file,
                    recoverable_message_file=recoverable_message_file,
                    database=args.orc_db,
                    database_source=database_source,
                    error=str(exc),
                )
        recovery = (
            f"; recoverable message: {recoverable_message_file}"
            if recoverable_message_file is not None
            else ""
        )
        fail(f"{exc}{recovery}")

    print(
        f"orc-hermit-msg: sent to {session}:{window} ({pane_id}) "
        f"[ack={ack.evidence} composer_state={ack.composer_state or 'unknown'}]"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
