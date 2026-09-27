import copy
import gc
import hashlib
import json
import os
import pickle
import re
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent.parent / "validate"))

import validate_rows  # noqa: E402
import ledger  # noqa: E402
import reverie_node  # noqa: E402

qualifying_receipt = validate_rows.qualifying_receipt


def git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )


def repository(path: Path, *, content: str = "one\n") -> str:
    path.mkdir(parents=True, exist_ok=True)
    git(path, "init", "-q")
    git(path, "config", "user.email", "ledger-test@example.invalid")
    git(path, "config", "user.name", "ledger test")
    (path / "tracked.txt").write_text(content)
    git(path, "add", "tracked.txt")
    git(path, "commit", "-qm", "fixture")
    return git(path, "rev-parse", "HEAD").stdout.strip()


def row(root: Path, *, record_id: str = "host-1-7", host: str = "hosta") -> dict:
    checkout = root / "worktrees" / "slot" / "hermit"
    commit = repository(checkout) if not (checkout / ".git").exists() else git(checkout, "rev-parse", "HEAD").stdout.strip()
    return {
        "schema_version": 5,
        "record_id": record_id,
        "repo": "hermit",
        "producer": "hermit-validate-rs",
        "host": host,
        "started_at": "2026-08-09T01:00:00Z",
        "finished_at": "2026-08-09T01:01:00Z",
        "result": "pass",
        "commit": commit,
        "cwd": str(checkout),
    }


def write_events(root: Path, *events: dict) -> Path:
    first = events[0]
    path = (
        root
        / "ledger"
        / str(first["team"])
        / str(first["host"])
        / "2026-08.jsonl"
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "".join(json.dumps(event, sort_keys=True) + "\n" for event in events),
        encoding="utf-8",
    )
    return path


def production_historical_repository_mismatch(event_id: str) -> dict:
    root = HERE.parents[2]
    primary = root / validate_rows.HISTORICAL_REPOSITORY_MISMATCH_SHARDS[0]
    return next(
        event
        for event in ledger.read_shard(primary)
        if event.get("event_id") == event_id
    )


def write_historical_repository_mismatch_copies(root: Path, event: dict) -> None:
    if not (root / ".git").exists():
        repository(root)
    text = json.dumps(event, sort_keys=True) + "\n"
    paths = []
    for relative in validate_rows.HISTORICAL_REPOSITORY_MISMATCH_SHARDS:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        paths.append(relative.as_posix())
    git(root, "add", *paths)
    git(root, "commit", "-qm", "track historical source copies")


def event_with_null_failed_substeps(root: Path) -> dict:
    source = row(root, record_id="historical-null")
    source.update(
        result="fail",
        exit_code=1,
        failures=1,
        gates=[
            {
                "name": "check.example",
                "result": "fail",
                "failure_origin": "outer_gate",
                "failed_substeps": None,
            }
        ],
    )
    return validate_rows._event_for_row(source, root)


def correction_for(original: dict) -> dict:
    corrected = copy.deepcopy(original["legacy_row"])
    corrected["gates"][0]["failed_substeps"] = []
    return {
        "schema": "validate-ledger/v1",
        "event_id": "historical-null-correction",
        "event_type": "run.correct",
        "run_id": original["run_id"],
        "emitted_at": "2026-08-10T02:00:00Z",
        "team": original["team"],
        "host": original["host"],
        "producer": {
            "source": "reconstructed",
            "tool": "validate_rows.py",
            "tool_version": "1",
        },
        "supersedes": original["event_id"],
        "reason": "replace explicit null with the writer's current representation",
        "legacy_row": corrected,
    }


def record_event_for_team(
    root: Path, event_type: str, team: str
) -> tuple[list[dict], dict]:
    """Return a valid result and the requested legacy-row-bearing event."""
    original = event_with_null_failed_substeps(root)
    original["team"] = team
    original["legacy_row"]["repo"] = team
    if event_type == "run.result":
        return [original], original
    if event_type == "run.enrich":
        enrichment = {
            **{key: original[key] for key in ledger.ENVELOPE_KEYS},
            "event_id": "historical-null-enrichment",
            "event_type": "run.enrich",
            "emitted_at": "2026-08-10T01:00:00Z",
            "enriches": original["event_id"],
            "legacy_row": copy.deepcopy(original["legacy_row"]),
        }
        return [original, enrichment], enrichment
    correction = correction_for(original)
    return [original, correction], correction


def identity() -> dict:
    return {
        "lane": "strict",
        "category": "record-replay",
        "test": "hello",
        "mode": "verify",
        "backend": "ptrace",
    }


