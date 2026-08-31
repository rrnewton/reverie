#!/usr/bin/env python3
"""Pure compatibility-website reduction over already-canonical fixture inputs.

The canonical input and manifest exports own source parsing and schema
validation.  This module owns only per-current-cell state reduction. Projection
validation, product result, producer-owned execution time, validate-run
selection, and current-main ancestry are injected authority decisions. Their
absence is incomplete data, never permission to reconstruct another predicate
or trust similarly named raw fields.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections import defaultdict
from collections.abc import Mapping, Sequence
from datetime import datetime
from typing import Any


SCHEMA_VERSION = 1
_CANONICAL_INPUT_SCHEMA = 1
_MANIFEST_METADATA_SCHEMA = 1
_PRODUCERS = frozenset({"validate", "pressure-test", "hermit-repeat"})
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")


class ReductionRefused(ValueError):
    """The supplied inputs cannot support one deterministic honest view."""


def _object(value: Any, label: str) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ReductionRefused(f"{label} must be an object")
    return value


def _array(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise ReductionRefused(f"{label} must be an array")
    return value


def _string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ReductionRefused(f"{label} must be a nonempty string")
    return value


def _integer(value: Any, label: str, *, minimum: int = 0) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        raise ReductionRefused(f"{label} must be an integer >= {minimum}")
    return value


def _optional_string(value: Any, label: str) -> str | None:
    if value is None:
        return None
    return _string(value, label)


def _parse_observed_at(value: Any, label: str) -> tuple[datetime, str] | None:
    if value is None:
        return None
    raw = _string(value, label)
    assert raw is not None
    try:
        parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError as error:
        raise ReductionRefused(f"{label} must be an RFC3339 timestamp") from error
    if parsed.tzinfo is None:
        raise ReductionRefused(f"{label} must include a timezone")
    return (parsed, raw)


def _canonical_cell(test: str, mode: str, backend: str) -> str:
    return f"{test}/{mode}/{backend}"


def _digest(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def _sha256(value: Any, label: str) -> str:
    digest = _string(value, label) or ""
    if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise ReductionRefused(f"{label} must be lowercase 64-hex")
    return digest


def _commit(value: Any, label: str) -> str:
    commit = _string(value, label) or ""
    if _SHA_RE.fullmatch(commit) is None:
        raise ReductionRefused(f"{label} must be a lowercase 40-hex commit")
    return commit


def _manifest_cell(value: Any, index: int) -> dict[str, Any]:
    cell = _object(value, f"manifest cells[{index}]")
    label = f"manifest cells[{index}]"
    forbidden = {"measured", "measurement_state", "never_measured", "state"}
    present = sorted(forbidden.intersection(cell))
    if present:
        raise ReductionRefused(
            f"{label} contains ledger-derived field(s): {', '.join(present)}"
        )
    test = _string(cell.get("test"), f"{label}.test") or ""
    mode = _string(cell.get("mode"), f"{label}.mode") or ""
    backend = _string(cell.get("backend"), f"{label}.backend") or ""
    return {
        "cell": _canonical_cell(test, mode, backend),
        "test": test,
        "mode": mode,
        "backend": backend,
    }


def _normalise_manifest(
    manifest_metadata: Any,
) -> list[dict[str, Any]]:
    manifest = _object(manifest_metadata, "manifest metadata")
    schema = _integer(manifest.get("schema"), "manifest metadata schema", minimum=1)
    if schema != _MANIFEST_METADATA_SCHEMA:
        raise ReductionRefused(
            f"manifest metadata schema must be {_MANIFEST_METADATA_SCHEMA}"
        )
    cells = [
        _manifest_cell(value, index)
        for index, value in enumerate(_array(manifest.get("cells"), "manifest cells"))
    ]
    cells.sort(key=lambda cell: (cell["test"], cell["mode"], cell["backend"]))
    seen_cells: set[str] = set()
    for cell in cells:
        if cell["cell"] in seen_cells:
            raise ReductionRefused(f"duplicate manifest cell {cell['cell']!r}")
        seen_cells.add(cell["cell"])
    return cells


def _normalise_series_row(value: Any, index: int) -> dict[str, Any]:
    row = _object(value, f"series rows[{index}]")
    label = f"series rows[{index}]"
    event_id = _string(row.get("event_id"), f"{label}.event_id") or ""
    producer = _string(row.get("producer"), f"{label}.producer") or ""
    if producer not in _PRODUCERS:
        raise ReductionRefused(f"{label}.producer is unknown: {producer!r}")
    payload = _object(row.get("series"), f"{label}.series")
    cell = _string(payload.get("cell"), f"{label}.series.cell") or ""
    tree = _string(payload.get("tree"), f"{label}.series.tree") or ""
    if _SHA_RE.fullmatch(tree) is None:
        raise ReductionRefused(f"{label}.series.tree must be lowercase 40-hex")
    run_index = _integer(
        payload.get("run_index"), f"{label}.series.run_index", minimum=0
    )
    attempt = payload.get("attempt")
    if attempt is not None:
        attempt = _integer(attempt, f"{label}.series.attempt", minimum=1)
    num_runs = _integer(
        payload.get("num_runs", 1), f"{label}.series.num_runs", minimum=1
    )
    return {
        "event_id": event_id,
        "observed_at": None,
        "order_at": None,
        "producer": producer,
        "run_id": _string(row.get("run_id"), f"{label}.run_id"),
        "cell": cell,
        "tree": tree,
        "run_index": run_index,
        "attempt": attempt,
        "num_runs": num_runs,
        "product_result_known": False,
        "product_result": None,
    }


def _normalise_incomplete(value: Any, index: int) -> dict[str, str]:
    item = _object(value, f"incomplete_data[{index}]")
    out = {
        "field": _string(item.get("field"), f"incomplete_data[{index}].field") or "",
        "input": _string(item.get("input"), f"incomplete_data[{index}].input") or "",
        "reason": _string(item.get("reason"), f"incomplete_data[{index}].reason") or "",
    }
    for field in ("cell", "event_id", "path", "record_id", "run_id", "schema"):
        value = _optional_string(item.get(field), f"incomplete_data[{index}].{field}")
        if value is not None:
            out[field] = value
    return out


def _normalise_canonical_input(
    canonical_input: Any,
) -> tuple[list[dict[str, Any]], list[dict[str, str]]]:
    canonical = _object(canonical_input, "canonical input")
    schema_version = _integer(
        canonical.get("schema_version"), "canonical input schema_version", minimum=1
    )
    if schema_version != _CANONICAL_INPUT_SCHEMA:
        raise ReductionRefused(
            f"canonical input schema_version must be {_CANONICAL_INPUT_SCHEMA}"
        )
    series = _object(canonical.get("series"), "canonical input series")
    raw_rows = _array(series.get("rows"), "canonical input series.rows")
    rows_sha256 = _sha256(
        series.get("rows_sha256"), "canonical input series.rows_sha256"
    )
    if rows_sha256 != _digest(raw_rows):
        raise ReductionRefused("canonical input series.rows_sha256 disagrees with rows")
    rows = [_normalise_series_row(value, index) for index, value in enumerate(raw_rows)]
    event_ids: set[str] = set()
    for row in rows:
        if row["event_id"] in event_ids:
            raise ReductionRefused(
                f"duplicate canonical series event_id {row['event_id']!r}"
            )
        event_ids.add(row["event_id"])
    row_count = _integer(
        series.get("row_count"), "canonical input series.row_count", minimum=0
    )
    if row_count != len(rows):
        raise ReductionRefused(
            f"canonical input series.row_count={row_count} but rows has {len(rows)} item(s)"
        )
    represented = sum(row["num_runs"] for row in rows)
    declared_represented = _integer(
        series.get("represented_run_count"),
        "canonical input series.represented_run_count",
        minimum=0,
    )
    if declared_represented != represented:
        raise ReductionRefused(
            "canonical input series.represented_run_count disagrees with num_runs"
        )

    incomplete = [
        _normalise_incomplete(value, index)
        for index, value in enumerate(
            _array(canonical.get("incomplete_data"), "canonical input incomplete_data")
        )
    ]
    return rows, incomplete


def _normalise_decisions(
    event_ids: set[str],
    hermit_commits: set[str],
    projection_results: Mapping[str, str | None] | None,
    current_main_ancestry: Mapping[str, bool | None] | None,
) -> tuple[dict[str, str | None], dict[str, bool | None]]:
    projection = (
        {}
        if projection_results is None
        else dict(_object(projection_results, "projection_results"))
    )
    ancestry = (
        {}
        if current_main_ancestry is None
        else dict(_object(current_main_ancestry, "current_main_ancestry"))
    )
    if any(not isinstance(key, str) or not key.strip() for key in projection):
        raise ReductionRefused("projection_results keys must be nonempty strings")
    if any(
        not isinstance(key, str) or _SHA_RE.fullmatch(key) is None for key in ancestry
    ):
        raise ReductionRefused(
            "current_main_ancestry keys must be lowercase 40-hex Hermit commits"
        )
    extra_projection = sorted(set(projection).difference(event_ids))
    if extra_projection:
        raise ReductionRefused(
            "projection_results names unknown series event_id(s): "
            + ", ".join(extra_projection)
        )
    extra_ancestry = sorted(set(ancestry).difference(hermit_commits))
    if extra_ancestry:
        raise ReductionRefused(
            "current_main_ancestry names unknown Hermit commit(s): "
            + ", ".join(extra_ancestry)
        )
    if any(
        reason is not None and (not isinstance(reason, str) or not reason.strip())
        for reason in projection.values()
    ):
        raise ReductionRefused("projection_results values must be null or a reason")
    if any(
        value is not None and not isinstance(value, bool) for value in ancestry.values()
    ):
        raise ReductionRefused("current_main_ancestry values must be bool or null")
    return projection, ancestry


def _normalise_validate_run_selection(
    rows: Sequence[Mapping[str, Any]],
    value: Mapping[str, Mapping[str, Any] | None] | None,
) -> dict[str, dict[str, Any] | None]:
    selections = {} if value is None else dict(_object(value, "validate_run_selection"))
    validate_event_ids = {
        str(row["event_id"]) for row in rows if row["producer"] == "validate"
    }
    normalised: dict[str, dict[str, Any] | None] = {}
    for event_id, decision in selections.items():
        if not isinstance(event_id, str) or not event_id.strip():
            raise ReductionRefused(
                "validate_run_selection keys must be nonempty strings"
            )
        if event_id not in validate_event_ids:
            raise ReductionRefused(
                "validate_run_selection names an unknown or non-validate "
                f"series event_id: {event_id}"
            )
        if decision is None:
            normalised[event_id] = None
            continue
        item = _object(decision, f"validate_run_selection[{event_id!r}]")
        if set(item) != {"path", "selected"}:
            raise ReductionRefused(
                f"validate_run_selection[{event_id!r}] must contain only path and selected"
            )
        path = _string(item.get("path"), f"validate_run_selection[{event_id!r}].path")
        selected = item.get("selected")
        if not isinstance(selected, bool):
            raise ReductionRefused(
                f"validate_run_selection[{event_id!r}].selected must be bool"
            )
        if selected is False:
            raise ReductionRefused(
                f"validate series event {event_id!r} records a cell unselected by {path}"
            )
        normalised[event_id] = {"path": path, "selected": selected}
    return normalised


def _selected_by(
    rows: Sequence[Mapping[str, Any]],
    selections: Mapping[str, Mapping[str, Any] | None],
    missing_executions: Sequence[Mapping[str, Any]],
) -> dict[str, dict[str, list[dict[str, Any]]]]:
    evidence: dict[
        tuple[str, str, str, str], list[tuple[str | None, tuple[datetime, str] | None]]
    ] = defaultdict(list)
    paths_by_run: dict[tuple[str, str], set[str]] = defaultdict(set)
    for row in rows:
        if row["producer"] != "validate":
            continue
        decision = selections.get(str(row["event_id"]))
        if decision is None:
            continue
        path = str(decision["path"])
        run_id = str(row["run_id"])
        tree = str(row["tree"])
        paths_by_run[(run_id, tree)].add(path)
        evidence[(str(row["cell"]), path, run_id, tree)].append(
            (str(row["event_id"]), row["observed_at"])
        )
    for item in missing_executions:
        path = str(item["path"])
        run_id = str(item["run_id"])
        tree = str(item["tree"])
        paths_by_run[(run_id, tree)].add(path)
        evidence[(str(item["cell"]), path, run_id, tree)].append(
            (None, item["observed_at"])
        )
    for (run_id, _tree), paths in sorted(paths_by_run.items()):
        if len(paths) != 1:
            raise ReductionRefused(
                f"validate run {run_id!r} is selected by multiple paths"
            )

    by_cell: dict[str, dict[str, list[dict[str, Any]]]] = defaultdict(
        lambda: defaultdict(list)
    )
    for (cell, path, run_id, tree), items in sorted(evidence.items()):
        observed = [value for _event_id, value in items if value is not None]
        observed_keys = {value[0] for value in observed}
        if len(observed_keys) > 1:
            raise ReductionRefused(
                f"validate run {run_id!r} selected by {path} carries "
                "contradictory observed_at values"
            )
        observed_at = (
            min(value[1] for value in observed)
            if len(observed) == len(items) and observed
            else None
        )
        by_cell[cell][path].append(
            {
                "run_id": run_id,
                "tree": tree,
                "observed_at": observed_at,
                "event_ids": sorted(
                    event_id for event_id, _value in items if event_id is not None
                ),
            }
        )
    return {
        cell: {path: facts for path, facts in sorted(paths.items())}
        for cell, paths in sorted(by_cell.items())
    }


def _normalise_product_results(
    rows: Sequence[dict[str, Any]], value: Mapping[str, str | None] | None
) -> None:
    results = {} if value is None else dict(_object(value, "product_results"))
    by_event = {str(row["event_id"]): row for row in rows}
    for event_id in results:
        if not isinstance(event_id, str) or not event_id.strip():
            raise ReductionRefused("product_results keys must be nonempty strings")
    extra = sorted(set(results).difference(by_event))
    if extra:
        raise ReductionRefused(
            "product_results names unknown series event_id(s): " + ", ".join(extra)
        )
    for event_id, result in results.items():
        if result is not None and (
            not isinstance(result, str) or result not in {"pass", "product_failure"}
        ):
            raise ReductionRefused(
                f"product_results[{event_id!r}] must be pass, product_failure, or null"
            )
        by_event[event_id]["product_result_known"] = True
        by_event[event_id]["product_result"] = result


def _execution_key(row: Mapping[str, Any]) -> tuple[Any, ...]:
    if row["producer"] == "validate":
        return (row["producer"], row["run_id"], row["cell"])
    return (
        row["producer"],
        row["run_id"],
        row["cell"],
        row["run_index"],
    )


def _retry_ordinal(row: Mapping[str, Any]) -> int:
    if row["producer"] == "validate":
        return int(row["run_index"])
    return int(row["attempt"] if row["attempt"] is not None else 1)


def _retry_issues(
    cell: str, grouped: Sequence[Mapping[str, Any]]
) -> tuple[bool, bool, list[dict[str, str]]]:
    ordinals: dict[int, int] = defaultdict(int)
    for row in grouped:
        ordinals[_retry_ordinal(row)] += 1
    overlap = any(count > 1 for count in ordinals.values()) or (
        len(grouped) > 1 and any(int(row["num_runs"]) > 1 for row in grouped)
    )
    common = {"cell": cell, "run_id": str(grouped[0]["run_id"])}
    if overlap:
        return (
            True,
            True,
            [
                _entry(
                    field="retry grouping",
                    input_name="series",
                    reason=(
                        "overlapping retry ordinals prevent one logical execution "
                        "from being reduced without inferring compressed membership"
                    ),
                    **common,
                )
            ],
        )

    ordinals_in_order = sorted(ordinals)
    gap = ordinals_in_order != list(range(1, ordinals_in_order[-1] + 1))
    compressed_validate = grouped[0]["producer"] == "validate" and any(
        int(row["num_runs"]) > 1 for row in grouped
    )
    explicit_compressed_retry = grouped[0]["producer"] != "validate" and any(
        int(row["num_runs"]) > 1 and row["attempt"] is not None for row in grouped
    )
    reasons = []
    if gap:
        reasons.append(
            "recorded retry ordinals do not start at 1 and remain contiguous"
        )
    if compressed_validate:
        reasons.append(
            "a compressed validate row does not prove complete retry ordinals"
        )
    if explicit_compressed_retry:
        reasons.append(
            "a compressed pressure-test or hermit-repeat row with an explicit "
            "attempt does not prove complete retry ordinals"
        )
    return (
        False,
        bool(reasons),
        [
            _entry(field="retry grouping", input_name="series", reason=reason, **common)
            for reason in reasons
        ],
    )


def _entry(
    field: str,
    input_name: str,
    reason: str,
    **identifiers: str | None,
) -> dict[str, str]:
    item = {"field": field, "input": input_name, "reason": reason}
    for key in ("cell", "event_id", "path", "record_id", "run_id", "schema"):
        if identifiers.get(key) is not None:
            item[key] = str(identifiers[key])
    return item


def _incomplete_sort_key(item: Mapping[str, Any]) -> tuple[str, ...]:
    return tuple(
        str(item.get(field, ""))
        for field in (
            "input",
            "field",
            "cell",
            "run_id",
            "path",
            "event_id",
            "record_id",
            "schema",
            "reason",
        )
    )


def _qualifying_rows(
    cell: str,
    rows: Sequence[Mapping[str, Any]],
    projection_results: Mapping[str, str | None],
    current_main_ancestry: Mapping[str, bool | None],
    validate_run_selection: Mapping[str, Mapping[str, Any] | None],
) -> tuple[
    list[Mapping[str, Any]],
    list[Mapping[str, Any]],
    list[dict[str, str]],
]:
    qualifying = []
    qualification_unknown = []
    incomplete: list[dict[str, str]] = []
    for row in rows:
        event_id = str(row["event_id"])
        common = {
            "cell": cell,
            "event_id": event_id,
            "run_id": str(row["run_id"]),
        }

        def report(field: str, input_name: str, reason: str) -> None:
            incomplete.append(
                _entry(field=field, input_name=input_name, reason=reason, **common)
            )

        has_projection = event_id in projection_results
        projection = projection_results.get(event_id)
        if not has_projection:
            report(
                "projection validation reason",
                "series",
                "the canonical input does not carry a shared projection result",
            )
        elif projection is not None:
            report("qualifying row", "series", projection)

        tree = str(row["tree"])
        has_ancestry = tree in current_main_ancestry
        ancestry = current_main_ancestry.get(tree)
        if not has_ancestry or ancestry is None:
            report(
                "current-main ancestry",
                "series",
                "current-main ancestry was not recomputed for this row",
            )
        elif ancestry is False:
            report(
                "current-main ancestry",
                "series",
                "the row's Hermit tree is not an ancestor of current main",
            )

        if row["producer"] == "validate" and (
            event_id not in validate_run_selection
            or validate_run_selection[event_id] is None
        ):
            report(
                "validate path selection",
                "validation ledger",
                "the validate path and whether it selected this series row's cell "
                "were not supplied",
            )

        if (has_projection and projection is not None) or (
            has_ancestry and ancestry is False
        ):
            continue
        if has_projection and projection is None and ancestry is True:
            qualifying.append(row)
            if not row["product_result_known"]:
                report(
                    "product result",
                    "series",
                    "the product result was not supplied for this "
                    "projection-accepted current-main row",
                )
        else:
            qualification_unknown.append(row)
    return qualifying, qualification_unknown, incomplete


def _reduce_execution(
    cell: str,
    key: tuple[Any, ...],
    grouped: Sequence[Mapping[str, Any]],
    qualifying_event_ids: set[str],
) -> tuple[dict[str, Any], list[dict[str, str]]]:
    trees = {str(row["tree"]) for row in grouped}
    if len(trees) != 1:
        raise ReductionRefused(f"logical execution {key!r} spans multiple Hermit trees")
    grouping_unavailable, red_unavailable, retry_incomplete = _retry_issues(
        cell, grouped
    )
    if grouping_unavailable:
        result = None
    else:
        known_results = [
            row["product_result"]
            for row in grouped
            if row["event_id"] in qualifying_event_ids and row["product_result_known"]
        ]
        has_unknown = len(known_results) != len(grouped)
        if "pass" in known_results:
            result = "pass"
        elif (
            known_results
            and not has_unknown
            and not red_unavailable
            and all(value == "product_failure" for value in known_results)
        ):
            result = "product_failure"
        else:
            result = None

    observed_rows = [row["order_at"] for row in grouped if row["order_at"] is not None]
    observed_keys = {value[0] for value in observed_rows}
    if len(observed_keys) > 1:
        raise ReductionRefused(
            f"logical execution {key!r} carries contradictory observed_at values"
        )
    observed_at = (
        min(value[1] for value in observed_rows)
        if len(observed_rows) == len(grouped)
        else None
    )
    observed_key = observed_rows[0][0] if observed_at is not None else None
    return (
        {
            "input": "series",
            "producer": str(grouped[0]["producer"]),
            "run_id": str(grouped[0]["run_id"]),
            "run_index": (
                None
                if grouped[0]["producer"] == "validate"
                else int(grouped[0]["run_index"])
            ),
            "tree": next(iter(trees)),
            "observed_at": observed_at,
            "observed_key": observed_key,
            "result": result,
            "event_ids": sorted(str(row["event_id"]) for row in grouped),
            "has_exact_run_index": all(int(row["num_runs"]) == 1 for row in grouped),
        },
        retry_incomplete,
    )


def _latest_execution(
    executions: Sequence[Mapping[str, Any]],
) -> Mapping[str, Any] | None:
    if len(executions) == 1:
        return executions[0]
    producers = {execution["producer"] for execution in executions}
    run_ids = {execution["run_id"] for execution in executions}
    same_pressure_or_repeat_run = (
        len(producers) == 1
        and next(iter(producers)) in {"pressure-test", "hermit-repeat"}
        and len(run_ids) == 1
    )
    if same_pressure_or_repeat_run:
        if all(execution["has_exact_run_index"] for execution in executions):
            return max(executions, key=lambda execution: execution["run_index"])
        return None
    if not all(execution["observed_key"] is not None for execution in executions):
        return None
    newest = max(execution["observed_key"] for execution in executions)
    candidates = [
        execution for execution in executions if execution["observed_key"] == newest
    ]
    return candidates[0] if len(candidates) == 1 else None


def reduce_cell(
    cell: str,
    rows: Sequence[Mapping[str, Any]],
    *,
    projection_results: Mapping[str, str | None],
    current_main_ancestry: Mapping[str, bool | None],
    validate_run_selection: Mapping[str, Mapping[str, Any] | None],
    incomplete_executions: Sequence[Mapping[str, Any]] = (),
) -> dict[str, Any]:
    """Reduce one current manifest cell without consulting another state view."""
    _validate_run_trees(rows, incomplete_executions)
    return _reduce_cell(
        cell,
        rows,
        projection_results=projection_results,
        current_main_ancestry=current_main_ancestry,
        validate_run_selection=validate_run_selection,
        incomplete_executions=incomplete_executions,
    )


def _reduce_cell(
    cell: str,
    rows: Sequence[Mapping[str, Any]],
    *,
    projection_results: Mapping[str, str | None],
    current_main_ancestry: Mapping[str, bool | None],
    validate_run_selection: Mapping[str, Mapping[str, Any] | None],
    incomplete_executions: Sequence[Mapping[str, Any]] = (),
) -> dict[str, Any]:
    ordered = sorted(
        rows,
        key=lambda row: (
            str(row["producer"]),
            str(row["run_id"]),
            int(row["run_index"]),
            _retry_ordinal(row),
            str(row["event_id"]),
        ),
    )
    qualifying, qualification_unknown, incomplete = _qualifying_rows(
        cell,
        ordered,
        projection_results,
        current_main_ancestry,
        validate_run_selection,
    )

    physical_row_count = len(ordered)
    represented_run_count = sum(int(row["num_runs"]) for row in ordered)
    qualifying_physical = len(qualifying)
    qualifying_represented = sum(int(row["num_runs"]) for row in qualifying)

    state: str | None
    latest_result: str | None = None
    latest_tree: str | None = None
    latest_time: str | None = None
    latest_event_ids: list[str] = []
    executions: list[dict[str, Any]] = []
    groups: dict[tuple[Any, ...], list[Mapping[str, Any]]] = defaultdict(list)
    for row in [*qualifying, *qualification_unknown]:
        groups[_execution_key(row)].append(row)
    qualifying_event_ids = {str(row["event_id"]) for row in qualifying}
    for key in sorted(groups):
        execution, retry_incomplete = _reduce_execution(
            cell, key, groups[key], qualifying_event_ids
        )
        executions.append(execution)
        incomplete.extend(retry_incomplete)
    executions.extend(dict(item) for item in incomplete_executions)

    if not executions:
        state = "never-measured"
    else:
        latest = _latest_execution(executions)
        if latest is None:
            state = None
            sources = {str(execution["input"]) for execution in executions}
            input_name = (
                next(iter(sources))
                if len(sources) == 1
                else "validation ledger and series"
            )
            incomplete.append(
                _entry(
                    field="latest-run order",
                    input_name=input_name,
                    reason=(
                        "multiple logical executions have no unique producer-owned "
                        "observed_at or exact within-run order"
                    ),
                    cell=cell,
                )
            )
        else:
            latest_result = latest["result"]
            latest_tree = latest["tree"]
            latest_time = latest["observed_at"]
            latest_event_ids = list(latest["event_ids"])
            if latest_result == "pass":
                state = "green"
            elif latest_result == "product_failure":
                state = "red"
            else:
                state = None
                incomplete.append(
                    _entry(
                        field="latest-run state",
                        input_name=str(latest["input"]),
                        reason=(
                            "the newest logical execution does not establish pass or "
                            "product failure"
                        ),
                        cell=cell,
                        run_id=latest["run_id"],
                    )
                )
            if latest_time is None:
                incomplete.append(
                    _entry(
                        field="latest result time",
                        input_name=str(latest["input"]),
                        reason="the logical execution has no producer-owned observed_at",
                        cell=cell,
                        run_id=latest["run_id"],
                    )
                )

    incomplete.sort(key=_incomplete_sort_key)
    return {
        "state": state,
        "latest_result": latest_result,
        "latest_result_tree": latest_tree,
        "latest_result_time": latest_time,
        "latest_event_ids": latest_event_ids,
        "physical_row_count": physical_row_count,
        "represented_run_count": represented_run_count,
        "qualifying_physical_row_count": qualifying_physical,
        "qualifying_represented_run_count": qualifying_represented,
        "event_ids": sorted(str(row["event_id"]) for row in ordered),
        "qualifying_event_ids": sorted(str(row["event_id"]) for row in qualifying),
        "incomplete_fields": sorted({item["field"] for item in incomplete}),
        "incomplete_data": incomplete,
    }


def _normalise_producer_observed_at(
    rows: Sequence[dict[str, Any]], value: Mapping[str, str] | None
) -> None:
    observed = {} if value is None else dict(_object(value, "producer_observed_at"))
    by_event = {str(row["event_id"]): row for row in rows}
    for event_id in observed:
        if not isinstance(event_id, str) or not event_id.strip():
            raise ReductionRefused("producer_observed_at keys must be nonempty strings")
    extra = sorted(set(observed).difference(by_event))
    if extra:
        raise ReductionRefused(
            "producer_observed_at names unknown series event_id(s): " + ", ".join(extra)
        )
    for event_id, raw in observed.items():
        if raw is None:
            raise ReductionRefused(
                f"producer_observed_at[{event_id!r}] must be an RFC3339 timestamp"
            )
        parsed = _parse_observed_at(raw, f"producer_observed_at[{event_id!r}]")
        assert parsed is not None
        by_event[event_id]["observed_at"] = parsed
        by_event[event_id]["order_at"] = parsed


def _normalise_missing_executions(
    value: list[Mapping[str, Any]] | None,
    series_rows: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    raw_entries = [] if value is None else _array(value, "missing_executions")
    existing = {
        (str(row["run_id"]), str(row["cell"]))
        for row in series_rows
        if row["producer"] == "validate"
    }
    seen: set[tuple[str, str]] = set()
    out = []
    for index, raw in enumerate(raw_entries):
        item = _object(raw, f"missing_executions[{index}]")
        label = f"missing_executions[{index}]"
        cell = _string(item.get("cell"), f"{label}.cell") or ""
        run_id = _string(item.get("run_id"), f"{label}.run_id") or ""
        path = _string(item.get("path"), f"{label}.path") or ""
        tree = _commit(item.get("tree"), f"{label}.tree")
        observed_at = _parse_observed_at(
            item.get("observed_at"), f"{label}.observed_at"
        )
        record_id = _optional_string(item.get("record_id"), f"{label}.record_id")
        identity = (run_id, cell)
        if identity in seen:
            raise ReductionRefused(
                f"duplicate missing execution for run {run_id!r} and cell {cell!r}"
            )
        if identity in existing:
            raise ReductionRefused(
                f"missing execution for run {run_id!r} and cell {cell!r} "
                "contradicts a validate series row"
            )
        seen.add(identity)
        out.append(
            {
                "cell": cell,
                "run_id": run_id,
                "path": path,
                "tree": tree,
                "observed_at": observed_at,
                "record_id": record_id,
            }
        )
    return sorted(out, key=lambda item: (item["cell"], item["run_id"]))


def _validate_run_trees(
    rows: Sequence[Mapping[str, Any]],
    missing_executions: Sequence[Mapping[str, Any]],
) -> None:
    trees_by_run: dict[tuple[str, str], set[str]] = defaultdict(set)
    for row in rows:
        trees_by_run[(str(row["producer"]), str(row["run_id"]))].add(str(row["tree"]))
    for item in missing_executions:
        trees_by_run[("validate", str(item["run_id"]))].add(str(item["tree"]))
    for (producer, run_id), trees in sorted(trees_by_run.items()):
        if len(trees) != 1:
            raise ReductionRefused(
                f"{producer} run {run_id!r} spans multiple Hermit trees"
            )


def _reduce_missing_executions(
    entries: Sequence[Mapping[str, Any]],
    current_main_ancestry: Mapping[str, bool | None],
) -> tuple[list[dict[str, str]], dict[str, list[dict[str, Any]]]]:
    incomplete: list[dict[str, str]] = []
    by_cell: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for item in entries:
        cell = str(item["cell"])
        run_id = str(item["run_id"])
        tree = str(item["tree"])
        common = {
            "cell": cell,
            "path": str(item["path"]),
            "record_id": item["record_id"],
            "run_id": run_id,
        }
        incomplete.append(
            _entry(
                field="series row",
                input_name="validation ledger",
                reason=(
                    f"the cell was selected by {item['path']}, but no matching "
                    "validate series row exists"
                ),
                **common,
            )
        )
        ancestry = current_main_ancestry.get(tree)
        if tree not in current_main_ancestry or ancestry is None:
            incomplete.append(
                _entry(
                    field="current-main ancestry",
                    input_name="validation ledger",
                    reason=(
                        "current-main ancestry was not recomputed for this "
                        "validation record"
                    ),
                    **common,
                )
            )
        elif ancestry is False:
            incomplete.append(
                _entry(
                    field="current-main ancestry",
                    input_name="validation ledger",
                    reason=(
                        "the validation record's Hermit commit is not an "
                        "ancestor of current main"
                    ),
                    **common,
                )
            )
            continue
        observed_at = item["observed_at"]
        by_cell[cell].append(
            {
                "input": "validation ledger",
                "producer": "validate",
                "run_id": run_id,
                "run_index": None,
                "tree": tree,
                "observed_at": (observed_at[1] if observed_at is not None else None),
                "observed_key": (observed_at[0] if observed_at is not None else None),
                "result": None,
                "event_ids": [],
                "has_exact_run_index": False,
            }
        )
    return incomplete, dict(by_cell)


def reduce_inputs(
    canonical_input: Any,
    manifest_metadata: Any,
    *,
    projection_results: Mapping[str, str | None] | None = None,
    current_main_ancestry: Mapping[str, bool | None] | None = None,
    product_results: Mapping[str, str | None] | None = None,
    producer_observed_at: Mapping[str, str] | None = None,
    validate_run_selection: Mapping[str, Mapping[str, Any] | None] | None = None,
    missing_executions: list[Mapping[str, Any]] | None = None,
) -> dict[str, Any]:
    """Reduce canonical series rows without reading another state view.

    ``projection_results`` maps each series ``event_id`` to ``None`` when the
    canonical projection accepted it, or to its exact rejection reason.
    ``current_main_ancestry`` maps each Hermit tree to true, false, or
    unavailable. ``product_results`` supplies the already-decided product
    result for each event. ``producer_observed_at`` supplies producer-owned
    execution times by series event id; raw series ``observed_at`` is not trusted.
    ``validate_run_selection`` cross-checks each validate row with the actual
    validate path and its selection decision; unselected refuses, while a
    missing decision remains visible without erasing direct series evidence.
    ``missing_executions`` must contain only cells listed as selected by a
    validate run whose ``cell_results_verdict`` is satisfied and for which the
    later validation-ledger adapter found no matching validate series row. An
    unselected cell contributes no entry.
    """
    rows, input_incomplete = _normalise_canonical_input(canonical_input)
    manifest_cells = _normalise_manifest(manifest_metadata)
    current_cells = {str(cell["cell"]) for cell in manifest_cells}
    _normalise_product_results(rows, product_results)
    _normalise_producer_observed_at(rows, producer_observed_at)
    missing = _normalise_missing_executions(missing_executions, rows)
    _validate_run_trees(rows, missing)
    event_ids = {str(row["event_id"]) for row in rows}
    hermit_commits = {str(row["tree"]) for row in rows} | {
        str(item["tree"]) for item in missing
    }
    projection, ancestry = _normalise_decisions(
        event_ids,
        hermit_commits,
        projection_results,
        current_main_ancestry,
    )
    selection = _normalise_validate_run_selection(rows, validate_run_selection)
    selected_by_cell = _selected_by(rows, selection, missing)

    missing_incomplete, missing_by_cell = _reduce_missing_executions(missing, ancestry)
    incomplete: list[dict[str, str]] = [*input_incomplete, *missing_incomplete]
    by_cell: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        by_cell[str(row["cell"])].append(row)
    external_incomplete_fields: dict[str, set[str]] = defaultdict(set)
    for entry in incomplete:
        if "cell" in entry:
            external_incomplete_fields[entry["cell"]].add(entry["field"])
    output_cells = []
    for manifest_cell in manifest_cells:
        reduced = _reduce_cell(
            manifest_cell["cell"],
            by_cell.get(manifest_cell["cell"], []),
            projection_results=projection,
            current_main_ancestry=ancestry,
            validate_run_selection=selection,
            incomplete_executions=missing_by_cell.get(manifest_cell["cell"], []),
        )
        item = dict(manifest_cell)
        item.update(
            {key: value for key, value in reduced.items() if key != "incomplete_data"}
        )
        item["selected_by"] = selected_by_cell.get(manifest_cell["cell"], {})
        item["incomplete_fields"] = sorted(
            {
                *item["incomplete_fields"],
                *external_incomplete_fields.get(manifest_cell["cell"], set()),
            }
        )
        output_cells.append(item)
        incomplete.extend(reduced["incomplete_data"])

    for cell in sorted(set(by_cell).difference(current_cells)):
        incomplete.append(
            _entry(
                field="current manifest join",
                input_name="series",
                reason="the historical cell is absent from the current manifest",
                cell=cell,
            )
        )
    for cell in sorted({item["cell"] for item in missing}.difference(current_cells)):
        incomplete.append(
            _entry(
                field="current manifest join",
                input_name="validation ledger",
                reason="the historical cell is absent from the current manifest",
                cell=cell,
            )
        )

    incomplete.sort(key=_incomplete_sort_key)
    state_counts = {
        state: sum(cell["state"] == state for cell in output_cells)
        for state in ("green", "red", "never-measured")
    }
    counts = {
        "in_manifest_cell_count": len(output_cells),
        "green_cell_count": state_counts["green"],
        "red_cell_count": state_counts["red"],
        "never_measured_cell_count": state_counts["never-measured"],
        "state_unavailable_cell_count": sum(
            cell["state"] is None for cell in output_cells
        ),
        "physical_row_count": len(rows),
        "represented_run_count": sum(row["num_runs"] for row in rows),
        "incomplete_data_count": len(incomplete),
    }
    result = {
        "schema_version": SCHEMA_VERSION,
        "counts": counts,
        "cells": output_cells,
        "incomplete_data": incomplete,
    }
    forbidden = {"measured", "measurement_state", "never_measured"}
    if forbidden.intersection(_recursive_keys(result)):
        raise AssertionError("reducer wrote a forbidden stored measurement field")
    return result


def _recursive_keys(value: Any) -> set[str]:
    if isinstance(value, Mapping):
        return set(value).union(*(_recursive_keys(item) for item in value.values()))
    if isinstance(value, list):
        return set().union(*(_recursive_keys(item) for item in value))
    return set()


def render_reduction(value: Mapping[str, Any]) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        + "\n"
    ).encode()


__all__ = [
    "ReductionRefused",
    "SCHEMA_VERSION",
    "reduce_cell",
    "reduce_inputs",
    "render_reduction",
]
