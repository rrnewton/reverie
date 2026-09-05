#!/usr/bin/env python3
"""Coordinator-pane discovery tests for ``scripts/orc-hermit-msg.py``.

The bug these cover: a relay aimed at an *agent* pane (``--window
hermit-perf``) failed with "expected exactly one live Orc coordinator for
database 'hermit'; found 0 (coordinators: none)". That message reports a count
without the condition that produced it, so it reads as "the coordinator has
vanished" when it means "your window hint excluded the coordinator" — and it
was read that way, producing a bogus "pane-detect finds 0 coordinators"
outage report.

Every discovery outcome is bracketed on both sides: the qualifying case must
select the coordinator, and the disqualifying case must be refused with a
message that names *which* condition refused it.
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import os
import socket
import threading
import datetime
import importlib.util
import json
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "orc-hermit-msg.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("orc_hermit_msg", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    # dataclasses resolves the defining module out of sys.modules, so the entry
    # must exist before exec_module runs.
    sys.modules["orc_hermit_msg"] = module
    spec.loader.exec_module(module)
    return module


ohm = _load_module()


@contextlib.contextmanager
def _patched(module, **attrs):
    """Temporarily replace module attributes, restoring them on exit.

    Used to stand in for tmux so the delivery branches can be driven directly.
    Nothing here touches a socket, a pane, or a live coordinator.
    """
    saved = {name: getattr(module, name) for name in attrs}
    for name, value in attrs.items():
        setattr(module, name, value)
    try:
        yield
    finally:
        for name, value in saved.items():
            setattr(module, name, value)

ORC_START = (
    '"if [ -f \'/home/test/.orc/tmux.conf\' ]; then tmux source-file '
    "'/home/test/.orc/tmux.conf'; fi; exec '/home/test/orc-bin/orc' "
    "'--db' 'hermit' '--resume' '--tui'\""
)
AGENT_START = (
    '"cd /home/test/work/dev-hermit && exec env AGENT=orc claude '
    '--permission-mode acceptEdits"'
)


def pane_line(session, window, pane_id, command, start, dead="0"):
    return "\t".join([session, window, pane_id, dead, command, start])


# The live socket shape this fleet actually runs: one `orc` coordinator pane
# plus one window per agent, all inside a single session.
LIVE_SOCKET = "\n".join(
    [
        pane_line("orc-hermit", "orc", "%0", "orc", ORC_START),
        pane_line("orc-hermit", "hermit-coord", "%1", "codex", AGENT_START),
        pane_line("orc-hermit", "hermit-perf", "%2", "claude", AGENT_START),
    ]
)


class FakeTmux:
    """Stands in for run_tmux so discovery can be tested without a server."""

    def __init__(self, panes: str):
        self.panes = panes

    def __call__(self, socket, *args, input_text=None):
        if args and args[0] == "list-panes":
            return self.panes
        raise AssertionError(f"unexpected tmux call: {args}")


class DiscoveryTestCase(unittest.TestCase):
    def setUp(self):
        self._real_run_tmux = ohm.run_tmux
        self.addCleanup(setattr, ohm, "run_tmux", self._real_run_tmux)

    def find(self, panes=LIVE_SOCKET, sessions=("orc-hermit",), db="hermit", **hints):
        ohm.run_tmux = FakeTmux(panes)
        return ohm.find_coordinator_pane(
            Path("/nonexistent.sock"), list(sessions), db, **hints
        )

    def find_error(self, **kwargs):
        with self.assertRaises(ohm.OrcMessageError) as caught:
            self.find(**kwargs)
        return str(caught.exception)


class TestCoordinatorSelection(DiscoveryTestCase):
    """Positive bracket: the coordinator is found in the normal shapes."""

    def test_finds_the_single_coordinator_among_agent_panes(self):
        pane = self.find()
        self.assertEqual(
            (pane.session, pane.window, pane.pane_id), ("orc-hermit", "orc", "%0")
        )

    def test_coordinator_window_hint_still_selects_it(self):
        pane = self.find(window_hint="orc")
        self.assertEqual(pane.pane_id, "%0")

    def test_session_hint_matching_the_coordinator_selects_it(self):
        pane = self.find(session_hint="orc-hermit")
        self.assertEqual(pane.pane_id, "%0")

    def test_coordinator_without_a_db_flag_falls_back_to_session_name(self):
        panes = pane_line(
            "orc-hermit", "orc", "%0", "orc", '"exec /home/test/orc-bin/orc --tui"'
        )
        self.assertEqual(self.find(panes=panes).pane_id, "%0")


class TestAgentWindowHintIsDiagnosed(DiscoveryTestCase):
    """The regression this file exists for.

    Aiming the coordinator relay at an agent window must fail, and must say so
    in terms of the hint — never as a bare "found 0 (coordinators: none)".
    """

    def test_agent_window_hint_names_the_window_and_its_command(self):
        message = self.find_error(window_hint="hermit-perf")
        self.assertIn("hermit-perf", message)
        self.assertIn("claude", message)
        self.assertIn("agent", message)

    def test_agent_window_hint_still_reports_the_reachable_coordinator(self):
        # The operator's next action is "drop the hint", so the message has to
        # show that a coordinator *was* available.
        message = self.find_error(window_hint="hermit-perf")
        self.assertIn("orc-hermit:orc", message)

    def test_agent_window_hint_does_not_claim_there_are_no_coordinators(self):
        message = self.find_error(window_hint="hermit-perf")
        self.assertNotIn("coordinators: none", message)
        self.assertNotIn("no live Orc coordinator pane", message)

    def test_unknown_window_hint_says_no_pane_has_that_window(self):
        message = self.find_error(window_hint="nonesuch")
        self.assertIn("no live pane has window 'nonesuch'", message)


class TestGenuinelyAbsentCoordinator(DiscoveryTestCase):
    """Negative bracket: a real zero must stay distinguishable from a hint miss."""

    def test_no_orc_pane_reports_the_socket_and_suggests_a_retry(self):
        panes = pane_line("orc-hermit", "hermit-perf", "%2", "claude", AGENT_START)
        message = self.find_error(panes=panes)
        self.assertIn("no live Orc coordinator pane", message)
        self.assertIn("retry", message)
        # It must still show what *was* live, so the reader can tell a
        # restarting coordinator from an empty socket.
        self.assertIn("hermit-perf", message)

    def test_a_dead_orc_pane_does_not_count_as_a_coordinator(self):
        panes = pane_line("orc-hermit", "orc", "%0", "orc", ORC_START, dead="1")
        self.assertIn("no live Orc coordinator pane", self.find_error(panes=panes))

    def test_a_pane_outside_the_active_sessions_is_ignored(self):
        message = self.find_error(sessions=("orc-other",))
        self.assertIn("no live Orc coordinator pane", message)


class TestDatabaseAndAmbiguity(DiscoveryTestCase):
    def test_wrong_database_is_reported_as_a_database_mismatch(self):
        message = self.find_error(db="other")
        self.assertIn("running database 'other'", message)
        self.assertIn("db=hermit", message)

    def test_two_coordinators_for_one_database_ask_for_disambiguation(self):
        panes = "\n".join(
            [
                pane_line("orc-hermit", "orc", "%0", "orc", ORC_START),
                pane_line("orc-hermit", "orc-two", "%9", "orc", ORC_START),
            ]
        )
        message = self.find_error(panes=panes)
        self.assertIn("found 2", message)
        self.assertIn("--session/--window", message)

    def test_session_hint_outside_active_sessions_is_refused_early(self):
        message = self.find_error(session_hint="orc-nope")
        self.assertIn("is not present in orc tmux ls", message)


class TestStartCommandDbParsing(unittest.TestCase):
    def test_parses_the_live_quoted_orc_start_command(self):
        self.assertEqual(ohm.orc_db_from_start_command(ORC_START), "hermit")

    def test_parses_the_equals_form(self):
        self.assertEqual(
            ohm.orc_db_from_start_command('"exec orc --db=hermit --tui"'), "hermit"
        )

    def test_agent_start_command_has_no_db(self):
        self.assertIsNone(ohm.orc_db_from_start_command(AGENT_START))

    def test_unparseable_start_command_is_not_an_exception(self):
        self.assertIsNone(ohm.orc_db_from_start_command('"exec orc --db \'unclosed'))

    def test_parses_exact_orc_session_id_from_live_command(self):
        command = '"exec /home/test/orc --db hermit2 --session-id abc-123 --tui"'
        self.assertEqual(ohm.orc_session_id_from_start_command(command), "abc-123")

    def test_missing_session_id_stays_unknown(self):
        self.assertIsNone(ohm.orc_session_id_from_start_command(ORC_START))


class TestDatabaseIdentityDerivation(unittest.TestCase):
    """A successor session must not inherit the predecessor's DB default."""

    def test_tg_db_path_names_the_successor_database(self):
        self.assertEqual(
            ohm.configured_orc_db(None, "/home/test/.tg/hermit2.db", None),
            ("hermit2", "TG_DB_PATH"),
        )

    def test_explicit_override_wins_over_environment_and_session(self):
        self.assertEqual(
            ohm.configured_orc_db(
                "chosen", "/home/test/.tg/hermit2.db", "orc-hermit3"
            ),
            ("chosen", "explicit --orc-db"),
        )

    def test_session_hint_is_a_fallback_when_tg_path_is_absent(self):
        self.assertEqual(
            ohm.configured_orc_db(None, None, "orc-hermit2"),
            ("hermit2", "--session"),
        )

    def test_live_session_metadata_ignores_an_unrelated_coordinator(self):
        self.assertEqual(
            ohm.orc_db_from_active_sessions(["orc-hatch2", "orc-hermit2"]),
            ("hermit2", "live Orc session metadata"),
        )

    def test_two_live_hermit_sessions_are_refused_not_guessed(self):
        with self.assertRaises(ohm.OrcMessageError) as caught:
            ohm.orc_db_from_active_sessions(["orc-hermit", "orc-hermit2"])
        self.assertIn("cannot derive one Hermit database", str(caught.exception))


