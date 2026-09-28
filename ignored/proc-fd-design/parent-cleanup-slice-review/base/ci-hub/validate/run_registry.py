#!/usr/bin/env python3
"""Durable handles for ci-hub-owned validation services and observer panes."""

from __future__ import annotations

import fcntl
import json
import os
import socket
import sys
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Any, Mapping, TypedDict, cast

import service_result


SCHEMA_VERSION = 1
PRODUCER = "ci-hub/validate/run_registry.py"
OWNER_WATCH_CORRECTION_TRANSACTION = "owner-watch-handle-correction.transaction"


def is_current_schema_version(value: object) -> bool:
    """Accept only the exact JSON integer run-handle schema marker."""
    return type(value) is int and value == SCHEMA_VERSION


class RunKind(str, Enum):
    """The existing validate-lock kinds, recorded on every current handle."""

    VALIDATE = "validate"
    REVERIE_VALIDATE = "reverie-validate"
    FROZEN_VALIDATE = "frozen-validate"
    BENCH = "bench"

    @property
    def counts_as_validation(self) -> bool:
        return self is not RunKind.BENCH


class RunState(str, Enum):
    PREPARING = "preparing"
    LAUNCHING = "launching"
    RUNNING = "running"
    REFUSED = "refused"
    COMPLETED = "completed"
    KILLED = "killed"
    UNKNOWN = "unknown"
    FAILED = "failed"

    @property
    def lock_admissible(self) -> bool:
        return self in {RunState.LAUNCHING, RunState.RUNNING}


class AdmissionState(str, Enum):
    ADMITTED = "admitted"
    REFUSED = "refused"


class AdmissionRefusalReason(str, Enum):
    STALE_BASE = "stale-base"
    VALIDATION_LOCK_REFUSAL = "validation-lock-refusal"


class ProcessIdentityRecord(TypedDict):
    pid: int
    start_ticks: int
    boot_id: str


class AdmissionResultRecord(TypedDict, total=False):
    state: str
    recorded_at: str
    run_number: int
    reason: str
    exit_code: int


class CommitStatusPublicationRecord(TypedDict, total=False):
    state: str
    recorded_at: str
    repository: str
    sha: str
    reason: str
    detail: str
    description: str
    receipt_commit: str
    receipt_path: str


class TerminationRequestRecord(TypedDict, total=False):
    requested_at: str
    agent: str
    pid: int
    reason: str
    confirmed_at: str


class RunRecord(TypedDict, total=False):
    """Complete serialized shape; required and conditional fields are checked below."""

    schema_version: int
    kind: str
    state: str
    unit: str
    target: str
    repo: str
    branch: str | None
    checkout: str
    source_checkout: str
    materialized_target: bool
    temporary_checkout: bool
    cargo_home: str
    log: str
    agent: str
    pr: int | None
    started_at: str
    parent_checkout_head: str | None
    producer: str
    admission: str
    pane_role: str
    validate_lock_child_deadline_seconds: int
    e2e_result_root: str
    safe_ci_dag_runner_log_dir: str
    hermit_run_timeout_seconds: int
    validation_kind: str
    result_record: str
    measured_against_superseded_tip: bool
    current_main_before_launch: str
    target_contains_current_main_before_launch: bool
    qualifying_receipt: bool
    dag_jobs: int
    workspace_id: str | None
    tab_id: str | None
    pane_id: str | None
    pane_title: str | None
    process_identity: ProcessIdentityRecord | None
    admission_result: AdmissionResultRecord
    result: str
    exit_code: int | None
    detail: str | None
    finished_at: str
    signal: int
    executed_nodes: int | None
    executed_tests: int | None
    passed_tests: int | None
    final_validate_status: str | None
    service_result_schema: int
    selection_mode: str | None
    scorecard_writeback: dict[str, str] | None
    scorecard_writeback_files: list[dict[str, object]]
    scorecard_handoff: str
    result_source: str
    observed_safe_ci_cgroups: list[str]
    canonical_verdict: str
    wrapper_exit_code: int
    commit_status_publication: CommitStatusPublicationRecord
    termination_requests: list[TerminationRequestRecord]
    measured_result: str
    checkout_removed_at: str
    wrkslots_slot: str
    wrkslots_generation: int
    archived_orphaned_receipts: list[str]
    cargo_home_removed_at: str
    host: str
    results: str
    cleanup_error: str | None


VALIDATION_STATES = frozenset(
    {
        RunState.PREPARING,
        RunState.LAUNCHING,
        RunState.RUNNING,
        RunState.REFUSED,
        RunState.COMPLETED,
        RunState.KILLED,
        RunState.UNKNOWN,
    }
)
BENCH_STATES = frozenset(
    {
        RunState.LAUNCHING,
        RunState.RUNNING,
        RunState.REFUSED,
        RunState.COMPLETED,
        RunState.FAILED,
    }
)


@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    start_ticks: int
    boot_id: str


@dataclass(frozen=True)
class AdmissionResult:
    state: AdmissionState
    recorded_at: str
    run_number: int | None
    reason: AdmissionRefusalReason | None
    exit_code: int | None


@dataclass(frozen=True)
class TerminationRequest:
    requested_at: str
    agent: str
    pid: int
    reason: str
    confirmed_at: str | None


@dataclass(frozen=True)
class RunHandle:
    """The complete current contract shared by every producer and consumer.

    Historical schema-1 rows remain readable because a process started before
    deployment may finish after it. Their missing `kind` discriminator and
    field generations are never promoted into this type. Current writes retain
    wire schema 1 for compatibility with already-deployed readers and require
    `kind`, which keeps validation and pressure-test state meanings distinct.
    """

    kind: RunKind
    state: RunState
    unit: str
    target: str
    repo: str
    branch: str | None
    checkout: str
    log: str
    agent: str
    started_at: str
    producer: str
    admission: str
    process_identity: ProcessIdentity | None
    admission_result: AdmissionResult | None
    termination_requests: tuple[TerminationRequest, ...]
    raw: RunRecord

    @property
    def lock_admissible(self) -> bool:
        return self.state.lock_admissible

    @property
    def counts_as_validation(self) -> bool:
        return self.kind.counts_as_validation


COMMON_FIELDS = frozenset(
    {
        "schema_version",
        "kind",
        "state",
        "unit",
        "target",
        "repo",
        "checkout",
        "log",
        "agent",
        "started_at",
        "producer",
        "admission",
        "process_identity",
        "admission_result",
        "result",
        "exit_code",
        "detail",
        "finished_at",
    }
)

VALIDATION_FIELDS = COMMON_FIELDS | frozenset(
    {
        "source_checkout",
        "materialized_target",
        "branch",
        "temporary_checkout",
        "cargo_home",
        "pr",
        "parent_checkout_head",
        "pane_role",
        "validate_lock_child_deadline_seconds",
        "e2e_result_root",
        "safe_ci_dag_runner_log_dir",
        "hermit_run_timeout_seconds",
        "validation_kind",
        "result_record",
        "measured_against_superseded_tip",
        "current_main_before_launch",
        "target_contains_current_main_before_launch",
        "qualifying_receipt",
        "dag_jobs",
        "workspace_id",
        "tab_id",
        "pane_id",
        "pane_title",
        "signal",
        "executed_nodes",
        "executed_tests",
        "passed_tests",
        "final_validate_status",
        "service_result_schema",
        "selection_mode",
        "scorecard_writeback",
        "scorecard_writeback_files",
        "scorecard_handoff",
        "result_source",
        "observed_safe_ci_cgroups",
        "canonical_verdict",
        "wrapper_exit_code",
        "commit_status_publication",
        "measured_result",
        "checkout_removed_at",
        "wrkslots_slot",
        "wrkslots_generation",
        "archived_orphaned_receipts",
        "cargo_home_removed_at",
        "host",
        "termination_requests",
    }
)

