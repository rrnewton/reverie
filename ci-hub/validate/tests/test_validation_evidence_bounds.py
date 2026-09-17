"""Reader capacity and live process controls; no Hermit guests or native builds."""

import hashlib
import io
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time
from unittest import mock

import pytest

from test_validation_evidence import observation, policy10, receipt, row10


@pytest.mark.parametrize("field", ["row_sha256", "status", "verdict-state"])
@pytest.mark.parametrize("value", [[], {}, None, True, 7])
def test_nonstring_native_primitives_are_reader_faults(field, value):
    row = row10()
    row["cell_results"] = {}
    report = {
        "row_sha256": receipt._evidence_row_digest(row), "status": "verified",
        "full_validation_refusal": None, "backend_parity_matches": [],
        "evidence": {
            "cell_results": {}, "test_results": {},
            "observations": [observation("ordinary", "matched")],
            "missing_cells": [], "missing_backend_parity": [], "missing_test_producers": [],
            "full_test_results": True, "full_backend_parity": True,
        },
    }
    if field == "verdict-state":
        report["evidence"]["observations"][0]["verdict"]["state"] = value
    else:
        report[field] = value
    with mock.patch.object(receipt, "_capture_validation_evidence", return_value=(0, json.dumps(report).encode() + b"\n", b"")):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            assert receipt.validation_evidence(row).status == "reader-fault"
            with pytest.raises(receipt.ValidationEvidenceReaderFault):
                receipt.row_qualification(row, row["commit"], policy10())


def test_incremental_row_encoding_preserves_exact_digest():
    row = {"unicode": "\u00e9\u2603", "escapes": "\n\\\"", "number": 1.25,
           "array": [True, None, 7], "long": "x" * 70000}
    raw = receipt._evidence_row_json(row).encode()
    assert b"".join(receipt._evidence_json_chunks(row)) == raw
    assert receipt._evidence_row_digest(row) == hashlib.sha256(raw).hexdigest()


@pytest.mark.parametrize("value", [
    "", '"\\\b\f\n\r\t\x00\x1f\x7f', "\u00e9\u2603\U0001f642\ud800",
    [True, False, None, 0, -1, 1.25, -0.0, 1e300],
    {"nested": ({"key": "value"}, [])},
    {7: "integer key"}, {None: "null key"}, {False: "boolean key"},
    {1.25: "float key"}, 10 ** 31, -(10 ** 31),
])
def test_preflight_counts_exact_standard_json_bytes(value):
    raw = json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()
    assert receipt._preflight_evidence_json(value, len(raw)) == len(raw)
    assert b"".join(receipt._evidence_json_chunks(value, limit=len(raw))) == raw
    with pytest.raises(receipt.ValidationEvidenceReaderFault, match=f"exceeds {len(raw) - 1} bytes"):
        receipt._preflight_evidence_json(value, len(raw) - 1)


@pytest.mark.parametrize("value", ["x" * 65, "\x00" * 11, {"\u2603" * 11: None}, 10 ** 64])
def test_oversized_scalar_refused_before_standard_encoder_allocates(value):
    with mock.patch.object(receipt.json.JSONEncoder, "iterencode", side_effect=AssertionError("oversized scalar reached encoder")):
        with pytest.raises(receipt.ValidationEvidenceReaderFault, match="request line exceeds 64 bytes"):
            list(receipt._evidence_json_chunks(value, limit=64))


def test_request_envelope_expansion_refused_before_escaped_row_allocation():
    row = {"text": "\x00" * 5}
    raw = json.dumps(row, sort_keys=True, separators=(",", ":"), allow_nan=False)
    limit = len(raw)
    real = receipt.json.JSONEncoder.iterencode
    encoded = []

    def encode(encoder, value, *args, **kwargs):
        assert not (isinstance(value, dict) and "row_json" in value), "oversized envelope reached encoder"
        encoded.append(value)
        return real(encoder, value, *args, **kwargs)

    source = io.BytesIO()
    with mock.patch.object(receipt.json.JSONEncoder, "iterencode", new=encode):
        requests, failure = receipt._write_evidence_requests([row], source, line_bytes=limit, batch_bytes=128)
    assert encoded == [row]
    assert set(requests) == {hashlib.sha256(raw.encode()).hexdigest()}
    assert failure == f"validation artifact checker request line exceeds {limit} bytes"
    assert source.getvalue() == b""


