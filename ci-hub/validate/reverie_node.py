#!/usr/bin/env python3
"""Run Reverie's full validator inside one proven safe-ci step and record it."""

from __future__ import annotations

import argparse
import json
import os
import re
import socket
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


class Refusal(RuntimeError):
    pass


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


def load_source_row(root: Path, target: str, checkout: Path, log: Path, started: datetime) -> dict[str, Any]:
    result = run(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"], cwd=root
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise Refusal(f"canonical ledger union unavailable: {detail}")
    candidates: list[dict[str, Any]] = []
    for line in result.stdout.splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(row, dict):
            continue
        if row.get("repo") not in ("reverie", "rrnewton/reverie"):
            continue
        if row.get("commit") != target or row.get("cwd") != str(checkout):
            continue
        if row.get("log_file") != str(log):
            continue
        try:
            row_started = datetime.fromisoformat(str(row["started_at"]).replace("Z", "+00:00"))
        except (KeyError, TypeError, ValueError):
            continue
        if row_started >= started:
            candidates.append(row)
    if len(candidates) != 1:
        raise Refusal(
            "expected exactly one Reverie source row for this safe-ci node, "
            f"found {len(candidates)}"
        )
    return candidates[0]


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


def record(root: Path, row: dict[str, Any]) -> None:
    result = bounded_capture.capture(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "record"],
        input=json.dumps(row, sort_keys=True) + "\n",
        cwd=root,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise Refusal(f"canonical ledger writer refused Reverie row: {detail}")


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
        source = load_source_row(root, args.target, checkout, log, started)
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
        record(root, canonical)
        if result.returncode == 0:
            status = "PASSED"
        elif result.returncode == 1:
            status = "FAILED"
        else:
            status = "COULD_NOT_RUN"
        write_final_status(status_file, status)
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
