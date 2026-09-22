#!/usr/bin/env python3
"""Authenticate and admit one completed HTTPCoro time-input invocation."""

from __future__ import annotations

import argparse
import copy
import datetime as dt
import hashlib
import json
import re
import shutil
import signal as signal_module
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from create_httpcoro_time_plan import (
    CAMPAIGN_CWD,
    HERMIT_BINARY,
    HERMIT_REVISION,
    HERMIT_SHA256,
    SCRATCH_ROOT,
    TEST_BINARY,
    TEST_BYTES,
    TEST_DISPLAY,
    TEST_SHA256,
    VERIFY_DIR,
)
from run_httpcoro_time_seed import (
    QGROUP_LIMIT,
    RECORD,
    RECORD_REL,
    REPO_ROOT,
    ROOT,
    SAFEHERMIT_LOG_ROOT,
    Refusal,
    assert_preflight,
    evidence_report_locator,
    materialize_argv,
    next_seed,
    parse_qgroup_report,
    qgroup_report,
    report_fields,
)


FAILURE_CLASS = "reset-rate-limit-line1685-body-event-fatal"
FATAL_TEXT = (
    "HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: "
    "bodyEvent.hasException()"
)
SELECTED_STACK = "H2DownstreamSessionTest_ResetRateLimit_Test"
STRICT_COMPARISON = ":: comparison=BitwiseInfoV1 relaxations=none"
STRICT_SUCCESS = ":: Success: deterministic. Determinism verified."
ONE_TEST = "[==========] Running 1 test from 1 test suite."
PASSED_ONE = "[  PASSED  ] 1 test."
EVIDENCE_LIMIT = 67_108_864


@dataclass(frozen=True)
class Classification:
    status: str
    target_outcome: str | None
    failure_class_id: str | None
    harness_status: str
    selected_count: int
    censor_reason: str | None
    strict_match: bool


