from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import Any

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/build_data.py"
SPEC = importlib.util.spec_from_file_location("compatibility_website_build_data", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
build_data = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = build_data
SPEC.loader.exec_module(build_data)


CELL = "tests/hello/verify/ptrace"
OTHER_CELL = "tests/other/verify/ptrace"
LEGACY_SERIES_REASON = "stress-series/v1 does not record machine_shortname, kernel_version, and host_capabilities"
LEGACY_SERIES_GROUPS = (
    (
        "validate-coord-helper-7ddb3559a09f-1787860887855607878-3901740-a5dfdd07",
        "7ddb3559a09f2709a67ce2b605426b43c33f070f", 351, 453,
    ),
    (
        "validate-hermit-121-ledger-fast18-0b2772a7",
        "0b2772a74fc70a1eb4b24ef3385cc1a51f9cc028", 1, 1,
    ),
    (
        "validate-hermit-139-d4d9fe5effe3-1787834846589488461-1094101-3efbe98f",
        "d4d9fe5effe31a90c5a64238ce99fc5ddeea4710", 1, 1,
    ),
    (
        "validate-hermit-141-4944fb5b3cc0-1787834650301238559-798721-c47ac7c9",
        "4944fb5b3cc029459056a3b9743f0d0df3ad0209", 305, 305,
    ),
    (
        "validate-hermit-147-green-bar-baseline-26f5bc2de3d3",
        "26f5bc2de3d328b64c41c42ef093ecb9d89e98ab", 435, 483,
    ),
    (
        "validate-mega-lander-677df8ff06dc-1787938221909556153-528140-b73f3ab4",
        "677df8ff06dc72941c2eaa5826c7ba32f4ca36aa", 1, 1,
    ),
)
REASONED_CELLS = (
    (
        "backend-parity-c/environment-and-workdir/custom/ptrace",
        "performs-no-comparison-by-design",
        "custom mode does not perform canonical two-run comparison",
        32,
        31,
    ),
    (
        "system-utils/clock-determinism/custom/liteinst",
        "performs-no-comparison-by-design",
        "custom mode does not perform canonical two-run comparison",
        32,
        31,
    ),
    (
        "system-utils/clock-determinism/custom/ptrace",
        "performs-no-comparison-by-design",
        "custom mode does not perform canonical two-run comparison",
        32,
        31,
    ),
    (
        "system-utils/record-getpid/replay/ptrace",
        "unavailable-with-reason",
        "attempt 1 did not compare canonical nonzero INFO evidence",
        32,
        31,
    ),
)


def git(repo: Path, *arguments: str) -> str:
    result = build_data._modules().bounded_capture.git(
        arguments,
        cwd=repo,
        check=True,
        text=True,
        timeout=60,
    )
    return result.stdout.strip()


def checker_input_sha256(rows: list[dict[str, Any]]) -> str:
    encoded = sorted(build_data._canonical_bytes(row) for row in rows)
    return hashlib.sha256(b"".join(row + b"\n" for row in encoded)).hexdigest()


def repository(tmp_path: Path, name: str) -> tuple[Path, str]:
    repo = tmp_path / name
    repo.mkdir()
    git(repo, "init", "-b", "main")
    git(repo, "config", "user.email", "compatibility-website@example.invalid")
    git(repo, "config", "user.name", "compatibility website test")
    (repo / "tracked.txt").write_text("initial\n")
    git(repo, "add", "tracked.txt")
    git(repo, "commit", "-m", "initial")
    commit = git(repo, "rev-parse", "HEAD")
    git(repo, "update-ref", "refs/remotes/origin/main", commit)
    return repo, commit


def manifest_metadata(*cells: str) -> dict[str, Any]:
    cells = cells or (CELL,)
    test_ids = sorted({cell.rsplit("/", 2)[0] for cell in cells})
    return {
        "schema": 1,
        "manifest_schema": 3,
        "manifest_sha256": "a" * 64,
        "tests": [
            {
                "build": None,
                "category": "tests",
                "description": f"Fixture for {test_id}",
                "direct": None,
                "id": test_id,
                "lane": "portable",
                "observation": {
                    "artifacts": [],
                    "status": True,
                    "stderr": True,
                    "stdout": True,
                },
                "occasional": False,
                "preprocessors": [],
                "program": test_id,
                "requires": [],
            }
            for test_id in test_ids
        ],
        "cells": [
            {
                "test": cell.rsplit("/", 2)[0],
                "mode": cell.rsplit("/", 2)[1],
                "backend": cell.rsplit("/", 2)[2],
                "category": "tests",
                "current_reproducer": {
                    "argv": ["./ci/test-harness.rs", "--exact-cell", cell],
                    "shell_command": f"./ci/test-harness.rs --exact-cell {cell}",
                },
                "current_reproducer_unavailable_reason": None,
                "guest_args": [],
                "lane": "portable",
                "not_applicable_reason": None,
                "not_selected_by_full_reason": None,
                "selected_by_full": True,
                "timeout_seconds": 60,
                "workdir": None,
            }
            for cell in cells
        ],
        "selected_by_full_custom_commands": [],
    }


def series_row(
    event_id: str,
    tree: str,
    *,
    schema: str = "stress-series/v3",
    producer: str = "validate",
    run_id: str = "run-1",
    cell: str = CELL,
    run_index: int = 1,
    num_runs: int = 1,
    outcome: str = "passed",
    result: str | None = "pass",
    failure_class: str | None = None,
) -> dict[str, Any]:
    payload = {
        "cell": cell,
        "tree": tree,
        "run_index": run_index,
        "num_runs": num_runs,
        "outcome": outcome,
        "result": result,
        "failure_class": failure_class,
    }
    if schema != "stress-series/v1":
        payload.update(
            machine_shortname="fixture-host",
            kernel_version="fixture-kernel",
            host_capabilities={
                "cpuid-faulting": {"present": True, "evidence": "fixture"},
                "kvm": {"present": True, "evidence": "fixture"},
            },
        )
    return {
        "schema": schema,
        "event_id": event_id,
        "event_type": "series.observation",
        "emitted_at": "2026-08-31T00:00:00Z",
        "host": "fixture-host",
        "producer": producer,
        "run_id": run_id,
        "team": "hermit",
        "series": payload,
    }


def validation_row(
    commit: str,
    *,
    run_id: str = "run-1",
    profile: str = "quick",
    selection_mode: str = "full",
    selected: list[str] | None = None,
    record_id: str = "record-1",
) -> dict[str, Any]:
    selected = [CELL] if selected is None else selected
    identities = [
        {
            "lane": "portable",
            "category": "tests",
            "test": cell.rsplit("/", 2)[0],
            "mode": cell.rsplit("/", 2)[1],
            "backend": cell.rsplit("/", 2)[2],
        }
        for cell in selected
    ]
    comparison = {
        "strictness": "canonical",
        "display_name": "BitwiseInfoV1",
        "compare_logs": True,
        "compare_io_buffers": True,
        "log_scope": "info",
        "record_envelope": "all_records_v1",
        "virtualize_time": True,
        "strip_lines": False,
        "canonicalize_addresses": True,
        "full_trace": True,
        "exact_remainder": True,
        "stripped_prefixes": ["real-wall-clock-prefix/v1"],
        "canonicalizations": ["host-address-to-first-appearance-ordinal/v1"],
        "ignore_lines": False,
        "skip_commit": False,
        "skip_detlog": False,
    }
    verdict = {
        "state": "compared-and-matched",
        "comparison_tier": "canonical-bitwise",
        "comparison": comparison,
        "bitwise_parity": True,
        "compared_log_messages": {"left": 1, "right": 1},
    }
    qualifying_receipt = build_data._modules().qualifying_receipt
    return {
        "schema_version": 7,
        "repo": "hermit",
        "record_id": record_id,
        "run_id": run_id,
        "profile": profile,
        "selection_mode": selection_mode,
        "commit": commit,
        "started_at": "2026-08-31T00:00:00Z",
        "finished_at": "2026-08-31T00:01:00Z",
        "cell_results": {
            "run_id": run_id,
            "hermit_sha": commit,
            "source_tree_dirty": False,
            "selected_count": len(identities),
            "recorded_count": len(identities),
            "population_sha256": (
                qualifying_receipt.cell_population_sha256(identities)
            ),
            "artifact": {
                "path": "ignored/validate/artifacts/run/cell-results.jsonl",
                "sha256": "c" * 64,
                "row_count": len(identities),
            },
            "selected": identities,
            "cells": [
                {**identity, "cell_verdict": verdict}
                for identity in identities
            ],
        },
    }


def schema4_corrects_row(commit: str) -> dict[str, Any]:
    row = validation_row(commit)
    row.pop("cell_results")
    row.update(schema_version=4, slot="primary")
    row["corrects"] = {
        "commit": commit,
        "finished_at": row["finished_at"],
        "host": "fixture-host",
        "slot": "primary",
        "was_exit_code": 1,
        "was_result": "fail",
    }
    row["host"] = "fixture-host"
    return row


def schema5_unknown_tree_rows() -> list[dict[str, Any]]:
    facts = (
        ("a7a4bb90e2ddb8363414faacfa43a8ccfbe50409", "devbig030-1786355481-3906492", "devbig030", "2026-08-10T09:50:57Z", "2026-08-10T09:51:21Z"),
        ("a1b0ba162044f610ea07c9575553d5e9c086f3b3", "devbig014-1786377294-598509", "devbig014", "2026-08-10T15:52:20Z", "2026-08-10T15:54:54Z"),
    )
    rows = []
    for commit, record_id, host, started_at, finished_at in facts:
        row = validation_row(commit, record_id=record_id, profile="full")
        row.pop("cell_results")
        row.pop("run_id")
        row.update(schema_version=5, corrects=None, host=host, result="fail", tree="unknown", started_at=started_at, finished_at=finished_at)
        rows.append(row)
    return rows


def reasoned_validation_row(commit: str) -> dict[str, Any]:
    cells = [cell for cell, _state, _reason, _history, _current in REASONED_CELLS]
    row = validation_row(
        commit,
        run_id="validate-hermit-147-green-main-330f01f88eae",
        profile="full",
        selected=cells,
        record_id="devbig014-1787897410-2661156",
    )
    for selected, recorded, (_cell, state, reason, _history, _current) in zip(
        row["cell_results"]["selected"], row["cell_results"]["cells"], REASONED_CELLS, strict=True
    ):
        category = selected["test"].split("/", 1)[0]
        selected["category"] = category
        recorded["category"] = category
        recorded["cell_verdict"] = {
            "state": state,
            "comparison_tier": "declared-but-unverifiable",
            "reason": reason,
        }
    row["cell_results"]["population_sha256"] = build_data._modules().qualifying_receipt.cell_population_sha256(row["cell_results"]["selected"])
    row["result"] = "pass"
    return row


def unmatched_legacy_series_rows() -> list[dict[str, Any]]:
    rows = []
    for group, (run_id, tree, physical, represented) in enumerate(LEGACY_SERIES_GROUPS):
        for index in range(physical):
            rows.append(
                series_row(
                    f"legacy-{group}-{index}",
                    tree,
                    schema="stress-series/v1",
                    run_id=run_id,
                    num_runs=represented - physical + 1 if index == 0 else 1,
                )
            )
    return rows


def unmatched_legacy_fixture_rows(tree: str) -> list[dict[str, Any]]:
    return [
        series_row(
            "legacy-event", tree, schema="stress-series/v1",
            run_id="legacy-run", num_runs=3,
        ),
        series_row(
            "legacy-other", tree, schema="stress-series/v1",
            run_id="legacy-run", cell=OTHER_CELL,
        ),
    ]


def unavailable_schema6_row(commit: str, suffix: str) -> dict[str, Any]:
    row = validation_row(commit, run_id=f"retired-{suffix}", record_id=f"retired-record-{suffix}")
    row["schema_version"] = 6
    comparison = row["cell_results"]["cells"][0]["cell_verdict"]["comparison"]
    for field in ("compare_io_buffers", "display_name", "record_envelope", "virtualize_time"):
        del comparison[field]
    return row


def cell_results_gap(row: dict[str, Any]) -> dict[str, str]:
    return {"field": "cell_results", "input": "validation ledger", "reason": "schema-6 cell_results are unavailable for website selection", "record_id": row["record_id"], "run_id": row["run_id"], "schema": "6"}


def duplicate_marker_history(document: dict[str, Any], retired: list[dict[str, Any]]) -> None:
    duplicate = copy.deepcopy(retired[0])
    duplicate["finished_at"] = "2026-08-31T00:02:00Z"
    rows = document["validation_ledger"]["history_rows"]
    rows.append(duplicate)
    document["validation_ledger"].update(history_row_count=len(rows), history_rows_sha256=build_data._digest(rows))


def canonical_input(
    parent_commit: str,
    rows: list[dict[str, Any]],
    *,
    validation_rows: list[dict[str, Any]] | None = None,
    current_validation_rows: list[dict[str, Any]] | None = None,
    incomplete_data: list[dict[str, str]] | None = None,
    projection: dict[str, str | None] | None = None,
) -> dict[str, Any]:
    validation_rows = [] if validation_rows is None else validation_rows
    current_validation_rows = validation_rows if current_validation_rows is None else current_validation_rows
    incomplete_data = [] if incomplete_data is None else incomplete_data
    projection = projection or {row["event_id"]: None for row in rows}
    reader_files = [
        {
            "bytes": 1,
            "path": "ci-hub/series/series.py",
            "sha256": "3" * 64,
        }
    ]
    sources = [
        {
            "bytes": 2,
            "git_object": "4" * 40,
            "line_count": 1,
            "location": "published",
            "path": "series/hermit/host/2026-08.jsonl",
            "sha256": "5" * 64,
            "store": "series",
        }
    ]
    represented = {
        row["event_id"]: row["series"].get("num_runs", 1) for row in rows
    }
    excluded_physical: dict[tuple[str, str], int] = {}
    excluded_represented: dict[tuple[str, str], int] = {}
    admitted_physical = 0
    admitted_represented = 0
    for row in rows:
        event_id = row["event_id"]
        reason = projection.get(event_id)
        if reason is None:
            admitted_physical += 1
            admitted_represented += represented[event_id]
            continue
        key = (row["schema"], reason)
        excluded_physical[key] = excluded_physical.get(key, 0) + 1
        excluded_represented[key] = (
            excluded_represented.get(key, 0) + represented[event_id]
        )
    row_count = len(rows)
    represented_run_count = sum(represented.values())
    validation_schema_counts: dict[str, int] = {}
    for row in validation_rows:
        schema = str(row["schema_version"])
        validation_schema_counts[schema] = validation_schema_counts.get(schema, 0) + 1
    series_schema_counts: dict[str, int] = {}
    for row in rows:
        schema = str(row["schema"])
        series_schema_counts[schema] = series_schema_counts.get(schema, 0) + 1
    return {
        "schema_version": 1,
        "provenance": {
            "parent_commit": parent_commit,
            "published_ref": parent_commit,
            "reader_ci_hub_tree": "1" * 40,
            "reader_commit": parent_commit,
            "reader_files": reader_files,
            "reader_files_sha256": build_data._digest(reader_files),
            "reader_hermit": {
                "checkout_commit": "2" * 40,
                "gitlink_commit": "2" * 40,
            },
            "source_count": len(sources),
            "sources": sources,
            "sources_sha256": build_data._digest(sources),
        },
        "series": {
            "rows": rows,
            "row_count": row_count,
            "represented_run_count": represented_run_count,
            "rows_sha256": build_data._digest(rows),
            "projection_results": projection,
            "projection_result_physical_row_count": len(projection),
            "projection_results_sha256": build_data._digest(projection),
            "projection_results_binding": {
                "checker_input_jsonl_sha256": checker_input_sha256(rows),
                "physical_row_count": row_count,
                "represented_run_count": represented_run_count,
                "rows_sha256": build_data._digest(rows),
            },
            "projection_admitted_physical_row_count": admitted_physical,
            "projection_admitted_represented_run_count": admitted_represented,
            "projection_excluded_physical_row_count": row_count - admitted_physical,
            "projection_excluded_represented_run_count": (
                represented_run_count - admitted_represented
            ),
            "projection_exclusion_counts": [
                {
                    "physical_row_count": count,
                    "reason": reason,
                    "represented_run_count": excluded_represented[(schema, reason)],
                    "schema": schema,
                }
                for (schema, reason), count in sorted(excluded_physical.items())
            ],
            "schema_counts": series_schema_counts,
        },
        "validation_ledger": {
            "current_validation_run_rows": current_validation_rows,
            "current_validation_run_row_count": len(current_validation_rows),
            "current_validation_run_rows_sha256": build_data._digest(current_validation_rows),
            "event_count": 0,
            "event_schema_counts": {},
            "events": [],
            "events_sha256": build_data._digest([]),
            "history_row_count": len(validation_rows),
            "history_row_schema_counts": validation_schema_counts,
            "history_rows": validation_rows,
            "history_rows_sha256": build_data._digest(validation_rows),
        },
        "incomplete_data": incomplete_data,
    }


def captured(value: dict[str, Any]) -> Any:
    return build_data.CapturedJson(
        value=value,
        encoded=build_data._canonical_bytes(value) + b"\n",
    )


def install_inputs(
    monkeypatch: pytest.MonkeyPatch,
    canonical: dict[str, Any],
    manifest: dict[str, Any],
) -> None:
    monkeypatch.setattr(
        build_data,
        "_capture_canonical_input",
        lambda _parent, _commit: captured(canonical),
    )
    monkeypatch.setattr(
        build_data,
        "_run_manifest_metadata",
        lambda _hermit: captured(manifest),
    )


def make_checkouts(tmp_path: Path) -> tuple[Path, str, Path, str]:
    parent, parent_commit = repository(tmp_path, "parent")
    hermit, hermit_commit = repository(tmp_path, "hermit")
    return parent, parent_commit, hermit, hermit_commit


def run_build(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    rows: list[dict[str, Any]],
    *,
    validation_rows: list[dict[str, Any]] | None = None,
    manifest_cells: tuple[str, ...] = (CELL,),
) -> dict[str, Any]:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    canonical = canonical_input(
        parent_commit, rows, validation_rows=validation_rows
    )
    install_inputs(monkeypatch, canonical, manifest_metadata(*manifest_cells))
    return build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )


