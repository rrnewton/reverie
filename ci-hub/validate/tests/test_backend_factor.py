#!/usr/bin/env python3
"""Tests for same-run backend timing comparison."""

from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
import sys

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import backend_factor as factor


SHA = "a" * 40
RUN_ID = "validate-test-run"


def cell(
    test: str,
    backend: str | None,
    cpu: int | None,
    wall: int | None,
    **overrides: object,
) -> dict[str, object]:
    row: dict[str, object] = {
        "schema": 4,
        "run_id": RUN_ID,
        "machine_shortname": "devbig014",
        "kernel_version": "test-kernel",
        "host_capabilities": {
            "cpuid-faulting": {"present": True, "evidence": "fixture"},
            "kvm": {"present": True, "evidence": "fixture"},
        },
        "attempt": 1,
        "hermit_sha": SHA,
        "source_tree_dirty": False,
        "binary_sha256": "b" * 64,
        "binary_build_sha": SHA[:12],
        "test_sha256": hashlib.sha256(test.encode()).hexdigest(),
        "test": f"category/{test}",
        "category": "category",
        "lane": "portable",
        "mode": "verify",
        "backend": backend,
        "classification": "required",
        "outcome": "PASS",
        "result": "pass",
        "timeout_seconds": 30,
        "execution_cpu_timeout_seconds": 30,
        "execution_wall_timeout_seconds": 60,
        "log_level": "info",
        "guest_argv": [f"/{test}"],
        "artifact_dir": f"/results/{test}-{backend or 'native'}",
        "env": {"FIXTURE": f"/results/{test}-{backend or 'native'}/fixtures"},
        "cwd": "/src",
        "relaxations": [],
        "argv": ["/hermit", "--backend", backend or "native", "--", f"/{test}"],
        "effective_args": ["--backend", backend or "native", "--", f"/{test}"],
        "shell_command": f"/hermit --backend {backend or 'native'} -- /{test}",
        "cpu_usage_usec": cpu,
        "duration_ms": wall,
    }
    row.update(overrides)
    if row["mode"] == "naked" and backend is None:
        row["argv"] = [f"/{test}"]
        row["effective_args"] = []
        row["guest_argv"] = [f"/{test}"]
        row["shell_command"] = f"/{test}"
        row["log_level"] = None
    return row


def compare(
    rows: list[dict[str, object]],
    *,
    baseline_backend: str,
    comparison_backend: str,
    mode: str | None,
) -> dict[str, object]:
    verdicts = {
        (*factor._identity(row), factor._backend(row)): "compared-and-matched"
        for row in rows
    }
    return factor.compare_rows(
        rows,
        baseline_backend=baseline_backend,
        comparison_backend=comparison_backend,
        mode=mode,
        ledger_verdicts=verdicts,
    )


def test_compare_rows_reports_both_means_and_direction() -> None:
    report = compare(
        [
            cell("one", "ptrace", 10, 100),
            cell("one", "kvm", 20, 200),
            cell("two", "ptrace", 20, 200),
            cell("two", "kvm", 80, 800),
        ],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )

    population = report["population"]
    assert {
        key: value
        for key, value in population.items()
        if key != "included_pair_set_sha256"
    } == {
        "baseline_cells": 2,
        "comparison_cells": 2,
        "union_cells": 2,
        "cells_with_both_backends": 2,
        "included_pairs": 2,
        "excluded_counts": {},
        "exclusions": [],
    }
    assert len(population["included_pair_set_sha256"]) == 64
    cpu = report["metrics"]["cpu_usage_usec"]
    assert cpu["arithmetic_mean_factor"] == 3.0
    assert cpu["geometric_mean_factor"] == pytest.approx(math.sqrt(8))
    assert cpu["ratio_of_totals"] == 100 / 30
    assert cpu["comparison_slower_cells"] == 2
    assert report["pairs"][0]["factors"]["duration_ms"] == 2.0


