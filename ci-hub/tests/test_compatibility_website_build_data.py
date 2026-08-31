from __future__ import annotations

import copy
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


def git(repo: Path, *arguments: str) -> str:
    result = build_data._modules().bounded_capture.git(
        arguments,
        cwd=repo,
        check=True,
        text=True,
        timeout=60,
    )
    return result.stdout.strip()


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
    return {
        "schema": 1,
        "manifest_schema": 3,
        "manifest_sha256": "a" * 64,
        "tests": [],
        "cells": [
            {
                "test": cell.rsplit("/", 2)[0],
                "mode": cell.rsplit("/", 2)[1],
                "backend": cell.rsplit("/", 2)[2],
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
    return {
        "schema": schema,
        "event_id": event_id,
        "event_type": "series.observation",
        "emitted_at": "2026-08-31T00:00:00Z",
        "producer": producer,
        "run_id": run_id,
        "series": {
            "cell": cell,
            "tree": tree,
            "run_index": run_index,
            "num_runs": num_runs,
            "outcome": outcome,
            "result": result,
            "failure_class": failure_class,
        },
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


def canonical_input(
    parent_commit: str,
    rows: list[dict[str, Any]],
    *,
    validation_rows: list[dict[str, Any]] | None = None,
    projection: dict[str, str | None] | None = None,
) -> dict[str, Any]:
    validation_rows = validation_rows or []
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
                "checker_input_jsonl_sha256": build_data._checker_input_sha256(
                    rows
                ),
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
        },
        "validation_ledger": {
            "current_validation_run_rows": validation_rows,
            "current_validation_run_row_count": len(validation_rows),
            "current_validation_run_rows_sha256": build_data._digest(validation_rows),
        },
        "incomplete_data": [],
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
            result=None,
        ),
        series_row(
            "host-facts",
            hermit_commit,
            schema="stress-series/v2",
            producer="pressure-test",
            run_id="host-facts",
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


def test_absent_selection_stays_incomplete(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    row = series_row("event-1", hermit_commit)
    canonical = canonical_input(parent_commit, [row])
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


def test_legacy_current_row_without_stable_run_id_does_not_block_join(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    parent, parent_commit, hermit, hermit_commit = make_checkouts(tmp_path)
    legacy = {
        "schema_version": 1,
        "repo": "hermit",
        "profile": "full",
        "commit": hermit_commit,
        "cell_results": None,
    }
    row = series_row("event-1", hermit_commit)
    canonical = canonical_input(parent_commit, [row], validation_rows=[legacy])
    install_inputs(monkeypatch, canonical, manifest_metadata())

    value = build_data.build_data(
        parent, parent_commit, hermit, hermit_commit, hermit_commit
    )

    assert value["cells"][0]["selected_by"] == {}
    assert any(
        item["field"] == "validate path selection"
        for item in value["incomplete_data"]
    )


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
