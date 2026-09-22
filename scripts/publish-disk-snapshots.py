#!/usr/bin/env python3
"""Publish both disk-health artifacts under one stable lock authority."""

from __future__ import annotations

import argparse
from collections.abc import Callable, Iterator, Sequence
from contextlib import contextmanager
import errno
import fcntl
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys


class LockAuthorityError(RuntimeError):
    """The state-root path cannot prove one stable directory authority."""


class LockBusyError(RuntimeError):
    """Another publisher already holds the stable lock authority."""


def _positive_integer(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return parsed


def _root_metadata(fd: int) -> os.stat_result:
    metadata = os.fstat(fd)
    if not stat.S_ISDIR(metadata.st_mode):
        raise LockAuthorityError("state-root lock authority is not a directory")
    return metadata


def _open_root_nofollow(path: Path) -> int:
    flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_DIRECTORY
    try:
        return os.open(path, flags)
    except OSError as error:
        if error.errno == errno.ELOOP:
            raise LockAuthorityError("state root must not be a symlink") from error
        raise LockAuthorityError(
            f"cannot open state-root authority: {error}"
        ) from error


def _verify_root_path(path: Path, held_fd: int) -> None:
    current_fd = _open_root_nofollow(path)
    try:
        held = _root_metadata(held_fd)
        current = _root_metadata(current_fd)
        expected = str(path)
        held_path = os.readlink(f"/proc/self/fd/{held_fd}")
        current_path = os.readlink(f"/proc/self/fd/{current_fd}")
        if (
            (held.st_dev, held.st_ino) != (current.st_dev, current.st_ino)
            or held_path != expected
            or current_path != expected
        ):
            raise LockAuthorityError("state-root path changed after it was opened")
    finally:
        os.close(current_fd)


@contextmanager
def stable_root_lock(path: Path) -> Iterator[None]:
    """Lock the canonical retained state-root directory inode."""
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise LockAuthorityError(f"cannot resolve state root: {error}") from error
    if resolved != path:
        raise LockAuthorityError(
            f"state root is not its exact resolved path: {path} -> {resolved}"
        )
    held_fd = _open_root_nofollow(path)
    try:
        _root_metadata(held_fd)
        try:
            fcntl.flock(held_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise LockBusyError(
                "the state-root lifecycle lock is busy"
            ) from error
        _verify_root_path(path, held_fd)
        yield
    finally:
        os.close(held_fd)


def _producer_path() -> Path:
    return Path(__file__).resolve().parents[1] / "ci-hub/health/operational_health.py"


def _check_dependencies(producer: Path) -> None:
    for dependency in ("btrfs", "compsize", "nice", "ionice"):
        if shutil.which(dependency) is None:
            raise RuntimeError(f"required command is unavailable: {dependency}")
    if (
        producer.is_symlink()
        or not producer.is_file()
        or not os.access(producer, os.X_OK)
    ):
        raise RuntimeError(f"operational-health producer is unavailable: {producer}")


def publish(
    *,
    state_root: Path,
    population_timeout: int,
    producer: Path | None = None,
    run: Callable[..., subprocess.CompletedProcess[object]] = subprocess.run,
) -> int:
    selected_producer = _producer_path() if producer is None else producer
    environment = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}
    with stable_root_lock(state_root):
        state_dir = state_root / "ignored/ops-state"
        state_dir.mkdir(parents=True, exist_ok=True)
        if state_dir.resolve(strict=True) != state_dir:
            raise LockAuthorityError(
                f"ops-state is not its exact resolved path: {state_dir}"
            )
        commands = (
            [
                sys.executable,
                str(selected_producer),
                "cache-disk-paths",
                "--state-root",
                str(state_root),
                "--state-file",
                str(state_dir / "disk-paths.json"),
                "--previous-state-file",
                str(state_dir / "disk-paths.previous.json"),
                "--population-timeout",
                str(population_timeout),
            ],
            [
                sys.executable,
                str(selected_producer),
                "cache-disk",
                "--filesystem-path",
                str(state_root),
                "--state-file",
                str(state_dir / "disk.json"),
            ],
        )
        aggregate_returncode = 0
        for command in commands:
            completed = run(command, env=environment, check=False)
            if completed.returncode != 0 and aggregate_returncode == 0:
                aggregate_returncode = (
                    completed.returncode if completed.returncode > 0 else 1
                )
    return aggregate_returncode


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Publish both disk-health artifacts under one stable lock."
    )
    parser.add_argument("--check", action="store_true")
    parser.add_argument(
        "--state-root",
        type=Path,
        default=Path(
            os.environ.get("DEV_HERMIT_PARENT", Path.home() / "work/dev-hermit")
        ),
    )
    parser.add_argument("--population-timeout", type=_positive_integer, default=180)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parsed = build_parser().parse_args(argv)
    try:
        state_root = parsed.state_root.absolute()
        resolved_root = state_root.resolve(strict=True)
        if resolved_root != state_root:
            raise LockAuthorityError(
                f"state root is not its exact resolved path: "
                f"{state_root} -> {resolved_root}"
            )
        if not state_root.is_dir():
            raise RuntimeError(f"state root is not a directory: {state_root}")
        producer = _producer_path()
        _check_dependencies(producer)
        if parsed.check:
            print("publish-disk-snapshots: producer and dependencies are available")
            return 0
        return publish(
            state_root=state_root,
            population_timeout=parsed.population_timeout,
            producer=producer,
        )
    except LockBusyError as error:
        print(f"publish-disk-snapshots: {error}", file=sys.stderr)
        return 75
    except (LockAuthorityError, OSError, RuntimeError) as error:
        print(f"publish-disk-snapshots: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
