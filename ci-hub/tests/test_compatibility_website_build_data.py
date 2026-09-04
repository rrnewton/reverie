from __future__ import annotations

import ast
import copy
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import time
from collections import Counter
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/build_data.py"
SPEC = importlib.util.spec_from_file_location(
    "compatibility_website_build_data", SCRIPT
)
assert SPEC is not None and SPEC.loader is not None
build_data = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = build_data
SPEC.loader.exec_module(build_data)

CELL = "tests/hello/verify/ptrace"


def git(repo: Path, *arguments: str) -> str:
    return build_data._git_text(repo, arguments)


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


def make_checkouts(tmp_path: Path) -> tuple[Path, str, Path, str]:
    parent, parent_commit = repository(tmp_path, "parent")
    hermit, hermit_commit = repository(tmp_path, "hermit")
    return parent, parent_commit, hermit, hermit_commit


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


def _population_digest(identities: list[dict[str, Any]]) -> str:
    ordered = sorted(
        identities,
        key=lambda item: tuple(
            item[key] for key in ("lane", "category", "test", "mode", "backend")
        ),
    )
    return hashlib.sha256(build_data._canonical_bytes(ordered)).hexdigest()


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
        "result": "pass",
        "cell_results": {
            "run_id": run_id,
            "hermit_sha": commit,
            "source_tree_dirty": False,
            "selected_count": len(identities),
            "recorded_count": len(identities),
            "population_sha256": _population_digest(identities),
            "artifact": {
                "path": "ignored/validate/artifacts/run/cell-results.jsonl",
                "sha256": "c" * 64,
                "row_count": len(identities),
            },
            "selected": identities,
            "cells": [
                {**identity, "cell_verdict": copy.deepcopy(verdict)}
                for identity in identities
            ],
        },
    }


def unavailable_schema6_row(commit: str, suffix: str) -> dict[str, Any]:
    row = validation_row(
        commit,
        run_id=f"retired-{suffix}",
        record_id=f"retired-record-{suffix}",
    )
    row["schema_version"] = 6
    comparison = row["cell_results"]["cells"][0]["cell_verdict"]["comparison"]
    for field in (
        "compare_io_buffers",
        "display_name",
        "record_envelope",
        "virtualize_time",
    ):
        del comparison[field]
    return row


def cell_results_gap(row: dict[str, Any]) -> dict[str, str]:
    return {
        "field": "cell_results",
        "input": "validation ledger",
        "reason": "schema-6 cell_results are unavailable for website selection",
        "record_id": row["record_id"],
        "run_id": row["run_id"],
        "schema": "6",
    }


def _jsonl(rows: list[dict[str, Any]]) -> bytes:
    return b"".join(build_data._canonical_bytes(row) + b"\n" for row in rows)


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
    current_validation_rows = (
        validation_rows if current_validation_rows is None else current_validation_rows
    )
    projection = projection or {row["event_id"]: None for row in rows}
    history_jsonl = _jsonl(validation_rows)
    series_jsonl = _jsonl(rows)
    reader_files = [{"bytes": 1, "path": "ci-hub/series/series.py", "sha256": "3" * 64}]
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
            "row_count": len(rows),
            "rows_jsonl": series_jsonl.decode(),
            "rows_sha256": hashlib.sha256(series_jsonl).hexdigest(),
        },
        "validation_ledger": {
            "event_count": 0,
            "events_sha256": build_data._digest([]),
            "history_row_count": len(validation_rows),
            "history_rows_jsonl": history_jsonl.decode(),
            "history_rows_sha256": hashlib.sha256(history_jsonl).hexdigest(),
        },
        "incomplete_data": [] if incomplete_data is None else incomplete_data,
        "_fixture_current_validation_rows": current_validation_rows,
        "_fixture_history_rows": validation_rows,
        "_fixture_projection_results": projection,
        "_fixture_series_rows": rows,
    }


def _public_cell_results(row: dict[str, Any]) -> dict[str, Any] | None:
    evidence = row.get("cell_results")
    if evidence is None or row.get("schema_version") == 6:
        return None
    return {
        "hermit_sha": evidence["hermit_sha"],
        "population_sha256": evidence["population_sha256"],
        "recorded": {
            "artifact": {
                "row_count": evidence["artifact"]["row_count"],
                "sha256": evidence["artifact"]["sha256"],
            },
            "cell_count": len(evidence["cells"]),
            "cells": evidence["cells"],
        },
        "run_id": evidence["run_id"],
        "selected_by": {
            row["profile"]: {
                "cell_count": len(evidence["selected"]),
                "cells": evidence["selected"],
            }
        },
        "source_tree_dirty": evidence["source_tree_dirty"],
    }


def _public_history(row: dict[str, Any]) -> dict[str, Any]:
    public = {
        key: copy.deepcopy(value) for key, value in row.items() if key != "cell_results"
    }
    public["history_row_sha256"] = build_data._digest(row)
    if "cell_results" in row:
        public["cell_results"] = _public_cell_results(row)
    return public


