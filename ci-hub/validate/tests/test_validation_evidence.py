"""Transport and attribution controls for the schema-10 native reader.

These controls exercise the batch boundary and projection, not the producer's
artifact validator. Actual artifact admission is covered separately using its
retained canonical fixtures.
"""

import copy
import hashlib
import io
import json
from pathlib import Path
import sys
from unittest import mock

import pytest

CI_HUB = Path(__file__).resolve().parents[2]
# --import-mode=importlib does not put this directory on sys.path. Insert it
# here so the sibling test-module import below does not depend on another
# module having inserted it earlier in the same session.
sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(CI_HUB))
sys.path.insert(0, str(CI_HUB / "ledger"))
sys.path.insert(0, str(CI_HUB / "validate"))
import qualifying_receipt as receipt
import failure_evidence
import validate_rows
from test_qualifying_receipt_mutation import _green_row


def policy10():
    predicate = copy.deepcopy(receipt.active())
    predicate["cell_results"]["supported_schema_max"] = 10
    return predicate


def row10(label="one"):
    row = _green_row("a" * 40)
    row.update(schema_version=10, run_id=label)
    return row


class RefusingChecker:
    """A transport peer which refuses each request with its exact digest."""

    pid = 12345

    def __init__(self, command, *, stdin, stdout, stderr, start_new_session):
        assert command == [str(CI_HUB / "ci-hub"), "validate-evidence"]
        assert start_new_session is True
        for line in stdin:
            raw = json.loads(line)["row_json"]
            stdout.write(json.dumps({
                "row_sha256": hashlib.sha256(raw.encode()).hexdigest(),
                "status": "refused",
                "reason": "fixture artifact refusal",
            }).encode() + b"\n")

    def wait(self, timeout=None):
        return 0


def captured_checker(factory):
    """Mock the peer protocol only; live capture controls are in bounds tests."""
    def capture(command, source, **limits):
        output, errors = io.BytesIO(), io.BytesIO()
        child = factory(command, stdin=source, stdout=output, stderr=errors,
                        start_new_session=True)
        return child.wait(), output.getvalue(), errors.getvalue()
    return capture


def test_native_reader_batches_rows_and_never_trusts_row_supplied_authority():
    first, second = row10("first"), row10("second")
    first["validation_evidence"] = {"status": "verified", "accepted": True}
    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=captured_checker(RefusingChecker)) as launch:
        with receipt.validation_evidence_batch([first, second], predicate=policy10()):
            assert receipt.cell_results_verdict(first, policy10()) is receipt.CellResultsVerdict.ARTIFACT_REFUSED
            assert receipt.cell_results_verdict(second, policy10()) is receipt.CellResultsVerdict.ARTIFACT_REFUSED
            assert receipt.validation_evidence(first).reason == "fixture artifact refusal"
        assert launch.call_count == 1


def test_native_reader_rejects_changed_row_in_an_existing_batch():
    row = row10()
    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=captured_checker(RefusingChecker)) as launch:
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            row["run_id"] = "changed"
            with pytest.raises(receipt.ValidationEvidenceReaderFault, match="changed"):
                receipt.validation_evidence(row)
        assert launch.call_count == 1


def test_native_reader_wrong_response_digest_is_a_reader_fault():
    class WrongDigest(RefusingChecker):
        def __init__(self, command, **streams):
            super().__init__(command, **streams)
            output = streams["stdout"]
            output.seek(0)
            output.truncate()
            output.write(json.dumps({
                "row_sha256": "b" * 64, "status": "refused", "reason": "wrong row",
            }).encode() + b"\n")

    row = row10()
    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=captured_checker(WrongDigest)):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            checked = receipt.validation_evidence(row)
            assert checked.status == "reader-fault"
            assert "digest" in checked.reason
            with pytest.raises(receipt.ValidationEvidenceReaderFault, match="digest"):
                receipt.row_qualification(row, row["commit"], policy10())


@pytest.mark.parametrize("mutation,expected", [
    ("empty", "omitted a requested row"),
    ("omitted", "omitted a requested row"),
    ("repeated", "unexpected or repeated row digest"),
    ("non-object", "non-object response"),
    ("nonzero", "exited 7: synthetic diagnostic"),
    ("unknown-status", "unknown status"),
    ("malformed-failure", "malformed failure response"),
])
def test_native_transport_faults_apply_to_every_requested_row(mutation, expected):
    rows = [row10("first"), row10("second")]

    def capture(command, source, **limits):
        reports = [{"row_sha256": hashlib.sha256(json.loads(line)["row_json"].encode()).hexdigest(),
                    "status": "refused", "reason": "synthetic refusal"} for line in source]
        if mutation == "empty":
            reports = []
        elif mutation == "omitted":
            reports.pop()
        elif mutation == "repeated":
            reports.append(reports[0])
        elif mutation == "non-object":
            reports[0] = []
        elif mutation == "unknown-status":
            reports[0]["status"] = "future-status"
        elif mutation == "malformed-failure":
            reports[0]["reason"] = " "
        return (7 if mutation == "nonzero" else 0,
                b"".join(json.dumps(report).encode() + b"\n" for report in reports),
                b"synthetic diagnostic")

    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=capture):
        with receipt.validation_evidence_batch(rows, predicate=policy10()):
            for row in rows:
                checked = receipt.validation_evidence(row)
                assert checked.status == "reader-fault"
                assert expected in checked.reason
                with pytest.raises(receipt.ValidationEvidenceReaderFault):
                    receipt.row_qualification(row, row["commit"], policy10())


