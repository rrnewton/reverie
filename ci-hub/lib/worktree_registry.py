#!/usr/bin/env python3
"""Read `worktree-state.json` only after the producer-owned Rust type accepts it.

Python consumers intentionally ask different questions of the registry. They
therefore keep the original dictionary, but no consumer may interpret it until
`scripts/worktree_registry.rs` has accepted the exact bytes being read.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any


class RegistrySchemaError(ValueError):
    """The registry bytes are unreadable or refused by the shared schema."""


def default_checker() -> Path:
    return (
        Path(__file__).resolve().parents[2] / "scripts" / "check-worktree-registry.rs"
    )


def parse_registry_bytes(raw: bytes, *, checker: Path | None = None) -> dict[str, Any]:
    authority = checker or default_checker()
    try:
        result = subprocess.run(
            [str(authority), "--validate-state-stdin"],
            input=raw,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        raise RegistrySchemaError(
            f"cannot execute worktree registry schema checker {authority}: {error}"
        ) from error
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise RegistrySchemaError(
            f"worktree registry schema checker refused rc={result.returncode}: "
            f"{detail or 'no diagnostic'}"
        )
    try:
        document = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RegistrySchemaError(
            f"validated registry bytes could not be decoded: {error}"
        ) from error
    if not isinstance(document, dict):
        raise RegistrySchemaError("validated registry root is not an object")
    return document


def read_registry(path: Path, *, checker: Path | None = None) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise RegistrySchemaError(
            f"cannot read worktree registry {path}: {error}"
        ) from error
    return parse_registry_bytes(raw, checker=checker)
