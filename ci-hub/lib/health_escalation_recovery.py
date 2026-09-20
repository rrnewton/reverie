#!/usr/bin/env python3
"""Hash-bound, non-publishing recovery for legacy health escalation queues."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import re
import secrets
import shlex
import stat
from pathlib import Path
from typing import Any, Callable

from health_escalation_event import (  # noqa: E402
    DEFAULT_DIGEST_IGNORE,
    EVENT_RE,
    InvalidEvent,
    normalized_sha256,
    validate_event,
)


MANIFEST_SCHEMA = "hermit-health-escalation-legacy-recovery/v1"
TRANSACTION_SCHEMA = "hermit-health-escalation-legacy-recovery-transaction/v1"
LEGACY_EVENT_RE = re.compile(r"^(event-.+)\.(alarm|resolve)$")
LEGACY_ACK_RE = re.compile(r"^(event-.+\.(?:alarm|resolve))\.(taskgraph|github)\.done$")
V1_ACK_RE = re.compile(
    r"^(event-[0-9]{8}T[0-9]{12}Z-[0-9]+-[A-Za-z0-9]{8}\.json)"
    r"\.(taskgraph|github)\.done$"
)
V1_CIRCUIT_RE = re.compile(
    r"^(event-[0-9]{8}T[0-9]{12}Z-[0-9]+-[A-Za-z0-9]{8}\.json)"
    r"\.(taskgraph|github)\.circuit\.json$"
)
RECOVERY_SENTINEL_RE = re.compile(r"^event-legacy-recovery-([0-9a-f]{16})\.alarm$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
SINKS = ("taskgraph", "github")
SOURCE_RECORD_KEYS = ("ctime_ns", "filename", "mtime_ns", "mode", "sha256", "size")
ARCHIVE_RECORD_KEYS = ("filename", "mode", "sha256", "size")


class RecoveryRefused(RuntimeError):
    """The queue or manifest cannot safely be recovered."""


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def canonical_json(value: Any) -> bytes:
    return (
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    ).encode("utf-8")


def fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def directory_identity(path: Path) -> dict[str, Any]:
    try:
        metadata = path.stat(follow_symlinks=False)
    except OSError as error:
        raise RecoveryRefused(f"cannot stat directory {path}: {error}") from error
    if not stat.S_ISDIR(metadata.st_mode):
        raise RecoveryRefused(
            f"expected a real directory, found another file type: {path}"
        )
    return {
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
    }


def require_directory_identity(path: Path, expected: dict[str, Any]) -> None:
    observed = directory_identity(path)
    wanted = {key: expected[key] for key in ("device", "inode", "mode")}
    if observed != wanted:
        raise RecoveryRefused(
            f"directory identity changed for {path}: expected={wanted} observed={observed}"
        )


def require_held_lock(lock: Any, lock_path: Path) -> None:
    held = os.fstat(lock.fileno())
    try:
        current = lock_path.stat(follow_symlinks=False)
    except OSError as error:
        raise RecoveryRefused(f"queue lock path changed: {error}") from error
    if (
        not stat.S_ISREG(held.st_mode)
        or not stat.S_ISREG(current.st_mode)
        or (held.st_dev, held.st_ino) != (current.st_dev, current.st_ino)
    ):
        raise RecoveryRefused("held .worker.lock no longer names the queue lock path")


def path_entry_exists(path: Path) -> bool:
    """Existence without following the final component's symlink."""
    return path.exists() or path.is_symlink()


