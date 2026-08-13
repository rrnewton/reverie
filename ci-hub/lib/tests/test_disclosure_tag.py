#!/usr/bin/env python3
"""The parser must accept exactly what the formatter writes, and no more."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import agent_identity  # noqa: E402
import disclosure_tag  # noqa: E402


class RoundTrip(unittest.TestCase):
    """The property that keeps the reader from drifting from the writer.

    Every other test here asserts a specific string.  This one asserts the
    RELATIONSHIP, so a future change to the tag format fails here even if
    nobody remembers to update the literals below.
    """

    CASES = (
        ("hermit2", "hermit-903", "unresolved", "devbig030", "impl"),
        ("hermit2", "hermit-bpf-research", "claude-opus-5", "devbig014", "reviewer"),
        ("hermit2", "codex-lsm", "gpt-5", "devbig030", "coordinator"),
        # model is the one field allowed to contain spaces and dots
        ("t", "a", "some model 1.2", "m", "human"),
    )

    def test_parse_inverts_tag(self) -> None:
        for fields in self.CASES:
            with self.subTest(fields=fields):
                original = disclosure_tag.Disclosure(*fields)
                self.assertEqual(disclosure_tag.parse(original.tag), original)

    def test_parse_prefix_inverts_tag_with_a_subject_after_it(self) -> None:
        for fields in self.CASES:
            with self.subTest(fields=fields):
                original = disclosure_tag.Disclosure(*fields)
                subject = f"{original.tag} Do a thing, and another"
                self.assertEqual(disclosure_tag.parse_prefix(subject), original)


class Refusals(unittest.TestCase):
    GOOD = "[hermit2, hermit-903, unresolved, devbig030, role=impl]"

    def test_parses_the_reference_tag(self) -> None:
        """The positive case, stated so the refusals below mean something."""
        parsed = disclosure_tag.parse(self.GOOD)
        self.assertEqual(parsed.agent, "hermit-903")
        self.assertEqual(parsed.role, "impl")

    def test_unknown_role_refused(self) -> None:
        bad = "[hermit2, hermit-903, unresolved, devbig030, role=author]"
        with self.assertRaises(disclosure_tag.DisclosureError):
            disclosure_tag.parse(bad)

    def test_truncated_tag_refused(self) -> None:
        with self.assertRaises(disclosure_tag.DisclosureError):
            disclosure_tag.parse("[hermit2, hermit-903, unresolved]")

    def test_plain_prose_refused(self) -> None:
        with self.assertRaises(disclosure_tag.DisclosureError):
            disclosure_tag.parse("Bind the merge gate to the exact PR head")

    def test_trailing_text_refused_by_parse_but_allowed_by_parse_prefix(self) -> None:
        """`parse` is whole-string; `parse_prefix` is anchored at the start."""
        with self.assertRaises(disclosure_tag.DisclosureError):
            disclosure_tag.parse(f"{self.GOOD} and then some")
        self.assertEqual(
            disclosure_tag.parse_prefix(f"{self.GOOD} and then some").agent,
            "hermit-903",
        )

    def test_a_quoted_tag_mid_subject_is_not_an_attribution(self) -> None:
        """A citation is not a claim.

        A commit that MENTIONS another agent's tag must not be attributed to
        that agent -- the same distinction the closure ancestry audit draws
        between a claim line and a quotation of one.
        """
        subject = f"Revert the change from {self.GOOD}"
        with self.assertRaises(disclosure_tag.DisclosureError):
            disclosure_tag.parse_prefix(subject)


class ClientFamily(unittest.TestCase):
    def test_recognizes_each_family_from_the_binary(self) -> None:
        self.assertEqual(
            agent_identity.client_family(("/usr/local/bin/claude_code/claude", "-p")),
            "claude",
        )
        self.assertEqual(
            agent_identity.client_family(("codex", "--sandbox", "workspace-write")),
            "codex",
        )

    def test_unrecognized_argv_yields_none_not_a_guess(self) -> None:
        self.assertIsNone(agent_identity.client_family(("node", "server.js")))
        self.assertIsNone(agent_identity.client_family(()))

    def test_family_is_not_inferable_from_an_agent_name(self) -> None:
        """The proxy this tool must never use.

        `hermit-907` is Codex-family.  Nothing in that name says so, which is
        why family is read from the running binary instead.
        """
        self.assertIsNone(agent_identity.client_family(("hermit-907",)))


if __name__ == "__main__":
    unittest.main()