def _fixture_projection(
    canonical: dict[str, Any], manifest: dict[str, Any], main: str
) -> dict[str, Any]:
    raw_series = canonical["_fixture_series_rows"]
    raw_history = canonical["_fixture_history_rows"]
    raw_current = canonical["_fixture_current_validation_rows"]
    projected_cells = {}
    cells_by_test: dict[str, list[str]] = {}
    for raw in manifest["cells"]:
        identity = f"{raw['test']}/{raw['mode']}/{raw['backend']}"
        projected_cells[identity] = {**copy.deepcopy(raw), "cell": identity}
        cells_by_test.setdefault(raw["test"], []).append(identity)
    projected_manifest = {
        "cells": dict(sorted(projected_cells.items())),
        "manifest_schema": manifest["manifest_schema"],
        "manifest_sha256": manifest["manifest_sha256"],
        "selected_by_full_custom_commands": [
            {
                **copy.deepcopy(raw),
                "cell": f"{raw['test']}/{raw['mode']}/{raw['backend']}",
            }
            for raw in manifest["selected_by_full_custom_commands"]
        ],
        "tests": [
            {
                **copy.deepcopy(raw),
                "cells": sorted(cells_by_test.get(raw["id"], [])),
            }
            for raw in manifest["tests"]
        ],
    }

    def admitted_history(row: dict[str, Any]) -> bool:
        return (
            row.get("schema_version") in {7, 8}
            and all(
                isinstance(row.get(field), str) and row[field].strip()
                for field in (
                    "record_id",
                    "run_id",
                    "started_at",
                    "finished_at",
                    "profile",
                    "selection_mode",
                    "result",
                )
            )
            and _public_cell_results(row) is not None
        )

    history = [_public_history(row) for row in raw_history if admitted_history(row)]
    current = [_public_history(row) for row in raw_current if admitted_history(row)]
    selection_by_run = {
        row["run_id"]: (
            row["profile"],
            row["commit"],
            {
                f"{identity['test']}/{identity['mode']}/{identity['backend']}"
                for identity in row["cell_results"]["selected"]
            },
        )
        for row in raw_current
        if admitted_history(row)
    }

    raw_projection = canonical["_fixture_projection_results"]
    admitted_series = []
    projection_results: dict[str, None] = {}
    product_results: dict[str, str | None] = {}
    validate_selection: dict[str, dict[str, Any]] = {}
    ancestry: dict[str, bool | None] = {}
    excluded: Counter[tuple[str, str]] = Counter()
    excluded_runs: Counter[tuple[str, str]] = Counter()
    raw_represented = 0
    schema_counts: Counter[str] = Counter()
    for raw in raw_series:
        count = raw["series"].get("num_runs", 1)
        raw_represented += count
        schema_counts[raw["schema"]] += 1
        reasons = set()
        if raw["schema"] != "stress-series/v3":
            reasons.add("series schema is not stress-series/v3")
        if raw_projection.get(raw["event_id"]) is not None:
            reasons.add(str(raw_projection[raw["event_id"]]))
        if raw["producer"] == "validate":
            selection = selection_by_run.get(raw["run_id"])
            if selection is None:
                reasons.add(
                    "validate series row has no admitted terminal validation row"
                )
            else:
                path, commit, cells = selection
                if raw["series"]["tree"] != commit:
                    reasons.add(
                        "validate series row and terminal validation row record different Hermit commits"
                    )
                if raw["series"]["cell"] not in cells:
                    reasons.add(
                        "validate series row names a cell not selected by its validate path"
                    )
        if reasons:
            for reason in reasons:
                key = (raw["schema"], reason)
                excluded[key] += 1
                excluded_runs[key] += count
            continue

        payload = copy.deepcopy(raw["series"])
        payload.pop("main_ancestry", None)
        payload.pop("first_divergent_messages", None)
        result = None
        if payload.get("outcome") == "passed" and payload.get("result") == "pass":
            result = "pass"
        elif payload.get("failure_class") == "product_failure":
            result = "product_failure"
        tree = payload["tree"]
        ancestry[tree] = tree == main
        projected = {
            **{
                key: copy.deepcopy(value)
                for key, value in raw.items()
                if key not in {"series", "source"}
            },
            "current_main_ancestry": ancestry[tree],
            "product_result": result,
            "series": payload,
        }
        admitted_series.append(projected)
        projection_results[raw["event_id"]] = None
        product_results[raw["event_id"]] = result
        if raw["producer"] == "validate":
            path, _commit, _cells = selection_by_run[raw["run_id"]]
            validate_selection[raw["event_id"]] = {
                "path": path,
                "selected": True,
            }

    exclusions = [
        {
            "physical_row_count": count,
            "reason": reason,
            "represented_run_count": excluded_runs[(schema, reason)],
            "schema": schema,
        }
        for (schema, reason), count in sorted(excluded.items())
    ]
    for row in raw_history:
        if admitted_history(row):
            continue
        schema = str(row.get("schema_version", "not recorded"))
        reasons = []
        if row.get("schema_version") not in {7, 8}:
            reasons.append("validation HistoryRow schema is not 7 or 8")
        for field in (
            "record_id",
            "run_id",
            "started_at",
            "finished_at",
            "profile",
            "selection_mode",
            "result",
        ):
            if not isinstance(row.get(field), str) or not row[field].strip():
                reasons.append(f"validation HistoryRow omits {field}")
        if _public_cell_results(row) is None:
            reasons.append(
                "validation HistoryRow cell_results lack the later comparison fields"
                if row.get("schema_version") == 6
                else "validation HistoryRow omits cell_results"
            )
        exclusions.extend(
            {
                "physical_row_count": 1,
                "reason": reason,
                "represented_run_count": 1,
                "schema": schema,
            }
            for reason in sorted(set(reasons))
        )
    exclusions.sort(key=lambda row: (row["schema"], row["reason"]))

    manifest_bytes = build_data._canonical_bytes(manifest) + b"\n"
    history_bytes = canonical["validation_ledger"]["history_rows_jsonl"].encode()
    series_bytes = canonical["series"]["rows_jsonl"].encode()
    admitted_represented = sum(
        row["series"].get("num_runs", 1) for row in admitted_series
    )
    result: dict[str, Any] = {
        "schema_version": 2,
        "input": {
            "manifest_metadata": {
                "bytes": len(manifest_bytes),
                "sha256": hashlib.sha256(manifest_bytes).hexdigest(),
            },
            "validation_history_rows": {
                "bytes": len(history_bytes),
                "row_count": len(raw_history),
                "sha256": hashlib.sha256(history_bytes).hexdigest(),
            },
            "series_rows": {
                "bytes": len(series_bytes),
                "row_count": len(raw_series),
                "sha256": hashlib.sha256(series_bytes).hexdigest(),
            },
        },
        "manifest": projected_manifest,
        "validation_history_rows": history,
        "current_validation_run_rows": current,
        "series_rows": admitted_series,
        "current_main_ancestry": dict(sorted(ancestry.items())),
        "projection_results": dict(sorted(projection_results.items())),
        "product_results": dict(sorted(product_results.items())),
        "validate_run_selection": dict(sorted(validate_selection.items())),
        "counts": {
            "current_validation_run_row_count": len(current),
            "projection_admitted_physical_row_count": len(admitted_series),
            "projection_admitted_represented_run_count": admitted_represented,
            "projection_excluded_physical_row_count": len(raw_series)
            - len(admitted_series),
            "projection_excluded_represented_run_count": raw_represented
            - admitted_represented,
            "series_physical_row_count": len(admitted_series),
            "series_represented_run_count": admitted_represented,
            "series_schema_counts": dict(sorted(schema_counts.items())),
            "validation_history_row_count": len(history),
        },
        "projection_exclusion_counts": exclusions,
    }
    request = canonical["provenance"].get("historical")
    if request is not None:
        matches = [
            row
            for row in raw_current
            if admitted_history(row)
            and row["record_id"] == request["record_id"]
            and row["run_id"] == request["run_id"]
        ]
        assert len(matches) == 1
        row = matches[0]
        result["historical"] = {
            "finished_at": row["finished_at"],
            "history_row_sha256": build_data._digest(row),
            "measured_hermit_commit": row["commit"],
            "record_id": row["record_id"],
            "run_id": row["run_id"],
            "selected_by_full_cell_count": row["cell_results"]["selected_count"],
            "started_at": row["started_at"],
            "validate_path": "full",
        }
    result["projection_sha256"] = build_data._digest(result)
    return result


