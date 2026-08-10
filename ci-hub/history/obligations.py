#!/usr/bin/env python3
"""Append-only persistence for speculative-land verification obligations.

Cross-store joins are intentionally file-contract-only: ``landed_sha`` joins
``ignored/ci-hub/gha-runs.csv:head_sha`` and
``ignored/ci-hub/local-runs.csv:git_sha``; ``github.run_ids`` joins
``gha-runs.csv:run_id``.  All stores use the same ``OWNER/REPO`` string.
"""

from __future__ import annotations

import copy
import fcntl
import hashlib
import json
import os
import re
import tempfile
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, BinaryIO, Mapping, TextIO

ROOT = Path(__file__).resolve().parents[2]
SCHEMA_VERSION = 1
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
CLOSED_STATES = frozenset(("satisfied", "remediated"))
CHECKPOINT_SCHEMA_VERSION = 1
CHECKPOINT_FINGERPRINT_BYTES = 4096
CHECKPOINT_EVENT_TYPE = "projection-checkpoint"
CHECKPOINT_OBLIGATION_ID = "__ci_hub_projection_checkpoint__"

# Process-local reuse removes the second read performed by `watch --once --gate`.
# The durable sidecar removes the prefix read on later tick processes.  Both are
# performance hints: the append-only JSONL remains the authority, and every
# checkpoint is bound to its inode, byte offset, and the bytes immediately before
# that offset before it can seed an incremental tail scan.
_MEMORY_CHECKPOINTS: dict[
    Path, tuple[tuple[int, int, int, int, int], dict[str, dict[str, Any]]]
] = {}


class StoreError(RuntimeError):
    """The obligation event store is missing, corrupt, or inconsistent."""


class DuplicateOpenObligation(StoreError):
    """An unresolved obligation already exists for the repository and SHA."""

    def __init__(self, record: Mapping[str, Any]):
        self.record = dict(record)
        super().__init__(
            f"open obligation {record['obligation_id']} already exists for "
            f"{record['repo']}@{record['landed_sha']}"
        )


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def default_store_path() -> Path:
    override = os.environ.get("CI_HUB_OBLIGATIONS_STORE")
    return Path(override).expanduser() if override else ROOT / "ignored/ci-hub/obligations.jsonl"


def _validate_identity(repo: str, landed_sha: str) -> None:
    if not REPO_RE.fullmatch(repo):
        raise StoreError(f"invalid GitHub repository {repo!r}; expected OWNER/REPO")
    if not SHA_RE.fullmatch(landed_sha):
        raise StoreError(f"invalid landed SHA {landed_sha!r}; expected 40 lowercase hex digits")


def _apply_event(
    latest: dict[str, dict[str, Any]], event: Any, line_number: int
) -> None:
    if not isinstance(event, dict):
        raise StoreError(f"event at line {line_number} is not an object")
    obligation_id = event.get("obligation_id")
    if not isinstance(obligation_id, str) or not obligation_id:
        raise StoreError(f"event at line {line_number} has no obligation_id")
    if event.get("schema_version") != SCHEMA_VERSION:
        raise StoreError(
            f"event at line {line_number} uses unsupported schema "
            f"{event.get('schema_version')!r}"
        )
    latest[obligation_id] = event


def _is_checkpoint_event(event: Any) -> bool:
    return isinstance(event, dict) and event.get("event_type") == CHECKPOINT_EVENT_TYPE


def _validate_checkpoint_event(
    event: Any,
    latest: Mapping[str, Mapping[str, Any]],
    *,
    line_number: int,
    line_offset: int | None = None,
    expected_checkpoint_id: str | None = None,
    expected_projection_sha256: str | None = None,
) -> None:
    if not _is_checkpoint_event(event):
        raise StoreError(f"event at line {line_number} is not a projection checkpoint")
    projection_sha256 = _projection_fingerprint(latest)
    if (
        event.get("schema_version") != SCHEMA_VERSION
        or event.get("checkpoint_schema_version") != CHECKPOINT_SCHEMA_VERSION
        or event.get("covers_through_line_count") != line_number - 1
        or event.get("latest_sha256") != projection_sha256
        or (
            line_offset is not None
            and event.get("covers_through_offset") != line_offset
        )
        or (
            expected_checkpoint_id is not None
            and event.get("checkpoint_id") != expected_checkpoint_id
        )
        or (
            expected_projection_sha256 is not None
            and projection_sha256 != expected_projection_sha256
        )
    ):
        raise StoreError(f"projection checkpoint at line {line_number} is inconsistent")


