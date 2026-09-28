#!/usr/bin/env python3
"""Start one periodic validate when the exact Hermit main tip has no record or run."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import io
import json
import os
import re
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import time
import traceback
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Iterator, Mapping, Sequence


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

# THE DECISION DOES NOT FIT IN THE HEALTH TICK, SO IT RUNS BESIDE IT.
# Measured on devbig014, 2026-09-28: the first ci-hub call in each freshly
# materialized tool tree is a cold release compile (44.7s for `--help`), and
# each exact `validate-status --sha` then spends 34-60s, about 31s of it
# replaying the whole validation ledger (7170 events). The decision needs two
# such reads plus admission, roughly 115-150s, against the 75s this gate may
# spend inside the serial tick. Largely as a result devbig014 has launched no
# periodic validate since 2026-09-18T21:49Z, and devbig030 never has.
#
# The gate therefore reports the previous decision and starts the next one in a
# transient user unit running this same file (`--decide`) from the same exact
# tool commit. That decider holds the same tick.lock and makes exactly the
# decision the gate used to make, with the budgets below; nothing it checks was
# removed. It records its outcome in decisions/<dispatch id>.json, which the
# next gate reports with that decision's exit code: findings lag one cadence.
# While a decider is live the gate starts no second one, and a decider that
# ended without a record is itself reported as an unknown decision.
DETACHED_TOTAL_TIMEOUT_SECONDS = 600.0
DETACHED_RUNTIME_MAX_SECONDS = 900
DECIDER_LOCK_WAIT_SECONDS = 120.0
DECIDER_DISPATCH_TIMEOUT_SECONDS = 10.0
DECIDER_OBSERVATION_TIMEOUT_SECONDS = 5.0
DISPATCH_RECORD = "dispatch.json"
UNRELATED_UNREADABLE_RECORD = "unrelated-unreadable-handles.json"
DECIDER_TOOL_RELATIVE = "ci-hub/health/integration_tip_validate.py"
DISPATCH_ID_PATTERN = r"[0-9]{8}T[0-9]{6}Z-[0-9a-f]{12}"
DECIDER_HISTORY_KEEP = 48

# A DECISION BUILDS CI-HUB IN ITS OWN RUST-SCRIPT CACHE.
# `ci-hub/ci-hub` runs `rust-script --force`, and every rust-script on the host
# builds into the one target directory ~/.cache/rust-script/binaries under one
# Cargo lock, so even an already-built ci-hub waits for every other lane's
# script build. Measured on devbig014, 2026-09-28: `ci-hub --help` with its
# binary already built took 0.14-0.36s alone but 54.5s and 72.8s while other
# lanes' validate.rs, scorecard.rs and pressure-test.rs builds held that lock,
# and decider 20260928T174539Z-c17ba8f8b9de spent its entire 180s
# second-status phase without an answer. In a private cache a fully cold ci-hub
# build took 46.3s (293 MB) and exact validate-status then took 37-42s, waiting
# on no other lane. validate-run already isolates itself the same way in
# ci-hub/ci-hub. The cache path is fixed and is only used under tick.lock, which
# every decider and dry run holds for its whole decision; it starts empty and
# is removed afterwards. Other tools write into it too: exact validate-status
# runs ci-hub/ledger/validate_rows.py, whose ci-hub/ledger/ledger.py checker is
# materialized under $XDG_CACHE_HOME with 0o555 directories whenever
# DEV_HERMIT_TOOL_CACHE is unset. Only the operational tool exports that
# variable, so a dry run started from an agent shell leaves such a tree here;
# one was found 2026-09-28 after a hand-run validate-status in a private cache.
# A plain rmtree cannot empty it, so remove_decision_cache makes the cache's own
# directories writable first.
DECISION_RUST_SCRIPT_CACHE = "decision-rust-script-cache"

# An unreadable run handle blocks the launch unless its file last changed
# before the target commit existed: a handle is written only after its target
# is chosen, so bytes last written before that commit cannot name it, and a
# live owner keeps the target it launched with. The margin absorbs clock skew
# between the committing host and this one.
UNREADABLE_HANDLE_CLOCK_MARGIN_SECONDS = 3600
COMMIT_TIME_TIMEOUT_SECONDS = 5.0

# THE LAUNCHER'S CHECKOUT HOOK MUST BE ABLE TO CLONE THE TARGET'S SUBMODULES.
# The launched validate provisions its checkout with wrkslots, whose
# post-provision hook runs `git submodule update --init --recursive` inside the
# launcher unit, with the environment the systemd user manager hands that unit.
# Measured on devbig014, 2026-09-28: ~/.gitconfig rewrote every Hermit
# submodule URL (url.<local mirror>.insteadOf) to a local path, a submodule
# clone runs with GIT_PROTOCOL_FROM_USER=0, and the manager environment carried
# no GIT_ALLOW_PROTOCOL, so git refused "transport 'file' not allowed". An
# interactive shell with GIT_ALLOW_PROTOCOL=https:file:ssh succeeds, which is
# why a hand-run validate works while the periodic one cannot. The refused
# create left a CREATE journal that made wrkslots refuse heartbeat, hold,
# remove and handoff commands for every lane on the machine until an operator
# ran `wrkslots recover`. The launch therefore asks git itself, in the unit's
# environment, whether each local submodule source is readable the way the
# hook will read it, and refuses before systemd-run if not.
LAUNCHER_GIT_ENVIRONMENT = re.compile(r"GIT_[A-Za-z0-9_]+|HOME|XDG_CONFIG_HOME")

# The launcher unit carries the file-transport allowance the agent shells on
# this host already carry, set with its own `systemd-run --setenv` so nothing
# outside that one unit changes: not the systemd user manager's environment,
# not ~/.gitconfig, and not the decider or validate units. GIT_ALLOW_PROTOCOL,
# when set, is git's whole protocol allowlist and overrides every
# protocol.<name>.allow setting, so the unit's clones no longer depend on
# whether the manager or a global config happens to admit `file`. It also
# refuses http:// and git:// in that unit; every recursive submodule URL of
# Hermit main is https (checked at 10770b2f34, 2026-09-28). The preflight
# probes with this same mapping laid over the manager environment, so removing
# the allowance makes the preflight refuse again instead of starting a
# launcher whose create would fail and leave a journal.
LAUNCHER_UNIT_GIT_SETENV: Mapping[str, str] = {"GIT_ALLOW_PROTOCOL": "https:file:ssh"}


class ObservationUnavailable(RuntimeError):
    """One required authority could not answer within this tick."""


class CommandNotStarted(ObservationUnavailable):
    """The command was refused before its child process could be started."""


@dataclass(frozen=True)
class PhaseLimits:
    """Per-authority caps; the decision's total deadline still bounds their sum."""

    fetch: float = 25.0
    first_status: float = FIRST_STATUS_TIMEOUT_SECONDS
    launcher_observation: float = LAUNCHER_OBSERVATION_TIMEOUT_SECONDS
    consumed_identity: float = CONSUMED_IDENTITY_TIMEOUT_SECONDS
    admission: float = 10.0
    second_status: float = 25.0
    launch: float = LAUNCH_TIMEOUT_SECONDS


