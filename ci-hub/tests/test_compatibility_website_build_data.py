from __future__ import annotations

import ast
import copy
import enum
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import time
from collections import Counter
from collections.abc import Iterator, Mapping
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
    (hermit / "rust-toolchain.toml").write_bytes(
        (REPOSITORY / "hermit/rust-toolchain.toml").read_bytes()
    )
    git(hermit, "add", "rust-toolchain.toml")
    git(hermit, "commit", "-m", "bind the selected Hermit toolchain")
    hermit_commit = git(hermit, "rev-parse", "HEAD")
    git(hermit, "update-ref", "refs/remotes/origin/main", hermit_commit)
    return parent, parent_commit, hermit, hermit_commit


def manifest_metadata(*cells: str) -> dict[str, Any]:
    cells = cells or (CELL,)
    test_ids = sorted({cell.rsplit("/", 2)[0] for cell in cells})
    return {
        "schema": 2,
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
                "cpu_timeout_seconds": 60,
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
        "host": "fixture-host",
        "result": "pass",
        "reverie_base_sha": "d" * 40,
        "reverie_base_tree": "e" * 40,
        "reverie_pin_current": True,
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
        key: copy.deepcopy(value)
        for key, value in row.items()
        if key
        not in {
            "cell_results",
            "reverie_base_sha",
            "reverie_base_tree",
            "reverie_pin_current",
        }
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
        environments = {
            build_data._canonical_bytes(
                {
                    "host_capabilities": series["series"]["host_capabilities"],
                    "kernel_version": series["series"]["kernel_version"],
                    "measurement_host": series["host"],
                }
            )
            for series in raw_series
            if series["run_id"] == row["run_id"]
        }
        assert len(environments) == 1
        environment = json.loads(next(iter(environments)))
        assert environment["measurement_host"] == row["host"]
        result["historical"] = {
            "finished_at": row["finished_at"],
            "history_row_sha256": build_data._digest(row),
            "host_capabilities": environment["host_capabilities"],
            "kernel_version": environment["kernel_version"],
            "measured_hermit_commit": row["commit"],
            "measured_reverie_commit": row["reverie_base_sha"],
            "measurement_host": environment["measurement_host"],
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


def _leave_to_the_reference(value: Any) -> str | None:
    raise build_data._NotPlainOutput


def _publishable_verdict(value: Any, *, reference: bool) -> tuple[str, ...] | None:
    # The reference is _require_publishable_output with its one-pass check
    # declined, which leaves only the stored-field pass and string pass the
    # function has always run.  Both calls are made from this frame, so a
    # recursion-limit sweep compares them at the same stack height.
    one_pass = build_data._plain_publishable_refusal
    if reference:
        build_data._plain_publishable_refusal = _leave_to_the_reference
    try:
        build_data._require_publishable_output(value)
    except build_data.BuildRefused as error:
        return ("refused", str(error))
    except RecursionError:
        return ("recursion",)
    finally:
        build_data._plain_publishable_refusal = one_pass
    return None


def _assert_publishable_verdicts_agree(value: Any) -> tuple[str, ...] | None:
    reference = _publishable_verdict(value, reference=True)
    assert _publishable_verdict(value, reference=False) == reference
    return reference


def _committed_ledger_events() -> list[Any]:
    events = []
    for pattern in ("ledger/**/*.jsonl", "ledger-recovery/**/*.jsonl"):
        for path in sorted(REPOSITORY.glob(pattern)):
            for line in path.read_text(encoding="utf-8").splitlines():
                if line.strip():
                    events.append(json.loads(line))
    return events


def _distinct_strings(value: Any) -> list[str]:
    found: set[str] = set()
    stack = [value]
    while stack:
        item = stack.pop()
        if isinstance(item, str):
            found.add(item)
        elif isinstance(item, dict):
            found.update(key for key in item if isinstance(key, str))
            stack.extend(item.values())
        elif isinstance(item, list):
            stack.extend(item)
    return sorted(found)


def test_publishable_output_matches_the_reference_on_every_committed_ledger_string() -> None:
    # The committed validation ledger is the real input behind Stage 1's
    # history rows. Every distinct string gets the same verdict as a key, a
    # value and a list item from the one-pass check and the reference check.
    events = _committed_ledger_events()
    strings = _distinct_strings(events)
    assert len(events) > 100 and len(strings) > 1000, "the committed ledger corpus is missing"
    refused = 0
    for text in strings:
        for value in ({text: 0}, {"field": text}, {"field": [0, text]}):
            refused += _assert_publishable_verdicts_agree(value) is not None
    assert refused > 0  # the corpus also exercises refusals
    _assert_publishable_verdicts_agree({"events": events[:200]})


# Each string is refused by the reference check, through the regex named.
_OWNER_PATH_STRINGS = [
    "/home/alice/work", "/home/alice", "/HOME/Alice/x", "/Users/bob/src", "/users/bob",
    "/U\u017fers/carol/x", "/root", "/root/.ssh", "/ROOT/x", "C:\\Users\\dave\\x",
    "c:/users/erin/x", "D:\\Documents and Settings\\frank\\x",
    "d:/documents and \u017fettings/gina/x", "prefix /home/hal/y suffix",
]
_CREDENTIAL_STRINGS = [
    "Authorization: Bearer abc123", "authorization=basic dXNlcjpwYXNz",
    "AUTHORIZATION : BEARER token", "password=hunter2", "x_api_key: value",
    "db-passwd = s3cret", "CLIENT_SECRET=abc", "private-key: abc", "pa\u017f\u017fword=x",
    "a;token=abc", "access_token:abc", "credential=abc", "ghp_abcdef012345",
    "GHS_abc", "github_pat_ABCdef_012", "https://user:pass@example.invalid/",
    "http://u:p@host", "-----BEGIN RSA PRIVATE KEY-----",
    "-----BEGIN PRIVATE KEY-----", "-----begin openssh private key-----",
    "private_\u212aey=x",
]
_CREDENTIAL_KEY_STRINGS = [
    "access_token", "api_key", "apikey", "Authorization", "client-secret", "credential",
    "passwd", "password", "private_key", "secret", "token", "x_token", "token_value",
    "hub-secret-name", "\u017fecret", "private_\u212aey", "API-KEY",
]


@pytest.mark.parametrize(
    ("pattern", "text"),
    [("owner", text) for text in _OWNER_PATH_STRINGS]
    + [("credential", text) for text in _CREDENTIAL_STRINGS]
    + [("credential_key", text) for text in _CREDENTIAL_KEY_STRINGS],
)
def test_publishable_output_adversarial_strings_refuse_in_every_position(
    pattern: str, text: str
) -> None:
    regex = {
        "owner": build_data._OWNER_PATH_RE,
        "credential": build_data._CREDENTIAL_RE,
        "credential_key": build_data._CREDENTIAL_KEY_RE,
    }[pattern]
    assert regex.search(text)
    message = "absolute owner path" if pattern == "owner" else "credential-bearing"
    positions = [{text: 0}, {"clean": {"deeper": [{text: None}]}}]
    if pattern != "credential_key":
        positions += [{"field": text}, {"field": ["clean", [text]]},
                      {"clean": "clean", "later": text}]
    else:
        # A key-only credential name is accepted as a value.
        for value in ({"field": text}, {"field": [text]}):
            assert _assert_publishable_verdicts_agree(value) is None
    for value in positions:
        reference = _assert_publishable_verdicts_agree(value)
        assert reference is not None and message in reference[1]


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        # First string refusal in walk order; owner is judged before credential.
        ({"a": "password=x", "b": "/home/alice/x"}, "credential-bearing"),
        ({"a": "/home/alice/x", "b": "password=x"}, "absolute owner path"),
        ({"a": "/home/alice/x password=y"}, "absolute owner path"),
        ({"/home/alice/x password=y": 0}, "absolute owner path"),
        ({"a": [{"b": "ghp_abc"}], "/root": 0}, "credential-bearing"),
        # A forbidden stored field anywhere wins over any string refusal.
        (
            {"a": "/home/alice/x", "b": {"c": [{"measured": 1}]}},
            "forbidden stored field 'measured'",
        ),
        ({"password": 1, "enabled": 1}, "forbidden stored field 'enabled'"),
        # A key that is not a string wins over later fields and string refusals.
        ({"a": "/home/alice/x", "b": {7: 1}, "enabled": 1}, "keys must be strings"),
        ({"enabled": 1, "b": {7: 1}}, "forbidden stored field 'enabled'"),
        # A string refusal found in a key is deferred exactly like one in a
        # value: a later forbidden field or non-string key still wins.
        ({"/home/alice/x": 0, "enabled": 1}, "forbidden stored field 'enabled'"),
        ({"/home/alice/x": 0, "b": {7: 1}}, "keys must be strings"),
        ({"api_token": 0, "enabled": 1}, "forbidden stored field 'enabled'"),
        ({"api_token": 0, "b": {7: 1}}, "keys must be strings"),
        # The same text is judged separately as a key and as a value.
        ({"a": "api_token", "api_token": 1}, "credential-bearing"),
        ({"a": ["secret", "secret"], "b": {"secret": 0}}, "credential-bearing"),
        ({"secret_name": "x", "b": "secret_name"}, "credential-bearing"),
        # Tuples and other scalars are not walked by the reference.
        ({"a": ("/home/alice/x",), "b": 1.5, "c": None, "d": True}, None),
        ({"a": ["api_token", {"clean": "api_token"}]}, None),
        ({}, None),
    ],
)
def test_publishable_output_keeps_the_reference_refusal_order(
    value: Any, expected: str | None
) -> None:
    reference = _assert_publishable_verdicts_agree(value)
    if expected is None:
        assert reference is None
    else:
        assert reference is not None and expected in reference[1]


