from __future__ import annotations

import importlib.util
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import pytest


REPOSITORY = Path(__file__).resolve().parents[2]
SCRIPT = REPOSITORY / "ci-hub/compatibility-website/export_inputs.py"
SPEC = importlib.util.spec_from_file_location(
    "compatibility_website_export_inputs",
    SCRIPT,
)
assert SPEC is not None and SPEC.loader is not None
export_inputs = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = export_inputs
SPEC.loader.exec_module(export_inputs)
REAL_READER_STATE = export_inputs._reader_state
TEST_READER_STATE = {
    "ci_hub_tree": "a" * 40,
    "commit": "a" * 40,
    "dirty_paths": [],
    "files": [],
    "files_sha256": export_inputs._value_sha256([]),
    "hermit": {
        "checkout_commit": "b" * 40,
        "gitlink_commit": "b" * 40,
    },
    "source_identity_sha256": "c" * 64,
    "worktree_clean": True,
}
export_inputs._reader_state = lambda: TEST_READER_STATE
export_inputs._load_authorities()


def git(repo: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *arguments],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def write_fake_git(path: Path, body: str) -> None:
    path.write_text("#!/usr/bin/python3\n" + body)
    path.chmod(0o755)


def assert_process_group_gone(marker: Path) -> None:
    process_group = int(marker.read_text())
    deadline = time.monotonic() + 2
    while True:
        try:
            os.killpg(process_group, 0)
        except ProcessLookupError:
            return
        if time.monotonic() >= deadline:
            pytest.fail(f"process group {process_group} survived bounded capture")
        time.sleep(0.01)


