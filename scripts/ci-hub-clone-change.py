#!/usr/bin/env python3
"""Prepare, seal, publish, and activate one isolated ci-hub change."""

from __future__ import annotations

import argparse
import copy
import fcntl
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Sequence, TextIO


SCHEMA = "ci-hub-change/v1"
MANIFEST = "ci-hub-change-manifest.json"
SEAL = "ci-hub-change-seal.json"
TASK_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")
ACTIVATION_SCHEMA = "ci-hub-change-activation-obligation/v1"
ACTIVATION_STORE = "activation-obligations.jsonl"
ACTIVATION_CLOSED_STATES = frozenset(("satisfied", "cancelled"))
ACTIVATION_EVENT_TYPES = frozenset(
    (
        "opened",
        "publication_unresolved",
        "publication_confirmed_after_refusal",
        "publication_refused",
        "publication_moved",
        "publication_confirmed",
        "activation_deferred",
        "activation_satisfied",
    )
)
ACTIVATION_OVERALL_STATES = frozenset(("open", "satisfied", "cancelled"))
ACTIVATION_STATES = frozenset(
    ("awaiting_publication", "pending", "publication_refused", "deferred", "satisfied")
)
OID_RE = re.compile(r"^[0-9a-f]{40}$")
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
EVENT_ID_RE = re.compile(r"^[0-9a-f]{32}$")
OBLIGATION_ID_RE = re.compile(r"^[0-9]{8}-[0-9]{6}-[0-9a-f]{12}-[0-9a-f]{6}$")
UTC_TIMESTAMP_RE = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")


class Refusal(RuntimeError):
    pass


def _exact_mapping(
    value: object, *, fields: frozenset[str], role: str
) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        raise Refusal(f"{role}: expected an object")
    missing = sorted(fields - value.keys())
    unknown = sorted(value.keys() - fields)
    if missing:
        raise Refusal(f"{role}: missing field(s): {', '.join(missing)}")
    if unknown:
        raise Refusal(f"{role}: unknown field(s): {', '.join(unknown)}")
    return value


def _versioned_mapping(
    value: object, *, schema: str, fields: frozenset[str], role: str
) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        raise Refusal(f"{role}: expected an object")
    found = value.get("schema")
    if not isinstance(found, str) or not found:
        raise Refusal(f"{role}-schema: expected a non-empty string")
    if found != schema:
        raise Refusal(f"{role}-schema: unsupported value {found!r}")
    return _exact_mapping(value, fields=fields, role=role)


def _required_string(value: Mapping[str, Any], field: str, role: str) -> str:
    found = value[field]
    if not isinstance(found, str) or not found:
        raise Refusal(f"{role}-{field}: expected a non-empty string")
    return found


def _optional_string(value: Mapping[str, Any], field: str, role: str) -> str | None:
    found = value[field]
    if found is None:
        return None
    if not isinstance(found, str) or not found:
        raise Refusal(f"{role}-{field}: expected null or a non-empty string")
    return found


def _timestamp(value: str, field: str) -> str:
    if not UTC_TIMESTAMP_RE.fullmatch(value):
        raise Refusal(
            f"activation-obligation-{field}: expected UTC YYYY-MM-DDTHH:MM:SSZ"
        )
    return value


def _metadata_timestamp(value: str, field: str) -> str:
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise Refusal(
            f"ci-hub-change-{field}: expected an ISO-8601 timestamp"
        ) from error
    if parsed.utcoffset() != timezone.utc.utcoffset(parsed):
        raise Refusal(f"ci-hub-change-{field}: expected a UTC timestamp")
    return value


def _change_task(row: Mapping[str, Any], role: str) -> str:
    try:
        return validate_task(_required_string(row, "task", role))
    except Refusal as error:
        raise Refusal(f"{role}-task: {error}") from error


@dataclass(frozen=True)
class ChangeManifest:
    schema: str
    task: str
    clone: str
    created_at: str
    initial_base: str
    claims: tuple[str, ...]

    FIELDS = frozenset(("schema", "task", "clone", "created_at", "initial_base", "claims"))

    @classmethod
    def from_mapping(cls, value: object) -> ChangeManifest:
        role = "ci-hub-change-manifest"
        row = _versioned_mapping(value, schema=SCHEMA, fields=cls.FIELDS, role=role)
        schema = str(row["schema"])
        task = _change_task(row, role)
        clone = _required_string(row, "clone", role)
        if not Path(clone).is_absolute():
            raise Refusal(f"{role}-clone: expected an absolute path")
        created_at = _metadata_timestamp(
            _required_string(row, "created_at", role), "manifest-created_at"
        )
        initial_base = _required_string(row, "initial_base", role)
        if not OID_RE.fullmatch(initial_base):
            raise Refusal(f"{role}-initial_base: expected a forty-hex commit")
        raw_claims = row["claims"]
        if not isinstance(raw_claims, list) or not raw_claims or not all(
            isinstance(claim, str) and claim for claim in raw_claims
        ):
            raise Refusal(f"{role}-claims: expected a non-empty string array")
        claims = tuple(raw_claims)
        if tuple(sorted(set(claims))) != claims:
            raise Refusal(f"{role}-claims: expected sorted unique paths")
        for claim in claims:
            try:
                normalized = normalize_claim(claim)
            except Refusal as error:
                raise Refusal(f"{role}-claims: {error}") from error
            if normalized != claim:
                raise Refusal(f"{role}-claims: path is not normalized: {claim!r}")
        return cls(schema, task, clone, created_at, initial_base, claims)

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema": self.schema,
            "task": self.task,
            "clone": self.clone,
            "created_at": self.created_at,
            "initial_base": self.initial_base,
            "claims": list(self.claims),
        }


@dataclass(frozen=True)
class SealTestResult:
    command: str
    exit_code: int

    @classmethod
    def from_mapping(cls, value: object) -> SealTestResult:
        role = "ci-hub-change-seal-tests"
        row = _exact_mapping(
            value, fields=frozenset(("command", "exit_code")), role=role
        )
        command = _required_string(row, "command", role)
        exit_code = row["exit_code"]
        if type(exit_code) is not int:
            raise Refusal(f"{role}-exit_code: expected an integer")
        if exit_code != 0:
            raise Refusal(f"{role}-exit_code: expected 0, got {exit_code}")
        return cls(command, exit_code)

    def to_dict(self) -> dict[str, Any]:
        return {"command": self.command, "exit_code": self.exit_code}


