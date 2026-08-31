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
        ).encode()
    except (TypeError, ValueError, RecursionError) as error:
        raise BuildRefused(f"value is not canonical JSON: {error}") from error


def _digest(value: Any) -> str:
    return hashlib.sha256(_canonical_bytes(value)).hexdigest()


def _bytes_digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _process_detail(stderr: bytes, stdout: bytes) -> str:
    detail = (stderr or stdout).decode(errors="replace").strip()
    if not detail:
        return "no output"
    limit = 2_000
    return detail if len(detail) <= limit else detail[:limit] + "... [truncated]"


def _object(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise BuildRefused(f"{label} must be an object")
    return value


def _array(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise BuildRefused(f"{label} must be an array")
    return value


def _string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise BuildRefused(f"{label} must be a nonempty string")
    return value


def _integer(value: Any, label: str, *, minimum: int = 0) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise BuildRefused(f"{label} must be an integer >= {minimum}")
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


def _exact_object(
    value: Any,
    label: str,
    *,
    required: frozenset[str],
    optional: frozenset[str] = frozenset(),
) -> Mapping[str, Any]:
    item = _object(value, label)
    missing = sorted(required.difference(item))
    unknown = sorted(set(item).difference(required | optional))
    if missing:
        raise BuildRefused(f"{label} is missing required field(s): {', '.join(missing)}")
    if unknown:
        raise BuildRefused(f"{label} has unknown field(s): {', '.join(unknown)}")
    return item


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
    cwd: Path | None = None,
    env: Mapping[str, str] | None = None,
    input: bytes | None = None,
) -> subprocess.CompletedProcess[bytes]:
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


def _git(
    checkout: Path,
    arguments: Sequence[str],
    *,
    accepted_returncodes: frozenset[int] = frozenset({0}),
    input: bytes | None = None,
) -> subprocess.CompletedProcess[bytes]:
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


def _verify_checkout_unchanged(
    checkout: Path,
    expected_commit: str,
    *,
    label: str,
    clean: bool,
) -> None:
    _require_checkout(checkout, expected_commit, label=label, clean=clean)


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


def _represented_runs(row: Mapping[str, Any], index: int) -> int:
    payload = _object(row.get("series"), f"series rows[{index}].series")
    return _integer(
        payload.get("num_runs", 1),
        f"series rows[{index}].series.num_runs",
        minimum=1,
    )


def _checker_input_sha256(rows: Sequence[Mapping[str, Any]]) -> str:
    encoded_rows = sorted(_canonical_bytes(row) for row in rows)
    checker_input = b"".join(row + b"\n" for row in encoded_rows)
    return _bytes_digest(checker_input)


@dataclass(frozen=True)
class SeriesPopulation:
    rows: list[dict[str, Any]]
    schemas: dict[str, str]
    represented_runs: dict[str, int]
    represented_run_count: int
    rows_sha256: str


def _series_population(
    canonical_input: Mapping[str, Any],
) -> tuple[Mapping[str, Any], SeriesPopulation]:
    schema_version = _integer(
        canonical_input.get("schema_version"),
        "canonical input schema_version",
        minimum=1,
    )
    if schema_version != 1:
        raise BuildRefused("canonical input schema_version must be 1")
    series = _object(canonical_input.get("series"), "canonical input series")
    rows = _array(series.get("rows"), "canonical input series.rows")
    row_count = _integer(series.get("row_count"), "series.row_count")
    if row_count != len(rows):
        raise BuildRefused("series.row_count disagrees with rows")
    rows_sha256 = _sha256(series.get("rows_sha256"), "series.rows_sha256")
    if rows_sha256 != _digest(rows):
        raise BuildRefused("series.rows_sha256 disagrees with rows")

    event_ids: list[str] = []
    copied_rows: list[dict[str, Any]] = []
    schemas: dict[str, str] = {}
    represented: dict[str, int] = {}
    for index, raw in enumerate(rows):
        row = _object(raw, f"series rows[{index}]")
        event_id = _string(row.get("event_id"), f"series rows[{index}].event_id")
        schema = _string(row.get("schema"), f"series rows[{index}].schema")
        event_ids.append(event_id)
        copied_rows.append(dict(row))
        schemas[event_id] = schema
        represented[event_id] = _represented_runs(row, index)
    if len(event_ids) != len(set(event_ids)):
        raise BuildRefused("canonical series rows contain duplicate event_id values")
    represented_run_count = _integer(
        series.get("represented_run_count"), "series.represented_run_count"
    )
    if represented_run_count != sum(represented.values()):
        raise BuildRefused("series.represented_run_count disagrees with rows")
    return series, SeriesPopulation(
        rows=copied_rows,
        schemas=schemas,
        represented_runs=represented,
        represented_run_count=represented_run_count,
        rows_sha256=rows_sha256,
    )


def _projection_results(
    series: Mapping[str, Any], population: SeriesPopulation
) -> dict[str, str | None]:
    event_ids = set(population.schemas)

    raw_projection = _object(
        series.get("projection_results"), "series.projection_results"
    )
    projection: dict[str, str | None] = {}
    for event_id, reason in raw_projection.items():
        if not isinstance(event_id, str) or not event_id.strip():
            raise BuildRefused("series.projection_results keys must be nonempty strings")
        if reason is not None and (
            not isinstance(reason, str) or not reason.strip()
        ):
            raise BuildRefused(
                "series.projection_results values must be null or a nonempty reason"
            )
        projection[event_id] = reason
    missing = sorted(event_ids.difference(projection))
    unknown = sorted(set(projection).difference(event_ids))
    if missing:
        raise BuildRefused(
            "series.projection_results omitted event_id(s): " + ", ".join(missing)
        )
    if unknown:
        raise BuildRefused(
            "series.projection_results names unknown event_id(s): "
            + ", ".join(unknown)
        )

    declared_result_count = _integer(
        series.get("projection_result_physical_row_count"),
        "series.projection_result_physical_row_count",
    )
    if declared_result_count != len(population.rows):
        raise BuildRefused(
            "series.projection_result_physical_row_count disagrees with rows"
        )
    projection_sha256 = _sha256(
        series.get("projection_results_sha256"),
        "series.projection_results_sha256",
    )
    if projection_sha256 != _digest(projection):
        raise BuildRefused(
            "series.projection_results_sha256 disagrees with projection_results"
        )

    return dict(sorted(projection.items()))


def _validate_projection_binding(
    series: Mapping[str, Any],
    population: SeriesPopulation,
    projection: Mapping[str, str | None],
) -> None:
    binding = _object(
        series.get("projection_results_binding"),
        "series.projection_results_binding",
    )
    if set(binding) != {
        "checker_input_jsonl_sha256",
        "physical_row_count",
        "represented_run_count",
        "rows_sha256",
    }:
        raise BuildRefused("series.projection_results_binding has an invalid shape")
    checker_input_sha256 = _sha256(
        binding.get("checker_input_jsonl_sha256"),
        "series.projection_results_binding.checker_input_jsonl_sha256",
    )
    if checker_input_sha256 != _checker_input_sha256(population.rows):
        raise BuildRefused(
            "projection binding checker_input_jsonl_sha256 disagrees with rows"
        )
    if _integer(
        binding.get("physical_row_count"),
        "series.projection_results_binding.physical_row_count",
    ) != len(population.rows):
        raise BuildRefused("projection binding physical row count disagrees with rows")
    if _integer(
        binding.get("represented_run_count"),
        "series.projection_results_binding.represented_run_count",
    ) != population.represented_run_count:
        raise BuildRefused(
            "projection binding represented run count disagrees with rows"
        )
    if _sha256(
        binding.get("rows_sha256"),
        "series.projection_results_binding.rows_sha256",
    ) != population.rows_sha256:
        raise BuildRefused("projection binding rows_sha256 disagrees with rows")


def _validate_projection_counts(
    series: Mapping[str, Any],
    population: SeriesPopulation,
    projection: Mapping[str, str | None],
) -> None:
    admitted_physical = sum(reason is None for reason in projection.values())
    admitted_represented = sum(
        population.represented_runs[event_id]
        for event_id, reason in projection.items()
        if reason is None
    )
    excluded_physical = len(population.rows) - admitted_physical
    excluded_represented = population.represented_run_count - admitted_represented
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
        key = (population.schemas[event_id], reason)
        physical_by_reason[key] += 1
        represented_by_reason[key] += population.represented_runs[event_id]
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


def _validate_projection(
    canonical_input: Mapping[str, Any],
) -> tuple[list[dict[str, Any]], dict[str, str | None]]:
    series, population = _series_population(canonical_input)
    projection = _projection_results(series, population)
    _validate_projection_binding(series, population, projection)
    _validate_projection_counts(series, population, projection)
    return population.rows, projection


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
    rows = _array(
        ledger.get("current_validation_run_rows"),
        "validation_ledger.current_validation_run_rows",
    )
    declared_count = _integer(
        ledger.get("current_validation_run_row_count"),
        "validation_ledger.current_validation_run_row_count",
    )
    if declared_count != len(rows):
        raise BuildRefused(
            "validation_ledger.current_validation_run_row_count disagrees with rows"
        )
    declared_digest = _sha256(
        ledger.get("current_validation_run_rows_sha256"),
        "validation_ledger.current_validation_run_rows_sha256",
    )
    if declared_digest != _digest(rows):
        raise BuildRefused(
            "validation_ledger.current_validation_run_rows_sha256 disagrees with rows"
        )
    return [dict(_object(row, "current validation row")) for row in rows]


def _require_satisfied_cell_results(row: Mapping[str, Any]) -> None:
    qualifying_receipt = _modules().qualifying_receipt
    try:
        verdict = qualifying_receipt.cell_results_verdict(
            dict(row), qualifying_receipt.active()
        )
    except (OSError, RuntimeError, ValueError) as error:
        raise BuildRefused(
            f"canonical cell_results verdict could not be evaluated: {error}"
        ) from error
    if verdict is not qualifying_receipt.CellResultsVerdict.SATISFIED:
        value = getattr(verdict, "value", repr(verdict))
        raise BuildRefused(f"canonical cell_results verdict is {value}")


def _cell_identity(value: Any, label: str) -> str:
    item = _object(value, label)
    test = _string(item.get("test"), f"{label}.test")
    mode = _string(item.get("mode"), f"{label}.mode")
    backend = _string(item.get("backend"), f"{label}.backend")
    return f"{test}/{mode}/{backend}"


@dataclass(frozen=True)
class ValidationSelection:
    commit: str
    path: str
    record_id: str | None
    selected_cells: frozenset[str]


def _validation_selection_runs(
    canonical_input: Mapping[str, Any],
) -> dict[str, ValidationSelection]:
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
            raise BuildRefused(
                f"current validation ledger contains duplicate run_id {run_id!r}"
            )
        rows_by_run[run_id] = row
        evidence = row.get("cell_results")
        if evidence is None:
            continue
        _require_satisfied_cell_results(row)
        evidence = _object(
            evidence, f"current validation rows[{index}].cell_results"
        )
        if _string(
            evidence.get("run_id"),
            f"current validation rows[{index}].cell_results.run_id",
        ) != run_id:
            raise BuildRefused(f"cell_results run_id disagrees for {run_id!r}")
        commit = _sha(row.get("commit"), f"current validation rows[{index}].commit")
        if _sha(
            evidence.get("hermit_sha"),
            f"current validation rows[{index}].cell_results.hermit_sha",
        ) != commit:
            raise BuildRefused(f"cell_results Hermit commit disagrees for {run_id!r}")
        _string(
            row.get("selection_mode"),
            f"current validation rows[{index}].selection_mode",
        )
        path = _string(row.get("profile"), f"current validation rows[{index}].profile")
        selected = {
            _cell_identity(value, f"current validation rows[{index}].selected[{item_index}]")
            for item_index, value in enumerate(
                _array(
                    evidence.get("selected"),
                    f"current validation rows[{index}].cell_results.selected",
                )
            )
        }
        if len(selected) != len(evidence["selected"]):
            raise BuildRefused(
                f"cell_results selected population for {run_id!r} contains duplicates"
            )
        record_id = row.get("record_id")
        if record_id is not None:
            record_id = _string(
                record_id, f"current validation rows[{index}].record_id"
            )
        selections[run_id] = ValidationSelection(
            commit=commit,
            path=path,
            record_id=record_id,
            selected_cells=frozenset(selected),
        )
    return selections


def _selection_authorities(
    canonical_input: Mapping[str, Any], rows: Sequence[Mapping[str, Any]]
) -> tuple[dict[str, dict[str, Any]], list[dict[str, Any]]]:
    selections = _validation_selection_runs(canonical_input)

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


def _main_ancestry(
    hermit: Path, hermit_main_commit: str, trees: Sequence[str]
) -> dict[str, bool | None]:
    hermit_main_commit = _sha(hermit_main_commit, "Hermit main commit")
    _require_complete_commit_graph(hermit)

    decisions: dict[str, bool | None] = {}
    identities = sorted(set(trees))
    for tree in identities:
        _sha(tree, "series Hermit tree")
    if not identities:
        return decisions

    types = _git(
        hermit,
        ["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        input=("\n".join(identities) + "\n").encode(),
    )
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


def _validate_manifest_metadata(value: Mapping[str, Any]) -> tuple[int, str]:
    schema = _integer(value.get("schema"), "manifest metadata schema", minimum=1)
    if schema != 1:
        raise BuildRefused("manifest metadata schema must be 1")
    manifest_schema = _integer(
        value.get("manifest_schema"), "manifest metadata manifest_schema", minimum=1
    )
    manifest_sha256 = _sha256(
        value.get("manifest_sha256"), "manifest metadata manifest_sha256"
    )
    return manifest_schema, manifest_sha256


def _validate_reader_hermit(value: Any) -> None:
    reader_hermit = _exact_object(
        value,
        "provenance.reader_hermit",
        required=frozenset({"checkout_commit", "gitlink_commit"}),
    )
    checkout_commit = _sha(
        reader_hermit.get("checkout_commit"),
        "provenance.reader_hermit.checkout_commit",
    )
    gitlink_commit = _sha(
        reader_hermit.get("gitlink_commit"),
        "provenance.reader_hermit.gitlink_commit",
    )
    if checkout_commit != gitlink_commit:
        raise BuildRefused(
            "provenance.reader_hermit checkout_commit and gitlink_commit disagree"
        )


def _validate_reader_files(value: Any) -> list[Any]:
    reader_files = _array(value, "provenance.reader_files")
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
            raise BuildRefused(f"provenance.reader_files repeats path {path}")
        reader_paths.add(path)
        _sha256(item.get("sha256"), f"provenance.reader_files[{index}].sha256")
    return reader_files


def _validate_sources(value: Any) -> list[Any]:
    sources = _array(value, "provenance.sources")
    source_identities: set[tuple[str, str, str]] = set()
    for index, raw in enumerate(sources):
        item = _exact_object(
            raw,
            f"provenance.sources[{index}]",
            required=frozenset(
                {"bytes", "line_count", "location", "path", "sha256", "store"}
            ),
            optional=frozenset({"git_object"}),
        )
        _integer(item.get("bytes"), f"provenance.sources[{index}].bytes")
        _integer(item.get("line_count"), f"provenance.sources[{index}].line_count")
        location = _string(
            item.get("location"), f"provenance.sources[{index}].location"
        )
        path = _relative_path(item.get("path"), f"provenance.sources[{index}].path")
        _sha256(item.get("sha256"), f"provenance.sources[{index}].sha256")
        store = _string(item.get("store"), f"provenance.sources[{index}].store")
        identity = (store, location, path)
        if identity in source_identities:
            raise BuildRefused(
                "provenance.sources repeats identity " + "/".join(identity)
            )
        source_identities.add(identity)
        if "git_object" in item:
            _object_id(
                item.get("git_object"), f"provenance.sources[{index}].git_object"
            )
    return sources


def _canonical_provenance(
    value: Mapping[str, Any], expected_parent_commit: str
) -> dict[str, Any]:
    value = _exact_object(
        value,
        "canonical input provenance",
        required=frozenset(
            {
                "parent_commit",
                "published_ref",
                "reader_ci_hub_tree",
                "reader_commit",
                "reader_files",
                "reader_files_sha256",
                "reader_hermit",
                "source_count",
                "sources",
                "sources_sha256",
            }
        ),
    )
    parent_commit = _sha(value.get("parent_commit"), "provenance.parent_commit")
    published_ref = _sha(value.get("published_ref"), "provenance.published_ref")
    reader_commit = _sha(value.get("reader_commit"), "provenance.reader_commit")
    if {parent_commit, published_ref, reader_commit} != {expected_parent_commit}:
        raise BuildRefused(
            "canonical input provenance parent_commit, published_ref, and "
            "reader_commit must equal the requested parent commit"
        )
    _object_id(value.get("reader_ci_hub_tree"), "provenance.reader_ci_hub_tree")
    _validate_reader_hermit(value.get("reader_hermit"))
    reader_files = _validate_reader_files(value.get("reader_files"))
    reader_files_sha256 = _sha256(
        value.get("reader_files_sha256"), "provenance.reader_files_sha256"
    )
    if reader_files_sha256 != _digest(reader_files):
        raise BuildRefused("provenance.reader_files_sha256 disagrees with reader_files")

    sources = _validate_sources(value.get("sources"))
    if _integer(value.get("source_count"), "provenance.source_count") != len(sources):
        raise BuildRefused("provenance.source_count disagrees with sources")
    sources_sha256 = _sha256(
        value.get("sources_sha256"), "provenance.sources_sha256"
    )
    if sources_sha256 != _digest(sources):
        raise BuildRefused("provenance.sources_sha256 disagrees with sources")
    if _contains_string(value, _OWNER_PATH_NEEDLE):
        raise BuildRefused("canonical input provenance contains an absolute owner path")
    copied = json.loads(_canonical_bytes(value))
    if copied.get("parent_commit") != parent_commit:
        raise BuildRefused("canonical input provenance changed during normalization")
    return copied


def _contains_string(value: Any, needle: str) -> bool:
    if isinstance(value, str):
        return needle in value
    if isinstance(value, Mapping):
        return any(
            (isinstance(key, str) and needle in key)
            or _contains_string(item, needle)
            for key, item in value.items()
        )
    if isinstance(value, list):
        return any(_contains_string(item, needle) for item in value)
    return False


def _recursive_keys(value: Any) -> set[str]:
    if isinstance(value, Mapping):
        return set(value).union(*(_recursive_keys(item) for item in value.values()))
    if isinstance(value, list):
        return set().union(*(_recursive_keys(item) for item in value))
    return set()


def _build_provenance(
    canonical_document: CapturedJson,
    canonical_provenance: Mapping[str, Any],
    manifest_document: CapturedJson,
    *,
    parent_commit: str,
    hermit_commit: str,
    hermit_main_commit: str,
    manifest_schema: int,
    manifest_sha256: str,
    ancestry: Mapping[str, bool | None],
    products: Mapping[str, str | None],
    selection: Mapping[str, Mapping[str, Any]],
    missing: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    canonical = canonical_document.value
    series = _object(canonical.get("series"), "canonical input series")
    validation = _object(
        canonical.get("validation_ledger"), "canonical input validation_ledger"
    )
    projection_binding = _object(
        series.get("projection_results_binding"),
        "series.projection_results_binding",
    )
    return {
        "canonical_input_bytes_sha256": _bytes_digest(canonical_document.encoded),
        "canonical_input_provenance": dict(canonical_provenance),
        "current_main_ancestry_sha256": _digest(ancestry),
        "current_validation_run_rows_sha256": _sha256(
            validation.get("current_validation_run_rows_sha256"),
            "validation_ledger.current_validation_run_rows_sha256",
        ),
        "hermit_commit": hermit_commit,
        "hermit_main_commit": hermit_main_commit,
        "manifest_metadata_bytes_sha256": _bytes_digest(manifest_document.encoded),
        "manifest_schema": manifest_schema,
        "manifest_sha256": manifest_sha256,
        "missing_executions_sha256": _digest(missing),
        "parent_commit": parent_commit,
        "product_results_sha256": _digest(products),
        "projection_checker_input_jsonl_sha256": _sha256(
            projection_binding.get("checker_input_jsonl_sha256"),
            "series.projection_results_binding.checker_input_jsonl_sha256",
        ),
        "projection_results_sha256": _sha256(
            series.get("projection_results_sha256"),
            "series.projection_results_sha256",
        ),
        "series_rows_sha256": _sha256(
            series.get("rows_sha256"), "series.rows_sha256"
        ),
        "validate_run_selection_sha256": _digest(selection),
    }


def _validated_reduction(value: Any) -> dict[str, Any]:
    reducer = _modules().reducer
    reduced = _object(value, "compatibility reduction")
    if _integer(
        reduced.get("schema_version"),
        "compatibility reduction schema_version",
        minimum=1,
    ) != reducer.SCHEMA_VERSION:
        raise BuildRefused("compatibility reduction schema_version is unsupported")
    cells = reduced.get("cells")
    counts = reduced.get("counts")
    incomplete = reduced.get("incomplete_data")
    if not isinstance(cells, list):
        raise BuildRefused("compatibility reduction cells must be an array")
    if not isinstance(counts, Mapping):
        raise BuildRefused("compatibility reduction counts must be an object")
    if not isinstance(incomplete, list):
        raise BuildRefused("compatibility reduction incomplete_data must be an array")
    return {
        "cells": cells,
        "counts": dict(counts),
        "incomplete_data": incomplete,
    }


def _require_publishable_output(value: Mapping[str, Any]) -> None:
    forbidden = {"enabled", "measured", "measurement_state", "never_measured"}
    present = sorted(forbidden.intersection(_recursive_keys(value)))
    if present:
        raise BuildRefused(
            "compatibility reduction contains forbidden stored field(s): "
            + ", ".join(present)
        )
    if _contains_string(value, _OWNER_PATH_NEEDLE):
        raise BuildRefused("compatibility reduction contains an absolute owner path")


def build_data(
    parent_value: Path,
    parent_commit_value: str,
    hermit_value: Path,
    hermit_commit_value: str,
    hermit_main_commit_value: str,
) -> dict[str, Any]:
    """Build one deterministic reduction from exact canonical authorities."""
    _refuse_authority_overrides()
    reducer = _modules().reducer
    parent_commit = _sha(parent_commit_value, "parent commit")
    hermit_commit = _sha(hermit_commit_value, "Hermit commit")
    hermit_main_commit = _sha(hermit_main_commit_value, "Hermit main commit")
    parent = _require_checkout(
        parent_value, parent_commit, label="dev-hermit", clean=True
    )
    hermit = _require_checkout(
        hermit_value, hermit_commit, label="Hermit", clean=True
    )
    _require_main_target(hermit, hermit_main_commit)

    canonical_document = _capture_canonical_input(parent, parent_commit)
    manifest_document = _run_manifest_metadata(hermit)
    _verify_checkout_unchanged(
        parent, parent_commit, label="dev-hermit", clean=True
    )
    _verify_checkout_unchanged(
        hermit, hermit_commit, label="Hermit", clean=True
    )
    _require_main_target(hermit, hermit_main_commit)

    canonical = _object(canonical_document.value, "canonical input")
    provenance = _canonical_provenance(
        _object(canonical.get("provenance"), "canonical input provenance"),
        parent_commit,
    )
    if _sha(
        provenance.get("parent_commit"), "canonical input provenance.parent_commit"
    ) != parent_commit:
        raise BuildRefused("canonical input parent commit disagrees with requested commit")
    rows, projection = _validate_projection(canonical)
    manifest = _object(manifest_document.value, "manifest metadata")
    manifest_schema, manifest_sha256 = _validate_manifest_metadata(manifest)
    products = _product_results(rows)
    selection, missing = _selection_authorities(canonical, rows)
    trees = [
        *(
            _string(_object(row.get("series"), "series row.series").get("tree"), "series row tree")
            for row in rows
        ),
        *(str(item["tree"]) for item in missing),
    ]
    ancestry = _main_ancestry(hermit, hermit_main_commit, trees)

    try:
        reduction = reducer.reduce_inputs(
            canonical,
            manifest,
            projection_results=projection,
            current_main_ancestry=ancestry,
            product_results=products,
            producer_observed_at={},
            validate_run_selection=selection,
            missing_executions=missing,
        )
    except reducer.ReductionRefused as error:
        raise BuildRefused(f"compatibility reduction refused: {error}") from error
    result = {
        **_validated_reduction(reduction),
        "provenance": _build_provenance(
            canonical_document,
            provenance,
            manifest_document,
            parent_commit=parent_commit,
            hermit_commit=hermit_commit,
            hermit_main_commit=hermit_main_commit,
            manifest_schema=manifest_schema,
            manifest_sha256=manifest_sha256,
            ancestry=ancestry,
            products=products,
            selection=selection,
            missing=missing,
        ),
        "schema_version": SCHEMA_VERSION,
    }
    _require_publishable_output(result)
    _verify_checkout_unchanged(
        parent, parent_commit, label="dev-hermit", clean=True
    )
    _verify_checkout_unchanged(
        hermit, hermit_commit, label="Hermit", clean=True
    )
    _require_main_target(hermit, hermit_main_commit)
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
        help=(
            "full Hermit ancestry target; must equal the checkout's stable "
            "refs/remotes/origin/main"
        ),
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