@pytest.mark.parametrize("_iteration", range(5))
def test_git_returns_when_a_descendant_holds_output_descriptors(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, _iteration: int
) -> None:
    marker = tmp_path / "process-group"
    write_fake_git(
        tmp_path / "git",
        "import os, pathlib, signal, time\n"
        "if os.fork() == 0:\n"
        "    signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
        "    time.sleep(30)\n"
        "    os._exit(0)\n"
        "pathlib.Path(os.environ['WEBSITE_PROCESS_GROUP']).write_text(str(os.getpgrp()))\n"
        "os.write(1, b'ready\\n')\n"
        "os._exit(0)\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.setenv("WEBSITE_PROCESS_GROUP", str(marker))

    started = time.monotonic()
    assert export_inputs._git(tmp_path, ["fixture"]) == b"ready\n"
    assert time.monotonic() - started < 2
    assert_process_group_gone(marker)


def test_git_timeout_kills_the_direct_child_before_it_finishes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    marker = tmp_path / "finished"
    write_fake_git(
        tmp_path / "git",
        "import os, pathlib, time\n"
        "time.sleep(30)\n"
        "pathlib.Path(os.environ['WEBSITE_TEST_MARKER']).write_bytes(b'finished')\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.setenv("WEBSITE_TEST_MARKER", str(marker))
    monkeypatch.setattr(export_inputs, "_GIT_TIMEOUT_SECONDS", 0.05)

    with pytest.raises(export_inputs.ExportRefused, match="timed out"):
        export_inputs._git(tmp_path, ["fixture"])
    assert not marker.exists()


@pytest.mark.parametrize(
    ("body", "message"),
    [
        (
            "import os; os.write(2, b'failure detail\\n'); raise SystemExit(7)\n",
            "failure detail",
        ),
        (
            "import os; os.write(1, b'ok\\n'); os.write(2, b'unexpected detail\\n')\n",
            "wrote unexpected stderr",
        ),
    ],
)
def test_git_refuses_nonzero_and_success_stderr(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    body: str,
    message: str,
) -> None:
    write_fake_git(tmp_path / "git", body)
    monkeypatch.setenv("PATH", str(tmp_path))

    with pytest.raises(export_inputs.ExportRefused, match=message):
        export_inputs._git(tmp_path, ["fixture"])


def test_git_failure_diagnostic_is_bounded(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    write_fake_git(
        tmp_path / "git",
        "import os\n"
        f"os.write(2, b'x' * {export_inputs._MAX_PROCESS_DETAIL_BYTES + 1000})\n"
        "raise SystemExit(7)\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path))

    with pytest.raises(export_inputs.ExportRefused) as raised:
        export_inputs._git(tmp_path, ["fixture"])

    message = str(raised.value)
    assert message.endswith("... [truncated]")
    assert len(message) < export_inputs._MAX_PROCESS_DETAIL_BYTES + 200


@pytest.mark.parametrize(
    ("stream", "descriptor"),
    [("stdout", 1), ("stderr", 2)],
)
def test_git_refuses_output_over_the_cap_before_the_child_finishes(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    stream: str,
    descriptor: int,
) -> None:
    marker = tmp_path / "finished"
    write_fake_git(
        tmp_path / "git",
        "import os, pathlib, time\n"
        f"os.write({descriptor}, b'x' * 9)\n"
        "time.sleep(30)\n"
        "pathlib.Path(os.environ['WEBSITE_TEST_MARKER']).write_bytes(b'finished')\n",
    )
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.setenv("WEBSITE_TEST_MARKER", str(marker))
    monkeypatch.setattr(export_inputs, f"_MAX_GIT_{stream.upper()}_BYTES", 8)

    with pytest.raises(
        export_inputs.ExportRefused, match=rf"{stream} exceeded 8 bytes"
    ):
        export_inputs._git(tmp_path, ["fixture"])
    assert not marker.exists()


def repository(
    tmp_path: Path, files: dict[str, bytes] | None = None
) -> tuple[Path, str]:
    repo = tmp_path / "parent"
    repo.mkdir()
    git(repo, "init", "-q")
    git(repo, "config", "user.email", "compatibility-website@example.invalid")
    git(repo, "config", "user.name", "compatibility website test")
    for relative, data in (files or {}).items():
        path = repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    if files:
        git(repo, "add", *sorted(files))
        git(repo, "commit", "-qm", "fixture inputs")
    else:
        git(repo, "commit", "--allow-empty", "-qm", "empty fixture")
    return repo, git(repo, "rev-parse", "HEAD")


def write_spool(repo: Path, relative: str, body: bytes = b"{}\n") -> Path:
    path = repo / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(body)
    return path


def build_export(repo: Path, commit: str) -> dict[str, Any]:
    return export_inputs.build_export(repo, commit)


def source_fixture(tmp_path: Path) -> tuple[Path, str]:
    repo, commit = repository(
        tmp_path,
        {
            "ledger/hermit/hosta/2026-08.jsonl": b"{}\n",
            "ledger-recovery/hermit/hosta/2026-08/recovered.jsonl": b"{}\n",
            "series/hermit/hosta/2026-08.jsonl": b"{}\n",
        },
    )
    write_spool(
        repo,
        "ignored/ci-hub/validate-ledger-spool/" "hermit__hosta__2026-08__live.jsonl",
    )
    write_spool(
        repo,
        "ignored/ci-hub/validate-ledger-spool/published/"
        "hermit__hosta__2026-08__retained.jsonl",
    )
    write_spool(
        repo,
        "ignored/ci-hub/series-spool/hermit__hosta__2026-08__live.jsonl",
    )
    write_spool(
        repo,
        "ignored/ci-hub/series-spool/published/"
        "hermit__hosta__2026-08__retained.jsonl",
    )
    return repo, commit


def validation_event() -> dict[str, Any]:
    return {
        "schema": "validate-ledger/v1",
        "event_id": "validation-event-1",
        "event_type": "run.result",
        "emitted_at": "2026-08-30T01:00:00Z",
        "team": "hermit",
        "host": "hosta",
        "run_id": "validation-run-1",
        "producer": {
            "source": "observed",
            "tool": "compatibility-website-test",
            "tool_version": "7",
        },
        "commit": "a" * 40,
        "outcome": "pass",
        "legacy_row": {
            "schema_version": 7,
            "record_id": "validation-event-1",
            "run_id": "validation-run-1",
            "repo": "hermit",
            "producer": "compatibility-website-test",
            "host": "hosta",
            "started_at": "2026-08-30T00:59:00Z",
            "finished_at": "2026-08-30T01:00:00Z",
            "result": "pass",
            "commit": "a" * 40,
            "cwd": "{{WORKSPACE_ROOT}}/hermit",
        },
    }


def pre_comparison_field_row(
    *,
    record_id: str,
    commit: str,
    run_id: str,
    artifact_path: str,
    artifact_sha256: str,
    compared_messages: int,
) -> dict[str, Any]:
    identity = {
        "backend": "ptrace",
        "category": "applications",
        "lane": "portable",
        "mode": "verify",
        "test": "applications/timed-progress-bar",
    }
    return {
        "record_id": record_id,
        "commit": commit,
        "schema_version": 6,
        "run_id": run_id,
        "cell_results": {
            "artifact": {
                "path": artifact_path,
                "row_count": 1,
                "sha256": artifact_sha256,
            },
            "cells": [
                {
                    **identity,
                    "cell_verdict": {
                        "bitwise_parity": True,
                        "compared_log_messages": {
                            "left": compared_messages,
                            "right": compared_messages,
                        },
                        "comparison": {
                            "canonicalizations": [
                                "host-address-to-first-appearance-ordinal/v1"
                            ],
                            "canonicalize_addresses": True,
                            "compare_logs": True,
                            "exact_remainder": True,
                            "full_trace": True,
                            "ignore_lines": False,
                            "log_scope": "info",
                            "skip_commit": False,
                            "skip_detlog": False,
                            "strictness": "canonical",
                            "strip_lines": False,
                            "stripped_prefixes": ["real-wall-clock-prefix/v1"],
                        },
                        "comparison_tier": "canonical-bitwise",
                        "state": "compared-and-matched",
                    },
                }
            ],
            "hermit_sha": commit,
            "population_sha256": (
                "92652bcf4b3ae41ded219a3aaaebdabbd1118141daa2153b82bca354a3cbb456"
            ),
            "recorded_count": 1,
            "run_id": run_id,
            "selected": [identity],
            "selected_count": 1,
            "source_tree_dirty": False,
        },
    }


def august_17_pre_comparison_field_rows() -> list[dict[str, Any]]:
    return [
        pre_comparison_field_row(
            record_id="devbig014-1787003302-994556",
            commit="99e378524b9a6a22f5b74ab24192b906ab371a13",
            run_id=(
                "portable-applications-applications-timed-progress-bar-verify-"
                "ptrace-repetition-0001"
            ),
            artifact_path=(
                "ignored/validate/artifacts/portable-applications-applications-"
                "timed-progress-bar-verify-ptrace-repetition-0001/cell-results.jsonl"
            ),
            artifact_sha256=(
                "f851f2b82dbfc941f5ca101d88411bedb8796e61366d9d0c0b00fb491e3d580e"
            ),
            compared_messages=106662,
        ),
        pre_comparison_field_row(
            record_id="devbig014-1787005074-1930458",
            commit="88d4d61bfa9be43fa10513980485e9e55ef353dc",
            run_id=(
                "validate-cell-requalification-88d4d61bfa9b-20260817T220655Z-"
                "pid1935623--portable-applications-applications-timed-progress-bar-"
                "verify-ptrace-repetition-0001"
            ),
            artifact_path=(
                "ignored/validate/artifacts/validate-cell-requalification-"
                "88d4d61bfa9b-20260817T220655Z-pid1935623--portable-applications-"
                "applications-timed-progress-bar-verify-ptrace-repetition-0001/"
                "cell-results.jsonl"
            ),
            artifact_sha256=(
                "5bee38c8e3b9f6a6476fe07ad06f8fc8584fe22dfe48bc39411d1bb64115c938"
            ),
            compared_messages=106663,
        ),
    ]


def historical_validation_row(
    record_id: str = "record-1", run_id: str = "run-1"
) -> dict[str, Any]:
    return {
        "cell_results": {
            "artifact": {
                "path": f"ignored/validate/artifacts/{run_id}/cell-results.jsonl",
                "row_count": 0,
                "sha256": "b" * 64,
            },
            "cells": [],
            "hermit_sha": "c" * 40,
            "population_sha256": "a" * 64,
            "recorded_count": 0,
            "run_id": run_id,
            "selected": [
                {
                    "backend": "ptrace",
                    "category": "c-programs",
                    "lane": "portable",
                    "mode": "verify",
                    "test": "tests/hello",
                }
            ],
            "selected_count": 1,
            "source_tree_dirty": False,
        },
        "commit": "c" * 40,
        "finished_at": "2026-08-30T01:00:00Z",
        "profile": "full",
        "record_id": record_id,
        "repo": "hermit",
        "run_id": run_id,
        "schema_version": 7,
        "selection_mode": "full",
        "started_at": "2026-08-30T00:00:00Z",
    }


def series_row(**changes: Any) -> dict[str, Any]:
    row = {
        "schema": "stress-series/v1",
        "event_id": "series-event-1",
        "event_type": "series.observation",
        "emitted_at": "2026-08-30T01:00:00Z",
        "team": "hermit",
        "host": "hosta",
        "run_id": "series-run-1",
        "producer": "pressure-test",
        "series": {
            "cell": "tests/hello/verify/ptrace",
            "tree": "a" * 40,
            "run_index": 0,
            "outcome": "passed",
        },
    }
    for key, value in changes.items():
        if key == "series":
            row["series"] = {**row["series"], **value}
        else:
            row[key] = value
    return row


def current_series_row(**changes: Any) -> dict[str, Any]:
    series_changes = {
        "failure_class": None,
        "host_capabilities": {
            "cpuid-faulting": {
                "evidence": "fixture absence",
                "present": False,
            },
            "kvm": {
                "evidence": "fixture presence",
                "present": True,
            },
        },
        "kernel_version": "7.1.3-test",
        "machine_shortname": "hosta",
        "result": "pass",
        **changes.pop("series", {}),
    }
    return series_row(
        schema="stress-series/v3",
        series=series_changes,
        **changes,
    )


def install_authority_fixtures(
    monkeypatch: pytest.MonkeyPatch,
    commit: str,
) -> dict[str, Any]:
    event = {
        "schema": "validate-ledger/v1",
        "event_id": "validation-event",
        "event_type": "run.result",
        "emitted_at": "2026-08-30T01:00:00Z",
        "run_id": "validation-run",
        "unknown_event_field": {"kept": True},
    }
    published_series = {
        "schema": "stress-series/v1",
        "event_id": "series-published",
        "event_type": "series.observation",
        "emitted_at": "2026-08-30T01:00:00Z",
        "run_id": "series-run",
        "series": {
            "cell": "tests/hello/verify/ptrace",
            "num_runs": 1,
            "unknown_series_field": "kept",
        },
    }
    local_series = {
        "schema": "stress-series/v3",
        "event_id": "series-local",
        "event_type": "series.observation",
        "emitted_at": "2026-08-30T02:00:00Z",
        "run_id": "series-run-local",
        "series": {
            "cell": "tests/other/verify/ptrace",
            "num_runs": 3,
        },
    }

    monkeypatch.setattr(
        export_inputs.validation_ledger,
        "events",
        lambda *, root: [event],
    )

    def history_rows(*, root: Path) -> list[dict]:
        return [
            {
                "schema_version": 7,
                "record_id": "old",
                "run_id": "validation-run",
                "cwd": str(root / "worktrees/slot/hermit"),
                "unknown_history_field": {"kept": True},
            },
            {
                "schema_version": 7,
                "record_id": "new",
                "run_id": "validation-run",
                "corrects": "old",
                "cwd": str(root / "worktrees/slot/hermit"),
            },
        ]

    monkeypatch.setattr(export_inputs.validation_ledger, "rows", history_rows)
    current_inputs: list[list[dict]] = []
    current_projection = export_inputs.validation_ledger.current_validation_run_rows

    def current_validation_run_rows(rows):
        current_inputs.append(rows)
        return current_projection(rows)

    monkeypatch.setattr(
        export_inputs.validation_ledger,
        "current_validation_run_rows",
        current_validation_run_rows,
    )

    published_calls: list[tuple[Path, str]] = []
    projection_calls: list[list[dict[str, Any]]] = []
    union_calls: list[list[Path]] = []

    def published_snapshot_at(parent: Path, reference: str):
        published_calls.append((parent, reference))
        return export_inputs.series_store.PublishedSnapshot(
            commit=commit,
            rows=[published_series],
        )

    monkeypatch.setattr(
        export_inputs.series_store,
        "published_snapshot_at",
        published_snapshot_at,
    )

    def union(paths):
        paths = list(paths)
        union_calls.append(paths)
        has_local = any(
            "ignored/ci-hub/series-spool" in path.as_posix() for path in paths
        )
        has_published = any("/series/" in path.as_posix() for path in paths)
        if has_local and has_published:
            return [published_series, local_series]
        if has_local:
            return [local_series]
        return [published_series] if has_published else []

    monkeypatch.setattr(export_inputs.series_store, "union", union)

    def projection_results(rows):
        rows = list(rows)
        projection_calls.append(rows)
        return export_inputs.series_store.ProjectionResults(
            row_count=len(rows),
            input_jsonl_sha256="c" * 64,
            results={
                "series-local": None,
                "series-published": (
                    "stress-series/v1 does not record machine_shortname, "
                    "kernel_version, and host_capabilities"
                ),
            },
        )

    monkeypatch.setattr(
        export_inputs.series_store,
        "projection_results",
        projection_results,
    )
    monkeypatch.setattr(
        export_inputs.series_store,
        "refresh_origin_main",
        lambda *_args, **_kwargs: pytest.fail("exporter fetched origin/main"),
    )
    return {
        "event": event,
        "current_inputs": current_inputs,
        "local_series": local_series,
        "published_calls": published_calls,
        "published_series": published_series,
        "projection_calls": projection_calls,
        "union_calls": union_calls,
    }


def recursive_keys(value: Any) -> set[str]:
    if isinstance(value, dict):
        return set(value).union(*(recursive_keys(item) for item in value.values()))
    if isinstance(value, list):
        return set().union(*(recursive_keys(item) for item in value))
    return set()


def jsonl_rows(value: str) -> list[dict[str, Any]]:
    return [json.loads(line) for line in value.splitlines()]


def test_empty_inputs_stay_empty_without_a_stored_measurement_state(tmp_path: Path):
    repo, commit = repository(tmp_path)

    first = build_export(repo, commit)
    second = build_export(repo, commit)

    assert export_inputs.render_export(first) == export_inputs.render_export(second)
    assert first["schema_version"] == 1
    assert first["validation_ledger"] == {
        "event_count": 0,
        "events_sha256": export_inputs._value_sha256([]),
        "history_row_count": 0,
        "history_rows_jsonl": "",
        "history_rows_sha256": hashlib.sha256(b"").hexdigest(),
    }
    assert first["series"] == {
        "row_count": 0,
        "rows_jsonl": "",
        "rows_sha256": hashlib.sha256(b"").hexdigest(),
    }
    assert first["provenance"]["source_count"] == 0
    assert {"measured", "measurement_state", "never_measured"}.isdisjoint(
        recursive_keys(first)
    )
    assert "state" not in first
    assert "state" not in first["series"]
    assert "state" not in first["validation_ledger"]


def test_export_includes_valid_rows_from_a_partially_rejected_pressure_campaign(
    tmp_path: Path,
):
    repo, commit = repository(tmp_path)

    def pressure_row(event_id: str, cell: str) -> dict[str, Any]:
        return series_row(
            schema="stress-series/v3",
            event_id=event_id,
            run_id="pressure-mixed-evidence",
            series={
                "cell": cell,
                "failure_class": None,
                "host_capabilities": {
                    "cpuid-faulting": {
                        "present": False,
                        "evidence": "fixture absence",
                    },
                    "kvm": {"present": True, "evidence": "fixture presence"},
                },
                "kernel_version": "7.1.3-test",
                "machine_shortname": "hosta",
                "num_runs": 1,
                "outcome": "passed",
                "result": "pass",
                "run_index": 0,
                "tree": "a" * 40,
            },
        )

    retained = [
        pressure_row("series-pressure-valid-1", "applications/thread-output/verify/kvm"),
        pressure_row(
            "series-pressure-valid-2",
            "applications/perl-io-subprocess-time/verify/kvm",
        ),
    ]
    write_spool(
        repo,
        "ignored/ci-hub/series-spool/pressure-mixed-evidence.jsonl",
        b"".join(json.dumps(row, sort_keys=True).encode() + b"\n" for row in retained),
    )
    # The rejected input remains in its pressure evidence tree, outside the
    # canonical series store. It must neither hide the valid observations nor
    # be mistaken for one by the exporter.
    write_spool(
        repo,
        "ignored/compat-envelope/pressure-mixed-evidence/"
        "invalid-verify-kvm/results.jsonl",
        b'{"attempts":[],"mode":"verify","outcome":"ERROR"}\n',
    )

    output = build_export(repo, commit)

    assert output["series"]["row_count"] == 2
    assert len(output["series"]["rows_jsonl"].splitlines()) == 2
    assert [
        json.loads(line) for line in output["series"]["rows_jsonl"].splitlines()
    ] == retained
    assert {
        (source["store"], source["location"])
        for source in output["provenance"]["sources"]
    } == {("series", "live")}
    assert "invalid-verify-kvm" not in json.dumps(output)


def test_real_nonempty_readers_keep_v1_and_deduplicate_published_and_live(
    tmp_path: Path,
):
    event = validation_event()
    observed = series_row()
    repo, commit = repository(
        tmp_path,
        {
            "ledger/hermit/hosta/2026-08.jsonl": (
                json.dumps(event, sort_keys=True).encode() + b"\n"
            ),
            "series/hermit/hosta/2026-08.jsonl": (
                json.dumps(observed, sort_keys=True).encode() + b"\n"
            ),
        },
    )
    write_spool(
        repo,
        "ignored/ci-hub/series-spool/hermit__hosta__2026-08__duplicate.jsonl",
        json.dumps(observed, sort_keys=True).encode() + b"\n",
    )

    output = build_export(repo, commit)

    assert output["validation_ledger"]["event_count"] == 1
    assert output["validation_ledger"]["history_row_count"] == 1
    assert output["series"]["row_count"] == 1
    assert jsonl_rows(output["series"]["rows_jsonl"]) == [observed]
    assert (
        output["series"]["rows_sha256"]
        == hashlib.sha256(output["series"]["rows_jsonl"].encode()).hexdigest()
    )
    assert output["provenance"]["source_count"] == 3


def test_export_delegates_to_authorities_and_records_all_source_locations(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    repo, commit = source_fixture(tmp_path)
    fixtures = install_authority_fixtures(monkeypatch, commit)
    monkeypatch.setattr(
        export_inputs.validation_ledger,
        "_replace",
        lambda *_args, **_kwargs: pytest.fail("private _replace was called"),
    )

    output = build_export(repo, commit)

    assert fixtures["published_calls"] == [(repo.resolve(), commit)]
    assert len(fixtures["union_calls"]) == 2
    assert all("/series/" in path.as_posix() for path in fixtures["union_calls"][0])
    assert any("/series/" in path.as_posix() for path in fixtures["union_calls"][1])
    assert any(
        "ignored/ci-hub/series-spool" in path.as_posix()
        for path in fixtures["union_calls"][1]
    )
    assert output["validation_ledger"]["history_row_count"] == 2
    history_rows = jsonl_rows(output["validation_ledger"]["history_rows_jsonl"])
    assert history_rows[0]["unknown_history_field"] == {"kept": True}
    assert history_rows[0]["cwd"].startswith("{{WORKSPACE_ROOT}}/")
    assert fixtures["current_inputs"] == []

    assert jsonl_rows(output["series"]["rows_jsonl"]) == [
        fixtures["published_series"],
        fixtures["local_series"],
    ]
    assert fixtures["projection_calls"] == []

    locations = {
        (source["store"], source["location"])
        for source in output["provenance"]["sources"]
    }
    assert locations == {
        ("series", "live"),
        ("series", "published"),
        ("series", "retained"),
        ("validation-ledger", "live"),
        ("validation-ledger", "published"),
        ("validation-ledger", "recovery"),
        ("validation-ledger", "retained"),
    }
    assert output["provenance"]["source_count"] == 7
    rendered = export_inputs.render_export(output).decode()
    assert str(repo) not in rendered
    assert "/home/" not in rendered
    assert output["incomplete_data"]
    assert [item["field"] for item in output["incomplete_data"]] == [
        "fields allowed for publication",
        "row source path and line",
        "HistoryRow source path and line",
    ]


def test_exporter_keeps_series_opaque_for_the_single_rust_projection(
    tmp_path: Path,
):
    legacy = series_row(event_id="legacy")
    current = current_series_row(
        event_id="current",
        emitted_at="2026-08-30T02:00:00Z",
        series={"run_index": 1},
    )
    dirty = current_series_row(
        event_id="dirty",
        emitted_at="2026-08-30T03:00:00Z",
        series={"run_index": 2, "source_tree_dirty": True},
    )
    repo, commit = repository(
        tmp_path,
        {
            "series/hermit/hosta/2026-08.jsonl": "".join(
                json.dumps(item, sort_keys=True) + "\n"
                for item in (dirty, legacy, current)
            ).encode(),
        },
    )

    output = build_export(repo, commit)
    exported = output["series"]

    assert {row["event_id"] for row in jsonl_rows(exported["rows_jsonl"])} == {
        "current",
        "dirty",
        "legacy",
    }
    assert set(exported) == {"row_count", "rows_jsonl", "rows_sha256"}
    assert (
        exported["rows_sha256"]
        == hashlib.sha256(exported["rows_jsonl"].encode()).hexdigest()
    )


def test_one_series_field_mutation_changes_the_exact_jsonl_binding(tmp_path: Path):
    original = current_series_row()
    mutated = current_series_row(emitted_at="2026-08-30T01:00:01Z")
    outputs = []
    for name, fixture in (("original", original), ("mutated", mutated)):
        fixture_root = tmp_path / name
        fixture_root.mkdir()
        repo, commit = repository(
            fixture_root,
            {
                "series/hermit/hosta/2026-08.jsonl": (
                    json.dumps(fixture, sort_keys=True).encode() + b"\n"
                ),
            },
        )
        outputs.append(build_export(repo, commit)["series"])

    before, after = outputs
    assert before["rows_sha256"] != after["rows_sha256"]
    assert before["rows_jsonl"] != after["rows_jsonl"]


def test_exporter_never_calls_the_python_projection_classifier(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    fixture = current_series_row()
    repo, commit = repository(
        tmp_path,
        {
            "series/hermit/hosta/2026-08.jsonl": (
                json.dumps(fixture, sort_keys=True).encode() + b"\n"
            ),
        },
    )
    monkeypatch.setattr(
        export_inputs.series_store,
        "projection_results",
        lambda _rows: pytest.fail("Python classified a SeriesRow"),
    )
    assert build_export(repo, commit)["series"]["row_count"] == 1


def test_exact_august_17_rows_stay_in_opaque_history(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
):
    rows = august_17_pre_comparison_field_rows()
    monkeypatch.setattr(export_inputs.validation_ledger, "events", lambda *, root: [])
    monkeypatch.setattr(export_inputs.validation_ledger, "rows", lambda *, root: rows)

    validation = export_inputs._validation_export(tmp_path)
    exported_rows = [
        json.loads(line) for line in validation["history_rows_jsonl"].splitlines()
    ]

    assert exported_rows == rows
    assert validation["history_row_count"] == 2


def test_validation_acceptance_fixture_is_excluded_by_the_canonical_reader(
    tmp_path: Path,
):
    event = {
        "event_id": "acceptance-fixture",
        "emitted_at": "2026-08-30T00:00:00Z",
        "producer": {"tool": "ledger-live-sync-acceptance"},
    }
    body = json.dumps(event, sort_keys=True).encode() + b"\n"
    repo, commit = repository(
        tmp_path,
        {"ledger/hermit/hosta/2026-08.jsonl": body},
    )

    output = build_export(repo, commit)

    assert output["validation_ledger"]["event_count"] == 0
    assert output["validation_ledger"]["history_rows_jsonl"] == ""
    assert output["provenance"]["source_count"] == 1


def test_truncated_validation_shard_refuses(tmp_path: Path):
    repo, commit = repository(
        tmp_path,
        {"ledger/hermit/hosta/2026-08.jsonl": b'{"event_id":"cut"}'},
    )

    with pytest.raises(export_inputs.ledger_store.MalformedEvent, match="terminator"):
        build_export(repo, commit)


def test_conflicting_validation_event_id_refuses(tmp_path: Path):
    first = b'{"event_id":"same","emitted_at":"2026-08-30T00:00:00Z"}\n'
    second = b'{"event_id":"same","emitted_at":"2026-08-30T01:00:00Z"}\n'
    repo, commit = repository(
        tmp_path,
        {
            "ledger/hermit/hosta/2026-08.jsonl": first,
            "ledger/hermit/hostb/2026-08.jsonl": second,
        },
    )

    with pytest.raises(export_inputs.ledger_store.ConflictingEventId):
        build_export(repo, commit)


def test_content_change_during_spool_capture_refuses(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    repo, _commit = repository(tmp_path)
    batch = write_spool(
        repo,
        "ignored/ci-hub/series-spool/hermit__hosta__2026-08__live.jsonl",
        b"one\n",
    )
    original = export_inputs._read_regular_file
    reads = 0

    def changing(path: Path) -> bytes:
        nonlocal reads
        data = original(path)
        if path == batch and reads == 0:
            reads += 1
            batch.write_bytes(b"two\n")
        return data

    monkeypatch.setattr(export_inputs, "_read_regular_file", changing)

    with pytest.raises(export_inputs.ExportRefused, match="changed while"):
        export_inputs._capture_mutable_sources(repo, repo)


def test_path_set_change_during_spool_capture_refuses(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    repo, _commit = repository(tmp_path)
    original = export_inputs._discover_mutable_sources
    calls = 0

    def changing(parent: Path, data_root: Path):
        nonlocal calls
        found = original(parent, data_root)
        calls += 1
        if calls == 1:
            write_spool(
                repo,
                "ignored/ci-hub/validate-ledger-spool/"
                "hermit__hosta__2026-08__new.jsonl",
            )
        return found

    monkeypatch.setattr(export_inputs, "_discover_mutable_sources", changing)

    with pytest.raises(export_inputs.ExportRefused, match="path set changed"):
        export_inputs._capture_mutable_sources(repo, repo)


def test_symlinked_spool_batch_refuses(tmp_path: Path):
    repo, _commit = repository(tmp_path)
    outside = tmp_path / "outside.jsonl"
    outside.write_text("{}\n")
    batch = repo / ("ignored/ci-hub/series-spool/hermit__hosta__2026-08__link.jsonl")
    batch.parent.mkdir(parents=True)
    batch.symlink_to(outside)

    with pytest.raises(export_inputs.ExportRefused, match="contains a symlink"):
        export_inputs._capture_mutable_sources(repo, repo)


def test_symlinked_spool_directory_refuses(tmp_path: Path):
    repo, _commit = repository(tmp_path)
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "external.jsonl").write_text("{}\n")
    spool = repo / "ignored/ci-hub/series-spool"
    spool.parent.mkdir(parents=True)
    spool.symlink_to(outside, target_is_directory=True)

    with pytest.raises(export_inputs.ExportRefused, match="contains a symlink"):
        export_inputs._capture_mutable_sources(repo, repo)


def test_fifo_spool_entry_refuses_without_opening_it(tmp_path: Path):
    repo, _commit = repository(tmp_path)
    fifo = repo / ("ignored/ci-hub/series-spool/hermit__hosta__2026-08__fifo.jsonl")
    fifo.parent.mkdir(parents=True)
    os.mkfifo(fifo)

    with pytest.raises(export_inputs.ExportRefused, match="not a regular file"):
        export_inputs._capture_mutable_sources(repo, repo)


def test_authority_returned_unknown_series_source_refuses(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    repo, _commit = repository(tmp_path)
    unknown = write_spool(
        repo,
        "ignored/ci-hub/future-series-source/rows.jsonl",
    )
    monkeypatch.setattr(
        export_inputs.series_store,
        "store_paths",
        lambda _parent: [unknown],
    )

    with pytest.raises(export_inputs.ExportRefused, match="unrecognised source"):
        export_inputs._capture_mutable_sources(repo, repo)


def test_tracked_symlink_at_a_ledger_path_refuses(tmp_path: Path):
    repo = tmp_path / "parent"
    repo.mkdir()
    git(repo, "init", "-q")
    git(repo, "config", "user.email", "compatibility-website@example.invalid")
    git(repo, "config", "user.name", "compatibility website test")
    payload = repo / "payload.jsonl"
    payload.write_text("{}\n")
    shard = repo / "ledger/hermit/hosta/2026-08.jsonl"
    shard.parent.mkdir(parents=True)
    shard.symlink_to(payload)
    git(repo, "add", "payload.jsonl", "ledger/hermit/hosta/2026-08.jsonl")
    git(repo, "commit", "-qm", "symlink fixture")
    commit = git(repo, "rev-parse", "HEAD")

    with pytest.raises(export_inputs.ExportRefused, match="not a regular blob"):
        build_export(repo, commit)


def test_conflicting_published_and_live_series_event_refuses(tmp_path: Path):
    published = series_row()
    conflicting = series_row(run_id="different-run")
    repo, commit = repository(
        tmp_path,
        {
            "series/hermit/hosta/2026-08.jsonl": (
                json.dumps(published, sort_keys=True).encode() + b"\n"
            ),
        },
    )
    write_spool(
        repo,
        "ignored/ci-hub/series-spool/hermit__hosta__2026-08__conflict.jsonl",
        json.dumps(conflicting, sort_keys=True).encode() + b"\n",
    )

    with pytest.raises(export_inputs.ledger_store.ConflictingEventId):
        build_export(repo, commit)


def test_malformed_current_series_schema_refuses(tmp_path: Path):
    malformed = series_row(schema="stress-series/v3")
    repo, commit = repository(
        tmp_path,
        {
            "series/hermit/hosta/2026-08.jsonl": (
                json.dumps(malformed, sort_keys=True).encode() + b"\n"
            ),
        },
    )

    with pytest.raises(export_inputs.series_store.MalformedRow):
        build_export(repo, commit)


def test_validation_checker_override_is_refused(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    repo, commit = repository(
        tmp_path,
        {"ledger/hermit/hosta/2026-08.jsonl": b'{"not":"an event"}\n'},
    )
    monkeypatch.setenv("CI_HUB_LEDGER_EVENT_CHECKER", "/bin/true")

    with pytest.raises(export_inputs.ExportRefused, match="fixture-only override"):
        build_export(repo, commit)


def test_reader_change_during_export_refuses(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    repo, commit = repository(tmp_path)
    before = {
        "ci_hub_tree": "a" * 40,
        "commit": "a" * 40,
        "files": [],
        "files_sha256": export_inputs._value_sha256([]),
        "hermit": {
            "checkout_commit": "b" * 40,
            "gitlink_commit": "b" * 40,
        },
    }
    after = {**before, "ci_hub_tree": "c" * 40, "commit": "c" * 40}
    states = iter([before, after])
    monkeypatch.setattr(export_inputs, "_AUTHORITY_IMPORT_READER_STATE", before)
    monkeypatch.setattr(
        export_inputs,
        "_reader_state",
        lambda: next(states),
    )

    with pytest.raises(export_inputs.ExportRefused, match="reader commit or files"):
        build_export(repo, commit)


def test_owner_path_in_authority_row_refuses(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    repo, commit = repository(tmp_path)
    monkeypatch.setattr(export_inputs.validation_ledger, "events", lambda *, root: [])
    monkeypatch.setattr(
        export_inputs.validation_ledger,
        "rows",
        lambda *, root: [{"cwd": "/" + "home" + "/person/private/hermit"}],
    )
    monkeypatch.setattr(
        export_inputs.series_store,
        "published_snapshot_at",
        lambda parent, reference: export_inputs.series_store.PublishedSnapshot(
            commit=commit,
            rows=[],
        ),
    )

    with pytest.raises(export_inputs.ExportRefused, match="owner path"):
        build_export(repo, commit)


def test_main_refuses_without_partial_stdout(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
):
    monkeypatch.setattr(
        export_inputs,
        "build_export",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            export_inputs.ExportRefused("fixture refusal")
        ),
    )

    result = export_inputs.main(
        ["--parent", "/does/not/matter", "--published-ref", "HEAD"]
    )

    captured = capsys.readouterr()
    assert result == 2
    assert captured.out == ""
    assert "REFUSED: fixture refusal" in captured.err


def test_historical_export_reads_only_published_bytes_and_binds_one_row(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo, commit = source_fixture(tmp_path)
    row = historical_validation_row()
    monkeypatch.setattr(
        export_inputs, "_historical_reader_state", lambda: TEST_READER_STATE
    )
    monkeypatch.setattr(export_inputs.validation_ledger, "events", lambda *, root: [])
    monkeypatch.setattr(export_inputs.validation_ledger, "rows", lambda *, root: [row])
    monkeypatch.setattr(
        export_inputs,
        "_discover_mutable_sources",
        lambda *_args: pytest.fail("historical export discovered a mutable spool"),
    )
    monkeypatch.setattr(
        export_inputs,
        "_capture_mutable_sources",
        lambda *_args: pytest.fail("historical export captured a mutable spool"),
    )
    monkeypatch.setattr(
        export_inputs.series_store,
        "published_snapshot_at",
        lambda _parent, reference: export_inputs.series_store.PublishedSnapshot(
            commit=reference, rows=[]
        ),
    )

    def published_union(paths):
        paths = list(paths)
        assert all("ignored/" not in path.as_posix() for path in paths)
        return []

    monkeypatch.setattr(export_inputs.series_store, "union", published_union)
    value = export_inputs.build_historical_export(repo, commit, "record-1", "run-1")

    assert value["provenance"]["mode"] == "historical"
    assert value["provenance"]["published_only"] is True
    assert value["provenance"]["published_ref"] == commit
    assert value["provenance"]["parent_commit"] == commit
    assert value["provenance"]["historical"] == {
        "record_id": "record-1",
        "run_id": "run-1",
    }
    assert jsonl_rows(value["validation_ledger"]["history_rows_jsonl"]) == [row]
    assert value["provenance"]["reader_source_identity_sha256"] == "c" * 64
    assert value["provenance"]["reader_worktree_clean"] is True
    assert value["provenance"]["reader_worktree_dirty_paths"] == []
    assert {source["location"] for source in value["provenance"]["sources"]} <= {
        "published",
        "recovery",
    }


def test_historical_export_requires_full_commit(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo, commit = repository(tmp_path)
    with pytest.raises(export_inputs.ExportRefused, match="full lowercase 40-hex"):
        export_inputs.build_historical_export(repo, "HEAD", "record-1", "run-1")


def _install_hermit_reader_observation(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    *,
    actual: str,
    recorded: str,
    status: bytes = b"",
    reported_root: Path | None = None,
) -> Path:
    repository = tmp_path / "reader-parent"
    checkout = repository / "hermit"
    checkout.mkdir(parents=True)
    monkeypatch.setattr(export_inputs, "REPOSITORY", repository)

    def fake_git(root: Path, arguments: list[str]) -> bytes:
        if root == repository and arguments == ["ls-tree", "HEAD", "--", "hermit"]:
            return f"160000 commit {recorded}\thermit\n".encode()
        if root == checkout and arguments[0] == "status":
            return status
        pytest.fail(f"unexpected git call: {root} {arguments}")

    monkeypatch.setattr(export_inputs, "_git", fake_git)
    monkeypatch.setattr(
        export_inputs,
        "_git_text",
        lambda root, arguments: str(reported_root or checkout),
    )
    monkeypatch.setattr(export_inputs, "_resolve_commit", lambda *_args: actual)
    return checkout


def test_historical_reader_records_mismatch_while_live_reader_refuses(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    actual = "a" * 40
    recorded = "b" * 40
    _install_hermit_reader_observation(
        tmp_path, monkeypatch, actual=actual, recorded=recorded
    )

    assert export_inputs._historical_hermit_reader() == {
        "checkout_commit": actual,
        "gitlink_commit": recorded,
    }
    with pytest.raises(export_inputs.ExportRefused, match="expected recorded gitlink"):
        export_inputs._hermit_reader()


def test_historical_reader_refuses_missing_checkout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repository = tmp_path / "reader-parent"
    repository.mkdir()
    monkeypatch.setattr(export_inputs, "REPOSITORY", repository)
    monkeypatch.setattr(
        export_inputs,
        "_git",
        lambda *_args: f"160000 commit {'b' * 40}\thermit\n".encode(),
    )
    with pytest.raises(export_inputs.ExportRefused, match="not materialized"):
        export_inputs._historical_hermit_reader()


def test_historical_reader_refuses_dirty_checkout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _install_hermit_reader_observation(
        tmp_path,
        monkeypatch,
        actual="a" * 40,
        recorded="b" * 40,
        status=b" M tracked.txt\0",
    )
    with pytest.raises(export_inputs.ExportRefused, match="has local changes"):
        export_inputs._historical_hermit_reader()


def test_historical_reader_refuses_checkout_with_wrong_git_root(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    wrong_root = tmp_path / "other"
    _install_hermit_reader_observation(
        tmp_path,
        monkeypatch,
        actual="a" * 40,
        recorded="b" * 40,
        reported_root=wrong_root,
    )
    with pytest.raises(export_inputs.ExportRefused, match="not its own checkout"):
        export_inputs._historical_hermit_reader()


def test_opaque_jsonl_frames_without_interpreting_rust_fields() -> None:
    rows = [{"future": {"field": True}}, {"schema_version": "not-an-integer"}]
    encoded = export_inputs._opaque_jsonl(rows, "HistoryRow")
    assert jsonl_rows(encoded.decode()) == rows
    assert encoded.endswith(b"\n")


def test_exporter_source_contains_no_projection_or_historyrow_mini_contract() -> None:
    source = SCRIPT.read_text()
    forbidden = (
        "_projection_export",
        "_represented_runs",
        "_schema_counts",
        "_run_history_row_checker",
        "_run_validation_row_selector",
        "_validation_row_report",
        "projection_results(",
        "current_validation_run_rows(",
        "require_recorded_gitlink",
    )
    assert not [name for name in forbidden if name in source]


def test_reader_state_refuses_a_reported_dirty_ci_hub_path(
    monkeypatch: pytest.MonkeyPatch,
):
    dirty_path = "ci-hub/ledger/validate_rows.py"
    monkeypatch.setattr(export_inputs, "_reader_state", REAL_READER_STATE)
    monkeypatch.setattr(
        export_inputs,
        "_unexpected_ci_hub_changes",
        lambda: [dirty_path],
    )

    with pytest.raises(
        export_inputs.ExportRefused,
        match=re.escape(f"ci-hub reader files have unrecorded changes: {dirty_path}"),
    ):
        export_inputs._reader_state()


def test_main_emits_exact_valid_json_on_success(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
):
    output = {"z": 0, "schema_version": 1}
    monkeypatch.setattr(export_inputs, "build_export", lambda *_args: output)

    result = export_inputs.main(
        ["--parent", "/does/not/matter", "--published-ref", "HEAD"]
    )

    captured = capsys.readouterr()
    assert result == 0
    assert captured.err == ""
    assert captured.out == '{"schema_version":1,"z":0}\n'
    assert json.loads(captured.out) == output
