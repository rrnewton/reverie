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
CELL = "tests/hello/verify/ptrace"
OTHER_CELL = "tests/other/verify/ptrace"


def canonical_digest(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def manifest_cell(
    test: str = "tests/hello",
    mode: str = "verify",
    backend: str = "ptrace",
    **changes: Any,
) -> dict[str, Any]:
    value = {
        "test": test,
        "mode": mode,
        "backend": backend,
    }
    value.update(changes)
    return value


def manifest_metadata(*cells: dict[str, Any]) -> dict[str, Any]:
    cells = cells or (manifest_cell(),)
    return {
        "schema": 1,
        "cells": list(cells),
    }


def series_row(
    event_id: str,
    *,
    producer: str = "validate",
    run_id: str = "run-1",
    cell: str = CELL,
    tree: str = TREE,
    run_index: int = 0,
    attempt: int | None = None,
    outcome: str = "passed",
    result: str | None = "pass",
    failure_class: str | None = None,
    num_runs: int = 1,
    last_run_index: int | None = None,
    observed_at: str | None = None,
    **extra: Any,
) -> dict[str, Any]:
    payload = {
        "cell": cell,
        "tree": tree,
        "run_index": run_index,
        "outcome": outcome,
        "result": result,
        "failure_class": failure_class,
        "num_runs": num_runs,
    }
    if attempt is not None:
        payload["attempt"] = attempt
    if last_run_index is not None:
        payload["last_run_index"] = last_run_index
    row = {
        "schema": "stress-series/v3",
        "event_id": event_id,
        "event_type": "series.observation",
        "emitted_at": "2026-08-30T01:00:00Z",
        "producer": producer,
        "run_id": run_id,
        "series": payload,
    }
    if observed_at is not None:
        row["observed_at"] = observed_at
    row.update(extra)
    return row


def canonical_input(
    rows: list[dict[str, Any]],
    *,
    incomplete_data: list[dict[str, str]] | None = None,
) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "series": {
            "rows": rows,
            "row_count": len(rows),
            "rows_sha256": canonical_digest(rows),
            "represented_run_count": sum(
                row["series"].get("num_runs", 1) for row in rows
            ),
        },
        "incomplete_data": incomplete_data or [],
    }


def authorities(
    rows: list[dict[str, Any]],
) -> tuple[dict[str, str | None], dict[str, bool | None]]:
    return (
        {row["event_id"]: None for row in rows},
        {row["series"]["tree"]: True for row in rows},
    )