BENCH_FIELDS = COMMON_FIELDS | frozenset({"results", "cleanup_error"})

_OPTIONAL_TEXT_FIELDS = frozenset(
    {
        "source_checkout",
        "branch",
        "cargo_home",
        "parent_checkout_head",
        "pane_role",
        "e2e_result_root",
        "safe_ci_dag_runner_log_dir",
        "validation_kind",
        "result_record",
        "current_main_before_launch",
        "workspace_id",
        "tab_id",
        "pane_id",
        "pane_title",
        "result",
        "detail",
        "finished_at",
        "final_validate_status",
        "selection_mode",
        "result_source",
        "scorecard_handoff",
        "canonical_verdict",
        "measured_result",
        "checkout_removed_at",
        "wrkslots_slot",
        "cargo_home_removed_at",
        "host",
        "results",
        "cleanup_error",
    }
)
_OPTIONAL_INTEGER_FIELDS = frozenset(
    {
        "pr",
        "wrkslots_generation",
        "validate_lock_child_deadline_seconds",
        "hermit_run_timeout_seconds",
        "dag_jobs",
        "exit_code",
        "signal",
        "executed_nodes",
        "executed_tests",
        "passed_tests",
        "service_result_schema",
        "wrapper_exit_code",
    }
)
_OPTIONAL_BOOLEAN_FIELDS = frozenset(
    {
        "temporary_checkout",
        "measured_against_superseded_tip",
        "target_contains_current_main_before_launch",
        "qualifying_receipt",
    }
)
_OPTIONAL_STRING_LIST_FIELDS = frozenset(
    {"observed_safe_ci_cgroups", "archived_orphaned_receipts"}
)


def _error(field: str, detail: str) -> RuntimeError:
    return RuntimeError(f"validation-run-handle-{field}: {detail}")


def _commit_status_publication(value: Any) -> None:
    if not isinstance(value, dict):
        raise _error("commit_status_publication", "must be an object")
    state = value.get("state")
    common = {"state", "recorded_at", "repository", "sha"}
    if state in {"published", "unchanged"}:
        required = common | {"description", "receipt_commit", "receipt_path"}
    elif state == "failed":
        required = common | {"detail"}
    elif state == "not-attempted":
        required = common | {"reason"}
    else:
        raise _error(
            "commit_status_publication.state",
            f"unsupported value {state!r}",
        )
    if set(value) != required:
        raise _error(
            "commit_status_publication",
            f"{state} must contain exactly {', '.join(sorted(required))}",
        )
    _timestamp(
        value,
        "recorded_at",
        error_field="commit_status_publication.recorded_at",
    )
    repository = _required_text(value, "repository")
    if repository not in {"rrnewton/hermit", "rrnewton/reverie"}:
        raise _error(
            "commit_status_publication.repository",
            f"unsupported value {repository!r}",
        )
    sha = _required_text(value, "sha")
    if len(sha) != 40 or any(ch not in "0123456789abcdef" for ch in sha):
        raise _error(
            "commit_status_publication.sha",
            "must be a lowercase forty-hex commit",
        )
    for field in required - common:
        _required_text(value, field)


def _required_text(value: Mapping[str, Any], field: str) -> str:
    raw = value.get(field)
    if not isinstance(raw, str) or not raw.strip():
        raise _error(field, "must be a nonempty string")
    return raw


def _optional_text(value: Mapping[str, Any], field: str) -> None:
    if (
        field in value
        and value[field] is not None
        and not isinstance(value[field], str)
    ):
        raise _error(field, "must be a string or null")


def _optional_integer(value: Mapping[str, Any], field: str) -> None:
    raw = value.get(field)
    if (
        field in value
        and raw is not None
        and (not isinstance(raw, int) or isinstance(raw, bool))
    ):
        raise _error(field, "must be an integer or null")


def _optional_boolean(value: Mapping[str, Any], field: str) -> None:
    raw = value.get(field)
    if field in value and raw is not None and not isinstance(raw, bool):
        raise _error(field, "must be a boolean or null")


def _optional_string_list(value: Mapping[str, Any], field: str) -> None:
    raw = value.get(field)
    if field not in value or raw is None:
        return
    if not isinstance(raw, list) or any(not isinstance(item, str) for item in raw):
        raise _error(field, "must be a list of strings or null")


def _scorecard_writeback(value: Any) -> dict[str, str] | None:
    try:
        return service_result.read_scorecard_writeback_value(value)
    except RuntimeError as error:
        detail = str(error).removeprefix(
            "validation-service-result-scorecard_writeback: "
        )
        raise _error("scorecard_writeback", detail) from error


SCORECARD_WRITEBACK_PATHS = (
    "SCORECARD.md",
    "ci/compat-envelope/cells.json",
)


def read_scorecard_writeback_files(value: Any) -> list[dict[str, object]]:
    """Validate the exact generated bytes captured after scorecard writeback."""

    if not isinstance(value, list) or len(value) != len(SCORECARD_WRITEBACK_PATHS):
        raise _error(
            "scorecard_writeback_files",
            "must identify exactly SCORECARD.md and ci/compat-envelope/cells.json",
        )
    rows: dict[str, dict[str, object]] = {}
    for index, item in enumerate(value):
        if not isinstance(item, dict) or set(item) != {"path", "sha256", "size"}:
            raise _error(
                "scorecard_writeback_files",
                f"entry {index} must contain exactly path, sha256, and size",
            )
        path = item.get("path")
        sha256 = item.get("sha256")
        size = item.get("size")
        if not isinstance(path, str) or path not in SCORECARD_WRITEBACK_PATHS:
            raise _error(
                "scorecard_writeback_files", f"entry {index} has an unknown path"
            )
        if path in rows:
            raise _error(
                "scorecard_writeback_files", f"entry {index} repeats {path!r}"
            )
        if (
            not isinstance(sha256, str)
            or len(sha256) != 64
            or any(character not in "0123456789abcdef" for character in sha256)
        ):
            raise _error(
                "scorecard_writeback_files",
                f"entry {index} has no lowercase SHA-256",
            )
        if not isinstance(size, int) or isinstance(size, bool) or size < 0:
            raise _error(
                "scorecard_writeback_files",
                f"entry {index} has no nonnegative byte size",
            )
        rows[path] = {"path": path, "sha256": sha256, "size": size}
    if set(rows) != set(SCORECARD_WRITEBACK_PATHS):
        raise _error(
            "scorecard_writeback_files",
            "must identify exactly SCORECARD.md and ci/compat-envelope/cells.json",
        )
    return [rows[path] for path in SCORECARD_WRITEBACK_PATHS]


def _positive_integer(
    value: Mapping[str, Any], field: str, *, error_field: str | None = None
) -> None:
    raw = value.get(field)
    if not isinstance(raw, int) or isinstance(raw, bool) or raw <= 0:
        raise _error(error_field or field, "must be a positive integer")


def _nonnegative_integer(value: Mapping[str, Any], field: str) -> None:
    raw = value.get(field)
    if not isinstance(raw, int) or isinstance(raw, bool) or raw < 0:
        raise _error(field, "must be a nonnegative integer")