def _latest_from_handle(handle: TextIO) -> dict[str, dict[str, Any]]:
    handle.seek(0)
    latest: dict[str, dict[str, Any]] = {}
    line_number = 0
    while True:
        line_offset = handle.tell()
        line = handle.readline()
        if not line:
            break
        line_number += 1
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise StoreError(f"invalid JSONL event at line {line_number}: {error}") from error
        if _is_checkpoint_event(event):
            _validate_checkpoint_event(
                event,
                latest,
                line_number=line_number,
                line_offset=line_offset,
            )
        else:
            _apply_event(latest, event, line_number)
    return latest


def _checkpoint_path(store: Path) -> Path:
    return store.with_name(f".{store.name}.latest-checkpoint.json")


def _prefix_fingerprint(handle: BinaryIO, offset: int) -> str:
    start = max(0, offset - CHECKPOINT_FINGERPRINT_BYTES)
    handle.seek(start)
    return hashlib.sha256(handle.read(offset - start)).hexdigest()


def _projection_fingerprint(latest: Mapping[str, Mapping[str, Any]]) -> str:
    encoded = json.dumps(latest, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def _checkpoint_seed(
    handle: BinaryIO, store: Path, stat: os.stat_result
) -> tuple[int, int, dict[str, dict[str, Any]]] | None:
    try:
        checkpoint = json.loads(_checkpoint_path(store).read_text(encoding="utf-8"))
        offset = checkpoint["store_offset"]
        commit_offset = checkpoint["commit_offset"]
        checkpoint_id = checkpoint["checkpoint_id"]
        line_count = checkpoint["line_count"]
        latest = checkpoint["latest"]
        if (
            checkpoint.get("checkpoint_schema_version") != CHECKPOINT_SCHEMA_VERSION
            or checkpoint.get("store_dev") != stat.st_dev
            or checkpoint.get("store_ino") != stat.st_ino
            or not isinstance(offset, int)
            or offset < 0
            or offset > stat.st_size
            or not isinstance(commit_offset, int)
            or commit_offset < 0
            or commit_offset >= offset
            or not isinstance(checkpoint_id, str)
            or not checkpoint_id
            or not isinstance(line_count, int)
            or line_count < 0
            or not isinstance(latest, dict)
            or checkpoint.get("latest_sha256") != _projection_fingerprint(latest)
            or checkpoint.get("prefix_sha256")
            != _prefix_fingerprint(handle, offset)
            or (
                stat.st_size == offset
                and (
                    checkpoint.get("store_mtime_ns") != stat.st_mtime_ns
                    or checkpoint.get("store_ctime_ns") != stat.st_ctime_ns
                )
            )
        ):
            return None
        seeded: dict[str, dict[str, Any]] = {}
        for record in latest.values():
            _apply_event(seeded, record, line_count)
        projection_sha256 = _projection_fingerprint(seeded)
        handle.seek(commit_offset)
        raw_commit = handle.readline()
        if handle.tell() != offset:
            return None
        try:
            commit = json.loads(raw_commit)
        except (UnicodeDecodeError, json.JSONDecodeError):
            return None
        _validate_checkpoint_event(
            commit,
            seeded,
            line_number=line_count,
            line_offset=commit_offset,
            expected_checkpoint_id=checkpoint_id,
            expected_projection_sha256=projection_sha256,
        )
        return offset, line_count, seeded
    except (KeyError, OSError, TypeError, ValueError, json.JSONDecodeError, StoreError):
        return None


def _scan_binary_tail(
    handle: BinaryIO,
    *,
    offset: int,
    line_count: int,
    latest: dict[str, dict[str, Any]],
) -> tuple[int, int, dict[str, dict[str, Any]]]:
    handle.seek(offset)
    while True:
        line_offset = handle.tell()
        raw_line = handle.readline()
        if not raw_line:
            break
        line_count += 1
        if not raw_line.strip():
            continue
        try:
            event = json.loads(raw_line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise StoreError(f"invalid JSONL event at line {line_count}: {error}") from error
        if _is_checkpoint_event(event):
            _validate_checkpoint_event(
                event,
                latest,
                line_number=line_count,
                line_offset=line_offset,
            )
        else:
            _apply_event(latest, event, line_count)
    return handle.tell(), line_count, latest


def _append_checkpoint_commit(
    handle: BinaryIO,
    *,
    offset: int,
    line_count: int,
    latest: Mapping[str, Mapping[str, Any]],
) -> tuple[int, int, int, str]:
    checkpoint_id = uuid.uuid4().hex
    commit_offset = offset
    recorded_at = utc_now()
    event = {
        "schema_version": SCHEMA_VERSION,
        # Compatibility envelope: older readers require every JSONL line to be
        # an obligation projection.  They see one harmless CLOSED synthetic
        # record instead of crashing during the rolling deployment; new readers
        # recognize event_type first and never expose it as an obligation.
        "obligation_id": CHECKPOINT_OBLIGATION_ID,
        "event_id": checkpoint_id,
        "event_type": CHECKPOINT_EVENT_TYPE,
        "repo": "rrnewton/dev-hermit",
        "landed_sha": "0" * 40,
        "overall_state": "satisfied",
        "opened_at": recorded_at,
        "updated_at": recorded_at,
        "local": {"state": "checkpoint"},
        "github": {"state": "checkpoint"},
        "checkpoint_schema_version": CHECKPOINT_SCHEMA_VERSION,
        "checkpoint_id": checkpoint_id,
        "recorded_at": recorded_at,
        "covers_through_offset": commit_offset,
        "covers_through_line_count": line_count,
        "latest_sha256": _projection_fingerprint(latest),
    }
    encoded = json.dumps(event, sort_keys=True, separators=(",", ":")).encode() + b"\n"
    handle.seek(0, os.SEEK_END)
    if handle.tell() != commit_offset:
        raise StoreError("obligation store changed while checkpoint was prepared")
    handle.write(encoded)
    handle.flush()
    os.fsync(handle.fileno())
    return handle.tell(), line_count + 1, commit_offset, checkpoint_id


def _write_checkpoint(
    handle: BinaryIO,
    store: Path,
    stat: os.stat_result,
    offset: int,
    line_count: int,
    latest: Mapping[str, Mapping[str, Any]],
    commit_offset: int,
    checkpoint_id: str,
) -> None:
    checkpoint = {
        "checkpoint_schema_version": CHECKPOINT_SCHEMA_VERSION,
        "store_dev": stat.st_dev,
        "store_ino": stat.st_ino,
        "store_offset": offset,
        "store_mtime_ns": stat.st_mtime_ns,
        "store_ctime_ns": stat.st_ctime_ns,
        "commit_offset": commit_offset,
        "checkpoint_id": checkpoint_id,
        "line_count": line_count,
        "prefix_sha256": _prefix_fingerprint(handle, offset),
        "latest_sha256": _projection_fingerprint(latest),
        "latest": latest,
    }
    path = _checkpoint_path(store)
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w", encoding="utf-8", dir=path.parent, prefix=f".{path.name}.", delete=False
        ) as temporary:
            temporary_name = temporary.name
            json.dump(checkpoint, temporary, sort_keys=True, separators=(",", ":"))
            temporary.write("\n")
        os.replace(temporary_name, path)
    except OSError:
        # The sidecar is only an optimization. A read-only directory or a lost
        # cache race must never turn a valid authoritative store into NO-RESULT.
        if temporary_name is not None:
            try:
                os.unlink(temporary_name)
            except OSError:
                pass


def latest_records(path: Path | None = None) -> dict[str, dict[str, Any]]:
    store = path or default_store_path()
    if not store.exists():
        return {}
    store = store.resolve()
    with store.open("a+b") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            stat = os.fstat(handle.fileno())
            identity = (
                stat.st_dev,
                stat.st_ino,
                stat.st_size,
                stat.st_mtime_ns,
                stat.st_ctime_ns,
            )
            memory = _MEMORY_CHECKPOINTS.get(store)
            if memory is not None and memory[0] == identity:
                return copy.deepcopy(memory[1])

            seed = _checkpoint_seed(handle, store, stat)
            if seed is None:
                offset, line_count, latest = 0, 0, {}
            else:
                offset, line_count, latest = seed
            checkpointed_offset = offset
            offset, line_count, latest = _scan_binary_tail(
                handle,
                offset=offset,
                line_count=line_count,
                latest=latest,
            )
            # The sidecar is accepted only when its projection hash is committed
            # by an event in the authoritative JSONL.  The exclusive lock makes
            # appending that small commitment and publishing the sidecar one
            # serial operation; a crash between them merely leaves an unused
            # commitment and forces a safe full scan next time.
            if seed is None or offset != checkpointed_offset:
                offset, line_count, commit_offset, checkpoint_id = (
                    _append_checkpoint_commit(
                        handle,
                        offset=offset,
                        line_count=line_count,
                        latest=latest,
                    )
                )
                stat = os.fstat(handle.fileno())
                _write_checkpoint(
                    handle,
                    store,
                    stat,
                    offset,
                    line_count,
                    latest,
                    commit_offset,
                    checkpoint_id,
                )
            identity = (
                stat.st_dev,
                stat.st_ino,
                stat.st_size,
                stat.st_mtime_ns,
                stat.st_ctime_ns,
            )
            _MEMORY_CHECKPOINTS[store] = (identity, copy.deepcopy(latest))
            return latest
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def get_record(obligation_id: str, path: Path | None = None) -> dict[str, Any]:
    try:
        return latest_records(path)[obligation_id]
    except KeyError as error:
        raise StoreError(f"unknown obligation {obligation_id!r}") from error


def unresolved_records(path: Path | None = None) -> list[dict[str, Any]]:
    records = latest_records(path).values()
    return sorted(
        (record for record in records if record.get("overall_state") not in CLOSED_STATES),
        key=lambda record: (str(record.get("opened_at", "")), record["obligation_id"]),
    )


def _merge(target: dict[str, Any], patch: Mapping[str, Any]) -> None:
    for key, value in patch.items():
        if isinstance(value, Mapping) and isinstance(target.get(key), dict):
            _merge(target[key], value)
        else:
            target[key] = copy.deepcopy(value)


def _append_locked(handle: TextIO, record: dict[str, Any]) -> None:
    handle.seek(0, os.SEEK_END)
    handle.write(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n")
    handle.flush()
    os.fsync(handle.fileno())


def create_obligation(
    *,
    repo: str,
    landed_sha: str,
    land_mode: str,
    verification_scope: str = "total",
    verification_policy: Mapping[str, Any] | None = None,
    actor: str = "unknown",
    obligation_id: str | None = None,
    path: Path | None = None,
) -> dict[str, Any]:
    landed_sha = landed_sha.lower()
    _validate_identity(repo, landed_sha)
    if land_mode not in {"admin", "speculative"}:
        raise StoreError("land_mode must be 'admin' or 'speculative'")
    if verification_scope not in {"total", "incremental"}:
        raise StoreError("verification_scope must be 'total' or 'incremental'")
    if verification_policy is not None and not isinstance(verification_policy, Mapping):
        raise StoreError("verification_policy must be an object")

    opened_at = utc_now()
    obligation_id = obligation_id or (
        f"{opened_at.replace('-', '').replace(':', '').replace('T', '-')[:-1]}-"
        f"{landed_sha[:12]}-{uuid.uuid4().hex[:6]}"
    )
    if obligation_id == CHECKPOINT_OBLIGATION_ID:
        raise StoreError(f"obligation_id {obligation_id!r} is reserved")
    store = path or default_store_path()
    store.parent.mkdir(parents=True, exist_ok=True)
    with store.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            latest = _latest_from_handle(handle)
            for existing in latest.values():
                if (
                    existing.get("repo") == repo
                    and existing.get("landed_sha") == landed_sha
                    and existing.get("overall_state") not in CLOSED_STATES
                ):
                    raise DuplicateOpenObligation(existing)
            if obligation_id in latest:
                raise StoreError(f"obligation_id {obligation_id!r} already exists")
            record: dict[str, Any] = {
                "schema_version": SCHEMA_VERSION,
                "event_id": uuid.uuid4().hex,
                "event_type": "opened",
                "recorded_at": opened_at,
                "obligation_id": obligation_id,
                "repo": repo,
                "landed_sha": landed_sha,
                "land_mode": land_mode,
                "verification_scope": verification_scope,
                "verification_policy": copy.deepcopy(verification_policy),
                "actor": actor,
                "opened_at": opened_at,
                "updated_at": opened_at,
                "overall_state": "open",
                "launch": {
                    "state": "pending",
                    "token": None,
                    "launcher_pid": None,
                    "attempt": 0,
                    "started_at": None,
                    "armed_at": None,
                    "last_error": None,
                },
                "first_terminal_at": None,
                "satisfied_at": None,
                "failure_source": None,
                "failure_summary": None,
                "recommendation": None,
                "local": {
                    "state": "not_started",
                    "started_at": None,
                    "finished_at": None,
                    "exit_code": None,
                    "log_path": None,
                    "pid": None,
                    "launch_token": None,
                    "registered_at": None,
                    "workspace": None,
                    "receipt_verification": None,
                    "cost": {
                        "estimate": None,
                        "actual": None,
                        "record_path": None,
                    },
                },
                "github": {
                    "state": "no_result",
                    "started_at": None,
                    "finished_at": None,
                    "run_ids": [],
                    "urls": [],
                    "workflow_name": None,
                    "event": None,
                    "required_positive_count": None,
                    "positive_count": 0,
                    "jobs": [],
                    "last_poll_error": "no dereferenced workflow producer",
                },
                "watcher": {
                    "state": "pending",
                    "pid": None,
                    "launch_token": None,
                    "identity": None,
                    "log_path": None,
                    "started_at": None,
                    "finished_at": None,
                    "exit_code": None,
                    "cancellation": None,
                },
                "alert": {"state": "none", "raised_at": None},
                "remediation": {
                    "state": "none",
                    "kind": None,
                    "ref": None,
                    "started_at": None,
                    "completed_at": None,
                },
            }
            _append_locked(handle, record)
            return record
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def transition(
    obligation_id: str,
    event_type: str,
    patch: Mapping[str, Any],
    path: Path | None = None,
) -> dict[str, Any]:
    if not event_type or not re.fullmatch(r"[a-z][a-z0-9_-]*", event_type):
        raise StoreError(f"invalid event_type {event_type!r}")
    store = path or default_store_path()
    store.parent.mkdir(parents=True, exist_ok=True)
    with store.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            latest = _latest_from_handle(handle)
            try:
                previous = latest[obligation_id]
            except KeyError as error:
                raise StoreError(f"unknown obligation {obligation_id!r}") from error
            record = copy.deepcopy(previous)
            immutable = {"obligation_id", "repo", "landed_sha", "opened_at"}
            if immutable.intersection(patch):
                raise StoreError("transition cannot change immutable obligation identity")
            if (
                "verification_policy" in patch
                and previous.get("verification_policy") is not None
                and patch["verification_policy"] != previous["verification_policy"]
            ):
                raise StoreError("transition cannot change bound verification policy")
            _merge(record, patch)
            now = utc_now()
            record.update(
                {
                    "schema_version": SCHEMA_VERSION,
                    "event_id": uuid.uuid4().hex,
                    "event_type": event_type,
                    "recorded_at": now,
                    "updated_at": now,
                }
            )
            _append_locked(handle, record)
            return record
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _matches_expected(record: Mapping[str, Any], expected: Mapping[str, Any]) -> bool:
    """Whether every expected leaf still has the same value in ``record``."""
    for key, value in expected.items():
        observed = record.get(key)
        if isinstance(value, Mapping):
            if not isinstance(observed, Mapping) or not _matches_expected(
                observed, value
            ):
                return False
        elif observed != value:
            return False
    return True


def transition_if_matches(
    obligation_id: str,
    event_type: str,
    patch: Mapping[str, Any],
    expected: Mapping[str, Any],
    path: Path | None = None,
) -> dict[str, Any] | None:
    """Append ``patch`` only if the latest record still matches ``expected``.

    This compare-and-append primitive is the launch/recovery arbitration point:
    two recovering processes may inspect the same OPEN record, but only one can
    claim a verifier token or register a process for that token.
    """
    if not event_type or not re.fullmatch(r"[a-z][a-z0-9_-]*", event_type):
        raise StoreError(f"invalid event_type {event_type!r}")
    store = path or default_store_path()
    store.parent.mkdir(parents=True, exist_ok=True)
    with store.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            latest = _latest_from_handle(handle)
            try:
                previous = latest[obligation_id]
            except KeyError as error:
                raise StoreError(f"unknown obligation {obligation_id!r}") from error
            if not _matches_expected(previous, expected):
                return None
            record = copy.deepcopy(previous)
            immutable = {"obligation_id", "repo", "landed_sha", "opened_at"}
            if immutable.intersection(patch):
                raise StoreError(
                    "transition cannot change immutable obligation identity"
                )
            if (
                "verification_policy" in patch
                and previous.get("verification_policy") is not None
                and patch["verification_policy"] != previous["verification_policy"]
            ):
                raise StoreError("transition cannot change bound verification policy")
            _merge(record, patch)
            now = utc_now()
            record.update(
                {
                    "schema_version": SCHEMA_VERSION,
                    "event_id": uuid.uuid4().hex,
                    "event_type": event_type,
                    "recorded_at": now,
                    "updated_at": now,
                }
            )
            _append_locked(handle, record)
            return record
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