@dataclass(frozen=True)
class ChangeSeal:
    schema: str
    task: str
    base: str
    candidate: str
    changed_paths: tuple[str, ...]
    diff_sha256: str
    tests: tuple[SealTestResult, ...]
    sealed_at: str

    FIELDS = frozenset(
        (
            "schema",
            "task",
            "base",
            "candidate",
            "changed_paths",
            "diff_sha256",
            "tests",
            "sealed_at",
        )
    )

    @classmethod
    def from_mapping(cls, value: object) -> ChangeSeal:
        role = "ci-hub-change-seal"
        row = _versioned_mapping(value, schema=SCHEMA, fields=cls.FIELDS, role=role)
        schema = str(row["schema"])
        task = _change_task(row, role)
        base = _required_string(row, "base", role)
        if not OID_RE.fullmatch(base):
            raise Refusal(f"{role}-base: expected a forty-hex commit")
        candidate = _required_string(row, "candidate", role)
        if not OID_RE.fullmatch(candidate):
            raise Refusal(f"{role}-candidate: expected a forty-hex commit")
        if base == candidate:
            raise Refusal(f"{role}-candidate: must differ from base")
        raw_paths = row["changed_paths"]
        if not isinstance(raw_paths, list) or not raw_paths or not all(
            isinstance(path, str) and path for path in raw_paths
        ):
            raise Refusal(f"{role}-changed_paths: expected a non-empty string array")
        changed_paths = tuple(raw_paths)
        if tuple(sorted(set(changed_paths))) != changed_paths:
            raise Refusal(f"{role}-changed_paths: expected sorted unique paths")
        for path in changed_paths:
            try:
                normalized = normalize_claim(path)
            except Refusal as error:
                raise Refusal(f"{role}-changed_paths: {error}") from error
            if normalized != path:
                raise Refusal(f"{role}-changed_paths: path is not normalized: {path!r}")
        diff_sha256 = _required_string(row, "diff_sha256", role)
        if not DIGEST_RE.fullmatch(diff_sha256):
            raise Refusal(f"{role}-diff_sha256: expected 64 lowercase hex digits")
        raw_tests = row["tests"]
        if not isinstance(raw_tests, list) or not raw_tests:
            raise Refusal(f"{role}-tests: expected a non-empty array")
        tests = tuple(SealTestResult.from_mapping(test) for test in raw_tests)
        sealed_at = _metadata_timestamp(
            _required_string(row, "sealed_at", role), "seal-sealed_at"
        )
        return cls(
            schema,
            task,
            base,
            candidate,
            changed_paths,
            diff_sha256,
            tests,
            sealed_at,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema": self.schema,
            "task": self.task,
            "base": self.base,
            "candidate": self.candidate,
            "changed_paths": list(self.changed_paths),
            "diff_sha256": self.diff_sha256,
            "tests": [test.to_dict() for test in self.tests],
            "sealed_at": self.sealed_at,
        }


@dataclass(frozen=True)
class Activation:
    state: str
    attempts: int
    last_attempt_at: str | None
    last_reason: str | None
    activated_head: str | None

    @classmethod
    def from_mapping(cls, value: object) -> Activation:
        role = "activation-obligation-activation"
        row = _exact_mapping(
            value,
            fields=frozenset(
                ("state", "attempts", "last_attempt_at", "last_reason", "activated_head")
            ),
            role=role,
        )
        state = _required_string(row, "state", role)
        if state not in ACTIVATION_STATES:
            raise Refusal(f"{role}-state: unsupported value {state!r}")
        attempts = row["attempts"]
        if type(attempts) is not int or attempts < 0:
            raise Refusal(f"{role}-attempts: expected a non-negative integer")
        last_attempt_at = _optional_string(row, "last_attempt_at", role)
        if last_attempt_at is not None:
            _timestamp(last_attempt_at, "activation-last_attempt_at")
        last_reason = _optional_string(row, "last_reason", role)
        activated_head = _optional_string(row, "activated_head", role)
        if activated_head is not None and not OID_RE.fullmatch(activated_head):
            raise Refusal(f"{role}-activated_head: expected a forty-hex commit")
        return cls(state, attempts, last_attempt_at, last_reason, activated_head)

    def to_dict(self) -> dict[str, Any]:
        return {
            "state": self.state,
            "attempts": self.attempts,
            "last_attempt_at": self.last_attempt_at,
            "last_reason": self.last_reason,
            "activated_head": self.activated_head,
        }


@dataclass(frozen=True)
class ActivationObligation:
    schema: str
    event_id: str
    event_type: str
    recorded_at: str
    obligation_id: str
    task: str
    candidate: str
    primary: str
    changed_paths: tuple[str, ...]
    diff_sha256: str
    opened_at: str
    updated_at: str
    overall_state: str
    satisfied_at: str | None
    activation: Activation

    FIELDS = frozenset(
        (
            "schema",
            "event_id",
            "event_type",
            "recorded_at",
            "obligation_id",
            "task",
            "candidate",
            "primary",
            "changed_paths",
            "diff_sha256",
            "opened_at",
            "updated_at",
            "overall_state",
            "satisfied_at",
            "activation",
        )
    )

    @classmethod
    def from_mapping(cls, value: object) -> ActivationObligation:
        role = "activation-obligation"
        row = _versioned_mapping(
            value, schema=ACTIVATION_SCHEMA, fields=cls.FIELDS, role=role
        )
        schema = str(row["schema"])
        event_id = _required_string(row, "event_id", role)
        if not EVENT_ID_RE.fullmatch(event_id):
            raise Refusal(f"{role}-event_id: expected 32 lowercase hex digits")
        event_type = _required_string(row, "event_type", role)
        if event_type not in ACTIVATION_EVENT_TYPES:
            raise Refusal(f"{role}-event_type: unsupported value {event_type!r}")
        recorded_at = _timestamp(
            _required_string(row, "recorded_at", role), "recorded_at"
        )
        obligation_id = _required_string(row, "obligation_id", role)
        if not OBLIGATION_ID_RE.fullmatch(obligation_id):
            raise Refusal(f"{role}-obligation_id: unsupported value {obligation_id!r}")
        try:
            task = validate_task(_required_string(row, "task", role))
        except Refusal as error:
            raise Refusal(f"{role}-task: {error}") from error
        candidate = _required_string(row, "candidate", role)
        if not OID_RE.fullmatch(candidate):
            raise Refusal(f"{role}-candidate: expected a forty-hex commit")
        primary = _required_string(row, "primary", role)
        if not Path(primary).is_absolute():
            raise Refusal(f"{role}-primary: expected an absolute path")
        raw_paths = row["changed_paths"]
        if not isinstance(raw_paths, list) or not raw_paths or not all(
            isinstance(path, str) and path for path in raw_paths
        ):
            raise Refusal(f"{role}-changed_paths: expected a non-empty string array")
        changed_paths = tuple(raw_paths)
        if tuple(sorted(set(changed_paths))) != changed_paths:
            raise Refusal(f"{role}-changed_paths: expected sorted unique paths")
        for path in changed_paths:
            normalized = PurePosixPath(path)
            if normalized.is_absolute() or ".." in normalized.parts or normalized.as_posix() != path:
                raise Refusal(f"{role}-changed_paths: invalid path {path!r}")
        diff_sha256 = _required_string(row, "diff_sha256", role)
        if not DIGEST_RE.fullmatch(diff_sha256):
            raise Refusal(f"{role}-diff_sha256: expected 64 lowercase hex digits")
        opened_at = _timestamp(_required_string(row, "opened_at", role), "opened_at")
        updated_at = _timestamp(_required_string(row, "updated_at", role), "updated_at")
        if recorded_at != updated_at:
            raise Refusal(f"{role}-updated_at: must equal recorded_at")
        if opened_at > updated_at:
            raise Refusal(f"{role}-opened_at: cannot be after updated_at")
        overall_state = _required_string(row, "overall_state", role)
        if overall_state not in ACTIVATION_OVERALL_STATES:
            raise Refusal(f"{role}-overall_state: unsupported value {overall_state!r}")
        satisfied_at = _optional_string(row, "satisfied_at", role)
        if satisfied_at is not None:
            _timestamp(satisfied_at, "satisfied_at")
        activation = Activation.from_mapping(row["activation"])
        result = cls(
            schema,
            event_id,
            event_type,
            recorded_at,
            obligation_id,
            task,
            candidate,
            primary,
            changed_paths,
            diff_sha256,
            opened_at,
            updated_at,
            overall_state,
            satisfied_at,
            activation,
        )
        result.validate_state()
        return result

    def validate_state(self) -> None:
        state = self.activation.state
        if self.overall_state == "open":
            if self.satisfied_at is not None or self.activation.activated_head is not None:
                raise Refusal(
                    "activation-obligation-overall_state: open record cannot be satisfied"
                )
            if state not in {"awaiting_publication", "pending", "deferred"}:
                raise Refusal(
                    "activation-obligation-activation-state: disagrees with open overall_state"
                )
        elif self.overall_state == "cancelled":
            if self.satisfied_at is not None or self.activation.activated_head is not None:
                raise Refusal(
                    "activation-obligation-overall_state: cancelled record cannot be satisfied"
                )
            if state != "publication_refused":
                raise Refusal(
                    "activation-obligation-activation-state: cancelled record must be publication_refused"
                )
        elif (
            self.satisfied_at is None
            or state != "satisfied"
            or self.activation.activated_head is None
            or self.activation.last_reason is None
        ):
            raise Refusal(
                "activation-obligation-overall_state: satisfied record requires satisfied_at, activated_head, and reason"
            )

        if state in {"awaiting_publication", "pending", "publication_refused"}:
            if self.activation.attempts != 0 or self.activation.last_attempt_at is not None:
                raise Refusal(
                    "activation-obligation-activation-attempts: pre-activation state cannot carry an attempt"
                )
        elif state == "deferred":
            if self.activation.attempts == 0 or self.activation.last_attempt_at is None:
                raise Refusal(
                    "activation-obligation-activation-attempts: deferred state requires an attempted activation"
                )
            if self.activation.last_reason is None:
                raise Refusal(
                    "activation-obligation-activation-last_reason: deferred state requires a reason"
                )
        elif state == "satisfied" and self.activation.attempts > 0:
            if self.activation.last_attempt_at is None:
                raise Refusal(
                    "activation-obligation-activation-last_attempt_at: attempted satisfaction requires a timestamp"
                )

        expected = {
            "opened": {("open", "awaiting_publication")},
            "publication_unresolved": {("open", "awaiting_publication")},
            "publication_confirmed_after_refusal": {("open", "pending")},
            "publication_refused": {("cancelled", "publication_refused")},
            "publication_moved": {
                ("open", "pending"),
                ("cancelled", "publication_refused"),
            },
            "publication_confirmed": {("open", "pending")},
            "activation_deferred": {("open", "deferred")},
            "activation_satisfied": {("satisfied", "satisfied")},
        }[self.event_type]
        if (self.overall_state, state) not in expected:
            raise Refusal(
                "activation-obligation-event_type: disagrees with overall_state and activation.state"
            )

    def identity(self) -> tuple[object, ...]:
        return (
            self.obligation_id,
            self.task,
            self.candidate,
            self.primary,
            self.changed_paths,
            self.diff_sha256,
            self.opened_at,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema": self.schema,
            "event_id": self.event_id,
            "event_type": self.event_type,
            "recorded_at": self.recorded_at,
            "obligation_id": self.obligation_id,
            "task": self.task,
            "candidate": self.candidate,
            "primary": self.primary,
            "changed_paths": list(self.changed_paths),
            "diff_sha256": self.diff_sha256,
            "opened_at": self.opened_at,
            "updated_at": self.updated_at,
            "overall_state": self.overall_state,
            "satisfied_at": self.satisfied_at,
            "activation": self.activation.to_dict(),
        }


