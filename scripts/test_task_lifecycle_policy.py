#!/usr/bin/env python3
"""Keep the parent task lifecycle bound to close-after-landing policy.

INVERTED 2026-08-08. This file used to assert the opposite: it forbade any text
telling an agent to self-close and REQUIRED the coordinator-only closure
gateway. That policy was unsatisfiable -- the ORC coordinator has no shell and
so could never invoke `./ci-hub/bin/close-task` -- while the generic ORC
dispatch preamble told every agent to self-close on every dispatch. This gate
was therefore actively enforcing the contradiction, and would reject the stock
ORC preamble as a violation.

The owning agent publishes evidence and adds `implemented`, then keeps the task
open while shepherding it. Only after landing does the agent close through
`./ci-hub/bin/close-task`, whose CLOSURE-VERIFIED note discharges landing debt.
"""

from __future__ import annotations

import re
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TEXT_SUFFIXES = {".md", ".py", ".rs", ".sh", ".ts", ".txt", ".yaml", ".yml"}
SCAN_ROOTS = (
    ROOT / "AGENTS.md",
    ROOT / ".claude" / "skills",
    ROOT / ".orc" / "plugins" / "hermit-dev",
    ROOT / "scripts",
    ROOT / "ci-hub",
    ROOT / "agent-utils",
)

# Reinstating any of these would restore the jam: a rule only the coordinator
# can satisfy, addressed to a coordinator that cannot run commands.
FORBIDDEN = (
    re.compile(r"only\s+the\s+coordinator\s+closes", re.IGNORECASE),
    re.compile(
        r"a\s+working\s+agent\s+NEVER\s+moves\s+a\s+task\s+to\s+a\s+terminal\s+status",
        re.IGNORECASE,
    ),
    re.compile(r"leaves?\s+status\s+`?in_progress`?,?\s+and\s+stops?", re.IGNORECASE),
)


def iter_text_files(roots: tuple[Path, ...]):
    this_file = Path(__file__).resolve()
    for root in roots:
        if not root.exists():
            continue
        candidates = (root,) if root.is_file() else root.rglob("*")
        for candidate in candidates:
            if (
                candidate.is_file()
                and candidate.resolve() != this_file
                and candidate.suffix in TEXT_SUFFIXES
                and not {".git", "target", "node_modules", "__pycache__"}
                & set(candidate.parts)
            ):
                yield candidate


def find_forbidden(roots: tuple[Path, ...]) -> list[str]:
    findings: list[str] = []
    for path in iter_text_files(roots):
        text = path.read_text(encoding="utf-8", errors="replace")
        for pattern in FORBIDDEN:
            for match in pattern.finditer(text):
                line = text.count("\n", 0, match.start()) + 1
                findings.append(f"{path}:{line}: {match.group(0)!r}")
    return findings


class TaskLifecyclePolicyTest(unittest.TestCase):
    def test_no_coordinator_only_closure_rule_survives(self) -> None:
        self.assertEqual([], find_forbidden(SCAN_ROOTS))

    def test_planted_coordinator_only_rule_reaches_matcher(self) -> None:
        """NEGATIVE bracket: the matcher is not inert."""
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "stale-policy.md"
            fixture.write_text(
                "Only the coordinator closes tasks, and only through the gateway.\n"
                "The working agent leaves status in_progress and stops.\n",
                encoding="utf-8",
            )
            findings = find_forbidden((fixture,))

        self.assertEqual(2, len(findings))
        self.assertTrue(all("stale-policy.md" in finding for finding in findings))

    def test_current_self_close_wording_is_not_flagged(self) -> None:
        """POSITIVE bracket: the wording the policy now MANDATES must pass.

        Without this, tightening a pattern above could silently start rejecting
        the correct instruction again -- which is exactly the failure this file
        was rewritten to end.
        """
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "good-policy.md"
            fixture.write_text(
                "Post the evidence and add implemented, then keep the task "
                "in_progress while shepherding it. After landing, the owning "
                "agent closes through ci-hub/bin/close-task.\n",
                encoding="utf-8",
            )
            self.assertEqual([], find_forbidden((fixture,)))

    def test_canonical_policy_states_the_agent_closes_its_own_task(self) -> None:
        policy = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
        normalized = " ".join(policy.split())
        for required in (
            # The rule itself.
            "After landing, the owning agent closes its own task",
            "Publishing a pull request or durable artifact is implementation evidence, not the end of the task",
            "Keep the task `in_progress`",
            "Do not bypass the gateway",
            # The evidence half, which the gatekeeper's removal must NOT take
            # with it.
            'tg note <id> "IMPLEMENTED:',
            "`implemented` tag while preserving every existing tag",
            # Landing debt still has an owner and a discharge mechanism.
            "Landing debt rides on the `implemented` tag, never on the status",
            "published but unlanded work is open and `implemented`",
            "CLOSURE-VERIFIED",
            "./ci-hub/bin/close-task",
        ):
            self.assertIn(required, normalized)

    def test_running_consumers_encode_both_directions(self) -> None:
        gateway = (ROOT / "ci-hub/closure/verified_close.py").read_text(
            encoding="utf-8"
        )
        health = (ROOT / "ci-hub/health/operational_health.py").read_text(
            encoding="utf-8"
        )
        self.assertNotIn('landing="implemented-unlanded"', gateway)
        self.assertIn("if not task_has_implemented_tag(task, run=run):", gateway)
        self.assertIn("self.implemented and (self.closed != self.landed)", health)


if __name__ == "__main__":
    unittest.main()
