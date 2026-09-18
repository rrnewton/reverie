#!/usr/bin/env python3
"""Package the typed Rust compatibility projection as Stage 1 website data.

Python owns capture, byte/count/digest bindings, the pure reducer call, and the
Stage 1 envelope.  The private Rust command owns every manifest, HistoryRow,
CellResult, and SeriesRow field decision.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
from collections import Counter
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any


CI_HUB = Path(__file__).resolve().parents[1]
REPOSITORY = CI_HUB.parent
EXPORT_INPUTS_PATH = Path(__file__).resolve().with_name("export_inputs.py")
REDUCE_PATH = Path(__file__).resolve().with_name("reduce.py")
PROJECTION_COMMAND = CI_HUB / "ci-hub.rs"

SCHEMA_VERSION = 3
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_OWNER_PATH_NEEDLE = "/" + "home" + "/"
_OWNER_PATH_RE = re.compile(
    r"(?:/" + "home" + r"/[^/\s]+|/Users/[^/\s]+|/root)(?:/|$)"
    r"|[A-Z]:[\\/](?:Users|Documents and Settings)[\\/][^\\/\s]+",
    re.IGNORECASE,
)
_CREDENTIAL_RE = re.compile(
    r"\bauthorization\s*[:=]\s*(?:basic|bearer)\s+\S+"
    r"|(?:^|[\s;&])(?:[A-Za-z0-9]+[_-])*(?:access[_-]?(?:key|token)"
    r"|api[_-]?key|client[_-]?secret|credential|passwd|password"
    r"|private[_-]?key|secret|token)\s*[:=]\s*\S+"
    r"|\b(?:gh[pousr]|github_pat)_[A-Za-z0-9_]+\b"
    r"|https?://[^/@\s:]+:[^/@\s]+@"
    r"|-----BEGIN [A-Z ]*PRIVATE KEY-----",
    re.IGNORECASE,
)
_CREDENTIAL_KEY_RE = re.compile(
    r"(?:^|[_-])(?:access[_-]?token|api[_-]?key|authorization"
    r"|client[_-]?secret|credential|passwd|password|private[_-]?key"
    r"|secret|token)(?:$|[_-])",
    re.IGNORECASE,
)
_PROVENANCE_FIELDS = frozenset(
    "build_data_schema_version canonical_input_bytes_sha256 "
    "canonical_input_provenance current_main_ancestry_sha256 "
    "current_validation_run_row_count current_validation_run_rows_projection_sha256 "
    "current_validation_run_rows_sha256 hermit_commit hermit_main_commit "
    "manifest_metadata_bytes_sha256 manifest_metadata_projection_sha256 "
    "manifest_metadata_schema manifest_schema manifest_sha256 "
    "parent_commit product_results_sha256 "
    "projection_checker_input_jsonl_sha256 projection_results_sha256 "
    "reducer_schema_version series_physical_row_count "
    "series_represented_run_count series_rows_projection_sha256 "
    "series_rows_sha256 series_schema_counts validate_run_selection_sha256 "
    "validation_event_count "
    "validation_events_sha256 validation_history_row_count "
    "validation_history_rows_projection_sha256 validation_history_rows_sha256".split()
)
_HISTORICAL_SOURCE_FIELDS = frozenset(
    "finished_at history_row_sha256 host_capabilities kernel_version "
    "measured_hermit_commit measured_reverie_commit measurement_host record_id "
    "run_id selected_by_full_cell_count started_at validate_path".split()
)
_HISTORICAL_PROVENANCE_FIELDS = frozenset(
    (
        *_HISTORICAL_SOURCE_FIELDS,
        "ledger_commit",
        "ledger_ref",
        "manifest_source_commit",
    )
)
_RUN_FIELDS = frozenset(
    "current_main_ancestry current_validation_run_row_count "
    "current_validation_run_rows projection_admitted_physical_row_count "
    "projection_admitted_represented_run_count "
    "projection_excluded_physical_row_count "
    "projection_excluded_represented_run_count schema_version "
    "projection_exclusion_counts "
    "product_results projection_results validate_run_selection "
    "series_physical_row_count series_represented_run_count series_rows "
    "validation_history_row_count validation_history_rows".split()
)
_PROJECTION_FIELDS = frozenset(
    {
        "counts",
        "current_main_ancestry",
        "current_validation_run_rows",
        "input",
        "manifest",
        "product_results",
        "projection_exclusion_counts",
        "projection_results",
        "projection_sha256",
        "schema_version",
        "series_rows",
        "validate_run_selection",
        "validation_history_rows",
    }
)
_PROJECTION_COUNT_FIELDS = frozenset(
    {
        "current_validation_run_row_count",
        "projection_admitted_physical_row_count",
        "projection_admitted_represented_run_count",
        "projection_excluded_physical_row_count",
        "projection_excluded_represented_run_count",
        "series_physical_row_count",
        "series_represented_run_count",
        "series_schema_counts",
        "validation_history_row_count",
    }
)
# These are the persisted Stage-1 schema-v3 fields consumed by the website.
# They deliberately do not describe the Rust-owned source DTOs retained below
# ``runs``; those rows stay opaque except for the identities needed to bind the
# website's own counts, links, and reducer inputs.
_STAGE1_POLICY_CELL_KEYS = frozenset(
    "backend category cell current_reproducer "
    "current_reproducer_unavailable_reason guest_args lane mode "
    "not_applicable_reason not_selected_by_full_reason selected_by_full test "
    "cpu_timeout_seconds timeout_seconds workdir".split()
)
_STAGE1_PERSISTED_TEST_KEYS = frozenset(
    "build category cells description direct id lane observation occasional "
    "preprocessors program requires".split()
)
_DELIBERATE_OMISSION_FIELDS = frozenset(
    "backend cell event_ids event_references mode physical_row_count "
    "qualifying_event_ids qualifying_physical_row_count "
    "qualifying_represented_run_count reason represented_run_count test".split()
)
_EVENT_REFERENCE_FIELDS = frozenset(
    {"event_id", "observed_at", "product_result", "run_id", "tree"}
)
_DELIBERATE_OMISSION_REASONS = frozenset(
    {
        "the latest qualifying executions do not establish one unique latest execution",
        "the latest qualifying execution does not establish pass or product failure",
    }
)
_NOT_SELECTED_BY_FULL_RESULTS = frozenset(
    {
        "not-measured",
        "determinism-failure",
        "parity-failure",
        "replay-failure",
        "crash-error",
        "timeout",
        "oom",
        "infrastructure-error",
        "unavailable",
    }
)
_MAX_PROJECTION_STDOUT_BYTES = 256 << 20
_MAX_PROJECTION_STDERR_BYTES = 64 << 10
_MAX_GIT_STDOUT_BYTES = 256 << 20
_MAX_GIT_STDERR_BYTES = 64 << 10
_MAX_MANIFEST_STDOUT_BYTES = 64 << 20
_MAX_MANIFEST_STDERR_BYTES = 64 << 10
_GIT_TIMEOUT_SECONDS = 60
_MANIFEST_TIMEOUT_SECONDS = 900
_PROJECTION_TIMEOUT_SECONDS = 900


class BuildRefused(Exception):
    """The supplied authorities cannot support one deterministic reduction."""


@dataclass(frozen=True)
class CapturedJson:
    value: dict[str, Any]
    encoded: bytes


@dataclass(frozen=True)
class Modules:
    export_inputs: Any
    reducer: Any


_LOADED_MODULES: Modules | None = None


def _load_module(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise BuildRefused(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    previous = sys.modules.get(name)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as error:
        if previous is None:
            sys.modules.pop(name, None)
        else:
            sys.modules[name] = previous
        raise BuildRefused(f"cannot load {path}: {error}") from error
    return module


def _modules() -> Modules:
    global _LOADED_MODULES
    if _LOADED_MODULES is None:
        _LOADED_MODULES = Modules(
            export_inputs=_load_module(
                "compatibility_website_export_inputs", EXPORT_INPUTS_PATH
            ),
            reducer=_load_module("compatibility_website_reduce", REDUCE_PATH),
        )
    return _LOADED_MODULES


def _canonical_bytes(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode()
    except (TypeError, ValueError, RecursionError) as error:
        raise BuildRefused(f"value is not canonical JSON: {error}") from error


def _digest(value: Any) -> str:
    return hashlib.sha256(_canonical_bytes(value)).hexdigest()


def _object(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise BuildRefused(f"{label} must be an object")
    return value


def _array(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise BuildRefused(f"{label} must be an array")
    return value


def _exact_object(
    value: Any,
    label: str,
    *,
    required: frozenset[str],
    optional: frozenset[str] = frozenset(),
) -> Mapping[str, Any]:
    item = _object(value, label)
    fields = set(item)
    missing = required - fields
    unknown = fields - required - optional
    if missing:
        raise BuildRefused(f"{label} is missing field(s): {', '.join(sorted(missing))}")
    if unknown:
        raise BuildRefused(
            f"{label} has unknown field(s): {', '.join(sorted(unknown))}"
        )
    return item


def _string(value: Any, label: str, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value.strip()):
        qualifier = "a string" if allow_empty else "a nonempty string"
        raise BuildRefused(f"{label} must be {qualifier}")
    return value


def _integer(value: Any, label: str, *, minimum: int = 0) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise BuildRefused(f"{label} must be an integer >= {minimum}")
    return value


def _boolean(value: Any, label: str) -> bool:
    if not isinstance(value, bool):
        raise BuildRefused(f"{label} must be a bool")
    return value


def _sha(value: Any, label: str) -> str:
    value = _string(value, label)
    if _SHA_RE.fullmatch(value) is None:
        raise BuildRefused(f"{label} must be a lowercase 40-hex commit")
    return value


def _sha256(value: Any, label: str) -> str:
    value = _string(value, label)
    if _SHA256_RE.fullmatch(value) is None:
        raise BuildRefused(f"{label} must be lowercase 64-hex")
    return value


def _object_id(value: Any, label: str) -> str:
    value = _string(value, label)
    if re.fullmatch(r"[0-9a-f]{40,64}", value) is None:
        raise BuildRefused(f"{label} must be a lowercase 40- or 64-hex object id")
    return value


def _relative_path(value: Any, label: str) -> str:
    raw = _string(value, label)
    path = PurePosixPath(raw)
    if path.is_absolute() or not path.parts or ".." in path.parts:
        raise BuildRefused(f"{label} must be a safe relative path")
    return path.as_posix()


def _process_detail(stderr: bytes, stdout: bytes) -> str:
    detail = (stderr or stdout).decode(errors="replace").strip()
    if len(detail) > 2_000:
        return detail[:2_000] + "... [truncated]"
    return detail or "no output"


def _git_environment() -> dict[str, str]:
    environment = {
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    environment["GIT_NO_LAZY_FETCH"] = "1"
    return environment


def _capture(
    command: Sequence[str],
    *,
    timeout: float,
    stdout_limit: int,
    stderr_limit: int,
    cwd: Path | None = None,
    env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[bytes]:
    environment = dict(os.environ if env is None else env)
    return _modules().export_inputs.capture_bounded_process(
        command,
        cwd=cwd,
        environment=environment,
        timeout=timeout,
        stdout_limit=stdout_limit,
        stderr_limit=stderr_limit,
    )


def _git(
    checkout: Path,
    arguments: Sequence[str],
    *,
    accepted_returncodes: frozenset[int] = frozenset({0}),
) -> subprocess.CompletedProcess[bytes]:
    export_inputs = _modules().export_inputs
    try:
        result = _capture(
            ["git", "--no-replace-objects", "-C", str(checkout), *arguments],
            timeout=_GIT_TIMEOUT_SECONDS,
            stdout_limit=_MAX_GIT_STDOUT_BYTES,
            stderr_limit=_MAX_GIT_STDERR_BYTES,
            env=_git_environment(),
        )
    except (
        OSError,
        subprocess.TimeoutExpired,
        export_inputs.OutputLimitExceeded,
    ) as error:
        raise BuildRefused(f"cannot run git in {checkout}: {error}") from error
    if result.returncode not in accepted_returncodes:
        detail = _process_detail(result.stderr, result.stdout)
        raise BuildRefused(f"git {' '.join(arguments)} failed in {checkout}: {detail}")
    if result.stderr:
        raise BuildRefused(
            f"git {' '.join(arguments)} wrote unexpected stderr in {checkout}: "
            f"{_process_detail(result.stderr, b'')}"
        )
    return result


def _git_text(checkout: Path, arguments: Sequence[str]) -> str:
    try:
        return _git(checkout, arguments).stdout.decode(errors="strict").strip()
    except UnicodeDecodeError as error:
        raise BuildRefused(f"git returned non-UTF-8 output in {checkout}") from error


def _require_complete_commit_graph(checkout: Path) -> None:
    shallow = _git_text(checkout, ["rev-parse", "--is-shallow-repository"])
    if shallow == "true":
        raise BuildRefused(
            "Hermit checkout has a shallow commit graph; ancestry cannot be decided"
        )
    if shallow != "false":
        raise BuildRefused(
            f"git returned an invalid shallow-repository result: {shallow!r}"
        )


def _require_checkout(
    value: Path,
    expected_commit: str,
    *,
    label: str,
    clean: bool,
) -> Path:
    expected_commit = _sha(expected_commit, f"{label} commit")
    checkout = value.expanduser().resolve()
    if not checkout.is_dir():
        raise BuildRefused(f"{label} checkout is not a directory: {checkout}")
    top = Path(_git_text(checkout, ["rev-parse", "--show-toplevel"])).resolve()
    if top != checkout:
        raise BuildRefused(
            f"{label} checkout must name its Git root, got {checkout} (root is {top})"
        )
    actual = _git_text(checkout, ["rev-parse", "--verify", "HEAD^{commit}"])
    if actual != expected_commit:
        raise BuildRefused(
            f"{label} checkout is at {actual}, expected {expected_commit}"
        )
    if (
        clean
        and _git(
            checkout,
            [
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        ).stdout
    ):
        raise BuildRefused(f"{label} checkout has local changes")
    return checkout


def _require_main_target(checkout: Path, expected_commit: str) -> None:
    _require_complete_commit_graph(checkout)
    resolved = _git_text(
        checkout, ["rev-parse", "--verify", f"{expected_commit}^{{commit}}"]
    )
    if resolved != expected_commit:
        raise BuildRefused(
            f"Hermit main commit resolved to {resolved}, expected {expected_commit}"
        )
    current = _git_text(
        checkout,
        ["rev-parse", "--verify", "refs/remotes/origin/main^{commit}"],
    )
    if current != expected_commit:
        raise BuildRefused(
            f"Hermit origin/main is {current}, expected {expected_commit}"
        )


def _capture_canonical_input(parent: Path, parent_commit: str) -> CapturedJson:
    export_inputs = _modules().export_inputs
    try:
        value = export_inputs.build_export(parent, parent_commit)
        encoded = export_inputs.render_export(value)
    except (export_inputs.ExportRefused, OSError, ValueError) as error:
        raise BuildRefused(f"canonical input export refused: {error}") from error
    except Exception as error:
        ledger = export_inputs.ledger_store
        if ledger is None or not isinstance(error, ledger.LedgerError):
            raise
        raise BuildRefused(f"canonical input export refused: {error}") from error
    if not isinstance(value, dict) or not isinstance(encoded, bytes):
        raise BuildRefused("canonical input exporter returned an invalid document")
    return CapturedJson(value=value, encoded=encoded)


def _capture_historical_input(
    parent: Path, parent_commit: str, record_id: str, run_id: str
) -> CapturedJson:
    export_inputs = _modules().export_inputs
    try:
        value = export_inputs.build_historical_export(
            parent, parent_commit, record_id, run_id
        )
        encoded = export_inputs.render_export(value)
    except (export_inputs.ExportRefused, OSError, ValueError) as error:
        raise BuildRefused(f"historical input export refused: {error}") from error
    except Exception as error:
        ledger = export_inputs.ledger_store
        if ledger is None or not isinstance(error, ledger.LedgerError):
            raise
        raise BuildRefused(f"historical input export refused: {error}") from error
    if not isinstance(value, dict) or not isinstance(encoded, bytes):
        raise BuildRefused("historical input exporter returned an invalid document")
    return CapturedJson(value=value, encoded=encoded)


def _gitlink_commit(parent: Path, parent_commit: str, path: str) -> str:
    listing = _git_text(parent, ["ls-tree", parent_commit, "--", path])
    match = re.fullmatch(rf"160000 commit ([0-9a-f]{{40}})\t{re.escape(path)}", listing)
    if match is None:
        raise BuildRefused(
            f"dev-hermit commit {parent_commit} does not contain one {path} gitlink"
        )
    return match.group(1)


def _run_manifest_metadata(hermit: Path) -> CapturedJson:
    command = [
        "cargo",
        "run",
        "--quiet",
        "--locked",
        "-p",
        "hermit-manifest-plan",
        "--bin",
        "manifest-metadata",
    ]
    export_inputs = _modules().export_inputs
    try:
        result = _capture(
            command,
            cwd=hermit,
            timeout=_MANIFEST_TIMEOUT_SECONDS,
            stdout_limit=_MAX_MANIFEST_STDOUT_BYTES,
            stderr_limit=_MAX_MANIFEST_STDERR_BYTES,
            env=_git_environment(),
        )
    except (
        OSError,
        subprocess.TimeoutExpired,
        export_inputs.OutputLimitExceeded,
    ) as error:
        raise BuildRefused(
            f"manifest metadata command could not run: {error}"
        ) from error
    if result.returncode != 0:
        raise BuildRefused(
            "manifest metadata command failed: "
            + _process_detail(result.stderr, result.stdout)
        )
    if result.stderr:
        raise BuildRefused(
            "manifest metadata command wrote unexpected stderr: "
            + _process_detail(result.stderr, b"")
        )
    value = _one_json_line(result.stdout, "manifest metadata command")
    if not isinstance(value, dict):
        raise BuildRefused("manifest metadata command must return one JSON object")
    return CapturedJson(value=value, encoded=result.stdout)


def _opaque_projection_inputs(canonical: Mapping[str, Any]) -> tuple[bytes, bytes]:
    _exact_object(
        canonical,
        "canonical input",
        required=frozenset(
            {
                "incomplete_data",
                "provenance",
                "schema_version",
                "series",
                "validation_ledger",
            }
        ),
    )
    if canonical.get("schema_version") != 1:
        raise BuildRefused("canonical input schema_version must be 1")

    def bound_jsonl(
        section: Any,
        label: str,
        text_field: str,
        count_field: str,
        digest_field: str,
    ) -> bytes:
        section = _object(section, label)
        text = _string(
            section.get(text_field), f"{label}.{text_field}", allow_empty=True
        )
        encoded = text.encode()
        count = _integer(section.get(count_field), f"{label}.{count_field}")
        if count == 0:
            if encoded:
                raise BuildRefused(f"{label}.{text_field} must be empty for zero rows")
        elif not encoded.endswith(b"\n") or encoded.count(b"\n") != count:
            raise BuildRefused(
                f"{label}.{text_field} framing disagrees with {count_field}"
            )
        expected = _sha256(section.get(digest_field), f"{label}.{digest_field}")
        if hashlib.sha256(encoded).hexdigest() != expected:
            raise BuildRefused(
                f"{label}.{digest_field} disagrees with exact JSONL bytes"
            )
        return encoded

    history = bound_jsonl(
        canonical.get("validation_ledger"),
        "validation_ledger",
        "history_rows_jsonl",
        "history_row_count",
        "history_rows_sha256",
    )
    series = bound_jsonl(
        canonical.get("series"),
        "series",
        "rows_jsonl",
        "row_count",
        "rows_sha256",
    )
    return history, series


def _bounded_projection_process(
    command: Sequence[str], *, cwd: Path, environment: Mapping[str, str]
) -> subprocess.CompletedProcess[bytes]:
    return _capture(
        command,
        cwd=cwd,
        timeout=_PROJECTION_TIMEOUT_SECONDS,
        stdout_limit=_MAX_PROJECTION_STDOUT_BYTES,
        stderr_limit=_MAX_PROJECTION_STDERR_BYTES,
        env=environment,
    )


def _one_json_line(encoded: bytes, label: str) -> Any:
    if (
        not encoded.endswith(b"\n")
        or b"\n" in encoded[:-1]
        or encoded[:-1].strip() != encoded[:-1]
    ):
        raise BuildRefused(f"{label} did not emit exactly one JSON line")
    try:
        return json.loads(encoded[:-1].decode(errors="strict"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BuildRefused(f"{label} returned malformed JSON: {error}") from error


def _projection_binding(
    value: Any, label: str, expected: bytes, rows: int | None
) -> None:
    required = {"bytes", "sha256"}
    if rows is not None:
        required.add("row_count")
    binding = _exact_object(value, label, required=frozenset(required))
    if _integer(binding.get("bytes"), f"{label}.bytes") != len(expected):
        raise BuildRefused(f"{label}.bytes disagrees with exact input")
    if (
        _sha256(binding.get("sha256"), f"{label}.sha256")
        != hashlib.sha256(expected).hexdigest()
    ):
        raise BuildRefused(f"{label}.sha256 disagrees with exact input")
    if (
        rows is not None
        and _integer(binding.get("row_count"), f"{label}.row_count") != rows
    ):
        raise BuildRefused(f"{label}.row_count disagrees with exact input")


def _validate_projection_envelope(
    value: Any,
    manifest: bytes,
    history: bytes,
    series: bytes,
    historical_identity: tuple[str, str] | None,
) -> dict[str, Any]:
    optional = frozenset({"historical"})
    projection = _exact_object(
        value,
        "compatibility website projection",
        required=_PROJECTION_FIELDS,
        optional=optional,
    )
    if projection.get("schema_version") != 2:
        raise BuildRefused("compatibility website projection schema_version must be 2")
    expected_digest = _sha256(projection.get("projection_sha256"), "projection_sha256")
    unsigned = dict(projection)
    del unsigned["projection_sha256"]
    if _digest(unsigned) != expected_digest:
        raise BuildRefused("projection_sha256 disagrees with the exact projection")
    input_bindings = _exact_object(
        projection.get("input"),
        "projection.input",
        required=frozenset(
            {"manifest_metadata", "series_rows", "validation_history_rows"}
        ),
    )
    _projection_binding(
        input_bindings["manifest_metadata"],
        "projection.input.manifest_metadata",
        manifest,
        None,
    )
    _projection_binding(
        input_bindings["validation_history_rows"],
        "projection.input.validation_history_rows",
        history,
        history.count(b"\n"),
    )
    _projection_binding(
        input_bindings["series_rows"],
        "projection.input.series_rows",
        series,
        series.count(b"\n"),
    )
    counts = _exact_object(
        projection.get("counts"),
        "projection.counts",
        required=_PROJECTION_COUNT_FIELDS,
    )
    for field in _PROJECTION_COUNT_FIELDS - {"series_schema_counts"}:
        _integer(counts.get(field), f"projection.counts.{field}")
    _object(
        counts.get("series_schema_counts"), "projection.counts.series_schema_counts"
    )
    history_rows = _array(
        projection.get("validation_history_rows"), "projection.validation_history_rows"
    )
    current_rows = _array(
        projection.get("current_validation_run_rows"),
        "projection.current_validation_run_rows",
    )
    series_rows = _array(projection.get("series_rows"), "projection.series_rows")
    if counts["validation_history_row_count"] != len(history_rows):
        raise BuildRefused(
            "projection validation history count disagrees with its rows"
        )
    if counts["current_validation_run_row_count"] != len(current_rows):
        raise BuildRefused(
            "projection current validation count disagrees with its rows"
        )
    if counts["series_physical_row_count"] != len(series_rows):
        raise BuildRefused("projection series physical count disagrees with its rows")
    if counts["projection_admitted_physical_row_count"] != len(series_rows):
        raise BuildRefused("projection admitted physical count disagrees with its rows")
    if (
        counts["projection_admitted_physical_row_count"]
        + counts["projection_excluded_physical_row_count"]
        != input_bindings["series_rows"]["row_count"]
    ):
        raise BuildRefused(
            "projection admitted and excluded counts disagree with raw input"
        )
    for field in (
        "manifest",
        "current_main_ancestry",
        "projection_results",
        "product_results",
        "validate_run_selection",
    ):
        _object(projection.get(field), f"projection.{field}")
    for field in ("projection_exclusion_counts",):
        _array(projection.get(field), f"projection.{field}")
    if historical_identity is None:
        if "historical" in projection:
            raise BuildRefused(
                "live projection unexpectedly contains historical source"
            )
    else:
        source = _historical_source(
            projection.get("historical"), "projection.historical"
        )
        if (source["record_id"], source["run_id"]) != historical_identity:
            raise BuildRefused(
                "projection historical source disagrees with requested identity"
            )
    return json.loads(_canonical_bytes(projection))


def _run_projection(
    manifest: CapturedJson,
    canonical: CapturedJson,
    hermit: Path,
    hermit_main_commit: str,
    historical_identity: tuple[str, str] | None,
) -> dict[str, Any]:
    history, series = _opaque_projection_inputs(canonical.value)
    with tempfile.TemporaryDirectory(prefix="compatibility-website-projection-") as raw:
        root = Path(raw)
        manifest_path = root / "manifest.json"
        history_path = root / "history.jsonl"
        series_path = root / "series.jsonl"
        manifest_path.write_bytes(manifest.encoded)
        history_path.write_bytes(history)
        series_path.write_bytes(series)
        command = [
            str(PROJECTION_COMMAND),
            "compatibility-website-projection",
            "--manifest-json",
            str(manifest_path),
            "--history-jsonl",
            str(history_path),
            "--series-jsonl",
            str(series_path),
            "--hermit-checkout",
            str(hermit),
            "--hermit-main-commit",
            hermit_main_commit,
        ]
        if historical_identity is not None:
            command.extend(
                [
                    "--record-id",
                    historical_identity[0],
                    "--run-id",
                    historical_identity[1],
                ]
            )
        export_inputs = _modules().export_inputs
        try:
            result = _bounded_projection_process(
                command, cwd=REPOSITORY, environment=_git_environment()
            )
        except (
            OSError,
            subprocess.TimeoutExpired,
            export_inputs.OutputLimitExceeded,
        ) as error:
            raise BuildRefused(
                f"compatibility website projection could not run: {error}"
            ) from error
    if result.returncode != 0:
        raise BuildRefused(
            "compatibility website projection failed: "
            + _process_detail(result.stderr, result.stdout)
        )
    if result.stderr:
        raise BuildRefused(
            "compatibility website projection wrote unexpected stderr: "
            + _process_detail(result.stderr, b"")
        )
    value = _one_json_line(result.stdout, "compatibility website projection")
    return _validate_projection_envelope(
        value,
        manifest.encoded,
        history,
        series,
        historical_identity,
    )


def _historical_source(value: Any, label: str) -> dict[str, Any]:
    source = _exact_object(value, label, required=_HISTORICAL_SOURCE_FIELDS)
    for field in (
        "record_id",
        "run_id",
        "started_at",
        "finished_at",
        "measurement_host",
        "kernel_version",
    ):
        _string(source.get(field), f"{label}.{field}")
    if source.get("validate_path") != "full":
        raise BuildRefused(f"{label}.validate_path must be full")
    _integer(
        source.get("selected_by_full_cell_count"),
        f"{label}.selected_by_full_cell_count",
    )
    _sha(source.get("measured_hermit_commit"), f"{label}.measured_hermit_commit")
    _sha(
        source.get("measured_reverie_commit"),
        f"{label}.measured_reverie_commit",
    )
    capabilities = _exact_object(
        source.get("host_capabilities"),
        f"{label}.host_capabilities",
        required=frozenset({"cpuid-faulting", "kvm"}),
    )
    for name, raw in capabilities.items():
        capability = _exact_object(
            raw,
            f"{label}.host_capabilities.{name}",
            required=frozenset({"evidence", "present"}),
        )
        _string(
            capability.get("evidence"),
            f"{label}.host_capabilities.{name}.evidence",
        )
        _boolean(
            capability.get("present"),
            f"{label}.host_capabilities.{name}.present",
        )
    _sha256(source.get("history_row_sha256"), f"{label}.history_row_sha256")
    return json.loads(_canonical_bytes(source))


def _canonical_provenance(
    value: Any, expected_parent_commit: str, *, historical: bool
) -> dict[str, Any]:
    base = frozenset(
        "parent_commit published_ref reader_ci_hub_tree reader_commit reader_files "
        "reader_files_sha256 reader_hermit source_count sources sources_sha256".split()
    )
    historical_fields = frozenset(
        {
            "historical",
            "mode",
            "published_only",
            "reader_source_identity_sha256",
            "reader_worktree_clean",
            "reader_worktree_dirty_paths",
        }
    )
    provenance = _exact_object(
        value,
        "canonical input provenance",
        required=base | (historical_fields if historical else frozenset()),
    )
    parent = _sha(provenance.get("parent_commit"), "provenance.parent_commit")
    published = _sha(provenance.get("published_ref"), "provenance.published_ref")
    reader_commit = _sha(provenance.get("reader_commit"), "provenance.reader_commit")
    if parent != expected_parent_commit or published != expected_parent_commit:
        raise BuildRefused(
            "canonical input provenance parent_commit and published_ref must equal the requested parent commit"
        )
    if not historical and reader_commit != expected_parent_commit:
        raise BuildRefused(
            "live canonical input reader commit must equal the requested parent commit"
        )
    _object_id(provenance.get("reader_ci_hub_tree"), "provenance.reader_ci_hub_tree")
    reader = _exact_object(
        provenance.get("reader_hermit"),
        "provenance.reader_hermit",
        required=frozenset({"checkout_commit", "gitlink_commit"}),
    )
    reader_checkout = _sha(
        reader.get("checkout_commit"), "reader_hermit.checkout_commit"
    )
    reader_gitlink = _sha(reader.get("gitlink_commit"), "reader_hermit.gitlink_commit")
    if not historical and reader_checkout != reader_gitlink:
        raise BuildRefused("reader Hermit checkout and gitlink commits disagree")
    reader_files = _array(provenance.get("reader_files"), "provenance.reader_files")
    reader_paths: set[str] = set()
    for index, raw in enumerate(reader_files):
        item = _exact_object(
            raw,
            f"provenance.reader_files[{index}]",
            required=frozenset({"bytes", "path", "sha256"}),
        )
        _integer(item.get("bytes"), f"provenance.reader_files[{index}].bytes")
        path = _relative_path(
            item.get("path"), f"provenance.reader_files[{index}].path"
        )
        if path in reader_paths:
            raise BuildRefused("canonical input provenance repeats a reader path")
        reader_paths.add(path)
        _sha256(item.get("sha256"), f"provenance.reader_files[{index}].sha256")
    if _sha256(
        provenance.get("reader_files_sha256"), "provenance.reader_files_sha256"
    ) != _digest(reader_files):
        raise BuildRefused("provenance.reader_files_sha256 disagrees with reader_files")
    sources = _array(provenance.get("sources"), "provenance.sources")
    identities: set[tuple[str, str, str]] = set()
    for index, raw in enumerate(sources):
        label = f"provenance.sources[{index}]"
        item = _exact_object(
            raw,
            label,
            required=frozenset(
                {"bytes", "line_count", "location", "path", "sha256", "store"}
            ),
            optional=frozenset({"git_object"}),
        )
        _integer(item.get("bytes"), f"{label}.bytes")
        _integer(item.get("line_count"), f"{label}.line_count")
        identity = (
            _string(item.get("store"), f"{label}.store"),
            _string(item.get("location"), f"{label}.location"),
            _relative_path(item.get("path"), f"{label}.path"),
        )
        if identity in identities:
            raise BuildRefused("canonical input provenance repeats a source identity")
        identities.add(identity)
        _sha256(item.get("sha256"), f"{label}.sha256")
        if "git_object" in item:
            _object_id(item.get("git_object"), f"{label}.git_object")
    if _integer(provenance.get("source_count"), "provenance.source_count") != len(
        sources
    ):
        raise BuildRefused("provenance.source_count disagrees with sources")
    if _sha256(
        provenance.get("sources_sha256"), "provenance.sources_sha256"
    ) != _digest(sources):
        raise BuildRefused("provenance.sources_sha256 disagrees with sources")
    if historical:
        if (
            provenance.get("mode") != "historical"
            or provenance.get("published_only") is not True
        ):
            raise BuildRefused("historical canonical provenance mode is invalid")
        request = _exact_object(
            provenance.get("historical"),
            "provenance.historical",
            required=frozenset({"record_id", "run_id"}),
        )
        _string(request.get("record_id"), "provenance.historical.record_id")
        _string(request.get("run_id"), "provenance.historical.run_id")
        dirty = _array(
            provenance.get("reader_worktree_dirty_paths"),
            "provenance.reader_worktree_dirty_paths",
        )
        paths = [
            _relative_path(path, f"provenance.reader_worktree_dirty_paths[{index}]")
            for index, path in enumerate(dirty)
        ]
        if paths != sorted(set(paths)):
            raise BuildRefused(
                "historical reader dirty paths must be unique and sorted"
            )
        worktree_clean = _boolean(
            provenance.get("reader_worktree_clean"),
            "provenance.reader_worktree_clean",
        )
        if worktree_clean != (not paths):
            raise BuildRefused(
                "historical reader cleanliness disagrees with dirty paths"
            )
        source_identity = {
            "ci_hub_tree": provenance["reader_ci_hub_tree"],
            "commit": reader_commit,
            "dirty_paths": paths,
            "files": reader_files,
            "hermit": dict(reader),
            "worktree_clean": worktree_clean,
        }
        if _sha256(
            provenance.get("reader_source_identity_sha256"),
            "provenance.reader_source_identity_sha256",
        ) != _digest(source_identity):
            raise BuildRefused(
                "provenance.reader_source_identity_sha256 disagrees with the "
                "historical reader identity"
            )
    _require_publishable_output(provenance)
    return json.loads(_canonical_bytes(provenance))


def _validated_reduction(value: Any) -> dict[str, Any]:
    reduced = _exact_object(
        value,
        "compatibility reduction",
        required=frozenset({"cells", "counts", "projection_audit", "schema_version"}),
    )
    if reduced.get("schema_version") != _modules().reducer.SCHEMA_VERSION:
        raise BuildRefused("compatibility reduction schema_version is unsupported")
    return {
        "cells": _array(reduced.get("cells"), "compatibility reduction cells"),
        "counts": dict(
            _object(reduced.get("counts"), "compatibility reduction counts")
        ),
        "projection_audit": _validated_projection_audit(
            reduced.get("projection_audit"),
            "compatibility reduction projection_audit",
        ),
    }


def _canonical_string_array(value: Any, label: str) -> list[str]:
    items = [
        _string(item, f"{label}[{index}]")
        for index, item in enumerate(_array(value, label))
    ]
    if items != sorted(set(items)):
        raise BuildRefused(f"{label} must contain unique values in canonical order")
    return items


def _validated_projection_audit(value: Any, label: str) -> dict[str, Any]:
    audit = _exact_object(
        value,
        label,
        required=frozenset({"deliberate_omissions"}),
    )
    omissions = []
    identities = []
    for index, raw in enumerate(
        _array(audit.get("deliberate_omissions"), f"{label}.deliberate_omissions")
    ):
        item_label = f"{label}.deliberate_omissions[{index}]"
        item = dict(
            _exact_object(
                raw,
                item_label,
                required=_DELIBERATE_OMISSION_FIELDS,
            )
        )
        for field in ("backend", "cell", "mode", "test"):
            _string(item.get(field), f"{item_label}.{field}")
        identity = "/".join(str(item[field]) for field in ("test", "mode", "backend"))
        if item["cell"] != identity:
            raise BuildRefused(
                f"{item_label}.cell disagrees with test, mode, and backend"
            )
        for field, minimum in (
            ("physical_row_count", 1),
            ("represented_run_count", 1),
            ("qualifying_physical_row_count", 1),
            ("qualifying_represented_run_count", 1),
        ):
            _integer(item.get(field), f"{item_label}.{field}", minimum=minimum)
        if (
            item["qualifying_physical_row_count"] > item["physical_row_count"]
            or item["qualifying_represented_run_count"] > item["represented_run_count"]
        ):
            raise BuildRefused(
                f"{item_label} qualifying counts exceed the recorded population"
            )
        event_ids = _canonical_string_array(
            item.get("event_ids"), f"{item_label}.event_ids"
        )
        qualifying_event_ids = _canonical_string_array(
            item.get("qualifying_event_ids"),
            f"{item_label}.qualifying_event_ids",
        )
        if not set(qualifying_event_ids) <= set(event_ids):
            raise BuildRefused(
                f"{item_label}.qualifying_event_ids are not a subset of event_ids"
            )
        if (
            len(event_ids) != item["physical_row_count"]
            or len(qualifying_event_ids) != item["qualifying_physical_row_count"]
        ):
            raise BuildRefused(f"{item_label} event counts disagree with event ids")
        references = []
        for reference_index, raw_reference in enumerate(
            _array(item.get("event_references"), f"{item_label}.event_references")
        ):
            reference_label = f"{item_label}.event_references[{reference_index}]"
            reference = dict(
                _exact_object(
                    raw_reference,
                    reference_label,
                    required=_EVENT_REFERENCE_FIELDS,
                )
            )
            for field in ("event_id", "observed_at", "run_id"):
                _string(reference.get(field), f"{reference_label}.{field}")
            _sha(reference.get("tree"), f"{reference_label}.tree")
            if reference.get("product_result") not in {
                None,
                "pass",
                "product_failure",
            }:
                raise BuildRefused(f"{reference_label}.product_result is unsupported")
            references.append(reference)
        if [reference["event_id"] for reference in references] != event_ids:
            raise BuildRefused(f"{item_label}.event_references disagree with event_ids")
        if item.get("reason") not in _DELIBERATE_OMISSION_REASONS:
            raise BuildRefused(f"{item_label}.reason is unsupported")
        identities.append(identity)
        omissions.append(item)
    if identities != sorted(set(identities)):
        raise BuildRefused(f"{label}.deliberate_omissions must be unique and canonical")
    return json.loads(_canonical_bytes({"deliberate_omissions": omissions}))


def _stage1_policy_cell(value: Any, label: str) -> dict[str, Any]:
    cell = _object(value, label)
    missing = _STAGE1_POLICY_CELL_KEYS.difference(cell)
    if missing:
        raise BuildRefused(f"{label} is missing field(s): {', '.join(sorted(missing))}")
    policy = {field: cell[field] for field in _STAGE1_POLICY_CELL_KEYS}
    for field in ("backend", "category", "cell", "lane", "mode", "test"):
        _string(policy[field], f"{label}.{field}")
    identity = "/".join(str(policy[field]) for field in ("test", "mode", "backend"))
    if policy["cell"] != identity:
        raise BuildRefused(f"{label}.cell disagrees with test, mode, and backend")
    _boolean(policy["selected_by_full"], f"{label}.selected_by_full")
    reason = policy["not_selected_by_full_reason"]
    if reason is not None:
        reason = _exact_object(
            reason,
            f"{label}.not_selected_by_full_reason",
            required=frozenset({"reason"}),
            optional=frozenset({"evidence", "result"}),
        )
        _string(reason.get("reason"), f"{label}.not_selected_by_full_reason.reason")
        has_evidence = "evidence" in reason
        has_result = "result" in reason
        if has_evidence != has_result:
            raise BuildRefused(
                f"{label}.not_selected_by_full_reason result and evidence must be "
                "present together"
            )
        if has_evidence:
            _string(
                reason.get("evidence"),
                f"{label}.not_selected_by_full_reason.evidence",
            )
            result = _string(
                reason.get("result"),
                f"{label}.not_selected_by_full_reason.result",
            )
            if result not in _NOT_SELECTED_BY_FULL_RESULTS:
                raise BuildRefused(
                    f"{label}.not_selected_by_full_reason.result is unsupported"
                )
        policy["not_selected_by_full_reason"] = dict(reason)
    _integer(
        policy["cpu_timeout_seconds"],
        f"{label}.cpu_timeout_seconds",
        minimum=1,
    )
    _integer(policy["timeout_seconds"], f"{label}.timeout_seconds", minimum=1)
    for index, argument in enumerate(
        _array(policy["guest_args"], f"{label}.guest_args")
    ):
        _string(argument, f"{label}.guest_args[{index}]", allow_empty=True)
    for field in ("workdir", "current_reproducer_unavailable_reason"):
        if policy[field] is not None:
            _string(policy[field], f"{label}.{field}")
    reproducer = policy["current_reproducer"]
    if reproducer is not None:
        command = _exact_object(
            reproducer,
            f"{label}.current_reproducer",
            required=frozenset({"argv", "shell_command"}),
        )
        for index, argument in enumerate(
            _array(command["argv"], f"{label}.current_reproducer.argv")
        ):
            _string(argument, f"{label}.current_reproducer.argv[{index}]")
        _string(command["shell_command"], f"{label}.current_reproducer.shell_command")
    return policy


def _projected_manifest(value: Mapping[str, Any]) -> dict[str, Any]:
    """Package an already-Rust-projected manifest without re-reading source types."""
    manifest = _exact_object(
        value,
        "manifest projection",
        required=frozenset(
            {
                "cells",
                "manifest_schema",
                "manifest_sha256",
                "selected_by_full_custom_commands",
                "tests",
            }
        ),
        optional=frozenset({"schema"}),
    )
    raw_cells = _object(manifest.get("cells"), "manifest cells")
    cells = {
        identity: _stage1_policy_cell(raw, f"manifest cells[{identity!r}]")
        for identity, raw in raw_cells.items()
    }
    if any(identity != cell["cell"] for identity, cell in cells.items()):
        raise BuildRefused("manifest cell map key disagrees with its cell identity")
    _integer(manifest.get("manifest_schema"), "manifest_schema", minimum=1)
    _sha256(manifest.get("manifest_sha256"), "manifest_sha256")
    return {
        "cells": dict(sorted(cells.items())),
        "manifest_schema": manifest["manifest_schema"],
        "manifest_sha256": manifest["manifest_sha256"],
        "selected_by_full_custom_commands": _array(
            manifest.get("selected_by_full_custom_commands"),
            "manifest custom commands",
        ),
        "tests": _array(manifest.get("tests"), "manifest tests"),
    }


def _join_manifest_data(
    reduced_cells: Sequence[Any], manifest: Mapping[str, Any]
) -> list[dict[str, Any]]:
    policies = _object(manifest.get("cells"), "manifest cells")
    output: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, raw in enumerate(reduced_cells):
        cell = dict(_object(raw, f"reduced cells[{index}]"))
        identity = _string(cell.get("cell"), f"reduced cells[{index}].cell")
        policy = policies.get(identity)
        if not isinstance(policy, Mapping) or identity in seen:
            raise BuildRefused(f"invalid reduced cell identity {identity!r}")
        seen.add(identity)
        cell.update(
            {
                key: value
                for key, value in policy.items()
                if key not in {"cell", "test", "mode", "backend"}
            }
        )
        output.append(cell)
    return output


def _reduce_projection(projection: Mapping[str, Any]) -> dict[str, Any]:
    manifest = _projected_manifest(
        _object(projection.get("manifest"), "projection.manifest")
    )
    rows = _array(projection.get("series_rows"), "projection.series_rows")
    counts = _object(projection.get("counts"), "projection.counts")
    canonical = {
        "schema_version": 2,
        "series": {
            "row_count": len(rows),
            "represented_run_count": counts["series_represented_run_count"],
            "rows": rows,
            "rows_sha256": _digest(rows),
        },
    }
    reducer_manifest = {"schema": 1, "cells": list(manifest["cells"].values())}
    try:
        reduced = _modules().reducer.reduce_inputs(
            canonical,
            reducer_manifest,
            projection_results=projection["projection_results"],
            current_main_ancestry=projection["current_main_ancestry"],
            product_results=projection["product_results"],
            validate_run_selection=projection["validate_run_selection"],
        )
    except _modules().reducer.ReductionRefused as error:
        raise BuildRefused(f"compatibility reduction refused: {error}") from error
    return {"manifest": manifest, "reduced": _validated_reduction(reduced)}


def _build_provenance(
    canonical_document: CapturedJson,
    canonical_provenance: Mapping[str, Any],
    manifest_document: CapturedJson,
    projection: Mapping[str, Any],
    commits: Mapping[str, str],
) -> dict[str, Any]:
    manifest = _object(projection["manifest"], "projection.manifest")
    counts = _object(projection["counts"], "projection.counts")
    inputs = _object(projection["input"], "projection.input")
    history_input = _object(
        inputs["validation_history_rows"], "projection history input"
    )
    series_input = _object(inputs["series_rows"], "projection series input")
    current = _array(
        projection["current_validation_run_rows"], "projection current rows"
    )
    history = _array(projection["validation_history_rows"], "projection history rows")
    series = _array(projection["series_rows"], "projection series rows")
    canonical_provenance = dict(canonical_provenance)
    if "historical" in projection:
        canonical_provenance["historical"] = projection["historical"]
    provenance = {
        "build_data_schema_version": SCHEMA_VERSION,
        "canonical_input_bytes_sha256": hashlib.sha256(
            canonical_document.encoded
        ).hexdigest(),
        "canonical_input_provenance": canonical_provenance,
        "current_main_ancestry_sha256": _digest(projection["current_main_ancestry"]),
        "current_validation_run_row_count": counts["current_validation_run_row_count"],
        "current_validation_run_rows_projection_sha256": _digest(current),
        "current_validation_run_rows_sha256": _digest(current),
        "hermit_commit": commits["hermit"],
        "hermit_main_commit": commits["hermit_main"],
        "manifest_metadata_bytes_sha256": hashlib.sha256(
            manifest_document.encoded
        ).hexdigest(),
        "manifest_metadata_projection_sha256": _digest(manifest),
        "manifest_metadata_schema": 1,
        "manifest_schema": manifest["manifest_schema"],
        "manifest_sha256": manifest["manifest_sha256"],
        "parent_commit": commits["parent"],
        "product_results_sha256": _digest(projection["product_results"]),
        "projection_checker_input_jsonl_sha256": series_input["sha256"],
        "projection_results_sha256": _digest(projection["projection_results"]),
        "reducer_schema_version": _modules().reducer.SCHEMA_VERSION,
        "series_physical_row_count": counts["series_physical_row_count"],
        "series_represented_run_count": counts["series_represented_run_count"],
        "series_rows_projection_sha256": _digest(series),
        "series_rows_sha256": series_input["sha256"],
        "series_schema_counts": counts["series_schema_counts"],
        "validate_run_selection_sha256": _digest(projection["validate_run_selection"]),
        "validation_event_count": canonical_document.value["validation_ledger"][
            "event_count"
        ],
        "validation_events_sha256": canonical_document.value["validation_ledger"][
            "events_sha256"
        ],
        "validation_history_row_count": counts["validation_history_row_count"],
        "validation_history_rows_projection_sha256": _digest(history),
        "validation_history_rows_sha256": history_input["sha256"],
    }
    if "historical" in projection:
        source = _historical_source(projection["historical"], "projection.historical")
        provenance["historical"] = {
            **source,
            "ledger_commit": commits["parent"],
            "ledger_ref": canonical_document.value["provenance"]["published_ref"],
            "manifest_source_commit": commits["hermit"],
        }
    return provenance


def _artifact_result(
    projection: Mapping[str, Any],
    reduced_bundle: Mapping[str, Any],
    provenance: Mapping[str, Any],
) -> dict[str, Any]:
    manifest = _object(reduced_bundle["manifest"], "manifest")
    reduced = _object(reduced_bundle["reduced"], "reduced data")
    counts = _object(projection["counts"], "projection.counts")
    runs = {
        "current_main_ancestry": projection["current_main_ancestry"],
        "current_validation_run_row_count": counts["current_validation_run_row_count"],
        "current_validation_run_rows": projection["current_validation_run_rows"],
        "product_results": projection["product_results"],
        "projection_admitted_physical_row_count": counts[
            "projection_admitted_physical_row_count"
        ],
        "projection_admitted_represented_run_count": counts[
            "projection_admitted_represented_run_count"
        ],
        "projection_excluded_physical_row_count": counts[
            "projection_excluded_physical_row_count"
        ],
        "projection_excluded_represented_run_count": counts[
            "projection_excluded_represented_run_count"
        ],
        "projection_exclusion_counts": projection["projection_exclusion_counts"],
        "projection_results": projection["projection_results"],
        "schema_version": SCHEMA_VERSION,
        "series_physical_row_count": counts["series_physical_row_count"],
        "series_represented_run_count": counts["series_represented_run_count"],
        "series_rows": projection["series_rows"],
        "validate_run_selection": projection["validate_run_selection"],
        "validation_history_row_count": counts["validation_history_row_count"],
        "validation_history_rows": projection["validation_history_rows"],
    }
    cells = _join_manifest_data(reduced["cells"], manifest)
    output_counts = dict(_object(reduced["counts"], "reduced counts"))
    output_counts.update(
        current_validation_run_row_count=counts["current_validation_run_row_count"],
        selected_by_full_custom_command_count=len(
            manifest["selected_by_full_custom_commands"]
        ),
        test_count=len(manifest["tests"]),
        validation_history_row_count=counts["validation_history_row_count"],
    )
    return validate_build_data(
        {
            "cells": cells,
            "counts": output_counts,
            "projection_audit": reduced["projection_audit"],
            "provenance": dict(provenance),
            "runs": runs,
            "schema_version": SCHEMA_VERSION,
            "selected_by_full_custom_commands": manifest[
                "selected_by_full_custom_commands"
            ],
            "tests": manifest["tests"],
        }
    )


def _walk_strings(value: Any, *, key: bool = False) -> Any:
    if isinstance(value, str):
        yield key, value
    elif isinstance(value, Mapping):
        for name, item in value.items():
            if not isinstance(name, str):
                raise BuildRefused("published object keys must be strings")
            yield True, name
            yield from _walk_strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from _walk_strings(item)


def _require_no_stored_measurement_fields(value: Mapping[str, Any]) -> None:
    forbidden = {
        "enabled",
        "measured",
        "measurement_state",
        "never_measured",
    }
    for is_key, text in _walk_strings(value):
        if is_key and text in forbidden:
            raise BuildRefused(
                f"compatibility reduction contains forbidden stored field {text!r}"
            )


def _require_publishable_output(value: Mapping[str, Any]) -> None:
    _require_no_stored_measurement_fields(value)
    for is_key, text in _walk_strings(value):
        if _OWNER_PATH_RE.search(text):
            raise BuildRefused(
                "compatibility reduction contains an absolute owner path"
            )
        if (is_key and _CREDENTIAL_KEY_RE.search(text)) or _CREDENTIAL_RE.search(text):
            raise BuildRefused(
                "compatibility reduction contains a credential-bearing string"
            )


def _require_website_owned_text(value: Any) -> None:
    for _is_key, text in _walk_strings(value):
        if re.search(r"(?<![A-Za-z0-9])enabled(?![A-Za-z0-9])", text, re.IGNORECASE):
            raise BuildRefused("website-owned text contains dropped vocabulary")


def _validated_output_runs(data: Mapping[str, Any]) -> dict[str, Any]:
    runs = _exact_object(data.get("runs"), "runs", required=_RUN_FIELDS)
    if runs.get("schema_version") != SCHEMA_VERSION:
        raise BuildRefused("runs.schema_version is unsupported")
    integer_fields = (
        "current_validation_run_row_count",
        "projection_admitted_physical_row_count",
        "projection_admitted_represented_run_count",
        "projection_excluded_physical_row_count",
        "projection_excluded_represented_run_count",
        "series_physical_row_count",
        "series_represented_run_count",
        "validation_history_row_count",
    )
    for field in integer_fields:
        _integer(runs.get(field), f"runs.{field}")

    history = _array(
        runs.get("validation_history_rows"), "runs.validation_history_rows"
    )
    current = _array(
        runs.get("current_validation_run_rows"),
        "runs.current_validation_run_rows",
    )
    series = _array(runs.get("series_rows"), "runs.series_rows")
    if len(history) != runs["validation_history_row_count"]:
        raise BuildRefused("runs validation history count disagrees with its rows")
    if len(current) != runs["current_validation_run_row_count"]:
        raise BuildRefused("runs current validation count disagrees with its rows")
    if len(series) != runs["series_physical_row_count"]:
        raise BuildRefused("runs series physical count disagrees with its rows")
    represented = sum(
        _integer(
            _object(row, f"runs.series_rows[{index}]")
            .get("series", {})
            .get("num_runs"),
            f"runs.series_rows[{index}].series.num_runs",
            minimum=1,
        )
        for index, row in enumerate(series)
    )
    if represented != runs["series_represented_run_count"]:
        raise BuildRefused("runs series represented count disagrees with its rows")
    if (
        runs["projection_admitted_physical_row_count"]
        != runs["series_physical_row_count"]
        or runs["projection_admitted_represented_run_count"]
        != runs["series_represented_run_count"]
    ):
        raise BuildRefused("runs admitted counts disagree with persisted series rows")

    remaining = Counter(_canonical_bytes(row) for row in history)
    for row in current:
        encoded = _canonical_bytes(row)
        if remaining[encoded] == 0:
            raise BuildRefused(
                "runs current validation rows are not a sub-multiset of history rows"
            )
        remaining[encoded] -= 1

    exclusions = _array(
        runs.get("projection_exclusion_counts"),
        "runs.projection_exclusion_counts",
    )
    identities = []
    for index, raw in enumerate(exclusions):
        row = _exact_object(
            raw,
            f"runs.projection_exclusion_counts[{index}]",
            required=frozenset(
                {
                    "physical_row_count",
                    "reason",
                    "represented_run_count",
                    "schema",
                }
            ),
        )
        _string(row.get("schema"), f"runs.projection_exclusion_counts[{index}].schema")
        _string(row.get("reason"), f"runs.projection_exclusion_counts[{index}].reason")
        _integer(
            row.get("physical_row_count"),
            f"runs.projection_exclusion_counts[{index}].physical_row_count",
            minimum=1,
        )
        _integer(
            row.get("represented_run_count"),
            f"runs.projection_exclusion_counts[{index}].represented_run_count",
            minimum=1,
        )
        identities.append((str(row["schema"]), str(row["reason"])))
    if identities != sorted(set(identities)):
        raise BuildRefused(
            "runs.projection_exclusion_counts must be unique and canonical"
        )

    for field in (
        "current_main_ancestry",
        "product_results",
        "projection_results",
        "validate_run_selection",
    ):
        _object(runs.get(field), f"runs.{field}")
    return json.loads(_canonical_bytes(runs))


def _validated_tests_and_cells(
    data: Mapping[str, Any],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], dict[str, dict[str, Any]]]:
    tests = []
    test_by_id: dict[str, dict[str, Any]] = {}
    manifest_identities: set[str] = set()
    for index, raw in enumerate(_array(data.get("tests"), "tests")):
        row = dict(
            _exact_object(
                raw,
                f"tests[{index}]",
                required=_STAGE1_PERSISTED_TEST_KEYS,
            )
        )
        test_id = _string(row.get("id"), f"tests[{index}].id")
        if test_id in test_by_id:
            raise BuildRefused(f"tests repeats id {test_id!r}")
        for field in ("category", "description", "lane"):
            _string(row.get(field), f"tests[{index}].{field}")
        _boolean(row.get("occasional"), f"tests[{index}].occasional")
        identities = _array(row.get("cells"), f"tests[{index}].cells")
        for cell_index, identity_value in enumerate(identities):
            identity = _string(identity_value, f"tests[{index}].cells[{cell_index}]")
            if identity in manifest_identities or not identity.startswith(
                test_id + "/"
            ):
                raise BuildRefused(
                    f"tests[{index}].cells repeats or misbinds identity {identity!r}"
                )
            manifest_identities.add(identity)
        tests.append(row)
        test_by_id[test_id] = row
    if [row["id"] for row in tests] != sorted(test_by_id):
        raise BuildRefused("tests are not in canonical id order")

    cells = []
    cells_by_id = {}
    for index, raw in enumerate(_array(data.get("cells"), "cells")):
        row = dict(_object(raw, f"cells[{index}]"))
        policy = _stage1_policy_cell(row, f"cells[{index}]")
        identity = str(policy["cell"])
        if identity in cells_by_id or identity not in manifest_identities:
            raise BuildRefused(
                f"cells has an unknown or repeated identity {identity!r}"
            )
        if row.get("state") not in {"green", "red", "never-measured"}:
            raise BuildRefused(f"cells[{index}].state is unsupported")
        qualifying = _integer(
            row.get("qualifying_physical_row_count"),
            f"cells[{index}].qualifying_physical_row_count",
        )
        if row["state"] == "never-measured":
            if qualifying != 0:
                raise BuildRefused("never-measured cell has a qualifying row")
            for field in (
                "latest_event_ids",
                "latest_result",
                "latest_result_time",
                "latest_result_tree",
            ):
                if field in row:
                    raise BuildRefused(f"never-measured cell must omit {field}")
        else:
            if qualifying == 0:
                raise BuildRefused("green or red cell has no qualifying row")
            expected = "pass" if row["state"] == "green" else "product_failure"
            if row.get("latest_result") != expected:
                raise BuildRefused("cell state disagrees with latest_result")
            for field in (
                "latest_event_ids",
                "latest_result_time",
                "latest_result_tree",
            ):
                if field not in row:
                    raise BuildRefused(f"result cell omits {field}")
        cells.append(row)
        cells_by_id[identity] = row

    custom = []
    custom_ids = set()
    for index, raw in enumerate(
        _array(
            data.get("selected_by_full_custom_commands"),
            "selected_by_full_custom_commands",
        )
    ):
        policy = _stage1_policy_cell(
            _exact_object(
                raw,
                f"selected_by_full_custom_commands[{index}]",
                required=_STAGE1_POLICY_CELL_KEYS,
            ),
            f"selected_by_full_custom_commands[{index}]",
        )
        identity = str(policy["cell"])
        if identity in manifest_identities or identity in custom_ids:
            raise BuildRefused(f"custom commands repeats identity {identity!r}")
        if str(policy["test"]) not in test_by_id:
            raise BuildRefused(f"custom command {identity!r} names unknown test")
        custom_ids.add(identity)
        custom.append(policy)
    return cells, custom, test_by_id


def _validate_stage_provenance(
    provenance: Any, runs: Mapping[str, Any], historical: bool
) -> dict[str, Any]:
    item = _exact_object(
        provenance,
        "provenance",
        required=_PROVENANCE_FIELDS,
        optional=frozenset({"historical"}),
    )
    for field in ("hermit_commit", "hermit_main_commit", "parent_commit"):
        _sha(item.get(field), f"provenance.{field}")
    for field, value in item.items():
        if field.endswith("_sha256"):
            _sha256(value, f"provenance.{field}")
    for field in (
        "build_data_schema_version",
        "current_validation_run_row_count",
        "manifest_metadata_schema",
        "manifest_schema",
        "reducer_schema_version",
        "series_physical_row_count",
        "series_represented_run_count",
        "validation_event_count",
        "validation_history_row_count",
    ):
        _integer(item.get(field), f"provenance.{field}")
    if item["build_data_schema_version"] != SCHEMA_VERSION:
        raise BuildRefused("provenance build-data schema version is unsupported")
    if item["manifest_metadata_schema"] != 1:
        raise BuildRefused("provenance manifest metadata schema is unsupported")
    if item["reducer_schema_version"] != _modules().reducer.SCHEMA_VERSION:
        raise BuildRefused("provenance reducer schema version is unsupported")
    _object(item.get("series_schema_counts"), "provenance.series_schema_counts")

    expected_counts = {
        "current_validation_run_row_count": runs["current_validation_run_row_count"],
        "series_physical_row_count": runs["series_physical_row_count"],
        "series_represented_run_count": runs["series_represented_run_count"],
        "validation_history_row_count": runs["validation_history_row_count"],
    }
    if any(item[field] != value for field, value in expected_counts.items()):
        raise BuildRefused("provenance counts disagree with published runs")
    expected_digests = {
        "current_main_ancestry_sha256": _digest(runs["current_main_ancestry"]),
        "current_validation_run_rows_projection_sha256": _digest(
            runs["current_validation_run_rows"]
        ),
        "product_results_sha256": _digest(runs["product_results"]),
        "projection_results_sha256": _digest(runs["projection_results"]),
        "series_rows_projection_sha256": _digest(runs["series_rows"]),
        "validate_run_selection_sha256": _digest(runs["validate_run_selection"]),
        "validation_history_rows_projection_sha256": _digest(
            runs["validation_history_rows"]
        ),
    }
    if any(item[field] != value for field, value in expected_digests.items()):
        raise BuildRefused("provenance digests disagree with published runs")

    canonical = _object(
        item.get("canonical_input_provenance"),
        "provenance.canonical_input_provenance",
    )
    if historical != ("historical" in item) or historical != (
        "historical" in canonical
    ):
        raise BuildRefused(
            "historical provenance must exist exactly for historical data"
        )
    canonical_request = dict(canonical)
    if historical:
        historical_source = _object(
            canonical.get("historical"),
            "provenance.canonical_input_provenance.historical",
        )
        canonical_request["historical"] = {
            field: historical_source[field] for field in ("record_id", "run_id")
        }
    canonical_value = _canonical_provenance(
        canonical_request, item["parent_commit"], historical=historical
    )
    comparable_canonical = dict(canonical)
    if historical:
        comparable_canonical["historical"] = canonical_request["historical"]
    if canonical_value != comparable_canonical:
        raise BuildRefused("canonical input provenance is not canonical")
    if historical:
        source = _exact_object(
            item.get("historical"),
            "provenance.historical",
            required=_HISTORICAL_PROVENANCE_FIELDS,
        )
        base = _historical_source(
            {field: source[field] for field in _HISTORICAL_SOURCE_FIELDS},
            "provenance.historical",
        )
        if base != _historical_source(
            canonical.get("historical"),
            "provenance.canonical_input_provenance.historical",
        ):
            raise BuildRefused("historical provenance disagrees with canonical input")
        expected_commits = {
            "ledger_commit": item["parent_commit"],
            "ledger_ref": canonical.get("published_ref"),
            "manifest_source_commit": item["hermit_commit"],
        }
        for field, expected_commit in expected_commits.items():
            if (
                _sha(source.get(field), f"provenance.historical.{field}")
                != expected_commit
            ):
                raise BuildRefused(
                    f"provenance.historical.{field} disagrees with its bound commit"
                )
    return dict(item)


def _validate_build_data(value: Any) -> dict[str, Any]:
    data = _exact_object(
        value,
        "build data",
        required=frozenset(
            {
                "cells",
                "counts",
                "provenance",
                "projection_audit",
                "runs",
                "schema_version",
                "selected_by_full_custom_commands",
                "tests",
            }
        ),
    )
    if data.get("schema_version") != SCHEMA_VERSION:
        raise BuildRefused("build data schema_version is unsupported")
    runs = _validated_output_runs(data)
    historical = "historical" in _object(data.get("provenance"), "provenance")
    provenance = _validate_stage_provenance(data.get("provenance"), runs, historical)
    cells, custom, tests_by_id = _validated_tests_and_cells(data)
    projection_audit = _validated_projection_audit(
        data.get("projection_audit"), "projection_audit"
    )
    deliberate_omissions = projection_audit["deliberate_omissions"]
    counts = dict(_object(data.get("counts"), "counts"))
    expected_count_fields = {
        "current_validation_run_row_count",
        "green_cell_count",
        "in_manifest_cell_count",
        "never_measured_cell_count",
        "physical_row_count",
        "red_cell_count",
        "represented_run_count",
        "selected_by_full_custom_command_count",
        "test_count",
        "validation_history_row_count",
    }
    if set(counts) != expected_count_fields:
        raise BuildRefused("counts has an unsupported shape")
    for field in expected_count_fields:
        _integer(counts.get(field), f"counts.{field}")
    state_counts = {
        state: sum(cell["state"] == state for cell in cells)
        for state in ("green", "red", "never-measured")
    }
    expected_counts = {
        "current_validation_run_row_count": runs["current_validation_run_row_count"],
        "green_cell_count": state_counts["green"],
        "in_manifest_cell_count": sum(
            len(_array(test["cells"], f"tests[{test_id!r}].cells"))
            for test_id, test in tests_by_id.items()
        ),
        "never_measured_cell_count": state_counts["never-measured"],
        "physical_row_count": runs["series_physical_row_count"],
        "red_cell_count": state_counts["red"],
        "represented_run_count": runs["series_represented_run_count"],
        "selected_by_full_custom_command_count": len(custom),
        "test_count": len(tests_by_id),
        "validation_history_row_count": runs["validation_history_row_count"],
    }
    if counts != expected_counts:
        raise BuildRefused("counts disagree with exact persisted populations")
    if len(cells) != sum(state_counts.values()):
        raise BuildRefused(
            "cell rows disagree with green, red, and never-measured counts"
        )
    if len(cells) > counts["in_manifest_cell_count"]:
        raise BuildRefused("cell rows exceed the in-manifest population")
    manifest_identities = {
        str(identity) for test in tests_by_id.values() for identity in test["cells"]
    }
    cell_identities = {str(cell["cell"]) for cell in cells}
    omission_identities = {str(omission["cell"]) for omission in deliberate_omissions}
    if cell_identities.intersection(omission_identities):
        raise BuildRefused(
            "published cells and deliberate omissions have overlapping identities"
        )
    if cell_identities.union(omission_identities) != manifest_identities:
        raise BuildRefused(
            "published cells and deliberate omissions do not exhaust manifest identities"
        )
    if len(cells) + len(deliberate_omissions) != counts["in_manifest_cell_count"]:
        raise BuildRefused(
            "published cells and deliberate omissions disagree with manifest count"
        )
    decisions = [*cells, *deliberate_omissions]
    custom_identities = {str(item["cell"]) for item in custom}
    custom_physical_row_count = 0
    custom_represented_run_count = 0
    for index, raw in enumerate(runs["series_rows"]):
        row = _object(raw, f"runs.series_rows[{index}]")
        series = _object(row.get("series"), f"runs.series_rows[{index}].series")
        identity = _string(series.get("cell"), f"runs.series_rows[{index}].series.cell")
        if identity not in manifest_identities and identity not in custom_identities:
            raise BuildRefused(
                f"runs.series_rows[{index}] names an unknown cell identity"
            )
        if identity in custom_identities:
            custom_physical_row_count += 1
            custom_represented_run_count += _integer(
                series.get("num_runs"),
                f"runs.series_rows[{index}].series.num_runs",
                minimum=1,
            )
    if (
        sum(int(item["physical_row_count"]) for item in decisions)
        + custom_physical_row_count
        != runs["series_physical_row_count"]
    ):
        raise BuildRefused(
            "published cells, deliberate omissions, and custom commands do not "
            "partition physical rows"
        )
    if (
        sum(int(item["represented_run_count"]) for item in decisions)
        + custom_represented_run_count
        != runs["series_represented_run_count"]
    ):
        raise BuildRefused(
            "published cells, deliberate omissions, and custom commands do not "
            "partition represented runs"
        )

    reducer_manifest_cells = []
    for test in tests_by_id.values():
        for identity in test["cells"]:
            try:
                test_name, mode, backend = str(identity).rsplit("/", 2)
            except ValueError as error:
                raise BuildRefused(
                    f"manifest cell identity is malformed: {identity!r}"
                ) from error
            reducer_manifest_cells.append(
                {"test": test_name, "mode": mode, "backend": backend}
            )
    canonical = {
        "schema_version": 2,
        "series": {
            "row_count": len(runs["series_rows"]),
            "represented_run_count": runs["series_represented_run_count"],
            "rows": runs["series_rows"],
            "rows_sha256": _digest(runs["series_rows"]),
        },
    }
    try:
        reduced = _modules().reducer.reduce_inputs(
            canonical,
            {"schema": 1, "cells": reducer_manifest_cells},
            projection_results=runs["projection_results"],
            current_main_ancestry=runs["current_main_ancestry"],
            product_results=runs["product_results"],
            validate_run_selection=runs["validate_run_selection"],
        )
    except _modules().reducer.ReductionRefused as error:
        raise BuildRefused(f"published reducer inputs refuse: {error}") from error
    if reduced["counts"] != {
        field: counts[field]
        for field in (
            "green_cell_count",
            "in_manifest_cell_count",
            "never_measured_cell_count",
            "physical_row_count",
            "red_cell_count",
            "represented_run_count",
        )
    }:
        raise BuildRefused("published reducer counts disagree with exact rerun")
    if reduced["projection_audit"] != projection_audit:
        raise BuildRefused(
            "published projection audit disagrees with exact reducer rerun"
        )
    expected_by_id = {row["cell"]: row for row in reduced["cells"]}
    for cell in cells:
        identity = str(cell["cell"])
        expected = expected_by_id.get(identity)
        if expected is None:
            raise BuildRefused(
                f"published cell {identity!r} is absent from reducer output"
            )
        allowed = set(expected) | set(_STAGE1_POLICY_CELL_KEYS)
        unknown = set(cell).difference(allowed)
        if unknown:
            raise BuildRefused(
                f"published cell {identity!r} contains unknown field(s): "
                + ", ".join(sorted(unknown))
            )
        for key, expected_value in expected.items():
            if cell.get(key) != expected_value:
                raise BuildRefused(
                    f"published cell {identity!r} disagrees with reducer field {key}"
                )
    if set(expected_by_id) != {str(cell["cell"]) for cell in cells}:
        raise BuildRefused("published cells omit or add a reducer result")

    _require_publishable_output(data)
    return json.loads(_canonical_bytes(data))


def validate_build_data(value: Any) -> dict[str, Any]:
    try:
        return _validate_build_data(value)
    except (KeyError, RecursionError, TypeError) as error:
        raise BuildRefused(f"build data is malformed: {error}") from error


def _build_from_captured(
    canonical_document: CapturedJson,
    manifest_document: CapturedJson,
    hermit: Path,
    commits: Mapping[str, str],
    historical_identity: tuple[str, str] | None,
) -> dict[str, Any]:
    historical = historical_identity is not None
    canonical_provenance = _canonical_provenance(
        canonical_document.value.get("provenance"),
        commits["parent"],
        historical=historical,
    )
    if historical:
        request = _object(canonical_provenance["historical"], "historical request")
        if (request["record_id"], request["run_id"]) != historical_identity:
            raise BuildRefused(
                "historical canonical input disagrees with requested identity"
            )
        reader = _object(
            canonical_provenance["reader_hermit"],
            "historical reader Hermit identity",
        )
        if reader["checkout_commit"] != commits["hermit"]:
            raise BuildRefused(
                "historical reader Hermit checkout disagrees with the exact "
                "manifest source checkout"
            )
        recorded_reader_gitlink = _gitlink_commit(
            REPOSITORY,
            canonical_provenance["reader_commit"],
            "hermit",
        )
        if reader["gitlink_commit"] != recorded_reader_gitlink:
            raise BuildRefused(
                "historical reader Hermit gitlink disagrees with the gitlink "
                "recorded by the generator base commit"
            )
    projection = _run_projection(
        manifest_document,
        canonical_document,
        hermit,
        commits["hermit_main"],
        historical_identity,
    )
    reduced = _reduce_projection(projection)
    provenance = _build_provenance(
        canonical_document,
        canonical_provenance,
        manifest_document,
        projection,
        commits,
    )
    return _artifact_result(projection, reduced, provenance)


def build_data(
    parent_value: Path,
    parent_commit_value: str,
    hermit_value: Path,
    hermit_commit_value: str,
    hermit_main_commit_value: str,
) -> dict[str, Any]:
    """Build one deterministic reduction from exact current authorities."""
    _modules()
    parent_commit = _sha(parent_commit_value, "parent commit")
    hermit_commit = _sha(hermit_commit_value, "Hermit commit")
    main = _sha(hermit_main_commit_value, "Hermit main commit")
    parent = _require_checkout(
        parent_value, parent_commit, label="dev-hermit", clean=True
    )
    hermit = _require_checkout(hermit_value, hermit_commit, label="Hermit", clean=True)
    _require_main_target(hermit, main)
    canonical = _capture_canonical_input(parent, parent_commit)
    manifest = _run_manifest_metadata(hermit)
    _require_checkout(parent, parent_commit, label="dev-hermit", clean=True)
    _require_checkout(hermit, hermit_commit, label="Hermit", clean=True)
    _require_main_target(hermit, main)
    result = _build_from_captured(
        canonical,
        manifest,
        hermit,
        {"hermit": hermit_commit, "hermit_main": main, "parent": parent_commit},
        None,
    )
    _require_checkout(parent, parent_commit, label="dev-hermit", clean=True)
    _require_checkout(hermit, hermit_commit, label="Hermit", clean=True)
    _require_main_target(hermit, main)
    return result


def build_historical_data(
    parent_value: Path,
    parent_commit_value: str,
    record_id: str,
    run_id: str,
    manifest_hermit_value: Path,
    manifest_commit_value: str,
) -> dict[str, Any]:
    """Build from one immutable published ledger snapshot and named HistoryRow."""
    _modules()
    parent_commit = _sha(parent_commit_value, "historical parent commit")
    manifest_commit = _sha(manifest_commit_value, "manifest source commit")
    parent = parent_value.expanduser().resolve()
    if not parent.is_dir():
        raise BuildRefused(f"dev-hermit checkout is not a directory: {parent}")
    top = Path(_git_text(parent, ["rev-parse", "--show-toplevel"])).resolve()
    if top != parent:
        raise BuildRefused(
            f"dev-hermit checkout must name its Git root, got {parent} (root is {top})"
        )
    resolved_parent = _git_text(
        parent, ["rev-parse", "--verify", f"{parent_commit}^{{commit}}"]
    )
    if resolved_parent != parent_commit:
        raise BuildRefused(
            f"historical parent commit resolved to {resolved_parent}, expected {parent_commit}"
        )
    hermit_main = _gitlink_commit(parent, parent_commit, "hermit")
    manifest_hermit = _require_checkout(
        manifest_hermit_value,
        manifest_commit,
        label="manifest source Hermit",
        clean=True,
    )
    _require_complete_commit_graph(manifest_hermit)
    canonical = _capture_historical_input(parent, parent_commit, record_id, run_id)
    manifest = _run_manifest_metadata(manifest_hermit)
    result = _build_from_captured(
        canonical,
        manifest,
        manifest_hermit,
        {
            "hermit": manifest_commit,
            "hermit_main": hermit_main,
            "parent": parent_commit,
        },
        (record_id, run_id),
    )
    if (
        _git_text(parent, ["rev-parse", "--verify", f"{parent_commit}^{{commit}}"])
        != parent_commit
        or _gitlink_commit(parent, parent_commit, "hermit") != hermit_main
    ):
        raise BuildRefused("historical parent commit changed during build")
    _require_checkout(
        manifest_hermit,
        manifest_commit,
        label="manifest source Hermit",
        clean=True,
    )
    return result


def render_build_data(value: Mapping[str, Any]) -> bytes:
    return _canonical_bytes(value) + b"\n"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--parent",
        required=True,
        type=Path,
        help="exact dev-hermit checkout whose canonical ledger and series are read",
    )
    parser.add_argument(
        "--parent-commit",
        required=True,
        help="full dev-hermit commit currently checked out by --parent",
    )
    parser.add_argument(
        "--hermit",
        required=True,
        type=Path,
        help="exact clean Hermit checkout used for manifest policy and ancestry",
    )
    parser.add_argument(
        "--hermit-commit",
        required=True,
        help="full Hermit commit currently checked out by --hermit",
    )
    parser.add_argument(
        "--hermit-main-commit",
        required=True,
        help="full Hermit ancestry target; must equal the checkout's stable refs/remotes/origin/main",
    )
    arguments = parser.parse_args(argv)
    try:
        value = build_data(
            arguments.parent,
            arguments.parent_commit,
            arguments.hermit,
            arguments.hermit_commit,
            arguments.hermit_main_commit,
        )
        encoded = render_build_data(value)
    except (BuildRefused, OSError, ValueError) as error:
        print(f"compatibility-website build data: REFUSED: {error}", file=sys.stderr)
        return 2
    sys.stdout.buffer.write(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