def reduced(
    rows: list[dict[str, Any]],
    *,
    manifest: dict[str, Any] | None = None,
    projection: dict[str, str | None] | None = None,
    ancestry: dict[str, bool | None] | None = None,
    product_results: dict[str, str | None] | None = None,
    producer_observed_at: dict[str, str] | None = None,
    validate_run_selection: dict[str, dict[str, Any] | None] | None = None,
    missing_executions: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    default_authorities = projection is None and ancestry is None
    if default_authorities:
        projection, ancestry = authorities(rows)
    if product_results is None:
        product_results = {
            row["event_id"]: (
                "pass"
                if row["series"].get("result") == "pass"
                else (
                    "product_failure"
                    if row["series"].get("failure_class") == "product_failure"
                    else None
                )
            )
            for row in rows
        }
    if producer_observed_at is None:
        producer_observed_at = {
            row["event_id"]: row["observed_at"]
            for row in rows
            if row.get("observed_at") is not None
        }
    if validate_run_selection is None:
        validate_run_selection = {
            row["event_id"]: {"path": "full", "selected": True}
            for row in rows
            if row["producer"] == "validate"
        }
    missing_executions = missing_executions or []
    if default_authorities and ancestry is not None:
        ancestry = dict(ancestry)
        for item in missing_executions:
            if item.get("tree") is not None:
                ancestry.setdefault(item["tree"], True)
    return reduce.reduce_inputs(
        canonical_input(rows),
        manifest or manifest_metadata(),
        projection_results=projection,
        current_main_ancestry=ancestry,
        product_results=product_results,
        producer_observed_at=producer_observed_at,
        validate_run_selection=validate_run_selection,
        missing_executions=missing_executions,
    )


def only_cell(value: dict[str, Any]) -> dict[str, Any]:
    assert len(value["cells"]) == 1
    return value["cells"][0]


def product_failure(event_id: str, **changes: Any) -> dict[str, Any]:
    values = {
        "outcome": "diverged",
        "result": "determinism-failure",
        "failure_class": "product_failure",
        **changes,
    }
    return series_row(event_id, **values)


def no_result(event_id: str, **changes: Any) -> dict[str, Any]:
    values = {
        "outcome": "no_result",
        "result": None,
        "failure_class": "no_result",
        **changes,
    }
    return series_row(event_id, **values)


def test_empty_series_derives_never_measured_from_no_qualifying_rows() -> None:
    value = reduced([])
    cell = only_cell(value)

    assert cell["state"] == "never-measured"
    assert cell["physical_row_count"] == 0
    assert cell["represented_run_count"] == 0
    assert value["counts"]["never_measured_cell_count"] == 1
    assert value["counts"]["in_manifest_cell_count"] == 1
    assert "manifest_cell_count" not in value["counts"]
    assert "measured" not in reduce._recursive_keys(value)
    assert "measurement_state" not in reduce._recursive_keys(value)


def test_authoritative_exclusions_are_nonqualifying_but_missing_decisions_are_unknown() -> (
    None
):
    rows = [series_row("event-1")]

    rejected = reduced(
        rows,
        projection={"event-1": "projection rejected the row"},
        ancestry={TREE: True},
    )
    assert only_cell(rejected)["state"] == "never-measured"
    assert "qualifying row" in only_cell(rejected)["incomplete_fields"]

    historical = reduced(
        rows,
        projection={"event-1": None},
        ancestry={TREE: False},
    )
    assert only_cell(historical)["state"] == "never-measured"
    assert "current-main ancestry" in only_cell(historical)["incomplete_fields"]
    assert any(
        item.get("event_id") == "event-1" and "not an ancestor" in item["reason"]
        for item in historical["incomplete_data"]
    )

    missing_projection = reduced(rows, projection={}, ancestry={TREE: True})
    assert only_cell(missing_projection)["state"] is None
    assert (
        "projection validation reason"
        in only_cell(missing_projection)["incomplete_fields"]
    )

    missing_ancestry = reduced(
        rows,
        projection={"event-1": None},
        ancestry={TREE: None},
    )
    assert only_cell(missing_ancestry)["state"] is None
    assert "current-main ancestry" in only_cell(missing_ancestry)["incomplete_fields"]


@pytest.mark.parametrize(
    ("projection_value", "ancestry_value", "expected_state"),
    [
        ("accepted", True, "green"),
        ("accepted", False, "never-measured"),
        ("accepted", "missing", None),
        ("rejected", True, "never-measured"),
        ("rejected", False, "never-measured"),
        ("rejected", "missing", "never-measured"),
        ("missing", True, None),
        ("missing", False, "never-measured"),
        ("missing", "missing", None),
    ],
)
def test_projection_and_ancestry_qualification_is_commutative(
    projection_value: str,
    ancestry_value: bool | str,
    expected_state: str | None,
) -> None:
    rows = [series_row("event-1")]
    projection = (
        {}
        if projection_value == "missing"
        else {
            "event-1": (
                None if projection_value == "accepted" else "projection rejected"
            )
        }
    )
    ancestry = {} if ancestry_value == "missing" else {TREE: ancestry_value}

    cell = only_cell(reduced(rows, projection=projection, ancestry=ancestry))
    assert cell["state"] == expected_state
    if projection_value == "missing":
        assert "projection validation reason" in cell["incomplete_fields"]
    if ancestry_value == "missing":
        assert "current-main ancestry" in cell["incomplete_fields"]


def test_one_sha_ancestry_decision_applies_to_every_row_at_that_sha() -> None:
    rows = [
        series_row("event-1", producer="pressure-test", run_id="run-1"),
        series_row("event-2", producer="pressure-test", run_id="run-2"),
    ]
    projection = {"event-1": None, "event-2": None}

    excluded = only_cell(reduced(rows, projection=projection, ancestry={TREE: False}))
    assert excluded["state"] == "never-measured"
    assert excluded["qualifying_event_ids"] == []

    accepted = only_cell(reduced(rows, projection=projection, ancestry={TREE: True}))
    assert accepted["qualifying_event_ids"] == ["event-1", "event-2"]


def test_validate_retries_reduce_one_logical_execution() -> None:
    rows = [
        product_failure("event-1", run_index=1),
        series_row("event-2", run_index=2),
    ]
    cell = only_cell(reduced(rows))

    assert cell["state"] == "green"
    assert cell["latest_result"] == "pass"
    assert cell["physical_row_count"] == 2
    assert cell["represented_run_count"] == 2
    assert cell["event_ids"] == ["event-1", "event-2"]
    assert cell["qualifying_event_ids"] == ["event-1", "event-2"]
    assert cell["latest_event_ids"] == ["event-1", "event-2"]

    failures = [
        product_failure("event-1", run_index=1),
        product_failure("event-2", run_index=2),
    ]
    assert only_cell(reduced(failures))["state"] == "red"


def test_pressure_test_retries_and_later_run_index_are_distinct() -> None:
    rows = [
        product_failure("event-1", producer="pressure-test", run_index=0, attempt=1),
        series_row("event-2", producer="pressure-test", run_index=0, attempt=2),
        product_failure("event-3", producer="pressure-test", run_index=1, attempt=1),
    ]
    cell = only_cell(reduced(rows))

    assert cell["state"] == "red"
    assert cell["latest_result"] == "product_failure"

    for older_time, newer_time in (
        ("2026-08-30T01:00:00Z", "2026-08-30T01:00:00Z"),
        ("2026-08-30T03:00:00Z", "2026-08-30T02:00:00Z"),
    ):
        ordered_by_run_index = [
            series_row(
                "pass",
                producer="pressure-test",
                run_index=0,
                observed_at=older_time,
            ),
            product_failure(
                "fail",
                producer="pressure-test",
                run_index=1,
                observed_at=newer_time,
            ),
        ]
        assert only_cell(reduced(ordered_by_run_index))["state"] == "red"


def test_hermit_repeat_uses_attempt_as_retry_ordinal() -> None:
    rows = [
        product_failure("event-1", producer="hermit-repeat", run_index=7, attempt=1),
        series_row("event-2", producer="hermit-repeat", run_index=7, attempt=2),
    ]

    assert only_cell(reduced(rows))["state"] == "green"


def test_cross_run_order_requires_unique_producer_owned_observed_at() -> None:
    unordered = [
        series_row("pass", producer="pressure-test", run_id="older"),
        product_failure("fail", producer="pressure-test", run_id="newer"),
    ]
    forward = reduced(unordered)
    reverse = reduced(list(reversed(unordered)))

    assert only_cell(forward)["state"] is None
    assert "latest-run order" in only_cell(forward)["incomplete_fields"]
    assert reduce.render_reduction(forward) == reduce.render_reduction(reverse)

    ordered = [
        series_row(
            "pass",
            producer="pressure-test",
            run_id="older",
            observed_at="2026-08-30T01:00:00Z",
        ),
        product_failure(
            "fail",
            producer="pressure-test",
            run_id="newer",
            observed_at="2026-08-30T02:00:00Z",
        ),
    ]
    assert only_cell(reduced(ordered))["state"] == "red"

    tied = copy.deepcopy(ordered)
    tied[1]["observed_at"] = tied[0]["observed_at"]
    assert only_cell(reduced(tied))["state"] is None

    offsets = [
        series_row(
            "utc-pass",
            producer="pressure-test",
            run_id="utc-run",
            observed_at="2026-08-30T08:00:00Z",
        ),
        product_failure(
            "offset-failure",
            producer="pressure-test",
            run_id="offset-run",
            run_index=1,
            observed_at="2026-08-30T01:30:00-07:00",
        ),
    ]
    assert only_cell(reduced(offsets))["state"] == "red"

    same_execution = [
        product_failure("retry-1", run_index=1, observed_at="2026-08-30T08:00:00Z"),
        series_row("retry-2", run_index=2, observed_at="2026-08-30T01:00:00-07:00"),
    ]
    assert only_cell(reduced(same_execution))["state"] == "green"


def test_newest_no_product_result_does_not_fall_back_to_older_pass() -> None:
    rows = [
        series_row("pass", run_id="older", observed_at="2026-08-30T01:00:00Z"),
        no_result("unavailable", run_id="newer", observed_at="2026-08-30T02:00:00Z"),
    ]
    cell = only_cell(reduced(rows))

    assert cell["state"] is None
    assert cell["latest_result"] is None
    assert cell["latest_result_tree"] == TREE
    assert "latest-run state" in cell["incomplete_fields"]


def test_unknown_row_qualification_only_blocks_state_when_it_can_decide() -> None:
    unknown_old = series_row(
        "unknown-old",
        producer="pressure-test",
        run_id="unknown-run",
        observed_at="2026-08-30T01:00:00Z",
    )
    known_new = series_row(
        "known-new",
        producer="pressure-test",
        run_id="known-run",
        observed_at="2026-08-30T02:00:00Z",
    )
    cell = only_cell(
        reduced(
            [unknown_old, known_new],
            projection={"known-new": None},
            ancestry={TREE: True},
        )
    )
    assert cell["state"] == "green"
    assert "projection validation reason" in cell["incomplete_fields"]

    unknown_new = copy.deepcopy(unknown_old)
    unknown_new["observed_at"] = "2026-08-30T03:00:00Z"
    cell = only_cell(
        reduced(
            [unknown_new, known_new],
            projection={"known-new": None},
            ancestry={TREE: True},
        )
    )
    assert cell["state"] is None
    assert cell["latest_event_ids"] == ["unknown-old"]


def test_outer_product_failure_is_not_overridden_by_no_verdict_evidence() -> None:
    row = product_failure(
        "event-1",
        run_index=1,
        outcome="errored",
        result="crash-error",
        no_verdict_evidence={
            "kind": "first_run_rejected",
            "disposition": "no_result",
        },
    )

    cell = only_cell(reduced([row]))
    assert cell["state"] == "red"
    assert cell["latest_result"] == "product_failure"


def test_explicit_no_product_result_is_distinct_from_missing_authority() -> None:
    legacy = series_row(
        "legacy-pass",
        outcome="passed",
        result=None,
        failure_class=None,
    )

    cell = only_cell(reduced([legacy], product_results={"legacy-pass": None}))
    assert cell["state"] is None
    assert cell["latest_result"] is None
    assert "latest-run state" in cell["incomplete_fields"]
    assert "product result" not in cell["incomplete_fields"]


def test_product_result_authority_is_injected_not_reconstructed() -> None:
    row = series_row(
        "event",
        run_index=1,
        outcome="errored",
        result="pass",
        failure_class="product_failure",
    )

    assert only_cell(reduced([row], product_results={"event": "pass"}))["state"] == (
        "green"
    )
    assert (
        only_cell(reduced([row], product_results={"event": "product_failure"}))["state"]
        == "red"
    )

    unavailable = only_cell(reduced([row], product_results={}))
    assert unavailable["state"] is None
    assert "product result" in unavailable["incomplete_fields"]


@pytest.mark.parametrize(
    ("producer", "retry_field"),
    [("validate", "run_index"), ("pressure-test", "attempt")],
)
def test_failure_with_missing_earlier_retry_is_unavailable_but_pass_is_green(
    producer: str,
    retry_field: str,
) -> None:
    arguments = {"producer": producer, "run_index": 2}
    if retry_field == "attempt":
        arguments.update(run_index=0, attempt=2)

    failure = product_failure("failure", **arguments)
    failed = only_cell(reduced([failure]))
    assert failed["state"] is None
    assert "retry grouping" in failed["incomplete_fields"]

    passing = series_row("pass", **arguments)
    passed = only_cell(reduced([passing]))
    assert passed["state"] == "green"
    assert "retry grouping" in passed["incomplete_fields"]


@pytest.mark.parametrize(
    ("outcome", "result", "failure_class"),
    [
        ("timeout", "timeout", "no_result"),
        ("no_result", "oom", "no_result"),
        ("errored", None, "understood_infrastructure_failure"),
        ("skipped", None, "understood_prerequisite_failure"),
        ("no_result", None, "no_result"),
    ],
)
def test_product_failure_mixed_with_no_product_retry_is_unavailable(
    outcome: str,
    result: str | None,
    failure_class: str,
) -> None:
    rows = [
        product_failure("failure", run_index=1),
        series_row(
            "no-product",
            run_index=2,
            outcome=outcome,
            result=result,
            failure_class=failure_class,
        ),
    ]

    cell = only_cell(reduced(rows))
    assert cell["state"] is None
    assert cell["latest_result"] is None


def missing_execution(
    run_id: str,
    *,
    cell: str = CELL,
    path: str = "full",
    tree: str = TREE,
    observed_at: str | None = None,
    record_id: str | None = None,
) -> dict[str, Any]:
    value = {
        "cell": cell,
        "run_id": run_id,
        "path": path,
        "tree": tree,
        "observed_at": observed_at,
    }
    if record_id is not None:
        value["record_id"] = record_id
    return value


def test_partial_run_omission_leaves_prior_cell_history_untouched() -> None:
    manifest = manifest_metadata(manifest_cell(), manifest_cell("tests/other"))
    prior = series_row("old-pass", run_id="old-run")
    value = reduced(
        [prior],
        manifest=manifest,
        missing_executions=[
            missing_execution("partial-run", cell=OTHER_CELL, observed_at=None)
        ],
    )

    by_cell = {cell["cell"]: cell for cell in value["cells"]}
    assert by_cell[CELL]["state"] == "green"
    assert by_cell[CELL]["physical_row_count"] == 1
    assert by_cell[OTHER_CELL]["state"] is None


def test_missing_execution_cannot_contradict_an_existing_validate_row() -> None:
    row = series_row("present", run_id="same-run")
    with pytest.raises(reduce.ReductionRefused, match="contradicts"):
        reduced(
            [row],
            missing_executions=[missing_execution("same-run")],
        )


@pytest.mark.parametrize("decisions", [{}, {"validate-row": None}])
def test_validate_selection_is_a_visible_cross_check_and_false_refuses(
    decisions: dict[str, dict[str, Any] | None],
) -> None:
    row = series_row("validate-row", run_id="run-current")

    value = reduced([row], validate_run_selection=decisions)
    unknown = only_cell(value)
    assert unknown["state"] == "green"
    assert unknown["qualifying_physical_row_count"] == 1
    assert "validate path selection" in unknown["incomplete_fields"]
    assert any(
        item["field"] == "validate path selection"
        and "path and whether it selected" in item["reason"]
        for item in value["incomplete_data"]
    )

    with pytest.raises(reduce.ReductionRefused, match="unselected by quick"):
        reduced(
            [row],
            validate_run_selection={
                "validate-row": {"path": "quick", "selected": False}
            },
        )

    non_validate = series_row("pressure", producer="pressure-test")
    with pytest.raises(reduce.ReductionRefused, match="non-validate"):
        reduced(
            [non_validate],
            validate_run_selection={"pressure": {"path": "full", "selected": True}},
        )


def test_selected_by_preserves_distinct_validate_run_facts_by_actual_path() -> None:
    rows = [
        series_row(
            "event-b",
            run_id="super-run",
            run_index=2,
            observed_at="2026-08-30T02:00:00Z",
        ),
        series_row(
            "event-a",
            run_id="super-run",
            run_index=1,
            observed_at="2026-08-30T02:00:00Z",
        ),
    ]
    manifest = manifest_metadata(manifest_cell(), manifest_cell("tests/other"))
    selection = {
        event_id: {"path": "super", "selected": True}
        for event_id in ("event-a", "event-b")
    }

    value = reduced(rows, manifest=manifest, validate_run_selection=selection)
    by_cell = {cell["cell"]: cell for cell in value["cells"]}

    assert value["counts"]["in_manifest_cell_count"] == 2
    assert by_cell[CELL]["selected_by"] == {
        "super": [
            {
                "run_id": "super-run",
                "tree": TREE,
                "observed_at": "2026-08-30T02:00:00Z",
                "event_ids": ["event-a", "event-b"],
            }
        ]
    }
    assert by_cell[OTHER_CELL]["selected_by"] == {}
    assert by_cell[OTHER_CELL]["state"] == "never-measured"
    assert all("selected" not in key for key in value["counts"])


def test_selected_without_series_preserves_path_with_no_event() -> None:
    value = reduced(
        [],
        missing_executions=[
            missing_execution(
                "missing-super",
                path="super",
                observed_at="2026-08-30T03:00:00Z",
            )
        ],
    )

    assert only_cell(value)["selected_by"] == {
        "super": [
            {
                "run_id": "missing-super",
                "tree": TREE,
                "observed_at": "2026-08-30T03:00:00Z",
                "event_ids": [],
            }
        ]
    }


def test_missing_execution_participates_in_latest_order_without_becoming_a_row() -> (
    None
):
    newer_pass = series_row(
        "new-pass",
        run_id="newer-complete",
        observed_at="2026-08-30T02:00:00Z",
    )
    older_gap = missing_execution(
        "older-missing", tree=OTHER_TREE, observed_at="2026-08-30T01:00:00Z"
    )
    value = reduced([newer_pass], missing_executions=[older_gap])
    cell = only_cell(value)
    assert cell["state"] == "green"
    assert cell["physical_row_count"] == 1
    assert cell["latest_result_time"] == "2026-08-30T02:00:00Z"
    assert "series row" in cell["incomplete_fields"]

    older_pass = series_row(
        "old-pass",
        run_id="older-complete",
        observed_at="2026-08-30T01:00:00Z",
    )
    newer_gap = missing_execution(
        "newer-missing", tree=OTHER_TREE, observed_at="2026-08-30T02:00:00Z"
    )
    value = reduced([older_pass], missing_executions=[newer_gap])
    cell = only_cell(value)
    assert cell["state"] is None
    assert cell["latest_result_tree"] == OTHER_TREE
    assert cell["latest_result_time"] == "2026-08-30T02:00:00Z"
    assert any(
        item["field"] == "latest-run state" and item["input"] == "validation ledger"
        for item in value["incomplete_data"]
    )
    assert any(
        item["field"] == "series row"
        and item["path"] == "full"
        and "selected by full" in item["reason"]
        for item in value["incomplete_data"]
    )

    tied = reduced(
        [
            series_row(
                "tied-pass",
                run_id="complete",
                observed_at="2026-08-30T02:00:00Z",
            )
        ],
        missing_executions=[
            missing_execution("missing", observed_at="2026-08-30T02:00:00Z")
        ],
    )
    assert any(
        item["field"] == "latest-run order"
        and item["input"] == "validation ledger and series"
        for item in tied["incomplete_data"]
    )


def test_nonancestor_missing_execution_does_not_prevent_never_measured() -> None:
    gap = missing_execution(
        "feature-run", tree=OTHER_TREE, record_id="validation-record"
    )
    value = reduced(
        [],
        projection={},
        ancestry={OTHER_TREE: False},
        missing_executions=[gap],
    )
    assert only_cell(value)["state"] == "never-measured"
    assert any(
        item.get("record_id") == "validation-record"
        and "not an ancestor" in item["reason"]
        for item in value["incomplete_data"]
    )


@pytest.mark.parametrize("ancestry", [{}, {TREE: None}])
def test_missing_execution_with_unavailable_ancestry_is_incomplete(
    ancestry: dict[str, bool | None],
) -> None:
    value = reduced(
        [],
        projection={},
        ancestry=ancestry,
        missing_executions=[missing_execution("run")],
    )
    assert only_cell(value)["state"] is None
    assert "current-main ancestry" in only_cell(value)["incomplete_fields"]


def test_missing_execution_shape_and_join_are_validated() -> None:
    historical = reduced(
        [], missing_executions=[missing_execution("run", cell=OTHER_CELL)]
    )
    assert any(
        item["field"] == "current manifest join"
        and item["input"] == "validation ledger"
        and item.get("cell") == OTHER_CELL
        for item in historical["incomplete_data"]
    )
    with pytest.raises(reduce.ReductionRefused, match="duplicate missing execution"):
        reduced(
            [],
            missing_executions=[missing_execution("run"), missing_execution("run")],
        )
    without_path = missing_execution("run")
    del without_path["path"]
    with pytest.raises(reduce.ReductionRefused, match=r"missing_executions\[0\]\.path"):
        reduced([], missing_executions=[without_path])


def test_raw_observed_at_is_ignored_without_producer_owned_injection() -> None:
    rows = [
        series_row(
            "old-pass",
            run_id="old-run",
            observed_at="2026-08-30T01:00:00Z",
        ),
        product_failure(
            "new-failure",
            run_id="new-run",
            run_index=1,
            observed_at="2026-08-30T02:00:00Z",
        ),
    ]

    unavailable = only_cell(reduced(rows, producer_observed_at={}))
    assert unavailable["state"] is None
    injected = only_cell(
        reduced(
            rows,
            producer_observed_at={
                "old-pass": "2026-08-30T01:00:00Z",
                "new-failure": "2026-08-30T02:00:00Z",
            },
        )
    )
    assert injected["state"] == "red"


def test_num_runs_is_counted_literally_without_expanding_rows() -> None:
    row = series_row("compressed", num_runs=3, last_run_index=2)
    value = reduced([row])
    cell = only_cell(value)

    assert value["counts"]["physical_row_count"] == 1
    assert value["counts"]["represented_run_count"] == 3
    assert cell["physical_row_count"] == 1
    assert cell["represented_run_count"] == 3


def test_lone_compressed_validate_failure_cannot_prove_complete_retries() -> None:
    failure = product_failure(
        "compressed-failure", run_index=1, num_runs=3, last_run_index=7
    )
    failed = only_cell(reduced([failure]))
    assert failed["state"] is None
    assert "retry grouping" in failed["incomplete_fields"]

    passing = series_row("compressed-pass", run_index=1, num_runs=3, last_run_index=7)
    passed = only_cell(reduced([passing]))
    assert passed["state"] == "green"
    assert "retry grouping" in passed["incomplete_fields"]


@pytest.mark.parametrize("producer", ["pressure-test", "hermit-repeat"])
@pytest.mark.parametrize("product_passed", [True, False])
def test_lone_compressed_campaign_row_can_decide_equal_repetitions(
    producer: str,
    product_passed: bool,
) -> None:
    arguments = {
        "producer": producer,
        "run_index": 4,
        "num_runs": 3,
        "last_run_index": 9,
    }
    row = (
        series_row("compressed", **arguments)
        if product_passed
        else product_failure("compressed", **arguments)
    )

    cell = only_cell(reduced([row]))
    assert cell["state"] == ("green" if product_passed else "red")


@pytest.mark.parametrize("producer", ["pressure-test", "hermit-repeat"])
def test_one_producer_run_cannot_span_multiple_hermit_trees_across_cells(
    producer: str,
) -> None:
    rows = [
        series_row(
            "older",
            producer=producer,
            run_id="one-run",
            run_index=0,
            tree="1" * 40,
        ),
        product_failure(
            "newer",
            producer=producer,
            run_id="one-run",
            run_index=1,
            cell=OTHER_CELL,
            tree="2" * 40,
        ),
    ]

    with pytest.raises(reduce.ReductionRefused, match="spans multiple Hermit trees"):
        reduced(rows)


def test_reduce_cell_rejects_one_run_spanning_trees_without_top_level_validation() -> (
    None
):
    raw_rows = [
        series_row(
            "older",
            producer="pressure-test",
            run_id="one-run",
            run_index=0,
            tree="1" * 40,
        ),
        product_failure(
            "newer",
            producer="pressure-test",
            run_id="one-run",
            run_index=1,
            tree="2" * 40,
        ),
    ]
    rows, _incomplete = reduce._normalise_canonical_input(canonical_input(raw_rows))

    with pytest.raises(reduce.ReductionRefused, match="spans multiple Hermit trees"):
        reduce.reduce_cell(
            CELL,
            rows,
            projection_results={"older": None, "newer": None},
            current_main_ancestry={"1" * 40: True, "2" * 40: True},
            validate_run_selection={},
        )


def test_validate_run_tree_includes_selected_without_series_evidence() -> None:
    row = series_row("series", run_id="one-run", tree="1" * 40)
    missing = [
        {
            "cell": OTHER_CELL,
            "run_id": "one-run",
            "path": "full",
            "tree": "2" * 40,
            "observed_at": "2026-08-30T12:00:00Z",
        }
    ]

    with pytest.raises(reduce.ReductionRefused, match="spans multiple Hermit trees"):
        reduced([row], missing_executions=missing)


@pytest.mark.parametrize("producer", ["pressure-test", "hermit-repeat"])
def test_explicit_attempt_on_compressed_campaign_row_cannot_prove_red(
    producer: str,
) -> None:
    arguments = {
        "producer": producer,
        "run_index": 4,
        "attempt": 1,
        "num_runs": 3,
        "last_run_index": 9,
    }

    failed = only_cell(reduced([product_failure("failure", **arguments)]))
    assert failed["state"] is None
    assert "retry grouping" in failed["incomplete_fields"]

    passed = only_cell(reduced([series_row("pass", **arguments)]))
    assert passed["state"] == "green"
    assert "retry grouping" in passed["incomplete_fields"]


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (
            lambda source: source.update(schema_version=2),
            "canonical input schema_version",
        ),
        (
            lambda source: source.update(schema_version=True),
            "canonical input schema_version",
        ),
        (
            lambda source: source["series"].update(row_count=99),
            "row_count",
        ),
        (
            lambda source: source["series"].update(represented_run_count=99),
            "represented_run_count",
        ),
        (
            lambda source: source["series"]["rows"][0]["series"].update(num_runs=True),
            "num_runs",
        ),
    ],
)
def test_malformed_envelopes_and_used_fields_refuse(mutate, message: str) -> None:
    rows = [series_row("event-1")]
    source = canonical_input(rows)
    mutate(source)
    if message == "num_runs":
        source["series"]["rows_sha256"] = canonical_digest(source["series"]["rows"])

    with pytest.raises(reduce.ReductionRefused, match=message):
        projection, ancestry = authorities(rows)
        reduce.reduce_inputs(
            source,
            manifest_metadata(),
            projection_results=projection,
            current_main_ancestry=ancestry,
        )