def recursive_keys(value: Any) -> set[str]:
    if isinstance(value, dict):
        return set(value).union(*(recursive_keys(item) for item in value.values()))
    if isinstance(value, list):
        return set().union(*(recursive_keys(item) for item in value))
    return set()


@pytest.mark.parametrize(
    ("value", "message"),
    [
        ({"value": "/root/work/private"}, "absolute owner path"),
        ({"value": "/Users/example/private"}, "absolute owner path"),
        ({"value": r"C:\Users\example\private"}, "absolute owner path"),
        ({"value": "Authorization: Bearer abc123"}, "credential-bearing"),
        ({"value": "ghp_abcdefghijklmnopqrstuvwxyz"}, "credential-bearing"),
        ({"access_token": "redacted"}, "credential-bearing"),
        ({"value": "access_token=abc"}, "credential-bearing"),
        ({"value": "client_secret=abc"}, "credential-bearing"),
        ({"value": "AWS_SECRET_ACCESS_KEY=abc"}, "credential-bearing"),
        ({"value": "GITHUB_TOKEN=abc"}, "credential-bearing"),
        ({"value": "APP_CLIENT_SECRET=abc"}, "credential-bearing"),
        ({"reason": "this cell was enabled by policy"}, "dropped vocabulary"),
        ({"is_enabled": False}, "dropped vocabulary"),
    ],
)
def test_publishable_output_refuses_owner_paths_credentials_and_dropped_vocabulary(
    value: Any, message: str
) -> None:
    with pytest.raises(build_data.BuildRefused, match=message):
        build_data._require_publishable_output(value)


def test_build_binds_exact_inputs_and_uses_profile_as_the_validate_path(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    ledger = validation_row(
        hermit_commit,
        profile="quick",
        selection_mode="full",
    )
    canonical = canonical_input(parent_commit, [row], validation_rows=[ledger])
    manifest = manifest_metadata()
    install_inputs(monkeypatch, canonical, manifest)

    first = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )
    second = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert build_data.render_build_data(first) == build_data.render_build_data(second)
    assert first["schema_version"] == 1
    assert first["counts"]["in_manifest_cell_count"] == 1
    assert first["counts"]["green_cell_count"] == 1
    assert first["cells"][0]["selected_by"] == {
        "quick": [
            {
                "event_ids": ["event-1"],
                "observed_at": None,
                "run_id": "run-1",
                "tree": hermit_commit,
            }
        ]
    }
    assert "full" not in first["cells"][0]["selected_by"]
    assert first["cells"][0]["latest_result_time"] is None
    assert first["provenance"]["parent_commit"] == parent_commit
    assert first["provenance"]["hermit_commit"] == hermit_commit
    assert first["provenance"]["series_rows_sha256"] == canonical["series"][
        "rows_sha256"
    ]
    assert first["provenance"]["projection_results_sha256"] == canonical[
        "series"
    ]["projection_results_sha256"]
    assert first["provenance"]["canonical_input_provenance"] == canonical[
        "provenance"
    ]
    assert {"enabled", "measured", "measurement_state", "never_measured"}.isdisjoint(
        recursive_keys(first)
    )


