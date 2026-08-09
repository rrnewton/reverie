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
import json
import os
import re
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping, TextIO

ROOT = Path(__file__).resolve().parents[2]
SCHEMA_VERSION = 1
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
REPO_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
CLOSED_STATES = frozenset(("satisfied", "remediated"))


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


def _latest_from_handle(handle: TextIO) -> dict[str, dict[str, Any]]:
    handle.seek(0)
    latest: dict[str, dict[str, Any]] = {}
    for line_number, line in enumerate(handle, 1):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise StoreError(f"invalid JSONL event at line {line_number}: {error}") from error
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
    return latest


def latest_records(path: Path | None = None) -> dict[str, dict[str, Any]]:
    store = path or default_store_path()
    if not store.exists():
        return {}
    with store.open("r", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_SH)
        try:
            return _latest_from_handle(handle)
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
