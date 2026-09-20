#!/usr/bin/env python3
"""Durable identities and lossless bounded parts for health escalations."""

from __future__ import annotations

import hashlib
import json
import os
import re
import secrets
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SCHEMA = "hermit-health-escalation/v1"
PART_SCHEMA = "hermit-health-escalation-part/v1"
EVENT_RE = re.compile(r"^event-[0-9]{8}T[0-9]{12}Z-[0-9]+-[A-Za-z0-9]{8}$")
HOST_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,252}$")
KEY_RE = re.compile(r"^[a-z0-9][a-z0-9-]{2,63}$")
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
DEFAULT_DIGEST_IGNORE = (
    r"observed_at=[0-9TZ:+-]+",
    r"input_sha=[0-9a-f]{7,40}",
)
PART_BEGIN = "<!-- hermit-health-escalation-part-v1 -->"
PART_END = "<!-- /hermit-health-escalation-part-v1 -->"
# A tg import JSON string can approximately double on backslash-heavy input.
# This ceiling remains well below both Linux's per-argument limit and GitHub's
# issue-body limit after the disclosure and identity headers are added.
PART_PAYLOAD_BYTES = 30_000


class InvalidEvent(ValueError):
    pass


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def normalized_sha256(payload: str, ignore: tuple[str, ...]) -> str:
    normalized = payload
    for pattern in ignore:
        normalized = re.sub(pattern, "<ignored>", normalized)
    return sha256_text(normalized)


def host_key(host: str) -> str:
    if not HOST_RE.fullmatch(host):
        raise InvalidEvent(f"origin host has unsupported syntax: {host!r}")
    slug = re.sub(r"[^a-z0-9]+", "-", host.lower()).strip("-")
    prefix = "operational-tick-findings-"
    # The suffix is unconditional: dots, dashes and repeated separators all
    # normalize to the same slug, but they are different origin identities and
    # one host's resolve must never match the other's open record.
    suffix = hashlib.sha256(host.encode("utf-8")).hexdigest()[:10]
    readable = (prefix + slug)[: 64 - len(suffix) - 1].rstrip("-")
    candidate = readable + "-" + suffix
    if not KEY_RE.fullmatch(candidate):
        raise InvalidEvent(f"origin host cannot form a sink key: {host!r}")
    return candidate


def new_event_id(*, pid: int | None = None) -> str:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    return f"event-{stamp}-{pid if pid is not None else os.getpid()}-{secrets.token_hex(4)}"


def make_event(
    *, kind: str, payload: str, origin_host: str, findings: int,
    event_id: str | None = None, created_at: str | None = None,
    digest_ignore: tuple[str, ...] = DEFAULT_DIGEST_IGNORE,
) -> dict[str, Any]:
    if kind not in {"alarm", "resolve"}:
        raise InvalidEvent(f"kind must be alarm or resolve, got {kind!r}")
    if not isinstance(findings, int) or findings < 0:
        raise InvalidEvent("findings must be a nonnegative integer")
    if kind == "alarm" and (findings < 1 or not payload.strip()):
        raise InvalidEvent("alarm must contain at least one finding")
    if kind == "resolve" and (findings != 0 or payload):
        raise InvalidEvent("resolve must have zero findings and an empty payload")
    eid = event_id or new_event_id()
    if not EVENT_RE.fullmatch(eid):
        raise InvalidEvent(f"event id has unsupported syntax: {eid!r}")
    key = host_key(origin_host)
    ignored = list(digest_ignore)
    for pattern in ignored:
        try:
            re.compile(pattern)
        except re.error as error:
            raise InvalidEvent(f"digest-ignore pattern is invalid: {pattern!r}: {error}") from error
    created = created_at or utc_now()
    # Parsing also proves the timestamp is not just a display label.
    try:
        datetime.fromisoformat(created.replace("Z", "+00:00"))
    except ValueError as error:
        raise InvalidEvent(f"created_at is not ISO-8601: {created!r}") from error
    return {
        "schema": SCHEMA,
        "event_id": eid,
        "created_at": created,
        "origin_host": origin_host,
        "key": key,
        "kind": kind,
        "title": f"operational health tick reports {findings} finding(s) on {origin_host}",
        "findings": findings,
        "digest_ignore": ignored,
        "payload": payload,
        "payload_sha256": sha256_text(payload),
        "normalized_sha256": normalized_sha256(payload, tuple(ignored)),
    }


