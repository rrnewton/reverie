#!/usr/bin/env python3
"""Run Reverie's full validator inside one proven safe-ci step and record it."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import socket
import stat
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from final_validate_status import (
    FINAL_VALIDATE_STATUS_EXIT_CODES,
    FINAL_VALIDATE_STATUS_PREFIX,
)

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402


# Keep each published contract immutable.  The central producer accepts each
# exact list so deploying parent support does not invalidate a still-current v3
# checkout; it emits the version selected by the source row's ordered gates.
V3_GATES = (
    "Cross-client skill discovery",
    "Build workspace",
    "DBT virtual identity and pidfd_open policy",
    "Test regular workspace cases",
    "Documentation tests",
    "Clippy",
    "Rustfmt",
)
V3_TEST_GATES = (
    "Test regular workspace cases",
    "Documentation tests",
)
V4_GATES = (
    "Cross-client skill discovery",
    "Build workspace",
    "DBT virtual identity and pidfd_open policy",
    "Compile owned public instruction target",
    "Inventory owned public instruction target",
    "Test all owned public instruction cases",
    "Test regular workspace cases",
    "Documentation tests",
    "Clippy",
    "Rustfmt",
)
V4_TEST_GATES = (
    "Test all owned public instruction cases",
    "Test regular workspace cases",
    "Documentation tests",
)
V4_OWNED_PUBLIC_GATE = "Test all owned public instruction cases"
V4_OWNED_PUBLIC_POPULATION = (17, 0)

V5_GATES = (
    "Cross-client skill discovery",
    "Build workspace",
    "DBT virtual identity and pidfd_open policy",
    "Backend maturity",
    "Test regular workspace cases",
    "Documentation tests",
    "Clippy",
    "Rustfmt",
)
V5_TEST_GATES = V3_TEST_GATES

# Preserve the v4 defaults for callers constructing a source row. Production
# selects the contract from the exact ordered source gates below.
EXPECTED_GATES = V4_GATES
TEST_GATES = V4_TEST_GATES
GATE_CONTRACTS = (
    ("reverie-local-validation/v5", V5_GATES, V5_TEST_GATES),
    ("reverie-local-validation/v4", V4_GATES, V4_TEST_GATES),
    ("reverie-local-validation/v3", V3_GATES, V3_TEST_GATES),
)
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
SAFE_CI_STEP = "validate.reverie-full"
I64_MAX = (1 << 63) - 1
TEST_COUNT_FIELDS = ("executed_tests", "passed_tests", "filtered_tests")
COUNT_SCHEMA = "reverie-count-handoff/v1"
COUNT_EVIDENCE_SCHEMA = "reverie-count-evidence/v1"
COUNT_HANDLE_ENV = "CI_HUB_REVERIE_COUNT_HANDLE"
COUNT_SOURCE_PATHS = (
    "ci-hub/validate/start_unit.py", "ci-hub/validate/run_registry.py",
    "ci-hub/validate/reverie_node.py", "ci-hub/ledger/validate_rows.py",
)
COUNT_CONTEXT_FIELDS = (
    "unit", "repo", "target", "checkout", "log", "started_at",
    "parent_checkout_head", "process_identity", "reverie_count_request",
)
COUNT_MAX_BYTES = 1024 * 1024


class Refusal(RuntimeError):
    pass


def count_json(value: Any) -> bytes:
    """Documented canonical JSON encoding, not an original ledger-line hash."""
    return (json.dumps(value, sort_keys=True, separators=(",", ":"),
                       ensure_ascii=True, allow_nan=False) + "\n").encode()


def strict_count_json(raw: str | bytes) -> dict[str, Any]:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in items:
            if key in value:
                raise Refusal(f"duplicate count-evidence JSON key {key!r}")
            value[key] = item
        return value

    def invalid_constant(value: str) -> None:
        raise Refusal(f"invalid count-evidence JSON constant {value}")

    value = json.loads(raw, object_pairs_hook=pairs, parse_constant=invalid_constant)
    if not isinstance(value, dict):
        raise Refusal("count evidence must be a JSON object")
    return value


def count_source_hashes(root: Path) -> dict[str, str]:
    return {name: hashlib.sha256((root / name).read_bytes()).hexdigest()
            for name in COUNT_SOURCE_PATHS}


def validate_count_request(value: object) -> None:
    if (not isinstance(value, dict) or set(value) != {"schema", "nonce", "producer_files"}
            or value.get("schema") != COUNT_SCHEMA
            or not isinstance(value.get("nonce"), str)
            or re.fullmatch(r"[0-9a-f]{32}", value["nonce"]) is None):
        raise Refusal("invalid Reverie count request")
    files = value["producer_files"]
    if (not isinstance(files, dict) or set(files) != set(COUNT_SOURCE_PATHS)
            or any(not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None
                   for digest in files.values())):
        raise Refusal("count request does not bind all producer sources")


def count_sidecar_path(record_path: Path) -> Path:
    return record_path.with_name(record_path.name + ".reverie-count.json")


def count_context(record: dict[str, Any], record_path: Path) -> dict[str, Any]:
    if record.get("kind") != "reverie-validate" or record.get("repo") != "rrnewton/reverie":
        raise Refusal("count handoff requires a Reverie validation handle")
    validate_count_request(record.get("reverie_count_request"))
    if not SHA_RE.fullmatch(str(record.get("parent_checkout_head", ""))):
        raise Refusal("count handoff has no exact producer source commit")
    identity = record.get("process_identity")
    if (not isinstance(identity, dict) or set(identity) != {"pid", "start_ticks", "boot_id"}
            or any(type(identity.get(key)) is not int or identity[key] <= 0
                   for key in ("pid", "start_ticks"))
            or not isinstance(identity.get("boot_id"), str) or not identity["boot_id"]):
        raise Refusal("count handoff has no validation process generation")
    return {"record_path": str(record_path.absolute()),
            **{key: record[key] for key in COUNT_CONTEXT_FIELDS}}


def _file_generation(value: os.stat_result) -> tuple[int, ...]:
    return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)


def read_count_sidecar(path: Path) -> tuple[dict[str, Any], str, tuple[int, ...]]:
    """Read a bounded regular file through one descriptor; refuse named movement."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size > COUNT_MAX_BYTES:
            raise Refusal("count sidecar is not a bounded regular file")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            raw = stream.read(COUNT_MAX_BYTES + 1)
        generation = _file_generation(before)
        if (len(raw) > COUNT_MAX_BYTES or generation != _file_generation(os.fstat(descriptor))
                or generation != _file_generation(path.lstat()) or not raw.endswith(b"\n")):
            raise Refusal("count sidecar moved or is truncated")
        return strict_count_json(raw), hashlib.sha256(raw).hexdigest(), generation
    finally:
        os.close(descriptor)