def test_consumed_array_digests_refuse_same_count_mutation() -> None:
    row = series_row("event-1")
    source = canonical_input([row])
    source["series"]["rows"][0]["event_id"] = "mutated"
    with pytest.raises(reduce.ReductionRefused, match="rows_sha256 disagrees"):
        reduce.reduce_inputs(source, manifest_metadata())


def test_duplicate_event_ids_and_unknown_decision_keys_refuse() -> None:
    rows = [series_row("same"), series_row("same", run_index=1)]
    with pytest.raises(reduce.ReductionRefused, match="duplicate.*event_id"):
        reduced(rows)

    one = [series_row("known")]
    with pytest.raises(reduce.ReductionRefused, match="unknown series event_id"):
        reduced(
            one,
            projection={"known": None, "unknown": None},
            ancestry={TREE: True},
        )

    with pytest.raises(reduce.ReductionRefused, match="unknown Hermit commit"):
        reduced(one, projection={"known": None}, ancestry={OTHER_TREE: True})

    with pytest.raises(reduce.ReductionRefused, match="keys must be nonempty strings"):
        reduced(one, projection={1: None}, ancestry={TREE: True})

    with pytest.raises(reduce.ReductionRefused, match="product_results names unknown"):
        reduced(one, product_results={"unknown": "pass"})
    with pytest.raises(reduce.ReductionRefused, match="must be pass"):
        reduced(one, product_results={"known": "unknown"})
    with pytest.raises(
        reduce.ReductionRefused, match="producer_observed_at names unknown"
    ):
        reduced(one, producer_observed_at={"unknown": "2026-08-30T00:00:00Z"})
    with pytest.raises(reduce.ReductionRefused, match="RFC3339"):
        reduced(one, producer_observed_at={"known": None})