def validate_event(value: Any, *, expected_filename: str | None = None) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise InvalidEvent("event envelope must be a JSON object")
    expected = {
        "schema", "event_id", "created_at", "origin_host", "key", "kind",
        "title", "findings", "digest_ignore", "payload", "payload_sha256",
        "normalized_sha256",
    }
    if set(value) != expected:
        missing = sorted(expected - set(value))
        extra = sorted(set(value) - expected)
        raise InvalidEvent(f"event fields differ from schema (missing={missing}, extra={extra})")
    if value["schema"] != SCHEMA:
        raise InvalidEvent(f"unsupported event schema: {value['schema']!r}")
    string_fields = (
        "event_id", "created_at", "origin_host", "key", "kind", "title",
        "payload", "payload_sha256", "normalized_sha256",
    )
    for field in string_fields:
        if not isinstance(value[field], str):
            raise InvalidEvent(f"event field {field!r} must be a string")
    if isinstance(value["findings"], bool) or not isinstance(value["findings"], int):
        raise InvalidEvent("event field 'findings' must be an integer")
    if not isinstance(value["digest_ignore"], list) or not all(
        isinstance(pattern, str) for pattern in value["digest_ignore"]
    ):
        raise InvalidEvent("event field 'digest_ignore' must be a list of strings")
    rebuilt = make_event(
        kind=value["kind"], payload=value["payload"], origin_host=value["origin_host"],
        findings=value["findings"], event_id=value["event_id"],
        created_at=value["created_at"], digest_ignore=tuple(value["digest_ignore"]),
    )
    for field in expected:
        if value[field] != rebuilt[field]:
            raise InvalidEvent(f"event field {field!r} does not match its durable identity")
    if not SHA_RE.fullmatch(value["payload_sha256"]) or not SHA_RE.fullmatch(value["normalized_sha256"]):
        raise InvalidEvent("event digest is not a SHA-256 value")
    if expected_filename is not None and expected_filename != f"{value['event_id']}.json":
        raise InvalidEvent(
            f"event filename {expected_filename!r} does not match {value['event_id']!r}"
        )
    return value


def load_event(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise InvalidEvent(f"cannot read event envelope {path}: {error}") from error
    return validate_event(value, expected_filename=path.name)


def write_event(queue: Path, event: dict[str, Any]) -> Path:
    validate_event(event)
    queue.mkdir(parents=True, exist_ok=True)
    final = queue / f"{event['event_id']}.json"
    temporary = queue / f".{event['event_id']}.{secrets.token_hex(6)}.tmp"
    encoded = (json.dumps(event, sort_keys=True, ensure_ascii=False) + "\n").encode("utf-8")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, final)
        directory_fd = os.open(queue, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
    return final


def split_utf8(text: str, limit: int = PART_PAYLOAD_BYTES) -> list[str]:
    if limit < 1:
        raise ValueError("part limit must be positive")
    if not text:
        return [""]
    parts: list[str] = []
    start = 0
    used = 0
    for index, char in enumerate(text):
        width = len(char.encode("utf-8"))
        if used and used + width > limit:
            parts.append(text[start:index])
            start = index
            used = 0
        used += width
    parts.append(text[start:])
    return parts


def part_documents(event: dict[str, Any]) -> list[str]:
    event = validate_event(event)
    chunks = split_utf8(event["payload"])
    total = len(chunks)
    documents = []
    for index, chunk in enumerate(chunks, 1):
        header = (
            f"{PART_BEGIN}\n"
            f"escalation-schema: {PART_SCHEMA}\n"
            f"escalation-key: {event['key']}\n"
            f"escalation-event-id: {event['event_id']}\n"
            f"escalation-origin-host: {event['origin_host']}\n"
            f"escalation-created-at: {event['created_at']}\n"
            f"escalation-digest: {event['normalized_sha256']}\n"
            f"escalation-payload-sha256: {event['payload_sha256']}\n"
            f"escalation-part: {index}/{total}\n"
            f"escalation-part-sha256: {sha256_text(chunk)}\n"
            f"{PART_END}\n"
        )
        documents.append(header + chunk)
    return documents


def parse_part(document: str) -> dict[str, Any] | None:
    if not document.startswith(PART_BEGIN + "\n"):
        return None
    boundary = "\n" + PART_END + "\n"
    split = document.find(boundary)
    if split < 0:
        return None
    header_text = document[len(PART_BEGIN) + 1 : split]
    payload = document[split + len(boundary) :]
    fields: dict[str, str] = {}
    for line in header_text.splitlines():
        if ": " not in line:
            return None
        name, value = line.split(": ", 1)
        if name in fields:
            return None
        fields[name] = value
    required = {
        "escalation-schema", "escalation-key", "escalation-event-id",
        "escalation-origin-host", "escalation-created-at", "escalation-digest",
        "escalation-payload-sha256", "escalation-part", "escalation-part-sha256",
    }
    if set(fields) != required or fields["escalation-schema"] != PART_SCHEMA:
        return None
    if not all(
        SHA_RE.fullmatch(fields[name])
        for name in (
            "escalation-digest",
            "escalation-payload-sha256",
            "escalation-part-sha256",
        )
    ):
        return None
    try:
        number, total = (int(item) for item in fields["escalation-part"].split("/", 1))
    except (ValueError, TypeError):
        return None
    if number < 1 or total < number or sha256_text(payload) != fields["escalation-part-sha256"]:
        return None
    return {"number": number, "total": total, "payload": payload, **fields}


def stable_task_id(event: dict[str, Any]) -> str:
    event = validate_event(event)
    identity = f"{event['key']}\0{event['event_id']}".encode("utf-8")
    return "health_event_" + hashlib.sha256(identity).hexdigest()[:32]
