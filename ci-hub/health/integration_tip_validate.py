#!/usr/bin/env python3
"""Start one periodic validate when the exact Hermit main tip has no record or run."""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import re
import signal
import stat
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Mapping, Sequence


TOOL_ROOT = Path(__file__).resolve().parents[2]
STATE_ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", TOOL_ROOT)).resolve()
sys.path.insert(0, str(TOOL_ROOT / "ci-hub" / "lib"))
import fetch_ref  # noqa: E402
import git_env  # noqa: E402


REPO = "rrnewton/hermit"
AGENT = "ops-tick-integration-tip"
DEFAULT_TOTAL_TIMEOUT_SECONDS = 75.0
# The first query includes cold Rust compilation: a fresh tool clone took
# 46.847s even after removing repeated Git discovery. Allocate that startup
# within the existing total deadline; later observations still have their
# shorter cap and every phase is limited by the time remaining.
FIRST_STATUS_TIMEOUT_SECONDS = 50.0
LAUNCH_TIMEOUT_SECONDS = 10.0
LAUNCHER_OBSERVATION_TIMEOUT_SECONDS = 5.0
CONSUMED_IDENTITY_TIMEOUT_SECONDS = 5.0
EXACT_VALIDATE_STATUS_SCHEMA_VERSION = 3


class ObservationUnavailable(RuntimeError):
    """One required authority could not answer within this tick."""


class CommandNotStarted(ObservationUnavailable):
    """The command was refused before its child process could be started."""


@dataclass(frozen=True)
class Settings:
    tool_root: Path
    state_root: Path
    source_checkout: Path
    ci_hub: Path
    operational_tool: Path | None
    tool_parent_sha: str | None
    state_dir: Path
    total_timeout_seconds: float

    @classmethod
    def defaults(
        cls,
        *,
        tool_root: Path = TOOL_ROOT,
        state_root: Path = STATE_ROOT,
        source_checkout: Path | None = None,
        ci_hub: Path | None = None,
        operational_tool: Path | None = None,
        tool_parent_sha: str | None = None,
        state_dir: Path | None = None,
        total_timeout_seconds: float = DEFAULT_TOTAL_TIMEOUT_SECONDS,
    ) -> "Settings":
        state_root = state_root.resolve()
        configured_operational_tool = os.environ.get("DEV_HERMIT_OPERATIONAL_TOOL")
        configured_parent_sha = os.environ.get("DEV_HERMIT_TOOL_PARENT_SHA")
        return cls(
            tool_root=tool_root.resolve(),
            state_root=state_root,
            source_checkout=(source_checkout or state_root / "hermit").resolve(),
            ci_hub=(ci_hub or tool_root / "ci-hub" / "ci-hub").resolve(),
            operational_tool=(
                operational_tool
                or (Path(configured_operational_tool) if configured_operational_tool else None)
            ),
            tool_parent_sha=tool_parent_sha or configured_parent_sha,
            state_dir=(
                state_dir
                or state_root / "ignored" / "ci-hub" / "integration-tip-validate"
            ).resolve(),
            total_timeout_seconds=total_timeout_seconds,
        )


Runner = Callable[[Sequence[str], Path, float], subprocess.CompletedProcess[str]]
Fetcher = Callable[[Settings, float], str]
Spawner = Callable[[Settings, str, str, str, Path, Path, float], Mapping[str, object]]


def one_line(value: object) -> str:
    return " ".join(str(value).split())


def emit(**fields: object) -> None:
    for key, value in fields.items():
        print(f"{key}={one_line(value)}")


def remaining(deadline: float, limit: float) -> float:
    value = min(limit, deadline - time.monotonic())
    if value <= 0:
        raise ObservationUnavailable("the decision deadline expired")
    return value