def _optional_commit(value: Mapping[str, Any], field: str) -> None:
    raw = value.get(field)
    if raw is None:
        return
    if (
        not isinstance(raw, str)
        or len(raw) != 40
        or any(ch not in "0123456789abcdef" for ch in raw)
    ):
        raise _error(field, "must be null or a lowercase forty-hex commit")


def _process_identity(value: Mapping[str, Any]) -> ProcessIdentity | None:
    raw = value.get("process_identity")
    if raw is None:
        return None
    if not isinstance(raw, dict) or set(raw) != {"pid", "start_ticks", "boot_id"}:
        raise _error(
            "process_identity", "must contain exactly pid, start_ticks, and boot_id"
        )
    pid = raw.get("pid")
    start_ticks = raw.get("start_ticks")
    boot_id = raw.get("boot_id")
    if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 0:
        raise _error("process_identity.pid", "must be a positive integer")
    if (
        not isinstance(start_ticks, int)
        or isinstance(start_ticks, bool)
        or start_ticks <= 0
    ):
        raise _error("process_identity.start_ticks", "must be a positive integer")
    if not isinstance(boot_id, str) or not boot_id.strip():
        raise _error("process_identity.boot_id", "must be a nonempty string")
    return ProcessIdentity(pid=pid, start_ticks=start_ticks, boot_id=boot_id)


def _timestamp(
    value: Mapping[str, Any], field: str, *, error_field: str | None = None
) -> str:
    error_field = error_field or field
    raw = _required_text(value, field)
    try:
        parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError as error:
        raise _error(error_field, "must be an RFC3339 timestamp") from error
    if parsed.utcoffset() is None:
        raise _error(error_field, "must include a UTC offset")
    return raw


def _admission_result(
    value: Mapping[str, Any], *, kind: RunKind
) -> AdmissionResult | None:
    raw = value.get("admission_result")
    if raw is None:
        # A handle written before this field existed remains readable, but its
        # missing evidence stays unknown. It is not inferred from the log.
        return None
    if not isinstance(raw, dict):
        raise _error("admission_result", "must be an object")
    try:
        state = AdmissionState(_required_text(raw, "state"))
    except ValueError as error:
        raise _error(
            "admission_result.state", f"unsupported value {raw.get('state')!r}"
        ) from error
    recorded_at = _timestamp(
        raw, "recorded_at", error_field="admission_result.recorded_at"
    )
    if state is AdmissionState.ADMITTED:
        required = {"state", "recorded_at"}
        allowed = required | ({"run_number"} if kind.counts_as_validation else set())
        if not required.issubset(raw) or not set(raw).issubset(allowed):
            raise _error(
                "admission_result",
                f"admitted {kind.value} must contain state and recorded_at"
                + (", with optional run_number" if kind.counts_as_validation else ""),
            )
        run_number = None
        if "run_number" in raw:
            _positive_integer(
                raw, "run_number", error_field="admission_result.run_number"
            )
            run_number = int(raw["run_number"])
        return AdmissionResult(state, recorded_at, run_number, None, None)
    if set(raw) != {"state", "recorded_at", "reason", "exit_code"}:
        raise _error(
            "admission_result",
            "refused must contain exactly state, recorded_at, reason, and exit_code",
        )
    try:
        reason = AdmissionRefusalReason(_required_text(raw, "reason"))
    except ValueError as error:
        raise _error(
            "admission_result.reason", f"unsupported value {raw.get('reason')!r}"
        ) from error
    exit_code = raw.get("exit_code")
    if not isinstance(exit_code, int) or isinstance(exit_code, bool) or exit_code == 0:
        raise _error("admission_result.exit_code", "must be a nonzero integer")
    return AdmissionResult(state, recorded_at, None, reason, exit_code)


def _termination_requests(value: Mapping[str, Any]) -> tuple[TerminationRequest, ...]:
    raw = value.get("termination_requests")
    if raw is None:
        return ()
    if not isinstance(raw, list):
        raise _error("termination_requests", "must be a list")
    requests: list[TerminationRequest] = []
    for index, item in enumerate(raw):
        field = f"termination_requests[{index}]"
        if not isinstance(item, dict):
            raise _error(field, "must be an object")
        required = {"requested_at", "agent", "pid", "reason"}
        allowed = required | {"confirmed_at"}
        if not required.issubset(item) or not set(item).issubset(allowed):
            raise _error(
                field,
                "must contain requested_at, agent, pid, and reason, with optional confirmed_at",
            )
        requested_at = _timestamp(
            item, "requested_at", error_field=f"{field}.requested_at"
        )
        agent = _required_text(item, "agent")
        reason = _required_text(item, "reason")
        pid = item.get("pid")
        if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 0:
            raise _error(f"{field}.pid", "must be a positive integer")
        confirmed_at = None
        if "confirmed_at" in item:
            confirmed_at = _timestamp(
                item, "confirmed_at", error_field=f"{field}.confirmed_at"
            )
        requests.append(
            TerminationRequest(
                requested_at=requested_at,
                agent=agent,
                pid=pid,
                reason=reason,
                confirmed_at=confirmed_at,
            )
        )
    return tuple(requests)


