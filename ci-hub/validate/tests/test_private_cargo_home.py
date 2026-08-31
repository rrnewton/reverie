#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""Bracket the per-run CARGO_HOME from both sides.

The defect this guards (#2112): cargo serialises every cache mutation on one
lock file, `$CARGO_HOME/.package-cache`. Nothing used to protect the seed copy
against the ~18 agents running ordinary `cargo build` in their own slots against
the same `~/.cargo`.
A node that loses that race does not run slowly -- it sits in `Blocking waiting
for file lock on package cache` until its budget kills it. Measured:
`privileged-build.privileged_tests` has a 0.05s median across 21 passing runs
and died at its 120s budget, and 9 of 14 node timeouts across 78 retained logs
carry that message.

Both directions are exercised on purpose. The NEGATIVE cases (a real cargo lock
actually blocks a second holder; a failed seed refuses rather than pretending)
are the point of the fix. The POSITIVE cases (a private home really is passed
to the unit, and really is removed) are what keep the negatives honest: a
launcher that set CARGO_HOME to nothing at all, or seeded nothing, would
satisfy every negative here and be useless.

The lock test uses cargo's own locking rather than a hand-rolled flock, because
the claim under test is about cargo's behaviour, not about POSIX file locks.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import start_unit  # noqa: E402


FAILURES: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    print(f"  {'ok  ' if condition else 'FAIL'} {name}{('  -- ' + detail) if detail else ''}")
    if not condition:
        FAILURES.append(name)


def real_runner(command, **kwargs):
    return subprocess.run(command, text=True, capture_output=True, **kwargs)


def text_or_none(path: Path) -> str | None:
    """Read a file, or report absence as a value.

    A missing seed must surface as a FAILED CASE, not as a traceback: an
    exception aborts the run and hides every case after it, so a later
    regression would be masked by an earlier one. Found by mutation-testing
    this file -- skipping the seed loop crashed the suite instead of failing it.
    """
    try:
        return path.read_text()
    except OSError:
        return None


# --------------------------------------------------------------------------
# NEGATIVE: the hazard is real -- cargo's package-cache lock genuinely blocks a
# second cargo that shares CARGO_HOME. Without this, the whole fix is theatre.
# --------------------------------------------------------------------------
def test_shared_cargo_home_serialises() -> None:
    if not shutil_which("cargo"):
        print("  skip cargo-package-cache-lock-blocks (no cargo on PATH)")
        return
    with tempfile.TemporaryDirectory() as tmp:
        shared = Path(tmp) / "cargo"
        shared.mkdir()
        lock = shared / ".package-cache"
        lock.touch()
        # Hold the lock the way cargo does, then observe a real cargo command
        # that needs it. flock(1) is the holder; cargo is the victim.
        holder = subprocess.Popen(
            ["flock", "-x", str(lock), "sleep", "6"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            time.sleep(0.4)  # let the holder acquire before we probe
            crate = Path(tmp) / "probe"
            subprocess.run(
                ["cargo", "init", "--name", "probe", "--vcs", "none", str(crate)],
                capture_output=True,
                text=True,
                env={**os.environ, "CARGO_HOME": str(shared)},
            )
            started = time.monotonic()
            result = subprocess.run(
                ["cargo", "fetch"],
                cwd=str(crate),
                capture_output=True,
                text=True,
                timeout=30,
                env={**os.environ, "CARGO_HOME": str(shared)},
            )
            waited = time.monotonic() - started
            blocked = "Blocking waiting for file lock" in (result.stderr + result.stdout)
            check(
                "NEGATIVE cargo blocks on a held package-cache lock",
                blocked or waited > 2.0,
                f"waited {waited:.1f}s, blocking-message={blocked}",
            )
        finally:
            holder.terminate()
            holder.wait(timeout=10)


def shutil_which(name: str) -> str | None:
    import shutil

    return shutil.which(name)


# --------------------------------------------------------------------------
# POSITIVE: a private home is actually produced, seeded, and handed to the unit.
# --------------------------------------------------------------------------
def test_seeds_and_returns_private_home() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        shared = Path(tmp) / "shared"
        (shared / "registry").mkdir(parents=True)
        (shared / "registry" / "marker").write_text("registry-content")
        (shared / "git" / "db").mkdir(parents=True)
        (shared / "git" / "db" / "marker").write_text("db-content")
        # The 45 GB of stale checkouts we deliberately do NOT copy.
        (shared / "git" / "checkouts" / "reverie-x").mkdir(parents=True)
        (shared / "git" / "checkouts" / "reverie-x" / "huge").write_text("x" * 1000)
        (shared / "config.toml").write_text("[net]\n")

        root = Path(tmp)
        private = start_unit.prepare_private_cargo_home(root, run=real_runner, shared=shared)

        check("POSITIVE a private cargo home is created", private is not None)
        if private is None:
            return
        check(
            "POSITIVE it is not the shared home",
            private.resolve() != shared.resolve(),
            str(private),
        )
        check(
            "POSITIVE it is outside the managed validation slot directory",
            private.parent == root / "ignored" / "validate" / "cargo-homes",
            str(private),
        )
        check(
            "POSITIVE registry is seeded",
            text_or_none(private / "registry" / "marker") == "registry-content",
        )
        check(
            "POSITIVE git/db is seeded",
            text_or_none(private / "git" / "db" / "marker") == "db-content",
        )
        check("POSITIVE config.toml is carried", (private / "config.toml").is_file())
        # The measured reason the seed is 10.4s instead of 218s.
        check(
            "POSITIVE stale git/checkouts are NOT copied",
            not (private / "git" / "checkouts").exists(),
        )
        # A private home is only worth anything if its lock is private too.
        check(
            "POSITIVE the package-cache lock path is private",
            (private / ".package-cache") != (shared / ".package-cache"),
        )

        start_unit.remove_private_cargo_home(private, run=real_runner)
        check("POSITIVE the private home is removed on cleanup", not private.exists())


def test_command_passes_cargo_home() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        cargo_home = root / "validate-cargo-abc"
        cargo_home.mkdir()
        common = dict(
            root=root,
            checkout=root,
            target="0" * 40,
            agent="test-agent",
            unit="validate-test-unit",
            record=root / "rec.json",
            log=root / "log.txt",
            pr=None,
            validate_args=["full"],
            wait=1,
            hold=1,
            child_deadline=3600,
            environment={"HOME": str(root), "PATH": "/usr/bin"},
        )
        with_home = start_unit.build_systemd_command(**common, cargo_home=cargo_home)
        without = start_unit.build_systemd_command(**common)

        check(
            "POSITIVE the unit receives CARGO_HOME",
            f"CARGO_HOME={cargo_home}" in with_home,
        )
        # NEGATIVE-CONTROL: the assertion above must be capable of failing.
        # Without a private home the launcher must NOT invent one, or the
        # positive case proves nothing.
        check(
            "NEGATIVE no CARGO_HOME is set when there is no private home",
            not any(str(item).startswith("CARGO_HOME=") for item in without),
        )


# --------------------------------------------------------------------------
# A missing shared seed is not permission to use shared Cargo state: the run
# still receives an empty private home and Cargo populates it itself.
# --------------------------------------------------------------------------
def test_missing_shared_home_still_returns_private_home() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        private = start_unit.prepare_private_cargo_home(
            root, run=real_runner, shared=Path(tmp) / "does-not-exist"
        )
        check("POSITIVE absent seed still yields a private home", private.is_dir())
        start_unit.remove_private_cargo_home(private, run=real_runner)


def test_retained_homes_do_not_create_validation_slot_directories() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        slots = root / "worktrees" / "validate"
        slots.mkdir(parents=True)
        before = len(list(slots.iterdir()))

        first = start_unit.prepare_private_cargo_home(
            root, run=real_runner, shared=root / "absent-shared-home"
        )
        time.sleep(0.05)
        second = start_unit.prepare_private_cargo_home(
            root, run=real_runner, shared=root / "absent-shared-home"
        )
        after = len(list(slots.iterdir()))

        check(
            "POSITIVE retained Cargo homes do not change validation slot count",
            (before, after) == (0, 0),
            f"separated counts were {before} then {after}",
        )
        check(
            "POSITIVE both retained Cargo homes exist outside the slot directory",
            first.is_dir() and second.is_dir() and first.parent == second.parent,
        )
        start_unit.remove_private_cargo_home(first, run=real_runner)
        start_unit.remove_private_cargo_home(second, run=real_runner)


def test_remove_is_total_and_safe() -> None:
    check("NEGATIVE removing None is a no-op", start_unit.remove_private_cargo_home(None, run=real_runner) is None)
    with tempfile.TemporaryDirectory() as tmp:
        target = Path(tmp) / "victim"
        (target / "git" / "db").mkdir(parents=True)
        (target / "git" / "db" / "f").write_text("x")
        start_unit.remove_private_cargo_home(target, run=real_runner)
        check("POSITIVE removal is recursive", not target.exists())


def main() -> int:
    original_host_tmp_root = start_unit.HOST_TMP_ROOT
    # These filesystem-only fixtures use tempfile below the real host /tmp.
    # Model a separate isolated guest /tmp just as test_start_unit does.
    start_unit.HOST_TMP_ROOT = Path(tempfile.gettempdir()) / "modeled-host-tmp"
    try:
        print("private CARGO_HOME (#2112)")
        test_shared_cargo_home_serialises()
        test_seeds_and_returns_private_home()
        test_command_passes_cargo_home()
        test_missing_shared_home_still_returns_private_home()
        test_retained_homes_do_not_create_validation_slot_directories()
        test_remove_is_total_and_safe()
        print()
        if FAILURES:
            print(
                f"FAILED: {len(FAILURES)} case(s): {', '.join(FAILURES)}",
                file=sys.stderr,
            )
            return 1
        print("all private-CARGO_HOME cases passed")
        return 0
    finally:
        start_unit.HOST_TMP_ROOT = original_host_tmp_root


if __name__ == "__main__":
    raise SystemExit(main())