@pytest.mark.parametrize(
    ("arguments", "message"),
    [
        ({"projection": {"known": ""}}, "projection_results values"),
        ({"ancestry": {TREE: "yes"}}, "current_main_ancestry values"),
        ({"product_results": {1: "pass"}}, "product_results keys"),
        (
            {"producer_observed_at": {1: "2026-08-30T00:00:00Z"}},
            "producer_observed_at keys",
        ),
        (
            {"validate_run_selection": {"known": "yes"}},
            "validate_run_selection.*object",
        ),
        (
            {"validate_run_selection": {"known": {"path": "full", "selected": "yes"}}},
            "validate_run_selection.*selected must be bool",
        ),
        (
            {"validate_run_selection": {"known": {"selected": True}}},
            "must contain only path and selected",
        ),
        (
            {"validate_run_selection": {"known": {"path": "", "selected": True}}},
            "validate_run_selection.*path.*nonempty string",
        ),
        ({"producer_observed_at": {"known": "not-a-time"}}, "RFC3339"),
        ({"producer_observed_at": {"known": "2026-08-30T00:00:00"}}, "timezone"),
    ],
)
def test_malformed_authority_values_refuse(
    arguments: dict[str, Any], message: str
) -> None:
    with pytest.raises(reduce.ReductionRefused, match=message):
        reduced([series_row("known")], **arguments)