def parse_current_record(value: Mapping[str, Any]) -> RunHandle:
    if not is_current_schema_version(value.get("schema_version")):
        raise _error(
            "schema_version",
            f"expected {SCHEMA_VERSION}, got {value.get('schema_version')!r}",
        )
    try:
        kind = RunKind(_required_text(value, "kind"))
    except ValueError as error:
        raise _error("kind", f"unsupported value {value.get('kind')!r}") from error
    try:
        state = RunState(_required_text(value, "state"))
    except ValueError as error:
        raise _error("state", f"unsupported value {value.get('state')!r}") from error

    allowed_states = BENCH_STATES if kind is RunKind.BENCH else VALIDATION_STATES
    if state not in allowed_states:
        raise _error("state", f"{state.value!r} is not valid for kind {kind.value!r}")
    allowed_fields = BENCH_FIELDS if kind is RunKind.BENCH else VALIDATION_FIELDS
    unknown = sorted(set(value) - allowed_fields)
    if unknown:
        raise _error(
            "field", f"unknown field(s) for kind {kind.value!r}: {', '.join(unknown)}"
        )

    unit = _required_text(value, "unit")
    target = _required_text(value, "target")
    repo = _required_text(value, "repo")
    checkout = _required_text(value, "checkout")
    log = _required_text(value, "log")
    agent = _required_text(value, "agent")
    started_at = _required_text(value, "started_at")
    producer = _required_text(value, "producer")
    admission = _required_text(value, "admission")
    if producer != PRODUCER:
        raise _error("producer", f"must be {PRODUCER!r}")
    if not unit.endswith(".service"):
        raise _error("unit", "must end in .service")
    if len(target) != 40 or any(ch not in "0123456789abcdef" for ch in target):
        raise _error("target", "must be a lowercase forty-hex commit")
    expected_repo = (
        "rrnewton/reverie" if kind is RunKind.REVERIE_VALIDATE else "rrnewton/hermit"
    )
    if repo != expected_repo:
        raise _error("repo", f"kind {kind.value!r} requires {expected_repo!r}")
    expected_admission = (
        "frozen-validate" if kind is RunKind.FROZEN_VALIDATE else "ci-hub validate-lock"
    )
    if admission != expected_admission:
        raise _error(
            "admission", f"kind {kind.value!r} requires {expected_admission!r}"
        )

    for field in _OPTIONAL_TEXT_FIELDS & allowed_fields:
        _optional_text(value, field)
    for field in _OPTIONAL_INTEGER_FIELDS & allowed_fields:
        _optional_integer(value, field)
    for field in _OPTIONAL_BOOLEAN_FIELDS & allowed_fields:
        _optional_boolean(value, field)
    for field in _OPTIONAL_STRING_LIST_FIELDS & allowed_fields:
        _optional_string_list(value, field)
    identity = _process_identity(value)
    admission_result = _admission_result(value, kind=kind)
    termination_requests = _termination_requests(value)
    branch_value = value.get("branch")
    branch = branch_value if isinstance(branch_value, str) else None
    if "commit_status_publication" in value:
        _commit_status_publication(value.get("commit_status_publication"))

    if kind is not RunKind.BENCH:
        if value.get("pane_role") != "observer-only":
            raise _error("pane_role", "validation handles require observer-only")
        if not isinstance(value.get("temporary_checkout"), bool):
            raise _error("temporary_checkout", "must be a boolean")
        if "materialized_target" in value and not isinstance(
            value.get("materialized_target"), bool
        ):
            raise _error("materialized_target", "must be a boolean when present")
        has_slot = "wrkslots_slot" in value
        has_generation = "wrkslots_generation" in value
        if has_slot != has_generation:
            raise _error(
                "wrkslots_identity",
                "wrkslots_slot and wrkslots_generation must be recorded together",
            )
        if has_slot:
            _required_text(value, "wrkslots_slot")
            _positive_integer(value, "wrkslots_generation")
        if value.get("materialized_target") is True:
            if value.get("temporary_checkout") is not True:
                raise _error(
                    "materialized_target",
                    "requires temporary_checkout=true",
                )
            if not has_slot:
                raise _error(
                    "materialized_target",
                    "requires an exact wrkslots identity",
                )
            if value.get("source_checkout") == value.get("checkout"):
                raise _error(
                    "materialized_target",
                    "requires distinct source and validation checkouts",
                )
            if kind is RunKind.FROZEN_VALIDATE:
                raise _error(
                    "materialized_target",
                    "is incompatible with frozen validation",
                )
        if "scorecard_handoff" in value:
            _required_text(value, "scorecard_handoff")
            if value.get("materialized_target") is not True:
                raise _error(
                    "scorecard_handoff",
                    "requires materialized_target=true",
                )
            if repo != "rrnewton/hermit":
                raise _error(
                    "scorecard_handoff",
                    "is valid only for Hermit validation handles",
                )
        for field in (
            "source_checkout",
            "cargo_home",
            "parent_checkout_head",
            "validate_lock_child_deadline_seconds",
        ):
            if field not in value:
                raise _error(field, "is required for validation handles")
        _required_text(value, "source_checkout")
        if value.get("branch") is not None:
            _required_text(value, "branch")
        _required_text(value, "cargo_home")
        _optional_commit(value, "parent_checkout_head")
        _positive_integer(value, "validate_lock_child_deadline_seconds")
        if value.get("pr") is not None:
            _positive_integer(value, "pr")
        if value.get("dag_jobs") is not None:
            _positive_integer(value, "dag_jobs")
        if repo == "rrnewton/hermit":
            for field in (
                "e2e_result_root",
                "safe_ci_dag_runner_log_dir",
                "hermit_run_timeout_seconds",
            ):
                if field not in value:
                    raise _error(field, "is required for Hermit validation handles")
            _required_text(value, "e2e_result_root")
            _required_text(value, "safe_ci_dag_runner_log_dir")
            _positive_integer(value, "hermit_run_timeout_seconds")
        marker = value.get("service_result_schema")
        if marker is not None:
            if repo != "rrnewton/hermit":
                raise _error(
                    "service_result_schema",
                    "is valid only for Hermit validation handles",
                )
            if not service_result.is_supported_schema_version(marker):
                raise _error(
                    "service_result_schema",
                    "expected "
                    f"{service_result.supported_schema_versions_text()}, got {marker!r}",
                )
        if "scorecard_writeback" in value:
            if marker not in service_result.WRITEBACK_SCHEMA_VERSIONS:
                raise _error(
                    "scorecard_writeback",
                    "is valid only with the current validation service result schema",
                )
            _scorecard_writeback(value.get("scorecard_writeback"))
        if "scorecard_writeback_files" in value:
            if value.get("materialized_target") is not True:
                raise _error(
                    "scorecard_writeback_files",
                    "requires materialized_target=true",
                )
            if repo != "rrnewton/hermit":
                raise _error(
                    "scorecard_writeback_files",
                    "is valid only for Hermit validation handles",
                )
            read_scorecard_writeback_files(value.get("scorecard_writeback_files"))
        if (
            marker not in service_result.TEST_COUNTS_SCHEMA_VERSIONS
            and value.get("passed_tests") is not None
        ):
            raise _error(
                "passed_tests",
                "must be null for a historical validation service result",
            )
        if kind is RunKind.FROZEN_VALIDATE:
            for field in (
                "validation_kind",
                "result_record",
                "measured_against_superseded_tip",
                "current_main_before_launch",
                "target_contains_current_main_before_launch",
                "qualifying_receipt",
            ):
                if field not in value:
                    raise _error(field, "is required for frozen validation handles")
            if value.get("validation_kind") != RunKind.FROZEN_VALIDATE.value:
                raise _error("validation_kind", "must be 'frozen-validate'")
            _required_text(value, "result_record")
            _optional_commit(value, "current_main_before_launch")
            if value.get("current_main_before_launch") is None:
                raise _error("current_main_before_launch", "is required")
            if value.get("measured_against_superseded_tip") is not True:
                raise _error("measured_against_superseded_tip", "must be true")
            if value.get("target_contains_current_main_before_launch") is not False:
                raise _error(
                    "target_contains_current_main_before_launch", "must be false"
                )
            if value.get("qualifying_receipt") is not False:
                raise _error("qualifying_receipt", "must be false")

    for field in ("executed_nodes", "executed_tests", "passed_tests"):
        if value.get(field) is not None:
            _nonnegative_integer(value, field)

    if state is RunState.REFUSED:
        for field in ("result", "detail", "finished_at", "exit_code"):
            if value.get(field) is None:
                raise _error(field, "is required when state is refused")
        if value.get("result") != "systemd-launch-refused":
            raise _error(
                "result", "must be 'systemd-launch-refused' when state is refused"
            )
        _required_text(value, "detail")
        _required_text(value, "finished_at")
        if value.get("exit_code") == 0:
            raise _error("exit_code", "must be nonzero when state is refused")
    elif state is RunState.COMPLETED:
        for field in ("result", "finished_at", "exit_code"):
            if value.get(field) is None:
                raise _error(field, "is required when state is completed")
        _required_text(value, "finished_at")
        if kind is RunKind.BENCH:
            if value.get("result") != "passed":
                raise _error("result", "must be 'passed' when a bench run is completed")
            if value.get("exit_code") != 0:
                raise _error("exit_code", "must be zero when a bench run is completed")
        else:
            outcomes = {
                "PASSED": ("success", 0),
                "FAILED": ("failure", 1),
                "COULD_NOT_RUN": ("no-result", 75),
            }
            status = value.get("final_validate_status")
            if status not in outcomes:
                raise _error(
                    "final_validate_status",
                    "is required and must be a closed value when completed",
                )
            expected_result, expected_exit = outcomes[status]
            if value.get("result") != expected_result:
                raise _error(
                    "result",
                    f"final_validate_status {status} requires {expected_result!r}",
                )
            marker = value.get("service_result_schema")
            if marker in service_result.AUTHORITATIVE_SCHEMA_VERSIONS:
                if "selection_mode" not in value:
                    raise _error(
                        "selection_mode",
                        "is required for a completed current validation result",
                    )
                if "scorecard_writeback" not in value:
                    raise _error(
                        "scorecard_writeback",
                        "is required for a completed current validation result",
                    )
                if "passed_tests" not in value:
                    raise _error(
                        "passed_tests",
                        "is required for a completed current validation result",
                    )
                if marker == service_result.SCHEMA_VERSION:
                    if "detail" not in value:
                        raise _error(
                            "detail",
                            "is required for a completed current validation result",
                        )
                    if status == "COULD_NOT_RUN":
                        _required_text(value, "detail")
                    elif value.get("detail") is not None:
                        raise _error(
                            "detail",
                            f"final_validate_status {status} requires null",
                        )
                writeback = _scorecard_writeback(value.get("scorecard_writeback"))
                service_result.read_test_counts(
                    value,
                    version=marker,
                    status=status,
                )
                expected_exit = service_result.current_command_exit(status, writeback)
            if value.get("exit_code") != expected_exit:
                raise _error(
                    "exit_code",
                    f"final_validate_status {status} with scorecard_writeback "
                    f"{value.get('scorecard_writeback')!r} requires {expected_exit}",
                )
    elif state is RunState.KILLED:
        if value.get("result") != "terminated":
            raise _error("result", "must be 'terminated' when state is killed")
        for field in ("finished_at", "exit_code", "signal"):
            if value.get(field) is None:
                raise _error(field, "is required when state is killed")
        _required_text(value, "finished_at")
        _positive_integer(value, "signal")
        if value.get("exit_code") != 128 + value["signal"]:
            raise _error("exit_code", "must equal 128 + signal when state is killed")
    elif state is RunState.UNKNOWN:
        if value.get("result") != "unknown" or not value.get("detail"):
            raise _error("result", "unknown state requires result='unknown' and detail")
    elif state is RunState.FAILED:
        if value.get("result") != "failed":
            raise _error("result", "must be 'failed' when state is failed")
        for field in ("finished_at", "exit_code"):
            if value.get(field) is None:
                raise _error(field, "is required when state is failed")
        _required_text(value, "finished_at")
        if value.get("exit_code") == 0:
            raise _error("exit_code", "must be nonzero when state is failed")

    return RunHandle(
        kind=kind,
        state=state,
        unit=unit,
        target=target,
        repo=repo,
        branch=branch,
        checkout=checkout,
        log=log,
        agent=agent,
        started_at=started_at,
        producer=producer,
        admission=admission,
        process_identity=identity,
        admission_result=admission_result,
        termination_requests=termination_requests,
        raw=cast(RunRecord, dict(value)),
    )


