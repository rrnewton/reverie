#!/usr/bin/env python3
"""Classify a detached validate from durable execution evidence.

``systemd-run --collect`` removes a transient unit as soon as it finishes.  A
later ``systemctl show`` still exits zero, but describes a synthetic
``LoadState=not-found`` unit with ``Result=success`` and
``ExecMainStatus=0``.  Those values are absence, not a verdict.

The cost wrapper's final line is written by the process that actually waited
for ``ci-hub validate-lock``.  It therefore supplies the inner exit after the
unit has been collected.  The validate driver's final status supplies the
outcome.  Both channels must agree; prose elsewhere in the log is diagnostic,
not a verdict.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

from final_validate_status import (
    FINAL_VALIDATE_STATUS_EXIT_CODES,
    FinalValidateStatusError,
    read_final_validate_status,
    require_final_validate_status_exit,
)


ACTUAL_EXIT_RE = re.compile(
    r"^# ci-hub/validate-lock tool COST ACTUAL\b.*\bexit=(signal:[0-9]+|[0-9]+|unknown)\s*$"
)
NODE_SUMMARY_RE = re.compile(r"^\s*nodes:\s+([0-9]+)\s+executed\b")
TEST_SUMMARY_RE = re.compile(r"^\s*([0-9]+)\s+test\(s\) executed\b")


@dataclass(frozen=True)
class RunEvidence:
    inner_exit: int | None = None
    executed_nodes: int | None = None
    executed_tests: int | None = None
    final_validate_status: str | None = None
    final_validate_status_error: str | None = None


def _decode_exit(raw: str) -> int | None:
    if raw == "unknown":
        return None
    if raw.startswith("signal:"):
        return 128 + int(raw.removeprefix("signal:"))
    return int(raw)


def read_output_evidence(output: str) -> RunEvidence:
    """Read the last authoritative inner exit, status, and count summaries."""
    inner_exit: int | None = None
    executed_nodes: int | None = None
    executed_tests: int | None = None
    lines = output.splitlines()
    for line in lines:
        actual = ACTUAL_EXIT_RE.match(line)
        if actual:
            inner_exit = _decode_exit(actual.group(1))
        nodes = NODE_SUMMARY_RE.match(line)
        if nodes:
            executed_nodes = int(nodes.group(1))
        tests = TEST_SUMMARY_RE.match(line)
        if tests:
            executed_tests = int(tests.group(1))
    final_status: str | None = None
    status_error: str | None = None
    try:
        final_status = read_final_validate_status("\n".join(lines))
    except FinalValidateStatusError as error:
        status_error = str(error)
    return RunEvidence(
        inner_exit=inner_exit,
        executed_nodes=executed_nodes,
        executed_tests=executed_tests,
        final_validate_status=final_status,
        final_validate_status_error=status_error,
    )


def read_log_evidence(path: Path) -> RunEvidence:
    """Read one durable log without turning an unreadable file into a verdict."""
    try:
        output = path.read_text(errors="replace")
    except (FileNotFoundError, OSError):
        return RunEvidence()
    return read_output_evidence(output)


def service_exit(properties: Mapping[str, str] | None) -> int | None:
    """Decode a real loaded unit's exit; reject collected-unit placeholders."""
    if not properties or properties.get("LoadState") != "loaded":
        return None
    if not properties.get("InvocationID"):
        return None
    try:
        status = int(properties.get("ExecMainStatus", ""))
    except ValueError:
        return None
    code = properties.get("ExecMainCode")
    if code in {"killed", "dumped"}:
        return 128 + status
    if code == "exited":
        return status
    return None


def classify(
    evidence: RunEvidence,
    *,
    properties: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    """Return one fail-closed terminal record.

    The durable inner exit wins over the service manager because it is the
    process that actually waited for validate-lock.  Systemd is only a fallback
    while the concrete unit identity still exists.
    """
    exit_code = evidence.inner_exit
    source = "durable-log"
    if exit_code is None:
        exit_code = service_exit(properties)
        source = "loaded-systemd-unit" if exit_code is not None else "no-exit-evidence"

    common: dict[str, Any] = {
        "exit_code": exit_code,
        "executed_nodes": evidence.executed_nodes,
        "executed_tests": evidence.executed_tests,
        "final_validate_status": evidence.final_validate_status,
        "result_source": source,
    }
    if evidence.final_validate_status_error is not None:
        return {
            **common,
            "state": "unknown",
            "result": "unknown",
            "detail": evidence.final_validate_status_error,
        }
    status: str | None = None
    if evidence.final_validate_status is not None:
        try:
            status = evidence.final_validate_status
            require_final_validate_status_exit(status, exit_code)
        except FinalValidateStatusError as error:
            return {
                **common,
                "state": "unknown",
                "result": "unknown",
                "detail": str(error),
            }
    if status == "PASSED":
        return {**common, "state": "completed", "result": "success"}
    if status == "FAILED":
        return {**common, "state": "completed", "result": "failure"}
    if status == "COULD_NOT_RUN":
        return {**common, "state": "completed", "result": "no-result"}
    if exit_code is not None and 129 <= exit_code <= 192:
        return {
            **common,
            "state": "killed",
            "result": "terminated",
            "signal": exit_code - 128,
        }
    # No final status means validate died before reporting.  An exit alone is
    # not promoted into passed, failed, or could-not-run.
    return {
        **common,
        "state": "unknown",
        "result": "unknown",
        "detail": "final validate status is absent",
    }


def from_log(
    path: Path, *, properties: Mapping[str, str] | None = None
) -> dict[str, Any]:
    return classify(read_log_evidence(path), properties=properties)


def is_evidenced_terminal(record: Mapping[str, Any]) -> bool:
    """Accept a durable terminal row only when its positive carries coverage.

    `unknown` is deliberately NOT terminal. It is the one state that means the
    writer could not tell, so freezing it as a durable answer is exactly the
    defect: an ignorant row would be preserved and re-served instead of being
    re-derived once the log or the unit can answer.
    """
    state = record.get("state")
    if state == "unknown":
        return False
    if state in {"killed", "not-run", "refused"}:
        return True
    if state != "completed":
        return False
    status = record.get("final_validate_status")
    expected = FINAL_VALIDATE_STATUS_EXIT_CODES.get(status)
    return expected is not None and record.get("exit_code") == expected
