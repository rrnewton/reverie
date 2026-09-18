#!/usr/bin/env python3
"""Validate the TFA cohort and any instantiated per-target records.

The repository publishes a Draft 7 schema, but this checker intentionally uses
only the Python standard library.  It implements the structural subset used by
``experiment.schema.json`` and then checks the cross-record and evidentiary
rules that JSON Schema cannot express.
"""

from __future__ import annotations

import argparse
import copy
import datetime as dt
import hashlib
import json
import math
import re
import sys
import subprocess
from pathlib import Path, PurePosixPath
from typing import Any, Iterable


ALLOWED_SERVICES = frozenset(
    {
        "cachelib",
        "folly",
        "logdevice",
        "manifold",
        "mcrouter",
        "mysql",
        "proxygen",
        "thrift",
        "warm_storage",
        "zippydb",
    }
)
OUTCOME_KINDS = frozenset(
    {"WORKING", "BLOCKED-WITH-NAMED-CAUSE", "NOT-DETERMINABLE"}
)
U64_MAX = (1 << 64) - 1


class ValidationFailure(Exception):
    """All validation errors from one tree, retained in deterministic order."""

    def __init__(self, errors: Iterable[str]):
        self.errors = tuple(dict.fromkeys(errors))
        super().__init__("\n".join(self.errors))


def _load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        raise ValidationFailure([f"{path}: missing required file"]) from None
    except json.JSONDecodeError as exc:
        raise ValidationFailure(
            [f"{path}:{exc.lineno}:{exc.colno}: invalid JSON: {exc.msg}"]
        ) from None


def _json_type_ok(value: Any, wanted: str) -> bool:
    if wanted == "null":
        return value is None
    if wanted == "boolean":
        return isinstance(value, bool)
    if wanted == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if wanted == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if wanted == "string":
        return isinstance(value, str)
    if wanted == "array":
        return isinstance(value, list)
    if wanted == "object":
        return isinstance(value, dict)
    return False


def _json_equal(left: Any, right: Any) -> bool:
    """JSON Schema equality: booleans are not the numbers 0 and 1."""

    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    numeric = (int, float)
    if isinstance(left, numeric) and isinstance(right, numeric):
        return left == right
    return type(left) is type(right) and left == right


def _resolve_ref(root_schema: dict[str, Any], ref: str) -> dict[str, Any]:
    if not ref.startswith("#/"):
        raise ValueError(f"only local JSON pointers are supported: {ref}")
    value: Any = root_schema
    for raw in ref[2:].split("/"):
        token = raw.replace("~1", "/").replace("~0", "~")
        value = value[token]
    if not isinstance(value, dict):
        raise ValueError(f"schema reference is not an object: {ref}")
    return value


def _format_ok(value: str, kind: str) -> bool:
    try:
        if kind == "date":
            dt.date.fromisoformat(value)
        elif kind == "date-time":
            parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
            if parsed.tzinfo is None:
                return False
        else:
            return True
    except ValueError:
        return False
    return True


def _schema_errors(
    value: Any,
    schema: dict[str, Any],
    root_schema: dict[str, Any],
    path: str = "$",
) -> list[str]:
    """Validate the Draft 7 vocabulary used by the published TFA schema."""

    if "$ref" in schema:
        return _schema_errors(value, _resolve_ref(root_schema, schema["$ref"]), root_schema, path)

    errors: list[str] = []

    if "allOf" in schema:
        for part in schema["allOf"]:
            errors.extend(_schema_errors(value, part, root_schema, path))

    if "anyOf" in schema:
        branches = [_schema_errors(value, part, root_schema, path) for part in schema["anyOf"]]
        if not any(not branch for branch in branches):
            errors.append(f"{path}: does not satisfy any allowed schema branch")

    if "oneOf" in schema:
        branches = [_schema_errors(value, part, root_schema, path) for part in schema["oneOf"]]
        matches = sum(not branch for branch in branches)
        if matches != 1:
            errors.append(f"{path}: must satisfy exactly one schema branch (matched {matches})")
            if matches == 0 and branches:
                errors.extend(min(branches, key=len))

    condition = schema.get("if")
    if isinstance(condition, dict):
        condition_matches = not _schema_errors(value, condition, root_schema, path)
        selected = schema.get("then") if condition_matches else schema.get("else")
        if isinstance(selected, dict):
            errors.extend(_schema_errors(value, selected, root_schema, path))

    if "const" in schema and not _json_equal(value, schema["const"]):
        errors.append(f"{path}: must equal {schema['const']!r}")
    if "enum" in schema and not any(_json_equal(value, item) for item in schema["enum"]):
        errors.append(f"{path}: {value!r} is not one of {schema['enum']!r}")

    wanted = schema.get("type")
    if wanted is not None:
        wanted_types = [wanted] if isinstance(wanted, str) else wanted
        if not any(_json_type_ok(value, item) for item in wanted_types):
            errors.append(f"{path}: expected type {wanted!r}, got {type(value).__name__}")
            return errors

    if isinstance(value, dict):
        required = schema.get("required", [])
        for key in required:
            if key not in value:
                errors.append(f"{path}: missing required key {key!r}")
        properties = schema.get("properties", {})
        for key, child in properties.items():
            if key in value:
                errors.extend(_schema_errors(value[key], child, root_schema, f"{path}.{key}"))
        additional = schema.get("additionalProperties", True)
        for key in value.keys() - properties.keys():
            if additional is False:
                errors.append(f"{path}: unknown key {key!r}")
            elif isinstance(additional, dict):
                errors.extend(
                    _schema_errors(value[key], additional, root_schema, f"{path}.{key}")
                )
        if "minProperties" in schema and len(value) < schema["minProperties"]:
            errors.append(
                f"{path}: has {len(value)} properties, minimum is {schema['minProperties']}"
            )

    if isinstance(value, list):
        if "minItems" in schema and len(value) < schema["minItems"]:
            errors.append(f"{path}: has {len(value)} items, minimum is {schema['minItems']}")
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            errors.append(f"{path}: has {len(value)} items, maximum is {schema['maxItems']}")
        if schema.get("uniqueItems"):
            encoded = [json.dumps(item, sort_keys=True, separators=(",", ":")) for item in value]
            if len(encoded) != len(set(encoded)):
                errors.append(f"{path}: items must be unique")
        item_schema = schema.get("items")
        if isinstance(item_schema, dict):
            for index, item in enumerate(value):
                errors.extend(_schema_errors(item, item_schema, root_schema, f"{path}[{index}]"))

    if isinstance(value, str):
        if "minLength" in schema and len(value) < schema["minLength"]:
            errors.append(f"{path}: string is shorter than {schema['minLength']}")
        if "pattern" in schema:
            # Python does not spell POSIX character classes the way JSON Schema
            # does.  This is the sole POSIX class used by the checked-in schema.
            pattern = schema["pattern"].replace("[:space:]", r"\s")
            try:
                matches = re.search(pattern, value) is not None
            except re.error as exc:
                errors.append(f"{path}: invalid published pattern {schema['pattern']!r}: {exc}")
            else:
                if not matches:
                    errors.append(f"{path}: does not match {schema['pattern']!r}")
        if "format" in schema and not _format_ok(value, schema["format"]):
            errors.append(f"{path}: invalid {schema['format']} value {value!r}")

    if isinstance(value, (int, float)) and not isinstance(value, bool):
        for keyword, predicate, phrase in (
            ("minimum", lambda a, b: a >= b, "at least"),
            ("maximum", lambda a, b: a <= b, "at most"),
            ("exclusiveMinimum", lambda a, b: a > b, "greater than"),
            ("exclusiveMaximum", lambda a, b: a < b, "less than"),
        ):
            if keyword in schema and not predicate(value, schema[keyword]):
                errors.append(f"{path}: must be {phrase} {schema[keyword]}")

    return errors


def _service_from_target(target: str) -> str | None:
    if not target.startswith("fbcode//"):
        return None
    remainder = target[len("fbcode//") :]
    return re.split(r"[/:]", remainder, maxsplit=1)[0] or None


def _identity_errors(identity: Any, path: str) -> list[str]:
    if not isinstance(identity, dict):
        return [f"{path}: identity must be an object"]
    errors: list[str] = []
    required = {
        "service",
        "canonical_target",
        "target_key_sha256",
        "target_directory",
        "testinfra_id",
    }
    for key in sorted(required - identity.keys()):
        errors.append(f"{path}: missing identity key {key!r}")
    if errors:
        return errors

    service = identity["service"]
    target = identity["canonical_target"]
    target_directory = identity["target_directory"]
    if service not in ALLOWED_SERVICES:
        errors.append(f"{path}.service: unrecognized service {service!r}")
    derived_service = _service_from_target(target) if isinstance(target, str) else None
    if service != derived_service:
        errors.append(
            f"{path}: service {service!r} does not equal canonical target service {derived_service!r}"
        )
    if isinstance(target, str):
        digest = hashlib.sha256(target.encode()).hexdigest()
        if identity["target_key_sha256"] != digest:
            errors.append(f"{path}.target_key_sha256: does not hash canonical_target")
        directory = PurePosixPath(target_directory)
        if directory.is_absolute() or ".." in directory.parts:
            errors.append(f"{path}.target_directory: must be a safe relative path")
        expected_prefix = ("services", service)
        if directory.parts[:2] != expected_prefix:
            errors.append(
                f"{path}.target_directory: must begin services/{service}/"
            )
        if not directory.name.endswith(f"--{digest[:12]}"):
            errors.append(
                f"{path}.target_directory: suffix must be --{digest[:12]}"
            )
        experiment_id = identity.get("experiment_id")
        expected_id = f"{service}/{directory.name}"
        if experiment_id is not None and experiment_id != expected_id:
            errors.append(f"{path}.experiment_id: must equal {expected_id!r}")
        alias_resolution = identity.get("alias_resolution")
        if isinstance(alias_resolution, dict) and alias_resolution.get("resolved_target") != target:
            errors.append(f"{path}.alias_resolution.resolved_target: must equal canonical_target")
    return errors


def _duplicates(values: Iterable[Any]) -> set[Any]:
    seen: set[Any] = set()
    duplicate: set[Any] = set()
    for value in values:
        if value in seen:
            duplicate.add(value)
        seen.add(value)
    return duplicate