def record_path(root: Path, unit: str) -> Path:
    return root / "ignored" / "validate" / "runs" / f"{unit.removesuffix('.service')}.json"


def owner_watch_correction_transaction(root: Path) -> Path:
    """Return the durable marker for an unfinished owner-watch correction."""

    return root / "ignored" / "validate" / OWNER_WATCH_CORRECTION_TRANSACTION


class OwnerWatchCorrectionUnavailable(RuntimeError):
    """The run-handle population cannot be read consistently right now."""


def owner_watch_correction_pending(root: Path) -> bool:
    """Treat every directory entry, including a dangling symlink, as pending."""

    try:
        os.lstat(owner_watch_correction_transaction(root))
    except FileNotFoundError:
        return False
    except OSError as error:
        raise OwnerWatchCorrectionUnavailable(
            "cannot inspect owner-watch correction transaction: "
            f"{owner_watch_correction_transaction(root)}: {error}"
        ) from error
    return True


def require_no_owner_watch_correction(root: Path) -> None:
    transaction = owner_watch_correction_transaction(root)
    if owner_watch_correction_pending(root):
        raise OwnerWatchCorrectionUnavailable(
            "owner-watch run-handle correction recovery is pending: " f"{transaction}"
        )


def _state_root_for_run_handle(path: Path) -> Path | None:
    absolute = path.absolute()
    if (
        absolute.parent.name == "runs"
        and absolute.parent.parent.name == "validate"
        and absolute.parent.parent.parent.name == "ignored"
    ):
        return absolute.parents[3]
    return None


def read_record(path: Path) -> dict[str, Any]:
    state_root = _state_root_for_run_handle(path)
    if state_root is not None:
        require_no_owner_watch_correction(state_root)
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"cannot read validation handle {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise RuntimeError(f"validation handle {path} is not an object")
    schema = value.get("schema_version")
    if not is_current_schema_version(schema):
        raise RuntimeError(f"validation handle {path} has an unsupported schema")
    if value.get("producer") == PRODUCER:
        parse_current_record(value)
    if state_root is not None:
        require_no_owner_watch_correction(state_root)
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
def owner_watch_population_lock(owner_state_dir: Path):
    """Serialize owner-watch production with evidence-bound corrections."""

    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(owner_state_dir, flags)
    except OSError as error:
        raise RuntimeError(
            f"cannot open owner-watch population directory {owner_state_dir}: {error}"
        ) from error
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield owner_state_dir
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


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
    if not is_current_schema_version(value.get("schema_version")):
        raise RuntimeError(f"validation handle {path} has an unsupported schema")
    if value.get("producer") == PRODUCER:
        parse_current_record(value)
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


def write_current_record(path: Path, value: Mapping[str, Any]) -> None:
    parse_current_record(value)
    write_record(path, value)


def create_record(path: Path, value: Mapping[str, Any]) -> None:
    """Create a run handle without replacing another run's identity."""
    with exclusive_record(path):
        if path.exists():
            raise RuntimeError(f"validation handle already exists: {path}")
        _write_unlocked(path, value)


def create_current_record(path: Path, value: Mapping[str, Any]) -> None:
    parse_current_record(value)
    create_record(path, value)


def reserve_log(path: Path) -> None:
    """Reserve a systemd append target; a second run must not share it."""
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError as error:
        raise RuntimeError(f"validation log already exists: {path}") from error
    os.close(descriptor)


def update_record(
    path: Path, *, blocking: bool = True, **fields: Any
) -> dict[str, Any]:
    # The caller and the observer finish at nearly the same time. Serialize the
    # read-modify-write so a final exit update cannot erase cgroup evidence (or
    # vice versa) while retaining atomic replacement for readers.
    with exclusive_record(path, blocking=blocking):
        value = read_record(path)
        value.update(fields)
        _write_unlocked(path, value)
    return value


def record_termination_request(
    path: Path,
    *,
    agent: str,
    pid: int,
    reason: str,
    requested_at: str | None = None,
) -> TerminationRequestRecord:
    """Record who is about to stop a validation before the stop is attempted."""

    encoded: TerminationRequestRecord = {
        "requested_at": requested_at or datetime.now(timezone.utc).isoformat(),
        "agent": agent,
        "pid": pid,
        "reason": reason,
    }
    with exclusive_record(path):
        value = read_record(path)
        handle = parse_current_record(value)
        if not handle.counts_as_validation:
            raise _error("termination_requests", "requires a validation handle")
        if not handle.lock_admissible:
            raise _error(
                "termination_requests",
                f"cannot request termination while state is {handle.state.value}",
            )
        if handle.process_identity is None:
            raise _error(
                "termination_requests",
                "cannot request termination before the validation process identity is recorded",
            )
        requests = list(value.get("termination_requests", []))
        requests.append(encoded)
        value["termination_requests"] = requests
        _write_unlocked(path, value)
    return encoded


