#!/usr/bin/env python3
"""Keep the parent instructions on author-owned closure.

This file used to assert the OPPOSITE. It required `AGENTS.md` to contain
"keep the task `in_progress`" and "do not close through a raw status update",
which is the coordinator-only closure rule the owner superseded on 2026-08-08.
Two things then went wrong at once: the rule was moved out of the live
`AGENTS.md` into the `.skill_reset_20260816/` migration staging area, so the
test's paths went stale; and nothing runs this file, so it failed silently
instead of reporting the drift. Both halves are fixed here -- the assertions are
inverted to forbid the old rule, and the paths point at the files that actually
carry the policy today.

The policy: an agent closes its own task. The implementor adds `implemented`;
whoever lands the change adds `landed`. That keeps "closed but not landed" a
cheap query.

Since 2026-09-28 that whole rule is scoped to ORC. The owner: "Tg is just orcs
short term memory / dispatch. Period." Evidence goes in the pull request,
product defects go to GitHub issues, and pull-request ownership is the pull
request's `Owner:` line. `test_taskgraph_rules_are_scoped_to_orc` keeps the
staging files from reinstating TaskGraph as a mandate for every agent.
"""

from __future__ import annotations

import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

#: Files that state the task-closure policy to agents. `.skill_reset_20260816/`
#: is a live migration staging area, not an archive -- content is ported from it
#: into the live surfaces, so a rule left there is a rule scheduled to come back.
POLICY_FILES = (
    ROOT / ".skill_reset_20260816" / "AGENTS.md",
    ROOT / ".skill_reset_20260816" / "skills" / "hermit-coord" / "SKILL.md",
    ROOT / ".skill_reset_20260816" / "skills" / "backend-reality-reviewer" / "SKILL.md",
)

#: Wording that reinstates coordinator-only closure. Substring match against the
#: whitespace-collapsed file, so a reflowed paragraph cannot smuggle one back in.
FORBIDDEN = (
    "keep the task `in_progress`",
    "keeps the task `in_progress`",
    "do not close through a raw status update",
    "Do not close at publication",
    "Agents MUST NOT close",
    "Only the coordinator closes tasks",
    "Never use raw `tg update --status closed`",
    # Caught only on a second sweep: hermit-coord's summary said "closes only
    # after fresh ancestry confirmation", which is the same rule without any of
    # the words above. Match the deferral, not just its usual phrasing.
    "closes only after",
    "close only after",
    "closed only after",
)


def _collapsed(path: Path) -> str:
    return " ".join(path.read_text(encoding="utf-8").split())


class TaskLifecyclePolicyTest(unittest.TestCase):
    def test_policy_files_exist(self) -> None:
        """A missing path would make every assertion below vacuously pass."""
        for path in POLICY_FILES:
            self.assertTrue(path.is_file(), f"policy file missing: {path}")

    def test_no_file_reinstates_coordinator_only_closure(self) -> None:
        for path in POLICY_FILES:
            policy = _collapsed(path)
            for banned in FORBIDDEN:
                self.assertNotIn(
                    banned,
                    policy,
                    f"{path.relative_to(ROOT)} reinstates the superseded "
                    f"coordinator-only closure rule: {banned!r}",
                )

    def test_canonical_policy_states_author_owned_closure(self) -> None:
        policy = _collapsed(POLICY_FILES[0])
        for required in (
            "close the task yourself with `tg update <id> --status closed`",
            "Closure is not routed through a coordinator",
            "`implemented` is added by the implementor, `landed` by whoever "
            "lands the change",
        ):
            self.assertIn(required, policy)

    def test_coordinator_summary_agrees(self) -> None:
        skill = _collapsed(POLICY_FILES[1])
        self.assertIn("closes its own task", skill)
        self.assertIn("Closure is not routed through the coordinator", skill)
        self.assertIn("whoever lands the change adds `landed`", skill)

    def test_taskgraph_rules_are_scoped_to_orc(self) -> None:
        policy = _collapsed(POLICY_FILES[0])
        for required in (
            "**ORC only.** TaskGraph (`tg`) is ORC's short-term to-do list, not a record",
            "Product defects always go to GitHub issues",
            "the pull request's `Owner:` line",
        ):
            self.assertIn(required, policy)
        # The ORC scope must come BEFORE the tg commands it governs.
        self.assertLess(
            policy.index("**ORC only.**"),
            policy.index("close the task yourself with `tg update <id> --status closed`"),
        )
        for path in POLICY_FILES[1:]:
            self.assertIn("TaskGraph is ORC's", _collapsed(path), str(path))


if __name__ == "__main__":
    unittest.main()
