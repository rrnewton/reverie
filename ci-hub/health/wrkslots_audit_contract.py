"""Strict contract shared by the wrkslots audit producer and consumer."""

from __future__ import annotations

import datetime as dt
import math
import re
from typing import Any


RECEIPT_SCHEMA = "wrkslots-audit-result/v1"
AUDIT_SCHEMA = 2
TOOL_NAME = "ci-hub/wrkslots-audit"
SHA = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
SCOPE = re.compile(r"^hermit-box-run-[0-9]+-[0-9]+[.]scope$")
COMPLETED_CPU_SOURCE = "systemd CPUUsageNSec after scope exit"
HISTORICAL_CPU_SOURCE = "cgroup-v2 cpu.stat usage_usec"


class ContractError(ValueError):
    """A producer or receipt violated the typed audit contract."""


def _dict(value: object, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ContractError(f"{field} is not an object")
    return value


def _integer(value: object, field: str, *, positive: bool = False) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ContractError(f"{field} is not an integer")
    if value < (1 if positive else 0):
        qualifier = "positive" if positive else "non-negative"
        raise ContractError(f"{field} is not {qualifier}")
    return value


def _number(value: object, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ContractError(f"{field} is not numeric")
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise ContractError(f"{field} is not finite and non-negative")
    return number


def _strings(value: object, field: str) -> list[str]:
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise ContractError(f"{field} is not a string list")
    if len(set(value)) != len(value):
        raise ContractError(f"{field} contains duplicates")
    return value


def _summary(value: object, field: str = "summary") -> str:
    if not isinstance(value, str) or not value or value != " ".join(value.split()):
        raise ContractError(f"{field} is absent or not one normalized line")
    if len(value) > 500:
        raise ContractError(f"{field} exceeds 500 characters")
    return value


def _timestamp(value: object, field: str) -> float:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise ContractError(f"{field} is not a UTC timestamp")
    try:
        timestamp = dt.datetime.fromisoformat(value.removesuffix("Z") + "+00:00")
    except ValueError as error:
        raise ContractError(f"{field} is not a UTC timestamp") from error
    return timestamp.timestamp()


def validate_audit_payload(payload: object, *, exit_code: int) -> dict[str, Any]:
    """Validate and reduce wrkslots' JSON without trusting summary prose."""

    audit = _dict(payload, "audit")
    if audit.get("schema") != AUDIT_SCHEMA:
        raise ContractError(f"audit schema is not {AUDIT_SCHEMA}")
    state = audit.get("state")
    expected_exit = {"ok": 0, "actionable": 1, "unknown": 2}
    if state not in expected_exit:
        raise ContractError(f"audit state {state!r} is unknown")
    if exit_code != expected_exit[state]:
        raise ContractError(f"audit state {state!r} contradicts exit {exit_code}")

    summary = _summary(audit.get("summary"), "audit.summary")
    worktrees = _integer(audit.get("worktree_count"), "audit.worktree_count")
    running = _integer(audit.get("running_agent_count"), "audit.running_agent_count")
    attention_count = _integer(audit.get("attention_count"), "audit.attention_count")
    attention_slots = _strings(audit.get("attention_slots"), "audit.attention_slots")
    unknown_count = _integer(audit.get("unknown_count"), "audit.unknown_count")
    unknown_slots = _strings(audit.get("unknown_slots"), "audit.unknown_slots")
    if attention_count != len(attention_slots):
        raise ContractError("audit attention count and slots disagree")
    if unknown_count != len(unknown_slots):
        raise ContractError("audit unknown count and slots disagree")
    if set(attention_slots) & set(unknown_slots):
        raise ContractError("audit attention and unknown slots overlap")
    if state == "ok" and (attention_count or unknown_count):
        raise ContractError("ok audit carries attention or unknown slots")
    if state == "actionable" and attention_count == 0:
        raise ContractError("actionable audit has no attention slots")
    if state == "unknown" and (unknown_count == 0 or attention_count != 0):
        raise ContractError("unknown audit has contradictory slot counts")

    return {
        "schema": AUDIT_SCHEMA,
        "state": state,
        "summary": summary,
        "worktree_count": worktrees,
        "running_agent_count": running,
        "attention_count": attention_count,
        "attention_slots": attention_slots,
        "unknown_count": unknown_count,
        "unknown_slots": unknown_slots,
    }


def validate_observer_cost(cost: object, *, exit_code: int) -> dict[str, Any]:
    record = _dict(cost, "observer_cost")
    if record.get("schema_version") != 1:
        raise ContractError("observer cost schema_version is not 1")
    if record.get("tool") != TOOL_NAME:
        raise ContractError(f"observer cost tool is not {TOOL_NAME}")
    estimate = _dict(record.get("estimate"), "observer_cost.estimate")
    if estimate.get("kind") != "derived":
        raise ContractError("observer cost estimate is not derived")
    _number(estimate.get("wall_seconds"), "observer_cost.estimate.wall_seconds")
    _number(estimate.get("cpu_seconds"), "observer_cost.estimate.cpu_seconds")
    _summary(estimate.get("basis"), "observer_cost.estimate.basis")

    actual = _dict(record.get("actual"), "observer_cost.actual")
    _number(actual.get("wall_seconds"), "observer_cost.actual.wall_seconds")
    cpu = _number(actual.get("cpu_seconds"), "observer_cost.actual.cpu_seconds")
    user = _number(
        actual.get("cpu_user_seconds"), "observer_cost.actual.cpu_user_seconds"
    )
    system = _number(
        actual.get("cpu_system_seconds"), "observer_cost.actual.cpu_system_seconds"
    )
    if not math.isclose(cpu, user + system, rel_tol=1e-9, abs_tol=1e-9):
        raise ContractError("observer cost CPU total contradicts user+system")
    expected_exits = {str(exit_code)}
    if 129 <= exit_code <= 255:
        expected_exits.add(f"signal:{exit_code - 128}")
    if actual.get("exit") not in expected_exits:
        raise ContractError("observer cost exit contradicts producer exit")
    return record


def validate_audit_cost(
    cost: object,
    *,
    observer_cost: dict[str, Any],
    exit_code: int,
    cpu_budget_seconds: int,
    wall_backstop_seconds: int,
) -> dict[str, Any]:
    record = _dict(cost, "cost")
    if record.get("schema") != "wrkslots-audit-cost/v1":
        raise ContractError("audit cost schema is not wrkslots-audit-cost/v1")
    wall = _number(record.get("wall_seconds"), "cost.wall_seconds")
    cpu = _number(record.get("cpu_seconds"), "cost.cpu_seconds")
    usage_usec = _integer(record.get("cgroup_usage_usec"), "cost.cgroup_usage_usec")
    if not math.isclose(cpu, usage_usec / 1_000_000, rel_tol=0, abs_tol=1e-9):
        raise ContractError("audit cost CPU contradicts cgroup usage_usec")
    observer_wall = _number(
        _dict(observer_cost.get("actual"), "observer_cost.actual").get("wall_seconds"),
        "observer_cost.actual.wall_seconds",
    )
    if not math.isclose(wall, observer_wall, rel_tol=0, abs_tol=1e-9):
        raise ContractError("audit cost wall contradicts outer observer")
    if record.get("cpu_source") == COMPLETED_CPU_SOURCE:
        usage_nsec = _integer(record.get("cpu_usage_nsec"), "cost.cpu_usage_nsec")
        # UINT64_MAX is systemd's unavailable-counter sentinel, not a measured
        # value. Preserve the exact completed counter and round only its unit
        # conversion, so sub-microsecond usage is never rounded down.
        if usage_nsec >= 2**64 - 1:
            raise ContractError("audit cost CPUUsageNSec is unavailable or out of range")
        if usage_usec != (usage_nsec + 999) // 1000:
            raise ContractError("audit cost usage_usec contradicts completed CPUUsageNSec")
    elif record.get("cpu_source") == HISTORICAL_CPU_SOURCE:
        if "cpu_usage_nsec" in record:
            raise ContractError("historical audit CPU source carries completed CPUUsageNSec")
    else:
        raise ContractError("audit cost has unknown CPU source")
    if record.get("wall_source") != "outer monotonic observer":
        raise ContractError("audit cost has unknown wall source")
    if (
        _integer(
            record.get("cpu_budget_seconds"),
            "cost.cpu_budget_seconds",
            positive=True,
        )
        != cpu_budget_seconds
    ):
        raise ContractError("audit cost CPU budget contradicts receipt limit")
    if (
        _integer(
            record.get("wall_backstop_seconds"),
            "cost.wall_backstop_seconds",
            positive=True,
        )
        != wall_backstop_seconds
    ):
        raise ContractError("audit cost wall backstop contradicts receipt limit")
    if _integer(record.get("exit_code"), "cost.exit_code") != exit_code:
        raise ContractError("audit cost exit contradicts producer exit")
    reaped = record.get("reaped")
    if not isinstance(reaped, bool):
        raise ContractError("audit cost reaped is not boolean")
    scope = record.get("scope")
    if not isinstance(scope, str) or not SCOPE.fullmatch(scope):
        raise ContractError("audit cost scope is invalid")
    termination = record.get("termination")
    if termination not in ("completed", "cpu_budget", "wall_backstop"):
        raise ContractError("audit cost termination is invalid")
    if termination == "completed" and reaped:
        raise ContractError("completed audit cost claims a limit reap")
    if termination in ("cpu_budget", "wall_backstop"):
        if not reaped or exit_code != 137:
            raise ContractError("limited audit cost contradicts reap or exit")
    if termination == "cpu_budget" and usage_usec <= cpu_budget_seconds * 1_000_000:
        raise ContractError("CPU-limited audit cost did not exceed its budget")
    if termination == "wall_backstop" and wall + 1.0 < wall_backstop_seconds:
        raise ContractError("wall-limited audit cost did not reach its backstop")
    return record


def validate_receipt(
    receipt: object,
    *,
    expected_producer: dict[str, str] | None = None,
) -> dict[str, Any]:
    result = _dict(receipt, "receipt")
    if result.get("schema") != RECEIPT_SCHEMA:
        raise ContractError(f"unsupported result schema {result.get('schema')!r}")
    started_iso = _timestamp(result.get("started_at"), "started_at")
    observed_iso = _timestamp(result.get("observed_at"), "observed_at")
    started = _number(result.get("started_at_epoch_seconds"), "started time")
    observed = _number(result.get("observed_at_epoch_seconds"), "observation time")
    if not math.isclose(started_iso, started, rel_tol=0, abs_tol=0.001):
        raise ContractError("started_at contradicts its epoch value")
    if not math.isclose(observed_iso, observed, rel_tol=0, abs_tol=0.001):
        raise ContractError("observed_at contradicts its epoch value")
    if observed < started:
        raise ContractError("observation time precedes start time")

    producer = _dict(result.get("producer"), "producer")
    for name in ("parent_sha", "hermit_sha", "agent_utils_sha"):
        value = producer.get(name)
        if not isinstance(value, str) or not SHA.fullmatch(value):
            raise ContractError(f"producer {name} is not a commit SHA")
    if expected_producer is not None:
        mismatch = [
            name
            for name, expected in expected_producer.items()
            if producer.get(name) != expected
        ]
        if mismatch:
            raise ContractError("producer provenance mismatch: " + ",".join(mismatch))

    limits = _dict(result.get("limits"), "limits")
    cpu_budget_seconds = _integer(
        limits.get("cpu_budget_seconds"), "CPU limit", positive=True
    )
    wall_backstop_seconds = _integer(
        limits.get("wall_backstop_seconds"), "wall limit", positive=True
    )
    exit_code = _integer(result.get("exit_code"), "exit_code")
    verdict = result.get("verdict")
    if verdict not in ("OK", "ACTIONABLE", "NO_RESULT"):
        raise ContractError(f"unknown verdict {verdict!r}")
    summary = _summary(result.get("summary"))
    stderr_sha256 = result.get("stderr_sha256")
    if not isinstance(stderr_sha256, str) or not SHA256.fullmatch(stderr_sha256):
        raise ContractError("stderr_sha256 is not a SHA-256 digest")

    audit = result.get("audit")
    if audit is None:
        if verdict != "NO_RESULT":
            raise ContractError(f"{verdict} receipt has no audit")
    else:
        audit = validate_audit_payload(audit, exit_code=exit_code)
        expected_verdict = {
            "ok": "OK",
            "actionable": "ACTIONABLE",
            "unknown": "NO_RESULT",
        }[audit["state"]]
        if verdict != expected_verdict:
            raise ContractError("verdict contradicts audit state")
        if summary != audit["summary"]:
            raise ContractError("receipt summary contradicts audit summary")

    observer_cost = result.get("observer_cost")
    if observer_cost is not None:
        observer_cost = validate_observer_cost(observer_cost, exit_code=exit_code)
    cost = result.get("cost")
    if cost is None:
        if audit is not None:
            raise ContractError("completed audit has no cost actual")
    else:
        if observer_cost is None:
            raise ContractError("audit cost has no outer observer cost")
        audit_cost = validate_audit_cost(
            cost,
            observer_cost=observer_cost,
            exit_code=exit_code,
            cpu_budget_seconds=cpu_budget_seconds,
            wall_backstop_seconds=wall_backstop_seconds,
        )
        if audit is not None and audit_cost["termination"] != "completed":
            raise ContractError("completed audit contradicts boxed termination")
    return result
