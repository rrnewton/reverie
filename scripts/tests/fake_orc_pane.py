#!/usr/bin/env python3
"""An inert stand-in for the Orc coordinator TUI, for testing the relay.

WHY THIS EXISTS
---------------
Validating `scripts/orc-hermit-msg.py` against the real coordinator pane
delivers real text to a real human: a self-test message reached the owner during
this script's own development. There must therefore be a target that is
end-to-end real — a genuine tmux pane, a genuine terminal application consuming
genuine keystrokes — while being incapable of reaching anybody. This is that
target. It renders the post-1.0 Orc layout that the relay's detection keys on:

    [idle] Enter: submit • Esc: cancel • Ctrl+G: pause • Ctrl+T: inbox
    ------------------------------------------------------------------
    › <draft>
    ------------------------------------------------------------------
    Session: <name> (...)  |  Ctx: 0%
    Agents: total=0 busy=0  |  Workflows: 0
    DB: <db>  |  Tasks: 0/0  |  Ready: 0

It is deliberately NOT an Orc: it has no network, no model, no task database,
and no way to forward anything. Everything it receives goes to a local
transcript and to --record, and nowhere else.

Run it under a tmux pane via a symlink named `orc` so that tmux reports
`pane_current_command == orc`, which is what the relay's discovery requires:

    ln -s "$(command -v python3)" "$dir/orc"
    tmux -S "$sock" new-session -d "$dir/orc fake_orc_pane.py --db faketest ..."
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import termios
import tty
from pathlib import Path

RULE_WIDTH = 178
#: What the shipped build shows in an EMPTY input box. The relay treats it as
#: empty; see COMPOSER_EMPTY_PLACEHOLDERS in scripts/orc-hermit-msg.py.
PLACEHOLDER = "Type / for commands"
ENTER = "\r"
CTRL_J = "\n"
CTRL_U = "\x15"
BACKSPACE = ("\x7f", "\x08")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    # Named --db because that is the flag the relay parses out of
    # `pane_start_command` to decide which coordinator it is looking at.
    parser.add_argument("--db", default="faketest")
    parser.add_argument(
        "--layout",
        choices=("ascii", "box"),
        default="ascii",
        help=(
            "which composer rendering to draw. `box` is what the shipped build "
            "actually draws -- U+250C/U+2502/U+2514/U+2500 with the key hints in "
            "the box title and a `Type / for commands` placeholder when empty. "
            "`ascii` is the older hyphen-rule form. BOTH are rendered by the "
            "suite: this fixture drew only `ascii` while production drew `box`, "
            "so the relay's detection was broken in production against a green "
            "test suite for as long as nobody compared them."
        ),
    )
    parser.add_argument(
        "--seed-transcript",
        type=Path,
        default=None,
        help=(
            "Render this text as an already-consumed [user] turn before any "
            "input arrives. With --ignore-input this models the pane state a "
            "relay sees when Orc took the draft before the relay looked: an "
            "EMPTY composer with the message present in the transcript."
        ),
    )
    parser.add_argument("--session-name", default="faketest")
    parser.add_argument(
        "--state",
        default="idle",
        help="composer state marker to render ([idle], [streaming], ...)",
    )
    parser.add_argument(
        "--queue-submissions",
        action="store_true",
        help="render submissions as a [queued] N pending list instead of a "
        "[user] transcript block, the way Orc does mid-turn",
    )
    parser.add_argument(
        "--ignore-input",
        action="store_true",
        help="accept keystrokes from the terminal and discard them, "
        "reproducing a pane that tmux writes to successfully while the "
        "application consumes nothing",
    )
    parser.add_argument(
        "--record",
        type=Path,
        help="append each submitted message to this file as JSON lines, so a "
        "test can assert exactly what was delivered and how many times",
    )
    return parser.parse_args()


class FakeOrcPane:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.draft = ""
        self.transcript: list[str] = []
        if getattr(args, "seed_transcript", None) is not None:
            self.transcript.append(args.seed_transcript.read_text(encoding="utf-8"))
        self.pending: list[str] = []

    def render(self) -> str:
        lines = ["\x1b[2J\x1b[H"]
        lines.extend(f"[user]\n{entry}" for entry in self.transcript[-10:])
        if self.pending:
            lines.append(f"[queued] {len(self.pending)} pending")
            lines.extend(f"> {entry}" for entry in self.pending)
        if self.args.layout == "box":
            lines.extend(self._box_composer())
        else:
            lines.extend(self._ascii_composer())
        lines.append(f"Session: {self.args.session_name} (fake-inert)  |  Ctx: 0%")
        lines.append("Agents: total=0 busy=0  |  Workflows: 0")
        lines.append(f"DB: {self.args.db}  |  Tasks: 0/0  |  Ready: 0")
        return "\r\n".join(lines) + "\r\n"

    def _ascii_composer(self) -> list[str]:
        rule = "-" * RULE_WIDTH
        draft_lines = self.draft.split("\n") if self.draft else [""]
        return [
            f"[{self.args.state}] Enter: submit • Esc: cancel • "
            "Ctrl+G: pause • Ctrl+T: inbox",
            rule,
            f"› {draft_lines[0]}".rstrip(),
            *draft_lines[1:],
            rule,
        ]

    def _box_composer(self) -> list[str]:
        """The shipped build's rendering, copied from a live capture.

        Three differences from `ascii` and all three broke the relay: the rules
        are U+2500 rather than hyphens, every body line carries a U+2502 left
        border, and an EMPTY box shows a placeholder rather than a bare prompt
        glyph -- this build renders no prompt glyph at all. The key hints live in
        the box title here, not on a line above, so the readiness marker is not
        separately parseable; that is why the relay records state as evidence
        and never keys delivery on it.
        """
        title = (
            "\u250c Input (Enter, Esc, Ctrl+G pause, Ctrl+T inbox, "
            "Ctrl+C clear/quit, Ctrl+X quit all)"
        )
        body = self.draft.split("\n") if self.draft else [PLACEHOLDER]
        return [
            title + "\u2500" * RULE_WIDTH,
            *(f"\u2502  {line}".rstrip() for line in body),
            "\u2502",
            "\u2514" + "\u2500" * RULE_WIDTH,
        ]

    def submit(self) -> None:
        if self.args.queue_submissions:
            self.pending.append(self.draft)
        else:
            self.transcript.append(self.draft)
        if self.args.record is not None:
            with self.args.record.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps({"message": self.draft}) + "\n")
        self.draft = ""

    def feed(self, char: str) -> None:
        if char == ENTER:
            self.submit()
        elif char == CTRL_J:
            self.draft += "\n"
        elif char == CTRL_U:
            # Kill-line, the cleanup key the relay uses for a draft it injected
            # but will not submit. Escape is deliberately NOT a clear here
            # either: in the real TUI it cancels the coordinator's turn.
            self.draft = ""
        elif char in BACKSPACE:
            self.draft = self.draft[:-1]
        elif char >= " ":
            self.draft += char

    def run(self) -> None:
        sys.stdout.write(self.render())
        sys.stdout.flush()
        while True:
            char = sys.stdin.read(1)
            if not char:
                return
            if not self.args.ignore_input:
                self.feed(char)
            sys.stdout.write(self.render())
            sys.stdout.flush()


def main() -> int:
    args = parse_args()
    if args.record is not None:
        args.record.parent.mkdir(parents=True, exist_ok=True)
    pane = FakeOrcPane(args)
    fd = sys.stdin.fileno()
    if os.isatty(fd):
        saved = termios.tcgetattr(fd)
        try:
            tty.setraw(fd)
            pane.run()
        finally:
            termios.tcsetattr(fd, termios.TCSADRAIN, saved)
    else:
        pane.run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
