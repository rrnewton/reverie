#!/usr/bin/env python3
"""Keep production worktree mutations routed through wrkslots."""

from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SEARCH_ROOTS = (
    ROOT / "ci-hub",
    ROOT / "scripts",
    ROOT / "multisect",
    ROOT / "hermit" / "ci",
    ROOT / "hermit" / "scripts",
)
LEGACY_IMPLEMENTATIONS = {
    ROOT / "scripts" / "allocate-worktree.rs",
    ROOT / "scripts" / "release-worktree.rs",
}
TEST_ONLY_GIT_FIXTURES = {
    ROOT / "ci-hub" / "lib" / "landing_lock.rs",
    ROOT / "ci-hub" / "lib" / "validate_lock.rs",
    ROOT / "hermit" / "scripts" / "bisect-probe.rs",
    ROOT / "hermit" / "scripts" / "check-reverie-pin.rs",
}
SOURCE_SUFFIXES = {"", ".py", ".rs", ".sh"}
SHELL_MUTATION = re.compile(
    r"\bgit(?:\s+-C\s+[^\s]+)?\s+worktree\s+(?:add|remove|prune)\b"
)
ARGV_MUTATION = re.compile(
    r"[\"']worktree[\"']\s*,\s*[\"'](?:add|remove|prune)[\"']"
)


def production_sources() -> list[Path]:
    result: list[Path] = []
    for root in SEARCH_ROOTS:
        for path in root.rglob("*"):
            if not path.is_file() or path.suffix not in SOURCE_SUFFIXES:
                continue
            relative = path.relative_to(ROOT)
            name = path.name.lower()
            if (
                "tests" in relative.parts
                or name.startswith(("test_", "test-"))
                or name.endswith(("_test.py", "-test.sh"))
            ):
                continue
            if path in TEST_ONLY_GIT_FIXTURES:
                continue
            result.append(path)
    return sorted(result)


class WorktreeInterfaceTest(unittest.TestCase):
    def test_production_worktree_mutations_use_wrkslots(self) -> None:
        found: list[str] = []
        for path in production_sources():
            if path in LEGACY_IMPLEMENTATIONS:
                # These retain their old implementation only for isolated
                # migration fixtures. In a configured project their main
                # functions refuse before reaching any Git mutation.
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            for pattern in (SHELL_MUTATION, ARGV_MUTATION):
                for match in pattern.finditer(text):
                    line_start = text.rfind("\n", 0, match.start()) + 1
                    line_end = text.find("\n", match.end())
                    line_text = text[line_start : None if line_end < 0 else line_end]
                    if "`git worktree" in line_text:
                        continue
                    line = text.count("\n", 0, match.start()) + 1
                    found.append(f"{path.relative_to(ROOT)}:{line}: {match.group(0)}")

        self.assertEqual(
            [],
            found,
            "direct Git worktree mutation bypasses ownership, time-to-live, salvage, "
            "and append-only recovery evidence. Replace it with ci-hub/bin/wrkslots; "
            "if the code does not need a worktree, remove the worktree operation. "
            "This check guides accidental callers and does not claim a permission boundary.",
        )


if __name__ == "__main__":
    unittest.main()
