#!/usr/bin/env python3
"""The health tick may run across a Hermit gitlink lag only by exact code identity."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
AGENT_TOOL = ROOT / "ci-hub" / "bin" / "agent-tool"


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=repo,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout.strip()


class AgentToolPinEquivalenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="agent-tool-pin-")
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name) / "parent"
        self.hermit = self.parent / "hermit"
        self.agent_utils = self.hermit / "agent-utils"

        self.agent_utils.mkdir(parents=True)
        git(self.agent_utils, "init", "-q")
        git(self.agent_utils, "config", "user.name", "Agent Tool Test")
        git(self.agent_utils, "config", "user.email", "agent-tool@example.invalid")
        (self.agent_utils / "py" / "tick_hub").mkdir(parents=True)
        (self.agent_utils / "py" / "dagrun").mkdir(parents=True)
        (self.agent_utils / "py" / "bin").mkdir(parents=True)
        (self.agent_utils / "py" / "tick_hub" / "__main__.py").write_text(
            "import sys\nprint('tick-hub ran ' + ' '.join(sys.argv[1:]))\n",
            encoding="utf-8",
        )
        (self.agent_utils / "py" / "dagrun" / "__main__.py").write_text(
            "import sys\nprint('dagrun ran ' + ' '.join(sys.argv[1:]))\n",
            encoding="utf-8",
        )
        (self.agent_utils / "py" / "bin" / "tick-hub").symlink_to("../tick_hub/__main__.py")
        (self.agent_utils / "py" / "bin" / "dagrun").symlink_to("../dagrun/__main__.py")
        git(self.agent_utils, "add", "py")
        git(self.agent_utils, "commit", "-qm", "expected tools")
        self.expected_agent_utils = git(self.agent_utils, "rev-parse", "HEAD")

        (self.agent_utils / "unrelated.txt").write_text("new work\n", encoding="utf-8")
        git(self.agent_utils, "add", "unrelated.txt")
        git(self.agent_utils, "commit", "-qm", "change outside tick-hub")
        self.same_tick_agent_utils = git(self.agent_utils, "rev-parse", "HEAD")

        (self.agent_utils / "py" / "tick_hub" / "__main__.py").write_text(
            "import sys\nprint('changed tick-hub ' + ' '.join(sys.argv[1:]))\n",
            encoding="utf-8",
        )
        git(self.agent_utils, "add", "py/tick_hub/__main__.py")
        git(self.agent_utils, "commit", "-qm", "change tick-hub")
        self.changed_tick_agent_utils = git(self.agent_utils, "rev-parse", "HEAD")

        git(self.agent_utils, "checkout", "-q", self.expected_agent_utils)
        git(self.hermit, "init", "-q")
        git(self.hermit, "config", "user.name", "Agent Tool Test")
        git(self.hermit, "config", "user.email", "agent-tool@example.invalid")
        git(self.hermit, "add", "agent-utils")
        git(self.hermit, "commit", "-qm", "record expected agent-utils")
        self.parent_hermit = git(self.hermit, "rev-parse", "HEAD")

        git(self.agent_utils, "checkout", "-q", self.same_tick_agent_utils)
        git(self.hermit, "add", "agent-utils")
        git(self.hermit, "commit", "-qm", "advance agent-utils outside tick-hub")
        self.same_tick_hermit = git(self.hermit, "rev-parse", "HEAD")

        git(self.agent_utils, "checkout", "-q", self.changed_tick_agent_utils)
        git(self.hermit, "add", "agent-utils")
        git(self.hermit, "commit", "-qm", "advance tick-hub")
        self.changed_tick_hermit = git(self.hermit, "rev-parse", "HEAD")

        (self.parent / "ci-hub" / "bin").mkdir(parents=True)
        shutil.copy2(AGENT_TOOL, self.parent / "ci-hub" / "bin" / "agent-tool")
        (self.parent / "agent-utils").symlink_to("hermit/agent-utils")
        git(self.parent, "init", "-q")
        git(self.parent, "config", "user.name", "Agent Tool Test")
        git(self.parent, "config", "user.email", "agent-tool@example.invalid")
        git(self.parent, "add", "ci-hub/bin/agent-tool", "agent-utils")
        git(
            self.parent,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{self.parent_hermit},hermit",
        )
        git(self.parent, "commit", "-qm", "record parent Hermit pin")
        git(self.parent, "update-ref", "refs/remotes/origin/main", "HEAD")

    def _select(self, hermit: str, agent_utils: str) -> None:
        git(self.hermit, "checkout", "-q", hermit)
        git(self.agent_utils, "checkout", "-q", agent_utils)

    def _run(self, tool: str) -> subprocess.CompletedProcess[str]:
        environment = dict(os.environ)
        environment["CI_HUB_PYTHON"] = sys.executable
        return subprocess.run(
            [str(self.parent / "ci-hub" / "bin" / "agent-tool"), tool, "marker"],
            cwd=self.parent,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
        )

    def test_tick_runs_when_its_complete_code_matches_the_parent_pin(self) -> None:
        self._select(self.same_tick_hermit, self.same_tick_agent_utils)

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "tick-hub ran marker\n")
        self.assertIn("complete tracked executable/package is byte-identical", result.stderr)
        self.assertEqual(git(self.hermit, "rev-parse", "HEAD"), self.same_tick_hermit)
        self.assertEqual(
            git(self.agent_utils, "rev-parse", "HEAD"), self.same_tick_agent_utils
        )

    def test_tick_refuses_when_its_code_differs_from_the_parent_pin(self) -> None:
        self._select(self.changed_tick_hermit, self.changed_tick_agent_utils)

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("agent-utils pin mismatch", result.stderr)

    def test_tick_refuses_an_untracked_module_inside_its_package(self) -> None:
        self._select(self.same_tick_hermit, self.same_tick_agent_utils)
        (self.agent_utils / "py" / "tick_hub" / "untracked.py").write_text(
            "raise RuntimeError('not parent-recorded')\n",
            encoding="utf-8",
        )

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("agent-utils pin mismatch", result.stderr)

    def test_actor_still_refuses_even_when_tick_code_matches(self) -> None:
        self._select(self.same_tick_hermit, self.same_tick_agent_utils)

        result = self._run("dagrun")

        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn("agent-utils pin mismatch", result.stderr)

    def test_exact_parent_pin_runs_without_the_tolerance_path(self) -> None:
        self._select(self.parent_hermit, self.expected_agent_utils)

        result = self._run("tick-hub")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "tick-hub ran marker\n")
        self.assertNotIn("byte-identical", result.stderr)


if __name__ == "__main__":
    unittest.main()
