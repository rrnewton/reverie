#!/usr/bin/env python3
"""Mutation brackets for the canonical qualified ledger view."""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import qualified_rows as qr


def _row(name: str, finished: str, **overrides: object) -> dict[str, object]:
    row: dict[str, object] = {
        "commit": name,
        "finished_at": finished,
        "result": "pass",
        "executed_tests": 10,
        "gates_run": 5,
        "gates_expected": 5,
    }
    row.update(overrides)
    return row


def test_qualified_rows_sort_by_event_time_not_file_position() -> None:
    late = _row("late", "2026-08-04T12:00:00Z")
    early = _row("early", "2026-08-04T10:00:00Z")
    assert [row["commit"] for row in qr.qualified_rows([late, early])] == [
        "early",
        "late",
    ]


def test_truncated_and_zero_executed_rows_do_not_qualify() -> None:
    truncated = _row("truncated", "2026-08-04T10:00:00Z", gates_run=2)
    inert = _row("inert", "2026-08-04T10:01:00Z", executed_tests=0)
    assert qr.qualified_rows([truncated, inert]) == []


def test_non_cargo_release_payload_never_qualifies() -> None:
    absent = _row("absent", "2026-08-04T10:00:00Z")
    cargo = _row("cargo", "2026-08-04T10:01:00Z", release_builder="cargo")
    assert [row["commit"] for row in qr.qualified_rows([absent, cargo])] == [
        "absent",
        "cargo",
    ]
    for builder in ("buck", None, 7, "Cargo"):
        row = _row("other", "2026-08-04T10:02:00Z", release_builder=builder)
        assert qr.qualified_rows([row]) == [], builder


def test_missing_conditions_fail_closed() -> None:
    missing_counts = _row("missing-counts", "2026-08-04T10:00:00Z")
    missing_counts.pop("gates_expected")
    missing_time = _row("missing-time", "")
    assert qr.qualified_rows([missing_counts, missing_time]) == []


def test_current_full_schema_requires_declared_gate_contract() -> None:
    current = _row("current", "2026-08-04T10:00:00Z", schema_version=5)
    current["profile"] = "full"
    current["checks"] = current.pop("gates_run")
    current.pop("gates_expected")
    assert qr.qualified_rows([current]) == []


def test_typed_empty_bucket_qualifies_without_counting_as_a_pass() -> None:
    current = _row(
        "typed-skip",
        "2026-08-04T10:00:00Z",
        gates_run=4,
        gates_expected=5,
        skipped_nodes=1,
        intentional_skipped_nodes=[
            {
                "name": "privileged-e2e.manifest_applications",
                "reason": "empty-manifest-bucket",
            }
        ],
        dependency_skipped_nodes=[],
        unaccounted_nodes=[],
    )
    assert qr.qualified_rows([current]) == [current]
    assert current["gates_run"] == 4


def test_over_run_pass_still_qualifies() -> None:
    # OVER-RUN bracket: the live plan can run more gates than the hardcoded
    # FULL_GATES_EXPECTED fallback (six live gates against a stale expectation of
    # five). A 6/5 PASS is a COMPLETE green, not a short run, and must not be
    # discarded by an equality test — the exact green the old `ran == expected`
    # bug excluded from the qualified population.
    over = _row("over-run", "2026-08-04T10:00:00Z", gates_run=6, gates_expected=5)
    assert qr.qualified_rows([over]) == [over]


def test_n_equals_three_legitimate_passes_remain_qualified() -> None:
    rows = [
        _row("a", "2026-08-04T10:00:00Z"),
        _row("b", "2026-08-04T10:01:00Z"),
        _row("c", "2026-08-04T10:02:00Z"),
    ]
    assert qr.qualified_rows(rows) == rows


# --- Brackets added after a mutation sweep found four unprotected clauses. ---
# Each mutation below (delete the clause / weaken the comparison) previously left
# the whole file green. A guard clause with no failing mutant is decoration: it
# can be deleted in a refactor and nothing reports it.


