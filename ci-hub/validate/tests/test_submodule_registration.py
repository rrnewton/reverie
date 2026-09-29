#!/usr/bin/env python3
"""Submodule registration is shared by every linked worktree of a repository.

Measured 2026-09-29: full validate s17 created its hermit checkout at 02:49:08Z
with agent-utils registered. At 02:50:41Z an agent tidying its own scratch
hermit worktree ran `git submodule deinit -f agent-utils` there, after plain
`git worktree remove` refused a tree containing submodules. `deinit` deletes the
whole `[submodule "agent-utils"]` section from `$GIT_COMMON_DIR/config`, which is
the one file every worktree reads. s17's populated agent-utils then showed `-`
in `git submodule status`, and its check.lint_checks exited 75 (NO_RESULT) with a
remedy that told the reader to initialize a checkout that was already populated.

These tests run real Git in a throwaway repository, isolated from the user's and
the system's config, and pin down:

* the mechanism: `deinit` in one linked worktree unregisters a populated peer;
* the safe release: `git worktree remove --force` leaves the common config's
  submodule sections byte-identical, so releasing a linked worktree must never
  need `deinit`;
* why validate creation could not see this loss: a NEW checkout's
  `submodule update --init` re-registers the section, so the loss is only
  visible to a run that was already in progress;
* how `start_unit.submodule_registration_gaps` reads `git submodule status`.
"""

from __future__ import annotations

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import start_unit  # noqa: E402


ISOLATED_GIT_ENV = {
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
    # A local-path submodule clone uses the file transport, which modern Git
    # refuses by default. GIT_CONFIG_COUNT reaches every nested git process.
    "GIT_CONFIG_COUNT": "1",
    "GIT_CONFIG_KEY_0": "protocol.file.allow",
    "GIT_CONFIG_VALUE_0": "always",
    "GIT_AUTHOR_NAME": "fixture",
    "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
    "GIT_COMMITTER_NAME": "fixture",
    "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
    "GIT_TERMINAL_PROMPT": "0",
}


class SharedSubmoduleRegistrationTest(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="submodule-registration-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        environment = mock.patch.dict(os.environ, ISOLATED_GIT_ENV)
        environment.start()
        self.addCleanup(environment.stop)
        # patch.dict restores the whole environment on stop, so these pops are
        # scoped to the test: an inherited GIT_DIR would retarget every command.
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
            os.environ.pop(name, None)

        dep = self.root / "dep"
        self.git(self.root, "init", "-q", "-b", "main", str(dep))
        (dep / "file.txt").write_text("dependency\n", encoding="utf-8")
        self.git(dep, "add", "file.txt")
        self.git(dep, "commit", "-q", "-m", "dependency")

        self.primary = self.root / "super"
        self.git(self.root, "init", "-q", "-b", "main", str(self.primary))
        self.git(self.primary, "submodule", "add", "-q", str(dep), "dep")
        self.git(self.primary, "commit", "-q", "-m", "add dep")
        self.common_config = self.primary / ".git" / "config"

        self.running = self.linked_worktree("running")
        self.scratch = self.linked_worktree("scratch")
        self.assertEqual([], self.gaps(self.running))
        self.assertEqual([], self.gaps(self.scratch))

    def git(
        self, cwd: Path, *arguments: str, check: bool = True
    ) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            ["git", "-C", str(cwd), *arguments],
            capture_output=True,
            text=True,
            check=False,
        )
        if check and result.returncode != 0:
            self.fail(
                f"git {' '.join(arguments)} exited {result.returncode}: "
                f"{result.stderr.strip()}"
            )
        return result

    def linked_worktree(self, name: str) -> Path:
        path = self.root / name
        self.git(self.primary, "worktree", "add", "-q", "--detach", str(path), "HEAD")
        self.git(path, "submodule", "update", "-q", "--init", "--recursive")
        self.assertTrue((path / "dep" / "file.txt").is_file())
        return path

    def gaps(self, checkout: Path) -> list[tuple[str, str]]:
        return start_unit.submodule_registration_gaps(
            checkout, run=start_unit.run_command
        )

    def test_deinit_in_one_linked_worktree_unregisters_a_populated_peer(self) -> None:
        self.assertIn('[submodule "dep"]', self.common_config.read_text())

        self.git(self.scratch, "submodule", "deinit", "-q", "-f", "dep")

        self.assertTrue(
            (self.running / "dep" / "file.txt").is_file(),
            "the peer's populated tree is untouched; only its registration went",
        )
        self.assertEqual([("dep", "not initialized")], self.gaps(self.running))
        self.assertNotIn('[submodule "dep"]', self.common_config.read_text())

        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            reported = start_unit.report_submodule_registration_loss(
                self.running, run=start_unit.run_command, restore_from=self.primary
            )
        self.assertEqual([("dep", "not initialized")], reported)
        message = stderr.getvalue()
        self.assertIn("SUBMODULE-REGISTRATION-LOST-DURING-RUN", message)
        self.assertIn(str(self.common_config.resolve()), message)
        self.assertIn(f"git -C {self.primary} submodule init -- dep", message)

        self.git(self.primary, "submodule", "init", "-q", "--", "dep")
        self.assertEqual([], self.gaps(self.running), "the printed remedy restores it")

    def test_linked_worktree_release_leaves_common_submodule_sections_untouched(
        self,
    ) -> None:
        before = self.common_config.read_bytes()

        refused = self.git(
            self.primary, "worktree", "remove", str(self.scratch), check=False
        )
        self.assertNotEqual(0, refused.returncode)
        self.assertIn("submodules", refused.stderr)
        self.assertEqual(before, self.common_config.read_bytes())

        self.git(self.primary, "worktree", "remove", "--force", str(self.scratch))

        self.assertFalse(self.scratch.exists())
        self.assertEqual(
            before,
            self.common_config.read_bytes(),
            "releasing a linked worktree must not edit the config its peers share",
        )
        self.assertEqual([], self.gaps(self.running))

    def test_a_new_checkout_re_registers_so_creation_cannot_see_an_earlier_loss(
        self,
    ) -> None:
        self.git(self.scratch, "submodule", "deinit", "-q", "-f", "dep")
        self.assertEqual([("dep", "not initialized")], self.gaps(self.running))

        fresh = self.linked_worktree("fresh")

        self.assertEqual([], self.gaps(fresh))
        self.assertEqual(
            [],
            self.gaps(self.running),
            "re-registration is shared too, so only a loss AFTER creation survives",
        )


