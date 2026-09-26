#!/usr/bin/env python3
"""Brackets for the `anchor_select` clauses found LIVE BUT UNTESTED.

The adversarial review (ai_docs/phase2-tightening-guards-adversarial-review-20260805.md)
mutation-tested the former local `row_qualifies` and `_coverage_satisfied`:
killing any of the clauses below left the existing suite green, so nothing held
them in place. Each
was then shown to be LIVE -- it rejects an input that no other clause rejects --
so they are missing tests, not dead code.

Every case here is built from a row that QUALIFIES at baseline and perturbs
exactly one field, so the refusal is attributable to the named clause and the
bracket cannot pass vacuously. The `test_baseline_row_qualifies` positive control
is what makes that attribution honest: if the baseline ever stops qualifying,
every negative below would pass for the wrong reason.
"""

from __future__ import annotations

import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import anchor_select as A  # noqa: E402

_VALIDATE = Path(__file__).resolve().parents[1]
CASES = json.loads((_VALIDATE / "tests" / "fixtures" / "release-payload-cases.json").read_text())[
    "cases"
]

PREDICATE = {
    "require": {
        "commit_anchored": True,
        "tree_dirty": False,
        "profile": "full",
        "selection_mode": "full",
        "release_builder": "cargo",
        "result": "pass",
        "failures_max": 0,
        "executed_tests_min": 1,
    },
    "counts_schema": 5,
    "passed_tests": {"applies_at_schema_min": 4},
    "count_representation": {
        "applies_at_schema_min": 4,
        "fields": ["executed_tests", "passed_tests", "filtered_tests"],
        "signed_bits": 64,
    },
    "coverage": {"applies_at_schema_min": 5, "per_node": True},
    "cell_results": {"applies_at_schema_min": 6, "supported_schema_max": 7},
    "producer": {
        "required": True,
        "applies_from_finished_at": None,
        "known": ["hermit-validate-sh"],
    },
    "base": {"applies_at_schema_min": 5, "branch": "main"},
    "release_payloads": json.loads((_VALIDATE / "qualifying-receipt.json").read_text())[
        "release_payloads"
    ],
    "release_builder_required_from_finished_at": json.loads(
        (_VALIDATE / "qualifying-receipt.json").read_text()
    )["release_builder_required_from_finished_at"],
    "admission": {
        "applies_at_schema_min": 5,
        "required_admission": "ci-hub-validate-lock",
        "required_concurrent_validates": 0,
        "required_concurrency_proof": "validate_lock_owner_ancestry",
        "require_registered_producer": True,
    },
}


def _apply_case(row: dict, case: dict) -> dict:
    """Apply a release-payload-cases.json case: drop the pair and every absent
    key, then set exactly the case's fields."""
    for key in ("release_builder", "e2e_payload", *case.get("absent", ())):
        row.pop(key, None)
    row.update(case["fields"])
    return row


def _row(**over: object) -> dict:
    row: dict[str, object] = {
        "commit": "a" * 40,
        "commit_anchored": True,
        "tree_dirty": False,
        "profile": "full",
        "selection_mode": "full",
        "result": "pass",
        "failures": 0,
        "checks": 1,
        "gates_run": 1,
        "gates_expected": 1,
        "gates": [{"name": "test", "result": "pass", "exit_code": 0}],
        "executed_tests": 100,
        "passed_tests": 100,
        "schema_version": 5,
        "finished_at": "2026-08-08T09:00:00Z",
        "producer": "hermit-validate-sh",
        "base_sha": "b" * 40,
        "base_tree": "c" * 40,
        "reverie_base_sha": "d" * 40,
        "reverie_base_tree": "e" * 40,
        "admission": "ci-hub-validate-lock",
        "concurrent_validates": 0,
        "concurrency_proof": "validate_lock_owner_ancestry",
        "coverage": {
            "planned_test_nodes": 10,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        },
    }
    row.update(over)
    return row


def test_baseline_row_qualifies() -> None:
    """POSITIVE CONTROL -- without this the negatives below prove nothing."""
    assert A.row_qualifies(_row(), PREDICATE) == (True, "qualifies")


# --- row_qualifies -----------------------------------------------------------


def test_receipt_without_a_usable_commit_is_refused() -> None:
    """The worst of the set: this would let `commit == "unknown"` be selected
    as a green ANCHOR, i.e. inherit a green from a receipt bound to nothing."""
    for missing in ("unknown", "", None):
        ok, reason = A.row_qualifies(_row(commit=missing), PREDICATE)
        assert (ok, reason) == (False, "no-commit"), missing


def test_non_cargo_release_builder_is_refused() -> None:
    """A Buck full run executed the release payload, not the debug binary a
    receipt certifies. The baseline carries neither field and finished before
    the pair was written, so it qualifies as a Cargo run and each refusal here
    belongs to the release_builder clause."""
    assert sum(case["expected"] is not None for case in CASES) == 7
    for case in CASES:
        row = _apply_case(_row(), case)
        expected = (
            (True, "qualifies")
            if case["expected"] == "cargo"
            else (False, f"release_builder={row.get('release_builder')!r}")
        )
        assert A.row_qualifies(row, PREDICATE) == expected, case["label"]


def test_failures_above_the_cap_are_refused() -> None:
    ok, reason = A.row_qualifies(_row(failures=5), PREDICATE)
    assert ok is False
    assert reason == "failures=5"


def test_count_capable_receipt_without_executed_tests_is_refused() -> None:
    """schema >= counts_schema: the receipt CAN carry counts, so a missing count
    is a defect rather than an old producer."""
    ok, reason = A.row_qualifies(_row(executed_tests=None), PREDICATE)
    assert ok is False
    assert reason == "passed_tests != executed_tests for passing row"