def confirm_termination_request(
    path: Path,
    request: Mapping[str, Any],
    *,
    confirmed_at: str | None = None,
) -> TerminationRequestRecord:
    """Confirm that the exact recorded stop request left its unit inactive."""

    with exclusive_record(path):
        value = read_record(path)
        requests = value.get("termination_requests")
        if not isinstance(requests, list):
            raise _error("termination_requests", "recorded request is missing")
        match = None
        for item in requests:
            if isinstance(item, dict) and all(
                item.get(field) == request.get(field)
                for field in ("requested_at", "agent", "pid", "reason")
            ):
                match = item
                break
        if match is None:
            raise _error(
                "termination_requests", "recorded request changed before confirmation"
            )
        if "confirmed_at" not in match:
            match["confirmed_at"] = (
                confirmed_at or datetime.now(timezone.utc).isoformat()
            )
        _write_unlocked(path, value)
        return cast(TerminationRequestRecord, dict(match))


def update_from_observer(
    path: Path, *, observation_detail: str | None = None, **fields: Any
) -> dict[str, Any]:
    """Publish observer evidence without overwriting a producer terminal state.

    The producer can publish a refusal, cancellation, or completed result after
    the observer reads its snapshot. Preserve that evidenced terminal cause and
    finish time under the same lock as the update. Unknown is not terminal and
    can still be repaired by a later observation with evidence. Observed cgroups
    accumulate on every row. Observation uncertainty uses the existing detail
    field only when no producer detail is present, without changing the outcome.
    """
    with exclusive_record(path):
        value = read_record(path)
        has_groups = "observed_safe_ci_cgroups" in fields
        groups = fields.pop("observed_safe_ci_cgroups", ())
        if service_result.is_evidenced_terminal(value):
            fields = {}
        elif observation_detail and not value.get("detail"):
            fields["detail"] = observation_detail
        if has_groups:
            fields["observed_safe_ci_cgroups"] = sorted(
                set(value.get("observed_safe_ci_cgroups", [])) | set(groups)
            )
        if fields:
            value.update(fields)
            _write_unlocked(path, value)
    return value


def bind_process_identity(path: Path, identity: ProcessIdentity) -> dict[str, Any]:
    """Publish one process identity without permitting a different replacement."""
    with exclusive_record(path):
        value = read_record(path)
        encoded = {
            "pid": identity.pid,
            "start_ticks": identity.start_ticks,
            "boot_id": identity.boot_id,
        }
        existing = value.get("process_identity")
        if existing is not None and existing != encoded:
            raise _error(
                "process_identity",
                "record already carries a different process identity",
            )
        value["process_identity"] = encoded
        _write_unlocked(path, value)
    return value


def publish_admission_result(
    path: Path,
    *,
    state: AdmissionState,
    reason: AdmissionRefusalReason | None = None,
    exit_code: int | None = None,
) -> dict[str, Any]:
    """Publish the one framework-owned admission result without replacement."""
    if state is AdmissionState.ADMITTED:
        if reason is not None or exit_code is not None:
            raise _error(
                "admission_result", "admitted does not carry reason or exit_code"
            )
        handle = parse_current_record(read_record(path))
        if handle.counts_as_validation:
            return _publish_numbered_admission_result(path)
    elif reason is None or exit_code is None:
        raise _error("admission_result", "refused requires reason and exit_code")
    with exclusive_record(path):
        value = read_record(path)
        if value.get("producer") != PRODUCER:
            raise _error("producer", "admission result requires a current handle")
        handle = parse_current_record(value)
        existing = handle.admission_result
        if existing is not None:
            if (
                existing.state is not state
                or existing.reason is not reason
                or existing.exit_code != exit_code
            ):
                raise _error(
                    "admission_result",
                    "record already carries a different admission result",
                )
            return value
        encoded: AdmissionResultRecord = {
            "state": state.value,
            "recorded_at": datetime.now(timezone.utc).isoformat(),
        }
        if state is AdmissionState.REFUSED:
            assert reason is not None and exit_code is not None
            encoded.update(reason=reason.value, exit_code=exit_code)
        value["admission_result"] = encoded
        _write_unlocked(path, value)
    return value


def _legacy_projection(value: Mapping[str, Any]) -> dict[str, Any]:
    """Project only the schema-1 fields existing readers historically required."""
    unit = value.get("unit")
    if isinstance(unit, str) and unit == "hermit-pressure-test.service":
        kind = RunKind.BENCH.value
    elif isinstance(unit, str) and unit.startswith("validate-"):
        kind = RunKind.VALIDATE.value
    else:
        kind = "legacy"
    state = value.get("state")
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": kind,
        "state": state,
        "unit": unit,
        "target": value.get("target"),
        "repo": value.get("repo"),
        "branch": value.get("branch"),
        "agent": value.get("agent"),
        "started_at": value.get("started_at"),
        "host": value.get("host"),
        "process_identity": value.get("process_identity"),
        "admission_result": None,
        "result": value.get("result"),
        "detail": value.get("detail"),
        "exit_code": value.get("exit_code"),
        "finished_at": value.get("finished_at"),
        "termination_requests": value.get("termination_requests", []),
        "lock_admissible": state in {"launching", "running"},
        "counts_as_validation": kind != RunKind.BENCH.value,
    }


def projection(value: Mapping[str, Any]) -> dict[str, Any]:
    if not is_current_schema_version(value.get("schema_version")):
        raise _error(
            "schema_version", f"unsupported value {value.get('schema_version')!r}"
        )
    if value.get("producer") != PRODUCER:
        return _legacy_projection(value)
    handle = parse_current_record(value)
    identity = handle.process_identity
    admission_result = handle.admission_result
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": handle.kind.value,
        "state": handle.state.value,
        "unit": handle.unit,
        "target": handle.target,
        "repo": handle.repo,
        "branch": handle.branch,
        "agent": handle.agent,
        "started_at": handle.started_at,
        "host": handle.raw.get("host"),
        "process_identity": (
            {
                "pid": identity.pid,
                "start_ticks": identity.start_ticks,
                "boot_id": identity.boot_id,
            }
            if identity is not None
            else None
        ),
        "admission_result": (
            {
                "state": admission_result.state.value,
                "recorded_at": admission_result.recorded_at,
                **(
                    {
                        "run_number": admission_result.run_number,
                    }
                    if admission_result.state is AdmissionState.ADMITTED
                    and admission_result.run_number is not None
                    else {}
                ),
                **(
                    {
                        "reason": admission_result.reason.value,
                        "exit_code": admission_result.exit_code,
                    }
                    if admission_result.state is AdmissionState.REFUSED
                    and admission_result.reason is not None
                    else {}
                ),
            }
            if admission_result is not None
            else None
        ),
        "result": handle.raw.get("result"),
        "detail": handle.raw.get("detail"),
        "exit_code": handle.raw.get("exit_code"),
        "finished_at": handle.raw.get("finished_at"),
        "termination_requests": [
            {
                "requested_at": request.requested_at,
                "agent": request.agent,
                "pid": request.pid,
                "reason": request.reason,
                **(
                    {"confirmed_at": request.confirmed_at}
                    if request.confirmed_at is not None
                    else {}
                ),
            }
            for request in handle.termination_requests
        ],
        "lock_admissible": handle.lock_admissible,
        "counts_as_validation": handle.counts_as_validation,
    }


