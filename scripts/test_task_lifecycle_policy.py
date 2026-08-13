#!/usr/bin/env python3
"""Keep the parent instructions aligned with author-owned landing."""

from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class TaskLifecyclePolicyTest(unittest.TestCase):
    def test_canonical_policy_keeps_work_open_until_landing(self) -> None:
        policy = " ".join((ROOT / "AGENTS.md").read_text(encoding="utf-8").split())
        for required in (
            "Publishing a pull request or durable artifact is implementation evidence, not completion",
            "keep the task `in_progress`",
            "The author retains ownership through review, rebases, qualifying validation, landing",
            "After the target `main` contains the change, use `./ci-hub/bin/close-task`",
            "do not close through a raw status update",
        ):
            self.assertIn(required, policy)

    def test_coordinator_summary_agrees(self) -> None:
        skill = " ".join(
            (ROOT / ".claude" / "skills" / "hermit-coord" / "SKILL.md")
            .read_text(encoding="utf-8")
            .split()
        )
        self.assertIn("the author keeps ownership through landing", skill)
        self.assertIn("keeps the task `in_progress`", skill)
        self.assertIn("Do not close at publication", skill)


if __name__ == "__main__":
    unittest.main()
