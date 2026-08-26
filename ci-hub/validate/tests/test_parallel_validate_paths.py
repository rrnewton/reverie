#!/usr/bin/env python3
"""Collision brackets for host paths used by concurrent validate launches."""

from __future__ import annotations

import fcntl
import json
import multiprocessing
import os
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
            "child_deadline": 1,
            "environment": {"HOME": "/home/test", "PATH": "/usr/bin", "TMPDIR": "/tmp"},
        }
        old_a = start_unit.build_systemd_command(**common, unit="validate-a")
        old_b = start_unit.build_systemd_command(**common, unit="validate-b")
        self.assertNotIn("TMPDIR=", "\n".join(old_a))
        self.assertNotIn("TMPDIR=", "\n".join(old_b))

        runtime_a = start_unit.prepare_runtime_root(self.root, "validate-a")
        runtime_b = start_unit.prepare_runtime_root(self.root, "validate-b")
        fixed_a = start_unit.build_systemd_command(
            **common, unit="validate-a", runtime_root=runtime_a
        )
        fixed_b = start_unit.build_systemd_command(
            **common, unit="validate-b", runtime_root=runtime_b
        )
        for name in ("tmp", "cache", "python-cache", "hermit-data"):
            self.assertTrue((runtime_a / name).is_dir())
            self.assertTrue((runtime_b / name).is_dir())
        self.assertIn(f"TMPDIR={runtime_a / 'tmp'}", fixed_a)
        self.assertIn(f"TMPDIR={runtime_b / 'tmp'}", fixed_b)
        self.assertNotEqual(runtime_a, runtime_b)
        # One run's cells deliberately share one E2E result root so the verifier
        # can aggregate them; only separate top-level runs get separate roots.
        self.assertIn("E2E_RUN_ID=validate-a", fixed_a)
        self.assertIn("E2E_RUN_ID=validate-b", fixed_b)

    def test_git_mutation_old_sections_overlap_and_fixed_section_waits(self) -> None:
        common = self.root / "common"
        common.mkdir()
        entered: multiprocessing.Queue[str] = multiprocessing.Queue()

        # Old behavior had no lock: both critical sections are entered together.
        entered.put("old-parent")
        child = multiprocessing.Process(target=entered.put, args=("old-child",))
        child.start()
        self.assertEqual(entered.get(timeout=1), "old-parent")
        self.assertEqual(entered.get(timeout=1), "old-child")
        child.join(timeout=2)

        lock_path = common / "ci-hub-validate.lock"
        with lock_path.open("a+") as lock:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
            child = multiprocessing.Process(
                target=enter_git_section, args=(str(common), entered)
            )
            child.start()
            with self.assertRaises(Exception):
                entered.get(timeout=0.15)
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
        self.assertEqual(entered.get(timeout=2), "entered")
        child.join(timeout=2)
        self.assertEqual(child.exitcode, 0)

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