def test_non_pass_results_never_qualify() -> None:
    """The headline claim: only a PASS counts as green.

    This is the fake-green direction the accessor exists to prevent, and it was
    the one clause with NO coverage — deleting `result == "pass"` from
    `is_qualified` left the suite fully green. Every row here is otherwise
    perfect (complete gate contract, positive executed count, parseable time),
    so only the result field can disqualify them.
    """
    rejected = [
        _row("failed", "2026-08-04T10:00:00Z", result="fail"),
        _row("timed-out", "2026-08-04T10:01:00Z", result="timeout"),
        _row("truncated", "2026-08-04T10:02:00Z", result="truncated"),
        # `pass-partial` is `flake_class.effective_result`'s downgrade for a pass
        # that did not cover the full profile. It contains the substring "pass";
        # a prefix/`in` test instead of equality would wrongly admit it.
        _row("partial", "2026-08-04T10:03:00Z", result="pass-partial"),
        _row("no-result", "2026-08-04T10:04:00Z", result="no_result"),
        _row("empty", "2026-08-04T10:05:00Z", result=""),
        _row("wrong-case", "2026-08-04T10:06:00Z", result="PASS"),
    ]
    assert qr.qualified_rows(rejected) == []

    missing_result = _row("missing", "2026-08-04T10:07:00Z")
    missing_result.pop("result")
    assert qr.qualified_rows([missing_result]) == []

    # Positive control: the identical row shape with `result == "pass"` DOES
    # qualify, so the refusals above are the result field and not some other
    # defect in the fixture.
    assert qr.qualified_rows([_row("ok", "2026-08-04T10:08:00Z")]) != []


def test_boolean_executed_tests_does_not_qualify() -> None:
    """`bool` is a subclass of `int` in Python, so `True > 0` is True.

    Without the explicit `not isinstance(executed, bool)` guard a row recording
    `executed_tests: true` — a producer type error, not a measurement — would
    satisfy a naive `isinstance(x, int) and x > 0` check and be counted as a
    green with an unknown test count.
    """
    assert qr.qualified_rows([_row("bool", "2026-08-04T10:00:00Z", executed_tests=True)]) == []
    assert qr.qualified_rows([_row("bool0", "2026-08-04T10:01:00Z", executed_tests=False)]) == []
    # Positive control: a real integer count still qualifies.
    assert qr.qualified_rows([_row("int", "2026-08-04T10:02:00Z", executed_tests=1)]) != []


def test_zero_expected_gate_contract_does_not_qualify() -> None:
    """`expected > 0` is load-bearing: without it `0 >= 0` admits a vacuous row.

    A row claiming zero expected gates has no gate contract to satisfy, so
    `ran >= expected` is trivially true. That is the "green over no work at all"
    no-result, and it must fail closed.
    """
    vacuous = _row("vacuous", "2026-08-04T10:00:00Z", gates_run=0, gates_expected=0)
    assert qr.qualified_rows([vacuous]) == []


def test_equal_event_times_sort_deterministically() -> None:
    """Rows sharing a `finished_at` must not fall back to file position.

    Concurrent slots routinely finish inside the same recorded second. Without
    the commit/slot/log_file tie-breakers the order of an equal-timestamp group
    is whatever order the reader happened to see, which is exactly the
    position-not-event-time defect this accessor exists to remove.
    """
    same = "2026-08-04T10:00:00Z"
    forward = [_row("c", same), _row("a", same), _row("b", same)]
    reverse = list(reversed(forward))
    assert [row["commit"] for row in qr.qualified_rows(forward)] == ["a", "b", "c"]
    assert qr.qualified_rows(forward) == qr.qualified_rows(reverse)


def test_naive_timestamp_fails_closed() -> None:
    """A timestamp without a zone cannot be ordered against zoned rows."""
    naive = _row("naive", "2026-08-04T10:00:00")
    unparseable = _row("junk", "not-a-timestamp")
    assert qr.event_time(naive) is None
    assert qr.qualified_rows([naive, unparseable]) == []
    # Positive control: the same instant WITH a zone parses and qualifies.
    assert qr.event_time(_row("zoned", "2026-08-04T10:00:00Z")) is not None


def test_load_rows_counts_malformed_and_skips_non_dict(tmp_path) -> None:
    """Malformed input is counted, never silently folded into the population."""
    ledger = tmp_path / "ledger.jsonl"
    ledger.write_text(
        "\n".join(
            [
                json.dumps(_row("good", "2026-08-04T10:00:00Z")),
                "",  # blank lines are skipped without counting
                "{not json",
                json.dumps(["a", "list", "is", "not", "a", "row"]),
                json.dumps(_row("also-good", "2026-08-04T10:01:00Z")),
            ]
        )
        + "\n"
    )
    rows, malformed = qr.load_rows(ledger)
    assert len(rows) == 2
    assert malformed == 2
    assert [row["commit"] for row in qr.qualified_rows(rows)] == ["good", "also-good"]


def test_load_rows_accepts_an_explicit_canonical_ledger_directory(
    tmp_path, monkeypatch
) -> None:
    state_root = tmp_path / "state-root"
    ledger = state_root / "ledger"
    ledger.mkdir(parents=True)
    seen = []
    monkeypatch.setattr(
        qr.canonical_ledger,
        "rows",
        lambda *, root=None, preserve_admission=False: seen.append(root) or [{"commit": "state-row"}],
    )

    rows, malformed = qr.load_rows(ledger)

    assert rows == [{"commit": "state-row"}]
    assert malformed == 0
    assert seen == [state_root.resolve()]
