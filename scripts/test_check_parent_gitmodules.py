#!/usr/bin/env python3
"""Functional checks for the parent submodule policy gate."""

from __future__ import annotations

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts" / "check-parent-gitmodules.sh"
GITLINKS = ("hermit", "reverie", "liteinst2", "hermit_test_ledger")


def git(root: Path, *arguments: str) -> str:
    return subprocess.run(
        ("git", "-C", str(root), *arguments),
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


class ParentGitmodulesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="check-parent-gitmodules-"
        )
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(CHECKER, self.root / "scripts" / CHECKER.name)
        shutil.copy2(ROOT / ".gitmodules", self.root / ".gitmodules")
        (self.root / "agent-utils").symlink_to("hermit/agent-utils")
        git(self.root, "init", "--initial-branch=main")
        git(self.root, "config", "user.email", "test@example.invalid")
        git(self.root, "config", "user.name", "Test")
        (self.root / "seed").write_text("seed\n", encoding="utf-8")
        git(self.root, "add", "seed")
        git(self.root, "commit", "-m", "seed")
        commit = git(self.root, "rev-parse", "HEAD")
        git(self.root, "add", ".gitmodules", "agent-utils")
        for path in GITLINKS:
            git(
                self.root,
                "update-index",
                "--add",
                "--cacheinfo",
                f"160000,{commit},{path}",
            )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def check(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ("bash", str(self.root / "scripts" / CHECKER.name)),
            cwd=self.root,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_accepts_the_canonical_four_gitlinks(self) -> None:
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("four pinned gitlinks", result.stdout)

    def test_refuses_a_missing_gitlink(self) -> None:
        git(self.root, "rm", "--cached", "hermit_test_ledger")
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "hermit_test_ledger is not recorded as a gitlink", result.stderr
        )

    def test_refuses_an_extra_gitlink(self) -> None:
        commit = git(self.root, "rev-parse", "HEAD")
        with (self.root / ".gitmodules").open("a", encoding="utf-8") as stream:
            stream.write(
                '[submodule "unexpected"]\n'
                "\tpath = unexpected\n"
                "\turl = https://example.invalid/unexpected.git\n"
                "\tupdate = checkout\n"
            )
        git(
            self.root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{commit},unexpected",
        )
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("exactly 4 parent submodules", result.stderr)

    def test_refuses_wrong_ledger_metadata(self) -> None:
        contents = (self.root / ".gitmodules").read_text(encoding="utf-8")
        (self.root / ".gitmodules").write_text(
            contents.replace(
                "https://github.com/rrnewton/hermit_test_ledger.git",
                "https://example.invalid/wrong.git",
            ),
            encoding="utf-8",
        )
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("submodule hermit_test_ledger URL", result.stderr)

    def test_refuses_a_regular_file_in_place_of_the_ledger_gitlink(self) -> None:
        git(self.root, "rm", "--cached", "hermit_test_ledger")
        (self.root / "hermit_test_ledger").write_text("not a gitlink\n", encoding="utf-8")
        git(self.root, "add", "hermit_test_ledger")
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("hermit_test_ledger is not recorded as a gitlink", result.stderr)


if __name__ == "__main__":
    unittest.main()
