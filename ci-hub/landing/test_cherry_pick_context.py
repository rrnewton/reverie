#!/usr/bin/env python3
"""Two-sided brackets for the cherry-pick disclosure/context-review gate."""

from __future__ import annotations

import importlib.util
import io
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("cherry_pick_context.py")
SPEC = importlib.util.spec_from_file_location("cherry_pick_context", MODULE_PATH)
assert SPEC and SPEC.loader
gate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gate
SPEC.loader.exec_module(gate)

HEAD = "b" * 40
SOURCE = "a" * 40
CARRIED = "c" * 40


def payload(*, disclosed: bool, reviewed: bool) -> dict:
    summary = "CPU width declarations only."
    if disclosed:
        summary += (
            f"\n\nCherry-pick-context: {SOURCE} - preserve audit-probe failure "
            "status and stderr before set -e unwinds"
        )
    fixture = {
        "number": 2095,
        "headRefOid": HEAD,
        "body": f"Historical source mentioned outside the section: {SOURCE}\n\n"
        f"## Summary\n\n{summary}\n\n## Validation\n\nfocused tests",
        "labels": [],
        "commits": [
            {
                "oid": CARRIED,
                "messageHeadline": "Preserve failing audit probe evidence",
                "messageBody": f"(cherry picked from commit {SOURCE})",
            }
        ],
        "comments": [],
    }
    comment = f"APPROVED-AT: codex {HEAD}"
    if reviewed:
        sources = {SOURCE: [CARRIED]}
        disclosures = {
            SOURCE: "preserve audit-probe failure status and stderr before set -e unwinds"
        }
        digest = gate.context_digest(sources, disclosures)
        comment += f"\nCHERRY-PICK-CONTEXT-REVIEWED: codex {digest} {HEAD}"
    fixture["comments"] = [{"body": comment}]
    return fixture


