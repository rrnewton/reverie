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
MANIFEST_SHA256 = "4821718ba4d479b09ea99cee7e5a20ec8750f289b2b047dcc11a43000118a7f7"
V1_MANIFEST_SHA256 = "74ce0748c4b2ab8e197a26df852f63bd38e1aeda2050d184dc8fd4b15721a307"


def retained_case(root, source, run, mutation=None):
    row = json.loads((FIXTURES / f"{source}-row.json").read_bytes())
    plan = json.loads((FIXTURES / f"{source}-plan.json").read_bytes())
    cells = [json.loads(line) for line in (FIXTURES / f"{source}-cells.jsonl").read_bytes().splitlines()]
    tests = [json.loads(line) for line in (FIXTURES / f"{source}-tests.jsonl").read_bytes().splitlines()]
    row["run_id"] = run
    plan["run_id"] = run
    for key in ("cell_results", "test_results"):
        row[key]["run_id"] = run
    for cell in row["cell_results"]["cells"]:
        if "evidence_binding" in cell:
            cell["evidence_binding"]["run_id"] = run
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


def test_selected_attempt_v1_fixture_manifest_integrity():
    fixtures = FIXTURES / "selected-attempt-v1"
    manifest_bytes = (fixtures / "FINAL-V1-FIXTURE-MANIFEST.json").read_bytes()
    assert hashlib.sha256(manifest_bytes).hexdigest() == V1_MANIFEST_SHA256
    manifest = json.loads(manifest_bytes)["partial_manifest"]
    assert manifest["synthetic_parser_controls_not_guest_measurements"] is True
    assert len(manifest["files"]) == 16
    for name, expected in manifest["files"].items():
        raw = (fixtures / name).read_bytes()
        assert len(raw) == expected["bytes"]
        assert hashlib.sha256(raw).hexdigest() == expected["sha256"]