def run_bounded(
    command: Sequence[str], cwd: Path, timeout: float
) -> subprocess.CompletedProcess[str]:
    """Run one authority query and kill its whole process group on timeout."""

    try:
        process = subprocess.Popen(
            list(command),
            cwd=cwd,
            env=git_env.sanitized_git_env(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
    except OSError as error:
        raise CommandNotStarted(f"cannot start {command[0]}: {error}") from error
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate()
        raise ObservationUnavailable(
            f"{' '.join(command[:3])} timed out after {timeout:.1f}s"
        ) from error
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def fetch_tip(settings: Settings, timeout: float) -> str:
    try:
        result = fetch_ref.fetch_remote_tracking_branch(
            settings.source_checkout,
            remote="origin",
            branch="main",
            network_timeout_s=timeout,
        )
    except (OSError, fetch_ref.FetchRefError) as error:
        raise ObservationUnavailable(f"cannot fetch the exact Hermit main tip: {error}") from error
    # Another fetcher may already have reconciled the shared tracking ref to a
    # newer, remotely confirmed descendant. Every successful FetchResult makes
    # `tracking_sha` the authoritative tip. Selecting this process's stale
    # `fetched_sha` would consume the scheduled cadence without measuring the
    # current integration tip.
    return result.tracking_sha


def command_json(
    command: Sequence[str], *, cwd: Path, timeout: float, run: Runner
) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
    completed = run(command, cwd, timeout)
    try:
        value = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        detail = (
            completed.stderr.strip()
            or completed.stdout.strip()
            or f"exit {completed.returncode}"
        )
        raise ObservationUnavailable(
            f"{' '.join(command[:3])} returned unreadable JSON ({one_line(detail)}; {error})"
        ) from error
    if not isinstance(value, dict):
        raise ObservationUnavailable(f"{' '.join(command[:3])} did not return a JSON object")
    return completed, value


def exact_record_count(
    settings: Settings, target: str, timeout: float, run: Runner
) -> tuple[int, int, dict[str, object]]:
    completed, report = command_json(
        [
            str(settings.ci_hub),
            "validate-status",
            "--sha",
            target,
            "--repo",
            REPO,
            "--log-paths",
            "--json",
        ],
        cwd=settings.state_root,
        timeout=timeout,
        run=run,
    )
    try:
        reported_sha = report["sha"]
        reported_exit = report["exit_code"]
        count = report["record_count"]
    except KeyError as error:
        raise ObservationUnavailable(
            f"exact validate-status omitted required fields: {error}"
        ) from error
    if (
        type(report.get("schema_version")) is not int
        or report.get("schema_version") != EXACT_VALIDATE_STATUS_SCHEMA_VERSION
        or report.get("repo") != REPO
        or reported_sha != target
        or type(reported_exit) is not int
        or reported_exit != completed.returncode
        or type(count) is not int
    ):
        raise ObservationUnavailable(
            "exact validate-status did not bind its JSON to the requested SHA and exit code"
        )
    if count < 0:
        raise ObservationUnavailable("exact validate-status returned a negative record count")
    unreadable = report.get("unreadable_handles")
    in_progress = report.get("in_progress")
    in_progress_count = report.get("in_progress_count")
    run_handles = report.get("run_handles")
    exact_records = report.get("exact_records")
    if (
        not isinstance(unreadable, list)
        or not isinstance(in_progress, list)
        or not isinstance(run_handles, list)
        or not isinstance(exact_records, list)
    ):
        raise ObservationUnavailable(
            "exact validate-status omitted exact_records, run_handles, in_progress, "
            "or unreadable_handles"
        )
    if unreadable:
        raise ObservationUnavailable(
            f"exact validate-status could not read {len(unreadable)} run handle(s); "
            "a malformed handle without a readable target cannot be proved unrelated"
        )
    if type(in_progress_count) is not int or in_progress_count != len(in_progress):
        raise ObservationUnavailable("exact validate-status returned inconsistent in-progress data")
    if any(
        not isinstance(row, dict)
        or row.get("sha") != target
        or row.get("in_progress") is not True
        for row in in_progress
    ):
        raise ObservationUnavailable("exact validate-status returned an unrelated in-progress row")
    if any(
        not isinstance(handle, dict) or handle.get("sha") != target
        for handle in run_handles
    ):
        raise ObservationUnavailable("exact validate-status returned an unrelated run handle")
    if count < len(exact_records) or (count == 0 and exact_records):
        raise ObservationUnavailable(
            "exact validate-status returned inconsistent exact-record data"
        )
    if any(
        not isinstance(record, dict)
        or record.get("verdict")
        not in {
            "VALIDATED",
            "FAILED",
            "TRUNCATED",
            "NEEDS-RERUN",
            "NO-RESULT",
            "NOT-VALIDATED",
        }
        or not isinstance(record.get("detail"), str)
        or not str(record["detail"]).strip()
        for record in exact_records
    ):
        raise ObservationUnavailable("exact validate-status returned a malformed exact record")
    if count == 0:
        freshness = report.get("ledger_freshness")
        if not isinstance(freshness, dict) or freshness.get(
            "local_absence_is_authoritative"
        ) is not True:
            raise ObservationUnavailable(
                "the validation ledger could not establish that this exact tip has no record"
            )
    return count, in_progress_count, report


def outstanding_run_handles(report: Mapping[str, object]) -> list[Mapping[str, object]]:
    """Return nonterminal ci-hub records that still require reconciliation."""

    selected: list[Mapping[str, object]] = []
    handles = report.get("run_handles")
    if not isinstance(handles, list):
        raise ObservationUnavailable("exact validate-status omitted run_handles")
    for handle in handles:
        if not isinstance(handle, dict):
            raise ObservationUnavailable("exact validate-status returned a malformed run handle")
        if handle.get("state") not in {"preparing", "launching", "running"}:
            continue
        if handle.get("admission") == "refused":
            continue
        if any(handle.get(field) is not None for field in ("finished_at", "result", "exit_code")):
            continue
        required = ("unit", "sha", "started_at")
        if any(not isinstance(handle.get(field), str) or not handle[field] for field in required):
            raise ObservationUnavailable(
                "an outstanding run handle omitted its unit, SHA, or start time"
            )
        if not isinstance(handle.get("process_verified"), bool):
            raise ObservationUnavailable(
                "an outstanding run handle omitted whether its process identity was verified"
            )
        selected.append(handle)
    return selected


def merge_run_handles(
    first: Sequence[Mapping[str, object]], second: Sequence[Mapping[str, object]]
) -> list[Mapping[str, object]]:
    merged: list[Mapping[str, object]] = []
    seen: set[tuple[object, object, object]] = set()
    for handle in [*first, *second]:
        key = (handle.get("unit"), handle.get("sha"), handle.get("started_at"))
        if key not in seen:
            seen.add(key)
            merged.append(handle)
    return merged


def run_handle_text(handles: Sequence[Mapping[str, object]]) -> str:
    return "; ".join(
        "unit={unit} state={state} head={sha} slot={slot} log={log} "
        "started={started_at} process_verified={process_verified}".format(
            unit=handle["unit"],
            state=handle["state"],
            sha=handle["sha"],
            slot=handle.get("slot") or "?",
            log=handle.get("log_file") or "?",
            started_at=handle["started_at"],
            process_verified=handle["process_verified"],
        )
        for handle in handles
    )


def admission_status(
    settings: Settings, target: str, timeout: float, run: Runner
) -> dict[str, object]:
    completed, report = command_json(
        [
            str(settings.ci_hub),
            "validate-lock",
            "admission-status",
            "--agent",
            AGENT,
            "--kind",
            "validate",
            "--target",
            target,
            "--target-was-fetched-main",
            "--json",
        ],
        cwd=settings.state_root,
        timeout=timeout,
        run=run,
    )
    admissible = report.get("admissible")
    if (
        report.get("schema_version") != 1
        or report.get("kind") != "validate"
        or not isinstance(admissible, bool)
        or report.get("target") != target
    ):
        raise ObservationUnavailable("validate-lock admission-status returned inconsistent JSON")
    state = report.get("state")
    expected = {
        "admissible": 0,
        "busy": 1,
        "unknown": 2,
        "refused": 3,
        "quarantined": 3,
        "stale": 3,
    }.get(state)
    if expected is None:
        raise ObservationUnavailable(
            f"validate-lock admission-status returned unsupported state {state!r}"
        )
    if completed.returncode != expected:
        raise ObservationUnavailable(
            f"validate-lock admission-status JSON disagrees with exit {completed.returncode}"
        )
    if admissible != (state == "admissible"):
        raise ObservationUnavailable(
            "validate-lock admission-status state disagrees with admissible"
        )
    return report


def admission_reports_exact_target(report: Mapping[str, object]) -> bool:
    return report.get("reason_code") in {
        "exact-target-already-queued",
        "exact-target-already-running",
    }


def atomic_write_json(path: Path, value: Mapping[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}-{uuid.uuid4().hex}")
    try:
        with temporary.open("x", encoding="utf-8") as stream:
            json.dump(dict(value), stream, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def read_json(path: Path) -> dict[str, object] | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, json.JSONDecodeError) as error:
        raise ObservationUnavailable(f"cannot read prior tick attempt {path}: {error}") from error
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise ObservationUnavailable(f"prior tick attempt {path} has an unsupported shape")
    return value


def latest_prior_attempt(
    tick_attempt: Mapping[str, object] | None,
    run_handles: object,
) -> Mapping[str, object] | None:
    candidates: list[Mapping[str, object]] = []
    if tick_attempt is not None:
        candidates.append(tick_attempt)
    if isinstance(run_handles, list):
        candidates.extend(handle for handle in run_handles if isinstance(handle, dict))
    return max(
        candidates,
        key=lambda row: str(row.get("started_at", "")),
        default=None,
    )


def prior_launcher_observation(
    settings: Settings,
    attempt: Mapping[str, object],
    timeout: float,
    run: Runner,
) -> dict[str, str]:
    """Read the exact launcher, without promoting it to validation evidence."""

    target = attempt.get("target")
    unit = attempt.get("launcher_unit")
    if (
        not isinstance(target, str)
        or re.fullmatch(r"[0-9a-f]{40}", target) is None
        or not isinstance(unit, str)
        or re.fullmatch(
            rf"validate-ops-tick-{target[:12]}-[0-9a-f]{{12}}-launcher\.service",
            unit,
        ) is None
    ):
        raise ObservationUnavailable(
            "prior attempt lacks an exact bound launcher identity; " + prior_attempt_text(attempt)
        )
    fields = ("Id", "LoadState", "ActiveState", "SubState", "MainPID", "InvocationID")
    completed = run(
        ["systemctl", "--user", "show", unit, "--no-pager", *(
            f"--property={name}" for name in fields
        )],
        settings.state_root,
        timeout,
    )
    if completed.returncode != 0:
        raise ObservationUnavailable(
            f"cannot observe prior launcher {unit}: "
            f"{one_line(completed.stderr.strip() or f'exit {completed.returncode}')}"
        )
    properties: dict[str, str] = {}
    for line in completed.stdout.splitlines():
        name, separator, value = line.partition("=")
        if not separator or name in properties or name not in fields:
            raise ObservationUnavailable(f"malformed systemctl reply for prior launcher {unit}")
        properties[name] = value
    if (
        set(properties) != set(fields)
        or properties["Id"] != unit
        or re.fullmatch(r"[0-9]+", properties["MainPID"]) is None
        or (
            properties["InvocationID"]
            and re.fullmatch(r"[0-9a-f]{32}", properties["InvocationID"]) is None
        )
    ):
        raise ObservationUnavailable(f"incomplete or mismatched prior launcher observation for {unit}")
    state = properties["ActiveState"]
    if (
        properties["LoadState"] == "not-found"
        and state == "inactive"
        and properties["SubState"] == "dead"
        and properties["MainPID"] == "0"
        and properties["InvocationID"] == ""
    ):
        return {**properties, "observation": "absent"}
    if properties["LoadState"] == "loaded":
        if (
            (state, properties["SubState"]) in {("inactive", "dead"), ("failed", "failed")}
            and properties["MainPID"] == "0"
        ):
            return {**properties, "observation": "terminal"}
        if state in {
            "active", "activating", "deactivating", "reloading", "refreshing", "maintenance"
        } and properties["InvocationID"]:
            return {**properties, "observation": "live"}
    raise ObservationUnavailable(f"uncertain prior launcher state for {unit}: {properties}")


def launcher_prerequisites(settings: Settings) -> tuple[Path, str]:
    operational_tool = settings.operational_tool
    parent_sha = settings.tool_parent_sha
    if (
        operational_tool is None
        or not operational_tool.is_file()
        or not os.access(operational_tool, os.X_OK)
    ):
        raise CommandNotStarted(
            "the verified installed operational-tool is unavailable; refusing a launcher "
            "that would depend on the health tick's short-lived tool snapshot"
        )
    if (
        parent_sha is None
        or re.fullmatch(r"[0-9a-f]{40}", parent_sha) is None
    ):
        raise CommandNotStarted(
            "the exact parent tool SHA is unavailable; refusing an unbound launcher"
        )
    return operational_tool, parent_sha


def consumed_launcher_identity(
    settings: Settings,
    attempt: Mapping[str, object],
    timeout: float,
    run: Runner,
) -> dict[str, object]:
    """Prove a retained terminal handle consumed this exact exclusive unit key.

    The current tool's parser supplies schema and producer authority. This is
    consumption evidence only; the old outcome is never a verdict for a new tip.
    """

    unit = str(attempt["launcher_unit"]).removesuffix("-launcher.service") + ".service"
    if attempt.get("unit", unit) != unit:
        raise ObservationUnavailable("prior launcher and validation unit identities disagree")
    path = settings.state_root / "ignored" / "validate" / "runs" / f"{unit.removesuffix('.service')}.json"

    def retained_identity() -> tuple[int, int, int, int]:
        try:
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode) or path.resolve(strict=True) != path:
                raise ObservationUnavailable(f"consumed identity is not a canonical regular handle: {path}")
            return metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns
        except OSError as error:
            raise ObservationUnavailable(f"cannot inspect retained consumed identity {path}: {error}") from error

    before = retained_identity()
    completed, report = command_json(
        [
            sys.executable,
            "-B",
            str(settings.tool_root / "ci-hub" / "validate" / "run_registry.py"),
            "inspect-current-records",
            "--path",
            str(path),
        ],
        cwd=settings.state_root,
        timeout=timeout,
        run=run,
    )
    if retained_identity() != before:
        raise ObservationUnavailable(f"retained consumed identity changed during inspection: {path}")
    records = report.get("records")
    if (
        completed.returncode != 0
        or report.get("unreadable") != []
        or not isinstance(records, list)
        or len(records) != 1
        or not isinstance(records[0], dict)
        or records[0].get("path") != str(path)
        or not isinstance(records[0].get("record"), dict)
    ):
        raise ObservationUnavailable(f"current run registry did not authenticate one readable exact handle: {path}")
    record = records[0]["record"]
    if (
        type(record.get("schema_version")) is not int
        or record["schema_version"] != 1
        or record.get("kind") != "validate"
        or record.get("unit") != unit
        or record.get("target") != attempt.get("target")
        or record.get("repo") != REPO
        or record.get("counts_as_validation") is not True
        or record.get("state") not in {"completed", "refused", "killed"}
        or not isinstance(record.get("finished_at"), str)
        or not record["finished_at"].strip()
        or type(record.get("exit_code")) is not int
    ):
        raise ObservationUnavailable(f"retained handle is not terminal evidence for the exact prior identity: {path}")
    return {
        "record_path": str(path),
        "record_dev": before[0],
        "record_ino": before[1],
        "tool_parent_sha": settings.tool_parent_sha,
        **{name: record.get(name) for name in ("unit", "target", "state", "result", "detail", "exit_code", "finished_at")},
    }


def spawn_validate(
    settings: Settings,
    target: str,
    attempt_id: str,
    unit: str,
    validate_log: Path,
    launcher_log: Path,
    timeout: float,
) -> Mapping[str, object]:
    operational_tool, parent_sha = launcher_prerequisites(settings)
    try:
        launcher_log.parent.mkdir(parents=True, exist_ok=True)
        descriptor = os.open(launcher_log, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(descriptor)
    except OSError as error:
        raise CommandNotStarted(f"cannot prepare launcher log {launcher_log}: {error}") from error
    launcher_unit = f"{unit}-launcher"
    validate_arguments = [
        "validate-run",
        "--checkout",
        str(settings.source_checkout),
        "--state-root",
        str(settings.state_root),
        "--repo",
        REPO,
        "--materialize-target",
        "--agent",
        AGENT,
        "--target",
        target,
        "--unit",
        unit,
        "--log",
        str(validate_log),
        "--no-wait",
        "--skip-if-recorded",
        "--json",
        "--",
        "full",
    ]
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--collect",
        f"--unit={launcher_unit}",
        "--description=start one periodic exact-tip Hermit validate",
        "--property=RuntimeMaxSec=5000",
        f"--property=StandardOutput=append:{launcher_log}",
        f"--property=StandardError=append:{launcher_log}",
        f"--working-directory={settings.state_root}",
        f"--setenv=PATH={os.environ.get('PATH', '')}",
        f"--setenv=DEV_HERMIT_PARENT={settings.state_root}",
        "--",
        str(operational_tool),
        "--run-current-tool",
        "--state-root",
        str(settings.state_root),
        "--tool-parent-sha",
        parent_sha,
        "--tool-relative",
        "ci-hub/ci-hub",
        "--",
        *validate_arguments,
    ]
    completed = run_bounded(command, settings.state_root, timeout)
    if completed.returncode != 0:
        detail = (
            completed.stderr.strip()
            or completed.stdout.strip()
            or f"exit {completed.returncode}"
        )
        raise ObservationUnavailable(
            f"systemd-run did not confirm {launcher_unit}: {one_line(detail)}"
        )
    return {"unit": f"{launcher_unit}.service", "accepted": True}


def prior_attempt_text(prior: Mapping[str, object] | None) -> str:
    if prior is None:
        return (
            "no readable prior attempt record was found; ledger absence alone cannot tell "
            "whether validation was never attempted or ended before writing a ledger row"
        )
    name = prior.get("attempt_id") or prior.get("unit") or "?"
    state = prior.get("state") or "unknown"
    result = prior.get("result") or "none"
    exit_code = prior.get("exit_code")
    exit_text = "unknown" if exit_code is None else str(exit_code)
    detail = prior.get("detail")
    detail_text = f" detail={one_line(detail)}" if detail else ""
    return (
        f"prior attempt {name} ended or stopped being observable "
        f"with state={state} result={result} exit={exit_text}{detail_text} "
        "and no ledger row"
    )


def record_status_text(report: Mapping[str, object]) -> str:
    records = report["exact_records"]
    if not records:
        return (
            "the raw record identity is present, but its current row was corrected "
            "or superseded"
        )
    pieces: list[str] = []
    for record in records:
        exit_code = record.get("exit_code")
        exit_text = "unknown" if exit_code is None else str(exit_code)
        result = record.get("result") or "none"
        pieces.append(
            f"{record['verdict']} result={result} exit={exit_text} "
            f"detail={one_line(record['detail'])}"
        )
    return "; ".join(pieces)


def run_tick(
    settings: Settings,
    *,
    fetch: Fetcher = fetch_tip,
    run: Runner = run_bounded,
    spawn: Spawner = spawn_validate,
    dry_run: bool = False,
) -> int:
    deadline = time.monotonic() + settings.total_timeout_seconds
    target = fetch(settings, remaining(deadline, 25.0))
    count, in_progress, status = exact_record_count(
        settings, target, remaining(deadline, FIRST_STATUS_TIMEOUT_SECONDS), run
    )
    first_outstanding = outstanding_run_handles(status)
    verified = [handle for handle in first_outstanding if handle["process_verified"] is True]
    if in_progress and not verified:
        raise ObservationUnavailable(
            "exact validate-status reported an in-progress run without a process-verified "
            "launching or running handle; rerun the exact validate-status query and repair "
            "its run record before retrying"
        )
    outstanding = first_outstanding
    admission: dict[str, object] | None = None
    lock_reports_exact_target = False
    tick_attempt: dict[str, object] | None = None
    reuse_launcher_identity = False
    consumed_prior: dict[str, object] | None = None
    if not count and not in_progress:
        tick_attempt = read_json(settings.state_dir / "latest.json")
        if tick_attempt is not None and tick_attempt.get("state") != "launch-not-started":
            observed = prior_launcher_observation(
                settings,
                tick_attempt,
                remaining(deadline, LAUNCHER_OBSERVATION_TIMEOUT_SECONDS),
                run,
            )
            if observed["observation"] == "live":
                emit(
                    state="launcher-in-progress",
                    target=target,
                    launcher_target=tick_attempt.get("target"),
                    launcher_unit=observed["Id"],
                    launcher_pid=observed["MainPID"],
                    launcher_invocation=observed["InvocationID"],
                    launched=0,
                    summary=(
                        f"prior launcher {observed['Id']} is still {observed['ActiveState']}; "
                        "no second launcher started; this is not validation result evidence"
                    ),
                )
                return 0
            launch_receipt = tick_attempt.get("launcher")
            unconfirmed = (
                tick_attempt.get("state") != "launcher-accepted"
                or not isinstance(launch_receipt, dict)
                or launch_receipt.get("accepted") is not True
                or launch_receipt.get("unit") != observed["Id"]
                or tick_attempt.get("launcher_identity_unconfirmed") is True
            )
            if unconfirmed:
                if tick_attempt.get("target") != target:
                    try:
                        consumed_prior = consumed_launcher_identity(
                            settings,
                            tick_attempt,
                            remaining(deadline, CONSUMED_IDENTITY_TIMEOUT_SECONDS),
                            run,
                        )
                    except ObservationUnavailable as error:
                        raise ObservationUnavailable(
                            f"prior launcher {observed['Id']} for another tip has an unconfirmed "
                            f"request and no retained terminal consumption proof: {error}"
                        ) from error
                else:
                    # A manager request may still be pending after the client
                    # was killed, even when a query reports not-found. Reuse
                    # both names so delayed acceptance races the same keys,
                    # never a second independently named launcher/run.
                    reuse_launcher_identity = True
        # A preparing record exists before validate-lock publishes its process
        # identity. Read the lock between snapshots and preserve outstanding
        # handles from either read so the periodic check never creates a
        # duplicate during that window.
        admission = admission_status(settings, target, remaining(deadline, 10.0), run)
        lock_reports_exact_target = admission_reports_exact_target(admission)
        count, in_progress, status = exact_record_count(
            settings, target, remaining(deadline, 25.0), run
        )
        second_outstanding = outstanding_run_handles(status)
        outstanding = merge_run_handles(first_outstanding, second_outstanding)
        verified = [handle for handle in second_outstanding if handle["process_verified"] is True]
        if in_progress and not verified:
            raise ObservationUnavailable(
                "exact validate-status reported an in-progress run without a process-verified "
                "launching or running handle; rerun the exact validate-status query and repair "
                "its run record before retrying"
            )
    if count:
        verdict = str(status["verdict"])
        detail = record_status_text(status)
        next_action = (
            "none; this exact tip is already validated"
            if verdict == "VALIDATED"
            else (
                "inspect the recorded outcome and explicitly rerun this exact tip "
                "when appropriate; no automatic duplicate was started"
            )
        )
        emit(
            state=("record-present" if verdict == "VALIDATED" else verdict.lower()),
            target=target,
            record_count=count,
            verdict=verdict,
            record_detail=detail,
            launched=0,
            next_action=next_action,
            summary=(
                f"exact Hermit main tip {target} has verdict {verdict}: {detail}; "
                f"{next_action}"
            ),
        )
        return int(status["exit_code"])

    # A process/cgroup-verified run handle or the exact validate-lock queue or
    # held slot is positive evidence. An unverified handle alone is not.
    if in_progress or lock_reports_exact_target:
        reported_count = in_progress if in_progress else 1
        details = (
            run_handle_text(verified)
            if verified
            else one_line(admission.get("detail", "validate-lock reports this exact target"))
        )
        emit(
            state="in-progress",
            target=target,
            record_count=0,
            in_progress_count=reported_count,
            runs=details,
            launched=0,
            summary=(
                f"exact Hermit main tip {target} already has {reported_count} "
                f"validate run(s) in progress; {details}; no validate started"
            ),
        )
        return 0

    if outstanding:
        details = run_handle_text(outstanding)
        remedy = (
            "inspect the listed run handle and validate-lock admission-status for this "
            "exact SHA; repair or finish the handle before retrying"
        )
        emit(
            state="unknown",
            target=target,
            record_count=0,
            in_progress_count="unknown",
            runs=details,
            launched=0,
            next_action=remedy,
            summary=(
                f"exact Hermit main tip {target} has an outstanding run handle whose "
                f"process identity is not verified: {details}; {remedy}; no validate started"
            ),
        )
        return 2

    prior = latest_prior_attempt(
        tick_attempt if tick_attempt and tick_attempt.get("target") == target else None,
        status["run_handles"],
    )
    if admission is None:
        admission = admission_status(settings, target, remaining(deadline, 10.0), run)
    if admission["admissible"] is not True:
        reason = one_line(admission.get("detail", admission.get("reason_code", "unknown")))
        admission_state = str(admission["state"])
        emit(
            state=admission_state,
            target=target,
            record_count=0,
            in_progress_count=0,
            launched=0,
            prior_attempt=prior_attempt_text(prior),
            next_action="retry on the next scheduled tick when immediate admission is available",
            summary=(
                "validate-lock cannot admit an immediate validate for exact "
                f"Hermit main tip {target}: {reason}; no validate started"
            ),
        )
        return 0 if admission_state == "busy" else 2

    if dry_run:
        prior_text = prior_attempt_text(prior)
        emit(
            state="would-launch",
            target=target,
            record_count=0,
            in_progress_count=0,
            launched=0,
            prior_attempt=prior_text,
            next_action="run without --dry-run to start one validate",
            summary=(
                f"would start one asynchronous validate for exact Hermit main tip {target}; "
                f"{prior_text}; run without --dry-run to start it"
            ),
        )
        return 0

    # Refuse an exhausted decision or missing launcher prerequisites before
    # writing intent. A later setup error can still be classified not-started.
    remaining(deadline, LAUNCH_TIMEOUT_SECONDS)
    launcher_prerequisites(settings)
    observed_at = datetime.now(timezone.utc).isoformat()
    token = uuid.uuid4().hex[:12]
    attempt_id = f"{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}-{token}"
    unit = f"validate-ops-tick-{target[:12]}-{token}"
    if reuse_launcher_identity:
        assert tick_attempt is not None
        unit = str(tick_attempt["launcher_unit"]).removesuffix("-launcher.service")
    logs = settings.state_dir / "logs"
    validate_log = logs / f"{attempt_id}-validate.log"
    launcher_log = logs / f"{attempt_id}-launcher.log"
    attempt_path = settings.state_dir / "latest.json"
    attempt: dict[str, object] = {
        "schema_version": 1,
        "attempt_id": attempt_id,
        "state": "starting",
        "target": target,
        "repo": REPO,
        "agent": AGENT,
        "unit": f"{unit}.service",
        "launcher_unit": f"{unit}-launcher.service",
        "started_at": observed_at,
        "validate_log": str(validate_log),
        "launcher_log": str(launcher_log),
        "launcher_identity_unconfirmed": reuse_launcher_identity,
    }
    if consumed_prior is not None:
        attempt["consumed_prior_launcher"] = consumed_prior
    if reuse_launcher_identity:
        assert tick_attempt is not None
        attempt["reused_launcher_from"] = {
            name: tick_attempt.get(name)
            for name in ("attempt_id", "started_at", "state", "detail", "validate_log", "launcher_log")
        }
    atomic_write_json(attempt_path, attempt)
    try:
        try:
            launch_timeout = remaining(deadline, LAUNCH_TIMEOUT_SECONDS)
        except ObservationUnavailable as error:
            raise CommandNotStarted(str(error)) from error
        launch = spawn(
            settings,
            target,
            attempt_id,
            unit,
            validate_log,
            launcher_log,
            launch_timeout,
        )
    except Exception as error:
        not_started = isinstance(error, CommandNotStarted) and not reuse_launcher_identity
        state = "launch-not-started" if not_started else "launch-unknown"
        detail = one_line(error)
        attempt.update(state=state, detail=detail)
        if not_started:
            attempt["finished_at"] = datetime.now(timezone.utc).isoformat()
        else:
            attempt["launcher_identity_unconfirmed"] = True
        try:
            atomic_write_json(attempt_path, attempt)
        except OSError as record_error:
            # The original starting intent still names the exact unit. Never
            # turn a failed evidence update into a claim that dispatch failed.
            detail += f"; could not update attempt evidence: {one_line(record_error)}"
        emit(
            state=state,
            target=target,
            record_count=0,
            launcher_unit=attempt["launcher_unit"],
            launched=0 if not_started else "unknown",
            prior_attempt=prior_attempt_text(prior),
            next_action=(
                "repair the pre-dispatch refusal, then retry this exact tip"
                if not_started else
                "observe the recorded launcher unit before retrying; its request may have been accepted"
            ),
            summary=(
                f"launcher {'was not started' if not_started else 'acceptance is unconfirmed'} "
                f"for exact Hermit main tip {target}: {detail}"
            ),
        )
        return 1 if not_started else 2
    attempt.update(state="launcher-accepted", launcher=dict(launch))
    try:
        atomic_write_json(attempt_path, attempt)
    except OSError as error:
        emit(
            state="launch-evidence-unknown",
            target=target,
            launcher_unit=attempt["launcher_unit"],
            launched=1,
            summary=(
                f"systemd accepted launcher {attempt['launcher_unit']}, but its evidence "
                f"update failed: {one_line(error)}; observe the retained starting intent"
            ),
        )
        return 2
    emit(
        state="launched",
        target=target,
        record_count=0,
        in_progress_count=0,
        launched=1,
        attempt=attempt_id,
        prior_attempt=prior_attempt_text(prior),
        next_action=f"follow {validate_log}; read validate-status when the run finishes",
        summary=(
            f"systemd accepted one asynchronous validation launcher for exact Hermit main tip {target}; "
            f"{prior_attempt_text(prior)}; follow {validate_log} and read validate-status "
            "when the run finishes"
        ),
    )
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    result.add_argument("--state-root", type=Path, default=STATE_ROOT)
    result.add_argument("--tool-root", type=Path, default=TOOL_ROOT)
    result.add_argument("--source-checkout", type=Path)
    result.add_argument("--ci-hub", type=Path)
    result.add_argument("--state-dir", type=Path)
    result.add_argument(
        "--dry-run",
        action="store_true",
        help="report whether a validate would start without creating a run or launcher",
    )
    result.add_argument(
        "--total-timeout-seconds",
        type=float,
        default=DEFAULT_TOTAL_TIMEOUT_SECONDS,
    )
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if args.total_timeout_seconds <= 0 or args.total_timeout_seconds >= 180:
        raise SystemExit("--total-timeout-seconds must be greater than 0 and less than 180")
    settings = Settings.defaults(
        tool_root=args.tool_root,
        state_root=args.state_root,
        source_checkout=args.source_checkout,
        ci_hub=args.ci_hub,
        state_dir=args.state_dir,
        total_timeout_seconds=args.total_timeout_seconds,
    )
    settings.state_dir.mkdir(parents=True, exist_ok=True)
    lock_path = settings.state_dir / "tick.lock"
    try:
        with lock_path.open("a+") as lock:
            try:
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                emit(
                    state="in-progress",
                    launched=0,
                    summary=(
                        "another operational tick is already deciding whether to "
                        "start this validate; no second decision started"
                    ),
                )
                return 0
            try:
                return run_tick(settings, dry_run=args.dry_run)
            except ObservationUnavailable as error:
                emit(
                    state="unknown",
                    launched=0,
                    summary=(
                        "could not decide whether to start the periodic validate: "
                        f"{one_line(error)}"
                    ),
                )
                return 2
    except OSError as error:
        emit(
            state="unknown",
            launched=0,
            summary=f"could not lock periodic validate state at {lock_path}: {one_line(error)}",
        )
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
