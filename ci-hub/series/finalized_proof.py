"""Exact artifact transport for finalized validation in the existing cell ledger.

These carriers are not validation authority. Callers select the terminal from
the complete canonical parent history and verify its original Git provenance.
The unchanged native schema-10 verifier then judges these exact operand bytes.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
import subprocess
from pathlib import Path
from typing import Any, Mapping

import bounded_capture

PARENT_REPOSITORY = "https://github.com/rrnewton/dev-hermit.git"
LEDGER_REPOSITORY = "https://github.com/rrnewton/hermit_test_ledger.git"
PREFIX = "scorecard/finalized"
ROLES = ("plan", "cells", "tests")
# Same operational per-row capacity as lib/validation_evidence.rs. Exhaustion
# is unavailable transport, not a verdict about a valid larger producer record.
ARTIFACT_BYTES_PER_ROW = 256 * 1024 * 1024
_SHA = re.compile(r"[0-9a-f]{40}")
_DIGEST = re.compile(r"[0-9a-f]{64}")
_OWNER_PATH = re.compile(
    r"(?:/home/[^/\s]+|/Users/[^/\s]+|/root)(?:/|$)"
    r"|[A-Z]:[\\/](?:Users|Documents and Settings)[\\/][^\\/\s]+", re.I)
_CREDENTIAL = re.compile(
    r"\bauthorization\s*[:=]\s*(?:basic|bearer)\s+\S+"
    r"|(?:^|[\s;&])(?:[A-Za-z0-9]+[_-])*(?:access[_-]?(?:key|token)"
    r"|api[_-]?key|client[_-]?secret|credential|passwd|password"
    r"|private[_-]?key|secret|token)\s*[:=]\s*\S+"
    r"|\b(?:gh[pousr]|github_pat)_[A-Za-z0-9_]+\b"
    r"|https?://[^/@\s:]+:[^/@\s]+@"
    r"|-----BEGIN [A-Z ]*PRIVATE KEY-----", re.I)
_CREDENTIAL_KEY = re.compile(
    r"(?:^|[_-])(?:access[_-]?token|api[_-]?key|authorization"
    r"|client[_-]?secret|credential|passwd|password|private[_-]?key"
    r"|secret|token)(?:$|[_-])", re.I)


class ProofRefused(ValueError):
    """A carrier cannot bind the supplied canonical terminal and exact bytes."""


class ProofUnavailable(ProofRefused):
    """Required input is unavailable or exceeds the existing reader capacity."""


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False, allow_nan=False).encode()


def _object(value: Any, fields: str, label: str) -> dict:
    if not isinstance(value, dict) or set(value) != set(fields.split()):
        raise ProofRefused(f"{label} has missing or unknown fields")
    return value


def _text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip() or "\0" in value:
        raise ProofRefused(f"{label} must be a nonempty string")
    return value


def _hash(value: Any, label: str, *, git: bool = False) -> str:
    if not isinstance(value, str) or not (_SHA if git else _DIGEST).fullmatch(value):
        raise ProofRefused(f"{label} is not an exact lowercase digest")
    return value


def _relative(value: Any, *, retained: bool = False) -> str:
    value = _text(value, "artifact path")
    parts = value.split("/")
    if ("\\" in value or any(part in {"", ".", ".."} for part in parts)
            or (retained and (parts[:3] != ["ignored", "validate", "artifacts"]
                              or len(parts) < 4))):
        raise ProofRefused("artifact path is not a safe original relative identity")
    return value


def _no_duplicates(pairs: list[tuple[str, Any]]) -> dict:
    value = {}
    for key, item in pairs:
        if key in value:
            raise ProofRefused(f"duplicate JSON field {key!r}")
        value[key] = item
    return value


def _json(encoded: bytes) -> Any:
    try:
        return json.loads(encoded, object_pairs_hook=_no_duplicates,
                          parse_constant=lambda value: (_ for _ in ()).throw(
                              ProofRefused(f"nonfinite JSON value {value}")))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise ProofRefused(f"unreadable structured proof: {error}") from error


def require_publishable_strings(value: Any) -> None:
    """Apply the website's owner/credential string policy without rewriting.