def canonical_object_sha256(value: dict[str, Any], omitted_key: str) -> str:
    """Hash canonical compact JSON after removing one self-referential key."""

    payload = dict(value)
    payload.pop(omitted_key, None)
    encoded = json.dumps(
        payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()

def _unique_ids(items: list[dict[str, Any]], key: str, path: str, errors: list[str]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    for index, item in enumerate(items):
        value = item.get(key)
        if not isinstance(value, str):
            continue
        if value in result:
            errors.append(f"{path}[{index}].{key}: duplicate id {value!r}")
        else:
            result[value] = item
    return result


def _artifact_objects(value: Any) -> Iterable[dict[str, Any]]:
    if isinstance(value, dict):
        if "artifact_id" in value:
            yield value
        for child in value.values():
            yield from _artifact_objects(child)
    elif isinstance(value, list):
        for child in value:
            yield from _artifact_objects(child)


def _parse_timestamp(value: str) -> dt.datetime:
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def _artifact_is_durable(artifact: dict[str, Any]) -> bool:
    return artifact.get("storage") in {"TRACKED", "EXTERNAL_DURABLE"}

def _json_sha256(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _flag_values(argv: list[str], flag: str, takes_value: bool = True) -> list[str | None]:
    values: list[str | None] = []
    index = 0
    while index < len(argv):
        token = argv[index]
        if token == flag:
            if takes_value:
                values.append(argv[index + 1] if index + 1 < len(argv) else None)
                index += 2
                continue
            values.append(None)
        elif token.startswith(flag + "="):
            values.append(token[len(flag) + 1 :])
        index += 1
    return values


def _replace_flag_value(argv: list[str], flag: str, replacement: str) -> list[str]:
    result = list(argv)
    index = 0
    while index < len(result):
        token = result[index]
        if token == flag and index + 1 < len(result):
            result[index + 1] = replacement
            index += 2
            continue
        if token.startswith(flag + "="):
            result[index] = flag + "=" + replacement
        index += 1
    return result


def _selector_errors(selector: dict[str, Any], path: str) -> list[str]:
    expected = {
        "EXACT_FILTER": ("--exact", True),
        "NAMED_TEST": ("--tests", True),
        "WHOLE_TARGET": (None, False),
    }
    kind = selector["kind"]
    flag, has_value = expected[kind]
    errors: list[str] = []
    if selector["flag"] != flag:
        errors.append(f"{path}.flag: must equal {flag!r} for {kind}")
    value = selector["value"]
    if has_value and (not isinstance(value, str) or not value):
        errors.append(f"{path}.value: must be nonempty for {kind}")
    if not has_value and value is not None:
        errors.append(f"{path}.value: must be null for {kind}")
    return errors


def _safehermit_run_boundary(argv: list[str], path: str) -> tuple[list[str], int | None]:
    """Check the wrapper prefix and return the Hermit `run` token index."""

    if not argv or argv[0] != "bin/safehermit":
        return [f"{path}: Hermit execution must route through bin/safehermit"], None
    value_options = {
        "--sh-bin", "--sh-deadline", "--sh-report", "--sh-max-log-bytes",
        "--sh-disk-limit",
    }
    switch_options = {"--sh-fresh", "--sh-no-deadline"}
    index = 1
    while index < len(argv):
        token = argv[index]
        if token in value_options:
            if index + 1 >= len(argv) or argv[index + 1].startswith("--"):
                return [f"{path}: safehermit option {token} lacks a value"], None
            index += 2
            continue
        if any(token.startswith(option + "=") for option in value_options):
            index += 1
            continue
        if token in switch_options:
            index += 1
            continue
        break
    # safehermit also accepts one explicit executable path before the Hermit
    # subcommand. Its actual digest is checked against the retained report.
    if index < len(argv) and "/" in argv[index] and argv[index] != "run":
        index += 1
    if index >= len(argv) or argv[index] != "run":
        return [f"{path}: safehermit must invoke the Hermit run subcommand"], None
    return [], index


def _argv_contract_errors(
    argv: list[str], design: dict[str, Any], resource_bounds: dict[str, Any],
    repeat_matcher: dict[str, Any], path: str, *,
    template: bool, require_safehermit: bool,
) -> list[str]:
    errors: list[str] = []
    if argv.count("--") != 1:
        errors.append(f"{path}: must contain exactly one Hermit/guest delimiter")
        option_argv = argv
    else:
        option_argv = argv[:argv.index("--")]
    wrapper_argv: list[str] = []
    hermit_argv = option_argv
    if require_safehermit:
        wrapper_errors, run_index = _safehermit_run_boundary(option_argv, path)
        errors.extend(wrapper_errors)
        if run_index is not None:
            wrapper_argv = option_argv[:run_index]
            hermit_argv = option_argv[run_index + 1:]
    for flag in ("--strict", "--chaos"):
        if _flag_values(hermit_argv, flag, False) != [None]:
            errors.append(f"{path}: requires exactly one explicit {flag}")
    if _flag_values(hermit_argv, "--backend") != ["ptrace"]:
        errors.append(f"{path}: requires exactly one explicit ptrace backend")
    heuristic_values = _flag_values(hermit_argv, "--sched-heuristic")
    allowed_heuristics = {"none", "roundrobin", "connectbind", "random", "stickyrandom"}
    if len(heuristic_values) != 1 or heuristic_values[0] not in allowed_heuristics:
        errors.append(f"{path}: requires exactly one supported --sched-heuristic")
    timeslice_values = _flag_values(hermit_argv, "--max-timeslice")
    try:
        requested_timeslice = int(timeslice_values[0]) if len(timeslice_values) == 1 else -1
    except (TypeError, ValueError):
        requested_timeslice = -1
    if requested_timeslice < 0 or requested_timeslice > U64_MAX:
        errors.append(f"{path}: requires exactly one unsigned 64-bit --max-timeslice")
    if repeat_matcher["kind"] == "BUILTIN_VERIFY":
        for flag in ("--verify", "--verify-strict"):
            if _flag_values(hermit_argv, flag, False) != [None]:
                errors.append(f"{path}: requires exactly one explicit {flag}")
        if _flag_values(hermit_argv, "--verify-allow") != ["both"]:
            errors.append(f"{path}: requires exactly one --verify-allow=both")
    else:
        verify_flags = {
            flag for flag in ("--verify", "--verify-strict", "--verify-allow")
            if _flag_values(hermit_argv, flag, flag == "--verify-allow")
        }
        if verify_flags:
            errors.append(
                f"{path}: EQUIVALENT matcher route cannot enable built-in verify {sorted(verify_flags)!r}"
            )
    for flag, expected in (
        ("--sh-deadline", str(resource_bounds["wall_seconds"])),
        ("--sh-max-log-bytes", str(resource_bounds["stderr_bytes"])),
        ("--sh-disk-limit", resource_bounds["disk_guard"]),
    ):
        if _flag_values(wrapper_argv, flag) != [expected]:
            errors.append(f"{path}: {flag} must occur exactly once with value {expected!r}")
    forbidden = {
        "--no-strict", "--no-sequentialize-threads", "--no-deterministic-io",
        "--allow-unsupported-syscalls", "--strace-only", "--namespace-only", "--lite",
        "--no-namespace", "--core-only", "--gdbserver", "--no-virtualize-time",
        "--no-virtualize-cpuid", "--no-virtualize-metadata", "--passthru-opt",
        "--imprecise-timers", "--debug-externalize-sockets",
        "--recordreplay-modes", "--no-detlog-io-buffers", "--seed-from",
        "--replay-preemptions-from", "--replay-schedule-from",
        "--stop-after-turn", "--stop-after-iter",
    }
    present = sorted(flag for flag in forbidden if any(
        token == flag or token.startswith(flag + "=") for token in hermit_argv
    ))
    for flag, disallowed_values in {
        "--network": {"host"},
        "--net": {"host"},
        "--debug-futex-mode": {"external", "polling"},
    }.items():
        if any(value in disallowed_values for value in _flag_values(hermit_argv, flag)):
            present.append(flag)
    if _flag_values(wrapper_argv, "--sh-no-deadline", False):
        present.append("--sh-no-deadline")
    if present:
        errors.append(f"{path}: contains disallowed relaxation(s) {sorted(set(present))!r}")
    swept = design["swept_coordinate"]["flag"]
    fixed = design["fixed_seed_roots"]
    for flag in ("--seed", "--rng-seed", "--sched-seed", "--fuzz-seed"):
        values = _flag_values(hermit_argv, flag)
        expected = "{SWEEP}" if flag == swept and template else (
            None if flag == swept else str(fixed[flag])
        )
        if flag == swept and not template:
            if len(values) != 1 or values[0] is None:
                errors.append(f"{path}: requires exactly one concrete {flag}")
        elif values != [expected]:
            errors.append(f"{path}: {flag} must occur exactly once with value {expected!r}")
    declared_modes = set(design["swept_coordinate"]["scheduler_modes"])
    all_modes = {"--fuzz-futexes", "--chaos-target-races"}
    actual_modes = {mode for mode in all_modes if _flag_values(hermit_argv, mode, False) == [None]}
    for mode in all_modes:
        if len(_flag_values(hermit_argv, mode, False)) > 1:
            errors.append(f"{path}: scheduler mode {mode} occurs more than once")
    if actual_modes != declared_modes:
        errors.append(f"{path}: scheduler modes {sorted(actual_modes)!r} do not equal declared {sorted(declared_modes)!r}")
    report_values = _flag_values(wrapper_argv, "--sh-report")
    if template and report_values != ["{SAFEHERMIT_REPORT}"]:
        errors.append(f"{path}: template requires --sh-report={{SAFEHERMIT_REPORT}} exactly once")
    if not template and (len(report_values) != 1 or not report_values[0]):
        errors.append(f"{path}: execution requires exactly one concrete --sh-report path")
    return errors


def _qualifying_result(result: dict[str, Any], timeout_policy: dict[str, Any]) -> bool:
    verdict = result["target_oracle"]
    if not result["guest_started"] or result["selected_test_executed_count"] < 1:
        return False
    if verdict == "PASS":
        return result["harness_status"] == "COMPLETE" and not result["timed_out"] and result["timeout_origin"] == "NONE" and result["exit_code"] == 0 and result["signal"] is None
    if verdict == "FAIL":
        if result["timed_out"]:
            return timeout_policy["target_timeout_can_fail"] and result["harness_status"] == "TIMEOUT" and result["timeout_origin"] == "TARGET" and result["failure_class_id"] == timeout_policy["target_timeout_failure_class_id"]
        return result["harness_status"] == "COMPLETE" and (result["exit_code"] not in (None, 0) or result["signal"] is not None)
    return False


def _artifact_is_working_evidence(artifact: dict[str, Any]) -> bool:
    return artifact.get("storage") == "TRACKED" and not artifact.get("truncated", False)


def _event_result_payload(event: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "event_id": event["event_id"],
        "kind": event["kind"],
        "route_id": event["route_id"],
        "cell_id": event["cell_id"],
        "measurement_role": event["measurement_role"],
        "invocation_id": event["invocation_id"],
        "replica_index": event["replica_index"],
        "seed": event["seed"],
        "argv": event["argv"],
        "result": event["result"],
    }


def _matcher_info_payload(matcher: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "matcher_id": matcher["matcher_id"],
        "kind": matcher["kind"],
        "execution_event_ids": matcher["execution_event_ids"],
        "complete": matcher["complete"],
        "same_target_outcome": matcher["same_target_outcome"],
        "same_failure_class": matcher["same_failure_class"],
        "details": matcher["details"],
    }


def _size_bytes(value: str) -> int | None:
    match = re.fullmatch(r"([0-9]+)([KMGT]?)", value)
    if match is None:
        return None
    multiplier = {"": 1, "K": 1 << 10, "M": 1 << 20, "G": 1 << 30, "T": 1 << 40}
    return int(match.group(1)) * multiplier[match.group(2)]


def _safehermit_report_errors(
    payload: bytes, cell: dict[str, Any], result: dict[str, Any],
    resource_bounds: dict[str, Any], path: str,
) -> list[str]:
    errors: list[str] = []
    try:
        lines = payload.decode("utf-8").splitlines()
    except UnicodeDecodeError:
        return [f"{path}: safehermit report is not UTF-8"]
    fields: dict[str, str] = {}
    duplicates: set[str] = set()
    for line in lines:
        if not line.startswith("safehermit: ") or "=" not in line:
            continue
        key, value = line[len("safehermit: "):].split("=", 1)
        if key in fields:
            duplicates.add(key)
        else:
            fields[key] = value
    if duplicates:
        errors.append(f"{path}: safehermit report duplicates fields {sorted(duplicates)!r}")
    required = {
        "run_id", "binary", "binary_source", "binary_sha256", "log_dir",
        "log_file", "unit", "bound.wall", "bound.cgroup", "bound.disk",
        "bound.bytes", "bound.logfilter", "env.forwarded", "note.run_record",
        "elapsed_secs", "bytes_written", "truncated", "runner_exit_code",
        "unit_result", "exit_code",
    }
    missing = sorted(required - fields.keys())
    if missing:
        errors.append(f"{path}: safehermit report missing required fields {missing!r}")
        return errors
    for key in ("run_id", "binary", "binary_source", "log_dir", "log_file"):
        if not fields[key]:
            errors.append(f"{path}: safehermit report field {key} is empty")
    if fields["unit"] == "none" or not fields["unit"]:
        errors.append(f"{path}: safehermit report has no bounded systemd unit")
    if fields["binary_sha256"] != cell["hermit_binary_sha256"]:
        errors.append(f"{path}: safehermit report does not bind hermit_binary_sha256")
    expected_wall = f"APPLIED:{resource_bounds['wall_seconds']}s"
    if fields["bound.wall"] != expected_wall:
        errors.append(f"{path}: safehermit wall bound differs from sealed resource bounds")
    cgroup = re.fullmatch(
        r"APPLIED:MemoryMax=([^ ]+) MemorySwapMax=([^ ]+)", fields["bound.cgroup"]
    )
    if (
        cgroup is None
        or _size_bytes(cgroup.group(1)) != resource_bounds["memory_bytes"]
        or _size_bytes(cgroup.group(2)) != resource_bounds["swap_bytes"]
    ):
        errors.append(f"{path}: safehermit cgroup bound differs from sealed resource bounds")
    expected_disk = f"APPLIED:{resource_bounds['disk_guard']} for log_dir only;"
    if not fields["bound.disk"].startswith(expected_disk):
        errors.append(f"{path}: safehermit disk bound differs from sealed resource bounds")
    expected_bytes = f"APPLIED:{resource_bounds['stderr_bytes']} ("
    if not fields["bound.bytes"].startswith(expected_bytes):
        errors.append(f"{path}: safehermit byte bound differs from sealed resource bounds")
    if fields["truncated"] != "false":
        errors.append(f"{path}: safehermit report is truncated or has unknown truncation state")
    numeric: dict[str, int] = {}
    for key in ("elapsed_secs", "bytes_written", "runner_exit_code", "exit_code"):
        try:
            numeric[key] = int(fields[key])
        except ValueError:
            errors.append(f"{path}: safehermit report field {key} is not an integer")
    for key in ("elapsed_secs", "bytes_written", "runner_exit_code", "exit_code"):
        if key in numeric and numeric[key] < 0:
            errors.append(f"{path}: safehermit report field {key} is negative")
    if numeric.get("bytes_written", resource_bounds["stderr_bytes"] + 1) > resource_bounds["stderr_bytes"]:
        errors.append(f"{path}: safehermit bytes_written exceeds sealed stderr bound")
    signal = result["signal"]
    expected_exit = 128 + signal if signal is not None else result["exit_code"]
    if result["timed_out"]:
        expected_unit_results = {"timeout"}
    elif signal is not None:
        expected_unit_results = {"signal", "core-dump"}
    elif expected_exit == 0:
        expected_unit_results = {"success"}
    else:
        expected_unit_results = {"exit-code"}
    if fields["unit_result"] not in expected_unit_results:
        errors.append(f"{path}: safehermit unit_result disagrees with event result")
    if expected_exit is None or numeric.get("exit_code") != expected_exit:
        errors.append(f"{path}: safehermit exit_code disagrees with event result")
    if signal is None and not result["timed_out"] and numeric.get("runner_exit_code") != expected_exit:
        errors.append(f"{path}: safehermit runner_exit_code disagrees with event result")
    if signal is not None:
        try:
            process_signal = int(fields.get("process_signal", ""))
        except ValueError:
            process_signal = None
        if process_signal != signal:
            errors.append(f"{path}: safehermit process_signal disagrees with event result")
    return errors


def _git_blob_hashes(
    root: Path, locator: str
) -> tuple[str | None, str | None, str | None]:
    """Return worktree, index, and HEAD blob IDs for one repository path."""

    tracked = subprocess.run(
        ["git", "-C", str(root), "ls-files", "--stage", "--full-name", "--", locator],
        capture_output=True, text=True, check=False,
    )
    stage_rows = [line.split(maxsplit=3) for line in tracked.stdout.splitlines()]
    stage_rows = [row for row in stage_rows if len(row) == 4 and row[2] == "0"]
    if tracked.returncode != 0 or len(stage_rows) != 1:
        return None, None, None
    stage_hash = stage_rows[0][1]
    repository_path = stage_rows[0][3]
    worktree = subprocess.run(
        ["git", "-C", str(root), "hash-object", "--", locator],
        capture_output=True, text=True, check=False,
    )
    head = subprocess.run(
        ["git", "-C", str(root), "ls-tree", "--full-tree", "HEAD", "--", repository_path],
        capture_output=True, text=True, check=False,
    )
    head_rows = [line.split(maxsplit=3) for line in head.stdout.splitlines()]
    head_rows = [row for row in head_rows if len(row) == 4 and row[1] == "blob"]
    worktree_hash = worktree.stdout.strip() if worktree.returncode == 0 else None
    head_hash = head_rows[0][2] if head.returncode == 0 and len(head_rows) == 1 else None
    return worktree_hash, stage_hash, head_hash


def _committed_path_errors(root: Path, locator: str, label: str) -> list[str]:
    worktree_hash, stage_hash, head_hash = _git_blob_hashes(root, locator)
    if stage_hash is None:
        return [f"{label} is not in the Git index"]
    if head_hash is None:
        return [f"{label} is not committed at HEAD"]
    if stage_hash != head_hash:
        return [f"{label} Git index differs from committed HEAD"]
    if worktree_hash != stage_hash:
        return [f"{label} differs from the Git index"]
    return []


def _validate_plan_receipt(
    record: dict[str, Any], path: str, root: Path, errors: list[str]
) -> None:
    receipt = record["plan_receipt"]
    if record["attempt_state"] == "PLANNED":
        if receipt is not None:
            errors.append(f"{path}: PLANNED record must have null plan_receipt")
        return
    if not isinstance(receipt, dict):
        errors.append(f"{path}: MATERIAL record requires a plan_receipt")
        return
    if receipt["path"] != path:
        errors.append(f"{path}: plan_receipt path must equal the material record path")
        return
    prefix = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--show-prefix"],
        capture_output=True, text=True, check=False,
    )
    repository_path = prefix.stdout.strip() + path if prefix.returncode == 0 else path
    commit = receipt["commit"]
    commit_exists = subprocess.run(
        ["git", "-C", str(root), "cat-file", "-e", f"{commit}^{{commit}}"],
        capture_output=True, check=False,
    )
    if commit_exists.returncode != 0:
        errors.append(f"{path}: plan_receipt commit is unavailable")
        return
    head = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        capture_output=True, text=True, check=False,
    ).stdout.strip()
    ancestor = subprocess.run(
        ["git", "-C", str(root), "merge-base", "--is-ancestor", commit, "HEAD"],
        capture_output=True, check=False,
    )
    if commit == head or ancestor.returncode != 0:
        errors.append(f"{path}: plan_receipt commit must be a strict ancestor of HEAD")
        return
    blob = subprocess.run(
        ["git", "-C", str(root), "rev-parse", f"{commit}:{repository_path}"],
        capture_output=True, text=True, check=False,
    )
    if blob.returncode != 0 or blob.stdout.strip() != receipt["blob"]:
        errors.append(f"{path}: plan_receipt blob does not match the sealed commit path")
        return
    prior = subprocess.run(
        ["git", "-C", str(root), "show", f"{commit}:{repository_path}"],
        capture_output=True, text=True, check=False,
    )
    try:
        snapshot = json.loads(prior.stdout) if prior.returncode == 0 else None
    except json.JSONDecodeError:
        snapshot = None
    expected = copy.deepcopy(record)
    expected.update(
        plan_receipt=None, attempt_state="PLANNED", cells=[], events=[],
        samples=[], matchers=[], outcome_history=[], current_outcome=None,
    )
    if snapshot != expected:
        errors.append(f"{path}: plan_receipt does not contain the exact pre-run PLANNED record")
        return
    for route in snapshot["plan"]["authorized_routes"]:
        matcher_plan = route["repeat_matcher"]
        if matcher_plan["kind"] != "EQUIVALENT":
            continue
        artifact = matcher_plan["comparator_artifact"]
        locator = artifact["locator"]
        repository_locator = prefix.stdout.strip() + locator
        prior_comparator = subprocess.run(
            ["git", "-C", str(root), "show", f"{commit}:{repository_locator}"],
            capture_output=True, check=False,
        )
        if prior_comparator.returncode != 0:
            errors.append(f"{path}: sealed comparator was absent from the plan commit")
            continue
        content = prior_comparator.stdout
        if (
            len(content) != artifact["bytes"]
            or hashlib.sha256(content).hexdigest() != artifact["sha256"]
        ):
            errors.append(f"{path}: sealed comparator bytes differ at the plan commit")


def _validate_identity_plan(
    record: dict[str, Any], path: str, campaign_owner: str, errors: list[str]
) -> dict[str, Any]:
    identity = record["identity"]
    errors.extend(_identity_errors(identity, f"{path}.identity"))
    errors.extend(_selector_errors(identity["test_selector"], f"{path}.identity.test_selector"))
    ranking = record["candidate_ranking"]
    native = ranking["native_failure_rate"]
    failures, executed, rate = native["failures"], native["executed"], native["rate"]
    if native["pass_statuses"] != ["PASSED"] or set(native["failure_statuses"]) != {"FAILED", "FATAL", "TIMEOUT"}:
        errors.append(f"{path}.candidate_ranking.native_failure_rate: status populations are not canonical")
    if failures > executed:
        errors.append(f"{path}.candidate_ranking.native_failure_rate: failures exceed executed")
    elif not math.isclose(rate, failures / executed, rel_tol=0, abs_tol=1e-15):
        errors.append(f"{path}.candidate_ranking.native_failure_rate.rate: must equal failures/executed")
    if native["rate_fraction"] != f"{failures}/{executed}":
        errors.append(f"{path}.candidate_ranking.native_failure_rate.rate_fraction: must equal failures/executed")
    if native["displayed_percent"] != f"{100 * rate:.2f}%":
        errors.append(f"{path}.candidate_ranking.native_failure_rate.displayed_percent: does not match rate")
    pfs = ranking["pfs"]
    if pfs["displayed_percent"] != f"{100 * pfs["value"]:.2f}%":
        errors.append(f"{path}.candidate_ranking.pfs.displayed_percent: does not match value")

    plan = record["plan"]
    if plan["campaign_owner_identity"] != campaign_owner:
        errors.append(f"{path}.plan: campaign_owner_identity differs from campaign metadata")
    plan_sha = plan["plan_sha256"]
    expected_plan_sha = canonical_object_sha256(plan, "plan_sha256")
    if plan_sha != expected_plan_sha:
        errors.append(f"{path}.plan.plan_sha256: expected canonical digest {expected_plan_sha}")
    routes = _unique_ids(plan["authorized_routes"], "route_id", f"{path}.plan.authorized_routes", errors)
    authorized_cell_to_route: dict[str, str] = {}
    for route_id, route in routes.items():
        expected_route_sha = canonical_object_sha256(route, "route_sha256")
        if route["route_sha256"] != expected_route_sha:
            errors.append(f"{path}: route {route_id!r} has wrong route_sha256; expected {expected_route_sha}")
        fallback = route["fallback_of"]
        if fallback is not None and fallback not in routes:
            errors.append(f"{path}: route {route_id!r} has unknown fallback_of {fallback!r}")
        if fallback == route_id:
            errors.append(f"{path}: route {route_id!r} cannot fall back to itself")
        for key in ("canonical_target", "test_selector", "build_mode_selector"):
            if route[key] != identity[key]:
                errors.append(f"{path}: route {route_id!r} {key} disagrees with identity")
        if route["source_revision"] != identity["alias_resolution"]["source_revision"]:
            errors.append(f"{path}: route {route_id!r} source_revision disagrees with identity")
        if route["build_argv"] != identity["standalone_argv"]:
            errors.append(f"{path}: route {route_id!r} build_argv is not the pinned standalone invocation")
        for cell_id in route["authorized_cell_ids"]:
            prior = authorized_cell_to_route.setdefault(cell_id, route_id)
            if prior != route_id:
                errors.append(f"{path}: authorized cell {cell_id!r} appears in more than one route")

    oracle = plan["target_oracle"]
    failure_classes = _unique_ids(oracle["fail_classes"], "failure_class_id", f"{path}.plan.target_oracle.fail_classes", errors)
    timeout_policy = oracle["timeout_policy"]
    if timeout_policy["target_timeout_can_fail"]:
        if not timeout_policy["target_timeout_predicate"] or timeout_policy["target_timeout_failure_class_id"] not in failure_classes:
            errors.append(f"{path}.plan.target_oracle.timeout_policy: qualifying target timeout requires predicate and declared timeout failure class")
    elif timeout_policy["target_timeout_predicate"] is not None or timeout_policy["target_timeout_failure_class_id"] is not None:
        errors.append(f"{path}.plan.target_oracle.timeout_policy: disabled target timeout requires null predicate and class")
    design = plan["measurement_design"]
    required_n = design["required_valid_seed_tuples"]
    attempted_cap = design["max_attempted_seed_tuples"]
    population = design["seed_population"]["values"]
    order = design["selection_order"]["seed_values"]
    if set(order) != set(population):
        errors.append(f"{path}.plan.measurement_design: selection order must contain exactly the seed population")
    if attempted_cap > len(order):
        errors.append(f"{path}.plan.measurement_design: attempted cap exceeds explicit seed order")
    if attempted_cap < required_n:
        errors.append(f"{path}.plan.measurement_design: max_attempted_seed_tuples must be >= required_valid_seed_tuples")
    swept_flag = design["swept_coordinate"]["flag"]
    fixed = design["fixed_seed_roots"]
    if fixed["--rng-seed"] is None:
        errors.append(f"{path}.plan.measurement_design: --rng-seed must always be pinned")
    if fixed[swept_flag] is not None:
        errors.append(f"{path}.plan.measurement_design: swept seed must be null in fixed roots")
    for flag, value in fixed.items():
        if flag != swept_flag and value is None:
            errors.append(f"{path}.plan.measurement_design: non-swept seed {flag} must be pinned")
    if swept_flag not in {"--sched-seed", "--fuzz-seed"}:
        errors.append(f"{path}.plan.measurement_design: {swept_flag!r} is not schedule-affecting")
    if swept_flag == "--fuzz-seed" and not design["swept_coordinate"]["scheduler_modes"]:
        errors.append(f"{path}.plan.measurement_design: fuzz-seed requires a scheduler mode")
    all_seed_values = list(population) + list(order) + [
        value for value in fixed.values() if value is not None
    ]
    if any(value < 0 or value > U64_MAX for value in all_seed_values):
        errors.append(f"{path}.plan.measurement_design: every seed must fit an unsigned 64-bit integer")
    for route_id, route in routes.items():
        matcher_plan = route["repeat_matcher"]
        if matcher_plan["kind"] == "EQUIVALENT":
            comparator = matcher_plan["comparator_artifact"]
            if (
                comparator["kind"] != "OTHER" or comparator["storage"] != "TRACKED"
                or comparator["truncated"]
            ):
                errors.append(f"{path}.plan.authorized_routes[{route_id}]: comparator artifact must be complete TRACKED OTHER evidence")
        errors.extend(_argv_contract_errors(
            route["launch_argv"], design, plan["resource_bounds"],
            route["repeat_matcher"],
            f"{path}.plan.authorized_routes[{route_id}].launch_argv",
            template=True, require_safehermit=True,
        ))

    basis = design["design_basis"]
    override = design["miss_probability_override"]
    if basis["kind"] == "RATE_BOUND":
        if re.search(r"\bPFS\b|probability of flaky state", basis["provenance"], re.IGNORECASE):
            errors.append(f"{path}.plan.measurement_design.design_basis: PFS cannot parameterize p")
        if basis["provenance_kind"] == "NATIVE_EMPIRICAL_RATE":
            if basis["equivalence_to_hermit"]:
                errors.append(f"{path}.plan.measurement_design.design_basis: native rate is only a planning prior")
            if not math.isclose(basis["expected_hermit_fail_probability"], rate, rel_tol=0, abs_tol=1e-15):
                errors.append(f"{path}.plan.measurement_design.design_basis: native-rate prior must equal registry native rate")
        allowed_miss = 0.05 if override is None else override["max_miss_probability"]
        if not math.isclose(basis["max_miss_probability"], allowed_miss, rel_tol=0, abs_tol=1e-15):
            errors.append(f"{path}.plan.measurement_design: miss probability differs from default/owner override")
        if override is not None:
            if override["owner_identity"] != plan["campaign_owner_identity"]:
                errors.append(f"{path}.plan.measurement_design: miss override is not from campaign owner")
            if _parse_timestamp(override["decided_at"]) > _parse_timestamp(plan["sealed_at"]):
                errors.append(f"{path}.plan.measurement_design: miss override postdates plan sealing")
        p = basis["expected_hermit_fail_probability"]
        minimal_n = next((n for n in range(2, len(order) + 1) if p ** n + (1 - p) ** n <= allowed_miss), None)
        if minimal_n is None:
            errors.append(f"{path}.plan.measurement_design: explicit seed population cannot meet miss bound")
        elif required_n != minimal_n:
            errors.append(f"{path}.plan.measurement_design: required N must be derived smallest N={minimal_n}")
    else:
        if override is not None:
            errors.append(f"{path}.plan.measurement_design: finite population cannot carry a miss override")
        if required_n != basis["population_size"] or len(population) != basis["population_size"]:
            errors.append(f"{path}.plan.measurement_design: finite population N must equal explicit population size")
    if design["purpose"] == "RATE_ESTIMATION" and design["stop_rule"] != "COMPLETE_N_FOR_RATE":
        errors.append(f"{path}.plan.measurement_design: rate estimation must complete N")
    return {"identity": identity, "plan": plan, "plan_sha": plan_sha, "routes": routes, "authorized_cells": authorized_cell_to_route, "oracle": oracle, "failure_classes": failure_classes, "design": design, "required_n": required_n, "attempted_cap": attempted_cap, "population": population, "order": order, "swept_flag": swept_flag}


def _validate_artifacts(
    record: dict[str, Any], path: str, root: Path, errors: list[str]
) -> tuple[dict[str, dict[str, Any]], dict[str, str], dict[str, bytes]]:
    by_id: dict[str, dict[str, Any]] = {}
    by_locator: dict[str, str] = {}
    tracked_payloads: dict[str, bytes] = {}
    for artifact in _artifact_objects(record):
        artifact_id = artifact["artifact_id"]
        locator = artifact["locator"]
        if artifact_id in by_id:
            errors.append(f"{path}: duplicate artifact_id {artifact_id!r}")
        else:
            by_id[artifact_id] = artifact
        if locator in by_locator:
            errors.append(f"{path}: duplicate artifact locator {locator!r}")
        else:
            by_locator[locator] = artifact_id
        if artifact["bytes"] > artifact["capture_limit_bytes"]:
            errors.append(f"{path}: artifact {artifact_id!r} exceeds its capture bound")
        if artifact["storage"] == "TRACKED":
            posix = PurePosixPath(locator)
            resolved = (root / locator).resolve()
            try:
                resolved.relative_to(root.resolve())
            except ValueError:
                errors.append(f"{path}: tracked artifact {artifact_id!r} escapes campaign root")
                continue
            candidate = root / locator
            has_symlink = any(
                part.is_symlink()
                for part in [candidate, *candidate.parents]
                if part != root.parent and part.exists()
            )
            if posix.is_absolute() or ".." in posix.parts or "\\" in locator or has_symlink:
                errors.append(f"{path}: tracked artifact {artifact_id!r} has unsafe locator or symlink")
                continue
            if not candidate.is_file():
                errors.append(f"{path}: tracked artifact {artifact_id!r} does not exist as a regular file")
                continue
            content = resolved.read_bytes()
            if len(content) != artifact["bytes"]:
                errors.append(f"{path}: tracked artifact {artifact_id!r} byte count disagrees")
            if hashlib.sha256(content).hexdigest() != artifact["sha256"]:
                errors.append(f"{path}: tracked artifact {artifact_id!r} digest disagrees")
            committed_errors = _committed_path_errors(
                root, locator, f"{path}: tracked artifact {artifact_id!r}"
            )
            errors.extend(committed_errors)
            if not committed_errors and len(content) == artifact["bytes"] and hashlib.sha256(content).hexdigest() == artifact["sha256"]:
                tracked_payloads[artifact_id] = content
        elif artifact["storage"] == "EXTERNAL_DURABLE":
            scheme = locator.split(":", 1)[0].lower() if ":" in locator else ""
            if scheme not in {"https", "s3", "gs", "cas", "oci"}:
                errors.append(f"{path}: external artifact {artifact_id!r} locator lacks an allowed durable URI scheme")
    return by_id, by_locator, tracked_payloads


def _durable_reference(
    ref: str,
    events: dict[str, dict[str, Any]],
    samples: dict[str, dict[str, Any]],
    matchers: dict[str, dict[str, Any]],
    artifacts: dict[str, dict[str, Any]],
    *,
    working: bool = False,
) -> bool:
    pred = _artifact_is_working_evidence if working else _artifact_is_durable
    if ref in artifacts:
        return pred(artifacts[ref])
    if ref in events:
        return any(pred(item) for item in events[ref]["artifacts"])
    if ref in samples:
        return any(
            event_id in events and any(pred(item) for item in events[event_id]["artifacts"])
            for event_id in samples[ref]["execution_event_ids"]
        )
    if ref in matchers:
        return pred(matchers[ref]["canonical_info_artifact"])
    return False


def _validate_cells_events(
    record: dict[str, Any], path: str, ctx: dict[str, Any],
    artifacts: dict[str, dict[str, Any]], tracked_payloads: dict[str, bytes],
    errors: list[str],
) -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]], dict[tuple[str, str, int], set[str]]]:
    plan, routes, design = ctx["plan"], ctx["routes"], ctx["design"]
    cells = _unique_ids(record["cells"], "cell_id", f"{path}.cells", errors)
    events = _unique_ids(record["events"], "event_id", f"{path}.events", errors)
    for cell_id, cell in cells.items():
        expected_route = ctx["authorized_cells"].get(cell_id)
        if expected_route is None:
            errors.append(f"{path}: cell {cell_id!r} was not preauthorized by a route")
        elif expected_route != cell["route_id"]:
            errors.append(f"{path}: cell {cell_id!r} is attached to the wrong route")
        route = routes.get(cell["route_id"])
        if cell["plan_sha256"] != ctx["plan_sha"]:
            errors.append(f"{path}: cell {cell_id!r} does not bind the sealed plan")
        if canonical_object_sha256(cell, "cell_sha256") != cell["cell_sha256"]:
            errors.append(f"{path}: cell {cell_id!r} has wrong canonical cell_sha256")
        if route is not None:
            if cell["route_sha256"] != route["route_sha256"]:
                errors.append(f"{path}: cell {cell_id!r} does not bind route_sha256")
            for key in ("canonical_target", "test_selector", "build_mode_selector"):
                if cell[key] != route[key]:
                    errors.append(f"{path}: cell {cell_id!r} {key} disagrees with route")
            if cell["target_source_revision"] != route["source_revision"]:
                errors.append(f"{path}: cell {cell_id!r} source revision disagrees with route")
            if cell["hermit"]["argv"] != route["launch_argv"]:
                errors.append(f"{path}: cell {cell_id!r} argv disagrees with sealed route")
            if cell["guest"]["environment"] != route["environment"]:
                errors.append(f"{path}: cell {cell_id!r} guest environment disagrees with sealed route")
        if cell["oracle_id"] != ctx["oracle"]["oracle_id"]:
            errors.append(f"{path}: cell {cell_id!r} references wrong oracle")
        if cell["hermit"]["swept_coordinate"] != ctx["swept_flag"]:
            errors.append(f"{path}: cell {cell_id!r} changes swept coordinate")
        if cell["hermit"]["fixed_seed_roots"] != design["fixed_seed_roots"]:
            errors.append(f"{path}: cell {cell_id!r} changes fixed seed roots")
        if cell["resource_bounds"] != plan["resource_bounds"]:
            errors.append(f"{path}: cell {cell_id!r} changes sealed resource bounds")
        errors.extend(_argv_contract_errors(
            cell["hermit"]["argv"], design, plan["resource_bounds"],
            route["repeat_matcher"] if route is not None else {"kind": "BUILTIN_VERIFY"},
            f"{path}.cells[{cell_id}].hermit.argv",
            template=True, require_safehermit=True,
        ))
        option_argv = cell["hermit"]["argv"][:cell["hermit"]["argv"].index("--")] if "--" in cell["hermit"]["argv"] else cell["hermit"]["argv"]
        if _flag_values(option_argv, "--sched-heuristic") != [cell["hermit"]["scheduler_heuristic"]]:
            errors.append(f"{path}: cell {cell_id!r} scheduler_heuristic disagrees with argv")
        if _flag_values(option_argv, "--max-timeslice") != [str(cell["timing"]["requested_max_timeslice"])]:
            errors.append(f"{path}: cell {cell_id!r} max_timeslice disagrees with argv")
        if cell["timing"]["effective_max_timeslice"] != "disabled" and not cell["pmu"]["available"]:
            errors.append(f"{path}: cell {cell_id!r} enables PMU timeslice without PMU availability")
        if cell["pmu"]["fallback_recorded"] and not (cell["timing"]["requested_max_timeslice"] != cell["timing"]["effective_max_timeslice"] and cell["timing"]["effective_max_timeslice"] == "disabled"):
            errors.append(f"{path}: cell {cell_id!r} records PMU fallback without disabled timeslice")
        if not cell["pmu"]["fallback_recorded"] and cell["timing"]["requested_max_timeslice"] != cell["timing"]["effective_max_timeslice"]:
            errors.append(f"{path}: cell {cell_id!r} changes timeslice without recorded fallback")
        if cell["hermit"]["argv"].count("--") == 1:
            guest_suffix = cell["hermit"]["argv"][cell["hermit"]["argv"].index("--") + 1:]
            if guest_suffix != cell["guest"]["argv"]:
                errors.append(f"{path}: cell {cell_id!r} Hermit argv guest suffix disagrees with guest.argv")

    seen_invocation_replicas: set[tuple[str, int]] = set()
    tuple_events: dict[tuple[str, str, int], set[str]] = {}
    for event_id, event in events.items():
        if event["plan_sha256"] != ctx["plan_sha"]:
            errors.append(f"{path}: event {event_id!r} does not bind sealed plan")
        route = routes.get(event["route_id"])
        if route is None:
            errors.append(f"{path}: event {event_id!r} references unknown route")
        else:
            if event["route_sha256"] != route["route_sha256"]:
                errors.append(f"{path}: event {event_id!r} does not bind route_sha256")
            for key in ("canonical_target", "test_selector", "build_mode_selector"):
                if event[key] != route[key]:
                    errors.append(f"{path}: event {event_id!r} {key} disagrees with route")
            if event["source_revision"] != route["source_revision"]:
                errors.append(f"{path}: event {event_id!r} source revision disagrees with route")
        cell_id = event["cell_id"]
        if cell_id is None:
            if event["cell_sha256"] is not None:
                errors.append(f"{path}: event {event_id!r} has cell digest without cell")
        elif cell_id not in cells:
            errors.append(f"{path}: event {event_id!r} references unknown cell")
        else:
            cell = cells[cell_id]
            if cell["route_id"] != event["route_id"]:
                errors.append(f"{path}: event {event_id!r} route disagrees with cell")
            if event["cell_sha256"] != cell["cell_sha256"]:
                errors.append(f"{path}: event {event_id!r} does not bind cell_sha256")
            if _parse_timestamp(event["started_at"]) < _parse_timestamp(cell["frozen_at"]):
                errors.append(f"{path}: event {event_id!r} predates frozen cell")
        started, finished = _parse_timestamp(event["started_at"]), _parse_timestamp(event["finished_at"])
        if started < _parse_timestamp(plan["sealed_at"]):
            errors.append(f"{path}: event {event_id!r} predates plan sealing")
        if finished < started:
            errors.append(f"{path}: event {event_id!r} finishes before start")

        result = event["result"]
        result_artifacts = [item for item in event["artifacts"] if item["kind"] == "RESULT"]
        if len(result_artifacts) != 1:
            errors.append(f"{path}: event {event_id!r} requires exactly one RESULT artifact")
        else:
            receipt = result_artifacts[0]
            payload = tracked_payloads.get(receipt["artifact_id"])
            if receipt["storage"] != "TRACKED" or receipt["truncated"] or payload is None:
                errors.append(f"{path}: event {event_id!r} RESULT must be complete committed evidence")
            else:
                try:
                    observed_result = json.loads(payload)
                except (UnicodeDecodeError, json.JSONDecodeError):
                    observed_result = None
                if observed_result != _event_result_payload(event):
                    errors.append(f"{path}: event {event_id!r} RESULT payload disagrees with event")
        verdict, failure_class = result["target_oracle"], result["failure_class_id"]
        if result["timed_out"] != (result["harness_status"] == "TIMEOUT") or (not result["timed_out"] and result["timeout_origin"] != "NONE"):
            errors.append(f"{path}: event {event_id!r} timeout and harness status disagree")
        if not result["guest_started"] and result["selected_test_executed_count"] != 0:
            errors.append(f"{path}: event {event_id!r} claims tests without guest start")
        if result["selected_test_executed_count"] == 0 and verdict in {"PASS", "FAIL"}:
            errors.append(f"{path}: event {event_id!r} assigns outcome with zero selected tests")
        if verdict == "FAIL" and failure_class not in ctx["failure_classes"]:
            errors.append(f"{path}: event {event_id!r} has undeclared failure class")
        if verdict != "FAIL" and failure_class is not None:
            errors.append(f"{path}: event {event_id!r} non-FAIL carries failure class")
        if verdict in {"PASS", "FAIL"} and not _qualifying_result(result, ctx["oracle"]["timeout_policy"]):
            errors.append(f"{path}: event {event_id!r} {verdict} is inconsistent with result fields")

        if route is not None and event["kind"] == "BUILD" and event["argv"] != route["build_argv"]:
            errors.append(f"{path}: BUILD event {event_id!r} argv disagrees with sealed route")
        if route is not None and event["kind"] == "LAUNCH" and event["argv"] != route["launch_argv"]:
            errors.append(f"{path}: LAUNCH event {event_id!r} argv disagrees with sealed route")
        if event["kind"] == "HERMIT_EXECUTION":
            for key in ("cell_id", "cell_sha256", "invocation_id", "replica_index", "seed", "effective_config_sha256"):
                if event[key] is None:
                    errors.append(f"{path}: Hermit event {event_id!r} requires non-null {key}")
            if event["measurement_role"] not in {"DISCOVERY", "CONFIRMATION"}:
                errors.append(f"{path}: Hermit event {event_id!r} requires measurement role")
            seed = event["seed"]
            if isinstance(seed, dict):
                if seed["flag"] != ctx["swept_flag"]:
                    errors.append(f"{path}: Hermit event {event_id!r} varies non-swept seed")
                if seed["value"] not in ctx["population"]:
                    errors.append(f"{path}: Hermit event {event_id!r} seed outside population")
                tuple_events.setdefault((event["cell_id"], seed["flag"], seed["value"]), set()).add(event_id)
            reports = [item for item in event["artifacts"] if item["kind"] == "SAFEHERMIT_REPORT"]
            if cell_id in cells and isinstance(seed, dict):
                cell = cells[cell_id]
                if event["effective_config_sha256"] != cell["cell_sha256"]:
                    errors.append(f"{path}: Hermit event {event_id!r} effective config differs from cell")
                errors.extend(_argv_contract_errors(
                    event["argv"], design, plan["resource_bounds"],
                    route["repeat_matcher"] if route is not None else {"kind": "BUILTIN_VERIFY"},
                    f"{path}.events[{event_id}].argv",
                    template=False, require_safehermit=True,
                ))
                expected = _replace_flag_value(cell["hermit"]["argv"], ctx["swept_flag"], str(seed["value"]))
                if len(reports) == 1:
                    expected = [reports[0]["locator"] if token == "{SAFEHERMIT_REPORT}" else token for token in expected]
                if event["argv"] != expected:
                    errors.append(f"{path}: Hermit event {event_id!r} argv drifts from cell template")
            if len(reports) != 1:
                errors.append(f"{path}: Hermit event {event_id!r} requires exactly one safehermit report")
            else:
                report = reports[0]
                payload = tracked_payloads.get(report["artifact_id"])
                if report["storage"] != "TRACKED" or report["truncated"] or payload is None:
                    errors.append(f"{path}: Hermit event {event_id!r} safehermit report must be complete tracked evidence")
                elif cell_id in cells:
                    errors.extend(_safehermit_report_errors(
                        payload, cells[cell_id], result, plan["resource_bounds"],
                        f"{path}: Hermit event {event_id!r}",
                    ))
            key = (event["invocation_id"], event["replica_index"])
            if key in seen_invocation_replicas:
                errors.append(f"{path}: duplicate invocation/replica {key!r}")
            seen_invocation_replicas.add(key)
        elif any(event[key] is not None for key in (
            "cell_id", "cell_sha256", "measurement_role", "invocation_id",
            "replica_index", "seed", "effective_config_sha256",
        )):
            errors.append(f"{path}: non-Hermit event {event_id!r} carries execution-only fields")
    return cells, events, tuple_events


