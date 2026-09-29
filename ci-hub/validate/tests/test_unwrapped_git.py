#!/usr/bin/env python3
"""Tests for running validation's git without the host telemetry wrapper.

Every binary here is a temporary fake shell script: a "wrapper" named `git`
next to a fake `git.meta.real`, and a plain `git` in another directory. The
host's /usr/local/bin is never executed.
"""
from __future__ import annotations

import os
import shlex
import stat
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

VALIDATE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(VALIDATE))
import unwrapped_git as ug  # noqa: E402

META_VERSION = "git version 2.53.0-Meta"
PLAIN_VERSION = "git version 2.52.0"


def write_script(path: Path, body: str, mode: int = 0o755) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"#!/bin/sh\n{body}\n", encoding="utf-8")
    path.chmod(mode)
    return path


def version_script(path: Path, version: str) -> Path:
    return write_script(
        path,
        f'if [ "$1" = --version ]; then echo {shlex.quote(version)}; exit 0; fi\n'
        "echo fake-git \"$@\"",
    )


class Fixture:
    """local/git (the wrapper) beside local/git.meta.real; plain/git elsewhere."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.local = root / "local"
        self.plain = root / "plain"
        self.wrapper = version_script(self.local / "git", META_VERSION)
        self.real = version_script(self.local / "git.meta.real", META_VERSION)
        self.plain_git = version_script(self.plain / "git", PLAIN_VERSION)
        self.directory = root / "run-state" / "unit-a" / "unwrapped-git"
        self.path = f"{self.local}{os.pathsep}{self.plain}"

    def plan(self, path: str | None = None, **kwargs: object) -> ug.GitSubstitution:
        return ug.plan(
            self.path if path is None else path,
            kwargs.pop("directory", self.directory),  # type: ignore[arg-type]
            real_git=self.real,
            **kwargs,  # type: ignore[arg-type]
        )


class UnwrappedGitTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.fx = Fixture(Path(self.temporary.name).resolve())

    def assert_unchanged(self, substitution: ug.GitSubstitution, path: str) -> None:
        self.assertFalse(substitution.applied)
        self.assertEqual(path, substitution.path)
        self.assertIsNone(substitution.directory)
        self.assertIsNone(substitution.record()["directory"])
        # Nothing is created for a substitution that does not apply.
        self.assertIs(substitution, ug.materialize(substitution))
        self.assertFalse(self.fx.directory.exists())
        self.assertFalse(self.fx.directory.parent.exists())

    # The four cases the change is specified by.

    def test_applied_when_path_git_is_the_wrapper_and_versions_match(self) -> None:
        planned = self.fx.plan()
        self.assertTrue(planned.applied, planned.reason)
        self.assertEqual(ug.APPLIED, planned.code)
        self.assertEqual(f"{self.fx.directory}{os.pathsep}{self.fx.path}", planned.path)
        self.assertEqual(str(self.fx.wrapper), planned.resolved_git)
        self.assertEqual(str(self.fx.real), planned.effective_git)
        self.assertEqual(META_VERSION, planned.effective_version)
        # Planning alone creates nothing: a dry run can show this PATH.
        self.assertFalse(self.fx.directory.exists())

        made = ug.materialize(planned)
        self.assertEqual(planned, made)
        self.assertEqual(["git"], sorted(p.name for p in self.fx.directory.iterdir()))
        link = self.fx.directory / "git"
        self.assertTrue(link.is_symlink())
        self.assertEqual(str(self.fx.real), os.readlink(link))
        # What an exec of `git` on the new PATH now reaches.
        self.assertEqual(str(link), ug.find_on_path("git", made.path))
        self.assertTrue(os.path.samefile(ug.find_on_path("git", made.path), self.fx.real))
        record = made.record()
        self.assertEqual(True, record["applied"])
        self.assertEqual(str(self.fx.directory), record["directory"])
        self.assertIn("git substitution APPLIED: ", made.summary())
        self.assertIn(f'"effective_version": "{META_VERSION}"', made.summary())

    def test_not_applied_when_git_meta_real_is_absent(self) -> None:
        self.fx.real.unlink()
        substitution = self.fx.plan()
        self.assertEqual(ug.REAL_GIT_UNAVAILABLE, substitution.code)
        self.assertIn(str(self.fx.real), substitution.reason)
        # The git the DAG will run, and its version, are still recorded.
        self.assertEqual(str(self.fx.wrapper), substitution.effective_git)
        self.assertEqual(META_VERSION, substitution.effective_version)
        self.assertIn("git substitution NOT-APPLIED: ", substitution.summary())
        self.assert_unchanged(substitution, self.fx.path)

    def test_not_applied_when_versions_differ(self) -> None:
        version_script(self.fx.real, PLAIN_VERSION)
        substitution = self.fx.plan()
        self.assertEqual(ug.VERSION_MISMATCH, substitution.code)
        self.assertIn(META_VERSION, substitution.reason)
        self.assertIn(PLAIN_VERSION, substitution.reason)
        self.assertEqual(str(self.fx.wrapper), substitution.effective_git)
        self.assertEqual(META_VERSION, substitution.effective_version)
        self.assert_unchanged(substitution, self.fx.path)

    def test_not_applied_when_path_git_is_already_a_plain_git(self) -> None:
        path = f"{self.fx.plain}{os.pathsep}{self.fx.local}"
        substitution = self.fx.plan(path)
        self.assertEqual(ug.NOT_THE_WRAPPER, substitution.code)
        self.assertEqual(str(self.fx.plain_git), substitution.effective_git)
        self.assertEqual(PLAIN_VERSION, substitution.effective_version)
        self.assert_unchanged(substitution, path)

    # Detection is by file identity, never by a name.

    def test_not_applied_when_path_git_is_already_the_unwrapped_binary(self) -> None:
        earlier = self.fx.root / "earlier"
        earlier.mkdir()
        (earlier / "git").symlink_to(self.fx.real)
        path = f"{earlier}{os.pathsep}{self.fx.path}"
        substitution = self.fx.plan(path)
        self.assertEqual(ug.ALREADY_UNWRAPPED, substitution.code)
        self.assertEqual(META_VERSION, substitution.effective_version)
        self.assert_unchanged(substitution, path)

    def test_a_second_launch_inside_a_substituted_run_changes_nothing(self) -> None:
        first = ug.materialize(self.fx.plan())
        self.assertTrue(first.applied)
        nested = ug.plan(first.path, self.fx.root / "nested", real_git=self.fx.real)
        self.assertEqual(ug.ALREADY_UNWRAPPED, nested.code)
        self.assertEqual(first.path, nested.path)

    def test_a_copy_of_the_wrapper_is_not_the_wrapper(self) -> None:
        # Same bytes, same name, same version, different file: not applied.
        copy = self.fx.root / "copy"
        copy.mkdir()
        (copy / "git").write_bytes(self.fx.wrapper.read_bytes())
        (copy / "git").chmod(0o755)
        path = f"{copy}{os.pathsep}{self.fx.path}"
        substitution = self.fx.plan(path)
        self.assertEqual(ug.NOT_THE_WRAPPER, substitution.code)
        self.assert_unchanged(substitution, path)

    def test_a_link_to_the_wrapper_under_another_name_is_the_wrapper(self) -> None:
        linked = self.fx.root / "linked"
        linked.mkdir()
        (linked / "git").symlink_to(self.fx.wrapper)
        path = f"{linked}{os.pathsep}{self.fx.plain}"
        substitution = self.fx.plan(path)
        self.assertTrue(substitution.applied, substitution.reason)
        self.assertEqual(str(linked / "git"), substitution.resolved_git)
        self.assertEqual(str(self.fx.wrapper), substitution.resolved_git_realpath)

    # Every other refusal leaves PATH unchanged and says why.

    def test_not_applied_when_git_meta_real_is_not_executable(self) -> None:
        self.fx.real.chmod(0o644)
        substitution = self.fx.plan()
        self.assertEqual(ug.REAL_GIT_UNAVAILABLE, substitution.code)
        self.assertIn("not an executable regular file", substitution.reason)
        self.assert_unchanged(substitution, self.fx.path)

    def test_not_applied_when_the_wrapper_version_probe_fails(self) -> None:
        write_script(self.fx.wrapper, "echo broken >&2; exit 3")
        substitution = self.fx.plan()
        self.assertEqual(ug.VERSION_PROBE_FAILED, substitution.code)
        self.assertIn("exited 3: broken", substitution.reason)
        self.assertIsNone(substitution.effective_version)
        self.assert_unchanged(substitution, self.fx.path)

    def test_not_applied_when_the_real_version_probe_fails(self) -> None:
        write_script(self.fx.real, "exit 0")
        substitution = self.fx.plan()
        self.assertEqual(ug.VERSION_PROBE_FAILED, substitution.code)
        self.assertIn("no output", substitution.reason)
        self.assert_unchanged(substitution, self.fx.path)

    def test_not_applied_without_git_on_path(self) -> None:
        empty = self.fx.root / "empty"
        empty.mkdir()
        substitution = self.fx.plan(str(empty))
        self.assertEqual(ug.NO_GIT_ON_PATH, substitution.code)
        self.assertIsNone(substitution.effective_git)
        self.assertIsNone(substitution.effective_version)
        self.assert_unchanged(substitution, str(empty))

    def test_non_executable_git_on_path_is_skipped_like_execvp(self) -> None:
        shadow = self.fx.root / "shadow"
        write_script(shadow / "git", "exit 0", mode=0o644)
        path = f"{shadow}{os.pathsep}{self.fx.path}"
        substitution = self.fx.plan(path)
        self.assertTrue(substitution.applied, substitution.reason)
        self.assertEqual(str(self.fx.wrapper), substitution.resolved_git)

    def test_not_applied_through_a_relative_path_entry(self) -> None:
        cwd = os.getcwd()
        self.addCleanup(os.chdir, cwd)
        os.chdir(self.fx.local)
        substitution = self.fx.plan(f".{os.pathsep}{self.fx.plain}")
        self.assertEqual(ug.RELATIVE_PATH_ENTRY, substitution.code)
        self.assertIsNone(substitution.effective_version)
        self.assert_unchanged(substitution, f".{os.pathsep}{self.fx.plain}")

    def test_not_applied_when_the_directory_cannot_be_a_path_entry(self) -> None:
        bad = self.fx.root / "unit:with-colon" / "unwrapped-git"
        substitution = self.fx.plan(directory=bad)
        self.assertEqual(ug.UNUSABLE_DIRECTORY, substitution.code)
        self.assertEqual(self.fx.path, substitution.path)
        self.assertIs(substitution, ug.materialize(substitution))
        self.assertFalse(bad.parent.exists())

    def test_an_existing_directory_is_never_reused(self) -> None:
        planned = self.fx.plan()
        self.fx.directory.mkdir(parents=True)
        (self.fx.directory / "git").symlink_to(self.fx.plain_git)
        made = ug.materialize(planned)
        self.assertEqual(ug.MATERIALIZE_FAILED, made.code)
        self.assertFalse(made.applied)
        self.assertEqual(self.fx.path, made.path)
        self.assertEqual(str(self.fx.wrapper), made.effective_git)
        self.assertIsNone(made.directory)
        # The foreign entry was not replaced.
        self.assertEqual(str(self.fx.plain_git), os.readlink(self.fx.directory / "git"))

    def test_version_probe_does_not_wait_for_a_detached_child(self) -> None:
        # The real wrapper leaves a detached logger holding its output. A pipe
        # read to EOF would wait for it; the regular-file capture must not.
        write_script(
            self.fx.wrapper,
            f"(sleep 60 &)\necho {shlex.quote(META_VERSION)}",
        )
        started = time.monotonic()
        substitution = self.fx.plan()
        elapsed = time.monotonic() - started
        self.assertTrue(substitution.applied, substitution.reason)
        self.assertLess(elapsed, 30.0)

    def test_version_probe_is_bounded(self) -> None:
        write_script(self.fx.real, "exec sleep 60")
        started = time.monotonic()
        version, failure = ug.probe_version(str(self.fx.real), timeout=0.5)
        self.assertIsNone(version)
        self.assertIn("did not finish in 0.5s", failure)
        self.assertLess(time.monotonic() - started, 30.0)

    def test_version_probe_waits_without_time_sleep(self) -> None:
        # Popen.wait(timeout=...) polls with time.sleep. validate-run's tests
        # patch time.sleep to prove the launcher never backs off, so a probe
        # that slept would read as a retry loop.
        write_script(self.fx.real, f"sleep 0.3\necho {shlex.quote(META_VERSION)}")
        with mock.patch("time.sleep") as sleep:
            version, failure = ug.probe_version(str(self.fx.real), timeout=30)
        self.assertEqual((META_VERSION, ""), (version, failure))
        sleep.assert_not_called()

    def test_record_fields_are_stable(self) -> None:
        self.assertEqual(
            {
                "applied",
                "code",
                "reason",
                "resolved_git",
                "resolved_git_realpath",
                "real_git",
                "effective_git",
                "effective_version",
                "directory",
            },
            set(self.fx.plan().record()),
        )
        for substitution in (self.fx.plan(), self.fx.plan(str(self.fx.plain))):
            self.assertIn(substitution.code, ug.CODES)

    def test_cli_creates_the_directory_and_prints_the_path(self) -> None:
        # The module's CLI is what the measurement used; exercise it here.
        import contextlib
        import io
        import unittest.mock as mock

        out = io.StringIO()
        err = io.StringIO()
        with mock.patch.object(ug, "REAL_GIT", self.fx.real), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = ug.main(["--path", self.fx.path, "--directory", str(self.fx.directory),
                          "--print-path"])
        self.assertEqual(0, rc)
        self.assertEqual(f"{self.fx.directory}{os.pathsep}{self.fx.path}\n", out.getvalue())
        self.assertIn("git substitution APPLIED", err.getvalue())
        self.assertTrue(stat.S_ISLNK(os.lstat(self.fx.directory / "git").st_mode))


if __name__ == "__main__":
    unittest.main()
