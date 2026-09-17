#!/usr/bin/env python3
"""Bind one published evidence file to a portable path, a digest and a length.

A compatibility test-detail page links to evidence inside a released artifact.
Three things have to be true of every such link, and none of them is true by
construction:

* the path must stay INSIDE the artifact and must be portable -- an absolute
  path, a ``..`` segment or a symlink either escapes the artifact or resolves
  somewhere else on whichever machine extracts it;
* the bytes must be the bytes the binding claims -- a digest or a length that
  does not match its file is a link that verifies on the producer and fails for
  every reader;
* the binding must be RECORDED rather than reconstructed. The task is explicit:
  old rows stay honestly missing, with no historical backfill and no fabricated
  URLs.

⚠️ THE THIRD IS THE ONE WITH NO TECHNICAL SYMPTOM. A forged digest fails
verification the first time anyone checks it. A backfilled binding VERIFIES
PERFECTLY -- it is internally consistent, it just describes bytes that were never
the evidence for that row. So this module refuses to construct a binding from
anything but the file in front of it, and :func:`parse_binding` refuses a record
that carries no schema rather than treating the absence as a shape it can fill
in.

WHAT THIS MODULE DOES NOT DO
----------------------------
It binds a FILE. Which cell, run and attempt that file is evidence for is the job
of the cell evidence binding, which is not on main at the time of writing; those
identity fields are carried through as opaque strings and are not interpreted
here. Redaction and the publish/withhold decision are
``evidence_redaction`` and ``evidence_publication``; the intended order is
redact, bind, decide, place.
"""

from __future__ import annotations

import hashlib
import os
import re
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Final, Mapping


EVIDENCE_BINDING_SCHEMA: Final = 1
"""Forward-only. A row without this key predates the binding and stays missing.

The task's design is explicit that new evidence gets a new strict schema and that
old rows are preserved as honestly missing. A reader must be able to tell "this
run never recorded evidence" from "this run recorded evidence and here it is",
and a default would erase exactly that distinction.
"""

_SHA256_RE: Final = re.compile(r"^[0-9a-f]{64}$")

#: Deliberately small. An evidence file is one of three known kinds, and an open
#: media-type field is an invitation to publish something nobody reviewed.
MEDIA_TYPES: Final = frozenset({"text/plain"})
ENCODINGS: Final = frozenset({"utf-8", "binary"})


class ArtifactRefused(RuntimeError):
    """The file or the binding is not safe to publish, so it is not published."""


@dataclass(frozen=True)
class EvidenceBinding:
    """One published file, bound so a reader can verify it independently."""

    schema: int
    artifact_path: str
    sha256: str
    byte_length: int
    media_type: str
    encoding: str

    def to_json(self) -> dict[str, Any]:
        return {
            "schema": self.schema,
            "artifact_path": self.artifact_path,
            "sha256": self.sha256,
            "byte_length": self.byte_length,
            "media_type": self.media_type,
            "encoding": self.encoding,
        }


def artifact_relative_path(*, artifact_root: Path, path: Path) -> str:
    """The portable path of ``path`` inside ``artifact_root``.

    ⚠️ EVERY COMPONENT IS CHECKED, NOT ONLY THE LAST. A symlink partway up the
    chain relocates the whole subtree, and ``Path.is_symlink`` on the leaf says
    nothing about its parents. The check walks from the root down.

    ⚠️ AND `resolve()` ALONE IS NOT THE CHECK. It silently FOLLOWS symlinks, so a
    resolved path that lands inside the root proves the target is inside -- not
    that the route was. Both the literal and the resolved path must be inside,
    and no component may be a link.
    """

    root = Path(artifact_root)
    if not root.is_absolute():
        raise ArtifactRefused(f"artifact root must be absolute: {artifact_root}")
    try:
        resolved_root = root.resolve(strict=True)
    except OSError as error:
        raise ArtifactRefused(f"artifact root is unusable: {error}") from error
    if not resolved_root.is_dir():
        raise ArtifactRefused(f"artifact root is not a directory: {artifact_root}")

    candidate = Path(path)
    if not candidate.is_absolute():
        candidate = root / candidate
    # Lexical containment first: this is what rejects a `..` segment, and it
    # rejects it whether or not the traversed directory happens to exist.
    try:
        relative = candidate.relative_to(root)
    except ValueError as error:
        raise ArtifactRefused(
            f"evidence path escapes the artifact root: {path}"
        ) from error
    if any(part == ".." for part in relative.parts):
        raise ArtifactRefused(f"evidence path traverses upward: {path}")
    if not relative.parts:
        raise ArtifactRefused("evidence path is the artifact root itself")

    cursor = root
    for part in relative.parts:
        cursor = cursor / part
        if cursor.is_symlink():
            raise ArtifactRefused(
                f"evidence path crosses a symlink at {cursor}; a link is not "
                "portable inside a released artifact"
            )

    try:
        resolved = candidate.resolve(strict=True)
    except OSError as error:
        raise ArtifactRefused(f"evidence file is unreadable: {error}") from error
    if resolved_root != resolved and resolved_root not in resolved.parents:
        raise ArtifactRefused(f"evidence path resolves outside the artifact: {path}")
    if not resolved.is_file():
        raise ArtifactRefused(f"evidence path is not a regular file: {path}")

    return str(PurePosixPath(*relative.parts))


