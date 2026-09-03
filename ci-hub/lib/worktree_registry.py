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