def test_schema10_real_batch_qualification_projection_and_artifact_refusals(tmp_path, monkeypatch):
    manifest_bytes = (FIXTURES / "SHA256SUMS.json").read_bytes()
    assert hashlib.sha256(manifest_bytes).hexdigest() == MANIFEST_SHA256
    manifest = json.loads(manifest_bytes)
    assert manifest["source_head"] == "73146a90f8f1dd468471ffab448b6b7d71f33bf6"
    assert manifest["synthetic"] is True
    assert len(manifest["files"]) == 16
    for name, expected in manifest["files"].items():
        raw = (FIXTURES / name).read_bytes()
        assert len(raw) == expected["bytes"]
        assert hashlib.sha256(raw).hexdigest() == expected["sha256"]

    matched = retained_case(tmp_path, "matched", "matched")
    ordinary = retained_case(tmp_path, "ordinary-only", "ordinary-only")
    reference = retained_case(tmp_path, "reference-diverged", "reference")
    retried = retained_case(tmp_path, "cross-diverged-then-matched", "retried")
    missing = retained_case(tmp_path, "matched", "missing-plan")
    (tmp_path / missing["constructed_plan"]["path"]).unlink()
    corrupt = retained_case(tmp_path, "matched", "corrupt-plan")
    with (tmp_path / corrupt["constructed_plan"]["path"]).open("ab") as output:
        output.write(b" ")
    same_length_corrupt = retained_case(tmp_path, "matched", "same-length-corrupt-plan")
    same_length_path = tmp_path / same_length_corrupt["constructed_plan"]["path"]
    original = same_length_path.read_bytes()
    assert original.startswith(b"{")
    assert hashlib.sha256(original).hexdigest() == same_length_corrupt["constructed_plan"]["sha256"]
    replacement = b"[" + original[1:]
    assert len(replacement) == same_length_corrupt["constructed_plan"]["bytes"]
    assert hashlib.sha256(replacement).hexdigest() != same_length_corrupt["constructed_plan"]["sha256"]
    same_length_path.write_bytes(replacement)
    empty_suffix = retained_case(tmp_path, "matched", "empty-test-suffix")
    empty_suffix["test_results"]["artifact"]["path"] = "ignored/validate/artifacts"
    nul_suffix = retained_case(tmp_path, "matched", "nul-test-suffix")
    nul_suffix["test_results"]["artifact"]["path"] = "ignored/validate/artifacts/nul-test-suffix/bad\0test-results.jsonl"
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
        selected = {"nodes": [], "compatibility": False}
        summary.update(selected=selected, selected_count=0, recorded_count=0, nodes=[],
                       totals={"executed_tests": 0, "passed_tests": 0, "failed_tests": 0, "filtered_tests": 0},
                       population_sha256=hashlib.sha256(json.dumps(selected, separators=(",", ":")).encode()).hexdigest())
        row.update(executed_tests=0, passed_tests=0, filtered_tests=0)
    missing_producer = retained_case(tmp_path, "matched", "missing-producer", omit_planned_population)
    def substitute_producer(row, plan, cells, tests):
        selected = {"nodes": ["synthetic.other-producer"], "compatibility": False}
        row["test_results"]["selected"] = selected
        row["test_results"]["nodes"][0]["node"] = selected["nodes"][0]
        tests[0]["producer"]["node"] = selected["nodes"][0]
        row["test_results"]["population_sha256"] = hashlib.sha256(json.dumps(selected, separators=(",", ":")).encode()).hexdigest()
    substituted_producer = retained_case(tmp_path, "matched", "substituted-producer", substitute_producer)

    # Exercise unchanged event transport before checking the exact reconstructed
    # compact summaries. Raw artifact report strings never enter the event.
    event = validate_rows._event_for_row(matched, tmp_path)
    restored = validate_rows._rows_from_events([event], root=tmp_path)[0]
    assert restored["cell_results"] == matched["cell_results"]
    assert "/home/fixture" not in json.dumps(event)
    assert b"/home/fixture" in (tmp_path / matched["cell_results"]["artifact"]["path"]).read_bytes()
    rows = [matched, ordinary, reference, retried, missing, corrupt, same_length_corrupt, empty_suffix, nul_suffix, wrong_compatibility, partial, missing_producer, substituted_producer, restored]
    predicate = copy.deepcopy(receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 10
    real_popen = receipt.subprocess.Popen
    # Exercise the default native resolver from this actual agent worktree.
    # An explicit fixture store remains supported for custom callers, but
    # production Python callers must honor DEV_HERMIT_PARENT just like Rust.
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(tmp_path))
    with mock.patch.object(receipt.subprocess, "Popen", wraps=real_popen) as launch, mock.patch.object(receipt, "active", return_value=predicate):
        with receipt.validation_evidence_batch(rows, predicate=predicate):
            for row in (matched, ordinary, restored):
                assert receipt.row_qualification(row, row["commit"], predicate) == (False, "cell_results legacy-unbound")
                validate_rows._require_cell_results(row, tmp_path)
            checked_ordinary = receipt.validation_evidence(ordinary)
            assert checked_ordinary.status == "legacy-unbound"
            assert checked_ordinary.evidence["full_backend_parity"] is False
            assert checked_ordinary.backend_parity_matches == ()
            projection = receipt.cell_scorecard_projection(ordinary, predicate)
            assert projection["eligible"] is False
            assert projection["binding_contract"] == "legacy-unbound"
            assert projection["bound_evidence_eligible"] is False
            assert len(projection["selected"]) == 1
            assert projection["ordinary_green_count"] == 1
            assert projection["selected_backend_parity"] == []
            assert projection["backend_parity_selected_count"] == 0
            assert projection["backend_parity_recorded_count"] == 0
            assert projection["backend_parity_green_count"] == 0
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
            for row, reason, qualification in (
                (corrupt, "artifact ignored/validate/artifacts/corrupt-plan/constructed-plan.json length differs from declared constructed_plan bytes", None),
                (same_length_corrupt, "schema 10 constructed plan bytes differ from their recorded identity", None),
                (empty_suffix, "retained artifact path is invalid: empty artifact path", None),
                (nul_suffix, "retained artifact path is invalid: artifact path contains NUL", None),
                (wrong_compatibility, "schema 9 test_results selected population differs from constructed plan", None),
                (missing_producer, "schema 9 test_results selected population differs from constructed plan", "executed_tests==0"),
                (substituted_producer, "schema 9 test_results selected population differs from constructed plan", None),
            ):
                checked = receipt.validation_evidence(row)
                assert checked.status == "refused"
                assert checked.reason == reason
                assert receipt.row_qualification(row, row["commit"], predicate) == (False, qualification or f"cell_results {reason}")
                with pytest.raises(ValueError) as refusal:
                    validate_rows._require_cell_results(row, tmp_path)
                assert str(refusal.value) == f"cell_results {reason}"
            assert receipt.row_qualification(partial, partial["commit"], predicate) == (False, "cell_results incomplete for full validation")
            projection = receipt.cell_scorecard_projection(partial, predicate)
            assert len(projection["selected"]) == 1
            assert projection["backend_parity_selected_count"] == 1
            assert projection["backend_parity_recorded_count"] == 0
            assert projection["backend_parity_green_count"] == 0
        assert launch.call_count == 1
        assert launch.call_args.args[0] == [str(CI_HUB / "ci-hub"), "validate-evidence"]


