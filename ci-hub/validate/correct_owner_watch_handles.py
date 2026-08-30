#!/usr/bin/env python3
"""Correct owner-watch handles proven to be pre-service log collisions.

Dry-run is the default. Applying a correction requires the exact plan hash
printed by a prior dry run. Every invocation first recovers an interrupted
correction transaction. This command rewrites run handles only: it neither
creates admission evidence nor appends a validation-ledger event.
"""

from __future__ import annotations

import argparse
import base64
from contextlib import ExitStack
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
from typing import Any, Callable, Mapping, Sequence

import run_registry


SCHEMA_VERSION = 1
TRANSACTION_SCHEMA_VERSION = 1
TRANSACTION_MANIFEST = "transaction.json"
TRANSACTION_PREPARING = "transaction.preparing.json"
TRANSACTION_COMMITTED = "committed.json"
OWNER_AGENT = "owner-checkout-watch"
OWNER_REPOSITORY = "rrnewton/hermit"
ATTEMPT_RE = re.compile(r"^\d{8}T\d{12}Z-\d+$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
UNIT_STEM_RE = re.compile(
    r"^validate-owner-checkout-(?P<date>\d{8})t(?P<clock>\d{12})z-(?P<pid>\d+)$"
)
DAG_TERMINAL_RE = re.compile(r"^\[[^]]+\].*(?:PASS|FAIL|SKIP)", re.MULTILINE)
ACQUIRED_RE = re.compile(r"^validate-lock: ACQUIRED\b", re.MULTILINE)
REFUSED_STATE_LINE = (
    "  state:  REFUSED -- the validate NEVER RAN, so there is no log and no result."
)
REFUSED_REMEDY_LINE = (
    "  remedy: the unit did not start; check `ci-hub validate-lock status` for a "
    "held box lock and retry once it is free"
)
Runner = Callable[..., subprocess.CompletedProcess[str]]
Replacer = Callable[[int, str, int, str], None]


class CorrectionRefused(RuntimeError):
    """The evidence does not uniquely prove the historical producer defect."""


@dataclass(frozen=True)
class EvidenceRoots:
    state_root: Path
    checkout: Path
    owner_state_dir: Path
    results: Path
    handles: Path
    logs: Path
    artifacts: Path
    transaction_parent: Path


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def plan_sha256(value: Mapping[str, Any]) -> str:
    body = dict(value)
    body.pop("plan_sha256", None)
    return sha256(json.dumps(body, sort_keys=True, separators=(",", ":")).encode())


def canonical_bytes(value: Mapping[str, Any]) -> bytes:
    return (json.dumps(dict(value), indent=2, sort_keys=True) + "\n").encode()


def absolute(path: Path) -> Path:
    return Path(os.path.abspath(os.fspath(path)))


def canonical_directory(path: Path, *, role: str) -> Path:
    candidate = absolute(path)
    if candidate.is_symlink() or not candidate.is_dir():
        raise CorrectionRefused(f"{role} {candidate} is not a real directory")
    try:
        resolved = candidate.resolve(strict=True)
    except OSError as error:
        raise CorrectionRefused(f"cannot resolve {role} {candidate}: {error}") from error
    if resolved != candidate:
        raise CorrectionRefused(f"{role} {candidate} contains a symlink component")
    return candidate


def descendant(path: Path, *, root: Path, role: str) -> Path:
    candidate = absolute(path)
    try:
        candidate.relative_to(root)
    except ValueError as error:
        raise CorrectionRefused(f"{role} {candidate} escapes {root}") from error
    resolved = candidate.resolve(strict=False)
    try:
        resolved.relative_to(root)
    except ValueError as error:
        raise CorrectionRefused(
            f"{role} {candidate} resolves outside {root}: {resolved}"
        ) from error
    cursor = root
    for part in candidate.relative_to(root).parts:
        cursor /= part
        if cursor.is_symlink():
            raise CorrectionRefused(f"{role} {candidate} contains symlink {cursor}")
    return candidate


def child(path: Path, *, parent: Path, role: str) -> Path:
    candidate = descendant(path, root=parent, role=role)
    if candidate.parent != parent:
        raise CorrectionRefused(f"{role} {candidate} is not a direct child of {parent}")
    return candidate


def load_roots(state_root: Path, owner_state_dir: Path) -> EvidenceRoots:
    state = canonical_directory(state_root, role="state root")
    owner = canonical_directory(owner_state_dir, role="owner state directory")
    validate_root = canonical_directory(
        state / "ignored/validate", role="validation state directory"
    )
    return EvidenceRoots(
        state_root=state,
        checkout=canonical_directory(state / "hermit", role="Hermit checkout"),
        owner_state_dir=owner,
        results=canonical_directory(owner / "results", role="owner results directory"),
        handles=canonical_directory(validate_root / "runs", role="run-handle directory"),
        logs=canonical_directory(owner / "logs", role="owner log directory"),
        artifacts=canonical_directory(
            validate_root / "artifacts", role="validation artifact directory"
        ),
        transaction_parent=validate_root,
    )


def transaction_path(roots: EvidenceRoots) -> Path:
    return roots.transaction_parent / run_registry.OWNER_WATCH_CORRECTION_TRANSACTION


def read_regular_bytes(path: Path, *, root: Path, role: str) -> bytes:
    candidate = child(path, parent=root, role=role)
    flags = os.O_RDONLY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor: int | None = None
    try:
        descriptor = os.open(candidate, flags)
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            raise CorrectionRefused(f"{role} {candidate} is not a regular file")
        bound = Path(f"/proc/self/fd/{descriptor}").resolve(strict=True)
        if bound != candidate:
            raise CorrectionRefused(
                f"{role} {candidate} opened as a different path: {bound}"
            )
        with os.fdopen(descriptor, "rb") as stream:
            descriptor = None
            raw = stream.read()
        current = os.stat(candidate, follow_symlinks=False)
        if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
            raise CorrectionRefused(f"{role} {candidate} changed while it was read")
        return raw
    except CorrectionRefused:
        if descriptor is not None:
            os.close(descriptor)
        raise
    except OSError as error:
        if descriptor is not None:
            os.close(descriptor)
        raise CorrectionRefused(f"{role} {candidate} is unreadable: {error}") from error


def read_object(
    path: Path, *, root: Path, role: str
) -> tuple[dict[str, Any], bytes]:
    raw = read_regular_bytes(path, root=root, role=role)
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise CorrectionRefused(f"{role} {path} is unreadable: {error}") from error
    if not isinstance(value, dict):
        raise CorrectionRefused(f"{role} {path} is not a JSON object")
    return value, raw


def directory_membership(path: Path) -> list[dict[str, object]]:
    membership: list[dict[str, object]] = []
    try:
        entries = list(os.scandir(path))
    except OSError as error:
        raise CorrectionRefused(f"cannot list evidence directory {path}: {error}") from error
    for entry in sorted(entries, key=lambda item: item.name):
        if entry.name.startswith(".") and entry.name.endswith(".lock"):
            continue
        try:
            metadata = entry.stat(follow_symlinks=False)
        except OSError as error:
            raise CorrectionRefused(
                f"cannot stat evidence-directory member {entry.path}: {error}"
            ) from error
        kind = (
            "symlink"
            if stat.S_ISLNK(metadata.st_mode)
            else "file"
            if stat.S_ISREG(metadata.st_mode)
            else "directory"
            if stat.S_ISDIR(metadata.st_mode)
            else "other"
        )
        membership.append({"name": entry.name, "kind": kind})
    return membership


def unit_is_absent(unit: str, *, run: Runner = subprocess.run) -> bool:
    completed = run(
        [
            "systemctl",
            "--user",
            "show",
            unit,
            "--property=LoadState",
            "--property=InvocationID",
            "--no-pager",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise CorrectionRefused(
            f"cannot establish whether systemd unit {unit} exists: "
            + (
                completed.stderr.strip()
                or completed.stdout.strip()
                or f"exit {completed.returncode}"
            )
        )
    properties = dict(
        line.split("=", 1) for line in completed.stdout.splitlines() if "=" in line
    )
    return properties.get("LoadState") == "not-found" and not properties.get(
        "InvocationID"
    )


def candidate_signal(result: Mapping[str, Any]) -> bool:
    refusal = result.get("admission_refusal")
    if not isinstance(refusal, dict):
        return False
    detail = refusal.get("detail")
    line = refusal.get("line")
    return (
        isinstance(detail, str) and detail.startswith("validation log already exists: ")
    ) or (
        isinstance(line, str)
        and line.startswith("validate-run: REFUSED: validation log already exists: ")
    )


def expected_paths(
    state_root: Path, owner_state_dir: Path, attempt: str
) -> tuple[str, Path, Path, Path, Path]:
    unit_stem = f"validate-owner-checkout-{attempt.lower()}"
    handle = state_root / "ignored/validate/runs" / f"{unit_stem}.json"
    log = owner_state_dir / "logs" / f"{attempt}-validate-run.log"
    service_result = handle.with_name(f"{unit_stem}.service-result.json")
    artifacts = state_root / "ignored/validate/artifacts" / unit_stem
    return unit_stem, handle, log, service_result, artifacts


def relevant_membership(roots: EvidenceRoots) -> dict[str, list[dict[str, object]]]:
    return {
        "owner_results": directory_membership(roots.results),
        "run_handles": directory_membership(roots.handles),
        "owner_logs": directory_membership(roots.logs),
        "validation_artifacts": directory_membership(roots.artifacts),
    }


def corrected_handle(handle: Mapping[str, Any]) -> dict[str, Any]:
    corrected = dict(handle)
    corrected.update(
        state="refused",
        result="systemd-launch-refused",
        exit_code=2,
    )
    return corrected


def verify_candidate(
    *,
    roots: EvidenceRoots,
    result_path: Path,
    result: Mapping[str, Any],
    handle_path: Path,
    handle: Mapping[str, Any],
    log: Path,
    service_result: Path,
    artifacts: Path,
    run: Runner,
) -> tuple[str, bytes]:
    attempt = result_path.stem
    errors: list[str] = []
    if ATTEMPT_RE.fullmatch(attempt) is None or result.get("attempt_id") != attempt:
        errors.append(
            "owner result filename and attempt_id are not the exact owner-watch identity"
        )
    unit_stem = f"validate-owner-checkout-{attempt.lower()}"
    expected_handle = roots.handles / f"{unit_stem}.json"
    expected_log = roots.logs / f"{attempt}-validate-run.log"
    if handle_path != expected_handle:
        errors.append("derived handle path mismatch")
    if log != expected_log:
        errors.append("derived launcher-log path mismatch")
    if handle.get("unit") != f"{unit_stem}.service":
        errors.append("handle unit does not match the owner attempt")
    recorded_handle = result.get("validation_run_record")
    if recorded_handle is not None and recorded_handle != str(handle_path):
        errors.append("owner result points at a different run handle")
    if handle.get("log") != str(log) or result.get("validate_log") != str(log):
        errors.append("handle/result/log binding is not exact")
    target = result.get("sha")
    if not isinstance(target, str) or SHA_RE.fullmatch(target) is None:
        errors.append("owner result sha is not a full lowercase commit id")
    if handle.get("target") != target:
        errors.append("handle target does not match owner result sha")
    if handle.get("repo") != OWNER_REPOSITORY or handle.get("agent") != OWNER_AGENT:
        errors.append("handle repository or agent is not the owner-watch producer")
    if handle.get("checkout") != str(roots.checkout):
        errors.append("handle checkout is not the canonical Hermit checkout")
    if (
        handle.get("executed_nodes") is not None
        or handle.get("executed_tests") is not None
    ):
        errors.append("handle records executed work")
    if "process_identity" in handle:
        errors.append("handle records a process identity")
    if "admission_result" in handle or "run_number" in handle:
        errors.append("handle records admission or a run number")
    if handle.get("observed_safe_ci_cgroups") != []:
        errors.append("handle records a safe-ci cgroup")
    if result.get("validate_run_rc") != 2:
        errors.append("owner result does not record validate-run rc 2")
    if result.get("ledger_row_found") is not False:
        errors.append("owner result does not record ledger_row_found=false")
    lock_admission = result.get("lock_admission")
    if not isinstance(lock_admission, dict) or lock_admission.get("state") != "UNKNOWN":
        errors.append("owner result does not retain UNKNOWN admission")

    refusal = result.get("admission_refusal")
    expected_detail = f"validation log already exists: {log}"
    expected_line = f"validate-run: REFUSED: {expected_detail}"
    if not isinstance(refusal, dict) or refusal.get("kind") != "admission-refusal":
        errors.append("owner result lacks the exact admission-refusal classification")
    elif (
        refusal.get("detail") != expected_detail or refusal.get("line") != expected_line
    ):
        errors.append("owner result refusal does not name its exact log")
    try:
        log_bytes = read_regular_bytes(log, root=roots.logs, role="refusal log")
        log_text = log_bytes.decode("utf-8", errors="replace")
    except CorrectionRefused as error:
        errors.append(str(error))
        log_bytes = b""
        log_text = ""
    if log_text.count(expected_line) != 1:
        errors.append(
            "refusal log does not contain its self-naming refusal exactly once"
        )
    if REFUSED_STATE_LINE not in log_text or REFUSED_REMEDY_LINE not in log_text:
        errors.append("refusal log lacks the standard never-ran terminal lines")
    if ACQUIRED_RE.search(log_text) or DAG_TERMINAL_RE.search(log_text):
        errors.append(
            "refusal log contains validate-lock acquisition or DAG terminal evidence"
        )

    expected_service_result = handle_path.with_name(f"{unit_stem}.service-result.json")
    expected_artifacts = roots.artifacts / unit_stem
    try:
        child(service_result, parent=roots.handles, role="service result")
        child(artifacts, parent=roots.artifacts, role="artifact root")
    except CorrectionRefused as error:
        errors.append(str(error))
    if (
        service_result != expected_service_result
        or service_result.exists()
        or service_result.is_symlink()
    ):
        errors.append("service-result path is mismatched or exists")
    if artifacts != expected_artifacts or artifacts.exists() or artifacts.is_symlink():
        errors.append("artifact root is mismatched or exists")
    if handle.get("e2e_result_root") != str(expected_artifacts / "e2e"):
        errors.append("handle e2e result root does not match the derived artifact root")
    if handle.get("safe_ci_dag_runner_log_dir") != str(
        expected_artifacts / "safe-ci-dag-runner"
    ):
        errors.append("handle DAG log root does not match the derived artifact root")
    cargo_home = handle.get("cargo_home")
    if isinstance(cargo_home, str):
        try:
            cargo_path = descendant(
                Path(cargo_home), root=roots.state_root, role="private Cargo home"
            )
            if cargo_path.exists() or cargo_path.is_symlink():
                errors.append("recorded private Cargo home still exists")
        except CorrectionRefused as error:
            errors.append(str(error))
    if not unit_is_absent(str(handle.get("unit", "")), run=run):
        errors.append("recorded systemd unit still exists")

    state = handle.get("state")
    if state == "unknown":
        if handle.get("result") != "unknown" or handle.get("exit_code") is not None:
            errors.append("unknown handle is not the exact unexecuted historical shape")
        disposition = "pending"
    elif state == "refused":
        if (
            handle.get("result") != "systemd-launch-refused"
            or handle.get("exit_code") != 2
        ):
            errors.append("refused handle is not the exact corrected shape")
        disposition = "already-corrected"
    else:
        errors.append(f"handle state is {state!r}, expected unknown or refused")
        disposition = "invalid"
    if not isinstance(handle.get("detail"), str) or not handle.get("detail"):
        errors.append("handle detail is absent")
    if not isinstance(handle.get("finished_at"), str) or not handle.get("finished_at"):
        errors.append("handle finished_at is absent")
    if errors:
        raise CorrectionRefused(f"{attempt}: " + "; ".join(errors))
    return disposition, log_bytes


def _build_plan(
    roots: EvidenceRoots,
    *,
    expected_count: int | None,
    run: Runner,
) -> dict[str, Any]:
    records: list[dict[str, Any]] = []
    rejected: list[str] = []
    membership_before = relevant_membership(roots)
    result_names = [
        str(row["name"])
        for row in membership_before["owner_results"]
        if str(row["name"]).endswith(".json")
    ]
    for name in result_names:
        result_path = roots.results / name
        try:
            result, result_bytes = read_object(
                result_path, root=roots.results, role="owner result"
            )
        except CorrectionRefused as error:
            rejected.append(str(error))
            continue
        if not candidate_signal(result):
            continue
        attempt = result_path.stem
        unit_stem, handle_path, log, service_result, artifacts = expected_paths(
            roots.state_root, roots.owner_state_dir, attempt
        )
        try:
            handle, before = read_object(
                handle_path, root=roots.handles, role="run handle"
            )
            disposition, log_bytes = verify_candidate(
                roots=roots,
                result_path=result_path,
                result=result,
                handle_path=handle_path,
                handle=handle,
                log=log,
                service_result=service_result,
                artifacts=artifacts,
                run=run,
            )
        except CorrectionRefused as error:
            rejected.append(str(error))
            continue
        after = (
            before
            if disposition == "already-corrected"
            else canonical_bytes(corrected_handle(handle))
        )
        records.append(
            {
                "attempt_id": attempt,
                "unit": f"{unit_stem}.service",
                "result_path": str(result_path),
                "handle_path": str(handle_path),
                "log_path": str(log),
                "disposition": disposition,
                "result_sha256": sha256(result_bytes),
                "log_sha256": sha256(log_bytes),
                "before_sha256": sha256(before),
                "after_sha256": sha256(after),
            }
        )
    accounted_handles = {Path(row["handle_path"]) for row in records}
    handle_names = [
        str(row["name"])
        for row in membership_before["run_handles"]
        if str(row["name"]).startswith("validate-owner-checkout-")
        and str(row["name"]).endswith(".json")
    ]
    for name in handle_names:
        handle_path = roots.handles / name
        if handle_path in accounted_handles:
            continue
        try:
            handle, _raw = read_object(
                handle_path, root=roots.handles, role="run handle"
            )
        except CorrectionRefused as error:
            rejected.append(str(error))
            continue
        if handle.get("agent") != OWNER_AGENT:
            continue
        match = UNIT_STEM_RE.fullmatch(handle_path.stem)
        if match is None:
            continue
        attempt = f"{match.group('date')}T{match.group('clock')}Z-{match.group('pid')}"
        _unit, _handle, log, _service, _artifacts = expected_paths(
            roots.state_root, roots.owner_state_dir, attempt
        )
        if handle.get("log") != str(log):
            continue
        if (
            handle.get("state") == "unknown"
            and handle.get("result") == "unknown"
            and handle.get("exit_code") is None
            and handle.get("executed_nodes") is None
            and handle.get("executed_tests") is None
            and "process_identity" not in handle
            and "admission_result" not in handle
            and "run_number" not in handle
        ):
            rejected.append(
                f"{attempt}: unexecuted owner-watch handle has no matching owner result"
            )
    membership_after = relevant_membership(roots)
    if membership_after != membership_before:
        rejected.append("evidence-directory membership changed while the plan was read")
    if rejected:
        raise CorrectionRefused(
            "candidate evidence mismatch; no records may be changed:\n  "
            + "\n  ".join(rejected)
        )
    if expected_count is not None and len(records) != expected_count:
        raise CorrectionRefused(
            f"expected exactly {expected_count} proven owner-watch log-collision records, "
            f"found {len(records)}"
        )
    planned_count = len(records) if expected_count is None else expected_count
    body = {
        "schema_version": SCHEMA_VERSION,
        "state_root": str(roots.state_root),
        "owner_state_dir": str(roots.owner_state_dir),
        "expected_count": planned_count,
        "matched_count": len(records),
        "pending_count": sum(row["disposition"] == "pending" for row in records),
        "already_corrected_count": sum(
            row["disposition"] == "already-corrected" for row in records
        ),
        "directory_membership": membership_before,
        "records": records,
    }
    body["plan_sha256"] = plan_sha256(body)
    return body


def build_plan(
    state_root: Path,
    owner_state_dir: Path,
    *,
    expected_count: int | None,
    run: Runner = subprocess.run,
) -> dict[str, Any]:
    roots = load_roots(state_root, owner_state_dir)
    with run_registry.owner_watch_population_lock(roots.owner_state_dir):
        recover_pending_transaction(roots)
        return _build_plan(roots, expected_count=expected_count, run=run)


def open_directory(path: Path) -> int:
    expected = canonical_directory(path, role="bound directory")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor: int | None = None
    try:
        descriptor = os.open(expected, flags)
        opened = os.fstat(descriptor)
        current = os.stat(expected, follow_symlinks=False)
        bound = Path(f"/proc/self/fd/{descriptor}").resolve(strict=True)
    except OSError as error:
        if descriptor is not None:
            os.close(descriptor)
        raise CorrectionRefused(f"cannot bind directory {expected}: {error}") from error
    if bound != expected or (opened.st_dev, opened.st_ino) != (
        current.st_dev,
        current.st_ino,
    ):
        os.close(descriptor)
        raise CorrectionRefused(f"directory changed while it was bound: {expected}")
    return descriptor


def read_child(descriptor: int, name: str, *, role: str) -> tuple[bytes, int]:
    if Path(name).name != name:
        raise CorrectionRefused(f"{role} name is not a basename: {name}")
    flags = os.O_RDONLY | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    child_fd: int | None = None
    try:
        child_fd = os.open(name, flags, dir_fd=descriptor)
        opened = os.fstat(child_fd)
        if not stat.S_ISREG(opened.st_mode):
            raise CorrectionRefused(f"{role} {name} is not a regular file")
        with os.fdopen(child_fd, "rb") as stream:
            child_fd = None
            data = stream.read()
        current = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
    except CorrectionRefused:
        if child_fd is not None:
            os.close(child_fd)
        raise
    except OSError as error:
        if child_fd is not None:
            os.close(child_fd)
        raise CorrectionRefused(f"cannot read {role} {name}: {error}") from error
    if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
        raise CorrectionRefused(f"{role} {name} changed while it was read")
    return data, stat.S_IMODE(opened.st_mode)


def write_child(descriptor: int, name: str, data: bytes, mode: int) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    child_fd: int | None = None
    try:
        child_fd = os.open(name, flags, mode, dir_fd=descriptor)
        os.fchmod(child_fd, mode)
        with os.fdopen(child_fd, "wb") as stream:
            child_fd = None
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        if child_fd is not None:
            os.close(child_fd)
        try:
            os.unlink(name, dir_fd=descriptor)
        except OSError:
            pass
        raise


def replace_child(
    source_descriptor: int,
    source_name: str,
    destination_descriptor: int,
    destination_name: str,
) -> None:
    os.replace(
        source_name,
        destination_name,
        src_dir_fd=source_descriptor,
        dst_dir_fd=destination_descriptor,
    )


def entry_exists(descriptor: int, name: str) -> bool:
    if Path(name).name != name:
        raise CorrectionRefused(f"transaction entry is not a basename: {name}")
    try:
        metadata = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
    except FileNotFoundError:
        return False
    except OSError as error:
        raise CorrectionRefused(f"cannot inspect transaction entry {name}: {error}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise CorrectionRefused(f"transaction entry {name} is not a regular file")
    return True


def write_or_verify_child(
    descriptor: int, name: str, data: bytes, mode: int
) -> None:
    if entry_exists(descriptor, name):
        existing, existing_mode = read_child(
            descriptor, name, role="transaction entry"
        )
        if existing != data or existing_mode != mode:
            raise CorrectionRefused(
                f"transaction entry {name} differs from its durable manifest"
            )
        return
    write_child(descriptor, name, data, mode)


def encoded(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def decoded(item: Mapping[str, Any], field: str, expected_hash: object) -> bytes:
    value = item.get(field)
    if not isinstance(value, str):
        raise CorrectionRefused(f"transaction {field} is absent")
    try:
        data = base64.b64decode(value, validate=True)
    except (ValueError, TypeError) as error:
        raise CorrectionRefused(f"transaction {field} is not base64: {error}") from error
    if not isinstance(expected_hash, str) or sha256(data) != expected_hash:
        raise CorrectionRefused(f"transaction {field} does not match its hash")
    return data


def manifest_sha256(value: Mapping[str, Any]) -> str:
    body = dict(value)
    body.pop("manifest_sha256", None)
    return sha256(json.dumps(body, sort_keys=True, separators=(",", ":")).encode())


def prepare_manifest(
    roots: EvidenceRoots,
    plan: Mapping[str, Any],
    pending: Sequence[Mapping[str, Any]],
    *,
    handles_fd: int,
) -> dict[str, Any]:
    records: list[dict[str, Any]] = []
    for index, row in enumerate(pending):
        path = child(
            Path(str(row["handle_path"])),
            parent=roots.handles,
            role="run handle",
        )
        before, mode = read_child(handles_fd, path.name, role="run handle")
        if sha256(before) != row["before_sha256"]:
            raise CorrectionRefused(
                f"run handle changed after locked verification: {path}"
            )
        try:
            value = json.loads(before)
        except json.JSONDecodeError as error:
            raise CorrectionRefused(
                f"locked run handle is no longer JSON: {path}: {error}"
            ) from error
        if not isinstance(value, dict):
            raise CorrectionRefused(f"locked run handle is not an object: {path}")
        after = canonical_bytes(corrected_handle(value))
        if sha256(after) != row["after_sha256"]:
            raise CorrectionRefused(f"prepared correction hash differs for {path}")
        records.append(
            {
                "handle_path": str(path),
                "handle_name": path.name,
                "mode": mode,
                "original_name": f"original-{index}.json",
                "replacement_name": f"replacement-{index}.json",
                "original_sha256": row["before_sha256"],
                "replacement_sha256": row["after_sha256"],
                "original_bytes_base64": encoded(before),
                "replacement_bytes_base64": encoded(after),
            }
        )
    body: dict[str, Any] = {
        "schema_version": TRANSACTION_SCHEMA_VERSION,
        "state": "prepared",
        "state_root": str(roots.state_root),
        "owner_state_dir": str(roots.owner_state_dir),
        "plan_sha256": plan["plan_sha256"],
        "plan": dict(plan),
        "records": records,
    }
    body["manifest_sha256"] = manifest_sha256(body)
    return body


def validate_manifest(
    roots: EvidenceRoots, value: Mapping[str, Any]
) -> list[dict[str, Any]]:
    if value.get("schema_version") != TRANSACTION_SCHEMA_VERSION:
        raise CorrectionRefused("correction transaction has an unsupported schema")
    if value.get("state") != "prepared":
        raise CorrectionRefused("correction transaction is not in prepared state")
    if value.get("state_root") != str(roots.state_root) or value.get(
        "owner_state_dir"
    ) != str(roots.owner_state_dir):
        raise CorrectionRefused("correction transaction belongs to different roots")
    if value.get("manifest_sha256") != manifest_sha256(value):
        raise CorrectionRefused("correction transaction manifest hash differs")
    plan = value.get("plan")
    if not isinstance(plan, dict) or plan.get("plan_sha256") != plan_sha256(plan):
        raise CorrectionRefused("correction transaction plan hash differs")
    if value.get("plan_sha256") != plan.get("plan_sha256"):
        raise CorrectionRefused("correction transaction names a different plan")
    planned = {
        str(row.get("handle_path")): row
        for row in plan.get("records", [])
        if isinstance(row, dict) and row.get("disposition") == "pending"
    }
    records = value.get("records")
    if (
        not isinstance(records, list)
        or not records
        or len(records) != plan.get("pending_count")
        or len(planned) != len(records)
    ):
        raise CorrectionRefused("correction transaction candidate set differs")
    validated: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, untyped in enumerate(records):
        if not isinstance(untyped, dict):
            raise CorrectionRefused("correction transaction record is not an object")
        item = dict(untyped)
        path = child(
            Path(str(item.get("handle_path", ""))),
            parent=roots.handles,
            role="transaction run handle",
        )
        name = item.get("handle_name")
        if not isinstance(name, str) or name != path.name or name in seen:
            raise CorrectionRefused("correction transaction handle identity differs")
        seen.add(str(name))
        row = planned.get(str(path))
        if (
            row is None
            or item.get("original_sha256") != row.get("before_sha256")
            or item.get("replacement_sha256") != row.get("after_sha256")
            or item.get("original_name") != f"original-{index}.json"
            or item.get("replacement_name") != f"replacement-{index}.json"
        ):
            raise CorrectionRefused(
                f"correction transaction record differs from its plan: {path}"
            )
        mode = item.get("mode")
        if not isinstance(mode, int) or isinstance(mode, bool) or not 0 <= mode <= 0o777:
            raise CorrectionRefused(f"correction transaction mode is invalid: {path}")
        item["original_bytes"] = decoded(
            item, "original_bytes_base64", item.get("original_sha256")
        )
        item["replacement_bytes"] = decoded(
            item, "replacement_bytes_base64", item.get("replacement_sha256")
        )
        validated.append(item)
    return validated


def transaction_members(descriptor: int) -> list[str]:
    try:
        os.lseek(descriptor, 0, os.SEEK_SET)
        names = sorted(os.listdir(descriptor))
    except OSError as error:
        raise CorrectionRefused(f"cannot list correction transaction: {error}") from error
    for name in names:
        if not entry_exists(descriptor, name):
            raise CorrectionRefused(f"correction transaction entry disappeared: {name}")
    return names


def clear_transaction(roots: EvidenceRoots, transaction_fd: int) -> None:
    members = transaction_members(transaction_fd)
    markers = {
        TRANSACTION_PREPARING,
        TRANSACTION_COMMITTED,
        TRANSACTION_MANIFEST,
    }
    ordered = [name for name in members if name not in markers]
    ordered.extend(
        name
        for name in (
            TRANSACTION_PREPARING,
            TRANSACTION_COMMITTED,
            TRANSACTION_MANIFEST,
        )
        if name in members
    )
    for name in ordered:
        os.unlink(name, dir_fd=transaction_fd)
    os.fsync(transaction_fd)
    parent_fd = open_directory(roots.transaction_parent)
    try:
        os.rmdir(transaction_path(roots).name, dir_fd=parent_fd)
        os.fsync(parent_fd)
    finally:
        os.close(parent_fd)


def create_transaction(
    roots: EvidenceRoots, manifest: Mapping[str, Any]
) -> int:
    parent_fd = open_directory(roots.transaction_parent)
    try:
        os.mkdir(
            transaction_path(roots).name,
            mode=0o700,
            dir_fd=parent_fd,
        )
        os.fsync(parent_fd)
    except FileExistsError as error:
        raise CorrectionRefused(
            f"correction transaction already exists: {transaction_path(roots)}"
        ) from error
    finally:
        os.close(parent_fd)
    transaction_fd = open_directory(transaction_path(roots))
    try:
        records = validate_manifest(roots, manifest)
        for item in records:
            write_child(
                transaction_fd,
                str(item["original_name"]),
                item["original_bytes"],
                int(item["mode"]),
            )
            write_child(
                transaction_fd,
                str(item["replacement_name"]),
                item["replacement_bytes"],
                int(item["mode"]),
            )
        os.fsync(transaction_fd)
        manifest_bytes = canonical_bytes(manifest)
        write_child(transaction_fd, TRANSACTION_PREPARING, manifest_bytes, 0o600)
        os.fsync(transaction_fd)
        replace_child(
            transaction_fd,
            TRANSACTION_PREPARING,
            transaction_fd,
            TRANSACTION_MANIFEST,
        )
        os.fsync(transaction_fd)
        return transaction_fd
    except BaseException:
        os.close(transaction_fd)
        raise


def load_manifest(
    roots: EvidenceRoots, transaction_fd: int
) -> tuple[dict[str, Any], list[dict[str, Any]], bytes]:
    raw, _mode = read_child(
        transaction_fd, TRANSACTION_MANIFEST, role="correction transaction manifest"
    )
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise CorrectionRefused(
            f"correction transaction manifest is malformed: {error}"
        ) from error
    if not isinstance(value, dict):
        raise CorrectionRefused("correction transaction manifest is not an object")
    return value, validate_manifest(roots, value), raw


def verify_prepared_files(
    transaction_fd: int, records: Sequence[Mapping[str, Any]]
) -> None:
    for item in records:
        for name_field, bytes_field in (
            ("original_name", "original_bytes"),
            ("replacement_name", "replacement_bytes"),
        ):
            data, mode = read_child(
                transaction_fd,
                str(item[name_field]),
                role="prepared transaction file",
            )
            if data != item[bytes_field] or mode != item["mode"]:
                raise CorrectionRefused(
                    f"prepared transaction file differs: {item[name_field]}"
                )


def restore_originals(
    records: Sequence[Mapping[str, Any]],
    *,
    handles_fd: int,
    transaction_fd: int,
    replace: Replacer,
) -> list[str]:
    failures: list[str] = []
    for item in reversed(records):
        name = str(item["handle_name"])
        try:
            current, _mode = read_child(handles_fd, name, role="run handle")
            current_hash = sha256(current)
            if current_hash == item["original_sha256"]:
                continue
            if current_hash != item["replacement_sha256"]:
                raise CorrectionRefused(
                    f"current bytes match neither side of the transaction for {name}"
                )
            original_name = str(item["original_name"])
            write_or_verify_child(
                transaction_fd,
                original_name,
                item["original_bytes"],
                int(item["mode"]),
            )
            replace(transaction_fd, original_name, handles_fd, name)
            os.fsync(handles_fd)
            restored, _mode = read_child(handles_fd, name, role="restored run handle")
            if sha256(restored) != item["original_sha256"]:
                raise CorrectionRefused(f"restored bytes differ for {name}")
        # A second interrupt must not stop restoration of the remaining files.
        except BaseException as error:
            failures.append(f"{name}: {type(error).__name__}: {error}")
    return failures


def cleanup_prepared_transaction(roots: EvidenceRoots, transaction_fd: int) -> None:
    allowed = {TRANSACTION_PREPARING}
    allowed.update(
        name
        for name in transaction_members(transaction_fd)
        if re.fullmatch(r"(?:original|replacement)-\d+\.json", name)
    )
    unexpected = set(transaction_members(transaction_fd)) - allowed
    if unexpected:
        raise CorrectionRefused(
            "manifest-free correction transaction has unexpected entries: "
            + ", ".join(sorted(unexpected))
        )
    clear_transaction(roots, transaction_fd)


def recover_pending_transaction(
    roots: EvidenceRoots, *, replace: Replacer = replace_child
) -> bool:
    path = transaction_path(roots)
    try:
        os.lstat(path)
    except FileNotFoundError:
        return False
    except OSError as error:
        raise CorrectionRefused(f"cannot inspect correction transaction {path}: {error}") from error
    transaction_fd = open_directory(path)
    try:
        members = transaction_members(transaction_fd)
        if TRANSACTION_MANIFEST not in members:
            cleanup_prepared_transaction(roots, transaction_fd)
            return True
        _manifest, records, raw = load_manifest(roots, transaction_fd)
        with ExitStack() as locks:
            for item in records:
                locks.enter_context(
                    run_registry.exclusive_record(Path(str(item["handle_path"])))
                )
            handles_fd = open_directory(roots.handles)
            try:
                if TRANSACTION_COMMITTED in members:
                    committed_raw, _mode = read_child(
                        transaction_fd,
                        TRANSACTION_COMMITTED,
                        role="correction transaction commit marker",
                    )
                    try:
                        committed = json.loads(committed_raw)
                    except json.JSONDecodeError as error:
                        raise CorrectionRefused(
                            f"correction transaction commit marker is malformed: {error}"
                        ) from error
                    if not isinstance(committed, dict) or committed != {
                        "manifest_sha256": sha256(raw),
                        "schema_version": TRANSACTION_SCHEMA_VERSION,
                        "state": "committed",
                    }:
                        raise CorrectionRefused(
                            "correction transaction commit marker differs from manifest"
                        )
                    for item in records:
                        current, _mode = read_child(
                            handles_fd,
                            str(item["handle_name"]),
                            role="committed run handle",
                        )
                        if sha256(current) != item["replacement_sha256"]:
                            raise CorrectionRefused(
                                "committed correction transaction has a mismatched handle: "
                                f"{item['handle_path']}"
                            )
                else:
                    failures = restore_originals(
                        records,
                        handles_fd=handles_fd,
                        transaction_fd=transaction_fd,
                        replace=replace,
                    )
                    if failures:
                        raise CorrectionRefused(
                            "correction transaction recovery failed; durable evidence retained: "
                            + "; ".join(failures)
                        )
                clear_transaction(roots, transaction_fd)
            finally:
                os.close(handles_fd)
        return True
    finally:
        os.close(transaction_fd)


def apply_plan(
    state_root: Path,
    owner_state_dir: Path,
    *,
    expected_count: int,
    expected_plan_sha256: str,
    run: Runner = subprocess.run,
    before_commit: Callable[[], None] | None = None,
    replace: Replacer = replace_child,
) -> dict[str, Any]:
    roots = load_roots(state_root, owner_state_dir)
    with run_registry.owner_watch_population_lock(roots.owner_state_dir):
        recover_pending_transaction(roots, replace=replace)
        initial = _build_plan(roots, expected_count=expected_count, run=run)
        if initial["plan_sha256"] != expected_plan_sha256:
            raise CorrectionRefused(
                "--expect-plan-sha256 does not match the current dry-run plan; run the "
                "dry run again and review the new plan"
            )
        pending = [
            row for row in initial["records"] if row["disposition"] == "pending"
        ]
        if not pending:
            initial["applied_count"] = 0
            initial["applied_plan_sha256"] = expected_plan_sha256
            initial["changes"] = []
            return initial
        with ExitStack() as locks:
            for row in initial["records"]:
                locks.enter_context(
                    run_registry.exclusive_record(Path(row["handle_path"]))
                )
            locked = _build_plan(roots, expected_count=expected_count, run=run)
            if locked["plan_sha256"] != expected_plan_sha256:
                raise CorrectionRefused(
                    "correction evidence changed while acquiring run-handle locks; "
                    "no records changed"
                )
            handles_fd = open_directory(roots.handles)
            transaction_fd: int | None = None
            committed = False
            try:
                manifest = prepare_manifest(
                    roots, locked, pending, handles_fd=handles_fd
                )
                transaction_fd = create_transaction(roots, manifest)
                _manifest, prepared, manifest_raw = load_manifest(
                    roots, transaction_fd
                )
                if before_commit is not None:
                    before_commit()
                final_preflight = _build_plan(
                    roots, expected_count=expected_count, run=run
                )
                if final_preflight["plan_sha256"] != expected_plan_sha256:
                    raise CorrectionRefused(
                        "correction evidence or directory membership changed before "
                        "mutation; no records changed"
                    )
                verify_prepared_files(transaction_fd, prepared)
                for item in prepared:
                    name = str(item["handle_name"])
                    current, _mode = read_child(handles_fd, name, role="run handle")
                    if sha256(current) != item["original_sha256"]:
                        raise CorrectionRefused(
                            f"run handle changed before replacement: {name}"
                        )
                    replacement_name = str(item["replacement_name"])
                    write_or_verify_child(
                        transaction_fd,
                        replacement_name,
                        item["replacement_bytes"],
                        int(item["mode"]),
                    )
                    replace(transaction_fd, replacement_name, handles_fd, name)
                    os.fsync(handles_fd)
                    after, _mode = read_child(
                        handles_fd, name, role="corrected run handle"
                    )
                    if sha256(after) != item["replacement_sha256"]:
                        raise CorrectionRefused(
                            f"atomic correction readback hash differs for {name}"
                        )
                final = _build_plan(roots, expected_count=expected_count, run=run)
                if final["pending_count"] != 0:
                    raise CorrectionRefused(
                        f"correction left {final['pending_count']} pending run handles"
                    )
                final_by_path = {
                    str(row["handle_path"]): row for row in final["records"]
                }
                for row in pending:
                    corrected = final_by_path.get(str(row["handle_path"]))
                    if (
                        corrected is None
                        or corrected["before_sha256"] != row["after_sha256"]
                        or corrected["disposition"] != "already-corrected"
                    ):
                        raise CorrectionRefused(
                            f"final correction verification differs for {row['handle_path']}"
                        )
                marker = canonical_bytes(
                    {
                        "manifest_sha256": sha256(manifest_raw),
                        "schema_version": TRANSACTION_SCHEMA_VERSION,
                        "state": "committed",
                    }
                )
                write_child(transaction_fd, TRANSACTION_COMMITTED, marker, 0o600)
                os.fsync(transaction_fd)
                committed = True
                clear_transaction(roots, transaction_fd)
            except BaseException as error:
                rollback_failures: list[str] = []
                if transaction_fd is not None and not committed:
                    try:
                        _manifest, prepared, _raw = load_manifest(
                            roots, transaction_fd
                        )
                        rollback_failures = restore_originals(
                            prepared,
                            handles_fd=handles_fd,
                            transaction_fd=transaction_fd,
                            replace=replace,
                        )
                        if not rollback_failures:
                            clear_transaction(roots, transaction_fd)
                    except BaseException as rollback_error:
                        rollback_failures.append(
                            f"transaction recovery: {type(rollback_error).__name__}: "
                            f"{rollback_error}"
                        )
                if rollback_failures:
                    raise CorrectionRefused(
                        f"batch correction failed ({error}); ROLLBACK FAILED; durable "
                        "recovery evidence retained: " + "; ".join(rollback_failures)
                    ) from error
                if not isinstance(error, Exception):
                    raise
                if committed:
                    raise CorrectionRefused(
                        "correction committed but cleanup is pending; readers remain "
                        f"unavailable until recovery: {error}"
                    ) from error
                raise CorrectionRefused(
                    f"batch correction failed; exact originals restored: {error}"
                ) from error
            finally:
                if transaction_fd is not None:
                    os.close(transaction_fd)
                os.close(handles_fd)
        final["applied_count"] = len(pending)
        final["applied_plan_sha256"] = expected_plan_sha256
        final["changes"] = pending
        return final


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument(
        "--state-root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="canonical dev-hermit state root",
    )
    result.add_argument(
        "--owner-state-dir",
        type=Path,
        default=Path.home() / ".local/state/hermit-owner-validate",
    )
    result.add_argument(
        "--expected-count",
        type=int,
        help="exact matched count from the reviewed dry run; required with --apply",
    )
    result.add_argument(
        "--apply",
        action="store_true",
        help="atomically rewrite proven handles; omitted means read-only dry run",
    )
    result.add_argument(
        "--expect-plan-sha256",
        help="required with --apply; exact plan_sha256 from a reviewed dry run",
    )
    return result


def main(argv: Sequence[str] | None = None, *, run: Runner = subprocess.run) -> int:
    args = parser().parse_args(argv)
    if args.expected_count is not None and args.expected_count <= 0:
        print(
            "owner-watch-handle-correction: REFUSED: --expected-count must be positive",
            file=sys.stderr,
        )
        return 2
    if args.apply and args.expected_count is None:
        print(
            "owner-watch-handle-correction: REFUSED: --apply requires the exact "
            "--expected-count printed by a prior dry run",
            file=sys.stderr,
        )
        return 2
    if args.apply and not args.expect_plan_sha256:
        print(
            "owner-watch-handle-correction: REFUSED: --apply requires the "
            "--expect-plan-sha256 printed by a prior dry run",
            file=sys.stderr,
        )
        return 2
    if not args.apply and args.expect_plan_sha256:
        print(
            "owner-watch-handle-correction: REFUSED: --expect-plan-sha256 is "
            "valid only with --apply",
            file=sys.stderr,
        )
        return 2
    try:
        report = (
            apply_plan(
                args.state_root,
                args.owner_state_dir,
                expected_count=int(args.expected_count),
                expected_plan_sha256=args.expect_plan_sha256,
                run=run,
            )
            if args.apply
            else build_plan(
                args.state_root,
                args.owner_state_dir,
                expected_count=args.expected_count,
                run=run,
            )
        )
    except CorrectionRefused as error:
        print(f"owner-watch-handle-correction: REFUSED: {error}", file=sys.stderr)
        return 2
    report = dict(report)
    report["action"] = "applied" if args.apply else "dry-run"
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