def bind_published_file(
    *,
    artifact_root: Path,
    path: Path,
    media_type: str = "text/plain",
    encoding: str = "binary",
) -> EvidenceBinding:
    """Bind the file that is actually there, by reading it.

    The digest and the length come from the bytes on disk rather than from a
    caller-supplied value, so a binding cannot describe a file that was never
    written. Deterministic: the same bytes at the same path always bind the same.
    """

    if media_type not in MEDIA_TYPES:
        raise ArtifactRefused(f"unknown media type {media_type!r}")
    if encoding not in ENCODINGS:
        raise ArtifactRefused(f"unknown encoding {encoding!r}")
    relative = artifact_relative_path(artifact_root=artifact_root, path=path)
    data = (Path(artifact_root) / relative).read_bytes()
    return EvidenceBinding(
        schema=EVIDENCE_BINDING_SCHEMA,
        artifact_path=relative,
        sha256=hashlib.sha256(data).hexdigest(),
        byte_length=len(data),
        media_type=media_type,
        encoding=encoding,
    )


def parse_binding(value: Mapping[str, Any]) -> EvidenceBinding:
    """Read a recorded binding, refusing anything this writer did not produce.

    ⚠️ A MISSING SCHEMA IS A LEGACY ROW AND IS REFUSED, NOT DEFAULTED. Defaulting
    it would silently promote every historical row into the new contract -- a
    backfill with no technical symptom, because the result is internally
    consistent and merely untrue.
    """

    if not isinstance(value, Mapping):
        raise ArtifactRefused("evidence binding is not an object")
    if "schema" not in value:
        raise ArtifactRefused(
            "evidence binding records no schema; this row predates the binding "
            "and its evidence is missing rather than reconstructable"
        )
    schema = value.get("schema")
    if schema != EVIDENCE_BINDING_SCHEMA:
        raise ArtifactRefused(f"unsupported evidence binding schema {schema!r}")

    expected = {
        "schema",
        "artifact_path",
        "sha256",
        "byte_length",
        "media_type",
        "encoding",
    }
    if set(value) != expected:
        missing = sorted(expected - set(value))
        unknown = sorted(set(value) - expected)
        raise ArtifactRefused(
            f"evidence binding shape is wrong; missing={missing} unknown={unknown}"
        )

    artifact_path = value["artifact_path"]
    if not isinstance(artifact_path, str) or not artifact_path:
        raise ArtifactRefused("artifact_path must be a nonempty string")
    if artifact_path.startswith("/") or "\\" in artifact_path:
        raise ArtifactRefused(f"artifact_path must be relative and posix: {artifact_path!r}")
    if any(part in ("..", "", ".") for part in artifact_path.split("/")):
        raise ArtifactRefused(f"artifact_path is not normalised: {artifact_path!r}")

    sha256 = value["sha256"]
    if not isinstance(sha256, str) or not _SHA256_RE.fullmatch(sha256):
        raise ArtifactRefused("sha256 must be 64 lowercase hex characters")

    byte_length = value["byte_length"]
    if (
        not isinstance(byte_length, int)
        or isinstance(byte_length, bool)
        or byte_length < 0
    ):
        raise ArtifactRefused("byte_length must be a non-negative integer")

    media_type = value["media_type"]
    if media_type not in MEDIA_TYPES:
        raise ArtifactRefused(f"unknown media type {media_type!r}")
    encoding = value["encoding"]
    if encoding not in ENCODINGS:
        raise ArtifactRefused(f"unknown encoding {encoding!r}")

    return EvidenceBinding(
        schema=schema,
        artifact_path=artifact_path,
        sha256=sha256,
        byte_length=byte_length,
        media_type=media_type,
        encoding=encoding,
    )


def verify_binding(binding: EvidenceBinding, *, artifact_root: Path) -> None:
    """Prove the file on disk is the file the binding describes.

    ⚠️ LENGTH IS CHECKED AS WELL AS DIGEST, AND THAT IS NOT REDUNDANT FOR THE
    REASON IT LOOKS. A digest already detects truncation, but the length is what
    lets the FAILURE say which of the two happened: a short file with a
    mismatched digest is almost always truncation, and reporting it as "digest
    mismatch" sends the reader looking for corruption instead of a cut-off write.
    """

    resolved = artifact_relative_path(
        artifact_root=artifact_root,
        path=Path(artifact_root) / binding.artifact_path,
    )
    if resolved != binding.artifact_path:
        raise ArtifactRefused(
            f"artifact path is not canonical: recorded {binding.artifact_path!r}, "
            f"resolves to {resolved!r}"
        )
    data = (Path(artifact_root) / resolved).read_bytes()
    if len(data) != binding.byte_length:
        raise ArtifactRefused(
            f"{binding.artifact_path}: recorded {binding.byte_length} bytes, found "
            f"{len(data)}"
            + (" -- the published file is truncated" if len(data) < binding.byte_length else "")
        )
    observed = hashlib.sha256(data).hexdigest()
    if observed != binding.sha256:
        raise ArtifactRefused(
            f"{binding.artifact_path}: recorded digest {binding.sha256}, found {observed}"
        )
