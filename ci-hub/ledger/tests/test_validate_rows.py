import json
import hashlib
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import validate_rows  # noqa: E402


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
        "host_after": {
            "load1": 0.0,
            "load5": 0.0,
            "load15": 0.0,
            "nproc": 1,
            "concurrent_procs": 0,
            "proc_pattern": "hermit",
            "cpu_pressure_avg10": 0.0,
            "mem_pressure_avg10": 0.0,
            "mem_avail_ratio": 1.0,
            "captured_at": "2026-08-09T01:01:00Z",
        },
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
        "compare_logs": True,
        "log_scope": "full_trace",
        "strip_lines": False,
        "canonicalize_addresses": True,
        "full_trace": True,
        "exact_remainder": True,
        "stripped_prefixes": [],
        "canonicalizations": [],
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


def test_record_retains_leaf_failure_and_host_sample_before_log_eviction(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    source = row(tmp_path, record_id="failed-with-diagnostics")
    source.pop("host_after")
    source["result"] = "fail"
    source["exit_code"] = 1
    source["gates"] = [{"name": "portable CI DAG manifest", "result": "fail"}]
    log = tmp_path / "validate.log"
    log.write_text(
        "[build.runtime_release] scheduler_impl.h:1325: undefined reference to "
        "`dynamorio::drmemtrace::op_infile[abi:cxx11]'\n"
        "[build.runtime_release] ✗ FAIL   build.runtime_release (exit 101)\n"
    )
    source["log_file"] = str(log)

    class FixedHostConditions:
        @staticmethod
        def sample(proc_pattern: str):
            assert proc_pattern == "hermit"
            return FixedHostConditions()

        def as_dict(self):
            return {
                "load1": 12.5,
                "load5": 8.0,
                "load15": 4.0,
                "nproc": 8,
                "concurrent_procs": 3,
                "proc_pattern": "hermit",
                "cpu_pressure_avg10": 21.0,
                "mem_pressure_avg10": 0.0,
                "mem_avail_ratio": 0.5,
                "captured_at": "2026-08-09T01:01:00Z",
            }

    attribution_dir = Path(__file__).resolve().parents[2] / "attribution"
    sys.path.insert(0, str(attribution_dir))
    import attribution

    monkeypatch.setattr(attribution, "HostConditions", FixedHostConditions)
    validate_rows.record(source, root=tmp_path)

    # The log may disappear immediately after record(); the canonical row must
    # remain sufficient to name the leaf and its cause.
    log.unlink()
    [recorded] = validate_rows.rows(root=tmp_path)
    assert recorded["log_status"] == "present"
    assert recorded["host_after"]["load1"] == 12.5
    assert recorded["host_after"]["concurrent_procs"] == 3
    assert recorded["failed_substep_classes"][0]["node"] == "build.runtime_release"
    assert recorded["failed_substep_classes"][0]["fault_class"] == "infrastructure"
    assert "undefined reference to" in recorded["first_error_line"]


def test_record_marks_missing_failure_log_without_fabricating_cause(tmp_path: Path):
    source = row(tmp_path, record_id="failed-with-missing-log")
    source["result"] = "fail"
    source["exit_code"] = 1
    source["gates"] = [{"name": "portable CI DAG manifest", "result": "fail"}]
    source["log_file"] = str(tmp_path / "never-existed.log")

    validate_rows.record(source, root=tmp_path)

    [recorded] = validate_rows.rows(root=tmp_path)
    assert recorded["log_status"] == "missing"
    assert recorded["failed_substep_classes"] == []
    assert recorded["first_error_line"] is None


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


@pytest.mark.parametrize("field", ["finished_at"])
def test_incomplete_row_is_refused_without_creating_an_alternate_ledger(
    tmp_path: Path, field: str
):
    source = row(tmp_path)
    del source[field]
    with pytest.raises(ValueError):
        validate_rows.record(source, root=tmp_path)
    assert not (tmp_path / "ledger").exists()