def test_compare_rows_names_every_excluded_cell_once() -> None:
    rows = [
        cell("good", "ptrace", 10, 10),
        cell("good", "kvm", 20, 20),
        cell("ptrace-only", "ptrace", 10, 10),
        cell("kvm-only", "kvm", 10, 10),
        cell("retry", "ptrace", 10, 10, outcome="ERROR", result="crash-error"),
        cell("retry", "ptrace", 10, 10, attempt=2),
        cell("retry", "kvm", 10, 10),
        cell("failed", "ptrace", 10, 10, outcome="FAIL", result="determinism-failure"),
        cell("failed", "kvm", 10, 10),
        cell("provenance", "ptrace", 10, 10),
        cell("provenance", "kvm", 10, 10, binary_sha256="d" * 64),
        cell("timing", "ptrace", 0, 10),
        cell("timing", "kvm", 10, 10),
        cell("other-mode", "ptrace", 10, 10, mode="custom"),
        cell("other-mode", "kvm", 10, 10, mode="custom"),
    ]

    report = compare(
        rows,
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )

    population = report["population"]
    assert population["baseline_cells"] == 6
    assert population["comparison_cells"] == 6
    assert population["union_cells"] == 7
    assert population["cells_with_both_backends"] == 5
    assert population["included_pairs"] == 1
    assert population["excluded_counts"] == {
        "missing_baseline": 1,
        "missing_comparison": 1,
        "missing_or_nonpositive_timing": 1,
        "non_pass": 1,
        "provenance_mismatch": 1,
        "retry_or_ambiguous": 1,
    }
    excluded = population["exclusions"]
    assert len(excluded) == 6
    assert len({item["test"] for item in excluded}) == 6