def comparison() -> dict:
    return {
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


def verdict(state: str = "compared-and-matched") -> dict:
    if state in {"performs-no-comparison-by-design", "unavailable-with-reason"}:
        return {
            "state": state,
            "comparison_tier": "declared-but-unverifiable",
            "reason": "this cell is an exit-status probe" if state.startswith("performs") else "raw no_result: comparator output absent",
        }
    return {
        "state": state,
        "comparison_tier": "canonical-bitwise",
        "comparison": comparison(),
        "bitwise_parity": state == "compared-and-matched",
        "compared_log_messages": {"left": 3, "right": 3},
    }


def with_cell_results(
    source: dict,
    root: Path,
    state: str = "compared-and-matched",
    *,
    run_id: str | None = None,
) -> dict:
    source["schema_version"] = 6
    source["run_id"] = run_id or source["record_id"]
    selected = [identity()]
    cell = {**identity(), "cell_verdict": verdict(state)}
    artifact_row = {
        "run_id": source["run_id"],
        "hermit_sha": source["commit"],
        "source_tree_dirty": False,
        **cell,
    }
    raw = (json.dumps(artifact_row, sort_keys=True, separators=(",", ":")) + "\n").encode()
    artifact_rel = Path("ignored/validate/artifacts") / source["record_id"] / "e2e-results.jsonl"
    artifact = root / artifact_rel
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_bytes(raw)
    source["cell_results"] = {
        "run_id": source["run_id"],
        "hermit_sha": source["commit"],
        "source_tree_dirty": False,
        "selected_count": 1,
        "recorded_count": 1,
        "population_sha256": validate_rows.qualifying_receipt.cell_population_sha256(selected),
        "artifact": {
            "path": artifact_rel.as_posix(),
            "sha256": hashlib.sha256(raw).hexdigest(),
            "row_count": 1,
        },
        "selected": selected,
        "cells": [cell],
    }
    return source


@pytest.fixture
def schema8_reader_policy(monkeypatch: pytest.MonkeyPatch):
    predicate = copy.deepcopy(validate_rows.qualifying_receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 8
    monkeypatch.setattr(validate_rows.qualifying_receipt, "active", lambda: predicate)


def with_schema8_cell_results(
    source: dict,
    root: Path,
    *,
    path: str = "full",
    selected_count: int = 1,
    recorded_count: int = 1,
) -> dict:
    source = with_cell_results(source, root)
    source["schema_version"] = 8
    source["profile"] = path
    source["selection_mode"] = "full"
    evidence = source["cell_results"]
    evidence["path"] = path
    if selected_count == 0:
        evidence["selected"] = []
    elif selected_count == 2:
        second = {**evidence["selected"][0], "test": "second"}
        evidence["selected"].append(second)
    evidence["selected_count"] = selected_count
    if recorded_count == 0:
        evidence["cells"] = []
    evidence["recorded_count"] = recorded_count
    evidence["population_sha256"] = (
        validate_rows.qualifying_receipt.cell_population_sha256(evidence["selected"])
    )
    artifact = root / evidence["artifact"]["path"]
    raw = b"" if recorded_count == 0 else artifact.read_bytes()
    artifact.write_bytes(raw)
    evidence["artifact"]["sha256"] = hashlib.sha256(raw).hexdigest()
    evidence["artifact"]["row_count"] = recorded_count
    return source


def test_record_is_immediately_visible_through_canonical_union(tmp_path: Path):
    source = row(tmp_path)
    report = validate_rows.record(source, root=tmp_path)

    assert report["spool"].startswith(
        "ignored/ci-hub/validate-ledger-spool/hermit__hosta__2026-08__"
    )
    assert report["published_shard"] == "ledger/hermit/hosta/2026-08.jsonl"
    assert not (tmp_path / report["published_shard"]).exists()
    assert validate_rows.rows(root=tmp_path) == [source]


def test_record_carries_the_admitted_run_number(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setenv(validate_rows.RUN_NUMBER_ENV, "701")
    source = row(tmp_path)

    validate_rows.record(source, root=tmp_path)

    [event] = validate_rows.events(root=tmp_path)
    assert event["run_number"] == 701
    assert event["legacy_row"]["run_number"] == 701
    assert validate_rows.rows(root=tmp_path)[0]["run_number"] == 701


def test_record_carries_the_admitted_source_branch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setenv(validate_rows.BRANCH_ENV, "codex/validate-branch-provenance")
    source = row(tmp_path)

    validate_rows.record(source, root=tmp_path)

    [event] = validate_rows.events(root=tmp_path)
    assert event["legacy_row"]["branch"] == "codex/validate-branch-provenance"
    assert validate_rows.rows(root=tmp_path)[0]["branch"] == (
        "codex/validate-branch-provenance"
    )


def test_record_without_admission_branch_keeps_branch_absent(tmp_path: Path):
    validate_rows.record(row(tmp_path), root=tmp_path)

    [event] = validate_rows.events(root=tmp_path)
    assert "branch" not in event["legacy_row"]
    assert "branch" not in validate_rows.rows(root=tmp_path)[0]


def test_record_prefers_the_admission_producers_source_branch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setenv(validate_rows.BRANCH_ENV, "codex/admitted")
    source = row(tmp_path)
    source["branch"] = "codex/stale-copy"

    validate_rows.record(source, root=tmp_path)

    [event] = validate_rows.events(root=tmp_path)
    assert event["legacy_row"]["branch"] == "codex/admitted"


@pytest.mark.parametrize("run_number", ["0", "-1", "01", "not-a-number"])
def test_record_omits_an_invalid_optional_admitted_run_number(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, run_number: str
):
    monkeypatch.setenv(validate_rows.RUN_NUMBER_ENV, run_number)
    validate_rows.record(row(tmp_path), root=tmp_path)
    [event] = validate_rows.events(root=tmp_path)
    assert "run_number" not in event
    assert "run_number" not in event["legacy_row"]


def test_record_prefers_the_admission_producers_optional_run_number(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setenv(validate_rows.RUN_NUMBER_ENV, "701")
    source = row(tmp_path)
    source["run_number"] = 702
    validate_rows.record(source, root=tmp_path)
    [event] = validate_rows.events(root=tmp_path)
    assert event["run_number"] == 701
    assert event["legacy_row"]["run_number"] == 701


@pytest.mark.parametrize("run_number", [0, -1, True, "701"])
def test_record_omits_an_invalid_optional_row_run_number(
    tmp_path: Path, run_number: object
):
    source = row(tmp_path)
    source["run_number"] = run_number
    validate_rows.record(source, root=tmp_path)
    [event] = validate_rows.events(root=tmp_path)
    assert "run_number" not in event
    assert "run_number" not in event["legacy_row"]


@pytest.mark.parametrize(
    "gate",
    [
        {
            "name": "pass-with-null",
            "result": "pass",
            "failure_origin": None,
            "failed_substeps": None,
        },
        {"name": "outer-missing", "result": "fail", "failure_origin": "outer_gate"},
        {
            "name": "outer-nonempty",
            "result": "fail",
            "failure_origin": "outer_gate",
            "failed_substeps": ["test.child"],
        },
        {
            "name": "lane-empty",
            "result": "fail",
            "failure_origin": "lane_substep",
            "failed_substeps": [],
        },
        {
            "name": "unbound-empty",
            "result": "fail",
            "failed_substeps": [],
        },
        {
            "name": "aborted-outer",
            "result": "fail",
            "aborted": True,
            "failure_origin": "outer_gate",
            "failed_substeps": [],
        },
    ],
)
def test_record_refuses_inconsistent_gate_failure_bindings(
    tmp_path: Path, gate: dict
):
    source = row(tmp_path, record_id=f"bad-{gate['name']}")
    source.update(result="fail", exit_code=1, failures=1, gates=[gate])

    with pytest.raises(ValueError, match="inconsistent failure_origin/failed_substeps"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


@pytest.mark.parametrize(
    "gate",
    [
        {"name": "unbound", "result": "fail"},
        {
            "name": "outer",
            "result": "fail",
            "failure_origin": "outer_gate",
            "failed_substeps": [],
        },
        {
            "name": "lane",
            "result": "fail",
            "failure_origin": "lane_substep",
            "failed_substeps": ["test.child"],
        },
    ],
)
def test_record_preserves_valid_gate_failure_bindings(tmp_path: Path, gate: dict):
    source = row(tmp_path, record_id=f"good-{gate['name']}")
    source.update(result="fail", exit_code=1, failures=1, gates=[gate])

    validate_rows.record(source, root=tmp_path)

    assert validate_rows.rows(root=tmp_path) == [source]


def test_canonical_reverie_repo_uses_top_level_reverie_shard(tmp_path: Path):
    source = row(tmp_path)
    source["repo"] = "rrnewton/reverie"
    reverie = tmp_path / "worktrees" / "slot" / "reverie"
    source["commit"] = repository(reverie, content="reverie\n")
    source["cwd"] = str(reverie)

    report = validate_rows.record(source, root=tmp_path)

    assert report["spool"].startswith(
        "ignored/ci-hub/validate-ledger-spool/reverie__hosta__2026-08__"
    )
    assert report["published_shard"] == "ledger/reverie/hosta/2026-08.jsonl"
    assert validate_rows.rows(root=tmp_path) == [source]


def test_unmapped_path_bearing_repo_is_refused_before_spool(tmp_path: Path):
    source = row(tmp_path)
    source["repo"] = "unknown/repository"

    with pytest.raises(ValueError, match="path-safe ledger team mapping"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_repeated_record_of_same_event_unions_once(tmp_path: Path):
    source = row(tmp_path)
    validate_rows.record(source, root=tmp_path)
    validate_rows.record(source, root=tmp_path)

    assert len(validate_rows.events(root=tmp_path)) == 1
    assert validate_rows.rows(root=tmp_path) == [source]


def test_valid_correction_replaces_legacy_row_without_hiding_original_event(
    tmp_path: Path,
):
    original = event_with_null_failed_substeps(tmp_path)
    correction = correction_for(original)
    path = write_events(tmp_path, original, correction)
    original_line = path.read_text(encoding="utf-8").splitlines()[0]

    canonical_events = validate_rows.events(root=tmp_path)
    assert canonical_events == [original, correction]
    assert json.loads(original_line) == original

    compatibility_rows = validate_rows.rows(root=tmp_path)
    assert len(compatibility_rows) == 1
    assert compatibility_rows[0]["record_id"] == original["legacy_row"]["record_id"]
    assert compatibility_rows[0]["gates"][0]["failed_substeps"] == []
    assert original["legacy_row"]["gates"][0]["failed_substeps"] is None

    status = validate_rows.status_input(root=tmp_path)
    assert status["schema_version"] == 1
    assert status["rows"] == compatibility_rows
    assert [record["event_id"] for record in status["records"]] == [
        original["event_id"],
        correction["event_id"],
    ]
    assert [record["event_type"] for record in status["records"]] == [
        "run.result",
        "run.correct",
    ]
    assert [record["commit"] for record in status["records"]] == [
        original["commit"],
        original["commit"],
    ]


def test_status_input_cli_reads_the_production_event_adapter_once(tmp_path: Path):
    original = event_with_null_failed_substeps(tmp_path)
    correction = correction_for(original)
    write_events(tmp_path, original, correction)
    environment = os.environ.copy()
    environment.update(
        HERMIT_VALIDATE_STOP_TEST_MODE="1",
        CI_HUB_VALIDATE_LEDGER_TEST_ROOT=str(tmp_path),
    )

    result = subprocess.run(
        [sys.executable, str(HERE.parent / "validate_rows.py"), "status-input"],
        check=True,
        capture_output=True,
        text=True,
        env=environment,
    )

    status = json.loads(result.stdout)
    assert len(status["rows"]) == 1
    assert len(status["records"]) == 2
    assert {record["event_type"] for record in status["records"]} == {
        "run.result",
        "run.correct",
    }


@pytest.mark.parametrize("event_type", ["run.result", "run.enrich", "run.correct"])
@pytest.mark.parametrize(
    ("team", "embedded_repo"),
    [("hermit", "reverie"), ("reverie", "hermit")],
)
def test_record_event_team_must_match_embedded_repo(
    tmp_path: Path, event_type: str, team: str, embedded_repo: str
):
    items, candidate = record_event_for_team(tmp_path, event_type, team)
    candidate["legacy_row"]["repo"] = embedded_repo
    write_events(tmp_path, *items)

    for reader in (validate_rows.rows, validate_rows.status_input):
        with pytest.raises(ValueError, match="conflicts with legacy_row.repo"):
            reader(root=tmp_path)


@pytest.mark.parametrize("event_type", ["run.result", "run.enrich", "run.correct"])
@pytest.mark.parametrize("representation", ["missing", "null"])
def test_historical_hermit_record_without_repo_remains_compatible(
    tmp_path: Path, event_type: str, representation: str
):
    items, candidate = record_event_for_team(tmp_path, event_type, "hermit")
    if representation == "missing":
        candidate["legacy_row"].pop("repo")
    else:
        candidate["legacy_row"]["repo"] = None
    write_events(tmp_path, *items)

    status = validate_rows.status_input(root=tmp_path)

    assert all(record["repo"] == "hermit" for record in status["records"])
    if representation == "missing":
        assert "repo" not in status["rows"][-1]
    else:
        assert status["rows"][-1]["repo"] is None


@pytest.mark.parametrize("event_type", ["run.result", "run.enrich", "run.correct"])
@pytest.mark.parametrize("representation", ["missing", "null"])
def test_reverie_record_without_repo_is_refused(
    tmp_path: Path, event_type: str, representation: str
):
    items, candidate = record_event_for_team(tmp_path, event_type, "reverie")
    if representation == "missing":
        candidate["legacy_row"].pop("repo")
    else:
        candidate["legacy_row"]["repo"] = None
    write_events(tmp_path, *items)

    with pytest.raises(ValueError, match="outside the historical Hermit"):
        validate_rows.status_input(root=tmp_path)


def test_allowlisted_id_does_not_admit_a_similar_repository_mismatch(
    tmp_path: Path,
):
    items, candidate = record_event_for_team(tmp_path, "run.result", "hermit")
    candidate.update(
        event_id="legacy-000246-000",
        run_id="legacy-000246",
        producer={
            "source": "imported",
            "tool": "ci-hub/ledger/migrate_legacy",
            "tool_version": "1",
        },
    )
    candidate["legacy_row"]["repo"] = "reverie"
    write_events(tmp_path, *items)

    with pytest.raises(ValueError, match="differs from its immutable source record"):
        validate_rows.status_input(root=tmp_path)


def test_exact_historical_repository_mismatch_population_is_auditable():
    root = HERE.parents[2]
    expected = validate_rows.HISTORICAL_REPOSITORY_MISMATCHES
    assert len(expected) == 19

    copies = []
    for relative in validate_rows.HISTORICAL_REPOSITORY_MISMATCH_SHARDS:
        selected = {
            event["event_id"]: event
            for event in ledger.read_shard(root / relative)
            if event.get("event_id") in expected
        }
        assert set(selected) == set(expected)
        copies.append(selected)
    assert copies[0] == copies[1]

    source_digests = validate_rows._historical_repository_mismatch_source_digests(
        root
    )
    assert len(source_digests) == 19
    for event_id, event in copies[0].items():
        commit, digest = expected[event_id]
        row = event["legacy_row"]
        assert event["schema"] == "validate-ledger/v1"
        assert event["event_type"] == "run.result"
        assert event["team"] == "hermit"
        assert event["host"] == "devbig014"
        assert event["producer"] == {
            "source": "imported",
            "tool": "ci-hub/ledger/migrate_legacy",
            "tool_version": "1",
        }
        assert row["schema_version"] == 3
        assert row["repo"] == "reverie"
        assert event["commit"] == row["commit"] == commit
        assert "reason" not in row
        assert validate_rows._canonical_event_sha256(event) == digest
        assert (
            validate_rows._legacy_row_repo(
                event,
                historical_source_digests=source_digests,
            )
            == "reverie"
        )


def test_tracked_production_union_is_readable_without_count_loss(
    monkeypatch: pytest.MonkeyPatch,
):
    root = HERE.parents[2]
    gathered = []
    for path in validate_rows.shard_paths(root):
        gathered.extend(ledger.read_shard(path))
    items = [
        event
        for event in ledger.union_events(gathered)
        if not validate_rows._is_acceptance_fixture_event(event)
    ]
    monkeypatch.setattr(validate_rows, "events", lambda **_kwargs: items)

    validated, _historical_source_digests = validate_rows._validated_events(
        root=root
    )
    status = validate_rows.status_input(root=root)

    assert validated == items
    assert len(status["rows"]) == len(
        validate_rows._rows_from_events(items, root=root)
    )
    expected_record_ids = [
        event["event_id"]
        for event in items
        if event.get("event_type") == "run.result"
        or (
            event.get("event_type") == "run.correct"
            and isinstance(event.get("legacy_row"), dict)
        )
    ]
    assert [record["event_id"] for record in status["records"]] == expected_record_ids
    assert set(validate_rows.HISTORICAL_REPOSITORY_MISMATCHES) <= set(
        expected_record_ids
    )


def test_exact_historical_repository_mismatch_is_readable(tmp_path: Path):
    event = production_historical_repository_mismatch("legacy-000246-000")
    write_historical_repository_mismatch_copies(tmp_path, event)

    status = validate_rows.status_input(root=tmp_path)

    assert len(status["rows"]) == 1
    assert status["rows"][0]["repo"] == "reverie"
    assert status["rows"][0]["commit"] == event["commit"]
    assert status["records"] == [
        {
            "event_id": event["event_id"],
            "event_type": "run.result",
            "run_id": event["run_id"],
            "repo": "reverie",
            "commit": event["commit"],
        }
    ]


def historical_snapshot_fixture(tmp_path: Path) -> tuple[Path, Path, str, str]:
    source = tmp_path / "source"
    previous = repository(source)
    event = production_historical_repository_mismatch("legacy-000246-000")
    write_historical_repository_mismatch_copies(source, event)
    commit = git(source, "rev-parse", "HEAD").stdout.strip()
    snapshot = tmp_path / "snapshot"
    for relative in validate_rows.HISTORICAL_REPOSITORY_MISMATCH_SHARDS:
        destination = snapshot / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes((source / relative).read_bytes())
    return source, snapshot, commit, previous


def test_captured_history_binds_original_commit_without_a_git_snapshot(tmp_path: Path):
    source, snapshot, commit, _ = historical_snapshot_fixture(tmp_path)
    expected = validate_rows.rows(root=source)
    with pytest.raises(ValueError, match="historical repository-mismatch event"):
        validate_rows.rows(root=snapshot)

    # The capture remains anchored when the source checkout advances or has dirt.
    primary = source / validate_rows.HISTORICAL_REPOSITORY_MISMATCH_SHARDS[0]
    primary.write_text("{}\n")
    git(source, "add", str(primary))
    git(source, "commit", "-qm", "move the source beyond the captured commit")
    primary.write_text("uncommitted source bytes\n")
    actual = validate_rows.rows(root=snapshot, committed_source=(source, commit))
    assert json.dumps(actual, sort_keys=True).replace(str(snapshot), "{{ROOT}}") == \
        json.dumps(expected, sort_keys=True).replace(str(source), "{{ROOT}}")
    assert actual[0]["repo"] == "reverie"


@pytest.mark.parametrize("shard_index", [0, 1])
@pytest.mark.parametrize("damage", ["missing", "byte-change", "symlink"])
def test_captured_history_requires_both_exact_regular_git_blobs(
    tmp_path: Path, shard_index: int, damage: str,
):
    source, snapshot, commit, _ = historical_snapshot_fixture(tmp_path)
    relative = validate_rows.HISTORICAL_REPOSITORY_MISMATCH_SHARDS[shard_index]
    path = snapshot / relative
    if damage == "missing":
        path.unlink()
    elif damage == "byte-change":
        # Parsed events and their known original digests still match; the
        # immutable captured file nevertheless differs from its named Git blob.
        path.write_bytes(path.read_bytes() + b"\n")
    else:
        path.unlink()
        path.symlink_to(source / relative)
    with pytest.raises(ValueError, match="historical repository-mismatch event"):
        validate_rows.rows(root=snapshot, committed_source=(source, commit))


@pytest.mark.parametrize("binding", ["wrong-commit", "foreign-repository", "tree-object", "moving-ref"])
def test_captured_history_refuses_wrong_immutable_source(tmp_path: Path, binding: str):
    source, snapshot, commit, previous = historical_snapshot_fixture(tmp_path)
    if binding == "wrong-commit":
        commit = previous
    elif binding == "foreign-repository":
        source = tmp_path / "foreign"
        repository(source, content="different repository\n")
    elif binding == "tree-object":
        commit = git(source, "rev-parse", "HEAD^{tree}").stdout.strip()
    else:
        commit = "HEAD"
    with pytest.raises(ValueError, match="historical repository-mismatch event|immutable commit SHA"):
        validate_rows.rows(root=snapshot, committed_source=(source, commit))


@pytest.mark.parametrize("provenance", ["non-git", "untracked", "foreign-root"])
def test_historical_repository_mismatch_requires_tracked_source_provenance(
    tmp_path: Path, provenance: str
):
    authority = tmp_path
    if provenance in {"untracked", "foreign-root"}:
        repository(tmp_path)
    if provenance == "foreign-root":
        authority = tmp_path / "foreign"
    event = production_historical_repository_mismatch("legacy-000246-000")
    text = json.dumps(event, sort_keys=True) + "\n"
    for relative in validate_rows.HISTORICAL_REPOSITORY_MISMATCH_SHARDS:
        path = authority / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    with pytest.raises(ValueError, match="historical repository-mismatch event"):
        validate_rows.status_input(root=authority)


def test_historical_repository_mismatch_requires_both_current_source_copies(
    tmp_path: Path,
):
    event = production_historical_repository_mismatch("legacy-000246-000")
    write_historical_repository_mismatch_copies(tmp_path, event)
    (tmp_path / validate_rows.HISTORICAL_REPOSITORY_MISMATCH_SHARDS[0]).write_text(
        "", encoding="utf-8"
    )

    with pytest.raises(ValueError, match="historical repository-mismatch event"):
        validate_rows.status_input(root=tmp_path)


def test_historical_repository_mismatch_refuses_symlinked_source_ancestors(
    tmp_path: Path,
):
    event = production_historical_repository_mismatch("legacy-000246-000")
    write_historical_repository_mismatch_copies(tmp_path, event)
    relocated = tmp_path / "relocated"
    relocated.mkdir()
    for directory in ("ledger", "ledger-recovery"):
        destination = relocated / directory
        shutil.move(str(tmp_path / directory), destination)
        (tmp_path / directory).symlink_to(destination, target_is_directory=True)

    with pytest.raises(ValueError, match="historical repository-mismatch event"):
        validate_rows.status_input(root=tmp_path)


def test_moved_historical_repository_mismatch_is_refused(tmp_path: Path):
    event = production_historical_repository_mismatch("legacy-000246-000")
    moved = tmp_path / "ledger/reverie/devbig014/2026-08.jsonl"
    moved.parent.mkdir(parents=True)
    moved.write_text(json.dumps(event, sort_keys=True) + "\n", encoding="utf-8")

    with pytest.raises(ValueError, match="differs from its immutable source record"):
        validate_rows.status_input(root=tmp_path)


@pytest.mark.parametrize(
    "case",
    [
        "event-id",
        "event-type",
        "run-id",
        "emitted-at",
        "team",
        "team-matches-row",
        "host",
        "producer-source",
        "producer-tool",
        "producer-version",
        "outcome",
        "event-commit",
        "row-commit",
        "row-repo",
        "row-missing",
        "row-repo-missing",
        "row-repo-null",
        "row-schema",
        "row-reason",
    ],
)
def test_altered_historical_repository_mismatch_is_refused(
    tmp_path: Path, case: str
):
    event = copy.deepcopy(
        production_historical_repository_mismatch("legacy-000246-000")
    )
    if case == "event-id":
        event["event_id"] += "-new"
    elif case == "event-type":
        event["event_type"] = "run.enrich"
        event["enriches"] = "some-earlier-event"
    elif case == "run-id":
        event["run_id"] += "-new"
    elif case == "emitted-at":
        event["emitted_at"] = "2026-08-04T09:52:23Z"
    elif case == "team":
        event["team"] = "other"
    elif case == "team-matches-row":
        event["team"] = "reverie"
    elif case == "host":
        event["host"] = "other-host"
    elif case == "producer-source":
        event["producer"]["source"] = "observed"
    elif case == "producer-tool":
        event["producer"]["tool"] = "other-tool"
    elif case == "producer-version":
        event["producer"]["tool_version"] = "2"
    elif case == "outcome":
        event["outcome"] = "pass" if event["outcome"] != "pass" else "fail"
    elif case == "event-commit":
        event["commit"] = "0" * 40
    elif case == "row-commit":
        event["legacy_row"]["commit"] = "0" * 40
    elif case == "row-repo":
        event["legacy_row"]["repo"] = "hermit"
    elif case == "row-missing":
        event.pop("legacy_row")
    elif case == "row-repo-missing":
        event["legacy_row"].pop("repo")
    elif case == "row-repo-null":
        event["legacy_row"]["repo"] = None
    elif case == "row-schema":
        event["legacy_row"]["schema_version"] = 4
    elif case == "row-reason":
        event["legacy_row"]["reason"] = "fabricated"
    write_historical_repository_mismatch_copies(tmp_path, event)

    message = (
        "conflicts with legacy_row.repo"
        if case == "event-id"
        else "differs from its immutable source record"
    )
    with pytest.raises(ValueError, match=message):
        validate_rows.status_input(root=tmp_path)


def test_historical_repository_exception_preserves_corrections_and_counts(
    tmp_path: Path,
):
    historical = production_historical_repository_mismatch("legacy-000246-000")
    write_historical_repository_mismatch_copies(tmp_path, historical)
    original = event_with_null_failed_substeps(tmp_path)
    correction = correction_for(original)
    write_events(tmp_path, original, correction)

    status = validate_rows.status_input(root=tmp_path)

    assert len(status["rows"]) == 2
    assert len(status["records"]) == 3
    assert status["rows"][0]["repo"] == "reverie"
    assert status["rows"][1]["gates"][0]["failed_substeps"] == []
    assert [record["event_type"] for record in status["records"]] == [
        "run.result",
        "run.result",
        "run.correct",
    ]


@pytest.mark.parametrize("event_type", ["run.enrich", "run.correct"])
def test_new_event_for_historical_repository_mismatch_is_refused(
    tmp_path: Path, event_type: str
):
    historical = production_historical_repository_mismatch("legacy-000246-000")
    write_historical_repository_mismatch_copies(tmp_path, historical)
    candidate = {
        **{key: historical[key] for key in ledger.ENVELOPE_KEYS},
        "event_id": f"{historical['event_id']}-{event_type}",
        "event_type": event_type,
        "emitted_at": "2026-08-10T02:00:00Z",
        "legacy_row": copy.deepcopy(historical["legacy_row"]),
    }
    if event_type == "run.enrich":
        candidate["enriches"] = historical["event_id"]
    else:
        candidate["supersedes"] = historical["event_id"]
        candidate["reason"] = "fixture correction"
    write_events(tmp_path, historical, candidate)

    with pytest.raises(ValueError, match="conflicts with legacy_row.repo"):
        validate_rows.status_input(root=tmp_path)


@pytest.mark.parametrize(
    "case",
    [
        "missing-event-id",
        "missing-supersedes",
        "dangling-supersedes",
        "wrong-run",
        "wrong-team",
        "wrong-host",
        "empty-reason",
        "non-object-row",
        "explicit-null-remains",
    ],
)
def test_malformed_correction_never_hides_original_row(tmp_path: Path, case: str):
    original = event_with_null_failed_substeps(tmp_path)
    correction = correction_for(original)
    if case == "missing-event-id":
        correction.pop("event_id")
    elif case == "missing-supersedes":
        correction.pop("supersedes")
    elif case == "dangling-supersedes":
        correction["supersedes"] = "absent-event"
    elif case == "wrong-run":
        correction["run_id"] = "different-run"
    elif case == "wrong-team":
        correction["team"] = "different-team"
    elif case == "wrong-host":
        correction["host"] = "different-host"
    elif case == "empty-reason":
        correction["reason"] = ""
    elif case == "non-object-row":
        correction["legacy_row"] = None
    elif case == "explicit-null-remains":
        correction["legacy_row"]["gates"][0]["failed_substeps"] = None
    path = write_events(tmp_path, original, correction)
    original_line = path.read_text(encoding="utf-8").splitlines()[0]

    assert original in validate_rows.events(root=tmp_path)
    with pytest.raises(ValueError, match="HistoryRow correction"):
        validate_rows.rows(root=tmp_path)
    assert path.read_text(encoding="utf-8").splitlines()[0] == original_line


# https://github.com/rrnewton/dev-hermit/issues/390: each replay ran the shared
# checker once for the whole union and then once more for every correction
# (101 runs, each under the host-wide checker cache lock, for the 99
# corrections in the 2026-09-29 ledger). These pin the batched replacement.


def _corrected_pairs(root: Path, count: int) -> list[dict]:
    """``count`` originals, each followed by its valid correction."""
    events = []
    for index in range(count):
        source = row(root, record_id=f"historical-null-{index}")
        source.update(
            result="fail",
            exit_code=1,
            failures=1,
            gates=[{
                "name": "check.example",
                "result": "fail",
                "failure_origin": "outer_gate",
                "failed_substeps": None,
            }],
        )
        original = validate_rows._event_for_row(source, root)
        correction = correction_for(original)
        correction["event_id"] = f"historical-null-correction-{index}"
        events += [original, correction]
    return events


def _count_checker_runs(monkeypatch) -> list[int]:
    """Lines sent on each real shared-checker run, in order."""
    real = ledger._run_shared_type_checker
    runs: list[int] = []

    def counting(payload, arguments=()):
        runs.append(payload.count("\n"))
        return real(payload, arguments)

    monkeypatch.setattr(ledger, "_run_shared_type_checker", counting)
    return runs


@pytest.mark.parametrize("preserve_admission", [False, True])
def test_replay_checks_every_correction_in_one_checker_run(
    tmp_path: Path, monkeypatch, preserve_admission: bool
):
    events = _corrected_pairs(tmp_path, 4)
    write_events(tmp_path, *events)
    runs = _count_checker_runs(monkeypatch)

    replayed = validate_rows.rows(root=tmp_path, preserve_admission=preserve_admission)
    # One run over all eight events. The corrections reuse its verdict on
    # their exact lines instead of four more single-line runs.
    assert runs == [8]
    assert [r["record_id"] for r in replayed] == [f"historical-null-{i}" for i in range(4)]
    assert all(r["gates"][0]["failed_substeps"] == [] for r in replayed)

    runs.clear()
    assert validate_rows.status_input(
        root=tmp_path, preserve_admission=preserve_admission,
    )["rows"] == replayed
    assert runs == [8]

    # A caller without that verdict still checks all four corrections in one
    # run rather than one run each.
    items = validate_rows.events(root=tmp_path, preserve_admission=preserve_admission)
    runs.clear()
    assert validate_rows._rows_from_events(items, root=tmp_path) == replayed
    assert runs == [4]


def test_replay_without_a_comparable_line_checks_corrections_again(
    tmp_path: Path, monkeypatch
):
    events = _corrected_pairs(tmp_path, 4)
    write_events(tmp_path, *events)
    expected = validate_rows.rows(root=tmp_path, preserve_admission=True)
    runs = _count_checker_runs(monkeypatch)

    def unavailable(event):
        raise TypeError("no comparable line")

    # Preserve mode sends the admission identity. When the plain line it is
    # compared with cannot be built, nothing is reused and the corrections get
    # their own run.
    monkeypatch.setattr(validate_rows, "shared_type_line", unavailable)
    assert validate_rows.rows(root=tmp_path, preserve_admission=True) == expected
    assert runs == [8, 4]


def test_committed_namespace_checks_corrections_in_one_checker_run(
    tmp_path: Path, monkeypatch
):
    source = _producer_selector_row(tmp_path, gates=[{
        "name": "check.example", "result": "fail", "failure_origin": "outer_gate",
        "failed_substeps": None,
    }])
    original = _producer_selector_event(source, tmp_path)
    write_events(tmp_path, original, correction_for(original))
    commit = _commit_namespace(tmp_path)
    runs = _count_checker_runs(monkeypatch)
    # It projects the union twice; before the batching that was 1 + 2 runs.
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        assert runs == [2]
        [corrected] = ns.rows()
        assert corrected["gates"][0]["failed_substeps"] == []


def test_batched_correction_refusal_is_validate_events_refusal(tmp_path: Path):
    events = _corrected_pairs(tmp_path, 2)
    events[3]["producer"]["source"] = "bogus-source"
    expected = ledger.validate_event(
        events[3], known_ids={event["event_id"] for event in events},
    )
    assert expected, "the shared checker must refuse the unknown producer source"

    with pytest.raises(ValueError) as refused:
        validate_rows._rows_from_events(events, root=tmp_path)
    assert str(refused.value) == (
        f"HistoryRow correction {events[3]['event_id']!r} is invalid: "
        f"event violates {','.join(expected)}"
    )


def test_validate_each_event_matches_validate_event_per_event(
    tmp_path: Path, monkeypatch
):
    events = _corrected_pairs(tmp_path, 3)
    ok, bad_type, later_ok, owner, bad_outcome = (
        events[1], copy.deepcopy(events[3]), events[5],
        copy.deepcopy(events[1]), copy.deepcopy(events[0]),
    )
    bad_type["producer"]["source"] = "bogus-source"
    owner["reason"] = "copied from /home/someone/notes"
    bad_outcome["outcome"] = "bogus-outcome"
    mixed = [ok, bad_type, later_ok, owner, bad_outcome]
    known = {event["event_id"] for event in events}
    expected = [ledger.validate_event(event, known_ids=known) for event in mixed]
    assert [bool(codes) for codes in expected] == [False, True, False, True, True]

    runs = _count_checker_runs(monkeypatch)
    assert list(ledger.validate_each_event(mixed, known_ids=known)) == expected
    # The checker stops at its first refused line, so the three lines after it
    # are each checked again on their own.
    assert runs == [5, 1, 1, 1]

    runs.clear()
    clean = [ok, later_ok, events[0]]
    assert list(ledger.validate_each_event(clean, known_ids=known)) == [[], [], []]
    assert runs == [3]


def test_validate_each_event_reuses_only_an_exactly_accepted_line(
    tmp_path: Path, monkeypatch
):
    events = _corrected_pairs(tmp_path, 1)
    accepted = events[1]
    changed = copy.deepcopy(accepted)
    changed["producer"]["source"] = "bogus-source"
    known = {event["event_id"] for event in events}
    expected = ledger.validate_event(changed, known_ids=known)
    assert expected
    lines = frozenset({ledger.shared_type_line(accepted)})

    runs = _count_checker_runs(monkeypatch)
    assert list(ledger.validate_each_event(
        [accepted], known_ids=known, accepted_lines=lines,
    )) == [[]]
    assert runs == []
    # Reuse is keyed by the exact line, so a changed event is checked again.
    assert list(ledger.validate_each_event(
        [changed], known_ids=known, accepted_lines=lines,
    )) == [expected]
    assert runs == [1]


def _real_checker_verdict(lines: list[str]) -> tuple[int, str] | None:
    """The real checker's (line number, message) for its first refusal, or None."""
    result = ledger._run_shared_type_checker("".join(line + "\n" for line in lines))
    if result.returncode == 0:
        return None
    detail = result.stderr.strip() or result.stdout.strip()
    match = re.search(r"stdin line (\d+): (.*)", detail, re.DOTALL)
    assert match, f"checker refused without naming a line: {detail!r}"
    return int(match.group(1)), match.group(2).strip()


def test_real_checker_refusal_does_not_depend_on_earlier_lines(tmp_path: Path):
    # validate_each_event reads the refusal of line N in a many-line run as
    # the refusal a run over line N alone would give. That holds only while
    # check-ledger-events.rs judges each line on its own; pin it on the real
    # checker, not a stand-in.
    events = _corrected_pairs(tmp_path, 2)
    bad_source = copy.deepcopy(events[3])
    bad_source["producer"]["source"] = "bogus-source"
    bad_outcome = copy.deepcopy(events[0])
    bad_outcome["outcome"] = "bogus-outcome"
    accepted = [ledger.shared_type_line(event) for event in events]
    for refused, valid in ((bad_source, events[3]), (bad_outcome, events[0])):
        line = ledger.shared_type_line(refused)
        alone = _real_checker_verdict([line])
        assert alone is not None and alone[0] == 1 and alone[1]
        # Earlier lines a stateful checker could remember: the valid form of
        # the same event (same event_id), and every line of both event types.
        contexts = {"same event": [ledger.shared_type_line(valid)], "all": accepted}
        for name, context in contexts.items():
            # Refused at its own position with the same message, which also
            # means the checker accepted every line before it.
            assert _real_checker_verdict(context + [line]) == (
                len(context) + 1, alone[1],
            ), name


def test_preserve_replay_rechecks_a_correction_whose_admission_line_differs(
    tmp_path: Path, monkeypatch
):
    events = _corrected_pairs(tmp_path, 4)
    write_events(tmp_path, *events)
    expected = validate_rows.rows(root=tmp_path, preserve_admission=True)
    odd = events[5]
    real_identity = validate_rows._admission_event_identity

    def identity(value):
        if value.get("event_id") == odd["event_id"]:
            # The same JSON value with spaces after separators: the checker
            # accepts it, but it is not the line the correction check sends.
            return json.dumps(value, sort_keys=True)
        return real_identity(value)

    monkeypatch.setattr(validate_rows, "_admission_event_identity", identity)
    real_run = ledger._run_shared_type_checker
    payloads: list[str] = []

    def capturing(payload, arguments=()):
        payloads.append(payload)
        return real_run(payload, arguments)

    monkeypatch.setattr(ledger, "_run_shared_type_checker", capturing)
    assert validate_rows.rows(root=tmp_path, preserve_admission=True) == expected
    # The union run judged the spaced line, so that verdict is not reused for
    # the plain line: exactly that one correction is checked again, and the
    # other three reuse the union run.
    #
    # What this pins is the lookup: reuse is keyed on the exact line the
    # correction check sends. Recording shared_type_line(event) for every
    # correction the union run passed fails here (measured: 1 checker run,
    # not 2). Removing only the `same` comparison in _checked_events does not:
    # the spaced line it would then record matches no line the correction
    # check sends, so that comparison is a second guard, not the only one.
    assert len(payloads) == 2
    assert len(payloads[0].splitlines()) == 8
    assert json.dumps(odd, sort_keys=True) in payloads[0].splitlines()
    assert ledger.shared_type_line(odd) not in payloads[0].splitlines()
    assert payloads[1] == ledger.shared_type_line(odd) + "\n"


@pytest.mark.parametrize("enabled", [True, False])
def test_replay_pauses_cycle_collection_and_restores_it(
    tmp_path: Path, monkeypatch, enabled: bool
):
    write_events(tmp_path, *_corrected_pairs(tmp_path, 1))
    real = validate_rows._replace
    observed: list[bool] = []

    def observing(value, old, new):
        observed.append(gc.isenabled())
        return real(value, old, new)

    def failing(value, old, new):
        raise RuntimeError("replay failed")

    was_enabled = gc.isenabled()
    try:
        (gc.enable if enabled else gc.disable)()
        monkeypatch.setattr(validate_rows, "_replace", observing)
        validate_rows.rows(root=tmp_path, preserve_admission=True)
        assert gc.isenabled() is enabled
        monkeypatch.setattr(validate_rows, "_replace", failing)
        with pytest.raises(RuntimeError, match="replay failed"):
            validate_rows.rows(root=tmp_path, preserve_admission=True)
        assert gc.isenabled() is enabled
    finally:
        (gc.enable if was_enabled else gc.disable)()
    assert observed and not any(observed)


def _producer_selector_row(root: Path, **changes) -> dict:
    # Data-only event/selection fixture, not a qualified finalized-run proof.
    return {
        "schema_version": 6,
        "repo": "hermit",
        "run_id": "producer-run",
        "record_id": "producer-final",
        "producer": "hermit-validate-rs",
        "host": "hosta",
        "commit": "a" * 40,
        "cwd": str(root / "hermit"),
        "started_at": "2026-08-09T01:00:00Z",
        "finished_at": "2026-08-09T01:01:00Z",
        "result": "fail",
        "executed_tests": None,
        "passed_tests": None,
        **changes,
    }


def _producer_selector_event(row: dict, root: Path) -> dict:
    # This data-only miniature ledger owns root, even when pytest stores it
    # inside a real dev-hermit workspace. Declare its portable cwd explicitly;
    # leave the independent expected selector in the original absolute form.
    relative_cwd = Path(row["cwd"]).relative_to(root).as_posix()
    portable = {**row, "cwd": f"{validate_rows.WORKSPACE_TOKEN}/{relative_cwd}"}
    return validate_rows._event_for_row(portable, root)


def _producer_selector_args(row: dict) -> dict:
    return {key: row[key] for key in
            ("repo", "run_id", "record_id", "commit", "cwd", "started_at")}


def _commit_namespace(root: Path) -> str:
    # Tiny real Git data repository, not a product checkout or admitted run.
    if not (root / ".git").exists():
        repository(root)
        git(root, "remote", "add", "origin", validate_rows.CANONICAL_PARENT_REPOSITORY)
    git(root, "add", "ledger", *(["ledger-recovery"] if (root / "ledger-recovery").exists() else []))
    git(root, "commit", "-qm", "synthetic committed ledger namespace")
    commit = git(root, "rev-parse", "HEAD").stdout.strip()
    git(root, "update-ref", "refs/remotes/origin/main", commit)
    return commit


def test_committed_namespace_complete_recovery_and_original_occurrences(tmp_path: Path):
    source = tmp_path / "source"
    state = tmp_path / "reader-state"
    state.mkdir()
    first = _producer_selector_row(source, record_id="first")
    final = _producer_selector_row(source, corrects="first")
    original = _producer_selector_event(first, source)
    terminal = _producer_selector_event(final, source)
    terminal["event_id"] = "actual-envelope-id-not-record-id"
    primary = write_events(source, original, terminal)
    recovery = source / "ledger-recovery/hermit/hosta/2026-08/retained.jsonl"
    recovery.parent.mkdir(parents=True)
    recovery.write_bytes((json.dumps(terminal, sort_keys=True) + "\n").encode())
    commit = _commit_namespace(source)
    expected = {**final, "cwd": str(state / "hermit")}

    with validate_rows.capture_committed_validation_namespace(source, commit, state_root=state) as ns:
        selected = ns.select(**_producer_selector_args(expected))
        assert selected["row"] == expected
        assert selected["terminal"]["event_id"] == terminal["event_id"]
        assert selected["terminal"]["record_id"] == final["record_id"]
        assert selected["terminal"]["event_sha256"] == hashlib.sha256(
            validate_rows._admission_event_identity(terminal).encode()).hexdigest()
        assert selected["row"]["result"] == "fail"
        assert selected["row"]["executed_tests"] is None
        assert len(ns.rows()) == 2  # identical primary/recovery overlap collapses
        assert ns.events() == validate_rows.events(root=source, preserve_admission=True)
        copied_events = ns.events()
        copied_events[0]["legacy_row"]["result"] = "pass"
        assert copied_events != ns.events()
        ns.verify_origin(selected["parent"], selected["terminal"])
        with pytest.raises(ValueError, match="terminal differs from requested record_id"):
            ns.select(**_producer_selector_args({**first, "cwd": expected["cwd"]}))
        captured = ns.source_files()
        assert {item["path"] for item in captured} == {
            primary.relative_to(source).as_posix(), recovery.relative_to(source).as_posix()}
        selected["row"]["result"] = "pass"
        captured[0]["data"] = b"not the original bytes"
        assert ns.select(**_producer_selector_args(expected))["row"]["result"] == "fail"
        assert ns.source_files()[0]["data"] != captured[0]["data"]


def _committed_view_namespace(tmp_path: Path):
    source = tmp_path / "source"
    state = tmp_path / "reader-state"
    state.mkdir()
    first = _producer_selector_row(source, record_id="first")
    final = _producer_selector_row(source, corrects="first")
    write_events(source, _producer_selector_event(first, source),
                 _producer_selector_event(final, source))
    commit = _commit_namespace(source)
    expected = {**final, "cwd": str(state / "hermit")}
    return (validate_rows.capture_committed_validation_namespace(source, commit, state_root=state),
            expected)


def _namespace_answers(ns, expected: dict) -> bytes:
    # Pickle records every container type, AdmissionJsonObject.pairs, and
    # which objects are shared, so equal bytes mean the same answers.
    return pickle.dumps((ns.rows(), ns.events(), ns.source_files(),
                         ns.select(**_producer_selector_args(expected)),
                         ns.terminals(repo="hermit")), 5)


def _reachable_containers(value) -> list:
    found, seen, stack = [], set(), [value]
    while stack:
        item = stack.pop()
        if isinstance(item, (str, int, float, bool, type(None))) or id(item) in seen:
            continue
        seen.add(id(item))
        found.append(item)
        if isinstance(item, dict):
            stack.extend(item.values())
            stack.extend(getattr(item, "pairs", ()))
        else:
            stack.extend(item)
    return found


class _MutableList(list):
    __slots__ = ()


class _MutableDict(dict):
    __slots__ = ()


def _list_mutations(value: list) -> list:
    def assign_class():
        # Layout-compatible with the view class, so only the view's own
        # __setattr__ can refuse it.
        value.__class__ = _MutableList

    def augmented_add():
        view = value
        view += [1]

    def augmented_multiply():
        view = value
        view *= 2

    def slice_delete():
        del value[:]

    def slice_assign():
        value[0:0] = [1]

    return [lambda: value.append(1), lambda: value.extend([1]), lambda: value.insert(0, 1),
            lambda: value.pop(), lambda: value.remove(value[0] if value else 1),
            value.clear, value.sort, value.reverse, lambda: value.__setitem__(0, 1),
            lambda: value.__delitem__(0), augmented_add, augmented_multiply,
            slice_delete, slice_assign, lambda: value.__init__([9]), assign_class]


def _dict_mutations(value: dict) -> list:
    key = next(iter(value), "absent")

    def augmented_or():
        view = value
        view |= {"new": 1}

    def item_delete():
        del value[key]

    def item_assign():
        value[key] = "changed"

    def assign_class():
        value.__class__ = _MutableDict

    return [value.clear, lambda: value.pop(key), value.popitem,
            lambda: value.setdefault("new", 1), lambda: value.update(new=1),
            lambda: value.update({key: "changed"}), augmented_or, item_delete, item_assign,
            lambda: value.__init__(zzz=1), assign_class]


def _admission_attribute_mutations(value) -> list:
    def assign_pairs():
        value.pairs = ()

    def delete_pairs():
        del value.pairs

    def assign_other():
        value.other = 1

    def assign_class():
        value.__class__ = qualifying_receipt.AdmissionJsonObject

    return [assign_pairs, delete_pairs, assign_other, assign_class]


@pytest.mark.parametrize("view_name", ["rows_view", "events_view"])
def test_committed_namespace_views_refuse_every_ordinary_mutation(tmp_path: Path, view_name: str):
    ns, expected = _committed_view_namespace(tmp_path)
    answers = _namespace_answers(ns, expected)
    view = getattr(ns, view_name)()
    before = pickle.dumps(copy.deepcopy(view), 5)
    containers = _reachable_containers(view)
    kinds = {type(item) for item in containers}
    # The fixture must reach an admission object and a list; plain dicts and
    # tuples are covered by test_read_only_view_keeps_types_sharing_and_pairs.
    assert validate_rows._ReadOnlyAdmissionObject in kinds and validate_rows._ReadOnlyList in kinds
    assert not kinds & {list, dict, qualifying_receipt.AdmissionJsonObject}
    attempted = 0
    for item in containers:
        if isinstance(item, tuple):
            continue
        mutations = _list_mutations(item) if isinstance(item, list) else _dict_mutations(item)
        if isinstance(item, qualifying_receipt.AdmissionJsonObject):
            mutations += _admission_attribute_mutations(item)
        for mutate in mutations:
            with pytest.raises(TypeError, match="committed validation views are read-only"):
                mutate()
            attempted += 1
    # At least one list and one admission object received every mutation.
    assert attempted >= len(_list_mutations([])) + len(_dict_mutations({})) + 4
    assert pickle.dumps(copy.deepcopy(view), 5) == before
    again = getattr(ns, view_name)()
    if view_name == "rows_view":
        # The export reads rows twice, so this view is built once and kept.
        assert again is view and ns._views == {"rows": view}
    else:
        # The export reads events once, so each call builds a new view and
        # the namespace keeps no reference to either.
        assert again is not view and again == view
        assert "events" not in ns._views
        assert not any(item is view or item is again
                       for item in (*vars(ns).values(), *ns._views.values()))
    assert _namespace_answers(ns, expected) == answers


def test_committed_namespace_view_bypass_cannot_reach_the_namespace(tmp_path: Path):
    # Even the C-level interface that the view does not guard changes only the
    # view: select(), terminals(), rows(), events() and source_files() read
    # separate objects.  This fails if a view shares a container with them.
    ns, expected = _committed_view_namespace(tmp_path)
    answers = _namespace_answers(ns, expected)
    rows, events = ns.rows_view(), ns.events_view()
    for view in (rows, events):
        for item in _reachable_containers(view):
            if isinstance(item, qualifying_receipt.AdmissionJsonObject):
                object.__setattr__(item, "pairs", ())
            if isinstance(item, dict):
                for key in list(dict.keys(item)):
                    dict.__setitem__(item, key, "bypassed")
                dict.__setitem__(item, "added", "bypassed")
            elif isinstance(item, list):
                list.append(item, "bypassed")
    assert rows[0]["result"] == "bypassed" and events[-1] == "bypassed"
    assert _namespace_answers(ns, expected) == answers


def test_committed_namespace_views_copy_back_to_rows_and_events(tmp_path: Path):
    ns, _expected = _committed_view_namespace(tmp_path)
    for view, copied in ((ns.rows_view(), ns.rows()), (ns.events_view(), ns.events())):
        assert view == copied
        assert pickle.dumps(copy.deepcopy(view), 5) == pickle.dumps(copied, 5)
        assert type(copy.copy(view)) is list and copy.copy(view) == copied
        assert json.dumps(view) == json.dumps(copied)
        assert (json.dumps(view, sort_keys=True, separators=(",", ":"))
                == json.dumps(copied, sort_keys=True, separators=(",", ":")))
        for item in _reachable_containers(view):
            if isinstance(item, tuple):
                continue  # an admission pair; pickling its container is refused
            with pytest.raises(TypeError, match="committed validation views are read-only"):
                pickle.dumps(item, 5)
            if isinstance(item, dict):
                assert type(copy.copy(item)) is (qualifying_receipt.AdmissionJsonObject
                    if isinstance(item, qualifying_receipt.AdmissionJsonObject) else dict)
        # A deep copy is the ordinary mutable population rows()/events() return.
        mutable = copy.deepcopy(view)
        mutable[0]["changed"] = True
        mutable.append({})
        assert view == copied


def test_read_only_view_keeps_types_sharing_and_pairs():
    shared = ["shared"]
    duplicate = qualifying_receipt.AdmissionJsonObject(
        [("k", shared), ("k", {"inner": 1}), ("m", shared)])
    cyclic: list = []
    cyclic.append(cyclic)
    through_tuple: list = []
    looped = (through_tuple, "leaf")
    through_tuple.append(looped)
    value = [duplicate, {"t": (shared, 1.5, None, True, "text"), 7: None},
             shared, cyclic, looped]
    original = pickle.dumps(copy.deepcopy(value), 5)
    view = validate_rows._read_only_view(value, {})
    assert pickle.dumps(copy.deepcopy(view), 5) == original
    assert type(view) is validate_rows._ReadOnlyList
    assert type(view[0]) is validate_rows._ReadOnlyAdmissionObject
    assert type(view[1]) is validate_rows._ReadOnlyDict and type(view[1]["t"]) is tuple
    assert view[0].pairs == (("k", ["shared"]), ("k", {"inner": 1}), ("m", ["shared"]))
    assert view[0].pairs[0][1] is view[0]["m"] is view[1]["t"][0] is view[2]
    assert view[0].pairs[1][1] is view[0]["k"] and view[3][0] is view[3]
    assert view[4][0][0] is view[4] and type(view[4][0]) is validate_rows._ReadOnlyList
    assert view[2] is not shared and view[0] is not duplicate
    copied = copy.deepcopy(view)
    assert type(copied[0]) is qualifying_receipt.AdmissionJsonObject
    assert copied[0].pairs[1][1] is copied[0]["k"] and copied[0]["m"] is copied[2]
    assert json.dumps(view[:3]) == json.dumps(value[:3])
    assert pickle.dumps(copy.deepcopy(value), 5) == original


def test_read_only_view_refuses_every_ordinary_mutation_of_plain_containers():
    # Committed rows and events are admission objects and lists, so the
    # namespace test above never reaches a plain dict view.
    value = [{"a": ["x", {"b": 1}], "c": {"d": "e"}}, ["f"]]
    original = pickle.dumps(value, 5)
    view = validate_rows._read_only_view(value, {})
    containers = _reachable_containers(view)
    assert [type(item) for item in containers].count(validate_rows._ReadOnlyList) == 3
    assert [type(item) for item in containers].count(validate_rows._ReadOnlyDict) == 3
    assert len(containers) == 6
    before = pickle.dumps(copy.deepcopy(view), 5)
    attempted = 0
    for item in containers:
        mutations = _list_mutations(item) if isinstance(item, list) else _dict_mutations(item)
        for mutate in mutations:
            with pytest.raises(TypeError, match="committed validation views are read-only"):
                mutate()
            attempted += 1
    assert attempted == 3 * len(_list_mutations([])) + 3 * len(_dict_mutations({}))
    assert pickle.dumps(copy.deepcopy(view), 5) == before
    assert pickle.dumps(value, 5) == original


@pytest.mark.parametrize("unsupported", ["subclass", "extra_attribute", "list_pairs", "tuple_key"])
def test_committed_namespace_view_falls_back_to_a_private_copy(unsupported: str):
    class Row(dict):
        pass

    if unsupported == "subclass":
        item = Row(repo="hermit")
    elif unsupported == "tuple_key":
        item = {"repo": "hermit", ("tuple",): "key"}
    else:
        item = qualifying_receipt.AdmissionJsonObject([("repo", "hermit")])
        if unsupported == "extra_attribute":
            item.note = "unexpected"
        else:
            item.pairs = [("repo", "hermit")]
    population = [item]
    with pytest.raises(validate_rows._NotViewable):
        validate_rows._read_only_view(population, {})
    holder = validate_rows.CommittedValidationNamespace.__new__(
        validate_rows.CommittedValidationNamespace)
    holder._views = {}
    copied = holder._view("rows", population)
    assert copied == population and copied[0] is not item and type(copied[0]) is type(item)
    assert "rows" not in holder._views
    copied[0]["repo"] = "changed"
    assert item["repo"] == "hermit"
    # events_view takes the same fallback without retaining anything.
    holder._events = {"event": item}
    events = holder.events_view()
    assert events == [item] and events[0] is not item and type(events[0]) is type(item)
    assert holder._views == {} and holder.events_view() is not events


def test_committed_namespace_retry_keeps_original_origin_after_unrelated_advance(tmp_path: Path):
    source = _producer_selector_row(tmp_path)
    event = _producer_selector_event(source, tmp_path)
    path = write_events(tmp_path, event)
    before = _commit_namespace(tmp_path)
    selected = validate_rows.canonical_finalized_row_at_commit(
        tmp_path, before, **_producer_selector_args(source))
    unrelated = _producer_selector_event(
        _producer_selector_row(tmp_path, run_id="other-run", record_id="other-record"), tmp_path)
    path.write_bytes(path.read_bytes() + (json.dumps(unrelated) + "\n").encode())
    after = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, after) as ns:
        current = ns.select(**_producer_selector_args(source))
        assert current["terminal"] == selected["terminal"]
        assert current["parent"] != selected["parent"]
        ns.verify_origin(selected["parent"], selected["terminal"])
        ns.verify_origin(selected["parent"], selected["terminal"])
    assert selected["parent"]["commit"] == before


def test_committed_namespace_ignores_mutable_sources_but_rejects_committed_successor(tmp_path: Path):
    source = _producer_selector_row(tmp_path)
    event = _producer_selector_event(source, tmp_path)
    path = write_events(tmp_path, event)
    old = _commit_namespace(tmp_path)
    selected = validate_rows.canonical_finalized_row_at_commit(tmp_path, old, **_producer_selector_args(source))
    successor = _producer_selector_row(tmp_path, record_id="successor", corrects=source["record_id"])
    next_event = _producer_selector_event(successor, tmp_path)
    validate_rows._spool_record_event(next_event, tmp_path)
    path.write_bytes(path.read_bytes() + (json.dumps(next_event) + "\n").encode())
    # A mutable worktree and its live spool cannot replace captured Git bytes.
    assert validate_rows.canonical_finalized_row_at_commit(
        tmp_path, old, **_producer_selector_args(source)) == selected
    new = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, new) as ns:
        with pytest.raises(ValueError, match="terminal differs from requested record_id"):
            ns.select(**_producer_selector_args(source))
        with pytest.raises(ValueError, match="terminal differs from requested record_id"):
            ns.verify_origin(selected["parent"], selected["terminal"])
        assert ns.select(**_producer_selector_args(successor))["terminal"] != selected["terminal"]


def test_committed_namespace_tracks_event_correction_with_unchanged_record_id(tmp_path: Path):
    source = _producer_selector_row(tmp_path, gates=[{
        "name": "check.example", "result": "fail", "failure_origin": "outer_gate",
        "failed_substeps": None,
    }])
    original = _producer_selector_event(source, tmp_path)
    path = write_events(tmp_path, original)
    first = _commit_namespace(tmp_path)
    old = validate_rows.canonical_finalized_row_at_commit(tmp_path, first, **_producer_selector_args(source))
    correction = correction_for(original)
    path.write_bytes(path.read_bytes() + (json.dumps(correction) + "\n").encode())
    second = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, second) as ns:
        new = ns.select(**_producer_selector_args(source))
        assert new["row"]["gates"][0]["failed_substeps"] == []
        assert new["terminal"]["record_id"] == old["terminal"]["record_id"]
        assert new["terminal"]["event_id"] == correction["event_id"]
        assert new["terminal"]["event_sha256"] != old["terminal"]["event_sha256"]
        ns.verify_origin(new["parent"], new["terminal"])
        with pytest.raises(ValueError, match="absent or superseded"):
            ns.verify_origin(old["parent"], old["terminal"])


@pytest.mark.parametrize("first_call", ["shared-row", "other-row"])
def test_committed_namespace_refuses_a_row_carried_by_two_original_events(tmp_path: Path, first_call: str):
    # Two envelopes carry one identical row, so no single original event can be
    # named. select() must refuse on the call that builds its identity index
    # and on every later call that reuses it, whichever row is selected first.
    shared = _producer_selector_row(tmp_path)
    other = _producer_selector_row(tmp_path, run_id="other-run", record_id="other-record")
    original = _producer_selector_event(shared, tmp_path)
    twin = copy.deepcopy(original)
    twin["event_id"] = "second-envelope-same-row"
    unrelated = _producer_selector_event(other, tmp_path)
    write_events(tmp_path, original, twin, unrelated)
    commit = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        assert len(ns.rows()) == 2  # the shared row projects once
        if first_call == "other-row":
            assert ns.select(**_producer_selector_args(other))["terminal"]["event_id"] == unrelated["event_id"]
        for _ in range(2):
            with pytest.raises(ValueError, match="terminal has ambiguous original events"):
                ns.select(**_producer_selector_args(shared))
        selected = ns.select(**_producer_selector_args(other))
        assert selected["terminal"]["event_id"] == unrelated["event_id"]
        ns.verify_origin(selected["parent"], selected["terminal"])
        with pytest.raises(ValueError, match="terminal has ambiguous original events"):
            ns.select(**_producer_selector_args(shared))
        with pytest.raises(ValueError, match="terminal has ambiguous original events"):
            ns.terminals(repo="hermit")


def test_committed_namespace_repeated_selects_return_each_rows_own_event(tmp_path: Path):
    # Interleaved and repeated selections, including a correction chain, must
    # each resolve to the row's own original event and pass verify_origin().
    rows = [_producer_selector_row(tmp_path, run_id=f"run-{index}", record_id=f"record-{index}",
                                   result="pass" if index % 2 else "fail")
            for index in range(4)]
    older = _producer_selector_row(tmp_path, run_id="chain", record_id="chain-old")
    newer = _producer_selector_row(tmp_path, run_id="chain", record_id="chain-new", corrects="chain-old")
    events = [_producer_selector_event(row, tmp_path) for row in (*rows, older, newer)]
    write_events(tmp_path, *events)
    commit = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        first_selection: dict[int, dict] = {}
        for index in (2, 0, 3, 1, 2, 0):
            selected = ns.select(**_producer_selector_args(rows[index]))
            assert selected["row"] == rows[index]
            assert selected["terminal"]["event_id"] == events[index]["event_id"]
            assert selected["terminal"]["record_id"] == rows[index]["record_id"]
            assert first_selection.setdefault(index, selected) == selected
            ns.verify_origin(selected["parent"], selected["terminal"])
        chained = ns.select(**_producer_selector_args(newer))
        assert chained["terminal"]["event_id"] == events[-1]["event_id"]
        ns.verify_origin(chained["parent"], chained["terminal"])
        with pytest.raises(ValueError, match="terminal differs from requested record_id"):
            ns.select(**_producer_selector_args(older))
        assert [item["terminal"]["event_id"] for item in ns.terminals(repo="hermit")] == [
            events[-1]["event_id"], *(event["event_id"] for event in events[:4])]


def test_committed_namespace_projection_identity_error_raises_from_every_select(tmp_path: Path, monkeypatch):
    # An identity failure on another row's projection must raise from each
    # select(), and the failed calls must leave no partial index: a partial
    # index would omit run-2's projection and refuse it as ambiguous.
    rows = [_producer_selector_row(tmp_path, run_id=f"run-{index}", record_id=f"record-{index}")
            for index in range(3)]
    events = [_producer_selector_event(row, tmp_path) for row in rows]
    write_events(tmp_path, *events)
    commit = _commit_namespace(tmp_path)
    real = validate_rows._admission_event_identity

    def failing(value):
        if isinstance(value, dict) and "legacy_row" not in value and value.get("record_id") == "record-1":
            raise RuntimeError("projection identity failed")
        return real(value)

    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        monkeypatch.setattr(validate_rows, "_admission_event_identity", failing)
        for _ in range(2):
            with pytest.raises(RuntimeError, match="projection identity failed"):
                ns.select(**_producer_selector_args(rows[2]))
        monkeypatch.setattr(validate_rows, "_admission_event_identity", real)
        selected = ns.select(**_producer_selector_args(rows[2]))
        assert selected["terminal"]["event_id"] == events[2]["event_id"]
        assert ns.select(**_producer_selector_args(rows[1]))["terminal"]["event_id"] == events[1]["event_id"]


@pytest.mark.parametrize("kind", ["conflict", "truncated", "symlink", "gitlink", "branch"])
def test_committed_namespace_refuses_opposing_full_namespace(tmp_path: Path, kind: str):
    source = _producer_selector_row(tmp_path)
    event = _producer_selector_event(source, tmp_path)
    write_events(tmp_path, event)
    first = _commit_namespace(tmp_path)
    recovery = tmp_path / "ledger-recovery/hermit/hosta/2026-08/opponent.jsonl"
    recovery.parent.mkdir(parents=True)
    if kind == "symlink":
        recovery.symlink_to(tmp_path / "ledger/hermit/hosta/2026-08.jsonl")
    elif kind == "gitlink":
        git(tmp_path, "update-index", "--add", "--cacheinfo", "160000", first,
            "ledger-recovery/hermit/hosta/2026-08/opponent")
    else:
        opposing = copy.deepcopy(event)
        if kind == "conflict":
            opposing["legacy_row"]["result"] = "pass"
        elif kind == "branch":
            opposing = _producer_selector_event(
                _producer_selector_row(tmp_path, record_id="unrelated-terminal"), tmp_path)
        recovery.write_bytes((json.dumps(opposing) + ("" if kind == "truncated" else "\n")).encode())
    if kind == "gitlink":
        git(tmp_path, "commit", "-qm", "synthetic nonregular namespace")
        commit = git(tmp_path, "rev-parse", "HEAD").stdout.strip()
        git(tmp_path, "update-ref", "refs/remotes/origin/main", commit)
    else:
        commit = _commit_namespace(tmp_path)
    with pytest.raises((ValueError, ledger.LedgerError)):
        validate_rows.canonical_finalized_row_at_commit(tmp_path, commit, **_producer_selector_args(source))


@pytest.mark.parametrize("field,value", [
    ("repository", "https://github.com/somewhere/else.git"),
    ("commit", "f" * 40), ("blob_oid", "f" * 40),
    ("event_line_sha256", "f" * 64), ("shard", "ledger/../outside.jsonl"),
    ("shard", "ledger/hermit/hosta/not-a-shard.txt"),
])
def test_committed_namespace_original_provenance_is_not_a_boolean(tmp_path: Path, field, value):
    source = _producer_selector_row(tmp_path)
    write_events(tmp_path, _producer_selector_event(source, tmp_path))
    commit = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        selected = ns.select(**_producer_selector_args(source))
        ns.verify_origin(selected["parent"], selected["terminal"])
        with pytest.raises(ValueError):
            ns.verify_origin({**selected["parent"], field: value}, selected["terminal"])
        with pytest.raises(ValueError, match="unknown or missing"):
            ns.verify_origin({**selected["parent"], "qualified": True}, selected["terminal"])


def test_committed_namespace_source_identity_and_hostile_git_environment(tmp_path: Path, monkeypatch):
    source = _producer_selector_row(tmp_path)
    write_events(tmp_path, _producer_selector_event(source, tmp_path))
    commit = _commit_namespace(tmp_path)
    alternate = tmp_path / "other-repository"
    replacement = repository(alternate)
    git(alternate, "remote", "add", "origin", "https://github.com/wrong/repo.git")
    monkeypatch.setenv("GIT_DIR", str(alternate / ".git"))
    monkeypatch.setenv("GIT_WORK_TREE", str(alternate))
    monkeypatch.setenv("GIT_CONFIG_COUNT", "1")
    monkeypatch.setenv("GIT_CONFIG_KEY_0", "remote.origin.url")
    monkeypatch.setenv("GIT_CONFIG_VALUE_0", "https://github.com/wrong/repo.git")
    selected = validate_rows.canonical_finalized_row_at_commit(tmp_path, commit, **_producer_selector_args(source))
    assert selected["row"] == source
    with pytest.raises(ValueError, match="exact lowercase digest"):
        validate_rows.capture_committed_validation_namespace(tmp_path, commit[:12])
    with pytest.raises(ValueError, match="exact Git root"):
        validate_rows.capture_committed_validation_namespace(tmp_path / "ledger", commit)
    with pytest.raises(ValueError, match="canonical parent"):
        validate_rows.capture_committed_validation_namespace(alternate, replacement)


def test_committed_namespace_refuses_nonancestor_origin_and_ignores_replacements(tmp_path: Path):
    source = _producer_selector_row(tmp_path)
    write_events(tmp_path, _producer_selector_event(source, tmp_path))
    commit = _commit_namespace(tmp_path)
    selected = validate_rows.canonical_finalized_row_at_commit(tmp_path, commit, **_producer_selector_args(source))
    tree = git(tmp_path, "rev-parse", f"{commit}^{{tree}}").stdout.strip()
    disconnected = git(tmp_path, "commit-tree", tree, "-m", "unrelated identical tree").stdout.strip()
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        with pytest.raises(ValueError, match="Git read refused"):
            ns.verify_origin({**selected["parent"], "commit": disconnected}, selected["terminal"])
    empty_tree = subprocess.run(
        ["git", "-C", str(tmp_path), "hash-object", "-t", "tree", "-w", "--stdin"],
        input=b"", capture_output=True, check=True,
    ).stdout.decode().strip()
    replacement = git(tmp_path, "commit-tree", empty_tree, "-m", "replacement hides all events").stdout.strip()
    git(tmp_path, "replace", commit, replacement)
    assert git(tmp_path, "ls-tree", "-r", commit).stdout == ""
    assert validate_rows.canonical_finalized_row_at_commit(
        tmp_path, commit, **_producer_selector_args(source)) == selected


def test_committed_namespace_git_output_has_an_enforced_capacity(tmp_path: Path):
    repository(tmp_path)
    with pytest.raises(ValueError, match="exceeds capacity"):
        validate_rows._committed_git(tmp_path, "rev-parse", "HEAD", limit=3)


def test_committed_namespace_ssh_origin_and_unpublished_source_refusal(tmp_path: Path):
    source = _producer_selector_row(tmp_path)
    event = _producer_selector_event(source, tmp_path)
    path = write_events(tmp_path, event)
    original = _commit_namespace(tmp_path)
    git(tmp_path, "remote", "set-url", "origin", "git@github.com:rrnewton/dev-hermit.git")
    selected = validate_rows.canonical_finalized_row_at_commit(
        tmp_path, original, **_producer_selector_args(source))
    assert selected["parent"]["repository"] == validate_rows.CANONICAL_PARENT_REPOSITORY
    event["legacy_row"]["result"] = "no_result"
    path.write_bytes((json.dumps(event) + "\n").encode())
    git(tmp_path, "add", "ledger")
    git(tmp_path, "commit", "-qm", "unpublished synthetic change")
    unpublished = git(tmp_path, "rev-parse", "HEAD").stdout.strip()
    with pytest.raises(ValueError, match="Git read refused"):
        validate_rows.capture_committed_validation_namespace(tmp_path, unpublished)
    assert validate_rows.canonical_finalized_row_at_commit(
        tmp_path, original, **_producer_selector_args(source)) == selected
    git(tmp_path, "remote", "set-url", "origin", "git@not-github.example:rrnewton/dev-hermit.git")
    with pytest.raises(ValueError, match="canonical parent"):
        validate_rows.capture_committed_validation_namespace(tmp_path, original)


def test_committed_terminals_keep_complete_cross_schema_groups_and_failures(tmp_path: Path):
    first = _producer_selector_row(tmp_path, record_id="old", schema_version=6)
    modern = _producer_selector_row(tmp_path, record_id="modern", schema_version=10, corrects="old")
    latest = _producer_selector_row(tmp_path, record_id="latest", schema_version=6, corrects="modern", result="no_result")
    failed = _producer_selector_row(tmp_path, run_id="second-run", record_id="failed", schema_version=10)
    write_events(tmp_path, *[_producer_selector_event(row, tmp_path) for row in (first, modern, latest, failed)])
    commit = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        selections = ns.terminals(repo="rrnewton/hermit", schema_version=10)
        assert [item["row"] for item in selections] == [latest, failed]
        assert [item["row"]["result"] for item in selections] == ["no_result", "fail"]
        assert all(item["row"]["executed_tests"] is None for item in selections)
        assert "cell_results" not in selections[1]["row"]  # selection is not admission
        selections[0]["row"]["result"] = "pass"
        assert ns.terminals(repo="hermit", schema_version=10)[0]["row"] == latest


def test_committed_census_terminals_select_current_rows_of_claiming_runs(tmp_path: Path):
    # Only a run with a schema-5 raw_result_input_census_v1 row is selected,
    # and it is selected at its current row, even when that row has another
    # schema and no census. A schema-5 run without the field stays absent.
    census = {"schema": 1, "files": []}
    claimed = _producer_selector_row(tmp_path, run_id="census-run", record_id="census-old",
                                     schema_version=5, raw_result_input_census_v1=census)
    current = _producer_selector_row(tmp_path, run_id="census-run", record_id="census-new",
                                     schema_version=6, corrects="census-old")
    plain = _producer_selector_row(tmp_path, run_id="plain-run", record_id="plain", schema_version=5)
    other = _producer_selector_row(tmp_path, repo="reverie", run_id="reverie-run",
                                   record_id="reverie-census", schema_version=5,
                                   raw_result_input_census_v1=census)
    write_events(tmp_path, *[_producer_selector_event(row, tmp_path) for row in (claimed, current, plain)])
    write_events(tmp_path, _producer_selector_event(other, tmp_path))
    commit = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        selected = ns.census_terminals(repo="rrnewton/hermit")
        assert selected == [ns.select(**_producer_selector_args(current))]
        assert selected[0]["row"] == current and "raw_result_input_census_v1" not in selected[0]["row"]
        assert [item["row"]["record_id"] for item in ns.census_terminals(repo="reverie")] == [
            "reverie-census"]
        selected[0]["row"]["result"] = "pass"
        assert ns.census_terminals(repo="hermit")[0]["row"] == current
        with pytest.raises(ValueError, match="nonempty repo"):
            ns.census_terminals(repo=" ")


def test_committed_census_terminals_refuse_a_census_without_stable_run_identity(tmp_path: Path):
    unstable = _producer_selector_row(tmp_path, record_id="census-without-run", schema_version=5,
                                      raw_result_input_census_v1={"schema": 1, "files": []})
    del unstable["run_id"]
    historical = _producer_selector_row(tmp_path, record_id="historical", schema_version=5)
    del historical["run_id"]
    write_events(tmp_path, *[_producer_selector_event(row, tmp_path) for row in (unstable, historical)])
    commit = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        with pytest.raises(ValueError, match="claimed derived census lacks a stable run identity"):
            ns.census_terminals(repo="hermit")
    # Historical schema-5 rows without a census do not acquire the requirement.
    write_events(tmp_path, _producer_selector_event(historical, tmp_path))
    commit = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        assert ns.census_terminals(repo="hermit") == []


@pytest.mark.parametrize("opponent", ["missing", "branched", "disconnected"])
def test_committed_terminals_schema_filter_cannot_hide_opposing_chain(tmp_path: Path, opponent):
    first = _producer_selector_row(tmp_path, record_id="old", schema_version=6)
    modern = _producer_selector_row(tmp_path, record_id="modern", schema_version=10, corrects="old")
    records = [first, modern]
    if opponent == "missing":
        records = [modern]
    else:
        records.append(_producer_selector_row(tmp_path, record_id="opposing", schema_version=6,
                       **({"corrects": "old"} if opponent == "branched" else {})))
    write_events(tmp_path, *[_producer_selector_event(row, tmp_path) for row in records])
    commit = _commit_namespace(tmp_path)
    with validate_rows.capture_committed_validation_namespace(tmp_path, commit) as ns:
        with pytest.raises(ValueError, match="canonical producer run"):
            ns.terminals(repo="hermit", schema_version=10)


def test_committed_git_refuses_implicit_fetch_at_real_transport(tmp_path: Path, monkeypatch):
    commit = repository(tmp_path)
    real_popen = subprocess.Popen
    observed = []
    def checked_popen(argv, **kwargs):
        observed.append(kwargs["env"])
        assert kwargs["env"]["GIT_NO_LAZY_FETCH"] == "1"
        assert "GIT_DIR" not in kwargs["env"]
        return real_popen(argv, **kwargs)
    monkeypatch.setenv("GIT_DIR", str(tmp_path / "missing"))
    monkeypatch.setenv("GIT_NO_LAZY_FETCH", "0")
    monkeypatch.setattr(subprocess, "Popen", checked_popen)
    assert validate_rows._committed_git(tmp_path, "rev-parse", "HEAD").decode().strip() == commit
    assert len(observed) == 1


@pytest.mark.parametrize("result", ["pass", "fail", "no_result"])
def test_producer_row_preserves_terminal_result_and_null_counts(tmp_path: Path, result: str):
    source = _producer_selector_row(tmp_path, result=result)
    event = _producer_selector_event(source, tmp_path)
    assert event["workspace"]["repo_relative"] == "{{WORKSPACE_ROOT}}/hermit"
    assert event["legacy_row"]["cwd"] == "{{WORKSPACE_ROOT}}/hermit"
    path = write_events(tmp_path, event)
    before = path.read_bytes()

    selected = validate_rows.canonical_finalized_row(tmp_path, **_producer_selector_args(source))

    assert selected == source
    assert selected["result"] == result
    assert selected["executed_tests"] is None and selected["passed_tests"] is None
    assert path.read_bytes() == before


def test_producer_row_uses_complete_union_and_ignores_other_runs(tmp_path: Path):
    first = _producer_selector_row(tmp_path, record_id="producer-first")
    final = _producer_selector_row(tmp_path, corrects="producer-first")
    # A newer same-head run must neither substitute for nor poison this run.
    other = _producer_selector_row(tmp_path, run_id="other-run", record_id="other",
                                   corrects="missing", finished_at="2026-08-09T02:00:00Z")
    original = _producer_selector_event(first, tmp_path)
    write_events(tmp_path, original, _producer_selector_event(other, tmp_path))
    validate_rows._spool_record_event(original, tmp_path)  # identical overlap is ordinary union
    validate_rows._spool_record_event(_producer_selector_event(final, tmp_path), tmp_path)

    selected = validate_rows.canonical_finalized_row(tmp_path, **_producer_selector_args(final))

    assert selected == final
    assert len(validate_rows.events(root=tmp_path)) == 3
    assert len(validate_rows.rows(root=tmp_path)) == 3
    with pytest.raises(ValueError, match="terminal differs from requested record_id"):
        validate_rows.canonical_finalized_row(tmp_path, **_producer_selector_args(first))


def test_producer_row_cannot_hide_a_competing_repository_alias(tmp_path: Path):
    source = _producer_selector_row(tmp_path)
    opponent = _producer_selector_row(tmp_path, repo="rrnewton/hermit", record_id="alias-opponent")
    write_events(tmp_path, _producer_selector_event(source, tmp_path),
                 _producer_selector_event(opponent, tmp_path))

    with pytest.raises(ValueError, match="no unique terminal record"):
        validate_rows.canonical_finalized_row(tmp_path, **_producer_selector_args(source))

    # An alias may be part of the same valid chain but cannot satisfy a literal
    # repository selector for its terminal, even when all other fields agree.
    successor = {**source, "repo": "rrnewton/hermit", "record_id": "alias-opponent",
                 "corrects": source["record_id"]}
    write_events(tmp_path, _producer_selector_event(source, tmp_path),
                 _producer_selector_event(successor, tmp_path))
    request = {**_producer_selector_args(successor), "repo": "hermit"}
    with pytest.raises(ValueError, match="terminal differs from requested repo"):
        validate_rows.canonical_finalized_row(tmp_path, **request)


def test_producer_row_keeps_normal_event_correction_projection(tmp_path: Path):
    source = _producer_selector_row(tmp_path, gates=[{
        "name": "check.example", "result": "fail", "failure_origin": "outer_gate",
        "failed_substeps": None,
    }])
    original = _producer_selector_event(source, tmp_path)
    replacement = correction_for(original)
    path = write_events(tmp_path, original, replacement)
    before = path.read_bytes()

    selected = validate_rows.canonical_finalized_row(tmp_path, **_producer_selector_args(source))

    assert selected["record_id"] == source["record_id"]
    assert selected["gates"][0]["failed_substeps"] == []
    assert len(validate_rows.events(root=tmp_path)) == 2
    assert path.read_bytes() == before


@pytest.mark.parametrize("records", [
    [("producer-final", "absent")],                         # dangling singleton
    [("producer-final", "producer-final")],                 # self singleton
    [("producer-final", 7)],                                # non-null non-ID
    [("producer-final", "")],                               # empty predecessor
    [(None, None)],                                         # missing record identity
    [("producer-final", None), ("producer-final", None)],   # duplicate record identity
    [("first", None), ("producer-final", "missing-middle")],
    [("first", None), ("opponent", "first"), ("producer-final", "first")],
    [("left", "right"), ("right", "left")],                 # cycle, no terminal
    [("left", "right"), ("right", "left"), ("producer-final", None)],
    [("first", None), ("producer-final", None)],             # unrelated roots
])
def test_producer_row_refuses_entire_bad_group_before_identity_filter(tmp_path: Path, records):
    source = _producer_selector_row(tmp_path)
    events = []
    for index, (record_id, corrects) in enumerate(records):
        candidate = {**source, "record_id": record_id, "corrects": corrects,
                     "finished_at": f"2026-08-09T01:01:{index:02d}Z"}
        if record_id != source["record_id"]:
            candidate.update(commit="b" * 40, cwd=str(tmp_path / "other"),
                             started_at="2026-08-09T00:00:00Z")
        event = _producer_selector_event(candidate, tmp_path)
        # Distinct canonical events can contain conflicting compatibility IDs;
        # event IDs are not universally the projected HistoryRow record IDs.
        event["event_id"] = f"producer-event-{index}"
        events.append(event)
    path = write_events(tmp_path, *events)
    before = path.read_bytes()
    diagnostic = validate_rows.rows(root=tmp_path, preserve_admission=True)
    current = validate_rows.current_validation_run_rows(diagnostic)

    with pytest.raises(ValueError, match="canonical producer run"):
        validate_rows.canonical_finalized_row(tmp_path, **_producer_selector_args(source))

    assert validate_rows.rows(root=tmp_path, preserve_admission=True) == diagnostic
    assert validate_rows.current_validation_run_rows(diagnostic) == current
    if len(records) == 1:
        assert current == diagnostic  # diagnostic singleton behavior stays intact
    assert path.read_bytes() == before


@pytest.mark.parametrize("field,value", [
    ("repo", "reverie"), ("run_id", "missing-run"), ("record_id", "different-record"),
    ("commit", "b" * 40), ("cwd", "/different-checkout"),
    ("started_at", "2026-08-09T00:00:00Z"),
])
def test_producer_row_requires_every_exact_identity_field(tmp_path: Path, field: str, value: str):
    source = _producer_selector_row(tmp_path)
    write_events(tmp_path, _producer_selector_event(source, tmp_path))
    assert validate_rows.canonical_finalized_row(
        tmp_path, **_producer_selector_args(source)
    ) == source
    request = {**_producer_selector_args(source), field: value}
    expected_refusal = (
        "canonical producer run is absent" if field in {"repo", "run_id"}
        else f"canonical producer terminal differs from requested {field}"
    )

    with pytest.raises(ValueError, match=f"^{expected_refusal}$"):
        validate_rows.canonical_finalized_row(tmp_path, **request)


@pytest.mark.parametrize("defect", ["fixture", "unknown-envelope", "conflicting-event", "truncated"])
def test_producer_row_keeps_canonical_union_refusals(tmp_path: Path, defect: str):
    source = _producer_selector_row(tmp_path)
    if defect == "fixture":
        source["producer"] = "ledger-live-sync-acceptance"
    event = _producer_selector_event(source, tmp_path)
    if defect == "unknown-envelope":
        event["schema"] = "validate-ledger/v999"
    path = write_events(tmp_path, event)
    if defect == "conflicting-event":
        conflict = copy.deepcopy(event)
        conflict["legacy_row"]["result"] = "pass"
        path.write_text(path.read_text() + json.dumps(conflict) + "\n")
    if defect == "truncated":
        path.write_bytes(path.read_bytes().rstrip(b"\n"))
    before = path.read_bytes()

    error = {"conflicting-event": ledger.ConflictingEventId,
             "truncated": ledger.MalformedEvent}.get(defect, ValueError)
    with pytest.raises(error):
        validate_rows.canonical_finalized_row(tmp_path, **_producer_selector_args(source))

    assert path.read_bytes() == before


def test_producer_row_cli_preserves_claim_pairs_and_refuses_stale_identity(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys,
):
    source = _producer_selector_row(tmp_path)
    event = _producer_selector_event(source, tmp_path)
    # Present claim bytes belong to the downstream admission verifier. Selection
    # must not silently normalize an ambiguous claim into apparently good data.
    event["legacy_row"]["admission_floor_evidence"] = (
        validate_rows.qualifying_receipt.AdmissionJsonObject([
            ("schema_version", 1), ("context", {}), ("context", {"opponent": True}),
        ])
    )
    path = write_events(tmp_path, event)
    path.write_text(validate_rows.qualifying_receipt.admission_json_dumps(event) + "\n")
    monkeypatch.setattr(validate_rows, "repository_root", lambda: tmp_path)
    argv = ["producer-row"]
    for name, value in _producer_selector_args(source).items():
        argv.extend(["--" + name.replace("_", "-"), value])

    assert validate_rows.main(argv) == 0
    emitted = capsys.readouterr()
    assert emitted.err == ""
    decoded = json.loads(emitted.out,
                         object_pairs_hook=validate_rows.qualifying_receipt.AdmissionJsonObject)
    claim = decoded["admission_floor_evidence"]
    assert [value for key, value in claim.pairs if key == "context"] == [{}, {"opponent": True}]
    assert decoded["result"] == "fail" and decoded["passed_tests"] is None

    argv[argv.index("--record-id") + 1] = "stale-record"
    assert validate_rows.main(argv) == 2
    refused = capsys.readouterr()
    assert refused.out == ""
    assert "terminal differs from requested record_id" in refused.err
    assert "inspect canonical rows" in refused.err

    path.write_bytes(path.read_bytes().rstrip(b"\n"))
    assert validate_rows.main(argv) == 2
    malformed = capsys.readouterr()
    assert malformed.out == ""
    assert "truncated" in malformed.err


@pytest.mark.parametrize("flag", ["-h", "--help"])
def test_producer_row_cli_exposes_required_selectors(flag: str, capsys):
    with pytest.raises(SystemExit) as exit_status:
        validate_rows.main(["producer-row", flag])
    assert exit_status.value.code == 0
    help_text = capsys.readouterr().out
    for name in ("repo", "run-id", "record-id", "commit", "cwd", "started-at"):
        assert "--" + name in help_text
    with pytest.raises(SystemExit) as missing:
        validate_rows.main(["producer-row"])
    assert missing.value.code == 2


def test_enrichment_compatibility_rows_do_not_collapse_by_run_id(tmp_path: Path):
    source = row(tmp_path, record_id="enriched-run")
    original = validate_rows._event_for_row(source, tmp_path)
    enriched_once = copy.deepcopy(original["legacy_row"])
    enriched_once["compatibility_step"] = 1
    enriched_twice = copy.deepcopy(enriched_once)
    enriched_twice["compatibility_step"] = 2

    first = {
        **{key: original[key] for key in ledger.ENVELOPE_KEYS},
        "event_id": "enriched-run-1",
        "event_type": "run.enrich",
        "emitted_at": "2026-08-10T02:00:00Z",
        "enriches": original["event_id"],
        "legacy_row": enriched_once,
    }
    second = {
        **{key: original[key] for key in ledger.ENVELOPE_KEYS},
        "event_id": "enriched-run-2",
        "event_type": "run.enrich",
        "emitted_at": "2026-08-10T03:00:00Z",
        "enriches": first["event_id"],
        "legacy_row": enriched_twice,
    }
    write_events(tmp_path, original, first, second)

    compatibility_rows = validate_rows.rows(root=tmp_path)
    assert [item.get("compatibility_step") for item in compatibility_rows] == [
        None,
        1,
        2,
    ]


def test_commit_from_independent_repo_is_refused_before_spool(tmp_path: Path):
    repo_a = tmp_path / "repo-a"
    repo_b = tmp_path / "repo-b"
    commit_a = repository(repo_a, content="repo a\n")
    repository(repo_b, content="repo b\n")
    assert subprocess.run(
        ["git", "--no-replace-objects", "-C", str(repo_b), "cat-file", "-t", commit_a],
        check=False,
        capture_output=True,
    ).returncode != 0
    source = row(tmp_path)
    source["cwd"] = str(repo_b)
    source["commit"] = commit_a

    with pytest.raises(ValueError, match="not a commit object in row.cwd"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()
    assert validate_rows.events(root=tmp_path) == []
    assert validate_rows.rows(root=tmp_path) == []


def test_ambient_git_dir_cannot_redirect_commit_admission(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    repo_a = tmp_path / "ambient-repo-a"
    repo_b = tmp_path / "claimed-repo-b"
    commit_a = repository(repo_a, content="ambient repo a\n")
    repository(repo_b, content="claimed repo b\n")
    assert subprocess.run(
        ["git", "--no-replace-objects", "-C", str(repo_b), "cat-file", "-t", commit_a],
        check=False,
        capture_output=True,
    ).returncode != 0
    source = row(tmp_path, record_id="ambient-git-dir")
    source["cwd"] = str(repo_b)
    source["commit"] = commit_a
    monkeypatch.setenv("GIT_DIR", str(repo_a / ".git"))

    with pytest.raises(ValueError, match="not a commit object in row.cwd"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()
    assert validate_rows.events(root=tmp_path) == []
    assert validate_rows.rows(root=tmp_path) == []


@pytest.mark.parametrize(
    "state",
    [
        "compared-and-matched",
        "compared-and-diverged",
        "performs-no-comparison-by-design",
        "unavailable-with-reason",
    ],
)
def test_schema6_accepts_each_closed_cell_verdict(tmp_path: Path, state: str):
    source = with_cell_results(row(tmp_path), tmp_path, state)

    validate_rows.record(source, root=tmp_path)

    event = validate_rows.events(root=tmp_path)[0]
    assert event["event_id"] == source["record_id"]
    assert event["run_id"] == source["run_id"]
    assert validate_rows.rows(root=tmp_path) == [source]


def test_schema8_accepts_a_retained_subset_of_the_selected_path(tmp_path: Path, schema8_reader_policy):
    source = with_schema8_cell_results(
        row(tmp_path), tmp_path, path="quick", selected_count=2, recorded_count=1
    )

    validate_rows.record(source, root=tmp_path)

    [recorded] = validate_rows.rows(root=tmp_path)
    assert recorded["cell_results"]["path"] == "quick"
    assert recorded["cell_results"]["selected_count"] == 2
    assert recorded["cell_results"]["recorded_count"] == 1


def test_schema8_accepts_an_explicit_empty_selected_population(tmp_path: Path, schema8_reader_policy):
    source = with_schema8_cell_results(
        row(tmp_path), tmp_path, path="super", selected_count=0, recorded_count=0
    )

    validate_rows.record(source, root=tmp_path)

    [recorded] = validate_rows.rows(root=tmp_path)
    assert recorded["cell_results"]["path"] == "super"
    assert recorded["cell_results"]["selected"] == []
    assert recorded["cell_results"]["cells"] == []


def test_schema8_refuses_path_substitution_before_spool(tmp_path: Path, schema8_reader_policy):
    source = with_schema8_cell_results(row(tmp_path), tmp_path, path="quick")
    source["cell_results"]["path"] = "super"

    with pytest.raises(ValueError, match="cell_results path-mismatch"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_schema8_refuses_a_recorded_cell_outside_selected(tmp_path: Path, schema8_reader_policy):
    source = with_schema8_cell_results(
        row(tmp_path), tmp_path, path="quick", selected_count=2, recorded_count=1
    )
    source["cell_results"]["cells"][0]["test"] = "outside"

    with pytest.raises(ValueError, match="cell_results population-mismatch"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_schema7_refuses_the_schema8_path_field(tmp_path: Path, schema8_reader_policy):
    source = with_cell_results(row(tmp_path), tmp_path)
    source["schema_version"] = 7
    source["cell_results"]["path"] = "full"

    with pytest.raises(ValueError, match="cell_results malformed"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_raw_no_result_is_not_a_fifth_cell_verdict(tmp_path: Path):
    source = with_cell_results(row(tmp_path), tmp_path)
    source["cell_results"]["cells"][0]["cell_verdict"] = {
        "state": "no_result",
        "comparison_tier": "declared-but-unverifiable",
        "reason": "raw producer cause",
    }

    with pytest.raises(ValueError, match="cell-verdict-invalid"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_schema6_artifact_digest_mismatch_is_refused_before_spool(tmp_path: Path):
    source = with_cell_results(row(tmp_path), tmp_path)
    source["cell_results"]["artifact"]["sha256"] = "0" * 64

    with pytest.raises(ValueError, match="artifact sha256 mismatch"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


@pytest.mark.parametrize("prefix,expected_status", [
    ("", "legacy-unbound"), ("selected-attempt-v1/", "verified"),
])
@pytest.mark.parametrize("case", ["matched", "reference-diverged", "cross-diverged-then-matched"])
def test_schema10_ledger_roundtrip_retains_capability_and_original_failures(tmp_path, monkeypatch, prefix, expected_status, case):
    # Read the exact producer exports, then exercise the real native verifier
    # on both sides of the ordinary ledger event serialization. No parser mock.
    receipt = validate_rows.qualifying_receipt
    fixtures = HERE.parent.parent / "validate" / "tests" / "fixtures" / "schema10"
    source = json.loads((fixtures / f"{prefix}{case}-row.json").read_bytes())
    # Synthetic envelope only: this exercises event storage, not admission of
    # a production validation run or a claim that the fabricated SHA was built.
    source.update(
        repo="hermit", producer="synthetic-parser-control", host="fixture",
        cwd=str(tmp_path / "hermit"), started_at="2026-09-16T00:00:00Z",
        finished_at="2026-09-16T00:01:00Z",
        result="pass" if case == "matched" else "fail",
    )
    for reference, suffix in (
        (source["constructed_plan"], "plan.json"),
        (source["cell_results"]["artifact"], "cells.jsonl"),
        (source["test_results"]["artifact"], "tests.jsonl"),
    ):
        target = tmp_path / reference["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        raw = (fixtures / f"{prefix}{case}-{suffix}").read_bytes()
        assert hashlib.sha256(raw).hexdigest() == reference["sha256"]
        target.write_bytes(raw)
    predicate = copy.deepcopy(receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 10
    monkeypatch.setattr(receipt, "active", lambda: predicate)
    with receipt.validation_evidence_batch([source], artifact_root=tmp_path, predicate=predicate):
        checked = receipt.validation_evidence(source)
        assert checked.status == expected_status
        validate_rows._require_cell_results(source, tmp_path)
        observations = copy.deepcopy(checked.evidence["observations"])
    validate_rows._bind_comparison_failures(source, observations)
    event = validate_rows._event_for_row(source, tmp_path)
    write_events(tmp_path, event)
    restored = validate_rows.rows(root=tmp_path)
    assert len(restored) == 1
    restored = restored[0]
    assert restored["cell_results"] == source["cell_results"]
    assert restored["failed_cells"] == source["failed_cells"]
    assert restored["failed_comparisons"] == source["failed_comparisons"]
    assert len(restored["failed_comparisons"]) == {
        "matched": 0, "reference-diverged": 2, "cross-diverged-then-matched": 1,
    }[case]
    with receipt.validation_evidence_batch([restored], artifact_root=tmp_path, predicate=predicate):
        checked = receipt.validation_evidence(restored)
        assert checked.status == expected_status
        assert checked.evidence["observations"] == observations
        validate_rows._require_cell_results(restored, tmp_path)


def test_schema5_cannot_claim_cell_results(tmp_path: Path):
    source = with_cell_results(row(tmp_path), tmp_path)
    source["schema_version"] = 5

    with pytest.raises(ValueError, match="legacy-schema-claims-cell-results"):
        validate_rows.record(source, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_checkout_local_cell_artifact_is_refused(tmp_path: Path):
    source = with_cell_results(row(tmp_path), tmp_path)
    retained = tmp_path / source["cell_results"]["artifact"]["path"]
    checkout_local = Path(source["cwd"]) / "target" / "e2e-results.jsonl"
    checkout_local.parent.mkdir(parents=True, exist_ok=True)
    checkout_local.write_bytes(retained.read_bytes())
    source["cell_results"]["artifact"]["path"] = "worktrees/slot/hermit/target/e2e-results.jsonl"

    with pytest.raises(ValueError, match="not in retained ignored/validate/artifacts"):
        validate_rows.record(source, root=tmp_path)

    assert validate_rows.events(root=tmp_path) == []


def test_retained_cell_artifact_survives_checkout_removal(tmp_path: Path):
    source = with_cell_results(row(tmp_path), tmp_path)
    artifact_rel = source["cell_results"]["artifact"]["path"]
    expected_digest = source["cell_results"]["artifact"]["sha256"]
    validate_rows.record(source, root=tmp_path)

    shutil.rmtree(source["cwd"])

    artifact = validate_rows._artifact_path(tmp_path, artifact_rel)
    assert artifact.is_file()
    assert hashlib.sha256(artifact.read_bytes()).hexdigest() == expected_digest
    canonical = validate_rows.rows(root=tmp_path)
    assert len(canonical) == 1
    assert canonical[0]["cell_results"]["artifact"]["path"] == artifact_rel
    assert canonical[0]["cell_results"]["artifact"]["sha256"] == expected_digest


def test_same_sha_artifact_from_another_run_is_refused_before_spool(tmp_path: Path):
    run_a = with_cell_results(row(tmp_path, record_id="run-a"), tmp_path)
    run_b = row(tmp_path, record_id="run-b")
    assert run_a["commit"] == run_b["commit"]
    run_b["schema_version"] = 6
    run_b["run_id"] = "run-b"
    run_b["cell_results"] = run_a["cell_results"]

    with pytest.raises(ValueError, match="cell_results run-mismatch"):
        validate_rows.record(run_b, root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()
    assert validate_rows.events(root=tmp_path) == []
    assert validate_rows.rows(root=tmp_path) == []


def test_correction_record_preserves_distinct_stable_run_identity(tmp_path: Path):
    source = with_cell_results(
        row(tmp_path, record_id="correction-record"),
        tmp_path,
        run_id="original-run",
    )

    validate_rows.record(source, root=tmp_path)

    event = validate_rows.events(root=tmp_path)[0]
    assert event["event_id"] == "correction-record"
    assert event["run_id"] == "original-run"
    assert event["legacy_row"]["record_id"] == "correction-record"
    assert event["legacy_row"]["run_id"] == "original-run"
    assert event["legacy_row"]["cell_results"]["run_id"] == "original-run"


def test_every_published_machine_is_implicitly_unioned(tmp_path: Path):
    first = row(tmp_path, record_id="hosta-1-7", host="hosta")
    second = row(tmp_path, record_id="hostb-1-8", host="hostb")
    for source in (first, second):
        event = validate_rows._event_for_row(source, tmp_path)
        shard = (
            tmp_path
            / "ledger"
            / "hermit"
            / source["host"]
            / "2026-08.jsonl"
        )
        shard.parent.mkdir(parents=True)
        shard.write_text(json.dumps(event, sort_keys=True) + "\n")

    assert {item["host"] for item in validate_rows.rows(root=tmp_path)} == {
        "hosta",
        "hostb",
    }


def test_event_visible_in_spool_and_published_shard_collapses_to_one(tmp_path: Path):
    source = row(tmp_path)
    validate_rows.record(source, root=tmp_path)
    event = validate_rows._event_for_row(source, tmp_path)
    shard = tmp_path / "ledger/hermit/hosta/2026-08.jsonl"
    shard.parent.mkdir(parents=True)
    shard.write_text(json.dumps(event, sort_keys=True) + "\n")

    assert [item["event_id"] for item in validate_rows.events(root=tmp_path)] == [
        source["record_id"]
    ]
    assert validate_rows.rows(root=tmp_path) == [source]


def test_recovery_shard_is_part_of_the_same_canonical_union(tmp_path: Path):
    first = row(tmp_path, record_id="primary")
    second = row(tmp_path, record_id="preserved")
    primary = tmp_path / "ledger/hermit/hosta/2026-08.jsonl"
    recovery = tmp_path / "ledger-recovery/hermit/hosta/2026-08/base.jsonl"
    primary.parent.mkdir(parents=True)
    recovery.parent.mkdir(parents=True)
    primary.write_text(json.dumps(validate_rows._event_for_row(first, tmp_path), sort_keys=True) + "\n")
    recovery.write_text(
        json.dumps(validate_rows._event_for_row(first, tmp_path), sort_keys=True)
        + "\n"
        + json.dumps(validate_rows._event_for_row(second, tmp_path), sort_keys=True)
        + "\n"
    )

    assert {event["event_id"] for event in validate_rows.events(root=tmp_path)} == {
        "primary",
        "preserved",
    }


def acceptance_fixture(root: Path) -> dict:
    source = row(root, record_id="accept-0000")
    source["producer"] = "ledger-live-sync-acceptance"
    return source


def test_acceptance_fixture_is_not_canonical_but_real_validation_remains(
    tmp_path: Path,
):
    """Fixture rejection must not silently empty the real population."""
    fixture = acceptance_fixture(tmp_path)
    real = row(tmp_path, record_id="real-validation")
    shard = tmp_path / "ledger/hermit/hosta/2026-08.jsonl"
    shard.parent.mkdir(parents=True)
    shard.write_text(
        json.dumps(validate_rows._event_for_row(fixture, tmp_path), sort_keys=True)
        + "\n"
        + json.dumps(validate_rows._event_for_row(real, tmp_path), sort_keys=True)
        + "\n"
    )

    # Preserve the append-only bytes.  The authority view, not history, makes
    # the distinction between acceptance data and a validation observation.
    assert len(shard.read_text().splitlines()) == 2
    assert [event["event_id"] for event in validate_rows.events(root=tmp_path)] == [
        "real-validation"
    ]
    assert validate_rows.rows(root=tmp_path) == [real]


def test_production_record_refuses_acceptance_fixture_before_spool(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr(validate_rows, "repository_root", lambda: tmp_path)

    with pytest.raises(ValueError, match="acceptance fixture.*production ledger"):
        validate_rows.record(acceptance_fixture(tmp_path), root=tmp_path)

    assert not validate_rows.live_spool_dir(tmp_path).exists()


def test_explicit_parent_root_overrides_descriptor_backed_tool_checkout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    state_root = tmp_path / "state"
    state_root.mkdir()
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(state_root))

    assert validate_rows.repository_root() == state_root.resolve()


def test_isolated_fixture_store_does_not_enter_canonical_rows(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    production = tmp_path / "production"
    isolated = tmp_path / "fixture-store"
    monkeypatch.setattr(validate_rows, "repository_root", lambda: production)

    report = validate_rows.record(acceptance_fixture(isolated), root=isolated)

    assert (isolated / report["spool"]).is_file()
    assert validate_rows.events(root=isolated) == []
    assert validate_rows.rows(root=isolated) == []


def test_ignored_ephemeral_checkout_is_redacted_without_weakening_guard(
    tmp_path: Path,
):
    source = row(tmp_path)
    source["cwd"] = "/home/test/work/dev-hermit/ignored/validate-fresh-abc"
    source["first_error_line"] = "/home/test/.cargo/git/checkouts/reverie/file.rs:7"

    event = validate_rows._event_for_row(source, tmp_path)

    assert event["legacy_row"]["cwd"] == (
        "{{WORKSPACE_ROOT}}/ignored/validate-fresh-abc"
    )
    assert event["legacy_row"]["first_error_line"] == (
        "{{HOME}}/.cargo/git/checkouts/reverie/file.rs:7"
    )
    assert "/home/" not in json.dumps(event)


def test_linked_worktree_cli_filters_exact_source_row_and_refuses_ambiguity(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    primary = tmp_path / "dev-hermit"
    repository(primary)
    (primary / ".gitignore").write_text("/ignored/\n/worktrees/\n")
    fixture_files = (
        Path("ci-hub/ledger/ledger.py"),
        Path("ci-hub/ledger/publisher.py"),
        Path("ci-hub/ledger/validate_rows.py"),
        # publisher.py imports ci-hub/lib/bounded_capture, so the fixture tree
        # has to carry it or the copied script dies on import instead of
        # exercising the behaviour under test.
        Path("ci-hub/lib/bounded_capture.py"),
        # series.py resolves its checker compiler through this existing helper.
        Path("ci-hub/lib/rust_toolchain.py"),
        Path("ci-hub/qualifying_receipt.py"),
        Path("ci-hub/admission_paths.py"),
        Path("ci-hub/series/series.py"),
        Path("ci-hub/validate/flake_class.py"),
        Path("ci-hub/validate/flaky_cells.py"),
        Path("ci-hub/validate/gate_completeness.py"),
        Path("ci-hub/validate/green_class.py"),
        Path("ci-hub/validate/qualifying-receipt.json"),
    )
    repository_root = HERE.parents[2]
    monkeypatch.setenv(
        "CI_HUB_LEDGER_EVENT_CHECKER",
        str(repository_root / "ci-hub/ledger/check-ledger-events.rs"),
    )
    for relative in fixture_files:
        destination = primary / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(repository_root / relative, destination)
    git(primary, "add", ".gitignore", *(str(path) for path in fixture_files))
    git(primary, "commit", "-qm", "add production ledger fixture")

    linked = primary / "worktrees/slot/dev-hermit"
    git(primary, "worktree", "add", "--detach", str(linked), "HEAD")

    log = primary / "ignored/validate/reverie.log"
    started = datetime(2026, 8, 24, 14, 41, 55, tzinfo=timezone.utc)
    source = {
        "schema_version": 5,
        "record_id": "linked-worktree-row",
        "repo": "reverie",
        "producer": "reverie-validate-sh",
        "host": "hosta",
        "started_at": "2026-08-24T14:41:55Z",
        "finished_at": "2026-08-24T14:45:47Z",
        "result": "pass",
        "commit": git(linked, "rev-parse", "HEAD").stdout.strip(),
        "cwd": str(linked),
        "log_file": str(log),
    }

    decoys = [
        {**source, "record_id": "wrong-repo", "repo": "hermit"},
        {**source, "record_id": "wrong-path", "cwd": str(primary)},
        {
            **source,
            "record_id": "wrong-log",
            "log_file": str(primary / "ignored/validate/other.log"),
        },
        {
            **source,
            "record_id": "too-old",
            "started_at": "2026-08-24T14:41:54Z",
        },
    ]
    ledger_tool = linked / "ci-hub/ledger/validate_rows.py"

    def ledger_cli(
        command: str, item: dict | None = None
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["CI_HUB_LEDGER_EVENT_CHECKER"] = str(
            repository_root / "ci-hub/ledger/check-ledger-events.rs"
        )
        return subprocess.run(
            [sys.executable, str(ledger_tool), command],
            cwd=linked,
            input=(json.dumps(item, sort_keys=True) + "\n" if item else None),
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    for item in [*decoys, source]:
        result = ledger_cli("record", item)
        assert result.returncode == 0, result.stderr

    assert not (linked / validate_rows.LIVE_SPOOL_RELATIVE).exists()
    assert len(list((primary / validate_rows.LIVE_SPOOL_RELATIVE).glob("*.jsonl"))) == 5
    listed = ledger_cli("rows")
    assert listed.returncode == 0, listed.stderr
    assert {json.loads(line)["record_id"] for line in listed.stdout.splitlines()} == {
        source["record_id"],
        *(item["record_id"] for item in decoys),
    }

    selected, selected_event = reverie_node.load_source_row(
        linked,
        source["commit"],
        linked,
        log,
        started,
    )
    assert selected == source
    assert selected_event["event_id"] == source["record_id"]
    assert selected_event in validate_rows.events(root=primary)

    duplicate = {**source, "record_id": "second-exact-match"}
    result = ledger_cli("record", duplicate)
    assert result.returncode == 0, result.stderr
    with pytest.raises(reverie_node.Refusal, match="found 2"):
        reverie_node.load_source_row(
            linked,
            source["commit"],
            linked,
            log,
            started,
        )


@pytest.mark.parametrize("field", ["finished_at"])
def test_incomplete_row_is_refused_without_creating_an_alternate_ledger(
    tmp_path: Path, field: str
):
    source = row(tmp_path)
    del source[field]
    with pytest.raises(ValueError):
        validate_rows.record(source, root=tmp_path)
    assert not (tmp_path / "ledger").exists()


def failed_row(
    root: Path,
    *,
    record_id: str,
    jobs: int,
    concurrent: int,
    node: str = "e2e.manifest_c_programs",
) -> dict:
    source = row(root, record_id=record_id)
    log = root / "ignored" / "validate" / f"{record_id}.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    log.write_text(
        f"[{node}] ✗ FAIL   fixture failure\n"
        f"[{node}] error: deterministic fixture failure\n"
    )
    source.update(
        {
            "result": "fail",
            "raw_result": "fail",
            "exit_code": 1,
            "checks": 28,
            "failures": 1,
            "gates_run": 28,
            "gates_expected": None,
            "executed_tests": 271,
            "filtered_tests": 2,
            "dag_jobs": jobs,
            "concurrent_validates": concurrent,
            "log_file": str(log),
            "gates": [
                {
                    "name": node,
                    "result": "fail",
                    "exit_code": 1,
                    "aborted": False,
                    "real_seconds": 117.0,
                },
                {
                    "name": "test.aborted",
                    "result": "fail",
                    "exit_code": -15,
                    "aborted": True,
                    "reason": "eager-exit after another step failed",
                },
            ],
        }
    )
    return source


def test_failed_solo_row_carries_durable_failure_classification(tmp_path: Path):
    source = failed_row(
        tmp_path, record_id="solo-failure", jobs=4, concurrent=0
    )

    validate_rows.record(source, root=tmp_path)
    [recorded] = validate_rows.rows(root=tmp_path)

    assert recorded["known_flaky_failure"] is False
    assert recorded["solo_rerun_confirmation"] is False
    assert recorded["failed_substeps"] == ["e2e.manifest_c_programs"]
    assert recorded["first_error_line"] == "error: deterministic fixture failure"
    assert recorded["gates"][0]["failure_origin"] == "outer_gate"
    assert recorded["gates"][0]["failed_substeps"] == []
    assert "failure_origin" not in recorded["gates"][1]


def test_typed_gate_class_survives_without_the_log(tmp_path: Path):
    source = failed_row(
        tmp_path, record_id="typed-class-no-log", jobs=4, concurrent=0
    )
    Path(source["log_file"]).unlink()
    source["gates"][0]["failure_class"] = "understood_infrastructure_failure"
    source["gates"][0]["failure_detail"] = "failed to verify the checksum"

    validate_rows.record(source, root=tmp_path)
    [recorded] = validate_rows.rows(root=tmp_path)

    [classification] = recorded["failed_substep_classes"]
    assert classification["node"] == "e2e.manifest_c_programs"
    assert classification["failure_class"] == "understood_infrastructure_failure"
    assert classification["fault_class"] == "infrastructure"
    assert classification["infra_signature"] == "failed to verify the checksum"
    assert classification["first_error_line"] is None


def test_contended_row_carries_evidence_but_still_requires_solo_confirmation(
    tmp_path: Path,
):
    source = failed_row(
        tmp_path, record_id="contended-failure", jobs=16, concurrent=1
    )

    validate_rows.record(source, root=tmp_path)
    [recorded] = validate_rows.rows(root=tmp_path)

    assert recorded["known_flaky_failure"] is False
    assert recorded["solo_rerun_confirmation"] is False
    assert recorded["dag_jobs"] == 16
    assert recorded["concurrent_validates"] == 1


def test_failed_schema6_row_persists_exact_compared_cell_id(tmp_path: Path):
    source = with_cell_results(
        failed_row(tmp_path, record_id="exact-cell-failure", jobs=4, concurrent=0),
        tmp_path,
        "compared-and-diverged",
    )

    validate_rows.record(source, root=tmp_path)
    [recorded] = validate_rows.rows(root=tmp_path)

    assert recorded["failed_cells"] == ["hello/verify/ptrace"]
    assert recorded["flaky_failed_cells"] == []
    assert recorded["known_flaky_failure"] is False


def test_failed_schema6_row_matches_only_exact_tree_registry(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    source = with_cell_results(
        failed_row(tmp_path, record_id="exact-flaky-cell", jobs=4, concurrent=0),
        tmp_path,
        "compared-and-diverged",
    )
    registry = tmp_path / "flaky-cells.json"
    registry.write_text(json.dumps({
        "schema": 2,
        "cells": [{
            "cell": "hello/verify/ptrace",
            "product": "hermit",
            "tree": source["commit"],
            "observed_pass": 1,
            "observed_fail": 1,
            "sample_size": 2,
            "measured_at": "2026-08-27",
            "source": "series",
            "producers": ["validate"],
        }],
    }))
    import failure_evidence
    monkeypatch.setattr(failure_evidence, "DEFAULT_REGISTRY", registry)

    validate_rows.record(source, root=tmp_path)
    [recorded] = validate_rows.rows(root=tmp_path)

    assert recorded["failed_cells"] == ["hello/verify/ptrace"]
    assert recorded["flaky_failed_cells"] == ["hello/verify/ptrace"]
    assert recorded["known_flaky_failure"] is True


def test_failed_schema6_row_does_not_turn_no_result_into_a_failed_cell(
    tmp_path: Path,
):
    source = with_cell_results(
        failed_row(tmp_path, record_id="no-result-cell", jobs=4, concurrent=0),
        tmp_path,
        "unavailable-with-reason",
    )

    validate_rows.record(source, root=tmp_path)
    [recorded] = validate_rows.rows(root=tmp_path)

    assert recorded["failed_cells"] == []
    assert recorded["flaky_failed_cells"] == []
    assert recorded["known_flaky_failure"] is False


def test_schema6_failed_cell_survives_when_the_failure_log_is_missing(tmp_path: Path):
    source = with_cell_results(
        failed_row(tmp_path, record_id="missing-log-exact-cell", jobs=4, concurrent=0),
        tmp_path,
        "compared-and-diverged",
    )
    Path(source["log_file"]).unlink()

    validate_rows.record(source, root=tmp_path)
    [recorded] = validate_rows.rows(root=tmp_path)

    assert recorded["failed_cells"] == ["hello/verify/ptrace"]
    assert "flaky_failed_cells" not in recorded
    assert "known_flaky_failure" not in recorded


def test_schema6_cannot_override_the_exact_failed_cell_ids(tmp_path: Path):
    source = with_cell_results(
        failed_row(tmp_path, record_id="false-failed-cell", jobs=4, concurrent=0),
        tmp_path,
        "compared-and-diverged",
    )
    source["failed_cells"] = ["applications/not-the-measured-cell/verify/ptrace"]

    with pytest.raises(ValueError, match="failed_cells differ"):
        validate_rows.record(source, root=tmp_path)

    assert validate_rows.events(root=tmp_path) == []


def test_missing_failure_log_stays_unclassified_and_cannot_look_durable(
    tmp_path: Path,
):
    source = failed_row(
        tmp_path, record_id="missing-log", jobs=4, concurrent=0
    )
    Path(source["log_file"]).unlink()

    validate_rows.record(source, root=tmp_path)
    [recorded] = validate_rows.rows(root=tmp_path)

    assert "known_flaky_failure" not in recorded
    assert "failure_origin" not in recorded["gates"][0]


def reverie_finalization_fixture(root: Path, *, run_number: int = 153) -> tuple[dict, dict]:
    source = row(root, record_id=f"source-{run_number}")
    source.update(
        schema_version=4, producer="reverie-validate-sh", repo="reverie",
        run_number=run_number, log_file=str(root / f"run-{run_number}.log"),
        exit_code=1, result="fail", checks=1, failures=1,
        executed_tests=10, passed_tests=9, filtered_tests=2,
        gates=[dict(name="Test regular workspace cases", result="fail",
                    executed_tests=10, passed_tests=9, filtered_tests=2,
                    failure_origin="outer_gate", failed_substeps=[])],
    )
    Path(source["log_file"]).write_text("one real fixture failure\n")
    validate_rows.record(source, root=root)
    event = next(x for x in validate_rows.events(root=root) if x["event_id"] == source["record_id"])
    final = {**source, "schema_version": 5, "producer": "ci-hub-reverie-validate-run",
             "repo": "rrnewton/reverie", "raw_result": "fail", "gates_run": 1,
             "tree": git(Path(source["cwd"]), "rev-parse", "HEAD^{tree}").stdout.strip(),
             "safe_ci": {"required": True, "observed": True, "cgroup": "/fixture"}}
    return event, final


def count_handoff_fixture(root: Path) -> tuple[Path, dict, dict, dict]:
    """Normal writer/query data fixture with a real Git object store.

    Constrained hosts may reuse an explicitly supplied, already materialized
    Reverie checkout through a disposable alias. No Git metadata or object is
    written in that case; both routes exercise the same cwd normalization and
    real commit/tree checks. No executed guest is claimed by this fixture.
    """
    sys.path.insert(0, str(HERE.parent.parent / "validate/tests"))
    from test_reverie_validate import count_transport_fixture
    import run_registry

    alias = root / "worktrees" / "counted" / "reverie"
    alias.parent.mkdir(parents=True)
    existing = os.environ.get("REVERIE_COUNT_TEST_CHECKOUT")
    if existing is not None:
        checkout = Path(existing).resolve(strict=True)
        if not checkout.is_dir():
            raise ValueError("existing count fixture checkout is not a directory")
        alias.symlink_to(checkout, target_is_directory=True)
        target = git(checkout, "rev-parse", "HEAD^{commit}").stdout.strip()
    else:
        target = repository(alias)
    tree = git(alias, "rev-parse", target + "^{tree}").stdout.strip()
    path, handle, template = count_transport_fixture(root)
    handle.update(target=target, checkout=str(alias), source_checkout=str(alias))
    original = copy.deepcopy(template["canonical"]["source"]["legacy_row"])
    original.update(commit=target, cwd=str(alias))
    # The canonical event envelope requires UTC's Z spelling; preserve instants.
    for field in ("started_at", "finished_at"):
        original[field] = original[field].replace("+00:00", "Z")
    Path(original["log_file"]).write_text("fixture: one failed framework execution\n")
    raw_receipt = validate_rows.record(original, root=root)
    source_reply = validate_rows.reverie_source(target, str(alias), handle["log"],
                                              original["started_at"], root=root)
    source = source_reply["event"]
    assert source["event_id"] == raw_receipt["event_id"]
    # Keep the transport fixture's real owner-proof shape when binding it to
    # this temporary Git object. A holder-only stub is not admission authority.
    authority = copy.deepcopy(template["canonical"]["row"]["validate_lock_authority"])
    authority["holder"]["target"] = target
    final = {**source_reply["row"], "schema_version": 5, "producer": "ci-hub-reverie-validate-run",
             "repo": "rrnewton/reverie", "tree": tree, "raw_result": "fail", "gates_run": 8,
             "gates_expected": 8, "reverie_validation_policy": "reverie-local-validation/v5",
             "coverage": template["canonical"]["row"]["coverage"],
             "safe_ci": copy.deepcopy(template["canonical"]["row"]["safe_ci"]),
             "validate_lock_authority": authority}
    receipt = validate_rows.record_reverie_finalization(source, final, root=root)
    canonical = validate_rows.reverie_finalization(source["event_id"], receipt["event_id"], receipt["run_id"], root=root)
    assert canonical["row"]["cwd"] == str(alias)
    assert Path(canonical["row"]["cwd"]).resolve() == alias.resolve()
    binding = {**template, "context": reverie_node.count_context(handle, path),
               "source_id": source["event_id"], "final_id": receipt["event_id"], "run_id": receipt["run_id"],
               "canonical": canonical, "canonical_sha256": hashlib.sha256(reverie_node.count_json(canonical)).hexdigest()}
    binding["execution"]["authority"] = copy.deepcopy(authority)
    run_registry.create_record(path, handle)
    reverie_node.publish_count_sidecar(reverie_node.count_sidecar_path(path), binding)
    return path, handle, binding, receipt


def test_reverie_count_real_writer_reader_registry_preserves_failure(tmp_path: Path):
    import run_registry
    path, handle, binding, receipt = count_handoff_fixture(tmp_path)
    code_root = HERE.parents[2]
    evidence = reverie_node.consume_count_handoff(code_root, tmp_path, path, handle)
    result = run_registry.update_reverie_counts(path, handle, evidence=evidence)
    assert (result["executed_tests"], result["passed_tests"]) == (20, 19)
    assert result["exit_code"] == 1 and result["wrapper_exit_code"] == 4
    assert result["final_validate_status"] == "FAILED" and result["canonical_verdict"] == "NEEDS-RERUN"
    assert result.get("executed_nodes") is None
    assert run_registry.update_reverie_counts(path, handle, evidence=evidence) == result
    replay = validate_rows.record_reverie_finalization(binding["canonical"]["source"], binding["canonical"]["row"], root=tmp_path)
    assert replay["event_id"] == receipt["event_id"] and replay["already_recorded"] is True
    assert len(validate_rows.events(root=tmp_path)) == 2


def test_reverie_count_exact_query_refuses_missing_wrong_and_moving_chain(tmp_path: Path):
    path, handle, binding, _receipt = count_handoff_fixture(tmp_path)
    for source_id, final_id, run_id in (("absent", binding["final_id"], binding["run_id"]),
                                       (binding["source_id"], "absent", binding["run_id"]),
                                       (binding["source_id"], binding["final_id"], "other-run"),
                                       (binding["final_id"], binding["source_id"], binding["run_id"])):
        with pytest.raises(ValueError):
            validate_rows.reverie_finalization(source_id, final_id, run_id, root=tmp_path)
    # A later, intrinsically valid correction must invalidate the old handoff.
    later = copy.deepcopy(binding["canonical"]["final"])
    later.update(event_id="later-count", supersedes=binding["final_id"], reason="opposing later correction")
    validate_rows._spool_record_event(later, tmp_path)
    with pytest.raises(ValueError, match="moving chain"):
        validate_rows.reverie_finalization(binding["source_id"], binding["final_id"], binding["run_id"], root=tmp_path)
    with pytest.raises(reverie_node.Refusal, match="unavailable"):
        reverie_node.consume_count_handoff(HERE.parents[2], tmp_path, path, handle)


def test_reverie_count_query_refuses_conflicting_canonical_union(tmp_path: Path):
    _path, _handle, binding, _receipt = count_handoff_fixture(tmp_path)
    conflicting = copy.deepcopy(binding["canonical"]["final"])
    conflicting["legacy_row"]["tree"] = "d" * 40
    # Use the actual shard reader's conflicting-ID contract, not a mocked query.
    shard = tmp_path / "ledger/reverie/count-test/2026-09.jsonl"
    shard.parent.mkdir(parents=True)
    shard.write_text(json.dumps(conflicting) + "\n")
    with pytest.raises(ledger.ConflictingEventId) as refusal:
        validate_rows.reverie_finalization(binding["source_id"], binding["final_id"], binding["run_id"], root=tmp_path)
    assert str(refusal.value) == (
        f"event_id {binding['final_id']!r} appears twice with different bodies; "
        "refusing last-writer-wins (§5.2)"
    )


def test_reverie_finalization_preserves_one_failed_run_and_raw_source(tmp_path: Path):
    event, final = reverie_finalization_fixture(tmp_path)
    spool_dir = validate_rows.live_spool_dir(tmp_path)
    original_files = {p: p.read_bytes() for p in spool_dir.glob("*.jsonl")}
    receipt = validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    assert receipt["run_id"] == event["run_id"]
    events = validate_rows.events(root=tmp_path)
    assert len(events) == 2 and event in events
    correction = next(e for e in events if e["event_type"] == "run.correct")
    assert correction["supersedes"] == event["event_id"]
    assert correction["run_id"] == event["run_id"]
    projected = validate_rows.status_input(root=tmp_path)
    assert len(projected["rows"]) == 1
    assert len(projected["records"]) == 2
    current = projected["rows"][0]
    assert current["result"] == current["raw_result"] == "fail"
    assert (current["executed_tests"], current["passed_tests"], current["filtered_tests"]) == (10, 9, 2)
    assert current["gates"] == final["gates"]
    assert all(p.read_bytes() == raw for p, raw in original_files.items())
    replay = validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    assert replay["already_recorded"] is True
    assert replay["event_id"] == receipt["event_id"]
    assert len(list(spool_dir.glob("*.jsonl"))) == 2


def test_reverie_same_time_same_commit_runs_stay_distinct(tmp_path: Path):
    first, first_final = reverie_finalization_fixture(tmp_path, run_number=153)
    second, second_final = reverie_finalization_fixture(tmp_path, run_number=154)
    assert first["commit"] == second["commit"]
    assert first["host"] == second["host"]
    assert first["started_at"] == second["started_at"]
    for event, final in [(first, first_final), (second, second_final)]:
        validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    projected = validate_rows.status_input(root=tmp_path)
    assert len(projected["records"]) == 4
    assert {r["run_number"] for r in projected["rows"]} == {153, 154}
    assert len(projected["rows"]) == 2


@pytest.mark.parametrize("field,value", [
    ("commit", "b" * 40), ("host", "foreign"), ("run_number", 154),
    ("log_file", "/foreign.log"), ("cwd", "/missing-checkout"),
    ("started_at", "2026-08-09T01:00:01Z"), ("finished_at", "2026-08-09T01:01:01Z"),
    ("result", "pass"), ("raw_result", "pass"), ("exit_code", 0),
    ("executed_tests", 9), ("passed_tests", 10), ("filtered_tests", 0),
    ("failures", True), ("gates", []), ("repo", "hermit"),
    ("producer", "other"), ("schema_version", 6), ("new_fact", "unsupported"),
    ("tree", "f" * 40),
])
def test_reverie_finalization_refuses_execution_changes(tmp_path: Path, field: str, value):
    event, final = reverie_finalization_fixture(tmp_path)
    final[field] = value
    with pytest.raises(ValueError):
        validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    assert validate_rows.events(root=tmp_path) == [event]


@pytest.mark.parametrize("field", ["event_id", "run_id", "host", "legacy_row"])
def test_reverie_finalization_binds_exact_retained_source(tmp_path: Path, field: str):
    event, final = reverie_finalization_fixture(tmp_path)
    supplied = copy.deepcopy(event)
    supplied[field] = {} if field == "legacy_row" else "forged"
    with pytest.raises(ValueError, match="does not match the retained event"):
        validate_rows.record_reverie_finalization(supplied, final, root=tmp_path)
    assert validate_rows.events(root=tmp_path) == [event]


def test_reverie_conflicting_finalization_refuses_before_spooling(tmp_path: Path):
    event, final = reverie_finalization_fixture(tmp_path)
    validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    retained = validate_rows.events(root=tmp_path)
    final["safe_ci"]["cgroup"] = "/different-finalizer"
    with pytest.raises(ValueError, match="different finalization"):
        validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    assert validate_rows.events(root=tmp_path) == retained


def test_reverie_finalization_keeps_ordinary_admission_refusal(tmp_path: Path):
    event, final = reverie_finalization_fixture(tmp_path)
    final["admission_floor_evidence"] = {"schema_version": 999}
    with pytest.raises(ValueError):
        validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    assert validate_rows.events(root=tmp_path) == [event]


def test_reverie_finalization_keeps_ordinary_failed_gate_refusal(tmp_path: Path):
    event, final = reverie_finalization_fixture(tmp_path)
    final["gates"] = copy.deepcopy(final["gates"])
    final["gates"][0]["failed_substeps"] = None
    with pytest.raises(ValueError, match="inconsistent failure_origin"):
        validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    assert validate_rows.events(root=tmp_path) == [event]


def test_reverie_finalization_busy_source_refuses_without_history_change(tmp_path: Path):
    import fcntl
    event, final = reverie_finalization_fixture(tmp_path)
    directory = validate_rows.live_spool_dir(tmp_path) / ".reverie-finalization"
    directory.mkdir()
    guard = directory / (hashlib.sha256(event["event_id"].encode()).hexdigest() + ".lock")
    with guard.open("a") as owner:
        fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with pytest.raises(ValueError, match="already in progress"):
            validate_rows.record_reverie_finalization(event, final, root=tmp_path)
    assert validate_rows.events(root=tmp_path) == [event]


def test_reverie_finalization_cannot_promote_reconstructed_source(tmp_path: Path):
    fixture_root = tmp_path / "original"
    event, final = reverie_finalization_fixture(fixture_root)
    reconstructed = copy.deepcopy(event)
    reconstructed["producer"]["source"] = "reconstructed"
    replay_root = tmp_path / "reconstructed"
    write_events(replay_root, reconstructed)
    with pytest.raises(ValueError, match="not a raw Reverie producer result"):
        validate_rows.record_reverie_finalization(reconstructed, final, root=replay_root)
    assert validate_rows.events(root=replay_root) == [reconstructed]


def test_reverie_competing_writers_hold_guard_through_append(tmp_path: Path):
    import select

    event, final = reverie_finalization_fixture(tmp_path)
    competing = copy.deepcopy(final)
    competing["safe_ci"]["cgroup"] = "/other-finalizer"
    ready_read, ready_write = os.pipe()
    release_read, release_write = os.pipe()
    worker = r'''
import json,os,sys
from pathlib import Path
sys.path.insert(0,sys.argv[1])
import validate_rows
real_append=validate_rows._spool_record_event
def held_append(event,root):
    # The actual finalizer has already read the source and admitted no successor.
    # Hold precisely its last boundary before the real durable append.
    os.write(int(sys.argv[3]),b'L')
    assert os.read(int(sys.argv[4]),1)==b'G'
    return real_append(event,root)
validate_rows._spool_record_event=held_append
request=json.load(sys.stdin)
print(json.dumps(validate_rows.record_reverie_finalization(request['source'],request['row'],root=Path(sys.argv[2]))))
'''
    environment = {**os.environ, "DEV_HERMIT_PARENT": str(tmp_path)}
    first = subprocess.Popen(
        [sys.executable, "-c", worker, str(HERE.parent), str(tmp_path), str(ready_write), str(release_read)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, env=environment, pass_fds=(ready_write, release_read),
    )
    os.close(ready_write)
    os.close(release_read)
    try:
        assert first.stdin is not None
        first.stdin.write(json.dumps({"source": event, "row": final}))
        first.stdin.close()
        first.stdin = None
        assert select.select([ready_read], [], [], 20)[0], "first writer did not reach append"
        assert os.read(ready_read, 1) == b"L"
        argv = [sys.executable, str(HERE.parent / "validate_rows.py"), "record-reverie-finalization"]
        request = json.dumps({"source": event, "row": competing})
        second = subprocess.run(argv, input=request, text=True, capture_output=True, env=environment, timeout=20)
        assert second.returncode == 2
        assert "already in progress" in second.stderr
        assert validate_rows.events(root=tmp_path) == [event]
        os.write(release_write, b"G")
        stdout, stderr = first.communicate(timeout=20)
        assert first.returncode == 0, stderr
        assert json.loads(stdout)["run_id"] == event["run_id"]
        after = subprocess.run(argv, input=request, text=True, capture_output=True, env=environment, timeout=20)
        assert after.returncode == 2
        assert "different finalization" in after.stderr
        retained = validate_rows.events(root=tmp_path)
        assert len(retained) == 2 and event in retained
        current = validate_rows.rows(root=tmp_path)
        assert len(current) == 1 and current[0]["safe_ci"]["cgroup"] == "/fixture"
    finally:
        os.close(ready_read)
        os.close(release_write)
        if first.poll() is None:
            first.terminate()
            first.communicate(timeout=20)


# The two lost runs the retained-evidence quarantine allowlist names, read from
# the ledger shard it cites so the fixture cannot drift from the real events.
QUARANTINE_SHARD = HERE.parents[2] / "ledger" / "hermit" / "devbig014" / "2026-09.jsonl"


def quarantine_lost_events() -> list[dict]:
    allowlist = validate_rows.qualifying_receipt.retained_evidence_quarantine_allowlist()
    events = [
        json.loads(line)
        for line in QUARANTINE_SHARD.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    lost = [event for event in events if event.get("event_id") in allowlist]
    assert sorted(event["event_id"] for event in lost) == sorted(allowlist) == [
        "devbig014-1790052459-997811",
        "devbig014-1790052466-983899",
    ]
    return lost


def quarantine_correction(lost: dict, marker: dict | None = None) -> dict:
    receipt = validate_rows.qualifying_receipt
    if marker is None:
        marker = receipt.retained_evidence_quarantine_allowlist()[lost["event_id"]]["marker"]
    corrected = copy.deepcopy(lost["legacy_row"])
    corrected.update(
        result="no_result",
        source="reconstructed",
        **{receipt.RETAINED_EVIDENCE_QUARANTINE_KEY: copy.deepcopy(marker)},
    )
    return {
        "schema": "validate-ledger/v1",
        "event_id": f"{lost['event_id']}-quarantine",
        "event_type": "run.correct",
        "run_id": lost["run_id"],
        "emitted_at": "2026-09-26T00:00:00Z",
        "team": lost["team"],
        "host": lost["host"],
        "producer": {
            "source": "reconstructed",
            "tool": "validate_rows.py",
            "tool_version": "1",
        },
        "supersedes": lost["event_id"],
        "reason": "retained evidence lost",
        "legacy_row": corrected,
    }


def test_allowlisted_quarantine_corrections_project_as_non_green_rows(tmp_path: Path):
    lost = quarantine_lost_events()
    # One root at a time, so workspace-relative paths expand identically.
    write_events(tmp_path, *lost)
    unquarantined = validate_rows.rows(root=tmp_path)
    write_events(tmp_path, *lost, *(quarantine_correction(event) for event in lost))
    projected = validate_rows.rows(root=tmp_path)
    assert [row["record_id"] for row in projected] == [event["event_id"] for event in lost]
    key = validate_rows.qualifying_receipt.RETAINED_EVIDENCE_QUARANTINE_KEY
    for row, before, event in zip(projected, unquarantined, lost):
        assert before["result"] == before["raw_result"] == "pass"
        assert (row["result"], row["raw_result"], row["source"]) == (
            "no_result", "pass", "reconstructed",
        )
        assert row[key]["original_event_id"] == event["event_id"]
        # Everything but the three corrected fields is the lost row's projection.
        corrected = {"result", "source", key}
        assert {k: v for k, v in row.items() if k not in corrected} == {
            k: v for k, v in before.items() if k not in corrected
        }


def test_tracked_shard_carries_exactly_the_canonical_quarantine_corrections(tmp_path: Path):
    # The real shard holds one appended run.correct per lost run, equal to the
    # canonical correction apart from when it was emitted, and projecting that
    # whole shard quarantines exactly the two lost runs.
    lost = quarantine_lost_events()
    events = [
        json.loads(line)
        for line in QUARANTINE_SHARD.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    receipt = validate_rows.qualifying_receipt
    for event in lost:
        appended = [e for e in events if e.get("supersedes") == event["event_id"]]
        assert len(appended) == 1
        expected = quarantine_correction(event)
        assert {k: v for k, v in appended[0].items() if k != "emitted_at"} == {
            k: v for k, v in expected.items() if k != "emitted_at"
        }
        assert appended[0]["emitted_at"] > event["emitted_at"]
    copy_path = tmp_path / "ledger" / "hermit" / "devbig014" / "2026-09.jsonl"
    copy_path.parent.mkdir(parents=True)
    copy_path.write_bytes(QUARANTINE_SHARD.read_bytes())
    projected = validate_rows.rows(root=tmp_path)
    quarantined = [row for row in projected if receipt.RETAINED_EVIDENCE_QUARANTINE_KEY in row]
    assert sorted(row["record_id"] for row in quarantined) == sorted(e["event_id"] for e in lost)
    for row in quarantined:
        assert (row["result"], row["raw_result"], row["source"]) == (
            "no_result", "pass", "reconstructed",
        )
        assert receipt.retained_evidence_quarantine_entry(row) is not None


def _quarantine_variant(name: str, lost: list[dict]) -> list[dict]:
    receipt = validate_rows.qualifying_receipt
    key = receipt.RETAINED_EVIDENCE_QUARANTINE_KEY
    first, second = lost
    correction = quarantine_correction(first)
    if name == "self-authored-marker":
        marker = copy.deepcopy(correction["legacy_row"][key])
        marker["expected"]["constructed_plan"]["bytes"] += 1
        correction = quarantine_correction(first, marker)
    elif name == "other-entry-marker":
        correction = quarantine_correction(
            first, receipt.retained_evidence_quarantine_allowlist()[second["event_id"]]["marker"]
        )
    elif name == "unlisted-supersedes":
        other = copy.deepcopy(first)
        other["event_id"] = "devbig014-0000000000-000000"
        other["legacy_row"]["record_id"] = other["event_id"]
        correction = quarantine_correction(other, correction["legacy_row"][key])
        return [other, correction]
    elif name == "changed-count":
        correction["legacy_row"]["passed_tests"] += 1
    elif name == "changed-artifact":
        correction["legacy_row"]["constructed_plan"]["bytes"] += 1
    elif name == "wrong-result":
        correction["legacy_row"]["result"] = "fail"
    elif name == "wrong-source":
        correction["legacy_row"]["source"] = "observed"
    elif name == "dropped-field":
        del correction["legacy_row"]["run_number"]
    elif name == "retyped-schema":
        correction["legacy_row"]["schema_version"] = 10.0
    elif name == "lost-run-altered":
        first = copy.deepcopy(first)
        first["legacy_row"]["commit"] = "b" * 40
        correction = quarantine_correction(first)
    elif name == "changed-profile":
        correction["legacy_row"]["profile"] = "full"
    elif name == "changed-selection-mode":
        correction["legacy_row"]["selection_mode"] = "full"
    elif name == "dropped-passed-count":
        del correction["legacy_row"]["passed_tests"]
    elif name == "coverage-null-removed":
        # An absent key and a null differ; the recorded row carries the null.
        assert correction["legacy_row"]["coverage"] is None
        del correction["legacy_row"]["coverage"]
    elif name == "real-seconds-retyped":
        seconds = correction["legacy_row"]["real_seconds"]
        assert type(seconds) is float and seconds.is_integer()
        correction["legacy_row"]["real_seconds"] = int(seconds)
    elif name == "retyped-passed-count":
        correction["legacy_row"]["passed_tests"] = float(correction["legacy_row"]["passed_tests"])
    elif name == "boolean-exit-code":
        # 0 == False in Python; the binding compares types, not equality.
        correction["legacy_row"]["exit_code"] = False
    elif name == "added-field":
        correction["legacy_row"]["full_coverage"] = True
    elif name == "lost-run-profile-altered":
        # The lost run's own row is also bound, not only its identity fields.
        first = copy.deepcopy(first)
        first["legacy_row"]["profile"] = "full"
        correction = quarantine_correction(first)
    elif name == "lost-run-envelope-altered":
        first = copy.deepcopy(first)
        first["emitted_at"] = "2026-09-22T04:47:40Z"
    elif name == "lost-run-premarked":
        first = copy.deepcopy(first)
        first["legacy_row"][key] = copy.deepcopy(correction["legacy_row"][key])
        return [first, quarantine_correction(first)]
    else:
        raise AssertionError(name)
    return [first, correction]


@pytest.mark.parametrize("name", [
    "self-authored-marker",
    "other-entry-marker",
    "unlisted-supersedes",
    "changed-count",
    "changed-artifact",
    "wrong-result",
    "wrong-source",
    "dropped-field",
    "retyped-schema",
    "lost-run-altered",
    "lost-run-premarked",
    "changed-profile",
    "changed-selection-mode",
    "dropped-passed-count",
    "retyped-passed-count",
    "boolean-exit-code",
    "added-field",
    "lost-run-profile-altered",
    "lost-run-envelope-altered",
    "coverage-null-removed",
    "real-seconds-retyped",
])
def test_quarantine_correction_other_than_the_allowlisted_one_is_refused(tmp_path: Path, name: str):
    write_events(tmp_path, *_quarantine_variant(name, quarantine_lost_events()))
    for preserve_admission in (False, True):
        with pytest.raises(ValueError, match="retained evidence quarantine"):
            validate_rows.rows(root=tmp_path, preserve_admission=preserve_admission)


def _write_event_texts(root: Path, first: dict, *texts: str) -> None:
    path = root / "ledger" / str(first["team"]) / str(first["host"]) / "2026-08.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(text + "\n" for text in texts), encoding="utf-8")


def _repeat_first_key(text: str, anchor: str, key: str, value: str) -> str:
    """Write ``key`` once more, forged and first, inside the object ``anchor`` opens."""
    assert text.count(anchor) == 1
    return text.replace(anchor, f'{anchor}"{key}":{value},', 1)


@pytest.mark.parametrize("where", [
    "correction-row-branch", "correction-row-same-value", "correction-marker",
    "correction-envelope", "lost-run-row",
])
def test_quarantine_that_repeats_a_key_is_refused_by_every_ledger_reader(
    tmp_path: Path, where: str,
):
    """A dictionary keeps the last of a repeated key, so each forged text below
    would read as the exact correction. Both the pair-preserving authority
    readers and the default compatibility readers keep the written pairs of an
    object that repeats a key, and must refuse it."""
    key = validate_rows.qualifying_receipt.RETAINED_EVIDENCE_QUARANTINE_KEY
    lost = quarantine_lost_events()
    corrections = [json.dumps(quarantine_correction(event), sort_keys=True) for event in lost]
    texts = [json.dumps(event, sort_keys=True) for event in lost]
    _write_event_texts(tmp_path, lost[0], *texts, *corrections)
    for preserve_admission in (True, False):
        genuine = validate_rows.rows(root=tmp_path, preserve_admission=preserve_admission)
        assert sum(key in row for row in genuine) == 2
    # Objects that repeat no key still read as plain dictionaries by default.
    assert all(type(event) is dict for event in validate_rows.events(root=tmp_path))
    branch = json.dumps(lost[0]["legacy_row"]["branch"])
    refusal = "retained evidence quarantine names a key more than once"
    if where == "correction-row-branch":
        corrections[0] = _repeat_first_key(corrections[0], '"legacy_row": {', "branch", '"forged"')
    elif where == "correction-row-same-value":
        corrections[0] = _repeat_first_key(corrections[0], '"legacy_row": {', "branch", branch)
    elif where == "correction-marker":
        corrections[0] = _repeat_first_key(corrections[0], f'"{key}": {{', "reason", '"forged"')
    elif where == "correction-envelope":
        # The event schema's own decoder already names this one.
        corrections[0] = "{" + '"reason":"forged",' + corrections[0][1:]
        refusal = "duplicate field `reason`"
    else:
        texts[0] = _repeat_first_key(texts[0], '"legacy_row": {', "branch", '"forged"')
    _write_event_texts(tmp_path, lost[0], *texts, *corrections)
    with pytest.raises(ValueError, match=refusal):
        validate_rows.rows(root=tmp_path, preserve_admission=True)
    with pytest.raises(ValueError, match=refusal):
        validate_rows.status_input(root=tmp_path, preserve_admission=True)
    # The default readers serialize a merged envelope for the event schema, so
    # the repeated envelope key is refused by the quarantine judge instead.
    default_refusal = ("retained evidence quarantine names a key more than once"
                       if where == "correction-envelope" else refusal)
    with pytest.raises(ValueError, match=default_refusal):
        validate_rows.rows(root=tmp_path)
    with pytest.raises(ValueError, match=default_refusal):
        validate_rows.status_input(root=tmp_path)
    # The raw union judges no correction; it must hand the written pairs on.
    forged = [event for event in validate_rows.events(root=tmp_path)
              if validate_rows.qualifying_receipt._quarantine_repeated_key(event)]
    assert len(forged) == 1


def test_quarantine_marker_in_a_result_row_is_refused(tmp_path: Path):
    """A self-consistent row that marks itself quarantined is a forgery."""
    lost = quarantine_lost_events()[0]
    forged = copy.deepcopy(lost)
    forged["legacy_row"] = quarantine_correction(lost)["legacy_row"]
    write_events(tmp_path, forged)
    with pytest.raises(ValueError, match="outside a correction"):
        validate_rows.rows(root=tmp_path)