def _validate_samples_matchers(
    record: dict[str, Any], path: str, ctx: dict[str, Any],
    cells: dict[str, dict[str, Any]], events: dict[str, dict[str, Any]],
    tuple_events: dict[tuple[str, str, int], set[str]],
    tracked_payloads: dict[str, bytes], errors: list[str],
) -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]], dict[str, list[dict[str, Any]]]]:
    samples = _unique_ids(record["samples"], "sample_id", f"{path}.samples", errors)
    matchers = _unique_ids(record["matchers"], "matcher_id", f"{path}.matchers", errors)
    tuple_to_sample: dict[tuple[str, str, int], str] = {}
    assigned: dict[str, str] = {}
    by_cell: dict[str, list[dict[str, Any]]] = {}
    for sample_id, sample in samples.items():
        key = (sample["cell_id"], sample["seed"]["flag"], sample["seed"]["value"])
        prior = tuple_to_sample.setdefault(key, sample_id)
        if prior != sample_id:
            errors.append(f"{path}: duplicate sample tuple in {prior!r} and {sample_id!r}")
        by_cell.setdefault(sample["cell_id"], []).append(sample)
        if sample["cell_id"] not in cells:
            errors.append(f"{path}: sample {sample_id!r} references unknown cell")
        if sample["seed"]["flag"] != ctx["swept_flag"]:
            errors.append(f"{path}: sample {sample_id!r} varies non-swept seed")
        refs = set(sample["execution_event_ids"])
        if refs != tuple_events.get(key, set()):
            errors.append(f"{path}: sample {sample_id!r} must cover every Hermit event for its tuple exactly")
        roles: list[str] = []
        qualifying: set[tuple[Any, Any]] = set()
        all_qualifying = True
        for ref in sample["execution_event_ids"]:
            old = assigned.setdefault(ref, sample_id)
            if old != sample_id:
                errors.append(f"{path}: Hermit event {ref!r} belongs to more than one sample")
            event = events.get(ref)
            if event is None or event["kind"] != "HERMIT_EXECUTION":
                errors.append(f"{path}: sample {sample_id!r} references unknown/non-Hermit event {ref!r}")
                all_qualifying = False
                continue
            roles.append(event["measurement_role"])
            if event["cell_id"] != sample["cell_id"] or event["seed"] != sample["seed"]:
                errors.append(f"{path}: sample {sample_id!r} event {ref!r} disagrees with tuple")
            result = event["result"]
            qualifies = _qualifying_result(result, ctx["oracle"]["timeout_policy"])
            all_qualifying = all_qualifying and qualifies
            if qualifies:
                qualifying.add((result["target_oracle"], result["failure_class_id"]))
            if sample["status"] == "VALID":
                if not qualifies:
                    errors.append(f"{path}: valid sample {sample_id!r} contains non-qualifying execution")
                if result["target_oracle"] != sample["target_outcome"]:
                    errors.append(f"{path}: sample {sample_id!r} event {ref!r} disagrees with target outcome")
                if result["failure_class_id"] != sample["failure_class_id"]:
                    errors.append(f"{path}: sample {sample_id!r} event {ref!r} disagrees with failure class")
        if roles.count("DISCOVERY") != 1 or any(role not in {"DISCOVERY", "CONFIRMATION"} for role in roles):
            errors.append(f"{path}: sample {sample_id!r} needs one DISCOVERY and only CONFIRMATION repeats")
        if sample["status"] == "VALID":
            if sample["target_outcome"] not in {"PASS", "FAIL"} or sample["censor_reason"] is not None:
                errors.append(f"{path}: valid sample {sample_id!r} needs uncensored PASS/FAIL")
            if len(qualifying) != 1:
                errors.append(f"{path}: valid sample {sample_id!r} has contradictory tuple results")
        else:
            if sample["target_outcome"] is not None or sample["failure_class_id"] is not None or not sample["censor_reason"]:
                errors.append(f"{path}: censored sample {sample_id!r} requires null outcome/class and reason")
            if all_qualifying and len(qualifying) == 1:
                errors.append(f"{path}: censored sample {sample_id!r} hides agreeing qualifying outcomes")
    hermit_ids = set().union(*tuple_events.values()) if tuple_events else set()
    unassigned = hermit_ids - set(assigned)
    if unassigned:
        errors.append(f"{path}: Hermit event(s) unassigned to samples: {sorted(unassigned)!r}")
    for cell_id, cell_samples in by_cell.items():
        ordered = sorted(cell_samples, key=lambda item: item["selection_index"])
        indexes = [item["selection_index"] for item in ordered]
        if indexes != list(range(len(ordered))):
            errors.append(f"{path}: cell {cell_id!r} selection indexes must be zero-based prefix")
        values = [item["seed"]["value"] for item in ordered]
        if values != ctx["order"][:len(values)]:
            errors.append(f"{path}: cell {cell_id!r} samples do not follow sealed seed prefix")
        if len(values) > ctx["attempted_cap"]:
            errors.append(f"{path}: cell {cell_id!r} exceeds attempted seed cap")

    for matcher_id, matcher in matchers.items():
        refs = matcher["execution_event_ids"]
        matched = [events.get(ref) for ref in refs]
        if any(event is None or event["kind"] != "HERMIT_EXECUTION" for event in matched):
            errors.append(f"{path}: matcher {matcher_id!r} references unknown/non-Hermit event")
            continue
        tuples = {(event["cell_id"], json.dumps(event["seed"], sort_keys=True)) for event in matched if event}
        outcomes = {(event["result"]["target_oracle"], event["result"]["failure_class_id"]) for event in matched if event}
        if len(tuples) != 1 or len(outcomes) != 1:
            errors.append(f"{path}: matcher {matcher_id!r} does not compare one agreeing tuple")
        details = matcher["details"]
        route_ids = {event["route_id"] for event in matched if event}
        sealed_matcher = (
            ctx["routes"][next(iter(route_ids))]["repeat_matcher"]
            if len(route_ids) == 1 and next(iter(route_ids)) in ctx["routes"]
            else None
        )
        if sealed_matcher is None or matcher["kind"] != sealed_matcher["kind"]:
            errors.append(f"{path}: matcher {matcher_id!r} kind differs from sealed route")
        if matcher["kind"] == "BUILTIN_VERIFY":
            if set(details) != {"verified", "bitwise_parity", "comparison", "relaxations"}:
                errors.append(f"{path}: matcher {matcher_id!r} BUILTIN_VERIFY details mismatch")
            if len(refs) != 2:
                errors.append(f"{path}: matcher {matcher_id!r} BUILTIN_VERIFY must pair two executions")
            invocations = {event["invocation_id"] for event in matched if event}
            replicas = {event["replica_index"] for event in matched if event}
            if len(invocations) != 1 or replicas != {1, 2}:
                errors.append(f"{path}: matcher {matcher_id!r} BUILTIN_VERIFY invocation/replica pairing invalid")
            for event in matched:
                if event and (_flag_values(event["argv"][:event["argv"].index("--")] if "--" in event["argv"] else event["argv"], "--verify", False) != [None] or _flag_values(event["argv"][:event["argv"].index("--")] if "--" in event["argv"] else event["argv"], "--verify-strict", False) != [None]):
                    errors.append(f"{path}: matcher {matcher_id!r} requires exact --verify --verify-strict")
                if event and _flag_values(event["argv"][:event["argv"].index("--")] if "--" in event["argv"] else event["argv"], "--verify-allow") != ["both"]:
                    errors.append(f"{path}: matcher {matcher_id!r} requires exact --verify-allow=both")
        else:
            if set(details) != {"comparator_argv", "comparator_sha256", "exit_signal_match", "stdout_match", "stderr_match", "canonical_info_match", "relaxations"}:
                errors.append(f"{path}: matcher {matcher_id!r} EQUIVALENT details mismatch")
            if sealed_matcher is not None and (
                details.get("comparator_argv") != sealed_matcher.get("comparator_argv")
                or details.get("comparator_sha256")
                != sealed_matcher.get("comparator_artifact", {}).get("sha256")
            ):
                errors.append(f"{path}: matcher {matcher_id!r} comparator differs from sealed route")
            comparator_artifact = sealed_matcher.get("comparator_artifact", {}) if sealed_matcher else {}
            if not _artifact_is_working_evidence(comparator_artifact) or comparator_artifact.get("artifact_id") not in tracked_payloads:
                errors.append(f"{path}: matcher {matcher_id!r} sealed comparator is not complete committed evidence")
        info = matcher["canonical_info_artifact"]
        if info["kind"] != "CANONICAL_INFO" or info["bytes"] < 1:
            errors.append(f"{path}: matcher {matcher_id!r} needs nonempty canonical INFO")
        payload = tracked_payloads.get(info["artifact_id"])
        if not _artifact_is_working_evidence(info) or payload is None:
            errors.append(f"{path}: matcher {matcher_id!r} canonical INFO is not complete committed evidence")
        else:
            try:
                observed_info = json.loads(payload)
            except (UnicodeDecodeError, json.JSONDecodeError):
                observed_info = None
            if observed_info != _matcher_info_payload(matcher):
                errors.append(f"{path}: matcher {matcher_id!r} canonical INFO payload disagrees with matcher")
    return samples, matchers, by_cell


