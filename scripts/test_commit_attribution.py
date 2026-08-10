#!/usr/bin/env python3
"""Both-direction tests for machine-bearing commit attribution."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts" / "check_commit_attribution.py"
DISPATCHER = ROOT / ".githooks" / "dev-hermit-hook-dispatch"
INSTALLER = ROOT / "scripts" / "setup-hooks.sh"
TRAILER_030 = "[impl agent, gpt-5.6-sol] [hermit-902, devbig030]"
TRAILER_014 = "[coordinator, claude-opus-5] [hermit-coord, devbig014]"
QUERY_030 = (
    r"^\[(impl agent|adversarial-reviewer agent|coordinator|Human), [^][]+\] "
    r"\[[^][]+, devbig030\]$"
)


def run(*args: str, cwd: Path | None = None, env: dict[str, str] | None = None):
    merged = dict(os.environ, **(env or {}))
    return subprocess.run(
        args, cwd=cwd, env=merged, text=True, capture_output=True, check=False
    )


def tagged(subject: str, trailer: str = TRAILER_030) -> str:
    return f"{subject}\n\n{trailer}\n"


class MessageValidationTests(unittest.TestCase):
    def check(self, message: str, *extra: str):
        return subprocess.run(
            (sys.executable, str(CHECKER), "--stdin", *extra),
            input=message,
            text=True,
            capture_output=True,
            check=False,
            cwd=ROOT,
            env=dict(os.environ, HERMIT_COMMIT_MACHINE="devbig030"),
        )

    def test_positive_exact_machine_is_accepted(self) -> None:
        result = self.check(tagged("change"), "--expected-machine", "auto")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("machines=devbig030", result.stdout)

    def test_positive_other_machine_is_valid_when_preserving_provenance(self) -> None:
        result = self.check(tagged("change", TRAILER_014))
        self.assertEqual(0, result.returncode, result.stderr)

    def test_negative_wrong_writer_machine_is_refused(self) -> None:
        result = self.check(tagged("change", TRAILER_014), "--expected-machine", "auto")
        self.assertEqual(1, result.returncode)
        self.assertIn("claims machine devbig014", result.stderr)

    def test_negative_missing_machine_field_is_refused(self) -> None:
        result = self.check("change\n\n[impl agent, model] [hermit-902]\n")
        self.assertEqual(1, result.returncode)
        self.assertIn("final commit line must be", result.stderr)

    def test_negative_machine_embedded_in_team_slug_is_refused(self) -> None:
        result = self.check("change\n\n[impl agent, model] [hermit-902-devbig030]\n")
        self.assertEqual(1, result.returncode)

    def test_negative_attribution_not_final_is_refused(self) -> None:
        result = self.check(tagged("change") + "Validation: pass\n")
        self.assertEqual(1, result.returncode)
        self.assertIn("Validation: pass", result.stderr)

    def test_positive_trailing_editor_comments_do_not_hide_the_final_line(self) -> None:
        result = self.check(tagged("change") + "\n# Changes to be committed:\n")
        self.assertEqual(0, result.returncode, result.stderr)


class RepositoryRangeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name)
        run("git", "init", "-q", "-b", "main", cwd=self.repo)
        run("git", "config", "user.name", "test", cwd=self.repo)
        run("git", "config", "user.email", "test@example.invalid", cwd=self.repo)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def commit(self, subject: str, trailer: str | None) -> str:
        path = self.repo / f"f-{subject}"
        path.write_text(subject)
        run("git", "add", "--", path.name, cwd=self.repo)
        args = ["git", "-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", subject]
        if trailer is not None:
            args.extend(("-m", trailer))
        result = run(*args, cwd=self.repo)
        self.assertEqual(0, result.returncode, result.stderr)
        return run("git", "rev-parse", "HEAD", cwd=self.repo).stdout.strip()

    def test_range_refuses_one_missing_commit(self) -> None:
        base = self.commit("base", TRAILER_014)
        self.commit("good", TRAILER_030)
        self.commit("bad", None)
        result = run(
            sys.executable,
            str(CHECKER),
            "--repo",
            str(self.repo),
            "--range",
            f"{base}..HEAD",
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("bad", run("git", "log", "-1", "--format=%s", cwd=self.repo).stdout)

    def test_range_does_not_retroactively_reject_a_historical_author(self) -> None:
        base = self.commit("base", TRAILER_014)
        self.commit("collaborator", TRAILER_030)
        result = run(
            sys.executable,
            str(CHECKER),
            "--repo",
            str(self.repo),
            "--range",
            f"{base}..HEAD",
        )
        self.assertEqual(0, result.returncode, result.stderr)

    def test_git_query_separates_devbig030_from_devbig014(self) -> None:
        sha_014 = self.commit("other-team", TRAILER_014)
        sha_030 = self.commit("our-team", TRAILER_030)
        result = run(
            "git",
            "log",
            "--all",
            "--extended-regexp",
            f"--grep={QUERY_030}",
            "--format=%H",
            cwd=self.repo,
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual([sha_030], result.stdout.splitlines())
        self.assertNotIn(sha_014, result.stdout)


class HookAndInstallerTests(unittest.TestCase):
    def hooked_repo(self, repo: Path, email: str) -> None:
        run("git", "init", "-q", "-b", "main", cwd=repo)
        run("git", "config", "user.name", "Ryan Newton", cwd=repo)
        run("git", "config", "user.email", email, cwd=repo)
        link = repo / ".git" / "hooks" / "commit-msg"
        link.symlink_to(DISPATCHER)

    def real_commit(self, repo: Path, *extra: str):
        path = repo / "tracked"
        path.write_text("content\n")
        run("git", "add", "--", path.name, cwd=repo)
        return run(
            "git",
            "commit",
            "-q",
            *extra,
            "-m",
            tagged("identity bracket"),
            cwd=repo,
            env={"HERMIT_COMMIT_MACHINE": "devbig030"},
        )

    def test_real_commit_accepts_owner_approved_oss_email(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "rrnewton@gmail.com")
            result = self.real_commit(repo)
            self.assertEqual(0, result.returncode, result.stderr)
            author = run("git", "show", "-s", "--format=%ae", cwd=repo)
            self.assertEqual("rrnewton@gmail.com", author.stdout.strip())

    def test_real_commit_refuses_fabricated_repo_email(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "rrnewton@meta.com")
            result = self.real_commit(repo)
            self.assertEqual(1, result.returncode)
            self.assertIn("author email 'rrnewton@meta.com'", result.stderr)
            self.assertIn("owner-approved", result.stderr)

    def test_real_commit_refuses_fabricated_explicit_author(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "rrnewton@gmail.com")
            result = self.real_commit(
                repo, "--author=Ryan Newton <rrnewton@meta.com>"
            )
            self.assertEqual(1, result.returncode)
            self.assertIn("author email 'rrnewton@meta.com'", result.stderr)

    def test_dispatcher_refuses_a_wrong_machine_before_repo_hooks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            run("git", "init", "-q", cwd=repo)
            link = repo / "commit-msg"
            link.symlink_to(DISPATCHER)
            message = repo / "message"
            message.write_text(tagged("change", TRAILER_014))
            result = run(
                str(link),
                str(message),
                cwd=repo,
                env={"HERMIT_COMMIT_MACHINE": "devbig030"},
            )
            self.assertEqual(1, result.returncode)
            self.assertIn("claims machine devbig014", result.stderr)

    def test_installer_covers_parent_and_submodule_common_dirs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "scripts").mkdir()
            (root / ".githooks").mkdir()
            shutil.copy2(CHECKER, root / "scripts" / CHECKER.name)
            shutil.copy2(INSTALLER, root / "scripts" / INSTALLER.name)
            shutil.copy2(DISPATCHER, root / ".githooks" / DISPATCHER.name)
            for path in (
                root / "scripts" / CHECKER.name,
                root / "scripts" / INSTALLER.name,
                root / ".githooks" / DISPATCHER.name,
            ):
                path.chmod(0o755)
            run("git", "init", "-q", "-b", "main", cwd=root)
            child = root / "child"
            run("git", "init", "-q", "-b", "main", str(child), cwd=root)
            run(
                "git", "config", "-f", ".gitmodules", "submodule.child.path", "child",
                cwd=root,
            )
            for repo in (root, child):
                (repo / ".githooks").mkdir(exist_ok=True)
                local_hook = repo / ".githooks" / "pre-commit"
                local_hook.write_text("#!/bin/sh\necho delegated\n")
                local_hook.chmod(0o755)

            installed = run(
                str(root / "scripts" / INSTALLER.name),
                cwd=root,
                env={"HERMIT_COMMIT_MACHINE": "devbig030"},
            )
            self.assertEqual(0, installed.returncode, installed.stderr)
            self.assertIn("repositories=2", installed.stdout)
            for repo in (root, child):
                configured = run(
                    "git", "config", "--get", "core.hooksPath", cwd=repo
                ).stdout.strip()
                self.assertTrue(configured.endswith("/dev-hermit-hooks"), configured)
                delegated = run(str(Path(configured) / "pre-commit"), cwd=repo)
                self.assertEqual("delegated", delegated.stdout.strip())
                checked = run(
                    str(root / "scripts" / INSTALLER.name),
                    "--check",
                    cwd=root,
                    env={"HERMIT_COMMIT_MACHINE": "devbig030"},
                )
                self.assertEqual(0, checked.returncode, checked.stderr)


if __name__ == "__main__":
    unittest.main()
