#!/usr/bin/env python3
"""Solo-rerun evidence controls; verdicts remain validate_status's job."""

from __future__ import annotations

import importlib.util
from pathlib import Path


PATH = Path(__file__).with_name("failure_evidence.py")
SPEC = importlib.util.spec_from_file_location("failure_evidence", PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


LOG = "[test.command_strict_verify] ✗ FAIL   command strict verification\n"
COMMIT = "3a4048791b8035375bf7e90b0ced149dfaa3adf1"


def prior(**updates: object) -> dict[str, object]:
    row: dict[str, object] = {
        "commit": COMMIT,
        "dag_jobs": 16,
        "concurrent_validates": 0,
        "known_flaky_failure": True,
        "gates": [
            {
                "result": "fail",
                "failure_origin": "lane_substep",
                "failed_substeps": ["test.command_strict_verify"],
            }
        ],
    }
    row.update(updates)
    return row


def test_measured_flake_is_bound_to_failed_cell() -> None:
    evidence = MODULE.build_evidence(
        log_text=LOG,
        registry={"test.command_strict_verify"},
        prior=[],
        commit=COMMIT,
        dag_jobs=16,
        concurrent_validates=0,
    )
    assert evidence == {
        "failed_substeps": ["test.command_strict_verify"],
        "failed_substep_classes": [
            {
                "node": "test.command_strict_verify",
                "group": "test",
                "sub_step_class": "lane-run",
                "fault_class": "code",
                "infra_signature": None,
                "host_env_signature": None,
                "first_error_line": "✗ FAIL   command strict verification",
                "known_flaky": True,
                "timing": {
                    "wall_seconds": None,
                    "cpu_seconds": None,
                    "cpu_user_seconds": None,
                    "cpu_sys_seconds": None,
                    "cpu_usage_usec": None,
                    "cpu_source": "unavailable:no-run-bound-cgroup-usage",
                    "cpu_per_wall": None,
                    "timed_out": None,
                    "cpu_timed_out": None,
                    "throttled_seconds": None,
                    "quota_utilization_pct": None,
                    "co_tenants_end": None,
                    "timing_verdict": None,
                    "timing_source": "unavailable:no-observable-run-identity",
                    "profile_rows_matched": 0,
                    "profile_candidate_rows": 0,
                    "profile_row_timestamp": None,
                },
            }
        ],
        "first_error_line": "✗ FAIL   command strict verification",
        "flaky_failed_substeps": ["test.command_strict_verify"],
        "known_flaky_failure": True,
        "solo_rerun_confirmation": False,
        "solo_rerun_of": None,
    }


def test_only_matching_solo_j4_run_confirms() -> None:
    base = dict(
        log_text=LOG,
        registry={"test.command_strict_verify"},
        prior=[prior()],
        commit=COMMIT,
    )
    confirmed = MODULE.build_evidence(**base, dag_jobs=4, concurrent_validates=0)
    assert confirmed["solo_rerun_confirmation"] is True
    assert confirmed["solo_rerun_of"] == {"finished_at": None, "log_file": None}
    assert MODULE.build_evidence(
        **base, dag_jobs=16, concurrent_validates=0
    )["solo_rerun_confirmation"] is False
    assert MODULE.build_evidence(
        **base, dag_jobs=4, concurrent_validates=1
    )["solo_rerun_confirmation"] is False


def test_current_outer_gate_shape_confirms_by_its_gate_name() -> None:
    old = prior(
        gates=[
            {
                "name": "test.command_strict_verify",
                "result": "fail",
                "failure_origin": "outer_gate",
                "failed_substeps": [],
            }
        ]
    )

    evidence = MODULE.build_evidence(
        log_text=LOG,
        registry={"test.command_strict_verify"},
        prior=[old],
        commit=COMMIT,
        dag_jobs=4,
        concurrent_validates=0,
    )

    assert evidence["solo_rerun_confirmation"] is True


def test_only_coherent_failure_bindings_supply_identity() -> None:
    invalid_gates = [
        {
            "name": "outer-missing",
            "result": "fail",
            "failure_origin": "outer_gate",
        },
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
            "name": "lane-pass",
            "result": "pass",
            "failure_origin": "lane_substep",
            "failed_substeps": ["test.child"],
        },
        {
            "name": "lane-aborted",
            "result": "fail",
            "aborted": True,
            "failure_origin": "lane_substep",
            "failed_substeps": ["test.child"],
        },
        {
            "name": "unbound-list",
            "result": "fail",
            "failed_substeps": ["test.child"],
        },
        {
            "name": "aborted-outer",
            "result": "fail",
            "aborted": True,
            "failure_origin": "outer_gate",
            "failed_substeps": [],
        },
    ]
    assert MODULE.row_failed_substeps({"gates": invalid_gates}) == set()

    assert MODULE.row_failed_substeps(
        {
            "gates": [
                {
                    "name": "aggregate",
                    "result": "fail",
                    "failure_origin": "lane_substep",
                    "failed_substeps": ["test.child"],
                }
            ]
        }
    ) == {"test.child"}

    assert MODULE.row_failed_substeps(
        {
            "gates": [
                {
                    "name": "legacy-aggregate",
                    "kind": "timeout",
                    "failure_origin": "lane_substep",
                    "failed_substeps": ["test.legacy-child"],
                }
            ]
        }
    ) == {"test.legacy-child"}