def test_pre_count_receipt_without_executed_tests_is_refused() -> None:
    """schema < counts_schema: an old receipt cannot PROVE nonzero execution, so
    it is refused rather than trusted."""
    # coverage=None because _row defaults to a schema-5 shape that CARRIES
    # coverage, and since d58edca a pre-count schema paired with a non-null
    # coverage object is refused earlier, by the coverage-schema clause. Leaving
    # it set would still refuse the row but for the WRONG reason, so this clause
    # would go untested while looking green.
    ok, reason = A.row_qualifies(
        _row(
            schema_version=1,
            executed_tests=None,
            passed_tests=None,
            coverage=None,
        ),
        PREDICATE,
    )
    assert ok is False
    assert reason == "pre-count receipt cannot prove nonzero execution"


# --- canonical coverage authority through anchor selection ------------------


def test_coverage_without_planned_test_nodes_is_refused() -> None:
    for cov in (
        {"zero_executed_nodes": [], "absent_nodes": []},          # field absent
        {"planned_test_nodes": 0, "zero_executed_nodes": [], "absent_nodes": []},
        {"planned_test_nodes": "10", "zero_executed_nodes": [], "absent_nodes": []},
    ):
        ok, reason = A.row_qualifies(_row(coverage=cov), PREDICATE)
        assert ok is False, cov
        assert reason == "count-capable receipt coverage unavailable", cov


def test_coverage_planned_count_outside_u64_is_unavailable() -> None:
    for planned in (-1, 0, True, 1 << 64):
        cov = {
            "planned_test_nodes": planned,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        }
        ok, reason = A.row_qualifies(_row(coverage=cov), PREDICATE)
        assert ok is False, cov
        assert reason == "count-capable receipt coverage unavailable", cov
        assert (
            A.qualifying_receipt.coverage_verdict(cov)
            is A.qualifying_receipt.CoverageVerdict.UNAVAILABLE
        )


def test_coverage_executed_count_outside_u64_is_unavailable() -> None:
    for executed in ("4", 1.5, True, -1, 1 << 64, None):
        cov = {
            "planned_test_nodes": 10,
            "executed_test_nodes": executed,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        }
        ok, reason = A.row_qualifies(_row(coverage=cov), PREDICATE)
        assert ok is False, cov
        assert reason == "count-capable receipt coverage unavailable", cov
        assert (
            A.qualifying_receipt.coverage_verdict(cov)
            is A.qualifying_receipt.CoverageVerdict.UNAVAILABLE
        )


def test_coverage_executed_count_accepts_rust_u64_domain() -> None:
    for executed in (None, 0, (1 << 64) - 1):
        cov = {
            "planned_test_nodes": 10,
            "zero_executed_nodes": [],
            "absent_nodes": [],
        }
        if executed is not None:
            cov["executed_test_nodes"] = executed
        assert (
            A.qualifying_receipt.coverage_verdict(cov)
            is A.qualifying_receipt.CoverageVerdict.SATISFIED
        ), cov
        assert A.row_qualifies(_row(coverage=cov), PREDICATE) == (
            True,
            "qualifies",
        ), cov


def test_non_string_failure_list_entries_are_unavailable() -> None:
    malformed_values = (None, False, 7, 1.5, {}, [])
    for field in ("zero_executed_nodes", "absent_nodes"):
        for value in malformed_values:
            cov = {
                "planned_test_nodes": 10,
                "zero_executed_nodes": [],
                "absent_nodes": [],
            }
            cov[field] = [value]
            ok, reason = A.row_qualifies(_row(coverage=cov), PREDICATE)
            assert ok is False, (field, value)
            assert reason == "count-capable receipt coverage unavailable"
            assert (
                A.qualifying_receipt.coverage_verdict(cov)
                is A.qualifying_receipt.CoverageVerdict.UNAVAILABLE
            )


def test_coverage_with_zero_executed_nodes_is_refused() -> None:
    """A node that discovered tests and executed none is a no-result wearing a
    success badge; completeness is COVERAGE, not a total count."""
    cov = {"planned_test_nodes": 10, "zero_executed_nodes": ["node-a"], "absent_nodes": []}
    ok, reason = A.row_qualifies(_row(coverage=cov), PREDICATE)
    assert ok is False
    assert reason == "count-capable receipt coverage unsatisfied"


def test_coverage_without_absent_nodes_is_unavailable() -> None:
    cov = {"planned_test_nodes": 10, "zero_executed_nodes": []}
    ok, reason = A.row_qualifies(_row(coverage=cov), PREDICATE)
    assert ok is False
    assert reason == "count-capable receipt coverage unavailable"


def test_coverage_with_absent_nodes_is_unsatisfied() -> None:
    cov = {
        "planned_test_nodes": 10,
        "zero_executed_nodes": [],
        "absent_nodes": ["node-b"],
    }
    ok, reason = A.row_qualifies(_row(coverage=cov), PREDICATE)
    assert ok is False
    assert reason == "count-capable receipt coverage unsatisfied"


def test_coverage_clauses_are_independently_reachable() -> None:
    """Guard against one clause masking another: each perturbation alone flips
    the verdict, so none of the brackets above is riding on a shared failure."""
    assert A.row_qualifies(_row(), PREDICATE)[0] is True
    for coverage in (
        {"planned_test_nodes": 10, "zero_executed_nodes": [],
         "absent_nodes": ["node-b"]},
        "not-a-dict",
        None,
    ):
        assert A.row_qualifies(_row(coverage=coverage), PREDICATE)[0] is False