def atomic_write(path: Path, content: bytes, *, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.parent / f".{path.name}.{os.getpid()}.{secrets.token_hex(6)}.tmp"
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
        mode,
    )
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        fsync_directory(path.parent)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def _stable_stat(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_nlink,
        metadata.st_uid,
        metadata.st_gid,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def read_regular_file(path: Path) -> tuple[bytes, dict[str, Any]]:
    """Read one queue member without following links and detect concurrent writes."""
    try:
        before_path = path.stat(follow_symlinks=False)
    except FileNotFoundError as error:
        raise RecoveryRefused(
            f"queue member disappeared while being read: {path.name}"
        ) from error
    if not stat.S_ISREG(before_path.st_mode):
        raise RecoveryRefused(
            f"queue member is not a regular file and will not be followed: {path.name}"
        )
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise RecoveryRefused(
            f"cannot open queue member safely: {path.name}: {error}"
        ) from error
    try:
        held_before = os.fstat(descriptor)
        if _stable_stat(held_before) != _stable_stat(before_path):
            raise RecoveryRefused(
                f"queue member changed before it was read: {path.name}"
            )
        chunks: list[bytes] = []
        observed = 0
        while True:
            block = os.read(descriptor, 1024 * 1024)
            if not block:
                break
            chunks.append(block)
            observed += len(block)
        held_after = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    try:
        after_path = path.stat(follow_symlinks=False)
    except FileNotFoundError as error:
        raise RecoveryRefused(
            f"queue member disappeared while being read: {path.name}"
        ) from error
    if (
        observed != held_before.st_size
        or _stable_stat(held_after) != _stable_stat(held_before)
        or _stable_stat(after_path) != _stable_stat(held_before)
    ):
        raise RecoveryRefused(f"queue member changed while it was read: {path.name}")
    content = b"".join(chunks)
    return content, {
        "ctime_ns": held_before.st_ctime_ns,
        "filename": path.name,
        "mtime_ns": held_before.st_mtime_ns,
        "mode": f"{stat.S_IMODE(held_before.st_mode):04o}",
        "sha256": sha256_bytes(content),
        "size": len(content),
    }


def inventory(queue: Path) -> tuple[dict[str, bytes], dict[str, dict[str, Any]]]:
    try:
        names = sorted((entry.name for entry in os.scandir(queue)), key=os.fsencode)
    except OSError as error:
        raise RecoveryRefused(f"cannot list queue {queue}: {error}") from error
    contents: dict[str, bytes] = {}
    records: dict[str, dict[str, Any]] = {}
    for name in names:
        try:
            name.encode("utf-8")
        except UnicodeEncodeError as error:
            raise RecoveryRefused("queue filenames must be valid UTF-8") from error
        content, record = read_regular_file(queue / name)
        contents[name] = content
        records[name] = record
    try:
        after = sorted((entry.name for entry in os.scandir(queue)), key=os.fsencode)
    except OSError as error:
        raise RecoveryRefused(f"cannot relist queue {queue}: {error}") from error
    if names != after:
        raise RecoveryRefused("queue population changed while it was inventoried")
    return contents, records


def _attribution() -> dict[str, Any]:
    return {
        "confidence": "unknown",
        "origin_host": None,
        "reason": (
            "legacy payload-only filenames and acknowledgements do not bind an origin host"
        ),
    }


def _population_record(
    record: dict[str, Any], *, kind: str, disposition: str, anomalies: list[str]
) -> dict[str, Any]:
    return {
        **record,
        "anomalies": sorted(anomalies),
        "disposition": disposition,
        "kind": kind,
    }


def _ack_record(
    primary: str,
    sink: str,
    records: dict[str, dict[str, Any]],
    attempts: dict[tuple[str, str], list[dict[str, Any]]],
) -> dict[str, Any]:
    filename = f"{primary}.{sink}.done"
    if filename not in records:
        return {
            "correlated_attempts": attempts.get((primary, sink), []),
            "delivery_state": "uncertain",
            "expected_filename": filename,
            "sidecar_state": "absent",
        }
    return {
        "correlated_attempts": attempts.get((primary, sink), []),
        "delivery_state": "acknowledged-by-sidecar",
        "evidence": records[filename],
        "sidecar_state": "present",
    }


def _tool_record(path: Path) -> dict[str, Any]:
    content, record = read_regular_file(path)
    return {
        "path": str(path.resolve(strict=True)),
        "sha256": sha256_bytes(content),
        "size": record["size"],
    }


def capture_source_log(path: Path) -> tuple[dict[str, Any], bytes]:
    path = path.resolve(strict=True)
    content, record = read_regular_file(path)
    archive_name = (
        f"source-{sha256_bytes(str(path).encode('utf-8'))[:12]}-"
        f"{record['sha256']}.log"
    )
    return (
        {
            "archive_name": archive_name,
            "ctime_ns": record["ctime_ns"],
            "mode": record["mode"],
            "mtime_ns": record["mtime_ns"],
            "path": str(path),
            "sha256": record["sha256"],
            "size": record["size"],
        },
        content,
    )


def load_source_log_prefix(record: dict[str, Any]) -> tuple[dict[str, Any], bytes]:
    required = {
        "archive_name",
        "ctime_ns",
        "mode",
        "mtime_ns",
        "path",
        "sha256",
        "size",
    }
    if not isinstance(record, dict) or set(record) != required:
        raise RecoveryRefused("source-log record has unexpected fields")
    if (
        not isinstance(record["archive_name"], str)
        or Path(record["archive_name"]).name != record["archive_name"]
        or record["archive_name"] in {"", ".", ".."}
        or not isinstance(record["path"], str)
        or not isinstance(record["sha256"], str)
        or not SHA_RE.fullmatch(record["sha256"])
        or isinstance(record["size"], bool)
        or not isinstance(record["size"], int)
        or record["size"] < 0
    ):
        raise RecoveryRefused(f"source-log record is invalid: {record!r}")
    path = Path(record["path"])
    if not path.is_absolute():
        raise RecoveryRefused(f"source-log path is not absolute: {path}")
    try:
        metadata = path.stat(follow_symlinks=False)
    except OSError as error:
        raise RecoveryRefused(f"cannot restat source log {path}: {error}") from error
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size < record["size"]:
        raise RecoveryRefused(f"source log was replaced or truncated: {path}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(path, flags)
    try:
        remaining = record["size"]
        chunks: list[bytes] = []
        while remaining:
            block = os.read(descriptor, min(1024 * 1024, remaining))
            if not block:
                raise RecoveryRefused(f"source log was truncated while read: {path}")
            chunks.append(block)
            remaining -= len(block)
    finally:
        os.close(descriptor)
    content = b"".join(chunks)
    if sha256_bytes(content) != record["sha256"]:
        raise RecoveryRefused(f"source-log reviewed prefix changed: {path}")
    return record, content


def _key_values(tokens: list[str]) -> dict[str, str]:
    values: dict[str, str] = {}
    for token in tokens:
        if "=" in token:
            key, value = token.split("=", 1)
            values[key] = value
    return values


def correlate_attempts(
    *,
    queue: Path,
    legacy_names: set[str],
    source_log_snapshots: list[tuple[dict[str, Any], bytes]],
) -> dict[tuple[str, str], list[dict[str, Any]]]:
    attempts: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for source_record, content in source_log_snapshots:
        offset = 0
        for raw_line in content.splitlines(keepends=True):
            line_offset = offset
            offset += len(raw_line)
            try:
                line = raw_line.decode("utf-8")
                tokens = shlex.split(line)
            except (UnicodeDecodeError, ValueError):
                continue
            if not tokens:
                continue
            outer = _key_values(tokens[1:])
            event_value = outer.get("event")
            sink = outer.get("sink")
            if event_value is None or sink not in SINKS:
                continue
            event_path = Path(event_value)
            if event_path.parent != queue or event_path.name not in legacy_names:
                continue
            detail = outer.get("detail", "")
            try:
                detail_values = _key_values(shlex.split(detail))
            except ValueError:
                detail_values = {}
            returncode: int | None
            findings: int | None
            try:
                returncode = int(outer["escalation_rc"])
            except (KeyError, ValueError):
                returncode = None
            try:
                findings = int(outer["findings"])
            except (KeyError, ValueError):
                findings = None
            attempt = {
                "database": detail_values.get("database"),
                "digest": detail_values.get("digest"),
                "findings": findings,
                "issue_url": detail_values.get("issue"),
                "key": detail_values.get("key"),
                "outcome": detail_values.get("outcome"),
                "returncode": returncode,
                "source_line_length": len(raw_line),
                "source_line_offset": line_offset,
                "source_line_sha256": sha256_bytes(raw_line),
                "source_log": source_record["path"],
                "taskgraph_row": detail_values.get("queue-row"),
                "timestamp": tokens[0],
            }
            attempts.setdefault((event_path.name, sink), []).append(attempt)
    return attempts


def build_manifest(
    queue: Path,
    *,
    source_log_snapshots: list[tuple[dict[str, Any], bytes]] | None = None,
) -> dict[str, Any]:
    """Build a deterministic manifest from one locked, stable queue snapshot."""
    queue = queue.resolve(strict=True)
    initial_queue_identity = directory_identity(queue)
    contents, raw_records = inventory(queue)
    source_log_snapshots = source_log_snapshots or []
    if ".worker.lock" not in raw_records:
        raise RecoveryRefused(
            "queue has no .worker.lock; refusing an unlocked recovery plan"
        )

    legacy: dict[str, tuple[str, dict[str, Any]]] = {}
    orphan_legacy_ack_names: list[str] = []
    v1_events: set[str] = set()
    population: dict[str, dict[str, Any]] = {}

    for name, record in raw_records.items():
        if name == ".worker.lock":
            population[name] = _population_record(
                record, kind="worker-lock", disposition="retain", anomalies=[]
            )
            continue
        sentinel = RECOVERY_SENTINEL_RE.fullmatch(name)
        if sentinel:
            raise RecoveryRefused(
                f"unfinished recovery sentinel {name} exists; inspect its transaction before replanning"
            )
        legacy_match = LEGACY_EVENT_RE.fullmatch(name)
        if legacy_match:
            kind = legacy_match.group(2)
            anomalies: list[str] = []
            if kind == "alarm" and record["size"] == 0:
                anomalies.append("empty-alarm-payload")
            if kind == "resolve" and record["size"] != 0:
                anomalies.append("nonempty-resolve-payload")
            legacy[name] = (kind, record)
            population[name] = _population_record(
                record,
                kind=f"legacy-{kind}",
                disposition="archive",
                anomalies=anomalies,
            )
            continue
        ack_match = LEGACY_ACK_RE.fullmatch(name)
        if ack_match:
            primary, sink = ack_match.groups()
            if primary not in raw_records:
                orphan_legacy_ack_names.append(name)
            population[name] = _population_record(
                record,
                kind=f"legacy-{sink}-acknowledgement",
                disposition="archive",
                anomalies=(
                    []
                    if primary in raw_records
                    else ["acknowledgement-without-primary"]
                ),
            )
            continue
        if name.endswith(".json") and EVENT_RE.fullmatch(Path(name).stem):
            try:
                value = json.loads(contents[name].decode("utf-8"))
                validate_event(value, expected_filename=name)
            except (UnicodeError, json.JSONDecodeError, InvalidEvent) as error:
                raise RecoveryRefused(
                    f"schema-v1 event is invalid and cannot be hidden by legacy recovery: {name}: {error}"
                ) from error
            v1_events.add(name)
            population[name] = _population_record(
                record, kind="schema-v1-event", disposition="retain", anomalies=[]
            )
            continue

    # Related schema-v1 records are retained. An orphan is visible as an anomaly
    # rather than silently treated as an acknowledgement for a legacy payload.
    for name, record in raw_records.items():
        if name in population:
            continue
        match = V1_ACK_RE.fullmatch(name) or V1_CIRCUIT_RE.fullmatch(name)
        if match:
            primary = match.group(1)
            anomalies = (
                [] if primary in v1_events else ["schema-v1-sidecar-without-primary"]
            )
            kind = (
                "schema-v1-sidecar"
                if primary in v1_events
                else "orphan-schema-v1-sidecar"
            )
            population[name] = _population_record(
                record, kind=kind, disposition="retain", anomalies=anomalies
            )

    attempts = correlate_attempts(
        queue=queue,
        legacy_names=set(legacy),
        source_log_snapshots=source_log_snapshots,
    )
    legacy_entries: list[dict[str, Any]] = []
    for ordinal, name in enumerate(sorted(legacy, key=os.fsencode), 1):
        kind, record = legacy[name]
        entry_anomalies = list(population[name]["anomalies"])
        payload = contents[name]
        try:
            payload_text = payload.decode("utf-8")
            legacy_digest: str | None = normalized_sha256(
                payload_text, DEFAULT_DIGEST_IGNORE
            )
        except UnicodeDecodeError:
            legacy_digest = None
            entry_anomalies.append("payload-is-not-utf8")
        extra_sidecars = sorted(
            candidate
            for candidate in raw_records
            if candidate.startswith(name + ".")
            and candidate not in {f"{name}.{sink}.done" for sink in SINKS}
        )
        if extra_sidecars:
            entry_anomalies.append("unrecognized-sidecar")
        legacy_entries.append(
            {
                **record,
                "acknowledgements": {
                    sink: _ack_record(name, sink, raw_records, attempts)
                    for sink in SINKS
                },
                "anomalies": sorted(entry_anomalies),
                "attribution": _attribution(),
                "disposition": "archive-preserve-no-replay",
                "extra_sidecars": extra_sidecars,
                "findings_count": sum(1 for line in payload.splitlines() if line),
                "kind": kind,
                "legacy_normalized_sha256": legacy_digest,
                "line_count": len(payload.splitlines()),
                "ordinal": ordinal,
                "replacement_event_id": None,
                "routing_identity": "top-level legacy_routing_identity",
            }
        )

    # Everything left is retained only when it is unrelated control material.
    # Legacy-looking sidecars and hidden producer remnants are archived so the
    # recovery neither destroys nor silently leaves historical evidence behind.
    additional: list[dict[str, Any]] = [
        {
            **raw_records[name],
            "anomalies": ["acknowledgement-without-primary"],
            "attribution": _attribution(),
            "disposition": "archive",
            "kind": "orphan-legacy-acknowledgement",
        }
        for name in orphan_legacy_ack_names
    ]
    for name, record in raw_records.items():
        if name in population:
            continue
        if name.startswith(".event-"):
            kind = "incomplete-staging-artifact"
            anomaly = "incomplete-staging-artifact"
            disposition = "archive"
        elif any(name.startswith(primary + ".") for primary in legacy):
            kind = "unrecognized-legacy-sidecar"
            anomaly = "unrecognized-sidecar"
            disposition = "archive"
        elif LEGACY_ACK_RE.fullmatch(name):
            kind = "orphan-legacy-acknowledgement"
            anomaly = "acknowledgement-without-primary"
            disposition = "archive"
        elif name.startswith("event-") and name.endswith(".json"):
            raise RecoveryRefused(
                f"event-like JSON has no valid schema-v1 identity and cannot be archived: {name}"
            )
        else:
            kind = "unrecognized-queue-artifact"
            anomaly = "unrecognized-queue-artifact"
            disposition = "retain"
        population[name] = _population_record(
            record, kind=kind, disposition=disposition, anomalies=[anomaly]
        )
        additional.append(
            {
                **record,
                "anomalies": [anomaly],
                "attribution": _attribution(),
                "disposition": disposition,
                "kind": kind,
            }
        )

    ordered_population = [
        population[name] for name in sorted(population, key=os.fsencode)
    ]
    archive_members = [
        item["filename"]
        for item in ordered_population
        if item["disposition"] == "archive"
    ]
    retained_members = [
        item["filename"]
        for item in ordered_population
        if item["disposition"] == "retain"
    ]
    acknowledgement_count = sum(
        acknowledgement["sidecar_state"] == "present"
        for event in legacy_entries
        for acknowledgement in event["acknowledgements"].values()
    )
    require_directory_identity(queue, initial_queue_identity)
    root = Path(__file__).resolve().parents[2]
    event_list_digest = sha256_bytes(
        canonical_json(
            [
                {"filename": item["filename"], "sha256": item["sha256"]}
                for item in legacy_entries
            ]
        )
    )
    manifest = {
        "schema": MANIFEST_SCHEMA,
        "queue_path": str(queue),
        "queue_identity": {
            **initial_queue_identity,
            "planning_host": os.uname().nodename,
            "planning_host_is_not_event_attribution": True,
        },
        "tooling": {
            "enqueue": _tool_record(root / "ci-hub/bin/enqueue-health-escalation"),
            "recovery_cli": _tool_record(
                root / "ci-hub/bin/recover-health-escalation-legacy"
            ),
            "recovery_library": _tool_record(Path(__file__)),
            "runner": _tool_record(root / "ci-hub/bin/run-health-escalation"),
        },
        "normalization_policy": list(DEFAULT_DIGEST_IGNORE),
        "legacy_routing_identity": {
            "github_repository": "rrnewton/dev-hermit",
            "key": "operational-tick-findings",
            "role": "relay",
            "taskgraph_database": "hermit2",
            "team": "hermit2",
            "title": None,
            "title_state": "unknown-without-origin-host; never reconstruct for archival",
        },
        "source_logs": [record for record, _content in source_log_snapshots],
        "policy": {
            "attribution": "unknown in this archive-only manifest; no host is guessed",
            "delivery": "archive-only; no replay, backfill, TaskGraph write, or GitHub write",
            "owner_decision": (
                "no summary alarm is authorized because legacy evidence does not prove "
                "the current host and current state"
            ),
        },
        "counts": {
            "acknowledgements_present": acknowledgement_count,
            "archive_members": len(archive_members),
            "legacy_alarms": sum(
                1 for kind, _record in legacy.values() if kind == "alarm"
            ),
            "legacy_events": len(legacy),
            "legacy_resolves": sum(
                1 for kind, _record in legacy.values() if kind == "resolve"
            ),
            "orphan_acknowledgements": len(orphan_legacy_ack_names),
            "population_files": len(ordered_population),
            "retained_members": len(retained_members),
        },
        "legacy_events": legacy_entries,
        "additional_artifacts": sorted(
            additional, key=lambda item: os.fsencode(item["filename"])
        ),
        "population": ordered_population,
        "archive_members": archive_members,
        "retained_members": retained_members,
        "canonical_legacy_event_list_sha256": event_list_digest,
        "review_binding": (
            "the operator supplies the canonical file SHA-256 through "
            "--expected-manifest-sha256; a manifest cannot contain its own digest"
        ),
    }
    manifest["population_sha256"] = sha256_bytes(canonical_json(ordered_population))
    return manifest


def validate_manifest(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or value.get("schema") != MANIFEST_SCHEMA:
        raise RecoveryRefused("manifest has an unsupported schema")
    required = {
        "schema",
        "queue_path",
        "queue_identity",
        "tooling",
        "normalization_policy",
        "legacy_routing_identity",
        "source_logs",
        "policy",
        "counts",
        "legacy_events",
        "additional_artifacts",
        "population",
        "archive_members",
        "retained_members",
        "population_sha256",
        "canonical_legacy_event_list_sha256",
        "review_binding",
    }
    if set(value) != required:
        raise RecoveryRefused(
            "manifest fields differ from the recovery schema: "
            f"missing={sorted(required - set(value))} extra={sorted(set(value) - required)}"
        )
    queue_identity = value["queue_identity"]
    identity_fields = {
        "device",
        "inode",
        "mode",
        "planning_host",
        "planning_host_is_not_event_attribution",
    }
    if (
        not isinstance(queue_identity, dict)
        or set(queue_identity) != identity_fields
        or isinstance(queue_identity.get("device"), bool)
        or not isinstance(queue_identity.get("device"), int)
        or isinstance(queue_identity.get("inode"), bool)
        or not isinstance(queue_identity.get("inode"), int)
        or not isinstance(queue_identity.get("mode"), str)
        or not re.fullmatch(r"[0-7]{4}", queue_identity.get("mode", ""))
        or not isinstance(queue_identity.get("planning_host"), str)
        or queue_identity.get("planning_host_is_not_event_attribution") is not True
    ):
        raise RecoveryRefused("manifest queue identity is invalid")
    if not isinstance(value["population"], list):
        raise RecoveryRefused("manifest population must be a list")
    names: list[str] = []
    for record in value["population"]:
        if not isinstance(record, dict):
            raise RecoveryRefused("manifest population records must be objects")
        needed = {
            "ctime_ns",
            "filename",
            "mtime_ns",
            "mode",
            "sha256",
            "size",
            "kind",
            "disposition",
            "anomalies",
        }
        if set(record) != needed:
            raise RecoveryRefused(
                f"manifest population record has unexpected fields: {record!r}"
            )
        name = record["filename"]
        if (
            not isinstance(name, str)
            or name in {"", ".", ".."}
            or Path(name).name != name
            or "\0" in name
        ):
            raise RecoveryRefused(f"unsafe manifest filename: {name!r}")
        if not isinstance(record["sha256"], str) or not SHA_RE.fullmatch(
            record["sha256"]
        ):
            raise RecoveryRefused(f"manifest record has invalid SHA-256: {name}")
        if (
            isinstance(record["size"], bool)
            or not isinstance(record["size"], int)
            or record["size"] < 0
        ):
            raise RecoveryRefused(f"manifest record has invalid size: {name}")
        if record["disposition"] not in {"archive", "retain"}:
            raise RecoveryRefused(f"manifest record has invalid disposition: {name}")
        names.append(name)
    if names != sorted(names, key=os.fsencode) or len(names) != len(set(names)):
        raise RecoveryRefused("manifest population is not unique and bytewise sorted")
    archive = [
        item["filename"]
        for item in value["population"]
        if item["disposition"] == "archive"
    ]
    retained = [
        item["filename"]
        for item in value["population"]
        if item["disposition"] == "retain"
    ]
    if value["archive_members"] != archive or value["retained_members"] != retained:
        raise RecoveryRefused(
            "manifest disposition indexes do not match its population"
        )
    if value["population_sha256"] != sha256_bytes(canonical_json(value["population"])):
        raise RecoveryRefused("manifest population digest does not match its records")
    return value


def load_manifest(path: Path, expected_sha256: str) -> tuple[bytes, dict[str, Any]]:
    if not SHA_RE.fullmatch(expected_sha256):
        raise RecoveryRefused(
            "expected manifest SHA-256 must be 64 lowercase hex characters"
        )
    try:
        raw, _record = read_regular_file(path)
    except (OSError, RecoveryRefused) as error:
        raise RecoveryRefused(f"cannot read manifest {path}: {error}") from error
    observed = sha256_bytes(raw)
    if observed != expected_sha256:
        raise RecoveryRefused(
            f"manifest SHA-256 mismatch: expected={expected_sha256} observed={observed}"
        )
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise RecoveryRefused(f"manifest is not canonical JSON: {error}") from error
    manifest = validate_manifest(value)
    if canonical_json(manifest) != raw:
        raise RecoveryRefused(
            "manifest bytes are not the canonical reviewed representation"
        )
    return raw, manifest


def _record_map(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {item["filename"]: item for item in manifest["population"]}


def _matches_record(path: Path, record: dict[str, Any]) -> bool:
    try:
        _content, observed = read_regular_file(path)
    except RecoveryRefused:
        return False
    return all(observed[key] == record[key] for key in ARCHIVE_RECORD_KEYS)


def _matches_source_record(path: Path, record: dict[str, Any]) -> bool:
    try:
        _content, observed = read_regular_file(path)
    except RecoveryRefused:
        return False
    return all(observed[key] == record[key] for key in SOURCE_RECORD_KEYS)


def _copy_snapshot(source: Path, destination: Path, record: dict[str, Any]) -> None:
    content, observed = read_regular_file(source)
    for key in SOURCE_RECORD_KEYS:
        if observed[key] != record[key]:
            raise RecoveryRefused(
                f"queue member drifted before snapshot: {source.name}"
            )
    descriptor = os.open(
        destination,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
        int(record["mode"], 8),
    )
    try:
        os.fchmod(descriptor, int(record["mode"], 8))
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        raise
    if not _matches_record(destination, record):
        raise RecoveryRefused(f"snapshot readback differs from manifest: {source.name}")


def _verify_archive_tree(root: Path, manifest: dict[str, Any]) -> None:
    records = _record_map(manifest)
    expected = set(manifest["archive_members"])
    evidence = root / "evidence"
    snapshots = root / "snapshots"
    for directory, label in ((evidence, "evidence"), (snapshots, "snapshot")):
        directory_identity(directory)
        try:
            observed_names = set(os.listdir(directory))
        except OSError as error:
            raise RecoveryRefused(
                f"cannot list archived {label} directory: {error}"
            ) from error
        if observed_names != expected:
            raise RecoveryRefused(
                f"archived {label} population differs from manifest: "
                f"missing={sorted(expected - observed_names)} extra={sorted(observed_names - expected)}"
            )
        for name in sorted(expected, key=os.fsencode):
            if not _matches_record(directory / name, records[name]):
                raise RecoveryRefused(f"archived {label} differs from manifest: {name}")
    source_directory = root / "source-logs"
    directory_identity(source_directory)
    expected_sources = {
        record["archive_name"]: record for record in manifest["source_logs"]
    }
    try:
        observed_sources = set(os.listdir(source_directory))
    except OSError as error:
        raise RecoveryRefused(
            f"cannot list archived source-log directory: {error}"
        ) from error
    if observed_sources != set(expected_sources):
        raise RecoveryRefused(
            "archived source-log population differs from manifest: "
            f"missing={sorted(set(expected_sources) - observed_sources)} "
            f"extra={sorted(observed_sources - set(expected_sources))}"
        )
    for name, record in expected_sources.items():
        content, observed = read_regular_file(source_directory / name)
        if (
            observed["size"] != record["size"]
            or sha256_bytes(content) != record["sha256"]
        ):
            raise RecoveryRefused(
                f"archived source-log prefix differs from manifest: {name}"
            )


def _transaction_content(manifest_sha256: str) -> bytes:
    return canonical_json(
        {
            "manifest_sha256": manifest_sha256,
            "schema": TRANSACTION_SCHEMA,
            "state": "applying-with-queue-sentinel",
        }
    )


def _archive_ready_content(manifest_sha256: str, members: int) -> bytes:
    return canonical_json(
        {
            "archive_members": members,
            "manifest_sha256": manifest_sha256,
            "schema": TRANSACTION_SCHEMA,
            "state": "archive-ready-sentinel-still-blocks-delivery",
        }
    )


def _released_content(manifest_sha256: str) -> bytes:
    return canonical_json(
        {
            "manifest_sha256": manifest_sha256,
            "schema": TRANSACTION_SCHEMA,
            "state": "applied",
        }
    )


def _require_exact_file(path: Path, expected: bytes, label: str) -> None:
    content, _record = read_regular_file(path)
    if content != expected:
        raise RecoveryRefused(f"{label} differs from its hash-bound transaction")


def _validate_final_transaction(
    *,
    final: Path,
    manifest: dict[str, Any],
    manifest_sha256: str,
    sentinel_name: str,
    sentinel_content: bytes,
    queue_sentinel_present: bool,
) -> tuple[Path | None, bool]:
    """Validate the complete final archive and its release state."""
    directory_identity(final)
    try:
        names = set(os.listdir(final))
    except OSError as error:
        raise RecoveryRefused(f"cannot list final archive: {error}") from error
    base = {
        "archive-ready.json",
        "evidence",
        "manifest.json",
        "snapshots",
        "source-logs",
        "transaction.json",
    }
    allowed = base | {"control", "released.json"}
    if not base <= names or not names <= allowed:
        raise RecoveryRefused(
            "final archive control population is incomplete or unexpected: "
            f"missing={sorted(base - names)} extra={sorted(names - allowed)}"
        )
    _require_exact_file(
        final / "transaction.json",
        _transaction_content(manifest_sha256),
        "transaction receipt",
    )
    _require_exact_file(
        final / "archive-ready.json",
        _archive_ready_content(manifest_sha256, len(manifest["archive_members"])),
        "archive-ready receipt",
    )

    control = final / "control"
    control_exists = "control" in names
    if control_exists:
        directory_identity(control)
        control_names = set(os.listdir(control))
        expected_control = set() if queue_sentinel_present else {sentinel_name}
        if control_names != expected_control:
            raise RecoveryRefused(
                "final archive control evidence differs from release state: "
                f"expected={sorted(expected_control)} observed={sorted(control_names)}"
            )
        if not queue_sentinel_present:
            _require_exact_file(
                control / sentinel_name, sentinel_content, "archived recovery sentinel"
            )
    elif not queue_sentinel_present:
        raise RecoveryRefused("released archive has no preserved control sentinel")

    released = "released.json" in names
    if released:
        if queue_sentinel_present:
            raise RecoveryRefused(
                "release receipt exists while the queue sentinel still blocks"
            )
        _require_exact_file(
            final / "released.json",
            _released_content(manifest_sha256),
            "release receipt",
        )
    return control if control_exists else None, released


def _queue_matches_manifest(
    queue: Path,
    raw_manifest: bytes,
    source_log_snapshots: list[tuple[dict[str, Any], bytes]],
) -> dict[str, Any]:
    observed = build_manifest(queue, source_log_snapshots=source_log_snapshots)
    if canonical_json(observed) != raw_manifest:
        raise RecoveryRefused(
            "locked queue population/content/sidecar evidence differs from the reviewed manifest"
        )
    return observed


def _sentinel_content(manifest_sha256: str) -> bytes:
    return canonical_json(
        {
            "manifest_sha256": manifest_sha256,
            "purpose": "block queue delivery until every reviewed legacy artifact is archived",
            "schema": TRANSACTION_SCHEMA,
        }
    )


def _current_population(queue: Path) -> dict[str, dict[str, Any]]:
    _contents, records = inventory(queue)
    return records


def _retained_plus_sentinel_matches(
    queue: Path, manifest: dict[str, Any], sentinel_name: str, sentinel_sha: str
) -> bool:
    current = _current_population(queue)
    expected_records = _record_map(manifest)
    expected_names = set(manifest["retained_members"]) | {sentinel_name}
    if set(current) != expected_names:
        return False
    for name in manifest["retained_members"]:
        expected = expected_records[name]
        if any(current[name][key] != expected[key] for key in SOURCE_RECORD_KEYS):
            return False
    return current[sentinel_name]["sha256"] == sentinel_sha


def _failure_destination(archive_root: Path, manifest_sha256: str) -> Path:
    for suffix in range(1000):
        candidate = archive_root / (
            f"{manifest_sha256}.failed.{os.getpid()}"
            + ("" if suffix == 0 else f".{suffix}")
        )
        if not candidate.exists():
            return candidate
    raise RecoveryRefused("cannot allocate a failure archive name")


def _rollback_in_process(
    *,
    queue: Path,
    staging: Path,
    manifest: dict[str, Any],
    manifest_sha256: str,
    sentinel: Path,
) -> tuple[bool, str]:
    """Restore the reviewed population after an apply failure; preserve all variants."""
    records = _record_map(manifest)
    evidence = staging / "evidence"
    snapshots = staging / "snapshots"
    mutated = staging / "mutated-during-apply"
    mutated.mkdir(exist_ok=True)
    try:
        for name in reversed(manifest["archive_members"]):
            source = queue / name
            moved = evidence / name
            snapshot = snapshots / name
            record = records[name]
            if path_entry_exists(source) and not _matches_record(source, record):
                os.replace(source, mutated / name)
            if path_entry_exists(source) and _matches_record(source, record):
                if path_entry_exists(moved):
                    if not _matches_record(moved, record):
                        os.replace(moved, mutated / f"{name}.archived")
                    else:
                        # Preserve the duplicate exact copy; it is not evidence loss.
                        pass
                continue
            if path_entry_exists(moved) and _matches_record(moved, record):
                os.replace(moved, source)
            elif path_entry_exists(snapshot) and _matches_record(snapshot, record):
                content = snapshot.read_bytes()
                temporary = queue / f".{name}.restore.{secrets.token_hex(4)}"
                descriptor = os.open(
                    temporary,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                    int(record["mode"], 8),
                )
                os.fchmod(descriptor, int(record["mode"], 8))
                with os.fdopen(descriptor, "wb") as handle:
                    handle.write(content)
                    handle.flush()
                    os.fsync(handle.fileno())
                os.replace(temporary, source)
            else:
                return False, f"no exact recovery copy remains for {name}"
        control = staging / "control"
        control.mkdir(exist_ok=True)
        if path_entry_exists(sentinel):
            os.replace(sentinel, control / sentinel.name)
        # These directories participate in cross-directory renames above.
        # Persist each destination before its corresponding source removal so
        # a power loss cannot lose the only named copy of evidence.
        fsync_directory(control)
        fsync_directory(mutated)
        fsync_directory(queue)
        fsync_directory(evidence)
        fsync_directory(snapshots)
        atomic_write(
            staging / "rollback.json",
            canonical_json(
                {
                    "manifest_sha256": manifest_sha256,
                    "schema": TRANSACTION_SCHEMA,
                    "state": "rolled-back-after-refusal",
                }
            ),
        )
        return True, "reviewed queue population restored"
    except (OSError, RecoveryRefused) as error:
        return False, str(error)


def apply_recovery(
    *,
    queue: Path,
    manifest_path: Path,
    expected_manifest_sha256: str,
    archive_root: Path,
    phase_hook: Callable[[str], None] | None = None,
) -> Path:
    """Archive reviewed legacy evidence without publishing or deleting it."""
    hook = phase_hook or (lambda _phase: None)
    raw_manifest, manifest = load_manifest(manifest_path, expected_manifest_sha256)
    queue = queue.resolve(strict=True)
    if manifest["queue_path"] != str(queue):
        raise RecoveryRefused(
            f"manifest targets {manifest['queue_path']!r}, not requested queue {str(queue)!r}"
        )
    archive_root = archive_root.resolve()
    if archive_root == queue or queue in archive_root.parents:
        raise RecoveryRefused("archive root must be outside the queue")
    archive_root.mkdir(parents=True, exist_ok=True)
    if queue.stat().st_dev != archive_root.stat().st_dev:
        raise RecoveryRefused(
            "archive root must share the queue filesystem for atomic moves"
        )

    final = archive_root / expected_manifest_sha256
    staging = archive_root / f".{expected_manifest_sha256}.applying"
    sentinel_name = f"event-legacy-recovery-{expected_manifest_sha256[:16]}.alarm"
    sentinel = queue / sentinel_name
    sentinel_content = _sentinel_content(expected_manifest_sha256)
    sentinel_sha = sha256_bytes(sentinel_content)

    for candidate, label in (
        (final, "final archive"),
        (staging, "staging transaction"),
    ):
        if candidate.exists() or candidate.is_symlink():
            metadata = candidate.stat(follow_symlinks=False)
            if not stat.S_ISDIR(metadata.st_mode):
                raise RecoveryRefused(
                    f"existing {label} is not a real directory: {candidate}"
                )

    lock_path = queue / ".worker.lock"
    try:
        lock = lock_path.open("r+b")
    except OSError as error:
        raise RecoveryRefused(f"cannot open existing queue lock: {error}") from error
    with lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        require_held_lock(lock, lock_path)
        require_directory_identity(queue, manifest["queue_identity"])
        if final.is_dir() and staging.exists():
            raise RecoveryRefused(
                "final archive and unfinished staging transaction coexist"
            )

        # Idempotent completion after either a crash immediately around the
        # sentinel release or a caller losing the success response.
        if final.is_dir():
            archived_manifest, _record = read_regular_file(final / "manifest.json")
            if archived_manifest != raw_manifest:
                raise RecoveryRefused(
                    "existing manifest-addressed archive has different bytes"
                )
            _verify_archive_tree(final, manifest)
            sentinel_present = sentinel.exists() or sentinel.is_symlink()
            control, released = _validate_final_transaction(
                final=final,
                manifest=manifest,
                manifest_sha256=expected_manifest_sha256,
                sentinel_name=sentinel_name,
                sentinel_content=sentinel_content,
                queue_sentinel_present=sentinel_present,
            )
            if sentinel_present:
                # The reviewed prefix may have been appended, but every byte in
                # the manifest-bound prefix must still be identical immediately
                # before the queue is released.
                for record in manifest["source_logs"]:
                    load_source_log_prefix(record)
                if not _retained_plus_sentinel_matches(
                    queue, manifest, sentinel_name, sentinel_sha
                ):
                    raise RecoveryRefused(
                        "queue drifted while an archived transaction awaited release"
                    )
                if control is None:
                    control = final / "control"
                    control.mkdir(mode=0o700)
                    fsync_directory(final)
                hook("control-durable")
                require_directory_identity(queue, manifest["queue_identity"])
                if not _retained_plus_sentinel_matches(
                    queue, manifest, sentinel_name, sentinel_sha
                ):
                    raise RecoveryRefused(
                        "queue population drifted before archived release retry"
                    )
                for record in manifest["source_logs"]:
                    load_source_log_prefix(record)
                os.replace(sentinel, control / sentinel.name)
                fsync_directory(control)
                fsync_directory(queue)
                _validate_final_transaction(
                    final=final,
                    manifest=manifest,
                    manifest_sha256=expected_manifest_sha256,
                    sentinel_name=sentinel_name,
                    sentinel_content=sentinel_content,
                    queue_sentinel_present=False,
                )
            else:
                current = _current_population(queue)
                legacy_names = sorted(
                    name for name in current if LEGACY_EVENT_RE.fullmatch(name)
                )
                restored_names = sorted(set(current) & set(manifest["archive_members"]))
                if legacy_names or restored_names:
                    raise RecoveryRefused(
                        "legacy evidence reappeared after archive release: "
                        f"legacy={legacy_names[:6]} restored={restored_names[:6]}"
                    )
            if not released:
                atomic_write(
                    final / "released.json",
                    _released_content(expected_manifest_sha256),
                )
            _validate_final_transaction(
                final=final,
                manifest=manifest,
                manifest_sha256=expected_manifest_sha256,
                sentinel_name=sentinel_name,
                sentinel_content=sentinel_content,
                queue_sentinel_present=False,
            )
            return final

        if staging.exists():
            raise RecoveryRefused(
                f"unfinished recovery transaction exists at {staging}; inspect and roll it back before retrying"
            )

        source_log_snapshots = [
            load_source_log_prefix(record) for record in manifest["source_logs"]
        ]
        _queue_matches_manifest(queue, raw_manifest, source_log_snapshots)
        staging.mkdir(mode=0o700)
        (staging / "evidence").mkdir(mode=0o700)
        (staging / "snapshots").mkdir(mode=0o700)
        (staging / "source-logs").mkdir(mode=0o700)
        atomic_write(staging / "manifest.json", raw_manifest)
        if (staging / "manifest.json").read_bytes() != raw_manifest:
            raise RecoveryRefused(
                "staged manifest readback differs from reviewed bytes"
            )
        atomic_write(
            staging / "transaction.json",
            _transaction_content(expected_manifest_sha256),
        )
        records = _record_map(manifest)

        try:
            for source_record, source_content in source_log_snapshots:
                source_destination = (
                    staging / "source-logs" / source_record["archive_name"]
                )
                atomic_write(source_destination, source_content)
                if (
                    source_destination.stat().st_size != source_record["size"]
                    or sha256_bytes(source_destination.read_bytes())
                    != source_record["sha256"]
                ):
                    raise RecoveryRefused(
                        f"source-log archive readback differs: {source_record['path']}"
                    )
            # Persist the transaction directory entry and all prepared
            # children before the queue gains a durable recovery sentinel.
            fsync_directory(staging)
            fsync_directory(archive_root)
            hook("staging-durable")
            descriptor = os.open(
                sentinel,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
                0o600,
            )
            with os.fdopen(descriptor, "wb") as handle:
                handle.write(sentinel_content)
                handle.flush()
                os.fsync(handle.fileno())
            fsync_directory(queue)
            hook("sentinel-created")

            for name in manifest["archive_members"]:
                _copy_snapshot(
                    queue / name, staging / "snapshots" / name, records[name]
                )
                hook(f"snapshot:{name}")
            fsync_directory(staging / "snapshots")

            # A second complete comparison closes the snapshot window before
            # any original evidence is moved.
            current = _current_population(queue)
            current.pop(sentinel_name, None)
            expected = {
                name: {key: record[key] for key in SOURCE_RECORD_KEYS}
                for name, record in records.items()
            }
            if current != expected:
                raise RecoveryRefused(
                    "queue drifted while exact snapshots were prepared"
                )
            hook("snapshots-complete")

            for name in manifest["archive_members"]:
                source = queue / name
                if not _matches_source_record(source, records[name]):
                    raise RecoveryRefused(
                        f"queue member drifted before atomic move: {name}"
                    )
                destination = staging / "evidence" / name
                os.replace(source, destination)
                if not _matches_record(destination, records[name]):
                    raise RecoveryRefused(
                        f"queue member drifted during atomic move: {name}"
                    )
                hook(f"moved:{name}")
            fsync_directory(staging / "evidence")
            fsync_directory(queue)

            if not _retained_plus_sentinel_matches(
                queue, manifest, sentinel_name, sentinel_sha
            ):
                raise RecoveryRefused(
                    "queue population drifted before archive finalization"
                )
            for source_record, _source_content in source_log_snapshots:
                load_source_log_prefix(source_record)
            _verify_archive_tree(staging, manifest)
            atomic_write(
                staging / "archive-ready.json",
                _archive_ready_content(
                    expected_manifest_sha256, len(manifest["archive_members"])
                ),
            )
            hook("archive-ready")
            os.replace(staging, final)
            fsync_directory(archive_root)
            hook("archive-published")
            _validate_final_transaction(
                final=final,
                manifest=manifest,
                manifest_sha256=expected_manifest_sha256,
                sentinel_name=sentinel_name,
                sentinel_content=sentinel_content,
                queue_sentinel_present=True,
            )
            for source_record, _source_content in source_log_snapshots:
                load_source_log_prefix(source_record)

            # This one rename is the visibility boundary: until it happens the
            # runner sees a legacy .alarm blocker; after it happens every exact
            # reviewed byte is in the manifest-addressed archive.
            control = final / "control"
            control.mkdir(mode=0o700)
            fsync_directory(final)
            hook("control-durable")
            require_directory_identity(queue, manifest["queue_identity"])
            if not _retained_plus_sentinel_matches(
                queue, manifest, sentinel_name, sentinel_sha
            ):
                raise RecoveryRefused(
                    "queue population drifted after archive publication"
                )
            for source_record, _source_content in source_log_snapshots:
                load_source_log_prefix(source_record)
            os.replace(sentinel, control / sentinel.name)
            fsync_directory(control)
            fsync_directory(queue)
            atomic_write(
                final / "released.json",
                _released_content(expected_manifest_sha256),
            )
            _validate_final_transaction(
                final=final,
                manifest=manifest,
                manifest_sha256=expected_manifest_sha256,
                sentinel_name=sentinel_name,
                sentinel_content=sentinel_content,
                queue_sentinel_present=False,
            )
            hook("released")
            return final
        except BaseException as error:
            # Once staging becomes final, archive completeness is durable and a
            # retry can finish only the sentinel release. Never roll that state
            # back implicitly.
            if final.exists():
                raise
            restored, detail = _rollback_in_process(
                queue=queue,
                staging=staging,
                manifest=manifest,
                manifest_sha256=expected_manifest_sha256,
                sentinel=sentinel,
            )
            failure = _failure_destination(archive_root, expected_manifest_sha256)
            if staging.exists():
                os.replace(staging, failure)
                fsync_directory(archive_root)
            if not restored:
                raise RecoveryRefused(
                    f"apply failed and automatic rollback is incomplete ({detail}); "
                    f"the recovery sentinel remains authoritative: {error}"
                ) from error
            if isinstance(error, RecoveryRefused):
                raise RecoveryRefused(
                    f"{error}; automatic rollback succeeded; audit={failure}"
                ) from error
            raise


def write_plan(
    queue: Path,
    output: Path,
    *,
    source_logs: tuple[Path, ...] = (),
) -> tuple[str, dict[str, Any]]:
    queue = queue.resolve(strict=True)
    output_resolved = output.resolve()
    if output_resolved == queue or queue in output_resolved.parents:
        raise RecoveryRefused("manifest output must be outside the queue population")
    try:
        resolved_source_logs = tuple(path.resolve(strict=True) for path in source_logs)
    except OSError as error:
        raise RecoveryRefused(f"cannot resolve source log: {error}") from error
    if len(set(resolved_source_logs)) != len(resolved_source_logs):
        raise RecoveryRefused("each source log may be supplied only once")
    if output_resolved in resolved_source_logs:
        raise RecoveryRefused("manifest output must not alias a source log")
    source_identities = [
        (path.stat().st_dev, path.stat().st_ino) for path in resolved_source_logs
    ]
    if len(set(source_identities)) != len(source_identities):
        raise RecoveryRefused("source-log paths must not alias the same file")
    if output.exists() or output.is_symlink():
        output_metadata = output.stat()
        if (output_metadata.st_dev, output_metadata.st_ino) in source_identities:
            raise RecoveryRefused("manifest output must not alias a source log")
    for source_log in resolved_source_logs:
        if source_log == queue or queue in source_log.parents:
            raise RecoveryRefused("source logs must be outside the queue population")
    lock_path = queue / ".worker.lock"
    try:
        lock = lock_path.open("rb")
    except OSError as error:
        raise RecoveryRefused(f"cannot open existing queue lock: {error}") from error
    with lock:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        require_held_lock(lock, lock_path)
        source_log_snapshots = [
            capture_source_log(path) for path in resolved_source_logs
        ]
        source_log_snapshots.sort(key=lambda item: os.fsencode(item[0]["path"]))
        manifest = build_manifest(queue, source_log_snapshots=source_log_snapshots)
        raw = canonical_json(manifest)
    atomic_write(output, raw)
    output_content, _record = read_regular_file(output)
    if output_content != raw:
        raise RecoveryRefused("manifest output readback differs from planned bytes")
    return sha256_bytes(raw), manifest