def publish_count_sidecar(path: Path, value: dict[str, Any]) -> None:
    """Publish once without replacement. A retry must have identical bytes."""
    raw = count_json(value)
    if len(raw) > COUNT_MAX_BYTES:
        raise Refusal("count sidecar exceeds its byte bound")
    with tempfile.NamedTemporaryFile(prefix=f".{path.name}.", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    try:
        try:
            os.link(temporary, path)
        except FileExistsError:
            _value, digest, _generation = read_count_sidecar(path)
            if digest != hashlib.sha256(raw).hexdigest():
                raise Refusal("a different count sidecar already exists")
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink()


def read_finalization(
    root: Path, source_id: str, final_id: str, run_id: str, *, state_root: Path | None = None
) -> dict[str, Any]:
    result = run(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "reverie-finalization",
         "--source-id", source_id, "--final-id", final_id, "--run-id", run_id],
        cwd=state_root or root,
        env={**os.environ, **({"DEV_HERMIT_PARENT": str(state_root)} if state_root else {})},
    )
    if result.returncode:
        raise Refusal("canonical finalization unavailable: " +
                      (result.stderr.strip() or result.stdout.strip() or str(result.returncode)))
    value = strict_count_json(result.stdout)
    if set(value) != {"source", "final", "row"} or not all(isinstance(item, dict) for item in value.values()):
        raise Refusal("canonical finalization has no exact event/row binding")
    return value


