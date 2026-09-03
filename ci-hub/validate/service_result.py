#!/usr/bin/env python3
"""Classify a detached validate from its framework result or retained log.

``systemd-run --collect`` removes a transient unit as soon as it finishes.  A
later ``systemctl show`` still exits zero, but describes a synthetic
``LoadState=not-found`` unit with ``Result=success`` and
``ExecMainStatus=0``.  Those values are absence, not a verdict.

Current Hermit targets write ``ValidationServiceResult`` directly. Targets
predating that schema retain the older contract below: the cost wrapper's final
line supplies the inner exit and the validation driver's final status supplies
the outcome. Both historical channels must agree; prose elsewhere in the log
is diagnostic, not a verdict.
"""

from __future__ import annotations

import json
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
HISTORICAL_SCHEMA_VERSION = 1
WRITEBACK_SCHEMA_VERSION = 2
SCHEMA_VERSION = 3
SCHEMA_RELATIVE_PATH = Path(
    "ci/manifest-plan/validation-service-result-schema.json"
)
HISTORICAL_FIELD_NAMES = (
    "schema_version",
    "commit",
    "profile",
    "final_validate_status",
    "exit_code",
    "executed_nodes",
    "executed_tests",
)
WRITEBACK_FIELD_NAMES = HISTORICAL_FIELD_NAMES + ("scorecard_writeback",)
FIELD_NAMES = (
    "schema_version",
    "commit",
    "profile",
    "selection_mode",
    "final_validate_status",
    "exit_code",
    "executed_nodes",
    "executed_tests",
    "scorecard_writeback",
)
HISTORICAL_OUTCOME_FIELDS = frozenset(("exit_code", "state", "result"))
OUTCOME_FIELDS = frozenset(("validation_exit_code", "state", "result"))
EXPECTED_OUTCOMES = {
    "PASSED": {
        "validation_exit_code": 0,
        "state": "completed",
        "result": "success",
    },
    "FAILED": {
        "validation_exit_code": 1,
        "state": "completed",
        "result": "failure",
    },
    "COULD_NOT_RUN": {
        "validation_exit_code": 75,
        "state": "completed",
        "result": "no-result",
    },
}
HISTORICAL_EXPECTED_OUTCOMES = {
    status: {
        "exit_code": row["validation_exit_code"],
        "state": row["state"],
        "result": row["result"],
    }
    for status, row in EXPECTED_OUTCOMES.items()
}
EXPECTED_SCORECARD_WRITEBACK = {
    "nullable": True,
    "variants": {
        "completed": ["status"],
        "failed": ["status", "error"],
    },
}


@dataclass(frozen=True)
class RunEvidence:
    inner_exit: int | None = None
    executed_nodes: int | None = None
    executed_tests: int | None = None
    final_validate_status: str | None = None
    final_validate_status_error: str | None = None
    result_source: str = "durable-log"
    commit: str | None = None
    profile: str | None = None
    selection_mode: str | None = None
    service_result_schema: int | None = None
    scorecard_writeback: dict[str, str] | None = None


def _error(field: str, detail: str) -> RuntimeError:
    return RuntimeError(f"validation-service-result-{field}: {detail}")


