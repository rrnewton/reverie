#!/usr/bin/env python3
"""Tests for finalize_receipt: schema-5 coverage{} + counts finalizer.

Each test states N (the number of synthetic test nodes) and asserts the emitted
coverage{} would SATISFY or REFUSE per the consumer rule:
  satisfied == planned_test_nodes > 0 && zero_executed_nodes == [] && absent_nodes == []
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import finalize_receipt as fr
import pytest

HERE = Path(__file__).resolve().parent
MODULE = HERE / "finalize_receipt.py"
BASE_FIELDS = {
    "base_sha": "1" * 40,
    "base_tree": "2" * 40,
    "reverie_base_sha": "3" * 40,
    "reverie_base_tree": "4" * 40,
}


@pytest.fixture(autouse=True)
def _producer_definition_fixture(monkeypatch):
    """Keep coverage/finalizer unit fixtures focused on their own condition.

    Individual finalizer tests use synthetic SHAs and non-git paths. No
    registered producer map exists to resolve against
    (https://github.com/rrnewton/dev-hermit/issues/414).
    """
    monkeypatch.setenv("CI_HUB_LEDGER_FIXTURE_MODE", "1")


def _satisfied(cov: dict) -> bool:
    return fr.qualifying_receipt.coverage_satisfied(cov)


def _passing_node(tag: str, n: int) -> str:
    return (
        f"[{tag}] running {n} tests\n"
        f"[{tag}] test result: ok. {n} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s\n"
        f"[{tag}] ✓ PASS   {tag} ({n}s)  [test result: ok. {n} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out]\n"
    )


def _structured_result(owner: str) -> dict:
    return {
        "kind": "structured-test-results",
        "schema": 2,
        "path_env": "DAGRUN_TEST_COUNTS_PATH",
        "owner": owner,
    }


def _canonical_step(
    group: str,
    job: str,
    *,
    labels: list[str] | None = None,
    deps: list[str] | None = None,
    result_manifests: list[dict] | None = None,
) -> dict:
    tag = f"{group}.{job}"
    return {
        "group": group,
        "job": job,
        "labels": ["full"] if labels is None else labels,
        "deps": [] if deps is None else deps,
        "result_manifests": (
            [_structured_result(tag)] if result_manifests is None else result_manifests
        ),
    }


# --- N=13 all-passing -> SATISFIES ------------------------------------------

def test_thirteen_nodes_all_pass_satisfies():
    tags = [f"test.node{i:02d}" for i in range(13)]  # N = 13
    log = "".join(_passing_node(t, i + 1) for i, t in enumerate(tags))
    row = fr.build_coverage(log, set(tags))
    cov = row["coverage"]
    assert cov["planned_test_nodes"] == 13
    assert cov["executed_test_nodes"] == 13
    assert cov["zero_executed_nodes"] == []
    assert cov["absent_nodes"] == []
    assert _satisfied(cov)
    assert row["schema_version"] == 5
    assert row["executed_tests"] == sum(range(1, 14))  # 91 passed aggregate


# --- banner-node-zero -> REFUSES (the most important clause) -----------------

def test_banner_node_zero_refuses():
    """N=2 planned nodes; one emits a `0 passed; 5 filtered out` banner + terminal
    PASS. Its passed-sum is 0 with a banner present -> zero_executed_nodes ->
    coverage NOT satisfied. This is the positive proof the guard FIRES."""
    good = "test.good"
    zero = "test.zero"
    log = (
        _passing_node(good, 7)
        + f"[{zero}] running 0 tests\n"
        + f"[{zero}] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.0s\n"
        + f"[{zero}] ✓ PASS   {zero} (1s)  [test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out]\n"
    )
    row = fr.build_coverage(log, {good, zero})
    cov = row["coverage"]
    assert cov["zero_executed_nodes"] == [zero]
    assert cov["absent_nodes"] == []
    assert cov["planned_test_nodes"] == 2
    assert cov["executed_test_nodes"] == 1
    assert not _satisfied(cov)


# --- multi-banner node aggregates -> not over-refused ------------------------

def test_multibanner_node_not_over_refused():
    """A node with a `0 passed; 213 filtered out` banner AND a `40 passed` banner
    aggregates to 40 -> NOT zero_executed. Proves node-level aggregation."""
    tag = "test.multi"
    log = (
        f"[{tag}] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 213 filtered out; finished in 0.0s\n"
        f"[{tag}] test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.5s\n"
        f"[{tag}] ✓ PASS   {tag} (1s)\n"
    )
    row = fr.build_coverage(log, {tag})  # N = 1
    cov = row["coverage"]
    assert cov["zero_executed_nodes"] == []
    assert cov["absent_nodes"] == []
    assert _satisfied(cov)
    assert row["executed_tests"] == 40
    assert row["filtered_tests"] == 213


# --- absent planned node -> REFUSES -----------------------------------------

def test_absent_node_refuses():
    """N=2 planned; the log only mentions one -> the other is absent -> REFUSES."""
    present = "test.present"
    missing = "test.missing"
    log = _passing_node(present, 4)
    row = fr.build_coverage(log, {present, missing})
    cov = row["coverage"]
    assert cov["absent_nodes"] == [missing]
    assert cov["zero_executed_nodes"] == []
    assert not _satisfied(cov)


# --- no-banner node -> EXEMPT, SATISFIES ------------------------------------

def test_no_banner_node_exempt_satisfies():
    """A planned shell/e2e test node with terminal PASS and ZERO banners is
    EXEMPT: not zero_executed, not absent -> SATISFIES."""
    tag = "test.shell"
    log = f"[{tag}] ✓ PASS   shell/e2e node, ran real work, no libtest banner (3s)\n"
    row = fr.build_coverage(log, {tag})  # N = 1
    cov = row["coverage"]
    assert cov["zero_executed_nodes"] == []
    assert cov["absent_nodes"] == []
    assert cov["executed_test_nodes"] == 1
    assert _satisfied(cov)
    # No banner anywhere in the log -> aggregate counts are UNKNOWN (null).
    assert row["executed_tests"] is None
    assert row["filtered_tests"] is None


# --- executed_tests 0 vs null (distinguishable) -----------------------------

def test_executed_tests_zero_vs_null_distinguishable():
    # Banners present, all summing to 0 -> Some(0), NOT null.
    zero_log = (
        "[test.a] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out\n"
        "[test.a] ✓ PASS a\n"
    )
    row_zero = fr.build_coverage(zero_log, {"test.a"})
    assert row_zero["executed_tests"] == 0
    assert row_zero["filtered_tests"] == 9

    # No banner at all -> null (UNKNOWN), distinct from 0.
    none_log = "[test.a] ✓ PASS a (no libtest banner)\n"
    row_none = fr.build_coverage(none_log, {"test.a"})
    assert row_none["executed_tests"] is None
    assert row_none["filtered_tests"] is None

    # And the JSON serialization keeps 0 vs null distinct.
    assert json.dumps(row_zero["executed_tests"]) == "0"
    assert json.dumps(row_none["executed_tests"]) == "null"


def test_realistic_run_executed_positive():
    tags = {f"test.n{i}" for i in range(5)}
    log = "".join(_passing_node(t, 12) for t in sorted(tags))
    row = fr.build_coverage(log, tags)
    assert row["executed_tests"] == 60  # Some(n>0)
    assert _satisfied(row["coverage"])


def test_schema6_clone_preserves_cell_results_without_downgrade():
    selected = [{
        "lane": "strict",
        "category": "record-replay",
        "test": "hello",
        "mode": "verify",
        "backend": "ptrace",
    }]
    cells = {
        "run_id": "run-1",
        "hermit_sha": "a" * 40,
        "source_tree_dirty": False,
        "selected_count": 1,
        "recorded_count": 1,
        "population_sha256": fr.qualifying_receipt.cell_population_sha256(selected),
        "artifact": {"path": "ignored/validate/artifacts/run-1/e2e-results.jsonl", "sha256": "c" * 64, "row_count": 1},
        "selected": selected,
        "cells": [{
            "lane": "strict", "category": "record-replay", "test": "hello", "mode": "verify", "backend": "ptrace",
            "cell_verdict": {"state": "unavailable-with-reason", "comparison_tier": "declared-but-unverifiable", "reason": "raw no_result"},
        }],
    }
    base = {
        "schema_version": 6,
        "record_id": "source-row",
        "run_id": "run-1",
        "commit": "a" * 40,
        "producer": "hermit-validate-rs",
        "cell_results": cells,
    }
    encoded = json.dumps(cells, sort_keys=True, separators=(",", ":"))

    clone = fr._clone_upgraded(
        base,
        {
            "schema_version": 5,
            "executed_tests": 1,
            "filtered_tests": 0,
            "coverage": {"planned_test_nodes": 1, "executed_test_nodes": 1, "zero_executed_nodes": [], "absent_nodes": []},
        },
        "d" * 64,
    )

    assert clone["schema_version"] == 6
    assert clone["record_id"] != base["record_id"]
    assert clone["run_id"] == base["run_id"] == cells["run_id"]
    assert json.dumps(clone["cell_results"], sort_keys=True, separators=(",", ":")) == encoded
    assert fr.qualifying_receipt.cell_results_verdict(
        clone, fr.qualifying_receipt.active()
    ) is fr.qualifying_receipt.CellResultsVerdict.SATISFIED


def test_writer_records_exact_base_and_reverie_tree(tmp_path):
    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "t",
        "GIT_AUTHOR_EMAIL": "t@e",
        "GIT_COMMITTER_NAME": "t",
        "GIT_COMMITTER_EMAIL": "t@e",
    }

    def git(repo: Path, *args: str) -> str:
        return subprocess.run(
            ["git", "-C", str(repo), *args], check=True, capture_output=True,
            text=True, env=env,
        ).stdout.strip()

    reverie = tmp_path / "reverie"
    reverie.mkdir()
    git(reverie, "init", "-q")
    (reverie / "reverie-dbi").mkdir()
    (reverie / "reverie-dbi/lib.rs").write_text("pub fn dbi() {}\n")
    git(reverie, "add", "-A")
    git(reverie, "commit", "-qm", "reverie base")
    reverie_sha = git(reverie, "rev-parse", "HEAD")

    hermit = tmp_path / "hermit"
    hermit.mkdir()
    git(hermit, "init", "-q")
    (hermit / "Cargo.lock").write_text(
        f'source = "git+https://github.com/rrnewton/reverie.git?rev={reverie_sha}#{reverie_sha}"\n'
    )
    (hermit / "src").mkdir()
    (hermit / "src/lib.rs").write_text("pub fn base() {}\n")
    git(hermit, "add", "-A")
    git(hermit, "commit", "-qm", "base")
    base = git(hermit, "rev-parse", "HEAD")
    git(hermit, "update-ref", "refs/remotes/origin/main", base)
    (hermit / "src/lib.rs").write_text("pub fn branch() {}\n")
    git(hermit, "add", "-A")
    git(hermit, "commit", "-qm", "branch")
    head = git(hermit, "rev-parse", "HEAD")

    fields = fr.build_base_evidence(str(hermit), head, str(reverie))
    assert fields["base_sha"] == base
    assert fields["base_tree"] == git(hermit, "rev-parse", f"{base}^{{tree}}")
    assert fields["reverie_base_sha"] == reverie_sha
    assert fields["reverie_base_tree"] == git(
        reverie, "rev-parse", f"{reverie_sha}^{{tree}}"
    )


# --- in-place mutation is gone ---------------------------------------------

def test_in_place_ledger_rewrite_is_refused(tmp_path):
    ledger = tmp_path / "l.jsonl"
    ledger.write_text(json.dumps({"commit": "c" * 40}) + "\n")
    proc = subprocess.run(
        [sys.executable, str(MODULE), "--log", "/dev/null",
         "--sha", "c" * 40, "--hermit-checkout", str(tmp_path),
         "--ledger", str(ledger)],
        capture_output=True, text=True,
    )
    assert proc.returncode == 2
    assert "in-place ledger rewrites are disabled" in proc.stderr


# --- CLI emit-only ----------------------------------------------------------

def test_cli_emit_only_refuses_without_replayable_base(tmp_path):
    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@e",
        "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@e",
    }
    subprocess.run(["git", "-C", str(tmp_path), "init", "-q"], check=True, env=env)
    manifest = tmp_path / "ci/dag/validate.json"
    manifest.parent.mkdir(parents=True)
    manifest.write_text(json.dumps({"steps": [_canonical_step("test", "a")]}) + "\n")
    subprocess.run(
        ["git", "-C", str(tmp_path), "add", "ci/dag/validate.json"],
        check=True,
        env=env,
    )
    subprocess.run(
        ["git", "-C", str(tmp_path), "commit", "-qm", "canonical plan"],
        check=True,
        env=env,
    )
    sha = subprocess.run(
        ["git", "-C", str(tmp_path), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
        env=env,
    ).stdout.strip()
    log = tmp_path / "dag.log"
    log.write_text(_passing_node("test.a", 3))
    proc = subprocess.run(
        [sys.executable, str(MODULE), "--log", str(log),
         "--sha", sha, "--hermit-checkout", str(tmp_path), "--emit-only"],
        capture_output=True, text=True,
    )
    assert proc.returncode == 2
    assert "cannot record base evidence" in proc.stderr


# --- race-safe exact-row append minting -------------------------------------

def _countless_green_row(sha: str, log_path: str) -> dict:
    return {
        "schema_version": 4, "repo": "hermit", "commit": sha,
        "tree": "5" * 40, "result": "pass", "raw_result": "pass",
        "commit_anchored": True, "tree_dirty": False,
        "selection_mode": "full", "profile": "full",
        "executed_tests": None, "filtered_tests": None,
        "exit_code": 0, "checks": 2, "failures": 0,
        "gates_run": 2, "gates_expected": 2,
        "gates": [
            {"name": "portable CI DAG lane", "result": "pass", "exit_code": 0},
            {"name": "privileged CI DAG lane", "result": "pass", "exit_code": 0},
        ],
        "started_at": "2026-08-08T00:00:00Z",
        "finished_at": "2026-08-08T00:10:00Z",
        "host": "fixture-host", "slot": "fixture-slot",
        "log_file": log_path, "real_seconds": 900,
        "producer": "hermit-validate-sh",
        "admission": "ci-hub-validate-lock",
        "concurrent_validates": 0,
        "concurrency_proof": "validate_lock_owner_ancestry",
    }


def _qualifying_schema5_row(sha: str) -> dict:
    row = _countless_green_row(sha, "/unused")
    row.update(
        schema_version=5,
        executed_tests=6,
        passed_tests=6,
        filtered_tests=0,
        coverage={
            "planned_test_nodes": 1,
            "executed_test_nodes": 1,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        },
        **BASE_FIELDS,
        producer="ci-hub-finalize-receipt",
        admission="ci-hub-validate-lock",
        concurrent_validates=0,
        concurrency_proof="validate_lock_owner_ancestry",
    )
    return row


def _counted_schema5_source(sha: str, log_path: str = "/already-evicted") -> dict:
    """A real producer-complete measurement that lacks only base evidence."""
    row = _countless_green_row(sha, log_path)
    row.update(
        schema_version=5,
        executed_tests=862,
        passed_tests=862,
        filtered_tests=693,
        coverage={
            "planned_test_nodes": 19,
            "executed_test_nodes": 19,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        },
        producer="hermit-validate-sh",
        admission="ci-hub-validate-lock",
        concurrent_validates=0,
        concurrency_proof="validate_lock_owner_ancestry",
    )
    return row


def test_schema5_idempotency_guard_uses_canonical_coverage_authority():
    """Positive and absent-list controls bracket the finalizer consumer."""
    row = _qualifying_schema5_row("9" * 40)
    assert fr._has_satisfied_schema5(row) is True

    omitted = json.loads(json.dumps(row))
    del omitted["coverage"]["absent_nodes"]
    assert fr._has_satisfied_schema5(omitted) is False

    tampered = json.loads(json.dumps(row))
    tampered["coverage"]["absent_nodes"] = ["test.missing"]
    assert fr._has_satisfied_schema5(tampered) is False


def test_countless_exact_row_refuses_and_preserves_every_byte(tmp_path, monkeypatch):
    """A log replay cannot manufacture the framework-owned pass count."""
    sha = "a" * 40
    selected_log = tmp_path / "selected.log"
    selected_log.write_text(_passing_node("test.only", 6))
    other_log = tmp_path / "other.log"
    other_log.write_text(_passing_node("test.only", 99))
    ledger = tmp_path / "validate-run-ledger.jsonl"
    foreign = {"schema_version": 3, "commit": "b" * 40, "result": "fail"}
    unselected = _countless_green_row(sha, str(other_log))
    unselected["source_marker"] = "must-not-clone"
    unselected["finished_at"] = "2026-08-08T00:09:00Z"
    selected = _countless_green_row(sha, str(selected_log))
    selected["source_marker"] = "selected"
    original_lines = [
        json.dumps(foreign, separators=(", ", ": ")),
        json.dumps(unselected, sort_keys=True),
        json.dumps(selected, separators=(",", ":")),
    ]
    ledger.write_text("\n".join(original_lines) + "\n")

    monkeypatch.setattr(fr, "planned_test_nodes", lambda c, s: {"test.only"})
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))
    selected_digest = fr.canonical_row_sha256(selected, sha)
    assert fr.select_candidate_sha256(str(ledger), sha) == selected_digest
    result = fr.scan_and_finalize(
        str(ledger), str(tmp_path), sha, selected_digest
    )

    assert result["satisfied"] is False and result["sha"] == sha
    assert result["reason"] == "insufficient-source-row"
    assert result["appended"] == 0
    out_lines = [l for l in ledger.read_text().splitlines() if l.strip()]
    assert out_lines == original_lines


def test_counted_schema5_source_preserves_measurement_and_adds_only_base(
    tmp_path, monkeypatch
):
    """A complete producer row needs no surviving log to gain base evidence."""
    sha = "4" * 40
    source = _counted_schema5_source(sha)
    ledger = tmp_path / "counted.jsonl"
    ledger.write_text(json.dumps(source) + "\n")
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))
    monkeypatch.setattr(
        fr,
        "build_coverage",
        lambda *a: (_ for _ in ()).throw(AssertionError("coverage was recomputed")),
    )
    digest = fr.canonical_row_sha256(source, sha)

    assert fr.select_candidate_sha256(str(ledger), sha) == digest
    result = fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)

    assert result == {
        "sha": sha,
        "satisfied": True,
        "reason": "minted",
        "executed_tests": 862,
        "planned_test_nodes": 19,
        "exit_code": 0,
        "appended": 1,
    }
    minted = json.loads(ledger.read_text().splitlines()[-1])
    assert minted["executed_tests"] == source["executed_tests"]
    assert minted["filtered_tests"] == source["filtered_tests"]
    assert minted["coverage"] == source["coverage"]
    assert {key: minted[key] for key in BASE_FIELDS} == BASE_FIELDS
    assert minted["producer"] == "ci-hub-finalize-receipt"


def test_counted_schema5_source_without_admission_remains_refused(
    tmp_path, monkeypatch
):
    """Base binding cannot launder the pre-admission producer shape."""
    sha = "5" * 40
    source = _counted_schema5_source(sha)
    source.pop("admission")
    source.pop("producer")
    source["concurrent_validates"] = 16
    source["concurrency_proof"] = "process_group_overlap_monitor"
    ledger = tmp_path / "unadmitted.jsonl"
    before = json.dumps(source) + "\n"
    ledger.write_text(before)
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))
    digest = fr.canonical_row_sha256(source, sha)

    assert fr.select_candidate_sha256(str(ledger), sha) == digest
    result = fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)

    assert result["reason"] == "insufficient-source-row"
    assert result["satisfied"] is False and result["appended"] == 0
    assert ledger.read_text() == before


def test_file_registry_does_not_block_finalization(tmp_path, monkeypatch):
    sha = "f" * 40
    ledger = tmp_path / "ledger.jsonl"
    source = _counted_schema5_source(sha)
    before = json.dumps(source) + "\n"
    ledger.write_text(before)
    digest = fr.canonical_row_sha256(source, sha)
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))

    # The registered-map resolver is gone, so nothing can refuse on it.
    assert not hasattr(fr.qualifying_receipt, "resolve_producer_definition")
    result = fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)
    assert result["reason"] == "minted"
    assert result["exit_code"] == 0 and result["appended"] == 1
    assert ledger.read_text().startswith(before)


def test_exact_row_finalization_is_idempotent(tmp_path, monkeypatch):
    sha = "c" * 40
    ledger = tmp_path / "l.jsonl"
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))
    source = _counted_schema5_source(sha)
    ledger.write_text(json.dumps(source) + "\n")
    digest = fr.canonical_row_sha256(source, sha)

    first = fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)
    assert first["reason"] == "minted"
    n_after_first = len([l for l in ledger.read_text().splitlines() if l.strip()])

    second = fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)
    assert second["reason"] == "already-finalized"
    n_after_second = len([l for l in ledger.read_text().splitlines() if l.strip()])
    assert n_after_second == n_after_first


def test_exact_row_dry_run_writes_nothing(tmp_path, monkeypatch):
    sha = "d" * 40
    ledger = tmp_path / "l.jsonl"
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))
    source = _counted_schema5_source(sha)
    before = json.dumps(source) + "\n"
    ledger.write_text(before)
    digest = fr.canonical_row_sha256(source, sha)

    result = fr.scan_and_finalize(
        str(ledger), str(tmp_path), sha, digest, dry_run=True
    )
    assert result["reason"] == "would-mint" and result["satisfied"]
    assert ledger.read_text() == before


def test_exact_row_missing_log_or_manifest_never_appends(tmp_path, monkeypatch):
    missing_log_sha = "1" * 40
    missing_log = _countless_green_row(
        missing_log_sha, str(tmp_path / "missing.log")
    )
    missing_log_ledger = tmp_path / "missing-log.jsonl"
    missing_log_before = json.dumps(missing_log) + "\n"
    missing_log_ledger.write_text(missing_log_before)
    missing_log_digest = fr.canonical_row_sha256(missing_log, missing_log_sha)
    result = fr.scan_and_finalize(
        str(missing_log_ledger), str(tmp_path), missing_log_sha, missing_log_digest
    )
    assert result["reason"] == "no-log" and result["exit_code"] != 0
    assert result["appended"] == 0
    assert missing_log_ledger.read_text() == missing_log_before

    no_manifest_sha = "2" * 40
    log = tmp_path / "run.log"
    log.write_text(_passing_node("test.only", 3))
    no_manifest = _countless_green_row(no_manifest_sha, str(log))
    no_manifest_ledger = tmp_path / "no-manifest.jsonl"
    no_manifest_before = json.dumps(no_manifest) + "\n"
    no_manifest_ledger.write_text(no_manifest_before)
    no_manifest_digest = fr.canonical_row_sha256(no_manifest, no_manifest_sha)
    monkeypatch.setattr(fr, "planned_test_nodes", lambda c, s: set())
    result = fr.scan_and_finalize(
        str(no_manifest_ledger), str(tmp_path), no_manifest_sha, no_manifest_digest
    )
    assert result["reason"] == "no-manifest" and result["exit_code"] != 0
    assert result["appended"] == 0
    assert no_manifest_ledger.read_text() == no_manifest_before


def test_exact_row_identity_negatives_append_nothing(tmp_path):
    sha = "e" * 40
    log = tmp_path / "run.log"
    log.write_text(_passing_node("test.only", 5))
    source = _countless_green_row(sha, str(log))
    ledger = tmp_path / "l.jsonl"
    before = json.dumps(source) + "\n"
    ledger.write_text(before)
    digest = fr.canonical_row_sha256(source, sha)

    for wrong_sha, wrong_digest, reason in (
        (sha, "0" * 64, "selected-row-missing"),
        ("f" * 40, digest, "selected-row-missing"),
    ):
        try:
            fr.scan_and_finalize(
                str(ledger), str(tmp_path), wrong_sha, wrong_digest
            )
        except fr.SelectedRowError as error:
            assert error.reason == reason
        else:
            raise AssertionError("identity mismatch was accepted")
        assert ledger.read_text() == before

    missing = tmp_path / "missing.jsonl"
    missing.write_text(json.dumps({"commit": "f" * 40}) + "\n")
    try:
        fr.scan_and_finalize(str(missing), str(tmp_path), sha, digest)
    except fr.SelectedRowError as error:
        assert error.reason == "selected-row-missing"
    else:
        raise AssertionError("missing selected row was accepted")
    assert len(missing.read_text().splitlines()) == 1


def test_ambiguous_duplicate_selected_row_refuses_without_append(tmp_path):
    sha = "6" * 40
    log = tmp_path / "run.log"
    log.write_text(_passing_node("test.only", 5))
    source = _countless_green_row(sha, str(log))
    line = json.dumps(source)
    ledger = tmp_path / "l.jsonl"
    before = f"{line}\n{line}\n"
    ledger.write_text(before)
    digest = fr.canonical_row_sha256(source, sha)
    try:
        fr.select_candidate_sha256(str(ledger), sha)
    except fr.SelectedRowError as error:
        assert error.reason == "ambiguous-selected-row"
    else:
        raise AssertionError("selector accepted duplicate exact source rows")
    try:
        fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)
    except fr.SelectedRowError as error:
        assert error.reason == "ambiguous-selected-row"
    else:
        raise AssertionError("duplicate exact selected rows were accepted")
    assert ledger.read_text() == before


def test_insufficient_old_source_row_never_appends_or_claims_satisfied(
    tmp_path, monkeypatch
):
    """Coverage cannot repair absent tree/raw_result/gates/admission evidence."""
    sha = "7" * 40
    log = tmp_path / "run.log"
    log.write_text(_passing_node("test.only", 5))
    old = {
        "schema_version": 3, "commit": sha, "result": "pass",
        "commit_anchored": True, "tree_dirty": False,
        "selection_mode": "full", "profile": "full",
        "executed_tests": None, "filtered_tests": None,
        "log_file": str(log),
    }
    ledger = tmp_path / "l.jsonl"
    before = json.dumps(old) + "\n"
    ledger.write_text(before)
    digest = fr.canonical_row_sha256(old, sha)
    monkeypatch.setattr(fr, "planned_test_nodes", lambda c, s: {"test.only"})
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))

    result = fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)
    assert result["exit_code"] != 0
    assert result["reason"] == "insufficient-source-row"
    assert result["satisfied"] is False and result["appended"] == 0
    assert ledger.read_text() == before


def test_complete_preflight_rejects_each_missing_load_bearing_condition(
    tmp_path, monkeypatch
):
    """Schema promotion cannot manufacture tree/result/gate/admission proof."""
    log = tmp_path / "run.log"
    log.write_text(_passing_node("test.only", 5))
    monkeypatch.setattr(fr, "planned_test_nodes", lambda c, s: {"test.only"})
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))

    for index, missing in enumerate(
        ("tree", "raw_result", "gates", "admission", "concurrency")
    ):
        sha = f"{index + 9:x}" * 40
        source = _countless_green_row(sha, str(log))
        if missing == "concurrency":
            source.pop("concurrent_validates")
            source.pop("concurrency_proof")
        else:
            source.pop(missing)
        ledger = tmp_path / f"missing-{missing}.jsonl"
        before = json.dumps(source) + "\n"
        ledger.write_text(before)
        digest = fr.canonical_row_sha256(source, sha)

        result = fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)
        assert result["reason"] == "insufficient-source-row", missing
        assert result["exit_code"] != 0 and result["appended"] == 0
        assert result["satisfied"] is False
        assert ledger.read_text() == before


def test_existing_clone_cannot_launder_incomplete_selected_source(
    tmp_path, monkeypatch
):
    """Already-finalized handling must follow the complete source preflight."""
    sha = "4" * 40
    log = tmp_path / "run.log"
    log.write_text(_passing_node("test.only", 5))
    old = {
        "schema_version": 3, "commit": sha, "result": "pass",
        "commit_anchored": True, "tree_dirty": False,
        "selection_mode": "full", "profile": "full",
        "executed_tests": None, "filtered_tests": None,
        "log_file": str(log),
    }
    digest = fr.canonical_row_sha256(old, sha)
    claimed_clone = _qualifying_schema5_row(sha)
    claimed_clone["finalized_from"] = {
        "digest_algorithm": "sha256",
        "canonicalization": fr.RECEIPT_CANONICALIZATION,
        "digest": digest,
    }
    ledger = tmp_path / "l.jsonl"
    before = json.dumps(old) + "\n" + json.dumps(claimed_clone) + "\n"
    ledger.write_text(before)
    monkeypatch.setattr(fr, "planned_test_nodes", lambda c, s: {"test.only"})
    monkeypatch.setattr(fr, "build_base_evidence", lambda *a: dict(BASE_FIELDS))

    result = fr.scan_and_finalize(str(ledger), str(tmp_path), sha, digest)
    assert result["reason"] == "insufficient-source-row"
    assert result["exit_code"] != 0 and result["appended"] == 0
    assert result["satisfied"] is False
    assert ledger.read_text() == before


def test_scan_cli_requires_both_exact_identities(tmp_path, capsys):
    ledger = tmp_path / "l.jsonl"
    ledger.write_text("")
    rc = fr.main([
        "--scan", "--ledger", str(ledger),
        "--hermit-checkout", str(tmp_path), "--sha", "a" * 40,
    ])
    assert rc != 0
    assert "--selected-row-sha256" in capsys.readouterr().err


def test_scan_cli_identity_refusals_are_nonzero_and_append_nothing(
    tmp_path, capsys
):
    sha = "8" * 40
    log = tmp_path / "run.log"
    log.write_text(_passing_node("test.only", 2))
    source = _countless_green_row(sha, str(log))
    digest = fr.canonical_row_sha256(source, sha)

    cases = (
        ([source], sha, "0" * 64, "selected-row-missing"),
        ([source], "9" * 40, digest, "selected-row-missing"),
        ([{"commit": "9" * 40}], sha, digest, "selected-row-missing"),
        ([source, source], sha, digest, "ambiguous-selected-row"),
    )
    for index, (rows, selected_sha, selected_digest, reason) in enumerate(cases):
        ledger = tmp_path / f"case-{index}.jsonl"
        before = "".join(json.dumps(row) + "\n" for row in rows)
        ledger.write_text(before)
        rc = fr.main([
            "--scan", "--ledger", str(ledger),
            "--hermit-checkout", str(tmp_path), "--sha", selected_sha,
            "--selected-row-sha256", selected_digest,
        ])
        assert rc != 0
        assert reason in capsys.readouterr().err
        assert ledger.read_text() == before


def test_landing_callers_pass_both_exact_identities():
    root = HERE.parents[1]
    lander = (root / "ci-hub/landing/land-pr.sh").read_text()
    prevalidate = (root / "ci-hub/landing/parallel-prevalidate.sh").read_text()
    wrapper = (root / "ci-hub/validate/scan-finalize.sh").read_text()
    assert "--select-candidate-sha256" in lander
    assert "--select-candidate-sha256" in prevalidate
    assert '--sha "$HEAD" --selected-row-sha256 "$selected_row_sha256"' in lander
    assert '--sha "$exact_head" --selected-row-sha256 "$selected_row_sha256"' in prevalidate
    assert '--sha "$sha"' in wrapper
    assert '--selected-row-sha256 "$selected_row_sha256"' in wrapper


def test_scan_wrapper_selects_exact_digest_and_propagates_refusals(tmp_path):
    sha = "3" * 40
    log = tmp_path / "run.log"
    log.write_text(_passing_node("test.only", 2))
    source = _countless_green_row(sha, str(log))
    ledger = tmp_path / "ledger.jsonl"
    before = json.dumps(source) + "\n"
    ledger.write_text(before)
    checkout = tmp_path / "hermit"
    checkout.mkdir()
    (checkout / ".git").write_text("gitdir: unused\n")
    wrapper = HERE / "scan-finalize.sh"

    selected = subprocess.run(
        [
            str(wrapper), "--hermit-checkout", str(checkout),
            "--ledger", str(ledger), "--sha", sha,
            "--select-candidate-sha256",
        ],
        capture_output=True,
        text=True,
    )
    assert selected.returncode == 0, selected.stderr
    assert selected.stdout.strip() == fr.canonical_row_sha256(source, sha)

    missing = subprocess.run(
        [
            str(wrapper), "--hermit-checkout", str(checkout),
            "--ledger", str(ledger), "--sha", sha,
        ],
        capture_output=True,
        text=True,
    )
    assert missing.returncode != 0
    wrong = subprocess.run(
        [
            str(wrapper), "--hermit-checkout", str(checkout),
            "--ledger", str(ledger), "--sha", sha,
            "--selected-row-sha256", "0" * 64,
        ],
        capture_output=True,
        text=True,
    )
    assert wrong.returncode != 0
    assert ledger.read_text() == before


# --- planned_test_nodes reads the one canonical DAG at an exact SHA ---------


def test_canonical_full_plan_selects_test_producers_and_dependency_ancestry():
    data = {
        "steps": [
            _canonical_step("test", "direct"),
            _canonical_step("test", "dependency", labels=[]),
            _canonical_step(
                "gate", "consumer", deps=["test.dependency"], result_manifests=[]
            ),
            _canonical_step("test", "host_only", labels=["hosted-portable"]),
            _canonical_step(
                "e2e",
                "not_a_test_group",
                result_manifests=[_structured_result("e2e.not_a_test_group")],
            ),
            _canonical_step("privileged-test", "suffix"),
            _canonical_step("compat:test", "colon_suffix"),
        ]
    }

    assert fr._canonical_full_test_producers(data) == {
        "test.direct",
        "test.dependency",
        "privileged-test.suffix",
        "compat:test.colon_suffix",
    }


def test_canonical_manifest_missing_and_malformed_refuse(monkeypatch):
    calls = []

    def missing(args, *, cwd):
        calls.append((args, cwd))
        return subprocess.CompletedProcess(args, 128, "", "missing")

    monkeypatch.setattr(fr.bounded_capture, "git", missing)
    with pytest.raises(RuntimeError, match="ci/dag/validate.json"):
        fr.planned_test_nodes("/fixture/hermit", "a" * 40)
    assert calls == [
        (["show", f"{'a' * 40}:ci/dag/validate.json"], "/fixture/hermit")
    ]

    monkeypatch.setattr(
        fr.bounded_capture,
        "git",
        lambda _args, *, cwd: subprocess.CompletedProcess(
            _args, 0, "{not-json", ""
        ),
    )
    with pytest.raises(RuntimeError, match="malformed JSON"):
        fr.planned_test_nodes("/fixture/hermit", "b" * 40)


def test_canonical_manifest_duplicate_step_tag_refuses():
    duplicate = _canonical_step("test", "same")
    with pytest.raises(RuntimeError, match="duplicate step tag test.same"):
        fr._canonical_full_test_producers({"steps": [duplicate, dict(duplicate)]})


def test_canonical_test_producer_missing_result_manifests_refuses():
    step = _canonical_step("test", "one")
    del step["result_manifests"]
    with pytest.raises(RuntimeError, match="has no result_manifests list"):
        fr._canonical_full_test_producers({"steps": [step]})


@pytest.mark.parametrize(
    "result_manifests, message",
    [
        ([], "exactly one structured-test-results"),
        ([{"lane": "full"}], "exactly one structured-test-results"),
        (
            [_structured_result("test.one"), "not-an-object"],
            "malformed result manifest",
        ),
        (
            [_structured_result("test.one"), _structured_result("test.one")],
            "duplicate structured result manifest",
        ),
        ([_structured_result("test.someone_else")], "mismatched result owner"),
        (
            [
                {
                    "kind": "structured-test-results",
                    "schema": "2",
                    "path_env": "DAGRUN_TEST_COUNTS_PATH",
                    "owner": "test.one",
                }
            ],
            "unsupported structured result schema",
        ),
    ],
)
def test_canonical_test_producer_result_manifest_controls(result_manifests, message):
    data = {
        "steps": [_canonical_step("test", "one", result_manifests=result_manifests)]
    }
    with pytest.raises(RuntimeError, match=message):
        fr._canonical_full_test_producers(data)


def test_canonical_structured_result_contract_accepts_exact_valid_declaration():
    data = {
        "steps": [
            _canonical_step("test", "one"),
            _canonical_step(
                "e2e",
                "selected_producer",
                result_manifests=[_structured_result("e2e.selected_producer")],
            ),
        ]
    }
    assert fr._canonical_full_test_producers(data) == {"test.one"}


@pytest.mark.parametrize(
    "mutation, message",
    [
        ({"schema": 3}, "unsupported structured result schema 3"),
        ({"path_env": "OTHER"}, "path_env must be 'DAGRUN_TEST_COUNTS_PATH'"),
        ({"future": True}, "missing or unknown keys"),
    ],
)
def test_every_selected_structured_declaration_matches_canonical_contract(
    mutation, message
):
    declaration = _structured_result("e2e.selected_producer")
    declaration.update(mutation)
    # These are the exact conditions the pre-review implementation checked;
    # all three mutations passed that weaker positive/nonempty predicate.
    assert (
        isinstance(declaration["schema"], int)
        and declaration["schema"] > 0
        and isinstance(declaration["path_env"], str)
        and declaration["path_env"]
    )
    data = {
        "steps": [
            _canonical_step("test", "one"),
            _canonical_step(
                "e2e", "selected_producer", result_manifests=[declaration]
            ),
        ]
    }
    with pytest.raises(RuntimeError, match=message):
        fr._canonical_full_test_producers(data)


def test_planned_from_real_hermit_manifest_at_head():
    """Positive control that the exact Hermit HEAD canonical plan is readable.

    This was RED after the single-DAG cutover because the finalizer still read
    deleted portable/privileged files and returned an empty set.
    """
    hermit = HERE.parent.parent / "hermit"
    if not (hermit / ".git").exists() and not (hermit / ".git").is_file():
        import pytest
        pytest.skip("no hermit checkout")
    head = subprocess.run(["git", "-C", str(hermit), "rev-parse", "HEAD"],
                          capture_output=True, text=True)
    if head.returncode != 0:
        import pytest
        pytest.skip("hermit checkout has no HEAD")
    sha = head.stdout.strip()
    planned = fr.planned_test_nodes(str(hermit), sha)
    assert "test.detcore_unit" in planned
    assert "privileged-test.cli_kvm" in planned
    assert "test.cli_on_host" not in planned
    assert "test.isolated_dbt_workdir" in planned
    assert "test.isolated_detcore_workdir" in planned
    assert len(planned) == 22
    print(
        f"\n[planned-control] {len(planned)} planned test.* nodes at hermit HEAD {sha[:12]}"
    )
