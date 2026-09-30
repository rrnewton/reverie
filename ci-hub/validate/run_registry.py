#!/usr/bin/env python3
"""Durable handles for ci-hub-owned validation services and observer panes."""

from __future__ import annotations

import ctypes
import errno
import fcntl
import json
import os
import re
import secrets
import socket
import stat
import sys
import tempfile
import threading
from contextlib import contextmanager, suppress
from dataclasses import dataclass
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Any, Callable, Mapping, TypedDict, cast

import service_result
import reverie_node


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


class RuntimeRootIdentityRecord(TypedDict):
    device: int
    inode: int
    mode: int
    uid: int
    gid: int


class CleanupRemovalIntentRecord(TypedDict, total=False):
    recorded_at: str
    path: str
    identity: RuntimeRootIdentityRecord
    wrkslots_slot: str
    wrkslots_generation: int


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


class LedgerMirrorRecord(TypedDict, total=False):
    state: str
    recorded_at: str
    commit: str
    source_commit: str
    source_tree: str
    rows: int
    detail: str
    repository: str
    source_repository: str
    series_commit: str
    raw_publication: dict[str, Any]
    scorecard: dict[str, Any]


class CpuPlacementRecord(TypedDict):
    source: str
    excluded_cpus: str
    allowed_cpus: str
    online_cpus: str
    pmus: list[str]
    skipped_pmus: list[str]
    detail: str
    override_env: str


class GitSubstitutionRecord(TypedDict):
    applied: bool
    code: str
    reason: str
    resolved_git: str | None
    resolved_git_realpath: str | None
    real_git: str
    effective_git: str | None
    effective_version: str | None
    wrapper_config: str | None
    wrapper_config_git: str | None
    directory: str | None


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
    runtime_root: str
    runtime_root_identity: RuntimeRootIdentityRecord
    log: str
    agent: str
    pr: int | None
    started_at: str
    parent_checkout_head: str | None
    github_executable: str
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
    reverie_count_request: dict[str, Any]
    reverie_count_evidence: dict[str, Any]
    reverie_count_refusal: str | None
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
    ledger_mirror: LedgerMirrorRecord
    cpu_placement: CpuPlacementRecord
    git_substitution: GitSubstitutionRecord
    termination_requests: list[TerminationRequestRecord]
    measured_result: str
    checkout_removed_at: str
    checkout_removal_intent: CleanupRemovalIntentRecord
    wrkslots_slot: str
    wrkslots_generation: int
    archived_orphaned_receipts: list[str]
    cargo_home_removed_at: str
    cargo_home_removal_intent: CleanupRemovalIntentRecord
    runtime_root_removed_at: str
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
        "runtime_root",
        "runtime_root_identity",
        "pr",
        "parent_checkout_head",
        "github_executable",
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
        "reverie_count_request",
        "reverie_count_evidence",
        "reverie_count_refusal",
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
        "ledger_mirror",
        "measured_result",
        "checkout_removed_at",
        "checkout_removal_intent",
        "wrkslots_slot",
        "wrkslots_generation",
        "archived_orphaned_receipts",
        "cargo_home_removed_at",
        "cargo_home_removal_intent",
        "runtime_root_removed_at",
        "host",
        "termination_requests",
        "cpu_placement",
        "git_substitution",
    }
)

BENCH_FIELDS = COMMON_FIELDS | frozenset(
    {"results", "cleanup_error", "cpu_placement", "git_substitution"}
)

# Written by ci-hub/validate/cpu_placement.py (CpuPlacement.record). Checked
# here structurally rather than by importing that module: several exact-tool
# authority fixtures copy this file with a fixed dependency set.
# tests/test_cpu_placement.py pins this set to cpu_placement.RECORD_FIELDS.
CPU_PLACEMENT_FIELDS = frozenset(
    {
        "source",
        "excluded_cpus",
        "allowed_cpus",
        "online_cpus",
        "pmus",
        "skipped_pmus",
        "detail",
        "override_env",
    }
)
# Pinned to cpu_placement.SOURCES and cpu_placement.MAX_CPU_NUMBER by the same
# test, for the same reason.
CPU_PLACEMENT_SOURCES = frozenset(
    {
        "sysfs-pmu-cpumask",
        "sysfs-none",
        "sysfs-too-broad",
        "sysfs-malformed",
        "override",
        "override-none",
    }
)
CPU_PLACEMENT_MAX_CPU = 8191
_CPU_LIST_RE = re.compile(r"\A(?:(?:\d+(?:-\d+)?)(?:,\d+(?:-\d+)?)*)?\Z")


def _cpu_list_set(field: str, text: str) -> frozenset[int]:
    """Expand one recorded kernel CPU list, refusing any CPU above the ceiling
    before a range is expanded."""
    if _CPU_LIST_RE.fullmatch(text) is None:
        raise _error(f"cpu_placement.{field}", "must be a kernel CPU list")
    cpus: set[int] = set()
    for part in filter(None, text.split(",")):
        low_text, _, high_text = part.partition("-")
        high_text = high_text or low_text
        for number in (low_text, high_text):
            digits = number.lstrip("0") or "0"
            if (
                len(digits) > len(str(CPU_PLACEMENT_MAX_CPU))
                or int(digits) > CPU_PLACEMENT_MAX_CPU
            ):
                raise _error(
                    f"cpu_placement.{field}",
                    f"names CPU {number[:20]}, above {CPU_PLACEMENT_MAX_CPU}",
                )
        low, high = int(low_text), int(high_text)
        if high < low:
            raise _error(f"cpu_placement.{field}", "must be a kernel CPU list")
        cpus.update(range(low, high + 1))
    return frozenset(cpus)


# Written by ci-hub/validate/unwrapped_git.py (GitSubstitution.record): which
# git a run's PATH reaches and why. Checked structurally here for the same
# fixed-dependency reason as cpu_placement; tests/test_unwrapped_git.py pins
# these sets to unwrapped_git's record fields and CODES.
GIT_SUBSTITUTION_FIELDS = frozenset(
    {
        "applied",
        "code",
        "reason",
        "resolved_git",
        "resolved_git_realpath",
        "real_git",
        "effective_git",
        "effective_version",
        "wrapper_config",
        "wrapper_config_git",
        "directory",
    }
)
GIT_SUBSTITUTION_CODES = frozenset(
    {
        "applied",
        "no-git-on-path",
        "relative-path-entry",
        "unusable-directory",
        "real-git-unavailable",
        "already-unwrapped",
        "not-the-wrapper",
        "wrapper-config-unreadable",
        "wrapper-config-mismatch",
        "version-probe-failed",
        "version-mismatch",
        "materialize-failed",
    }
)