def _witness_ok(
    sample: dict[str, Any], expected: str,
    events: dict[str, dict[str, Any]], matchers: dict[str, dict[str, Any]],
    oracle: dict[str, Any],
) -> bool:
    if sample["status"] != "VALID" or sample["target_outcome"] != expected:
        return False
    refs = set(sample["execution_event_ids"])
    if len(refs) < 2:
        return False
    selected = [events.get(ref) for ref in refs]
    if any(event is None for event in selected):
        return False
    for event in selected:
        assert event is not None
        result = event["result"]
        if not _qualifying_result(result, oracle["timeout_policy"]):
            return False
        if result["target_oracle"] != expected or result["failure_class_id"] != sample["failure_class_id"]:
            return False
        durable = [item for item in event["artifacts"] if _artifact_is_working_evidence(item)]
        kinds = {item["kind"] for item in durable}
        if not {"STDOUT", "STDERR", "RESULT", "SAFEHERMIT_REPORT"}.issubset(kinds):
            return False
        result_artifacts = [item for item in durable if item["kind"] == "RESULT"]
        report_artifacts = [item for item in durable if item["kind"] == "SAFEHERMIT_REPORT"]
        if (
            len(result_artifacts) != 1 or result_artifacts[0]["bytes"] < 1
            or len(report_artifacts) != 1 or report_artifacts[0]["bytes"] < 1
        ):
            return False
    return any(
        set(matcher["execution_event_ids"]) == refs
        and matcher["canonical_info_artifact"]["kind"] == "CANONICAL_INFO"
        and _artifact_is_working_evidence(matcher["canonical_info_artifact"])
        for matcher in matchers.values()
    )


