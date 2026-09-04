#!/usr/bin/env python3
"""Reduce Rust-projected compatibility evidence to current manifest cells.

Rust owns manifest, validation-history, and series schema interpretation. This
module accepts only the version-2 projection envelope, verifies that every
authority decision covers exactly the persisted rows, and derives cell results.
A cell is never-measured only when it has zero qualifying rows.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections import defaultdict
from collections.abc import Mapping, Sequence
from datetime import datetime
from typing import Any


SCHEMA_VERSION = 3
_CANONICAL_INPUT_SCHEMA = 2
_MANIFEST_METADATA_SCHEMA = 1
_PRODUCERS = frozenset({"validate", "pressure-test", "hermit-repeat"})
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_AMBIGUOUS_LATEST_REASON = (
    "the latest qualifying executions do not establish one unique latest execution"
)
_NO_RESULT_REASON = (
    "the latest qualifying execution does not establish pass or product failure"
)


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


def _parse_time(value: Any, label: str) -> tuple[datetime, str]:
    raw = _string(value, label)
    try:
        parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError as error:
        raise ReductionRefused(f"{label} must be an RFC3339 timestamp") from error
    if parsed.tzinfo is None:
        raise ReductionRefused(f"{label} must include a timezone")
    return parsed, raw


def _digest(value: Any) -> str:
    encoded = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()
    return hashlib.sha256(encoded).hexdigest()


def _sha256(value: Any, label: str) -> str:
    digest = _string(value, label)
    if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise ReductionRefused(f"{label} must be lowercase 64-hex")
    return digest


def _commit(value: Any, label: str) -> str:
    commit = _string(value, label)
    if _SHA_RE.fullmatch(commit) is None:
        raise ReductionRefused(f"{label} must be a lowercase 40-hex commit")
    return commit


def _canonical_cell(test: str, mode: str, backend: str) -> str:
    return f"{test}/{mode}/{backend}"


def _manifest_cell(value: Any, index: int) -> dict[str, str]:
    cell = _object(value, f"manifest cells[{index}]")
    forbidden = {"measured", "measurement_state", "never_measured", "state"}
    present = sorted(forbidden.intersection(cell))
    if present:
        raise ReductionRefused(
            f"manifest cells[{index}] contains ledger-derived field(s): "
            + ", ".join(present)
        )
    test = _string(cell.get("test"), f"manifest cells[{index}].test")
    mode = _string(cell.get("mode"), f"manifest cells[{index}].mode")
    backend = _string(cell.get("backend"), f"manifest cells[{index}].backend")
    return {
        "cell": _canonical_cell(test, mode, backend),
        "test": test,
        "mode": mode,
        "backend": backend,
    }


def _normalise_manifest(value: Any) -> list[dict[str, str]]:
    manifest = _object(value, "manifest metadata")
    schema = _integer(manifest.get("schema"), "manifest metadata schema", minimum=1)
    if schema != _MANIFEST_METADATA_SCHEMA:
        raise ReductionRefused(
            f"manifest metadata schema must be {_MANIFEST_METADATA_SCHEMA}"
        )
    cells = [
        _manifest_cell(raw, index)
        for index, raw in enumerate(_array(manifest.get("cells"), "manifest cells"))
    ]
    cells.sort(key=lambda cell: (cell["test"], cell["mode"], cell["backend"]))
    identities = [cell["cell"] for cell in cells]
    if len(identities) != len(set(identities)):
        raise ReductionRefused("manifest metadata repeats a cell identity")
    return cells


def _normalise_series_row(value: Any, index: int) -> dict[str, Any]:
    row = _object(value, f"series rows[{index}]")
    label = f"series rows[{index}]"
    if _string(row.get("schema"), f"{label}.schema") != "stress-series/v3":
        raise ReductionRefused(f"{label}.schema must be stress-series/v3")
    producer = _string(row.get("producer"), f"{label}.producer")
    if producer not in _PRODUCERS:
        raise ReductionRefused(f"{label}.producer is unknown: {producer!r}")
    payload = _object(row.get("series"), f"{label}.series")
    tree = _commit(payload.get("tree"), f"{label}.series.tree")
    result = row.get("product_result")
    if result is not None and result not in {"pass", "product_failure"}:
        raise ReductionRefused(
            f"{label}.product_result must be pass, product_failure, or null"
        )
    ancestry = row.get("current_main_ancestry")
    if ancestry is not None and not isinstance(ancestry, bool):
        raise ReductionRefused(f"{label}.current_main_ancestry must be bool or null")
    return {
        "event_id": _string(row.get("event_id"), f"{label}.event_id"),
        "emitted_at": _parse_time(row.get("emitted_at"), f"{label}.emitted_at"),
        "producer": producer,
        "run_id": _string(row.get("run_id"), f"{label}.run_id"),
        "cell": _string(payload.get("cell"), f"{label}.series.cell"),
        "tree": tree,
        "run_index": _integer(
            payload.get("run_index"), f"{label}.series.run_index", minimum=0
        ),
        "attempt": (
            None
            if payload.get("attempt") is None
            else _integer(payload.get("attempt"), f"{label}.series.attempt", minimum=1)
        ),
        "num_runs": _integer(
            payload.get("num_runs"), f"{label}.series.num_runs", minimum=1
        ),
        "product_result": result,
        "current_main_ancestry": ancestry,
    }


def _normalise_canonical_input(value: Any) -> list[dict[str, Any]]:
    canonical = _object(value, "canonical input")
    if set(canonical) != {"schema_version", "series"}:
        raise ReductionRefused(
            "canonical input must contain only schema_version and series"
        )
    if (
        _integer(
            canonical.get("schema_version"),
            "canonical input schema_version",
            minimum=1,
        )
        != _CANONICAL_INPUT_SCHEMA
    ):
        raise ReductionRefused(
            f"canonical input schema_version must be {_CANONICAL_INPUT_SCHEMA}"
        )
    series = _object(canonical.get("series"), "canonical input series")
    if set(series) != {
        "row_count",
        "represented_run_count",
        "rows",
        "rows_sha256",
    }:
        raise ReductionRefused("canonical input series has an unsupported shape")
    raw_rows = _array(series.get("rows"), "canonical input series.rows")
    if _sha256(
        series.get("rows_sha256"), "canonical input series.rows_sha256"
    ) != _digest(raw_rows):
        raise ReductionRefused("canonical input series.rows_sha256 disagrees with rows")
    rows = [_normalise_series_row(row, index) for index, row in enumerate(raw_rows)]
    event_ids = [row["event_id"] for row in rows]
    if len(event_ids) != len(set(event_ids)):
        raise ReductionRefused("canonical series repeats an event_id")
    if _integer(series.get("row_count"), "canonical input series.row_count") != len(
        rows
    ):
        raise ReductionRefused("canonical input series.row_count disagrees with rows")
    represented = sum(row["num_runs"] for row in rows)
    if (
        _integer(
            series.get("represented_run_count"),
            "canonical input series.represented_run_count",
        )
        != represented
    ):
        raise ReductionRefused(
            "canonical input series.represented_run_count disagrees with num_runs"
        )
    return rows


def _exact_event_map(
    value: Any,
    rows: Sequence[Mapping[str, Any]],
    label: str,
) -> dict[str, Any]:
    result = dict(_object(value, label))
    expected = {str(row["event_id"]) for row in rows}
    if set(result) != expected:
        raise ReductionRefused(f"{label} must cover exactly the persisted series rows")
    return result


def _normalise_authorities(
    rows: Sequence[dict[str, Any]],
    *,
    projection_results: Mapping[str, str | None],
    current_main_ancestry: Mapping[str, bool | None],
    product_results: Mapping[str, str | None],
    validate_run_selection: Mapping[str, Mapping[str, Any]],
) -> tuple[
    dict[str, None],
    dict[str, bool | None],
    dict[str, str | None],
    dict[str, dict[str, Any]],
]:
    projection = _exact_event_map(projection_results, rows, "projection_results")
    for event_id, reason in projection.items():
        if reason is not None:
            raise ReductionRefused(
                f"persisted series event {event_id!r} carries an exclusion reason"
            )

    products = _exact_event_map(product_results, rows, "product_results")
    by_event = {str(row["event_id"]): row for row in rows}
    for event_id, result in products.items():
        if result is not None and result not in {"pass", "product_failure"}:
            raise ReductionRefused(
                f"product_results[{event_id!r}] must be pass, product_failure, or null"
            )
        if by_event[event_id]["product_result"] != result:
            raise ReductionRefused(
                f"product_results[{event_id!r}] disagrees with its persisted series row"
            )

    ancestry = dict(_object(current_main_ancestry, "current_main_ancestry"))
    trees = {str(row["tree"]) for row in rows}
    if set(ancestry) != trees:
        raise ReductionRefused(
            "current_main_ancestry must cover exactly the persisted Hermit commits"
        )
    for tree, decision in ancestry.items():
        _commit(tree, "current_main_ancestry key")
        if decision is not None and not isinstance(decision, bool):
            raise ReductionRefused("current_main_ancestry values must be bool or null")
    for row in rows:
        raw = row["current_main_ancestry"] if "current_main_ancestry" in row else None
        if raw != ancestry[row["tree"]]:
            raise ReductionRefused(
                f"series event {row['event_id']!r} ancestry disagrees with its authority"
            )

    selection = dict(_object(validate_run_selection, "validate_run_selection"))
    validate_events = {
        str(row["event_id"]) for row in rows if row["producer"] == "validate"
    }
    if set(selection) != validate_events:
        raise ReductionRefused(
            "validate_run_selection must cover exactly the persisted validate rows"
        )
    normalised_selection = {}
    for event_id, value in selection.items():
        item = _object(value, f"validate_run_selection[{event_id!r}]")
        if set(item) != {"path", "selected"}:
            raise ReductionRefused(
                f"validate_run_selection[{event_id!r}] must contain only path and selected"
            )
        path = _string(item.get("path"), f"validate_run_selection[{event_id!r}].path")
        if item.get("selected") is not True:
            raise ReductionRefused(
                f"persisted validate series event {event_id!r} was not selected by {path}"
            )
        normalised_selection[event_id] = {"path": path, "selected": True}

    return (
        {event_id: None for event_id in projection},
        ancestry,
        products,
        normalised_selection,
    )


def _selected_by(
    rows: Sequence[Mapping[str, Any]],
    selections: Mapping[str, Mapping[str, Any]],
) -> dict[str, dict[str, list[dict[str, Any]]]]:
    grouped: dict[tuple[str, str, str, str], list[tuple[str, tuple[datetime, str]]]] = (
        defaultdict(list)
    )
    for row in rows:
        if row["producer"] != "validate":
            continue
        decision = selections[str(row["event_id"])]
        grouped[
            (
                str(row["cell"]),
                str(decision["path"]),
                str(row["run_id"]),
                str(row["tree"]),
            )
        ].append((str(row["event_id"]), row["emitted_at"]))

    by_cell: dict[str, dict[str, list[dict[str, Any]]]] = defaultdict(
        lambda: defaultdict(list)
    )
    for (cell, path, run_id, tree), values in sorted(grouped.items()):
        times = {value[0] for _event_id, value in values}
        if len(times) != 1:
            raise ReductionRefused(
                f"validate run {run_id!r} selected by {path} has conflicting emitted_at values"
            )
        by_cell[cell][path].append(
            {
                "run_id": run_id,
                "tree": tree,
                "observed_at": values[0][1][1],
                "event_ids": sorted(event_id for event_id, _value in values),
            }
        )
    return {
        cell: {path: facts for path, facts in sorted(paths.items())}
        for cell, paths in sorted(by_cell.items())
    }


def _qualifying_rows(
    rows: Sequence[Mapping[str, Any]],
    ancestry: Mapping[str, bool | None],
    selection: Mapping[str, Mapping[str, Any]],
) -> list[Mapping[str, Any]]:
    return [
        row
        for row in rows
        if ancestry[str(row["tree"])] is True
        and (
            row["producer"] != "validate"
            or selection[str(row["event_id"])]["selected"] is True
        )
    ]


def _execution_key(row: Mapping[str, Any]) -> tuple[Any, ...]:
    if row["producer"] == "validate":
        return row["producer"], row["run_id"], row["cell"]
    return row["producer"], row["run_id"], row["cell"], row["run_index"]


def _execution(row_group: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    trees = {str(row["tree"]) for row in row_group}
    if len(trees) != 1:
        raise ReductionRefused("one logical execution spans multiple Hermit commits")
    times = {row["emitted_at"][0] for row in row_group}
    if len(times) != 1:
        raise ReductionRefused(
            "one logical execution has conflicting emitted_at values"
        )
    results = [row["product_result"] for row in row_group]
    if "pass" in results:
        result = "pass"
    elif results and all(value == "product_failure" for value in results):
        result = "product_failure"
    else:
        result = None
    return {
        "producer": str(row_group[0]["producer"]),
        "run_id": str(row_group[0]["run_id"]),
        "run_index": (
            None
            if row_group[0]["producer"] == "validate"
            else int(row_group[0]["run_index"])
        ),
        "tree": next(iter(trees)),
        "observed_at": row_group[0]["emitted_at"],
        "result": result,
        "event_ids": sorted(str(row["event_id"]) for row in row_group),
        "has_exact_run_index": all(int(row["num_runs"]) == 1 for row in row_group),
    }


def _latest_execution(
    executions: Sequence[Mapping[str, Any]],
) -> Mapping[str, Any] | None:
    if not executions:
        return None
    newest = max(execution["observed_at"][0] for execution in executions)
    candidates = [
        execution for execution in executions if execution["observed_at"][0] == newest
    ]
    if len(candidates) == 1:
        return candidates[0]
    producers = {execution["producer"] for execution in candidates}
    run_ids = {execution["run_id"] for execution in candidates}
    if (
        len(producers) == 1
        and next(iter(producers)) in {"pressure-test", "hermit-repeat"}
        and len(run_ids) == 1
        and all(execution["has_exact_run_index"] for execution in candidates)
    ):
        return max(candidates, key=lambda execution: execution["run_index"])
    return None


def _deliberate_omission(
    manifest_cell: Mapping[str, str],
    rows: Sequence[Mapping[str, Any]],
    qualifying: Sequence[Mapping[str, Any]],
    reason: str,
) -> dict[str, Any]:
    event_references = sorted(
        (
            {
                "event_id": str(row["event_id"]),
                "run_id": str(row["run_id"]),
                "tree": str(row["tree"]),
                "observed_at": row["emitted_at"][1],
                "product_result": row["product_result"],
            }
            for row in rows
        ),
        key=lambda reference: reference["event_id"],
    )
    return {
        **manifest_cell,
        "physical_row_count": len(rows),
        "represented_run_count": sum(int(row["num_runs"]) for row in rows),
        "qualifying_physical_row_count": len(qualifying),
        "qualifying_represented_run_count": sum(
            int(row["num_runs"]) for row in qualifying
        ),
        "event_ids": [reference["event_id"] for reference in event_references],
        "qualifying_event_ids": sorted(str(row["event_id"]) for row in qualifying),
        "reason": reason,
        "event_references": event_references,
    }


def _reduce_cell(
    manifest_cell: Mapping[str, str],
    rows: Sequence[Mapping[str, Any]],
    ancestry: Mapping[str, bool | None],
    selection: Mapping[str, Mapping[str, Any]],
    selected_by: Mapping[str, Mapping[str, list[dict[str, Any]]]],
) -> tuple[dict[str, Any] | None, dict[str, Any] | None]:
    identity = manifest_cell["cell"]
    ordered = sorted(
        rows,
        key=lambda row: (
            str(row["producer"]),
            str(row["run_id"]),
            int(row["run_index"]),
            int(row["attempt"] if row["attempt"] is not None else 1),
            str(row["event_id"]),
        ),
    )
    qualifying = _qualifying_rows(ordered, ancestry, selection)
    item: dict[str, Any] = dict(manifest_cell)
    item.update(
        physical_row_count=len(ordered),
        represented_run_count=sum(int(row["num_runs"]) for row in ordered),
        qualifying_physical_row_count=len(qualifying),
        qualifying_represented_run_count=sum(
            int(row["num_runs"]) for row in qualifying
        ),
        event_ids=sorted(str(row["event_id"]) for row in ordered),
        qualifying_event_ids=sorted(str(row["event_id"]) for row in qualifying),
        selected_by=selected_by.get(identity, {}),
    )
    if not qualifying:
        item["state"] = "never-measured"
        return item, None

    groups: dict[tuple[Any, ...], list[Mapping[str, Any]]] = defaultdict(list)
    for row in qualifying:
        groups[_execution_key(row)].append(row)
    latest = _latest_execution([_execution(groups[key]) for key in sorted(groups)])
    if latest is None:
        return None, _deliberate_omission(
            manifest_cell,
            ordered,
            qualifying,
            _AMBIGUOUS_LATEST_REASON,
        )
    if latest["result"] is None:
        return None, _deliberate_omission(
            manifest_cell,
            ordered,
            qualifying,
            _NO_RESULT_REASON,
        )
    result = str(latest["result"])
    item.update(
        state="green" if result == "pass" else "red",
        latest_result=result,
        latest_result_tree=latest["tree"],
        latest_result_time=latest["observed_at"][1],
        latest_event_ids=latest["event_ids"],
    )
    return item, None


def reduce_cell(
    cell: str,
    rows: Sequence[Mapping[str, Any]],
    *,
    projection_results: Mapping[str, str | None],
    current_main_ancestry: Mapping[str, bool | None],
    validate_run_selection: Mapping[str, Mapping[str, Any]],
    product_results: Mapping[str, str | None] | None = None,
) -> dict[str, Any] | None:
    """Reduce one cell from an already-projected version-2 row population."""
    normalised_rows = [
        _normalise_series_row(value, index) for index, value in enumerate(rows)
    ]
    if any(row["cell"] != cell for row in normalised_rows):
        raise ReductionRefused("reduce_cell rows must all name the requested cell")
    products = (
        {str(row["event_id"]): row["product_result"] for row in normalised_rows}
        if product_results is None
        else product_results
    )
    _projection, ancestry, _products, selection = _normalise_authorities(
        normalised_rows,
        projection_results=projection_results,
        current_main_ancestry=current_main_ancestry,
        product_results=products,
        validate_run_selection=validate_run_selection,
    )
    manifest_cell = {
        "cell": cell,
        "test": cell.rsplit("/", 2)[0],
        "mode": cell.rsplit("/", 2)[1],
        "backend": cell.rsplit("/", 2)[2],
    }
    result, _omission = _reduce_cell(
        manifest_cell,
        normalised_rows,
        ancestry,
        selection,
        _selected_by(normalised_rows, selection),
    )
    return result


def reduce_inputs(
    canonical_input: Any,
    manifest_metadata: Any,
    *,
    projection_results: Mapping[str, str | None],
    current_main_ancestry: Mapping[str, bool | None],
    product_results: Mapping[str, str | None],
    validate_run_selection: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    """Reduce only evidence admitted by the Rust version-2 projection."""
    rows = _normalise_canonical_input(canonical_input)
    manifest_cells = _normalise_manifest(manifest_metadata)
    manifest_identities = {cell["cell"] for cell in manifest_cells}
    _projection, ancestry, _products, selection = _normalise_authorities(
        rows,
        projection_results=projection_results,
        current_main_ancestry=current_main_ancestry,
        product_results=product_results,
        validate_run_selection=validate_run_selection,
    )
    selected_by = _selected_by(rows, selection)
    by_cell: dict[str, list[Mapping[str, Any]]] = defaultdict(list)
    for row in rows:
        by_cell[str(row["cell"])].append(row)

    cells = []
    deliberate_omissions = []
    for manifest_cell in manifest_cells:
        reduced, omission = _reduce_cell(
            manifest_cell,
            by_cell.get(manifest_cell["cell"], []),
            ancestry,
            selection,
            selected_by,
        )
        if reduced is not None:
            cells.append(reduced)
        if omission is not None:
            deliberate_omissions.append(omission)

    state_counts = {
        state: sum(cell["state"] == state for cell in cells)
        for state in ("green", "red", "never-measured")
    }
    counts = {
        "in_manifest_cell_count": len(manifest_cells),
        "green_cell_count": state_counts["green"],
        "red_cell_count": state_counts["red"],
        "never_measured_cell_count": state_counts["never-measured"],
        "physical_row_count": len(rows),
        "represented_run_count": sum(int(row["num_runs"]) for row in rows),
    }
    if len(cells) != sum(state_counts.values()):
        raise AssertionError("cell rows and state counts disagree")
    cell_identities = {str(cell["cell"]) for cell in cells}
    omission_identities = {str(omission["cell"]) for omission in deliberate_omissions}
    if (
        cell_identities.intersection(omission_identities)
        or cell_identities.union(omission_identities) != manifest_identities
    ):
        raise AssertionError(
            "cell rows and deliberate omissions do not partition the manifest"
        )
    decisions = [*cells, *deliberate_omissions]
    manifest_rows = [row for row in rows if str(row["cell"]) in manifest_identities]
    if sum(int(item["physical_row_count"]) for item in decisions) != len(manifest_rows):
        raise AssertionError(
            "cell rows and deliberate omissions do not partition manifest physical rows"
        )
    if sum(int(item["represented_run_count"]) for item in decisions) != sum(
        int(row["num_runs"]) for row in manifest_rows
    ):
        raise AssertionError(
            "cell rows and deliberate omissions do not partition manifest represented runs"
        )
    if any(
        int(omission["qualifying_physical_row_count"]) <= 0
        or int(omission["qualifying_represented_run_count"]) <= 0
        for omission in deliberate_omissions
    ):
        raise AssertionError("a deliberate omission has no qualifying evidence")
    result = {
        "schema_version": SCHEMA_VERSION,
        "counts": counts,
        "cells": cells,
        "projection_audit": {
            "deliberate_omissions": deliberate_omissions,
        },
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
