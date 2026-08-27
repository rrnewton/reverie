from __future__ import annotations

import copy
import sys
from pathlib import Path

import pytest


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import flaky_cells  # noqa: E402
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "series"))
import series  # noqa: E402


TREE_A = "a" * 40
TREE_B = "b" * 40
CELL = "applications/git-repository-workflow/verify/ptrace"
ROOT = Path(__file__).resolve().parents[3]


def row(
    outcome: str,
    *,
    tree: str = TREE_A,
    run_index: int = 0,
    num_runs: int = 1,
    dirty: bool = False,
) -> dict:
    return {
        "schema": series.ENVELOPE_VERSION,
        "event_id": f"event-{tree}-{run_index}-{outcome}",
        "event_type": "series.observation",
        "emitted_at": "2026-08-27T02:59:31Z",
        "team": "hermit",
        "host": "devbig014",
        "run_id": "campaign-1",
        "producer": "pressure-test",
        "series": {
            "cell": CELL,
            "tree": tree,
            "run_index": run_index,
            "num_runs": num_runs,
            "outcome": outcome,
            "source_tree_dirty": dirty,
            "machine_shortname": "devbig014",
            "kernel_version": "7.1.3-test",
            "host_capabilities": {
                "cpuid-faulting": {"present": False, "evidence": "fixture absence"},
                "kvm": {"present": True, "evidence": "fixture presence"},
            },
        },
    }


def test_attempt_order_does_not_change_the_classification() -> None:
    fail_then_pass = flaky_cells.derive(
        [row("diverged", run_index=1), row("passed", run_index=2)]
    )
    pass_then_fail = flaky_cells.derive(
        [row("passed", run_index=1), row("diverged", run_index=2)]
    )
    assert fail_then_pass == pass_then_fail
    assert fail_then_pass["cells"][0]["observed_pass"] == 1
    assert fail_then_pass["cells"][0]["observed_fail"] == 1
    assert fail_then_pass["cells"][0]["sample_size"] == 2


def test_pass_and_failure_from_different_trees_do_not_make_a_flaky_cell() -> None:
    registry = flaky_cells.derive(
        [row("passed", tree=TREE_A), row("diverged", tree=TREE_B)]
    )
    assert registry["cells"] == []


def test_non_comparisons_and_dirty_rows_are_excluded_from_the_sample() -> None:
    rows = [
        row("passed", run_index=1),
        row("diverged", run_index=2),
        row("no_result", run_index=3, num_runs=100),
        row("timeout", run_index=103, num_runs=100),
        row("errored", run_index=203, num_runs=100),
        row("skipped", run_index=303, num_runs=100),
        row("diverged", run_index=403, num_runs=100, dirty=True),
    ]
    [entry] = flaky_cells.derive(rows)["cells"]
    assert entry["observed_pass"] == 1
    assert entry["observed_fail"] == 1
    assert entry["sample_size"] == 2
    assert "failure_rate" not in entry


def test_pass_and_no_result_do_not_enter_the_registry() -> None:
    registry = flaky_cells.derive(
        [row("passed", run_index=1), row("no_result", run_index=2)]
    )
    assert registry["cells"] == []


def test_compressed_rows_contribute_the_number_of_represented_runs() -> None:
    [entry] = flaky_cells.derive(
        [
            row("passed", run_index=1, num_runs=15),
            row("diverged", run_index=16, num_runs=5),
        ]
    )["cells"]
    assert entry["observed_pass"] == 15
    assert entry["observed_fail"] == 5
    assert entry["sample_size"] == 20


def test_registry_is_bound_to_the_exact_tree() -> None:
    registry = flaky_cells.derive(
        [row("passed", run_index=1), row("diverged", run_index=2)]
    )
    assert flaky_cells.cells_for_tree(registry, TREE_A) == {CELL}
    assert flaky_cells.cells_for_tree(registry, TREE_B) == set()


def test_registry_refuses_a_sample_size_that_implies_more_evidence() -> None:
    registry = flaky_cells.derive(
        [row("passed", run_index=1), row("diverged", run_index=2)]
    )
    inflated = copy.deepcopy(registry)
    inflated["cells"][0]["sample_size"] = 29
    with pytest.raises(ValueError, match=r"sample_size differs from pass\+fail"):
        flaky_cells.validate_registry(inflated)


def test_checked_in_registry_is_the_projection_of_the_tracked_series() -> None:
    rows = series.union(sorted((ROOT / "series").glob("*/*/*.jsonl")))
    expected = flaky_cells.render(flaky_cells.derive(rows))
    assert flaky_cells.DEFAULT_REGISTRY.read_text() == expected


def test_retained_v1_rows_remain_readable() -> None:
    legacy = row("passed")
    legacy["schema"] = series.LEGACY_ENVELOPE_VERSION
    del legacy["series"]["machine_shortname"]
    del legacy["series"]["kernel_version"]
    del legacy["series"]["host_capabilities"]
    registry = flaky_cells.derive([
        legacy,
        {
            **legacy,
            "event_id": "legacy-fail",
            "series": {
                **legacy["series"],
                "outcome": "diverged",
                "run_index": 1,
            },
        },
    ])
    assert registry["cells"][0]["sample_size"] == 2


@pytest.mark.parametrize("capability", ["cpuid-faulting", "kvm"])
def test_incomplete_v2_rows_cannot_enter_the_registry(capability: str) -> None:
    incomplete = row("passed")
    del incomplete["series"]["host_capabilities"][capability]
    with pytest.raises(series.MalformedRow, match=capability):
        flaky_cells.derive([incomplete])