class TestRecoverableMessagePersistence(unittest.TestCase):
    def test_exact_body_is_content_addressed_and_private(self):
        with tempfile.TemporaryDirectory() as tmp:
            body = "the exact wake body\nincluding its second line"
            path = ohm.persist_recoverable_message(Path(tmp) / "relay.log", body)
            self.assertEqual(path.read_text(encoding="utf-8"), body)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(
                path.name,
                f"{hashlib.sha256(body.encode()).hexdigest()}.txt",
            )

    def test_repeating_the_same_body_reuses_the_verified_artifact(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "relay.log"
            first = ohm.persist_recoverable_message(log, "same body")
            second = ohm.persist_recoverable_message(log, "same body")
            self.assertEqual(first, second)


class TestLivePaneParsing(unittest.TestCase):
    def test_short_lines_are_skipped_rather_than_crashing(self):
        panes = "not\tenough\tfields\n" + LIVE_SOCKET
        parsed = ohm.parse_live_panes(panes, ["orc-hermit"])
        self.assertEqual([pane.pane_id for pane in parsed], ["%0", "%1", "%2"])

    def test_command_is_reduced_to_its_basename(self):
        panes = pane_line("orc-hermit", "orc", "%0", "/home/test/orc-bin/orc", ORC_START)
        self.assertTrue(ohm.parse_live_panes(panes, ["orc-hermit"])[0].is_orc)


# ---------------------------------------------------------------------------
# Composer recognition and delivery acknowledgement
# ---------------------------------------------------------------------------
#
# The second bug this file covers: the relay probed for the literal strings
# "orc (hermit)" and "Type / for commands" to decide the composer was ready.
# The post-1.0 Orc TUI renders NEITHER, so 12 consecutive relays failed over six
# hours while the pane was healthy and idle. The third: delivery was never
# acknowledged — every tmux call exits 0 for a pane that ignored the keystrokes,
# so a total non-delivery logged status="sent" and exited 0.

# Verbatim tail of a real capture of the live coordinator pane orc-hermit:orc
# (%0, 178 columns) taken while it was idle. The fixtures below are generated,
# so this anchors them to something actually observed rather than to a guess
# about how the TUI renders.
REAL_IDLE_CAPTURE_TAIL = (
    "[assistant]\n"
    "`hermit-coord` resumed successfully and is actively reviewing PR #1746.\n"
    "\n"
    "[idle] Enter: submit • Esc: cancel • Ctrl+G: pause • Ctrl+T: inbox\n"
    + "-" * 178
    + "\n"
    "›\n"
    + "-" * 178
    + "\n"
    "Session: hermit (4fb50e87-5d91-4294-88b2-afeedf6cc917)  |  Turn: 393k "
    "(Cached: 390k) │ Ctx: 39% │ Total: 13.7M (Cached: 11.2M)\n"
    "Agents: total=15 busy=13 needs_input=1 unreachable=1  |  Workflows: 30\n"
    "DB: hermit  |  Tasks: 4054/4564  |  Ready: 14"
)

RULE = "-" * 178
KEY_HINTS = "Enter: submit • Esc: cancel • Ctrl+G: pause • Ctrl+T: inbox"
ORC_FOOTER = (
    "Session: hermit (4fb50e87-5d91-4294-88b2-afeedf6cc917)  |  Ctx: 39%\n"
    "Agents: total=15 busy=13 needs_input=1 unreachable=1  |  Workflows: 30\n"
    "DB: hermit  |  Tasks: 4054/4564  |  Ready: 14"
)


def orc_screen(draft: str = "", state: str = "idle", transcript: str = "") -> str:
    """Render a post-1.0 Orc TUI pane the way the live coordinator renders one."""
    lines: list[str] = []
    if transcript:
        lines.extend(transcript.splitlines())
    lines.append(f"[{state}] {KEY_HINTS}" if state else KEY_HINTS)
    lines.append(RULE)
    draft_lines = draft.split("\n") if draft else [""]
    lines.append(f"› {draft_lines[0]}".rstrip())
    lines.extend(draft_lines[1:])
    lines.append(RULE)
    lines.extend(ORC_FOOTER.splitlines())
    return "\n".join(lines)


class TestPostOneDotZeroComposerIsRecognised(unittest.TestCase):
    """The regression: a healthy post-1.0 pane must read as ready."""

    def test_the_real_idle_capture_parses_as_an_empty_ready_composer(self):
        composer = ohm.parse_composer(REAL_IDLE_CAPTURE_TAIL)
        self.assertIsNotNone(composer)
        self.assertTrue(composer.is_empty)
        self.assertEqual(composer.state, "idle")

    def test_the_real_idle_capture_contains_neither_retired_text_probe(self):
        # Pins the exact cause of the 12 failed relays. If a future change
        # reintroduces either probe, this fails rather than the fleet.
        self.assertNotIn("orc (hermit)", REAL_IDLE_CAPTURE_TAIL)
        self.assertNotIn("Type / for commands", REAL_IDLE_CAPTURE_TAIL)

    def test_generated_fixture_matches_the_real_capture_shape(self):
        real = ohm.parse_composer(REAL_IDLE_CAPTURE_TAIL)
        generated = ohm.parse_composer(orc_screen())
        self.assertEqual((real.text, real.state), (generated.text, generated.state))

    def test_a_streaming_composer_is_still_recognised_and_empty(self):
        # Readiness must not require [idle]: demanding an idle coordinator is
        # what silently dropped 17 relays in one day on a busy fleet.
        composer = ohm.parse_composer(orc_screen(state="streaming"))
        self.assertTrue(composer.is_empty)
        self.assertEqual(composer.state, "streaming")

    def test_a_draft_in_the_box_is_read_back_without_the_prompt_glyph(self):
        composer = ohm.parse_composer(orc_screen(draft="half typed"))
        self.assertEqual(composer.text, "half typed")
        self.assertFalse(composer.is_empty)

    def test_a_multiline_draft_is_read_back_whole(self):
        composer = ohm.parse_composer(orc_screen(draft="line one\nline two"))
        self.assertEqual(composer.text, "line one\nline two")

    def test_a_build_without_a_state_marker_still_parses(self):
        composer = ohm.parse_composer(orc_screen(state=""))
        self.assertTrue(composer.is_empty)
        self.assertIsNone(composer.state)

    def test_a_pane_that_is_not_an_orc_composer_is_not_recognised(self):
        # Must be distinguishable from "the composer is busy" — different
        # operator action, so parse_composer returns None rather than empty.
        self.assertIsNone(ohm.parse_composer("$ ls\nfoo bar\n$ "))


class TestProbeSurvivesWrapping(unittest.TestCase):
    def test_squash_matches_text_wrapped_mid_word(self):
        probe = ohm.message_probe("frontier update for the lander")
        wrapped = "frontier update for th\ne lander"
        self.assertIn(probe, ohm.squash(wrapped))

    def test_squash_matches_text_wrapped_at_a_space(self):
        probe = ohm.message_probe("frontier update for the lander")
        self.assertIn(probe, ohm.squash("frontier update for\nthe lander"))

    def test_a_different_message_does_not_match(self):
        probe = ohm.message_probe("frontier update for the lander")
        self.assertNotIn(probe, ohm.squash("unrelated coordinator chatter"))

    def test_two_hourly_reminders_get_different_probes(self):
        # The hourly relay sends a byte-identical file every tick; only the
        # timestamp prefix differs. A probe that cannot tell them apart would
        # corroborate this hour's delivery against last hour's copy still on
        # screen.
        body = "Read the alignment reminder and re-orient. " * 5
        first = ohm.prefix_eastern_time(
            body, datetime.datetime(2026, 8, 6, 18, 0, tzinfo=ohm.EASTERN_TIME_ZONE)
        )
        second = ohm.prefix_eastern_time(
            body, datetime.datetime(2026, 8, 6, 19, 0, tzinfo=ohm.EASTERN_TIME_ZONE)
        )
        self.assertNotEqual(ohm.message_probe(first), ohm.message_probe(second))

    def test_a_reminder_probe_does_not_match_the_previous_hours_copy(self):
        body = "Read the alignment reminder and re-orient. " * 5
        first = ohm.prefix_eastern_time(
            body, datetime.datetime(2026, 8, 6, 18, 0, tzinfo=ohm.EASTERN_TIME_ZONE)
        )
        second = ohm.prefix_eastern_time(
            body, datetime.datetime(2026, 8, 6, 19, 0, tzinfo=ohm.EASTERN_TIME_ZONE)
        )
        self.assertNotIn(ohm.message_probe(second), ohm.squash(first))


class FakeOrcPane:
    """A scriptable stand-in for the live Orc TUI behind ``run_tmux``.

    Models the one thing the real defect turned on: tmux exits 0 for every
    call regardless of whether the TUI consumed the keystrokes. ``accepts_input``
    and ``drains_on_enter`` switch that behaviour independently so each failure
    mode can be exercised while every tmux call still "succeeds".
    """

    def __init__(
        self,
        *,
        state: str = "idle",
        draft: str = "",
        accepts_input: bool = True,
        drains_on_enter: bool = True,
        echoes_to_transcript: bool = True,
        paint_delay: int = 0,
        honours_clear: bool = True,
        queues_on_enter: bool = False,
        panes: str = LIVE_SOCKET,
    ):
        self.state = state
        self.draft = draft
        self.accepts_input = accepts_input
        self.drains_on_enter = drains_on_enter
        self.echoes_to_transcript = echoes_to_transcript
        # Captures that still show the PRE-paste screen after an injection. The
        # live TUI repaints asynchronously and took several seconds under load,
        # which made a single post-injection capture report a false
        # non-delivery.
        self.paint_delay = paint_delay
        self.honours_clear = honours_clear
        self.queues_on_enter = queues_on_enter
        self.panes = panes
        self.transcript = ""
        self.pending: list[str] = []
        self.enters = 0
        self.clears = 0
        self.buffers: dict[str, str] = {}

    def screen(self, *, scrollback: bool = False) -> str:
        painted = self.draft
        if self.paint_delay > 0:
            self.paint_delay -= 1
            painted = ""
        queue = ""
        if self.pending:
            queue = f"[queued] {len(self.pending)} pending\n" + "\n".join(
                f"> {entry}" for entry in self.pending
            )
        transcript = "\n".join(
            part for part in ((self.transcript if scrollback else ""), queue) if part
        )
        return orc_screen(draft=painted, state=self.state, transcript=transcript)

    def __call__(self, socket, *args, input_text=None):
        argv = list(args)
        if argv[0] == "list-panes":
            return self.panes
        if argv[0] == "capture-pane":
            return self.screen(scrollback="-S" in argv)
        if argv[0] == "load-buffer":
            self.buffers[argv[argv.index("-b") + 1]] = input_text or ""
            return ""
        if argv[0] == "paste-buffer":
            text = self.buffers.pop(argv[argv.index("-b") + 1], "")
            if self.accepts_input:
                self.draft += text
            return ""
        if argv[0] == "delete-buffer":
            self.buffers.pop(argv[argv.index("-b") + 1], None)
            return ""
        if argv[0] == "send-keys":
            key = argv[-1]
            if key == "C-j":
                if self.accepts_input:
                    self.draft += "\n"
                return ""
            if key == "C-u":
                self.clears += 1
                if self.honours_clear:
                    self.draft = ""
                    self.paint_delay = 0
                return ""
            if key == "Enter":
                self.enters += 1
                if self.drains_on_enter:
                    if self.queues_on_enter:
                        self.pending.append(self.draft)
                    elif self.echoes_to_transcript:
                        self.transcript += f"[user]\n{self.draft}\n"
                    self.draft = ""
                    self.paint_delay = 0
                return ""
        raise AssertionError(f"unexpected tmux call: {argv}")


class DeliveryTestCase(unittest.TestCase):
    """Base for delivery tests: no real sleeping, no real tmux."""

    MESSAGE = "frontier update: hermit origin/main is 4c70658e, lock free"

    def setUp(self):
        for name in (
            "run_tmux",
            "COMPOSER_ATTEMPTS",
            "COMPOSER_RETRY_SLEEP",
            "INJECT_ATTEMPTS",
            "INJECT_RETRY_SLEEP",
            "DRAIN_ATTEMPTS",
            "DRAIN_RETRY_SLEEP",
        ):
            self.addCleanup(setattr, ohm, name, getattr(ohm, name))
        # Every retry budget is collapsed so the suite exercises the polling
        # logic without spending its wall-clock on real backoff.
        ohm.COMPOSER_ATTEMPTS = 2
        ohm.COMPOSER_RETRY_SLEEP = 0
        ohm.INJECT_ATTEMPTS = 2
        ohm.INJECT_RETRY_SLEEP = 0
        ohm.DRAIN_ATTEMPTS = 2
        ohm.DRAIN_RETRY_SLEEP = 0

    def deliver(self, pane: FakeOrcPane, message: str | None = None):
        ohm.run_tmux = pane
        return ohm.send_message(
            Path("/nonexistent.sock"), "%0", message or self.MESSAGE
        )

    def deliver_error(self, pane: FakeOrcPane, message: str | None = None) -> str:
        with self.assertRaises(ohm.OrcMessageError) as caught:
            self.deliver(pane, message)
        return str(caught.exception)


class TestDeliveryPositiveBracket(DeliveryTestCase):
    """The qualifying case must fire — a readiness check that never passes is
    just as broken as one that always passes."""

    def test_an_idle_pane_accepts_the_message_and_acknowledges_it(self):
        pane = FakeOrcPane()
        ack = self.deliver(pane)
        self.assertTrue(ack.echo)
        self.assertEqual(ack.evidence, "composer-drained+echo")
        self.assertEqual(ack.composer_state, "idle")

    def test_the_message_is_submitted_exactly_once(self):
        pane = FakeOrcPane()
        self.deliver(pane)
        self.assertEqual(pane.enters, 1)

    def test_the_whole_message_reaches_the_transcript_once(self):
        pane = FakeOrcPane()
        self.deliver(pane)
        self.assertEqual(pane.transcript.count(self.MESSAGE), 1)
        self.assertEqual(pane.transcript.count("[user]"), 1)

    def test_a_multiline_message_arrives_with_its_line_breaks(self):
        pane = FakeOrcPane()
        self.deliver(pane, "first line\nsecond line")
        self.assertIn("first line\nsecond line", pane.transcript)
        self.assertEqual(pane.enters, 1)

    def test_delivery_succeeds_while_the_coordinator_is_streaming(self):
        # Backpressure must not be a drop; the state travels with the result.
        pane = FakeOrcPane(state="streaming")
        ack = self.deliver(pane)
        self.assertEqual(ack.composer_state, "streaming")
        self.assertEqual(pane.enters, 1)

    def test_the_composer_is_left_empty_afterwards(self):
        pane = FakeOrcPane()
        self.deliver(pane)
        self.assertEqual(pane.draft, "")

    def test_an_acknowledged_send_without_a_transcript_echo_is_still_sent(self):
        # Weaker evidence, but the composer did drain. It must report the
        # weaker grade rather than claim the echo it did not see.
        pane = FakeOrcPane(echoes_to_transcript=False)
        ack = self.deliver(pane)
        self.assertFalse(ack.echo)
        self.assertEqual(ack.evidence, "composer-drained")


class TestDeliveryNegativeBracket(DeliveryTestCase):
    """Every non-delivery must raise. None of these tmux calls fail."""

    def test_a_pane_that_ignores_keystrokes_is_refused(self):
        pane = FakeOrcPane(accepts_input=False)
        message = self.deliver_error(pane)
        self.assertIn("did not reach the Orc composer", message)

    def test_a_pane_that_ignores_keystrokes_is_never_sent_an_enter(self):
        # The critical half: submitting into a pane that took nothing could
        # fire whatever the composer already had, or a stray blank turn.
        pane = FakeOrcPane(accepts_input=False)
        self.deliver_error(pane)
        self.assertEqual(pane.enters, 0)

    def test_a_composer_that_never_drains_is_refused(self):
        """The refusal must point at Orc, not at the composer.

        This asserted the earlier wording, "did not drain" plus "NOT accepted".
        Both were accurate about what was seen and misleading about what it
        meant: measured 2026-09-05, the box was found, took the keystrokes and
        echoed them back, and Orc did not consume them -- the same coordinator
        was failing an identity probe on its own control socket. A reader sent
        after the composer is sent after a component that is working.
        """
        pane = FakeOrcPane(drains_on_enter=False)
        message = self.deliver_error(pane)
        self.assertIn("Orc did not accept the submission", message)
        self.assertIn("NOT delivered", message)
        self.assertIn("THE COMPOSER IS NOT THE SUSPECT", message)
        self.assertIn("orc session socket", message)

    def test_a_composer_that_never_drains_has_our_own_draft_removed(self):
        """Nothing is stranded: the relay takes back only what it typed."""
        pane = FakeOrcPane(drains_on_enter=False)
        message = self.deliver_error(pane)
        self.assertIn("this relay's own and has been removed", message)

    def test_a_composer_that_never_drains_is_not_resent(self):
        pane = FakeOrcPane(drains_on_enter=False)
        self.deliver_error(pane)
        self.assertEqual(pane.enters, 1)

    def test_an_unrecognised_pane_layout_is_refused_before_typing(self):
        pane = FakeOrcPane()
        pane.screen = lambda **kwargs: "$ ls\nfoo\n$ "
        message = self.deliver_error(pane)
        self.assertIn("does not render an Orc composer", message)
        self.assertIn("nothing was typed", message)
        self.assertEqual(pane.enters, 0)

    def test_an_occupied_composer_is_not_typed_into(self):
        # Injecting here would concatenate with somebody else's draft and
        # submit the result as one message.
        pane = FakeOrcPane(draft="somebody else was typing")
        message = self.deliver_error(pane)
        self.assertIn("unsent draft", message)
        self.assertIn("somebody else was typing", message)
        self.assertEqual(pane.enters, 0)
        self.assertEqual(pane.draft, "somebody else was typing")


class TestObservedLiveTuiBehaviour(DeliveryTestCase):
    """Behaviours measured on the live coordinator pane during this fix.

    Both were found by running the relay for real, and both would otherwise be
    misreported: a slow repaint as a non-delivery, and a queued submission as a
    delivery with no evidence behind it.
    """

    def setUp(self):
        super().setUp()
        # Enough polls to outlast a slow repaint; the base class restores these.
        ohm.INJECT_ATTEMPTS = 6
        ohm.DRAIN_ATTEMPTS = 6

    def test_a_slow_repaint_is_waited_out_rather_than_called_a_failure(self):
        # Measured live: paste-buffer returned, the composer stayed blank for
        # seconds, then the text appeared. A single capture called that a
        # non-delivery.
        pane = FakeOrcPane(paint_delay=3)
        ack = self.deliver(pane)
        self.assertEqual(pane.enters, 1)
        self.assertTrue(ack.echo)

    def test_a_repaint_slower_than_the_budget_is_still_refused(self):
        pane = FakeOrcPane(paint_delay=99)
        message = self.deliver_error(pane)
        self.assertIn("did not reach the Orc composer", message)
        self.assertEqual(pane.enters, 0)

    def test_an_unverifiable_injection_clears_the_box_for_the_next_relay(self):
        # Without this a refused relay wedges every later one, because the next
        # run correctly refuses to type into an occupied composer.
        pane = FakeOrcPane(accepts_input=False)
        message = self.deliver_error(pane)
        self.assertEqual(pane.clears, 1)
        self.assertIn("Composer cleared", message)

    def test_a_composer_that_cannot_be_cleared_says_so(self):
        pane = FakeOrcPane(accepts_input=False, honours_clear=False)
        pane.draft = ""

        # Leave a residue that C-u will not remove, as a stuck TUI would.
        def stubborn_screen(*, scrollback=False):
            return orc_screen(draft="stuck residue", state="idle")

        original = pane.screen
        pane.screen = lambda **kwargs: (
            original(**kwargs) if pane.clears == 0 else stubborn_screen(**kwargs)
        )
        message = self.deliver_error(pane)
        self.assertIn("could not clear the composer", message)

    def test_a_message_queued_while_streaming_counts_as_delivered(self):
        # Orc parks submissions during a turn and renders `[queued] N pending`
        # instead of a transcript block. That is acceptance, not loss.
        pane = FakeOrcPane(state="streaming", queues_on_enter=True)
        ack = self.deliver(pane)
        self.assertTrue(ack.echo)
        self.assertEqual(ack.pending, 1)
        self.assertEqual(ack.composer_state, "streaming")

    def test_the_queue_depth_is_reported_so_delivery_is_not_read_as_read(self):
        pane = FakeOrcPane(state="streaming", queues_on_enter=True)
        pane.pending = ["an earlier message"]
        ack = self.deliver(pane)
        self.assertEqual(ack.pending, 2)

    def test_an_idle_delivery_reports_no_queue(self):
        ack = self.deliver(FakeOrcPane())
        self.assertIsNone(ack.pending)


class TestPendingQueueParsing(unittest.TestCase):
    def test_the_live_queue_marker_is_parsed(self):
        # Verbatim shape captured from the live pane while it was streaming.
        screen = orc_screen(
            state="streaming", transcript="[queued] 1 pending\n> Great keep working"
        )
        self.assertEqual(ohm.parse_pending(screen), 1)

    def test_a_pane_with_no_queue_reports_none(self):
        self.assertIsNone(ohm.parse_pending(orc_screen()))


class TestExitStatusAndLog(DeliveryTestCase):
    """rc and the durable log must agree with what happened.

    The reported defect was rc=0 on a failed relay, which made upstream
    automation report false success.
    """

    def run_main(self, pane: FakeOrcPane, tmp_path: Path):
        import tempfile

        ohm.run_tmux = pane
        self.addCleanup(setattr, ohm, "list_active_orc_sessions", ohm.list_active_orc_sessions)
        self.addCleanup(setattr, ohm, "resolve_socket", ohm.resolve_socket)
        ohm.list_active_orc_sessions = lambda command: ["orc-hermit"]
        ohm.resolve_socket = lambda socket, **kwargs: socket

        log_file = tmp_path / "delivery.log"
        argv = [
            "orc-hermit-msg",
            self.MESSAGE,
            "--orc-db",
            "hermit",
            "--socket",
            str(tmp_path / "fake.sock"),
            "--log-file",
            str(log_file),
        ]
        self.addCleanup(setattr, sys, "argv", sys.argv)
        sys.argv = argv
        try:
            code = ohm.main()
        except SystemExit as exit_error:
            code = exit_error.code
        records = [
            json.loads(line)
            for line in log_file.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
        return code, records

    def test_a_successful_relay_exits_zero_and_logs_sent(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, records = self.run_main(FakeOrcPane(), Path(tmp))
            recovery_text = Path(
                records[0]["recoverable_message_file"]
            ).read_text(encoding="utf-8")
        self.assertEqual(code, 0)
        self.assertEqual([record["status"] for record in records], ["sent"])
        self.assertEqual(records[0]["ack"], "composer-drained+echo")
        self.assertEqual(records[0]["composer_state"], "idle")
        self.assertEqual(recovery_text, self.MESSAGE)

    def test_a_pane_that_ignores_keystrokes_exits_nonzero_and_logs_failed(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, records = self.run_main(FakeOrcPane(accepts_input=False), Path(tmp))
            recovery_text = Path(
                records[0]["recoverable_message_file"]
            ).read_text(encoding="utf-8")
        self.assertNotEqual(code, 0)
        self.assertEqual([record["status"] for record in records], ["failed"])
        self.assertIn("did not reach the Orc composer", records[0]["error"])
        self.assertEqual(recovery_text, self.MESSAGE)

    def test_a_composer_that_never_drains_exits_nonzero_and_logs_failed(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, records = self.run_main(FakeOrcPane(drains_on_enter=False), Path(tmp))
        self.assertNotEqual(code, 0)
        self.assertEqual([record["status"] for record in records], ["failed"])

    def test_a_failed_relay_never_logs_sent(self):
        # The precise false-success channel: status="sent" with rc=0 while
        # nothing was delivered.
        with tempfile.TemporaryDirectory() as tmp:
            code, records = self.run_main(FakeOrcPane(accepts_input=False), Path(tmp))
        self.assertNotIn("sent", [record["status"] for record in records])

    def test_a_successful_relay_logs_exactly_one_record(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, records = self.run_main(FakeOrcPane(), Path(tmp))
        self.assertEqual(len(records), 1)

    def test_missing_api_receiver_fails_loudly_with_recoverable_body(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            log_file = root / "delivery.log"
            argv = [
                "orc-hermit-msg",
                self.MESSAGE,
                "--transport",
                "api",
                "--orc-db",
                "hermit",
                "--log-file",
                str(log_file),
            ]
            with mock.patch.object(sys, "argv", argv), \
                 mock.patch.object(ohm, "resolve_api_socket", return_value=None), \
                 contextlib.redirect_stderr(io.StringIO()) as stderr:
                with self.assertRaises(SystemExit) as caught:
                    ohm.main()
            records = [
                json.loads(line)
                for line in log_file.read_text(encoding="utf-8").splitlines()
            ]
            recovery = Path(records[0]["recoverable_message_file"])
            self.assertEqual(recovery.read_text(encoding="utf-8"), self.MESSAGE)
        self.assertNotEqual(caught.exception.code, 0)
        self.assertEqual([record["status"] for record in records], ["failed"])
        self.assertIn("recoverable message:", stderr.getvalue())



class VerifyInjectedConsumptionTests(unittest.TestCase):
    """An empty composer is ambiguous; the scrollback is what disambiguates it.

    `verify_injected` used to read "composer empty and probe not on screen" as
    non-delivery. That is only one of its two meanings: the other is that Orc
    took the draft before the relay looked, which is DELIVERY. The 2026-08-07
    T03:00Z hourly wake hit the ambiguous state, was reported relay-rc=1, and
    its hour was released -- so a later run would have sent the whole status
    again. These tests drive the three states directly, with tmux replaced, so
    the branch is exercised deterministically rather than by racing a pane.
    """

    RULE = "-" * 40
    PROBE = "theexactmessageprobe"

    def _screen(self, draft: str = "") -> str:
        return "\n".join([self.RULE, f"> {draft}", self.RULE])

    def _patch(self, screen: str, transcript: str):
        calls = []

        def fake_capture(socket, pane_id, *, scrollback: int = 0) -> str:
            calls.append(scrollback)
            return transcript if scrollback else screen

        return fake_capture, calls

    def test_a_populated_composer_needs_no_scrollback_lookup(self):
        capture, calls = self._patch(self._screen("some draft"), "")
        with _patched(ohm, capture_pane=capture):
            outcome = ohm.verify_injected(Path("/nonexistent"), "%0", self.PROBE)
        self.assertEqual(outcome, ohm.INJECTED_IN_COMPOSER)
        self.assertEqual(calls, [0], "must not read scrollback when the box is full")

    def test_an_empty_composer_with_the_message_in_scrollback_is_a_delivery(self):
        # THE CASE THAT USED TO DOUBLE-SEND.
        capture, calls = self._patch(self._screen(""), f"[user]\n{self.PROBE}\n")
        with _patched(ohm, capture_pane=capture, INJECT_ATTEMPTS=2, INJECT_RETRY_SLEEP=0.0):
            outcome = ohm.verify_injected(Path("/nonexistent"), "%0", self.PROBE)
        self.assertEqual(outcome, ohm.INJECTED_ALREADY_CONSUMED)
        self.assertIn(ohm.TRANSCRIPT_SCROLLBACK_LINES, calls, "scrollback must be consulted")

    def test_an_empty_composer_with_nothing_anywhere_still_fails(self):
        # The genuine non-delivery must NOT have been converted into a success.
        capture, _ = self._patch(self._screen(""), "[user]\nsomething entirely else\n")
        with _patched(
            ohm, capture_pane=capture, INJECT_ATTEMPTS=2, INJECT_RETRY_SLEEP=0.0,
            clear_composer=lambda *a, **k: True,
        ):
            with self.assertRaises(ohm.OrcMessageError) as caught:
                ohm.verify_injected(Path("/nonexistent"), "%0", self.PROBE)
        self.assertIn("did not reach the Orc composer", str(caught.exception))

    def test_the_two_outcomes_differ_only_by_the_scrollback(self):
        """Identical screens, opposite verdicts -- the transcript decides."""
        screen = self._screen("")
        for transcript, expected in (
            (f"[user]\n{self.PROBE}\n", ohm.INJECTED_ALREADY_CONSUMED),
            ("[user]\nunrelated turn\n", None),
        ):
            capture, _ = self._patch(screen, transcript)
            with _patched(
                ohm, capture_pane=capture, INJECT_ATTEMPTS=2, INJECT_RETRY_SLEEP=0.0,
                clear_composer=lambda *a, **k: True,
            ):
                if expected is None:
                    with self.assertRaises(ohm.OrcMessageError):
                        ohm.verify_injected(Path("/nonexistent"), "%0", self.PROBE)
                else:
                    self.assertEqual(
                        ohm.verify_injected(Path("/nonexistent"), "%0", self.PROBE),
                        expected,
                    )

    def test_a_consumed_draft_is_never_given_an_enter(self):
        """send_message must not submit something Orc already holds."""
        sent_keys = []

        def fake_run_tmux(socket, *args):
            sent_keys.append(args)
            return ""

        capture, _ = self._patch(self._screen(""), f"[user]\n{self.PROBE}\n")
        with _patched(
            ohm,
            capture_pane=capture,
            run_tmux=fake_run_tmux,
            inject_message=lambda *a, **k: None,
            wait_for_ready_composer=lambda *a, **k: ohm.Composer(text="", state="idle"),
            message_probe=lambda _m: self.PROBE,
            INJECT_ATTEMPTS=2,
            INJECT_RETRY_SLEEP=0.0,
        ):
            ack = ohm.send_message(Path("/nonexistent"), "%0", "irrelevant body")
        self.assertTrue(ack.consumed_without_submit)
        self.assertEqual(ack.evidence, "consumed-before-submit+echo")
        self.assertFalse(
            any("Enter" in a for call in sent_keys for a in call),
            f"Enter must not be sent for an already-consumed draft: {sent_keys}",
        )


class _InertReceiver:
    """A real HTTP server on a Unix socket that records what it was sent.

    Inert by construction: it is a socket in a temp directory with no route to
    any coordinator, agent, or chat surface. Nothing here can deliver a
    user-facing message; the point is to prove the relay binds success to a
    RECEIVER'S ACKNOWLEDGEMENT rather than to its own write succeeding.
    """

    def __init__(self, path, status=200, body='{"status":"accepted","message_id":"m-1"}'):
        self.path = str(path)
        self.status = status
        self.body = body
        self.requests: list[str] = []
        self._stop = False
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.bind(self.path)
        self._sock.listen(8)
        self._sock.settimeout(0.25)
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self):
        while not self._stop:
            try:
                conn, _ = self._sock.accept()
            except OSError:
                continue
            with conn:
                conn.settimeout(2.0)
                try:
                    raw = b""
                    while b"\r\n\r\n" not in raw:
                        chunk = conn.recv(4096)
                        if not chunk:
                            break
                        raw += chunk
                    head, _, rest = raw.partition(b"\r\n\r\n")
                    length = 0
                    for line in head.decode("latin1").split("\r\n"):
                        if line.lower().startswith("content-length:"):
                            length = int(line.split(":", 1)[1].strip())
                    while len(rest) < length:
                        chunk = conn.recv(4096)
                        if not chunk:
                            break
                        rest += chunk
                    self.requests.append(rest.decode("utf-8", errors="replace"))
                    payload = self.body.encode()
                    conn.sendall(
                        f"HTTP/1.1 {self.status} X\r\nContent-Type: application/json\r\n"
                        f"Content-Length: {len(payload)}\r\nConnection: close\r\n\r\n".encode()
                        + payload
                    )
                except OSError:
                    pass

    def stop(self):
        self._stop = True
        with contextlib.suppress(OSError):
            self._sock.close()
        self._thread.join(timeout=2)
        with contextlib.suppress(OSError):
            os.unlink(self.path)


class ApiDeliveryTests(unittest.TestCase):
    """Delivery must bind to the receiver's acknowledgement.

    The tmux path infers delivery from a rendered TUI, which is why this file
    has a history of ambiguous outcomes. Here the receiver answers, so success
    is a reply from the thing that will consume the message.
    """

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="ohm-api-"))
        self.sock = self.tmp / "recv.sock"
        self.receiver = None

    def tearDown(self):
        if self.receiver is not None:
            self.receiver.stop()

    def test_a_live_receiver_acknowledges_the_exact_payload_once(self):
        self.receiver = _InertReceiver(self.sock)
        ack = ohm.deliver_via_api(self.sock, "the exact hourly wake body")
        self.assertEqual(ack.status, 200)
        self.assertEqual(ack.ack_id, "m-1")
        self.assertEqual(ack.evidence, "api-accepted:m-1")
        # EXACTLY ONE request, carrying EXACTLY the payload.
        self.assertEqual(len(self.receiver.requests), 1)
        self.assertEqual(
            json.loads(self.receiver.requests[0])["message"],
            "the exact hourly wake body",
        )

    def test_a_missing_receiver_refuses_and_sends_nothing(self):
        # Fail-closed: no socket means nothing can accept the message. The relay
        # must not silently fall back to typing, which is how a refusal becomes
        # a blind write.
        with self.assertRaises(ohm.OrcMessageError) as caught:
            ohm.deliver_via_api(self.tmp / "absent.sock", "must not be delivered")
        self.assertIn("no Orc receiver", str(caught.exception))

    def test_a_receiver_that_refuses_is_not_reported_as_delivered(self):
        self.receiver = _InertReceiver(self.sock, status=503, body='{"error":"busy"}')
        with self.assertRaises(ohm.OrcMessageError) as caught:
            ohm.deliver_via_api(self.sock, "rejected body")
        self.assertIn("503", str(caught.exception))
        # It still reached the receiver once; the point is that a non-2xx is
        # never laundered into success.
        self.assertEqual(len(self.receiver.requests), 1)

    def test_a_restarted_receiver_does_not_receive_a_duplicate(self):
        """One send per invocation, across a receiver restart.

        The dangerous shape is a relay that retries after an ambiguous outcome:
        an unanswered POST may still have been processed, so a retry is how the
        owner gets the same status twice. Each invocation posts exactly once.
        """
        self.receiver = _InertReceiver(self.sock)
        ohm.deliver_via_api(self.sock, "before restart")
        self.assertEqual(len(self.receiver.requests), 1)

        # Restart the receiver on the same path, as a coordinator relaunch would.
        self.receiver.stop()
        self.receiver = _InertReceiver(self.sock)
        self.assertEqual(len(self.receiver.requests), 0, "a fresh receiver starts empty")

        ohm.deliver_via_api(self.sock, "after restart")
        self.assertEqual(
            len(self.receiver.requests), 1,
            "the second invocation must post ONCE, not replay the first",
        )
        self.assertEqual(
            json.loads(self.receiver.requests[0])["message"], "after restart"
        )

    def test_a_receiver_that_vanishes_mid_flight_refuses_without_retrying(self):
        # Bound to the socket then gone: the relay must report failure rather
        # than reconnect-and-resend.
        self.receiver = _InertReceiver(self.sock)
        self.receiver.stop()
        self.sock.touch()  # path exists, nothing listening
        with self.assertRaises(ohm.OrcMessageError) as caught:
            ohm.deliver_via_api(self.sock, "nobody is listening")
        self.assertIn("did not answer", str(caught.exception))

    def test_an_acknowledgement_without_an_id_is_still_an_acknowledgement(self):
        # A 2xx IS the ack; the id is extra provenance, and its absence must not
        # invent a failure.
        self.receiver = _InertReceiver(self.sock, body='{"status":"queued"}')
        ack = ohm.deliver_via_api(self.sock, "no id in the reply")
        self.assertIsNone(ack.ack_id)
        self.assertEqual(ack.evidence, "api-accepted")

    def test_the_ack_id_is_read_from_the_shapes_orc_actually_uses(self):
        for body, expected in (
            ('{"message_id":"a"}', "a"),
            ('{"id":123}', "123"),
            ('{"data":{"request_id":"r"}}', "r"),
            ('{"result":{"queue_id":"q"}}', "q"),
            ("not json at all", None),
            ('["a","list"]', None),
        ):
            self.assertEqual(ohm._api_ack_id(body), expected, body)


class ApiSocketResolutionTests(unittest.TestCase):
    def test_current_schema_binds_to_exact_parent_session(self):
        entries = [
            {
                "session_id": "current-session",
                "name": "1b176094",
                "uds_socket_path": "/run/user/1/orc/current.sock",
            },
            {
                "session_id": "predecessor-session",
                "name": "d0f81544",
                "uds_socket_path": "/run/user/1/orc/predecessor.sock",
            },
        ]
        completed = mock.Mock(returncode=0, stdout=json.dumps(entries))
        with mock.patch.object(ohm.subprocess, "run", return_value=completed):
            resolved = ohm.resolve_api_socket(
                None, Path("/inert/orc"), "hermit2", "current-session"
            )
        self.assertEqual(resolved, Path("/run/user/1/orc/current.sock"))

    def test_unknown_parent_session_refuses_instead_of_guessing(self):
        entries = [
            {
                "session_id": "some-other-session",
                "uds_socket_path": "/run/user/1/orc/other.sock",
            }
        ]
        completed = mock.Mock(returncode=0, stdout=json.dumps(entries))
        with mock.patch.object(ohm.subprocess, "run", return_value=completed):
            resolved = ohm.resolve_api_socket(
                None, Path("/inert/orc"), "hermit2", "missing-session"
            )
        self.assertIsNone(resolved)

    def test_old_schema_without_session_ids_keeps_database_fallback(self):
        entries = [
            {"name": "hermit2", "uds_socket": "/run/user/1/orc/legacy.sock"}
        ]
        completed = mock.Mock(returncode=0, stdout=json.dumps(entries))
        with mock.patch.object(ohm.subprocess, "run", return_value=completed):
            resolved = ohm.resolve_api_socket(
                None, Path("/inert/orc"), "hermit2", "current-session"
            )
        self.assertEqual(resolved, Path("/run/user/1/orc/legacy.sock"))

if __name__ == "__main__":
    unittest.main(verbosity=2)


class TestSelfTestCannotReachAUser(unittest.TestCase):
    """A self-test that can reach a person is a production side effect.

    This has already happened twice -- `RELAY SELF-TEST from hermit-w12...` and
    a socket-fallback probe both landed in front of the owner. The guard is
    checked here structurally: the refusal happens on the DESTINATION, before
    anything is composed or typed, so it cannot be defeated by a test that
    merely intends not to send.

    Bracketed both directions. Negatives alone would be satisfied by a guard
    that refuses every self-test, which would make self-testing impossible
    rather than safe -- so the inert-fixture positive is load-bearing.
    """

    def test_negative_self_test_without_an_explicit_socket_is_refused(self):
        # No --socket means the LIVE coordinator, which is exactly the leak.
        reason = ohm.self_test_destination_refusal(None)
        self.assertIsNotNone(reason)
        self.assertIn("explicit --socket", reason)

    def test_negative_self_test_against_the_live_socket_is_refused(self):
        reason = ohm.self_test_destination_refusal(ohm.DEFAULT_SOCKET)
        self.assertIsNotNone(reason)
        self.assertIn("live coordinator socket", reason)

    def test_negative_any_socket_in_the_live_runtime_dir_is_refused(self):
        """Naming a sibling server explicitly must not buy a way in.

        Real orc servers are published under the runtime dir, so a socket there
        is presumed user-facing even if it is not the default one.
        """
        sibling = ohm.LIVE_SOCKET_DIR / f"tmux-{ohm.UID}" / "some-other-server"
        reason = ohm.self_test_destination_refusal(sibling)
        self.assertIsNotNone(reason)
        self.assertIn(str(ohm.LIVE_SOCKET_DIR), reason)

    def test_positive_an_inert_fixture_socket_is_allowed(self):
        """The guard must still permit a real self-test against a fixture."""
        with tempfile.TemporaryDirectory() as tmp:
            fixture = Path(tmp) / "fixture-server"
            self.assertIsNone(ohm.self_test_destination_refusal(fixture))

    def test_dry_run_is_not_accepted_as_a_substitute_for_an_inert_target(self):
        """Dry-run promises not to type; it still touches the live pane.

        The earlier leak came from a probe that was expected not to deliver, so
        'well-behaved on the happy path' is not the property being enforced.
        """
        reason = ohm.self_test_destination_refusal(None)
        self.assertIsNotNone(reason, "dry-run must not exempt the destination check")

    def test_cli_refuses_a_self_test_aimed_at_the_live_coordinator(self):
        """End to end through argparse: exit before any tmux call happens."""
        argv = ["orc-hermit-msg", "--self-test", "hello"]
        with mock.patch.object(sys, "argv", argv), \
             mock.patch.object(ohm, "run_tmux") as never_called:
            with self.assertRaises(SystemExit) as caught:
                ohm.parse_args()
        self.assertEqual(2, caught.exception.code, "argparse error exit")
        never_called.assert_not_called()