# Each cap is at least three times the slowest reading measured for its phase
# on 2026-09-28 (first status 106.6s cold through operational-tool, a later
# status 59.6s, admission 1.1s).
DETACHED_PHASE_LIMITS = PhaseLimits(
    fetch=60.0,
    first_status=330.0,
    launcher_observation=15.0,
    consumed_identity=15.0,
    admission=30.0,
    second_status=180.0,
    launch=30.0,
)


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
    limits: PhaseLimits = PhaseLimits()

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
        limits: PhaseLimits = PhaseLimits(),
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
            limits=limits,
        )


Runner = Callable[[Sequence[str], Path, float], subprocess.CompletedProcess[str]]
Fetcher = Callable[[Settings, float], str]
Spawner = Callable[[Settings, str, str, str, Path, Path, float], Mapping[str, object]]
DeciderSpawner = Callable[[Settings, str, str, Path, float], None]
Tick = Callable[[Settings], int]


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


def utc_text(moment: float) -> str:
    return datetime.fromtimestamp(moment, timezone.utc).isoformat()


def handle_last_change(path: Path) -> float:
    """Latest mtime or ctime of the path and, for a symlink, of its target.

    ctime cannot be set from user space and moves on every rename or copy, so a
    restored or relabelled file cannot look older than its latest write.
    """

    metadata = path.lstat()
    moments = [metadata.st_mtime, metadata.st_ctime]
    if stat.S_ISLNK(metadata.st_mode):
        target = path.stat()
        moments += [target.st_mtime, target.st_ctime]
    return max(moments)


def target_commit_time(settings: Settings, target: str, timeout: float, run: Runner) -> int:
    completed = run(
        [
            "git",
            "-C",
            str(settings.source_checkout),
            "show",
            "-s",
            "--format=%ct",
            f"{target}^{{commit}}",
        ],
        settings.state_root,
        timeout,
    )
    text = completed.stdout.strip()
    if completed.returncode != 0 or re.fullmatch(r"[0-9]+", text) is None:
        detail = completed.stderr.strip() or f"exit {completed.returncode}"
        raise ObservationUnavailable(
            f"cannot read the commit time of exact tip {target}: {one_line(detail)}"
        )
    return int(text)