def test_native_is_a_backend_and_boolean_timing_is_refused() -> None:
    assert factor._backend(cell("native", None, 10, 10, mode="naked")) == "native"
    report = compare(
        [
            cell("good", "ptrace", 10, 10),
            cell("good", "kvm", 20, 20),
            cell("bad-timing", "ptrace", 10, 10),
            cell("bad-timing", "kvm", 20, True),
        ],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 1
    assert report["population"]["excluded_counts"] == {
        "missing_or_nonpositive_timing": 1
    }


def test_same_backend_is_refused_and_empty_population_is_reported() -> None:
    with pytest.raises(factor.BackendFactorError, match="must differ"):
        compare([], baseline_backend="kvm", comparison_backend="kvm", mode=None)
    report = compare(
        [cell("only", "ptrace", 10, 10)],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 0
    assert report["population"]["excluded_counts"] == {"missing_comparison": 1}
    assert report["metrics"]["cpu_usage_usec"]["geometric_mean_factor"] is None


def test_empty_population_is_printed_before_the_cli_returns_nonzero(
    monkeypatch, tmp_path: Path, capsys
) -> None:
    report = compare(
        [cell("only", "ptrace", 10, 10)],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    monkeypatch.setattr(factor, "build_report", lambda *args, **kwargs: report)
    result = factor.main(
        [
            "--state-root",
            str(tmp_path),
            "--run-id",
            RUN_ID,
            "--baseline",
            "ptrace",
            "--comparison",
            "kvm",
            "--mode",
            "verify",
            "--json",
        ]
    )
    assert result == 1
    output = json.loads(capsys.readouterr().out)
    assert output["population"]["excluded_counts"] == {"missing_comparison": 1}


def test_execution_paths_are_normalized_but_real_argument_changes_are_excluded() -> (
    None
):
    base = cell("args", "ptrace", 10, 10)
    other = cell("args", "kvm", 20, 20)
    base["guest_argv"] = [f"{base['artifact_dir']}/fixtures/program"]
    other["guest_argv"] = [f"{other['artifact_dir']}/fixtures/program"]
    report = compare(
        [base, other],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 1

    other["guest_argv"].append("--different")
    report = compare(
        [
            base,
            other,
            cell("control", "ptrace", 10, 10),
            cell("control", "kvm", 20, 20),
        ],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 1
    assert report["population"]["excluded_counts"] == {"provenance_mismatch": 1}
    assert report["population"]["exclusions"][0]["detail"] == {"fields": ["guest_argv"]}


def test_artifact_path_normalization_requires_a_path_boundary() -> None:
    base = cell("paths", "ptrace", 10, 10)
    other = cell("paths", "kvm", 20, 20)
    base["guest_argv"] = [f"{base['artifact_dir']}-sibling/program"]
    other["guest_argv"] = [f"{other['artifact_dir']}-sibling/program"]
    report = compare(
        [base, other],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 0
    assert report["population"]["exclusions"][0]["detail"] == {"fields": ["guest_argv"]}


def test_sabre_evidence_sink_is_not_a_workload_difference() -> None:
    base = cell("sabre", "ptrace", 10, 10)
    other = cell("sabre", "sabre", 20, 20)
    evidence = f"{other['artifact_dir']}/captures/verify-1.sabre-path.jsonl"
    assert isinstance(other["env"], dict)
    other["env"][factor.SABRE_PATH_EVIDENCE_ENV] = evidence
    other["shell_command"] = (
        f"env '{factor.SABRE_PATH_EVIDENCE_ENV}={evidence}' "
        f"/hermit --backend sabre -- /sabre"
    )
    base["shell_command"] = "env /hermit --backend ptrace -- /sabre"
    report = compare(
        [base, other],
        baseline_backend="ptrace",
        comparison_backend="sabre",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 1

    assert isinstance(other["env"], dict)
    other["env"]["UNRELATED"] = "different"
    report = compare(
        [base, other],
        baseline_backend="ptrace",
        comparison_backend="sabre",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 0
    assert report["population"]["exclusions"][0]["detail"] == {"fields": ["env"]}

    other["env"] = {
        key: value for key, value in other["env"].items() if key != "UNRELATED"
    }
    other["env"][factor.SABRE_PATH_EVIDENCE_ENV] = "/unexpected/evidence.jsonl"
    report = compare(
        [base, other],
        baseline_backend="ptrace",
        comparison_backend="sabre",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 0
    assert report["population"]["exclusions"][0]["detail"] == {
        "fields": ["env", "shell_command"]
    }


def test_backend_specific_timeout_policy_is_not_comparable() -> None:
    report = compare(
        [
            cell("timeout", "ptrace", 10, 10, execution_cpu_timeout_seconds=20),
            cell("timeout", "kvm", 20, 20, execution_cpu_timeout_seconds=30),
            cell("control", "ptrace", 10, 10),
            cell("control", "kvm", 20, 20),
        ],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 1
    assert report["population"]["excluded_counts"] == {"provenance_mismatch": 1}
    assert report["population"]["exclusions"][0]["detail"] == {
        "fields": ["execution_cpu_timeout_seconds"]
    }


def test_full_hermit_invocation_must_match_after_backend_normalization() -> None:
    base = cell("flags", "ptrace", 10, 10)
    other = cell("flags", "kvm", 20, 20)
    other["argv"].append("--extra")
    other["effective_args"].append("--extra")
    other["shell_command"] += " --extra"
    report = compare(
        [
            base,
            other,
            cell("control", "ptrace", 10, 10),
            cell("control", "kvm", 20, 20),
        ],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 1
    assert report["population"]["exclusions"][0]["detail"] == {
        "fields": ["argv", "effective_args", "shell_command"]
    }


def test_raw_pass_requires_a_ledger_bound_matched_verdict() -> None:
    rows = [
        cell("diverged", "ptrace", 10, 10),
        cell("diverged", "kvm", 20, 20),
        cell("control", "ptrace", 10, 10),
        cell("control", "kvm", 20, 20),
    ]
    verdicts = {
        (*factor._identity(row), factor._backend(row)): "compared-and-matched"
        for row in rows
    }
    verdicts[(*factor._identity(rows[1]), "kvm")] = "compared-and-diverged"
    report = factor.compare_rows(
        rows,
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
        ledger_verdicts=verdicts,
    )
    assert report["population"]["included_pairs"] == 1
    assert report["population"]["excluded_counts"] == {"ledger_verdict_not_matched": 1}
    assert report["population"]["exclusions"][0]["detail"] == {
        "baseline_state": "compared-and-matched",
        "comparison_state": "compared-and-diverged",
    }


def test_backend_selector_must_be_present_once_before_guest_arguments() -> None:
    for name in ("missing", "duplicate"):
        base = cell(name, "ptrace", 10, 10)
        other = cell(name, "kvm", 20, 20)
        for row in (base, other):
            assert isinstance(row["argv"], list)
            assert isinstance(row["effective_args"], list)
            if name == "missing":
                del row["argv"][1:3]
                del row["effective_args"][0:2]
                row["shell_command"] = row["shell_command"].replace(
                    f"--backend {row['backend']} ", ""
                )
            else:
                argv_separator = row["argv"].index("--")
                args_separator = row["effective_args"].index("--")
                row["argv"][argv_separator:argv_separator] = [
                    "--backend",
                    "liteinst",
                ]
                row["effective_args"][args_separator:args_separator] = [
                    "--backend",
                    "liteinst",
                ]
                row["shell_command"] = row["shell_command"].replace(
                    " -- ", " --backend liteinst -- "
                )
        report = compare(
            [base, other],
            baseline_backend="ptrace",
            comparison_backend="kvm",
            mode="verify",
        )
        assert report["population"]["included_pairs"] == 0
        assert report["population"]["exclusions"][0]["detail"] == {
            "baseline_fields": ["backend_selector"],
            "comparison_fields": ["backend_selector"],
        }


def test_guest_backend_argument_is_not_normalized() -> None:
    base = cell("guest-option", "ptrace", 10, 10)
    other = cell("guest-option", "kvm", 20, 20)
    assert isinstance(base["argv"], list)
    assert isinstance(base["effective_args"], list)
    assert isinstance(other["argv"], list)
    assert isinstance(other["effective_args"], list)
    base["argv"].append("--backend=ptrace")
    base["effective_args"].append("--backend=ptrace")
    base["shell_command"] += " --backend=ptrace"
    other["argv"].append("--backend=kvm")
    other["effective_args"].append("--backend=kvm")
    other["shell_command"] += " --backend=kvm"
    report = compare(
        [base, other],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 0
    assert report["population"]["exclusions"][0]["detail"] == {
        "fields": ["argv", "effective_args", "shell_command"]
    }


def test_naked_rows_still_require_the_recorded_direct_invocation() -> None:
    row = cell("naked", None, 10, 10, mode="naked")
    assert factor._invalid_pairing_evidence(row) == []
    row["argv"] = None
    row["effective_args"] = None
    row["shell_command"] = None
    assert factor._invalid_pairing_evidence(row) == [
        "argv",
        "effective_args",
        "shell_command",
    ]


def test_missing_invocation_evidence_and_noninteger_timing_are_excluded() -> None:
    missing_base = cell("missing", "ptrace", 10, 10)
    missing_other = cell("missing", "kvm", 20, 20)
    for row in (missing_base, missing_other):
        row["argv"] = None
        row["effective_args"] = None
        row["shell_command"] = None
    report = compare(
        [
            missing_base,
            missing_other,
            cell("infinite", "ptrace", 10, 10),
            cell("infinite", "kvm", float("inf"), 20),
            cell("fractional", "ptrace", 10, 10),
            cell("fractional", "kvm", 20.5, 20),
        ],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 0
    assert report["population"]["excluded_counts"] == {
        "invalid_provenance": 1,
        "missing_or_nonpositive_timing": 2,
    }
    assert report["population"]["exclusions"][2]["detail"] == {
        "baseline_fields": ["argv", "effective_args", "shell_command"],
        "comparison_fields": ["argv", "effective_args", "shell_command"],
    }


@pytest.mark.parametrize(
    "capabilities",
    [
        {},
        {
            "cpuid-faulting": {"present": 1, "evidence": "fixture"},
            "kvm": {"present": True, "evidence": "fixture"},
        },
        {
            "cpuid-faulting": {"present": True, "evidence": "fixture"},
            "kvm": {"present": True, "evidence": "fixture", "extra": "field"},
        },
    ],
)
def test_host_capabilities_require_the_closed_typed_shape(
    capabilities: dict[str, object],
) -> None:
    rows = [
        cell("capabilities", "ptrace", 10, 10, host_capabilities=capabilities),
        cell("capabilities", "kvm", 20, 20, host_capabilities=capabilities),
    ]
    report = compare(
        rows,
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    assert report["population"]["included_pairs"] == 0
    assert report["population"]["exclusions"][0]["detail"] == {
        "baseline_fields": ["host_capabilities"],
        "comparison_fields": ["host_capabilities"],
    }


def test_text_output_names_identity_fields_and_exclusion_detail(capsys) -> None:
    report = compare(
        [
            cell("good", "ptrace", 10, 10),
            cell("good", "kvm", 20, 20),
            cell("retry", "ptrace", 10, 10),
            cell("retry", "ptrace", 10, 10, attempt=2),
            cell("retry", "kvm", 20, 20),
        ],
        baseline_backend="ptrace",
        comparison_backend="kvm",
        mode="verify",
    )
    factor._print_text(
        {
            "run": {
                "run_id": RUN_ID,
                "run_number": 1,
                "commit": SHA,
                "tree": "b" * 40,
                "host": "devbig014",
                "raw_result_root": "ignored/validate/artifacts/test/e2e",
                "raw_result_rows": 5,
                "raw_result_digest": "c" * 64,
                "timing_binding": "fixture",
            },
            **report,
        }
    )
    output = capsys.readouterr().out
    assert "reason=retry_or_ambiguous lane=portable category=category" in output
    assert "test=category/retry mode=verify detail=" in output
    assert "cell=portable/category/category/retry/verify" not in output


def test_qualified_run_uses_current_correction(monkeypatch, tmp_path: Path) -> None:
    base = {
        "run_id": RUN_ID,
        "record_id": "original",
        "finished_at": "2026-09-04T16:47:39Z",
        "result": "pass",
        "executed_tests": 1,
        "gates_run": 1,
        "gates_expected": 1,
    }
    corrected = {**base, "record_id": "finalized", "corrects": "original"}
    monkeypatch.setattr(
        factor.qualified_rows.canonical_ledger,
        "rows",
        lambda *, root: [base, corrected],
    )
    monkeypatch.setattr(factor.qualified_rows, "qualified_rows", list)
    assert factor._qualified_run(tmp_path, RUN_ID)["record_id"] == "finalized"


def test_qualified_run_rejects_ambiguous_current_rows_before_qualification(
    monkeypatch, tmp_path: Path
) -> None:
    current = [
        {"run_id": RUN_ID, "record_id": "one"},
        {"run_id": RUN_ID, "record_id": "two"},
    ]
    monkeypatch.setattr(
        factor.qualified_rows.canonical_ledger,
        "rows",
        lambda *, root: current,
    )
    monkeypatch.setattr(
        factor.qualified_rows.canonical_ledger,
        "current_validation_run_rows",
        lambda rows: rows,
    )
    monkeypatch.setattr(
        factor.qualified_rows,
        "qualified_rows",
        lambda rows: [rows[0]],
    )
    with pytest.raises(factor.BackendFactorError, match="2 current ledger rows"):
        factor._qualified_run(tmp_path, RUN_ID)


def test_raw_results_must_have_exact_compact_identity_set(tmp_path: Path) -> None:
    compact = (
        tmp_path / "ignored" / "validate" / "artifacts" / RUN_ID / "cell-results.jsonl"
    )
    compact.parent.mkdir(parents=True)
    compact_row = {
        "run_id": RUN_ID,
        "hermit_sha": SHA,
        "source_tree_dirty": False,
        "lane": "portable",
        "category": "category",
        "test": "category/one",
        "mode": "verify",
        "backend": "ptrace",
        "cell_verdict": {"state": "compared-and-matched"},
    }
    compact_raw = (json.dumps(compact_row) + "\n").encode()
    compact.write_bytes(compact_raw)
    ledger_row = {
        "commit": SHA,
        "host": "devbig014.example.com",
        "cell_results": {
            "run_id": RUN_ID,
            "artifact": {
                "path": compact.relative_to(tmp_path).as_posix(),
                "row_count": 1,
                "sha256": hashlib.sha256(compact_raw).hexdigest(),
            },
        },
    }
    compact_path, verdicts = factor._compact_verdicts(tmp_path, ledger_row, RUN_ID)
    assert verdicts == {
        ("portable", "category", "category/one", "verify", "ptrace"): (
            "compared-and-matched"
        )
    }
    results = compact.parent / "e2e" / "portable" / "manifest" / "results.jsonl"
    results.parent.mkdir(parents=True)
    results.write_text(json.dumps(cell("one", "ptrace", 10, 10)) + "\n")

    loaded, sources, digest = factor._raw_results(
        compact_path, ledger_row, RUN_ID, set(verdicts)
    )
    assert len(loaded) == 1
    assert sources[0]["path"] == "portable/manifest/results.jsonl"
    assert len(digest) == 64

    with results.open("a") as handle:
        handle.write(json.dumps(cell("extra", "ptrace", 10, 10)) + "\n")
    with pytest.raises(factor.BackendFactorError, match="identities differ"):
        factor._raw_results(compact_path, ledger_row, RUN_ID, set(verdicts))


def test_raw_results_reject_stale_binary_and_e2e_root_escape(tmp_path: Path) -> None:
    compact = (
        tmp_path / "ignored" / "validate" / "artifacts" / RUN_ID / "cell-results.jsonl"
    )
    compact.parent.mkdir(parents=True)
    compact.write_text("{}\n")
    outside = tmp_path / "outside"
    outside.mkdir()
    (compact.parent / "e2e").symlink_to(outside, target_is_directory=True)
    ledger_row = {"commit": SHA, "host": "devbig014"}
    with pytest.raises(factor.BackendFactorError, match="outside the retained run"):
        factor._raw_results(compact, ledger_row, RUN_ID, set())

    (compact.parent / "e2e").unlink()
    results = compact.parent / "e2e" / "portable" / "manifest" / "results.jsonl"
    results.parent.mkdir(parents=True)
    stale = cell("one", "ptrace", 10, 10, binary_build_sha="d" * 12)
    results.write_text(json.dumps(stale) + "\n")
    identity = (*factor._identity(stale), factor._backend(stale))
    with pytest.raises(factor.BackendFactorError, match="absent, dirty, or stale"):
        factor._raw_results(compact, ledger_row, RUN_ID, {identity})

    stale["binary_build_sha"] = None
    results.write_text(json.dumps(stale) + "\n")
    with pytest.raises(factor.BackendFactorError, match="absent, dirty, or stale"):
        factor._raw_results(compact, ledger_row, RUN_ID, {identity})

    stale["binary_build_sha"] = SHA[:12]
    stale["machine_shortname"] = "different-host"
    results.write_text(json.dumps(stale) + "\n")
    with pytest.raises(factor.BackendFactorError, match="host differs"):
        factor._raw_results(compact, ledger_row, RUN_ID, {identity})