class _CollidingText(str):
    """Equal to, and hashed like, an already-accepted clean string."""

    def __eq__(self, other: object) -> bool:
        return True

    def __hash__(self) -> int:
        return hash("clean")


class _OrderedMapping(Mapping):
    def __init__(self, pairs: list[tuple[Any, Any]]) -> None:
        self._pairs = pairs

    def __getitem__(self, key: Any) -> Any:
        return dict(self._pairs)[key]

    def __iter__(self) -> Iterator[Any]:
        return iter(key for key, _item in self._pairs)

    def __len__(self) -> int:
        return len(self._pairs)


class _Row(dict):
    pass


class _Rows(list):
    pass


@pytest.mark.parametrize(
    "value",
    [
        {"a": "clean", "b": _CollidingText("/home/alice/x")},
        {"a": {"clean": 0}, "b": {_CollidingText("/home/alice/x"): 0}},
        {"a": {"clean": 0}, "b": {_CollidingText("enabled"): 0}},
        {"a": _Row({"/home/alice/x": 0})},
        {"a": _Rows(["password=x"])},
        {"a": _OrderedMapping([("b", "/home/alice/x"), ("measured", 1)])},
        {"a": _OrderedMapping([(7, 1)]), "b": "/home/alice/x"},
        {"a": b"/home/alice/x", "b": "clean"},
        _Row({"a": "/home/alice/x"}),
        ["/home/alice/x"],
        "/home/alice/x",
    ],
)
def test_publishable_output_leaves_other_types_to_the_reference(value: Any) -> None:
    # A str or container subclass could compare equal to text already judged,
    # or iterate differently; only exact dict, list and str use one pass.
    with pytest.raises(build_data._NotPlainOutput):
        build_data._plain_publishable_refusal(value)
    _assert_publishable_verdicts_agree(value)


def _nested(depth: int, leaf: Any) -> dict[str, Any]:
    value: Any = leaf
    for level in range(depth):
        value = {"level": value} if level % 2 else [value]
    return {"root": value}


@pytest.mark.parametrize(
    "leaf", ["clean", "/home/alice/x", {"measured": 1}, {"clean": 0}, {7: "clean"}]
)
def test_publishable_output_matches_the_reference_at_every_depth_and_stack_height(
    leaf: Any,
) -> None:
    # Near the recursion limit the reference raises RecursionError.  The
    # one-pass check uses fewer frames for some data (a forbidden key, or a
    # key with a non-string value, needs no call of its own), so it must
    # leave a call without enough headroom to the reference.
    for depth in (1, 2, 20, build_data._PLAIN_OUTPUT_DEPTH - 1,
                  build_data._PLAIN_OUTPUT_DEPTH, build_data._PLAIN_OUTPUT_DEPTH + 1, 300):
        _assert_publishable_verdicts_agree(_nested(depth, leaf))
    limit = sys.getrecursionlimit()
    base = build_data._stack_depth()
    outcomes = set()
    try:
        for headroom in (20, 40, 80, 2 * build_data._PLAIN_OUTPUT_DEPTH + 20):
            sys.setrecursionlimit(base + headroom)
            for depth in range(headroom + 10):
                outcomes.add(_assert_publishable_verdicts_agree(_nested(depth, leaf)))
    finally:
        sys.setrecursionlimit(limit)
    assert ("recursion",) in outcomes  # the sweep reached the reference's limit


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
    assert result["provenance"]["manifest_metadata_schema"] == 2
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