def test_record_preserves_the_native_artifact_refusal_reason(tmp_path):
    row = row10()
    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=captured_checker(RefusingChecker)), mock.patch.object(receipt, "active", return_value=policy10()):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            with pytest.raises(ValueError) as failure:
                validate_rows._require_cell_results(row, tmp_path)
            assert str(failure.value) == "cell_results fixture artifact refusal"


def test_native_reader_timeout_is_a_reader_fault_not_a_refusal():
    # Actual group kill/reap is exercised with real children in bounds tests.
    row = row10()
    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=receipt.ValidationEvidenceReaderFault("validation artifact checker timed out after 300 seconds")):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            checked = receipt.validation_evidence(row)
            assert checked.status == "reader-fault"
            assert "timed out" in checked.reason
            with pytest.raises(receipt.ValidationEvidenceReaderFault, match="timed out"):
                receipt.row_qualification(row, row["commit"], policy10())


def test_schema9_does_not_gain_authority_from_policy_maximum10():
    row = row10()
    row["schema_version"] = 9
    with mock.patch.object(receipt.subprocess, "Popen", side_effect=AssertionError("no checker should launch")):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            assert receipt.cell_results_verdict(row, policy10()) is receipt.CellResultsVerdict.SCHEMA_UNAVAILABLE


def test_schema10_base_refusal_precedes_a_checker_fault():
    row = row10()
    row["base_sha"] = None
    with mock.patch.object(receipt.subprocess, "Popen", side_effect=OSError("checker missing")):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            accepted, reason = receipt.row_qualification(row, row["commit"], policy10())
            assert accepted is False
            assert reason == "base base-sha-missing"


def test_three_clause_bridge_cannot_launch_artifact_checker_or_claim_whole_receipt():
    row = row10()
    predicate = policy10()
    request = {"row": row, **{key: predicate[key] for key in ("admission", "base", "coverage", "producer", "cell_results")}}
    output = io.StringIO()
    with mock.patch.object(receipt.sys, "stdin", io.StringIO(json.dumps(request))), mock.patch.object(receipt.sys, "stdout", output), mock.patch.object(receipt, "_check_validation_evidence", side_effect=AssertionError("recursive checker")):
        assert receipt.main(["--coverage-base-admission-only"]) == 0
    report = json.loads(output.getvalue())
    assert report["scope"] == "coverage-base-admission/v1"
    assert report["refusal"] is None
    assert "accepted" not in report
    assert "cell_results_status" not in report


def observation(role, state, *, outer_attempt=1):
    candidate = {"lane": "portable", "category": "c-programs", "test": "c-programs/probe", "mode": "verify", "backend": "kvm"}
    identity = dict(candidate)
    relation = {"kind": role}
    if role == "reference":
        identity["backend"] = "ptrace"
        relation["candidate"] = candidate
    elif role == "backend_parity":
        relation.update(reference_backend="ptrace", record_envelope="cross_backend_detcore_v1")
    return {"identity": identity, "relation": relation, "outer_attempt": outer_attempt, "verdict": {"state": state}}


def test_reference_failure_is_separate_from_candidate_and_ordinary_flake_registry():
    observations = [observation("ordinary", "diverged"), observation("reference", "diverged"), observation("backend_parity", "unavailable-with-reason")]
    observations[-1]["verdict"]["reason"] = "Reference comparison unavailable"
    row = {}
    validate_rows._bind_comparison_failures(row, observations)
    assert row["failed_cells"] == ["c-programs/probe/verify/kvm"]
    assert row["failed_comparisons"] == observations
    evidence = failure_evidence.build_evidence(
        log_text="", registry={"c-programs/probe/verify/kvm"}, prior=[], commit="a" * 40,
        dag_jobs=4, concurrent_validates=0, cell_results={"cells": []},
        failed_nodes=["e2e.manifest_c_programs"], comparison_observations=observations,
    )
    assert evidence["known_flaky_failure"] is False
    with pytest.raises(ValueError, match="cannot cover reference"):
        validate_rows._bind_comparison_failures({"known_flaky_failure": True}, observations)