def test_compressed_and_explicit_validate_retry_overlap_is_unavailable() -> None:
    rows = [
        no_result(
            "compressed",
            run_index=1,
            num_runs=2,
            last_run_index=2,
        ),
        no_result("attempt-1", run_index=1, attempt=1),
        no_result("attempt-2", run_index=2, attempt=2),
    ]
    value = reduced(rows)
    cell = only_cell(value)

    assert cell["state"] is None
    assert cell["physical_row_count"] == 3
    assert cell["represented_run_count"] == 4
    assert "retry grouping" in cell["incomplete_fields"]


def test_compressed_validate_overlap_does_not_require_an_endpoint_collision() -> None:
    rows = [
        no_result(
            "compressed",
            run_index=1,
            num_runs=2,
            last_run_index=7,
        ),
        series_row("explicit", run_index=4, attempt=4),
    ]

    cell = only_cell(reduced(rows))
    assert cell["state"] is None
    assert cell["physical_row_count"] == 2
    assert cell["represented_run_count"] == 3
    assert "retry grouping" in cell["incomplete_fields"]


@pytest.mark.parametrize("producer", ["pressure-test", "hermit-repeat"])
def test_compressed_retry_overlap_and_campaign_order_are_unavailable(
    producer: str,
) -> None:
    same_execution = [
        series_row(
            "compressed",
            producer=producer,
            run_index=1,
            num_runs=2,
            last_run_index=7,
        ),
        series_row(
            "explicit-retry",
            producer=producer,
            run_index=1,
            attempt=2,
        ),
    ]
    cell = only_cell(reduced(same_execution))
    assert cell["state"] is None
    assert "retry grouping" in cell["incomplete_fields"]

    same_campaign = [
        series_row(
            "compressed",
            producer=producer,
            run_index=1,
            num_runs=2,
            last_run_index=7,
        ),
        product_failure("later", producer=producer, run_index=4),
    ]
    cell = only_cell(reduced(same_campaign))
    assert cell["state"] is None
    assert cell["physical_row_count"] == 2
    assert cell["represented_run_count"] == 3
    assert "latest-run order" in cell["incomplete_fields"]