def test_different_commit_or_cell_never_confirms() -> None:
    rows = [prior(commit="b" * 40), prior(gates=[{"failed_substeps": ["test.other"]}])]
    evidence = MODULE.build_evidence(
        log_text=LOG,
        registry={"test.command_strict_verify"},
        prior=rows,
        commit=COMMIT,
        dag_jobs=4,
        concurrent_validates=0,
    )
    assert evidence["solo_rerun_confirmation"] is False


def test_surviving_preclassification_log_can_confirm_first_solo_rerun(
    tmp_path: Path,
) -> None:
    prior_log = tmp_path / "wide.log"
    prior_log.write_text(LOG)
    old = prior(
        known_flaky_failure=None,
        gates=[{"name": "test.command_strict_verify", "result": "fail"}],
        log_file=str(prior_log),
    )

    evidence = MODULE.build_evidence(
        log_text=LOG,
        registry={"test.command_strict_verify"},
        prior=[old],
        commit=COMMIT,
        dag_jobs=4,
        concurrent_validates=0,
    )

    assert evidence["solo_rerun_confirmation"] is True
    assert evidence["solo_rerun_of"] == {
        "finished_at": None,
        "log_file": str(prior_log),
    }


def test_missing_prior_log_cannot_confirm_a_solo_rerun(tmp_path: Path) -> None:
    old = prior(
        known_flaky_failure=None,
        gates=[{"name": "test.command_strict_verify", "result": "fail"}],
        log_file=str(tmp_path / "missing.log"),
    )

    evidence = MODULE.build_evidence(
        log_text=LOG,
        registry={"test.command_strict_verify"},
        prior=[old],
        commit=COMMIT,
        dag_jobs=4,
        concurrent_validates=0,
    )

    assert evidence["solo_rerun_confirmation"] is False


def cell_results(test: str) -> dict[str, object]:
    return {
        "cells": [
            {
                "lane": "portable",
                "category": "applications",
                "test": test,
                "mode": "verify",
                "backend": "ptrace",
                "cell_verdict": {
                    "state": "compared-and-diverged",
                    "comparison_tier": "canonical-bitwise",
                },
            }
        ]
    }


def test_aggregate_node_sibling_does_not_confirm_an_exact_cell() -> None:
    current_test = "applications/git-repository-workflow"
    current_cell = f"{current_test}/verify/ptrace"
    old = prior(
        gates=[
            {
                "name": "e2e.manifest_applications",
                "result": "fail",
                "failure_origin": "outer_gate",
                "failed_substeps": [],
            }
        ],
        failed_cells=["applications/c-toolchain-workflow/verify/ptrace"],
    )
    evidence = MODULE.build_evidence(
        log_text="[e2e.manifest_applications] ✗ FAIL   applications\n",
        registry={current_cell},
        prior=[old],
        commit=COMMIT,
        dag_jobs=4,
        concurrent_validates=0,
        cell_results=cell_results(current_test),
    )
    assert evidence["solo_rerun_confirmation"] is False


def test_same_exact_cell_can_confirm_a_prior_aggregate_failure() -> None:
    current_test = "applications/git-repository-workflow"
    current_cell = f"{current_test}/verify/ptrace"
    old = prior(
        gates=[
            {
                "name": "e2e.manifest_applications",
                "result": "fail",
                "failure_origin": "outer_gate",
                "failed_substeps": [],
            }
        ],
        failed_cells=[current_cell],
    )
    evidence = MODULE.build_evidence(
        log_text="[e2e.manifest_applications] ✗ FAIL   applications\n",
        registry={current_cell},
        prior=[old],
        commit=COMMIT,
        dag_jobs=4,
        concurrent_validates=0,
        cell_results=cell_results(current_test),
    )
    assert evidence["solo_rerun_confirmation"] is True
