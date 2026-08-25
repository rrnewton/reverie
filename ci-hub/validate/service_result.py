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
    # NO EVIDENCE AT ALL. Not a verdict -- an admission.
    #
    # This branch is reached when there is neither a durable inner exit in the
    # log nor a resolvable systemd unit. The writer knows NOTHING about the
    # outcome, and it used to answer `not-run` / `no-result`, which are POSITIVE
    # CLAIMS THAT THE RUN NEVER HAPPENED. They are not the same statement as "I
    # cannot tell", and readers cannot recover the difference: the honest
    # `detail` string sat right beside a `state` that contradicted it.
    #
    # Measured on 2026-08-24, three times in one night, all costly:
    #   * hermit-005's record said finished while its unit ran another 50
    #     MINUTES. A quiet box was declared on that record and the window was
    #     spent while the box was not quiet.
    #   * hermit-006's earlier record said the run was dead while that unit
    #     still held the box-global lock.
    #   * the record for the main-tip run said `not-run` for a run whose
    #     canonical verdict is VALIDATED.
    # The failure ran in BOTH directions -- finished-while-running and
    # not-run-while-passed -- so it was never lag or truncation. It was a writer
    # reporting its own ignorance as a fact about the run.
    return {
        **common,
        "state": "unknown",
        "result": "unknown",
        "detail": "no durable inner exit and no live systemd unit identity",
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
    if record.get("result") == "success":
        nodes = record.get("executed_nodes")
        return isinstance(nodes, int) and nodes > 0 and record.get("exit_code") == 0
    return isinstance(record.get("exit_code"), int)