@dataclass(frozen=True)
class ActivationObservation:
    obligation: ActivationObligation
    reason: str
    head: str | None


def run(
    argv: Sequence[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        list(argv), cwd=cwd, text=True, capture_output=capture, check=False
    )
    if check and result.returncode != 0:
        detail = (
            (result.stderr or "").strip()
            or (result.stdout or "").strip()
            or f"exit {result.returncode}"
        )
        raise Refusal(f"{shlex.join(argv)}: {detail}")
    return result


def git(repo: Path, *args: str, check: bool = True) -> str:
    return run(("git", "-C", str(repo), *args), check=check).stdout.strip()


def emit_completed(result: subprocess.CompletedProcess[str]) -> None:
    if result.stdout:
        print(result.stdout, end="")
    if result.stderr:
        print(result.stderr, end="", file=sys.stderr)


def network_git(repo: Path | None, *args: str) -> str:
    command: list[str] = ["git"]
    if repo is not None:
        command.extend(("-C", str(repo)))
    command.extend(args)
    if shutil.which("with-proxy"):
        command.insert(0, "with-proxy")
    return run(command).stdout.strip()


def repository_root(start: Path | None = None) -> Path:
    here = (start or Path.cwd()).resolve()
    result = run(("git", "-C", str(here), "rev-parse", "--show-toplevel"))
    return Path(result.stdout.strip()).resolve()


def git_dir(repo: Path) -> Path:
    raw = Path(git(repo, "rev-parse", "--git-dir"))
    return (repo / raw).resolve() if not raw.is_absolute() else raw.resolve()


def metadata_path(repo: Path, name: str) -> Path:
    return git_dir(repo) / name


def write_json(path: Path, value: dict[str, Any]) -> None:
    temporary = path.with_name(f".{path.name}.{os.getpid()}")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, path)


