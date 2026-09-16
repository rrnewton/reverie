"""Real native artifact-reader controls over producer-owned synthetic fixtures.

This module launches the normal ci-hub front door once for a batch. It does not
mock artifact parsing, replace the checker executable, or measure a guest.
"""

import copy
import hashlib
import json
from pathlib import Path
import sys
from unittest import mock

import pytest

CI_HUB = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(CI_HUB))
sys.path.insert(0, str(CI_HUB / "ledger"))
sys.path.insert(0, str(CI_HUB / "validate"))
import qualifying_receipt as receipt
import failure_evidence
import validate_rows
from test_qualifying_receipt_mutation import _green_row

FIXTURES = Path(__file__).parent / "fixtures" / "schema10"
MANIFEST_SHA256 = "87199552c47d880656625e19b8bee54fc3f5b6e89c88462365955c74ced07b5a"


def retained_case(root, source, run, mutation=None):
    row = json.loads((FIXTURES / f"{source}-row.json").read_bytes())
    plan = json.loads((FIXTURES / f"{source}-plan.json").read_bytes())
    cells = [json.loads(line) for line in (FIXTURES / f"{source}-cells.jsonl").read_bytes().splitlines()]
    tests = [json.loads(line) for line in (FIXTURES / f"{source}-tests.jsonl").read_bytes().splitlines()]
    row["run_id"] = run
    plan["run_id"] = run
    for key in ("cell_results", "test_results"):
        row[key]["run_id"] = run
    for result in [*cells, *tests]:
        result["run_id"] = run
    if mutation:
        mutation(row, plan, cells, tests)
    data = {
        "constructed-plan.json": json.dumps(plan, separators=(",", ":")).encode(),
        "cell-results.jsonl": b"".join(json.dumps(cell, sort_keys=True, separators=(",", ":")).encode() + b"\n" for cell in cells),
        # The schema-9 typed artifact keeps its declaration order unchanged.
        "test-results.jsonl": b"".join(json.dumps(test, separators=(",", ":")).encode() + b"\n" for test in tests),
    }
    for reference, name, count in (
        (row["constructed_plan"], "constructed-plan.json", None),
        (row["cell_results"]["artifact"], "cell-results.jsonl", len(cells)),
        (row["test_results"]["artifact"], "test-results.jsonl", len(tests)),
    ):
        relative = f"ignored/validate/artifacts/{run}/{name}"
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data[name])
        reference.update(path=relative, sha256=hashlib.sha256(data[name]).hexdigest())
        reference["bytes" if count is None else "row_count"] = len(data[name]) if count is None else count

    # Supply the ordinary admission/value clauses as explicit synthetic input.
    # The independently retained one-node plan and test count remain one.
    green = _green_row(row["commit"])
    green.update({key: value for key, value in row.items() if value is not None})
    green.update(
        cwd=str(root / "hermit"), checks=1, gates_run=1, gates_expected=1,
        gates=[{"name": "e2e.manifest_backend_parity_c_on_host", "result": "pass", "exit_code": 0}],
        coverage={"planned_test_nodes": 1, "executed_test_nodes": 1, "zero_executed_nodes": [], "absent_nodes": []},
    )
    return green


