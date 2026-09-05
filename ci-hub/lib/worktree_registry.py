#!/usr/bin/env python3
"""Read the legacy and current worktree registries without guessing a source.

Legacy consumers keep the original ``worktree-state.json`` dictionary, but may
not interpret it until ``scripts/worktree_registry.rs`` accepts the exact bytes.
Current consumers select ``ACTIVE.<machine>.json`` from ``.wrkslots.yml`` and
refuse missing, empty, cross-machine, duplicate, or unsupported state.
"""

from __future__ import annotations

import json
import os
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


# ⚠️ VALID AND CURRENT ARE DIFFERENT QUESTIONS, AND ONLY THE FIRST WAS ASKED.
#
# `worktree-state.json` is written by scripts/release-worktree.rs, which carries a
# LEGACY INTERFACE banner and refuses every invocation in a project configured
# with .wrkslots.yml. The migration to wrkslots retired the WRITERS and left the
# READERS, so in a configured project this file has no producer at all and holds
# whatever the last legacy invocation left.
#
# It passes the schema checker perfectly, because it is a well-formed document of
# a schema nobody writes any more. Measured 2026-09-03: the real file had been
# frozen since 2026-08-28 07:40 while five consumers read it as current state,
# and a real reclaim that moved the live registry from 100 rows to 93 changed
# nothing any of them reported.
#
# So the presence of .wrkslots.yml beside this file is itself proof that the
# bytes cannot be current, and it is checked here rather than in five consumers
# because a consumer moved to a live source while a dead producer stays in place
# leaves the next reader to trip over the same thing.
RETIRED_PRODUCER_MARKER = ".wrkslots.yml"
WRKSLOTS_REGISTRY_DIR = "worktrees"


def producer_is_retired(path: Path) -> bool:
    return (path.parent / RETIRED_PRODUCER_MARKER).is_file()


def read_registry(
    path: Path,
    *,
    checker: Path | None = None,
    include_owner_sidecars: bool = False,
) -> ValidatedRegistry:
    if path.name == "worktree-state.json" and producer_is_retired(path):
        raise RegistrySchemaError(
            f"{path} has no producer in this project: scripts/release-worktree.rs "
            f"writes it and refuses whenever {RETIRED_PRODUCER_MARKER} is present, "
            "so these bytes are the last legacy invocation rather than current "
            "state. Read the wrkslots registry at "
            "worktrees/ACTIVE.<machine>.json instead"
        )
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise RegistrySchemaError(
            f"cannot read worktree registry {path}: {error}"
        ) from error
    return parse_registry_bytes(
        raw, checker=checker, include_owner_sidecars=include_owner_sidecars
    )


def wrkslots_machine(root: Path) -> str:
    """Return the configured machine shard without guessing between registries."""

    override = os.environ.get("WRKSLOTS_MACHINE")
    if override:
        return override
    path = root / RETIRED_PRODUCER_MARKER
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RegistrySchemaError(f"cannot read {path}: {error}") from error
    machine = document.get("machine") if isinstance(document, dict) else None
    if not isinstance(machine, str) or not machine:
        raise RegistrySchemaError(f"{path} records no machine shard")
    return machine


def read_active_wrkslots_registry(
    root: Path, path: Path | None = None
) -> dict[str, dict[str, Any]]:
    """Read the current machine's active wrkslots rows, keyed by slot name.

    ``ACTIVE.<machine>.json`` contains only active rows. An empty registry is
    refused because zero rows would make capacity and parity checks report a
    clean empty fleet when their input had disappeared.
    """

    machine = wrkslots_machine(root)
    registry_path = path or root / WRKSLOTS_REGISTRY_DIR / f"ACTIVE.{machine}.json"
    try:
        document = json.loads(registry_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RegistrySchemaError(
            f"cannot read active wrkslots registry {registry_path}: {error}"
        ) from error
    if not isinstance(document, dict):
        raise RegistrySchemaError(
            f"active wrkslots registry {registry_path} root is not an object"
        )
    if document.get("schema") != 2:
        raise RegistrySchemaError(
            f"active wrkslots registry {registry_path} has unsupported schema "
            f"{document.get('schema')!r}"
        )
    if document.get("machine") != machine:
        raise RegistrySchemaError(
            f"active wrkslots registry {registry_path} names machine "
            f"{document.get('machine')!r}, expected {machine!r}"
        )
    rows = document.get("slots")
    if not isinstance(rows, list):
        raise RegistrySchemaError(
            f"active wrkslots registry {registry_path} slots is not a list"
        )
    active: dict[str, dict[str, Any]] = {}
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            raise RegistrySchemaError(
                f"active wrkslots registry {registry_path} slots[{index}] is not an object"
            )
        slot = row.get("slot")
        if not isinstance(slot, str) or not slot:
            raise RegistrySchemaError(
                f"active wrkslots registry {registry_path} slots[{index}] has no slot name"
            )
        if row.get("machine") != machine:
            raise RegistrySchemaError(
                f"active wrkslots registry {registry_path} slot {slot!r} names machine "
                f"{row.get('machine')!r}, expected {machine!r}"
            )
        if slot in active:
            raise RegistrySchemaError(
                f"active wrkslots registry {registry_path} repeats slot {slot!r}"
            )
        active[slot] = row
    if not active:
        raise RegistrySchemaError(
            f"active wrkslots registry {registry_path} lists no slots; a fleet with "
            "no rows at all is unverifiable rather than clean"
        )
    return active
