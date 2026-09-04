#!/usr/bin/env python3
"""Stale review labels are removed; current-head labels are untouched."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import os
from pathlib import Path
import unittest
from unittest import mock


TARGET = Path(
    os.environ.get(
        "APPROVAL_BINDING_TOOL",
        str(Path(__file__).resolve().parents[1] / "approval_binding.py"),
    )
)


def _load():
    loader = importlib.machinery.SourceFileLoader("approval_binding_reconcile", str(TARGET))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


ab = _load()
OLD = "1" * 40
HEAD = "2" * 40


def row(*, state: str, source: str = "label") -> dict[str, object]:
    return {
        "number": 521,
        "label": "passed-review-codex" if source == "label" else None,
        "source": source,
        "lane": "codex",
        "head": HEAD,
        "bound_to": OLD if state == ab.SUPERSEDED else HEAD,
        "binding_issuer": "reviewer-codex",
        "state": state,
    }


def selected(
    labels: list[str], comments: list[dict[str, str]] | None = None
) -> dict[str, object]:
    return {
        "number": 521,
        "head": HEAD,
        "labels": labels,
        "state": "OPEN",
        "draft": False,
        "change_author": "change-author",
        "comments": comments or [],
    }


class ReviewLabelReconcileTests(unittest.TestCase):
    def test_notice_names_heads_labels_and_owners_through_attributed_writer(self) -> None:
        action = {
            "pr": 521,
            "head": HEAD,
            "labels": ["passed-review-codex"],
            "owners": ["change-author", "reviewer-codex"],
            "bound_to": [OLD],
            "marker": ab.reconciliation_marker(
                "rrnewton/reverie", 521, HEAD, ["passed-review-codex"]
            ),
        }
        with mock.patch.object(ab, "gh_mutate") as mutate:
            ab._post_reconciliation_notice(
                "rrnewton/reverie", action, "health-tick", "hermit2"
            )

        args = mutate.call_args.args[0]
        self.assertEqual(args[:7], [
            "--who", "health-tick", "--team", "hermit2", "--role", "relay",
            "pr",
        ])
        body = args[args.index("--body") + 1]
        self.assertIn(HEAD, body)
        self.assertIn(OLD[:12], body)
        self.assertIn("passed-review-codex", body)
        self.assertIn("agent(change-author)", body)
        self.assertIn("agent(reviewer-codex)", body)
        self.assertIn(action["marker"], body)

    def test_moved_head_removes_label_and_names_affected_owners(self) -> None:
        stale = row(state=ab.SUPERSEDED)
        snapshots = iter(
            [
                ([stale], selected(["passed-review-codex"])),
                ([row(state=ab.SUPERSEDED, source="comment")], selected([])),
            ]
        )
        edits: list[tuple[str, int, str, bool]] = []
        notices: list[tuple[str, dict[str, object]]] = []

        notifications = ab.reconcile_stale_review_labels(
            "rrnewton/reverie",
            [stale],
            snapshot=lambda _repo, _number: next(snapshots),
            edit_label=lambda repo, number, label, add: edits.append(
                (repo, number, label, add)
            ),
            notify=lambda repo, action: notices.append((repo, action)),
        )

        self.assertEqual(
            edits,
            [("rrnewton/reverie", 521, "passed-review-codex", False)],
        )
        self.assertEqual(len(notifications), 1)
        self.assertEqual(notifications[0]["removed"], ["passed-review-codex"])
        self.assertEqual(
            notifications[0]["owners"], ["change-author", "reviewer-codex"]
        )
        self.assertEqual(notifications[0]["bound_to"], [OLD])
        self.assertEqual(notifications[0]["head"], HEAD)
        self.assertEqual(len(notices), 1)
        self.assertEqual(notices[0][0], "rrnewton/reverie")
        self.assertEqual(notices[0][1]["owners"], ["change-author", "reviewer-codex"])

    def test_unchanged_head_does_not_notify_or_mutate(self) -> None:
        current = row(state=ab.BOUND_CURRENT)
        snapshot_calls: list[int] = []
        edits: list[tuple[str, int, str, bool]] = []

        notifications = ab.reconcile_stale_review_labels(
            "rrnewton/reverie",
            [current],
            snapshot=lambda _repo, number: (
                snapshot_calls.append(number),
                ([], None),
            )[1],
            edit_label=lambda repo, number, label, add: edits.append(
                (repo, number, label, add)
            ),
            notify=lambda _repo, _action: self.fail("unchanged head was notified"),
        )

        self.assertEqual(notifications, [])
        self.assertEqual(snapshot_calls, [])
        self.assertEqual(edits, [])

    def test_existing_notice_is_not_posted_again(self) -> None:
        stale = row(state=ab.SUPERSEDED)
        marker = ab.reconciliation_marker(
            "rrnewton/reverie", 521, HEAD, ["passed-review-codex"]
        )
        snapshots = iter(
            [
                (
                    [stale],
                    selected(
                        ["passed-review-codex"],
                        [{"body": f"earlier notice\n{marker}"}],
                    ),
                ),
                ([row(state=ab.SUPERSEDED, source="comment")], selected([])),
            ]
        )
        edits: list[tuple[str, int, str, bool]] = []

        notifications = ab.reconcile_stale_review_labels(
            "rrnewton/reverie",
            [stale],
            snapshot=lambda _repo, _number: next(snapshots),
            edit_label=lambda repo, number, label, add: edits.append(
                (repo, number, label, add)
            ),
            notify=lambda _repo, _action: self.fail("notice was duplicated"),
        )

        self.assertEqual(len(notifications), 1)
        self.assertFalse(notifications[0]["notified"])
        self.assertEqual(
            edits,
            [("rrnewton/reverie", 521, "passed-review-codex", False)],
        )

    def test_notice_failure_prevents_label_removal(self) -> None:
        stale = row(state=ab.SUPERSEDED)
        edits: list[tuple[str, int, str, bool]] = []

        def fail_notice(_repo: str, _action: dict[str, object]) -> None:
            raise ab.GitHubUnavailable("notice failed")

        with self.assertRaisesRegex(ab.GitHubUnavailable, "notice failed"):
            ab.reconcile_stale_review_labels(
                "rrnewton/reverie",
                [stale],
                snapshot=lambda _repo, _number: (
                    [stale],
                    selected(["passed-review-codex"]),
                ),
                edit_label=lambda repo, number, label, add: edits.append(
                    (repo, number, label, add)
                ),
                notify=fail_notice,
            )

        self.assertEqual(edits, [])

    def test_exact_head_approval_racing_removal_restores_label(self) -> None:
        stale = row(state=ab.SUPERSEDED)
        current_comment = row(state=ab.BOUND_CURRENT, source="comment")
        snapshots = iter(
            [
                ([stale], selected(["passed-review-codex"])),
                ([current_comment], selected([])),
                ([row(state=ab.BOUND_CURRENT)], selected(["passed-review-codex"])),
            ]
        )
        edits: list[tuple[str, int, str, bool]] = []
        notices: list[tuple[str, dict[str, object]]] = []

        notifications = ab.reconcile_stale_review_labels(
            "rrnewton/reverie",
            [stale],
            snapshot=lambda _repo, _number: next(snapshots),
            edit_label=lambda repo, number, label, add: edits.append(
                (repo, number, label, add)
            ),
            notify=lambda repo, action: notices.append((repo, action)),
        )

        self.assertEqual(
            edits,
            [
                ("rrnewton/reverie", 521, "passed-review-codex", False),
                ("rrnewton/reverie", 521, "passed-review-codex", True),
            ],
        )
        self.assertEqual(len(notifications), 1)
        self.assertEqual(notifications[0]["removed"], [])
        self.assertEqual(notifications[0]["restored"], ["passed-review-codex"])
        self.assertEqual(len(notices), 1)


if __name__ == "__main__":
    unittest.main()