@pytest.mark.parametrize("shape,expected", [
    ("nan", "Out of range float values are not JSON compliant"),
    ("unsupported", "Object of type object is not JSON serializable"),
    ("cycle", "Circular reference detected"),
    ("deep", "maximum recursion depth exceeded"),
])
def test_unencodable_input_preserves_fault_attribution_and_earlier_refusal(shape, expected):
    # Use the same default JSON loader as the public CLI for the NaN case.
    first = json.loads(json.dumps(row10("first")))
    first["base_sha"] = None
    if shape == "nan":
        first["auxiliary"] = json.loads("NaN")
    elif shape == "unsupported":
        first["auxiliary"] = object()
    elif shape == "cycle":
        first["auxiliary"] = first
    else:
        nested = None
        for _ in range(sys.getrecursionlimit() + 10):
            nested = [nested]
        first["auxiliary"] = nested
    second = row10("second")
    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=AssertionError("invalid batch launched")):
        with receipt.validation_evidence_batch([first, second], predicate=policy10()):
            assert receipt.row_qualification(first, first["commit"], policy10()) == (False, "base base-sha-missing")
            for row in (first, second):
                checked = receipt.validation_evidence(row)
                assert checked.status == "reader-fault"
                assert checked.row_sha256 is None
                assert expected in checked.reason
            with pytest.raises(receipt.ValidationEvidenceReaderFault, match=expected):
                receipt.row_qualification(second, second["commit"], policy10())
            # Object identity only retains failure, even if its bytes change.
            second["run_id"] = "changed"
            assert receipt.validation_evidence(second).status == "reader-fault"
            with pytest.raises(receipt.ValidationEvidenceReaderFault, match="not included"):
                receipt.validation_evidence(dict(second))
        # The direct API also returns a fault instead of indexing a missing hash.
        assert receipt.validation_evidence(first).status == "reader-fault"


def test_cli_nan_keeps_base_refusal_and_reports_unreadable_artifact():
    row = row10()
    row.update(base_sha=None, auxiliary=float("nan"))
    input_body = json.dumps(row)  # default encoding/CLI loading both admit NaN
    output, errors = io.StringIO(), io.StringIO()
    with mock.patch.object(receipt, "active", return_value=policy10()), mock.patch.object(
        receipt.sys, "stdin", io.StringIO(input_body)
    ), mock.patch.object(receipt.sys, "stdout", output), mock.patch.object(
        receipt.sys, "stderr", errors
    ), mock.patch.object(receipt, "_capture_validation_evidence", side_effect=AssertionError("NaN batch launched")):
        assert receipt.main(["--sha", row["commit"], "--json"]) == 1
    report = json.loads(output.getvalue())
    assert report["accepted"] is False
    assert report["base_status"] == "base-sha-missing"
    assert report["cell_results_status"] == "reader-fault"
    assert report["cell_scorecard"]["green_count"] is None
    assert errors.getvalue() == ""


@pytest.mark.parametrize("bound", ["line", "batch"])
def test_request_exact_capacity_and_one_byte_over(bound):
    rows = [{"run_id": "first"}, {"run_id": "second"}]
    encoded = [json.dumps({"row_json": receipt._evidence_row_json(row)}, sort_keys=True,
                          separators=(",", ":")).encode() + b"\n" for row in rows]
    maximum_line = max(map(len, encoded))
    total = sum(map(len, encoded))
    source = io.BytesIO()
    requests, failure = receipt._write_evidence_requests(
        rows, source, line_bytes=maximum_line, batch_bytes=total)
    assert failure is None
    assert source.read() == b"".join(encoded)
    assert set(requests) == {receipt._evidence_row_digest(row) for row in rows}
    source = io.BytesIO()
    requests, failure = receipt._write_evidence_requests(
        rows, source, line_bytes=maximum_line - (bound == "line"),
        batch_bytes=total - (bound == "batch"))
    limit = maximum_line - 1 if bound == "line" else total - 1
    assert failure == f"validation artifact checker request {bound} exceeds {limit} bytes"
    assert set(requests) == {receipt._evidence_row_digest(row) for row in rows}
    assert len(source.getvalue()) <= total - (bound == "batch")