def test_deleting_rows_changes_only_the_state_or_counts_the_row_decides() -> None:
    older = series_row("older", producer="pressure-test", run_index=0)
    newer = product_failure("newer", producer="pressure-test", run_index=1)

    both = only_cell(reduced([older, newer]))
    only_newer = only_cell(reduced([newer]))
    only_older = only_cell(reduced([older]))
    empty = only_cell(reduced([]))

    assert both["state"] == only_newer["state"] == "red"
    assert both["physical_row_count"] == 2
    assert only_newer["physical_row_count"] == 1
    assert only_older["state"] == "green"
    assert empty["state"] == "never-measured"


def test_historical_cells_are_visible_as_incomplete_not_joined_to_current_cells() -> (
    None
):
    row = series_row("historical", cell="tests/retired/verify/ptrace")
    value = reduced([row])

    assert only_cell(value)["state"] == "never-measured"
    assert any(
        item["field"] == "current manifest join"
        and item["input"] == "series"
        and item.get("cell") == "tests/retired/verify/ptrace"
        for item in value["incomplete_data"]
    )


def test_canonical_input_incomplete_identifiers_are_preserved() -> None:
    source = canonical_input(
        [],
        incomplete_data=[
            {
                "field": "source path",
                "input": "series",
                "reason": "the canonical export does not retain the source path",
                "cell": CELL,
                "event_id": "event-1",
                "run_id": "run-1",
                "secret": "do not publish",
            }
        ],
    )

    value = reduce.reduce_inputs(
        source,
        manifest_metadata(),
    )
    assert value["incomplete_data"] == [
        {
            "field": "source path",
            "input": "series",
            "reason": "the canonical export does not retain the source path",
            "cell": CELL,
            "event_id": "event-1",
            "run_id": "run-1",
        }
    ]


