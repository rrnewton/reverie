#!/usr/bin/env python3
"""What the relay recognises as the Orc input box, and what it cleans up.

THE DEFECT THIS GUARDS. ``COMPOSER_RULE_RE`` was ``^-{20,}$`` -- a line of twenty
or more ASCII hyphens and nothing else -- while the shipped build draws the box
with U+250C/U+2502/U+2514/U+2500. ``parse_composer`` returned None on every
attempt and the relay refused with "does not render an Orc composer": accurate
about what it saw, misleading about what it meant. That refusal is why
``hermit-owner-checkout-validate.service`` exited 5 on every firing.

WHY THE EXISTING SUITE COULD NOT FAIL. ``scripts/tests/fake_orc_pane.py``
rendered ``"-" * RULE_WIDTH`` -- exactly what the detector matched and exactly
what the coordinator had stopped drawing. The fixture and the detector agreed
with each other and neither agreed with reality. Both this file and that fixture
now read the SAME captured frame, ``scripts/tests/data/captured-orc-composer.txt``,
taken with ``tmux capture-pane -p`` from the live coordinator, so a rendering we
invented cannot stand in for one the coordinator produces.

This file lives in ``scripts/`` because the Makefile's ``unittest discover -s
scripts`` cannot descend into ``scripts/tests/`` (no ``__init__.py``), so
everything under there is unscheduled today.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
RELAY = ROOT / "scripts/orc-hermit-msg.py"
CAPTURE = ROOT / "scripts/tests/data/captured-orc-composer.txt"


def _relay():
    spec = importlib.util.spec_from_file_location("orc_hermit_msg_layouts", RELAY)
    module = importlib.util.module_from_spec(spec)
    sys.modules["orc_hermit_msg_layouts"] = module
    spec.loader.exec_module(module)
    return module


ohm = _relay()


def captured_pane(body: str | None = None) -> str:
    """The captured frame, optionally with a draft in place of the placeholder."""
    frame = CAPTURE.read_text(encoding="utf-8").splitlines()
    lines = [frame[0]]
    lines.extend(f"│  {line}" for line in (body or "Type / for commands").split("\n"))
    lines.append(frame[-1])
    return "\n".join(lines) + "\n"


ASCII_PANE = (
    "[idle] Enter: submit • Esc: cancel • Ctrl+G: pause\n"
    + "-" * 60 + "\n"
    "› \n"
    + "-" * 60 + "\n"
    "Session: s (fake-inert)  |  Ctx: 0%\n"
)


class TheCapturedFrameIsUsableAsAFixture(unittest.TestCase):
    def test_the_capture_is_present_and_is_a_box_frame(self) -> None:
        """A missing or rewritten capture must fail loudly, not quietly pass."""
        frame = CAPTURE.read_text(encoding="utf-8").splitlines()
        self.assertGreaterEqual(len(frame), 2, "capture is too short to be a frame")
        self.assertTrue(frame[0].lstrip().startswith("┌"), frame[0][:40])
        self.assertTrue(frame[-1].lstrip().startswith("└"), frame[-1][:40])


class TheShippedBoxDrawingLayout(unittest.TestCase):
    def test_an_empty_box_is_detected_and_reads_empty(self) -> None:
        """THE REGRESSION. Before the fix this returned None on the same bytes."""
        composer = ohm.parse_composer(captured_pane())
        self.assertIsNotNone(composer, "the captured pane must parse as a composer")
        self.assertTrue(composer.is_empty)

    def test_the_placeholder_is_kept_as_evidence_rather_than_normalised_away(self) -> None:
        self.assertEqual("Type / for commands", ohm.parse_composer(captured_pane()).text)

    def test_an_occupied_box_is_not_empty_so_the_relay_still_refuses(self) -> None:
        """The load-bearing safety property: never type into somebody's draft."""
        composer = ohm.parse_composer(captured_pane("somebody else's unsent draft"))
        self.assertIsNotNone(composer)
        self.assertFalse(composer.is_empty)
        self.assertEqual("somebody else's unsent draft", composer.text)

    def test_a_draft_made_only_of_border_characters_stays_occupied(self) -> None:
        """Greedy border stripping would reduce this to '' and license typing."""
        self.assertFalse(ohm.parse_composer(captured_pane("││|||")).is_empty)


