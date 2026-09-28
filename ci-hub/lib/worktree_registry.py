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
import sys
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


def _refuse_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    document: dict[str, Any] = {}
    for key, value in pairs:
        if key in document:
            raise ValueError(f"duplicate key {key!r}")
        document[key] = value
    return document


def _load_yaml_refusing_duplicate_keys(path: Path, text: str) -> Any:
    try:
        import yaml
    except ImportError as error:
        raise RegistrySchemaError(
            f"cannot read {path}: it is YAML, and PyYAML is not importable by "
            f"{sys.executable} (install ci-hub/health/requirements.txt): {error}"
        ) from error

    class Loader(yaml.SafeLoader):
        def construct_mapping(self, node: Any, deep: bool = False) -> Any:
            mapping = super().construct_mapping(node, deep=deep)
            if len(mapping) != len(node.value):
                seen: set[Any] = set()
                for key_node, _ in node.value:
                    key = self.construct_object(key_node, deep=deep)
                    if key in seen:
                        raise yaml.constructor.ConstructorError(
                            "while constructing a mapping",
                            node.start_mark,
                            f"found duplicate key {key!r}",
                            key_node.start_mark,
                        )
                    seen.add(key)
            return mapping

    try:
        return yaml.load(text, Loader=Loader)
    except yaml.YAMLError as error:
        raise RegistrySchemaError(f"cannot read {path}: {error}") from error
    except Exception as error:
        # PyYAML builds values with plain Python and lets those errors escape:
        # a plain `2026-13-45` raises ValueError (the date resolver matches,
        # then datetime refuses month 13), `!!bool maybe` raises KeyError and
        # `!!timestamp garbage` raises AttributeError. yaml.load reads nothing
        # but this text, so any failure inside it means the file cannot be
        # read as configuration. Letting it escape would turn a caller's
        # "could not be determined" into a traceback: slot_agent_parity.py
        # catches only RegistrySchemaError and an uncaught exception exits 1,
        # its "actionable" verdict. The type name stays in the message so a
        # loader bug is still recognisable.
        raise RegistrySchemaError(
            f"cannot read {path}: a value could not be constructed "
            f"({type(error).__name__}: {error})"
        ) from error


def read_wrkslots_config(root: Path) -> Any:
    """Decode ``.wrkslots.yml`` in either of the two forms wrkslots reads.

    The file was historically JSON. ``wrkslots config convert --to yaml``
    (agent-utils bada63e and later) rewrites it as literate YAML with the same
    values. wrkslots itself decides by the first non-blank character: ``{`` is
    JSON, anything else is YAML. This reader makes the same decision, so a
    converted file keeps meaning what it meant.

    Duplicate keys are refused in both forms. agent-utils bada63e's reader
    refuses them too (``yamlconfig.loads_json`` for JSON, its YAML subset for
    YAML). The wrkslots dev-hermit pinned when this was written, agent-utils
    26dd3eaa (through hermit 10770b2f), reads only JSON and decodes it with a
    plain ``json.load`` that keeps the last duplicate, so on the JSON path this
    reader is stricter than that wrkslots. It errs toward refusing: picking one
    of two ``machine`` values could select a shard wrkslots does not use.

    PyYAML is imported only for the YAML form. It is already a ci-hub dependency
    (ci-hub/health/requirements.txt), but callers that never meet a YAML file
    must not start requiring it. PyYAML is not wrkslots' reader. It resolves
    plain scalars by YAML 1.1 where wrkslots uses 1.2, so a plain ``yes``,
    ``2026-09-28`` or ``1_000`` is a string to wrkslots and a bool, date or int
    here; a caller that needs a string must refuse any other type, as
    ``wrkslots_machine`` does. A plain scalar that matches the YAML 1.1 date
    pattern but is not a real date (``2026-13-45``) is a string to wrkslots,
    and bada63e's writer emits such strings unquoted, but PyYAML cannot
    construct it, so the whole file is refused with "cannot read". The reverse
    also exists for hand-written files: a plain ``1e5``, ``+.5``, ``0o17`` or
    ``09`` is a number to wrkslots and a string here (the writer quotes such
    strings, so a converted file does not contain them). PyYAML also accepts
    syntax the wrkslots subset refuses (anchors, tags, block scalars, non-JSON
    escapes), but wrkslots cannot operate on such a file at all. None of these
    differences changes a value in dev-hermit's own converted file: its JSON
    and YAML forms decode identically (ci-hub/lib/tests/fixtures/wrkslots-config.*).
    """

    path = root / RETIRED_PRODUCER_MARKER
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise RegistrySchemaError(f"cannot read {path}: {error}") from error
    if text.lstrip().startswith("{"):
        try:
            return json.loads(text, object_pairs_hook=_refuse_duplicate_keys)
        except (ValueError, RecursionError) as error:
            raise RegistrySchemaError(f"cannot read {path}: {error}") from error
    return _load_yaml_refusing_duplicate_keys(path, text)


def wrkslots_machine(root: Path) -> str:
    """Return the configured machine shard without guessing between registries."""

    override = os.environ.get("WRKSLOTS_MACHINE")
    if override:
        return override
    path = root / RETIRED_PRODUCER_MARKER
    document = read_wrkslots_config(root)
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