def test_earlier_candidate_divergence_survives_a_terminal_match():
    observations = [observation("ordinary", "diverged"), observation("ordinary", "matched", outer_attempt=2)]
    present, diverged, unavailable = failure_evidence.failed_cells(
        {"cells": []}, comparison_observations=observations,
    )
    assert present is True
    assert diverged == ["c-programs/probe/verify/kvm"]
    assert unavailable == []


@pytest.mark.parametrize("mutation", [
    "extra-field", "unknown-relation", "boolean-attempt", "retired-parity-matches",
    "legacy-parity-flag-type", "legacy-parity-missing-type", "unknown-evidence-field",
])
def test_malformed_verified_response_is_a_reader_fault(mutation):
    row = row10()
    row["cell_results"] = {"binding_contract": 1}

    class MalformedVerified(RefusingChecker):
        def __init__(self, command, *, stdin, stdout, stderr, start_new_session):
            for line in stdin:
                raw = json.loads(line)["row_json"]
                report = {
                    "row_sha256": hashlib.sha256(raw.encode()).hexdigest(), "status": "verified",
                    "full_validation_refusal": None,
                    "evidence": {
                        "cell_results": row["cell_results"], "test_results": {},
                        "observations": [observation("ordinary", "matched")],
                        "missing_cells": [], "missing_backend_parity": [], "missing_test_producers": [],
                        "full_test_results": True, "full_backend_parity": True,
                    },
                }
                if mutation == "extra-field":
                    report["accepted"] = True
                elif mutation == "unknown-relation":
                    report["evidence"]["observations"][0]["relation"]["kind"] = "future"
                elif mutation == "boolean-attempt":
                    report["evidence"]["observations"][0]["outer_attempt"] = True
                elif mutation == "retired-parity-matches":
                    # The native reply no longer carries parity matches; an
                    # extra transport key is refused, not silently ignored.
                    report["backend_parity_matches"] = []
                elif mutation == "legacy-parity-flag-type":
                    report["evidence"]["full_backend_parity"] = "true"
                elif mutation == "legacy-parity-missing-type":
                    report["evidence"]["missing_backend_parity"] = [None]
                else:
                    report["evidence"]["selected_backend_parity"] = []
                stdout.write(json.dumps(report).encode() + b"\n")

    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=captured_checker(MalformedVerified)):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            checked = receipt.validation_evidence(row)
            assert checked.status == "reader-fault"
            assert "malformed verified response" in checked.reason


@pytest.mark.parametrize("legacy_parity", ["present-full", "present-incomplete", "absent"])
@pytest.mark.parametrize("status,marker,valid", [
    ("legacy-unbound", "absent", True), ("verified", 1, True),
    ("verified", "absent", False), ("legacy-unbound", 1, False),
    ("legacy-unbound", None, False), ("verified", True, False),
    ("verified", 1.0, False), ("verified", "1", False),
    ("verified", 0, False), ("verified", 2, False),
])
def test_native_transport_checks_capability_outcome_without_inventing_it(status, marker, valid, legacy_parity):
    # A protocol control only. Real artifact authentication and Rust/Python
    # qualification parity are covered by the native fixture tests. Historical
    # parity fields may be present, incomplete, or absent; qualification never
    # reads them (https://github.com/rrnewton/hermit/issues/3301).
    row = row10()
    cells = {} if marker == "absent" else {"binding_contract": marker}
    row["cell_results"] = cells
    evidence = {
        "cell_results": cells, "test_results": {},
        "observations": [observation("ordinary", "matched")],
        "missing_cells": [], "missing_test_producers": [],
        "full_test_results": True,
    }
    if legacy_parity == "present-full":
        evidence.update(missing_backend_parity=[], full_backend_parity=True)
    elif legacy_parity == "present-incomplete":
        evidence.update(missing_backend_parity=[{"candidate": {}}], full_backend_parity=False)
    report = {
        "row_sha256": receipt._evidence_row_digest(row), "status": status,
        "full_validation_refusal": None, "evidence": evidence,
    }
    with mock.patch.object(receipt, "_capture_validation_evidence", return_value=(0, json.dumps(report).encode() + b"\n", b"")):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            checked = receipt.validation_evidence(row)
            assert checked.status == (status if valid else "reader-fault")
            if not valid:
                assert "inconsistent binding contract" in checked.reason
            elif status == "legacy-unbound":
                assert receipt.cell_results_verdict(row, policy10()) is receipt.CellResultsVerdict.LEGACY_UNBOUND
                assert receipt.row_qualification(row, row["commit"], policy10()) == (False, "cell_results legacy-unbound")
            else:
                assert receipt.cell_results_verdict(row, policy10()) is receipt.CellResultsVerdict.SATISFIED
