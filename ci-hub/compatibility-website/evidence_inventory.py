#!/usr/bin/env python3
"""Bind EVERY published evidence file, in both directions, under one digest.

Per-file bindings prove that each file a page links to is the file it claims to
be. They cannot prove anything about the SET. Two set-level failures survive a
tree of individually perfect bindings:

* A DANGLING ENTRY -- the inventory lists evidence the artifact does not contain.
  The page renders a link and the reader gets a 404, which reads as a broken site
  rather than as missing evidence.
* AN UNLISTED FILE -- the artifact contains evidence the inventory does not
  mention. ⚠️ THIS IS THE DANGEROUS ONE. Nothing 404s, nothing fails to verify,
  and a reader who finds the file has no way to tell whether it is this run's
  evidence, a leftover from a previous build, or something that was never meant
  to be public at all. Every individual binding can be correct while the artifact
  publishes bytes nobody bound.

The task's acceptance says the inventory and build provenance must bind EVERY
published evidence file. "Every" is the both-directions requirement, and
:func:`verify_inventory` checks both.

DETERMINISM
-----------
Entries are stored sorted by path and the digest is taken over canonical JSON
with sorted keys and no insignificant whitespace -- the same spelling
``ci-hub/qualifying_receipt.py`` and ``ci-hub/health/residue_sweep.py`` already
use. So a rebuild from the same source bytes produces a byte-identical inventory
and the same digest, which is the task's stated rebuild property applied to the
set rather than to one file.

ORDER
-----
Fourth of four. ``evidence_redaction`` removes host identity, ``evidence_artifact``
binds one file, ``evidence_publication`` decides whether a run's evidence may be
shown at all, and this binds the collection. Building an inventory over
un-redacted bytes would publish host identity under a digest that certifies it.
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final, Iterable, Mapping

import evidence_artifact
from evidence_artifact import ArtifactRefused, EvidenceBinding


INVENTORY_SCHEMA: Final = 1

#: The DIGEST pattern -- sha256, 64 hex.
_SHA_RE: Final = evidence_artifact._SHA256_RE
#: The COMMIT pattern -- git sha1, 40 hex. Deliberately separate: reusing the
#: digest pattern for a commit refuses every real commit, which is how this was
#: written the first time and how the test caught it.
_COMMIT_RE: Final = re.compile(r"^[0-9a-f]{40}$")


class InventoryRefused(RuntimeError):
    """The published set does not match its inventory, so it is not published."""


@dataclass(frozen=True)
class BuildProvenance:
    """What produced this artifact, recorded so a reader need not guess."""

    generator: str
    source_commit: str

    def to_json(self) -> dict[str, Any]:
        return {"generator": self.generator, "source_commit": self.source_commit}


@dataclass(frozen=True)
class Inventory:
    schema: int
    provenance: BuildProvenance
    entries: tuple[EvidenceBinding, ...]

    def body(self) -> dict[str, Any]:
        """Everything the digest covers. The digest itself is never inside it."""

        return {
            "schema": self.schema,
            "provenance": self.provenance.to_json(),
            "entries": [entry.to_json() for entry in self.entries],
        }

    def digest(self) -> str:
        return hashlib.sha256(canonical_bytes(self.body())).hexdigest()

    def to_json(self) -> dict[str, Any]:
        return {**self.body(), "inventory_sha256": self.digest()}


def canonical_bytes(value: Any) -> bytes:
    """One stable serialization, so the digest is a function of the content."""

    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("utf-8")


def build_inventory(
    *,
    artifact_root: Path,
    evidence_root: Path,
    generator: str,
    source_commit: str,
) -> Inventory:
    """Bind every regular file under ``evidence_root``, refusing anything unsafe.

    ⚠️ IT WALKS THE TREE RATHER THAN TAKING A LIST OF FILES, and that is the
    point. An inventory built from the caller's list can only ever contain what
    the caller remembered; the unlisted-file failure is precisely the file the
    caller forgot. Discovering the set from the artifact itself is what makes
    "every" checkable instead of aspirational.
    """

    if not generator or not isinstance(generator, str):
        raise InventoryRefused("build provenance requires a generator")
    if not isinstance(source_commit, str) or not _COMMIT_RE.fullmatch(source_commit):
        raise InventoryRefused(
            f"build provenance requires a full 40-hex commit sha, got {source_commit!r}"
        )

    root = Path(artifact_root)
    evidence = Path(evidence_root)
    if not evidence.is_absolute():
        evidence = root / evidence
    # Route the evidence root through the same path guard as a file, so a
    # symlinked or escaping evidence directory is refused before it is walked.
    if evidence.is_symlink():
        raise InventoryRefused(f"evidence root is a symlink: {evidence_root}")
    try:
        evidence.relative_to(root)
    except ValueError as error:
        raise InventoryRefused(
            f"evidence root is outside the artifact: {evidence_root}"
        ) from error
    if not evidence.is_dir():
        raise InventoryRefused(f"evidence root is not a directory: {evidence_root}")

    bindings: list[EvidenceBinding] = []
    for path in sorted(evidence.rglob("*")):
        if path.is_dir() and not path.is_symlink():
            continue
        try:
            bindings.append(
                evidence_artifact.bind_published_file(artifact_root=root, path=path)
            )
        except ArtifactRefused as error:
            # Refuse the whole build. Skipping the offending file would publish
            # an artifact whose inventory is silently incomplete, which is the
            # unlisted-file failure arriving by a different route.
            raise InventoryRefused(
                f"cannot bind published evidence {path}: {error}"
            ) from error

    return Inventory(
        schema=INVENTORY_SCHEMA,
        provenance=BuildProvenance(generator=generator, source_commit=source_commit),
        entries=tuple(sorted(bindings, key=lambda entry: entry.artifact_path)),
    )


def parse_inventory(value: Mapping[str, Any]) -> Inventory:
    """Read a recorded inventory, checking its own digest before anything else."""

    if not isinstance(value, Mapping):
        raise InventoryRefused("inventory is not an object")
    expected = {"schema", "provenance", "entries", "inventory_sha256"}
    if set(value) != expected:
        raise InventoryRefused(
            f"inventory shape is wrong; missing={sorted(expected - set(value))} "
            f"unknown={sorted(set(value) - expected)}"
        )
    if value["schema"] != INVENTORY_SCHEMA:
        raise InventoryRefused(f"unsupported inventory schema {value['schema']!r}")

    raw_provenance = value["provenance"]
    if not isinstance(raw_provenance, Mapping) or set(raw_provenance) != {
        "generator",
        "source_commit",
    }:
        raise InventoryRefused("build provenance shape is wrong")
    generator = raw_provenance["generator"]
    source_commit = raw_provenance["source_commit"]
    if not isinstance(generator, str) or not generator:
        raise InventoryRefused("build provenance requires a generator")
    if not isinstance(source_commit, str) or not _COMMIT_RE.fullmatch(source_commit):
        raise InventoryRefused("build provenance requires a full 40-hex commit sha")

    raw_entries = value["entries"]
    if not isinstance(raw_entries, list):
        raise InventoryRefused("inventory entries must be a list")
    entries = tuple(evidence_artifact.parse_binding(entry) for entry in raw_entries)

    paths = [entry.artifact_path for entry in entries]
    if len(set(paths)) != len(paths):
        raise InventoryRefused("inventory lists the same artifact path twice")
    if paths != sorted(paths):
        raise InventoryRefused("inventory entries are not in canonical order")

    inventory = Inventory(
        schema=INVENTORY_SCHEMA,
        provenance=BuildProvenance(generator=generator, source_commit=source_commit),
        entries=entries,
    )
    recorded = value["inventory_sha256"]
    if not isinstance(recorded, str) or not _SHA_RE.fullmatch(recorded):
        raise InventoryRefused("inventory_sha256 must be 64 lowercase hex characters")
    if recorded != inventory.digest():
        raise InventoryRefused(
            f"inventory digest does not cover its own body: recorded {recorded}, "
            f"computed {inventory.digest()}"
        )
    return inventory


def verify_inventory(
    inventory: Inventory, *, artifact_root: Path, evidence_root: Path
) -> None:
    """Prove the published set is exactly the inventoried set, both ways."""

    root = Path(artifact_root)
    evidence = Path(evidence_root)
    if not evidence.is_absolute():
        evidence = root / evidence

    for entry in inventory.entries:
        try:
            evidence_artifact.verify_binding(entry, artifact_root=root)
        except ArtifactRefused as error:
            raise InventoryRefused(
                f"inventoried evidence does not verify: {error}"
            ) from error

    listed = {entry.artifact_path for entry in inventory.entries}
    present = {
        str(path.relative_to(root).as_posix())
        for path in evidence.rglob("*")
        if path.is_file() or path.is_symlink()
    }
    unlisted = sorted(present - listed)
    if unlisted:
        raise InventoryRefused(
            "the artifact publishes evidence the inventory does not bind: "
            + ", ".join(unlisted[:5])
            + (f" (and {len(unlisted) - 5} more)" if len(unlisted) > 5 else "")
        )
    # A dangling entry is already caught by verify_binding above, which refuses
    # an unreadable file. Named here so the two directions are both visible.
    dangling = sorted(listed - present)
    if dangling:
        raise InventoryRefused(
            "the inventory binds evidence the artifact does not contain: "
            + ", ".join(dangling[:5])
        )


def published_paths(inventory: Inventory) -> Iterable[str]:
    """The artifact-relative paths a page may link to, and no others."""

    return (entry.artifact_path for entry in inventory.entries)
