#!/usr/bin/env python3
"""Collision brackets for host paths used by concurrent validate launches."""

from __future__ import annotations

import fcntl
import json
import multiprocessing
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import run_registry  # noqa: E402
import start_unit  # noqa: E402


SHA = "a" * 40


def completed(command: list[str], rc: int = 0, stdout: str = ""):
    return subprocess.CompletedProcess(command, rc, stdout, "")


class GitRun:
    def __init__(self, common: Path) -> None:
        self.common = common

    def __call__(self, command: list[str], **_kwargs: object):
        if command[-1] == "--git-common-dir":
            return completed(command, stdout=f"{self.common}\n")
        raise AssertionError(command)


def enter_git_section(common: str, entered: multiprocessing.Queue[str]) -> None:
    source = Path(common).parent / "checkout"
    with start_unit.git_mutation_lock(source, run=GitRun(Path(common))):
        entered.put("entered")


def locked_fetch(repo: str, result: multiprocessing.Queue[tuple[int, str]]) -> None:
    def runner(command: list[str], **kwargs: object):
        return subprocess.run(command, capture_output=True, text=True, **kwargs)

    source = Path(repo)
    with start_unit.git_mutation_lock(source, run=runner):
        completed_fetch = subprocess.run(
            [
                "git",
                "-C",
                repo,
                "fetch",
                "origin",
                "main:refs/remotes/origin/main",
            ],
            capture_output=True,
            text=True,
        )
    result.put((completed_fetch.returncode, completed_fetch.stderr))


def seed_cargo(parent: str, shared: str, entered: multiprocessing.Queue[str]) -> None:
    def runner(command: list[str], **_kwargs: object):
        if command[:2] == ["mktemp", "-d"]:
            private = Path(parent) / f"private-{os.getpid()}"
            private.mkdir()
            return completed(command, stdout=f"{private}\n")
        if command[:2] == ["cp", "-a"]:
            entered.put("copying")
            return completed(command)
        if command[:2] == ["rm", "-rf"]:
            return completed(command)
        raise AssertionError(command)

    start_unit.prepare_private_cargo_home(
        Path(parent), run=runner, shared=Path(shared)
    )
    entered.put("done")


class ParallelValidatePathTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.addCleanup(self.temporary.cleanup)

    def test_unit_identity_old_second_resolution_collides_and_fixed_does_not(self) -> None:
        old = lambda: f"validate-agent-{SHA[:12]}-{int(time.time())}"
        with mock.patch.object(time, "time", return_value=1234.0):
            self.assertEqual(old(), old(), "old identity must reproduce its collision")
        with (
            mock.patch.object(time, "time_ns", return_value=1234000000000),
            mock.patch.object(os, "getpid", return_value=55),
            mock.patch.object(
                start_unit.secrets, "token_hex", side_effect=["11111111", "22222222"]
            ),
        ):
            self.assertNotEqual(
                start_unit.default_unit("agent", SHA),
                start_unit.default_unit("agent", SHA),
            )

    def test_record_and_log_old_writers_collide_and_new_creation_refuses(self) -> None:
        record = self.root / "runs/run.json"
        log = self.root / "run.log"
        run_registry.write_record(record, {"schema_version": 1, "owner": "first"})
        run_registry.write_record(record, {"schema_version": 1, "owner": "second"})
        self.assertEqual(run_registry.read_record(record)["owner"], "second")
        log.write_text("first\n")
        with log.open("a") as stream:
            stream.write("second\n")
        self.assertEqual(log.read_text(), "first\nsecond\n")

        fresh_record = self.root / "runs/fresh.json"
        run_registry.create_record(
            fresh_record, {"schema_version": 1, "owner": "first"}
        )
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            run_registry.create_record(
                fresh_record, {"schema_version": 1, "owner": "second"}
            )
        fresh_log = self.root / "fresh.log"
        run_registry.reserve_log(fresh_log)
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            run_registry.reserve_log(fresh_log)

    def test_runtime_paths_old_environment_is_shared_and_fixed_paths_are_per_run(self) -> None:
        common = {
            "root": self.root,
            "checkout": self.root / "checkout",
            "target": SHA,
            "agent": "agent",
            "record": self.root / "record.json",
            "log": self.root / "log",
            "pr": None,
            "validate_args": ["full"],
            "wait": 1,
            "hold": 1,
            "child_deadline": 3600,
            "environment": {
                "HOME": "/home/test",
                "PATH": "/usr/bin",
                "TMPDIR": "/tmp",
                "XDG_RUNTIME_DIR": "/run/user/1234",
                "DAGRUN_RESOURCE_CAPS_PATH": "/run/user/1234/ci-hub/resource-caps.json",
            },
        }
        old_a = start_unit.build_systemd_command(**common, unit="validate-a")
        old_b = start_unit.build_systemd_command(**common, unit="validate-b")
        self.assertNotIn("TMPDIR=", "\n".join(old_a))
        self.assertNotIn("TMPDIR=", "\n".join(old_b))

        # Removed behavior: both units resolve the same relative cache/temp
        # names.  The second writer replaces the first run's value at every
        # shared root; this is the collision, not merely equal command text.
        old_roots = {
            "tmp": self.root / "old-tmp",
            "cache": self.root / "old-cache",
            "python-cache": self.root / "old-python-cache",
            "hermit-data": self.root / "old-hermit-data",
        }
        for path in old_roots.values():
            path.mkdir()
            shared_child = path / "same-relative-name"
            shared_child.write_text("run-a")
            shared_child.write_text("run-b")
            self.assertEqual(shared_child.read_text(), "run-b")

        runtime_environment = {"XDG_RUNTIME_DIR": str(self.root)}
        runtime_a = start_unit.prepare_runtime_root(runtime_environment)
        runtime_b = start_unit.prepare_runtime_root(runtime_environment)
        fixed_a = start_unit.build_systemd_command(
            **common, unit="validate-a", runtime_root=runtime_a
        )
        other_parent = self.root / "other-parent-worktree"
        fixed_b = start_unit.build_systemd_command(
            **(common | {"root": other_parent}),
            unit="validate-b",
            runtime_root=runtime_b,
        )
        for name in ("cache", "python-cache", "hermit-data"):
            self.assertTrue((runtime_a / name).is_dir())
            self.assertTrue((runtime_b / name).is_dir())
            child_a = runtime_a / name / "same-relative-name"
            child_b = runtime_b / name / "same-relative-name"
            child_a.write_text("run-a")
            child_b.write_text("run-b")
            self.assertEqual(child_a.read_text(), "run-a")
            self.assertEqual(child_b.read_text(), "run-b")
        self.assertIn(f"TMPDIR={runtime_a}", fixed_a)
        self.assertIn(f"TMPDIR={runtime_b}", fixed_b)
        self.assertNotEqual(runtime_a, runtime_b)

        # The removed workspace-root path exceeds Linux's 107-byte Unix-socket
        # pathname payload for both the LiteInst coordinator and the skill
        # discovery socket fixture.  The short per-run root must leave room for
        # the longer fixture name, not only make the two runs distinct.
        old_tmp = self.root / "validate-runtime" / f"validate-{'x' * 72}" / "tmp"
        old_socket = old_tmp / "reverie-liteinst-XXXXXX/coordinator.sock"
        self.assertGreater(len(os.fsencode(old_socket)), 107)
        old_socket.parent.mkdir(parents=True)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            with self.assertRaises(OSError):
                listener.bind(str(old_socket))
        fixed_socket = runtime_a / (
            "reverie-skill-discovery-canonical-4194304-"
            "9999999999999999999/repo/README.md"
        )
        self.assertLessEqual(len(os.fsencode(fixed_socket)), 107)
        fixed_socket.parent.mkdir(parents=True)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(str(fixed_socket))
        long_runtime_parent = self.root / ("x" * 32)
        long_runtime_parent.mkdir()
        with self.assertRaisesRegex(ValueError, "too long"):
            start_unit.prepare_runtime_root(
                {"XDG_RUNTIME_DIR": str(long_runtime_parent)}
            )
        # One run's cells deliberately share one E2E result root so the verifier
        # can aggregate them; only separate top-level runs get separate roots.
        self.assertIn("E2E_RUN_ID=validate-a", fixed_a)
        self.assertIn("E2E_RUN_ID=validate-b", fixed_b)

        # Each top-level validate keeps the DAG's resource_caps process-local.
        # Even an ambient path from an older launcher must not opt these runs
        # into cross-process accounting.
        shared_caps = "DAGRUN_RESOURCE_CAPS_PATH="
        self.assertFalse(any(arg.startswith(shared_caps) for arg in fixed_a))
        self.assertFalse(any(arg.startswith(shared_caps) for arg in fixed_b))

    def test_git_mutation_old_sections_overlap_and_fixed_section_waits(self) -> None:
        origin = self.root / "origin.git"
        repo = self.root / "checkout"
        subprocess.run(["git", "init", "--bare", "--quiet", str(origin)], check=True)
        subprocess.run(["git", "init", "--quiet", str(repo)], check=True)
        subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.invalid"], check=True)
        subprocess.run(["git", "-C", str(repo), "config", "user.name", "test"], check=True)
        (repo / "seed").write_text("seed")
        subprocess.run(["git", "-C", str(repo), "add", "seed"], check=True)
        subprocess.run(["git", "-C", str(repo), "commit", "--quiet", "-m", "seed"], check=True)
        subprocess.run(["git", "-C", str(repo), "branch", "-M", "main"], check=True)
        subprocess.run(["git", "-C", str(repo), "remote", "add", "origin", str(origin)], check=True)
        subprocess.run(["git", "-C", str(repo), "push", "--quiet", "-u", "origin", "main"], check=True)
        updater = self.root / "updater"
        subprocess.run(
            ["git", "clone", "--quiet", "--branch", "main", str(origin), str(updater)],
            check=True,
        )
        subprocess.run(["git", "-C", str(updater), "config", "user.email", "test@example.invalid"], check=True)
        subprocess.run(["git", "-C", str(updater), "config", "user.name", "test"], check=True)
        (updater / "advance").write_text("advance")
        subprocess.run(["git", "-C", str(updater), "add", "advance"], check=True)
        subprocess.run(["git", "-C", str(updater), "commit", "--quiet", "-m", "advance"], check=True)
        subprocess.run(["git", "-C", str(updater), "push", "--quiet", "origin", "main"], check=True)

        common = Path(
            subprocess.run(
                ["git", "-C", str(repo), "rev-parse", "--path-format=absolute", "--git-common-dir"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
        )
        ref_lock = common / "refs/remotes/origin/main.lock"
        ref_lock.parent.mkdir(parents=True, exist_ok=True)

        # Removed behavior: a concurrent fetch reaches Git's shared ref lock and
        # fails instead of waiting for the other validate's Git mutation.
        ref_lock.touch(exist_ok=False)
        old = subprocess.run(
            ["git", "-C", str(repo), "fetch", "origin", "main:refs/remotes/origin/main"],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(old.returncode, 0)
        self.assertIn("origin/main", old.stderr)
        self.assertIn("failed", old.stderr)
        ref_lock.unlink()

        result: multiprocessing.Queue[tuple[int, str]] = multiprocessing.Queue()
        with start_unit.git_mutation_lock(repo, run=lambda command, **kwargs: subprocess.run(command, capture_output=True, text=True, **kwargs)):
            ref_lock.touch(exist_ok=False)
            child = multiprocessing.Process(target=locked_fetch, args=(str(repo), result))
            child.start()
            with self.assertRaises(Exception):
                result.get(timeout=0.15)
            ref_lock.unlink()
        rc, stderr = result.get(timeout=3)
        child.join(timeout=3)
        self.assertEqual(child.exitcode, 0)
        self.assertEqual(rc, 0, stderr)

    def test_cargo_seed_old_reader_can_overlap_writer_and_fixed_reader_waits(self) -> None:
        shared = self.root / "cargo"
        parent = self.root / "private"
        (shared / "registry").mkdir(parents=True)
        parent.mkdir()
        entered: multiprocessing.Queue[str] = multiprocessing.Queue()

        # Without taking Cargo's package-cache lock, the snapshot reaches the
        # copy while a writer holds the same mutation boundary.
        with (shared / ".package-cache").open("a+") as lock:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
            entered.put("old-copying")
            self.assertEqual(entered.get(timeout=1), "old-copying")

            child = multiprocessing.Process(
                target=seed_cargo, args=(str(parent), str(shared), entered)
            )
            child.start()
            with self.assertRaises(Exception):
                entered.get(timeout=0.15)
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
        self.assertEqual(entered.get(timeout=2), "copying")
        self.assertEqual(entered.get(timeout=2), "done")
        child.join(timeout=2)
        self.assertEqual(child.exitcode, 0)

    def test_same_sha_receipt_old_newest_only_binding_collides(self) -> None:
        current = {
            "commit": SHA,
            "tree": "b" * 40,
            "finished_at": "2026-08-26T01:00:00Z",
            "host": "host",
            "slot": "slot-a",
            "log_file": "/logs/a",
        }
        later = {**current, "finished_at": "2026-08-26T01:00:01Z", "slot": "slot-b", "log_file": "/logs/b"}
        current_receipt = {
            "receipt_identity": {
                "tuple": {
                    "sha": current["commit"],
                    "tree": current["tree"],
                    "finished_at": current["finished_at"],
                    "host": current["host"],
                    "slot": current["slot"],
                    "log_file": current["log_file"],
                }
            }
        }
        later_receipt = {
            "receipt_identity": {
                "tuple": {
                    "sha": later["commit"],
                    "tree": later["tree"],
                    "finished_at": later["finished_at"],
                    "host": later["host"],
                    "slot": later["slot"],
                    "log_file": later["log_file"],
                }
            }
        }
        self.assertFalse(
            start_unit.row_is_selected_qualifying_receipt(current, [later_receipt]),
            "old newest-only binding must reproduce the same-SHA collision",
        )
        self.assertTrue(
            start_unit.row_is_selected_qualifying_receipt(
                current, [current_receipt, later_receipt]
            )
        )


if __name__ == "__main__":
    unittest.main()