def captured(value: dict[str, Any]) -> Any:
    return build_data.CapturedJson(
        value=value,
        encoded=build_data._canonical_bytes(value) + b"\n",
    )


def install_inputs(
    monkeypatch: pytest.MonkeyPatch,
    canonical: dict[str, Any],
    manifest: dict[str, Any],
    *,
    real_projection: bool = False,
) -> None:
    captured_canonical = (
        {
            key: value
            for key, value in canonical.items()
            if not key.startswith("_fixture_")
        }
        if real_projection
        else canonical
    )
    monkeypatch.setattr(
        build_data,
        "_capture_canonical_input",
        lambda _parent, _commit: captured(captured_canonical),
    )
    monkeypatch.setattr(
        build_data,
        "_run_manifest_metadata",
        lambda _hermit: captured(manifest),
    )
    if not real_projection:
        monkeypatch.setattr(
            build_data,
            "_run_projection",
            lambda _manifest, _canonical, _hermit, main, _historical: _fixture_projection(
                canonical, manifest, main
            ),
        )


def _historical_request(canonical: dict[str, Any], record_id: str, run_id: str) -> None:
    provenance = canonical["provenance"]
    provenance.update(
        historical={"record_id": record_id, "run_id": run_id},
        mode="historical",
        published_only=True,
        reader_worktree_clean=True,
        reader_worktree_dirty_paths=[],
    )
    provenance["reader_source_identity_sha256"] = build_data._digest(
        {
            "ci_hub_tree": provenance["reader_ci_hub_tree"],
            "commit": provenance["reader_commit"],
            "dirty_paths": provenance["reader_worktree_dirty_paths"],
            "files": provenance["reader_files"],
            "hermit": provenance["reader_hermit"],
            "worktree_clean": provenance["reader_worktree_clean"],
        }
    )


def run_build(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    rows: list[dict[str, Any]],
    *,
    validation_rows: list[dict[str, Any]] | None = None,
    manifest_cells: tuple[str, ...] = (CELL,),
) -> dict[str, Any]:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    canonical = canonical_input(parent_commit, rows, validation_rows=validation_rows)
    install_inputs(monkeypatch, canonical, manifest_metadata(*manifest_cells))
    return build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )


@pytest.mark.parametrize(
    ("value", "message"),
    [
        ({"value": "/root/work/private"}, "absolute owner path"),
        ({"value": "Authorization: Bearer abc123"}, "credential-bearing"),
        ({"access_token": "redacted"}, "credential-bearing"),
        ({"enabled": True}, "forbidden stored field"),
        ({"measured": True}, "forbidden stored field"),
        ({"never_measured": False}, "forbidden stored field"),
    ],
)
def test_publishable_output_refuses_private_or_stale_values(
    value: Any, message: str
) -> None:
    with pytest.raises(build_data.BuildRefused, match=message):
        build_data._require_publishable_output(value)


def test_publishable_output_preserves_immutable_recorded_prose() -> None:
    value = {"gates": [{"name": "Prepare every CI-enabled portable manifest guest"}]}
    build_data._require_publishable_output(value)
    assert value["gates"][0]["name"] == (
        "Prepare every CI-enabled portable manifest guest"
    )


def test_website_owned_text_refuses_dropped_population_label() -> None:
    with pytest.raises(build_data.BuildRefused, match="dropped vocabulary"):
        build_data._require_website_owned_text(
            {"field": "Cells enabled for nightly validation"}
        )


def test_build_packages_one_rust_projection_without_source_interpretation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = validation_row(hermit_commit)
    canonical = canonical_input(
        parent_commit, [series_row("event-1", hermit_commit)], validation_rows=[row]
    )
    manifest = manifest_metadata()
    calls = 0

    def project(*_args: Any) -> dict[str, Any]:
        nonlocal calls
        calls += 1
        return _fixture_projection(canonical, manifest, hermit_commit)

    install_inputs(monkeypatch, canonical, manifest)
    monkeypatch.setattr(build_data, "_run_projection", project)
    result = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )
    assert calls == 1
    assert result["counts"]["in_manifest_cell_count"] == 1
    assert result["runs"]["series_physical_row_count"] == 1
    keys: set[str] = set()
    stack: list[Any] = [result]
    while stack:
        value = stack.pop()
        if isinstance(value, dict):
            keys.update(value)
            stack.extend(value.values())
        elif isinstance(value, list):
            stack.extend(value)
    assert not {"measured", "measurement_state", "never_measured"}.intersection(keys)


def _validated_stage1_fixture(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> dict[str, Any]:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = validation_row(hermit_commit)
    install_inputs(
        monkeypatch,
        canonical_input(
            parent_commit,
            [series_row("event-1", hermit_commit)],
            validation_rows=[row],
        ),
        manifest_metadata(),
    )
    return build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )


def test_stage1_consumer_refuses_cell_naming_unknown_test(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    value["cells"][0]["test"] = "tests/unknown"
    value["cells"][0]["cell"] = "tests/unknown/verify/ptrace"

    with pytest.raises(build_data.BuildRefused, match="unknown or repeated identity"):
        build_data.validate_build_data(value)


def test_stage1_consumer_refuses_false_projection_admitted_count(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    value["runs"]["projection_admitted_physical_row_count"] = 0

    with pytest.raises(build_data.BuildRefused, match="admitted counts"):
        build_data.validate_build_data(value)


def test_stage1_consumer_refuses_unknown_cell_key(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    value["cells"][0]["raw_log"] = "must not become public"

    with pytest.raises(build_data.BuildRefused, match="unknown field"):
        build_data.validate_build_data(value)


@pytest.mark.parametrize(
    "reason",
    [
        {"reason": "full does not select this cell"},
        {
            "evidence": "recorded <evidence> & details",
            "reason": "full does not select this cell",
            "result": "timeout",
        },
    ],
)
def test_stage1_consumer_accepts_typed_not_selected_by_full_reason(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, reason: dict[str, str]
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    value["cells"][0]["selected_by_full"] = False
    value["cells"][0]["not_selected_by_full_reason"] = reason

    validated = build_data.validate_build_data(value)

    assert validated["cells"][0]["not_selected_by_full_reason"] == reason


@pytest.mark.parametrize(
    ("reason", "message"),
    [
        ("plain text", "must be an object"),
        ({}, "missing field"),
        ({"reason": "why", "extra": "value"}, "unknown field"),
        ({"reason": "   "}, "must be a nonempty string"),
        (
            {"evidence": "recorded evidence", "reason": "why"},
            "result and evidence must be present together",
        ),
        (
            {"reason": "why", "result": "timeout"},
            "result and evidence must be present together",
        ),
        (
            {"evidence": " ", "reason": "why", "result": "timeout"},
            "must be a nonempty string",
        ),
        (
            {"evidence": "recorded evidence", "reason": "why", "result": " "},
            "must be a nonempty string",
        ),
        (
            {
                "evidence": "recorded evidence",
                "reason": "why",
                "result": "not-a-recorded-result",
            },
            "result is unsupported",
        ),
    ],
)
def test_stage1_consumer_refuses_malformed_not_selected_by_full_reason(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    reason: Any,
    message: str,
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    value["cells"][0]["selected_by_full"] = False
    value["cells"][0]["not_selected_by_full_reason"] = reason

    with pytest.raises(build_data.BuildRefused, match=message):
        build_data.validate_build_data(value)


def test_stage1_consumer_refuses_dangling_selected_by_event_id(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    value["cells"][0]["selected_by"]["quick"][0]["event_ids"] = ["unknown-event"]

    with pytest.raises(build_data.BuildRefused, match="disagrees with reducer field"):
        build_data.validate_build_data(value)


def test_stage1_v3_has_no_incomplete_inventory_or_unnamed_cell_population(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)

    assert value["schema_version"] == 3
    assert "incomplete_data" not in value
    assert "incomplete_data_count" not in value["counts"]
    assert "state_unavailable_cell_count" not in value["counts"]
    assert "cell_count" not in value["counts"]
    assert len(value["cells"]) == (
        value["counts"]["green_cell_count"]
        + value["counts"]["red_cell_count"]
        + value["counts"]["never_measured_cell_count"]
    )


def test_stage1_omits_a_cell_with_qualifying_rows_but_no_result(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    no_result_cell = "tests/no-result/verify/ptrace"
    row = series_row(
        "event-no-result",
        hermit_commit,
        producer="pressure-test",
        cell=no_result_cell,
        outcome="no_result",
        result=None,
        failure_class="no_result",
    )
    canonical = canonical_input(parent_commit, [row])
    install_inputs(
        monkeypatch,
        canonical,
        manifest_metadata(CELL, no_result_cell),
    )

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert value["counts"]["in_manifest_cell_count"] == 2
    assert len(value["cells"]) == 1
    assert value["cells"][0]["cell"] == CELL
    assert value["cells"][0]["state"] == "never-measured"
    assert value["counts"]["in_manifest_cell_count"] - len(value["cells"]) == 1
    [omission] = value["projection_audit"]["deliberate_omissions"]
    assert omission["cell"] == no_result_cell
    assert omission["reason"] == (
        "the latest qualifying execution does not establish pass or product failure"
    )
    assert omission["event_ids"] == ["event-no-result"]
    assert omission["qualifying_event_ids"] == ["event-no-result"]
    assert omission["event_references"] == [
        {
            "event_id": "event-no-result",
            "observed_at": "2026-08-31T00:00:00Z",
            "product_result": None,
            "run_id": "run-1",
            "tree": hermit_commit,
        }
    ]
    assert {row["cell"] for row in value["cells"]}.isdisjoint(
        {row["cell"] for row in value["projection_audit"]["deliberate_omissions"]}
    )
    assert (
        len(value["cells"]) + len(value["projection_audit"]["deliberate_omissions"])
        == value["counts"]["in_manifest_cell_count"]
    )


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        ("missing", "do not exhaust manifest identities"),
        ("overlap", "overlapping identities"),
        ("event-reference", "disagrees with exact reducer rerun"),
        ("admitted-flag", "unknown field"),
        ("represented-arithmetic", "do not partition represented runs"),
    ],
)
def test_stage1_refuses_invalid_deliberate_omission_audit(
    mutation: str,
    message: str,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    no_result_cell = "tests/no-result/verify/ptrace"
    canonical = canonical_input(
        parent_commit,
        [
            series_row(
                "event-no-result",
                hermit_commit,
                producer="pressure-test",
                cell=no_result_cell,
                outcome="no_result",
                result=None,
                failure_class="no_result",
            )
        ],
    )
    install_inputs(
        monkeypatch,
        canonical,
        manifest_metadata(CELL, no_result_cell),
    )
    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )
    omission = value["projection_audit"]["deliberate_omissions"][0]
    if mutation == "missing":
        value["projection_audit"]["deliberate_omissions"] = []
    elif mutation == "overlap":
        omission.update(
            cell=CELL,
            test=CELL.rsplit("/", 2)[0],
            mode="verify",
            backend="ptrace",
        )
    elif mutation == "event-reference":
        omission["event_references"][0]["run_id"] = "different-run"
    elif mutation == "admitted-flag":
        omission["admitted"] = False
    elif mutation == "represented-arithmetic":
        omission["represented_run_count"] += 1
    else:
        raise AssertionError(f"unknown mutation {mutation}")

    with pytest.raises(build_data.BuildRefused, match=message):
        build_data.validate_build_data(value)


def test_projection_exclusion_audit_is_retained_without_becoming_a_total(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    legacy = series_row(
        "legacy",
        hermit_commit,
        schema="stress-series/v1",
        run_id="missing-run",
    )
    canonical = canonical_input(parent_commit, [legacy])
    install_inputs(monkeypatch, canonical, manifest_metadata())
    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    audit = value["runs"]["projection_exclusion_counts"]
    assert len(audit) == 2
    assert sum(row["physical_row_count"] for row in audit) == 2
    assert value["runs"]["projection_excluded_physical_row_count"] == 1
    assert value["runs"]["series_physical_row_count"] == 0


@pytest.mark.parametrize("schema_version", [1, 2])
def test_older_stage1_schemas_fail_closed(
    schema_version: int, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    value["schema_version"] = schema_version
    with pytest.raises(build_data.BuildRefused, match="schema_version"):
        build_data.validate_build_data(value)


def test_historical_build_binds_distinct_ledger_manifest_and_measured_commits(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    parent, parent_commit, manifest_hermit, manifest_commit = make_checkouts(tmp_path)
    measured = manifest_commit
    row = validation_row(
        measured,
        run_id="historical-run",
        profile="full",
        record_id="historical-record",
    )
    canonical = canonical_input(
        parent_commit,
        [series_row("event-1", measured, run_id="historical-run")],
        validation_rows=[row],
        current_validation_rows=[row],
    )
    generator_reader_commit = "6" * 40
    base_recorded_gitlink = "e" * 40
    ledger_recorded_gitlink = "f" * 40
    canonical["provenance"]["reader_commit"] = generator_reader_commit
    canonical["provenance"]["reader_hermit"] = {
        "checkout_commit": manifest_commit,
        "gitlink_commit": base_recorded_gitlink,
    }
    _historical_request(canonical, "historical-record", "historical-run")
    manifest = manifest_metadata()
    monkeypatch.setattr(
        build_data,
        "_capture_historical_input",
        lambda *_args: captured(canonical),
    )
    monkeypatch.setattr(
        build_data, "_run_manifest_metadata", lambda _path: captured(manifest)
    )
    monkeypatch.setattr(
        build_data,
        "_run_projection",
        lambda *_args: _fixture_projection(canonical, manifest, measured),
    )

    def recorded_gitlink(checkout: Path, commit: str, path: str) -> str:
        assert path == "hermit"
        if checkout == parent:
            assert commit == parent_commit
            return ledger_recorded_gitlink
        if checkout == build_data.REPOSITORY:
            assert commit == generator_reader_commit
            return base_recorded_gitlink
        pytest.fail(f"unexpected gitlink checkout: {checkout}")

    monkeypatch.setattr(build_data, "_gitlink_commit", recorded_gitlink)
    result = build_data.build_historical_data(
        parent,
        parent_commit,
        "historical-record",
        "historical-run",
        manifest_hermit,
        manifest_commit,
    )
    historical = result["provenance"]["historical"]
    assert historical["ledger_commit"] == parent_commit
    assert historical["manifest_source_commit"] == manifest_commit
    assert historical["measured_hermit_commit"] == measured
    assert historical["validate_path"] == "full"
    assert result["provenance"]["hermit_main_commit"] == ledger_recorded_gitlink
    assert (
        result["provenance"]["canonical_input_provenance"]["reader_commit"]
        == generator_reader_commit
    )
    assert result["provenance"]["canonical_input_provenance"]["reader_hermit"] == {
        "checkout_commit": manifest_commit,
        "gitlink_commit": base_recorded_gitlink,
    }


def test_live_provenance_refuses_reader_hermit_mismatch() -> None:
    canonical = canonical_input("1" * 40, [])
    canonical["provenance"]["reader_hermit"]["gitlink_commit"] = "3" * 40
    with pytest.raises(
        build_data.BuildRefused,
        match="reader Hermit checkout and gitlink commits disagree",
    ):
        build_data._canonical_provenance(
            canonical["provenance"], "1" * 40, historical=False
        )


@pytest.mark.parametrize("gitlink", ["2" * 40, "3" * 40])
def test_historical_provenance_accepts_bound_match_or_mismatch(gitlink: str) -> None:
    canonical = canonical_input("1" * 40, [])
    canonical["provenance"]["reader_hermit"]["gitlink_commit"] = gitlink
    _historical_request(canonical, "historical-record", "historical-run")
    result = build_data._canonical_provenance(
        canonical["provenance"], "1" * 40, historical=True
    )
    assert result["reader_hermit"] == {
        "checkout_commit": "2" * 40,
        "gitlink_commit": gitlink,
    }


@pytest.mark.parametrize("field", ["checkout_commit", "gitlink_commit"])
def test_historical_provenance_refuses_tampered_reader_identity(field: str) -> None:
    canonical = canonical_input("1" * 40, [])
    _historical_request(canonical, "historical-record", "historical-run")
    canonical["provenance"]["reader_hermit"][field] = "4" * 40
    with pytest.raises(
        build_data.BuildRefused,
        match="reader_source_identity_sha256 disagrees",
    ):
        build_data._canonical_provenance(
            canonical["provenance"], "1" * 40, historical=True
        )


@pytest.mark.parametrize(
    ("field", "wrong_commit", "message"),
    [
        (
            "checkout_commit",
            "3" * 40,
            "checkout disagrees with the exact manifest source",
        ),
        (
            "gitlink_commit",
            "5" * 40,
            "gitlink disagrees with the gitlink recorded",
        ),
    ],
)
def test_historical_build_refuses_reader_identity_not_bound_to_inputs(
    field: str,
    wrong_commit: str,
    message: str,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    canonical = canonical_input("1" * 40, [])
    canonical["provenance"]["reader_hermit"] = {
        "checkout_commit": "2" * 40,
        "gitlink_commit": "3" * 40,
    }
    canonical["provenance"]["reader_hermit"][field] = wrong_commit
    _historical_request(canonical, "historical-record", "historical-run")
    monkeypatch.setattr(build_data, "_gitlink_commit", lambda *_args: "3" * 40)
    with pytest.raises(build_data.BuildRefused, match=message):
        build_data._build_from_captured(
            captured(canonical),
            captured(manifest_metadata()),
            Path("/unused"),
            {
                "hermit": "2" * 40,
                "hermit_main": "3" * 40,
                "parent": "1" * 40,
            },
            ("historical-record", "historical-run"),
        )


def test_projection_envelope_binds_exact_input_bytes() -> None:
    manifest = build_data._canonical_bytes(manifest_metadata()) + b"\n"
    history = _jsonl([])
    series = _jsonl([])
    projection = _fixture_projection(
        {
            **canonical_input("1" * 40, []),
            "_fixture_history_rows": [],
            "_fixture_current_validation_rows": [],
        },
        manifest_metadata(),
        "2" * 40,
    )
    assert (
        build_data._validate_projection_envelope(
            projection, manifest, history, series, None
        )["projection_sha256"]
        == projection["projection_sha256"]
    )
    projection["input"]["series_rows"]["bytes"] += 1
    projection["projection_sha256"] = build_data._digest(
        {key: value for key, value in projection.items() if key != "projection_sha256"}
    )
    with pytest.raises(build_data.BuildRefused, match="bytes disagrees"):
        build_data._validate_projection_envelope(
            projection, manifest, history, series, None
        )


def test_real_rust_projection_is_the_only_source_interpretation_process(
    tmp_path: Path,
) -> None:
    _parent, _parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = validation_row(hermit_commit)
    row["result"] = "pass"
    canonical = canonical_input(
        "1" * 40,
        [series_row("real-projection-event", hermit_commit)],
        validation_rows=[row],
        current_validation_rows=[row],
    )
    canonical = {
        key: value
        for key, value in canonical.items()
        if not key.startswith("_fixture_")
    }
    result = build_data._run_projection(
        captured(manifest_metadata()),
        captured(canonical),
        hermit,
        hermit_commit,
        None,
    )
    assert result["counts"]["validation_history_row_count"] == 1
    assert result["counts"]["series_physical_row_count"] == 1
    assert result["validate_run_selection"] == {
        "real-projection-event": {"path": "quick", "selected": True}
    }


def test_projection_process_returns_when_a_descendant_holds_output_descriptors(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(build_data, "_PROJECTION_TIMEOUT_SECONDS", 10)
    command = [
        sys.executable,
        "-c",
        "import os,time\n"
        "if os.fork() == 0:\n"
        "    time.sleep(5)\n"
        "    os._exit(0)\n"
        "os.write(1, b'{}\\n')\n"
        "os._exit(0)\n",
    ]

    started = time.monotonic()
    result = build_data._bounded_projection_process(
        command, cwd=tmp_path, environment=dict(os.environ)
    )

    assert time.monotonic() - started < 2
    assert result.returncode == 0
    assert result.stdout == b"{}\n"
    assert result.stderr == b""


def test_projection_process_timeout_kills_the_direct_child(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    marker = tmp_path / "finished"
    monkeypatch.setattr(build_data, "_PROJECTION_TIMEOUT_SECONDS", 0.05)
    environment = {**os.environ, "WEBSITE_TEST_MARKER": str(marker)}
    command = [
        sys.executable,
        "-c",
        "import os,pathlib,time\n"
        "time.sleep(30)\n"
        "pathlib.Path(os.environ['WEBSITE_TEST_MARKER']).write_bytes(b'finished')\n",
    ]

    with pytest.raises(subprocess.TimeoutExpired):
        build_data._bounded_projection_process(
            command, cwd=tmp_path, environment=environment
        )
    assert not marker.exists()


@pytest.mark.parametrize(
    ("stream", "descriptor"),
    [("stdout", 1), ("stderr", 2)],
)
def test_projection_process_refuses_output_over_the_cap_before_exit(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    stream: str,
    descriptor: int,
) -> None:
    marker = tmp_path / "finished"
    monkeypatch.setattr(build_data, "_PROJECTION_TIMEOUT_SECONDS", 10)
    monkeypatch.setattr(build_data, f"_MAX_PROJECTION_{stream.upper()}_BYTES", 8)
    environment = {**os.environ, "WEBSITE_TEST_MARKER": str(marker)}
    command = [
        sys.executable,
        "-c",
        "import os,pathlib,time\n"
        f"os.write({descriptor}, b'x' * 9)\n"
        "time.sleep(30)\n"
        "pathlib.Path(os.environ['WEBSITE_TEST_MARKER']).write_bytes(b'finished')\n",
    ]

    with pytest.raises(
        build_data._modules().export_inputs.OutputLimitExceeded,
        match=rf"{stream} exceeded 8 bytes",
    ):
        build_data._bounded_projection_process(
            command, cwd=tmp_path, environment=environment
        )
    assert not marker.exists()


@pytest.mark.parametrize(
    ("result", "message"),
    [
        (
            subprocess.CompletedProcess([], 7, b"", b"failure detail\n"),
            "failed: failure detail",
        ),
        (
            subprocess.CompletedProcess([], 0, b"{}\n", b"unexpected detail\n"),
            "wrote unexpected stderr: unexpected detail",
        ),
    ],
)
def test_manifest_metadata_refuses_nonzero_and_success_stderr(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    result: subprocess.CompletedProcess[bytes],
    message: str,
) -> None:
    monkeypatch.setattr(build_data, "_capture", lambda *_args, **_kwargs: result)

    with pytest.raises(build_data.BuildRefused, match=message):
        build_data._run_manifest_metadata(tmp_path)


@pytest.mark.parametrize(
    ("stdout", "message"),
    [
        (b"not-json\n", "returned malformed JSON"),
        (b"{}\n{}\n", "did not emit exactly one JSON line"),
        (b'{"schema":1\n', "returned malformed JSON"),
    ],
    ids=("malformed", "multiple-documents", "truncated"),
)
def test_projection_refuses_invalid_json_framing(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    stdout: bytes,
    message: str,
) -> None:
    canonical = {
        key: value
        for key, value in canonical_input("1" * 40, []).items()
        if not key.startswith("_fixture_")
    }
    monkeypatch.setattr(
        build_data,
        "_bounded_projection_process",
        lambda *_args, **_kwargs: subprocess.CompletedProcess([], 0, stdout, b""),
    )

    with pytest.raises(build_data.BuildRefused, match=message):
        build_data._run_projection(
            captured(manifest_metadata()),
            captured(canonical),
            tmp_path,
            "2" * 40,
            None,
        )


@pytest.mark.parametrize(
    ("result", "message"),
    [
        (
            subprocess.CompletedProcess(
                [],
                7,
                b"",
                b"x" * 2_100,
            ),
            "compatibility website projection failed: "
            + "x" * 2_000
            + "... [truncated]",
        ),
        (
            subprocess.CompletedProcess([], 0, b"{}\n", b"unexpected detail\n"),
            "compatibility website projection wrote unexpected stderr: "
            "unexpected detail",
        ),
    ],
    ids=("nonzero-bounded-diagnostic", "success-with-stderr"),
)
def test_projection_refuses_nonzero_and_success_stderr(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    result: subprocess.CompletedProcess[bytes],
    message: str,
) -> None:
    canonical = {
        key: value
        for key, value in canonical_input("1" * 40, []).items()
        if not key.startswith("_fixture_")
    }
    monkeypatch.setattr(
        build_data,
        "_bounded_projection_process",
        lambda *_args, **_kwargs: result,
    )

    with pytest.raises(build_data.BuildRefused) as raised:
        build_data._run_projection(
            captured(manifest_metadata()),
            captured(canonical),
            tmp_path,
            "2" * 40,
            None,
        )

    assert str(raised.value) == message


def test_projection_maps_live_output_cap_to_a_build_refusal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    canonical = {
        key: value
        for key, value in canonical_input("1" * 40, []).items()
        if not key.startswith("_fixture_")
    }
    exceeded = build_data._modules().export_inputs.OutputLimitExceeded("stdout", 8)
    monkeypatch.setattr(
        build_data,
        "_bounded_projection_process",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(exceeded),
    )

    with pytest.raises(build_data.BuildRefused, match="stdout exceeded 8 bytes"):
        build_data._run_projection(
            captured(manifest_metadata()),
            captured(canonical),
            tmp_path,
            "2" * 40,
            None,
        )


def test_source_has_no_parallel_rust_type_tables_or_private_qualifying_bridge() -> None:
    source = SCRIPT.read_text()
    tree = ast.parse(source)
    declared = {
        node.name
        for node in ast.walk(tree)
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
    }
    declared.update(
        target.id
        for node in ast.walk(tree)
        if isinstance(node, ast.Assign)
        for target in node.targets
        if isinstance(target, ast.Name)
    )
    forbidden = (
        "_TEST_FIELDS",
        "_CELL_FIELDS",
        "_VALIDATION_FIELDS",
        "_GATE_FIELDS",
        "_SERIES_FIELDS",
        "_SERIES_PAYLOAD_FIELDS",
        "_public_series_payload",
        "_public_identity",
        "_public_verdict",
        "_public_cell_results",
        "qualifying_receipt",
        "_RFC3339_Z",
        '"not-published"',
    )
    assert not [name for name in forbidden if name in source]
    assert not {
        "_manifest_test",
        "_manifest_cell",
        "_manifest_data",
        "_product_results",
        "_validation_data",
        "_history_incomplete",
        "_selection_authorities_from_runs",
        "ValidationSelection",
    }.intersection(declared)
    assert not [
        value
        for value in (
            "stress-series/v1",
            "stress-series/v2",
            "stress-series/v3",
            "product_result_available",
            "machine_shortname",
            "kernel_version",
            "host_capabilities",
            "exact pressure repetition membership",
            "runtime.run1.scheduler_turns",
            "runtime.run2.virtual_nanoseconds",
            "bitwise_parity",
            "compared_log_messages",
        )
        if value in source
    ]
    assert source.count('"compatibility-website-projection"') == 1


def test_git_environment_drops_untrusted_overrides(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GIT_DIR", "/tmp/not-trusted")
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", "/tmp/not-trusted")
    environment = build_data._git_environment()
    assert "GIT_DIR" not in environment
    assert environment["GIT_CONFIG_GLOBAL"] == os.devnull
    assert environment["GIT_NO_LAZY_FETCH"] == "1"


def test_main_refuses_without_partial_stdout(
    monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(
        build_data,
        "build_data",
        lambda *_args: (_ for _ in ()).throw(build_data.BuildRefused("fixture")),
    )
    result = build_data.main(
        [
            "--parent",
            "/unused",
            "--parent-commit",
            "1" * 40,
            "--hermit",
            "/unused",
            "--hermit-commit",
            "2" * 40,
            "--hermit-main-commit",
            "3" * 40,
        ]
    )
    captured_output = capfd.readouterr()
    assert result == 2
    assert captured_output.out == ""
    assert captured_output.err == "compatibility-website build data: REFUSED: fixture\n"


def test_cli_help_names_exact_authorities() -> None:
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--help"],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0
    help_text = " ".join(result.stdout.split())
    assert "canonical ledger and series" in help_text
    assert "manifest policy and ancestry" in help_text
    assert result.stderr == ""