def test_manifest_metadata_schema_identity_and_stored_state_are_validated() -> None:
    malformed = manifest_metadata()
    malformed["schema"] = 2
    with pytest.raises(reduce.ReductionRefused, match="manifest metadata schema"):
        reduced([], manifest=malformed)

    duplicate = manifest_metadata(manifest_cell(), manifest_cell())
    with pytest.raises(reduce.ReductionRefused, match="duplicate manifest cell"):
        reduced([], manifest=duplicate)

    stored = manifest_metadata()
    stored["cells"][0]["state"] = "green"
    with pytest.raises(reduce.ReductionRefused, match="ledger-derived"):
        reduced([], manifest=stored)


def test_manifest_policy_metadata_is_not_validate_run_selection_authority() -> None:
    row = series_row("measured")
    selected = manifest_metadata(
        manifest_cell(selected_by_full=True, not_selected_by_full_reason=None)
    )
    unselected = manifest_metadata(
        manifest_cell(
            selected_by_full=False,
            not_selected_by_full_reason="not selected by this manifest policy",
        )
    )

    selected_result = reduced([row], manifest=selected)
    unselected_result = reduced([row], manifest=unselected)

    assert selected_result == unselected_result
    assert only_cell(selected_result)["state"] == "green"
    output_keys = reduce._recursive_keys(selected_result)
    assert "selected_by_full" not in output_keys
    assert "not_selected_by_full_reason" not in output_keys
    assert all("enabled" not in key for key in output_keys)


