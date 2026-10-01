from __future__ import annotations

import importlib.util
import gzip
import hashlib
import json
import os
import pickle
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
    # Current series authority is a distinct repository. Keep original parent
    # bytes for historical readers while current export selects only the ledger.
    ledger = repo / "hermit_test_ledger"
    ledger.mkdir()
    git(ledger, "init", "-q")
    git(ledger, "config", "user.email", "ledger@example.invalid")
    git(ledger, "config", "user.name", "ledger fixture")
    git(ledger, "remote", "add", "origin", export_inputs.series_store.LEDGER_REPOSITORY)
    (ledger / "series").mkdir()
    (ledger / "series/.gitkeep").write_text("")
    for relative, data in (files or {}).items():
        if relative.startswith("series/"):
            path = ledger / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    git(ledger, "add", "series")
    git(ledger, "commit", "-qm", "ledger fixture inputs")
    git(ledger, "update-ref", "refs/remotes/origin/main", "HEAD")
    (repo / ".gitmodules").write_text(
        '[submodule "hermit_test_ledger"]\n path = hermit_test_ledger\n'
        f' url = {export_inputs.series_store.LEDGER_REPOSITORY}\n')
    (repo / ".gitignore").write_text("hermit_test_ledger/\nignored/\n")
    git(repo, "add", ".gitmodules", ".gitignore")
    git(repo, "commit", "-qm", "declare existing ledger repository")
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

    def history_rows(*, root: Path, committed_source=None) -> list[dict]:
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

    def published_snapshot_at(parent: Path, reference: str, **kwargs):
        published_calls.append((parent, reference))
        return export_inputs.series_store.PublishedSnapshot(
            commit=reference,
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
        "quarantine_row_count": 0,
        "quarantine_rows_jsonl": "",
        "quarantine_rows_sha256": hashlib.sha256(b"").hexdigest(),
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


def test_published_only_export_never_discovers_local_spools(tmp_path, monkeypatch):
    repo, commit = repository(tmp_path)
    write_spool(repo, "ignored/ci-hub/series-spool/incomplete.jsonl", b"not-json")
    monkeypatch.setattr(export_inputs, "_capture_mutable_sources",
                        lambda *_: pytest.fail("published-only export inspected a local spool"))
    result = export_inputs.build_export(repo, commit, published_only=True)
    assert result["provenance"]["published_only"] is True
    assert result["provenance"]["series_commit"] == git(repo / "hermit_test_ledger", "rev-parse", "HEAD")
    assert result["series"]["row_count"] == 0
    assert result["provenance"]["sources"] == []


QUARANTINE_SHARD = "quarantine/series/hermit/hosta/2026-08.jsonl"


def series_quarantine_record(run_id: str, raw: bytes = b'{"line":1}\n') -> dict[str, Any]:
    return export_inputs.series_store.quarantine_record(
        stage="batch", run_id=run_id, producer="validate", tree="a" * 40,
        emitted_at="2026-08-30T01:00:00Z", error="SeriesError", reason="fixture refusal",
        inputs=[export_inputs.series_store.quarantine_input(1, raw)],
        cell="tests/hello/verify/ptrace", host="hosta",
    )


def publish_series_quarantine(ledger: Path, records: list[dict[str, Any]]) -> str:
    path = ledger / QUARANTINE_SHARD
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("ab") as stream:
        stream.write(b"".join(json.dumps(record).encode() + b"\n" for record in records))
    git(ledger, "add", "quarantine")
    git(ledger, "commit", "-qm", "publish series quarantine")
    git(ledger, "update-ref", "refs/remotes/origin/main", "HEAD")
    return git(ledger, "rev-parse", "HEAD")


def test_export_binds_the_series_quarantine_published_at_the_series_commit(tmp_path):
    repo, commit = repository(tmp_path)
    ledger = repo / "hermit_test_ledger"
    empty = export_inputs.build_export(repo, commit, published_only=True)["series"]
    assert (empty["quarantine_row_count"], empty["quarantine_rows_jsonl"]) == (0, "")
    assert empty["quarantine_rows_sha256"] == hashlib.sha256(b"").hexdigest()

    record = series_quarantine_record("quarantined-run")
    # A committed path outside the published shard layout is not a published
    # refusal, and neither are working-tree bytes.
    held = ledger / "quarantine/series/held/local.jsonl"
    held.parent.mkdir(parents=True)
    held.write_bytes(json.dumps(series_quarantine_record("held-run")).encode() + b"\n")
    series_commit = publish_series_quarantine(ledger, [record])
    with (ledger / QUARANTINE_SHARD).open("ab") as stream:
        stream.write(json.dumps(series_quarantine_record("uncommitted-run")).encode() + b"\n")

    result = export_inputs.build_export(repo, commit, published_only=True)
    assert result["provenance"]["series_commit"] == series_commit
    expected = json.dumps(record, sort_keys=True, separators=(",", ":")).encode() + b"\n"
    assert result["series"]["quarantine_row_count"] == 1
    assert result["series"]["quarantine_rows_jsonl"].encode() == expected
    assert result["series"]["quarantine_rows_sha256"] == hashlib.sha256(expected).hexdigest()
    published = subprocess.run(
        ["git", "-C", str(ledger), "show", f"{series_commit}:{QUARANTINE_SHARD}"],
        check=True, capture_output=True,
    ).stdout
    assert result["provenance"]["sources"] == [{
        "bytes": len(published),
        "git_object": git(ledger, "rev-parse", f"{series_commit}:{QUARANTINE_SHARD}"),
        "line_count": 1,
        "location": "published",
        "path": QUARANTINE_SHARD,
        "sha256": hashlib.sha256(published).hexdigest(),
        "store": "series-quarantine",
    }]

    # A later refusal belongs to the later ledger commit only.
    git(ledger, "checkout", "-q", "--", QUARANTINE_SHARD)
    publish_series_quarantine(ledger, [series_quarantine_record("later-run", b'{"line":2}\n')])
    pinned = export_inputs.build_export(repo, commit, series_commit, published_only=True)
    assert pinned["series"] == result["series"]
    current = export_inputs.build_export(repo, commit, published_only=True)["series"]
    assert current["quarantine_row_count"] == 2
    assert "later-run" in current["quarantine_rows_jsonl"]


def test_export_refuses_series_quarantine_the_ledger_commit_does_not_hold(
    tmp_path, monkeypatch,
):
    repo, commit = repository(tmp_path)
    ledger = repo / "hermit_test_ledger"
    series_commit = publish_series_quarantine(ledger, [series_quarantine_record("quarantined-run")])
    reader = export_inputs.series_store.published_quarantine_at_repo

    monkeypatch.setattr(export_inputs.series_store, "published_quarantine_at_repo",
                        lambda repo, ref: (reader(repo, ref)[0], [series_quarantine_record("other-run")]))
    with pytest.raises(export_inputs.ExportRefused, match="disagrees with the captured ledger commit files"):
        export_inputs.build_export(repo, commit, published_only=True)

    monkeypatch.setattr(export_inputs.series_store, "published_quarantine_at_repo",
                        lambda repo, ref: ("f" * 40, reader(repo, ref)[1]))
    with pytest.raises(export_inputs.ExportRefused, match=f"resolved {'f' * 40}, expected {series_commit}"):
        export_inputs.build_export(repo, commit, published_only=True)

    monkeypatch.setattr(export_inputs.series_store, "published_quarantine_at_repo", reader)
    record = series_quarantine_record("malformed-run")
    record["reason"] = 7
    (ledger / QUARANTINE_SHARD).write_bytes(json.dumps(record).encode() + b"\n")
    git(ledger, "commit", "-qam", "publish a malformed refusal")
    git(ledger, "update-ref", "refs/remotes/origin/main", "HEAD")
    with pytest.raises(export_inputs.series_store.MalformedQuarantine, match="reason must be a string"):
        export_inputs.build_export(repo, commit, published_only=True)


@pytest.mark.parametrize("historical", [False, True])
def test_export_binds_historical_exception_through_real_non_git_snapshot(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, historical: bool,
):
    shards = export_inputs.validation_ledger.HISTORICAL_REPOSITORY_MISMATCH_SHARDS
    event = next(json.loads(line) for line in (REPOSITORY / shards[0]).read_text().splitlines()
                 if json.loads(line).get("event_id") == "legacy-000246-000")
    body = (json.dumps(event, sort_keys=True) + "\n").encode()
    repo, commit = repository(tmp_path, {**{str(path): body for path in shards},
                                        "series/.gitkeep": b""})
    monkeypatch.setattr(export_inputs, "_historical_reader_state", lambda: TEST_READER_STATE)
    seen = []
    real_export = export_inputs._validation_export

    def observe_snapshot(root, *, committed_source=None):
        assert not (root / ".git").exists()
        assert committed_source == (repo.resolve(), commit)
        seen.append(root)
        return real_export(root, committed_source=committed_source)

    monkeypatch.setattr(export_inputs, "_validation_export", observe_snapshot)
    if historical:
        result = export_inputs.build_historical_export(repo, commit, "historical-record", event["run_id"])
    else:
        result = export_inputs.build_export(repo, commit, published_only=True)
    assert len(seen) == 1
    rows = jsonl_rows(result["validation_ledger"]["history_rows_jsonl"])
    assert result["validation_ledger"]["event_count"] == 1
    assert len(rows) == 1 and rows[0]["repo"] == "reverie"
    assert rows[0]["commit"] == event["commit"]
    assert result["provenance"]["parent_commit"] == commit


def test_published_export_keeps_tool_commit_separate_from_ledger_state(tmp_path):
    state, _ = repository(tmp_path)
    tool = tmp_path / "tool"
    tool.mkdir()
    git(tool, "init", "-q")
    git(tool, "config", "user.name", "tool fixture")
    git(tool, "config", "user.email", "tool@example.invalid")
    git(tool, "commit", "--allow-empty", "-qm", "immutable tool fixture")
    tool_commit = git(tool, "rev-parse", "HEAD")
    ledger_commit = git(state / "hermit_test_ledger", "rev-parse", "HEAD")
    assert not (tool / "hermit_test_ledger").exists()
    result = export_inputs.build_export(tool, tool_commit, ledger_commit,
                                        published_only=True, state_root=state)
    assert result["provenance"]["parent_commit"] == tool_commit
    assert result["provenance"]["series_commit"] == ledger_commit
    assert result["series"]["row_count"] == 0


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

    ledger_commit = git(repo / "hermit_test_ledger", "rev-parse", "HEAD")
    assert fixtures["published_calls"] == [(repo.resolve(), ledger_commit)]
    assert output["provenance"]["parent_commit"] == commit
    assert output["provenance"]["series_commit"] == ledger_commit
    assert output["provenance"]["series_repository"] == export_inputs.series_store.LEDGER_REPOSITORY
    assert ledger_commit != commit
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
    assert set(exported) == {"row_count", "rows_jsonl", "rows_sha256", "quarantine_row_count",
                             "quarantine_rows_jsonl", "quarantine_rows_sha256"}
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
    monkeypatch.setattr(export_inputs.validation_ledger, "rows", lambda *, root, committed_source=None: rows)

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
    repo, _ = repository(tmp_path)
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
        lambda *, root, committed_source=None: [{"cwd": "/" + "home" + "/person/private/hermit"}],
    )
    monkeypatch.setattr(
        export_inputs.series_store,
        "published_snapshot_at",
        lambda parent, reference, **kwargs: export_inputs.series_store.PublishedSnapshot(
            commit=reference,
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
    monkeypatch.setattr(export_inputs.validation_ledger, "rows", lambda *, root, committed_source=None: [row])
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
        lambda _parent, reference, **kwargs: export_inputs.series_store.PublishedSnapshot(
            commit=reference, rows=[]
        ),
    )

    def published_union(paths):
        paths = list(paths)
        assert all("ignored/" not in path.as_posix() for path in paths)
        return []

    monkeypatch.setattr(export_inputs.series_store, "union", published_union)
    # Without a named ledger commit the series comes from the parent, so the
    # ledger's current refusals must not be bound to it.
    publish_series_quarantine(repo / "hermit_test_ledger", [series_quarantine_record("ledger-run")])
    value = export_inputs.build_historical_export(repo, commit, "record-1", "run-1")

    assert not {"quarantine_row_count", "quarantine_rows_jsonl", "quarantine_rows_sha256"} & set(value["series"])
    assert "series-quarantine" not in {source["store"] for source in value["provenance"]["sources"]}
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
    monkeypatch.setattr(export_inputs, "build_export", lambda *_args, **_kwargs: output)

    result = export_inputs.main(
        ["--parent", "/does/not/matter", "--published-ref", "HEAD"]
    )

    captured = capsys.readouterr()
    assert result == 0
    assert captured.err == ""
    assert captured.out == '{"schema_version":1,"z":0}\n'
    assert json.loads(captured.out) == output


# These transport cases use real immutable Git carriers and retained operand
# bytes. The namespace double tests caller ordering only; the canonical query
# and native schema-10 admission are exercised independently.
def portable_capture_fixture(tmp_path: Path):
    proof = export_inputs._proof_reader()
    fixtures = REPOSITORY / "ci-hub/validate/tests/fixtures/schema10"
    row = json.loads((fixtures / "ordinary-only-row.json").read_bytes())
    row.update(repo="hermit", record_id="fixture-record")
    state, ledger = tmp_path / "state", tmp_path / "ledger"
    state.mkdir()
    ledger.mkdir()
    git(ledger, "init", "-q")
    git(ledger, "config", "user.name", "Fixture")
    git(ledger, "config", "user.email", "fixture@example.invalid")
    git(ledger, "remote", "add", "origin", proof.LEDGER_REPOSITORY)
    for role, reference in proof.row_references(row).items():
        path = state / reference["path"]
        path.parent.mkdir(parents=True, exist_ok=True)
        suffix = "plan.json" if role == "plan" else role + ".jsonl"
        path.write_bytes((fixtures / ("ordinary-only-" + suffix)).read_bytes())
    artifacts, objects = proof.capture_artifacts(state, row)
    selected = {"row": row,
        "terminal": {"repository": "rrnewton/hermit", "run_id": row["run_id"],
            "record_id": row["record_id"], "event_id": "fixture-event", "event_sha256": "1" * 64,
            "hermit_commit": row["commit"]},
        "parent": {"repository": proof.PARENT_REPOSITORY, "commit": "2" * 40,
            "shard": "ledger/hermit/fixture/2026-09.jsonl", "blob_oid": "3" * 40,
            "event_line_sha256": "4" * 64}}
    manifest = proof.make_manifest(selected, artifacts, parent_commit="5" * 40,
        hermit_tool_commit="6" * 40,
        snapshot={"source": {"commit": "7" * 40, "tree": "8" * 40}, "rows_sha256": "9" * 64})
    proof.stage_carrier(ledger, manifest, objects)
    git(ledger, "add", "--", "scorecard")
    git(ledger, "commit", "-qm", "Retained public fixture operands")
    original_paths = {artifacts[role]["path"]: objects[proof.object_path(artifacts[role]["sha256"])]
                      for role in proof.ROLES}
    return proof, ledger, git(ledger, "rev-parse", "HEAD"), selected, manifest, original_paths


class TransportNamespace:
    def __init__(self, selection):
        self.selection = selection
        self.verified = []

    def terminals(self, *, repo, schema_version):
        assert (repo, schema_version) == ("hermit", 10)
        return [self.selection] if schema_version == self.selection["row"]["schema_version"] else []

    def rows(self):
        return [self.selection["row"], {"repo": "hermit", "schema_version": 5}]

    def rows_view(self):
        return self.rows()

    # The canonical census selector itself, run over this double's rows_view()
    # and select(), so these transport tests still exercise the real selection.
    census_terminals = export_inputs.validation_ledger.CommittedValidationNamespace.census_terminals
    # Likewise the canonical quarantine judgment, so a marked row is judged by
    # the reader's own allowlist check.
    retained_evidence_quarantined = (
        export_inputs.validation_ledger.CommittedValidationNamespace.retained_evidence_quarantined
    )

    def select(self, **selector):
        assert all(self.selection["row"].get(key) == value for key, value in selector.items())
        return self.selection

    def verify_origin(self, parent, terminal):
        assert parent == self.selection["parent"] and terminal == self.selection["terminal"]
        self.verified.append(terminal["event_id"])


def test_portable_capture_restores_exact_git_bytes_and_deduplicates(tmp_path: Path):
    proof, ledger, commit, selected, manifest, objects = portable_capture_fixture(tmp_path)
    namespace = TransportNamespace(selected)
    captured = export_inputs._capture_finalized_proof(namespace, ledger, commit)
    assert namespace.verified == ["fixture-event"]
    assert captured["manifests"] == [manifest]
    assert captured["missing_current_terminals"] == []
    assert set(captured["objects"]) == {hashlib.sha256(data).hexdigest() for data in objects.values()}
    output = tmp_path / "projection-only-state"
    output.mkdir()
    export_inputs.stage_captured_proof(output, captured)
    assert not (output / ".git").exists()
    assert {path: (output / path).read_bytes() for path in objects} == objects
    other = json.loads(json.dumps(manifest))
    other["terminal"].update(event_id="second-event", event_sha256="a" * 64, record_id="second-record")
    captured["manifests"] = sorted([manifest, other], key=proof.carrier_path)
    assert export_inputs.captured_proof_objects(captured) == objects
    assert len(captured["objects"]) == len(set(hashlib.sha256(data).hexdigest() for data in objects.values()))


def test_portable_capture_current_correction_never_stages_old_carrier(tmp_path: Path):
    _proof, ledger, commit, selected, _manifest, _objects = portable_capture_fixture(tmp_path)
    selected["terminal"].update(event_id="current-correction", event_sha256="a" * 64)
    namespace = TransportNamespace(selected)
    captured = export_inputs._capture_finalized_proof(namespace, ledger, commit)
    assert captured["manifests"] == [] and captured["objects"] == {}
    assert captured["missing_current_terminals"] == [selected["terminal"]]
    assert namespace.verified == []
    assert export_inputs.captured_proof_objects(captured) == {}


def _json_copy(value):
    return json.loads(json.dumps(value))


def _quarantined_lost_selection(selected, *, forge=False, root="/home/fixture/workspace"):
    # The whole projected row of the lost run: its recorded row, corrected,
    # with the projection's workspace rendering applied as a reader sees it.
    receipt = export_inputs.validation_ledger.qualifying_receipt
    entry = receipt.retained_evidence_quarantine_allowlist()["devbig014-1790052459-997811"]
    lost = _json_copy(selected)
    row = _json_copy(receipt.retained_evidence_quarantine_corrected_row(entry))
    for key, value in row.items():
        if isinstance(value, str) and value.startswith("{{WORKSPACE_ROOT}}"):
            row[key] = root + value[len("{{WORKSPACE_ROOT}}"):]
    if forge:
        row[receipt.RETAINED_EVIDENCE_QUARANTINE_KEY]["expected"]["cell_results"]["row_count"] += 1
    lost["row"] = row
    lost["terminal"].update(run_id=row["run_id"], record_id=entry["event_id"],
                            event_id=entry["event_id"], event_sha256="f" * 64)
    return lost


def test_portable_capture_reports_an_allowlisted_lost_run_unavailable(tmp_path: Path):
    _proof, ledger, commit, selected, _manifest, _objects = portable_capture_fixture(tmp_path)
    lost = _quarantined_lost_selection(selected)
    namespace = TransportNamespace(lost)
    captured = export_inputs._capture_finalized_proof(namespace, ledger, commit)
    assert captured["manifests"] == [] and captured["objects"] == {}
    assert captured["missing_current_terminals"] == [lost["terminal"]]
    assert namespace.verified == []


def test_portable_capture_refuses_a_quarantine_the_allowlist_does_not_name(tmp_path: Path):
    _proof, ledger, commit, selected, _manifest, _objects = portable_capture_fixture(tmp_path)
    exact = _quarantined_lost_selection(selected)
    changed_profile = _json_copy(exact)
    changed_profile["row"]["profile"] = "quick"
    split_root = _json_copy(exact)
    split_root["row"]["log_file"] = "/elsewhere" + split_root["row"]["log_file"][len("/home/fixture/workspace"):]
    for forged in (_quarantined_lost_selection(selected, forge=True),
                   {**selected, "row": {**selected["row"], "retained_evidence_quarantine":
                       exact["row"]["retained_evidence_quarantine"]}},
                   changed_profile, split_root):
        with pytest.raises(export_inputs.ExportRefused, match="no allowlisted lost run"):
            export_inputs._capture_finalized_proof(TransportNamespace(forged), ledger, commit)


def test_portable_capture_present_object_tampering_refuses(tmp_path: Path):
    proof, ledger, _commit, selected, manifest, _objects = portable_capture_fixture(tmp_path)
    path = ledger / proof.object_path(manifest["artifacts"]["cells"]["sha256"])
    path.write_bytes(path.read_bytes() + b" ")
    git(ledger, "add", "--", "scorecard")
    git(ledger, "commit", "-qm", "Tampered fixture object")
    with pytest.raises(proof.ProofRefused, match="length"):
        export_inputs._capture_finalized_proof(TransportNamespace(selected), ledger, git(ledger, "rev-parse", "HEAD"))


def test_derived_capture_stages_exact_public_object_without_raw_streams(tmp_path: Path):
    proof, ledger, _commit, selected, _manifest, _objects = portable_capture_fixture(tmp_path)
    row = selected["row"]
    row.update(schema_version=5, cell_results=None, result="fail", validation_complete=False)
    cell = {"lane": "portable", "category": "c-programs", "test": "fixture",
            "mode": "verify", "backend": "ptrace"}
    row["raw_result_input_census_v1"] = {
        "schema": 1, "run_id": row["run_id"], "hermit_sha": row["commit"],
        "files": [{"path": "portable/results.jsonl", "bytes": 12, "sha256": "a" * 64,
                   "rows": [{"line": 1, "cell": cell, "attempt": 1}]}],
    }
    value = {"schema": 1, "authority": proof.DERIVED_AUTHORITY,
             **proof.derived_row_binding(row), "attempts": [{
                 "input_path": "portable/results.jsonl", "input_line": 1, "cell": cell,
                 "attempt": 1, "source_row_sha256": "b" * 64, "outcome": "FAIL",
                 "result": None, "failure_class": None, "ordinary_verdict": {}, "backend_parity": None}]}
    encoded = proof.canonical_bytes(value) + b"\n"
    artifacts, objects = proof.capture_derived_cells(row, encoded)
    # A distinct canonical event has the new explicit role, never a rewrite of
    # the existing schema-10 carrier used above.
    selected["terminal"].update(event_id="derived-event", event_sha256="c" * 64)
    manifest = proof.make_manifest(selected, artifacts, parent_commit="5" * 40,
        hermit_tool_commit="6" * 40,
        snapshot={"source": {"commit": "7" * 40, "tree": "8" * 40}, "rows_sha256": "9" * 64})
    proof.stage_carrier(ledger, manifest, objects)
    git(ledger, "add", "--", "scorecard")
    git(ledger, "commit", "-qm", "Synthetic derived transport")
    namespace = TransportNamespace(selected)
    captured = export_inputs._capture_finalized_proof(namespace, ledger, git(ledger, "rev-parse", "HEAD"))
    assert captured["manifests"] == [manifest] and namespace.verified == ["derived-event"]
    assert export_inputs.captured_proof_objects(captured) == {proof.derived_path(value): encoded}
    assert row["schema_version"] == 5 and row["cell_results"] is None and row["result"] == "fail"
    corrected = json.loads(json.dumps(selected))
    corrected["row"].update(schema_version=6, record_id="corrected-record", corrects=row["record_id"])
    del corrected["row"]["raw_result_input_census_v1"]
    corrected["terminal"].update(record_id="corrected-record", event_id="corrected-event", event_sha256="d" * 64)
    later = TransportNamespace(corrected)
    later.rows = lambda: [row, corrected["row"], {"repo": "hermit", "schema_version": 5}]
    absent = export_inputs._capture_finalized_proof(later, ledger, git(ledger, "rev-parse", "HEAD"))
    assert absent["manifests"] == [] and absent["objects"] == {} and later.verified == []
    malformed = json.loads(json.dumps(selected))
    malformed["row"]["raw_result_input_census_v1"] = None
    malformed["terminal"].update(event_id="new-malformed-event", event_sha256="e" * 64)
    with pytest.raises(proof.ProofRefused, match="raw census"):
        export_inputs._capture_finalized_proof(TransportNamespace(malformed), ledger,
                                              git(ledger, "rev-parse", "HEAD"))
    row["raw_result_input_census_v1"]["files"][0]["rows"].append({"line": 2, "cell": cell, "attempt": 2})
    with pytest.raises(proof.ProofRefused):
        export_inputs._capture_finalized_proof(namespace, ledger, git(ledger, "rev-parse", "HEAD"))


@pytest.mark.parametrize("mutation", ["unknown", "boolean", "length", "unicode", "unused", "path"])
def test_portable_capture_closed_exact_byte_opponents(tmp_path: Path, mutation: str):
    proof, ledger, commit, selected, _manifest, _objects = portable_capture_fixture(tmp_path)
    captured = export_inputs._capture_finalized_proof(TransportNamespace(selected), ledger, commit)
    descriptor = captured["manifests"][0]["artifacts"]["plan"]
    if mutation == "unknown":
        captured["qualified"] = True
    elif mutation == "boolean":
        captured["schema"] = True
    elif mutation == "length":
        descriptor["bytes"] += 1
    elif mutation == "unicode":
        captured["objects"][descriptor["sha256"]] += "é"
    elif mutation == "unused":
        captured["objects"][hashlib.sha256(b"{}\n").hexdigest()] = "{}\n"
    else:
        descriptor["path"] = "ignored/validate/artifacts/../outside"
    with pytest.raises((export_inputs.ExportRefused, proof.ProofRefused)):
        export_inputs.captured_proof_objects(captured)


def test_portable_capture_original_destination_collision_refuses(tmp_path: Path):
    proof, ledger, commit, selected, manifest, _objects = portable_capture_fixture(tmp_path)
    captured = export_inputs._capture_finalized_proof(TransportNamespace(selected), ledger, commit)
    other = json.loads(json.dumps(manifest))
    other["terminal"].update(event_id="second-event", event_sha256="a" * 64, record_id="second-record")
    descriptor = other["artifacts"]["plan"]
    changed = captured["objects"][descriptor["sha256"]] + "\n"
    descriptor.update(sha256=hashlib.sha256(changed.encode()).hexdigest(), bytes=len(changed.encode()))
    captured["objects"][descriptor["sha256"]] = changed
    captured["manifests"] = sorted([manifest, other], key=proof.carrier_path)
    with pytest.raises(export_inputs.ExportRefused, match="conflicting captured bytes"):
        export_inputs.captured_proof_objects(captured)


def test_fresh_history_capture_uses_real_state_after_public_ledger(tmp_path, monkeypatch):
    # Reader identity is the module's existing source-guard fixture. Both data
    # roots and the complete empty canonical namespace below are real Git.
    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER", raising=False)
    state, data_commit = repository(tmp_path)
    git(state, "remote", "add", "origin", export_inputs.validation_ledger.CANONICAL_PARENT_REPOSITORY)
    git(state, "update-ref", "refs/remotes/origin/main", data_commit)
    tool = tmp_path / "tool"
    tool.mkdir()
    git(tool, "init", "-q")
    git(tool, "config", "user.name", "tool fixture")
    git(tool, "config", "user.email", "tool@example.invalid")
    git(tool, "commit", "--allow-empty", "-qm", "separate reader source")
    tool_commit = git(tool, "rev-parse", "HEAD")
    ledger = state / "hermit_test_ledger"
    ledger_commit = git(ledger, "rev-parse", "HEAD")
    order = []
    real_files = export_inputs._git_input_files
    real_namespace = export_inputs.validation_ledger.capture_committed_validation_namespace

    def capture_public(root, commit, prefixes):
        assert root == ledger and commit == ledger_commit
        order.append({("series",): "public-ledger", ("quarantine/series",): "public-quarantine"}[prefixes])
        return real_files(root, commit, prefixes)

    def capture_namespace(root, commit, *, state_root):
        assert order == ["public-ledger", "public-quarantine"]
        assert root == state and commit == data_commit
        assert state_root != state and not (state_root / ".git").exists()
        order.append("canonical-parent")
        return real_namespace(root, commit, state_root=state_root)

    monkeypatch.setattr(export_inputs, "_git_input_files", capture_public)
    monkeypatch.setattr(export_inputs.validation_ledger, "capture_committed_validation_namespace", capture_namespace)
    output = export_inputs.build_export(tool, tool_commit, ledger_commit,
        published_only=True, state_root=state, validation_history_commit=data_commit)
    assert order == ["public-ledger", "public-quarantine", "canonical-parent"]
    assert output["provenance"]["reader_commit"] == TEST_READER_STATE["commit"]
    assert output["provenance"]["parent_commit"] == output["provenance"]["published_ref"] == data_commit
    assert output["provenance"]["series_commit"] == ledger_commit
    assert output["provenance"]["finalized_proof"] == {
        "schema": 1, "manifests": [], "objects": {}, "missing_current_terminals": []}
    assert output["validation_ledger"]["history_row_count"] == 0
    assert not (tool / "hermit_test_ledger").exists()


def test_historical_export_reads_namespace_views_without_changing_the_namespace_or_output(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    # The export reads rows_view()/events_view() rather than deep copies. Its
    # output must equal the deep-copy path byte for byte, and no export step
    # may change what the namespace answers afterwards. The schema-5 census
    # row sends the proof capture through its rows_view() select path too.
    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER", raising=False)
    monkeypatch.setattr(export_inputs, "_historical_reader_state", lambda: TEST_READER_STATE)
    proof = export_inputs._proof_reader()
    event = validation_event()
    row = event["legacy_row"]
    cell = {"lane": "portable", "category": "c-programs", "test": "fixture",
            "mode": "verify", "backend": "ptrace"}
    row.update(schema_version=5, profile="full", selection_mode="full", tree_dirty=False,
               commit_anchored=True, result="fail", exit_code=1, validation_complete=False,
               cell_results=None, raw_result_input_census_v1={
                   "schema": 1, "run_id": row["run_id"], "hermit_sha": row["commit"],
                   "files": [{"path": "portable/results.jsonl", "bytes": 12, "sha256": "a" * 64,
                              "rows": [{"line": 1, "cell": cell, "attempt": 1}]}]})
    event["outcome"] = "fail"
    repo, parent_commit = repository(tmp_path, {
        "ledger/hermit/hosta/2026-08.jsonl": proof.canonical_bytes(event) + b"\n",
        "series/hermit/hosta/2026-08.jsonl": proof.canonical_bytes(series_row()) + b"\n",
    })
    git(repo, "remote", "add", "origin", export_inputs.validation_ledger.CANONICAL_PARENT_REPOSITORY)
    git(repo, "update-ref", "refs/remotes/origin/main", parent_commit)
    ledger_commit = git(repo / "hermit_test_ledger", "rev-parse", "HEAD")
    namespace_class = export_inputs.validation_ledger.CommittedValidationNamespace
    real_capture = export_inputs.validation_ledger.capture_committed_validation_namespace
    captured, calls = [], []

    def answers(namespace) -> bytes:
        return pickle.dumps((namespace.rows(), namespace.events(), namespace.source_files(),
                             namespace.terminals(repo="hermit")), 5)

    def capture(root, commit, *, state_root=None):
        namespace = real_capture(root, commit, state_root=state_root)
        captured.append((namespace, answers(namespace)))
        return namespace

    def spy(name, method):
        def observed(namespace):
            calls.append(name)
            return method(namespace)
        return observed

    def export():
        return export_inputs.build_historical_export(
            repo, parent_commit, row["record_id"], row["run_id"], series_commit=ledger_commit)

    monkeypatch.setattr(export_inputs.validation_ledger, "capture_committed_validation_namespace", capture)
    monkeypatch.setattr(namespace_class, "rows_view", spy("rows_view", namespace_class.rows_view))
    monkeypatch.setattr(namespace_class, "events_view", spy("events_view", namespace_class.events_view))
    monkeypatch.setattr(namespace_class, "rows", spy("rows", namespace_class.rows))
    monkeypatch.setattr(namespace_class, "events", spy("events", namespace_class.events))
    viewed = export()
    assert calls == ["rows", "events", "events_view", "rows_view", "rows_view"]
    calls.clear()
    # The deep-copy path this change replaced.
    monkeypatch.setattr(namespace_class, "rows_view", namespace_class.rows)
    monkeypatch.setattr(namespace_class, "events_view", namespace_class.events)
    copied = export()
    assert export_inputs.render_export(viewed) == export_inputs.render_export(copied)
    assert viewed["validation_ledger"]["history_row_count"] == 1
    assert len(viewed["provenance"]["finalized_proof"]["missing_current_terminals"]) == 1
    assert len(captured) == 2
    for namespace, before in captured:
        assert answers(namespace) == before


def test_historical_export_captures_schema5_proof_from_the_selected_ledger_commit(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Real immutable repositories, canonical terminal selection, carrier, and
    # object bytes. These synthetic operands establish transport, not execution.
    monkeypatch.delenv("CI_HUB_LEDGER_EVENT_CHECKER", raising=False)
    monkeypatch.setattr(export_inputs, "_historical_reader_state", lambda: TEST_READER_STATE)
    proof = export_inputs._proof_reader()
    event = validation_event()
    row = event["legacy_row"]
    cell = {"lane": "portable", "category": "c-programs", "test": "fixture",
            "mode": "verify", "backend": "ptrace"}
    row.update(schema_version=5, profile="full", selection_mode="full", tree_dirty=False,
               commit_anchored=True, result="fail", exit_code=1, validation_complete=False,
               cell_results=None, raw_result_input_census_v1={
                   "schema": 1, "run_id": row["run_id"], "hermit_sha": row["commit"],
                   "files": [{"path": "portable/results.jsonl", "bytes": 12, "sha256": "a" * 64,
                              "rows": [{"line": 1, "cell": cell, "attempt": 1}]}]})
    event["outcome"] = "fail"
    series_path = "series/hermit/hosta/2026-08.jsonl"
    repo, parent_commit = repository(tmp_path, {
        "ledger/hermit/hosta/2026-08.jsonl": proof.canonical_bytes(event) + b"\n",
        series_path: proof.canonical_bytes(series_row()) + b"\n",
    })
    git(repo, "remote", "add", "origin", export_inputs.validation_ledger.CANONICAL_PARENT_REPOSITORY)
    git(repo, "update-ref", "refs/remotes/origin/main", parent_commit)
    ledger = repo / "hermit_test_ledger"
    with export_inputs.validation_ledger.capture_committed_validation_namespace(repo, parent_commit) as namespace:
        canonical_row = namespace.rows()[0]
        selected = namespace.select(**{key: canonical_row[key] for key in
            ("repo", "record_id", "run_id", "commit", "cwd", "started_at")})
    value = {"schema": 1, "authority": proof.DERIVED_AUTHORITY,
             **proof.derived_row_binding(selected["row"]), "attempts": [{
                 "input_path": "portable/results.jsonl", "input_line": 1, "cell": cell,
                 "attempt": 1, "source_row_sha256": "b" * 64, "outcome": "FAIL",
                 "result": None, "failure_class": None, "ordinary_verdict": {}, "backend_parity": None}]}
    encoded = proof.canonical_bytes(value) + b"\n"
    artifacts, objects = proof.capture_derived_cells(selected["row"], encoded)
    manifest = proof.make_manifest(selected, artifacts, parent_commit=parent_commit,
        hermit_tool_commit="6" * 40,
        snapshot={"source": {"commit": "7" * 40, "tree": "8" * 40}, "rows_sha256": "9" * 64})
    proof.stage_carrier(ledger, manifest, objects)
    refusal = series_quarantine_record(row["run_id"])
    (ledger / QUARANTINE_SHARD).parent.mkdir(parents=True)
    (ledger / QUARANTINE_SHARD).write_bytes(json.dumps(refusal).encode() + b"\n")
    git(ledger, "add", "scorecard", "quarantine")
    git(ledger, "commit", "-qm", "Synthetic historical proof")
    ledger_commit = git(ledger, "rev-parse", "HEAD")
    # Later HEAD and working-tree data cannot supply a missing historical proof.
    git(ledger, "rm", "-qr", "scorecard")
    (ledger / series_path).write_bytes(proof.canonical_bytes(series_row(event_id="later-event")) + b"\n")
    with (ledger / QUARANTINE_SHARD).open("ab") as stream:
        stream.write(json.dumps(series_quarantine_record("later-refusal-run")).encode() + b"\n")
    git(ledger, "add", "series", "quarantine")
    git(ledger, "commit", "-qm", "Later independent ledger state")
    later = git(ledger, "rev-parse", "HEAD")
    captured = export_inputs.build_historical_export(repo, parent_commit, row["record_id"], row["run_id"],
                                                    series_commit=ledger_commit)
    provenance = captured["provenance"]
    assert provenance["parent_commit"] == parent_commit
    assert provenance["series_commit"] == ledger_commit != later
    assert provenance["series_tree"] == git(ledger, "rev-parse", f"{ledger_commit}:series")
    assert provenance["finalized_proof"]["manifests"] == [manifest]
    assert export_inputs.captured_proof_objects(provenance["finalized_proof"]) == {proof.derived_path(value): encoded}
    selected_row = json.loads(captured["validation_ledger"]["history_rows_jsonl"])
    assert selected_row["schema_version"] == 5 and selected_row["result"] == "fail"
    assert selected_row["validation_complete"] is False and selected_row["cell_results"] is None
    assert "later-event" not in json.dumps(captured["series"])
    # The refusals come from the same named ledger commit as the series rows.
    assert captured["series"]["quarantine_row_count"] == 1
    assert json.loads(captured["series"]["quarantine_rows_jsonl"]) == refusal
    assert ("series-quarantine", QUARANTINE_SHARD) in {
        (source["store"], source["path"]) for source in provenance["sources"]}
    absent_export = export_inputs.build_historical_export(repo, parent_commit, row["record_id"], row["run_id"],
                                                         series_commit=later)
    assert absent_export["series"]["quarantine_row_count"] == 2
    absent = absent_export["provenance"]["finalized_proof"]
    assert absent["objects"] == {} and absent["missing_current_terminals"] == [selected["terminal"]]
    with pytest.raises(export_inputs.ExportRefused, match="full immutable"):
        export_inputs.build_historical_export(repo, parent_commit, row["record_id"], row["run_id"], series_commit="HEAD")


# --- parity capture (https://github.com/rrnewton/hermit/issues/3301) -----------------

PARITY_FIXTURE = REPOSITORY / "ci-hub/tests/fixtures/parity-run-1953"
#: The Hermit commit every row of RUN 1953 names.
PARITY_HERMIT_SHA = "d3a0a4ae3d168565595ae4157b25ab8efbb1861a"


def _parity_committed_of(sha: str) -> list[tuple[str, str]] | None:
    """The committed selection at a commit, as hermit's scorecard reads it:
    RUN 1953's at its own commit (`committed-cells.json`, provenance beside
    it), and unreadable at any other."""
    if sha != PARITY_HERMIT_SHA:
        return None
    names = json.loads((PARITY_FIXTURE / "committed-cells.json").read_text(encoding="utf-8"))
    return [tuple(name.rsplit("@", 1)) for name in names]


def _parity_run_1953_shards(sources_of: Any = None) -> dict[str, bytes]:
    """RUN 1953's 192 sources, wrapped as `append-parity` wraps them and laid
    out in the ledger's `parity/` and `parity-runs/` shards. `sources_of`
    rewrites the source rows before the batch is built."""
    ledger_store = export_inputs._parity_reader().parity_ledger_module()
    text = (PARITY_FIXTURE / "ledger-sources-typed.jsonl").read_text(encoding="utf-8")
    sources = [ledger_store.loads(line, f"line {n}") for n, line in enumerate(text.splitlines(), 1)]
    if sources_of is not None:
        sources = sources_of(sources)
    batch = ledger_store.build_batch(
        sources, producer="validate",
        run_id="validate-coord-s17-d3a0a4ae3d16-1790649970094317174-3328074-da07a1da",
        hermit_sha=PARITY_HERMIT_SHA, emitted_at="2026-09-28T21:40:00Z",
        source_tree_dirty=False, host="fixturehost",
    )
    shards: dict[str, bytes] = {}
    for event in batch:
        path = ledger_store.event_shard(event)
        shards[path] = shards.get(path, b"") + ledger_store.encode_batch([event]).encode()
    return shards


def _store(shards: dict[str, bytes]) -> dict[str, bytes]:
    reader = export_inputs._parity_reader()
    return {path: data for path, data in shards.items() if reader.is_store_shard(path)}


def _parity_run_1953_summary_bytes(shards: dict[str, bytes] | None = None, series_commit: str | None = None) -> bytes:
    """RUN 1953 summarized as hermit's scorecard would summarize `shards`."""
    reader = export_inputs._parity_reader()
    shards = _parity_run_1953_shards() if shards is None else shards
    ledger_store = reader.parity_ledger_module()
    rows = [event for path, data in _store(shards).items()
            for event in ledger_store.parse_lines(data.decode(), path) if event["event_type"] == "parity.record"]
    return reader.encode_summary(reader.summarize_rows(
        rows, store_sha256=reader.store_digest(_store(shards)), series_commit=series_commit,
        committed_of=_parity_committed_of))


def _commit_ledger_parity(repo: Path, files: dict[str, bytes]) -> str:
    ledger = repo / "hermit_test_ledger"
    for relative, data in files.items():
        path = ledger / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    git(ledger, "add", *sorted(files))
    git(ledger, "commit", "-qm", "parity fixture")
    git(ledger, "update-ref", "refs/remotes/origin/main", "HEAD")
    return git(ledger, "rev-parse", "HEAD")


#: RUN 1953 reported all 192 cells its commit selects, and no other.
RUN_1953_LINE = (
    "parity: 0/192 matched; selected 192 of 192 committed; mean 0.052 over 175 measured; "
    "floor 0.051 over 178 of 192 selected "
    "(counted as 0: 3 unmeasured: no-result-row 3; excluded: 0 no golden; 14 not compared) [inputs not equalized]"
)


def test_export_without_parity_carries_an_absent_capture_not_zero(tmp_path: Path):
    repo, commit = repository(tmp_path)
    result = export_inputs.build_export(repo, commit, published_only=True)
    carriage = result["parity"]
    assert carriage["ledger_commit"] == result["provenance"]["series_commit"]
    assert (carriage["state"], carriage["present"], carriage["store_files"], carriage["store_sha256"],
            carriage["run_marker_files"], carriage["append_refusals_total"]) == ("absent", False, 0, None, 0, 0)
    assert export_inputs._parity_reader().validate_carriage(carriage) is None


def test_export_carries_the_summary_of_the_store_at_the_series_commit(tmp_path: Path):
    repo, commit = repository(tmp_path)
    shards = _parity_run_1953_shards()
    data = _parity_run_1953_summary_bytes(shards)
    ledger_commit = _commit_ledger_parity(repo, {**shards, "scorecard/parity.json": data})
    result = export_inputs.build_export(repo, commit, published_only=True)
    assert result["provenance"]["series_commit"] == ledger_commit
    carriage = result["parity"]
    reader = export_inputs._parity_reader()
    assert (carriage["state"], carriage["present"], carriage["ledger_commit"]) == ("current", True, ledger_commit)
    assert carriage["summary_json"].encode() == data and carriage["bytes"] == len(data)
    assert carriage["sha256"] == hashlib.sha256(data).hexdigest()
    assert carriage["git_object"] == git(repo / "hermit_test_ledger", "rev-parse", f"{ledger_commit}:scorecard/parity.json")
    # The digest is recomputed from the committed shard, not copied from the summary.
    assert carriage["store_sha256"] == reader.store_digest(_store(shards)) == json.loads(data)["generated_from"]["store_sha256"]
    assert (carriage["store_files"], carriage["store_records"], carriage["run_marker_files"]) == (1, 192, 1)
    summary = reader.validate_carriage(carriage)
    assert summary["producers"][0]["line"] == RUN_1953_LINE


def test_a_parity_only_publication_after_the_summary_makes_it_stale(tmp_path: Path):
    repo, _commit = repository(tmp_path)
    ledger = repo / "hermit_test_ledger"
    shards = _parity_run_1953_shards()
    series_commit = _commit_ledger_parity(repo, shards)
    # The mirror commits the scorecard one commit after the series commit it names.
    summarized = _commit_ledger_parity(
        repo, {"scorecard/parity.json": _parity_run_1953_summary_bytes(shards, series_commit)})
    current = export_inputs._parity_capture(ledger, summarized)
    assert (current["state"], current["summarized_commit"]) == ("current", series_commit)
    (path,) = _store(shards)
    first, second = shards[path].splitlines(keepends=True)[:2]
    extended = {path: shards[path] + first + second, "parity/hermit/laterhost/2026-10.jsonl": first}
    later = _commit_ledger_parity(repo, extended)
    stale = export_inputs._parity_capture(ledger, later)
    assert (stale["state"], stale["summarized_commit"], stale["unsummarized_records"], stale["store_records"]) == (
        "stale", series_commit, 3, 195)
    assert export_inputs._parity_reader().validate_carriage(stale)["producers"][0]["line"] == RUN_1953_LINE


def test_a_summary_that_is_not_of_its_store_is_refused_and_the_export_goes_on(tmp_path: Path):
    repo, commit = repository(tmp_path)
    shards = _parity_run_1953_shards()
    (path,) = _store(shards)
    data = _parity_run_1953_summary_bytes(shards)
    # The store at the series commit has one more record than the summary covers,
    # and the summary names no series commit that would locate what it covered.
    moved = {**shards, path: shards[path] + shards[path].splitlines(keepends=True)[0]}
    ledger_commit = _commit_ledger_parity(repo, {**moved, "scorecard/parity.json": data})
    result = export_inputs.build_export(repo, commit, published_only=True)
    reader = export_inputs._parity_reader()
    carriage = result["parity"]
    assert result["provenance"]["series_commit"] == ledger_commit and result["series"]
    assert (carriage["state"], carriage["summary_json"]) == ("refused", None)
    assert carriage["refusal"] == (
        f"ledger commit {ledger_commit} scorecard/parity.json is refused: its store_sha256 "
        f"{reader.store_digest(_store(shards))} is not {reader.store_digest(_store(moved))}, the digest of the "
        f"parity/ store at ledger commit {ledger_commit}, and it names no series_commit to locate what it summarized"
    )
    assert reader.validate_carriage(carriage) is None


def test_a_parity_summary_naming_a_commit_off_the_ledger_history_is_refused(tmp_path: Path):
    # https://github.com/rrnewton/hermit/issues/3301 findings [7] and [8]: the
    # summary's series_commit is checked against the ledger's history by git.
    # A side-branch commit holding the same store digests like the summary,
    # but it is not what the exported commit's history summarized.
    repo, _commit = repository(tmp_path)
    ledger = repo / "hermit_test_ledger"
    reader = export_inputs._parity_reader()
    shards = _parity_run_1953_shards()
    (path,) = _store(shards)
    branch = git(ledger, "symbolic-ref", "--short", "HEAD")
    git(ledger, "checkout", "-q", "--detach", "HEAD")
    side = _commit_ledger_parity(repo, shards)
    git(ledger, "checkout", "-q", branch)
    data = _parity_run_1953_summary_bytes(shards, side)
    matching = _commit_ledger_parity(repo, {**shards, "scorecard/parity.json": data})
    carriage = export_inputs._parity_capture(ledger, matching)
    assert (carriage["state"], carriage["summary_json"], carriage["refusal"]) == (
        "refused", None,
        f"ledger commit {matching} scorecard/parity.json is refused: its store_sha256 matches the parity/ store "
        f"at ledger commit {matching}, but its series_commit {side} is not that commit or an ancestor of it",
    )
    assert reader.validate_carriage(carriage) is None
    extended = {path: shards[path] + shards[path].splitlines(keepends=True)[0]}
    later = _commit_ledger_parity(repo, extended)
    carriage = export_inputs._parity_capture(ledger, later)
    assert (carriage["state"], carriage["summary_json"], carriage["refusal"]) == (
        "refused", None,
        f"ledger commit {later} scorecard/parity.json is refused: its store_sha256 "
        f"{reader.store_digest(_store(shards))} is not {reader.store_digest(extended)}, the digest of the "
        f"parity/ store at ledger commit {later}, and its series_commit {side} is not an ancestor of it",
    )
    assert reader.validate_carriage(carriage) is None


def test_parity_records_without_a_summary_are_carried_as_unsummarized(tmp_path: Path):
    repo, _commit = repository(tmp_path)
    shards = _parity_run_1953_shards()
    ledger_commit = _commit_ledger_parity(repo, shards)
    carriage = export_inputs._parity_capture(repo / "hermit_test_ledger", ledger_commit)
    reader = export_inputs._parity_reader()
    assert (carriage["state"], carriage["present"], carriage["store_files"], carriage["run_marker_files"],
            carriage["store_sha256"]) == ("absent", False, 1, 1, reader.store_digest(_store(shards)))
    assert reader.validate_carriage(carriage) is None


def test_a_malformed_committed_parity_summary_is_refused_not_the_export(tmp_path: Path):
    repo, commit = repository(tmp_path)
    shards = _parity_run_1953_shards()
    value = json.loads(_parity_run_1953_summary_bytes(shards))
    value["producers"][0]["total"]["matched"] = 1
    data = (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode()
    ledger_commit = _commit_ledger_parity(repo, {**shards, "scorecard/parity.json": data})
    result = export_inputs.build_export(repo, commit, published_only=True)
    carriage = result["parity"]
    assert carriage["state"] == "refused" and carriage["sha256"] == hashlib.sha256(data).hexdigest()
    assert re.fullmatch(
        rf"ledger commit {ledger_commit} scorecard/parity\.json is refused: .*\.total: "
        r"measured 175 is not matched \+ diverged.*", carriage["refusal"])


def test_a_symlinked_parity_summary_is_refused(tmp_path: Path):
    repo, _commit = repository(tmp_path)
    ledger = repo / "hermit_test_ledger"
    (ledger / "scorecard").mkdir()
    (ledger / "scorecard/parity.json").symlink_to("../series/.gitkeep")
    git(ledger, "add", "scorecard/parity.json")
    git(ledger, "commit", "-qm", "symlinked parity summary")
    ledger_commit = git(ledger, "rev-parse", "HEAD")
    carriage = export_inputs._parity_capture(ledger, ledger_commit)
    assert (carriage["state"], carriage["refusal"]) == (
        "refused", f"ledger commit {ledger_commit} parity ledger path(s) are not regular blobs: scorecard/parity.json")


def test_append_refusals_at_the_series_commit_are_carried(tmp_path: Path):
    repo, _commit = repository(tmp_path)
    reader = export_inputs._parity_reader()
    ledger_store = reader.parity_ledger_module()
    refusal = ledger_store.build_refusal(
        producer="validate", run_id="validate-refused", hermit_sha="d" * 40, emitted_at="2026-09-29T01:00:00Z",
        source_tree_dirty=None, host="fixturehost", input_bytes=b"x", input_lines=1,
        error="--source-tree-dirty is required",
    )
    ledger_commit = _commit_ledger_parity(
        repo, {ledger_store.event_shard(refusal): ledger_store.encode_batch([refusal]).encode()})
    carriage = export_inputs._parity_capture(repo / "hermit_test_ledger", ledger_commit)
    assert (carriage["state"], carriage["run_marker_files"], carriage["append_refusals_total"]) == ("absent", 1, 1)
    assert carriage["append_refusals"] == [{field: refusal[field] for field in reader.APPEND_REFUSAL_FIELDS}]


def test_a_git_failure_reading_parity_is_refused_in_the_panel_without_a_checkout_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A git failure names the checkout it ran in, and the ledger checkout sits
    # inside the parent checkout, whose absolute path refuses the whole export.
    repo, commit = repository(tmp_path)
    reader = export_inputs._parity_reader()
    capture = export_inputs.capture_bounded_process

    def parity_listing_times_out(command: list[str], **options: Any) -> Any:
        if "ls-tree" in command and reader.LEDGER_PATH in command:
            raise subprocess.TimeoutExpired(command, options["timeout"])
        return capture(command, **options)

    monkeypatch.setattr(export_inputs, "capture_bounded_process", parity_listing_times_out)
    result = export_inputs.build_export(repo, commit, published_only=True)
    series_commit = result["provenance"]["series_commit"]
    carriage = result["parity"]
    assert carriage["state"] == "refused" and result["series"]
    assert carriage["refusal"].startswith(
        f"ledger commit {series_commit} parity paths cannot be listed: cannot run git in the ledger checkout: ")
    assert str(tmp_path) not in carriage["refusal"]
    assert reader.validate_carriage(carriage) is None


def test_a_parity_carriage_still_holding_an_owner_path_is_refused_in_the_panel(tmp_path: Path) -> None:
    reader = export_inputs._parity_reader()
    ledger = tmp_path / "ledger"
    ledger_commit = "e" * 40
    owner = "/" + "home" + "/someone/objects"

    def without_paths(refusal: str) -> dict[str, Any]:
        carriage = reader.refused_carriage(ledger_commit, refusal, append_refusals_total=None)
        return export_inputs._parity_without_paths(carriage, ledger_commit, {"ledger": ledger})

    named = without_paths(f"git failed in {ledger}: cannot open {ledger}/objects")
    assert named["refusal"] == "git failed in the ledger checkout: cannot open the ledger checkout/objects"
    held = without_paths(f"git failed in {ledger}: cannot open {owner}")
    assert held["refusal"] == f"ledger commit {ledger_commit} parity data is refused: it holds an absolute path"
    assert reader.validate_carriage(held) is None


def test_a_refused_parity_carriage_holding_any_absolute_path_is_refused_in_the_panel(tmp_path: Path) -> None:
    # A refusal quotes git's output only up to _MAX_PROCESS_DETAIL_BYTES, so a
    # checkout path can arrive cut short, and then no checkout's role matches it.
    reader = export_inputs._parity_reader()
    ledger = tmp_path / "ledger"
    ledger_commit = "e" * 40
    refused = f"ledger commit {ledger_commit} parity data is refused: it holds an absolute path"

    def without_paths(refusal: str, **facts: Any) -> dict[str, Any]:
        carriage = reader.refused_carriage(ledger_commit, refusal, **{"append_refusals_total": None, **facts})
        return export_inputs._parity_without_paths(carriage, ledger_commit, {"ledger": ledger})

    cut = without_paths(f"git failed in {ledger}: cannot read /srv/ci/checkout/hermit_te... [truncated]")
    assert cut["refusal"] == refused
    assert reader.validate_carriage(cut) is None
    assert without_paths(f"git failed in {ledger}: cannot read /sr... [truncated]")["refusal"] == refused
    relative = ("cannot read scorecard/parity.json, hermit_test_ledger/parity/runs, ../.git/modules/hermit_test_ledger"
                " or ~/.config/git/config in the parity/ store; see"
                " https://github.com/rrnewton/hermit_test_ledger/tree/main/parity")
    assert without_paths(f"git failed in {ledger}: {relative}")["refusal"] == (
        f"git failed in the ledger checkout: {relative}")
    listed = [{"producer": "validate", "run_id": "validate-refused", "hermit_sha": "d" * 40,
               "emitted_at": "2026-09-29T01:00:00Z", "error": "cannot open /srv/ci/batch.jsonl"}]
    assert without_paths("the parity/ store cannot be read", append_refusals=listed,
                         append_refusals_total=1)["refusal"] == refused
    # Only a refused carriage: RUN 1953's summary quotes the guest's own "/etc/..."
    # paths, and test_export_carries_the_summary_of_the_store_at_the_series_commit
    # shows that summary carried as current.
    assert export_inputs._contains_absolute_path(_parity_run_1953_summary_bytes().decode())


def _renderer_test() -> Any:
    """The renderer's tests, for their representative site data."""
    name = "compatibility_website_export_inputs_renderer_fixture"
    if name not in sys.modules:
        spec = importlib.util.spec_from_file_location(
            name, REPOSITORY / "ci-hub/tests/test_compatibility_website_render_page.py")
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
    return sys.modules[name]


def _published_site(result: dict[str, Any]) -> dict[str, bytes]:
    """Every file the site publishes with `result`'s parity carriage, the
    compressed data files decompressed."""
    renderer = _renderer_test()
    data = renderer.representative_build_data()
    data["provenance"]["canonical_input_provenance"].update(
        series_repository=result["provenance"]["series_repository"],
        series_commit=result["provenance"]["series_commit"],
        series_tree=result["provenance"]["series_tree"],
    )
    data["runs"]["parity"] = result["parity"]
    tree = renderer._tree(data)
    pages = renderer.render_page.render_site(json.loads(tree["build.json"]), data)
    return {name: gzip.decompress(value) if name.endswith(".gz") else value
            for name, value in {**tree, **pages}.items()}


@pytest.mark.parametrize("case", [
    "git failure naming the ledger checkout",
    "git failure naming an owner path",
    "git failure cut short inside the parent checkout path",
    "current summary naming the ledger checkout",
])
def test_a_parity_refusal_puts_no_checkout_or_owner_path_in_the_export_or_the_site(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, case: str,
) -> None:
    # The site is public. Once a parity carriage that names a checkout or an
    # owner path becomes a panel refusal, neither the export nor any file the
    # site publishes may hold such a path.
    repo, commit = repository(tmp_path)
    ledger = repo / "hermit_test_ledger"
    reader = export_inputs._parity_reader()
    if case == "current summary naming the ledger checkout":
        def naming_the_ledger(sources: list[dict[str, Any]]) -> list[dict[str, Any]]:
            for row in sources:
                if row["verdict"] == "inputs-not-equalized":
                    row["reason"] = row["record"]["reason"] = row["reason"] + f" (read from {ledger}/parity)"
            return sources

        shards = _parity_run_1953_shards(naming_the_ledger)
        series_commit = _commit_ledger_parity(
            repo, {**shards, "scorecard/parity.json": _parity_run_1953_summary_bytes(shards)})
        captured = export_inputs._parity_capture(ledger, series_commit)
        assert captured["state"] == "current" and export_inputs._contains_string(captured, str(ledger))
    else:
        detail = f"fatal: cannot read {ledger}/objects"
        if case == "git failure naming an owner path":
            detail += " or " + export_inputs._OWNER_PATH_NEEDLE + "someone/.gitconfig"
        if case == "git failure cut short inside the parent checkout path":
            # The refusal quotes git's output only up to its byte limit: put the
            # cut three bytes before the end of the parent checkout's path.
            kept = len(f"fatal: cannot read {repo}") - 3
            detail = "x" * (export_inputs._MAX_PROCESS_DETAIL_BYTES - kept) + detail
        capture = export_inputs.capture_bounded_process

        def parity_listing_fails(command: list[str], **options: Any) -> Any:
            if "ls-tree" in command and reader.LEDGER_PATH in command:
                return subprocess.CompletedProcess(command, 128, b"", detail.encode())
            return capture(command, **options)

        monkeypatch.setattr(export_inputs, "capture_bounded_process", parity_listing_fails)
    result = export_inputs.build_export(repo, commit, published_only=True)
    files = {"export.json": export_inputs.render_export(result), **_published_site(result)}
    assert _path_hits(tmp_path, files) == []
    series_commit = result["provenance"]["series_commit"]
    refusal = result["parity"]["refusal"]
    assert result["parity"]["state"] == "refused"
    if case == "git failure naming the ledger checkout":
        assert refusal.startswith(f"ledger commit {series_commit} parity paths cannot be listed: git ls-tree ")
        assert refusal.endswith(" failed in the ledger checkout: fatal: cannot read the ledger checkout/objects")
    else:
        assert refusal == f"ledger commit {series_commit} parity data is refused: it holds an absolute path"
    assert json.loads(files["data/runs.json.gz"])["parity"] == result["parity"]
    assert refusal.encode() in files["index.html"]


def _path_hits(tmp_path: Path, files: dict[str, bytes]) -> list[tuple[str, str]]:
    """Each (file, path) where a published file holds the temporary checkouts'
    directory or its parent, this builder's own checkout, or the owner path."""
    needles = sorted({str(path) for path in (tmp_path, tmp_path.resolve(), tmp_path.parent,
                                             tmp_path.resolve().parent, REPOSITORY)}
                     | {export_inputs._OWNER_PATH_NEEDLE})
    return [(name, needle) for name, value in files.items() for needle in needles if needle.encode() in value]


def test_a_parity_refusal_names_the_ledger_git_directory_in_the_shared_checkout_by_its_role(
    tmp_path: Path,
) -> None:
    # Built from a linked worktree, the ledger's git directory sits in the
    # shared checkout's .git/modules, and git names it when an object there is
    # corrupt. Neither the parent checkout (the linked worktree) nor the ledger
    # checkout holds that path.
    primary, commit = repository(tmp_path)
    ledger = primary / "hermit_test_ledger"
    shards = _parity_run_1953_shards()
    series_commit = _commit_ledger_parity(
        primary, {**shards, "scorecard/parity.json": _parity_run_1953_summary_bytes(shards)})
    gitdir = primary / ".git/modules/hermit_test_ledger"
    gitdir.parent.mkdir(parents=True)
    (ledger / ".git").rename(gitdir)
    (ledger / ".git").write_text("gitdir: ../.git/modules/hermit_test_ledger\n")
    git(ledger, "config", "core.worktree", "../../../hermit_test_ledger")
    (primary / "ci-hub/series").mkdir(parents=True)
    (primary / "ci-hub/series/series.py").write_text("# shared-parent marker\n")
    worktree = tmp_path / "linked-worktree"
    git(primary, "worktree", "add", "-q", "--detach", str(worktree), commit)
    assert export_inputs.series_store.shared_parent_root(worktree) == primary.resolve()
    assert export_inputs.series_store.ledger_root(worktree) == ledger.resolve()
    tree = git(ledger, "rev-parse", f"{series_commit}:scorecard")
    loose = gitdir / "objects" / tree[:2] / tree[2:]
    loose.chmod(0o644)
    loose.write_bytes(b"this is not a zlib stream")
    result = export_inputs.build_export(worktree, commit, published_only=True)
    files = {"export.json": export_inputs.render_export(result), **_published_site(result)}
    assert _path_hits(tmp_path, files) == []
    refusal = result["parity"]["refusal"]
    assert result["parity"]["state"] == "refused"
    assert refusal.startswith(f"ledger commit {series_commit} parity paths cannot be listed: git ls-tree ")
    assert f"(stored in the shared checkout/.git/modules/hermit_test_ledger/objects/{tree[:2]}/{tree[2:]})" in refusal
    assert refusal.encode() in files["index.html"]