def _json_object(path: Path, *, role: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise _error(role, f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise _error(role, f"{path} must contain an object")
    return value


def read_schema(path: Path) -> dict[str, Any]:
    """Read the checked schema projection and refuse drift by field name."""
    value = _json_object(path, role="schema")
    version = value.get("schema_version")
    expected_top = {"schema_version", "fields", "outcomes"}
    if version in {WRITEBACK_SCHEMA_VERSION, SCHEMA_VERSION}:
        expected_top.add("scorecard_writeback")
    if set(value) != expected_top:
        raise _error(
            "schema-fields",
            f"expected {sorted(expected_top)}, got {sorted(value)}",
        )
    if version not in {
        HISTORICAL_SCHEMA_VERSION,
        WRITEBACK_SCHEMA_VERSION,
        SCHEMA_VERSION,
    }:
        raise _error(
            "schema_version",
            f"expected {HISTORICAL_SCHEMA_VERSION}, {WRITEBACK_SCHEMA_VERSION}, or {SCHEMA_VERSION}, got {version!r}",
        )
    expected_fields = {
        HISTORICAL_SCHEMA_VERSION: HISTORICAL_FIELD_NAMES,
        WRITEBACK_SCHEMA_VERSION: WRITEBACK_FIELD_NAMES,
        SCHEMA_VERSION: FIELD_NAMES,
    }[version]
    fields = value.get("fields")
    if fields != list(expected_fields):
        raise _error(
            "schema-fields", f"expected {list(expected_fields)!r}, got {fields!r}"
        )
    expected_outcomes = (
        HISTORICAL_EXPECTED_OUTCOMES
        if version == HISTORICAL_SCHEMA_VERSION
        else EXPECTED_OUTCOMES
    )
    outcome_fields = (
        HISTORICAL_OUTCOME_FIELDS
        if version == HISTORICAL_SCHEMA_VERSION
        else OUTCOME_FIELDS
    )
    outcomes = value.get("outcomes")
    if not isinstance(outcomes, dict) or set(outcomes) != set(expected_outcomes):
        raise _error(
            "schema-outcomes",
            f"expected {sorted(expected_outcomes)}, got "
            f"{sorted(outcomes) if isinstance(outcomes, dict) else outcomes!r}",
        )
    for status, expected in expected_outcomes.items():
        row = outcomes.get(status)
        if not isinstance(row, dict) or set(row) != outcome_fields or row != expected:
            raise _error(
                "schema-outcomes", f"{status} expected {expected!r}, got {row!r}"
            )
    exit_field = (
        "exit_code" if version == HISTORICAL_SCHEMA_VERSION else "validation_exit_code"
    )
    declared_exits = {
        status: row[exit_field] for status, row in expected_outcomes.items()
    }
    if declared_exits != FINAL_VALIDATE_STATUS_EXIT_CODES:
        raise _error(
            "schema-outcomes",
            "schema exits disagree with ci-hub final validate status exits",
        )
    if version in {WRITEBACK_SCHEMA_VERSION, SCHEMA_VERSION}:
        writeback = value.get("scorecard_writeback")
        if writeback != EXPECTED_SCORECARD_WRITEBACK:
            raise _error(
                "schema-scorecard_writeback",
                f"expected {EXPECTED_SCORECARD_WRITEBACK!r}, got {writeback!r}",
            )
    return value


def schema_for_checkout(checkout: Path, *, reference_checkout: Path) -> int | None:
    """Return the target schema, or None for a target predating the contract.

    A target that advertises the file must agree by value with the parent
    checkout's pinned producer schema. An absent file selects only the retained
    historical log reader; it never makes a current malformed result acceptable.
    """
    target_path = checkout / SCHEMA_RELATIVE_PATH
    if not target_path.exists():
        return None
    target = read_schema(target_path)
    reference_path = reference_checkout / SCHEMA_RELATIVE_PATH
    reference = read_schema(reference_path)
    if target["schema_version"] == SCHEMA_VERSION and target != reference:
        raise _error(
            "schema",
            f"target {target_path} does not match pinned producer {reference_path}",
        )
    return int(target["schema_version"])


SERVICE_RESULT_SUFFIX = ".service-result.json"


def result_path(record_path: Path) -> Path:
    return record_path.with_name(f"{record_path.stem}{SERVICE_RESULT_SUFFIX}")


def handle_path_for_result(result: Path) -> Path | None:
    """The run handle a framework result sidecar belongs to, or None.

    ⚠️ THIS IS THE INVERSE OF `result_path` AND IT LIVES BESIDE IT ON PURPOSE.
    Both sit in the runs directory and both end `.json`, so anything that
    enumerates run handles with a `*.json` glob picks up the sidecars too and
    then reports each one as a run handle with an unsupported schema. Measured
    2026-09-03: the validation cleanup gate reported 111 failures of which 98
    were exactly this, so a gate that had nothing wrong to say about them was
    red on all 98 and stayed red.

    Keeping the two directions adjacent is what stops the exclusion drifting
    from the naming: change one and the other is in view.
    """
    if not result.name.endswith(SERVICE_RESULT_SUFFIX):
        return None
    return result.with_name(f"{result.name.removesuffix(SERVICE_RESULT_SUFFIX)}.json")


def pinned_schema_path() -> Path:
    return Path(__file__).resolve().parents[2] / "hermit" / SCHEMA_RELATIVE_PATH


def read_framework_result_bytes(
    payload: bytes, *, expected_commit: str, schema_path: Path | None = None
) -> RunEvidence:
    """Read verified framework-result bytes under their exact versioned semantics."""
    schema = read_schema(schema_path or pinned_schema_path())
    if schema["schema_version"] != SCHEMA_VERSION:
        raise _error(
            "schema_version",
            f"pinned producer must declare current schema {SCHEMA_VERSION}",
        )
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise _error("read", f"malformed JSON: {error}") from error
    if not isinstance(value, dict):
        raise _error("read", "expected an object")
    version = value.get("schema_version")
    expected_fields = {
        HISTORICAL_SCHEMA_VERSION: HISTORICAL_FIELD_NAMES,
        WRITEBACK_SCHEMA_VERSION: WRITEBACK_FIELD_NAMES,
        SCHEMA_VERSION: FIELD_NAMES,
    }.get(version)
    if expected_fields is None:
        raise _error(
            "schema_version",
            f"expected {HISTORICAL_SCHEMA_VERSION}, {WRITEBACK_SCHEMA_VERSION}, or {SCHEMA_VERSION}, got {version!r}",
        )
    if set(value) != set(expected_fields):
        raise _error(
            "fields", f"schema {version} expected {sorted(expected_fields)}, got {sorted(value)}"
        )
    commit = value.get("commit")
    if commit != expected_commit:
        raise _error("commit", f"expected {expected_commit}, got {commit!r}")
    profile = value.get("profile")
    if not isinstance(profile, str) or not profile.strip():
        raise _error("profile", "must be a nonempty string")
    selection_mode: str | None = None
    if version == SCHEMA_VERSION:
        raw_selection = value.get("selection_mode")
        if raw_selection is not None and (
            not isinstance(raw_selection, str) or not raw_selection.strip()
        ):
            raise _error("selection_mode", "must be a nonempty string or null")
        selection_mode = raw_selection
    status = value.get("final_validate_status")
    if not isinstance(status, str) or status not in EXPECTED_OUTCOMES:
        raise _error("final_validate_status", f"unsupported value {status!r}")
    exit_code = value.get("exit_code")
    if not isinstance(exit_code, int) or isinstance(exit_code, bool):
        raise _error("exit_code", "must be an integer")
    scorecard_writeback: dict[str, str] | None = None
    if version in {WRITEBACK_SCHEMA_VERSION, SCHEMA_VERSION}:
        scorecard_writeback = read_scorecard_writeback_value(
            value.get("scorecard_writeback")
        )
        expected_exit = current_command_exit(status, scorecard_writeback)
        if exit_code != expected_exit:
            raise _error(
                "exit_code",
                f"{status} with scorecard_writeback {scorecard_writeback!r} "
                f"requires {expected_exit}, got {exit_code}",
            )
    else:
        expected_exit = HISTORICAL_EXPECTED_OUTCOMES[status]["exit_code"]
        if exit_code != expected_exit:
            raise _error(
                "exit_code",
                f"historical schema 1 {status} requires {expected_exit}, got {exit_code}",
            )
    executed_nodes = value.get("executed_nodes")
    if (
        not isinstance(executed_nodes, int)
        or isinstance(executed_nodes, bool)
        or executed_nodes < 0
    ):
        raise _error("executed_nodes", "must be a nonnegative integer")
    executed_tests = value.get("executed_tests")
    if executed_tests is not None and (
        not isinstance(executed_tests, int)
        or isinstance(executed_tests, bool)
        or executed_tests < 0
    ):
        raise _error("executed_tests", "must be a nonnegative integer or null")
    return RunEvidence(
        inner_exit=exit_code,
        executed_nodes=executed_nodes,
        executed_tests=executed_tests,
        final_validate_status=status,
        result_source=(
            "validation-service-result"
            if version == SCHEMA_VERSION
            else "historical-validation-service-result"
        ),
        commit=commit,
        profile=profile,
        selection_mode=selection_mode,
        service_result_schema=version,
        scorecard_writeback=scorecard_writeback,
    )


def read_framework_result(
    path: Path, *, expected_commit: str, schema_path: Path | None = None
) -> RunEvidence:
    """Read one framework-written result under its exact versioned semantics."""
    try:
        payload = path.read_bytes()
    except OSError as error:
        raise _error("read", f"cannot read {path}: {error}") from error
    return read_framework_result_bytes(
        payload,
        expected_commit=expected_commit,
        schema_path=schema_path,
    )


def read_scorecard_writeback_value(value: Any) -> dict[str, str] | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        raise _error("scorecard_writeback", "must be an object or null")
    status = value.get("status")
    if status == "completed" and set(value) == {"status"}:
        return {"status": "completed"}
    if status == "failed" and set(value) == {"status", "error"}:
        error = value.get("error")
        if isinstance(error, str) and error.strip():
            return {"status": "failed", "error": error}
        raise _error("scorecard_writeback", "failed requires a nonempty error")
    raise _error(
        "scorecard_writeback",
        "expected null, {'status': 'completed'}, or "
        "{'status': 'failed', 'error': <nonempty string>}",
    )


def current_command_exit(
    status: str, scorecard_writeback: Mapping[str, str] | None
) -> int:
    if status == "PASSED" and scorecard_writeback is not None:
        if scorecard_writeback.get("status") == "failed":
            return 75
    return FINAL_VALIDATE_STATUS_EXIT_CODES[status]


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
    source = evidence.result_source
    if exit_code is None:
        exit_code = service_exit(properties)
        if source == "durable-log":
            source = "loaded-systemd-unit" if exit_code is not None else "no-exit-evidence"

    common: dict[str, Any] = {
        "exit_code": exit_code,
        "executed_nodes": evidence.executed_nodes,
        "executed_tests": evidence.executed_tests,
        "final_validate_status": evidence.final_validate_status,
        "result_source": source,
    }
    if evidence.service_result_schema is not None:
        common["service_result_schema"] = evidence.service_result_schema
    if evidence.service_result_schema == SCHEMA_VERSION:
        common["selection_mode"] = evidence.selection_mode
    if evidence.service_result_schema == SCHEMA_VERSION:
        common["scorecard_writeback"] = evidence.scorecard_writeback
    if evidence.final_validate_status_error is not None:
        return {
            **common,
            "state": "unknown",
            "result": "unknown",
            "detail": evidence.final_validate_status_error,
        }
    status: str | None = None
    if evidence.final_validate_status is not None:
        if evidence.service_result_schema in {
            HISTORICAL_SCHEMA_VERSION,
            WRITEBACK_SCHEMA_VERSION,
        }:
            return {
                **common,
                "state": "unknown",
                "result": "unknown",
                "detail": (
                    "validation-service-result-schema_version: historical schema "
                    f"{evidence.service_result_schema} "
                    "is readable but has no current authority"
                ),
            }
        try:
            status = evidence.final_validate_status
            if evidence.service_result_schema == SCHEMA_VERSION:
                if status not in EXPECTED_OUTCOMES:
                    raise FinalValidateStatusError(f"unsupported value {status!r}")
                writeback = read_scorecard_writeback_value(
                    evidence.scorecard_writeback
                )
                expected_exit = current_command_exit(status, writeback)
                if exit_code != expected_exit:
                    raise FinalValidateStatusError(
                        f"{status} with scorecard_writeback "
                        f"{evidence.scorecard_writeback!r} requires exit "
                        f"{expected_exit}, got {exit_code!r}"
                    )
            else:
                require_final_validate_status_exit(status, exit_code)
        except (FinalValidateStatusError, RuntimeError) as error:
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


def from_run_record(
    record_path: Path,
    record: Mapping[str, Any],
    log_path: Path,
    *,
    schema_path: Path | None = None,
    properties: Mapping[str, str] | None = None,
) -> dict[str, Any]:
    """Use the producer result for current runs and logs for historical runs."""
    marker = record.get("service_result_schema")
    if marker is None:
        return from_log(log_path, properties=properties)
    if marker not in {
        HISTORICAL_SCHEMA_VERSION,
        WRITEBACK_SCHEMA_VERSION,
        SCHEMA_VERSION,
    }:
        return classify(
            RunEvidence(
                final_validate_status_error=(
                    "validation-service-result-schema_version: "
                    f"expected {HISTORICAL_SCHEMA_VERSION}, {WRITEBACK_SCHEMA_VERSION}, or {SCHEMA_VERSION}, got {marker!r}"
                ),
                result_source="validation-service-result",
            ),
            properties=properties,
        )
    expected_commit = record.get("target")
    if not isinstance(expected_commit, str):
        return classify(
            RunEvidence(
                final_validate_status_error=(
                    "validation-service-result-commit: run record target is absent"
                ),
                result_source="validation-service-result",
            ),
            properties=properties,
        )
    try:
        evidence = read_framework_result(
            result_path(record_path),
            expected_commit=expected_commit,
            schema_path=schema_path,
        )
        if evidence.service_result_schema != marker:
            raise _error(
                "schema_version",
                f"run record declares {marker}, result carries {evidence.service_result_schema}",
            )
    except RuntimeError as error:
        systemd_exit = service_exit(properties)
        if systemd_exit is not None and 129 <= systemd_exit <= 192:
            killed = classify(
                RunEvidence(
                    inner_exit=systemd_exit,
                    result_source="loaded-systemd-unit",
                )
            )
            killed["detail"] = str(error)
            return killed
        evidence = RunEvidence(
            final_validate_status_error=str(error),
            result_source="validation-service-result",
        )
    return classify(evidence, properties=properties)


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
    if not isinstance(status, str):
        return False
    schema_version = record.get("service_result_schema")
    if schema_version in {HISTORICAL_SCHEMA_VERSION, WRITEBACK_SCHEMA_VERSION}:
        return False
    if schema_version == SCHEMA_VERSION:
        if status not in EXPECTED_OUTCOMES:
            return False
        try:
            writeback = read_scorecard_writeback_value(
                record.get("scorecard_writeback")
            )
        except RuntimeError:
            return False
        expected = current_command_exit(status, writeback)
        return record.get("exit_code") == expected
    expected = FINAL_VALIDATE_STATUS_EXIT_CODES.get(status)
    return expected is not None and record.get("exit_code") == expected