def test_rendering_is_deterministic_allowlisted_and_does_not_mutate_inputs(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    rows = [
        series_row(
            "event-b",
            producer="pressure-test",
            run_index=1,
            secret_series_value="do not publish",
        ),
        series_row(
            "event-a",
            producer="pressure-test",
            run_index=0,
            unknown_input={"secret": "do not publish"},
        ),
    ]
    manifest = manifest_metadata()
    manifest["secret_manifest_value"] = "do not publish"
    manifest["cells"][0]["secret"] = "do not publish"
    source = canonical_input(rows)
    source_before = copy.deepcopy(source)
    manifest_before = copy.deepcopy(manifest)
    projection, ancestry = authorities(rows)
    products = {"event-a": "pass", "event-b": "pass"}

    def refuse_open(*_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("the pure reducer must not read files")

    monkeypatch.setattr("builtins.open", refuse_open)
    forward = reduce.reduce_inputs(
        source,
        manifest,
        projection_results=projection,
        current_main_ancestry=ancestry,
        product_results=products,
    )
    reverse = reduce.reduce_inputs(
        canonical_input(list(reversed(rows))),
        manifest,
        projection_results=dict(reversed(list(projection.items()))),
        current_main_ancestry=dict(reversed(list(ancestry.items()))),
        product_results=dict(reversed(list(products.items()))),
    )

    rendered = reduce.render_reduction(forward)
    assert rendered == reduce.render_reduction(reverse)
    assert rendered.endswith(b"\n")
    assert b"do not publish" not in rendered
    assert "selected_cell_count" not in forward["counts"]
    assert "unselected_cell_count" not in forward["counts"]
    assert source == source_before
    assert manifest == manifest_before