def inspect_current_records(paths: list[Path]) -> dict[str, list[dict[str, Any]]]:
    """Validate current handles independently with one authority process.

    A malformed handle is unavailable evidence about that handle. It must stay
    visible without hiding every valid handle in the same directory. Conditions
    that make the shared authority unavailable, such as an unfinished
    owner-watch correction, still refuse the whole request.
    """
    state_roots = {
        root for path in paths if (root := _state_root_for_run_handle(path)) is not None
    }
    for root in state_roots:
        require_no_owner_watch_correction(root)

    inspected: list[dict[str, Any]] = []
    unreadable: list[dict[str, Any]] = []
    for path in paths:
        try:
            value = read_record(path)
            if value.get("producer") != PRODUCER:
                raise _error(
                    "producer", f"current record {path} has the wrong producer"
                )
            inspected.append({"path": str(path), "record": projection(value)})
        except OwnerWatchCorrectionUnavailable:
            raise
        except RuntimeError as error:
            unreadable.append({"path": str(path), "error": str(error)})

    # A correction may begin while the files are being inspected. Preserve the
    # population-wide refusal instead of misreporting that shared state change
    # as an isolated malformed record.
    for root in state_roots:
        require_no_owner_watch_correction(root)
    return {"records": inspected, "unreadable": unreadable}


def verify_attribution(
    value: Mapping[str, Any], *, agent: str, target: str, kind: str
) -> dict[str, Any]:
    projected = projection(value)
    if (
        projected["agent"] != agent
        or projected["target"] != target
        or projected["kind"] != kind
        or projected["lock_admissible"] is not True
    ):
        raise _error(
            "attribution",
            f"does not bind kind={kind} state=launching|running agent={agent} target={target}",
        )
    return projected


def verify_holder(
    value: Mapping[str, Any],
    *,
    agent: str,
    target: str,
    kind: str,
    unit: str,
    identity: ProcessIdentity,
) -> dict[str, Any]:
    projected = verify_attribution(value, agent=agent, target=target, kind=kind)
    if projected["unit"] != unit:
        raise _error("unit", f"does not bind expected unit {unit!r}")
    observed = projected["process_identity"]
    expected = {
        "pid": identity.pid,
        "start_ticks": identity.start_ticks,
        "boot_id": identity.boot_id,
    }
    if observed != expected:
        raise _error("process_identity", "does not bind the held anchor identity")
    return projected


def _run_number_team(repo: str) -> str:
    if repo == "rrnewton/hermit":
        return "hermit"
    if repo == "rrnewton/reverie":
        return "reverie"
    raise _error("repo", f"cannot assign a run number for {repo!r}")


def _run_number_state(path: Path, *, repo: str, host: str) -> int:
    if not path.exists():
        return 0
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise _error("run_number", f"cannot read {path}: {error}") from error
    expected = {"schema_version", "repo", "host", "last_run_number"}
    if not isinstance(value, dict) or set(value) != expected:
        raise _error(
            "run_number", f"{path} does not contain exactly {sorted(expected)}"
        )
    if value.get("schema_version") != 1:
        raise _error("run_number", f"{path} has unsupported schema")
    if value.get("repo") != repo or value.get("host") != host:
        raise _error("run_number", f"{path} identity does not match {repo} on {host}")
    _positive_integer(value, "last_run_number")
    return int(value["last_run_number"])