Full proof operands deliberately retain measurement/report fields. The separate
website-summary forbidden-field rule therefore does not apply to these inputs.
"""
    if isinstance(value, dict):
        for key, item in value.items():
            if _CREDENTIAL_KEY.search(key):
                raise ProofRefused("proof contains a credential-bearing field")
            require_publishable_strings(key)
            require_publishable_strings(item)
    elif isinstance(value, list):
        for item in value:
            require_publishable_strings(item)
    elif isinstance(value, str):
        if _OWNER_PATH.search(value):
            raise ProofRefused("proof contains an absolute owner path")
        if _CREDENTIAL.search(value):
            raise ProofRefused("proof contains a credential-bearing string")
        # Plans and typed reports also retain structured JSON inside strings.
        # Inspect it without changing even one byte of the original operand.
        if value.lstrip().startswith(("{", "[")):
            try:
                nested = _json(value.encode())
            except ProofRefused:
                pass
            else:
                require_publishable_strings(nested)


def _structured_bytes(encoded: bytes, role: str) -> None:
    if role == "plan":
        require_publishable_strings(_json(encoded))
        return
    if encoded and not encoded.endswith(b"\n"):
        raise ProofRefused(f"{role} operand has a truncated JSONL tail")
    for line in encoded.splitlines():
        if line.strip():
            require_publishable_strings(_json(line))


def row_references(row: Mapping[str, Any]) -> dict[str, dict]:
    if type(row.get("schema_version")) is not int or row["schema_version"] != 10:
        raise ProofRefused("portable proof requires the original schema-10 row")
    try:
        refs = {"plan": row["constructed_plan"],
                "cells": row["cell_results"]["artifact"],
                "tests": row["test_results"]["artifact"]}
        for role, reference in refs.items():
            _relative(reference["path"], retained=True)
            _hash(reference["sha256"], role + " sha256")
        if type(refs["plan"]["bytes"]) is not int or refs["plan"]["bytes"] < 0:
            raise ProofRefused("constructed plan byte count is invalid")
    except (KeyError, TypeError) as error:
        raise ProofRefused("canonical row lacks original artifact references") from error
    return refs


def validate_manifest(value: Any) -> dict:
    value = _object(value, "schema terminal parent artifacts producer series", "carrier")
    if type(value["schema"]) is not int or value["schema"] != 1:
        raise ProofRefused("unsupported portable carrier schema")
    terminal = _object(value["terminal"],
                       "repository run_id record_id event_id event_sha256 hermit_commit", "terminal")
    if terminal["repository"] != "rrnewton/hermit":
        raise ProofRefused("carrier is not a Hermit terminal")
    for key in ("run_id", "record_id", "event_id"):
        _text(terminal[key], key)
    _hash(terminal["event_sha256"], "terminal event digest")
    _hash(terminal["hermit_commit"], "measured Hermit commit", git=True)
    parent = _object(value["parent"],
                     "repository commit shard blob_oid event_line_sha256", "parent provenance")
    if parent["repository"] != PARENT_REPOSITORY:
        raise ProofRefused("carrier names another parent repository")
    for key in ("commit", "blob_oid"):
        _hash(parent[key], "parent " + key, git=True)
    _hash(parent["event_line_sha256"], "parent event line digest")
    shard = _relative(parent["shard"])
    if re.fullmatch(r"ledger/[^/]+/[^/]+/\d{4}-\d{2}\.jsonl|"
                    r"ledger-recovery/[^/]+/[^/]+/\d{4}-\d{2}/[^/]+\.jsonl", shard) is None:
        raise ProofRefused("carrier parent shard is outside canonical namespaces")
    artifacts = _object(value["artifacts"], "schema plan cells tests", "artifacts")
    if type(artifacts["schema"]) is not int or artifacts["schema"] != 1:
        raise ProofRefused("unsupported artifact descriptor schema")
    for role in ROLES:
        descriptor = _object(artifacts[role], "path bytes sha256", role)
        _relative(descriptor["path"], retained=True)
        _hash(descriptor["sha256"], role + " digest")
        if type(descriptor["bytes"]) is not int or descriptor["bytes"] < 0:
            raise ProofRefused(f"{role} byte count is invalid")
    producer = _object(value["producer"], "parent_commit hermit_tool_commit", "producer")
    for key, item in producer.items():
        _hash(item, "producer " + key, git=True)
    series = _object(value["series"], "repository commit tree rows_sha256", "series")
    if series["repository"] != LEDGER_REPOSITORY:
        raise ProofRefused("carrier names another cell ledger")
    for key in ("commit", "tree"):
        _hash(series[key], "series " + key, git=True)
    _hash(series["rows_sha256"], "series rows digest")
    require_publishable_strings(value)
    return value


def carrier_path(value: Mapping[str, Any]) -> str:
    key = canonical_bytes({name: value[name] for name in ("terminal", "artifacts")})
    return f"{PREFIX}/runs/{hashlib.sha256(key).hexdigest()}.json"


def object_path(digest: str) -> str:
    return f"{PREFIX}/objects/sha256/{_hash(digest, 'object digest')}"


def carrier_identity(value: Mapping[str, Any]) -> tuple[str, ...]:
    return tuple(value["terminal"][key] for key in
                 ("repository", "run_id", "record_id", "event_id", "event_sha256"))


def carrier_for_row(index: Mapping[tuple[str, ...], dict], row: Mapping[str, Any],
                    terminal: Mapping[str, Any]) -> dict | None:
    identity = tuple(terminal[key] for key in
                     ("repository", "run_id", "record_id", "event_id", "event_sha256"))
    if any(value["terminal"]["event_id"] == terminal["event_id"]
           and key != identity for key, value in index.items()):
        raise ProofRefused("same terminal event has conflicting bodies")
    value = index.get(identity)
    if value is None:
        return None
    validate_manifest(value)
    if value["terminal"] != terminal:
        raise ProofRefused("carrier differs from canonical terminal identity")
    if row.get("repo") not in {"hermit", "rrnewton/hermit"}:
        raise ProofRefused("selected row is not the canonical Hermit repository")
    if any(terminal[key] != row[field] for key, field in
           (("run_id", "run_id"),
            ("record_id", "record_id"), ("hermit_commit", "commit"))):
        raise ProofRefused("carrier terminal differs from the selected row")
    for role, reference in row_references(row).items():
        descriptor = value["artifacts"][role]
        if any(descriptor[key] != reference[key] for key in ("path", "sha256")):
            raise ProofRefused(f"{role} descriptor differs from original row")
        if role == "plan" and descriptor["bytes"] != reference["bytes"]:
            raise ProofRefused("plan descriptor changes original byte count")
    return value


def _git(repo: Path, *arguments: str) -> bytes:
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith("GIT_")}
    environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                       GIT_NO_REPLACE_OBJECTS="1", GIT_NO_LAZY_FETCH="1")
    try:
        result = bounded_capture.capture(
            ["git", "--no-replace-objects", "-C", str(repo), *arguments],
            env=environment, text=False, timeout=60, check=False)
    except (OSError, subprocess.SubprocessError) as error:
        raise ProofUnavailable(f"cannot read immutable proof objects: {error}") from error
    if result.returncode:
        raise ProofUnavailable("immutable proof Git read failed: "
                               + result.stderr.decode(errors="replace").strip())
    return result.stdout


def _require_ledger(ledger: Path) -> None:
    root = _git(ledger, "rev-parse", "--show-toplevel").decode().strip()
    if Path(root).resolve() != ledger.resolve():
        raise ProofRefused("proof source is not the selected ledger Git root")
    if _git(ledger, "remote", "get-url", "origin").decode().strip() != LEDGER_REPOSITORY:
        raise ProofRefused("proof source has another ledger repository identity")


def _blob(repo: Path, commit: str, path: str, expected_bytes: int | None = None) -> bytes:
    _hash(commit, "ledger commit", git=True)
    _relative(path)
    listing = _git(repo, "ls-tree", "-z", commit, "--", path).split(b"\0")
    if len(listing) != 2 or listing[-1] != b"":
        raise ProofUnavailable(f"proof object is absent: {path}")
    header, name = listing[0].split(b"\t", 1)
    mode, kind, oid = header.split()
    if mode != b"100644" or kind != b"blob" or name.decode() != path:
        raise ProofRefused("proof object is not an ordinary immutable data blob")
    size = int(_git(repo, "cat-file", "-s", oid.decode()))
    if size > ARTIFACT_BYTES_PER_ROW:
        raise ProofUnavailable("proof object exceeds existing artifact reader capacity")
    if expected_bytes is not None and size != expected_bytes:
        raise ProofRefused("proof object length differs from its descriptor")
    encoded = _git(repo, "cat-file", "blob", oid.decode())
    if len(encoded) != size:
        raise ProofRefused("Git proof object length changed")
    return encoded


def read_carriers(ledger: Path, commit: str) -> dict[tuple[str, ...], dict]:
    """Read immutable metadata only; canonical-parent authority is separate."""
    _hash(commit, "ledger commit", git=True)
    _require_ledger(ledger)
    listing = _git(ledger, "ls-tree", "-rz", commit, "--", f"{PREFIX}/runs")
    index: dict[tuple[str, ...], dict] = {}
    events: dict[str, tuple[str, ...]] = {}
    for entry in listing.split(b"\0"):
        if not entry:
            continue
        _header, name = entry.split(b"\t", 1)
        path = name.decode("utf-8")
        encoded = _blob(ledger, commit, path)
        value = validate_manifest(_json(encoded))
        if path != carrier_path(value) or encoded != canonical_bytes(value) + b"\n":
            raise ProofRefused("carrier filename or canonical bytes do not match its identity")
        identity = carrier_identity(value)
        event_id = value["terminal"]["event_id"]
        if event_id in events and events[event_id] != identity:
            raise ProofRefused("same terminal event has conflicting bodies")
        if identity in index and index[identity] != value:
            raise ProofRefused("same terminal has conflicting artifact/provenance carriers")
        events[event_id] = identity
        index[identity] = value
    return index


def read_artifact_objects(ledger: Path, commit: str, manifest: Mapping[str, Any]) -> dict[str, bytes]:
    validate_manifest(manifest)
    _require_ledger(ledger)
    objects: dict[str, bytes] = {}
    total = 0
    for role in ROLES:
        descriptor = manifest["artifacts"][role]
        total += descriptor["bytes"]
        if total > ARTIFACT_BYTES_PER_ROW:
            raise ProofUnavailable("proof exceeds existing aggregate artifact reader capacity")
        encoded = _blob(ledger, commit, object_path(descriptor["sha256"]), descriptor["bytes"])
        if hashlib.sha256(encoded).hexdigest() != descriptor["sha256"]:
            raise ProofRefused("proof object digest differs from descriptor")
        _structured_bytes(encoded, role)
        path = descriptor["path"]
        if path in objects and objects[path] != encoded:
            raise ProofRefused("original artifact path has conflicting bytes")
        objects[path] = encoded
    return objects


def _open_parent(root: Path, relative: str, *, create: bool) -> tuple[int, str]:
    parts = _relative(relative).split("/")
    descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in parts[:-1]:
            if create:
                try:
                    os.mkdir(part, mode=0o700, dir_fd=descriptor)
                except FileExistsError:
                    pass
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                            dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        return descriptor, parts[-1]
    except BaseException:
        os.close(descriptor)
        raise


def _read_local(root: Path, relative: str) -> bytes:
    parent, name = _open_parent(root, relative, create=False)
    try:
        descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        with os.fdopen(descriptor, "rb") as source:
            metadata = os.fstat(source.fileno())
            if not stat.S_ISREG(metadata.st_mode):
                raise ProofRefused("proof input is not a regular file")
            if metadata.st_size > ARTIFACT_BYTES_PER_ROW:
                raise ProofUnavailable("proof input exceeds existing artifact reader capacity")
            encoded = source.read(ARTIFACT_BYTES_PER_ROW + 1)
            if len(encoded) != metadata.st_size:
                raise ProofRefused("proof input length changed while reading")
            return encoded
    finally:
        os.close(parent)


def capture_artifacts(state: Path, row: Mapping[str, Any]) -> tuple[dict, dict[str, bytes]]:
    artifacts: dict[str, Any] = {"schema": 1}
    objects: dict[str, bytes] = {}
    total = 0
    for role, reference in row_references(row).items():
        try:
            encoded = _read_local(state, reference["path"])
        except OSError as error:
            raise ProofUnavailable(f"cannot read original {role} artifact: {error}") from error
        total += len(encoded)
        if total > ARTIFACT_BYTES_PER_ROW:
            raise ProofUnavailable("proof exceeds existing aggregate artifact reader capacity")
        digest = hashlib.sha256(encoded).hexdigest()
        if digest != reference["sha256"] or (role == "plan" and len(encoded) != reference["bytes"]):
            raise ProofRefused(f"original {role} bytes differ from canonical row")
        _structured_bytes(encoded, role)
        artifacts[role] = {"path": reference["path"], "bytes": len(encoded), "sha256": digest}
        objects[object_path(digest)] = encoded
    return artifacts, objects


def _stage_files(root: Path, objects: Mapping[str, bytes]) -> None:
    for path, encoded in objects.items():
        _relative(path)
        parent, name = _open_parent(root, path, create=True)
        try:
            try:
                descriptor = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                                     0o600, dir_fd=parent)
            except FileExistsError:
                if _read_local(root, path) != encoded:
                    raise ProofRefused("proof destination already has different bytes")
            else:
                with os.fdopen(descriptor, "wb") as output:
                    output.write(encoded)
                    output.flush()
                    os.fsync(output.fileno())
        finally:
            os.close(parent)


def stage_artifacts(root: Path, objects: Mapping[str, bytes]) -> None:
    """Create exact private transport files; never overwrite a conflicting path."""
    for path in objects:
        _relative(path, retained=True)
    _stage_files(root, objects)


def make_manifest(selection: Mapping[str, Any], artifacts: Mapping[str, Any], *,
                  parent_commit: str, hermit_tool_commit: str,
                  snapshot: Mapping[str, Any]) -> dict:
    """Describe bytes already checked by the actual finalized native writer."""
    value = {
        "schema": 1, "terminal": selection["terminal"], "parent": selection["parent"],
        "artifacts": artifacts,
        "producer": {"parent_commit": parent_commit, "hermit_tool_commit": hermit_tool_commit},
        "series": {"repository": LEDGER_REPOSITORY,
                   "commit": snapshot["source"]["commit"],
                   "tree": snapshot["source"]["tree"],
                   "rows_sha256": snapshot["rows_sha256"]},
    }
    value = validate_manifest(_json(canonical_bytes(value)))
    carrier_for_row({carrier_identity(value): value}, selection["row"], selection["terminal"])
    return value


def stage_carrier(writer: Path, manifest: Mapping[str, Any], objects: Mapping[str, bytes],
                  *, existing: Mapping[str, Any] | None = None) -> dict[str, str]:
    """Stage after strict proof and origin recheck, under the existing writer lock.

An identical retry keeps the original carrier's provenance byte-for-byte. The
caller must revalidate that original provenance through the canonical query.
"""
    validate_manifest(manifest)
    if existing is not None:
        validate_manifest(existing)
        if any(existing[key] != manifest[key] for key in ("terminal", "artifacts")):
            raise ProofRefused("retry changes the same terminal or proof operands")
        manifest = existing
    expected = {}
    for role in ROLES:
        descriptor = manifest["artifacts"][role]
        path = object_path(descriptor["sha256"])
        encoded = objects.get(path)
        if (encoded is None or len(encoded) != descriptor["bytes"]
                or hashlib.sha256(encoded).hexdigest() != descriptor["sha256"]):
            raise ProofRefused("staged proof objects differ from the exact carrier")
        _structured_bytes(encoded, role)
        expected[path] = encoded
    if set(objects) != set(expected):
        raise ProofRefused("unreferenced object supplied for carrier publication")
    expected[carrier_path(manifest)] = canonical_bytes(manifest) + b"\n"
    _stage_files(writer, expected)
    return {path: hashlib.sha256(encoded).hexdigest() for path, encoded in expected.items()}