def validate_count_binding(
    value: object, record: dict[str, Any], record_path: Path
) -> tuple[int, int, int]:
    """Validate transport shape and framework populations; no ledger authority here."""
    if (not isinstance(value, dict)
            or set(value) != {"schema", "context", "invocation_id", "source_id", "final_id",
                              "run_id", "canonical", "canonical_sha256"}
            or value.get("schema") != COUNT_SCHEMA
            or value.get("context") != count_context(record, record_path)
            or not isinstance(value.get("invocation_id"), str)
            or re.fullmatch(r"[0-9a-f]{32}", value["invocation_id"]) is None):
        raise Refusal("count sidecar does not bind this handle/invocation/source")
    canonical = value["canonical"]
    if (not isinstance(canonical, dict) or set(canonical) != {"source", "final", "row"}
            or not all(isinstance(item, dict) for item in canonical.values())
            or hashlib.sha256(count_json(canonical)).hexdigest() != value["canonical_sha256"]):
        raise Refusal("count canonical-encoding digest disagrees")
    source, final, row = (canonical[key] for key in ("source", "final", "row"))
    if (any(not isinstance(value.get(key), str) or not value[key] for key in ("source_id", "final_id", "run_id"))
            or source.get("event_id") != value["source_id"] or final.get("event_id") != value["final_id"]
            or source.get("run_id") != value["run_id"] or final.get("run_id") != value["run_id"]
            or final.get("supersedes") != value["source_id"]
            or source.get("event_type") != "run.result" or final.get("event_type") != "run.correct"
            or row.get("producer") != "ci-hub-reverie-validate-run" or row.get("schema_version") != 5
            or row.get("repo") != record["repo"] or row.get("commit") != record["target"]
            or row.get("cwd") != record["checkout"] or row.get("log_file") != record["log"]
            or not SHA_RE.fullmatch(str(row.get("tree", "")))):
        raise Refusal("count source/final event or measured checkout does not match")
    if any(not isinstance(item, str) for item in
           (record.get("started_at"), row.get("started_at"), row.get("finished_at"))):
        raise Refusal("count start/finish boundaries must be timestamp strings")
    authority = row.get("validate_lock_authority")
    if not isinstance(authority, dict) or not isinstance(authority.get("holder"), dict):
        raise Refusal("count validation authority must contain a holder object")
    holder = authority["holder"]
    boundary = datetime.fromisoformat(record["started_at"].replace("Z", "+00:00")).replace(microsecond=0)
    started = datetime.fromisoformat(row["started_at"].replace("Z", "+00:00"))
    finished = datetime.fromisoformat(row["finished_at"].replace("Z", "+00:00"))
    if any(item.tzinfo is None for item in (boundary, started, finished)) or not boundary <= started <= finished:
        raise Refusal("count row does not bind this run's start boundary")
    if any(holder.get(key) != record.get(key) for key in ("unit", "target", "started_at", "process_identity")):
        raise Refusal("count row's validation authority belongs to a different handle")
    policy, expected, counted = require_expected_gate_set(row.get("gates"))
    totals = recorded_test_counts(row)
    executed, passed, filtered, zero, absent = recorded_gate_coverage(row, counted)
    if (totals != (executed, passed, filtered) or row.get("reverie_validation_policy") != policy
            or row.get("gates_expected") != len(expected)
            or (policy in {"reverie-local-validation/v4", "reverie-local-validation/v5"} and (zero or absent))):
        raise Refusal("count selection policy or aggregate differs from the counted gates")
    return totals


def produce_count_handoff(
    root: Path, row: dict[str, Any], source: dict[str, Any], receipt: str
) -> None:
    raw_handle = os.environ.get(COUNT_HANDLE_ENV)
    if raw_handle is None:
        return  # Older/standalone producers retain their existing null-count contract.
    import run_registry

    record_path = Path(raw_handle)
    if not record_path.is_absolute():
        raise Refusal("count handle path is not absolute")
    handle = run_registry.read_record(record_path)
    context = count_context(handle, record_path)
    if context["reverie_count_request"]["producer_files"] != count_source_hashes(root):
        raise Refusal("executing count producer differs from the launch source")
    invocation = os.environ.get("INVOCATION_ID", "")
    cgroup = Path("/proc/self/cgroup").read_text()
    if not any(handle["unit"] in line.split(":", 2)[-1].split("/") for line in cgroup.splitlines()):
        raise Refusal("count producer is outside the named service")
    ack = strict_count_json(receipt)
    if (ack.get("schema") != "ci-hub-validate-ledger-record/v1"
            or ack.get("run_id") != source.get("run_id")
            or not isinstance(ack.get("event_id"), str) or not ack["event_id"]):
        raise Refusal("ledger writer did not acknowledge an exact final event")
    canonical = read_finalization(root, source["event_id"], ack["event_id"], ack["run_id"])
    if count_json(canonical["source"]) != count_json(source) or count_json(canonical["row"]) != count_json(row):
        raise Refusal("acknowledged finalization differs from the producer's objects")
    value = {"schema": COUNT_SCHEMA, "context": context, "invocation_id": invocation,
             "source_id": source["event_id"], "final_id": ack["event_id"], "run_id": ack["run_id"],
             "canonical": canonical, "canonical_sha256": hashlib.sha256(count_json(canonical)).hexdigest()}
    validate_count_binding(value, handle, record_path)
    publish_count_sidecar(count_sidecar_path(record_path), value)


