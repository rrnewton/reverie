#!/usr/bin/env python3
"""Durable handles for ci-hub-owned validation services and observer panes."""

from __future__ import annotations

import fcntl
import json
import os
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Mapping


def record_path(root: Path, unit: str) -> Path:
    return root / "ignored" / "validate" / "runs" / f"{unit.removesuffix('.service')}.json"


def read_record(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"cannot read validation handle {path}: {exc}") from exc
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise RuntimeError(f"validation handle {path} has an unsupported schema")
    return value


class RecordLocked(RuntimeError):
    """Another process holds this record's lock and the caller would not wait.

    ⚠️ RAISED ONLY WHEN THE CALLER ASKED NOT TO BLOCK. The default is unchanged
    and still blocks, because most writers here need the read-modify-write to be
    exclusive for correctness. This exists for the callers whose write is
    BOOKKEEPING -- where blocking costs the user something more valuable than the
    record is worth.
    """


@contextmanager
def exclusive_record(path: Path, *, blocking: bool = True):
    """Hold this record's lock. `blocking=False` raises RecordLocked instead of waiting.

    ⚠️ THE BLOCKING FORM HELD THE OWNER'S TERMINAL FOR AN UNBOUNDED TIME. `flock`
    with LOCK_EX and no bound is correct for a writer that must not interleave,
    and wrong for one that runs AFTER a verdict has been computed and printed:
    the caller then has nothing left to do but say so, and it could not, because
    it was waiting to update a file nobody was reading yet. Reproduced
    2026-08-25: with the lock free the wrapper returned in 16.5s; with another
    process holding it, the wrapper printed its report and then blocked
    indefinitely, idle rather than spinning, which is why it read as a job still
    running.

    ⚠️ NOT SOLVED WITH A TIMEOUT ON PURPOSE. A timeout turns an unbounded wait
    into a bounded one and still throws away the reason; the caller is left
    guessing whether it waited long enough. Contention is a FACT the caller can
    report, so it is raised as one.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    lock_path = path.with_name(f".{path.name}.lock")
    with lock_path.open("a+") as lock:
        mode = fcntl.LOCK_EX if blocking else fcntl.LOCK_EX | fcntl.LOCK_NB
        try:
            fcntl.flock(lock.fileno(), mode)
        except BlockingIOError as exc:
            raise RecordLocked(
                f"another process holds the lock on {lock_path}"
            ) from exc
        try:
            yield
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def _write_unlocked(path: Path, value: Mapping[str, Any]) -> None:
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(dict(value), stream, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def write_record(path: Path, value: Mapping[str, Any]) -> None:
    with exclusive_record(path):
        _write_unlocked(path, value)


def create_record(path: Path, value: Mapping[str, Any]) -> None:
    """Create a run handle without replacing another run's identity."""
    with exclusive_record(path):
        if path.exists():
            raise RuntimeError(f"validation handle already exists: {path}")
        _write_unlocked(path, value)


def reserve_log(path: Path) -> None:
    """Reserve a systemd append target; a second run must not share it."""
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError as error:
        raise RuntimeError(f"validation log already exists: {path}") from error
    os.close(descriptor)


def update_record(path: Path, *, blocking: bool = True, **fields: Any) -> dict[str, Any]:
    # The caller and the observer finish at nearly the same time. Serialize the
    # read-modify-write so a final exit update cannot erase cgroup evidence (or
    # vice versa) while retaining atomic replacement for readers.
    with exclusive_record(path, blocking=blocking):
        value = read_record(path)
        value.update(fields)
        _write_unlocked(path, value)
    return value