_OPTIONAL_TEXT_FIELDS = frozenset(
    {
        "source_checkout",
        "branch",
        "cargo_home",
        "runtime_root",
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
        "runtime_root_removed_at",
        "host",
        "results",
        "cleanup_error",
        "reverie_count_refusal",
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


def _cpu_placement(value: Any) -> None:
    if not isinstance(value, dict):
        raise _error("cpu_placement", "must be an object")
    if set(value) != CPU_PLACEMENT_FIELDS:
        raise _error(
            "cpu_placement",
            f"must contain exactly {', '.join(sorted(CPU_PLACEMENT_FIELDS))}",
        )
    for field in ("source", "detail", "override_env"):
        if not isinstance(value.get(field), str) or not value[field]:
            raise _error(f"cpu_placement.{field}", "must be a non-empty string")
    if value["source"] not in CPU_PLACEMENT_SOURCES:
        raise _error(
            "cpu_placement.source",
            f"{value['source']!r} is not one of {', '.join(sorted(CPU_PLACEMENT_SOURCES))}",
        )
    lists: dict[str, frozenset[int]] = {}
    for field in ("excluded_cpus", "allowed_cpus", "online_cpus"):
        text = value.get(field)
        if not isinstance(text, str):
            raise _error(f"cpu_placement.{field}", "must be a kernel CPU list")
        lists[field] = _cpu_list_set(field, text)
    for field in ("pmus", "skipped_pmus"):
        items = value.get(field)
        if not isinstance(items, list) or not all(isinstance(i, str) for i in items):
            raise _error(f"cpu_placement.{field}", "must be a list of strings")
    excluded = lists["excluded_cpus"]
    allowed = lists["allowed_cpus"]
    online = lists["online_cpus"]
    if bool(excluded) != bool(allowed):
        raise _error(
            "cpu_placement",
            "allowed_cpus must be empty exactly when excluded_cpus is empty",
        )
    if excluded and not excluded <= online:
        raise _error("cpu_placement", "excluded_cpus must be a subset of online_cpus")
    if excluded and allowed != online - excluded:
        raise _error(
            "cpu_placement", "allowed_cpus must equal online_cpus minus excluded_cpus"
        )


def _git_substitution(value: Any) -> None:
    if not isinstance(value, dict):
        raise _error("git_substitution", "must be an object")
    if set(value) != GIT_SUBSTITUTION_FIELDS:
        raise _error(
            "git_substitution",
            f"must contain exactly {', '.join(sorted(GIT_SUBSTITUTION_FIELDS))}",
        )
    applied = value["applied"]
    if not isinstance(applied, bool):
        raise _error("git_substitution.applied", "must be a boolean")
    code = value["code"]
    if not isinstance(code, str) or code not in GIT_SUBSTITUTION_CODES:
        raise _error(
            "git_substitution.code",
            f"{code!r} is not one of {', '.join(sorted(GIT_SUBSTITUTION_CODES))}",
        )
    for field in ("reason", "real_git"):
        if not isinstance(value[field], str) or not value[field]:
            raise _error(f"git_substitution.{field}", "must be a non-empty string")
    for field in (
        "resolved_git",
        "resolved_git_realpath",
        "effective_git",
        "effective_version",
        "wrapper_config",
        "wrapper_config_git",
        "directory",
    ):
        item = value[field]
        if item is not None and (not isinstance(item, str) or not item):
            raise _error(
                f"git_substitution.{field}", "must be null or a non-empty string"
            )
    if applied != (code == "applied"):
        raise _error(
            "git_substitution", "applied must be true exactly when code is 'applied'"
        )
    if applied:
        directory = value["directory"]
        if directory is None or not Path(directory).is_absolute():
            raise _error(
                "git_substitution.directory",
                "an applied substitution must name its absolute directory",
            )
        if value["resolved_git"] is None or value["effective_version"] is None:
            raise _error(
                "git_substitution",
                "an applied substitution must name PATH's git and the version",
            )
        if value["effective_git"] != value["real_git"]:
            raise _error(
                "git_substitution.effective_git",
                "an applied substitution runs real_git",
            )
        for field in ("wrapper_config", "wrapper_config_git"):
            item = value[field]
            if item is None or not Path(item).is_absolute():
                raise _error(
                    f"git_substitution.{field}",
                    "an applied substitution must name the wrapper configuration "
                    "and the absolute git path it names",
                )
    else:
        if value["directory"] is not None:
            raise _error(
                "git_substitution.directory",
                "must be null when the substitution was not applied",
            )
        if value["effective_git"] != value["resolved_git"]:
            raise _error(
                "git_substitution.effective_git",
                "a substitution that was not applied runs PATH's git",
            )


def _ledger_mirror(value: Any) -> None:
    if not isinstance(value, dict):
        raise _error("ledger_mirror", "must be an object")
    if "repository" in value:
        _cell_ledger_publication(value)
        return
    state = value.get("state")
    if state in {"published", "unchanged"}:
        required = {
            "state",
            "recorded_at",
            "commit",
            "source_commit",
            "source_tree",
            "rows",
        }
    elif state == "failed":
        required = {"state", "recorded_at", "detail"}
        if "raw_publication" in value:
            # The refused inputs publish even when the series rows fail.
            required.add("raw_publication")
            check_failed_raw_publication(value["raw_publication"])
    else:
        raise _error("ledger_mirror.state", f"unsupported value {state!r}")
    if set(value) != required:
        raise _error(
            "ledger_mirror",
            f"{state} must contain exactly {', '.join(sorted(required))}",
        )
    _timestamp(value, "recorded_at", error_field="ledger_mirror.recorded_at")
    if state == "failed":
        _required_text(value, "detail")
        return
    for field in ("commit", "source_commit", "source_tree"):
        sha = _required_text(value, field)
        if len(sha) != 40 or any(ch not in "0123456789abcdef" for ch in sha):
            raise _error(f"ledger_mirror.{field}", "must be lowercase forty-hex")
    rows = value.get("rows")
    if not isinstance(rows, int) or isinstance(rows, bool) or rows < 0:
        raise _error("ledger_mirror.rows", "must be a nonnegative integer")


def _count(value: Any, field: str, *, nullable: bool) -> None:
    if value is None and nullable:
        return
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise _error(field, "must be a nonnegative integer" + (" or null" if nullable else ""))


def check_failed_raw_publication(value: Any) -> None:
    """A failed publication may keep only the quarantine outcome, fully typed.

    Public so that start_unit can check a mirror's failure report before it
    attaches the count: a count that does not fit must not stop the failure
    itself from being recorded.
    """
    field = "ledger_mirror.raw_publication"
    if not isinstance(value, dict) or set(value) != {"quarantine"}:
        raise _error(field, "a failed publication may carry only its quarantine outcome")
    quarantine = value["quarantine"]
    field += ".quarantine"
    keys = {"schema", "published", "already_published", "commit", "error", "pending_records",
            "held_records", "corrupt_files", "set_aside", "run"}
    if not isinstance(quarantine, dict) or set(quarantine) != keys:
        raise _error(field, "must contain exactly " + ", ".join(sorted(keys)))
    if quarantine["schema"] != "series-quarantine-publication/v1":
        raise _error(f"{field}.schema", "must be series-quarantine-publication/v1")
    for name in ("published", "already_published"):
        _count(quarantine[name], f"{field}.{name}", nullable=False)
    for name in ("pending_records", "held_records", "corrupt_files"):
        _count(quarantine[name], f"{field}.{name}", nullable=True)
    if quarantine["commit"] is not None and not _is_commit(quarantine["commit"]):
        raise _error(f"{field}.commit", "must be null or lowercase forty-hex")
    if quarantine["error"] is not None and (not isinstance(quarantine["error"], str)
                                            or not quarantine["error"].strip()):
        raise _error(f"{field}.error", "must be null or a nonempty string")
    if (not isinstance(quarantine["set_aside"], list)
            or any(not isinstance(item, str) for item in quarantine["set_aside"])):
        raise _error(f"{field}.set_aside", "must be a list of file names")
    run = quarantine["run"]
    if run is None:
        return
    run_keys = {"run_id", "published", "withheld", "unpublished", "records"}
    if not isinstance(run, dict) or set(run) != run_keys:
        raise _error(f"{field}.run", "must contain exactly " + ", ".join(sorted(run_keys)))
    _required_text(run, "run_id")
    # published/withheld are null only when nothing was read from the ledger.
    for name in ("published", "withheld"):
        _count(run[name], f"{field}.run.{name}", nullable=True)
    for name in ("unpublished", "records"):
        _count(run[name], f"{field}.run.{name}", nullable=False)


def _is_commit(value: Any) -> bool:
    return (isinstance(value, str) and len(value) == 40
            and all(ch in "0123456789abcdef" for ch in value))


def _scorecard_publication_files(files: Any, *, field: str = "ledger_mirror.scorecard.files",
                                 generated: frozenset[str] = frozenset(
                                     {"scorecard/SCORECARD.md", "scorecard/cells.json"})) -> None:
    """Validate publication metadata; the proof reader verifies operand bytes."""
    if not isinstance(files, dict) or not generated.issubset(files):
        raise _error(field, "must bind both generated files")
    for path, digest in files.items():
        if (not isinstance(path, str) or not isinstance(digest, str)
                or not re.fullmatch(r"[0-9a-f]{64}", digest)):
            raise _error(field, "must contain named complete SHA-256 identities")
    proof_paths = set(files) - generated
    if not proof_paths:
        return
    carriers = []
    objects = []
    for path in proof_paths:
        if re.fullmatch(r"scorecard/finalized/runs/[0-9a-f]{64}\.json", path):
            carriers.append(path)
        elif re.fullmatch(r"scorecard/finalized/objects/sha256/[0-9a-f]{64}", path):
            if files[path] != path.rsplit("/", 1)[1]:
                raise _error(field, "proof object path must match its SHA-256 identity")
            objects.append(path)
        else:
            raise _error(field, "contains an unsupported publication path")
    # One finalized terminal carries plan/cells/tests. Equal operand bytes may
    # share an object; full role/reference/content verification stays in the
    # finalized-proof reader and unchanged native evidence verifier.
    if len(carriers) != 1 or not 1 <= len(objects) <= 3:
        raise _error(field, "must bind one finalized carrier and one to three proof objects")


def _portable_proof(value: Any, *, publication_state: str) -> None:
    """A derived census carrier published (or refused) without a scorecard."""
    field = "ledger_mirror.portable_proof"
    if not isinstance(value, dict):
        raise _error(field, "must be an object")
    state = value.get("state")
    if state == "failed":
        if publication_state != "derived-failed":
            raise _error(field, "a failed proof must leave the publication derived-failed")
        if set(value) != {"state", "error"}:
            raise _error(field, "failed must contain exactly error, state")
        _required_text(value, "error")
        return
    if state not in {"published", "unchanged"}:
        raise _error(f"{field}.state", f"unsupported value {state!r}")
    if set(value) != {"state", "commit", "files", "hermit_commit", "tool_hermit_commit"}:
        raise _error(field, f"{state} must contain exactly commit, files, hermit_commit, state, tool_hermit_commit")
    for name in ("commit", "hermit_commit", "tool_hermit_commit"):
        if not _is_commit(value[name]):
            raise _error(f"{field}.{name}", "must be lowercase forty-hex")
    _scorecard_publication_files(value["files"], field=f"{field}.files", generated=frozenset())
    if not value["files"]:
        raise _error(f"{field}.files", "must bind one finalized carrier and one to three proof objects")


def _cell_ledger_publication(value: dict[str, Any]) -> None:
    """Extension of the historical mirror record, with explicit storage identity."""
    repository = "https://github.com/rrnewton/hermit_test_ledger.git"
    if value.get("repository") != repository:
        raise _error("ledger_mirror.repository", "must name hermit_test_ledger")
    if value.get("state") == "not-applicable":
        required = {"state", "recorded_at", "repository", "detail"}
        if set(value) != required:
            raise _error(
                "ledger_mirror",
                "not-applicable must contain exactly " + ", ".join(sorted(required)),
            )
        _timestamp(value, "recorded_at", error_field="ledger_mirror.recorded_at")
        _required_text(value, "detail")
        return
    if value.get("state") == "failed":
        _ledger_mirror({key: item for key, item in value.items() if key != "repository"})
        return
    required = {
        "state", "recorded_at", "repository", "commit", "source_commit", "source_tree",
        "rows", "source_repository", "series_commit", "raw_publication", "scorecard",
    }
    if value.get("state") == "derived-failed":
        required.add("detail")
        _required_text(value, "detail")
    if "portable_proof" in value:
        required.add("portable_proof")
        _portable_proof(value["portable_proof"], publication_state=value.get("state"))
    if set(value) != required or value.get("source_repository") != repository:
        raise _error("ledger_mirror", "direct publication has an invalid repository or field set")
    legacy = {key: value[key] for key in (
        "recorded_at", "commit", "source_commit", "source_tree", "rows"
    )}
    legacy["state"] = "published" if value["state"] == "derived-failed" else value["state"]
    _ledger_mirror(legacy)
    if value["series_commit"] != value["source_commit"]:
        raise _error("ledger_mirror.series_commit", "must equal the named source commit")
    if not isinstance(value["raw_publication"], dict):
        raise _error("ledger_mirror.raw_publication", "must retain the publisher outcome")
    scorecard = value["scorecard"]
    expected = "failed" if value["state"] == "derived-failed" else "published"
    if (expected == "published" and isinstance(scorecard, dict)
            and scorecard.get("state") == "superseded"):
        # A newer published head owns latest.json; this run's rows (and any
        # derived proof) published without moving the scorecard backward.
        if set(scorecard) != {"state", "hermit_commit", "current_hermit_commit", "detail"}:
            raise _error("ledger_mirror.scorecard", "superseded must contain exactly "
                         "current_hermit_commit, detail, hermit_commit, state")
        if (not _is_commit(scorecard["hermit_commit"]) or not _is_commit(scorecard["current_hermit_commit"])
                or scorecard["hermit_commit"] == scorecard["current_hermit_commit"]):
            raise _error("ledger_mirror.scorecard", "superseded must name this commit and a different newer head")
        if not isinstance(scorecard["detail"], str) or not scorecard["detail"].strip():
            raise _error("ledger_mirror.scorecard.detail", "must explain why the scorecard did not move")
        return
    if "portable_proof" in value and expected == "published":
        raise _error("ledger_mirror.portable_proof", "a regenerated scorecard binds its proof in scorecard.files")
    if not isinstance(scorecard, dict) or scorecard.get("state") != expected:
        raise _error("ledger_mirror.scorecard", f"must retain the {expected} scorecard outcome")
    if expected == "published":
        sha = scorecard.get("hermit_commit")
        files = scorecard.get("files")
        if (not isinstance(sha, str) or len(sha) != 40
                or any(ch not in "0123456789abcdef" for ch in sha)):
            raise _error("ledger_mirror.scorecard.hermit_commit", "must name the exact producer commit")
        _scorecard_publication_files(files)
        if "tool_hermit_commit" in scorecard:
            tool = scorecard["tool_hermit_commit"]
            if not isinstance(tool, str) or len(tool) != 40 or any(ch not in "0123456789abcdef" for ch in tool):
                raise _error("ledger_mirror.scorecard.tool_hermit_commit", "must name the exact tool commit")
    elif not isinstance(scorecard.get("error"), str) or not scorecard["error"].strip():
        raise _error("ledger_mirror.scorecard.error", "must explain incomplete derived publication")


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


def _cleanup_removal_intent(
    record: Mapping[str, Any], field: str
) -> None:
    value = record.get(field)
    if not isinstance(value, dict):
        raise _error(field, "must be an object")
    target_field = (
        "checkout" if field == "checkout_removal_intent" else "cargo_home"
    )
    expected = {"recorded_at", "path", "identity"}
    if field == "checkout_removal_intent" and "wrkslots_slot" in record:
        expected.update(("wrkslots_slot", "wrkslots_generation"))
    if set(value) != expected:
        raise _error(field, f"must contain exactly {', '.join(sorted(expected))}")
    _timestamp(value, "recorded_at", error_field=f"{field}.recorded_at")
    path = _required_text(value, "path")
    if not Path(path).is_absolute() or path != record.get(target_field):
        raise _error(field, f"must bind the exact absolute {target_field} path")
    identity = value.get("identity")
    identity_fields = {"device", "inode", "mode", "uid", "gid"}
    if (
        not isinstance(identity, dict)
        or set(identity) != identity_fields
        or any(
            type(identity.get(item)) is not int or identity[item] < 0
            for item in identity_fields
        )
        or not stat.S_ISDIR(identity["mode"])
    ):
        raise _error(field, "must bind an exact nonnegative directory identity")
    if field == "checkout_removal_intent":
        if record.get("temporary_checkout") is not True:
            raise _error(field, "requires temporary_checkout=true")
        if "wrkslots_slot" in expected:
            if (
                not isinstance(value.get("wrkslots_slot"), str)
                or not value["wrkslots_slot"]
                or type(value.get("wrkslots_generation")) is not int
                or value["wrkslots_generation"] < 0
                or value.get("wrkslots_slot") != record.get("wrkslots_slot")
                or value.get("wrkslots_generation")
                != record.get("wrkslots_generation")
            ):
                raise _error(field, "must bind the exact wrkslots identity")


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
    if "github_executable" in value:
        if not Path(_required_text(value, "github_executable")).is_absolute():
            raise _error("github_executable", "must be an absolute executable path")
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
    if "ledger_mirror" in value:
        _ledger_mirror(value.get("ledger_mirror"))
    if "cpu_placement" in value:
        _cpu_placement(value.get("cpu_placement"))
    if "git_substitution" in value:
        _git_substitution(value.get("git_substitution"))

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
        if "runtime_root_identity" in value:
            runtime_identity = value.get("runtime_root_identity")
            expected_identity_fields = {"device", "inode", "mode", "uid", "gid"}
            if (
                not isinstance(runtime_identity, dict)
                or set(runtime_identity) != expected_identity_fields
                or any(
                    type(runtime_identity.get(field)) is not int
                    or runtime_identity[field] < 0
                    for field in expected_identity_fields
                )
                or not stat.S_ISDIR(runtime_identity["mode"])
            ):
                raise _error(
                    "runtime_root_identity",
                    "must be an exact nonnegative directory identity",
                )
        for intent_field in (
            "checkout_removal_intent",
            "cargo_home_removal_intent",
        ):
            if intent_field in value:
                _cleanup_removal_intent(value, intent_field)
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
            and value.get("reverie_count_evidence") is None
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

    count_fields = {"reverie_count_request", "reverie_count_evidence", "reverie_count_refusal"}
    if count_fields.intersection(value):
        if kind is not RunKind.REVERIE_VALIDATE:
            raise _error("reverie_count_evidence", "is valid only for Reverie validation handles")
        try:
            if "reverie_count_request" in value:
                reverie_node.validate_count_request(value["reverie_count_request"])
            evidence = value.get("reverie_count_evidence")
            if evidence is not None:
                evidence_path = Path(evidence["binding"]["context"]["record_path"])
                if not evidence_path.is_absolute():
                    raise ValueError("count evidence path is not absolute")
                executed, passed, _filtered = reverie_node.parse_stored_count_evidence(
                    evidence, dict(value), evidence_path
                )
                if state not in {RunState.COMPLETED, RunState.KILLED}:
                    raise ValueError("count evidence requires an observed terminal handle")
                if value.get("reverie_count_refusal") is None and (
                    value.get("executed_tests"), value.get("passed_tests")
                ) != (executed, passed):
                    raise ValueError("registry counts disagree with the exact event evidence")
            if evidence is None or value.get("reverie_count_refusal") is not None:
                if any(value.get(key) is not None for key in ("executed_tests", "passed_tests")):
                    raise ValueError("unavailable/refused count evidence requires null counts")
            if value.get("executed_nodes") is not None:
                raise ValueError("framework test counts do not establish executed DAG nodes")
        except (KeyError, TypeError, ValueError, RuntimeError) as error:
            raise _error("reverie_count_evidence", str(error)) from error

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
        if state_root is None:
            payload = path.read_bytes()
        else:
            before = os.lstat(path)
            if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
                raise RuntimeError(
                    "canonical validation handle is not a regular single-link "
                    f"file: {path}"
                )
            flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NONBLOCK
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            descriptor = os.open(path, flags)
            try:
                opened_before = os.fstat(descriptor)
                chunks: list[bytes] = []
                offset = 0
                while True:
                    chunk = os.pread(descriptor, 1024 * 1024, offset)
                    if not chunk:
                        break
                    chunks.append(chunk)
                    offset += len(chunk)
                opened_after = os.fstat(descriptor)
            finally:
                os.close(descriptor)
            after = os.lstat(path)
            identities = {
                (
                    item.st_dev,
                    item.st_ino,
                    item.st_mode,
                    item.st_uid,
                    item.st_gid,
                    item.st_nlink,
                    item.st_size,
                    item.st_mtime_ns,
                    item.st_ctime_ns,
                )
                for item in (before, opened_before, opened_after, after)
            }
            if (
                not stat.S_ISREG(opened_before.st_mode)
                or opened_before.st_nlink != 1
                or len(identities) != 1
            ):
                raise RuntimeError(
                    f"canonical validation handle changed while it was read: {path}"
                )
            payload = b"".join(chunks)
        value = json.loads(payload)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
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
    """A requested record/population lock cannot safely be acquired.

    Nonblocking calls raise on ordinary contention. A blocking writer still
    waits unless this process already owns a callback-spanning population
    authority, where waiting could deadlock the owner; that case also raises
    and requires the explicit authority token for trusted internal bookkeeping.
    """


class RecordLockedByAnotherThread(RecordLocked):
    """A nonblocking request met another thread of this process that will release.

    Raised only when ``blocking=False`` and the in-process holder is a
    different thread that accepts waiters, which is exactly the case a
    blocking request waits out on the in-process condition. A caller that
    polls under its own bound may retry it. Same-thread re-entry, authority
    inherited through a no-exec fork, and holders that reject waiters raise a
    plain ``RecordLocked`` instead, because waiting can never end those.
    """


@dataclass
class RunPopulationAuthority:
    """An exclusive lock on one state root's complete run-handle population.

    The lock is on the state-root directory, above ``ignored/validate/runs``.
    Replacing the runs directory therefore cannot detach cleanup from writers
    that resolve the new canonical path.  The token is explicit so cleanup can
    perform its own bookkeeping without making arbitrary nested writers
    reentrant.
    """

    state_root: Path
    descriptor: int
    identity: tuple[int, ...]
    owner_pid: int
    generation_authority: RunWriterGenerationAuthority
    active: bool = True


@dataclass
class RunWriterGenerationAuthority:
    """A rollout-generation lock shared by writers and exclusive to cleanup.

    This lock is deliberately distinct from the population lock.  A launcher
    holds the shared side while it creates storage and its first run record;
    every current writer also takes the shared side before the population
    lock.  A future cleanup enablement must hold the exclusive side before it
    takes population/row authority.  That closes the pre-record launch window
    once every supported launcher has deployed this protocol.
    """

    state_root: Path
    path: Path
    descriptor: int
    identity: tuple[int, ...]
    exclusive: bool
    owner_pid: int
    active: bool = True


RUN_POPULATION_IDENTITY_FIELDS = (
    "st_dev",
    "st_ino",
    "st_mode",
    "st_uid",
    "st_gid",
)
_RUN_POPULATION_CONDITION = threading.Condition()
# canonical root -> (PID, owning thread, reject every tokenless local waiter)
_RUN_POPULATION_ACTIVE: dict[Path, tuple[int, int, bool]] = {}

RUN_WRITER_GENERATION_LOCK = Path(
    "ignored/validate/run-writer-generation-v1.lock"
)
RUN_WRITER_GENERATION_IDENTITY_FIELDS = (
    "st_dev",
    "st_ino",
    "st_mode",
    "st_uid",
    "st_gid",
    "st_nlink",
)
_RUN_WRITER_GENERATION_CONDITION = threading.Condition()
# canonical root -> [(PID, thread, exclusive, unique local reservation)]
_RUN_WRITER_GENERATION_ACTIVE: dict[
    Path, list[tuple[int, int, bool, object]]
] = {}


def _directory_identity(metadata: os.stat_result) -> tuple[int, ...]:
    return tuple(
        int(getattr(metadata, field)) for field in RUN_POPULATION_IDENTITY_FIELDS
    )


def _writer_generation_identity(metadata: os.stat_result) -> tuple[int, ...]:
    return tuple(
        int(getattr(metadata, field))
        for field in RUN_WRITER_GENERATION_IDENTITY_FIELDS
    )


def require_run_writer_generation_authority(
    root: Path,
    authority: RunWriterGenerationAuthority,
    *,
    exclusive: bool | None = None,
) -> None:
    """Require the named lock file to remain the authority's opened inode."""

    canonical = root.resolve(strict=True)
    if (
        not authority.active
        or authority.owner_pid != os.getpid()
        or authority.state_root != canonical
        or (exclusive is not None and authority.exclusive is not exclusive)
    ):
        raise RuntimeError(
            "run-writer generation authority is absent or belongs elsewhere"
        )
    opened = os.fstat(authority.descriptor)
    named = os.lstat(authority.path)
    if (
        not stat.S_ISREG(opened.st_mode)
        or opened.st_nlink != 1
        or _writer_generation_identity(opened) != authority.identity
        or _writer_generation_identity(named) != authority.identity
    ):
        raise RuntimeError(
            "run-writer generation lock changed while authority was held"
        )


@contextmanager
def run_writer_generation_lock(
    root: Path,
    *,
    exclusive: bool = False,
    blocking: bool = True,
):
    """Hold the outer writer-generation lock for launch or cleanup.

    Lock order is owner-state (only for owner-watch correction), then this
    generation lock, then population, then run-number (writers only), then
    sorted record locks.  Shared acquisitions may nest or coexist.  An
    exclusive acquisition conflicting with any same-process holder, or a
    shared acquisition conflicting with a same-process exclusive holder, fails
    immediately so a callback cannot deadlock its owner.  A no-exec fork also
    fails closed because it inherited the parent's open lock description.
    """

    canonical = root.resolve(strict=True)
    lock_path = canonical / RUN_WRITER_GENERATION_LOCK
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    owner_pid = os.getpid()
    thread_id = threading.get_ident()
    reservation = object()
    with _RUN_WRITER_GENERATION_CONDITION:
        active = _RUN_WRITER_GENERATION_ACTIVE.setdefault(canonical, [])
        if any(pid != owner_pid for pid, _thread, _exclusive, _token in active):
            raise RecordLocked(
                "forked process inherited run-writer generation authority for "
                f"{canonical}; exec before writing run records"
            )
        if exclusive and active:
            raise RecordLocked(
                "this process already holds run-writer generation authority for "
                f"{canonical}"
            )
        if not exclusive and any(item[2] for item in active):
            raise RecordLocked(
                "this process already holds exclusive run-writer generation "
                f"authority for {canonical}; run-population authority is "
                "unavailable to a callback writer"
            )
        active.append((owner_pid, thread_id, exclusive, reservation))

    flags = os.O_RDWR | os.O_CREAT | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor: int | None = None
    authority: RunWriterGenerationAuthority | None = None
    try:
        descriptor = os.open(lock_path, flags, 0o600)
        opened_before = os.fstat(descriptor)
        named_before = os.lstat(lock_path)
        identity = _writer_generation_identity(opened_before)
        if (
            not stat.S_ISREG(opened_before.st_mode)
            or opened_before.st_nlink != 1
            or _writer_generation_identity(named_before) != identity
        ):
            raise RuntimeError(
                f"run-writer generation lock is not canonical: {lock_path}"
            )
        mode = fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH
        if not blocking:
            mode |= fcntl.LOCK_NB
        try:
            fcntl.flock(descriptor, mode)
        except BlockingIOError as error:
            raise RecordLocked(
                "another process holds incompatible run-writer generation "
                f"authority for {canonical}"
            ) from error
        authority = RunWriterGenerationAuthority(
            canonical,
            lock_path,
            descriptor,
            identity,
            exclusive,
            owner_pid,
        )
        require_run_writer_generation_authority(
            canonical, authority, exclusive=exclusive
        )
        yield authority
        require_run_writer_generation_authority(
            canonical, authority, exclusive=exclusive
        )
    finally:
        if authority is not None:
            authority.active = False
        if descriptor is not None:
            with suppress(OSError):
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            os.close(descriptor)
        with _RUN_WRITER_GENERATION_CONDITION:
            active = _RUN_WRITER_GENERATION_ACTIVE.get(canonical, [])
            active = [item for item in active if item[3] is not reservation]
            if active:
                _RUN_WRITER_GENERATION_ACTIVE[canonical] = active
            else:
                _RUN_WRITER_GENERATION_ACTIVE.pop(canonical, None)
            _RUN_WRITER_GENERATION_CONDITION.notify_all()


def require_run_population_authority(
    root: Path, authority: RunPopulationAuthority
) -> None:
    """Require a live authority for the still-canonical state-root directory."""

    canonical = root.resolve(strict=True)
    if (
        not authority.active
        or authority.owner_pid != os.getpid()
        or authority.state_root != canonical
    ):
        raise RuntimeError("run-population authority is absent or belongs elsewhere")
    require_run_writer_generation_authority(
        canonical, authority.generation_authority
    )
    opened = os.fstat(authority.descriptor)
    current = os.stat(canonical, follow_symlinks=False)
    if (
        not stat.S_ISDIR(opened.st_mode)
        or _directory_identity(opened) != authority.identity
        or _directory_identity(current) != authority.identity
    ):
        raise RuntimeError(
            "validation state root changed while run-population authority was held"
        )


@contextmanager
def run_population_lock(
    root: Path,
    *,
    blocking: bool = True,
    reject_same_process: bool = False,
    _generation_authority: RunWriterGenerationAuthority | None = None,
):
    """Serialize every run-handle writer with population-wide cleanup.

    Ordinary writers take generation -> population -> run-number (when
    applicable) -> record.
    Owner-watch correction is the one deliberate outer-lock exception: its
    producer already holds owner-state while a validate-run child writes, so
    correction takes owner-state -> generation -> population -> records.
    Cleanup never takes owner-state, which keeps that order acyclic. Cleanup
    will take the exclusive generation side before population once its rollout
    fence is discharged. Cleanup/correction callers set ``reject_same_process``
    while invoking callbacks: a tokenless writer on *any* thread then fails
    rather than waiting for the callback that is waiting for it. Ordinary
    writers serialize threads locally and processes through ``flock``.
    """

    if _generation_authority is None:
        with run_writer_generation_lock(
            root, exclusive=False, blocking=blocking
        ) as generation_authority:
            with run_population_lock(
                root,
                blocking=blocking,
                reject_same_process=reject_same_process,
                _generation_authority=generation_authority,
            ) as authority:
                yield authority
        return

    require_run_writer_generation_authority(root, _generation_authority)

    canonical = root.resolve(strict=True)
    owner_pid = os.getpid()
    thread_id = threading.get_ident()
    with _RUN_POPULATION_CONDITION:
        while canonical in _RUN_POPULATION_ACTIVE:
            active_pid, owner_thread, owner_rejects_waiters = (
                _RUN_POPULATION_ACTIVE[canonical]
            )
            # A fork inherits both this marker and the open file description
            # carrying the parent's flock. Opening a second descriptor and
            # waiting on it can therefore deadlock the child against a lock it
            # inherited itself. A forked process must exec (O_CLOEXEC releases
            # the inherited descriptor) before it may become an independent
            # writer; without exec it fails closed here.
            if active_pid != owner_pid:
                raise RecordLocked(
                    "forked process inherited run-population authority for "
                    f"{canonical}; exec before writing run records"
                )
            if owner_thread == thread_id or owner_rejects_waiters:
                raise RecordLocked(
                    "this process already holds run-population authority for "
                    f"{canonical}"
                )
            if not blocking:
                # The one in-process conflict that ends on its own: a blocking
                # request would wait on the condition below.
                raise RecordLockedByAnotherThread(
                    "this process already holds run-population authority for "
                    f"{canonical} on another thread"
                )
            _RUN_POPULATION_CONDITION.wait()
        _RUN_POPULATION_ACTIVE[canonical] = (
            owner_pid,
            thread_id,
            reject_same_process,
        )
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor: int | None = None
    authority: RunPopulationAuthority | None = None
    try:
        descriptor = os.open(canonical, flags)
        mode = fcntl.LOCK_EX if blocking else fcntl.LOCK_EX | fcntl.LOCK_NB
        try:
            fcntl.flock(descriptor, mode)
        except BlockingIOError as error:
            raise RecordLocked(
                "another process holds run-population authority for "
                f"{canonical}"
            ) from error
        opened = os.fstat(descriptor)
        current = os.stat(canonical, follow_symlinks=False)
        if not stat.S_ISDIR(opened.st_mode) or _directory_identity(
            opened
        ) != _directory_identity(current):
            raise RuntimeError(
                "validation state root changed while run-population authority "
                "was acquired"
            )
        authority = RunPopulationAuthority(
            canonical,
            descriptor,
            _directory_identity(opened),
            os.getpid(),
            _generation_authority,
        )
        require_run_writer_generation_authority(root, _generation_authority)
        yield authority
        require_run_writer_generation_authority(root, _generation_authority)
    finally:
        if authority is not None:
            authority.active = False
        if descriptor is not None:
            with suppress(OSError):
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            os.close(descriptor)
        with _RUN_POPULATION_CONDITION:
            current = _RUN_POPULATION_ACTIVE.get(canonical)
            if current is not None and current[:2] == (owner_pid, thread_id):
                del _RUN_POPULATION_ACTIVE[canonical]
            _RUN_POPULATION_CONDITION.notify_all()


@contextmanager
def _record_population_authority(
    path: Path,
    *,
    blocking: bool,
    authority: RunPopulationAuthority | None = None,
):
    state_root = _state_root_for_run_handle(path)
    if state_root is None:
        if authority is not None:
            raise RuntimeError(
                "run-population authority supplied for a non-run-handle path"
            )
        yield None
        return
    if authority is not None:
        require_run_population_authority(state_root, authority)
        yield authority
        require_run_population_authority(state_root, authority)
        return
    with run_population_lock(state_root, blocking=blocking) as held:
        yield held


@contextmanager
def owner_watch_population_lock(owner_state_dir: Path):
    """Serialize owner-watch production with evidence-bound corrections.

    Owner watch holds this while its validate-run subprocess takes the writer
    generation lock and then the separate run-population lock. A correction
    needing all three therefore takes owner-state -> generation -> population;
    cleanup never takes owner-state, so that exceptional ordering stays
    acyclic. Ordinary writers instead take generation -> population ->
    run-number (when needed) -> record.
    """

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


def _write_unlocked(
    path: Path,
    value: Mapping[str, Any],
    *,
    _replacement_observer: Callable[[int], None] | None = None,
) -> None:
    if not is_current_schema_version(value.get("schema_version")):
        raise RuntimeError(f"validation handle {path} has an unsupported schema")
    if value.get("producer") == PRODUCER:
        parse_current_record(value)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    # A destructive-cleanup bookkeeper must bind its continuing authority to
    # the exact inode it is about to install, not reopen the pathname after the
    # rename.  Keep a private description of the temporary inode across
    # os.replace and lend it to the observer only for the duration of the
    # callback.  Ordinary writers pay no extra descriptor cost.
    try:
        replacement_descriptor = (
            os.dup(fd) if _replacement_observer is not None else None
        )
    except BaseException:
        os.close(fd)
        with suppress(FileNotFoundError):
            os.unlink(temporary)
        raise
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(dict(value), stream, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        if _replacement_observer is not None:
            assert replacement_descriptor is not None
            _replacement_observer(replacement_descriptor)
    finally:
        if replacement_descriptor is not None:
            os.close(replacement_descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


RUN_RECORD_IDENTITY_FIELDS = (
    "st_dev",
    "st_ino",
    "st_mode",
    "st_uid",
    "st_gid",
    "st_nlink",
    "st_size",
    "st_mtime_ns",
    "st_ctime_ns",
)


def _run_record_identity(metadata: os.stat_result) -> tuple[int, ...]:
    return tuple(int(getattr(metadata, field)) for field in RUN_RECORD_IDENTITY_FIELDS)


@contextmanager
def _pinned_parent_directory(path: Path):
    """Pin and verify the no-follow directory that will own a new handle."""

    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path.parent, flags)
    try:
        opened = os.fstat(descriptor)
        named = os.stat(path.parent, follow_symlinks=False)
        identity = _directory_identity(opened)
        if (
            not stat.S_ISDIR(opened.st_mode)
            or not stat.S_ISDIR(named.st_mode)
            or _directory_identity(named) != identity
        ):
            raise RuntimeError(
                f"validation handle parent is not canonical before creation: {path.parent}"
            )
        yield descriptor, identity
    finally:
        os.close(descriptor)


def _make_created_record_durable(
    path: Path,
    *,
    parent_descriptor: int,
    parent_identity: tuple[int, ...],
    record_descriptor: int,
) -> None:
    """Sync the pinned parent and prove it still names the installed inode."""

    os.fsync(parent_descriptor)
    try:
        opened_parent = os.fstat(parent_descriptor)
        named_parent = os.stat(path.parent, follow_symlinks=False)
        opened_record = os.fstat(record_descriptor)
        pinned_record = os.stat(
            path.name, dir_fd=parent_descriptor, follow_symlinks=False
        )
        named_record = os.lstat(path)
    except OSError as error:
        raise RuntimeError(
            "validation handle parent or installed record changed before "
            f"creation became durable: {path}: {error}"
        ) from error
    record_identities = {
        _run_record_identity(item)
        for item in (opened_record, pinned_record, named_record)
    }
    if (
        not stat.S_ISDIR(opened_parent.st_mode)
        or _directory_identity(opened_parent) != parent_identity
        or not stat.S_ISDIR(named_parent.st_mode)
        or _directory_identity(named_parent) != parent_identity
        or not stat.S_ISREG(opened_record.st_mode)
        or opened_record.st_nlink != 1
        or len(record_identities) != 1
    ):
        raise RuntimeError(
            "validation handle parent or installed record changed before "
            f"creation became durable: {path}"
        )


def _create_unlocked(
    path: Path,
    value: Mapping[str, Any],
    *,
    parent_descriptor: int,
    parent_identity: tuple[int, ...],
) -> None:
    """Create without clobbering, entirely through the pinned parent fd."""

    if not is_current_schema_version(value.get("schema_version")):
        raise RuntimeError(f"validation handle {path} has an unsupported schema")
    if value.get("producer") == PRODUCER:
        parse_current_record(value)

    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    temporary_name: str | None = None
    descriptor: int | None = None
    for _attempt in range(128):
        candidate = f".{path.name}.{secrets.token_hex(16)}.tmp"
        try:
            descriptor = os.open(
                candidate,
                flags,
                0o600,
                dir_fd=parent_descriptor,
            )
        except FileExistsError:
            continue
        temporary_name = candidate
        break
    if descriptor is None or temporary_name is None:
        raise RuntimeError(
            f"cannot allocate a unique temporary validation handle for {path}"
        )

    try:
        record_descriptor = os.dup(descriptor)
    except BaseException:
        os.close(descriptor)
        with suppress(FileNotFoundError):
            os.unlink(temporary_name, dir_fd=parent_descriptor)
        raise
    try:
        try:
            stream = os.fdopen(descriptor, "w")
        except BaseException:
            os.close(descriptor)
            raise
        with stream:
            json.dump(dict(value), stream, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        _rename_noreplace(parent_descriptor, temporary_name, path.name, path)
        _make_created_record_durable(
            path,
            parent_descriptor=parent_descriptor,
            parent_identity=parent_identity,
            record_descriptor=record_descriptor,
        )
    finally:
        os.close(record_descriptor)
        with suppress(FileNotFoundError):
            os.unlink(temporary_name, dir_fd=parent_descriptor)


def _rename_noreplace(
    parent_descriptor: int,
    temporary_name: str,
    destination_name: str,
    path: Path,
) -> None:
    """Publish one inode through a pinned directory without any clobber window."""

    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, "renameat2", None)
    if renameat2 is None:
        raise RuntimeError(
            f"atomic no-replace validation handle creation is unavailable: {path}"
        )
    renameat2.argtypes = [
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    ]
    renameat2.restype = ctypes.c_int
    ctypes.set_errno(0)
    if renameat2(
        parent_descriptor,
        os.fsencode(temporary_name),
        parent_descriptor,
        os.fsencode(destination_name),
        1,  # RENAME_NOREPLACE
    ) == 0:
        return
    number = ctypes.get_errno()
    if number == errno.EEXIST:
        raise RuntimeError(f"validation handle already exists: {path}")
    raise OSError(number, os.strerror(number), path)


def write_record(
    path: Path,
    value: Mapping[str, Any],
    *,
    _population_authority: RunPopulationAuthority | None = None,
) -> None:
    with _record_population_authority(
        path, blocking=True, authority=_population_authority
    ):
        with exclusive_record(path):
            _write_unlocked(path, value)


def write_current_record(
    path: Path,
    value: Mapping[str, Any],
    *,
    _population_authority: RunPopulationAuthority | None = None,
) -> None:
    parse_current_record(value)
    write_record(path, value, _population_authority=_population_authority)


def create_record(
    path: Path,
    value: Mapping[str, Any],
    *,
    _population_authority: RunPopulationAuthority | None = None,
) -> None:
    """Create a run handle without replacing another run's identity."""
    with _record_population_authority(
        path, blocking=True, authority=_population_authority
    ):
        with exclusive_record(path):
            with _pinned_parent_directory(path) as (
                parent_descriptor,
                parent_identity,
            ):
                try:
                    os.stat(
                        path.name,
                        dir_fd=parent_descriptor,
                        follow_symlinks=False,
                    )
                except FileNotFoundError:
                    pass
                else:
                    raise RuntimeError(f"validation handle already exists: {path}")
                # The file, publication, and durability barrier all resolve
                # through the pre-install directory descriptor. This makes a
                # parent rename detectable without ever overwriting a row in a
                # replacement directory.
                _create_unlocked(
                    path,
                    value,
                    parent_descriptor=parent_descriptor,
                    parent_identity=parent_identity,
                )


def create_current_record(
    path: Path,
    value: Mapping[str, Any],
    *,
    _population_authority: RunPopulationAuthority | None = None,
) -> None:
    parse_current_record(value)
    create_record(path, value, _population_authority=_population_authority)


def reserve_log(path: Path) -> None:
    """Reserve a systemd append target; a second run must not share it."""
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError as error:
        raise RuntimeError(f"validation log already exists: {path}") from error
    os.close(descriptor)


def update_record(
    path: Path,
    *,
    blocking: bool = True,
    _population_authority: RunPopulationAuthority | None = None,
    _record_lock_held: bool = False,
    _replacement_observer: Callable[[int], None] | None = None,
    _locked_base_record: Mapping[str, Any] | None = None,
    **fields: Any,
) -> dict[str, Any]:
    # The caller and the observer finish at nearly the same time. Serialize the
    # read-modify-write so a final exit update cannot erase cgroup evidence (or
    # vice versa) while retaining atomic replacement for readers.
    def update_locked() -> dict[str, Any]:
        nonlocal fields
        value = (
            read_record(path)
            if _locked_base_record is None
            else dict(_locked_base_record)
        )
        if "reverie_count_request" in value:
            # Generic terminal observers have no exact-event count authority.
            fields = {
                key: item
                for key, item in fields.items()
                if key not in {"executed_nodes", "executed_tests", "passed_tests"}
            }
        value.update(fields)
        _write_unlocked(
            path, value, _replacement_observer=_replacement_observer
        )
        return value

    if _record_lock_held:
        if _population_authority is None:
            raise RuntimeError(
                "record-lock-held update requires explicit run-population authority"
            )
        with _record_population_authority(
            path, blocking=blocking, authority=_population_authority
        ):
            return update_locked()
    if _locked_base_record is not None:
        raise RuntimeError(
            "a descriptor-bound base record requires an already-held record lock"
        )
    with _record_population_authority(
        path, blocking=blocking, authority=_population_authority
    ):
        with exclusive_record(path, blocking=blocking):
            return update_locked()


def update_reverie_counts(
    path: Path, expected: dict[str, Any], *, evidence: dict[str, Any] | None = None,
    refusal: str | None = None,
    _population_authority: RunPopulationAuthority | None = None,
) -> dict[str, Any]:
    """Bind counts under the existing record lock without changing any outcome.

    Retain an earlier proof when a later read refuses; its counts cease to have
    current authority, and the distinct refusal remains visible. A conflicting
    second handoff can never replace the original proof.
    """
    with _record_population_authority(
        path, blocking=False, authority=_population_authority
    ):
        with exclusive_record(path, blocking=False):
            value = read_record(path)
            if any(
                value.get(key) != expected.get(key)
                for key in reverie_node.COUNT_CONTEXT_FIELDS
            ):
                raise _error(
                    "reverie_count_evidence",
                    "handle identity changed during canonical read",
                )
            if evidence is not None:
                executed, passed, _filtered = reverie_node.validate_count_evidence(
                    evidence, value, path
                )
                previous = value.get("reverie_count_evidence")
                if previous is not None and previous != evidence:
                    refusal = (
                        "conflicting second Reverie count handoff; original proof retained"
                    )
                else:
                    value["reverie_count_evidence"] = evidence
                    value.update(executed_tests=executed, passed_tests=passed)
            if refusal is not None:
                value.update(executed_tests=None, passed_tests=None)
            value["reverie_count_refusal"] = refusal
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
    with _record_population_authority(path, blocking=True):
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

    with _record_population_authority(path, blocking=True):
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
                    "termination_requests",
                    "recorded request changed before confirmation",
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
    with _record_population_authority(path, blocking=True):
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
            if "reverie_count_request" in value:
                fields = {
                    key: item
                    for key, item in fields.items()
                    if key not in {"executed_nodes", "executed_tests", "passed_tests"}
                }
            if fields:
                value.update(fields)
                _write_unlocked(path, value)
    return value


def bind_process_identity(path: Path, identity: ProcessIdentity) -> dict[str, Any]:
    """Publish one process identity without permitting a different replacement."""
    with _record_population_authority(path, blocking=True):
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
    with _record_population_authority(path, blocking=True):
        with exclusive_record(path):
            value = read_record(path)
            if value.get("producer") != PRODUCER:
                raise _error(
                    "producer", "admission result requires a current handle"
                )
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
    with _record_population_authority(path, blocking=True):
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
                _write_run_number_state(
                    state_path, repo=repo, host=host, number=number
                )
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