def _check_route_evidence(
    entries: list[dict[str, Any]], *, path: str, ctx: dict[str, Any],
    events: dict[str, dict[str, Any]], errors: list[str], blocked: bool, exact: bool,
) -> None:
    by_route: dict[str, dict[str, Any]] = {}
    for item in entries:
        route_id = item["route_id"]
        if route_id in by_route:
            errors.append(f"{path}: duplicate route_evidence for {route_id!r}")
        by_route[route_id] = item
    if exact and set(by_route) != set(ctx["routes"]):
        errors.append(f"{path}: route_evidence must cover every authorized route exactly")
    elif not exact and (not by_route or not set(by_route).issubset(ctx["routes"])):
        errors.append(f"{path}: route_evidence must be a nonempty authorized subset")
    for route_id, item in by_route.items():
        route = ctx["routes"].get(route_id)
        if route is None:
            continue
        named_cells = set(item["cell_ids"])
        authorized_cells = set(route["authorized_cell_ids"])
        if (exact and named_cells != authorized_cells) or (not exact and not named_cells.issubset(authorized_cells)):
            errors.append(f"{path}: route_evidence {route_id!r} cell IDs violate authorized coverage")
        for event_id in item["evidence_event_ids"]:
            event = events.get(event_id)
            if event is None or event["route_id"] != route_id:
                errors.append(f"{path}: route_evidence {route_id!r} references wrong/unknown event")
                continue
            if event["cell_id"] is not None and event["cell_id"] not in item["cell_ids"]:
                errors.append(f"{path}: route_evidence {route_id!r} event outside named cells")
            if not any(_artifact_is_durable(a) for a in event["artifacts"]):
                errors.append(f"{path}: route_evidence {route_id!r} lacks durable event evidence")
            result = event["result"]
            command_succeeded = event["kind"] in {"BUILD", "LAUNCH"} and result["harness_status"] == "COMPLETE" and not result["timed_out"] and result["exit_code"] == 0 and result["signal"] is None
            if blocked and (command_succeeded or _qualifying_result(result, ctx["oracle"]["timeout_policy"])):
                errors.append(f"{path}: BLOCKED route {route_id!r} cites successful attempt evidence")