def test_new_metadata_provenance_refuses_legacy_schema(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    canonical = canonical_input(parent_commit, [])
    manifest = manifest_metadata()
    manifest["schema"] = 1
    # Even an already-constructed projection cannot relabel legacy metadata
    # as a newly produced schema-2 artifact.
    install_inputs(monkeypatch, canonical, manifest)
    with pytest.raises(build_data.BuildRefused, match="new manifest metadata must use schema 2"):
        build_data.build_data(parent, parent_commit, hermit, hermit_commit, hermit_commit)


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


def _without_fixture_fields(canonical: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in canonical.items() if not key.startswith("_fixture_")}


def _with_series_quarantine(
    canonical: dict[str, Any], records: list[dict[str, Any]]
) -> dict[str, Any]:
    encoded = _jsonl(records)
    canonical = copy.deepcopy(canonical)
    canonical["series"].update(
        quarantine_row_count=len(records),
        quarantine_rows_jsonl=encoded.decode(),
        quarantine_rows_sha256=hashlib.sha256(encoded).hexdigest(),
    )
    return canonical


QUARANTINE_RECORD = {"schema": "series-quarantine/v2", "event_id": "series-quarantine-fixture"}


def test_series_quarantine_fields_bind_together_or_not_at_all() -> None:
    canonical = _without_fixture_fields(canonical_input("1" * 40, []))
    assert build_data._opaque_projection_inputs(canonical)[2] is None
    empty = _with_series_quarantine(canonical, [])
    assert build_data._opaque_projection_inputs(empty)[2] == b""
    supplied = _with_series_quarantine(canonical, [QUARANTINE_RECORD])
    assert build_data._opaque_projection_inputs(supplied)[2] == _jsonl([QUARANTINE_RECORD])
    for missing in sorted(build_data._SERIES_QUARANTINE_FIELDS):
        partial = copy.deepcopy(supplied)
        del partial["series"][missing]
        with pytest.raises(build_data.BuildRefused, match="supplied together"):
            build_data._opaque_projection_inputs(partial)
    tampered = copy.deepcopy(supplied)
    tampered["series"]["quarantine_rows_jsonl"] = _jsonl(
        [{**QUARANTINE_RECORD, "event_id": "series-quarantine-other"}]
    ).decode()
    with pytest.raises(build_data.BuildRefused, match="quarantine_rows_sha256 disagrees"):
        build_data._opaque_projection_inputs(tampered)
    miscounted = copy.deepcopy(supplied)
    miscounted["series"]["quarantine_row_count"] = 2
    with pytest.raises(build_data.BuildRefused, match="framing disagrees with quarantine_row_count"):
        build_data._opaque_projection_inputs(miscounted)


def test_projection_envelope_binds_the_series_quarantine_exactly_when_supplied() -> None:
    manifest = build_data._canonical_bytes(manifest_metadata()) + b"\n"
    empty = _jsonl([])
    quarantine = _jsonl([QUARANTINE_RECORD])
    unbound = _fixture_projection(
        {
            **canonical_input("1" * 40, []),
            "_fixture_history_rows": [],
            "_fixture_current_validation_rows": [],
        },
        manifest_metadata(),
        "2" * 40,
    )
    bound = copy.deepcopy(unbound)
    bound["input"]["series_quarantine_rows"] = {
        "bytes": len(quarantine),
        "row_count": 1,
        "sha256": hashlib.sha256(quarantine).hexdigest(),
    }
    bound["projection_sha256"] = build_data._digest(
        {key: value for key, value in bound.items() if key != "projection_sha256"}
    )
    assert (
        build_data._validate_projection_envelope(
            bound, manifest, empty, empty, None, quarantine
        )["projection_sha256"]
        == bound["projection_sha256"]
    )
    with pytest.raises(build_data.BuildRefused, match="missing field.*series_quarantine_rows"):
        build_data._validate_projection_envelope(
            unbound, manifest, empty, empty, None, quarantine
        )
    with pytest.raises(build_data.BuildRefused, match="unknown field.*series_quarantine_rows"):
        build_data._validate_projection_envelope(bound, manifest, empty, empty, None)
    with pytest.raises(
        build_data.BuildRefused, match="series_quarantine_rows.bytes disagrees"
    ):
        build_data._validate_projection_envelope(
            bound, manifest, empty, empty, None, quarantine + quarantine
        )


def test_real_rust_projection_binds_the_series_quarantine_it_is_given(
    tmp_path: Path,
) -> None:
    _parent, _parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = validation_row(hermit_commit)
    row["result"] = "pass"
    canonical = _with_series_quarantine(
        _without_fixture_fields(
            canonical_input(
                "1" * 40,
                [series_row("real-projection-event", hermit_commit)],
                validation_rows=[row],
                current_validation_rows=[row],
            )
        ),
        [QUARANTINE_RECORD],
    )
    result = build_data._run_projection(
        captured(manifest_metadata()),
        captured(canonical),
        hermit,
        hermit_commit,
        None,
    )
    encoded = _jsonl([QUARANTINE_RECORD])
    assert result["input"]["series_quarantine_rows"] == {
        "bytes": len(encoded),
        "row_count": 1,
        "sha256": hashlib.sha256(encoded).hexdigest(),
    }
    # A record that accounts for no census attempt exempts nothing.
    assert result["counts"]["series_physical_row_count"] == 1
    assert not [
        row
        for row in result["projection_exclusion_counts"]
        if row["schema"] == "series-quarantine/v2"
    ]


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
    monkeypatch.setattr(build_data, "_hermit_toolchain", lambda *_args: "fixture-toolchain")
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
    monkeypatch.setattr(build_data, "_hermit_toolchain", lambda *_args: "fixture-toolchain")
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
    monkeypatch.setattr(build_data, "_hermit_toolchain", lambda *_args: "fixture-toolchain")
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
    monkeypatch.setattr(build_data, "_hermit_toolchain", lambda *_args: "fixture-toolchain")
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


def test_projection_carries_measured_hermit_toolchain_and_state(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    hermit = tmp_path / "measured-hermit"
    state = tmp_path / "canonical-state"
    monkeypatch.setenv("RUSTUP_TOOLCHAIN", "unrelated-ambient-stable")
    selected = "nightly-2099-01-01-x86_64-unknown-linux-gnu"
    calls = []

    def resolve(checkout, *, environment):
        assert checkout == hermit
        calls.append("resolve")
        return selected

    class ProjectionReached(Exception):
        pass

    def project(command, *, cwd, environment):
        assert calls == ["resolve"]
        assert command[:5] == ["rust-script", "--force", "--toolchain", selected, str(build_data.PROJECTION_COMMAND)]
        assert cwd == hermit
        assert environment["RUSTUP_TOOLCHAIN"] == selected
        assert environment["RUSTUP_AUTO_INSTALL"] == "0"
        assert environment["DEV_HERMIT_PARENT"] == str(state.resolve())
        raise ProjectionReached

    monkeypatch.setattr(build_data._modules().rust_toolchain, "selected_toolchain", resolve)
    monkeypatch.setattr(build_data, "_bounded_projection_process", project)
    canonical = {key: value for key, value in canonical_input("1" * 40, []).items()
                 if not key.startswith("_fixture_")}
    with pytest.raises(ProjectionReached):
        build_data._run_projection(
            captured(manifest_metadata()), captured(canonical),
            hermit, "2" * 40, None, state_root=state,
        )


def test_manifest_metadata_carries_declared_toolchain(tmp_path, monkeypatch):
    selected = "nightly-2099-01-01-x86_64-unknown-linux-gnu"
    monkeypatch.setenv("RUSTUP_TOOLCHAIN", "unrelated-stable")
    mutable_target = tmp_path / "state-target"
    monkeypatch.setenv("CARGO_TARGET_DIR", str(mutable_target))
    monkeypatch.setattr(build_data, "_hermit_toolchain", lambda *_args: selected)
    def capture_command(command, *, cwd, env, **_bounds):
        assert cwd == tmp_path and command[:2] == ["cargo", "run"]
        assert env["RUSTUP_TOOLCHAIN"] == selected
        assert env["RUSTUP_AUTO_INSTALL"] == "0"
        assert env["CARGO_TARGET_DIR"] == str(mutable_target)
        return subprocess.CompletedProcess(command, 0, build_data._canonical_bytes(manifest_metadata()) + b"\n", b"")
    monkeypatch.setattr(build_data, "_capture", capture_command)
    assert build_data._run_manifest_metadata(tmp_path).value["schema"] == 2


def test_selected_hermit_toolchain_refusal_prevents_compilation(tmp_path, monkeypatch):
    resolver = build_data._modules().rust_toolchain
    def refuse(*_args, **_kwargs):
        raise resolver.ToolchainRefused("required compiler unavailable")
    monkeypatch.setattr(resolver, "selected_toolchain", refuse)
    monkeypatch.setattr(build_data, "_capture", lambda *_args, **_kwargs: pytest.fail("must not compile"))
    with pytest.raises(build_data.BuildRefused, match="required compiler unavailable"):
        build_data._run_manifest_metadata(tmp_path)


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


def test_fresh_validation_provenance_keeps_reader_and_data_identity_separate():
    value = canonical_input("1" * 40, [])["provenance"]
    value.update(parent_commit="2" * 40, published_ref="2" * 40, published_only=True,
        finalized_proof={"schema": 1, "manifests": [], "objects": {}, "missing_current_terminals": []})
    result = build_data._canonical_provenance(value, "1" * 40, historical=False,
                                              validation_history_commit="2" * 40)
    assert result["reader_commit"] == "1" * 40
    assert result["parent_commit"] == result["published_ref"] == "2" * 40
    with pytest.raises(build_data.BuildRefused, match="parent_commit and published_ref"):
        build_data._canonical_provenance(value, "1" * 40, historical=False,
                                         validation_history_commit="3" * 40)
    with pytest.raises(build_data.BuildRefused, match="reader commit"):
        build_data._canonical_provenance(value, "3" * 40, historical=False,
                                         validation_history_commit="2" * 40)
    value["reader_hermit"]["checkout_commit"] = "4" * 40
    with pytest.raises(build_data.BuildRefused, match="checkout and gitlink"):
        build_data._canonical_provenance(value, "1" * 40, historical=False,
                                         validation_history_commit="2" * 40)


def test_fresh_validation_provenance_requires_closed_captured_proof():
    value = canonical_input("1" * 40, [])["provenance"]
    value.update(parent_commit="2" * 40, published_ref="2" * 40, published_only=True)
    with pytest.raises(build_data.BuildRefused, match="captured finalized proof"):
        build_data._canonical_provenance(value, "1" * 40, historical=False,
                                         validation_history_commit="2" * 40)
    value["finalized_proof"] = {"schema": 1, "manifests": [], "objects": {},
                               "missing_current_terminals": [], "qualified": True}
    with pytest.raises(build_data.BuildRefused, match="unknown or missing fields"):
        build_data._canonical_provenance(value, "1" * 40, historical=False,
                                         validation_history_commit="2" * 40)


@pytest.mark.parametrize("handoff", ["present", "absent", "corrected"])
def test_portable_proof_real_native_current_terminal_credit(tmp_path, monkeypatch, handoff):
    """Real Git query, carrier and native projection over structural fixtures.

    Rebind the retained ordinary schema-10 fixture to a tiny measured Git
    repository, updating original operands and their hashes consistently.
    This does not run a guest or qualify a real validation attempt.
    """
    monkeypatch.syspath_prepend(str(REPOSITORY / "ci-hub/ledger"))
    import validate_rows

    export = build_data._modules().export_inputs
    proof = export._proof_reader()
    parent, _old_parent, hermit, hermit_commit = make_checkouts(tmp_path)
    git(parent, "remote", "add", "origin", validate_rows.CANONICAL_PARENT_REPOSITORY)
    ledger, ledger_commit = repository(tmp_path, "public-ledger")
    git(ledger, "remote", "add", "origin", proof.LEDGER_REPOSITORY)
    state = tmp_path / "original-retained-state"
    state.mkdir()
    fixtures = REPOSITORY / "ci-hub/validate/tests/fixtures/schema10/selected-attempt-v1"
    row = json.loads((fixtures / "ordinary-only-row.json").read_bytes().replace(b"a" * 40, hermit_commit.encode()))
    row.update(repo="hermit", record_id="original", host="fixture-host",
               producer="synthetic-portable-proof-control",
               cwd=validate_rows.WORKSPACE_TOKEN + "/hermit", commit=hermit_commit,
               selection_mode="full", result="pass", started_at="2026-08-31T00:00:00Z",
               finished_at="2026-08-31T00:01:00Z")
    row["cell_results"]["hermit_sha"] = hermit_commit
    row["test_results"]["hermit_sha"] = hermit_commit
    for reference, suffix in (
        (row["constructed_plan"], "plan.json"),
        (row["cell_results"]["artifact"], "cells.jsonl"),
        (row["test_results"]["artifact"], "tests.jsonl"),
    ):
        raw = (fixtures / ("ordinary-only-" + suffix)).read_bytes()
        raw = raw.replace(b"a" * 40, hermit_commit.encode())
        reference["sha256"] = hashlib.sha256(raw).hexdigest()
        if "bytes" in reference:
            reference["bytes"] = len(raw)
        path = state / reference["path"]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(raw)
    original = validate_rows._event_for_row(row, parent)
    shard = parent / "ledger/hermit/fixture-host/2026-08.jsonl"
    shard.parent.mkdir(parents=True)
    shard.write_bytes(_jsonl([original]))

    def commit_namespace():
        git(parent, "add", "--", "ledger")
        git(parent, "commit", "-m", "Synthetic canonical proof input")
        commit = git(parent, "rev-parse", "HEAD")
        git(parent, "update-ref", "refs/remotes/origin/main", commit)
        return commit

    parent_commit = commit_namespace()
    with validate_rows.capture_committed_validation_namespace(parent, parent_commit, state_root=state) as ns:
        [selected] = ns.terminals(repo="hermit", schema_version=10)
        artifacts, objects = proof.capture_artifacts(state, selected["row"])
        manifest = proof.make_manifest(
            selected, artifacts, parent_commit=parent_commit, hermit_tool_commit=hermit_commit,
            snapshot={"source": {"commit": ledger_commit,
                                 "tree": git(ledger, "rev-parse", "HEAD^{tree}")},
                      "rows_sha256": hashlib.sha256(b"").hexdigest()},
        )
        if handoff != "absent":
            proof.stage_carrier(ledger, manifest, objects)
            git(ledger, "add", "--", "scorecard")
            git(ledger, "commit", "-m", "Synthetic published proof carrier")
            ledger_commit = git(ledger, "rev-parse", "HEAD")
    if handoff == "corrected":
        current = copy.deepcopy(row)
        current.update(record_id="current", corrects="original", result="no_result",
                       finished_at="2026-08-31T00:02:00Z")
        shard.write_bytes(_jsonl([original, validate_rows._event_for_row(current, parent)]))
        parent_commit = commit_namespace()

    with validate_rows.capture_committed_validation_namespace(parent, parent_commit, state_root=state) as ns:
        retained = export._capture_finalized_proof(ns, ledger, ledger_commit)
        [selected] = ns.terminals(repo="hermit", schema_version=10)
        assert selected["row"]["record_id"] == ("current" if handoff == "corrected" else "original")
        history = ns.rows()
    cell = "backend-parity-c/fixture/verify/kvm"
    observation = series_row("portable-native-event", hermit_commit, run_id=row["run_id"], cell=cell)
    canonical = canonical_input(parent_commit, [observation], validation_rows=history)
    canonical = {key: value for key, value in canonical.items() if not key.startswith("_fixture_")}
    canonical["provenance"]["finalized_proof"] = retained
    canonical["provenance"]["published_only"] = True
    canonical["provenance"]["reader_hermit"] = {"checkout_commit": hermit_commit, "gitlink_commit": hermit_commit}
    metadata = manifest_metadata(cell)
    metadata["tests"][0]["category"] = "backend-parity-c"
    metadata["cells"][0]["category"] = "backend-parity-c"
    result = build_data._run_projection(captured(metadata), captured(canonical), hermit, hermit_commit, None)
    (tmp_path / "native-projection.json").write_bytes(build_data._canonical_bytes(result) + b"\n")
    assert result["input"]["series_rows"]["row_count"] == 1
    if handoff == "present":
        assert len(retained["manifests"]) == 1 and retained["missing_current_terminals"] == []
        assert [item["record_id"] for item in result["current_validation_run_rows"]] == ["original"]
        assert result["counts"]["projection_admitted_physical_row_count"] == 1
        assert result["validate_run_selection"] == {"portable-native-event": {"path": "full", "selected": True}}
        provenance = build_data._build_provenance(
            captured(canonical), canonical["provenance"], captured(metadata), result,
            {"parent": parent_commit, "hermit": hermit_commit, "hermit_main": hermit_commit,
             "validation_history": parent_commit})
        stage = build_data._artifact_result(result, build_data._reduce_projection(result), provenance)
        assert build_data.validate_build_data(stage) == stage
        # Keep the actual admitted row, its output digests and counts intact;
        # a separately well-formed empty transport still cannot support it.
        altered = copy.deepcopy(stage)
        altered["provenance"]["canonical_input_provenance"]["finalized_proof"] = {
            "schema": 1, "manifests": [], "objects": {},
            "missing_current_terminals": [selected["terminal"]]}
        with pytest.raises(build_data.BuildRefused, match="without its current carrier"):
            build_data.validate_build_data(altered)
        # A coherent replacement object is still not the cells artifact named
        # by the unchanged public native row. Keep output digests/counts intact.
        altered = copy.deepcopy(stage)
        proof_value = altered["provenance"]["canonical_input_provenance"]["finalized_proof"]
        cells = proof_value["manifests"][0]["artifacts"]["cells"]
        replacement = proof_value["objects"].pop(cells["sha256"]) + "\n"
        cells["sha256"] = hashlib.sha256(replacement.encode()).hexdigest()
        cells["bytes"] = len(replacement.encode())
        proof_value["objects"][cells["sha256"]] = replacement
        export.captured_proof_objects(proof_value)
        with pytest.raises(build_data.BuildRefused, match="cells artifact differs"):
            build_data.validate_build_data(altered)
    else:
        assert retained["manifests"] == [] and retained["objects"] == {}
        assert retained["missing_current_terminals"] == [selected["terminal"]]
        assert result["current_validation_run_rows"] == []
        assert result["validate_run_selection"] == {}
        assert result["counts"]["projection_admitted_physical_row_count"] == 0
        assert result["counts"]["projection_excluded_physical_row_count"] == 1
        assert "validation cell evidence unavailable" in json.dumps(result["projection_exclusion_counts"])


PHASE_LINE = (
    r"compatibility-website: phase (?P<name>[a-z0-9_.]+)(?: build=(?P<build>\d+))? "
    r"wall=\d+\.\ds cpu=\d+\.\ds children_cpu=\d+\.\ds(?P<status> status=raised)?"
)


def _phase_lines(text: str) -> list[dict[str, str | None]]:
    import re

    lines = text.splitlines()
    parsed = [re.fullmatch(PHASE_LINE, line) for line in lines]
    assert all(parsed), lines
    return [match.groupdict() for match in parsed if match is not None]


@pytest.mark.parametrize("setting", [None, "", "0", "true", "yes", " 1", "1 "])
def test_phase_timing_is_silent_unless_enabled_exactly(
    monkeypatch: pytest.MonkeyPatch,
    capfd: pytest.CaptureFixture[str],
    setting: str | None,
) -> None:
    if setting is None:
        monkeypatch.delenv(build_data.PHASE_TIMING_ENVIRONMENT, raising=False)
    else:
        monkeypatch.setenv(build_data.PHASE_TIMING_ENVIRONMENT, setting)
    assert build_data.phase_timing_enabled() is False
    with build_data.phase("stage1.export"):
        pass
    with pytest.raises(ValueError, match="kept"):
        with build_data.phase("stage1.export"):
            raise ValueError("kept")
    assert capfd.readouterr() == ("", "")


def test_phase_timing_lines_go_to_stderr_only_and_never_change_stage1_data(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capfd: pytest.CaptureFixture[str],
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = validation_row(hermit_commit)
    canonical = canonical_input(
        parent_commit, [series_row("event-1", hermit_commit)], validation_rows=[row]
    )
    install_inputs(monkeypatch, canonical, manifest_metadata())

    def build() -> dict[str, Any]:
        return build_data.build_data(
            parent, parent_commit, hermit, hermit_commit, hermit_commit
        )

    monkeypatch.delenv(build_data.PHASE_TIMING_ENVIRONMENT, raising=False)
    silent = build()
    assert capfd.readouterr() == ("", "")

    monkeypatch.setenv(build_data.PHASE_TIMING_ENVIRONMENT, "1")
    timed = build()
    first = capfd.readouterr()
    again = build()
    second = capfd.readouterr()

    assert build_data._canonical_bytes(timed) == build_data._canonical_bytes(silent)
    assert build_data._canonical_bytes(again) == build_data._canonical_bytes(silent)
    assert first.out == "" and second.out == ""
    # The silent build already validated byte-identical Stage 1 data in this
    # process, so both timed builds reuse that verdict.
    expected = [
        "stage1.checkouts", "stage1.export", "stage1.manifest_metadata",
        "stage1.checkouts", "stage1.provenance", "stage1.projection",
        "stage1.reduce", "validate.key", "validate.hit", "stage1.validate",
        "stage1.checkouts", "stage1.total",
    ]
    lines = _phase_lines(first.err)
    later = _phase_lines(second.err)
    assert [line["name"] for line in lines] == expected
    assert [line["name"] for line in later] == expected
    assert all(line["status"] is None for line in lines + later)
    # Every line of one build carries that build's ordinal, and the next
    # build in the same process gets the next ordinal.
    ordinals = {line["build"] for line in lines}
    assert len(ordinals) == 1 and None not in ordinals
    assert {line["build"] for line in later} == {str(int(ordinals.pop()) + 1)}


def test_phase_timing_clamps_child_cpu_that_steps_backwards(
    monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv(build_data.PHASE_TIMING_ENVIRONMENT, "1")
    samples = iter(
        [
            os.times_result((1.0, 1.0, 5.0, 5.0, 0.0)),
            os.times_result((1.0, 1.0, 4.9, 5.0, 0.0)),
        ]
    )
    monkeypatch.setattr(build_data.os, "times", lambda: next(samples))
    with build_data.phase("compose"):
        pass
    captured = capfd.readouterr()
    assert captured.out == ""
    [line] = captured.err.splitlines()
    assert _phase_lines(line)[0]["name"] == "compose"
    assert line.endswith(" children_cpu=0.0s")


def test_phase_timing_reports_a_raising_phase_and_reraises_the_same_error(
    monkeypatch: pytest.MonkeyPatch, capfd: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv(build_data.PHASE_TIMING_ENVIRONMENT, "1")
    error = build_data.BuildRefused("expected refusal")
    with pytest.raises(build_data.BuildRefused) as raised:
        with build_data.phase("stage1.projection"):
            raise error
    assert raised.value is error
    captured = capfd.readouterr()
    assert captured.out == ""
    [line] = _phase_lines(captured.err)
    assert line["name"] == "stage1.projection" and line["build"] is None
    assert line["status"] == " status=raised"


@pytest.fixture
def counted_validation(monkeypatch: pytest.MonkeyPatch) -> Iterator[list[int]]:
    """Start and end with an empty memo, and count every full validation."""
    build_data._VALIDATION_MEMO.clear()
    calls: list[int] = []
    real = build_data._validate_build_data

    def counted(value: Any) -> bytes:
        calls.append(1)
        return real(value)

    monkeypatch.setattr(build_data, "_validate_build_data", counted)
    yield calls
    build_data._VALIDATION_MEMO.clear()


def _verdict(value: Any) -> tuple[str, Any]:
    try:
        return ("pass", build_data._canonical_bytes(build_data.validate_build_data(value)))
    except build_data.BuildRefused as error:
        return ("refused", str(error))


def _unmemoized_verdict(monkeypatch: pytest.MonkeyPatch, value: Any) -> tuple[str, Any]:
    with monkeypatch.context() as patch:
        patch.setattr(build_data, "_plain_value_digest", lambda _value: None)
        return _verdict(value)


def test_validation_memo_reuses_a_pass_only_for_byte_identical_input(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, counted_validation: list[int]
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    before = len(counted_validation)
    first = build_data.validate_build_data(value)
    assert len(counted_validation) == before + 1
    second = build_data.validate_build_data(copy.deepcopy(value))
    assert len(counted_validation) == before + 1
    assert second == first and second is not first and second is not value
    # A caller mutating a returned verdict cannot change a later one.
    first["cells"].clear()
    first["counts"]["in_manifest_cell_count"] = -1
    third = build_data.validate_build_data(value)
    assert len(counted_validation) == before + 1
    assert third == second


def _reordered(value: dict[str, Any]) -> dict[str, Any]:
    return dict(reversed(list(value.items())))


def _count_to_bool(value: dict[str, Any]) -> dict[str, Any]:
    assert value["counts"]["in_manifest_cell_count"] == 1
    value["counts"]["in_manifest_cell_count"] = True
    return value


def _count_to_float(value: dict[str, Any]) -> dict[str, Any]:
    value["counts"]["in_manifest_cell_count"] = 1.0
    return value


def _cells_to_tuple(value: dict[str, Any]) -> dict[str, Any]:
    value["cells"] = tuple(value["cells"])
    return value


def _one_character(value: dict[str, Any]) -> dict[str, Any]:
    value["cells"][0]["test"] += "x"
    return value


def _false_admitted_count(value: dict[str, Any]) -> dict[str, Any]:
    value["runs"]["projection_admitted_physical_row_count"] = 0
    return value


def _dropped_field(value: dict[str, Any]) -> dict[str, Any]:
    del value["runs"]
    return value


@pytest.mark.parametrize(
    "change",
    [
        _reordered,
        _count_to_bool,
        _count_to_float,
        _cells_to_tuple,
        _one_character,
        _false_admitted_count,
        _dropped_field,
    ],
)
def test_validation_memo_revalidates_any_byte_difference(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    counted_validation: list[int],
    change: Any,
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    assert _verdict(value)[0] == "pass"
    changed = change(copy.deepcopy(value))
    expected = _unmemoized_verdict(monkeypatch, changed)
    before = len(counted_validation)
    assert _verdict(changed) == expected
    assert len(counted_validation) == before + 1
    # Asking again gives the same verdict.  Only a pass is remembered.
    assert _verdict(changed) == expected
    assert len(counted_validation) == before + 1 + (expected[0] == "refused")


def test_validation_memo_never_turns_a_refusal_into_a_pass(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, counted_validation: list[int]
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    value["runs"]["projection_admitted_physical_row_count"] = 0
    entries = dict(build_data._VALIDATION_MEMO)
    before = len(counted_validation)
    messages = []
    for _ in range(3):
        with pytest.raises(build_data.BuildRefused, match="admitted counts") as raised:
            build_data.validate_build_data(value)
        messages.append(str(raised.value))
    assert len(set(messages)) == 1
    assert len(counted_validation) == before + 3
    assert build_data._VALIDATION_MEMO == entries


class _Text(str):
    pass


class _Object(dict):
    pass


def _text_subclass(value: dict[str, Any]) -> dict[str, Any]:
    value["cells"][0]["test"] = _Text(value["cells"][0]["test"])
    return value


def _object_subclass(value: dict[str, Any]) -> dict[str, Any]:
    return _Object(value)


def _cyclic(value: dict[str, Any]) -> dict[str, Any]:
    value["cells"].append(value["cells"])
    return value


@pytest.mark.parametrize("change", [_text_subclass, _object_subclass, _cyclic])
def test_validation_memo_never_remembers_a_value_that_is_not_plain_data(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    counted_validation: list[int],
    change: Any,
) -> None:
    value = change(_validated_stage1_fixture(monkeypatch, tmp_path))
    assert build_data._plain_value_digest(value) is None
    expected = _unmemoized_verdict(monkeypatch, value)
    entries = dict(build_data._VALIDATION_MEMO)
    before = len(counted_validation)
    assert _verdict(value) == expected
    assert _verdict(value) == expected
    assert len(counted_validation) == before + 2
    assert build_data._VALIDATION_MEMO == entries


def test_plain_value_digest_separates_values_that_compare_equal() -> None:
    digest = build_data._plain_value_digest
    assert digest([1]) != digest([True]) != digest([1.0])
    assert digest([1]) != digest((1,))
    assert digest({"a": 1, "b": 2}) != digest({"b": 2, "a": 1})
    assert digest({"a": "x"}) == digest({"a": "x"})
    assert digest(_Text("x")) is None and digest({"a": _Object()}) is None


def _patched(target: str) -> tuple[Any, str, Any]:
    refusal = build_data.BuildRefused("patched validator refuses")

    def refuse(*_args: Any, **_kwargs: Any) -> Any:
        raise refusal

    modules = build_data._modules()
    stand_in = type(modules.reducer)("stand_in_reducer")
    vars(stand_in).update(
        {key: item for key, item in vars(modules.reducer).items() if not key.startswith("__")}
    )
    stand_in.reduce_inputs = refuse
    return {
        "publishable_output": (build_data, "_require_publishable_output", refuse),
        "schema_version": (build_data, "SCHEMA_VERSION", build_data.SCHEMA_VERSION + 1),
        "reducer": (modules.reducer, "reduce_inputs", refuse),
        "loaded_modules": (
            build_data,
            "_LOADED_MODULES",
            build_data.Modules(
                export_inputs=modules.export_inputs,
                reducer=stand_in,
                rust_toolchain=modules.rust_toolchain,
            ),
        ),
    }[target]


@pytest.mark.parametrize(
    "target", ["publishable_output", "schema_version", "reducer", "loaded_modules"]
)
def test_validation_memo_revalidates_when_reachable_validator_state_changes(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    counted_validation: list[int],
    target: str,
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    passed = _verdict(value)
    assert passed[0] == "pass"
    before = len(counted_validation)
    owner, name, replacement = _patched(target)
    with monkeypatch.context() as patch:
        patch.setattr(owner, name, replacement)
        with pytest.raises(build_data.BuildRefused):
            build_data.validate_build_data(value)
    assert len(counted_validation) == before + 1
    # Restoring the same objects restores the same state, so the first pass
    # is reused.
    assert _verdict(value) == passed
    assert len(counted_validation) == before + 1


def test_validation_memo_revalidates_when_a_recorded_digest_disagrees(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, counted_validation: list[int]
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    passed = _verdict(value)
    key = next(
        key
        for key in build_data._VALIDATION_MEMO
        if key[0] == build_data._plain_value_digest(value)
    )
    recorded = build_data._VALIDATION_MEMO[key]
    build_data._VALIDATION_MEMO[key] = (bytes(32), *recorded[1:])
    before = len(counted_validation)
    assert _verdict(value) == passed
    assert len(counted_validation) == before + 1


def _deeper(levels: int, call: Any) -> Any:
    """Run ``call`` with ``levels`` more Python frames on the stack."""
    if levels == 0:
        return call()
    return _deeper(levels - 1, call)


def test_validation_memo_revalidates_for_a_call_with_less_recursion_headroom(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, counted_validation: list[int]
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    build_data._VALIDATION_MEMO.clear()
    before = len(counted_validation)
    passed = _verdict(value)
    assert passed[0] == "pass" and len(counted_validation) == before + 1
    # A deeper caller has less headroom than the recorded pass had, so its
    # verdict is computed again...
    assert _deeper(50, lambda: _verdict(value)) == passed
    assert len(counted_validation) == before + 2
    # ...and the pass recorded with the smaller headroom then serves callers
    # at either depth.
    assert _deeper(50, lambda: _verdict(value)) == passed
    assert _verdict(value) == passed
    assert len(counted_validation) == before + 2
    # A lower recursion limit leaves less headroom at the same depth.
    limit = sys.getrecursionlimit()
    sys.setrecursionlimit(limit - 100)
    try:
        assert _verdict(value) == passed
    finally:
        sys.setrecursionlimit(limit)
    assert len(counted_validation) == before + 3


def test_validation_memo_keeps_a_refusal_that_only_a_deeper_caller_gets(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    real = build_data._validate_build_data
    shallow = build_data._recursion_headroom()

    # Stands in for the reference string walk, which runs out of frames on a
    # value nested close to the caller's headroom.
    def headroom_sensitive(candidate: Any) -> bytes:
        if build_data._recursion_headroom() < shallow - 40:
            raise RecursionError("maximum recursion depth exceeded")
        return real(candidate)

    monkeypatch.setattr(build_data, "_validate_build_data", headroom_sensitive)
    build_data._VALIDATION_MEMO.clear()
    try:
        passed = _verdict(value)
        assert passed[0] == "pass"
        assert _deeper(60, lambda: _verdict(value)) == (
            "refused",
            "build data is malformed: maximum recursion depth exceeded",
        )
        assert _verdict(value) == passed
    finally:
        build_data._VALIDATION_MEMO.clear()


def test_validation_memo_hit_refuses_errors_like_a_full_validation(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, counted_validation: list[int]
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    assert _verdict(value)[0] == "pass"
    before = len(counted_validation)

    def exhausted(*_args: Any, **_kwargs: Any) -> Any:
        raise RecursionError("maximum recursion depth exceeded while decoding")

    # Only the hit path decodes here: the digest and state walk do not.
    with monkeypatch.context() as patch:
        patch.setattr(build_data.json, "loads", exhausted)
        assert _verdict(value) == (
            "refused",
            "build data is malformed: maximum recursion depth exceeded while decoding",
        )
    assert len(counted_validation) == before


def test_validation_memo_keeps_at_most_sixteen_recent_passes(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, counted_validation: list[int]
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    import itertools

    orders = [
        {key: value[key] for key in order}
        for order in itertools.islice(itertools.permutations(value), 24)
    ]
    assert len(orders) > build_data._VALIDATION_MEMO_LIMIT + 1
    build_data._VALIDATION_MEMO.clear()
    start = len(counted_validation)
    for ordered in orders:
        assert _verdict(ordered)[0] == "pass"
        # Keep the first ordering recent; it must survive every eviction.
        assert _verdict(orders[0])[0] == "pass"
    # One full validation per distinct ordering: the recently used first
    # ordering was never evicted and validated again.
    assert len(counted_validation) - start == len(orders)
    assert len(build_data._VALIDATION_MEMO) == build_data._VALIDATION_MEMO_LIMIT
    before = len(counted_validation)
    assert _verdict(orders[0])[0] == "pass"
    assert len(counted_validation) == before
    assert _verdict(orders[1])[0] == "pass"
    assert len(counted_validation) == before + 1


def _audited_events(active: list[bool], events: list[tuple[str, Any]]) -> None:
    def audit(event: str, args: Any) -> None:
        if active[0] and (
            event in {"open", "socket.connect", "subprocess.Popen"}
            or event.startswith(("os.exec", "os.posix_spawn", "os.spawn", "os.system"))
            or event.startswith(("os.listdir", "os.scandir", "os.remove", "os.rename"))
        ):
            events.append((event, args))

    sys.addaudithook(audit)


def test_validation_never_mutates_its_input_and_performs_no_io_once_its_readers_load(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, counted_validation: list[int]
) -> None:
    import pickle

    # The fixture's build has already loaded every reader validation uses;
    # the cold first load is covered by the next test.
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    exact = pickle.dumps(value)
    build_data._VALIDATION_MEMO.clear()
    events: list[str] = []
    active = [True]

    def audit(event: str, _args: Any) -> None:
        if active[0] and (
            event in {"open", "socket.connect", "subprocess.Popen"}
            or event.startswith(("os.exec", "os.posix_spawn", "os.spawn", "os.system"))
            or event.startswith(("os.listdir", "os.scandir", "os.remove", "os.rename"))
        ):
            events.append(event)

    sys.addaudithook(audit)
    before = len(counted_validation)
    try:
        miss = build_data.validate_build_data(value)
        hit = build_data.validate_build_data(value)
    finally:
        active[0] = False
    assert len(counted_validation) == before + 1
    assert events == []
    assert miss == hit
    assert pickle.dumps(value) == exact


def test_validation_reads_only_its_proof_reader_on_first_use_and_then_hits(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, counted_validation: list[int]
) -> None:
    value = _validated_stage1_fixture(monkeypatch, tmp_path)
    export_inputs = build_data._modules().export_inputs
    # As in a fresh process: export_inputs loads series/finalized_proof.py
    # the first time captured_proof_objects needs it.
    monkeypatch.setattr(export_inputs, "finalized_proof", None)
    counted = build_data._validate_build_data

    # Stage 1 data built from a validation history commit carries a captured
    # proof, which _canonical_provenance checks with captured_proof_objects.
    # This fixture carries none, so make the same call on an empty proof.
    def with_proof_check(candidate: Any) -> bytes:
        export_inputs.captured_proof_objects(
            {"schema": 1, "manifests": [], "objects": {}, "missing_current_terminals": []}
        )
        return counted(candidate)

    monkeypatch.setattr(build_data, "_validate_build_data", with_proof_check)
    build_data._VALIDATION_MEMO.clear()
    events: list[tuple[str, Any]] = []
    active = [True]
    _audited_events(active, events)
    before = len(counted_validation)
    try:
        cold = build_data.validate_build_data(value)
        cold_events = list(events)
        events.clear()
        warm = build_data.validate_build_data(value)
    finally:
        active[0] = False
    assert export_inputs.finalized_proof is not None
    reader = Path(export_inputs.SERIES_DIR / "finalized_proof.py")
    # The only events open that module's source or its bytecode cache (a
    # valid cache is read instead of the source).
    assert cold_events and all(event == "open" for event, _args in cold_events), cold_events
    opened = [Path(str(args[0])) for _event, args in cold_events]
    assert all(
        path == reader
        or (path.parent == reader.parent / "__pycache__" and path.name.startswith("finalized_proof."))
        for path in opened
    ), opened
    # The state recorded after the pass includes the reader it bound, so the
    # next call hits and does no I/O.
    assert events == []
    assert len(counted_validation) == before + 1
    assert warm == cold


_MUTATING_METHODS = frozenset(
    {
        "add", "append", "clear", "difference_update", "discard", "extend",
        "insert", "intersection_update", "pop", "popitem", "remove", "reverse",
        "setdefault", "sort", "symmetric_difference_update", "update",
        "__setitem__", "__delitem__", "__iadd__", "__ior__",
    }
)


def _names_changed_in_place(sources: list[Path]) -> set[str]:
    """Every name some statement changes in place, reached by any path.

    ``NAME.append(...)``, ``module.NAME[key] = item``, ``del self.NAME[key]``
    and ``NAME |= ...`` all count.  Matching by name alone over-reports, which
    is the safe direction for this test.
    """

    def named(node: ast.AST) -> str | None:
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Attribute):
            return node.attr
        return None

    changed: set[str] = set()
    for source in sources:
        for node in ast.walk(ast.parse(source.read_text(), str(source))):
            names: list[str | None] = []
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
                if node.func.attr in _MUTATING_METHODS:
                    names.append(named(node.func.value))
            elif isinstance(node, (ast.Assign, ast.AugAssign, ast.AnnAssign, ast.Delete)):
                targets = (
                    node.targets
                    if isinstance(node, (ast.Assign, ast.Delete))
                    else [node.target]
                )
                for target in targets:
                    names.extend(
                        named(part.value)
                        for part in ast.walk(target)
                        if isinstance(part, ast.Subscript)
                        and isinstance(part.ctx, (ast.Store, ast.Del))
                    )
                if isinstance(node, ast.AugAssign):
                    names.append(named(node.target))
            changed.update(name for name in names if name is not None)
    return changed


def _held_state(namespace: dict[str, Any]) -> tuple[set[str], set[str], set[str]]:
    """Mutable containers, caches and stateful objects one module holds.

    It looks at the module's globals, at the attributes of every class the
    module defines, and at static and class methods.  Enum's own sunder
    tables (_member_map_ and the like) are built with the class and are not
    listed.
    """
    module = namespace.get("__name__")
    containers: set[str] = set()
    caches: set[str] = set()
    objects: set[str] = set()

    def judge(label: str, item: Any) -> None:
        if isinstance(item, (staticmethod, classmethod)):
            item = item.__func__
        if isinstance(item, (dict, list, set, bytearray)):
            containers.add(label)
        elif callable(item) and hasattr(item, "cache_info") and hasattr(item, "cache_clear"):
            caches.add(label)

    for name, item in namespace.items():
        if name.startswith("__"):
            continue
        judge(name, item)
        if isinstance(item, type) and item.__module__ == module:
            for attribute, member in vars(item).items():
                if attribute.startswith("__"):
                    continue
                if (
                    isinstance(item, enum.EnumMeta)
                    and attribute.startswith("_")
                    and attribute.endswith("_")
                ):
                    continue
                judge(f"{name}.{attribute}", member)
        elif (
            not isinstance(item, type)
            and getattr(type(item), "__module__", None) == module
            and (hasattr(item, "__dict__") or hasattr(type(item), "__dataclass_fields__"))
        ):
            objects.add(name)
    return containers, caches, objects


def test_validation_state_reaches_the_loaded_modules_and_holds_no_hidden_state(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _validated_stage1_fixture(monkeypatch, tmp_path)
    identities, anchors = build_data._validation_state()
    assert build_data._validation_state()[0] == identities
    namespaces = {
        id(anchor): anchor
        for anchor in anchors
        if isinstance(anchor, dict) and "__builtins__" in anchor
    }
    modules = build_data._modules()
    for module in (build_data, modules.export_inputs, modules.reducer, modules.rust_toolchain):
        assert id(vars(module)) in namespaces
    tests = str(REPOSITORY / "ci-hub/tests") + os.sep
    # A hosted export binds its canonical readers before it validates, and
    # through them validate_rows, ledger, qualifying_receipt and the rest
    # become reachable.  Bind them here too, so the scan does not depend on
    # which tests ran first.
    export_inputs = modules.export_inputs
    monkeypatch.setattr(sys, "path", list(sys.path))
    for name in (
        "ledger_store", "ledger_publisher", "series_store",
        "validation_ledger", "finalized_proof", "_AUTHORITY_IMPORT_READER_STATE",
    ):
        monkeypatch.setattr(export_inputs, name, getattr(export_inputs, name))
    export_inputs._AUTHORITY_IMPORT_READER_STATE = export_inputs._import_authorities(dict)
    namespaces = {
        id(anchor): anchor
        for anchor in build_data._validation_state()[1]
        if isinstance(anchor, dict) and "__builtins__" in anchor
        and not str(anchor.get("__file__", "")).startswith(tests)
    }
    files = {Path(namespace["__file__"]).name for namespace in namespaces.values()}
    assert {"validate_rows.py", "ledger.py", "qualifying_receipt.py", "finalized_proof.py"} <= files

    containers: set[tuple[str, str]] = set()
    caches: set[tuple[str, str]] = set()
    objects: set[tuple[str, str]] = set()
    for namespace in namespaces.values():
        held = _held_state(namespace)
        origin = Path(namespace["__file__"]).name
        for found, into in zip(held, (containers, caches, objects)):
            into.update((origin, name) for name in found)
    # The memo keys on identities, so a cache or an object whose attributes
    # can change unseen would let it replay a stale pass.  The loaded
    # Modules record is frozen, and the state walk records each field.
    assert caches == set(), sorted(caches)
    assert objects == {("build_data.py", "_LOADED_MODULES")}, sorted(objects)

    # A mutable container is safe while nothing changes it in place.  Check
    # every ci-hub source, since any module could change another's global.
    sources = [
        path
        for path in sorted((REPOSITORY / "ci-hub").rglob("*.py"))
        if "tests" not in path.relative_to(REPOSITORY / "ci-hub").parts
    ]
    changed = _names_changed_in_place(sources)
    exempt = {
        # The memo itself, and phase-timing bookkeeping no verdict reads.
        ("build_data.py", "_VALIDATION_MEMO"),
        ("build_data.py", "_PHASE_STATE"),
        # validation_evidence_batch pushes and pops this stack inside one
        # with block, so it is empty between calls unless a caller validates
        # inside such a block, and no website source enters one (checked
        # below).
        ("qualifying_receipt.py", "_VALIDATION_EVIDENCE_BATCHES"),
    }
    # The scan must see the changes it exists to catch.
    assert {name for _origin, name in exempt} <= changed
    assert exempt <= containers
    unsafe = {
        (origin, name)
        for origin, name in containers - exempt
        if name.rsplit(".", 1)[-1] in changed
    }
    assert unsafe == set(), sorted(unsafe)
    website = REPOSITORY / "ci-hub/compatibility-website"
    assert not any(
        "validation_evidence_batch" in path.read_text() for path in website.glob("*.py")
    )
