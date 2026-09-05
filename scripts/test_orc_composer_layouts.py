#!/usr/bin/env python3
"""What the relay recognises as the Orc input box, in both renderings.

THE DEFECT THIS GUARDS. ``COMPOSER_RULE_RE`` was ``^-{20,}$`` -- a line of twenty
or more ASCII hyphens and nothing else -- and the shipped build draws the box
with U+250C/U+2502/U+2514/U+2500. ``parse_composer`` therefore returned None on
every attempt and the relay refused with "does not render an Orc composer",
accurate about what it saw and misleading about what it meant. The owner-checkout
validate report was undeliverable on that refusal, which is what made
``hermit-owner-checkout-validate.service`` exit 5 on every firing.

WHY THE EXISTING SUITE DID NOT CATCH IT. ``scripts/tests/fake_orc_pane.py``
rendered only the hyphen form -- exactly what the detector matched and exactly
what the coordinator no longer draws -- so the inert suite was green against a
rendering that does not exist. That fixture now draws both and the inert cases
run over both; this file is the fast parser-level half, and it lives in
``scripts/`` because the Makefile's ``unittest discover -s scripts`` cannot
descend into ``scripts/tests/`` (no ``__init__.py``), so everything under there
is unscheduled today.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
RELAY = ROOT / "scripts/orc-hermit-msg.py"


def _relay():
    spec = importlib.util.spec_from_file_location("orc_hermit_msg_layouts", RELAY)
    module = importlib.util.module_from_spec(spec)
    sys.modules["orc_hermit_msg_layouts"] = module
    spec.loader.exec_module(module)
    return module


ohm = _relay()

#: Verbatim from `tmux capture-pane -p -t %0` on the live coordinator socket,
#: 2026-09-05, trimmed only in the length of the trailing rule runs. Kept as
#: captured bytes rather than reconstructed, so this test cannot drift into
#: agreeing with a rendering we invented.
CAPTURED_BOX_PANE = (
    " Execute                                                  "
    "│○ workflow-crash-interjection (1 effects)        │\n"
    "┌ Input (Enter, Esc, Ctrl+G pause, Ctrl+T inbox, Ctrl+C cl"
    "└" + "─" * 49 + "┘" + "─" * 11 + "\n"
    "│  Type / for commands\n"
    "│\n"
    "│\n"
    "└" + "─" * 119 + "\n"
)

ASCII_PANE = (
    "[idle] Enter: submit • Esc: cancel • Ctrl+G: pause\n"
    + "-" * 60 + "\n"
    "› \n"
    + "-" * 60 + "\n"
    "Session: s (fake-inert)  |  Ctx: 0%\n"
)


class TheShippedBoxDrawingLayout(unittest.TestCase):
    def test_an_empty_box_is_detected_and_reads_empty(self) -> None:
        """THE REGRESSION. Before the fix this returned None on the same bytes."""
        composer = ohm.parse_composer(CAPTURED_BOX_PANE)
        self.assertIsNotNone(composer, "the live pane must parse as a composer")
        self.assertTrue(composer.is_empty)

    def test_the_placeholder_is_kept_as_evidence_rather_than_normalised_away(self) -> None:
        composer = ohm.parse_composer(CAPTURED_BOX_PANE)
        self.assertEqual("Type / for commands", composer.text)

    def test_an_occupied_box_is_not_empty_so_the_relay_still_refuses(self) -> None:
        """The load-bearing safety property: never type into somebody's draft."""
        occupied = CAPTURED_BOX_PANE.replace(
            "Type / for commands", "somebody else's unsent draft"
        )
        composer = ohm.parse_composer(occupied)
        self.assertIsNotNone(composer)
        self.assertFalse(composer.is_empty)
        self.assertEqual("somebody else's unsent draft", composer.text)

    def test_a_draft_made_only_of_border_characters_stays_occupied(self) -> None:
        """Greedy border stripping would reduce this to '' and license typing."""
        occupied = CAPTURED_BOX_PANE.replace("Type / for commands", "│││|||")
        self.assertFalse(ohm.parse_composer(occupied).is_empty)


class TheOlderHyphenLayoutStillWorks(unittest.TestCase):
    def test_an_empty_box_reads_empty_and_its_state_marker_is_parsed(self) -> None:
        composer = ohm.parse_composer(ASCII_PANE)
        self.assertIsNotNone(composer)
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

        This file already records that text probes flip between Orc builds. If
        the placeholder string changes again, an EMPTY box stops reading empty
        and the relay refuses -- costing a delivery. It must never fail the other
        way, where an occupied box reads empty and gets typed into.
        """
        renamed = CAPTURED_BOX_PANE.replace(
            "Type / for commands", "Press / for commands"
        )
        composer = ohm.parse_composer(renamed)
        self.assertIsNotNone(composer, "detection is structural and must still work")
        self.assertFalse(
            composer.is_empty,
            "a placeholder we no longer recognise must read as occupied, so the "
            "relay refuses rather than typing into what might be a real draft",
        )


if __name__ == "__main__":
    unittest.main()