def _derived_exhaustion(
    ctx: dict[str, Any], cells: dict[str, dict[str, Any]],
    samples_by_cell: dict[str, list[dict[str, Any]]],
) -> bool:
    if not ctx["authorized_cells"] or not set(ctx["authorized_cells"]).issubset(cells):
        return False
    for cell_id in ctx["authorized_cells"]:
        samples = samples_by_cell.get(cell_id, [])
        valid = sum(sample["status"] == "VALID" for sample in samples)
        if valid < ctx["required_n"] and len(samples) < ctx["attempted_cap"]:
            return False
    return True


def _validate_outcomes(
    record: dict[str, Any], path: str, ctx: dict[str, Any],
    cells: dict[str, dict[str, Any]], events: dict[str, dict[str, Any]],
    samples: dict[str, dict[str, Any]], matchers: dict[str, dict[str, Any]],
    samples_by_cell: dict[str, list[dict[str, Any]]],
    artifacts: dict[str, dict[str, Any]], locators: dict[str, str],
    tracked_payloads: dict[str, bytes], errors: list[str],
) -> bool:
    history = _unique_ids(record["outcome_history"], "classification_id", f"{path}.outcome_history", errors)
    current = record["current_outcome"]
    pass_samples = [sample for sample in samples.values() if _witness_ok(sample, "PASS", events, matchers, ctx["oracle"])]
    fail_samples = [sample for sample in samples.values() if _witness_ok(sample, "FAIL", events, matchers, ctx["oracle"])]
    working_pairs = [
        (p, f) for p in pass_samples for f in fail_samples
        if p["cell_id"] == f["cell_id"]
        and p["seed"]["flag"] == f["seed"]["flag"] == ctx["swept_flag"]
        and p["seed"]["value"] != f["seed"]["value"]
    ]
    if isinstance(current, dict) and current.get("kind") in {"BLOCKED-WITH-NAMED-CAUSE", "NOT-DETERMINABLE"} and working_pairs:
        errors.append(f"{path}: ordered outcome requires WORKING when full bracket exists")

    if record["attempt_state"] == "PLANNED":
        if any((record["cells"], record["events"], record["samples"], record["matchers"], record["outcome_history"])) or current is not None:
            errors.append(f"{path}: PLANNED record cannot contain attempt material or outcome")
    else:
        if not isinstance(current, dict) or current.get("kind") not in OUTCOME_KINDS:
            errors.append(f"{path}: MATERIAL record must have exactly one current outcome")
        if not record["outcome_history"]:
            errors.append(f"{path}: MATERIAL record must preserve outcome history")
    if record["outcome_history"]:
        previous: str | None = None
        for index, outcome in enumerate(record["outcome_history"]):
            if outcome["supersedes"] != previous:
                errors.append(f"{path}.outcome_history[{index}]: supersedes chain broken")
            previous = outcome["classification_id"]
        if current != record["outcome_history"][-1]:
            errors.append(f"{path}.current_outcome: must equal last history entry")
    known = set(cells) | set(events) | set(samples) | set(matchers) | set(artifacts)
    for index, outcome in enumerate(record["outcome_history"]):
        for ref in outcome["evidence_refs"]:
            if ref not in known:
                errors.append(f"{path}.outcome_history[{index}]: unknown evidence ref {ref!r}")
        if not any(_durable_reference(ref, events, samples, matchers, artifacts) for ref in outcome["evidence_refs"]):
            errors.append(f"{path}.outcome_history[{index}]: terminal evidence is not durable")

    if isinstance(current, dict) and current.get("kind") == "WORKING":
        witness_samples: list[dict[str, Any]] = []
        for label in ("PASS", "FAIL"):
            witness = current[label.lower() + "_witness"]
            sample = samples.get(witness["sample_id"])
            matcher = matchers.get(witness["matcher_id"])
            if sample is None or matcher is None:
                errors.append(f"{path}: {label} witness references unknown sample/matcher")
                continue
            witness_samples.append(sample)
            if not _witness_ok(sample, label, events, matchers, ctx["oracle"]):
                errors.append(f"{path}: {label} witness does not meet complete evidence bracket")
            if set(matcher["execution_event_ids"]) != set(sample["execution_event_ids"]):
                errors.append(f"{path}: {label} matcher does not exactly cover witness executions")
        if len(witness_samples) == 2:
            if witness_samples[0]["cell_id"] != witness_samples[1]["cell_id"]:
                errors.append(f"{path}: WORKING arms must share one frozen cell")
            if witness_samples[0]["seed"]["value"] == witness_samples[1]["seed"]["value"]:
                errors.append(f"{path}: WORKING arms require distinct swept seeds")

    if isinstance(current, dict) and current.get("kind") == "BLOCKED-WITH-NAMED-CAUSE":
        cause = current["cause"]
        _check_route_evidence(cause["route_evidence"], path=path, ctx=ctx, events=events, errors=errors, blocked=True, exact=True)
        cited = {event_id for item in cause["route_evidence"] for event_id in item["evidence_event_ids"]}
        if not cited.issubset(set(current["evidence_refs"])):
            errors.append(f"{path}: BLOCKED evidence_refs omit named cause event")
        stage = cause["stage"]
        diagnostic = cause["diagnostic"]
        stage_kinds = {
            "BUILD": {"BUILD"}, "LAUNCH": {"LAUNCH"},
            "HERMIT_EXECUTION": {"HERMIT_EXECUTION"},
            "TARGET_EXECUTION": {"HERMIT_EXECUTION"},
            "ORACLE": {"HERMIT_EXECUTION"},
            "REPEAT": {"HERMIT_EXECUTION"},
        }
        cited_events = [events[event_id] for event_id in cited if event_id in events]
        diagnostic_stages = {
            "BUILD_FAILURE": {"BUILD"}, "LAUNCH_FAILURE": {"LAUNCH"},
            "HARNESS_FAILURE": {"HERMIT_EXECUTION", "REPEAT"},
            "TARGET_FAILURE": {"TARGET_EXECUTION"},
            "ORACLE_AMBIGUITY": {"ORACLE", "REPEAT"},
            "RESOURCE_EXHAUSTION": {"BUILD", "LAUNCH", "HERMIT_EXECUTION", "TARGET_EXECUTION", "REPEAT"},
            "UNSUPPORTED": {"BUILD", "LAUNCH", "HERMIT_EXECUTION", "TARGET_EXECUTION"},
        }
        if stage not in diagnostic_stages[diagnostic["kind"]]:
            errors.append(f"{path}: BLOCKED diagnostic kind disagrees with stage")
        if any(event["kind"] not in stage_kinds[stage] for event in cited_events):
            errors.append(f"{path}: BLOCKED stage disagrees with cited event kind")
        if stage == "ORACLE" and any(event["result"]["target_oracle"] != "AMBIGUOUS" for event in cited_events):
            errors.append(f"{path}: ORACLE blocker lacks ambiguous oracle evidence")
        if stage == "REPEAT" and not any(event["measurement_role"] == "CONFIRMATION" for event in cited_events):
            errors.append(f"{path}: REPEAT blocker lacks confirmation evidence")
        for event in cited_events:
            result = event["result"]
            if diagnostic["kind"] == "BUILD_FAILURE" and (event["kind"] != "BUILD" or result["harness_status"] == "COMPLETE"):
                errors.append(f"{path}: BUILD_FAILURE diagnostic lacks build failure result")
            if diagnostic["kind"] == "LAUNCH_FAILURE" and (event["kind"] != "LAUNCH" or result["harness_status"] == "COMPLETE"):
                errors.append(f"{path}: LAUNCH_FAILURE diagnostic lacks launch failure result")
            if diagnostic["kind"] == "HARNESS_FAILURE" and result["harness_status"] == "COMPLETE":
                errors.append(f"{path}: HARNESS_FAILURE diagnostic cites complete harness")
            if diagnostic["kind"] == "TARGET_FAILURE" and result["target_oracle"] != "FAIL":
                errors.append(f"{path}: TARGET_FAILURE diagnostic lacks target FAIL")
            if diagnostic["kind"] == "ORACLE_AMBIGUITY" and result["target_oracle"] != "AMBIGUOUS":
                errors.append(f"{path}: ORACLE_AMBIGUITY diagnostic lacks ambiguity")
            if diagnostic["kind"] == "RESOURCE_EXHAUSTION" and result["harness_status"] not in {"TIMEOUT", "OOM", "KILLED"}:
                errors.append(f"{path}: RESOURCE_EXHAUSTION diagnostic lacks resource result")
            retained = result["diagnostic"]
            if diagnostic["message"] != retained or (diagnostic["code"] is not None and diagnostic["code"] not in retained):
                errors.append(f"{path}: BLOCKED diagnostic does not match retained event result")
        adjudication = cause["campaign_owner_adjudication"]
        if cited_events:
            latest_evidence = max(_parse_timestamp(event["finished_at"]) for event in cited_events)
            if latest_evidence > _parse_timestamp(adjudication["decided_at"]):
                errors.append(f"{path}: BLOCKED adjudication predates cited evidence")
        if _parse_timestamp(adjudication["decided_at"]) < _parse_timestamp(ctx["plan"]["sealed_at"]):
            errors.append(f"{path}: BLOCKED adjudication predates sealed plan")
        if adjudication["owner_identity"] != ctx["plan"]["campaign_owner_identity"]:
            errors.append(f"{path}: BLOCKED adjudication is not from campaign owner")
        if _parse_timestamp(adjudication["decided_at"]) > _parse_timestamp(current["classified_at"]):
            errors.append(f"{path}: BLOCKED adjudication postdates classification")
        adjudication_id = adjudication["evidence_artifact_id"]
        adjudication_artifact = artifacts.get(adjudication_id)
        if adjudication_id not in current["evidence_refs"]:
            errors.append(f"{path}: BLOCKED evidence_refs omit owner adjudication artifact")
        if (
            adjudication_artifact is None
            or adjudication_artifact["kind"] != "ADJUDICATION"
            or adjudication_artifact["storage"] != "TRACKED"
            or adjudication_artifact["truncated"]
        ):
            errors.append(f"{path}: BLOCKED requires complete committed owner adjudication evidence")
        else:
            payload = tracked_payloads.get(adjudication_id)
            try:
                observed_adjudication = json.loads(payload) if payload is not None else None
            except (UnicodeDecodeError, json.JSONDecodeError):
                observed_adjudication = None
            expected_adjudication = {
                "schema_version": 1,
                **{key: value for key, value in adjudication.items() if key != "evidence_artifact_id"},
            }
            if observed_adjudication != expected_adjudication:
                errors.append(f"{path}: BLOCKED adjudication payload disagrees with owner decision")
        for event in cited_events:
            diagnostics = [item for item in event["artifacts"] if item["kind"] == "DIAGNOSTIC"]
            if len(diagnostics) != 1:
                errors.append(f"{path}: BLOCKED cited event {event['event_id']!r} requires one DIAGNOSTIC artifact")
                continue
            diagnostic_artifact = diagnostics[0]
            payload = tracked_payloads.get(diagnostic_artifact["artifact_id"])
            expected_diagnostic = (event["result"]["diagnostic"] + "\n").encode("utf-8")
            if diagnostic_artifact["storage"] != "TRACKED" or payload != expected_diagnostic:
                errors.append(f"{path}: BLOCKED diagnostic artifact disagrees with cited event")

    derived = _derived_exhaustion(ctx, cells, samples_by_cell)
    if derived and not working_pairs:
        for cell_id, cell_samples in samples_by_cell.items():
            candidates = {"PASS": [], "FAIL": []}
            for sample in cell_samples:
                for event_id in sample["execution_event_ids"]:
                    event = events.get(event_id)
                    if event is not None and _qualifying_result(event["result"], ctx["oracle"]["timeout_policy"]):
                        candidates[event["result"]["target_oracle"]].append(sample)
            if candidates["PASS"] and candidates["FAIL"]:
                for arm in ("PASS", "FAIL"):
                    witnessed = any(_witness_ok(sample, arm, events, matchers, ctx["oracle"]) for sample in candidates[arm])
                    if not witnessed and any(len(sample["execution_event_ids"]) < ctx["design"]["repeat_attempt_cap"] for sample in candidates[arm]):
                        derived = False
    if isinstance(current, dict) and current.get("kind") == "NOT-DETERMINABLE":
        _check_route_evidence(current["route_evidence"], path=path, ctx=ctx, events=events, errors=errors, blocked=False, exact=current["measurement_budget_exhausted"])
        cited = {
            event_id
            for item in current["route_evidence"]
            for event_id in item["evidence_event_ids"]
        }
        if not cited.issubset(set(current["evidence_refs"])):
            errors.append(f"{path}: NOT-DETERMINABLE evidence_refs omit inspected event")
        cited_events = [events[event_id] for event_id in cited if event_id in events]
        cited_commands = {tuple(event["argv"]) for event in cited_events}
        inspected_commands = {tuple(command) for command in current["inspected_commands"]}
        if inspected_commands != cited_commands:
            errors.append(f"{path}: NOT-DETERMINABLE inspected_commands must equal cited event commands")
        inspected_artifact_ids: set[str] = set()
        for ref in current["inspected_artifacts"]:
            if ref in artifacts:
                inspected_artifact_ids.add(ref)
            elif ref in locators:
                inspected_artifact_ids.add(locators[ref])
            else:
                errors.append(f"{path}: NOT-DETERMINABLE inspected artifact {ref!r} is not retained")
        cited_artifact_ids = {
            item["artifact_id"]
            for event in cited_events
            for item in event["artifacts"]
        }
        if not inspected_artifact_ids.issubset(cited_artifact_ids):
            errors.append(f"{path}: NOT-DETERMINABLE inspected artifact was not produced by cited route evidence")
        for event in cited_events:
            event_artifact_ids = {item["artifact_id"] for item in event["artifacts"]}
            if not inspected_artifact_ids.intersection(event_artifact_ids):
                errors.append(f"{path}: NOT-DETERMINABLE omits inspected artifact for cited event {event['event_id']!r}")
        if current["measurement_budget_exhausted"] != derived:
            errors.append(f"{path}: NOT-DETERMINABLE exhaustion boolean disagrees with derived cell budgets")
        if current["measurement_budget_exhausted"] and current["pending_in_scope_measurements"]:
            errors.append(f"{path}: exhausted NOT-DETERMINABLE retains pending measurements")
        if not current["measurement_budget_exhausted"] and not current["pending_in_scope_measurements"]:
            errors.append(f"{path}: non-exhausted NOT-DETERMINABLE must name pending measurement")
        if not any(
            (ref in artifacts and _artifact_is_durable(artifacts[ref]))
            or (ref in locators and _artifact_is_durable(artifacts[locators[ref]]))
            for ref in current["inspected_artifacts"]
        ):
            errors.append(f"{path}: NOT-DETERMINABLE has no durable inspected artifact")
    return derived


