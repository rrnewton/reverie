#!/usr/bin/env python3
"""Classify a detached validate from durable execution evidence.

``systemd-run --collect`` removes a transient unit as soon as it finishes.  A
later ``systemctl show`` still exits zero, but describes a synthetic
``LoadState=not-found`` unit with ``Result=success`` and
``ExecMainStatus=0``.  Those values are absence, not a verdict.

The cost wrapper's final line is written by the process that actually waited
for ``ci-hub validate-lock``.  It therefore supplies the inner exit after the
unit has been collected.  A zero exit is only a passing validation when the
driver's final DAG summary also proves that at least one node executed.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping


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


def _decode_exit(raw: str) -> int | None:
    if raw == "unknown":
        return None
    if raw.startswith("signal:"):
        return 128 + int(raw.removeprefix("signal:"))
    return int(raw)


def read_log_evidence(path: Path) -> RunEvidence:
    """Read the last authoritative exit and top-level completion summaries."""
    inner_exit: int | None = None
    executed_nodes: int | None = None
    executed_tests: int | None = None
    try:
        lines = path.read_text(errors="replace").splitlines()
    except (FileNotFoundError, OSError):
        return RunEvidence()
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
    return RunEvidence(inner_exit, executed_nodes, executed_tests)


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
        "result_source": source,
    }
    if exit_code is not None and 129 <= exit_code <= 192:
        return {
            **common,
            "state": "killed",
            "result": "terminated",
            "signal": exit_code - 128,
        }
    if exit_code == 0 and evidence.executed_nodes is not None and evidence.executed_nodes > 0:
        return {**common, "state": "completed", "result": "success"}
    if exit_code == 0:
        return {
            **common,
            "state": "not-run",
            "result": "no-result",
            "detail": "exit 0 without evidence that any validation node executed",
        }
    if exit_code is not None:
        return {**common, "state": "completed", "result": "failure"}
    return {
        **common,
        "state": "not-run",
        "result": "no-result",
        "detail": "no durable inner exit and no live systemd unit identity",
    }


def from_log(
    path: Path, *, properties: Mapping[str, str] | None = None
) -> dict[str, Any]:
    return classify(read_log_evidence(path), properties=properties)


def is_evidenced_terminal(record: Mapping[str, Any]) -> bool:
    """Accept a durable terminal row only when its positive carries coverage."""
    state = record.get("state")
    if state in {"killed", "not-run", "refused"}:
        return True
    if state != "completed":
        return False
    if record.get("result") == "success":
        nodes = record.get("executed_nodes")
        return isinstance(nodes, int) and nodes > 0 and record.get("exit_code") == 0
    return isinstance(record.get("exit_code"), int)
