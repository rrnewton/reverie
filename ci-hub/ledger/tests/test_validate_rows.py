import copy
import hashlib
import json
import os
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
        Path("ci-hub/qualifying_receipt.py"),
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

    selected = reverie_node.load_source_row(
        linked,
        source["commit"],
        linked,
        log,
        started,
    )
    assert selected == source

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