def test_schema10_explicit_artifact_store_overrides_operational_parent(tmp_path, monkeypatch):
    custom = tmp_path / "custom"
    operational = tmp_path / "operational"
    operational.mkdir()
    row = retained_case(custom, "selected-attempt-v1/matched", "custom-store")
    monkeypatch.setenv("DEV_HERMIT_PARENT", str(operational))
    predicate = copy.deepcopy(receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 10
    real_popen = receipt.subprocess.Popen
    with mock.patch.object(receipt.subprocess, "Popen", wraps=real_popen) as launch:
        with receipt.validation_evidence_batch([row], predicate=predicate, artifact_root=custom):
            assert receipt.row_qualification(row, row["commit"], predicate) == (True, "qualifies")
        assert launch.call_count == 1
        assert launch.call_args.args[0] == [str(CI_HUB / "ci-hub"), "validate-evidence", "--artifact-root", str(custom)]


def test_schema10_real_mixed_contract_batch_preserves_history_and_refuses_upgrades(tmp_path):
    cases = {}
    expected = {}
    for prefix, status in (("", "legacy-unbound"), ("selected-attempt-v1/", "verified")):
        for source in ("matched", "ordinary-only", "reference-diverged", "cross-diverged-then-matched"):
            label = (prefix + source).replace("/", "-")
            cases[label] = retained_case(tmp_path, prefix + source, label)
            expected[label] = status
        for failure in ("missing", "corrupt"):
            label = (prefix + failure).replace("/", "-")
            row = retained_case(tmp_path, prefix + "matched", label)
            path = tmp_path / row["constructed_plan"]["path"]
            if failure == "missing":
                path.unlink()
            else:
                raw = path.read_bytes()
                path.write_bytes(b"[" + raw[1:])
            cases[label] = row
            expected[label] = "reader-fault" if failure == "missing" else "refused"
    for label, marker in (("null", None), ("bool", True), ("float", 1.0), ("string", "1"), ("zero", 0), ("unknown", 2), ("false-upgrade", 1)):
        label = "marker-" + label
        cases[label] = retained_case(
            tmp_path, "matched", label,
            lambda row, plan, cells, tests, marker=marker: row["cell_results"].update(binding_contract=marker),
        )
        expected[label] = "refused"

    def remove_contract(row, plan, cells, tests):
        del row["cell_results"]["binding_contract"]
    def change_binding(row, plan, cells, tests):
        row["cell_results"]["cells"][0]["evidence_binding"]["selected_attempt"] += 1
    def add_legacy_null_binding(row, plan, cells, tests):
        row["cell_results"]["cells"][0]["evidence_binding"] = None
    def add_legacy_null_attempt(row, plan, cells, tests):
        cells[0]["selected_attempt"] = None
    for label, source, mutation in (
        ("stripped-contract", "selected-attempt-v1/matched", remove_contract),
        ("wrong-attempt", "selected-attempt-v1/matched", change_binding),
        ("legacy-null-binding", "matched", add_legacy_null_binding),
        ("legacy-null-attempt", "matched", add_legacy_null_attempt),
    ):
        cases[label] = retained_case(tmp_path, source, label, mutation)
        expected[label] = "refused"

    predicate = copy.deepcopy(receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 10
    real_popen = receipt.subprocess.Popen
    with mock.patch.object(receipt.subprocess, "Popen", wraps=real_popen) as launch, mock.patch.object(receipt, "active", return_value=predicate):
        with receipt.validation_evidence_batch(list(cases.values()), artifact_root=tmp_path, predicate=predicate):
            for label, row in cases.items():
                checked = receipt.validation_evidence(row)
                assert checked.status == expected[label], (label, checked)
                if checked.status == "reader-fault":
                    with pytest.raises(receipt.ValidationEvidenceReaderFault):
                        receipt.row_qualification(row, row["commit"], predicate)
                    continue
                if checked.status == "refused":
                    with pytest.raises(ValueError):
                        validate_rows._require_cell_results(row, tmp_path)
                    continue
                validate_rows._require_cell_results(row, tmp_path)
                assert len(checked.evidence["observations"]) == (6 if "then-matched" in label else 1 if "ordinary-only" in label else 3)
                failed = "diverged" in label
                assert receipt.row_qualification(row, row["commit"], predicate) == (
                    (False, "cell_results comparison-diverged") if failed else
                    (False, "cell_results legacy-unbound") if checked.status == "legacy-unbound" else
                    (True, "qualifies")
                )
                projection = receipt.cell_scorecard_projection(row, predicate)
                assert projection["bound_evidence_eligible"] is (checked.status == "verified")
                if "reference-diverged" in label:
                    assert [item["verdict"]["state"] for item in checked.evidence["observations"]] == ["matched", "diverged", "unavailable-with-reason"]
                if "then-matched" in label:
                    assert [item["verdict"]["state"] for item in checked.evidence["observations"] if item["relation"]["kind"] == "backend_parity"] == ["diverged", "matched"]
        assert launch.call_count == 1


@pytest.mark.parametrize("case,accepted", [
    ("matched", False), ("ordinary-only", False),
    ("selected-attempt-v1/matched", True), ("selected-attempt-v1/ordinary-only", True),
    ("selected-attempt-v1/reference-diverged", False),
    ("selected-attempt-v1/cross-diverged-then-matched", False),
    ("reference-diverged", False), ("cross-diverged-then-matched", False),
    ("missing-plan", False), ("base-before-missing-plan", False),
])
def test_schema10_normal_rust_qualification_uses_the_retained_artifact_store(tmp_path, case, accepted):
    import tempfile
    source = "matched" if case in {"missing-plan", "base-before-missing-plan"} else case
    row = retained_case(tmp_path, source, "rust-" + case.replace("/", "-"))
    if case in {"missing-plan", "base-before-missing-plan"}:
        (tmp_path / row["constructed_plan"]["path"]).unlink()
    if case == "base-before-missing-plan":
        row["base_sha"] = None
    predicate = copy.deepcopy(receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 10
    policy_path = tmp_path / "qualifying-receipt.json"
    policy_path.write_text(json.dumps(predicate))
    # env modifies only the child. Parallel tests and the Python receipt module
    # retain their original environment and active policy. The actual normal
    # launcher reaches Rust row_qualifies, its three-clause Python bridge, then
    # the in-process producer-owned artifact verifier.
    command = [
        "env", f"DEV_HERMIT_PARENT={tmp_path}",
        f"DEV_HERMIT_TOOL_ROOT={CI_HUB.parent}",
        f"QUALIFYING_RECEIPT_PREDICATE={policy_path}",
        str(CI_HUB / "ci-hub"), "receipt-digest", "--sha", row["commit"],
        "--require-qualifying", "--json",
    ]
    with tempfile.TemporaryFile() as input_file:
        input_file.write(json.dumps(row).encode())
        input_file.seek(0)
        code, output, errors = receipt._capture_validation_evidence(
            command, input_file, line_bytes=receipt._EVIDENCE_LINE_BYTES,
            response_bytes=receipt._EVIDENCE_BATCH_BYTES,
            diagnostic_bytes=receipt._EVIDENCE_DIAGNOSTIC_BYTES, wall_seconds=60,
        )
    assert code == (0 if accepted else 2), (code, output, errors)
    if accepted:
        report = json.loads(output)
        assert report["sha"] == row["commit"]
        assert report["digest_algorithm"] == "sha256"
        assert len(report["digest"]) == 64 and set(report["digest"]) <= set("0123456789abcdef")
        assert report["executed_tests"] == 1
        assert b"validation evidence:" not in errors
    else:
        assert output == b""
        assert b"HistoryRow does not satisfy the shared qualifying predicate" in errors
        # This CLI intentionally has a boolean refusal API. The native helper
        # tests assert typed causes separately. Here a missing artifact reaches
        # the real store; an earlier base refusal must avoid that lookup.
        assert (b"validation evidence: cannot open retained artifact" in errors) == (case == "missing-plan")