def read_json(path: Path, role: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise Refusal(f"cannot read {role} {path}: {error}") from error
    if not isinstance(value, dict):
        raise Refusal(f"{role} {path} is not a JSON object")
    return value


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace(
        "+00:00", "Z"
    )


def activation_store_path(primary: Path, override: Path | None = None) -> Path:
    if override is not None:
        return override.expanduser().resolve()
    return primary / "ignored" / "ci-hub" / ACTIVATION_STORE


def _activation_latest_from_handle(
    handle: TextIO,
) -> dict[str, ActivationObligation]:
    handle.seek(0)
    latest: dict[str, ActivationObligation] = {}
    event_ids: set[str] = set()
    for line_number, line in enumerate(handle, 1):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise Refusal(
                f"activation obligation store line {line_number} is invalid JSON: {error}"
            ) from error
        try:
            obligation = ActivationObligation.from_mapping(event)
        except Refusal as error:
            raise Refusal(
                f"activation obligation store line {line_number}: {error}"
            ) from error
        if obligation.event_id in event_ids:
            raise Refusal(
                f"activation obligation store line {line_number}: "
                f"activation-obligation-event_id {obligation.event_id!r} is duplicated"
            )
        event_ids.add(obligation.event_id)
        previous = latest.get(obligation.obligation_id)
        if previous is not None and previous.identity() != obligation.identity():
            raise Refusal(
                f"activation obligation store line {line_number}: "
                "activation-obligation-identity changed across events"
            )
        latest[obligation.obligation_id] = obligation
    return latest


def activation_latest(store: Path) -> dict[str, ActivationObligation]:
    if not store.exists():
        return {}
    with store.open("r", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_SH)
        try:
            return _activation_latest_from_handle(handle)
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _append_activation_event(handle: TextIO, record: ActivationObligation) -> None:
    handle.seek(0, os.SEEK_END)
    handle.write(
        json.dumps(record.to_dict(), sort_keys=True, separators=(",", ":")) + "\n"
    )
    handle.flush()
    os.fsync(handle.fileno())


def _merge_mapping(target: dict[str, Any], patch: Mapping[str, Any]) -> None:
    for key, value in patch.items():
        if isinstance(value, Mapping) and isinstance(target.get(key), dict):
            _merge_mapping(target[key], value)
        else:
            target[key] = copy.deepcopy(value)


def create_activation_obligation(
    *,
    store: Path,
    task: str,
    candidate: str,
    primary: Path,
    changed_paths: Sequence[str],
    diff_sha256: str,
) -> ActivationObligation:
    store.parent.mkdir(parents=True, exist_ok=True)
    opened_at = utc_now()
    with store.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            latest = _activation_latest_from_handle(handle)
            for existing in latest.values():
                if (
                    existing.candidate == candidate
                    and existing.primary == str(primary)
                    and existing.overall_state not in ACTIVATION_CLOSED_STATES
                ):
                    expected_identity = {
                        "task": task,
                        "changed_paths": tuple(sorted(changed_paths)),
                        "diff_sha256": diff_sha256,
                    }
                    if any(
                        getattr(existing, key) != value
                        for key, value in expected_identity.items()
                    ):
                        raise Refusal(
                            "open activation obligation for this candidate has "
                            "different immutable identity"
                        )
                    return existing
            obligation_id = (
                f"{opened_at.replace('-', '').replace(':', '').replace('T', '-')[:-1]}-"
                f"{candidate[:12]}-{uuid.uuid4().hex[:6]}"
            )
            record = ActivationObligation.from_mapping({
                "schema": ACTIVATION_SCHEMA,
                "event_id": uuid.uuid4().hex,
                "event_type": "opened",
                "recorded_at": opened_at,
                "obligation_id": obligation_id,
                "task": task,
                "candidate": candidate,
                "primary": str(primary),
                "changed_paths": sorted(changed_paths),
                "diff_sha256": diff_sha256,
                "opened_at": opened_at,
                "updated_at": opened_at,
                "overall_state": "open",
                "satisfied_at": None,
                "activation": {
                    "state": "awaiting_publication",
                    "attempts": 0,
                    "last_attempt_at": None,
                    "last_reason": None,
                    "activated_head": None,
                },
            })
            _append_activation_event(handle, record)
            return record
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def transition_activation_obligation(
    store: Path,
    obligation_id: str,
    event_type: str,
    patch: Mapping[str, Any],
) -> ActivationObligation:
    if not re.fullmatch(r"[a-z][a-z0-9_-]*", event_type):
        raise Refusal(f"invalid activation obligation event type {event_type!r}")
    store.parent.mkdir(parents=True, exist_ok=True)
    with store.open("a+", encoding="utf-8") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            latest = _activation_latest_from_handle(handle)
            try:
                previous = latest[obligation_id]
            except KeyError as error:
                raise Refusal(f"unknown activation obligation {obligation_id}") from error
            immutable = {
                "obligation_id",
                "task",
                "candidate",
                "primary",
                "changed_paths",
                "diff_sha256",
                "opened_at",
            }
            if immutable.intersection(patch):
                raise Refusal("activation transition cannot change immutable identity")
            record = previous.to_dict()
            _merge_mapping(record, patch)
            now = utc_now()
            record.update(
                {
                    "schema": ACTIVATION_SCHEMA,
                    "event_id": uuid.uuid4().hex,
                    "event_type": event_type,
                    "recorded_at": now,
                    "updated_at": now,
                }
            )
            current = ActivationObligation.from_mapping(record)
            _append_activation_event(handle, current)
            return current
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def activation_for_candidate(
    store: Path, candidate: str, primary: Path, *, include_closed: bool = False
) -> ActivationObligation | None:
    matches = [
        record
        for record in activation_latest(store).values()
        if record.candidate == candidate
        and record.primary == str(primary)
        and (include_closed or record.overall_state not in ACTIVATION_CLOSED_STATES)
    ]
    return max(matches, key=lambda item: item.opened_at) if matches else None


def normalize_claim(raw: str) -> str:
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or raw.endswith("/"):
        raise Refusal(f"invalid claimed path {raw!r}")
    value = path.as_posix()
    if value != "ci-hub" and not value.startswith("ci-hub/"):
        raise Refusal(f"ci-hub change claims must stay below ci-hub/: {raw!r}")
    return value


def validate_task(task: str) -> str:
    if not TASK_RE.fullmatch(task):
        raise Refusal("--task must be a lowercase hyphenated slug")
    return task


def is_clean(repo: Path) -> bool:
    return not git(repo, "status", "--porcelain=v1", "--untracked-files=all")


def fetch_main(repo: Path) -> str:
    network_git(repo, "fetch", "origin", "main")
    return git(repo, "rev-parse", "refs/remotes/origin/main^{commit}")


def changed_paths(repo: Path, base: str, candidate: str) -> list[str]:
    output = git(repo, "diff", "--name-only", f"{base}..{candidate}")
    return [line for line in output.splitlines() if line]


def claim_covers(claim: str, changed: str) -> bool:
    return changed == claim or changed.startswith(f"{claim}/")


def diff_digest(repo: Path, base: str, candidate: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), "diff", "--binary", f"{base}..{candidate}"],
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        raise Refusal("cannot render candidate binary diff")
    return hashlib.sha256(result.stdout).hexdigest()


def initialize_manifest(repo: Path, task: str, claims: list[str], base: str) -> None:
    manifest = ChangeManifest.from_mapping(
        {
            "schema": SCHEMA,
            "task": validate_task(task),
            "clone": str(repo),
            "created_at": datetime.now(timezone.utc).isoformat(),
            "initial_base": base,
            "claims": sorted(set(claims)),
        }
    )
    write_json(
        metadata_path(repo, MANIFEST),
        manifest.to_dict(),
    )


def uninitialized_submodules(repo: Path) -> list[str]:
    """Top-level submodule paths `git submodule status` reports as not checked out.

    A separate post-condition rather than trust in the update's exit code,
    because the failure this guards against is a clone that succeeds while
    quietly lacking what the tests import. `git submodule status` prefixes an
    uninitialized entry with `-`; `+` and `U` mean checked out but moved or
    conflicted, which `is_clean` already refuses.

    Deliberately NOT `--recursive`, matching the non-recursive update it checks.
    An earlier revision used `--recursive` and refused every real clone, naming
    `hermit/agent-utils`, `hermit/third-party/rr` and reverie's `dynamorio`,
    `e9patch` and `sabre`. Those are nested build dependencies of the products;
    no test here reaches them, the working tree is clean without them, and
    fetching them would cost far more than the top-level four. A guard wider
    than the thing it guards is a false alarm, and a false alarm on every clone
    is worse than none.
    """
    listing = git(repo, "submodule", "status")
    missing = []
    for line in listing.splitlines():
        if line.startswith("-"):
            parts = line[1:].split()
            if len(parts) > 1:
                missing.append(parts[1])
    return missing