def _write_run_number_state(path: Path, *, repo: str, host: str, number: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(
                {
                    "schema_version": 1,
                    "repo": repo,
                    "host": host,
                    "last_run_number": number,
                },
                stream,
                indent=2,
                sort_keys=True,
            )
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def _run_number_source_paths(root: Path, *, team: str, host: str) -> list[Path]:
    published = root.glob(f"ledger/{team}/{host}/[0-9][0-9][0-9][0-9]-[0-9][0-9].jsonl")
    recovered = root.glob(
        f"ledger-recovery/{team}/{host}/[0-9][0-9][0-9][0-9]-[0-9][0-9]/*.jsonl"
    )
    spool = root / "ignored/ci-hub/validate-ledger-spool"
    pending = spool.glob(f"{team}__{host}__*.jsonl")
    retained = (spool / "published").glob(f"{team}__{host}__*.jsonl")
    return sorted([*published, *recovered, *pending, *retained])


def _run_number_from_value(value: object) -> int | None:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        return None
    return value


def _ledger_run_numbers_without_canonical_view(
    root: Path, *, team: str, host: str
) -> tuple[int, int]:
    """Find a non-reusing baseline even when the canonical view refuses.

    The admission gate must not deadlock every future validate because one old
    ledger line is unreadable. A nonempty unreadable line still consumes one
    position: that can leave a gap, but it cannot reuse an identity already
    issued to an earlier run.
    """
    identities: set[tuple[str, object]] = set()
    explicit: list[int] = []
    for path in _run_number_source_paths(root, team=team, host=host):
        try:
            lines = path.read_text().splitlines()
        except OSError as error:
            raise _error(
                "admission_result.run_number",
                f"cannot read ledger shard {path}: {error}",
            ) from error
        for line_number, line in enumerate(lines, 1):
            if not line.strip():
                continue
            physical_identity = ("line", f"{path}:{line_number}")
            try:
                value = json.loads(line)
            except (json.JSONDecodeError, ValueError):
                identities.add(physical_identity)
                continue
            if not isinstance(value, dict):
                identities.add(physical_identity)
                continue

            event_type = value.get("event_type")
            if event_type is not None:
                if event_type != "run.result":
                    continue
                event_team = value.get("team")
                event_host = str(value.get("host") or "").split(".", 1)[0]
                if event_team != team or event_host != host:
                    # The shard path already binds this line to the selected
                    # team and machine. If its body disagrees, the canonical
                    # reader quite properly refuses it, but ignoring the line
                    # here could reuse a number already consumed by that run.
                    identities.add(physical_identity)
                    continue
                legacy = value.get("legacy_row")
                if not isinstance(legacy, dict):
                    legacy = {}
                producer = value.get("producer")
                producer_tool = (
                    producer.get("tool") if isinstance(producer, dict) else producer
                )
                legacy_producer = legacy.get("producer")
                if (
                    producer_tool == "ledger-live-sync-acceptance"
                    or legacy_producer == "ledger-live-sync-acceptance"
                ):
                    continue
                run_id = value.get("run_id")
                event_id = value.get("event_id")
                identity = (
                    ("run", run_id)
                    if isinstance(run_id, str) and run_id
                    else (
                        ("event", event_id)
                        if isinstance(event_id, str) and event_id
                        else physical_identity
                    )
                )
                number = _run_number_from_value(value.get("run_number"))
                if number is None:
                    number = _run_number_from_value(legacy.get("run_number"))
            else:
                row_host = str(value.get("host") or "").split(".", 1)[0]
                repo = value.get("repo")
                repo_matches = (
                    repo in (None, "hermit", "rrnewton/hermit")
                    if team == "hermit"
                    else repo in ("reverie", "rrnewton/reverie")
                )
                if row_host != host or not repo_matches:
                    # As above, a malformed row in this machine's shard still
                    # consumes a conservative position. A gap is safer than
                    # issuing the same run number twice.
                    identities.add(physical_identity)
                    continue
                if value.get("producer") == "ledger-live-sync-acceptance":
                    continue
                run_id = value.get("run_id")
                record_id = value.get("record_id")
                identity = (
                    ("run", run_id)
                    if isinstance(run_id, str) and run_id
                    else (
                        ("record", record_id)
                        if isinstance(record_id, str) and record_id
                        else physical_identity
                    )
                )
                number = _run_number_from_value(value.get("run_number"))
            identities.add(identity)
            if number is not None:
                explicit.append(number)
    return len(identities), max(explicit, default=0)


def _ledger_run_numbers(root: Path, *, team: str, host: str) -> tuple[int, int]:
    def count_directly(error: Exception) -> tuple[int, int]:
        print(
            "run_registry: canonical ledger view could not supply the run "
            f"number count ({error}); counting result entries directly",
            file=sys.stderr,
        )
        return _ledger_run_numbers_without_canonical_view(root, team=team, host=host)

    ledger_dir = Path(__file__).resolve().parents[1] / "ledger"
    sys.path.insert(0, str(ledger_dir))
    try:
        try:
            import ledger as ledger_store
            import publisher as ledger_publisher
            import validate_rows
        except ModuleNotFoundError as error:
            return count_directly(error)
        try:
            relevant = [
                row
                for row in validate_rows.rows(root=root)
                if str(row.get("host") or "").split(".", 1)[0] == host
                and (
                    row.get("repo") in (None, "hermit", "rrnewton/hermit")
                    if team == "hermit"
                    else row.get("repo") in ("reverie", "rrnewton/reverie")
                )
            ]
        except (
            OSError,
            RuntimeError,
            ValueError,
            ledger_store.LedgerError,
            ledger_publisher.PublishRefused,
        ) as error:
            return count_directly(error)
    finally:
        sys.path.remove(str(ledger_dir))
    identities = {
        (
            ("run", str(row["run_id"]))
            if isinstance(row.get("run_id"), str) and row.get("run_id")
            else ("row", index)
        )
        for index, row in enumerate(relevant)
    }
    explicit = [
        number
        for row in relevant
        for number in (row.get("run_number"),)
        if isinstance(number, int) and not isinstance(number, bool) and number > 0
    ]
    return len(identities), max(explicit, default=0)


def _run_number_root(path: Path) -> tuple[Path, Path]:
    path = path.resolve()
    try:
        root = path.parents[3]
    except IndexError as error:
        raise _error("run_number", f"record path is too short: {path}") from error
    expected_directory = (root / "ignored/validate/runs").resolve()
    if path.parent != expected_directory:
        raise _error("run_number", f"record {path} is outside {expected_directory}")
    return root, path


def _publish_numbered_admission_result(path: Path) -> dict[str, Any]:
    """Publish one admitted validation result with its machine run number."""
    root, path = _run_number_root(path)
    initial = parse_current_record(read_record(path))
    if not initial.counts_as_validation:
        raise _error("admission_result.run_number", "is valid only for validation")
    repo = initial.repo
    team = _run_number_team(repo)
    host = socket.gethostname().split(".", 1)[0]
    if not host:
        raise _error("admission_result.run_number", "machine hostname is empty")
    recorded_host = initial.raw.get("host")
    if recorded_host is not None and str(recorded_host).split(".", 1)[0] != host:
        raise _error(
            "admission_result.run_number",
            f"record host {recorded_host!r} differs from machine {host!r}",
        )
    state_path = root / "ignored/validate/run-numbers" / team / f"{host}.json"
    with exclusive_record(state_path):
        with exclusive_record(path):
            value = read_record(path)
            handle = parse_current_record(value)
            if not handle.counts_as_validation or handle.repo != repo:
                raise _error(
                    "admission_result.run_number",
                    "run identity changed while admission was being recorded",
                )
            existing = handle.admission_result
            if existing is not None:
                if existing.state is not AdmissionState.ADMITTED:
                    raise _error(
                        "admission_result",
                        "record already carries a different admission result",
                    )
                number = existing.run_number
                if number is None:
                    # The counter is display metadata, not admission identity.
                    # Preserve an already-admitted historical record exactly;
                    # idempotent publication must not mutate it into a newer
                    # shape or make its real admission unusable.
                    return value
                previous = _run_number_state(state_path, repo=repo, host=host)
                if number > previous:
                    _write_run_number_state(
                        state_path, repo=repo, host=host, number=number
                    )
                return value

            previous = _run_number_state(state_path, repo=repo, host=host)
            if previous > 0:
                number = previous + 1
            else:
                ledger_count, ledger_max = _ledger_run_numbers(
                    root, team=team, host=host
                )
                number = max(ledger_count, ledger_max) + 1
            # Persist the counter first. If this process dies before the handle
            # is written, the number is skipped rather than issued to two runs.
            _write_run_number_state(state_path, repo=repo, host=host, number=number)
            value["host"] = host
            value["admission_result"] = {
                "state": AdmissionState.ADMITTED.value,
                "recorded_at": datetime.now(timezone.utc).isoformat(),
                "run_number": number,
            }
            _write_unlocked(path, value)
            return value


def _load_cli_record(path: Path) -> dict[str, Any]:
    return read_record(path)


def _cli(argv: list[str] | None = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    inspect = commands.add_parser("inspect")
    inspect.add_argument("--path", type=Path, required=True)
    inspect_records = commands.add_parser("inspect-current-records")
    inspect_records.add_argument("--path", type=Path, action="append", required=True)
    attribution = commands.add_parser("verify-attribution")
    attribution.add_argument("--path", type=Path, required=True)
    attribution.add_argument("--agent", required=True)
    attribution.add_argument("--target", required=True)
    attribution.add_argument("--kind", required=True)
    holder = commands.add_parser("verify-holder")
    holder.add_argument("--path", type=Path, required=True)
    holder.add_argument("--agent", required=True)
    holder.add_argument("--target", required=True)
    holder.add_argument("--kind", required=True)
    holder.add_argument("--unit", required=True)
    holder.add_argument("--pid", type=int, required=True)
    holder.add_argument("--start-ticks", type=int, required=True)
    holder.add_argument("--boot-id", required=True)
    bind = commands.add_parser("bind-process-identity")
    bind.add_argument("--path", type=Path, required=True)
    bind.add_argument("--pid", type=int, required=True)
    bind.add_argument("--start-ticks", type=int, required=True)
    bind.add_argument("--boot-id", required=True)
    publish_admission = commands.add_parser("publish-admission-result")
    publish_admission.add_argument("--path", type=Path, required=True)
    publish_admission.add_argument(
        "--state", choices=[state.value for state in AdmissionState], required=True
    )
    publish_admission.add_argument(
        "--reason", choices=[reason.value for reason in AdmissionRefusalReason]
    )
    publish_admission.add_argument("--exit-code", type=int)
    args = parser.parse_args(argv)
    try:
        if args.command == "inspect-current-records":
            report = inspect_current_records(args.path)
        elif args.command == "bind-process-identity":
            value = bind_process_identity(
                args.path,
                ProcessIdentity(
                    pid=args.pid, start_ticks=args.start_ticks, boot_id=args.boot_id
                ),
            )
            report = projection(value)
        elif args.command == "publish-admission-result":
            value = publish_admission_result(
                args.path,
                state=AdmissionState(args.state),
                reason=(
                    AdmissionRefusalReason(args.reason)
                    if args.reason is not None
                    else None
                ),
                exit_code=args.exit_code,
            )
            report = projection(value)
        else:
            value = _load_cli_record(args.path)
            if args.command == "inspect":
                report = projection(value)
            elif args.command == "verify-attribution":
                report = verify_attribution(
                    value, agent=args.agent, target=args.target, kind=args.kind
                )
            else:
                report = verify_holder(
                    value,
                    agent=args.agent,
                    target=args.target,
                    kind=args.kind,
                    unit=args.unit,
                    identity=ProcessIdentity(
                        pid=args.pid,
                        start_ticks=args.start_ticks,
                        boot_id=args.boot_id,
                    ),
                )
    except RuntimeError as error:
        print(f"run_registry: REFUSED: {error}", file=sys.stderr)
        return 2
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(_cli())
