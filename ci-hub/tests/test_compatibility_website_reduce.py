from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from typing import Any

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/reduce.py"
SPEC = importlib.util.spec_from_file_location("compatibility_website_reduce", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
reduce = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = reduce
SPEC.loader.exec_module(reduce)


TREE = "a" * 40
OTHER_TREE = "b" * 40


def canonical_digest(value: Any) -> str:
    encoded = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()
    return hashlib.sha256(encoded).hexdigest()


def manifest_cell(test: str, *, selected_by_full: bool = False) -> dict[str, Any]:
    return {
        "test": test,
        "mode": "verify",
        "backend": "ptrace",
        "selected_by_full": selected_by_full,
    }


def manifest_metadata(*cells: dict[str, Any]) -> dict[str, Any]:
    return {"schema": 1, "cells": list(cells)}


def series_row(
    event_id: str,
    cell: str,
    *,
    producer: str = "pressure-test",
    run_id: str = "run-1",
    tree: str = TREE,
    emitted_at: str = "2026-09-02T00:00:00Z",
    run_index: int = 1,
    num_runs: int = 1,
    result: str | None = "pass",
    ancestry: bool | None = True,
) -> dict[str, Any]:
    return {
        "schema": "stress-series/v3",
        "event_id": event_id,
        "event_type": "series.observation",
        "emitted_at": emitted_at,
        "producer": producer,
        "run_id": run_id,
        "product_result": result,
        "current_main_ancestry": ancestry,
        "series": {
            "cell": cell,
            "tree": tree,
            "run_index": run_index,
            "attempt": 1,
            "num_runs": num_runs,
        },
    }


def canonical_input(rows: list[dict[str, Any]], *, schema: int = 2) -> dict[str, Any]:
    return {
        "schema_version": schema,
        "series": {
            "rows": rows,
            "row_count": len(rows),
            "rows_sha256": canonical_digest(rows),
            "represented_run_count": sum(row["series"]["num_runs"] for row in rows),
        },
    }


def reduce_rows(rows: list[dict[str, Any]], manifest: dict[str, Any]) -> dict[str, Any]:
    return reduce.reduce_inputs(
        canonical_input(rows),
        manifest,
        projection_results={row["event_id"]: None for row in rows},
        current_main_ancestry={
            row["series"]["tree"]: row["current_main_ancestry"] for row in rows
        },
        product_results={row["event_id"]: row["product_result"] for row in rows},
        validate_run_selection={
            row["event_id"]: {"path": "full", "selected": True}
            for row in rows
            if row["producer"] == "validate"
        },
    )


def test_empty_series_derives_never_measured_from_zero_qualifying_rows() -> None:
    value = reduce_rows([], manifest_metadata(manifest_cell("tests/empty")))
    assert value["schema_version"] == 3
    assert value["cells"][0]["state"] == "never-measured"
    assert value["cells"][0]["qualifying_physical_row_count"] == 0
    assert value["counts"] == {
        "in_manifest_cell_count": 1,
        "green_cell_count": 0,
        "red_cell_count": 0,
        "never_measured_cell_count": 1,
        "physical_row_count": 0,
        "represented_run_count": 0,
    }


def test_output_omits_cells_with_qualifying_rows_but_no_result() -> None:
    green = "tests/green/verify/ptrace"
    red = "tests/red/verify/ptrace"
    never = "tests/never/verify/ptrace"
    no_result = "tests/no-result/verify/ptrace"
    manifest = manifest_metadata(
        manifest_cell("tests/green"),
        manifest_cell("tests/red"),
        manifest_cell("tests/never"),
        manifest_cell("tests/no-result"),
    )
    rows = [
        series_row("green", green),
        series_row("red", red, result="product_failure"),
        series_row("no-result", no_result, result=None),
    ]

    value = reduce_rows(rows, manifest)

    assert value["counts"]["in_manifest_cell_count"] == 4
    assert len(value["cells"]) == 3
    assert {row["cell"]: row["state"] for row in value["cells"]} == {
        green: "green",
        red: "red",
        never: "never-measured",
    }
    assert value["counts"]["green_cell_count"] == 1
    assert value["counts"]["red_cell_count"] == 1
    assert value["counts"]["never_measured_cell_count"] == 1
    assert "state_unavailable_cell_count" not in value["counts"]
    assert value["counts"]["in_manifest_cell_count"] - len(value["cells"]) == 1
    assert value["counts"]["green_cell_count"] + value["counts"]["red_cell_count"] == 2
    assert value["projection_audit"] == {
        "deliberate_omissions": [
            {
                "backend": "ptrace",
                "cell": no_result,
                "event_ids": ["no-result"],
                "event_references": [
                    {
                        "event_id": "no-result",
                        "observed_at": "2026-09-02T00:00:00Z",
                        "product_result": None,
                        "run_id": "run-1",
                        "tree": TREE,
                    }
                ],
                "mode": "verify",
                "physical_row_count": 1,
                "qualifying_event_ids": ["no-result"],
                "qualifying_physical_row_count": 1,
                "qualifying_represented_run_count": 1,
                "reason": (
                    "the latest qualifying execution does not establish pass "
                    "or product failure"
                ),
                "represented_run_count": 1,
                "test": "tests/no-result",
            }
        ]
    }
    decisions = [
        *value["cells"],
        *value["projection_audit"]["deliberate_omissions"],
    ]
    assert (
        sum(row["physical_row_count"] for row in decisions)
        == value["counts"]["physical_row_count"]
    )
    assert (
        sum(row["represented_run_count"] for row in decisions)
        == value["counts"]["represented_run_count"]
    )
    assert {row["cell"] for row in value["cells"]}.isdisjoint(
        {row["cell"] for row in value["projection_audit"]["deliberate_omissions"]}
    )


def test_never_measured_depends_on_qualifying_rows_not_physical_presence() -> None:
    cell = "tests/off-main/verify/ptrace"
    row = series_row("off-main", cell, ancestry=False)
    value = reduce_rows([row], manifest_metadata(manifest_cell("tests/off-main")))
    output = value["cells"][0]

    assert output["state"] == "never-measured"
    assert output["physical_row_count"] == 1
    assert output["qualifying_physical_row_count"] == 0
    assert output["event_ids"] == ["off-main"]
    assert output["qualifying_event_ids"] == []


def test_manifest_population_and_selection_by_path_remain_separate() -> None:
    selected = "tests/selected/verify/ptrace"
    unselected = "tests/unselected/verify/ptrace"
    rows = [
        series_row(
            "validate-selected",
            selected,
            producer="validate",
            run_id="validate-run",
        )
    ]
    value = reduce_rows(
        rows,
        manifest_metadata(
            manifest_cell("tests/selected", selected_by_full=False),
            manifest_cell("tests/unselected", selected_by_full=True),
        ),
    )
    by_id = {row["cell"]: row for row in value["cells"]}

    assert value["counts"]["in_manifest_cell_count"] == 2
    assert by_id[selected]["selected_by"]["full"][0]["run_id"] == "validate-run"
    assert by_id[unselected]["state"] == "never-measured"
    assert by_id[unselected]["selected_by"] == {}


def test_latest_producer_timestamp_controls_result_without_a_late_index() -> None:
    cell = "tests/latest/verify/ptrace"
    rows = [
        series_row(
            "old-failure",
            cell,
            result="product_failure",
            emitted_at="2026-09-02T00:00:00Z",
            run_id="pressure",
            run_index=1,
        ),
        series_row(
            "new-pass",
            cell,
            result="pass",
            emitted_at="2026-09-02T00:01:00Z",
            run_id="pressure",
            run_index=2,
        ),
    ]
    value = reduce_rows(rows, manifest_metadata(manifest_cell("tests/latest")))
    output = value["cells"][0]

    assert output["state"] == "green"
    assert output["latest_result"] == "pass"
    assert output["latest_result_time"] == "2026-09-02T00:01:00Z"
    assert output["latest_event_ids"] == ["new-pass"]


def test_tied_unorderable_results_are_omitted_not_given_a_state() -> None:
    cell = "tests/tied/verify/ptrace"
    rows = [
        series_row(
            "left",
            cell,
            result="pass",
            producer="pressure-test",
            run_id="left-run",
        ),
        series_row(
            "right",
            cell,
            result="product_failure",
            producer="pressure-test",
            run_id="right-run",
        ),
    ]
    value = reduce_rows(rows, manifest_metadata(manifest_cell("tests/tied")))

    assert value["cells"] == []
    assert value["counts"]["in_manifest_cell_count"] == 1
    assert value["counts"]["never_measured_cell_count"] == 0
    [omission] = value["projection_audit"]["deliberate_omissions"]
    assert omission["cell"] == cell
    assert omission["reason"] == (
        "the latest qualifying executions do not establish one unique latest "
        "execution"
    )
    assert omission["event_ids"] == ["left", "right"]
    assert omission["qualifying_event_ids"] == ["left", "right"]
    assert omission["event_references"] == [
        {
            "event_id": "left",
            "observed_at": "2026-09-02T00:00:00Z",
            "product_result": "pass",
            "run_id": "left-run",
            "tree": TREE,
        },
        {
            "event_id": "right",
            "observed_at": "2026-09-02T00:00:00Z",
            "product_result": "product_failure",
            "run_id": "right-run",
            "tree": TREE,
        },
    ]


@pytest.mark.parametrize(
    ("field", "replacement", "message"),
    [
        ("projection_results", {}, "cover exactly"),
        ("product_results", {}, "cover exactly"),
        ("current_main_ancestry", {}, "cover exactly"),
        ("validate_run_selection", {}, "cover exactly"),
    ],
)
def test_authority_maps_must_cover_exactly_the_persisted_rows(
    field: str, replacement: dict[str, Any], message: str
) -> None:
    cell = "tests/exact/verify/ptrace"
    rows = [series_row("event", cell, producer="validate")]
    arguments = {
        "projection_results": {"event": None},
        "current_main_ancestry": {TREE: True},
        "product_results": {"event": "pass"},
        "validate_run_selection": {"event": {"path": "full", "selected": True}},
    }
    arguments[field] = replacement

    with pytest.raises(reduce.ReductionRefused, match=message):
        reduce.reduce_inputs(
            canonical_input(rows),
            manifest_metadata(manifest_cell("tests/exact")),
            **arguments,
        )


def test_non_manifest_series_rows_remain_outside_the_manifest_partition() -> None:
    cell = "tests/unknown/verify/ptrace"
    value = reduce_rows(
        [series_row("event", cell)],
        manifest_metadata(manifest_cell("tests/known")),
    )

    assert value["counts"]["physical_row_count"] == 1
    assert value["cells"][0]["cell"] == "tests/known/verify/ptrace"
    assert value["cells"][0]["state"] == "never-measured"
    assert value["projection_audit"]["deliberate_omissions"] == []


def test_schema_v1_consumer_input_fails_closed() -> None:
    with pytest.raises(reduce.ReductionRefused, match="schema_version must be 2"):
        reduce.reduce_inputs(
            canonical_input([], schema=1),
            manifest_metadata(manifest_cell("tests/old")),
            projection_results={},
            current_main_ancestry={},
            product_results={},
            validate_run_selection={},
        )


def test_result_contains_no_stored_measurement_or_unavailable_state() -> None:
    value = reduce_rows([], manifest_metadata(manifest_cell("tests/empty")))
    keys = reduce._recursive_keys(value)
    assert "measured" not in keys
    assert "measurement_state" not in keys
    assert "never_measured" not in keys
    assert "state_unavailable_cell_count" not in keys
    assert json.loads(reduce.render_reduction(value)) == value


def test_product_and_ancestry_copies_must_match_the_authorities() -> None:
    cell = "tests/tampered/verify/ptrace"
    row = series_row("event", cell)
    arguments = {
        "projection_results": {"event": None},
        "current_main_ancestry": {TREE: True},
        "product_results": {"event": "product_failure"},
        "validate_run_selection": {},
    }
    with pytest.raises(reduce.ReductionRefused, match="product_results"):
        reduce.reduce_inputs(
            canonical_input([row]),
            manifest_metadata(manifest_cell("tests/tampered")),
            **arguments,
        )

    tampered = copy.deepcopy(row)
    tampered["current_main_ancestry"] = None
    arguments["product_results"] = {"event": "pass"}
    with pytest.raises(reduce.ReductionRefused, match="ancestry"):
        reduce.reduce_inputs(
            canonical_input([tampered]),
            manifest_metadata(manifest_cell("tests/tampered")),
            **arguments,
        )