def consume_count_handoff(
    root: Path, state_root: Path, record_path: Path, record: dict[str, Any]
) -> dict[str, Any]:
    """Authenticate the locator against one current strict canonical snapshot."""
    path = count_sidecar_path(record_path)
    value, digest, generation = read_count_sidecar(path)
    validate_count_binding(value, record, record_path)
    canonical = read_finalization(root, value["source_id"], value["final_id"], value["run_id"], state_root=state_root)
    if count_json(canonical) != count_json(value["canonical"]):
        raise Refusal("canonical finalization changed after the sidecar was published")
    _again, after_digest, after_generation = read_count_sidecar(path)
    if after_digest != digest or after_generation != generation:
        raise Refusal("count sidecar moved during canonical validation")
    return {"schema": COUNT_EVIDENCE_SCHEMA, "sidecar_sha256": digest, "binding": value}


def validate_count_evidence(
    value: object, record: dict[str, Any], record_path: Path
) -> tuple[int, int, int]:
    if (not isinstance(value, dict) or set(value) != {"schema", "sidecar_sha256", "binding"}
            or value.get("schema") != COUNT_EVIDENCE_SCHEMA
            or value.get("sidecar_sha256") != hashlib.sha256(count_json(value.get("binding"))).hexdigest()):
        raise Refusal("count evidence does not bind the published sidecar bytes")
    return validate_count_binding(value["binding"], record, record_path)


def require_kvm_environment() -> None:
    if os.environ.get("REVERIE_REQUIRE_KVM") != "1":
        raise Refusal("REVERIE_REQUIRE_KVM=1 is required; KVM tests must not silently skip")


