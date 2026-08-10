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