def canonical_digest(value: dict[str, Any], key: str) -> str:
    canonical = dict(value)
    canonical.pop(key, None)
    payload = json.dumps(
        canonical, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode()
    return hashlib.sha256(payload).hexdigest()


def compact_json(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        + "\n"
    ).encode()


def pretty_json(value: object) -> bytes:
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode()


def write_new(path: Path, payload: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        with path.open("xb") as handle:
            handle.write(payload)
    except FileExistsError as error:
        raise Refusal(f"refusing to overwrite durable evidence: {path}") from error


def copy_new(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        raise Refusal(f"refusing to overwrite durable evidence: {destination}")
    try:
        with destination.open("xb") as output, source.open("rb") as input_handle:
            shutil.copyfileobj(input_handle, output)
    except FileExistsError as error:
        raise Refusal(f"refusing to overwrite durable evidence: {destination}") from error


def artifact(
    path: Path,
    artifact_id: str,
    kind: str,
    media_type: str,
    capture_limit: int = EVIDENCE_LIMIT,
) -> dict[str, Any]:
    payload = path.read_bytes()
    if len(payload) > capture_limit:
        raise Refusal(f"artifact exceeds capture limit: {path}")
    return {
        "artifact_id": artifact_id,
        "kind": kind,
        "storage": "TRACKED",
        "locator": path.relative_to(ROOT).as_posix(),
        "sha256": hashlib.sha256(payload).hexdigest(),
        "bytes": len(payload),
        "capture_limit_bytes": capture_limit,
        "truncated": False,
        "media_type": media_type,
    }


def result_payload(event: dict[str, Any]) -> dict[str, Any]:
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


def matcher_payload(matcher: dict[str, Any]) -> dict[str, Any]:
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


def parse_int(fields: dict[str, str], key: str) -> int:
    try:
        value = int(fields[key])
    except (KeyError, ValueError) as error:
        raise Refusal(f"safehermit report field {key!r} is not an integer") from error
    if value < 0:
        raise Refusal(f"safehermit report field {key!r} is negative")
    return value


def authenticate_safehermit(
    report_path: Path,
    stderr_path: Path,
    process_returncode: int,
) -> dict[str, Any]:
    fields = report_fields(report_path)
    required = {
        "run_id",
        "binary",
        "binary_source",
        "binary_sha256",
        "log_dir",
        "log_file",
        "unit",
        "bound.wall",
        "bound.cgroup",
        "bound.disk",
        "bound.bytes",
        "bound.logfilter",
        "env.forwarded",
        "note.run_record",
        "elapsed_secs",
        "bytes_written",
        "truncated",
        "runner_exit_code",
        "unit_result",
        "exit_code",
    }
    missing = sorted(required - fields.keys())
    if missing:
        raise Refusal(f"safehermit report lacks required fields: {missing!r}")
    exact = {
        "binary": HERMIT_BINARY,
        "binary_source": "argv:positional-path",
        "binary_sha256": HERMIT_SHA256,
        "bound.wall": "APPLIED:120s",
        "bound.cgroup": "APPLIED:MemoryMax=16G MemorySwapMax=0",
        "bound.disk": (
            "APPLIED:8G for log_dir only; guest paths and explicit log files "
            "elsewhere are outside this bound"
        ),
        "bound.bytes": (
            "APPLIED:67108864 (LETHAL: the run is cgroup-killed at the cap)"
        ),
        "bound.logfilter": (
            "NOT_APPLIED:by owner ruling -- the emitter is left loud so a stuck "
            "run stays visible; the low byte cap is the mechanism that fires"
        ),
        "truncated": "false",
    }
    drift = {
        key: (fields.get(key), value)
        for key, value in exact.items()
        if fields.get(key) != value
    }
    if drift:
        raise Refusal(f"safehermit boundary drifted: {drift!r}")
    if not fields["env.forwarded"].startswith("APPLIED:"):
        raise Refusal("safehermit did not forward the launch environment")
    if fields["unit"] == "none" or not fields["unit"].startswith("safehermit-"):
        raise Refusal("safehermit has no bounded transient unit")
    log_dir = Path(fields["log_dir"])
    if log_dir.parent != SAFEHERMIT_LOG_ROOT or not log_dir.name.startswith(
        "brs-safehermit-"
    ):
        raise Refusal("safehermit log directory escaped the sealed Btrfs root")
    log_file = Path(fields["log_file"])
    if log_file != log_dir / "hermit.log" or not log_file.is_file():
        raise Refusal("safehermit retained log is missing or misbound")
    stderr_payload = stderr_path.read_bytes()
    if log_file.read_bytes() != stderr_payload:
        raise Refusal("captured stderr differs from safehermit's retained capped log")
    elapsed = parse_int(fields, "elapsed_secs")
    bytes_written = parse_int(fields, "bytes_written")
    runner_exit = parse_int(fields, "runner_exit_code")
    exit_code = parse_int(fields, "exit_code")
    if bytes_written != len(stderr_payload) or bytes_written > EVIDENCE_LIMIT:
        raise Refusal("safehermit stderr byte accounting disagrees with captured stderr")
    if process_returncode != exit_code:
        raise Refusal("launcher return code disagrees with safehermit final exit code")
    unit_result = fields["unit_result"]
    process_signal: int | None = None
    if unit_result in {"signal", "core-dump"}:
        process_signal = parse_int(fields, "process_signal")
        if process_signal not in range(1, 65) or exit_code != 128 + process_signal:
            raise Refusal("safehermit signal identity is inconsistent")
        if runner_exit == 0:
            raise Refusal("safehermit runner reported success for a signalled process")
    elif unit_result == "success":
        if exit_code != 0 or runner_exit != 0 or "process_signal" in fields:
            raise Refusal("safehermit success termination fields are inconsistent")
    elif unit_result == "timeout":
        if exit_code != 124 or "process_signal" in fields:
            raise Refusal("safehermit timeout termination fields are inconsistent")
    elif unit_result == "exit-code":
        if exit_code == 0 or "process_signal" in fields:
            raise Refusal("safehermit exit-code termination fields are inconsistent")
    else:
        raise Refusal(f"unsupported safehermit unit_result {unit_result!r}")
    run_id = fields["run_id"].split("-", 1)[0]
    try:
        started = dt.datetime.strptime(run_id, "%Y%m%dT%H%M%SZ").replace(
            tzinfo=dt.timezone.utc
        )
    except ValueError as error:
        raise Refusal("safehermit run_id has no parseable UTC timestamp") from error
    finished = started + dt.timedelta(seconds=elapsed)
    return {
        "fields": fields,
        "started_at": started.isoformat().replace("+00:00", "Z"),
        "finished_at": finished.isoformat().replace("+00:00", "Z"),
        "exit_code": exit_code,
        "signal_number": process_signal,
        "timed_out": unit_result == "timeout",
        "unit_result": unit_result,
        "log_dir": log_dir,
    }


def strict_bitwise_matched(stderr: str) -> bool:
    return (
        stderr.count(STRICT_COMPARISON) == 1
        and stderr.count(STRICT_SUCCESS) == 1
        and ":: Failure:" not in stderr
        and "Determinism verification failed" not in stderr
    )


def actual_sanitizer_failure(stderr: str) -> bool:
    markers = (
        "ERROR: AddressSanitizer",
        "SUMMARY: AddressSanitizer",
        "WARNING: ThreadSanitizer:",
        "SUMMARY: ThreadSanitizer:",
        "UndefinedBehaviorSanitizer:",
        "SUMMARY: UndefinedBehaviorSanitizer",
        "runtime error:",
        "MemorySanitizer:",
    )
    return any(marker in stderr for marker in markers)


def infrastructure_failure(stdout: str, stderr: str) -> bool:
    combined = stdout + "\n" + stderr
    markers = (
        "HERMIT_INTERNAL_FAILURE",
        "Sandbox container exited unexpectedly",
        "class=container-child-exit",
        "safehermit: byte limit reached",
        "[safehermit] LOG CAP OF",
        "error while loading shared libraries",
        "symbol lookup error:",
        "Could not create container",
        "thread 'main' panicked at",
        "memory allocation of ",
        "Out of memory",
    )
    return any(marker in combined for marker in markers)


def classify(
    stdout: str,
    stderr: str,
    termination: dict[str, Any],
) -> Classification:
    lines = stdout.splitlines()
    run_line = f"[ RUN      ] {TEST_DISPLAY}"
    selected_count = lines.count(run_line)
    run_indices = [index for index, line in enumerate(lines) if line.startswith("[ RUN      ] ")]
    selected_index = lines.index(run_line) if selected_count == 1 else None
    ok_re = re.compile(rf"^\[       OK \] {re.escape(TEST_DISPLAY)} \([0-9]+ ms\)$")
    ok_indices = [
        index for index, line in enumerate(lines) if ok_re.fullmatch(line) is not None
    ]
    passed_indices = [index for index, line in enumerate(lines) if line == PASSED_ONE]
    one_test_indices = [index for index, line in enumerate(lines) if line == ONE_TEST]
    strict = strict_bitwise_matched(stderr)
    common = bool(
        strict
        and not actual_sanitizer_failure(stderr)
        and not infrastructure_failure(stdout, stderr)
        and len(run_indices) == 1
        and selected_index is not None
        and len(one_test_indices) == 1
        and one_test_indices[0] < selected_index
        and not termination["timed_out"]
    )
    pass_signature = bool(
        common
        and termination["unit_result"] == "success"
        and termination["exit_code"] == 0
        and termination["signal_number"] is None
        and len(ok_indices) == 1
        and len(passed_indices) == 1
        and selected_index is not None
        and selected_index < ok_indices[0] < passed_indices[0]
        and FATAL_TEXT not in stderr
    )
    fail_signature = bool(
        common
        and termination["unit_result"] in {"signal", "core-dump"}
        and termination["exit_code"] == 134
        and termination["signal_number"] == signal_module.SIGABRT
        and stderr.count(FATAL_TEXT) == 1
        and SELECTED_STACK in stderr
        and not ok_indices
        and not passed_indices
    )
    if pass_signature:
        return Classification("VALID", "PASS", None, "COMPLETE", 1, None, True)
    if fail_signature:
        return Classification(
            "VALID", "FAIL", FAILURE_CLASS, "COMPLETE", 1, None, True
        )
    if termination["timed_out"]:
        harness_status = "TIMEOUT"
        reason = "The outer 120-second safehermit deadline fired."
    else:
        harness_status = "ERROR"
        reason = (
            "The invocation did not satisfy the exact predeclared PASS or line-1685 "
            "SIGABRT target-owned FAIL predicate."
        )
    return Classification(
        "CENSORED",
        None,
        None,
        harness_status,
        selected_count,
        reason,
        strict,
    )


def build_cell(record: dict[str, Any]) -> dict[str, Any]:
    plan = record["plan"]
    route = plan["authorized_routes"][0]
    guest_argv = route["launch_argv"][route["launch_argv"].index("--") + 1 :]
    if CAMPAIGN_CWD != str(ROOT):
        raise Refusal("sealed campaign working directory differs from this campaign root")
    cell: dict[str, Any] = {
        "cell_id": route["authorized_cell_ids"][0],
        "route_id": route["route_id"],
        "route_sha256": route["route_sha256"],
        "plan_sha256": plan["plan_sha256"],
        "frozen_at": plan["sealed_at"],
        "cell_sha256": "",
        "canonical_target": record["identity"]["canonical_target"],
        "test_selector": copy.deepcopy(record["identity"]["test_selector"]),
        "build_mode_selector": copy.deepcopy(
            record["identity"]["build_mode_selector"]
        ),
        "target_source_revision": route["source_revision"],
        "hermit_source_revision": HERMIT_REVISION,
        "hermit_binary_sha256": HERMIT_SHA256,
        "test_artifact_sha256": TEST_SHA256,
        "guest": {
            "argv": copy.deepcopy(guest_argv),
            "environment": copy.deepcopy(route["environment"]),
            "mounts": [
                "type=bind,source=/home/newton/work/tfa/fbsource/buck-out,"
                "target=/home/newton/work/tfa/fbsource/buck-out"
            ],
            "immutable_inputs": [
                {
                    "artifact_id": "httpcoro-time-kernel71-test-elf",
                    "kind": "OTHER",
                    "storage": "LOCAL_IGNORED",
                    "locator": TEST_BINARY,
                    "sha256": TEST_SHA256,
                    "bytes": TEST_BYTES,
                    "capture_limit_bytes": TEST_BYTES,
                    "truncated": False,
                    "media_type": "application/x-executable",
                }
            ],
            "working_directory": str(ROOT),
        },
        "hermit": {
            "backend": "ptrace",
            "strict": True,
            "chaos": True,
            "relaxations": [],
            "argv": copy.deepcopy(route["launch_argv"]),
            "scheduler_heuristic": "random",
            "swept_coordinate": "--sched-seed",
            "fixed_seed_roots": copy.deepcopy(
                plan["measurement_design"]["fixed_seed_roots"]
            ),
        },
        "timing": {
            "requested_max_timeslice": 200_000_000,
            "effective_max_timeslice": 200_000_000,
        },
        "pmu": {
            "available": True,
            "effective_timer": "RCB",
            "fallback_recorded": False,
        },
        "host": {
            "hostname": "devbig030.atn3.facebook.com",
            "kernel": "Linux 7.1.3-0_fbk0_0_g295ad0959f34",
            "architecture": "x86_64",
            "cpu": "AMD EPYC 9D85 158-Core Processor",
            "capabilities": {
                "pmu": True,
                "mount_namespace": True,
                "buck_out_bind": True,
                "safehermit_disk_qgroup": True,
                "verifier_log_qgroup": True,
            },
        },
        "oracle_id": plan["target_oracle"]["oracle_id"],
        "resource_bounds": copy.deepcopy(plan["resource_bounds"]),
    }
    cell["cell_sha256"] = canonical_digest(cell, "cell_sha256")
    return cell


def git_result(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(REPO_ROOT), *args],
        check=False,
        capture_output=True,
        text=True,
    )


def assert_admission_boundary(
    record: dict[str, Any], manifest: dict[str, Any], report_path: Path
) -> None:
    """Allow only the actual report created after the clean pre-run check."""

    repository_record = RECORD.relative_to(REPO_ROOT).as_posix()
    committed = git_result("show", f"HEAD:{repository_record}")
    if committed.returncode != 0 or committed.stdout.encode() != RECORD.read_bytes():
        raise Refusal("experiment record differs from the committed pre-run HEAD")
    status = git_result("status", "--porcelain=v1", "-z", "--untracked-files=all")
    if status.returncode != 0:
        raise Refusal("could not authenticate the admission worktree boundary")
    report_repository_path = report_path.relative_to(REPO_ROOT).as_posix()
    expected_status = f"?? {report_repository_path}\0"
    if status.stdout != expected_status:
        raise Refusal(
            "worktree changed after clean preflight by more than the exact actual "
            f"safehermit report: {status.stdout!r}"
        )
    head = git_result("rev-parse", "HEAD")
    blob = git_result("rev-parse", f"HEAD:{repository_record}")
    if head.returncode != 0 or blob.returncode != 0:
        raise Refusal("could not re-read committed execution boundary")
    route = record["plan"]["authorized_routes"][0]
    expected = {
        "head_commit": head.stdout.strip(),
        "record_repository_path": repository_record,
        "record_blob": blob.stdout.strip(),
        "plan_sha256": record["plan"]["plan_sha256"],
        "route_id": route["route_id"],
        "route_sha256": route["route_sha256"],
        "plan_receipt": manifest["execution_boundary"].get("plan_receipt"),
    }
    if manifest.get("execution_boundary") != expected:
        raise Refusal("PREPARED invocation does not bind the exact committed HEAD")
    if canonical_digest(record["plan"], "plan_sha256") != record["plan"]["plan_sha256"]:
        raise Refusal("sealed plan_sha256 is not canonical")
    if canonical_digest(route, "route_sha256") != route["route_sha256"]:
        raise Refusal("sealed route_sha256 is not canonical")


def plan_receipt(record: dict[str, Any], requested_commit: str) -> dict[str, str]:
    resolved = git_result("rev-parse", f"{requested_commit}^{{commit}}")
    if resolved.returncode != 0:
        raise Refusal("plan commit is unavailable")
    commit = resolved.stdout.strip()
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise Refusal("plan commit did not resolve to a full SHA-1")
    ancestor = git_result("merge-base", "--is-ancestor", commit, "HEAD")
    if ancestor.returncode != 0:
        raise Refusal("plan commit is not an ancestor of the execution boundary")
    repository_path = RECORD.relative_to(REPO_ROOT).as_posix()
    prior = git_result("show", f"{commit}:{repository_path}")
    if prior.returncode != 0:
        raise Refusal("plan commit does not contain the experiment record")
    try:
        snapshot = json.loads(prior.stdout)
    except json.JSONDecodeError as error:
        raise Refusal("plan commit experiment record is not JSON") from error
    expected = copy.deepcopy(record)
    expected.update(
        plan_receipt=None,
        attempt_state="PLANNED",
        cells=[],
        events=[],
        samples=[],
        matchers=[],
        outcome_history=[],
        current_outcome=None,
    )
    if snapshot != expected:
        raise Refusal("plan commit is not the exact prospective PLANNED record")
    blob = git_result("rev-parse", f"{commit}:{repository_path}")
    if blob.returncode != 0 or not re.fullmatch(r"[0-9a-f]{40}\n?", blob.stdout):
        raise Refusal("could not bind the plan record blob")
    receipt = {
        "commit": commit,
        "path": RECORD_REL.as_posix(),
        "blob": blob.stdout.strip(),
    }
    existing = record["plan_receipt"]
    if existing is not None and existing != receipt:
        raise Refusal("material record's immutable plan receipt changed")
    return receipt


def validate_run_dir(
    run_dir: Path, record: dict[str, Any], expected_index: int, expected_seed: int
) -> tuple[dict[str, Any], dict[str, Path]]:
    scratch = Path(SCRATCH_ROOT).resolve()
    if run_dir.is_symlink() or not run_dir.is_dir() or run_dir.resolve().parent != scratch:
        raise Refusal("run directory is not a direct regular child of sealed scratch root")
    expected_name = (
        f"selection-{expected_index:03d}-sched-seed-{expected_seed:020d}"
    )
    if run_dir.name != expected_name:
        raise Refusal(f"run directory is not the next sealed tuple {expected_name!r}")
    manifest_path = run_dir / "invocation.json"
    manifest = json.loads(manifest_path.read_text())
    paths = {
        key: Path(value)
        for key, value in manifest.get("paths", {}).items()
    }
    report_locator = evidence_report_locator(expected_index, expected_seed)
    expected_paths = {
        "stdout": run_dir / "stdout.txt",
        "stderr": run_dir / "stderr.txt",
        "safehermit_report": ROOT / report_locator,
        "verifier_qgroup_before": run_dir / "verifier-qgroup-before.txt",
        "verifier_qgroup_after": run_dir / "verifier-qgroup-after.txt",
        "safehermit_qgroup_after": run_dir / "safehermit-qgroup-after.txt",
    }
    if manifest.get("state") != "COMPLETE" or manifest.get("postcheck_errors") != []:
        raise Refusal("runner did not reach an authenticated COMPLETE boundary")
    if manifest.get("schema_version") != 1:
        raise Refusal("unsupported runner manifest schema")
    if manifest.get("selection_index") != expected_index or manifest.get("seed") != {
        "flag": "--sched-seed",
        "value": expected_seed,
    }:
        raise Refusal("runner manifest does not name the next sealed tuple")
    if manifest.get("cwd") != str(ROOT):
        raise Refusal("runner working directory differs from the sealed campaign root")
    if manifest.get("host_environment") != {
        "SAFEHERMIT_LOG_ROOT": str(SAFEHERMIT_LOG_ROOT),
        "RUST_LOG": None,
        "HERMIT_BIN": None,
    }:
        raise Refusal("runner host environment controls drifted")
    if paths != expected_paths:
        raise Refusal("runner manifest artifact paths drifted")
    for key, path in [("manifest", manifest_path), *expected_paths.items()]:
        expected_parent = ROOT / RECORD_REL.parent / "evidence" if key == "safehermit_report" else run_dir
        if path.is_symlink() or not path.is_file() or path.resolve().parent != expected_parent.resolve():
            raise Refusal(f"raw runtime artifact is missing, unsafe, or escaped: {path}")
    if expected_paths["safehermit_report"] != ROOT / report_locator:
        raise Refusal("runner report is not the eventual replica-1 tracked locator")
    expected_argv = materialize_argv(
        record, expected_seed, report_locator
    )
    if manifest.get("argv") != expected_argv:
        raise Refusal("runner argv differs from the complete sealed argv")
    if expected_paths["stdout"].stat().st_size > EVIDENCE_LIMIT:
        raise Refusal("stdout exceeds the durable evidence capture limit")
    if expected_paths["stderr"].stat().st_size > EVIDENCE_LIMIT:
        raise Refusal("stderr exceeds the sealed safehermit byte limit")
    return manifest, expected_paths


def authenticate_qgroups(
    manifest: dict[str, Any], paths: dict[str, Path], termination: dict[str, Any]
) -> dict[str, Any]:
    before_text = paths["verifier_qgroup_before"].read_text()
    after_text = paths["verifier_qgroup_after"].read_text()
    safehermit_text = paths["safehermit_qgroup_after"].read_text()
    before = parse_qgroup_report(before_text)
    after = parse_qgroup_report(after_text)
    safehermit = parse_qgroup_report(safehermit_text)
    if after[0] < before[0] or after[1] < before[1]:
        raise Refusal("verifier qgroup accounting moved backwards across the run")
    if before[2] != after[2] or after[2] != QGROUP_LIMIT:
        raise Refusal("verifier qgroup limit changed across the run")
    recorded_before = manifest["preflight"]["qgroup"]
    recorded_after = manifest["verifier_qgroup_after"]
    recorded_safehermit = manifest["safehermit_qgroup_after"]
    if recorded_before.get("stdout") != before_text:
        raise Refusal("manifest verifier qgroup preflight disagrees with raw report")
    if recorded_after.get("stdout") != after_text:
        raise Refusal("manifest verifier qgroup postcheck disagrees with raw report")
    if recorded_safehermit.get("stdout") != safehermit_text:
        raise Refusal("manifest safehermit qgroup postcheck disagrees with raw report")
    live_verify = qgroup_report(Path(VERIFY_DIR))
    live_verify_values = parse_qgroup_report(str(live_verify["stdout"]))
    if live_verify_values[0] < after[0]:
        raise Refusal("live verifier qgroup accounting moved backwards after capture")
    live_safehermit = qgroup_report(termination["log_dir"])
    live_safehermit_values = parse_qgroup_report(str(live_safehermit["stdout"]))
    if live_safehermit_values[0] < safehermit[0]:
        raise Refusal("live safehermit qgroup accounting moved backwards after capture")
    # Reading the retained log above can update atime metadata on a relatime
    # mount, and delayed Btrfs accounting can make the live referenced count rise.
    # The security property is the unchanged 8 GB limit plus an under-limit,
    # monotone readback, not byte-for-byte equality with the earlier snapshot.
    return {
        "verifier_path": VERIFY_DIR,
        "verifier_before": before_text.strip(),
        "verifier_after": after_text.strip(),
        "safehermit_path": str(termination["log_dir"]),
        "safehermit_after": safehermit_text.strip(),
    }


def classified_at(finished_at: str, record: dict[str, Any]) -> str:
    moment = dt.datetime.fromisoformat(finished_at.replace("Z", "+00:00"))
    if record["outcome_history"]:
        previous = dt.datetime.fromisoformat(
            record["outcome_history"][-1]["classified_at"].replace("Z", "+00:00")
        )
        moment = max(moment, previous)
    return (moment + dt.timedelta(seconds=1)).isoformat().replace("+00:00", "Z")


def measurement_exhausted(record: dict[str, Any], samples: list[dict[str, Any]]) -> bool:
    design = record["plan"]["measurement_design"]
    valid = sum(sample["status"] == "VALID" for sample in samples)
    return (
        valid >= design["required_valid_seed_tuples"]
        or len(samples) >= design["max_attempted_seed_tuples"]
    )


def matcher_for_sample(
    sample: dict[str, Any], matchers: list[dict[str, Any]]
) -> dict[str, str] | None:
    refs = set(sample["execution_event_ids"])
    for matcher in matchers:
        if set(matcher["execution_event_ids"]) == refs:
            return {
                "sample_id": sample["sample_id"],
                "matcher_id": matcher["matcher_id"],
            }
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--plan-commit", required=True)
    parser.add_argument("--plan-blob", required=True)
    parser.add_argument("--plan-sha256", required=True)
    parser.add_argument("--route-sha256", required=True)
    args = parser.parse_args()
    try:
        record = json.loads(RECORD.read_text())
        selection_index, seed = next_seed(record)
        run_dir = args.run_dir.resolve()
        manifest, raw_paths = validate_run_dir(
            run_dir, record, selection_index, seed
        )
        assert_admission_boundary(
            record, manifest, raw_paths["safehermit_report"]
        )
        receipt = plan_receipt(record, args.plan_commit)
        route = record["plan"]["authorized_routes"][0]
        if (
            args.plan_blob != receipt["blob"]
            or args.plan_sha256 != record["plan"]["plan_sha256"]
            or args.route_sha256 != route["route_sha256"]
        ):
            raise Refusal("explicit plan blob/plan digest/route digest binding drifted")
        if manifest["execution_boundary"]["plan_receipt"] != receipt:
            raise Refusal("runner and admission disagree on the immutable plan receipt")
        live_preflight = assert_preflight(record)
        if manifest["preflight"]["host"] != live_preflight["host"]:
            raise Refusal("runner host facts differ from admission host facts")
        if manifest["preflight"]["binaries"] != live_preflight["binaries"]:
            raise Refusal("runner binary identities differ from admission identities")
        process_returncode = manifest.get("process_returncode")
        if not isinstance(process_returncode, int):
            raise Refusal("runner manifest lacks an integer process return code")
        termination = authenticate_safehermit(
            raw_paths["safehermit_report"],
            raw_paths["stderr"],
            process_returncode,
        )
        qgroups = authenticate_qgroups(manifest, raw_paths, termination)
        stdout = raw_paths["stdout"].read_text(errors="strict")
        stderr = raw_paths["stderr"].read_text(errors="strict")
        classification = classify(stdout, stderr, termination)
        cell = build_cell(record)
        if record["attempt_state"] == "MATERIAL":
            if record["cells"] != [cell]:
                raise Refusal("frozen material cell changed before prefix extension")
        elif record["cells"]:
            raise Refusal("PLANNED record unexpectedly contains a frozen cell")

        evidence = RECORD.parent / "evidence"
        file_prefix = f"selection-{selection_index:03d}-sched-seed-{seed:020d}"
        replica_count = 2 if classification.strict_match else 1
        event_ids: list[str] = []
        new_events: list[dict[str, Any]] = []
        signal_name = (
            signal_module.Signals(termination["signal_number"]).name
            if termination["signal_number"] is not None
            else None
        )
        for replica in range(1, replica_count + 1):
            event_id = (
                f"httpcoro-time-sched-{seed:020d}-replica-{replica}"
            )
            event_ids.append(event_id)
            destination_paths = {
                "STDOUT": evidence / f"{file_prefix}-replica-{replica}.stdout.txt",
                "STDERR": evidence / f"{file_prefix}-replica-{replica}.stderr.txt",
                "SAFEHERMIT_REPORT": evidence
                / f"{file_prefix}-replica-{replica}.safehermit.txt",
                "DIAGNOSTIC": evidence
                / f"{file_prefix}-replica-{replica}.bounds.json",
                "INVOCATION": evidence
                / f"{file_prefix}-replica-{replica}.invocation.json",
                "RESULT": evidence / f"{file_prefix}-replica-{replica}.result.json",
            }
            for kind, path in destination_paths.items():
                if kind == "SAFEHERMIT_REPORT" and replica == 1:
                    continue
                if path.exists():
                    raise Refusal(f"durable evidence destination already exists: {path}")
            copy_new(raw_paths["stdout"], destination_paths["STDOUT"])
            copy_new(raw_paths["stderr"], destination_paths["STDERR"])
            if replica == 1:
                if destination_paths["SAFEHERMIT_REPORT"] != raw_paths["safehermit_report"]:
                    raise Refusal("actual report is not the replica-1 durable locator")
            else:
                copy_new(
                    raw_paths["safehermit_report"],
                    destination_paths["SAFEHERMIT_REPORT"],
                )
            bounds_payload = compact_json(
                {
                    "schema_version": 1,
                    "seed": {"flag": "--sched-seed", "value": seed},
                    "replica_index": replica,
                    "replica_semantics": (
                        "Actual outer safehermit invocation and internal --verify run 1."
                        if replica == 1
                        else "Hermit's internal --verify run 2; the outer safehermit "
                        "report is a copied logical envelope and the event report "
                        "locator is the schema-required projection. The exact single "
                        "wrapper argv is retained in the invocation artifact."
                    ),
                    "actual_wrapper_argv": manifest["argv"],
                    "actual_wrapper_report_locator": evidence_report_locator(
                        selection_index, seed, 1
                    ).as_posix(),
                    "safehermit": {
                        key: termination["fields"][key]
                        for key in (
                            "binary",
                            "binary_sha256",
                            "bound.wall",
                            "bound.cgroup",
                            "bound.disk",
                            "bound.bytes",
                            "truncated",
                        )
                    },
                    "qgroups": qgroups,
                }
            )
            write_new(destination_paths["DIAGNOSTIC"], bounds_payload)
            copy_new(run_dir / "invocation.json", destination_paths["INVOCATION"])
            if classification.status == "VALID":
                target_oracle = classification.target_outcome
            elif classification.selected_count == 0:
                target_oracle = "NOT_RUN"
            else:
                target_oracle = "AMBIGUOUS"
            result = {
                "exit_code": termination["exit_code"],
                "signal": signal_name,
                "timed_out": termination["timed_out"],
                "timeout_origin": "OUTER" if termination["timed_out"] else "NONE",
                "guest_started": classification.selected_count > 0,
                "selected_test_executed_count": classification.selected_count,
                "target_oracle": target_oracle,
                "failure_class_id": classification.failure_class_id,
                "harness_status": classification.harness_status,
                "diagnostic": (
                    f"status={classification.status}; selected={classification.selected_count}; "
                    f"unit_result={termination['unit_result']}; exit={termination['exit_code']}; "
                    f"BitwiseInfoV1={'matched' if classification.strict_match else 'not-matched'}; "
                    "safehermit_bounds=authenticated; verifier_and_wrapper_qgroups=authenticated"
                ),
            }
            route = record["plan"]["authorized_routes"][0]
            event_argv = materialize_argv(
                record,
                seed,
                destination_paths["SAFEHERMIT_REPORT"].relative_to(ROOT),
            )
            event: dict[str, Any] = {
                "event_id": event_id,
                "kind": "HERMIT_EXECUTION",
                "plan_sha256": record["plan"]["plan_sha256"],
                "route_id": route["route_id"],
                "route_sha256": route["route_sha256"],
                "canonical_target": route["canonical_target"],
                "test_selector": copy.deepcopy(route["test_selector"]),
                "build_mode_selector": copy.deepcopy(route["build_mode_selector"]),
                "source_revision": route["source_revision"],
                "cell_id": cell["cell_id"],
                "cell_sha256": cell["cell_sha256"],
                "measurement_role": "DISCOVERY" if replica == 1 else "CONFIRMATION",
                "started_at": termination["started_at"],
                "finished_at": termination["finished_at"],
                "argv": event_argv,
                "result": result,
                "invocation_id": f"httpcoro-time-sched-{seed:020d}-verify",
                "replica_index": replica,
                "seed": {"flag": "--sched-seed", "value": seed},
                "effective_config_sha256": cell["cell_sha256"],
                "artifacts": [],
            }
            write_new(destination_paths["RESULT"], compact_json(result_payload(event)))
            event["artifacts"] = [
                artifact(
                    destination_paths["STDOUT"],
                    f"{event_id}-stdout",
                    "STDOUT",
                    "text/plain",
                ),
                artifact(
                    destination_paths["STDERR"],
                    f"{event_id}-stderr",
                    "STDERR",
                    "text/plain",
                ),
                artifact(
                    destination_paths["RESULT"],
                    f"{event_id}-result",
                    "RESULT",
                    "application/json",
                ),
                artifact(
                    destination_paths["SAFEHERMIT_REPORT"],
                    f"{event_id}-safehermit",
                    "SAFEHERMIT_REPORT",
                    "text/plain",
                ),
                artifact(
                    destination_paths["DIAGNOSTIC"],
                    f"{event_id}-bounds",
                    "DIAGNOSTIC",
                    "application/json",
                    1_048_576,
                ),
                artifact(
                    destination_paths["INVOCATION"],
                    f"{event_id}-invocation",
                    "DIAGNOSTIC",
                    "application/json",
                    1_048_576,
                ),
            ]
            new_events.append(event)

        sample_id = f"httpcoro-time-sched-{seed:020d}-sample"
        sample = {
            "sample_id": sample_id,
            "cell_id": cell["cell_id"],
            "seed": {"flag": "--sched-seed", "value": seed},
            "selection_index": selection_index,
            "status": classification.status,
            "target_outcome": classification.target_outcome,
            "failure_class_id": classification.failure_class_id,
            "censor_reason": classification.censor_reason,
            "execution_event_ids": event_ids,
        }
        new_matcher: dict[str, Any] | None = None
        if classification.strict_match:
            matcher_id = f"httpcoro-time-sched-{seed:020d}-matcher"
            new_matcher = {
                "matcher_id": matcher_id,
                "kind": "BUILTIN_VERIFY",
                "execution_event_ids": event_ids,
                "complete": True,
                "same_target_outcome": True,
                "same_failure_class": True,
                "canonical_info_artifact": {},
                "details": {
                    "verified": True,
                    "bitwise_parity": True,
                    "comparison": "BitwiseInfoV1",
                    "relaxations": [],
                },
            }
            info_path = evidence / f"{file_prefix}.canonical-info.json"
            if info_path.exists():
                raise Refusal(f"durable evidence destination already exists: {info_path}")
            write_new(info_path, compact_json(matcher_payload(new_matcher)))
            new_matcher["canonical_info_artifact"] = artifact(
                info_path,
                f"{matcher_id}-canonical-info",
                "CANONICAL_INFO",
                "application/json",
                1_048_576,
            )

        events = copy.deepcopy(record["events"]) + new_events
        samples = copy.deepcopy(record["samples"]) + [sample]
        matchers = copy.deepcopy(record["matchers"])
        if new_matcher is not None:
            matchers.append(new_matcher)
        pass_witness = next(
            (
                matcher_for_sample(item, matchers)
                for item in samples
                if item["status"] == "VALID" and item["target_outcome"] == "PASS"
            ),
            None,
        )
        fail_witness = next(
            (
                matcher_for_sample(item, matchers)
                for item in samples
                if item["status"] == "VALID" and item["target_outcome"] == "FAIL"
            ),
            None,
        )
        when = classified_at(termination["finished_at"], record)
        supersedes = (
            record["current_outcome"]["classification_id"]
            if record["current_outcome"] is not None
            else None
        )
        if pass_witness is not None and fail_witness is not None:
            outcome: dict[str, Any] = {
                "classification_id": f"httpcoro-time-working-after-{seed:020d}",
                "classified_at": when,
                "supersedes": supersedes,
                "evidence_refs": [
                    pass_witness["sample_id"],
                    pass_witness["matcher_id"],
                    fail_witness["sample_id"],
                    fail_witness["matcher_id"],
                ],
                "kind": "WORKING",
                "pass_witness": pass_witness,
                "fail_witness": fail_witness,
            }
        else:
            exhausted = measurement_exhausted(record, samples)
            route = record["plan"]["authorized_routes"][0]
            all_event_ids = [event["event_id"] for event in events]
            inspected_results = [
                next(
                    item["artifact_id"]
                    for item in event["artifacts"]
                    if item["kind"] == "RESULT"
                )
                for event in events
            ]
            next_text = []
            if not exhausted:
                order = record["plan"]["measurement_design"]["selection_order"][
                    "seed_values"
                ]
                next_text = [
                    f"Execute selection index {len(samples)} at predeclared "
                    f"--sched-seed={order[len(samples)]}."
                ]
            outcome = {
                "classification_id": (
                    f"httpcoro-time-{'exhausted' if exhausted else 'pending'}-"
                    f"after-{seed:020d}"
                ),
                "classified_at": when,
                "supersedes": supersedes,
                "evidence_refs": all_event_ids,
                "kind": "NOT-DETERMINABLE",
                "route_evidence": [
                    {
                        "route_id": route["route_id"],
                        "cell_ids": [cell["cell_id"]],
                        "evidence_event_ids": all_event_ids,
                    }
                ],
                "inspected_commands": [event["argv"] for event in events],
                "inspected_artifacts": inspected_results,
                "missing_or_ambiguous_facts": [
                    "A complete distinct-scheduler-seed PASS/target-owned-FAIL "
                    "bracket is not yet present."
                ],
                "measurement_budget_exhausted": exhausted,
                "pending_in_scope_measurements": next_text,
            }
        history = copy.deepcopy(record["outcome_history"])
        history.append(outcome)
        record.update(
            plan_receipt=receipt,
            attempt_state="MATERIAL",
            cells=[cell],
            events=events,
            samples=samples,
            matchers=matchers,
            outcome_history=history,
            current_outcome=copy.deepcopy(outcome),
        )
        RECORD.write_bytes(pretty_json(record))
        print(
            json.dumps(
                {
                    "seed": seed,
                    "selection_index": selection_index,
                    "status": classification.status,
                    "target_outcome": classification.target_outcome,
                    "failure_class_id": classification.failure_class_id,
                    "strict_match": classification.strict_match,
                    "current_outcome": outcome["kind"],
                },
                sort_keys=True,
            )
        )
        return 0
    except (OSError, UnicodeError, json.JSONDecodeError, Refusal) as error:
        parser.exit(2, f"REFUSED: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