def unrelated_unreadable_handles(
    settings: Settings,
    target: str,
    unreadable: Sequence[object],
    deadline: float,
    run: Runner,
) -> dict[str, object]:
    """Prove every unreadable run handle predates `target`, or refuse the decision.

    Returns the evidence record for the handles set aside. Any handle that
    cannot be inspected, or that last changed within the clock margin of the
    target's commit time or later, still refuses the whole decision.
    """

    inspected: list[tuple[Path, str, float]] = []
    blocking: list[tuple[Path, str]] = []
    for entry in unreadable:
        if (
            not isinstance(entry, dict)
            or not isinstance(entry.get("path"), str)
            or not entry["path"]
        ):
            raise ObservationUnavailable(
                "exact validate-status returned an unreadable handle without a path"
            )
        path = Path(entry["path"])
        if not path.is_absolute():
            path = settings.state_root / path
        try:
            last_change = handle_last_change(path)
        except OSError as error:
            blocking.append((path, f"cannot inspect it: {one_line(error)}"))
            continue
        inspected.append((path, one_line(entry.get("error", "")), last_change))
    commit_time: int | None = None
    cutoff: int | None = None
    if inspected and not blocking:
        commit_time = target_commit_time(
            settings, target, remaining(deadline, COMMIT_TIME_TIMEOUT_SECONDS), run
        )
        cutoff = commit_time - UNREADABLE_HANDLE_CLOCK_MARGIN_SECONDS
        blocking.extend(
            (path, f"it last changed {utc_text(last_change)}, not before {utc_text(cutoff)}")
            for path, _error, last_change in inspected
            if last_change >= cutoff
        )
    if blocking:
        path, reason = blocking[0]
        raise ObservationUnavailable(
            f"exact validate-status could not read {len(unreadable)} run handle(s); "
            f"{len(blocking)} may postdate exact tip {target} (for example {path}: {reason}); "
            "a malformed handle without a readable target cannot be proved unrelated"
        )
    assert commit_time is not None and cutoff is not None
    return {
        "schema_version": 1,
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "target": target,
        "target_commit_time": utc_text(commit_time),
        "cutoff": utc_text(cutoff),
        "rule": (
            "each handle last changed before the target commit time minus "
            f"{UNREADABLE_HANDLE_CLOCK_MARGIN_SECONDS}s, so it cannot name the target"
        ),
        "count": len(inspected),
        "handles": [
            {"path": str(path), "error": error, "last_change": utc_text(last_change)}
            for path, error, last_change in inspected
        ],
    }


def exact_record_count(
    settings: Settings, target: str, deadline: float, cap: float, run: Runner
) -> tuple[int, int, dict[str, object]]:
    """Read and bind one exact validate-status report.

    Unreadable handles that provably predate the target are recorded in
    UNRELATED_UNREADABLE_RECORD and listed in the returned report under
    `unrelated_unreadable`; any other unreadable handle refuses the decision.
    """

    timeout = remaining(deadline, cap)
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
    # Checked last, so the commit-time query and the evidence write happen only
    # for a report that is otherwise bound to this exact target.
    report["unrelated_unreadable"] = None
    if unreadable:
        record = unrelated_unreadable_handles(settings, target, unreadable, deadline, run)
        record_path = settings.state_dir / UNRELATED_UNREADABLE_RECORD
        try:
            atomic_write_json(record_path, record)
        except OSError as error:
            raise ObservationUnavailable(
                f"cannot record {record['count']} unrelated unreadable run handle(s) "
                f"at {record_path}: {one_line(error)}"
            ) from error
        report["unrelated_unreadable"] = {"count": record["count"], "record": str(record_path)}
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


def read_json(path: Path, label: str = "prior tick attempt") -> dict[str, object] | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, json.JSONDecodeError) as error:
        raise ObservationUnavailable(f"cannot read {label} {path}: {error}") from error
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise ObservationUnavailable(f"{label} {path} has an unsupported shape")
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
    return observe_user_unit(settings, unit, timeout, run, "prior launcher")


def observe_user_unit(
    settings: Settings, unit: str, timeout: float, run: Runner, label: str
) -> dict[str, str]:
    """Classify one exact user unit as absent, terminal, or live, or refuse."""

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
            f"cannot observe {label} {unit}: "
            f"{one_line(completed.stderr.strip() or f'exit {completed.returncode}')}"
        )
    properties: dict[str, str] = {}
    for line in completed.stdout.splitlines():
        name, separator, value = line.partition("=")
        if not separator or name in properties or name not in fields:
            raise ObservationUnavailable(f"malformed systemctl reply for {label} {unit}")
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
        raise ObservationUnavailable(f"incomplete or mismatched {label} observation for {unit}")
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
    raise ObservationUnavailable(f"uncertain {label} state for {unit}: {properties}")


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


