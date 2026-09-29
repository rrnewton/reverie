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
* the protection: a registration captured at creation into the checkout's OWN
  Git directory, applied through an `includeIf.gitdir` override in the run's
  environment, keeps that run at ` ` through a peer's deinit, and reaches no
  other repository, worktree or fixture;
* the post-run report, and restore commands that name the checkout's own common
  Git directory even when the launch source was a separate clone;
* how `start_unit.submodule_registration_gaps` reads `git submodule status`.

The whole fixture lives beneath a directory whose name contains every wildmatch
metacharacter, so a path pattern that is not escaped fails to match here.
"""

from __future__ import annotations

import contextlib
import io
import os
import shlex
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
        # `*`, `?`, `[` and `\` are wildmatch syntax in an includeIf.gitdir
        # pattern; an unescaped `[x]` matches only the single character `x`.
        self.root = Path(temporary.name) / "w*?[x]\\"
        self.root.mkdir()
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

    def gaps(
        self,
        checkout: Path,
        registration: start_unit.SubmoduleRegistration | None = None,
    ) -> list[tuple[str, str]]:
        return start_unit.submodule_registration_gaps(
            checkout,
            run=start_unit.run_command,
            environment=None if registration is None else registration.environment(),
        )

    def capture(self, checkout: Path) -> start_unit.SubmoduleRegistration:
        return start_unit.capture_submodule_registration(
            checkout, run=start_unit.run_command
        )

    def report(
        self,
        checkout: Path,
        registration: start_unit.SubmoduleRegistration | None = None,
    ) -> tuple[list[tuple[str, str]], str]:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            reported = start_unit.report_submodule_registration_loss(
                checkout, run=start_unit.run_command, registration=registration
            )
        return reported, stderr.getvalue()

    @staticmethod
    def restore_commands(message: str) -> list[list[str]]:
        prefix = "validate-run:   restore: "
        return [
            shlex.split(line[len(prefix):])
            for line in message.splitlines()
            if line.startswith(prefix)
        ]

    def test_deinit_in_one_linked_worktree_unregisters_a_populated_peer(self) -> None:
        self.assertIn('[submodule "dep"]', self.common_config.read_text())

        self.git(self.scratch, "submodule", "deinit", "-q", "-f", "dep")

        self.assertTrue(
            (self.running / "dep" / "file.txt").is_file(),
            "the peer's populated tree is untouched; only its registration went",
        )
        self.assertEqual([("dep", "not initialized")], self.gaps(self.running))
        self.assertNotIn('[submodule "dep"]', self.common_config.read_text())

        # Without a captured registration (a run launched by older tooling) the
        # run's own view IS the shared one, so the loss reached the run.
        reported, message = self.report(self.running)
        self.assertEqual([("dep", "not initialized")], reported)
        self.assertIn("SUBMODULE-REGISTRATION-LOST-DURING-RUN", message)
        self.assertIn(str(self.common_config.resolve()), message)
        self.assertIn(
            "git submodule init -- dep   (in a work tree whose common Git directory "
            f"is {(self.primary / '.git').resolve()})",
            message,
        )

        self.git(self.primary, "submodule", "init", "-q", "--", "dep")
        self.assertEqual([], self.gaps(self.running), "the printed remedy restores it")

    def test_a_captured_registration_keeps_the_run_registered_through_a_peer_deinit(
        self,
    ) -> None:
        """Finding: the creation gate cannot catch a deinit that happens mid-run.

        The capture happens at creation; the peer's deinit happens afterwards,
        while the run is in progress. The run's own Git processes -- here the
        exact `git submodule status` hermit's check.lint_checks reads -- must
        still see ` `, while the unprotected view of the same checkout shows
        the loss.
        """
        registration = self.capture(self.running)
        own_git_dir = Path(
            self.git(self.running, "rev-parse", "--absolute-git-dir").stdout.strip()
        ).resolve()
        self.assertEqual(own_git_dir, registration.git_dir)
        self.assertEqual(own_git_dir, registration.file.parent)
        self.assertNotEqual(
            (self.primary / ".git").resolve(),
            registration.git_dir,
            "the captured file must live in the checkout's PRIVATE Git directory",
        )
        self.assertEqual(
            (
                ("submodule.dep.url", str(self.root / "dep")),
                ("submodule.dep.active", "true"),
            ),
            registration.entries,
        )
        shared_before = self.common_config.read_bytes()

        self.git(self.scratch, "submodule", "deinit", "-q", "-f", "dep")

        self.assertEqual([("dep", "not initialized")], self.gaps(self.running))
        self.assertEqual([], self.gaps(self.running, registration))
        lint_view = subprocess.run(
            ["git", "submodule", "status"],
            cwd=self.running,
            env=registration.environment(),
            capture_output=True,
            text=True,
            check=True,
        )
        self.assertTrue(
            lint_view.stdout.startswith(" "),
            f"the run must still read ` dep`, got {lint_view.stdout!r}",
        )
        self.assertNotIn(
            '[submodule "dep"]',
            self.common_config.read_text(),
            "the protection writes nothing shared: other worktrees keep the loss",
        )
        self.assertNotEqual(shared_before, self.common_config.read_bytes())

    def test_a_captured_registration_reaches_no_other_repository(self) -> None:
        """The include must match only the captured checkout's own Git directory.

        A peer worktree of the same repository, and a separate repository that
        reuses the submodule name -- as hermit's pre-push fixtures reuse
        `agent-utils` -- must keep their own, unregistered view.
        """
        # A POPULATED peer: an unpopulated one reads `-` whatever its config says.
        peer = self.linked_worktree("peer")
        registration = self.capture(self.running)
        self.git(self.scratch, "submodule", "deinit", "-q", "-f", "dep")
        fixture = self.root / "fixture"
        self.git(self.root, "clone", "-q", str(self.primary), str(fixture))
        self.assertEqual([("dep", "not initialized")], self.gaps(fixture))
        self.assertEqual([("dep", "not initialized")], self.gaps(peer))

        self.assertEqual([("dep", "not initialized")], self.gaps(fixture, registration))
        self.assertEqual([("dep", "not initialized")], self.gaps(peer, registration))
        nested = subprocess.run(
            ["git", "config", "--get", "submodule.dep.url"],
            cwd=self.running / "dep",
            env=registration.environment(),
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(
            1, nested.returncode, "the submodule's own repository must not match either"
        )

    def test_a_shared_loss_the_run_survived_is_reported_with_executable_restores(
        self,
    ) -> None:
        """The restore must target the checkout's common Git directory.

        On the wrkslots path the launch source may be a SEPARATE CLONE that
        only supplies objects; the validation checkout is a linked worktree of
        the managed repository. A remedy run in the separate clone registers
        the submodule in a config the checkout never reads.
        """
        separate_clone = self.root / "source-clone"
        self.git(self.root, "clone", "-q", str(self.primary), str(separate_clone))
        clone_config = (separate_clone / ".git" / "config").read_bytes()
        registration = self.capture(self.running)
        self.git(self.scratch, "submodule", "deinit", "-q", "-f", "dep")

        reported, message = self.report(self.running, registration)

        self.assertEqual([("dep", "not initialized")], reported)
        self.assertIn(
            "SUBMODULE-REGISTRATION-REMOVED-FROM-SHARED-CONFIG-DURING-RUN", message
        )
        self.assertNotIn("SUBMODULE-REGISTRATION-LOST-DURING-RUN", message)
        self.assertIn(str(registration.file), message)
        commands = self.restore_commands(message)
        common = (self.primary / ".git").resolve()
        self.assertEqual(
            [
                ["git", f"--git-dir={common}", "config", "submodule.dep.url", str(self.root / "dep")],
                ["git", f"--git-dir={common}", "config", "submodule.dep.active", "true"],
            ],
            commands,
        )
        self.assertNotIn(str(separate_clone), message)

        for command in commands:
            subprocess.run(command, cwd=self.root, check=True, capture_output=True)

        self.assertEqual([], self.gaps(self.running), "the printed restore repairs it")
        self.assertEqual(clone_config, (separate_clone / ".git" / "config").read_bytes())
        self.assertEqual(([], ""), self.report(self.running, registration))

    def test_a_loss_inside_the_run_view_is_named_as_reaching_the_run(self) -> None:
        registration = self.capture(self.running)
        registration.file.unlink()
        self.git(self.scratch, "submodule", "deinit", "-q", "-f", "dep")

        reported, message = self.report(self.running, registration)

        self.assertEqual([("dep", "not initialized")], reported)
        self.assertIn("SUBMODULE-REGISTRATION-LOST-DURING-RUN", message)
        self.assertIn(str(registration.file), message)
        self.assertEqual(2, len(self.restore_commands(message)))

    def test_a_later_attach_reloads_the_captured_registration(self) -> None:
        registration = self.capture(self.running)

        self.assertEqual(
            registration,
            start_unit.load_submodule_registration(
                self.running, run=start_unit.run_command
            ),
        )
        self.assertIsNone(
            start_unit.load_submodule_registration(
                self.scratch, run=start_unit.run_command
            ),
            "a checkout without a captured file reports the shared view alone",
        )

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


class RegistrationFileTest(unittest.TestCase):
    def test_special_names_and_values_round_trip_through_git(self) -> None:
        entries = [
            ('submodule.odd "name" \\ here.url', 'https://h/a "b"\\c\td'),
            ('submodule.odd "name" \\ here.active', "true"),
            ("submodule.plain.url", "../relative.git"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            written = Path(directory) / "registration"
            written.write_text(
                start_unit.render_submodule_registration(entries), encoding="utf-8"
            )
            with mock.patch.dict(os.environ, ISOLATED_GIT_ENV):
                parsed = start_unit.read_submodule_registration(
                    Path(directory), run=start_unit.run_command, config_file=written
                )
        self.assertEqual(entries, parsed)

    def test_a_name_git_cannot_store_is_refused(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "cannot write"):
            start_unit.render_submodule_registration([("submodule.a\nb.url", "x")])

    def test_the_include_pattern_escapes_every_wildmatch_metacharacter(self) -> None:
        self.assertEqual(
            "/a\\*b\\?c\\[d]\\\\e", start_unit.wildmatch_literal("/a*b?c[d]\\e")
        )

    def test_overrides_append_after_the_callers_own(self) -> None:
        merged = start_unit.with_git_config(
            {"GIT_CONFIG_COUNT": "2", "GIT_CONFIG_KEY_0": "a.b", "GIT_CONFIG_VALUE_0": "1"},
            [("c.d", "2")],
        )
        self.assertEqual("3", merged["GIT_CONFIG_COUNT"])
        self.assertEqual(("c.d", "2"), (merged["GIT_CONFIG_KEY_2"], merged["GIT_CONFIG_VALUE_2"]))
        self.assertEqual("a.b", merged["GIT_CONFIG_KEY_0"])
        with self.assertRaisesRegex(ValueError, "GIT_CONFIG_COUNT"):
            start_unit.with_git_config({"GIT_CONFIG_COUNT": "x"}, [("c.d", "2")])


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
        # Record rather than raise: the report swallows every exception it
        # meets, so a raising runner would pass even if the probe ran.
        calls: list[list[str]] = []

        def run(command, **_kwargs):
            calls.append(list(command))
            return subprocess.CompletedProcess(command, 0, f"-{'a' * 40} dep\n", "")

        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            reported = start_unit.report_submodule_registration_loss(
                Path("/nonexistent/checkout"), run=run
            )
        self.assertEqual([], reported)
        self.assertEqual([], calls)
        self.assertEqual("", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
