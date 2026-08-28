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


class ValidatedRegistry(dict[str, Any]):
    """Original registry object plus per-slot results from the Rust authority."""

    def __init__(
        self,
        document: dict[str, Any],
        *,
        owner_sidecar_errors: dict[str, str | None] | None = None,
    ) -> None:
        super().__init__(document)
        self.owner_sidecar_errors = owner_sidecar_errors


def default_checker() -> Path:
    return (
        Path(__file__).resolve().parents[2] / "scripts" / "check-worktree-registry.rs"
    )


def parse_registry_bytes(
    raw: bytes,
    *,
    checker: Path | None = None,
    include_owner_sidecars: bool = False,
) -> ValidatedRegistry:
    authority = checker or default_checker()
    mode = (
        "--owner-sidecar-errors-stdin"
        if include_owner_sidecars
        else "--validate-state-stdin"
    )
    try:
        result = subprocess.run(
            [str(authority), mode],
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
    owner_sidecar_errors: dict[str, str | None] | None = None
    if include_owner_sidecars:
        try:
            parsed_errors = json.loads(result.stdout)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RegistrySchemaError(
                f"owner_sidecar checker returned invalid JSON: {error}"
            ) from error
        if not isinstance(parsed_errors, dict) or not all(
            isinstance(slot, str) and (error is None or isinstance(error, str))
            for slot, error in parsed_errors.items()
        ):
            raise RegistrySchemaError(
                "owner_sidecar checker returned a malformed per-slot result"
            )
        slots = document.get("slots")
        if not isinstance(slots, dict) or set(parsed_errors) != set(slots):
            raise RegistrySchemaError(
                "owner_sidecar checker result does not cover the exact registry slots"
            )
        owner_sidecar_errors = parsed_errors
    return ValidatedRegistry(
        document, owner_sidecar_errors=owner_sidecar_errors
    )


def read_registry(
    path: Path,
    *,
    checker: Path | None = None,
    include_owner_sidecars: bool = False,
) -> ValidatedRegistry:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise RegistrySchemaError(
            f"cannot read worktree registry {path}: {error}"
        ) from error
    return parse_registry_bytes(
        raw, checker=checker, include_owner_sidecars=include_owner_sidecars
    )