def hermit_agent_utils_problem(repo: Path) -> str | None:
    """Why Hermit's one required nested submodule is unavailable, or None.

    The parent update deliberately remains non-recursive: Hermit's other nested
    submodules are product/backend inputs that this tool's tests do not need.
    agent-utils is the one exception because the parent `agent-utils` path is a
    symlink to it and the parent test suites import through that path.
    """
    link = repo / "agent-utils"
    if not link.is_symlink():
        return "agent-utils is not the required symlink to hermit/agent-utils"
    target = os.readlink(link)
    if target != "hermit/agent-utils":
        return f"agent-utils points to {target!r}, not 'hermit/agent-utils'"

    hermit = repo / "hermit"
    result = run(
        ("git", "-C", str(hermit), "submodule", "status", "--", "agent-utils"),
        check=False,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        return f"cannot inspect hermit/agent-utils: {detail or 'git submodule status failed'}"
    entries = [line for line in result.stdout.splitlines() if line.strip()]
    if len(entries) != 1:
        return "hermit records no single agent-utils submodule"
    if entries[0].startswith("-"):
        return "hermit/agent-utils is uninitialized; parent agent-utils exposes no files"
    if not link.is_dir():
        return "parent agent-utils symlink is dangling after hermit/agent-utils initialization"
    return None


def command_create(args: argparse.Namespace) -> None:
    destination = Path(args.clone).expanduser().resolve()
    if destination.exists() or destination.is_symlink():
        raise Refusal(
            f"clone destination already exists: {destination}; a coordinator must "
            "disposition the per-change clone before creating another"
        )
    claims = [normalize_claim(item) for item in args.path]
    if not claims:
        raise Refusal("create requires at least one --path claim")
    destination.parent.mkdir(parents=True, exist_ok=True)
    network_git(
        None,
        "clone",
        "--no-recurse-submodules",
        "--single-branch",
        "--branch",
        "main",
        args.remote,
        str(destination),
    )
    # Submodules arrive in a SECOND step because THE REPOSITORY'S OWN TESTS need
    # them, not because a ci-hub change does. `--no-recurse-submodules` above was
    # here in this tool's first commit 31cf7b51, with no comment and no mention
    # in the protocol doc, and its charitable reading -- a change scoped to
    # `ci-hub/` does not need product source -- is true of the change and false
    # of the tests, so the flag stays and the tests' need is met explicitly.
    #
    # Measured against rrnewton/dev-hermit on 2026-08-15.
    # `ci-hub/health/tests/test_bounded_gate.py` ran 0 tests and one collection
    # error without `agent-utils/py/tick_hub`, and 12 with it;
    # `test_merge_gate_authority.py` failed 7 of 11 without
    # `hermit/scripts/configure-merge-gate-ruleset.sh`, and passed 11 with it.
    #
    # Before the parent agent-utils gitlink was collapsed into Hermit's, two
    # mechanisms reached the same four top-level submodules and did NOT cost the
    # same. `git clone --recurse-submodules` produced a 566M clone in 34s;
    # clone-then-update produced 212M in 11s, most of the difference being
    # reverie's object store at 178M versus 14M. Hence the non-recursive parent
    # update remains. Over the no-submodule clone (3s, 100M) its measured cost
    # was about +8s and +112M.
    #
    # All three parent product submodules rather than only the ones the current
    # tests need: the small extra cost buys the next test that reaches for
    # `reverie` or `liteinst2` not failing this same way. Hermit's agent-utils is
    # initialized separately below; recursion would also fetch every unrelated
    # nested backend source.
    network_git(destination, "submodule", "update", "--init")
    uninitialized = uninitialized_submodules(destination)
    if uninitialized:
        raise Refusal(
            "clone left submodule(s) uninitialized, so this clone cannot run the "
            "repository's own tests: " + ", ".join(uninitialized)
        )
    link_problem = hermit_agent_utils_problem(destination)
    if link_problem and "uninitialized" not in link_problem:
        raise Refusal(f"clone has an invalid agent-utils path: {link_problem}")
    network_git(
        destination / "hermit", "submodule", "update", "--init", "--", "agent-utils"
    )
    link_problem = hermit_agent_utils_problem(destination)
    if link_problem:
        raise Refusal(
            "clone left hermit/agent-utils unavailable, so this clone cannot run "
            f"the repository's own tests: {link_problem}"
        )
    base = git(destination, "rev-parse", "HEAD^{commit}")
    remote = git(destination, "rev-parse", "refs/remotes/origin/main^{commit}")
    if base != remote or not is_clean(destination):
        raise Refusal("fresh clone is not clean at exact origin/main")
    setup = destination / "scripts/setup-hooks.sh"
    if setup.is_file():
        run((str(setup),), cwd=destination, capture=False)
    branch = f"ci-hub/{validate_task(args.task)}"
    run(("git", "-C", str(destination), "switch", "-c", branch), capture=False)
    initialize_manifest(destination, args.task, claims, base)
    print(f"CI_HUB_CHANGE created={destination} task={args.task} base={base} branch={branch}")


def load_manifest(repo: Path) -> ChangeManifest:
    path = metadata_path(repo, MANIFEST)
    manifest = ChangeManifest.from_mapping(read_json(path, "change manifest"))
    if Path(manifest.clone).resolve() != repo:
        raise Refusal("ci-hub-change-manifest-clone: does not match this checkout")
    return manifest


def verify_candidate(repo: Path) -> tuple[ChangeManifest, str, str, list[str], str]:
    manifest = load_manifest(repo)
    if not is_clean(repo):
        raise Refusal("candidate clone is dirty; commit the complete change before sealing")
    candidate = git(repo, "rev-parse", "HEAD^{commit}")
    base = fetch_main(repo)
    parents = git(repo, "rev-list", "--parents", "-n", "1", candidate).split()
    if len(parents) != 2 or parents[1] != base:
        raise Refusal(
            f"candidate must be exactly one commit on current origin/main; candidate={candidate} "
            f"parent={parents[1] if len(parents) > 1 else 'none'} origin/main={base}"
        )
    paths = changed_paths(repo, base, candidate)
    if not paths:
        raise Refusal("candidate commit changes zero paths")
    claims = list(manifest.claims)
    outside = [path for path in paths if not any(claim_covers(claim, path) for claim in claims)]
    if outside:
        raise Refusal("candidate changes unclaimed paths: " + ", ".join(outside))
    return manifest, base, candidate, paths, diff_digest(repo, base, candidate)


def command_seal(args: argparse.Namespace) -> None:
    repo = repository_root(Path(args.clone).expanduser() if args.clone else None)
    manifest, base, candidate, paths, digest = verify_candidate(repo)
    if not args.test:
        raise Refusal("seal requires at least one --test command")
    tests: list[SealTestResult] = []
    for command in args.test:
        result = run(("bash", "-lc", command), cwd=repo, check=False, capture=False)
        tests.append(SealTestResult(command=command, exit_code=result.returncode))
        if result.returncode != 0:
            raise Refusal(f"test failed with exit {result.returncode}: {command}")
    seal = ChangeSeal.from_mapping(
        {
            "schema": SCHEMA,
            "task": manifest.task,
            "base": base,
            "candidate": candidate,
            "changed_paths": paths,
            "diff_sha256": digest,
            "tests": [test.to_dict() for test in tests],
            "sealed_at": datetime.now(timezone.utc).isoformat(),
        }
    )
    write_json(metadata_path(repo, SEAL), seal.to_dict())
    print(
        f"CI_HUB_CHANGE sealed=1 task={manifest.task} base={base} "
        f"candidate={candidate} paths={len(paths)} tests={len(tests)} diff_sha256={digest}"
    )


def verified_seal(
    repo: Path, *, require_unpublished_base: bool
) -> tuple[ChangeManifest, ChangeSeal]:
    seal = ChangeSeal.from_mapping(
        read_json(metadata_path(repo, SEAL), "change seal")
    )
    manifest = load_manifest(repo)
    if not is_clean(repo):
        raise Refusal("candidate clone changed after sealing")
    base = seal.base
    candidate = git(repo, "rev-parse", "HEAD^{commit}")
    parents = git(repo, "rev-list", "--parents", "-n", "1", candidate).split()
    if len(parents) != 2 or parents[1] != base:
        raise Refusal("sealed candidate no longer has its recorded single parent")
    paths = changed_paths(repo, base, candidate)
    digest = diff_digest(repo, base, candidate)
    claims = list(manifest.claims)
    outside = [path for path in paths if not any(claim_covers(claim, path) for claim in claims)]
    if outside:
        raise Refusal("sealed candidate now changes unclaimed paths: " + ", ".join(outside))
    expected = {
        "task": manifest.task,
        "base": base,
        "candidate": candidate,
        "changed_paths": tuple(paths),
        "diff_sha256": digest,
    }
    for key, value in expected.items():
        sealed = getattr(seal, key)
        if sealed != value:
            raise Refusal(f"seal mismatch for {key}: sealed={sealed!r} current={value!r}")
    if require_unpublished_base:
        current = fetch_main(repo)
        if current != base:
            raise Refusal(
                f"sealed base is stale; sealed={base} current origin/main={current}"
            )
    return manifest, seal


def command_publish(args: argparse.Namespace) -> None:
    repo = repository_root(Path(args.clone).expanduser() if args.clone else None)
    primary = repository_root(Path(args.primary).expanduser())
    manifest, seal = verified_seal(repo, require_unpublished_base=True)
    store = activation_store_path(primary, args.store)
    obligation = create_activation_obligation(
        store=store,
        task=manifest.task,
        candidate=seal.candidate,
        primary=primary,
        changed_paths=list(seal.changed_paths),
        diff_sha256=seal.diff_sha256,
    )
    try:
        published_result = run(
            (str(repo / "scripts/parent-main-write"), "publish", "HEAD"),
            cwd=repo,
        )
        emit_completed(published_result)
    except Refusal as error:
        # A publisher can push successfully and then fail while fetching or
        # verifying the result.  Its nonzero exit proves only that the complete
        # publication operation did not finish; it does not prove that the
        # candidate is absent from main.  Resolve that state from the remote
        # graph.  If even that read is unavailable, retain the obligation open:
        # unknown publication is precisely when evidence must not disappear.
        try:
            observed = fetch_main(repo)
        except Refusal as observation_error:
            transition_activation_obligation(
                store,
                obligation.obligation_id,
                "publication_unresolved",
                {
                    "activation": {
                        "state": "awaiting_publication",
                        "last_reason": (
                            f"publisher refused: {error}; remote verification "
                            f"unavailable: {observation_error}"
                        ),
                    },
                },
            )
        else:
            contained = is_ancestor(repo, seal.candidate, observed)
            transition_activation_obligation(
                store,
                obligation.obligation_id,
                (
                    "publication_confirmed_after_refusal"
                    if contained
                    else "publication_refused"
                ),
                {
                    "overall_state": "open" if contained else "cancelled",
                    "activation": {
                        "state": "pending" if contained else "publication_refused",
                        "last_reason": (
                            f"publisher refused: {error}; observed origin/main "
                            f"{observed} {'contains' if contained else 'does not contain'} "
                            f"candidate {seal.candidate}"
                        ),
                    },
                },
            )
        raise
    published = fetch_main(repo)
    if published != seal.candidate:
        contained = is_ancestor(repo, seal.candidate, published)
        transition_activation_obligation(
            store,
            obligation.obligation_id,
            "publication_moved",
            {
                "overall_state": "open" if contained else "cancelled",
                "activation": {
                    "state": "pending" if contained else "publication_refused",
                    "last_reason": (
                        f"remote moved after publication: expected {seal.candidate}, "
                        f"observed {published}"
                    ),
                },
            },
        )
        raise Refusal(
            "remote main moved after publication: "
            f"expected {seal.candidate}, observed {published}"
        )
    obligation = transition_activation_obligation(
        store,
        obligation.obligation_id,
        "publication_confirmed",
        {
            "activation": {
                "state": "pending",
                "last_reason": "published candidate awaits activation in primary",
            }
        },
    )
    print(f"CI_HUB_CHANGE published=1 task={manifest.task} candidate={published}")
    print(
        f"CI_HUB_CHANGE activation_obligation={obligation.obligation_id} "
        f"state={obligation.overall_state} store={store}"
    )


def dirty_candidate_paths(primary: Path, paths: list[str]) -> list[str]:
    result = git(primary, "status", "--porcelain=v1", "--untracked-files=all", "--", *paths)
    return [line for line in result.splitlines() if line]


def already_holds_candidate(primary: Path, entries: list[str], candidate: str,
                            paths: list[str]) -> bool:
    """True when the reported paths hold the candidate's own content, unsaved-free.

    `dirty_candidate_paths` answers "does `git status` report anything here", and
    that one question covers two situations whose right responses are opposite:

    - somebody's uncommitted work sits in a path this activation would overwrite,
      which must be inspected and preserved before anything else happens; and
    - the index and working tree already carry the candidate's own content while
      HEAD has not moved yet, which is what a `parent-main-write sync` running
      concurrently looks like part-way through, and where there is nothing to
      preserve and nothing to inspect.

    The observed case, recorded in the activation obligation store at
    2026-08-15T22:51:06Z, was the second: `A  <path> | M  <path> | M  <path>`,
    every entry with a blank second column. Porcelain v1's two columns are index
    versus HEAD and working tree versus index, so a blank second column means
    nothing is unsaved; and comparing the index against the candidate commit
    settles whose content it is. Untracked files report `??`, whose second column
    is not blank, so they never reach the comparison.

    This decides WORDING ONLY. Both situations still refuse.
    """
    if not all(len(entry) > 1 and entry[1] == " " for entry in entries):
        return False
    staged = run(
        ("git", "-C", str(primary), "diff", "--cached", "--quiet", candidate, "--", *paths),
        check=False,
    )
    return staged.returncode == 0


def is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    result = run(
        ("git", "-C", str(repo), "merge-base", "--is-ancestor", ancestor, descendant),
        check=False,
    )
    return result.returncode == 0


def activation_completion(
    record: ActivationObligation,
) -> tuple[bool, str, str | None]:
    primary = Path(record.primary)
    candidate = record.candidate
    paths = record.changed_paths
    if not primary.is_dir() or not (primary / ".git").exists():
        return False, f"primary checkout is unavailable: {primary}", None
    branch = git(primary, "branch", "--show-current")
    if branch != "main":
        return False, f"primary checkout is on {branch or 'detached HEAD'}, not main", None
    head = git(primary, "rev-parse", "HEAD^{commit}")
    if not is_ancestor(primary, candidate, head):
        return False, f"primary HEAD {head} does not contain candidate {candidate}", head
    dirty = dirty_candidate_paths(primary, list(paths))
    if dirty:
        return (
            False,
            "candidate paths differ from primary HEAD: " + " | ".join(dirty),
            head,
        )
    return True, f"primary HEAD {head} contains candidate and candidate paths are clean", head


def reconcile_activation_obligations(
    store: Path, *, primary_filter: Path | None = None
) -> tuple[list[ActivationObligation], list[ActivationObservation]]:
    latest = activation_latest(store)
    open_records = sorted(
        (
            record
            for record in latest.values()
            if record.overall_state not in ACTIVATION_CLOSED_STATES
            and (primary_filter is None or record.primary == str(primary_filter))
        ),
        key=lambda item: (item.opened_at, item.obligation_id),
    )
    satisfied: list[ActivationObligation] = []
    remaining: list[ActivationObservation] = []
    for record in open_records:
        complete, reason, head = activation_completion(record)
        if complete:
            now = utc_now()
            record = transition_activation_obligation(
                store,
                record.obligation_id,
                "activation_satisfied",
                {
                    "overall_state": "satisfied",
                    "satisfied_at": now,
                    "activation": {
                        "state": "satisfied",
                        "last_reason": reason,
                        "activated_head": head,
                    },
                },
            )
            satisfied.append(record)
        else:
            remaining.append(ActivationObservation(record, reason, head))
    return satisfied, remaining


def render_activation_obligations(
    store: Path, *, primary_filter: Path | None = None
) -> tuple[int, str]:
    satisfied, remaining = reconcile_activation_obligations(
        store, primary_filter=primary_filter
    )
    lines = [
        f"open={len(remaining)}",
        f"newly_satisfied={len(satisfied)}",
        f"evaluated={len(remaining) + len(satisfied)}",
        f"store={store}",
    ]
    for observation in remaining:
        record = observation.obligation
        lines.append(
            "  OPEN "
            f"id={record.obligation_id} task={record.task} "
            f"candidate={record.candidate} primary_head={observation.head or 'unresolved'} "
            f"activation_state={record.activation.state} "
            f"reason={observation.reason}"
        )
    for record in satisfied:
        lines.append(
            "  SATISFIED "
            f"id={record.obligation_id} candidate={record.candidate} "
            f"activated_head={record.activation.activated_head}"
        )
    if remaining:
        subjects = ", ".join(
            f"{observation.obligation.task}@{observation.obligation.candidate[:12]}"
            for observation in remaining[:3]
        )
        extra = f" (+{len(remaining) - 3} more)" if len(remaining) > 3 else ""
        lines.append(
            f"summary={len(remaining)} activation obligation(s) remain OPEN: "
            f"{subjects}{extra}"
        )
    else:
        lines.append("summary=no open activation obligations")
    return (1 if remaining else 0), "\n".join(lines) + "\n"


def command_activate(args: argparse.Namespace) -> None:
    clone = repository_root(Path(args.clone).expanduser())
    primary = repository_root(Path(args.primary).expanduser())
    manifest, seal = verified_seal(clone, require_unpublished_base=False)
    candidate = seal.candidate
    paths = list(seal.changed_paths)
    store = activation_store_path(primary, args.store)
    obligation = activation_for_candidate(store, candidate, primary)
    if obligation is None:
        prior = activation_for_candidate(
            store, candidate, primary, include_closed=True
        )
        if prior is not None and prior.overall_state == "satisfied":
            complete, reason, head = activation_completion(prior)
            if not complete:
                raise Refusal(
                    "a satisfied activation obligation no longer matches the primary: "
                    + reason
                )
            print(
                f"CI_HUB_CHANGE activated=already task={manifest.task} "
                f"candidate={candidate} activated_head={head} "
                f"activation_obligation={prior.obligation_id}"
            )
            return
        obligation = create_activation_obligation(
            store=store,
            task=manifest.task,
            candidate=candidate,
            primary=primary,
            changed_paths=paths,
            diff_sha256=seal.diff_sha256,
        )
    attempts = obligation.activation.attempts + 1
    attempted_at = utc_now()
    try:
        published = fetch_main(primary)
        if not is_ancestor(primary, candidate, published):
            raise Refusal(
                f"activation requires current main to contain candidate; candidate={candidate} "
                f"origin/main={published}"
            )
        dirty = dirty_candidate_paths(primary, paths)
        if dirty:
            # Same refusal either way; only the stated cause differs, because a
            # reader sent looking for dirty paths that do not exist stops
            # believing the tool that sent them.
            if already_holds_candidate(primary, dirty, candidate, paths):
                raise Refusal(
                    "primary candidate paths already hold this candidate's content with "
                    "nothing unsaved, but HEAD has not moved yet -- a parent-main-write "
                    "sync is most likely running concurrently. Nothing here is at risk "
                    "and there is nothing to preserve; re-run activation once the "
                    "primary settles: " + " | ".join(dirty)
                )
            raise Refusal(
                "primary candidate paths carry work that is not this candidate's; "
                "refusing to sweep it into activation -- inspect and preserve it "
                "first: " + " | ".join(dirty)
            )
        sync_result = run(
            (str(primary / "scripts/parent-main-write"), "sync", "--automatic"),
            cwd=primary,
        )
        emit_completed(sync_result)
        head = git(primary, "rev-parse", "HEAD^{commit}")
        if head != published:
            raise Refusal(f"primary activation ended at {head}, not fetched main {published}")
        if not is_ancestor(primary, candidate, head):
            raise Refusal(f"activated primary {head} does not contain candidate {candidate}")
        tree_check = run(
            ("git", "-C", str(primary), "diff", "--quiet", head, "--", *paths),
            check=False,
        )
        if tree_check.returncode != 0:
            raise Refusal("activated primary paths differ from the candidate tree")
    except Refusal as error:
        transition_activation_obligation(
            store,
            obligation.obligation_id,
            "activation_deferred",
            {
                "activation": {
                    "state": "deferred",
                    "attempts": attempts,
                    "last_attempt_at": attempted_at,
                    "last_reason": str(error),
                }
            },
        )
        raise
    obligation = transition_activation_obligation(
        store,
        obligation.obligation_id,
        "activation_satisfied",
        {
            "overall_state": "satisfied",
            "satisfied_at": utc_now(),
            "activation": {
                "state": "satisfied",
                "attempts": attempts,
                "last_attempt_at": attempted_at,
                "last_reason": "activation completed and verified",
                "activated_head": head,
            },
        },
    )
    print(
        f"CI_HUB_CHANGE activated=1 task={manifest.task} candidate={candidate} "
        f"activated_head={head} paths={len(paths)} "
        f"activation_obligation={obligation.obligation_id}"
    )
    print(
        "CI_HUB_CHANGE LIMIT: git working-tree update is serialized but not a "
        "filesystem-atomic multi-file swap; see ai_docs/ci-hub-change-clone-protocol.md"
    )


def command_status(args: argparse.Namespace) -> None:
    repo = repository_root(Path(args.clone).expanduser() if args.clone else None)
    report: dict[str, Any] = {
        "schema": SCHEMA,
        "manifest": load_manifest(repo).to_dict(),
        "head": git(repo, "rev-parse", "HEAD^{commit}"),
        "clean": is_clean(repo),
        "seal": None,
    }
    seal_path = metadata_path(repo, SEAL)
    if seal_path.is_file():
        report["seal"] = ChangeSeal.from_mapping(
            read_json(seal_path, "change seal")
        ).to_dict()
    print(json.dumps(report, indent=2, sort_keys=True))


def unpushed_in_clone(repo: Path) -> list[str]:
    """Commits on this clone's OWN branches that no remote-tracking ref contains.

    The scope is `--branches` plus `HEAD`: every local branch, and also the
    detached HEAD an interrupted change leaves behind, which `--branches` alone
    would drop. Both halves are load-bearing and are bracketed separately.

    The scope is deliberately NOT `--all`. `--all` additionally sweeps
    fetch-destination ref namespaces, whose contents exist on a remote by
    construction: `agent-utils/py/pr_landing_planner/collect.py` fetches
    `refs/pull/<n>/head` into `refs/pr-landing-planner/pr-<n>` and a base
    `refs/heads/<ref>` into `refs/pr-landing-planner/base-<digest>`. Those
    commits are reachable from no *remote-tracking* ref only because they were
    filed outside `refs/remotes/`, so `--all` reported them as at risk when
    nothing was. Measured 2026-08-15 on two real clones:
    `dev-hermit-p1-a-producer-path` 38 under `--all` versus 1 here, and
    `dev-hermit-p1-validate-writes-receipts` 36 versus 0.

    Tags stay out of scope for the reason the parent's unpushed-commits health
    gate excludes them: a tag can point into a submodule's history and report
    commits that no push could ever retire.

    This answers against the clone's remote-tracking refs, which are only a
    snapshot from its last fetch, so `command_dispose` refreshes them first.
    """
    out = git(repo, "rev-list", "--branches", "HEAD", "--not", "--remotes")
    return [line for line in out.splitlines() if line]


SCOPED_REF_PREFIXES = ("refs/heads/", "refs/remotes/", "refs/tags/")


def unconsidered_unpushed(repo: Path) -> tuple[int, list[str]]:
    """Commits `unpushed_in_clone` does not consider, and the namespaces present.

    Narrowing the refusal must not make anything invisible, so disposal reports
    what it declined to weigh instead of silently dropping it. The namespaces
    are the out-of-scope ones this clone has, which is where such commits live;
    it is a place to look, not a per-commit attribution.

    The filtering is done here rather than with `for-each-ref --exclude`,
    because `--exclude=refs/heads/*` does not match a branch whose name itself
    contains a slash -- `refs/heads/ci-hub/p1-a-producer-path` survived it -- so
    that spelling reports local branches as out of scope when they are not.
    """
    out = git(
        repo, "rev-list", "--exclude=refs/tags/*", "--all",
        "--not", "--branches", "HEAD", "--remotes",
    )
    commits = [line for line in out.splitlines() if line]
    if not commits:
        return 0, []
    refs = git(repo, "for-each-ref", "--format=%(refname)").splitlines()
    spaces = sorted(
        {
            "/".join(ref.split("/")[:2]) + "/*"
            for ref in refs
            if ref and not ref.startswith(SCOPED_REF_PREFIXES)
        }
    )
    return len(commits), spaces


def summarize_refs(refs: Sequence[str], limit: int = 5) -> str:
    """Name where a disposed commit can be found again, readably.

    Recovery refs exist so a human can retrieve the commit, and fetching every
    head to answer containment can make that list enormous: measured
    2026-08-15, one clone's head sat on 130 refs, mostly `origin/rescue/auto-*`.
    A wall of names is not a recovery instruction, so lead with `main` where it
    qualifies and count the rest.
    """
    preferred = [ref for ref in refs if ref.rsplit("/", 1)[-1] == "main"]
    ordered = preferred + [ref for ref in refs if ref not in preferred]
    shown = ",".join(ordered[:limit])
    return f"{shown},+{len(ordered) - limit}-more" if len(ordered) > limit else shown


def refresh_remote_tracking(repo: Path) -> bool:
    """Bring `refs/remotes/origin/*` up to date with the remote; report success.

    A clone's remote-tracking refs record what the remote looked like when it
    last fetched, and disposal must not decide containment from that snapshot in
    EITHER direction. Stale refs under-report what is pushed: measured
    2026-08-15, clone `dev-hermit-p1-a-producer-path` carried exactly one
    remote-tracking ref, `origin/main` pinned at e8ba1762 (2026-08-12), while
    its own commit 80e50e9e was by then on the remote's `main` and on its own
    remote branch. They can equally over-report it, because a ref the snapshot
    still shows may since have been moved off that commit.

    Fetching every head rather than just `main` asks the accurate question: a
    clone's work may have been pushed to any branch, and guessing the wrong one
    produces exactly the false refusal this is fixing. The cost is that a
    refused clone keeps the remote-tracking refs and objects this brought in;
    they hold no local work and any later fetch would recreate them.
    """
    try:
        network_git(repo, "fetch", "--quiet", "origin", "+refs/heads/*:refs/remotes/origin/*")
    except Refusal:
        return False
    return True


def command_dispose(args: argparse.Namespace) -> None:
    """Retire one per-change clone, refusing whenever anything is at risk.

    THE REFUSAL IS THE POINT. `create` already tells a caller that "a
    coordinator must disposition the per-change clone before creating another",
    and until now no operation did that, so clones accumulated and the
    disposition decision rested on an agent noticing unpushed work by hand. One
    such clone held two commits that existed on no remote ref for two days.
    """
    destination = Path(args.clone).expanduser().resolve()
    if not destination.is_dir():
        raise Refusal(f"clone path does not exist: {destination}")
    # Check for the checkout before asking Git to resolve a root: pointed at a
    # plain directory, root resolution surfaces an unrelated tool's hint rather
    # than a refusal, and a refusal a caller cannot read is not a refusal.
    if not (destination / ".git").exists():
        raise Refusal(f"{destination} is not a Git checkout; dispose only retires change clones")
    repo = repository_root(destination)
    if repo != destination:
        raise Refusal(
            f"{destination} is not the root of a checkout (root is {repo}); "
            "dispose only ever removes a change clone it was pointed at exactly"
        )
    # Refuses unless a change manifest names this exact path, so dispose can
    # never be aimed at the primary or at an unrelated checkout.
    manifest = load_manifest(repo)
    if not is_clean(repo):
        dirty = git(repo, "status", "--porcelain")
        count = len([line for line in dirty.splitlines() if line])
        raise Refusal(
            f"clone {destination} has {count} uncommitted or untracked path(s); "
            "commit or remove them deliberately, never by disposal"
        )
    # Every containment answer below is only as good as this clone's picture of
    # the remote, so refresh it first and refuse outright when that is not
    # possible. Unverifiable is not the same as safe.
    if not refresh_remote_tracking(repo):
        raise Refusal(
            f"cannot reach the remote from clone {destination}, so nothing here "
            "can be shown to exist anywhere else; disposal decides containment "
            "against the remote, never against a snapshot of unknown age"
        )
    unpushed = unpushed_in_clone(repo)
    if unpushed:
        branch = git(repo, "rev-parse", "--abbrev-ref", "HEAD")
        raise Refusal(
            f"clone {destination} holds {len(unpushed)} commit(s) on its own "
            f"branches that no remote contains (branch {branch}, task "
            f"{manifest.task}, tip {unpushed[0]}); PRESERVE THEM FIRST "
            "-- disposal never removes work that exists nowhere else"
        )
    head = git(repo, "rev-parse", "HEAD^{commit}")
    reachable = git(repo, "branch", "--remotes", "--contains", head).split()
    if not reachable:
        raise Refusal(
            f"clone {destination} HEAD {head} is on no remote-tracking branch, "
            "so there would be no recovery commit to record"
        )
    skipped, namespaces = unconsidered_unpushed(repo)
    shutil.rmtree(destination)
    print(
        f"CI_HUB_CHANGE disposed={destination} task={manifest.task} "
        f"recovery_sha={head} recovery_refs={summarize_refs(reachable)}"
    )
    if skipped:
        print(
            f"CI_HUB_CHANGE unconsidered={skipped} namespaces={','.join(namespaces)} "
            "(outside this clone's branches; not weighed in the decision)"
        )


def command_activation_obligations(args: argparse.Namespace) -> int:
    primary = Path(args.primary).expanduser().resolve()
    store = activation_store_path(primary, args.store)
    code, report = render_activation_obligations(store, primary_filter=primary)
    print(report, end="")
    return code


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create", description="create one fresh per-change clone", help="create one fresh per-change clone")
    create.add_argument("--task", required=True)
    create.add_argument("--path", action="append", default=[])
    create.add_argument("--clone", default="~/temp/dev-hermit")
    create.add_argument("--remote", default="https://github.com/rrnewton/dev-hermit.git")
    create.set_defaults(func=command_create)
    seal = commands.add_parser("seal", description="test and seal one exact candidate commit", help="test and seal one exact candidate commit")
    seal.add_argument("--clone")
    seal.add_argument("--test", action="append", default=[])
    seal.set_defaults(func=command_seal)
    publish = commands.add_parser("publish", description="publish a still-current sealed commit", help="publish a still-current sealed commit")
    publish.add_argument("--clone")
    publish.add_argument("--primary", default="/home/newton/work/dev-hermit")
    publish.add_argument("--store", type=Path)
    publish.set_defaults(func=command_publish)
    activate = commands.add_parser(
        "activate", description="sync the published commit into a clean primary", help="sync the published commit into a clean primary"
    )
    activate.add_argument("--clone", required=True)
    activate.add_argument("--primary", default="/home/newton/work/dev-hermit")
    activate.add_argument("--store", type=Path)
    activate.set_defaults(func=command_activate)
    obligations = commands.add_parser(
        "activation-obligations",
        description="close completed activations and report every still-open obligation",
        help="close completed activations and report every still-open obligation",
    )
    obligations.add_argument("--primary", default="/home/newton/work/dev-hermit")
    obligations.add_argument("--store", type=Path)
    obligations.set_defaults(func=command_activation_obligations)
    dispose = commands.add_parser(
        "dispose",
        description="retire one per-change clone, refusing if anything is at risk",
        help="retire one per-change clone, refusing if anything is at risk",
    )
    dispose.add_argument("--clone", required=True)
    dispose.set_defaults(func=command_dispose)
    status = commands.add_parser("status", description="show manifest and seal state", help="show manifest and seal state")
    status.add_argument("--clone")
    status.set_defaults(func=command_status)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        return args.func(args) or 0
    except Refusal as error:
        print(f"ci-hub-clone-change: REFUSED: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
