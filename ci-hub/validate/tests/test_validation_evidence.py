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
import signal
import subprocess
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


def test_native_reader_batches_rows_and_never_trusts_row_supplied_authority():
    first, second = row10("first"), row10("second")
    first["validation_evidence"] = {"status": "verified", "accepted": True}
    with mock.patch.object(receipt.subprocess, "Popen", side_effect=RefusingChecker) as launch:
        with receipt.validation_evidence_batch([first, second], predicate=policy10()):
            assert receipt.cell_results_verdict(first, policy10()) is receipt.CellResultsVerdict.ARTIFACT_REFUSED
            assert receipt.cell_results_verdict(second, policy10()) is receipt.CellResultsVerdict.ARTIFACT_REFUSED
            assert receipt.validation_evidence(first).reason == "fixture artifact refusal"
        assert launch.call_count == 1


def test_native_reader_rejects_changed_row_in_an_existing_batch():
    row = row10()
    with mock.patch.object(receipt.subprocess, "Popen", side_effect=RefusingChecker) as launch:
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
    with mock.patch.object(receipt.subprocess, "Popen", side_effect=WrongDigest):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            checked = receipt.validation_evidence(row)
            assert checked.status == "reader-fault"
            assert "digest" in checked.reason
            with pytest.raises(receipt.ValidationEvidenceReaderFault, match="digest"):
                receipt.row_qualification(row, row["commit"], policy10())


def test_native_reader_timeout_kills_its_process_group_and_is_not_a_refusal():
    class TimedOut(RefusingChecker):
        def wait(self, timeout=None):
            if timeout is not None:
                raise subprocess.TimeoutExpired("fixture checker", timeout)
            return -signal.SIGKILL

    row = row10()
    with mock.patch.object(receipt.subprocess, "Popen", side_effect=TimedOut), mock.patch.object(receipt.os, "killpg") as kill:
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            checked = receipt.validation_evidence(row)
            assert checked.status == "reader-fault"
            assert "timed out" in checked.reason
        kill.assert_called_once_with(12345, signal.SIGKILL)


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


@pytest.mark.parametrize("mutation", ["extra-field", "unknown-relation", "boolean-attempt", "empty-match-identity"])
def test_malformed_verified_response_is_a_reader_fault(mutation):
    row = row10()

    class MalformedVerified(RefusingChecker):
        def __init__(self, command, *, stdin, stdout, stderr, start_new_session):
            for line in stdin:
                raw = json.loads(line)["row_json"]
                report = {
                    "row_sha256": hashlib.sha256(raw.encode()).hexdigest(), "status": "verified",
                    "full_validation_refusal": None, "backend_parity_matches": [],
                    "evidence": {
                        "cell_results": {}, "test_results": {},
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
                else:
                    report["backend_parity_matches"] = [{}]
                stdout.write(json.dumps(report).encode() + b"\n")

    row["cell_results"] = {}
    with mock.patch.object(receipt.subprocess, "Popen", side_effect=MalformedVerified):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            checked = receipt.validation_evidence(row)
            assert checked.status == "reader-fault"
            assert "malformed verified response" in checked.reason