class CherryPickContextTests(unittest.TestCase):
    def test_undisclosed_exact_head_approval_does_not_inherit_old_review(self) -> None:
        verdict = gate.classify(payload(disclosed=False, reviewed=False))
        self.assertEqual(len(verdict.units), 1)
        self.assertFalse(verdict.passes)
        self.assertFalse(verdict.units[0].disclosed)
        self.assertEqual(verdict.units[0].missing_review_lanes, ("codex",))

        output = io.StringIO()
        with redirect_stdout(output):
            gate._print(verdict, as_json=False)
        self.assertIn(
            "units=1 disclosed=0 context-reviewed=0 verdict=REFUSED",
            output.getvalue(),
        )
        self.assertIn("CONTEXT-CONTRACT-MISSING", output.getvalue())
        self.assertNotIn("code is wrong", output.getvalue().lower())

    def test_disclosed_and_reviewed_cherry_pick_passes(self) -> None:
        verdict = gate.classify(payload(disclosed=True, reviewed=True))
        self.assertTrue(verdict.passes)
        self.assertEqual(len(verdict.units), 1)
        self.assertTrue(verdict.units[0].disclosed)
        self.assertEqual(verdict.units[0].reviewed_lanes, ("codex",))

        output = io.StringIO()
        with redirect_stdout(output):
            gate._print(verdict, as_json=False)
        self.assertIn(
            "units=1 disclosed=1 context-reviewed=1 verdict=PASS",
            output.getvalue(),
        )

    def test_approval_uses_the_shared_marker_parser(self) -> None:
        fixture = payload(disclosed=True, reviewed=True)
        original = gate.approval_binding.parse_marker
        try:
            gate.approval_binding.parse_marker = lambda _line: None
            with self.assertRaises(gate.Unverifiable):
                gate.classify(fixture)
        finally:
            gate.approval_binding.parse_marker = original

    def test_shared_approval_decoration_and_code_block_rules_apply(self) -> None:
        fixture = payload(disclosed=True, reviewed=True)
        digest = gate.context_digest(
            {SOURCE: [CARRIED]},
            {
                SOURCE: "preserve audit-probe failure status and stderr before set -e unwinds"
            },
        )
        fixture["comments"] = [
            {
                "body": (
                    f"## APPROVED-AT: codex {HEAD} BY reviewer\n"
                    f"CHERRY-PICK-CONTEXT-REVIEWED: codex {digest} {HEAD}"
                )
            }
        ]
        self.assertTrue(gate.classify(fixture).passes)

        fixture["comments"] = [
            {
                "body": (
                    "```text\n"
                    f"APPROVED-AT: codex {HEAD}\n"
                    "```\n"
                    f"CHERRY-PICK-CONTEXT-REVIEWED: codex {digest} {HEAD}"
                )
            }
        ]
        verdict = gate.classify(fixture)
        self.assertFalse(verdict.passes)
        self.assertEqual(
            verdict.units[0].missing_review_lanes,
            ("<no-current-review-lane>",),
        )

    def test_disclosure_without_new_context_review_still_refuses(self) -> None:
        verdict = gate.classify(payload(disclosed=True, reviewed=False))
        self.assertFalse(verdict.passes)
        self.assertTrue(verdict.units[0].disclosed)
        self.assertEqual(verdict.units[0].missing_review_lanes, ("codex",))

    def test_non_cherry_pick_is_silent_pass(self) -> None:
        fixture = payload(disclosed=False, reviewed=False)
        fixture["commits"][0]["messageBody"] = "ordinary implementation"
        verdict = gate.classify(fixture)
        self.assertTrue(verdict.passes)
        self.assertEqual(len(verdict.units), 0)

    def test_live_head_must_equal_lander_expected_head(self) -> None:
        with self.assertRaises(gate.Unverifiable):
            gate.classify(
                payload(disclosed=True, reviewed=True), expected_head="d" * 40
            )

    def test_marker_must_share_comment_with_current_approval(self) -> None:
        fixture = payload(disclosed=True, reviewed=True)
        fixture["comments"] = [
            {"body": f"APPROVED-AT: codex {HEAD}"},
            {
                "body": "CHERRY-PICK-CONTEXT-REVIEWED: codex "
                f"{gate.context_digest({SOURCE: [CARRIED]}, {SOURCE: 'preserve audit-probe failure status and stderr before set -e unwinds'})} {HEAD}"
            },
        ]
        verdict = gate.classify(fixture)
        self.assertFalse(verdict.passes)

    def test_stale_context_marker_does_not_transfer_to_current_head(self) -> None:
        fixture = payload(disclosed=True, reviewed=True)
        stale = "d" * 40
        digest = gate.context_digest(
            {SOURCE: [CARRIED]},
            {
                SOURCE: "preserve audit-probe failure status and stderr before set -e unwinds"
            },
        )
        fixture["comments"][0]["body"] = (
            f"APPROVED-AT: codex {HEAD}\n"
            f"CHERRY-PICK-CONTEXT-REVIEWED: codex {digest} {stale}"
        )
        verdict = gate.classify(fixture)
        self.assertFalse(verdict.passes)

    def test_pr_body_semantics_change_invalidates_context_review(self) -> None:
        fixture = payload(disclosed=True, reviewed=True)
        fixture["body"] = fixture["body"].replace(
            "preserve audit-probe failure status and stderr",
            "change audit-probe behavior",
        )
        verdict = gate.classify(fixture)
        self.assertFalse(verdict.passes)

    def test_related_sources_can_share_one_disclosure_and_one_review_digest(self) -> None:
        source_two = "e" * 40
        carried_two = "f" * 40
        fixture = payload(disclosed=True, reviewed=False)
        fixture["commits"].append(
            {
                "oid": carried_two,
                "messageHeadline": "Preserve second probe",
                "messageBody": f"(cherry picked from commit {source_two})",
            }
        )
        fixture["body"] = fixture["body"].replace(
            f"Cherry-pick-context: {SOURCE}",
            f"Cherry-pick-context: {SOURCE}, {source_two}",
        )
        detail = "preserve audit-probe failure status and stderr before set -e unwinds"
        digest = gate.context_digest(
            {SOURCE: [CARRIED], source_two: [carried_two]},
            {SOURCE: detail, source_two: detail},
        )
        fixture["comments"][0]["body"] += (
            f"\nCHERRY-PICK-CONTEXT-REVIEWED: codex {digest} {HEAD}"
        )
        verdict = gate.classify(fixture)
        self.assertTrue(verdict.passes)
        self.assertEqual(len(verdict.units), 2)

    def test_every_relied_upon_review_lane_must_cover_the_unit(self) -> None:
        fixture = payload(disclosed=True, reviewed=True)
        fixture["labels"].append({"name": "passed-review-claude"})
        fixture["comments"].append({"body": f"APPROVED-AT: claude {HEAD}"})
        verdict = gate.classify(fixture)
        self.assertFalse(verdict.passes)
        self.assertEqual(verdict.units[0].reviewed_lanes, ("codex",))
        self.assertEqual(verdict.units[0].missing_review_lanes, ("claude",))

    def test_comment_lanes_are_authority_and_labels_cannot_change_them(self) -> None:
        codex_only = payload(disclosed=True, reviewed=True)
        codex_only["labels"] = []
        without_labels = gate.classify(codex_only)
        self.assertTrue(without_labels.passes)
        self.assertEqual(without_labels.approval_lanes, ("codex",))

        codex_only["labels"] = [{"name": "passed-review-claude"}]
        stale_unrelated_label = gate.classify(codex_only)
        self.assertTrue(stale_unrelated_label.passes)
        self.assertEqual(stale_unrelated_label.approval_lanes, ("codex",))

        both_comments = payload(disclosed=True, reviewed=True)
        both_comments["comments"].append(
            {"body": f"APPROVED-AT: claude {HEAD}"}
        )
        both_comments["labels"] = [{"name": "passed-review-codex"}]
        missing_claude_context = gate.classify(both_comments)
        self.assertFalse(missing_claude_context.passes)
        self.assertEqual(missing_claude_context.approval_lanes, ("claude", "codex"))
        self.assertEqual(
            missing_claude_context.units[0].missing_review_lanes,
            ("claude",),
        )

        both_comments["labels"] = []
        no_labels = gate.classify(both_comments)
        self.assertFalse(no_labels.passes)
        self.assertEqual(no_labels.approval_lanes, ("claude", "codex"))
        self.assertEqual(no_labels.units[0].missing_review_lanes, ("claude",))

    def test_malformed_provenance_is_unverifiable_not_absent(self) -> None:
        fixture = payload(disclosed=True, reviewed=True)
        fixture["commits"][0]["messageBody"] = "(cherry picked from commit short)"
        with self.assertRaises(gate.Unverifiable):
            gate.classify(fixture)


if __name__ == "__main__":
    unittest.main()
