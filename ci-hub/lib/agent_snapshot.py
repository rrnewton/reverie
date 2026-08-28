#!/usr/bin/env python3
"""Use the producer-owned Rust contract for agent snapshot schema 1."""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Any


class AgentSnapshotSchemaError(ValueError):
    """The snapshot bytes or one shared status value were refused."""


@dataclass(frozen=True)
class StatusSemantics:
    live: bool
    busy: bool
    broken: bool


def default_checker() -> Path:
    return Path(__file__).resolve().parents[2] / "scripts" / "check-agent-snapshot.rs"


def _run_checker(arguments: list[str], raw: bytes = b"") -> bytes:
    authority = default_checker()
    try:
        result = subprocess.run(
            [str(authority), *arguments],
            input=raw,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        raise AgentSnapshotSchemaError(
            f"cannot execute agent snapshot schema checker {authority}: {error}"
        ) from error
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise AgentSnapshotSchemaError(
            f"agent snapshot schema checker refused rc={result.returncode}: "
            f"{detail or 'no diagnostic'}"
        )
    return result.stdout


def parse_snapshot_bytes(raw: bytes) -> dict[str, Any]:
    _run_checker(["--validate-stdin"], raw)
    try:
        document = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AgentSnapshotSchemaError(
            f"validated agent snapshot bytes could not be decoded: {error}"
        ) from error
    if not isinstance(document, dict):
        raise AgentSnapshotSchemaError("validated agent snapshot root is not an object")
    return document


def parse_snapshot_payload(payload: object, *, captured_at: float = 0.0) -> list[dict[str, Any]]:
    raw = json.dumps(
        {"schema_version": 1, "captured_at": captured_at, "agents": payload},
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    document = parse_snapshot_bytes(raw)
    agents = document["agents"]
    assert isinstance(agents, list)
    return agents


@lru_cache(maxsize=None)
def status_semantics(status: str) -> StatusSemantics:
    raw = _run_checker(["--classify-status", status])
    try:
        document = json.loads(raw)
        if document.get("schema_version") != 1 or document.get("status") != status:
            raise ValueError("invalid status classification envelope")
        live = document["live"]
        busy = document["busy"]
        broken = document["broken"]
        if not all(type(value) is bool for value in (live, busy, broken)):
            raise TypeError("status classification fields are not booleans")
        return StatusSemantics(live=live, busy=busy, broken=broken)
    except (UnicodeDecodeError, json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
        raise AgentSnapshotSchemaError(
            f"agent snapshot checker returned an invalid status classification: {error}"
        ) from error