def test_request_capacity_fault_defers_to_earlier_base_refusal():
    first, second = row10("first"), row10("second")
    first["base_sha"] = None
    with mock.patch.object(receipt, "_EVIDENCE_LINE_BYTES", 32), mock.patch.object(
        receipt, "_capture_validation_evidence", side_effect=AssertionError("oversize batch launched")
    ):
        with receipt.validation_evidence_batch([first, second], predicate=policy10()):
            assert receipt.row_qualification(first, first["commit"], policy10()) == (False, "base base-sha-missing")
            for row in (first, second):
                checked = receipt.validation_evidence(row)
                assert checked.status == "reader-fault"
                assert checked.reason == "validation artifact checker request line exceeds 32 bytes"
            with pytest.raises(receipt.ValidationEvidenceReaderFault, match="exceeds 32 bytes"):
                receipt.row_qualification(second, second["commit"], policy10())


def test_malformed_native_digest_defers_to_earlier_base_refusal():
    row = row10()
    row["base_sha"] = None
    reply = json.dumps({"row_sha256": [], "status": "refused", "reason": "synthetic"}).encode() + b"\n"
    with mock.patch.object(receipt, "_capture_validation_evidence", return_value=(0, reply, b"")):
        with receipt.validation_evidence_batch([row], predicate=policy10()):
            assert receipt.validation_evidence(row).status == "reader-fault"
            assert receipt.row_qualification(row, row["commit"], policy10()) == (False, "base base-sha-missing")


def test_request_file_error_retains_every_row_and_original_cause():
    first, second = row10("first"), row10("second")
    first["base_sha"] = None
    with mock.patch.object(receipt.tempfile, "TemporaryFile", side_effect=OSError("synthetic request storage unavailable")):
        with receipt.validation_evidence_batch([first, second], predicate=policy10()):
            assert receipt.row_qualification(first, first["commit"], policy10()) == (False, "base base-sha-missing")
            for row in (first, second):
                checked = receipt.validation_evidence(row)
                assert checked.status == "reader-fault"
                assert checked.reason == "validation artifact checker could not complete: synthetic request storage unavailable"


def test_live_batch_overflow_cannot_accept_an_earlier_verified_response():
    first, second = row10("first"), row10("second")
    first["cell_results"] = {}
    reply = {
        "row_sha256": receipt._evidence_row_digest(first), "status": "verified",
        "full_validation_refusal": None, "backend_parity_matches": [],
        "evidence": {"cell_results": {}, "test_results": {}, "observations": [],
                     "missing_cells": [], "missing_backend_parity": [],
                     "missing_test_producers": [], "full_test_results": True,
                     "full_backend_parity": False},
    }
    raw = json.dumps(reply).encode() + b"\n"
    limit = len(raw) + 128
    code = f"import os; os.write(1,{raw!r}); os.write(1,b'x'*129)"
    capture = receipt._capture_validation_evidence

    def small_capture(command, source, **limits):
        return capture([sys.executable, "-B", "-c", code], source,
                       line_bytes=limit, response_bytes=limit,
                       diagnostic_bytes=64, wall_seconds=3)

    with mock.patch.object(receipt, "_capture_validation_evidence", side_effect=small_capture):
        with receipt.validation_evidence_batch([first, second], predicate=policy10()):
            for row in (first, second):
                checked = receipt.validation_evidence(row)
                assert checked.status == "reader-fault"
                assert checked.reason == f"validation artifact checker response output exceeds {limit} bytes"
                with pytest.raises(receipt.ValidationEvidenceReaderFault):
                    receipt.row_qualification(row, row["commit"], policy10())


def capture_child(code, *, line=64, output=64, diagnostic=64, wall=3):
    with tempfile.TemporaryFile() as source:
        return receipt._capture_validation_evidence(
            [sys.executable, "-B", "-c", code], source,
            line_bytes=line, response_bytes=output, diagnostic_bytes=diagnostic,
            wall_seconds=wall)


