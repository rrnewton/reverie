#!/usr/bin/env python3
"""Tests for the shared review-label contract."""

from __future__ import annotations

from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub"))
import review_contract  # noqa: E402


class ReviewContractTests(unittest.TestCase):
    def test_exact_round_labels_are_closed_and_parseable(self) -> None:
        expected = {
            "codex": tuple(f"adversarial-review-codex{value}" for value in range(1, 5)),
            "claude": tuple(f"adversarial-review-claude{value}" for value in range(1, 5)),
        }
        for family, labels in expected.items():
            self.assertEqual(review_contract.round_labels(family), labels)
            for round_number, label in zip(review_contract.REVIEW_ROUNDS, labels):
                with self.subTest(label=label):
                    self.assertEqual(
                        review_contract.parse_round_label(label),
                        (family, round_number),
                    )

    def test_non_labels_and_out_of_range_rounds_are_rejected(self) -> None:
        for label in (
            "review-round-codex",
            "adversarial-review-codex",
            "adversarial-review-codex0",
            "adversarial-review-codex5",
            "adversarial-review-claude10",
        ):
            with self.subTest(label=label):
                self.assertIsNone(review_contract.parse_round_label(label))

    def test_repository_requirement_contains_every_writer_label(self) -> None:
        required = set(review_contract.required_repository_labels())
        self.assertIn(review_contract.POST_FACTO_LABEL, required)
        for family in review_contract.REVIEW_FAMILIES:
            self.assertIn(review_contract.PASSED_REVIEW_LABELS[family], required)
            self.assertTrue(set(review_contract.round_labels(family)) <= required)

        inventories = {repo: set(required) for repo in review_contract.SUPPORTED_REPOS}
        self.assertEqual(
            review_contract.repository_label_gaps(inventories.__getitem__),
            (),
        )
        inventories["rrnewton/reverie"].remove("adversarial-review-codex4")
        self.assertEqual(
            review_contract.repository_label_gaps(inventories.__getitem__),
            ("rrnewton/reverie:adversarial-review-codex4",),
        )

    def test_lint_records_are_derived_from_the_same_values(self) -> None:
        records = review_contract.lint_records()
        self.assertEqual(records[0], f"post-facto\t{review_contract.POST_FACTO_LABEL}")
        for family, record in zip(review_contract.REVIEW_FAMILIES, records[1:]):
            self.assertEqual(
                record,
                "\t".join(
                    (
                        family,
                        review_contract.PASSED_REVIEW_LABELS[family],
                        ",".join(review_contract.round_labels(family)),
                    )
                ),
            )


if __name__ == "__main__":
    unittest.main()