def test_schema10_real_batch_qualification_projection_and_artifact_refusals(tmp_path):
    manifest_bytes = (FIXTURES / "SHA256SUMS.json").read_bytes()
    assert hashlib.sha256(manifest_bytes).hexdigest() == MANIFEST_SHA256
    manifest = json.loads(manifest_bytes)
    assert len(manifest) == 12
    for name, expected in manifest.items():
        raw = (FIXTURES / name).read_bytes()
        assert len(raw) == expected["bytes"]
        assert hashlib.sha256(raw).hexdigest() == expected["sha256"]

    matched = retained_case(tmp_path, "matched", "matched")
    reference = retained_case(tmp_path, "reference-diverged", "reference")
    retried = retained_case(tmp_path, "cross-diverged-then-matched", "retried")
    missing = retained_case(tmp_path, "matched", "missing-plan")
    (tmp_path / missing["constructed_plan"]["path"]).unlink()
    corrupt = retained_case(tmp_path, "matched", "corrupt-plan")
    with (tmp_path / corrupt["constructed_plan"]["path"]).open("ab") as output:
        output.write(b" ")
    wrong_compatibility = retained_case(
        tmp_path, "matched", "wrong-compatibility",
        lambda row, plan, cells, tests: plan.update(compatibility_selected=True),
    )
    def omit_recorded(row, plan, cells, tests):
        cells.clear()
        row["cell_results"].update(recorded_count=0, cells=[])
    partial = retained_case(tmp_path, "matched", "partial", omit_recorded)
    def omit_planned_population(row, plan, cells, tests):
        # This shrinks only result-derived summaries; the independently bound
        # plan still selects its producer and must prevent the invented green.
        tests.clear()
        summary = row["test_results"]
        summary.update(selected={"nodes": [], "compatibility": False}, selected_count=0, recorded_count=0, nodes=[])
    missing_producer = retained_case(tmp_path, "matched", "missing-producer", omit_planned_population)

    # Exercise unchanged event transport before checking the exact reconstructed
    # compact summaries. Raw artifact report strings never enter the event.
    event = validate_rows._event_for_row(matched, tmp_path)
    restored = validate_rows._rows_from_events([event], root=tmp_path)[0]
    assert restored["cell_results"] == matched["cell_results"]
    assert "/home/fixture" not in json.dumps(event)
    assert b"/home/fixture" in (tmp_path / matched["cell_results"]["artifact"]["path"]).read_bytes()
    rows = [matched, reference, retried, missing, corrupt, wrong_compatibility, partial, missing_producer, restored]
    predicate = copy.deepcopy(receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 10
    real_popen = receipt.subprocess.Popen
    with mock.patch.object(receipt.subprocess, "Popen", wraps=real_popen) as launch, mock.patch.object(receipt, "active", return_value=predicate):
        with receipt.validation_evidence_batch(rows, predicate=predicate, artifact_root=tmp_path):
            for row in (matched, restored):
                assert receipt.row_qualification(row, row["commit"], predicate) == (True, "qualifies")
                validate_rows._require_cell_results(row, tmp_path)
            for row in (reference, retried):
                assert receipt.row_qualification(row, row["commit"], predicate) == (False, "cell_results comparison-diverged")
                # A valid recorded failure remains publishable evidence.
                validate_rows._require_cell_results(row, tmp_path)
            for row in (matched, reference, retried):
                projection = receipt.cell_scorecard_projection(row, predicate)
                assert len(projection["selected"]) == 1
                assert projection["ordinary_green_count"] == 1
                assert projection["backend_parity_selected_count"] == 1
                assert projection["backend_parity_recorded_count"] == 1
                assert projection["backend_parity_green_count"] == int(row is matched)
                assert projection["green_count"] == int(row is matched)
            checked_reference = receipt.validation_evidence(reference)
            observations = checked_reference.evidence["observations"]
            present, diverged, unavailable = failure_evidence.failed_cells(reference["cell_results"], comparison_observations=observations)
            assert present and diverged == [] and unavailable == []
            projected_failure = copy.deepcopy(reference)
            validate_rows._bind_comparison_failures(projected_failure, observations)
            assert projected_failure["failed_cells"] == []
            assert [failure["relation"]["kind"] for failure in projected_failure["failed_comparisons"]] == ["reference", "backend_parity"]
            assert projected_failure["failed_comparisons"][0]["identity"]["backend"] == "ptrace"
            assert receipt.validation_evidence(missing).status == "reader-fault"
            with pytest.raises(receipt.ValidationEvidenceReaderFault):
                receipt.row_qualification(missing, missing["commit"], predicate)
            for row in (corrupt, wrong_compatibility, missing_producer):
                assert receipt.validation_evidence(row).status == "refused"
                assert receipt.row_qualification(row, row["commit"], predicate)[0] is False
            assert receipt.row_qualification(partial, partial["commit"], predicate) == (False, "cell_results incomplete for full validation")
            projection = receipt.cell_scorecard_projection(partial, predicate)
            assert len(projection["selected"]) == 1
            assert projection["backend_parity_selected_count"] == 1
            assert projection["backend_parity_recorded_count"] == 0
            assert projection["backend_parity_green_count"] == 0
        assert launch.call_count == 1
        assert launch.call_args.args[0][:2] == [str(CI_HUB / "ci-hub"), "validate-evidence"]