@pytest.mark.parametrize("bound", ["line", "output", "diagnostic"])
def test_live_capture_exact_capacity_and_one_byte_over(bound):
    # Distinct line and total limits prove each guard rather than one shared cap.
    values = {"line": (b"x" * 7 + b"\n", b"", 8, 32, 32),
              "output": (b"a\n" * 4, b"", 4, 8, 32),
              "diagnostic": (b"", b"e" * 8, 32, 32, 8)}
    stdout, stderr, line, output, diagnostic = values[bound]
    code = f"import os; os.write(1,{stdout!r}); os.write(2,{stderr!r})"
    assert capture_child(code, line=line, output=output, diagnostic=diagnostic) == (0, stdout, stderr)
    if bound == "diagnostic":
        stderr += b"e"
    elif bound == "line":
        stdout = b"x" + stdout
    else:
        stdout += b"z"
    code = f"import os; os.write(1,{stdout!r}); os.write(2,{stderr!r})"
    message = {"line": "response line", "output": "response output",
               "diagnostic": "diagnostic output"}[bound]
    with pytest.raises(receipt.ValidationEvidenceReaderFault, match=f"{message} exceeds 8 bytes"):
        capture_child(code, line=line, output=output, diagnostic=diagnostic)


def process_running(pid):
    try:
        state = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[0]
    except FileNotFoundError:
        return False
    return state != "Z"


@pytest.mark.parametrize("cause", ["timeout", "output"])
def test_live_capture_stops_child_group_and_reaps_direct_child(tmp_path, cause):
    pids = tmp_path / "pids"
    code = f"""import os,time
pid = os.fork()
if pid == 0:
    time.sleep(10)
else:
    with open({str(pids)!r}, 'w') as out:
        out.write(str(os.getpid()) + ' ' + str(pid))
    if {cause!r} == 'output':
        os.write(1, b'x' * 65)
    time.sleep(10)
"""
    children = []
    real = receipt.subprocess.Popen

    def spawn(*args, **kwargs):
        child = real(*args, **kwargs)
        children.append(child)
        return child

    started = time.monotonic()
    try:
        with mock.patch.object(receipt.subprocess, "Popen", side_effect=spawn):
            with pytest.raises(receipt.ValidationEvidenceReaderFault, match="timed out after 1 seconds" if cause == "timeout" else "response output exceeds 64 bytes"):
                capture_child(code, wall=1, line=128)
        assert time.monotonic() - started < 3
        assert len(children) == 1 and children[0].returncode == -signal.SIGKILL
        with pytest.raises(ChildProcessError):
            os.waitpid(children[0].pid, os.WNOHANG)
        recorded = [int(value) for value in pids.read_text().split()]
        assert recorded[0] == children[0].pid
        deadline = time.monotonic() + 1
        while any(process_running(pid) for pid in recorded) and time.monotonic() < deadline:
            time.sleep(.01)
        assert all(not process_running(pid) for pid in recorded)
    finally:
        for child in children:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=2)


def test_live_capture_returns_after_child_exit_with_inherited_pipe(tmp_path):
    marker = tmp_path / "descendant"
    code = f"""import os,time
pid = os.fork()
if pid == 0:
    time.sleep(10)
else:
    with open({str(marker)!r}, 'w') as out:
        out.write(str(os.getpid()) + ' ' + str(pid))
    os.write(1,b'finished\\n')
"""
    started = time.monotonic()
    group = None
    try:
        result = capture_child(code, wall=2)
        assert result == (0, b"finished\n", b"")
        assert time.monotonic() - started < 1
        group, descendant = map(int, marker.read_text().split())
        assert process_running(descendant)
    finally:
        # The normal-exit case intentionally leaves a holder to prove we never
        # await EOF. The test owns and removes that group after the observation.
        if group is None and marker.exists():
            group = int(marker.read_text().split()[0])
        if group is not None:
            try:
                os.killpg(group, signal.SIGKILL)
            except ProcessLookupError:
                pass