class TheOlderHyphenLayoutStillWorks(unittest.TestCase):
    def test_an_empty_box_reads_empty_and_its_state_marker_is_parsed(self) -> None:
        composer = ohm.parse_composer(ASCII_PANE)
        self.assertTrue(composer.is_empty)
        self.assertEqual("idle", composer.state)

    def test_an_occupied_box_is_not_empty(self) -> None:
        composer = ohm.parse_composer(ASCII_PANE.replace("› \n", "› a draft\n"))
        self.assertFalse(composer.is_empty)
        self.assertEqual("a draft", composer.text)


class WhatMustStillBeRefused(unittest.TestCase):
    def test_a_pane_with_no_box_at_all_is_not_a_composer(self) -> None:
        self.assertIsNone(ohm.parse_composer("plain output\nno box here\n"))

    def test_one_rule_is_not_a_box(self) -> None:
        self.assertIsNone(ohm.parse_composer("text\n" + "─" * 40 + "\ntail\n"))

    def test_a_stale_placeholder_fails_towards_refusing_not_towards_typing(self) -> None:
        """The direction of the text-probe risk, pinned as a test.

        The relay already records that text probes flip between Orc builds. If
        the placeholder changes again, an EMPTY box stops reading empty and the
        relay refuses -- costing a delivery. It must never fail the other way,
        where an occupied box reads empty and gets typed into.
        """
        composer = ohm.parse_composer(captured_pane("Press / for commands"))
        self.assertIsNotNone(composer, "detection is structural and must still work")
        self.assertFalse(
            composer.is_empty,
            "an unrecognised placeholder must read as occupied so the relay "
            "refuses rather than typing into what might be a real draft",
        )


class CleaningUpAnUndeliveredDraft(unittest.TestCase):
    """⚠️ Landing the detector without this stranded text in the owner's pane.

    On 2026-09-05 the relay found the box for the first time, typed the validate
    report into the live coordinator, failed to submit, and left it there. These
    pin the half that needs no tmux: whose draft it is willing to touch.
    """

    def test_a_draft_that_is_not_ours_is_left_alone(self) -> None:
        composer = ohm.Composer(text="somebody else's work in progress", state=None)
        result = ohm._clear_our_own_draft(
            Path("/nonexistent.sock"), "%0", "our-own-probe-text", composer
        )
        self.assertIn("left untouched", result)
        self.assertIn("not what this relay typed", result)

    def test_an_unparseable_composer_is_left_alone(self) -> None:
        result = ohm._clear_our_own_draft(
            Path("/nonexistent.sock"), "%0", "our-own-probe-text", None
        )
        self.assertIn("left untouched", result)

    def test_a_failed_clear_is_reported_as_a_failure_not_as_success(self) -> None:
        """The socket does not exist, so the clear cannot work; say so."""
        composer = ohm.Composer(text="our-own-probe-text and more", state=None)
        result = ohm._clear_our_own_draft(
            Path("/nonexistent.sock"), "%0", "our-own-probe-text", composer
        )
        self.assertIn("did not clear", result)
        self.assertNotIn("has been removed", result)


class TheRefusalNamesTheConditionNotTheSymptom(unittest.TestCase):
    def test_the_drain_failure_message_does_not_send_the_reader_at_the_composer(self) -> None:
        """Measured: the composer was fine and Orc was not answering at all."""
        self.assertIn("THE COMPOSER IS NOT THE SUSPECT", ohm.NOT_THE_COMPOSER)
        self.assertIn("orc session socket", ohm.NOT_THE_COMPOSER)
        self.assertIn("api/identity", ohm.NOT_THE_COMPOSER)
        source = RELAY.read_text(encoding="utf-8")
        self.assertIn("Orc did not accept the submission", source)
        self.assertIn("{NOT_THE_COMPOSER}", source, "the guidance must be used")
        self.assertNotIn("Clear the composer before the next relay", source)


if __name__ == "__main__":
    unittest.main()