def launcher_git_environment(
    settings: Settings,
    timeout: float,
    run: Runner,
    unit_setenv: Mapping[str, str],
) -> dict[str, str]:
    """Return the Git-relevant part of the environment a launcher unit inherits.

    A `systemd-run --user` service starts from the user manager's environment
    block plus its own `--setenv` values, not from this process's environment;
    `unit_setenv` is exactly the Git environment the launch passes as
    `--setenv`, and it wins over the manager's value, as systemd applies it.
    """

    completed = run(["systemctl", "--user", "show-environment"], settings.state_root, timeout)
    if completed.returncode != 0:
        detail = completed.stderr.strip() or f"exit {completed.returncode}"
        raise CommandNotStarted(
            "cannot read the systemd user manager environment a launcher would inherit: "
            f"{one_line(detail)}"
        )
    environment: dict[str, str] = {}
    for line in completed.stdout.splitlines():
        name, separator, value = line.partition("=")
        if not separator or LAUNCHER_GIT_ENVIRONMENT.fullmatch(name) is None:
            continue
        if value.startswith("$'"):
            raise CommandNotStarted(
                f"cannot decode the user manager's {name} value {value!r}; refusing a launcher "
                "whose Git environment this preflight cannot reproduce"
            )
        if value[:1] in {"'", '"'}:
            try:
                words = shlex.split(value)
            except ValueError as error:
                raise CommandNotStarted(
                    f"cannot decode the user manager's {name} value {value!r}: {error}"
                ) from error
            if len(words) != 1:
                raise CommandNotStarted(
                    f"cannot decode the user manager's {name} value {value!r} as one word"
                )
            value = words[0]
        environment[name] = value
    for name, value in unit_setenv.items():
        if LAUNCHER_GIT_ENVIRONMENT.fullmatch(name) is None:
            raise CommandNotStarted(
                f"launcher unit variable {name} is not a Git environment variable this preflight models"
            )
        environment[name] = value
    return environment


def launcher_setenv_arguments(unit_setenv: Mapping[str, str]) -> list[str]:
    """Spell the launcher unit's own Git environment as systemd-run arguments."""

    return [f"--setenv={name}={value}" for name, value in sorted(unit_setenv.items())]


def launcher_git(environment: Mapping[str, str], *arguments: str) -> list[str]:
    """Build a git command that sees only the launcher unit's Git environment."""

    assignments = [f"{name}={value}" for name, value in sorted(environment.items())]
    return ["env", "-i", *assignments, f"PATH={os.environ.get('PATH', '')}", "git", *arguments]


def uses_file_transport(url: str) -> bool:
    """Classify a submodule source the way git picks its transport."""

    scheme = re.match(r"([A-Za-z][A-Za-z0-9+.-]*)://", url)
    if scheme is not None:
        return scheme.group(1).lower() == "file"
    colon = url.find(":")
    slash = url.find("/")
    # `host:path` without a slash before the colon is git's scp-like SSH form.
    return not (colon > 0 and (slash < 0 or colon < slash))


