#!/usr/bin/env python3
"""Bind canonical compatibility inputs to the pure website reducer.

This is the production boundary between the existing canonical readers and the
pure reduction.  It does not invent unavailable producer facts: historical
v1/v2 product results and all producer-owned execution ordering remain absent.
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
from collections import Counter
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any


CI_HUB = Path(__file__).resolve().parents[1]
EXPORT_INPUTS_PATH = Path(__file__).resolve().with_name("export_inputs.py")
REDUCE_PATH = Path(__file__).resolve().with_name("reduce.py")
QUALIFYING_RECEIPT_PATH = CI_HUB / "qualifying_receipt.py"
BOUNDED_CAPTURE_PATH = CI_HUB / "lib/bounded_capture.py"

SCHEMA_VERSION = 1
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_OWNER_PATH_NEEDLE = "/" + "home" + "/"
_OWNER_PATH_RE = re.compile(r"(?:/" + "home" + r"/[^/\s]+|/Users/[^/\s]+|/root)(?:/|$)|[A-Z]:[\\/](?:Users|Documents and Settings)[\\/][^\\/\s]+", re.IGNORECASE)
_CREDENTIAL_RE = re.compile(r"\bauthorization\s*[:=]\s*(?:basic|bearer)\s+\S+|(?:^|[\s;&])(?:[A-Za-z0-9]+[_-])*(?:access[_-]?(?:key|token)|api[_-]?key|client[_-]?secret|credential|passwd|password|private[_-]?key|secret|token)\s*[:=]\s*\S+|\b(?:gh[pousr]|github_pat)_[A-Za-z0-9_]+\b|https?://[^/@\s:]+:[^/@\s]+@|-----BEGIN [A-Z ]*PRIVATE KEY-----", re.IGNORECASE)
_CREDENTIAL_KEY_RE = re.compile(r"(?:^|[_-])(?:access[_-]?token|api[_-]?key|authorization|client[_-]?secret|credential|passwd|password|private[_-]?key|secret|token)(?:$|[_-])", re.IGNORECASE)
_TEST_FIELDS = ("build", "category", "description", "direct", "id", "lane", "observation", "occasional", "preprocessors", "program", "requires")
_CELL_FIELDS = ("backend", "category", "current_reproducer", "current_reproducer_unavailable_reason", "guest_args", "lane", "mode", "not_applicable_reason", "not_selected_by_full_reason", "selected_by_full", "test", "timeout_seconds", "workdir")
_VALIDATION_FIELDS = ("checks", "commit", "corrects", "executed_nodes", "executed_tests", "exit_code", "failures", "filtered_tests", "finished_at", "gates_expected", "gates_run", "host", "producer", "profile", "real_seconds", "record_id", "repo", "result", "run_id", "run_number", "schema_version", "selection_mode", "skipped_nodes", "started_at", "sys_seconds", "tree", "tree_dirty", "user_seconds")
_GATE_FIELDS = ("aborted", "cpu_timed_out", "exit_code", "failure_class", "failure_origin", "first_attempt_result", "name", "oom_kills", "oomed", "real_seconds", "reported", "result", "retries", "timed_out")
_SERIES_FIELDS = ("emitted_at", "event_id", "event_type", "host", "producer", "run_id", "schema", "team")
_SERIES_PAYLOAD_FIELDS = ("attempt", "cell", "coordinates", "depth", "detcore_tree", "failure_class", "host_capabilities", "kernel_version", "last_run_index", "machine_shortname", "no_verdict_evidence", "num_runs", "outcome", "result", "run_index", "runtime", "source_tree_dirty", "tree")
_PROVENANCE_FIELDS = frozenset("build_data_schema_version canonical_input_bytes_sha256 canonical_input_provenance current_main_ancestry_sha256 current_validation_run_row_count current_validation_run_rows_projection_sha256 current_validation_run_rows_sha256 hermit_commit hermit_main_commit manifest_metadata_bytes_sha256 manifest_metadata_projection_sha256 manifest_metadata_schema manifest_schema manifest_sha256 missing_executions_sha256 parent_commit product_results_sha256 projection_checker_input_jsonl_sha256 projection_results_sha256 reducer_schema_version series_physical_row_count series_represented_run_count series_rows_projection_sha256 series_rows_sha256 series_schema_counts validate_run_selection_sha256 validation_cell_results_incomplete_data_sha256 validation_event_count validation_events_sha256 validation_history_row_count validation_history_rows_projection_sha256 validation_history_rows_sha256".split())
_PROVENANCE_COMMITS = "hermit_commit hermit_main_commit parent_commit".split()
_PROVENANCE_DIGESTS = "canonical_input_bytes_sha256 current_main_ancestry_sha256 current_validation_run_rows_projection_sha256 current_validation_run_rows_sha256 manifest_metadata_bytes_sha256 manifest_metadata_projection_sha256 manifest_sha256 missing_executions_sha256 product_results_sha256 projection_checker_input_jsonl_sha256 projection_results_sha256 series_rows_projection_sha256 series_rows_sha256 validate_run_selection_sha256 validation_cell_results_incomplete_data_sha256 validation_events_sha256 validation_history_rows_projection_sha256 validation_history_rows_sha256".split()
_PROVENANCE_COUNTS = "current_validation_run_row_count manifest_metadata_schema manifest_schema reducer_schema_version series_physical_row_count series_represented_run_count validation_event_count validation_history_row_count".split()
_INCOMPLETE_FIELDS = frozenset({"field", "input", "reason"})
_INCOMPLETE_OPTIONAL_FIELDS = frozenset({"cell", "event_id", "path", "record_id", "run_id", "schema"})
_RUN_FIELDS = frozenset("current_main_ancestry current_validation_run_row_count current_validation_run_rows projection_admitted_physical_row_count projection_admitted_represented_run_count projection_excluded_physical_row_count projection_excluded_represented_run_count schema_version series_physical_row_count series_represented_run_count series_rows validation_history_row_count validation_history_rows".split())
# Public series rows intentionally omit source `series.main_ancestry` and
# `series.first_divergent_messages`. They instead publish current authority and
# derived classification in these outer fields; the projection digest binds
# that exact lossy row shape and source order.
_SERIES_OUTPUT_FIELDS = frozenset(_SERIES_FIELDS) | frozenset({"current_main_ancestry", "product_result", "product_result_available", "projection_validation_reason", "series"})
_VALIDATION_OUTPUT_FIELDS = frozenset(_VALIDATION_FIELDS) | frozenset({"cell_results", "gates", "history_row_sha256"})
_LEGACY_NULL_FIELDS = {
    "corrects": (4, (), "schema-4 legacy corrects metadata is not a stable record-id correction edge"),
    "tree": (5, ("run_id", "cell_results"), "schema-5 validation rows do not contain a tree object id"),
}
_STAGE_INCOMPLETE_DATA = (
    {"field": "cells selected by quick", "input": "manifest metadata", "reason": "the current manifest export records only cells selected by full"},
    {"field": "cells selected by super", "input": "manifest metadata", "reason": "the current manifest export records only cells selected by full"},
    {"field": "source epoch", "input": "canonical inputs", "reason": "no explicit reproducible source epoch is recorded"},
    {"field": "safe build commands and raw audit output", "input": "canonical input provenance", "reason": "the captured provenance records digests but no safe tokenized command list or raw audit output"},
)


class BuildRefused(Exception):
    """The supplied authorities cannot support one deterministic reduction."""


@dataclass(frozen=True)
class CapturedJson:
    """One parsed JSON document and the exact bytes that supplied it."""

    value: dict[str, Any]
    encoded: bytes


@dataclass(frozen=True)
class Modules:
    """The existing authority modules used by this adapter."""

    export_inputs: Any
    reducer: Any
    qualifying_receipt: Any
    bounded_capture: Any


_LOADED_MODULES: Modules | None = None


def _refuse_authority_overrides() -> None:
    if "QUALIFYING_RECEIPT_PREDICATE" in os.environ:
        raise BuildRefused(
            "QUALIFYING_RECEIPT_PREDICATE is a fixture-only override and cannot "
            "be used for compatibility build data"
        )


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
            bounded_capture=_load_module(
                "compatibility_website_bounded_capture", BOUNDED_CAPTURE_PATH
            ),
            export_inputs=_load_module(
                "compatibility_website_export_inputs", EXPORT_INPUTS_PATH
            ),
            reducer=_load_module("compatibility_website_reduce", REDUCE_PATH),
            qualifying_receipt=_load_module(
                "compatibility_website_qualifying_receipt", QUALIFYING_RECEIPT_PATH
            ),
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


def _process_detail(stderr: bytes, stdout: bytes) -> str:
    detail = (stderr or stdout).decode(errors="replace").strip()
    if len(detail) > 2_000:
        return detail[:2_000] + "... [truncated]"
    return detail or "no output"


def _object(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise BuildRefused(f"{label} must be an object")
    return value


def _array(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise BuildRefused(f"{label} must be an array")
    return value


def _string(value: Any, label: str, *, optional: bool = False) -> str | None:
    if value is None and optional:
        return None
    if not isinstance(value, str) or not value.strip():
        raise BuildRefused(f"{label} must be a nonempty string")
    return value


def _integer(value: Any, label: str, *, minimum: int = 0) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise BuildRefused(f"{label} must be an integer >= {minimum}")
    return value


def _number(value: Any, label: str) -> int | float:
    if not isinstance(value, (int, float)) or isinstance(value, bool) or value < 0:
        raise BuildRefused(f"{label} must be a nonnegative number")
    return value


def _boolean(value: Any, label: str) -> bool:
    if not isinstance(value, bool):
        raise BuildRefused(f"{label} must be a bool")
    return value


def _sha(value: Any, label: str) -> str:
    digest = _string(value, label)
    if _SHA_RE.fullmatch(digest) is None:
        raise BuildRefused(f"{label} must be a lowercase 40-hex commit")
    return digest


def _sha256(value: Any, label: str) -> str:
    digest = _string(value, label)
    if _SHA256_RE.fullmatch(digest) is None:
        raise BuildRefused(f"{label} must be lowercase 64-hex")
    return digest


def _object_id(value: Any, label: str) -> str:
    object_id = _string(value, label)
    if re.fullmatch(r"[0-9a-f]{40,64}", object_id) is None:
        raise BuildRefused(f"{label} must be a lowercase 40- or 64-hex object id")
    return object_id


def _relative_path(value: Any, label: str) -> str:
    raw = _string(value, label)
    path = PurePosixPath(raw)
    if path.is_absolute() or not path.parts or ".." in path.parts:
        raise BuildRefused(f"{label} must be a safe relative path")
    return path.as_posix()


def _exact_object(value: Any, label: str, *, required: frozenset[str],
                  optional: frozenset[str] = frozenset()) -> Mapping[str, Any]:
    item = _object(value, label)
    if any(not isinstance(key, str) for key in item):
        raise BuildRefused(f"{label} keys must be strings")
    missing = sorted(required.difference(item))
    unknown = sorted(set(item).difference(required | optional))
    if missing:
        raise BuildRefused(f"{label} is missing required field(s): {', '.join(missing)}")
    if unknown:
        raise BuildRefused(f"{label} has unknown field(s): {', '.join(unknown)}")
    return item


def _copy_fields(value: Any, label: str, fields: Sequence[str], *,
                 required: Sequence[str] = (), exact: bool = False) -> dict[str, Any]:
    if exact:
        required_set = frozenset(required)
        item = _exact_object(value, label, required=required_set, optional=frozenset(fields) - required_set)
    else:
        item = _object(value, label)
        missing = [field for field in required if field not in item]
        if missing:
            raise BuildRefused(f"{label} is missing required field(s): {', '.join(missing)}")
    return json.loads(_canonical_bytes({field: item[field] for field in fields if field in item}))


def _strings(value: Any, label: str) -> list[str]:
    return [_string(item, f"{label}[{index}]") for index, item in enumerate(_array(value, label))]


def _validate_fields(value: Mapping[str, Any], label: str, fields: Sequence[str], validate: Any) -> None:
    for field in fields:
        if value.get(field) is not None:
            validate(value[field], f"{label}.{field}")


def _bound_rows(value: Mapping[str, Any], label: str, rows_field: str, count_field: str, digest_field: str) -> list[Any]:
    rows = _array(value.get(rows_field), f"{label}.{rows_field}")
    for field, validate, expected in ((count_field, _integer, len(rows)), (digest_field, _sha256, _digest(rows))):
        if validate(value.get(field), f"{label}.{field}") != expected:
            raise BuildRefused(f"{label}.{field} disagrees with rows")
    return rows


def _git_environment() -> dict[str, str]:
    environment = {
        name: value for name, value in os.environ.items() if not name.startswith("GIT_")
    }
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    environment["GIT_NO_LAZY_FETCH"] = "1"
    return environment


def _capture(command: Sequence[str], *, timeout: float, cwd: Path | None = None,
             env: Mapping[str, str] | None = None,
             input: bytes | None = None) -> subprocess.CompletedProcess[bytes]:
    options: dict[str, Any] = {
        "check": False,
        "cwd": cwd,
        "env": env,
        "text": False,
        "timeout": timeout,
    }
    if input is not None:
        options["input"] = input
    return _modules().bounded_capture.capture(command, **options)


def _git(checkout: Path, arguments: Sequence[str], *,
         accepted_returncodes: frozenset[int] = frozenset({0}),
         input: bytes | None = None) -> subprocess.CompletedProcess[bytes]:
    try:
        result = _capture(
            ["git", "--no-replace-objects", "-C", str(checkout), *arguments],
            timeout=60,
            env=_git_environment(),
            input=input,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise BuildRefused(f"cannot run git in {checkout}: {error}") from error
    if result.returncode not in accepted_returncodes:
        detail = _process_detail(result.stderr, result.stdout)
        raise BuildRefused(
            f"git {' '.join(arguments)} failed in {checkout}: "
            f"{detail if detail != 'no output' else f'exit {result.returncode} with no output'}"
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
    if clean and _git(
        checkout,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    ).stdout:
        raise BuildRefused(f"{label} checkout has local changes")
    return checkout


def _require_main_target(checkout: Path, expected_commit: str) -> None:
    _require_complete_commit_graph(checkout)
    resolved = _git_text(
        checkout,
        ["rev-parse", "--verify", f"{expected_commit}^{{commit}}"],
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
    try:
        result = _capture(
            command,
            cwd=hermit,
            timeout=900,
            env=_git_environment(),
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise BuildRefused(f"manifest metadata command could not run: {error}") from error
    if result.returncode != 0:
        detail = _process_detail(result.stderr, result.stdout)
        raise BuildRefused(
            "manifest metadata command failed: "
            f"{detail if detail != 'no output' else f'exit {result.returncode} with no output'}"
        )
    try:
        value = json.loads(result.stdout)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BuildRefused(
            f"manifest metadata command returned malformed JSON: {error}"
        ) from error
    if not isinstance(value, dict):
        raise BuildRefused("manifest metadata command must return one JSON object")
    return CapturedJson(value=value, encoded=result.stdout)


def _series_population(canonical_input: Mapping[str, Any]) -> tuple[Mapping[str, Any], list[dict[str, Any]], dict[str, str], dict[str, int], int, str]:
    if _integer(canonical_input.get("schema_version"), "canonical input schema_version", minimum=1) != 1:
        raise BuildRefused("canonical input schema_version must be 1")
    series = _object(canonical_input.get("series"), "canonical input series")
    rows = _bound_rows(series, "series", "rows", "row_count", "rows_sha256")
    copied_rows, schemas, represented = [], {}, {}
    for index, raw in enumerate(rows):
        row = _object(raw, f"series rows[{index}]")
        event_id = _string(row.get("event_id"), f"series rows[{index}].event_id")
        if event_id in schemas:
            raise BuildRefused("canonical series rows contain duplicate event_id values")
        copied_rows.append(dict(row))
        schemas[event_id] = _string(row.get("schema"), f"series rows[{index}].schema")
        payload = _object(row.get("series"), f"series rows[{index}].series")
        represented[event_id] = _integer(
            payload.get("num_runs", 1), f"series rows[{index}].series.num_runs", minimum=1
        )
    represented_run_count = _integer(series.get("represented_run_count"), "series.represented_run_count")
    if represented_run_count != sum(represented.values()):
        raise BuildRefused("series.represented_run_count disagrees with rows")
    expected_schemas = dict(sorted(Counter(schemas.values()).items()))
    if series.get("schema_counts") != expected_schemas:
        raise BuildRefused("series.schema_counts disagrees with rows")
    return series, copied_rows, schemas, represented, represented_run_count, str(series["rows_sha256"])


def _validate_projection_counts(
    series: Mapping[str, Any],
    schemas: Mapping[str, str],
    represented: Mapping[str, int],
    represented_run_count: int,
    projection: Mapping[str, str | None],
) -> None:
    admitted_physical = sum(reason is None for reason in projection.values())
    admitted_represented = sum(
        represented[event_id]
        for event_id, reason in projection.items()
        if reason is None
    )
    excluded_physical = len(projection) - admitted_physical
    excluded_represented = represented_run_count - admitted_represented
    declared_counts = {
        "projection_admitted_physical_row_count": admitted_physical,
        "projection_admitted_represented_run_count": admitted_represented,
        "projection_excluded_physical_row_count": excluded_physical,
        "projection_excluded_represented_run_count": excluded_represented,
    }
    for field, expected in declared_counts.items():
        if _integer(series.get(field), f"series.{field}") != expected:
            raise BuildRefused(f"series.{field} disagrees with projection_results")

    physical_by_reason: Counter[tuple[str, str]] = Counter()
    represented_by_reason: Counter[tuple[str, str]] = Counter()
    for event_id, reason in projection.items():
        if reason is None:
            continue
        key = (schemas[event_id], reason)
        physical_by_reason[key] += 1
        represented_by_reason[key] += represented[event_id]
    expected_exclusions = [
        {
            "physical_row_count": count,
            "reason": reason,
            "represented_run_count": represented_by_reason[(schema, reason)],
            "schema": schema,
        }
        for (schema, reason), count in sorted(physical_by_reason.items())
    ]
    if series.get("projection_exclusion_counts") != expected_exclusions:
        raise BuildRefused(
            "series.projection_exclusion_counts disagrees with projection_results"
        )


def _validate_projection(canonical_input: Mapping[str, Any]) -> tuple[list[dict[str, Any]], dict[str, str | None]]:
    series, rows, schemas, represented, represented_count, rows_sha256 = _series_population(canonical_input)
    raw_projection = _object(series.get("projection_results"), "series.projection_results")
    projection = {}
    for event_id, reason in raw_projection.items():
        event_id = _string(event_id, "series.projection_results key")
        projection[event_id] = None if reason is None else _string(reason, f"series.projection_results[{event_id!r}]")
    missing, unknown = sorted(set(schemas) - set(projection)), sorted(set(projection) - set(schemas))
    if missing or unknown:
        detail = "omitted" if missing else "names unknown"
        raise BuildRefused(f"series.projection_results {detail} event_id(s): " + ", ".join(missing or unknown))
    if _integer(series.get("projection_result_physical_row_count"), "series.projection_result_physical_row_count") != len(rows):
        raise BuildRefused("series.projection_result_physical_row_count disagrees with rows")
    if _sha256(series.get("projection_results_sha256"), "series.projection_results_sha256") != _digest(projection):
        raise BuildRefused("series.projection_results_sha256 disagrees with projection_results")
    binding = _exact_object(series.get("projection_results_binding"), "series.projection_results_binding", required=frozenset({"checker_input_jsonl_sha256", "physical_row_count", "represented_run_count", "rows_sha256"}))
    expected_binding = {
        "checker_input_jsonl_sha256": hashlib.sha256(b"".join(row + b"\n" for row in sorted(_canonical_bytes(row) for row in rows))).hexdigest(),
        "physical_row_count": len(rows),
        "represented_run_count": represented_count,
        "rows_sha256": rows_sha256,
    }
    mismatch = next((field for field, expected in expected_binding.items() if binding.get(field) != expected), None)
    if mismatch is not None:
        raise BuildRefused(f"series.projection_results_binding {mismatch} disagrees with rows")
    projection = dict(sorted(projection.items()))
    _validate_projection_counts(series, schemas, represented, represented_count, projection)
    return rows, projection


_V3_PRODUCT_RESULTS: dict[tuple[str, str | None, str | None], str | None] = {
    ("passed", "pass", None): "pass",
    ("diverged", "determinism-failure", "product_failure"): "product_failure",
    ("diverged", "parity-failure", "product_failure"): "product_failure",
    ("diverged", "replay-failure", "product_failure"): "product_failure",
    ("errored", "crash-error", "product_failure"): "product_failure",
    ("timeout", "timeout", "no_result"): None,
    ("no_result", "oom", "no_result"): None,
    ("errored", None, "understood_infrastructure_failure"): None,
    ("skipped", None, "understood_prerequisite_failure"): None,
    ("no_result", None, "no_result"): None,
}


def _product_results(rows: Sequence[Mapping[str, Any]]) -> dict[str, str | None]:
    results: dict[str, str | None] = {}
    for index, row in enumerate(rows):
        if row.get("schema") != "stress-series/v3":
            continue
        payload = _object(row.get("series"), f"series rows[{index}].series")
        triple = (
            payload.get("outcome"),
            payload.get("result"),
            payload.get("failure_class"),
        )
        if triple not in _V3_PRODUCT_RESULTS:
            raise BuildRefused(
                f"series row {row.get('event_id')!r} has an unknown or "
                f"contradictory stress-series/v3 classification {triple!r}"
            )
        results[str(row["event_id"])] = _V3_PRODUCT_RESULTS[triple]
    return dict(sorted(results.items()))


def _current_validation_rows(canonical_input: Mapping[str, Any]) -> list[dict]:
    ledger = _object(
        canonical_input.get("validation_ledger"),
        "canonical input validation_ledger",
    )
    rows = _bound_rows(
        ledger,
        "validation_ledger",
        "current_validation_run_rows",
        "current_validation_run_row_count",
        "current_validation_run_rows_sha256",
    )
    return [dict(_object(row, "current validation row")) for row in rows]


def _cell_results_verdict(row: Mapping[str, Any]) -> Any:
    qualifying_receipt = _modules().qualifying_receipt
    try:
        return qualifying_receipt.cell_results_verdict(dict(row), qualifying_receipt.active())
    except (OSError, RuntimeError, ValueError) as error:
        raise BuildRefused(f"canonical cell_results verdict could not be evaluated: {error}") from error


def _require_satisfied_cell_results(row: Mapping[str, Any]) -> None:
    qualifying_receipt = _modules().qualifying_receipt
    verdict = _cell_results_verdict(row)
    if verdict is not qualifying_receipt.CellResultsVerdict.SATISFIED:
        raise BuildRefused(f"canonical cell_results verdict is {getattr(verdict, 'value', repr(verdict))}")


def _cell_identity(value: Any, label: str) -> str:
    item = _object(value, label)
    test = _string(item.get("test"), f"{label}.test")
    mode = _string(item.get("mode"), f"{label}.mode")
    backend = _string(item.get("backend"), f"{label}.backend")
    return f"{test}/{mode}/{backend}"


def _cell_sort_key(identity: str) -> tuple[str, str, str]:
    test, mode, backend = identity.rsplit("/", 2)
    return test, mode, backend


@dataclass(frozen=True)
class ValidationSelection:
    commit: str
    path: str
    record_id: str | None
    selected_cells: frozenset[str]


def _validation_selection_runs(canonical_input: Mapping[str, Any]) -> dict[str, ValidationSelection]:
    rows_by_run: dict[str, Mapping[str, Any]] = {}
    selections: dict[str, ValidationSelection] = {}
    for index, row in enumerate(_current_validation_rows(canonical_input)):
        if row.get("repo") != "hermit":
            continue
        raw_run_id = row.get("run_id")
        if not isinstance(raw_run_id, str) or not raw_run_id.strip():
            if row.get("cell_results") is not None:
                _require_satisfied_cell_results(row)
                raise BuildRefused(
                    f"current validation rows[{index}] has cell_results without "
                    "a stable nonempty run_id"
                )
            continue
        run_id = raw_run_id
        if run_id in rows_by_run:
            raise BuildRefused(f"current validation ledger contains duplicate run_id {run_id!r}")
        rows_by_run[run_id] = row
        evidence = row.get("cell_results")
        if evidence is None:
            continue
        _require_satisfied_cell_results(row)
        evidence = _object(evidence, f"current validation rows[{index}].cell_results")
        if _string(evidence.get("run_id"), f"current validation rows[{index}].cell_results.run_id") != run_id:
            raise BuildRefused(f"cell_results run_id disagrees for {run_id!r}")
        commit = _sha(row.get("commit"), f"current validation rows[{index}].commit")
        if _sha(evidence.get("hermit_sha"), f"current validation rows[{index}].cell_results.hermit_sha") != commit:
            raise BuildRefused(f"cell_results Hermit commit disagrees for {run_id!r}")
        _string(row.get("selection_mode"), f"current validation rows[{index}].selection_mode")
        path = _string(row.get("profile"), f"current validation rows[{index}].profile")
        selected = {
            _cell_identity(value, f"current validation rows[{index}].selected[{item_index}]")
            for item_index, value in enumerate(_array(evidence.get("selected"), f"current validation rows[{index}].cell_results.selected"))
        }
        if len(selected) != len(evidence["selected"]):
            raise BuildRefused(f"cell_results selected population for {run_id!r} contains duplicates")
        record_id = _string(row.get("record_id"), f"current validation rows[{index}].record_id", optional=True)
        selections[run_id] = ValidationSelection(
            commit=commit,
            path=path,
            record_id=record_id,
            selected_cells=frozenset(selected),
        )
    return selections


def _selection_authorities_from_runs(selections: Mapping[str, ValidationSelection], rows: Sequence[Mapping[str, Any]]) -> tuple[dict[str, dict[str, Any]], list[dict[str, Any]]]:
    series_cells: set[tuple[str, str, str]] = set()
    selection: dict[str, dict[str, Any]] = {}
    for index, row in enumerate(rows):
        if row.get("producer") != "validate":
            continue
        run_id = _string(row.get("run_id"), f"series rows[{index}].run_id")
        run = selections.get(run_id)
        if run is None:
            continue
        payload = _object(row.get("series"), f"series rows[{index}].series")
        tree = _sha(payload.get("tree"), f"series rows[{index}].series.tree")
        if tree != run.commit:
            raise BuildRefused(
                f"validate series run {run_id!r} records Hermit tree {tree}, "
                f"but the canonical validation row records {run.commit}"
            )
        cell = _string(payload.get("cell"), f"series rows[{index}].series.cell")
        if cell not in run.selected_cells:
            raise BuildRefused(
                f"validate series event {row.get('event_id')!r} records cell "
                f"{cell!r}, which its canonical validation row did not select"
            )
        event_id = _string(row.get("event_id"), f"series rows[{index}].event_id")
        selection[event_id] = {"path": run.path, "selected": True}
        series_cells.add((run_id, tree, cell))

    missing = []
    for run_id, run in sorted(selections.items()):
        for cell in sorted(run.selected_cells):
            if (run_id, run.commit, cell) in series_cells:
                continue
            missing.append(
                {
                    "cell": cell,
                    "observed_at": None,
                    "path": run.path,
                    "record_id": run.record_id,
                    "run_id": run_id,
                    "tree": run.commit,
                }
            )
    return dict(sorted(selection.items())), missing


def _main_ancestry(hermit: Path, hermit_main_commit: str, trees: Sequence[str]) -> dict[str, bool | None]:
    hermit_main_commit = _sha(hermit_main_commit, "Hermit main commit")
    _require_complete_commit_graph(hermit)

    decisions: dict[str, bool | None] = {}
    identities = sorted(set(trees))
    for tree in identities:
        _sha(tree, "series Hermit tree")
    if not identities:
        return decisions

    types = _git(hermit, ["cat-file", "--batch-check=%(objectname) %(objecttype)"], input=("\n".join(identities) + "\n").encode())
    try:
        type_lines = types.stdout.decode(errors="strict").splitlines()
    except UnicodeDecodeError as error:
        raise BuildRefused("git returned non-UTF-8 object types") from error
    if len(type_lines) != len(identities):
        raise BuildRefused("git cat-file returned the wrong number of object types")

    present_commits: list[str] = []
    for tree, line in zip(identities, type_lines, strict=True):
        fields = line.split()
        if fields == [tree, "missing"]:
            decisions[tree] = None
            continue
        if len(fields) != 2 or fields[0] != tree:
            raise BuildRefused(f"git returned malformed object type line {line!r}")
        object_type = fields[1]
        if object_type != "commit":
            raise BuildRefused(
                f"series Hermit tree {tree} is a {object_type or 'malformed'} "
                "object, not a commit object"
            )
        present_commits.append(tree)

    if present_commits:
        try:
            _git_text(
                hermit,
                [
                    "rev-list",
                    "--missing=error",
                    "--count",
                    hermit_main_commit,
                    *present_commits,
                    "--",
                ],
            )
        except BuildRefused as error:
            raise BuildRefused(
                "Hermit commit graph is incomplete; ancestry cannot be decided"
            ) from error

    for tree in present_commits:
        ancestor = _git(
            hermit,
            ["merge-base", "--is-ancestor", tree, hermit_main_commit],
            accepted_returncodes=frozenset({0, 1}),
        )
        decisions[tree] = ancestor.returncode == 0
    return dict(sorted(decisions.items()))


def _manifest_test(value: Any, label: str) -> dict[str, Any]:
    test = _copy_fields(value, label, _TEST_FIELDS, required=_TEST_FIELDS)
    for field in ("category", "description", "id", "lane"):
        _string(test[field], f"{label}.{field}")
    _boolean(test["occasional"], f"{label}.occasional")
    test["program"] = _string(test["program"], f"{label}.program", optional=True)
    for field in ("preprocessors", "requires"):
        test[field] = _strings(test[field], f"{label}.{field}")
    observation = _exact_object(test["observation"], f"{label}.observation", required=frozenset({"artifacts", "status", "stderr", "stdout"}))
    test["observation"] = {"artifacts": _strings(observation["artifacts"], f"{label}.observation.artifacts"), **{field: _boolean(observation[field], f"{label}.observation.{field}") for field in ("status", "stderr", "stdout")}}
    if test["build"] is not None:
        build = _exact_object(test["build"], label + ".build", required=frozenset({"cflags", "rustflags"}))
        test["build"] = {field: _strings(build[field], f"{label}.build.{field}") for field in ("cflags", "rustflags")}
    if test["direct"] is not None:
        direct = _object(test["direct"], label + ".direct")
        kind = _string(direct.get("kind"), label + ".direct.kind")
        field = {"argv": "argv", "shell": "command"}.get(kind)
        if field is None:
            raise BuildRefused(f"{label}.direct.kind is unknown: {kind!r}")
        direct = _exact_object(direct, label + ".direct", required=frozenset({"kind", field}))
        test["direct"] = {"kind": kind, field: _strings(direct[field], label + ".direct.argv") if field == "argv" else _string(direct[field], label + ".direct.command")}
    return test


def _manifest_cell(value: Any, label: str) -> dict[str, Any]:
    cell = _copy_fields(value, label, _CELL_FIELDS, required=_CELL_FIELDS)
    for field in ("test", "mode", "backend", "category", "lane"):
        _string(cell.get(field), f"{label}.{field}")
    _boolean(cell["selected_by_full"], f"{label}.selected_by_full")
    cell["timeout_seconds"] = _integer(cell["timeout_seconds"], f"{label}.timeout_seconds", minimum=1)
    cell["guest_args"] = _strings(cell["guest_args"], f"{label}.guest_args")
    for field in ("current_reproducer_unavailable_reason", "not_applicable_reason", "workdir"):
        cell[field] = _string(cell[field], f"{label}.{field}", optional=True)
    reason = cell["not_selected_by_full_reason"]
    if reason is not None:
        reason = _exact_object(reason, f"{label}.not_selected_by_full_reason", required=frozenset({"reason"}), optional=frozenset({"evidence", "result"}))
        cell["not_selected_by_full_reason"] = {
            field: _string(reason[field], f"{label}.not_selected_by_full_reason.{field}")
            for field in ("evidence", "reason", "result")
            if reason.get(field) is not None
        }
    reproducer = cell["current_reproducer"]
    if reproducer is not None:
        reproducer = _exact_object(reproducer, f"{label}.current_reproducer", required=frozenset({"argv", "shell_command"}))
        cell["current_reproducer"] = {
            "argv": _strings(reproducer["argv"], f"{label}.current_reproducer.argv"),
            "shell_command": _string(
                reproducer["shell_command"], f"{label}.current_reproducer.shell_command"
            ),
        }
    identity = "/".join(str(cell[field]) for field in ("test", "mode", "backend"))
    cell["cell"] = identity
    return cell


def _manifest_data(value: Mapping[str, Any]) -> dict[str, Any]:
    manifest = _exact_object(
        value,
        "manifest metadata",
        required=frozenset({"cells", "manifest_schema", "manifest_sha256", "schema", "selected_by_full_custom_commands", "tests"}),
    )
    if _integer(manifest.get("schema"), "manifest metadata schema", minimum=1) != 1:
        raise BuildRefused("manifest metadata schema must be 1")
    tests = [
        _manifest_test(row, f"manifest tests[{index}]")
        for index, row in enumerate(_array(manifest.get("tests"), "manifest tests"))
    ]
    tests.sort(key=lambda row: str(row["id"]))
    tests_by_id = {str(row["id"]): row for row in tests}
    if len(tests_by_id) != len(tests):
        raise BuildRefused("manifest metadata tests contain duplicate ids")
    cells: dict[str, dict[str, Any]] = {}
    by_test = {test_id: [] for test_id in tests_by_id}
    for index, raw in enumerate(_array(manifest.get("cells"), "manifest cells")):
        cell = _manifest_cell(raw, f"manifest cells[{index}]")
        identity, test_id = str(cell["cell"]), str(cell["test"])
        test = tests_by_id.get(test_id)
        if test is None or identity in cells or cell["mode"] == "custom":
            raise BuildRefused(f"invalid manifest cell identity {identity!r}")
        if (cell["category"], cell["lane"]) != (test["category"], test["lane"]):
            raise BuildRefused(f"manifest cell {identity!r} disagrees with its test")
        cells[identity] = cell
        by_test[test_id].append(identity)
    custom = []
    seen_custom: set[str] = set()
    for index, raw in enumerate(_array(manifest.get("selected_by_full_custom_commands"), "manifest custom commands")):
        cell = _manifest_cell(raw, f"manifest custom commands[{index}]")
        identity, test_id = str(cell["cell"]), str(cell["test"])
        test = tests_by_id.get(test_id)
        if test is None or identity in cells or identity in seen_custom or cell["mode"] != "custom" or cell["selected_by_full"] is not True:
            raise BuildRefused(f"invalid custom command identity {identity!r}")
        if (cell["category"], cell["lane"]) != (test["category"], test["lane"]):
            raise BuildRefused(f"custom command {identity!r} disagrees with its test")
        seen_custom.add(identity)
        custom.append(cell)
    for test in tests:
        test["cells"] = sorted(by_test[str(test["id"])], key=_cell_sort_key)
        if not test["cells"] and not any(row["test"] == test["id"] for row in custom):
            raise BuildRefused(f"manifest test {test['id']!r} has no cells")
    return {
        "cells": dict(sorted(cells.items(), key=lambda item: _cell_sort_key(item[0]))),
        "manifest_schema": _integer(manifest.get("manifest_schema"), "manifest_schema", minimum=1),
        "manifest_sha256": _sha256(manifest.get("manifest_sha256"), "manifest_sha256"),
        "selected_by_full_custom_commands": sorted(custom, key=lambda row: _cell_sort_key(str(row["cell"]))),
        "tests": tests,
    }


def _join_manifest_data(reduced_cells: Sequence[Any], manifest: Mapping[str, Any]) -> list[dict[str, Any]]:
    policies = _object(manifest.get("cells"), "manifest cells")
    output, seen = [], set()
    for index, raw in enumerate(reduced_cells):
        cell = dict(_object(raw, f"reduced cells[{index}]"))
        identity = _string(cell.get("cell"), f"reduced cells[{index}].cell")
        policy = policies.get(identity)
        if not isinstance(policy, Mapping) or identity in seen:
            raise BuildRefused(f"invalid reduced cell identity {identity!r}")
        seen.add(identity)
        cell.update({key: value for key, value in policy.items() if key not in {"cell", "test", "mode", "backend"}})
        output.append(cell)
    if seen != set(policies):
        raise BuildRefused("reduced and manifest cell populations disagree")
    return output


def _public_identity(value: Any, label: str, *, exact: bool = False) -> dict[str, str]:
    fields = ("backend", "category", "lane", "mode", "test")
    identity = _copy_fields(value, label, fields, required=fields, exact=exact)
    for field in fields:
        _string(identity[field], f"{label}.{field}")
    return identity


def _public_verdict(value: Any, label: str, *, exact: bool = False) -> dict[str, Any]:
    verdict = _copy_fields(
        value,
        label,
        ("bitwise_parity", "compared_log_messages", "comparison", "comparison_tier", "reason", "state"),
        required=("comparison_tier", "state"), exact=exact,
    )
    for field, fields in (("comparison", ("canonicalizations", "canonicalize_addresses", "compare_io_buffers", "compare_logs", "display_name", "exact_remainder", "full_trace", "ignore_lines", "log_scope", "record_envelope", "skip_commit", "skip_detlog", "strictness", "strip_lines", "stripped_prefixes", "virtualize_time")), ("compared_log_messages", ("left", "right"))):
        if field in verdict and verdict[field] is not None:
            verdict[field] = _copy_fields(verdict[field], f"{label}.{field}", fields, exact=exact)
    return verdict


def _public_cell_results(row: Mapping[str, Any], label: str, *, unavailable: bool = False) -> dict[str, Any] | None:
    raw = row.get("cell_results")
    if unavailable:
        if raw is None or _cell_results_verdict(row) is not _modules().qualifying_receipt.CellResultsVerdict.VERDICT_INVALID:
            raise BuildRefused(f"{label} cell_results gap contradicts canonical evidence")
        return None
    if raw is None:
        return None
    _require_satisfied_cell_results(row)
    evidence = _object(raw, label + ".cell_results")
    selected = [_public_identity(item, f"{label}.selected[{index}]") for index, item in enumerate(_array(evidence.get("selected"), label + ".selected"))]
    recorded = []
    for index, raw_cell in enumerate(_array(evidence.get("cells"), label + ".cells")):
        item = _public_identity(raw_cell, f"{label}.cells[{index}]")
        item["cell_verdict"] = _public_verdict(_object(raw_cell, label + ".cell").get("cell_verdict"), f"{label}.cells[{index}].cell_verdict")
        recorded.append(item)
    path = _string(row.get("profile"), label + ".profile")
    # The ledger export validated the canonical source row, including its
    # artifact path, before the shared predicate above accepts it. This public
    # projection deliberately omits the source path.
    return {
        "hermit_sha": _sha(evidence.get("hermit_sha"), label + ".hermit_sha"),
        "population_sha256": _sha256(evidence.get("population_sha256"), label + ".population_sha256"),
        "recorded": {
            "artifact": _copy_fields(evidence.get("artifact"), label + ".artifact", ("row_count", "sha256"), required=("row_count", "sha256")),
            "cell_count": len(recorded), "cells": recorded,
        },
        "run_id": _string(evidence.get("run_id"), label + ".run_id"),
        "selected_by": {path: {"cell_count": len(selected), "cells": selected}},
        "source_tree_dirty": evidence.get("source_tree_dirty"),
    }


def _legacy_field_may_be_null(row: Mapping[str, Any], field: str) -> bool:
    return row.get("schema_version") == _LEGACY_NULL_FIELDS[field][0] and all(row.get(name) is None for name in _LEGACY_NULL_FIELDS[field][1])


def _legacy_null_fields(row: Mapping[str, Any], label: str) -> set[str]:
    nulls = set()
    value = row.get("corrects")
    if isinstance(value, str):
        _string(value, label + ".corrects")
    elif value is not None:
        if not _legacy_field_may_be_null(row, "corrects"):
            raise BuildRefused(f"{label}.corrects metadata is only valid in schema 4")
        item = _exact_object(value, label + ".corrects", required=frozenset("commit finished_at host slot was_exit_code was_result".split()))
        _sha(item["commit"], label + ".corrects.commit")
        _validate_fields(item, label + ".corrects", ("finished_at", "host", "slot", "was_result"), _string)
        if _modules().qualifying_receipt._RFC3339_Z.fullmatch(item["finished_at"]) is None:
            raise BuildRefused(f"{label}.corrects.finished_at must be a canonical timestamp")
        if type(item["was_exit_code"]) is not int:
            raise BuildRefused(f"{label}.corrects.was_exit_code must be an integer")
        if any(item[field] != row.get(field) for field in ("commit", "finished_at", "host", "slot")):
            raise BuildRefused(f"{label}.corrects metadata disagrees with its validation row")
        nulls.add("corrects")
    if "tree" in row and (row["tree"] is None or row["tree"] == "unknown"):
        if row["tree"] != "unknown" or not _legacy_field_may_be_null(row, "tree"):
            raise BuildRefused(f"{label}.tree is not a valid schema-5 legacy sentinel")
        nulls.add("tree")
    return nulls


def _public_validation_row(row: Mapping[str, Any], index: int, *, cell_results_unavailable: bool = False) -> dict[str, Any]:
    label = f"validation rows[{index}]"
    public = _copy_fields(row, label, _VALIDATION_FIELDS, required=("commit", "schema_version"))
    for field in _legacy_null_fields(row, label):
        public[field] = None
    if row.get("corrects") is None:
        public.pop("corrects", None)
    public["history_row_sha256"] = _digest(row)
    if "gates" in row:
        public["gates"] = [_copy_fields(gate, f"{label}.gates[{i}]", _GATE_FIELDS) for i, gate in enumerate(_array(row.get("gates"), label + ".gates"))]
    cell_results = _public_cell_results(row, label, unavailable=cell_results_unavailable)
    if cell_results_unavailable or cell_results is not None:
        public["cell_results"] = cell_results
    return public


def _validated_cell_results(value: Any, row: Mapping[str, Any], label: str) -> ValidationSelection:
    fields = ("hermit_sha", "population_sha256", "recorded", "run_id", "selected_by", "source_tree_dirty")
    evidence = _copy_fields(value, label, fields, required=fields, exact=True)
    path = _string(row.get("profile"), label + ".profile")
    selected_by = _exact_object(evidence["selected_by"], label + ".selected_by", required=frozenset({path}))
    selected = _exact_object(selected_by[path], label + ".selected_by path", required=frozenset({"cell_count", "cells"}))
    recorded = _exact_object(evidence["recorded"], label + ".recorded", required=frozenset({"artifact", "cell_count", "cells"}))
    artifact = _exact_object(recorded["artifact"], label + ".recorded.artifact", required=frozenset({"row_count", "sha256"}))
    # The source row was canonical-validated before its artifact path was
    # omitted. This internal placeholder exists only to invoke the same shared
    # predicate; it is neither published nor evidence about the source path.
    canonical = {
        "artifact": {"path": "not-published", **dict(artifact)},
        "cells": _array(recorded["cells"], label + ".recorded.cells"),
        "hermit_sha": evidence["hermit_sha"],
        "population_sha256": evidence["population_sha256"],
        "recorded_count": recorded["cell_count"],
        "run_id": evidence["run_id"],
        "selected": _array(selected["cells"], label + ".selected_by.cells"),
        "selected_count": selected["cell_count"],
        "source_tree_dirty": evidence["source_tree_dirty"],
    }
    canonical_row = dict(row)
    canonical_row["cell_results"] = canonical
    _require_satisfied_cell_results(canonical_row)
    selected_ids = [_cell_identity(item, label + ".selected") for item in canonical["selected"]]
    return ValidationSelection(
        commit=_sha(row.get("commit"), label + ".commit"),
        path=path,
        record_id=_string(row.get("record_id"), label + ".record_id", optional=True),
        selected_cells=frozenset(selected_ids),
    )


def _validated_validation_row(value: Any, label: str) -> tuple[Mapping[str, Any], ValidationSelection | None]:
    required = ("commit", "history_row_sha256", "schema_version")
    row = _copy_fields(value, label, _VALIDATION_OUTPUT_FIELDS, required=required, exact=True)
    _sha(row["commit"], label + ".commit")
    _sha256(row["history_row_sha256"], label + ".history_row_sha256")
    _integer(row["schema_version"], label + ".schema_version", minimum=1)
    _validate_fields(row, label, ("finished_at", "host", "producer", "profile", "record_id", "repo", "result", "run_id", "selection_mode", "started_at"), _string)
    for field, validate in (("corrects", _string), ("tree", _object_id)):
        if field not in row:
            continue
        if row[field] is not None:
            validate(row[field], f"{label}.{field}")
        elif not _legacy_field_may_be_null(row, field):
            raise BuildRefused(f"{label}.{field} may not be null in this validation row")
    _validate_fields(row, label, ("tree_dirty",), _boolean)
    _validate_fields(row, label, ("checks", "executed_nodes", "executed_tests", "exit_code", "failures", "filtered_tests", "gates_expected", "gates_run", "run_number", "skipped_nodes"), _integer)
    _validate_fields(row, label, ("real_seconds", "sys_seconds", "user_seconds"), _number)
    if "gates" in row:
        for index, gate in enumerate(_array(row["gates"], label + ".gates")):
            projected = _copy_fields(gate, f"{label}.gates[{index}]", _GATE_FIELDS, required=("name", "result"), exact=True)
            gate_label = f"{label}.gates[{index}]"
            _validate_fields(projected, gate_label, ("failure_class", "failure_origin", "first_attempt_result", "name", "result"), _string)
            _validate_fields(projected, gate_label, ("oom_kills", "retries"), _integer)
            if projected.get("exit_code") is not None:
                _integer(projected["exit_code"], gate_label + ".exit_code", minimum=-255)
            _validate_fields(projected, gate_label, ("aborted", "cpu_timed_out", "oomed", "reported", "timed_out"), _boolean)
            _validate_fields(projected, gate_label, ("real_seconds",), _number)
    selection = None if row.get("cell_results") is None else _validated_cell_results(row["cell_results"], row, label + ".cell_results")
    return row, selection


def _history_incomplete(rows: Sequence[Mapping[str, Any]]) -> list[dict[str, str]]:
    missing = {
        (str(row["schema_version"]), field)
        for row in rows for field in ("record_id", "repo", "run_id")
        if row.get(field) is None
    }
    incomplete = [
        {
            "field": field,
            "input": "validation ledger",
            "reason": f"historical validation rows have no stable {field}",
            "schema": schema,
        }
        for schema, field in sorted(missing)
    ]
    for field, (schema, _null_fields, reason) in _LEGACY_NULL_FIELDS.items():
        if any(field in row and row[field] is None for row in rows):
            incomplete.append({"field": field, "input": "validation ledger", "reason": reason, "schema": str(schema)})
    return incomplete


def _cell_results_markers(rows: Sequence[Mapping[str, Any]], current: Sequence[Mapping[str, Any]], value: Any) -> list[dict[str, str]]:
    items = [item for item in _validated_incomplete(value) if item["input"] == "validation ledger" and item["field"] == "cell_results"]
    fields = _INCOMPLETE_FIELDS | frozenset({"record_id", "run_id", "schema"})
    if any(set(item) != fields for item in items):
        raise BuildRefused("cell_results incomplete_data must preserve the exact source marker shape")
    keys = [tuple(_string(item.get(field), f"cell_results incomplete_data.{field}") for field in ("record_id", "run_id", "schema")) for item in items]
    matches = [key for row in rows if (key := _validation_marker_key(row)) in keys]
    current_keys = {_validation_marker_key(row) for row in current}
    if len(keys) != len(set(keys)) or len(matches) != len(keys) or set(matches) != set(keys) or set(keys) & current_keys:
        raise BuildRefused("cell_results incomplete_data must name one unique history-only validation row")
    return sorted(items, key=_canonical_bytes)


def _validation_marker_key(row: Any) -> tuple[str, str, str] | None:
    if not isinstance(row, Mapping):
        return None
    record_id, run_id, schema = row.get("record_id"), row.get("run_id"), row.get("schema_version")
    return (record_id, run_id, str(schema)) if isinstance(record_id, str) and record_id and isinstance(run_id, str) and run_id and isinstance(schema, int) and not isinstance(schema, bool) else None


def _validation_data(canonical: Mapping[str, Any]) -> tuple[list[dict], list[dict], list[dict], list[dict[str, str]]]:
    ledger = _object(canonical.get("validation_ledger"), "validation_ledger")
    history = _bound_rows(
        ledger, "validation_ledger", "history_rows", "history_row_count", "history_rows_sha256"
    )
    current = _current_validation_rows(canonical)
    markers = _cell_results_markers(history, current, canonical.get("incomplete_data"))
    marker_keys = {(item["record_id"], item["run_id"], item["schema"]) for item in markers}
    typed_history = [_object(row, "validation history row") for row in history]
    public_by_sha = {_digest(row): _public_validation_row(row, index, cell_results_unavailable=_validation_marker_key(row) in marker_keys) for index, row in enumerate(typed_history)}
    if len(public_by_sha) != len(history):
        raise BuildRefused("validation history contains duplicate rows")
    current_public = []
    for row in current:
        public = public_by_sha.get(_digest(row))
        if public is None:
            raise BuildRefused("current validation row is absent from history")
        current_public.append(public)
    history_public = [public_by_sha[_digest(row)] for row in history]
    return history_public, current_public, _history_incomplete(history_public), markers


def _public_series_payload(value: Any, label: str, *, exact: bool = False) -> dict[str, Any]:
    payload = _copy_fields(value, label, _SERIES_PAYLOAD_FIELDS, required=("cell", "num_runs", "outcome", "run_index", "tree"), exact=exact)
    _validate_fields(payload, label, ("failure_class", "kernel_version", "machine_shortname", "result"), _string)
    _validate_fields(payload, label, ("attempt", "last_run_index"), _integer)
    _validate_fields(payload, label, ("detcore_tree",), _sha)
    _validate_fields(payload, label, ("source_tree_dirty",), _boolean)
    if payload.get("depth") is not None:
        depth = _object(payload["depth"], label + ".depth")
        payload["depth"] = {name: _copy_fields(item, f"{label}.depth[{_string(name, label + '.depth key')!r}]", ("commits", "first_parent"), required=("commits", "first_parent"), exact=exact) for name, item in depth.items()}
        for name, counts in payload["depth"].items():
            _integer(counts["commits"], f"{label}.depth[{name!r}].commits", minimum=1)
            _integer(counts["first_parent"], f"{label}.depth[{name!r}].first_parent", minimum=1)
    if payload.get("host_capabilities") is not None:
        capabilities = _object(payload["host_capabilities"], label + ".host_capabilities")
        payload["host_capabilities"] = {name: _copy_fields(item, f"{label}.host_capabilities[{_string(name, label + '.host_capabilities key')!r}]", ("evidence", "present"), required=("evidence", "present"), exact=exact) for name, item in capabilities.items()}
        for name, capability in payload["host_capabilities"].items():
            _string(capability["evidence"], f"{label}.host_capabilities[{name!r}].evidence")
            _boolean(capability["present"], f"{label}.host_capabilities[{name!r}].present")
    if payload.get("runtime") is not None:
        runtime = _copy_fields(payload["runtime"], label + ".runtime", ("run1", "run2", "wall_time_max_ms", "wall_time_min_ms"), exact=exact)
        _validate_fields(runtime, label + ".runtime", ("wall_time_max_ms", "wall_time_min_ms"), _number)
        if runtime.get("wall_time_min_ms") is not None and runtime.get("wall_time_max_ms") is not None and runtime["wall_time_min_ms"] > runtime["wall_time_max_ms"]:
            raise BuildRefused(f"{label}.runtime minimum exceeds maximum")
        for side in ("run1", "run2"):
            if side not in runtime:
                continue
            runtime[side] = None if runtime[side] is None else _copy_fields(runtime[side], label + ".runtime." + side, ("scheduler_turns", "syscalls", "virtual_nanoseconds"), exact=exact)
            if runtime[side] is not None:
                for field, count in runtime[side].items():
                    _integer(count, f"{label}.runtime.{side}.{field}")
        payload["runtime"] = runtime
    if payload.get("coordinates") is not None:
        payload["coordinates"] = _copy_fields(payload["coordinates"], label + ".coordinates", ("first_divergent_record", "first_divergent_scheduler_turn", "first_divergent_syscall", "first_divergent_virtual_nanoseconds"), exact=exact)
        _validate_fields(payload["coordinates"], label + ".coordinates", tuple(payload["coordinates"]), _integer)
    if payload.get("no_verdict_evidence") is not None:
        evidence = _copy_fields(payload["no_verdict_evidence"], label + ".no_verdict_evidence", ("attempts", "evidence_sha256"), required=("attempts", "evidence_sha256"), exact=exact)
        attempt_fields = ("attempt_outcome", "disposition", "error_kind", "index", "kind", "signal", "status", "timed_out", "verification_report_sha256")
        attempts = _array(evidence["attempts"], label + ".attempts")
        if not attempts:
            raise BuildRefused(f"{label}.no_verdict_evidence.attempts must be nonempty")
        evidence["attempts"] = [_copy_fields(item, f"{label}.attempts[{i}]", attempt_fields, required=attempt_fields, exact=exact) for i, item in enumerate(attempts)]
        _sha256(evidence["evidence_sha256"], label + ".no_verdict_evidence.evidence_sha256")
        if payload.get("outcome") not in {"no_result", "timeout", "errored"}:
            raise BuildRefused(f"{label}.no_verdict_evidence contradicts outcome")
        for index, attempt in enumerate(evidence["attempts"]):
            attempt_label = f"{label}.attempts[{index}]"
            _validate_fields(attempt, attempt_label, ("attempt_outcome", "disposition", "error_kind", "index", "kind"), _string)
            if attempt.get("status") is not None:
                _integer(attempt["status"], attempt_label + ".status")
            if attempt.get("signal") is not None:
                _integer(attempt["signal"], attempt_label + ".signal", minimum=1)
            if attempt.get("status") is not None and attempt.get("signal") is not None:
                raise BuildRefused(f"{attempt_label} records both status and signal")
            _validate_fields(attempt, attempt_label, ("timed_out",), _boolean)
            _validate_fields(attempt, attempt_label, ("verification_report_sha256",), _sha256)
        payload["no_verdict_evidence"] = evidence
    return payload


def _series_data(rows: Sequence[Mapping[str, Any]], projection: Mapping[str, str | None], ancestry: Mapping[str, bool | None], products: Mapping[str, str | None]) -> list[dict[str, Any]]:
    output = []
    for index, row in enumerate(rows):
        label = f"series rows[{index}]"
        public = _copy_fields(row, label, _SERIES_FIELDS, required=_SERIES_FIELDS)
        event_id = _string(public.get("event_id"), label + ".event_id")
        public["series"] = _public_series_payload(row.get("series"), label + ".series")
        tree = _sha(public["series"].get("tree"), label + ".series.tree")
        public.update(current_main_ancestry=ancestry[tree], product_result=products.get(event_id), product_result_available=event_id in products, projection_validation_reason=projection[event_id])
        output.append(public)
    return output


def _canonical_provenance(value: Mapping[str, Any], expected_parent_commit: str) -> dict[str, Any]:
    fields = frozenset("parent_commit published_ref reader_ci_hub_tree reader_commit reader_files reader_files_sha256 reader_hermit source_count sources sources_sha256".split())
    value = _exact_object(value, "canonical input provenance", required=fields)
    commits = {_sha(value.get(field), f"provenance.{field}") for field in ("parent_commit", "published_ref", "reader_commit")}
    if commits != {expected_parent_commit}:
        raise BuildRefused("canonical input provenance commits must equal the requested parent commit")
    _object_id(value.get("reader_ci_hub_tree"), "provenance.reader_ci_hub_tree")
    label = "provenance.reader_hermit"
    reader = _exact_object(
        value.get("reader_hermit"), label, required=frozenset({"checkout_commit", "gitlink_commit"})
    )
    if _sha(reader.get("checkout_commit"), label + ".checkout_commit") != _sha(reader.get("gitlink_commit"), label + ".gitlink_commit"):
        raise BuildRefused(f"{label} checkout_commit and gitlink_commit disagree")
    reader_files = _array(value.get("reader_files"), "provenance.reader_files")
    paths: set[str] = set()
    for index, raw in enumerate(reader_files):
        label = f"provenance.reader_files[{index}]"
        item = _exact_object(raw, label, required=frozenset({"bytes", "path", "sha256"}))
        _integer(item.get("bytes"), label + ".bytes")
        path = _relative_path(item.get("path"), label + ".path")
        if path in paths:
            raise BuildRefused(f"provenance.reader_files repeats path {path}")
        paths.add(path)
        _sha256(item.get("sha256"), label + ".sha256")
    if _sha256(value.get("reader_files_sha256"), "provenance.reader_files_sha256") != _digest(reader_files):
        raise BuildRefused("provenance.reader_files_sha256 disagrees with reader_files")
    sources = _array(value.get("sources"), "provenance.sources")
    identities: set[tuple[str, str, str]] = set()
    for index, raw in enumerate(sources):
        label = f"provenance.sources[{index}]"
        item = _exact_object(raw, label, required=frozenset({"bytes", "line_count", "location", "path", "sha256", "store"}), optional=frozenset({"git_object"}))
        for field in ("bytes", "line_count"):
            _integer(item.get(field), f"{label}.{field}")
        location, path, store = _string(item.get("location"), label + ".location"), _relative_path(item.get("path"), label + ".path"), _string(item.get("store"), label + ".store")
        _sha256(item.get("sha256"), label + ".sha256")
        identity = (store, location, path)
        if identity in identities:
            raise BuildRefused("provenance.sources repeats identity " + "/".join(identity))
        identities.add(identity)
        if "git_object" in item:
            _object_id(item.get("git_object"), label + ".git_object")
    _bound_rows(value, "provenance", "sources", "source_count", "sources_sha256")
    if any(_OWNER_PATH_NEEDLE in text for _key, text in _walk_strings(value)):
        raise BuildRefused("canonical input provenance contains an absolute owner path")
    return json.loads(_canonical_bytes(value))


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


def _build_provenance(canonical_document: CapturedJson, canonical_provenance: Mapping[str, Any],
                      manifest_document: CapturedJson, commits: Mapping[str, str],
                      manifest: Mapping[str, Any], bound: Mapping[str, Any]) -> dict[str, Any]:
    canonical = canonical_document.value
    series = _object(canonical.get("series"), "canonical input series")
    validation = _object(canonical.get("validation_ledger"), "canonical input validation_ledger")
    projection_binding = _object(series.get("projection_results_binding"), "series.projection_results_binding")
    return {
        "build_data_schema_version": SCHEMA_VERSION,
        "canonical_input_bytes_sha256": hashlib.sha256(canonical_document.encoded).hexdigest(),
        "canonical_input_provenance": dict(canonical_provenance),
        "current_main_ancestry_sha256": _digest(bound["ancestry"]),
        "current_validation_run_rows_projection_sha256": _digest(bound["current"]),
        "current_validation_run_rows_sha256": _sha256(validation.get("current_validation_run_rows_sha256"), "validation_ledger.current_validation_run_rows_sha256"),
        "current_validation_run_row_count": _integer(validation.get("current_validation_run_row_count"), "validation_ledger.current_validation_run_row_count"),
        "hermit_commit": commits["hermit"],
        "hermit_main_commit": commits["hermit_main"],
        "manifest_metadata_bytes_sha256": hashlib.sha256(manifest_document.encoded).hexdigest(),
        "manifest_metadata_projection_sha256": _digest(manifest),
        "manifest_metadata_schema": 1,
        "manifest_schema": manifest["manifest_schema"],
        "manifest_sha256": manifest["manifest_sha256"],
        "missing_executions_sha256": _digest(bound["missing"]),
        "parent_commit": commits["parent"],
        "product_results_sha256": _digest(bound["products"]),
        "projection_checker_input_jsonl_sha256": _sha256(projection_binding.get("checker_input_jsonl_sha256"), "series.projection_results_binding.checker_input_jsonl_sha256"),
        "projection_results_sha256": _sha256(series.get("projection_results_sha256"), "series.projection_results_sha256"),
        "reducer_schema_version": _modules().reducer.SCHEMA_VERSION,
        "series_physical_row_count": _integer(series.get("row_count"), "series.row_count"),
        "series_represented_run_count": _integer(series.get("represented_run_count"), "series.represented_run_count"),
        "series_rows_projection_sha256": _digest(bound["series"]),
        "series_rows_sha256": _sha256(series.get("rows_sha256"), "series.rows_sha256"),
        "series_schema_counts": json.loads(_canonical_bytes(series.get("schema_counts"))),
        "validate_run_selection_sha256": _digest(bound["selection"]),
        "validation_cell_results_incomplete_data_sha256": _digest(bound["cell_results_incomplete"]),
        "validation_event_count": _integer(validation.get("event_count"), "validation_ledger.event_count"),
        "validation_events_sha256": _sha256(validation.get("events_sha256"), "validation_ledger.events_sha256"),
        "validation_history_row_count": _integer(validation.get("history_row_count"), "validation_ledger.history_row_count"),
        "validation_history_rows_projection_sha256": _digest(bound["history"]),
        "validation_history_rows_sha256": _sha256(validation.get("history_rows_sha256"), "validation_ledger.history_rows_sha256"),
    }


def _validated_reduction(value: Any) -> dict[str, Any]:
    reduced = _exact_object(value, "compatibility reduction", required=frozenset({"cells", "counts", "incomplete_data", "schema_version"}))
    if reduced.get("schema_version") != _modules().reducer.SCHEMA_VERSION:
        raise BuildRefused("compatibility reduction schema_version is unsupported")
    return {
        "cells": _array(reduced.get("cells"), "compatibility reduction cells"),
        "counts": dict(_object(reduced.get("counts"), "compatibility reduction counts")),
        "incomplete_data": _array(reduced.get("incomplete_data"), "compatibility reduction incomplete_data"),
    }


def _validated_output_manifest(data: Mapping[str, Any], provenance: Mapping[str, Any]) -> dict[str, Any]:
    cells = _array(data.get("cells"), "cells")
    tests = _array(data.get("tests"), "tests")
    custom = _array(data.get("selected_by_full_custom_commands"), "custom commands")
    for index, test in enumerate(tests):
        _exact_object(test, f"tests[{index}]", required=frozenset((*_TEST_FIELDS, "cells")))
    for index, cell in enumerate(custom):
        _exact_object(cell, f"custom commands[{index}]", required=frozenset((*_CELL_FIELDS, "cell")))
    projected = _manifest_data({"cells": cells, "manifest_schema": provenance["manifest_schema"], "manifest_sha256": provenance["manifest_sha256"], "schema": 1, "selected_by_full_custom_commands": custom, "tests": tests})
    identities = [_string(row.get("cell"), "cell identity") for row in cells]
    if identities != sorted(projected["cells"], key=_cell_sort_key) or projected["tests"] != tests or projected["selected_by_full_custom_commands"] != custom:
        raise BuildRefused("published manifest policy disagrees with its exact projection")
    if provenance["manifest_metadata_projection_sha256"] != _digest(projected):
        raise BuildRefused("manifest metadata projection digest disagrees with published policy")
    return projected


def _validated_history(runs: Mapping[str, Any], incomplete: Any) -> tuple[list[Any], list[Any], set[tuple[str, str]], dict[str, ValidationSelection], list[dict[str, str]]]:
    history = _array(runs.get("validation_history_rows"), "validation history")
    current = _array(runs.get("current_validation_run_rows"), "current validation rows")
    markers = _cell_results_markers(history, current, incomplete)
    marker_keys = {(item["record_id"], item["run_id"], item["schema"]) for item in markers}
    by_id, selections, history_runs = {}, {}, set()
    for index, raw in enumerate(history):
        row, selection = _validated_validation_row(raw, f"validation history[{index}]")
        identity = _sha256(row["history_row_sha256"], "history row sha256")
        if identity in by_id:
            raise BuildRefused("validation history repeats a row identity")
        unavailable = "cell_results" in row and row["cell_results"] is None
        if unavailable != (_validation_marker_key(row) in marker_keys):
            raise BuildRefused("historical cell_results and incomplete_data disagree")
        by_id[identity], selections[identity] = row, selection
        if row.get("repo") == "hermit" and row.get("run_id") is not None:
            history_runs.add((str(row["run_id"]), str(row["commit"])))
    current_ids, by_run = [], {}
    for index, raw in enumerate(current):
        row = _object(raw, f"current validation rows[{index}]")
        identity = _sha256(row.get("history_row_sha256"), "current history row sha256")
        if by_id.get(identity) != row:
            raise BuildRefused("current validation rows disagree with validation history")
        current_ids.append(identity)
        selection = selections[identity]
        if selection is not None and row.get("repo") == "hermit":
            run_id = _string(row.get("run_id"), "current validation run_id")
            if run_id in by_run:
                raise BuildRefused("current validation rows repeat a run_id")
            by_run[run_id] = selection
    if len(current_ids) != len(set(current_ids)):
        raise BuildRefused("current validation rows repeat a history row")
    return history, current, history_runs, by_run, markers


def _validated_series(runs: Mapping[str, Any], history_runs: set[tuple[str, str]], ancestry: Mapping[str, bool | None]) -> tuple[dict[str, Mapping[str, Any]], list[dict[str, Any]], int]:
    events, canonical_rows = {}, []
    represented = admitted = admitted_runs = 0
    for index, raw in enumerate(_array(runs.get("series_rows"), "series rows")):
        label = f"series rows[{index}]"
        row = _exact_object(raw, label, required=_SERIES_OUTPUT_FIELDS)
        payload = _public_series_payload(row["series"], label + ".series", exact=True)
        canonical = {field: _string(row[field], f"{label}.{field}") for field in _SERIES_FIELDS}
        canonical["series"] = payload
        event_id, tree = canonical["event_id"], _sha(payload.get("tree"), label + ".series.tree")
        if row["schema"] not in {"stress-series/v1", "stress-series/v2", "stress-series/v3"} or row["event_type"] != "series.observation" or row["team"] != "hermit":
            raise BuildRefused(f"series event {event_id!r} has an unsupported envelope")
        if row["schema"] != "stress-series/v1" and (payload.get("machine_shortname") != row["host"] or not isinstance(payload.get("host_capabilities"), Mapping) or set(payload["host_capabilities"]) != {"cpuid-faulting", "kvm"} or payload.get("kernel_version") is None):
            raise BuildRefused(f"series event {event_id!r} has incomplete host facts")
        _string(payload.get("cell"), label + ".series.cell")
        _string(payload.get("outcome"), label + ".series.outcome")
        _integer(payload.get("run_index"), label + ".series.run_index")
        count = _integer(payload.get("num_runs"), label + ".series.num_runs", minimum=1)
        if event_id in events:
            raise BuildRefused("series rows repeat an event_id")
        decision = row["current_main_ancestry"]
        if decision is not None and not isinstance(decision, bool):
            raise BuildRefused(f"{label}.current_main_ancestry must be bool or null")
        if tree not in ancestry or decision != ancestry[tree]:
            raise BuildRefused(f"series event {event_id!r} disagrees with current-main ancestry")
        available, product = row["product_result_available"], row["product_result"]
        if not isinstance(available, bool) or product not in {None, "pass", "product_failure"}:
            raise BuildRefused("series product result fields are invalid")
        if available != (row["schema"] == "stress-series/v3") or (not available and product is not None):
            raise BuildRefused("series product result availability disagrees with schema")
        reason = row["projection_validation_reason"]
        if reason is None:
            admitted, admitted_runs = admitted + 1, admitted_runs + count
        else:
            _string(reason, label + ".projection_validation_reason")
        if row["producer"] == "validate" and (row["run_id"], tree) not in history_runs and reason is None:
            raise BuildRefused(f"validate series event {event_id!r} has no validation history row")
        events[event_id], represented = row, represented + count
        canonical_rows.append(canonical)
    expected = dict(series_physical_row_count=len(canonical_rows), series_represented_run_count=represented,
                    projection_admitted_physical_row_count=admitted, projection_admitted_represented_run_count=admitted_runs,
                    projection_excluded_physical_row_count=len(canonical_rows) - admitted,
                    projection_excluded_represented_run_count=represented - admitted_runs)
    if any(_integer(runs.get(field), f"runs.{field}") != count for field, count in expected.items()):
        raise BuildRefused("runs counts disagree with their series rows")
    return events, canonical_rows, represented


def _validated_output_runs(data: Mapping[str, Any]) -> dict[str, Any]:
    runs = _exact_object(data.get("runs"), "runs", required=_RUN_FIELDS)
    if _integer(runs.get("schema_version"), "runs.schema_version", minimum=1) != SCHEMA_VERSION:
        raise BuildRefused("runs schema_version is unsupported")
    ancestry = {}
    for raw_tree, decision in _object(runs["current_main_ancestry"], "runs.current_main_ancestry").items():
        tree = _sha(raw_tree, "current-main ancestry tree")
        if decision is not None and not isinstance(decision, bool):
            raise BuildRefused("current-main ancestry values must be bool or null")
        ancestry[tree] = decision
    ancestry = dict(sorted(ancestry.items()))
    history, current, history_runs, current_selection, markers = _validated_history(runs, data.get("incomplete_data"))
    events, canonical_rows, represented = _validated_series(runs, history_runs, ancestry)
    if _integer(runs["validation_history_row_count"], "runs.validation_history_row_count") != len(history) or _integer(runs["current_validation_run_row_count"], "runs.current_validation_run_row_count") != len(current):
        raise BuildRefused("runs validation counts disagree with their rows")
    return {"ancestry": ancestry, "canonical_rows": canonical_rows, "cell_results_incomplete": markers, "current": current, "current_selection": current_selection, "events": events, "history": history, "represented": represented, "runs": runs}


def _validated_incomplete(value: Any) -> list[dict[str, str]]:
    def validate(raw: Any, index: int) -> dict[str, str]:
        label = f"incomplete_data[{index}]"
        item = _exact_object(raw, label, required=_INCOMPLETE_FIELDS, optional=_INCOMPLETE_OPTIONAL_FIELDS)
        return {field: _string(item[field], f"{label}.{field}") for field in item}

    return [validate(raw, index) for index, raw in enumerate(_array(value, "incomplete_data"))]


def _derived_reducer_inputs(manifest: Mapping[str, Any], output: Mapping[str, Any]) -> dict[str, Any]:
    events = output["events"]
    selected_runs = output["current_selection"]
    allowed = set(manifest["cells"]) | {str(item["cell"]) for item in manifest["selected_by_full_custom_commands"]}
    for run_id, run in selected_runs.items():
        unknown = sorted(run.selected_cells.difference(allowed))
        if unknown:
            raise BuildRefused(
                f"current validation run {run_id!r} selects unknown cell(s): "
                + ", ".join(unknown)
            )
    selection, missing = _selection_authorities_from_runs(selected_runs, list(events.values()))
    expected_trees = {str(_object(row["series"], "series payload")["tree"]) for row in events.values()} | {str(item["tree"]) for item in missing}
    if set(output["ancestry"]) != expected_trees:
        raise BuildRefused("current-main ancestry does not cover exactly the published runs")
    products = {event_id: row["product_result"] for event_id, row in events.items() if row["product_result_available"]}
    if products != _product_results(output["canonical_rows"]):
        raise BuildRefused("published product results disagree with their series rows")
    return {
        "ancestry": output["ancestry"],
        "missing": missing,
        "products": products,
        "projection": {event_id: row["projection_validation_reason"] for event_id, row in events.items()},
        "selection": dict(sorted(selection.items())),
    }


def _reduced_from_output(data: Mapping[str, Any], manifest: Mapping[str, Any], output: Mapping[str, Any]) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, str]]]:
    rows = output["canonical_rows"]
    published_incomplete = _validated_incomplete(data["incomplete_data"])
    allowed_source = _modules().export_inputs._incomplete_data()
    allowed_source.extend(item for item in published_incomplete if item["input"] == "validation ledger" and item["field"] == "cell_results")
    allowed_source_bytes = {_canonical_bytes(item) for item in allowed_source}
    source_incomplete = [item for item in published_incomplete if _canonical_bytes(item) in allowed_source_bytes]
    canonical = {
        "schema_version": 1,
        "series": {"row_count": len(rows), "represented_run_count": output["represented"], "rows": rows, "rows_sha256": _digest(rows)},
        "incomplete_data": source_incomplete,
    }
    bound = _derived_reducer_inputs(manifest, output)
    try:
        reduced = _modules().reducer.reduce_inputs(
            canonical,
            {"schema": 1, "cells": list(manifest["cells"].values())},
            projection_results=bound["projection"],
            current_main_ancestry=bound["ancestry"],
            product_results=bound["products"],
            producer_observed_at={},
            validate_run_selection=bound["selection"],
            missing_executions=bound["missing"],
        )
    except _modules().reducer.ReductionRefused as error:
        raise BuildRefused(f"published compatibility reduction refused: {error}") from error
    reduced = _validated_reduction(reduced)
    if _canonical_bytes(_join_manifest_data(reduced["cells"], manifest)) != _canonical_bytes(data["cells"]):
        raise BuildRefused("published cells disagree with the authoritative reducer")
    incomplete = [*reduced["incomplete_data"], *_history_incomplete(output["history"]), *_STAGE_INCOMPLETE_DATA]
    incomplete.sort(key=_canonical_bytes)
    if incomplete != published_incomplete:
        raise BuildRefused("published incomplete data disagrees with its authorities")
    return reduced, bound, incomplete


def _validate_output_provenance(provenance: Mapping[str, Any], output: Mapping[str, Any], bound: Mapping[str, Any]) -> None:
    for field in _PROVENANCE_COMMITS:
        _sha(provenance.get(field), f"provenance.{field}")
    for field in _PROVENANCE_DIGESTS:
        _sha256(provenance.get(field), f"provenance.{field}")
    for field in _PROVENANCE_COUNTS:
        _integer(provenance.get(field), f"provenance.{field}")
    if _integer(provenance.get("build_data_schema_version"), "provenance.build_data_schema_version", minimum=1) != SCHEMA_VERSION or provenance.get("manifest_metadata_schema") != 1 or provenance.get("reducer_schema_version") != _modules().reducer.SCHEMA_VERSION:
        raise BuildRefused("provenance schema versions are unsupported")
    schema_counts = _object(provenance.get("series_schema_counts"), "provenance.series_schema_counts")
    if any(not isinstance(schema, str) or not schema or _integer(count, f"series_schema_counts[{schema!r}]") < 0 for schema, count in schema_counts.items()):
        raise BuildRefused("provenance series_schema_counts is malformed")
    runs, events = output["runs"], output["events"]
    observed_schema_counts = dict(sorted(Counter(str(row["schema"]) for row in events.values()).items()))
    expected_counts = {
        "current_validation_run_row_count": runs["current_validation_run_row_count"],
        "series_physical_row_count": runs["series_physical_row_count"],
        "series_represented_run_count": runs["series_represented_run_count"],
        "validation_history_row_count": runs["validation_history_row_count"],
    }
    if dict(schema_counts) != observed_schema_counts or any(provenance[field] != count for field, count in expected_counts.items()):
        raise BuildRefused("provenance counts disagree with published runs")
    expected_digests = {
        "current_main_ancestry_sha256": _digest(bound["ancestry"]),
        "current_validation_run_rows_projection_sha256": _digest(output["current"]),
        "missing_executions_sha256": _digest(bound["missing"]),
        "product_results_sha256": _digest(bound["products"]),
        "projection_results_sha256": _digest(bound["projection"]),
        "series_rows_projection_sha256": _digest(output["runs"]["series_rows"]),
        "validate_run_selection_sha256": _digest(bound["selection"]),
        "validation_cell_results_incomplete_data_sha256": _digest(output["cell_results_incomplete"]),
        "validation_history_rows_projection_sha256": _digest(output["history"]),
    }
    if any(provenance[field] != digest for field, digest in expected_digests.items()):
        raise BuildRefused("provenance digests disagree with published runs")


def _validate_build_data(value: Any) -> dict[str, Any]:
    data = _exact_object(value, "build data", required=frozenset({"cells", "counts", "incomplete_data", "provenance", "runs", "schema_version", "selected_by_full_custom_commands", "tests"}))
    if _integer(data.get("schema_version"), "build data schema_version", minimum=1) != SCHEMA_VERSION:
        raise BuildRefused("build data schema_version is unsupported")
    provenance = _exact_object(data.get("provenance"), "provenance", required=_PROVENANCE_FIELDS)
    parent_commit = _sha(provenance.get("parent_commit"), "provenance.parent_commit")
    if _canonical_provenance(_object(provenance.get("canonical_input_provenance"), "provenance.canonical_input_provenance"), parent_commit) != provenance["canonical_input_provenance"]:
        raise BuildRefused("canonical input provenance is not canonical")
    manifest = _validated_output_manifest(data, provenance)
    output = _validated_output_runs(data)
    reduced, bound, incomplete = _reduced_from_output(data, manifest, output)
    _validate_output_provenance(provenance, output, bound)
    expected_counts = dict(reduced["counts"])
    expected_counts.update(
        current_validation_run_row_count=len(output["current"]),
        incomplete_data_count=len(incomplete),
        selected_by_full_custom_command_count=len(manifest["selected_by_full_custom_commands"]),
        test_count=len(manifest["tests"]),
        validation_history_row_count=len(output["history"]),
    )
    counts = _object(data.get("counts"), "counts")
    if set(counts) != set(expected_counts) or any(_integer(counts.get(field), f"counts.{field}") != expected for field, expected in expected_counts.items()):
        raise BuildRefused("build data counts disagree with their exact populations")
    _require_publishable_output(data)
    return json.loads(_canonical_bytes(data))


def validate_build_data(value: Any) -> dict[str, Any]:
    try:
        return _validate_build_data(value)
    except (KeyError, RecursionError, TypeError) as error:
        raise BuildRefused(f"build data is malformed: {error}") from error


def _require_publishable_output(value: Mapping[str, Any]) -> None:
    forbidden = {"enabled", "measured", "measurement_state", "never_measured", "selected", "selected_count"}
    for is_key, text in _walk_strings(value):
        if is_key and text in forbidden:
            raise BuildRefused(f"compatibility reduction contains forbidden stored field {text!r}")
        if _OWNER_PATH_RE.search(text):
            raise BuildRefused("compatibility reduction contains an absolute owner path")
        if (is_key and _CREDENTIAL_KEY_RE.search(text)) or _CREDENTIAL_RE.search(text):
            raise BuildRefused("compatibility reduction contains a credential-bearing string")
        if re.search(r"(?<![A-Za-z0-9])enabled(?![A-Za-z0-9])", text, re.IGNORECASE):
            raise BuildRefused("compatibility reduction contains dropped vocabulary")


def _bound_reduction(canonical: Mapping[str, Any], manifest: Mapping[str, Any], hermit: Path, main: str) -> dict[str, Any]:
    rows, projection = _validate_projection(canonical)
    products = _product_results(rows)
    selection, missing = _selection_authorities_from_runs(
        _validation_selection_runs(canonical), rows
    )
    trees = [str(_object(row.get("series"), "series row")["tree"]) for row in rows]
    trees.extend(str(item["tree"]) for item in missing)
    ancestry = _main_ancestry(hermit, main, trees)
    reducer = _modules().reducer
    try:
        reduced = reducer.reduce_inputs(
            canonical, manifest, projection_results=projection,
            current_main_ancestry=ancestry, product_results=products,
            producer_observed_at={}, validate_run_selection=selection,
            missing_executions=missing,
        )
    except reducer.ReductionRefused as error:
        raise BuildRefused(f"compatibility reduction refused: {error}") from error
    history, current, history_incomplete, cell_results_incomplete = _validation_data(canonical)
    return {
        "ancestry": ancestry, "current": current, "history": history,
        "cell_results_incomplete": cell_results_incomplete,
        "history_incomplete": history_incomplete, "missing": missing,
        "products": products, "projection": projection,
        "reduced": _validated_reduction(reduced), "rows": rows,
        "selection": selection, "series": _series_data(rows, projection, ancestry, products),
    }


def _artifact_result(bound: Mapping[str, Any], manifest: Mapping[str, Any], provenance: Mapping[str, Any]) -> dict[str, Any]:
    reduced = _object(bound["reduced"], "reduced data")
    incomplete = [*reduced["incomplete_data"], *bound["history_incomplete"]]
    incomplete.extend(_STAGE_INCOMPLETE_DATA)
    incomplete.sort(key=_canonical_bytes)
    series = bound["series"]
    source_series = _object(bound["canonical_series"], "canonical series")
    runs = {
        "current_main_ancestry": bound["ancestry"],
        "current_validation_run_row_count": len(bound["current"]),
        "current_validation_run_rows": bound["current"],
        "projection_admitted_physical_row_count": source_series["projection_admitted_physical_row_count"],
        "projection_admitted_represented_run_count": source_series["projection_admitted_represented_run_count"],
        "projection_excluded_physical_row_count": source_series["projection_excluded_physical_row_count"],
        "projection_excluded_represented_run_count": source_series["projection_excluded_represented_run_count"],
        "schema_version": SCHEMA_VERSION,
        "series_physical_row_count": len(series),
        "series_represented_run_count": sum(int(row["series"]["num_runs"]) for row in series),
        "series_rows": series,
        "validation_history_row_count": len(bound["history"]),
        "validation_history_rows": bound["history"],
    }
    cells = _join_manifest_data(reduced["cells"], manifest)
    counts = dict(reduced["counts"])
    counts.update(current_validation_run_row_count=len(bound["current"]), incomplete_data_count=len(incomplete), selected_by_full_custom_command_count=len(manifest["selected_by_full_custom_commands"]), test_count=len(manifest["tests"]), validation_history_row_count=len(bound["history"]))
    return validate_build_data({"cells": cells, "counts": counts, "incomplete_data": incomplete, "provenance": provenance, "runs": runs, "schema_version": SCHEMA_VERSION, "selected_by_full_custom_commands": manifest["selected_by_full_custom_commands"], "tests": manifest["tests"]})


def build_data(parent_value: Path, parent_commit_value: str, hermit_value: Path, hermit_commit_value: str, hermit_main_commit_value: str) -> dict[str, Any]:
    """Build one deterministic reduction from exact canonical authorities."""
    _refuse_authority_overrides()
    _modules()
    parent_commit = _sha(parent_commit_value, "parent commit")
    hermit_commit = _sha(hermit_commit_value, "Hermit commit")
    main = _sha(hermit_main_commit_value, "Hermit main commit")
    parent = _require_checkout(parent_value, parent_commit, label="dev-hermit", clean=True)
    hermit = _require_checkout(hermit_value, hermit_commit, label="Hermit", clean=True)
    _require_main_target(hermit, main)
    canonical_document = _capture_canonical_input(parent, parent_commit)
    manifest_document = _run_manifest_metadata(hermit)
    _require_checkout(parent, parent_commit, label="dev-hermit", clean=True)
    _require_checkout(hermit, hermit_commit, label="Hermit", clean=True)
    _require_main_target(hermit, main)
    canonical = _object(canonical_document.value, "canonical input")
    canonical_provenance = _canonical_provenance(_object(canonical.get("provenance"), "canonical input provenance"), parent_commit)
    manifest = _manifest_data(_object(manifest_document.value, "manifest metadata"))
    bound = _bound_reduction(canonical, manifest_document.value, hermit, main)
    bound["canonical_series"] = _object(canonical.get("series"), "canonical series")
    provenance = _build_provenance(
        canonical_document,
        canonical_provenance,
        manifest_document,
        {"hermit": hermit_commit, "hermit_main": main, "parent": parent_commit},
        manifest,
        bound,
    )
    result = _artifact_result(bound, manifest, provenance)
    _require_checkout(parent, parent_commit, label="dev-hermit", clean=True)
    _require_checkout(hermit, hermit_commit, label="Hermit", clean=True)
    _require_main_target(hermit, main)
    return result


def render_build_data(value: Mapping[str, Any]) -> bytes:
    return _canonical_bytes(value) + b"\n"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parent", required=True, type=Path, help="exact dev-hermit checkout whose canonical ledger and series are read")
    parser.add_argument("--parent-commit", required=True, help="full dev-hermit commit currently checked out by --parent")
    parser.add_argument("--hermit", required=True, type=Path, help="exact clean Hermit checkout used for manifest policy and ancestry")
    parser.add_argument("--hermit-commit", required=True, help="full Hermit commit currently checked out by --hermit")
    parser.add_argument(
        "--hermit-main-commit",
        required=True,
        help="full Hermit ancestry target; must equal the checkout's stable refs/remotes/origin/main",
    )
    arguments = parser.parse_args(argv)
    try:
        value = build_data(arguments.parent, arguments.parent_commit, arguments.hermit, arguments.hermit_commit, arguments.hermit_main_commit)
        encoded = render_build_data(value)
    except (BuildRefused, OSError, ValueError) as error:
        print(f"compatibility-website build data: REFUSED: {error}", file=sys.stderr)
        return 2
    sys.stdout.buffer.write(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