def _semantic_record_errors(
    record: dict[str, Any], path: str, campaign_root: Path,
    campaign_owner: str,
) -> list[str]:
    errors: list[str] = []
    _validate_plan_receipt(record, path, campaign_root, errors)
    if record["attempt_state"] == "MATERIAL":
        errors.extend(
            _committed_path_errors(campaign_root, path, f"{path}: material record")
        )
    ctx = _validate_identity_plan(record, path, campaign_owner, errors)
    artifacts, locators, tracked_payloads = _validate_artifacts(
        record, path, campaign_root, errors
    )
    cells, events, tuple_events = _validate_cells_events(
        record, path, ctx, artifacts, tracked_payloads, errors
    )
    samples, matchers, samples_by_cell = _validate_samples_matchers(
        record, path, ctx, cells, events, tuple_events, tracked_payloads, errors
    )
    _validate_outcomes(
        record, path, ctx, cells, events, samples, matchers,
        samples_by_cell, artifacts, locators, tracked_payloads, errors,
    )
    return errors


def _cohort_v2_errors(cohort: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    expected_keys = {
        "schema_version", "source", "counting_key", "expected_target_count",
        "test_selector_convention", "build_mode_selector_convention",
        "row_digest_convention", "targets_digest_convention",
        "targets_sha256", "targets",
    }
    if set(cohort) != expected_keys:
        errors.append("cohort.json: top-level metadata keys differ from published registry")
    if cohort.get("targets_sha256") != "49e1952b2bd857229646842f6a6da845e5b7ecea14e14cd9c077190a699750c0":
        errors.append("cohort.json: targets_sha256 must equal the published registry digest")
    if _json_sha256(cohort.get("targets")) != cohort.get("targets_sha256"):
        errors.append("cohort.json: targets array does not match targets_sha256")
    expected_conventions = {
        "test_selector_convention": "EXACT_FILTER preserves the --exact argument; NAMED_TEST preserves the launcher --tests argument; WHOLE_TARGET means the pinned invocation has neither selector flag.",
        "build_mode_selector_convention": "exact shlex token slice after buck test/run and before the invoked fbcode// target; an empty array means no explicit build-mode selector",
        "row_digest_convention": "sha256 of exact UTF-8 Markdown table-row bytes, excluding the line terminator",
        "targets_digest_convention": "sha256 of UTF-8 json.dumps(targets, sort_keys=True, separators=(" + chr(39) + "," + chr(39) + ", " + chr(39) + ":" + chr(39) + "), ensure_ascii=False)",
    }
    for key, value in expected_conventions.items():
        if cohort.get(key) != value:
            errors.append(f"cohort.json: {key} differs from published convention")
    expected_target_keys = {
        "service", "canonical_target", "target_key_sha256", "target_directory",
        "testinfra_id", "test_name", "source_label_qualifier", "test_selector",
        "build_mode_selector", "invoked_target", "standalone_argv",
        "candidate_ranking", "source_row_line", "source_row_sha256",
    }
    for index, entry in enumerate(cohort.get("targets", [])):
        here = f"cohort.json.targets[{index}]"
        if set(entry) != expected_target_keys:
            errors.append(f"{here}: keys differ from published registry shape")
            continue
        errors.extend(_selector_errors(entry["test_selector"], f"{here}.test_selector"))
        argv = entry["standalone_argv"]
        try:
            target_index = argv.index(entry["invoked_target"])
        except ValueError:
            errors.append(f"{here}.standalone_argv: missing invoked_target")
        else:
            if argv[:2] not in (["buck", "test"], ["buck", "run"]):
                errors.append(f"{here}.standalone_argv: must begin buck test/run")
            if argv[2:target_index] != entry["build_mode_selector"]:
                errors.append(f"{here}.standalone_argv: build-mode slice disagrees")
        if not isinstance(entry["source_row_line"], int) or isinstance(entry["source_row_line"], bool):
            errors.append(f"{here}.source_row_line: must be an integer")
        if re.fullmatch(r"[0-9a-f]{64}", entry["source_row_sha256"]) is None:
            errors.append(f"{here}.source_row_sha256: must be sha256")
        native = entry["candidate_ranking"]["native_failure_rate"]
        if not math.isclose(native["rate"], native["failures"] / native["executed"], rel_tol=0, abs_tol=1e-15):
            errors.append(f"{here}.candidate_ranking: native rate is not failures/executed")
    return errors


def _record_budget_exhausted(record: dict[str, Any]) -> bool:
    try:
        design = record["plan"]["measurement_design"]
        required_n = design["required_valid_seed_tuples"]
        cap = design["max_attempted_seed_tuples"]
        authorized = {cell_id for route in record["plan"]["authorized_routes"] for cell_id in route["authorized_cell_ids"]}
        cells = {cell["cell_id"] for cell in record["cells"]}
        if not authorized or not authorized.issubset(cells):
            return False
        by_cell = {cell_id: [] for cell_id in authorized}
        events = {event["event_id"]: event for event in record["events"]}
        matchers = {matcher["matcher_id"]: matcher for matcher in record["matchers"]}
        for sample in record["samples"]:
            if sample["cell_id"] in by_cell:
                by_cell[sample["cell_id"]].append(sample)
        for cell_id, samples in by_cell.items():
            valid = sum(sample["status"] == "VALID" for sample in samples)
            if valid < required_n and len(samples) < cap:
                return False
            candidates = {"PASS": [], "FAIL": []}
            for sample in samples:
                for event_id in sample["execution_event_ids"]:
                    event = events[event_id]
                    if _qualifying_result(event["result"], record["plan"]["target_oracle"]["timeout_policy"]):
                        candidates[event["result"]["target_oracle"]].append(sample)
            if candidates["PASS"] and candidates["FAIL"]:
                for arm in ("PASS", "FAIL"):
                    witnessed = any(_witness_ok(sample, arm, events, matchers, record["plan"]["target_oracle"]) for sample in candidates[arm])
                    if not witnessed and any(len(sample["execution_event_ids"]) < design["repeat_attempt_cap"] for sample in candidates[arm]):
                        return False
        return True
    except (KeyError, TypeError):
        return False


def validate_tree(root: Path) -> dict[str, int | str]:
    root = root.resolve()
    errors: list[str] = []
    cohort = _load_json(root / "cohort.json")
    schema = _load_json(root / "experiment.schema.json")
    metadata = _load_json(root / "metadata.json")
    for filename in ("cohort.json", "experiment.schema.json", "metadata.json"):
        errors.extend(
            _committed_path_errors(root, filename, filename)
        )
    campaign_owner = (
        metadata.get("campaign_owner_identity") if isinstance(metadata, dict) else None
    )
    if not isinstance(campaign_owner, str) or not campaign_owner:
        errors.append("metadata.json: campaign_owner_identity must be nonempty")
        campaign_owner = "<invalid>"

    if not isinstance(cohort, dict) or not isinstance(cohort.get("targets"), list):
        raise ValidationFailure([f"{root / 'cohort.json'}: targets must be an array"])
    targets = cohort["targets"]
    errors.extend(_cohort_v2_errors(cohort))
    expected_source = {
        "commit": "ee6562142e3796bb56a1dd0e28b77c1c16a78cc2",
        "path": "ai_docs/tfa-candidate-sourcing-2026-09-17.md",
        "blob": "ff01b534352e15a5685b8a41032973d46c089274",
    }
    if cohort.get("schema_version") != 2:
        errors.append("cohort.json: schema_version must equal 2")
    if cohort.get("source") != expected_source:
        errors.append("cohort.json: source must equal the pinned candidate cohort commit/path/blob")
    if cohort.get("counting_key") != "canonical_target":
        errors.append("cohort.json: counting_key must be canonical_target")
    if cohort.get("expected_target_count") != 30 or len(targets) != 30:
        errors.append(f"cohort.json: expected exactly 30 targets, found {len(targets)}")
    for index, entry in enumerate(targets):
        errors.extend(_identity_errors(entry, f"cohort.json.targets[{index}]"))
    canonical_targets = [entry.get("canonical_target") for entry in targets if isinstance(entry, dict)]
    target_directories = [entry.get("target_directory") for entry in targets if isinstance(entry, dict)]
    for duplicate in sorted(_duplicates(canonical_targets), key=repr):
        errors.append(f"cohort.json: duplicate canonical target {duplicate!r}")
    for duplicate in sorted(_duplicates(target_directories), key=repr):
        errors.append(f"cohort.json: duplicate target directory {duplicate!r}")
    cohort_services = {entry.get("service") for entry in targets if isinstance(entry, dict)}
    if cohort_services != ALLOWED_SERVICES:
        errors.append(
            f"cohort.json: service set must be exactly {sorted(ALLOWED_SERVICES)!r}, got {sorted(cohort_services, key=repr)!r}"
        )
    cohort_by_target = {
        entry["canonical_target"]: entry
        for entry in targets
        if isinstance(entry, dict) and isinstance(entry.get("canonical_target"), str)
    }

    services_root = root / "services"
    if not services_root.is_dir():
        errors.append("services/: missing service directory root")
    else:
        actual_service_dirs = {path.name for path in services_root.iterdir() if path.is_dir()}
        if actual_service_dirs != ALLOWED_SERVICES:
            missing = sorted(ALLOWED_SERVICES - actual_service_dirs)
            unexpected = sorted(actual_service_dirs - ALLOWED_SERVICES)
            errors.append(
                f"services/: directory set mismatch; missing={missing!r} unexpected={unexpected!r}"
            )

    record_paths = sorted(services_root.glob("*/*/experiment.json")) if services_root.exists() else []
    all_record_paths = sorted(services_root.rglob("experiment.json")) if services_root.exists() else []
    if record_paths != all_record_paths:
        errors.append("services/: experiment.json must appear exactly at services/<service>/<target>/")

    records: list[tuple[Path, dict[str, Any]]] = []
    seen_record_targets: set[str] = set()
    seen_record_dirs: set[str] = set()
    seen_artifact_locators: dict[str, str] = {}
    for record_path in record_paths:
        record = _load_json(record_path)
        rel = record_path.relative_to(root).as_posix()
        if not isinstance(record, dict):
            errors.append(f"{rel}: record must be an object")
            continue
        errors.extend(_schema_errors(record, schema, schema, rel))
        # Semantic validation assumes the structural keys exist.  Keep malformed
        # records diagnostic rather than turning them into checker tracebacks.
        if not _schema_errors(record, schema, schema, rel):
            errors.extend(
                _semantic_record_errors(record, rel, root, campaign_owner)
            )
            for artifact in _artifact_objects(record):
                locator = artifact["locator"]
                prior = seen_artifact_locators.setdefault(locator, rel)
                if prior != rel:
                    errors.append(
                        f"{rel}: artifact locator {locator!r} is already owned by {prior}"
                    )
        identity = record.get("identity")
        if not isinstance(identity, dict):
            continue
        canonical = identity.get("canonical_target")
        target_directory = identity.get("target_directory")
        expected_rel = f"{target_directory}/experiment.json"
        if rel != expected_rel:
            errors.append(f"{rel}: identity.target_directory points to {expected_rel!r}")
        if canonical in seen_record_targets:
            errors.append(f"{rel}: duplicate instantiated canonical target {canonical!r}")
        seen_record_targets.add(canonical)
        if target_directory in seen_record_dirs:
            errors.append(f"{rel}: duplicate instantiated target directory {target_directory!r}")
        seen_record_dirs.add(target_directory)
        registry = cohort_by_target.get(canonical)
        if registry is None:
            errors.append(f"{rel}: canonical target is not in cohort.json")
        else:
            for key in (
                "service", "canonical_target", "target_key_sha256",
                "target_directory", "testinfra_id", "test_name",
                "source_label_qualifier", "test_selector",
                "build_mode_selector", "invoked_target", "standalone_argv",
            ):
                if identity.get(key) != registry.get(key):
                    errors.append(f"{rel}: identity.{key} disagrees with cohort.json")
            if record.get("candidate_ranking") != registry.get("candidate_ranking"):
                errors.append(f"{rel}: candidate_ranking disagrees with cohort.json")
        records.append((record_path, record))

    counts = {kind: 0 for kind in OUTCOME_KINDS}
    planned = 0
    valid_discovery_tuples: set[tuple[str, str, str, int]] = set()
    censored_discovery = 0
    attempted_discovery = 0
    guest_executions = 0
    negative_details_terminal = True
    for _, record in records:
        identity = record.get("identity", {})
        canonical = identity.get("canonical_target", "")
        for sample in record.get("samples", []):
            attempted_discovery += 1
            if sample.get("status") == "VALID":
                seed = sample.get("seed", {})
                valid_discovery_tuples.add(
                    (canonical, sample.get("cell_id", ""), seed.get("flag", ""), seed.get("value", -1))
                )
            elif sample.get("status") == "CENSORED":
                censored_discovery += 1
        guest_executions += sum(
            event.get("kind") == "HERMIT_EXECUTION" and event.get("result", {}).get("guest_started") is True for event in record.get("events", [])
        )
        if record.get("attempt_state") == "PLANNED":
            planned += 1
        elif record.get("attempt_state") == "MATERIAL":
            outcome = record.get("current_outcome")
            if isinstance(outcome, dict) and outcome.get("kind") in counts:
                kind = outcome["kind"]
                counts[kind] += 1
                if kind == "BLOCKED-WITH-NAMED-CAUSE":
                    if outcome["cause"]["campaign_owner_adjudication"]["disposition"] != "OUTSIDE_SCOPE":
                        negative_details_terminal = False
                elif kind == "NOT-DETERMINABLE":
                    if (
                        not _record_budget_exhausted(record)
                        or outcome["pending_in_scope_measurements"]
                    ):
                        negative_details_terminal = False
            else:
                negative_details_terminal = False
    attempted = sum(counts.values())
    if counts["WORKING"] >= 3:
        conclusion = "SUCCESS"
    elif (
        attempted >= 30
        and counts["WORKING"] == 0
        and negative_details_terminal
    ):
        conclusion = "NEGATIVE RESULT"
    else:
        conclusion = "neither"
    if errors:
        raise ValidationFailure(errors)
    return {
        "cohort_targets": len(targets),
        "records": len(records),
        "planned": planned,
        "A": attempted,
        "W": counts["WORKING"],
        "B": counts["BLOCKED-WITH-NAMED-CAUSE"],
        "U": counts["NOT-DETERMINABLE"],
        "S": len(valid_discovery_tuples),
        "C": censored_discovery,
        "T": attempted_discovery,
        "R": guest_executions,
        "conclusion": conclusion,
        "evidence_authentication": "NOT_CHECKED",
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="TFA experiment root (defaults to the parent of tools/)",
    )
    parser.add_argument(
        "--print-plan-digest",
        type=Path,
        metavar="FILE",
        help="print canonical SHA-256 for FILE plan with plan_sha256 omitted",
    )
    args = parser.parse_args(argv)
    if args.print_plan_digest is not None:
        try:
            document = _load_json(args.print_plan_digest)
            plan = document.get("plan") if isinstance(document, dict) else None
            if not isinstance(plan, dict):
                raise ValidationFailure(
                    [f"{args.print_plan_digest}: top-level plan must be an object"]
                )
        except ValidationFailure as exc:
            for error in exc.errors:
                print(f"ERROR: {error}", file=sys.stderr)
            return 1
        print(canonical_object_sha256(plan, "plan_sha256"))
        return 0
    try:
        counts = validate_tree(args.root)
    except ValidationFailure as exc:
        for error in exc.errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print(
        "OK: cohort={cohort_targets} records={records} planned={planned} "
        "A={A} W={W} B={B} U={U} S={S} C={C} T={T} R={R} conclusion={conclusion} "
        "evidence_authentication={evidence_authentication}".format(**counts)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