def launcher_provisioning_preflight(
    settings: Settings,
    target: str,
    timeout: float,
    run: Runner | None = None,
    *,
    unit_setenv: Mapping[str, str] | None = None,
) -> list[dict[str, str]]:
    """Refuse a launch whose checkout hook cannot clone the target's submodules.

    Only the target's top-level submodules are probed; a nested submodule is
    reached through them. Every unanswered question refuses: this runs before
    systemd-run, so a refusal starts nothing and leaves no wrkslots journal.
    `unit_setenv` must be the Git environment the launch passes as `--setenv`;
    None means the production LAUNCHER_UNIT_GIT_SETENV, read at call time.
    """

    run = run or run_bounded
    if unit_setenv is None:
        unit_setenv = LAUNCHER_UNIT_GIT_SETENV
    deadline = time.monotonic() + timeout

    def query(command: Sequence[str]) -> subprocess.CompletedProcess[str]:
        try:
            return run(command, settings.source_checkout, remaining(deadline, timeout))
        except ObservationUnavailable as error:
            raise CommandNotStarted(f"launcher provisioning preflight: {error}") from error

    try:
        environment = launcher_git_environment(
            settings, remaining(deadline, timeout), run, unit_setenv
        )
    except ObservationUnavailable as error:
        if isinstance(error, CommandNotStarted):
            raise
        raise CommandNotStarted(f"launcher provisioning preflight: {error}") from error
    checkout = str(settings.source_checkout)
    listed = query(
        launcher_git(
            environment,
            "-C",
            checkout,
            "config",
            "--blob",
            f"{target}:.gitmodules",
            "--get-regexp",
            r"^submodule\..*\.url$",
        )
    )
    if listed.returncode == 1 and not listed.stdout.strip() and not listed.stderr.strip():
        return []
    if listed.returncode != 0:
        detail = listed.stderr.strip() or f"exit {listed.returncode}"
        raise CommandNotStarted(
            f"cannot read the submodule URLs of target {target}: {one_line(detail)}"
        )
    sources: list[dict[str, str]] = []
    for line in listed.stdout.splitlines():
        key, _, declared = line.partition(" ")
        match = re.fullmatch(r"submodule\.(.+)\.url", key)
        if match is None or not declared:
            raise CommandNotStarted(f"unreadable .gitmodules URL entry for target {target}: {line!r}")
        name = match.group(1)
        # `submodule update --init` keeps a URL the shared repository config
        # already records and copies the .gitmodules URL only when none is set.
        configured = query(launcher_git(environment, "-C", checkout, "config", "--get", f"submodule.{name}.url"))
        if configured.returncode == 0 and configured.stdout.strip():
            url = configured.stdout.strip()
        elif configured.returncode == 1 and not configured.stderr.strip():
            url = declared
        else:
            detail = configured.stderr.strip() or f"exit {configured.returncode}"
            raise CommandNotStarted(f"cannot read the configured URL of submodule {name}: {one_line(detail)}")
        if url.startswith(("./", "../")):
            raise CommandNotStarted(
                f"submodule {name} of target {target} has relative URL {url!r}; this preflight "
                "cannot tell whether its clone would use the local file transport"
            )
        expanded = query(launcher_git(environment, "-C", checkout, "ls-remote", "--get-url", url))
        source = expanded.stdout.strip()
        if expanded.returncode != 0 or not source:
            detail = expanded.stderr.strip() or f"exit {expanded.returncode}"
            raise CommandNotStarted(f"cannot expand the clone URL of submodule {name}: {one_line(detail)}")
        if not uses_file_transport(source):
            continue
        # A submodule clone runs with GIT_PROTOCOL_FROM_USER=0, so git applies
        # its non-user file-transport policy; probe exactly that policy.
        probe = query(
            launcher_git(
                {**environment, "GIT_PROTOCOL_FROM_USER": "0"},
                "-C",
                checkout,
                "ls-remote",
                source,
                "HEAD",
            )
        )
        if probe.returncode == 0:
            sources.append({"submodule": name, "url": url, "source": source})
            continue
        detail = one_line(probe.stderr.strip() or f"exit {probe.returncode}")
        unit_git = (
            ", ".join(f"{name}={value}" for name, value in sorted(unit_setenv.items()))
            or "none"
        )
        remedy = (
            "the launcher unit's own --setenv Git environment "
            f"({unit_git}) does not admit the file transport; restore "
            "LAUNCHER_UNIT_GIT_SETENV in ci-hub/health/integration_tip_validate.py, which "
            "scopes the allowance to that one unit, instead of changing the systemd user "
            "manager's environment or ~/.gitconfig, which every agent on this host shares"
            if "transport 'file' not allowed" in detail else
            "repair the local source named by the url.insteadOf rewrite"
        )
        raise CommandNotStarted(
            f"the launcher's checkout hook `git submodule update --init --recursive` would fail: "
            f"submodule {name} ({url}) is rewritten to local source {source}, and git in the "
            f"launcher unit's environment refuses it ({detail}); a failed create leaves a "
            "wrkslots CREATE journal that refuses heartbeat, hold, remove and handoff commands "
            f"for every lane on this machine; no launcher started; {remedy}"
        )
    return sources


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
    *,
    run: Runner | None = None,
) -> Mapping[str, object]:
    run = run or run_bounded
    operational_tool, parent_sha = launcher_prerequisites(settings)
    launch_deadline = time.monotonic() + timeout
    # One mapping feeds both the preflight and the unit's --setenv arguments,
    # so the preflight always probes the environment the launcher will get.
    unit_setenv = dict(LAUNCHER_UNIT_GIT_SETENV)
    # At most half the launch budget, so systemd-run keeps time to answer.
    launcher_provisioning_preflight(
        settings, target, timeout / 2, run, unit_setenv=unit_setenv
    )
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
        *launcher_setenv_arguments(unit_setenv),
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
    try:
        launch_timeout = remaining(launch_deadline, timeout)
    except ObservationUnavailable as error:
        raise CommandNotStarted(f"no time left to start {launcher_unit}: {error}") from error
    completed = run(command, settings.state_root, launch_timeout)
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
    limits = settings.limits
    target = fetch(settings, remaining(deadline, limits.fetch))
    count, in_progress, status = exact_record_count(
        settings, target, deadline, limits.first_status, run
    )
    set_aside = status["unrelated_unreadable"]
    if set_aside is not None:
        emit(
            unrelated_unreadable_handles=set_aside["count"],
            unrelated_unreadable_record=set_aside["record"],
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
                remaining(deadline, limits.launcher_observation),
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
                            remaining(deadline, limits.consumed_identity),
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
        admission = admission_status(
            settings, target, remaining(deadline, limits.admission), run
        )
        lock_reports_exact_target = admission_reports_exact_target(admission)
        count, in_progress, status = exact_record_count(
            settings, target, deadline, limits.second_status, run
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
        admission = admission_status(
            settings, target, remaining(deadline, limits.admission), run
        )
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
    remaining(deadline, limits.launch)
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
            launch_timeout = remaining(deadline, limits.launch)
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


def dispatch_token(dispatch_id: str) -> str:
    return dispatch_id.rsplit("-", 1)[1]


def decider_unit(dispatch_id: str) -> str:
    return f"integration-tip-decide-{dispatch_token(dispatch_id)}.service"


def decision_path(settings: Settings, dispatch_id: str) -> Path:
    return settings.state_dir / "decisions" / f"{dispatch_id}.json"


def decider_log(settings: Settings, dispatch_id: str) -> Path:
    return settings.state_dir / "logs" / f"{dispatch_id}-decider.log"


def captured_fields(output: str) -> dict[str, str]:
    """Parse the `key=value` lines tick-hub also captures; the last value wins."""

    fields: dict[str, str] = {}
    for line in output.splitlines():
        key, separator, value = line.partition("=")
        if separator and re.fullmatch(r"[a-z_][a-z0-9_]*", key):
            fields[key] = value
    return fields


def spawn_decider(
    settings: Settings, dispatch_id: str, unit: str, log: Path, timeout: float
) -> None:
    """Start this file's `--decide` mode from the same exact tool commit."""

    operational_tool, parent_sha = launcher_prerequisites(settings)
    try:
        log.parent.mkdir(parents=True, exist_ok=True)
        os.close(os.open(log, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
    except OSError as error:
        raise CommandNotStarted(f"cannot prepare decider log {log}: {error}") from error
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--collect",
        f"--unit={unit.removesuffix('.service')}",
        "--description=decide whether to start one periodic exact-tip Hermit validate",
        f"--property=RuntimeMaxSec={DETACHED_RUNTIME_MAX_SECONDS}",
        f"--property=StandardOutput=append:{log}",
        f"--property=StandardError=append:{log}",
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
        DECIDER_TOOL_RELATIVE,
        "--",
        "--state-root",
        str(settings.state_root),
        "--state-dir",
        str(settings.state_dir),
        "--source-checkout",
        str(settings.source_checkout),
        "--decide",
        "--dispatch-id",
        dispatch_id,
    ]
    completed = run_bounded(command, settings.state_root, timeout)
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or f"exit {completed.returncode}"
        raise ObservationUnavailable(f"systemd-run did not confirm {unit}: {one_line(detail)}")


def prune_decider_history(settings: Settings, keep: int = DECIDER_HISTORY_KEEP) -> None:
    """Keep the newest decision records and decider logs; the rest are routine."""

    for directory, suffix in (
        (settings.state_dir / "decisions", ".json"),
        (settings.state_dir / "logs", "-decider.log"),
    ):
        try:
            names = sorted(
                name
                for name in os.listdir(directory)
                if re.fullmatch(DISPATCH_ID_PATTERN + re.escape(suffix), name)
            )
        except OSError:
            continue
        for name in names[:-keep]:
            try:
                (directory / name).unlink()
            except OSError:
                pass


def dispatch_next_decider(
    settings: Settings, deadline: float, spawn: DeciderSpawner
) -> tuple[str, str]:
    """Record intent, start one decider, and return its dispatch id and unit."""

    remaining(deadline, DECIDER_DISPATCH_TIMEOUT_SECONDS)
    launcher_prerequisites(settings)
    now = datetime.now(timezone.utc)
    dispatch_id = f"{now.strftime('%Y%m%dT%H%M%SZ')}-{uuid.uuid4().hex[:12]}"
    unit = decider_unit(dispatch_id)
    log = decider_log(settings, dispatch_id)
    record: dict[str, object] = {
        "schema_version": 1,
        "dispatch_id": dispatch_id,
        "unit": unit,
        "state": "dispatching",
        "dispatched_at": now.isoformat(),
        "log": str(log),
        "decision_record": str(decision_path(settings, dispatch_id)),
        "tool_parent_sha": settings.tool_parent_sha,
    }
    path = settings.state_dir / DISPATCH_RECORD
    try:
        atomic_write_json(path, record)
    except OSError as error:
        raise ObservationUnavailable(f"cannot record decider intent at {path}: {error}") from error
    try:
        try:
            timeout = remaining(deadline, DECIDER_DISPATCH_TIMEOUT_SECONDS)
        except ObservationUnavailable as error:
            raise CommandNotStarted(str(error)) from error
        spawn(settings, dispatch_id, unit, log, timeout)
    except ObservationUnavailable as error:
        record.update(
            state="not-started" if isinstance(error, CommandNotStarted) else "unconfirmed",
            detail=one_line(error),
        )
        with contextlib.suppress(OSError):
            atomic_write_json(path, record)
        raise
    record["state"] = "accepted"
    with contextlib.suppress(OSError):
        atomic_write_json(path, record)
    prune_decider_history(settings)
    return dispatch_id, unit


def recorded_decision(
    dispatch: Mapping[str, object], result: Mapping[str, object]
) -> tuple[dict[str, object], int]:
    fields = result.get("fields")
    exit_code = result.get("exit_code")
    finished_at = result.get("finished_at")
    if (
        result.get("dispatch_id") != dispatch["dispatch_id"]
        or not isinstance(fields, dict)
        or not all(
            isinstance(key, str) and re.fullmatch(r"[a-z_][a-z0-9_]*", key) and isinstance(value, str)
            for key, value in fields.items()
        )
        or type(exit_code) is not int
        or not isinstance(finished_at, str)
    ):
        raise ObservationUnavailable(
            f"decision record for {dispatch['dispatch_id']} has an unsupported shape"
        )
    summary = fields.get("summary") or f"no summary was recorded; see {dispatch.get('log')}"
    report: dict[str, object] = {
        **fields,
        "decision": dispatch["dispatch_id"],
        "decided_at": finished_at,
        "decision_log": dispatch.get("log"),
        "summary": f"decision {dispatch['dispatch_id']} finished {finished_at}: {summary}",
    }
    return report, exit_code


def gate_tick(
    settings: Settings,
    *,
    run: Runner = run_bounded,
    spawn: DeciderSpawner = spawn_decider,
) -> int:
    """Report the previous detached decision and start the next one."""

    deadline = time.monotonic() + settings.total_timeout_seconds
    previous = read_json(settings.state_dir / DISPATCH_RECORD, "decider dispatch record")
    report: dict[str, object] = {}
    report_exit = 0
    if previous is not None:
        dispatch_id = previous.get("dispatch_id")
        if (
            not isinstance(dispatch_id, str)
            or re.fullmatch(DISPATCH_ID_PATTERN, dispatch_id) is None
            or previous.get("unit") != decider_unit(dispatch_id)
        ):
            raise ObservationUnavailable(
                f"decider dispatch record {settings.state_dir / DISPATCH_RECORD} "
                "does not bind a dispatch id to its unit"
            )
        result = read_json(decision_path(settings, dispatch_id), "decision record")
        if result is not None:
            report, report_exit = recorded_decision(previous, result)
        else:
            unit = decider_unit(dispatch_id)
            observed = observe_user_unit(
                settings,
                unit,
                remaining(deadline, DECIDER_OBSERVATION_TIMEOUT_SECONDS),
                run,
                "prior decider",
            )
            if observed["observation"] == "live":
                emit(
                    state="deciding",
                    decision=dispatch_id,
                    decision_unit=unit,
                    decision_pid=observed["MainPID"],
                    decision_log=previous.get("log"),
                    launched=0,
                    summary=(
                        f"decider {unit}, dispatched {previous.get('dispatched_at')}, is still "
                        f"{observed['ActiveState']}; no second decider started; its decision is "
                        "reported by the next tick"
                    ),
                )
                return 0
            report = {
                "state": "unknown",
                "launched": 0,
                "decision": dispatch_id,
                "decision_unit": unit,
                "decision_log": previous.get("log"),
                "summary": (
                    f"decider {unit}, dispatched {previous.get('dispatched_at')} with dispatch "
                    f"state {previous.get('state')}, is {observed['observation']} "
                    f"({observed['ActiveState']}/{observed['SubState']}) and recorded no "
                    f"decision; read {previous.get('log')}"
                ),
            }
            report_exit = 2
    try:
        next_id, next_unit = dispatch_next_decider(settings, deadline, spawn)
    except ObservationUnavailable as error:
        earlier = f"; previous: {report['summary']}" if report else ""
        emit(
            **{
                **report,
                "state": "unknown",
                "launched": 0,
                "next_decision": "not-started",
                "summary": f"could not start the next decision: {one_line(error)}{earlier}",
            }
        )
        return 2
    if not report:
        emit(
            state="decision-dispatched",
            launched=0,
            next_decision=next_id,
            next_decision_unit=next_unit,
            summary=(
                f"no earlier decision is recorded; started decider {next_unit}, whose "
                "decision is reported by the next tick"
            ),
        )
        return 0
    emit(**report, next_decision=next_id, next_decision_unit=next_unit)
    return report_exit


@contextlib.contextmanager
def decision_rust_script_cache(settings: Settings) -> Iterator[Path]:
    """Point this decision's ci-hub builds at a cache no other lane locks.

    Hold tick.lock around the whole context: the path is fixed, and whatever a
    killed decision left there is removed before use. A cache that cannot be
    emptied refuses the decision rather than growing unseen.
    """

    root = settings.state_dir / DECISION_RUST_SCRIPT_CACHE
    try:
        remove_decision_cache(root)
        root.mkdir(parents=True)
    except OSError as error:
        raise ObservationUnavailable(
            f"cannot start this decision with an empty rust-script cache at {root}: {error}"
        ) from error
    previous = os.environ.get("XDG_CACHE_HOME")
    os.environ["XDG_CACHE_HOME"] = str(root)
    try:
        yield root
    finally:
        if previous is None:
            os.environ.pop("XDG_CACHE_HOME", None)
        else:
            os.environ["XDG_CACHE_HOME"] = previous
        try:
            remove_decision_cache(root)
        except OSError:
            # Whatever survives is removed, or refused loudly, when the next
            # decision starts; the finished decision's record stays as it was.
            pass


def remove_decision_cache(root: Path) -> None:
    """Remove the decision cache, including read-only trees tools left in it.

    Only real directories inside the cache are made writable. A symlink is
    removed as an entry, and whatever it points at is never changed.
    """

    if root.is_symlink():
        root.unlink()
        return
    if not root.exists():
        return
    os.chmod(root, 0o700)
    for directory, names, _files in os.walk(root):
        for name in names:
            path = os.path.join(directory, name)
            if not os.path.islink(path):
                os.chmod(path, 0o700)
    shutil.rmtree(root)


def acquire_lock(lock: object, wait_seconds: float) -> bool:
    deadline = time.monotonic() + wait_seconds
    while True:
        try:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            return True
        except BlockingIOError:
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.5)


def decide(
    settings: Settings,
    dispatch_id: str,
    *,
    tick: Tick = run_tick,
    lock_wait_seconds: float = DECIDER_LOCK_WAIT_SECONDS,
) -> int:
    """Make one decision under tick.lock and record it for the gate.

    Returns 0 once the decision is recorded, whatever it was: the gate reports
    the recorded exit code. Nonzero means no decision record was written.
    """

    started_at = datetime.now(timezone.utc).isoformat()
    lock_path = settings.state_dir / "tick.lock"
    buffer = io.StringIO()
    exit_code = 2
    try:
        with lock_path.open("a+") as lock:
            acquired = acquire_lock(lock, lock_wait_seconds)
            with contextlib.redirect_stdout(buffer):
                if not acquired:
                    emit(
                        state="unknown",
                        launched=0,
                        summary=(
                            f"decider could not take {lock_path} within "
                            f"{lock_wait_seconds:.0f}s; no decision was made"
                        ),
                    )
                else:
                    try:
                        with decision_rust_script_cache(settings):
                            exit_code = tick(settings)
                    except ObservationUnavailable as error:
                        emit(
                            state="unknown",
                            launched=0,
                            summary=(
                                "could not decide whether to start the periodic validate: "
                                f"{one_line(error)}"
                            ),
                        )
                    except Exception as error:  # noqa: BLE001 - recorded, never silent
                        traceback.print_exc(file=sys.stderr)
                        emit(
                            state="unknown",
                            launched=0,
                            summary=f"the decider failed: {type(error).__name__}: {one_line(error)}",
                        )
            output = buffer.getvalue()
            sys.stdout.write(output)
            sys.stdout.flush()
            atomic_write_json(
                decision_path(settings, dispatch_id),
                {
                    "schema_version": 1,
                    "dispatch_id": dispatch_id,
                    "unit": decider_unit(dispatch_id),
                    "started_at": started_at,
                    "finished_at": datetime.now(timezone.utc).isoformat(),
                    "exit_code": exit_code,
                    "fields": captured_fields(output),
                    "output": output,
                },
            )
    except OSError as error:
        print(f"could not record decision {dispatch_id}: {error}", file=sys.stderr)
        return 2
    return 0


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    result.add_argument("--state-root", type=Path, default=STATE_ROOT)
    result.add_argument("--tool-root", type=Path, default=TOOL_ROOT)
    result.add_argument("--source-checkout", type=Path)
    result.add_argument("--ci-hub", type=Path)
    result.add_argument("--state-dir", type=Path)
    mode = result.add_mutually_exclusive_group()
    mode.add_argument(
        "--dry-run",
        action="store_true",
        help=(
            "decide in the foreground and report whether a validate would start, "
            "without creating a run or launcher"
        ),
    )
    mode.add_argument(
        "--decide",
        action="store_true",
        help="make one decision in the foreground and record it under --dispatch-id",
    )
    result.add_argument("--dispatch-id", help="the gate's dispatch id; requires --decide")
    result.add_argument(
        "--total-timeout-seconds",
        type=float,
        help=(
            f"decision deadline: default {DEFAULT_TOTAL_TIMEOUT_SECONDS:.0f}s for the gate, "
            f"{DETACHED_TOTAL_TIMEOUT_SECONDS:.0f}s for --dry-run and --decide"
        ),
    )
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    if args.decide != (args.dispatch_id is not None):
        raise SystemExit("--decide and --dispatch-id must be given together")
    if args.decide and re.fullmatch(DISPATCH_ID_PATTERN, args.dispatch_id) is None:
        raise SystemExit(f"--dispatch-id must match {DISPATCH_ID_PATTERN}")
    foreground = args.dry_run or args.decide
    total = args.total_timeout_seconds
    if foreground:
        total = DETACHED_TOTAL_TIMEOUT_SECONDS if total is None else total
        if total <= 0 or total > DETACHED_TOTAL_TIMEOUT_SECONDS:
            raise SystemExit(
                "--total-timeout-seconds must be greater than 0 and at most "
                f"{DETACHED_TOTAL_TIMEOUT_SECONDS:.0f} for a foreground decision"
            )
    else:
        total = DEFAULT_TOTAL_TIMEOUT_SECONDS if total is None else total
        if total <= 0 or total >= 180:
            raise SystemExit("--total-timeout-seconds must be greater than 0 and less than 180")
    settings = Settings.defaults(
        tool_root=args.tool_root,
        state_root=args.state_root,
        source_checkout=args.source_checkout,
        ci_hub=args.ci_hub,
        state_dir=args.state_dir,
        total_timeout_seconds=total,
        limits=DETACHED_PHASE_LIMITS if foreground else PhaseLimits(),
    )
    settings.state_dir.mkdir(parents=True, exist_ok=True)
    if args.decide:
        return decide(settings, args.dispatch_id)
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
                if args.dry_run:
                    with decision_rust_script_cache(settings):
                        return run_tick(settings, dry_run=True)
                return gate_tick(settings)
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