def run(argv: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return bounded_capture.capture(argv, cwd=cwd, env=env)


def git(checkout: Path, *args: str) -> str:
    result = run(["git", "-C", str(checkout), *args], cwd=checkout)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise Refusal(f"git {' '.join(args)}: {detail}")
    return result.stdout.strip()


def _pid_scope(component: str, prefix: str) -> bool:
    if not component.startswith(prefix) or not component.endswith(".scope"):
        return False
    pid = component[len(prefix) : -len(".scope")]
    return bool(pid) and pid.isascii() and pid.isdigit()


def is_safe_ci_step_cgroup(path: str, step: str) -> bool:
    if not path.startswith("/"):
        return False
    components = path[1:].split("/")
    if not components or any(not component for component in components):
        return False
    step_component = f"step-{step}"
    if components[-1] != step_component:
        return False
    legacy = (
        len(components) >= 4
        and components[-4] == "safe.slice"
        and components[-3] == "safe-ci.slice"
        and _pid_scope(components[-2], "safe-ci-")
    )
    current = (
        len(components) >= 3
        and components[-3] == "dagrun.slice"
        and _pid_scope(components[-2], "dagrun-")
    )
    return legacy or current


def safe_ci_cgroup() -> str:
    try:
        lines = Path("/proc/self/cgroup").read_text().splitlines()
    except OSError as error:
        raise Refusal(f"cannot inspect safe-ci cgroup: {error}") from error
    paths = [line.split(":", 2)[-1] for line in lines if line]
    matches = [
        path
        for path in paths
        if is_safe_ci_step_cgroup(path, SAFE_CI_STEP)
    ]
    if len(matches) != 1:
        raise Refusal(
            "process is not in exactly one safe-ci per-step cgroup; "
            f"observed={paths!r}"
        )
    return matches[0]


def lock_authority(root: Path, target: str) -> dict[str, Any]:
    result = run(
        [str(root / "ci-hub/ci-hub"), "validate-lock", "authority-status", "--json"],
        cwd=root,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise Refusal(f"validate-lock authority unavailable: {detail}")
    try:
        authority = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise Refusal(f"validate-lock authority returned malformed JSON: {error}") from error
    owner_pid = os.environ.get("CI_HUB_VALIDATE_LOCK_OWNER_PID", "")
    owner_file = os.environ.get("CI_HUB_VALIDATE_LOCK_OWNER_FILE", "")
    if not owner_pid.isdigit() or int(owner_pid) <= 1 or not owner_file:
        raise Refusal("validate-lock owner ancestry environment is absent")
    candidates = authority.get("authorities")
    if not isinstance(candidates, list):
        candidates = [authority]
    matches = []
    for candidate in candidates:
        if not isinstance(candidate, dict):
            continue
        holder = candidate.get("holder")
        owner = candidate.get("owner")
        if (
            candidate.get("schema_version") == 1
            and candidate.get("admissible") is True
            and isinstance(holder, dict)
            and holder.get("kind") == "reverie-validate"
            and holder.get("target") == target
            and isinstance(owner, dict)
            and owner.get("liveness") == "alive"
            and owner.get("pid") == int(owner_pid)
            and candidate.get("canonical_anchor_held") is True
        ):
            matches.append(candidate)
    if len(matches) != 1:
        raise Refusal(f"validate-lock authority does not bind this run: {authority!r}")
    authority = matches[0]
    try:
        observed_owner = Path(owner_file).read_text()
    except OSError as error:
        raise Refusal(f"validate-lock owner sidecar is unreadable: {error}") from error
    if f"pid={owner_pid}\n" not in observed_owner:
        raise Refusal("validate-lock owner sidecar does not match the inherited owner PID")

    # Prove the inherited owner is an ancestor of this safe-ci node. Merely
    # observing a live lock elsewhere on the box cannot authorize this process.
    wanted = int(owner_pid)
    cursor = os.getpid()
    seen: set[int] = set()
    while cursor > 1 and cursor not in seen:
        if cursor == wanted:
            return authority
        seen.add(cursor)
        try:
            stat = Path(f"/proc/{cursor}/stat").read_text()
        except OSError as error:
            raise Refusal(f"cannot prove validate-lock owner ancestry: {error}") from error
        close = stat.rfind(")")
        if close < 0:
            raise Refusal("cannot parse process ancestry")
        fields = stat[close + 2 :].split()
        if len(fields) < 2 or not fields[1].isdigit():
            raise Refusal("cannot parse parent PID from process ancestry")
        cursor = int(fields[1])
    raise Refusal(
        f"safe-ci node pid {os.getpid()} is not descended from validate-lock owner {wanted}"
    )


def load_source_row(root: Path, target: str, checkout: Path, log: Path, started: datetime) -> tuple[dict[str, Any], dict[str, Any]]:
    result = run(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "reverie-source",
         "--target", target, "--checkout", str(checkout), "--log", str(log),
         "--started", started.isoformat()], cwd=root
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise Refusal(f"canonical ledger union unavailable: {detail}")
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise Refusal("canonical ledger source reply is not JSON") from error
    if (not isinstance(value, dict) or set(value) != {"row", "event"}
            or not isinstance(value["row"], dict) or not isinstance(value["event"], dict)):
        raise Refusal("canonical ledger source reply has no exact row/event binding")
    return value["row"], value["event"]


def recorded_gate_coverage(
    source: dict[str, Any], test_gates: tuple[str, ...] = TEST_GATES
) -> tuple[int, int, int, list[str], list[str]]:
    """Read the counts each test gate wrote at the gate boundary."""
    gates = source.get("gates")
    if not isinstance(gates, list) or not all(isinstance(gate, dict) for gate in gates):
        raise Refusal("Reverie source row gates must be a list of objects")
    by_name = {gate.get("name"): gate for gate in gates}
    absent = [gate for gate in test_gates if gate not in by_name]
    zero: list[str] = []
    executed = 0
    passed = 0
    filtered = 0
    for name in test_gates:
        gate = by_name.get(name)
        if gate is None:
            continue
        values = _recorded_test_counts(
            gate, context=f"Reverie source row gate {name!r}"
        )
        # A failed gate still measured this exact selected population. Its
        # authenticated passed count may be smaller; _recorded_test_counts
        # separately requires every executed test to pass on a passing gate.
        if name == V4_OWNED_PUBLIC_GATE and (
            values[0], values[2]
        ) != V4_OWNED_PUBLIC_POPULATION:
            raise Refusal(
                f"Reverie source row gate {name!r} must record exactly "
                f"executed_tests={V4_OWNED_PUBLIC_POPULATION[0]} and "
                f"filtered_tests={V4_OWNED_PUBLIC_POPULATION[1]}, got "
                f"executed_tests={values[0]}, passed_tests={values[1]}, "
                f"and filtered_tests={values[2]}"
            )
        executed, passed, filtered = _checked_count_totals(
            (executed, passed, filtered),
            values,
            context="Reverie source row counted gates",
        )
        if values[0] == 0:
            zero.append(name)
    return executed, passed, filtered, zero, absent


def _recorded_test_counts(
    record: dict[str, Any], *, context: str
) -> tuple[int, int, int]:
    values: list[int] = []
    for field in TEST_COUNT_FIELDS:
        value = record.get(field)
        if (
            not isinstance(value, int)
            or isinstance(value, bool)
            or value < 0
            or value > I64_MAX
        ):
            raise Refusal(
                f"{context} {field} must be a nonnegative signed 64-bit integer, "
                f"got {value!r}"
            )
        values.append(value)
    executed, passed, filtered = values
    if passed > executed:
        raise Refusal(
            f"{context} passed_tests must not exceed executed_tests, "
            f"got {passed} > {executed}"
        )
    if record.get("result") == "pass" and passed != executed:
        raise Refusal(
            f"passing {context} requires passed_tests == executed_tests, "
            f"got {passed} != {executed}"
        )
    return executed, passed, filtered


def _checked_count_totals(
    totals: tuple[int, int, int],
    values: tuple[int, int, int],
    *,
    context: str,
) -> tuple[int, int, int]:
    """Add already-validated counts without exceeding the ledger's i64 type."""
    overflow = [
        field
        for field, total, value in zip(TEST_COUNT_FIELDS, totals, values, strict=True)
        if total > I64_MAX - value
    ]
    if overflow:
        raise Refusal(
            f"{context} aggregate exceeds signed 64-bit maximum for "
            + ", ".join(overflow)
        )
    return (
        totals[0] + values[0],
        totals[1] + values[1],
        totals[2] + values[2],
    )


def recorded_test_counts(source: dict[str, Any]) -> tuple[int, int, int]:
    """Read the aggregate counts written by Reverie's validation framework."""
    return _recorded_test_counts(source, context="Reverie source row")


def require_expected_gate_set(
    gates: object,
) -> tuple[str, tuple[str, ...], tuple[str, ...]]:
    gate_names = (
        [gate.get("name") for gate in gates]
        if isinstance(gates, list) and all(isinstance(gate, dict) for gate in gates)
        else []
    )
    for policy, expected, test_gates in GATE_CONTRACTS:
        if gate_names == list(expected):
            return policy, expected, test_gates
    raise Refusal(f"Reverie gate set drifted: {gate_names!r}")


def record(root: Path, row: dict[str, Any], source: dict[str, Any]) -> str:
    result = bounded_capture.capture(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "record-reverie-finalization"],
        input=json.dumps({"source": source, "row": row}, sort_keys=True) + "\n",
        cwd=root,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise Refusal(f"canonical ledger writer refused Reverie row: {detail}")
    return result.stdout


def write_final_status(path: Path, status: str) -> None:
    """Publish one complete status without leaving a partial file on interruption."""
    if status not in FINAL_VALIDATE_STATUS_EXIT_CODES:
        raise Refusal(f"unknown final validate status: {status!r}")
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", prefix=f".{path.name}.", dir=path.parent, delete=False
    ) as handle:
        temporary = Path(handle.name)
        handle.write(f"{FINAL_VALIDATE_STATUS_PREFIX}{status}\n")
    try:
        os.replace(temporary, path)
    except OSError:
        temporary.unlink(missing_ok=True)
        raise


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--checkout", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--log", type=Path, required=True)
    parser.add_argument("--status-file", type=Path, required=True)
    parser.add_argument("--pr", type=int)
    args = parser.parse_args(argv)
    root = args.root.resolve()
    checkout = args.checkout.resolve()
    log = args.log.resolve()
    status_file = args.status_file.resolve()
    # validate.sh records whole seconds. Floor this boundary to the same
    # precision so a row started later in this second cannot compare earlier
    # merely because the producer omitted microseconds.
    started = datetime.now(timezone.utc).replace(microsecond=0)
    try:
        if not SHA_RE.fullmatch(args.target):
            raise Refusal("target must be an exact lowercase 40-hex SHA")
        if git(checkout, "rev-parse", "HEAD^{commit}") != args.target:
            raise Refusal("checkout HEAD does not match target")
        if git(checkout, "status", "--porcelain=v1"):
            raise Refusal("checkout is dirty")
        require_kvm_environment()
        cgroup = safe_ci_cgroup()
        authority = lock_authority(root, args.target)
        tree = git(checkout, "rev-parse", "HEAD^{tree}")
        env = os.environ.copy()
        env["VALIDATE_LOG_FILE"] = str(log)
        env["VALIDATE_LABEL_PR"] = "0"
        if args.pr is not None:
            env["PR_NUMBER"] = str(args.pr)
        result = subprocess.run(
            [str(checkout / "validate.sh"), "--no-label-pr"],
            cwd=checkout,
            env=env,
            text=True,
            check=False,
        )
        source, source_event = load_source_row(root, args.target, checkout, log, started)
        gates = source.get("gates")
        policy, expected_gates, test_gates = require_expected_gate_set(gates)
        executed, passed, filtered = recorded_test_counts(source)
        gate_executed, gate_passed, gate_filtered, zero, absent = (
            recorded_gate_coverage(source, test_gates)
        )
        if (gate_executed, gate_passed, gate_filtered) != (
            executed,
            passed,
            filtered,
        ):
            raise Refusal(
                "Reverie aggregate test counts disagree with per-gate records: "
                f"aggregate={(executed, passed, filtered)!r} "
                f"per_gate={(gate_executed, gate_passed, gate_filtered)!r}"
            )
        if policy == "reverie-local-validation/v4" and (zero or absent):
            raise Refusal(
                "Reverie v4 requires all three counted test gates to execute: "
                f"zero={zero!r} absent={absent!r}"
            )
        if policy == "reverie-local-validation/v5" and (zero or absent):
            raise Refusal(
                "Reverie v5 requires both counted test gates to execute: "
                f"zero={zero!r} absent={absent!r}"
            )
        canonical = dict(source)
        canonical.update(
            {
                "schema_version": 5,
                "producer": "ci-hub-reverie-validate-run",
                "repo": "rrnewton/reverie",
                "tree": tree,
                "raw_result": source.get("result"),
                "gates_run": source.get("checks"),
                "gates_expected": len(expected_gates),
                "executed_tests": executed,
                "passed_tests": passed,
                "filtered_tests": filtered,
                "coverage": {
                    "planned_test_nodes": len(test_gates),
                    "executed_test_nodes": len(test_gates) - len(zero) - len(absent),
                    "zero_executed_nodes": zero,
                    "absent_nodes": absent,
                },
                "admission": "ci-hub-validate-lock",
                "concurrent_validates": 0,
                "concurrency_proof": "validate_lock_owner_ancestry",
                "safe_ci": {
                    "required": True,
                    "observed": True,
                    "cgroup": cgroup,
                    "step": SAFE_CI_STEP,
                    "fallback_allowed": False,
                },
                "validate_lock_authority": authority,
                "reverie_validation_policy": policy,
            }
        )
        # The canonical row records even a product red. Shape failures are not
        # repaired here; the reader will refuse them rather than minting green.
        receipt = record(root, canonical, source_event)
        if result.returncode == 0:
            status = "PASSED"
        elif result.returncode == 1:
            status = "FAILED"
        else:
            status = "COULD_NOT_RUN"
        write_final_status(status_file, status)
        try:
            produce_count_handoff(root, canonical, source_event, receipt)
        except (OSError, RuntimeError, ValueError, TypeError, KeyError) as error:
            # The ledger append and payload verdict already exist. Count
            # transport is separate bookkeeping and must not replace either.
            print(f"reverie-safe-validate: COUNT-HANDOFF-REFUSED: {error}", file=sys.stderr)
        return FINAL_VALIDATE_STATUS_EXIT_CODES[status]
    except (OSError, Refusal) as error:
        print(f"reverie-safe-validate: REFUSED: {error}", file=sys.stderr)
        try:
            write_final_status(status_file, "COULD_NOT_RUN")
        except OSError as status_error:
            print(
                f"reverie-safe-validate: could not publish final status: {status_error}",
                file=sys.stderr,
            )
        return FINAL_VALIDATE_STATUS_EXIT_CODES["COULD_NOT_RUN"]


if __name__ == "__main__":
    raise SystemExit(main())