def test_cell_order_uses_test_mode_backend_tuple(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    first = "backend-parity-c/pipe-capacity/chaos/dbt"
    second = "backend-parity-c/pipe-capacity-pin/chaos/dbt"
    install_inputs(monkeypatch, canonical_input(parent_commit, []), manifest_metadata(first, second))

    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)

    assert [cell["cell"] for cell in value["cells"]] == [first, second]
    assert build_data.validate_build_data(value) == value
    value["cells"].reverse()
    with pytest.raises(build_data.BuildRefused, match="manifest policy"):
        build_data.validate_build_data(value)


def test_nested_manifest_orders_use_test_mode_backend_tuple(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    ordinary = (
        "tests/prefix/chaos/dbt",
        "tests/prefix/chaos-pin/dbt",
    )
    custom = (
        "tests/custom-prefix/custom/ptrace",
        "tests/custom-prefix-pin/custom/ptrace",
    )
    manifest = manifest_metadata(*ordinary, *custom)
    manifest["selected_by_full_custom_commands"] = [
        cell for cell in manifest["cells"] if cell["mode"] == "custom"
    ]
    manifest["cells"] = [cell for cell in manifest["cells"] if cell["mode"] != "custom"]
    install_inputs(monkeypatch, canonical_input(parent_commit, []), manifest)

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    test = next(item for item in value["tests"] if item["id"] == "tests/prefix")
    assert test["cells"] == list(ordinary)
    assert [item["cell"] for item in value["selected_by_full_custom_commands"]] == list(custom)

    reversed_test = copy.deepcopy(value)
    next(
        item for item in reversed_test["tests"] if item["id"] == "tests/prefix"
    )["cells"].reverse()
    with pytest.raises(build_data.BuildRefused, match="manifest policy"):
        build_data.validate_build_data(reversed_test)

    reversed_custom = copy.deepcopy(value)
    reversed_custom["selected_by_full_custom_commands"].reverse()
    with pytest.raises(build_data.BuildRefused, match="manifest policy"):
        build_data.validate_build_data(reversed_custom)


def test_series_rows_bind_lossy_public_projection_without_rewriting_source_digest(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    rows = [
        series_row(
            "event-z",
            hermit_commit,
            run_index=1,
            outcome="diverged",
            result="determinism-failure",
            failure_class="product_failure",
        ),
        series_row("event-a", hermit_commit, run_index=2),
    ]
    rows[0]["series"].update(
        first_divergent_messages={"left": "private left", "right": "private right"},
        main_ancestry=False,
    )
    ledger = validation_row(hermit_commit)
    install_inputs(
        monkeypatch,
        canonical_input(parent_commit, rows, validation_rows=[ledger]),
        manifest_metadata(),
    )

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert [row["event_id"] for row in value["runs"]["series_rows"]] == [
        "event-z",
        "event-a",
    ]
    published = value["runs"]["series_rows"][0]
    assert "main_ancestry" not in published["series"]
    assert "first_divergent_messages" not in published["series"]
    assert published["current_main_ancestry"] is True
    assert published["product_result"] == "product_failure"
    assert published["product_result_available"] is True
    assert published["projection_validation_reason"] is None
    assert value["provenance"]["series_rows_sha256"] == build_data._digest(rows)
    assert value["provenance"]["series_rows_projection_sha256"] == build_data._digest(
        value["runs"]["series_rows"]
    )
    assert (
        value["provenance"]["series_rows_sha256"]
        != value["provenance"]["series_rows_projection_sha256"]
    )

    reversed_rows = copy.deepcopy(value)
    reversed_rows["runs"]["series_rows"].reverse()
    with pytest.raises(build_data.BuildRefused, match="provenance digests"):
        build_data.validate_build_data(reversed_rows)

    changed_row = copy.deepcopy(value)
    changed_row["runs"]["series_rows"][0]["emitted_at"] = "2026-08-31T00:00:01Z"
    with pytest.raises(build_data.BuildRefused, match="provenance digests"):
        build_data.validate_build_data(changed_row)

    changed_count = copy.deepcopy(value)
    changed_count["runs"]["series_physical_row_count"] += 1
    with pytest.raises(build_data.BuildRefused, match="runs counts"):
        build_data.validate_build_data(changed_count)

    changed_digest = copy.deepcopy(value)
    changed_digest["provenance"]["series_rows_projection_sha256"] = "0" * 64
    with pytest.raises(build_data.BuildRefused, match="provenance digests"):
        build_data.validate_build_data(changed_digest)


def test_lossy_public_projections_are_bound_separately(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    history_only = validation_row(
        hermit_commit,
        run_id="history-only",
        record_id="history-only-record",
    )
    current = validation_row(hermit_commit)
    install_inputs(
        monkeypatch,
        canonical_input(
            parent_commit,
            [series_row("event-1", hermit_commit)],
            validation_rows=[history_only, current],
            current_validation_rows=[current],
        ),
        manifest_metadata(),
    )
    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    changed_manifest = copy.deepcopy(value)
    changed_manifest["tests"][0]["description"] = "changed after projection"
    with pytest.raises(build_data.BuildRefused, match="manifest metadata projection"):
        build_data.validate_build_data(changed_manifest)

    changed_history = copy.deepcopy(value)
    changed_history["runs"]["validation_history_rows"][0][
        "selection_mode"
    ] = "targeted"
    with pytest.raises(build_data.BuildRefused, match="provenance digests"):
        build_data.validate_build_data(changed_history)

    changed_current = copy.deepcopy(value)
    for name in ("validation_history_rows", "current_validation_run_rows"):
        row = next(
            item
            for item in changed_current["runs"][name]
            if item["record_id"] == current["record_id"]
        )
        row["selection_mode"] = "targeted"
    changed_current["provenance"][
        "validation_history_rows_projection_sha256"
    ] = build_data._digest(changed_current["runs"]["validation_history_rows"])
    with pytest.raises(build_data.BuildRefused, match="provenance digests"):
        build_data.validate_build_data(changed_current)


def test_schema4_corrects_metadata_is_retained_as_null_with_one_grouped_gap(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = schema4_corrects_row(hermit_commit)
    install_inputs(
        monkeypatch,
        canonical_input(parent_commit, [], validation_rows=[row]),
        manifest_metadata(),
    )

    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)

    for field in ("validation_history_rows", "current_validation_run_rows"):
        assert value["runs"][field][0]["corrects"] is None
    marker = {
        "field": "corrects",
        "input": "validation ledger",
        "reason": "schema-4 legacy corrects metadata is not a stable record-id correction edge",
        "schema": "4",
    }
    assert [item for item in value["incomplete_data"] if item["field"] == "corrects"] == [marker]
    assert build_data.validate_build_data(value) == value


@pytest.mark.parametrize("index", range(17))
def test_string_corrects_edges_are_preserved(index: int) -> None:
    row = validation_row("a" * 40)
    row.pop("cell_results")
    row["schema_version"] = 5 if index < 16 else 7
    row["corrects"] = f"record-{index}"

    assert build_data._public_validation_row(row, index)["corrects"] == f"record-{index}"


@pytest.mark.parametrize(
    "mutate",
    [
        lambda row: row["corrects"].pop("host"),
        lambda row: row["corrects"].update(extra="value"),
        lambda row: row["corrects"].update(commit="bad"),
        lambda row: row["corrects"].update(finished_at="not-a-timestamp"),
        lambda row: row["corrects"].update(host=""),
        lambda row: row["corrects"].update(slot=""),
        lambda row: row["corrects"].update(was_result=""),
        lambda row: row["corrects"].update(was_exit_code=True),
        lambda row: row["corrects"].update(was_exit_code="1"),
        lambda row: row.update(commit="b" * 40),
        lambda row: row.update(finished_at="2026-08-31T00:02:00Z"),
        lambda row: row.update(host="other-host"),
        lambda row: row.update(slot="other-slot"),
        lambda row: row.update(schema_version=5),
        lambda row: row.update(corrects=[]),
    ],
)
def test_schema4_corrects_metadata_must_match_its_exact_contract(mutate: Any) -> None:
    row = schema4_corrects_row("a" * 40)
    mutate(row)

    with pytest.raises(build_data.BuildRefused):
        build_data._public_validation_row(row, 0)


@pytest.mark.parametrize("raw", [None])
def test_absent_or_null_corrects_is_omitted(raw: Any) -> None:
    row = validation_row("a" * 40)
    row.pop("cell_results")
    if raw is None:
        row["corrects"] = raw
    assert "corrects" not in build_data._public_validation_row(row, 0)
    row.pop("corrects")
    assert "corrects" not in build_data._public_validation_row(row, 0)
    row["corrects"] = ""
    with pytest.raises(build_data.BuildRefused, match="nonempty string"):
        build_data._public_validation_row(row, 0)


@pytest.mark.parametrize("mutation", ["missing", "duplicate", "reason", "schema"])
def test_schema4_corrects_gap_marker_is_exact(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, mutation: str
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = schema4_corrects_row(hermit_commit)
    install_inputs(monkeypatch, canonical_input(parent_commit, [], validation_rows=[row]), manifest_metadata())
    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)
    marker = next(item for item in value["incomplete_data"] if item["field"] == "corrects")
    if mutation == "missing":
        value["incomplete_data"].remove(marker)
    elif mutation == "duplicate":
        value["incomplete_data"].append(copy.deepcopy(marker))
    else:
        marker[mutation] = "wrong"
    value["counts"]["incomplete_data_count"] = len(value["incomplete_data"])

    with pytest.raises(build_data.BuildRefused, match="incomplete data disagrees"):
        build_data.validate_build_data(value)


def test_schema5_unknown_tree_rows_are_retained_without_creating_authority(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    rows = schema5_unknown_tree_rows()
    canonical = canonical_input(parent_commit, [], validation_rows=rows)
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)

    history = value["runs"]["validation_history_rows"]
    current = value["runs"]["current_validation_run_rows"]
    assert current == history
    assert [row["tree"] for row in history] == [None, None]
    assert [row["history_row_sha256"] for row in history] == [build_data._digest(row) for row in rows]
    assert value["runs"]["validation_history_row_count"] == 2
    assert value["runs"]["current_validation_run_row_count"] == 2
    assert value["provenance"]["validation_history_rows_sha256"] == canonical["validation_ledger"]["history_rows_sha256"]
    assert value["runs"]["series_rows"] == []
    assert value["runs"]["current_main_ancestry"] == {}
    assert value["cells"][0]["selected_by"] == {}
    markers = [item for item in value["incomplete_data"] if item["field"] == "tree"]
    assert markers == [{"field": "tree", "input": "validation ledger", "reason": "schema-5 validation rows do not contain a tree object id", "schema": "5"}]
    assert build_data.validate_build_data(value) == value


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("schema_version", 4),
        ("run_id", "stable-run"),
        ("run_id", ""),
        ("cell_results", {}),
        ("tree", None),
        ("tree", "UNKNOWN"),
        ("tree", "not-an-object-id"),
        ("tree", []),
    ],
)
def test_schema5_unknown_tree_requires_the_exact_legacy_conditions(field: str, value: Any) -> None:
    row = schema5_unknown_tree_rows()[0]
    row[field] = value

    with pytest.raises(build_data.BuildRefused):
        public = build_data._public_validation_row(row, 0)
        build_data._validated_validation_row(public, "validation row")


@pytest.mark.parametrize(("field", "value"), [("schema_version", 4), ("run_id", "stable-run"), ("cell_results", {})])
def test_published_null_tree_requires_the_exact_legacy_conditions(field: str, value: Any) -> None:
    public = build_data._public_validation_row(schema5_unknown_tree_rows()[0], 0)
    public[field] = value

    with pytest.raises(build_data.BuildRefused, match="tree may not be null"):
        build_data._validated_validation_row(public, "validation row")


@pytest.mark.parametrize("mutation", ["missing", "duplicate", "reason", "schema"])
def test_schema5_tree_gap_marker_is_exact(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, mutation: str
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    rows = schema5_unknown_tree_rows()
    install_inputs(monkeypatch, canonical_input(parent_commit, [], validation_rows=rows), manifest_metadata())
    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)
    marker = next(item for item in value["incomplete_data"] if item["field"] == "tree")
    if mutation == "missing":
        value["incomplete_data"].remove(marker)
    elif mutation == "duplicate":
        value["incomplete_data"].append(copy.deepcopy(marker))
    else:
        marker[mutation] = "wrong"
    value["counts"]["incomplete_data_count"] = len(value["incomplete_data"])

    with pytest.raises(build_data.BuildRefused, match="incomplete data disagrees"):
        build_data.validate_build_data(value)


def test_reasoned_verdict_union_preserves_exact_live_shapes_without_inference(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = reasoned_validation_row(hermit_commit)
    cells = [cell for cell, _state, _reason, _history, _current in REASONED_CELLS]
    manifest = manifest_metadata(*cells, OTHER_CELL)
    manifest["selected_by_full_custom_commands"] = [
        item for item in manifest["cells"] if item["mode"] == "custom"
    ]
    manifest["cells"] = [item for item in manifest["cells"] if item["mode"] != "custom"]
    for item in [*manifest["tests"], *manifest["cells"], *manifest["selected_by_full_custom_commands"]]:
        item["category"] = item.get("test", item.get("id", "")).split("/", 1)[0]
    install_inputs(monkeypatch, canonical_input(parent_commit, [], validation_rows=[row]), manifest)

    assert build_data._cell_results_verdict(row) is build_data._modules().qualifying_receipt.CellResultsVerdict.SATISFIED
    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)

    expected = {
        cell: {"state": state, "comparison_tier": "declared-but-unverifiable", "reason": reason}
        for cell, state, reason, _history, _current in REASONED_CELLS
    }
    for name in ("validation_history_rows", "current_validation_run_rows"):
        recorded = value["runs"][name][0]["cell_results"]["recorded"]["cells"]
        assert {
            "/".join(item[field] for field in ("test", "mode", "backend")): item["cell_verdict"]
            for item in recorded
        } == expected
    assert sum(item[3] for item in REASONED_CELLS) == 128
    assert sum(item[4] for item in REASONED_CELLS) == 124
    by_cell = {cell["cell"]: cell for cell in value["cells"]}
    assert set(by_cell[OTHER_CELL]["selected_by"]) == set()
    assert set(by_cell[cells[-1]]["selected_by"]) == {"full"}
    assert [item["cell"] for item in value["selected_by_full_custom_commands"]] == sorted(cells[:3])
    assert build_data.validate_build_data(value) == value


def test_execution_only_verdict_preserves_explicit_null_message_counts() -> None:
    row = validation_row("a" * 40)
    verdict = row["cell_results"]["cells"][0]["cell_verdict"]
    verdict["comparison_tier"] = "execution-only-self-consistent"
    verdict["comparison"]["compare_logs"] = False
    verdict["bitwise_parity"] = False
    verdict["compared_log_messages"] = None

    assert build_data._cell_results_verdict(row) is build_data._modules().qualifying_receipt.CellResultsVerdict.SATISFIED
    public = build_data._public_cell_results(row, "validation row")
    assert public is not None
    assert public["recorded"]["cells"][0]["cell_verdict"] == verdict
    build_data._validated_cell_results(public, row, "validation row")

    verdict["comparison"]["compare_logs"] = True
    with pytest.raises(build_data.BuildRefused):
        build_data._public_cell_results(row, "validation row")


def test_projected_cell_results_reuses_supported_schema_and_nonempty_rules() -> None:
    row = validation_row("a" * 40)
    public = build_data._public_cell_results(row, "validation row")
    assert public is not None
    assert not Path(row["cell_results"]["artifact"]["path"]).is_absolute()
    assert set(public["recorded"]["artifact"]) == {"row_count", "sha256"}

    unsupported = copy.deepcopy(row)
    unsupported["schema_version"] = 999
    with pytest.raises(build_data.BuildRefused, match="schema-unavailable"):
        build_data._validated_cell_results(public, unsupported, "validation row")

    empty = copy.deepcopy(public)
    selected = empty["selected_by"][row["profile"]]
    selected.update(cell_count=0, cells=[])
    empty["recorded"].update(cell_count=0, cells=[])
    empty["recorded"]["artifact"]["row_count"] = 0
    empty["population_sha256"] = (
        build_data._modules().qualifying_receipt.cell_population_sha256([])
    )
    with pytest.raises(build_data.BuildRefused, match="empty"):
        build_data._validated_cell_results(empty, row, "validation row")


@pytest.mark.parametrize("field", ["lane", "category"])
def test_projected_cell_results_reuses_all_five_identity_fields(field: str) -> None:
    row = validation_row("a" * 40)
    public = build_data._public_cell_results(row, "validation row")
    assert public is not None
    selected = public["selected_by"][row["profile"]]["cells"]
    selected[0][field] = "different"
    public["population_sha256"] = (
        build_data._modules().qualifying_receipt.cell_population_sha256(selected)
    )

    with pytest.raises(build_data.BuildRefused, match="population-mismatch"):
        build_data._validated_cell_results(public, row, "validation row")


@pytest.mark.parametrize(
    "mutate",
    [
        lambda verdict: verdict.update(comparison=None),
        lambda verdict: verdict.update(compared_log_messages=None),
        lambda verdict: verdict.update(bitwise_parity=None),
        lambda verdict: verdict.update(extra=True),
        lambda verdict: verdict.pop("reason"),
        lambda verdict: verdict.update(reason=None),
        lambda verdict: verdict.update(reason=""),
        lambda verdict: verdict.update(comparison_tier="canonical-bitwise"),
        lambda verdict: verdict.update(state="compared-and-matched"),
    ],
)
def test_reasoned_verdict_mixed_shapes_nulls_and_unknown_fields_refuse(mutate: Any) -> None:
    row = reasoned_validation_row("a" * 40)
    public = build_data._public_cell_results(row, "validation row")
    assert public is not None
    mutate(row["cell_results"]["cells"][0]["cell_verdict"])

    with pytest.raises(build_data.BuildRefused):
        build_data._public_cell_results(row, "validation row")
    mutate(public["recorded"]["cells"][0]["cell_verdict"])
    with pytest.raises(build_data.BuildRefused):
        build_data._validated_cell_results(public, row, "validation row")


def test_published_nested_unknown_fields_refuse(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    ledger = validation_row(hermit_commit)
    canonical = canonical_input(parent_commit, [row], validation_rows=[ledger])
    install_inputs(monkeypatch, canonical, manifest_metadata())
    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    def history(output: dict[str, Any], change: Any) -> None:
        for name in ("validation_history_rows", "current_validation_run_rows"):
            change(output["runs"][name][0])

    mutations = [
        ("gate", lambda output: history(output, lambda item: item.update(gates=[{"name": "gate", "result": "pass", "extra": True}]))),
        ("artifact", lambda output: history(output, lambda item: item["cell_results"]["recorded"]["artifact"].update(extra=True))),
        ("recorded identity", lambda output: history(output, lambda item: item["cell_results"]["recorded"]["cells"][0].update(extra=True))),
        ("comparison", lambda output: history(output, lambda item: item["cell_results"]["recorded"]["cells"][0]["cell_verdict"]["comparison"].update(extra=True))),
        ("message counts", lambda output: history(output, lambda item: item["cell_results"]["recorded"]["cells"][0]["cell_verdict"]["compared_log_messages"].update(extra=True))),
        ("selected-by fact", lambda output: history(output, lambda item: item["cell_results"]["selected_by"]["quick"].update(extra=True))),
        ("selected identity", lambda output: history(output, lambda item: item["cell_results"]["selected_by"]["quick"]["cells"][0].update(extra=True))),
        ("series payload", lambda output: output["runs"]["series_rows"][0]["series"].update(extra=True)),
        ("runtime", lambda output: output["runs"]["series_rows"][0]["series"].update(runtime={"extra": True})),
        ("coordinates", lambda output: output["runs"]["series_rows"][0]["series"].update(coordinates={"extra": True})),
        ("depth", lambda output: output["runs"]["series_rows"][0]["series"].update(depth={"hermit": {"commits": 1, "first_parent": 1, "extra": True}})),
        ("host capabilities", lambda output: output["runs"]["series_rows"][0]["series"].update(host_capabilities={"kvm": {"present": True, "evidence": "fixture", "extra": True}})),
        ("attempt evidence", lambda output: output["runs"]["series_rows"][0]["series"].update(no_verdict_evidence={"attempts": [{"extra": True}], "evidence_sha256": "a" * 64})),
    ]
    for label, mutate in mutations:
        candidate = copy.deepcopy(value)
        mutate(candidate)
        try:
            build_data.validate_build_data(candidate)
        except build_data.BuildRefused:
            pass
        else:
            pytest.fail(f"published {label} accepted an unknown field")


def test_published_join_count_type_and_provenance_mutations_refuse(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    ledger = validation_row(hermit_commit)
    install_inputs(
        monkeypatch,
        canonical_input(parent_commit, [row], validation_rows=[ledger]),
        manifest_metadata(),
    )
    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    def history(output: dict[str, Any], change: Any) -> None:
        for name in ("validation_history_rows", "current_validation_run_rows"):
            change(output["runs"][name][0])

    def no_verdict(output: dict[str, Any], status: int | None, signal: int | None) -> None:
        series = output["runs"]["series_rows"][0]
        series["product_result"] = None
        series["series"].update(
            attempt=1, failure_class="no_result", outcome="no_result", result=None,
            no_verdict_evidence={"attempts": [{"attempt_outcome": "ERROR", "disposition": "no_result", "error_kind": None, "index": "attempt-1", "kind": "not_run", "signal": signal, "status": status, "timed_out": False, "verification_report_sha256": None}], "evidence_sha256": "a" * 64},
        )

    mutations = [
        ("missing ancestry", lambda output: output["runs"]["current_main_ancestry"].clear()),
        ("extra ancestry", lambda output: output["runs"]["current_main_ancestry"].update({"f" * 40: None})),
        ("missing digest", lambda output: output["provenance"].update(missing_executions_sha256="0" * 64)),
        ("cell_results run", lambda output: history(output, lambda item: item["cell_results"].update(run_id="other"))),
        ("cell_results commit", lambda output: history(output, lambda item: item["cell_results"].update(hermit_sha="f" * 40))),
        ("selected count", lambda output: history(output, lambda item: item["cell_results"]["selected_by"]["quick"].update(cell_count=2))),
        ("artifact count", lambda output: history(output, lambda item: item["cell_results"]["recorded"]["artifact"].update(row_count=2))),
        ("selection path", lambda output: history(output, lambda item: item["cell_results"]["selected_by"].update(full=item["cell_results"]["selected_by"].pop("quick")))),
        ("validation commit", lambda output: history(output, lambda item: item.update(commit="bad"))),
        ("product availability", lambda output: output["runs"]["series_rows"][0].update(product_result_available="yes")),
        ("series schema", lambda output: output["runs"]["series_rows"][0].update(schema="foreign/v1")),
        ("series event type", lambda output: output["runs"]["series_rows"][0].update(event_type="other")),
        ("series team", lambda output: output["runs"]["series_rows"][0].update(team="other")),
        ("series host", lambda output: output["runs"]["series_rows"][0].update(host="other-host")),
        ("series product pair", lambda output: output["runs"]["series_rows"][0]["series"].update(outcome="diverged")),
        ("missing host capability", lambda output: output["runs"]["series_rows"][0]["series"]["host_capabilities"].pop("kvm")),
        ("source tree dirty type", lambda output: output["runs"]["series_rows"][0]["series"].update(source_tree_dirty="no")),
        ("attempt ordinal type", lambda output: output["runs"]["series_rows"][0]["series"].update(attempt=True)),
        ("detcore tree type", lambda output: output["runs"]["series_rows"][0]["series"].update(detcore_tree="bad")),
        ("cell state", lambda output: output["cells"][0].update(state="red")),
        ("latest result", lambda output: output["cells"][0].update(latest_result="product_failure")),
        ("incomplete value", lambda output: output["incomplete_data"][0].update(field=[])),
        ("count bool", lambda output: output["counts"].update(physical_row_count=True)),
        ("validation tree type", lambda output: history(output, lambda item: item.update(tree=[]))),
        ("validation bool type", lambda output: history(output, lambda item: item.update(tree_dirty="no"))),
        ("validation count type", lambda output: history(output, lambda item: item.update(executed_nodes="1"))),
        ("gate exit bool", lambda output: history(output, lambda item: item.update(gates=[{"name": "gate", "result": "fail", "exit_code": True}]))),
        ("top schema bool", lambda output: output.update(schema_version=True)),
        ("runs schema bool", lambda output: output["runs"].update(schema_version=True)),
        ("provenance schema bool", lambda output: output["provenance"].update(build_data_schema_version=True)),
        ("series ancestry integer", lambda output: output["runs"]["series_rows"][0].update(current_main_ancestry=1)),
        ("cell physical count bool", lambda output: output["cells"][0].update(physical_row_count=True)),
        ("cell represented count bool", lambda output: output["cells"][0].update(represented_run_count=True)),
        ("cell qualifying physical bool", lambda output: output["cells"][0].update(qualifying_physical_row_count=True)),
        ("cell qualifying represented bool", lambda output: output["cells"][0].update(qualifying_represented_run_count=True)),
        ("series failure class type", lambda output: output["runs"]["series_rows"][0]["series"].update(failure_class=[])),
        ("series result type", lambda output: output["runs"]["series_rows"][0]["series"].update(result=[])),
        ("kernel version type", lambda output: output["runs"]["series_rows"][0]["series"].update(kernel_version=1)),
        ("runtime value type", lambda output: output["runs"]["series_rows"][0]["series"].update(runtime={"wall_time_min_ms": "1"})),
        ("runtime ordering", lambda output: output["runs"]["series_rows"][0]["series"].update(runtime={"wall_time_min_ms": 10, "wall_time_max_ms": 9})),
        ("depth zero", lambda output: output["runs"]["series_rows"][0]["series"].update(depth={"hermit": {"commits": 10, "first_parent": 0}})),
        ("coordinate value type", lambda output: output["runs"]["series_rows"][0]["series"].update(coordinates={"first_divergent_record": "1"})),
        ("attempt evidence type", lambda output: output["runs"]["series_rows"][0]["series"].update(no_verdict_evidence={"attempts": [{"attempt_outcome": "ERROR", "disposition": "timeout", "error_kind": None, "index": "attempt-1", "kind": "not_run", "signal": None, "status": None, "timed_out": True, "verification_report_sha256": None}], "evidence_sha256": "a" * 64})),
        ("attempt status and signal", lambda output: no_verdict(output, 1, 9)),
        ("attempt zero signal", lambda output: no_verdict(output, None, 0)),
    ]
    accepted = []
    for label, mutate in mutations:
        candidate = copy.deepcopy(value)
        mutate(candidate)
        try:
            build_data.validate_build_data(candidate)
        except build_data.BuildRefused:
            continue
        accepted.append(label)
    assert accepted == []


def test_published_runtime_floats_and_signed_gate_exit_codes_are_retained(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    row["series"]["runtime"] = {
        "wall_time_min_ms": 1.25,
        "wall_time_max_ms": 1.5,
    }
    ledger = validation_row(hermit_commit)
    gate = {"aborted": True, "exit_code": -15, "name": "gate", "result": "fail"}
    ledger["gates"] = [gate]
    install_inputs(monkeypatch, canonical_input(parent_commit, [row], validation_rows=[ledger]), manifest_metadata())
    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)

    validated = build_data.validate_build_data(value)

    assert validated["runs"]["series_rows"][0]["series"]["runtime"] == value["runs"]["series_rows"][0]["series"]["runtime"]
    assert validated["runs"]["validation_history_rows"][0]["gates"] == [gate]


def test_v1_and_v2_product_results_remain_unavailable(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    rows = [
        series_row(
            "legacy",
            hermit_commit,
            schema="stress-series/v1",
                producer="pressure-test",
                run_id="legacy",
                num_runs=3,
                result=None,
        ),
        series_row(
            "host-facts",
            hermit_commit,
            schema="stress-series/v2",
                producer="pressure-test",
                run_id="host-facts",
                num_runs=4,
                result=None,
        ),
    ]
    projection = {
        "legacy": "stress-series/v1 lacks required host facts",
        "host-facts": None,
    }
    canonical = canonical_input(parent_commit, rows, projection=projection)
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert build_data._product_results(rows) == {}
    assert value["counts"]["physical_row_count"] == 2
    assert value["counts"]["represented_run_count"] == 7
    assert value["runs"]["projection_excluded_physical_row_count"] == 1
    assert value["runs"]["projection_excluded_represented_run_count"] == 3
    assert value["counts"]["state_unavailable_cell_count"] == 1
    assert any(
        item["field"] == "product result" and item.get("event_id") == "host-facts"
        for item in value["incomplete_data"]
    )


@pytest.mark.parametrize(
    ("outcome", "result", "failure_class", "expected"),
    [
        ("passed", "pass", None, "pass"),
        ("diverged", "determinism-failure", "product_failure", "product_failure"),
        ("diverged", "parity-failure", "product_failure", "product_failure"),
        ("diverged", "replay-failure", "product_failure", "product_failure"),
        ("errored", "crash-error", "product_failure", "product_failure"),
        ("timeout", "timeout", "no_result", None),
        ("no_result", "oom", "no_result", None),
        ("errored", None, "understood_infrastructure_failure", None),
        ("skipped", None, "understood_prerequisite_failure", None),
        ("no_result", None, "no_result", None),
    ],
)
def test_exact_v3_product_pairs_are_mapped(
    outcome: str,
    result: str | None,
    failure_class: str | None,
    expected: str | None,
) -> None:
    row = series_row(
        "event",
        "a" * 40,
        outcome=outcome,
        result=result,
        failure_class=failure_class,
    )
    assert build_data._product_results([row]) == {"event": expected}


def test_unknown_or_contradictory_v3_product_pair_refuses() -> None:
    row = series_row(
        "event",
        "a" * 40,
        outcome="passed",
        result="pass",
        failure_class="product_failure",
    )
    with pytest.raises(build_data.BuildRefused, match="unknown or contradictory"):
        build_data._product_results([row])


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (
            lambda value: value["series"]["projection_results"].pop("event-1"),
            "omitted event_id",
        ),
        (
            lambda value: value["series"]["projection_results"].update(
                {"unknown": None}
            ),
            "unknown event_id",
        ),
        (
            lambda value: value["series"].update(
                projection_result_physical_row_count=0
            ),
            "physical_row_count disagrees",
        ),
        (
            lambda value: value["series"].update(rows_sha256="0" * 64),
            "rows_sha256 disagrees",
        ),
        (
            lambda value: value["series"].update(
                projection_results_sha256="0" * 64
            ),
            "projection_results_sha256 disagrees",
        ),
        (
            lambda value: value["series"]["projection_results_binding"].update(
                checker_input_jsonl_sha256="not-a-digest"
            ),
            "checker_input_jsonl_sha256",
        ),
        (
            lambda value: value["series"]["projection_results_binding"].update(
                checker_input_jsonl_sha256="0" * 64
            ),
            "checker_input_jsonl_sha256 disagrees",
        ),
        (
            lambda value: value["series"].update(
                projection_admitted_physical_row_count=1
            ),
            "admitted_physical_row_count disagrees",
        ),
        (
            lambda value: value["series"].update(projection_exclusion_counts=[]),
            "projection_exclusion_counts disagrees",
        ),
    ],
)
def test_projection_binding_mutations_refuse(mutate, message: str) -> None:
    row = series_row("event-1", "a" * 40)
    value = canonical_input("b" * 40, [row], projection={"event-1": "excluded"})
    mutate(value)
    with pytest.raises(build_data.BuildRefused, match=message):
        build_data._validate_projection(value)


@pytest.mark.parametrize(
    ("mutate", "message"),
    [
        (
            lambda value: value["provenance"].update(published_ref="0" * 40),
            "must equal the requested parent commit",
        ),
        (
            lambda value: value["provenance"].update(reader_commit="0" * 40),
            "must equal the requested parent commit",
        ),
        (
            lambda value: value["provenance"].pop("reader_files_sha256"),
            "missing required field",
        ),
        (
            lambda value: value["provenance"].update(unexpected=True),
            "unknown field",
        ),
        (
            lambda value: value["provenance"]["reader_hermit"].update(
                unexpected=True
            ),
            "reader_hermit has unknown field",
        ),
        (
            lambda value: value["provenance"]["reader_files"][0].update(
                unexpected=True
            ),
            r"reader_files\[0\] has unknown field",
        ),
        (
            lambda value: value["provenance"]["sources"][0].update(
                unexpected=True
            ),
            r"sources\[0\] has unknown field",
        ),
        (
            lambda value: value["provenance"].update(
                reader_files_sha256="0" * 64
            ),
            "reader_files_sha256 disagrees",
        ),
        (
            lambda value: value["provenance"].update(sources_sha256="0" * 64),
            "sources_sha256 disagrees",
        ),
        (
            lambda value: value["provenance"].update(source_count=0),
            "source_count disagrees",
        ),
        (
            lambda value: value["provenance"].update(
                reader_ci_hub_tree="not-an-object"
            ),
            "reader_ci_hub_tree must be",
        ),
        (
            lambda value: value["provenance"]["reader_hermit"].update(
                checkout_commit="0" * 40
            ),
            "checkout_commit and gitlink_commit disagree",
        ),
        (
            lambda value: value["provenance"]["reader_files"].append(
                copy.deepcopy(value["provenance"]["reader_files"][0])
            ),
            "repeats path",
        ),
        (
            lambda value: value["provenance"]["sources"].append(
                copy.deepcopy(value["provenance"]["sources"][0])
            ),
            "repeats identity",
        ),
        (
            lambda value: value["provenance"]["sources"][0].update(
                git_object="not-an-object"
            ),
            "git_object must be",
        ),
        (
            lambda value: value["provenance"]["sources"][0].update(
                path="/home/example/series.jsonl"
            ),
            "safe relative path",
        ),
    ],
)
def test_canonical_input_provenance_mutations_refuse(mutate, message: str) -> None:
    parent_commit = "b" * 40
    value = canonical_input(parent_commit, [])
    mutate(value)
    with pytest.raises(build_data.BuildRefused, match=message):
        build_data._canonical_provenance(value["provenance"], parent_commit)


def test_git_environment_drops_untrusted_git_overrides(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GIT_CONFIG_COUNT", "1")
    monkeypatch.setenv("GIT_CONFIG_KEY_0", "core.hooksPath")
    monkeypatch.setenv("GIT_CONFIG_VALUE_0", "/untrusted")
    monkeypatch.setenv("GIT_OBJECT_DIRECTORY", "/untrusted/objects")

    environment = build_data._git_environment()

    assert "GIT_CONFIG_COUNT" not in environment
    assert "GIT_CONFIG_KEY_0" not in environment
    assert "GIT_CONFIG_VALUE_0" not in environment
    assert "GIT_OBJECT_DIRECTORY" not in environment
    assert environment["GIT_CONFIG_NOSYSTEM"] == "1"
    assert environment["GIT_CONFIG_GLOBAL"] == os.devnull
    assert environment["GIT_NO_LAZY_FETCH"] == "1"


def test_adapter_capture_delegates_to_the_shared_helper(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    calls: list[tuple[list[str], dict[str, Any]]] = []

    def capture(command: Sequence[str], **kwargs: Any):
        calls.append((list(command), kwargs))
        return subprocess.CompletedProcess(list(command), 0, b"out", b"err")

    monkeypatch.setattr(build_data._modules().bounded_capture, "capture", capture)

    result = build_data._capture(
        ["fixture", "argument"],
        timeout=7,
        cwd=tmp_path,
        env={"FIXTURE": "1"},
    )

    assert result.stdout == b"out"
    assert calls == [
        (
            ["fixture", "argument"],
            {
                "check": False,
                "cwd": tmp_path,
                "env": {"FIXTURE": "1"},
                "text": False,
                "timeout": 7,
            },
        )
    ]


def test_checkout_cleanliness_cannot_ignore_submodules(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    checkout = tmp_path / "checkout"
    checkout.mkdir()
    expected_commit = "a" * 40
    calls: list[list[str]] = []

    def git_text(_checkout: Path, arguments: Sequence[str]) -> str:
        if arguments == ["rev-parse", "--show-toplevel"]:
            return str(checkout)
        assert arguments == ["rev-parse", "--verify", "HEAD^{commit}"]
        return expected_commit

    def git_call(
        _checkout: Path,
        arguments: Sequence[str],
        *,
        accepted_returncodes: frozenset[int] = frozenset({0}),
    ) -> subprocess.CompletedProcess[bytes]:
        del accepted_returncodes
        calls.append(list(arguments))
        return subprocess.CompletedProcess([], 0, b"", b"")

    monkeypatch.setattr(build_data, "_git_text", git_text)
    monkeypatch.setattr(build_data, "_git", git_call)

    assert build_data._require_checkout(
        checkout, expected_commit, label="fixture", clean=True
    ) == checkout
    assert calls == [
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ]
    ]


def test_duplicate_series_event_id_refuses() -> None:
    rows = [series_row("same", "a" * 40), series_row("same", "a" * 40)]
    value = canonical_input("b" * 40, rows, projection={"same": None})
    value["series"]["projection_result_physical_row_count"] = 2
    with pytest.raises(build_data.BuildRefused, match="duplicate event_id"):
        build_data._validate_projection(value)


def test_excluded_validate_series_without_history_stays_auditable(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    rows = unmatched_legacy_fixture_rows(hermit_commit)
    canonical = canonical_input(
        parent_commit, rows,
        projection={row["event_id"]: LEGACY_SERIES_REASON for row in rows},
    )
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert {
        row["projection_validation_reason"] for row in value["runs"]["series_rows"]
    } == {LEGACY_SERIES_REASON}
    assert value["runs"]["series_physical_row_count"] == 2
    assert value["runs"]["series_represented_run_count"] == 4
    event_markers = {
        item["field"]: item for item in value["incomplete_data"]
        if item.get("event_id") == "legacy-event"
    }
    assert event_markers["qualifying row"]["reason"] == LEGACY_SERIES_REASON
    assert set(event_markers) == {"qualifying row", "validate path selection"}
    assert any(
        item["field"] == "current manifest join" and item.get("cell") == OTHER_CELL
        for item in value["incomplete_data"]
    )
    assert build_data.validate_build_data(value) == value


@pytest.mark.parametrize(
    ("field", "mutation"),
    [
        ("qualifying row", "remove"),
        ("qualifying row", "change"),
        ("validate path selection", "remove"),
        ("current manifest join", "remove"),
    ],
)
def test_unmatched_legacy_series_requires_exact_reducer_markers(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, field: str, mutation: str
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    rows = unmatched_legacy_fixture_rows(hermit_commit)
    canonical = canonical_input(
        parent_commit, rows,
        projection={row["event_id"]: LEGACY_SERIES_REASON for row in rows},
    )
    install_inputs(monkeypatch, canonical, manifest_metadata())
    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )
    marker = next(
        item for item in value["incomplete_data"]
        if item["field"] == field
        and (field == "current manifest join" or item.get("event_id") == "legacy-event")
    )
    if mutation == "remove":
        value["incomplete_data"].remove(marker)
    else:
        marker["reason"] = "wrong"
    value["counts"]["incomplete_data_count"] = len(value["incomplete_data"])

    with pytest.raises(build_data.BuildRefused, match="incomplete data disagrees"):
        build_data.validate_build_data(value)


def test_unmatched_validate_series_refuses_admission_or_current_selection(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row(
        "legacy-event", hermit_commit,
        schema="stress-series/v1", run_id="legacy-run",
    )
    install_inputs(monkeypatch, canonical_input(parent_commit, [row]), manifest_metadata())
    with pytest.raises(build_data.BuildRefused, match="no validation history row"):
        build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)

    selected = validation_row("b" * 40, run_id="legacy-run")
    canonical = canonical_input(
        parent_commit, [row], validation_rows=[selected],
        projection={"legacy-event": LEGACY_SERIES_REASON},
    )
    install_inputs(monkeypatch, canonical, manifest_metadata())
    with pytest.raises(build_data.BuildRefused, match="records Hermit tree"):
        build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)


def test_unmatched_legacy_series_preserves_exact_live_aggregate(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    rows = unmatched_legacy_series_rows()
    projection = {row["event_id"]: LEGACY_SERIES_REASON for row in rows}
    monkeypatch.setattr(
        build_data, "_main_ancestry",
        lambda _hermit, _main, trees: dict.fromkeys(sorted(set(trees)), True),
    )
    install_inputs(monkeypatch, canonical_input(parent_commit, rows, projection=projection), manifest_metadata())

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    published = value["runs"]["series_rows"]
    assert len(published) == value["runs"]["series_physical_row_count"] == 1094
    assert (
        sum(row["series"]["num_runs"] for row in published)
        == value["runs"]["series_represented_run_count"]
        == 1244
    )
    assert {row["event_id"]: row["series"]["num_runs"] for row in published} == {
        row["event_id"]: row["series"]["num_runs"] for row in rows
    }
    assert {row["projection_validation_reason"] for row in published} == {LEGACY_SERIES_REASON}
    assert value["cells"][0]["selected_by"] == {}
    observed = []
    for run_id, tree, _physical, _represented in LEGACY_SERIES_GROUPS:
        group = [
            row for row in published
            if (row["run_id"], row["series"]["tree"]) == (run_id, tree)
        ]
        observed.append((len(group), sum(row["series"]["num_runs"] for row in group)))
    assert observed == [
        (physical, represented)
        for _run, _tree, physical, represented in LEGACY_SERIES_GROUPS
    ]

    shorter_rows = rows[1:]
    shorter_projection = {row["event_id"]: LEGACY_SERIES_REASON for row in shorter_rows}
    install_inputs(
        monkeypatch,
        canonical_input(parent_commit, shorter_rows, projection=shorter_projection),
        manifest_metadata(),
    )
    shorter = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )
    assert (shorter["runs"]["series_physical_row_count"], shorter["runs"]["series_represented_run_count"]) == (1093, 1141)
    assert shorter["provenance"]["series_rows_sha256"] != value["provenance"]["series_rows_sha256"]
    assert build_data.validate_build_data(shorter) == shorter


def test_absent_selection_stays_incomplete(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    ledger = {
        "schema_version": 1,
        "repo": "hermit",
        "commit": hermit_commit,
        "run_id": "run-1",
    }
    canonical = canonical_input(parent_commit, [row], validation_rows=[ledger])
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert value["cells"][0]["selected_by"] == {}
    assert any(
        item["field"] == "validate path selection"
        and item.get("event_id") == "event-1"
        for item in value["incomplete_data"]
    )


def test_selected_without_series_becomes_exact_missing_execution(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    ledger = validation_row(
        hermit_commit,
        profile="cell-requalification",
        selection_mode="targeted",
        selected=[OTHER_CELL],
    )
    canonical = canonical_input(parent_commit, [], validation_rows=[ledger])
    install_inputs(monkeypatch, canonical, manifest_metadata(OTHER_CELL))

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert value["cells"][0]["selected_by"] == {
        "cell-requalification": [
            {
                "event_ids": [],
                "observed_at": None,
                "run_id": "run-1",
                "tree": hermit_commit,
            }
        ]
    }
    assert any(
        item["field"] == "series row"
        and item["path"] == "cell-requalification"
        and item["cell"] == OTHER_CELL
        for item in value["incomplete_data"]
    )
    assert value["runs"]["current_main_ancestry"] == {hermit_commit: True}
    assert "missing_executions" not in value["runs"]


def test_validate_retry_rows_for_one_selected_cell_remain_auditable(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    rows = [
        series_row("retry-1", hermit_commit, run_index=1),
        series_row("retry-2", hermit_commit, run_index=2),
    ]
    ledger = validation_row(hermit_commit)
    install_inputs(monkeypatch, canonical_input(parent_commit, rows, validation_rows=[ledger]), manifest_metadata())

    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)

    selected = value["cells"][0]["selected_by"]["quick"]
    assert selected[0]["event_ids"] == ["retry-1", "retry-2"]
    assert value["counts"]["physical_row_count"] == 2
    assert not any(item["field"] == "series row" for item in value["incomplete_data"])


def test_selected_custom_command_stays_separate_and_auditable(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    custom = "tests/custom/custom/ptrace"
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    ledger = validation_row(hermit_commit, selected=[custom])
    manifest = manifest_metadata(custom)
    manifest["selected_by_full_custom_commands"] = [manifest["cells"].pop()]
    install_inputs(
        monkeypatch,
        canonical_input(parent_commit, [], validation_rows=[ledger]),
        manifest,
    )

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert value["cells"] == []
    assert [item["cell"] for item in value["selected_by_full_custom_commands"]] == [custom]
    assert value["counts"]["selected_by_full_custom_command_count"] == 1
    assert any(item.get("cell") == custom for item in value["incomplete_data"])


def test_validate_path_cannot_publish_an_owner_path(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    ledger = validation_row(
        hermit_commit,
        profile="/home/newton/private-path",
    )
    canonical = canonical_input(parent_commit, [row], validation_rows=[ledger])
    install_inputs(monkeypatch, canonical, manifest_metadata())

    with pytest.raises(build_data.BuildRefused, match="absolute owner path"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_wrong_tree_and_unselected_series_refuse(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    ledger = validation_row(hermit_commit)

    wrong_tree = copy.deepcopy(ledger)
    wrong_tree["commit"] = "e" * 40
    wrong_tree["cell_results"]["hermit_sha"] = "e" * 40
    canonical = canonical_input(parent_commit, [row], validation_rows=[wrong_tree])
    install_inputs(monkeypatch, canonical, manifest_metadata())
    with pytest.raises(build_data.BuildRefused, match="records Hermit tree"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )

    unselected = validation_row(hermit_commit, selected=[OTHER_CELL])
    canonical = canonical_input(parent_commit, [row], validation_rows=[unselected])
    install_inputs(monkeypatch, canonical, manifest_metadata(CELL, OTHER_CELL))
    with pytest.raises(build_data.BuildRefused, match="did not select"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_duplicate_or_unsatisfied_current_validation_row_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    one = validation_row(hermit_commit)
    two = validation_row(hermit_commit, record_id="record-2")
    canonical = canonical_input(parent_commit, [row], validation_rows=[one, two])
    install_inputs(monkeypatch, canonical, manifest_metadata())
    with pytest.raises(build_data.BuildRefused, match="duplicate run_id"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )

    canonical = canonical_input(parent_commit, [row], validation_rows=[one])
    install_inputs(monkeypatch, canonical, manifest_metadata())
    monkeypatch.setattr(
        build_data,
        "_require_satisfied_cell_results",
        lambda _row: (_ for _ in ()).throw(
            build_data.BuildRefused("canonical cell_results verdict is malformed")
        ),
    )
    with pytest.raises(build_data.BuildRefused, match="verdict is malformed"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_malformed_current_schema_cell_results_refuses_through_canonical_verdict(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    invalid = validation_row(hermit_commit)
    invalid["schema_version"] = 6
    invalid["cell_results"]["cells"][0]["cell_verdict"] = {"state": "fixture"}
    canonical = canonical_input(parent_commit, [row], validation_rows=[invalid])
    monkeypatch.setattr(
        build_data,
        "_capture_canonical_input",
        lambda _parent, _commit: captured(canonical),
    )
    monkeypatch.setattr(
        build_data,
        "_run_manifest_metadata",
        lambda _hermit: captured(manifest_metadata()),
    )

    with pytest.raises(build_data.BuildRefused, match="cell-verdict-invalid"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_cell_results_uses_the_canonical_verdict(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen: list[tuple[dict[str, Any], dict[str, Any]]] = []
    predicate = {"cell_results": {"fixture": True}}
    qualifying_receipt = build_data._modules().qualifying_receipt
    monkeypatch.setattr(qualifying_receipt, "active", lambda: predicate)

    def verdict(row: dict[str, Any], supplied: dict[str, Any]):
        seen.append((row, supplied))
        return qualifying_receipt.CellResultsVerdict.SATISFIED

    monkeypatch.setattr(qualifying_receipt, "cell_results_verdict", verdict)
    row = {"record_id": "record"}
    build_data._require_satisfied_cell_results(row)
    assert seen == [(row, predicate)]

    monkeypatch.setattr(
        qualifying_receipt,
        "cell_results_verdict",
        lambda *_args: qualifying_receipt.CellResultsVerdict.MALFORMED,
    )
    with pytest.raises(build_data.BuildRefused, match="verdict is malformed"):
        build_data._require_satisfied_cell_results(row)


def test_current_validation_digest_mutation_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    canonical = canonical_input(
        parent_commit, [row], validation_rows=[validation_row(hermit_commit)]
    )
    canonical["validation_ledger"]["current_validation_run_rows_sha256"] = "0" * 64
    install_inputs(monkeypatch, canonical, manifest_metadata())
    with pytest.raises(build_data.BuildRefused, match="sha256 disagrees"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_fixture_only_qualifying_predicate_override_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    monkeypatch.setenv("QUALIFYING_RECEIPT_PREDICATE", str(tmp_path / "fixture.json"))
    with pytest.raises(build_data.BuildRefused, match="fixture-only override"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_unavailable_historical_object_stays_incomplete(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    unavailable = "f" * 40
    row = series_row(
        "event-1",
        unavailable,
        producer="pressure-test",
        run_id="pressure",
    )
    canonical = canonical_input(parent_commit, [row])
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert value["cells"][0]["state"] is None
    assert any(
        item["field"] == "current-main ancestry"
        and item.get("event_id") == "event-1"
        for item in value["incomplete_data"]
    )


def test_annotated_tag_object_cannot_stand_in_for_a_series_commit(
    tmp_path: Path,
) -> None:
    hermit, hermit_commit = repository(tmp_path, "hermit")
    git(hermit, "tag", "-a", "fixture-tag", "-m", "fixture tag")
    tag_object = git(hermit, "rev-parse", "refs/tags/fixture-tag")

    with pytest.raises(build_data.BuildRefused, match="not a commit object"):
        build_data._main_ancestry(hermit, hermit_commit, [tag_object])


def test_shallow_commit_graph_refuses_ancestry_instead_of_false(
    tmp_path: Path,
) -> None:
    origin, old_commit = repository(tmp_path, "origin")
    (origin / "tracked.txt").write_text("new\n")
    git(origin, "add", "tracked.txt")
    git(origin, "commit", "-m", "new")
    main_commit = git(origin, "rev-parse", "HEAD")
    shallow = tmp_path / "shallow"
    build_data._modules().bounded_capture.capture(
        [
            "git",
            "clone",
            "--quiet",
            "--depth=1",
            origin.as_uri(),
            str(shallow),
        ],
        check=True,
        timeout=60,
    )
    git(shallow, "fetch", "--quiet", "--depth=1", "origin", old_commit)
    assert git(shallow, "cat-file", "-t", old_commit) == "commit"

    with pytest.raises(build_data.BuildRefused, match="shallow commit graph"):
        build_data._main_ancestry(shallow, main_commit, [old_commit])


def test_incomplete_nonshallow_commit_graph_refuses_ancestry(
    tmp_path: Path,
) -> None:
    hermit, main_commit = repository(tmp_path, "hermit")
    tree = git(hermit, "show", "-s", "--format=%T", main_commit)
    missing_parent = "f" * 40
    commit_body = (
        f"tree {tree}\n"
        f"parent {missing_parent}\n"
        "author Fixture <fixture@example.invalid> 0 +0000\n"
        "committer Fixture <fixture@example.invalid> 0 +0000\n"
        "\n"
        "incomplete graph fixture\n"
    )
    result = build_data._modules().bounded_capture.git(
        ["hash-object", "-t", "commit", "-w", "--stdin"],
        cwd=hermit,
        input=commit_body,
        check=True,
        timeout=60,
    )
    broken_commit = result.stdout.strip()
    assert git(hermit, "cat-file", "-t", broken_commit) == "commit"

    with pytest.raises(build_data.BuildRefused, match="commit graph is incomplete"):
        build_data._main_ancestry(hermit, main_commit, [broken_commit])


def test_object_type_batch_malformed_output_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    hermit, main_commit = repository(tmp_path, "hermit")
    real_capture = build_data._capture

    def capture(command: Sequence[str], **kwargs: Any):
        if "cat-file" in command:
            return subprocess.CompletedProcess(command, 0, b"malformed\n", b"")
        return real_capture(command, **kwargs)

    monkeypatch.setattr(build_data, "_capture", capture)
    with pytest.raises(build_data.BuildRefused, match="malformed object type line"):
        build_data._main_ancestry(hermit, main_commit, [main_commit])


def test_object_type_batch_operational_failure_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    hermit, main_commit = repository(tmp_path, "hermit")
    real_capture = build_data._capture

    def capture(command: Sequence[str], **kwargs: Any):
        if "cat-file" in command:
            return subprocess.CompletedProcess(
                command, 128, b"", b"permission denied"
            )
        return real_capture(command, **kwargs)

    monkeypatch.setattr(build_data, "_capture", capture)
    with pytest.raises(build_data.BuildRefused, match="permission denied"):
        build_data._main_ancestry(hermit, main_commit, [main_commit])


def test_ancestry_uses_explicit_main_not_the_manifest_exporter_commit(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit = repository(tmp_path, "parent")
    hermit, common_base = repository(tmp_path, "hermit")
    (hermit / "tracked.txt").write_text("manifest exporter\n")
    git(hermit, "add", "tracked.txt")
    git(hermit, "commit", "-m", "manifest exporter")
    hermit_commit = git(hermit, "rev-parse", "HEAD")
    git(hermit, "checkout", "--detach", common_base)
    (hermit / "tracked.txt").write_text("current main\n")
    git(hermit, "add", "tracked.txt")
    git(hermit, "commit", "-m", "current main")
    hermit_main_commit = git(hermit, "rev-parse", "HEAD")
    git(hermit, "update-ref", "refs/remotes/origin/main", hermit_main_commit)
    git(hermit, "checkout", "--detach", hermit_commit)
    row = series_row(
        "event-1",
        hermit_commit,
        producer="pressure-test",
        run_id="pressure",
    )
    canonical = canonical_input(parent_commit, [row])
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(
        parent,
        parent_commit,
        hermit,
        hermit_commit,
        hermit_main_commit,
    )

    assert value["provenance"]["hermit_commit"] == hermit_commit
    assert value["provenance"]["hermit_main_commit"] == hermit_main_commit
    assert value["cells"][0]["state"] == "never-measured"
    assert any(
        item["field"] == "current-main ancestry"
        and "not an ancestor" in item["reason"]
        for item in value["incomplete_data"]
    )


def test_legacy_current_rows_without_stable_identity_are_grouped_and_retained(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    legacy = {
        "schema_version": 1,
        "profile": "full",
        "commit": hermit_commit,
        "cell_results": None,
    }
    other_legacy = {**legacy, "commit": "e" * 40, "profile": "only-portable"}
    unscoped = validation_row(hermit_commit, run_id="unscoped")
    del unscoped["repo"]
    row = series_row("event-1", hermit_commit)
    matching = {
        "schema_version": 1,
        "repo": "hermit",
        "commit": hermit_commit,
        "run_id": "run-1",
    }
    canonical = canonical_input(
        parent_commit, [row], validation_rows=[legacy, other_legacy, unscoped, matching]
    )
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert value["cells"][0]["selected_by"] == {}
    assert any(
        item["field"] == "validate path selection"
        for item in value["incomplete_data"]
    )
    repo_gaps = [item for item in value["incomplete_data"] if item["field"] == "repo"]
    assert repo_gaps == [
        {"field": "repo", "input": "validation ledger", "reason": "historical validation rows have no stable repo", "schema": schema}
        for schema in ("1", "7")
    ]
    assert value["runs"]["validation_history_row_count"] == 4
    assert value["provenance"]["validation_history_rows_sha256"] == canonical["validation_ledger"]["history_rows_sha256"]
    assert not any(item.get("run_id") == "unscoped" and item["field"] == "series row" for item in value["incomplete_data"])


def test_exact_cell_results_gaps_retain_history_but_not_selection(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    retired = [unavailable_schema6_row(hermit_commit, suffix) for suffix in ("one", "two")]
    current = validation_row(hermit_commit)
    canonical = canonical_input(
        parent_commit,
        [series_row("event-1", hermit_commit)],
        validation_rows=[*retired, current],
        current_validation_rows=[current],
        incomplete_data=[cell_results_gap(row) for row in retired],
    )
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)

    by_record = {row["record_id"]: row for row in value["runs"]["validation_history_rows"]}
    assert all(by_record[row["record_id"]]["cell_results"] is None for row in retired)
    assert value["runs"]["current_validation_run_row_count"] == 1
    assert value["cells"][0]["selected_by"]["quick"][0]["event_ids"] == ["event-1"]
    assert all(cell_results_gap(row) in value["incomplete_data"] for row in retired)


def test_cell_results_gap_preserves_and_binds_the_exact_source_reason(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    retired = unavailable_schema6_row(hermit_commit, "one")
    current = validation_row(hermit_commit)
    marker = cell_results_gap(retired)
    marker["reason"] = "the source records this exact schema-6 limitation"
    canonical = canonical_input(
        parent_commit,
        [series_row("event-1", hermit_commit)],
        validation_rows=[retired, current],
        current_validation_rows=[current],
        incomplete_data=[marker],
    )
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert marker in value["incomplete_data"]
    assert value["provenance"][
        "validation_cell_results_incomplete_data_sha256"
    ] == build_data._digest([marker])

    changed = copy.deepcopy(value)
    published = next(
        item
        for item in changed["incomplete_data"]
        if item["field"] == "cell_results"
    )
    published["reason"] = "rewritten after projection"
    with pytest.raises(build_data.BuildRefused, match="provenance digests"):
        build_data.validate_build_data(changed)


@pytest.mark.parametrize("mutation", [
    lambda document, retired: document["incomplete_data"].pop(),
    lambda document, retired: document["incomplete_data"].append(copy.deepcopy(document["incomplete_data"][0])),
    lambda document, retired: document["incomplete_data"][0].update(record_id="dangling"),
    lambda document, retired: document["incomplete_data"][0].update(schema="7"),
    lambda document, retired: document["incomplete_data"][0].update(extra="value"),
    lambda document, retired: document["incomplete_data"][0].pop("reason"),
    duplicate_marker_history,
    lambda document, retired: document["validation_ledger"].update(current_validation_run_rows=[retired[0]], current_validation_run_row_count=1, current_validation_run_rows_sha256=build_data._digest([retired[0]])),
])
def test_cell_results_gap_join_refuses_missing_duplicate_dangling_mismatch_or_current(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, mutation: Any
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    retired = [unavailable_schema6_row(hermit_commit, suffix) for suffix in ("one", "two")]
    current = validation_row(hermit_commit)
    canonical = canonical_input(parent_commit, [series_row("event-1", hermit_commit)], validation_rows=[*retired, current], current_validation_rows=[current], incomplete_data=[cell_results_gap(row) for row in retired])
    mutation(canonical, retired)
    install_inputs(monkeypatch, canonical, manifest_metadata())

    with pytest.raises(build_data.BuildRefused):
        build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)


def test_dirty_or_moving_hermit_checkout_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    canonical = canonical_input(parent_commit, [])
    install_inputs(monkeypatch, canonical, manifest_metadata())
    (hermit / "untracked.txt").write_text("dirty\n")
    with pytest.raises(build_data.BuildRefused, match="Hermit checkout has local changes"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )

    (hermit / "untracked.txt").unlink()

    def move_checkout(_hermit: Path) -> Any:
        (hermit / "tracked.txt").write_text("moved\n")
        git(hermit, "add", "tracked.txt")
        git(hermit, "commit", "-m", "move")
        return captured(manifest_metadata())

    monkeypatch.setattr(build_data, "_run_manifest_metadata", move_checkout)
    with pytest.raises(build_data.BuildRefused, match="Hermit checkout is at"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_dirty_parent_checkout_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    canonical = canonical_input(parent_commit, [])
    install_inputs(monkeypatch, canonical, manifest_metadata())
    (parent / "untracked.txt").write_text("dirty\n")
    with pytest.raises(
        build_data.BuildRefused, match="dev-hermit checkout has local changes"
    ):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_moving_origin_main_ref_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    canonical = canonical_input(parent_commit, [])
    install_inputs(monkeypatch, canonical, manifest_metadata())
    tree = git(hermit, "show", "-s", "--format=%T", hermit_commit)
    moved_main = git(
        hermit,
        "commit-tree",
        tree,
        "-p",
        hermit_commit,
        "-m",
        "advanced main",
    )

    def move_main(_hermit: Path) -> Any:
        git(hermit, "update-ref", "refs/remotes/origin/main", moved_main)
        return captured(manifest_metadata())

    monkeypatch.setattr(build_data, "_run_manifest_metadata", move_main)
    with pytest.raises(build_data.BuildRefused, match="Hermit origin/main is"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_moving_parent_checkout_refuses(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    canonical = canonical_input(parent_commit, [])

    def move_parent(_parent: Path, _commit: str) -> Any:
        (parent / "tracked.txt").write_text("moved\n")
        git(parent, "add", "tracked.txt")
        git(parent, "commit", "-m", "move")
        return captured(canonical)

    monkeypatch.setattr(build_data, "_capture_canonical_input", move_parent)
    monkeypatch.setattr(
        build_data,
        "_run_manifest_metadata",
        lambda _hermit: captured(manifest_metadata()),
    )
    with pytest.raises(build_data.BuildRefused, match="dev-hermit checkout is at"):
        build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )


def test_manifest_command_malformed_json_and_failure_refuse(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _parent, _parent_commit, hermit, _hermit_commit = make_checkouts(tmp_path)

    monkeypatch.setattr(
        build_data._modules().bounded_capture,
        "capture",
        lambda *_args, **_kwargs: subprocess.CompletedProcess([], 0, b"{", b""),
    )
    with pytest.raises(build_data.BuildRefused, match="malformed JSON"):
        build_data._run_manifest_metadata(hermit)

    monkeypatch.setattr(
        build_data._modules().bounded_capture,
        "capture",
        lambda *_args, **_kwargs: subprocess.CompletedProcess([], 9, b"partial", b"boom"),
    )
    with pytest.raises(build_data.BuildRefused, match="command failed: boom"):
        build_data._run_manifest_metadata(hermit)


def test_main_refuses_without_partial_stdout(
    monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(
        build_data,
        "build_data",
        lambda *_args: (_ for _ in ()).throw(build_data.BuildRefused("bad input")),
    )
    result = build_data.main(
        [
            "--parent",
            "/does/not/matter",
            "--parent-commit",
            "a" * 40,
            "--hermit",
            "/does/not/matter",
            "--hermit-commit",
            "b" * 40,
            "--hermit-main-commit",
            "c" * 40,
        ]
    )
    captured_output = capfd.readouterr()
    assert result == 2
    assert captured_output.out == ""
    assert captured_output.err == (
        "compatibility-website build data: REFUSED: bad input\n"
    )


def test_main_refuses_authority_module_load_without_traceback(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capfd: pytest.CaptureFixture[str],
) -> None:
    invalid_module = tmp_path / "invalid-export-inputs.py"
    invalid_module.write_text("def invalid(:\n")
    monkeypatch.setattr(build_data, "EXPORT_INPUTS_PATH", invalid_module)
    monkeypatch.setattr(build_data, "_LOADED_MODULES", None)

    result = build_data.main(
        [
            "--parent",
            "/does/not/matter",
            "--parent-commit",
            "a" * 40,
            "--hermit",
            "/does/not/matter",
            "--hermit-commit",
            "b" * 40,
            "--hermit-main-commit",
            "c" * 40,
        ]
    )

    captured_output = capfd.readouterr()
    assert result == 2
    assert captured_output.out == ""
    assert "compatibility-website build data: REFUSED: cannot load" in (
        captured_output.err
    )
    assert "Traceback" not in captured_output.err


def test_main_emits_one_canonical_json_document(
    monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]
) -> None:
    value = {"schema_version": 1, "z": 0}
    monkeypatch.setattr(build_data, "build_data", lambda *_args: value)
    result = build_data.main(
        [
            "--parent",
            "/does/not/matter",
            "--parent-commit",
            "a" * 40,
            "--hermit",
            "/does/not/matter",
            "--hermit-commit",
            "b" * 40,
            "--hermit-main-commit",
            "c" * 40,
        ]
    )
    captured_output = capfd.readouterr()
    assert result == 0
    assert captured_output.err == ""
    assert captured_output.out == '{"schema_version":1,"z":0}\n'
