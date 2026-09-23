"""Exact artifact transport for finalized validation in the existing cell ledger.

These carriers are not validation authority. Callers select the terminal from
the complete canonical parent history and verify its original Git provenance.
The unchanged native schema-10 verifier then judges those exact operand bytes.
The separate derived-census role carries a native writer's public projection,
not the private raw preimages that the writer verified before deriving it.
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
DERIVED_AUTHORITY = "writer_verified_raw_census_v1"
HISTORY_BINDING_FIELDS = (
    "schema_version", "run_id", "record_id", "commit", "tree_dirty", "commit_anchored",
    "profile", "selection_mode", "started_at", "finished_at", "result", "exit_code",
    "validation_complete", "raw_result_input_census_v1",
)
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
    if role == "derived_cells":
        value = _json(encoded)
        if encoded != canonical_bytes(value) + b"\n":
            raise ProofRefused("derived cells must contain one canonical JSON object line")
        _object(value, "schema authority run_id record_id hermit_sha history_binding_sha256 "
                "raw_census_sha256 attempts", "derived cells")
        if type(value["schema"]) is not int or value["schema"] != 1:
            raise ProofRefused("unsupported derived cells schema")
        if value["authority"] != DERIVED_AUTHORITY:
            raise ProofRefused("derived cells lack the explicit writer-derived authority")
        for key in ("run_id", "record_id"):
            _text(value[key], "derived " + key)
        _hash(value["hermit_sha"], "derived Hermit commit", git=True)
        for key in ("history_binding_sha256", "raw_census_sha256"):
            _hash(value[key], "derived " + key)
        if not isinstance(value["attempts"], list):
            raise ProofRefused("derived attempts must be an array")
        for attempt in value["attempts"]:
            _object(attempt, "input_path input_line cell attempt source_row_sha256 "
                    "outcome result failure_class ordinary_verdict backend_parity", "derived attempt")
            _relative(attempt["input_path"])
            for key in ("input_line", "attempt"):
                if type(attempt[key]) is not int or attempt[key] < 1:
                    raise ProofRefused("derived attempt needs positive line and attempt identities")
            _object(attempt["cell"], "lane category test mode backend", "derived cell")
            for key, item in attempt["cell"].items():
                _text(item, "derived cell " + key)
            _hash(attempt["source_row_sha256"], "derived source row digest")
        _no_raw_derived_fields(value)
        require_publishable_strings(value)
        return
    if role == "plan":
        require_publishable_strings(_json(encoded))
        return
    if encoded and not encoded.endswith(b"\n"):
        raise ProofRefused(f"{role} operand has a truncated JSONL tail")
    for line in encoded.splitlines():
        if line.strip():
            require_publishable_strings(_json(line))


def _no_raw_derived_fields(value: Any) -> None:
    # Typed comparison interpretation remains native. This separate publication
    # boundary must never transport invocation streams or raw diagnostic bodies,
    # even when they contain no owner path or recognizable credential.
    if isinstance(value, dict):
        if set(value) & {"stdout", "stderr", "verification_report", "raw_report", "diagnostics",
                         "error_detail", "argv", "guest_argv", "env", "cwd", "shell_command"}:
            raise ProofRefused("derived comparison object contains a raw stream or diagnostic field")
        for item in value.values():
            _no_raw_derived_fields(item)
    elif isinstance(value, list):
        for item in value:
            _no_raw_derived_fields(item)


def artifact_roles(artifacts: Mapping[str, Any]) -> tuple[str, ...]:
    schema = artifacts.get("schema")
    if type(schema) is int and schema == 1:
        return ROLES
    if type(schema) is int and schema == 2:
        return ("derived_cells",)
    raise ProofRefused("unsupported artifact descriptor schema")


def derived_row_binding(row: Mapping[str, Any]) -> dict[str, str]:
    """Bind unchanged path-independent fields; terminal authority binds the full row.

    This does not establish raw validity. Only the native writer with the complete
    original inputs can produce the separate writer-derived comparison operands.
    """
    if type(row.get("schema_version")) is not int or row["schema_version"] != 5:
        raise ProofRefused("derived census transport requires the original schema-5 row")
    if row.get("cell_results") is not None:
        raise ProofRefused("derived census cannot replace original cell result evidence")
    census = _object(row.get("raw_result_input_census_v1"),
                     "schema run_id hermit_sha files", "original raw census")
    if (type(census["schema"]) is not int or census["schema"] != 1
            or not isinstance(census["files"], list)
            or "raw_result_input_census_error" in row):
        raise ProofRefused("original raw census is missing, malformed, or contradicts an error")
    for field, original in (("run_id", "run_id"), ("hermit_sha", "commit")):
        if census[field] != row.get(original):
            raise ProofRefused("original raw census differs from the selected terminal")
    run_id = _text(row.get("run_id"), "derived run identity")
    if "/" in run_id or "\\" in run_id or run_id in {".", ".."}:
        raise ProofRefused("derived run identity is not a safe path component")
    return {
        "run_id": run_id,
        "record_id": _text(row.get("record_id"), "derived record identity"),
        "hermit_sha": _hash(row.get("commit"), "derived measured source", git=True),
        "history_binding_sha256": hashlib.sha256(canonical_bytes(
            {key: row.get(key) for key in HISTORY_BINDING_FIELDS})).hexdigest(),
        "raw_census_sha256": hashlib.sha256(canonical_bytes(census)).hexdigest(),
    }


def derived_path(binding: Mapping[str, str]) -> str:
    return _relative("ignored/validate/artifacts/" + binding["run_id"]
                     + "/website-derived-census-" + binding["raw_census_sha256"]
                     + "-" + binding["history_binding_sha256"] + ".json",
                     retained=True)


def validate_derived_cells(encoded: bytes, row: Mapping[str, Any]) -> dict:
    """Join public derived spans to the exact census; do not infer comparisons."""
    _structured_bytes(encoded, "derived_cells")
    value = _json(encoded)
    binding = derived_row_binding(row)
    if any(value[key] != item for key, item in binding.items()):
        raise ProofRefused("derived cells differ from the original terminal/census binding")
    expected = []
    try:
        for source in row["raw_result_input_census_v1"]["files"]:
            for entry in source["rows"]:
                expected.append((source["path"], entry["line"], entry["cell"], entry["attempt"]))
    except (KeyError, TypeError) as error:
        raise ProofRefused("original raw census has malformed attempt spans") from error
    actual = [(entry["input_path"], entry["input_line"], entry["cell"], entry["attempt"])
              for entry in value["attempts"]]
    if actual != expected:
        raise ProofRefused("derived attempts differ from the complete original census spans")
    return value


def capture_derived_cells(row: Mapping[str, Any], encoded: bytes) -> tuple[dict, dict[str, bytes]]:
    if len(encoded) > ARTIFACT_BYTES_PER_ROW:
        raise ProofUnavailable("derived census exceeds existing artifact reader capacity")
    value = validate_derived_cells(encoded, row)
    digest = hashlib.sha256(encoded).hexdigest()
    artifacts = {"schema": 2, "raw_census_sha256": value["raw_census_sha256"],
                 "history_binding_sha256": value["history_binding_sha256"],
                 "derived_cells": {"path": derived_path(value), "bytes": len(encoded),
                                   "sha256": digest}}
    return artifacts, {object_path(digest): encoded}


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
    artifacts = value["artifacts"]
    if not isinstance(artifacts, dict):
        raise ProofRefused("artifacts must be an object")
    roles = artifact_roles(artifacts)
    _object(artifacts, "schema plan cells tests" if artifacts["schema"] == 1
            else "schema raw_census_sha256 history_binding_sha256 derived_cells", "artifacts")
    if artifacts["schema"] == 2:
        _hash(artifacts["raw_census_sha256"], "original raw census digest")
        _hash(artifacts["history_binding_sha256"], "original history binding digest")
        if any(part in terminal["run_id"] for part in ("/", "\\")) or terminal["run_id"] in {".", ".."}:
            raise ProofRefused("derived run identity is not a safe path component")
    for role in roles:
        descriptor = _object(artifacts[role], "path bytes sha256", role)
        _relative(descriptor["path"], retained=True)
        _hash(descriptor["sha256"], role + " digest")
        if type(descriptor["bytes"]) is not int or descriptor["bytes"] < 0:
            raise ProofRefused(f"{role} byte count is invalid")
    if artifacts["schema"] == 2 and artifacts["derived_cells"]["path"] != derived_path({
        "run_id": terminal["run_id"], "raw_census_sha256": artifacts["raw_census_sha256"],
        "history_binding_sha256": artifacts["history_binding_sha256"],
    }):
        raise ProofRefused("derived descriptor changes the deterministic census path")
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


def validate_artifact_bytes(encoded: bytes, role: str, manifest: Mapping[str, Any]) -> None:
    _structured_bytes(encoded, role)
    if role == "derived_cells":
        value = _json(encoded)
        terminal = manifest["terminal"]
        if (any(value[key] != terminal[field] for key, field in
                (("run_id", "run_id"), ("record_id", "record_id"), ("hermit_sha", "hermit_commit")))
                or value["raw_census_sha256"] != manifest["artifacts"]["raw_census_sha256"]
                or value["history_binding_sha256"] != manifest["artifacts"]["history_binding_sha256"]):
            raise ProofRefused("derived object differs from the carrier terminal/census")


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
    if value["artifacts"]["schema"] == 2:
        binding = derived_row_binding(row)
        if (value["artifacts"]["raw_census_sha256"] != binding["raw_census_sha256"]
                or value["artifacts"]["history_binding_sha256"] != binding["history_binding_sha256"]
                or value["artifacts"]["derived_cells"]["path"] != derived_path(binding)):
            raise ProofRefused("derived descriptor differs from the original census")
        return value
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
    for role in artifact_roles(manifest["artifacts"]):
        descriptor = manifest["artifacts"][role]
        total += descriptor["bytes"]
        if total > ARTIFACT_BYTES_PER_ROW:
            raise ProofUnavailable("proof exceeds existing aggregate artifact reader capacity")
        encoded = _blob(ledger, commit, object_path(descriptor["sha256"]), descriptor["bytes"])
        if hashlib.sha256(encoded).hexdigest() != descriptor["sha256"]:
            raise ProofRefused("proof object digest differs from descriptor")
        validate_artifact_bytes(encoded, role, manifest)
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
    for role in artifact_roles(manifest["artifacts"]):
        descriptor = manifest["artifacts"][role]
        path = object_path(descriptor["sha256"])
        encoded = objects.get(path)
        if (encoded is None or len(encoded) != descriptor["bytes"]
                or hashlib.sha256(encoded).hexdigest() != descriptor["sha256"]):
            raise ProofRefused("staged proof objects differ from the exact carrier")
        validate_artifact_bytes(encoded, role, manifest)
        expected[path] = encoded
    if set(objects) != set(expected):
        raise ProofRefused("unreferenced object supplied for carrier publication")
    expected[carrier_path(manifest)] = canonical_bytes(manifest) + b"\n"
    _stage_files(writer, expected)
    return {path: hashlib.sha256(encoded).hexdigest() for path, encoded in expected.items()}