class SubmoduleStatusParsingTest(unittest.TestCase):
    def status(self, stdout: str, returncode: int = 0, stderr: str = ""):
        def run(command, **_kwargs):
            self.assertEqual(["git", "-C", "/checkout", "submodule", "status"], command)
            return subprocess.CompletedProcess(command, returncode, stdout, stderr)

        return start_unit.submodule_registration_gaps(Path("/checkout"), run=run)

    def test_every_documented_prefix_and_describe_suffix(self) -> None:
        sha = "d" * 40
        self.assertEqual(
            [
                ("dir with space", "checked out at a different commit than recorded"),
                ("conflicted", "in a merge conflict"),
                ("never", "not initialized"),
            ],
            self.status(
                f" {sha} healthy (heads/main)\n"
                f"+{sha} dir with space (v1.2-3-g{sha[:7]})\n"
                f"U{sha} conflicted\n"
                f"-{sha} never\n"
                "\n"
            ),
        )

    def test_a_leading_space_marks_the_first_entry_healthy(self) -> None:
        self.assertEqual([], self.status(f" {'e' * 40} agent-utils (heads/main)\n"))

    def test_an_unrecognized_prefix_is_an_error_not_a_pass(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "unrecognized"):
            self.status(f"X{'f' * 40} agent-utils\n")

    def test_a_failed_status_is_an_error_not_a_pass(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "fatal: not a git repository"):
            self.status("", returncode=128, stderr="fatal: not a git repository\n")


class PostRunReportTest(unittest.TestCase):
    def test_a_failing_probe_is_named_and_never_raises(self) -> None:
        def run(command, **_kwargs):
            raise ValueError("probe exploded")

        with tempfile.TemporaryDirectory() as checkout:
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                reported = start_unit.report_submodule_registration_loss(
                    Path(checkout), run=run
                )
        self.assertEqual([], reported)
        self.assertIn("could not re-read submodule registration", stderr.getvalue())
        self.assertIn("probe exploded", stderr.getvalue())

    def test_a_removed_checkout_is_not_probed(self) -> None:
        def run(command, **_kwargs):
            raise AssertionError(f"must not run {command}")

        self.assertEqual(
            [],
            start_unit.report_submodule_registration_loss(
                Path("/nonexistent/checkout"), run=run
            ),
        )


if __name__ == "__main__":
    unittest.main()
