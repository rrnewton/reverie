#!/usr/bin/env python3
"""Run the fixed-name host cleanup worker.

Drain terminal validation checkouts, then consume one bounded agent retirement batch.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
import re
import subprocess
import sys
import uuid
from pathlib import Path
from typing import Any, Callable, Mapping, Sequence


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "validate"))
import start_unit  # noqa: E402


WORKER_UNIT = "validate-checkout-cleanup-worker"
RESULT_SCHEMA = 1
MAX_SWEEPS_PER_WORKER = 64
RETIREMENT_BATCH_LIMIT = 8
RETIREMENT_WAIT_LOCK_SECONDS = 10
RETIREMENT_RESULT_SCHEMA = 1
STATE_RELATIVE = Path("ignored/ci-hub/validate-checkout-cleanup")
Runner = Callable[..., subprocess.CompletedProcess[str]]


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def state_paths(state_root: Path) -> tuple[Path, Path, Path]:
    directory = state_root / STATE_RELATIVE
    return (
        directory / "latest.json",
        directory / "progress.json",
        directory / "worker.log",
    )


def atomic_write_json(path: Path, value: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}-{uuid.uuid4().hex}")
    try:
        with temporary.open("x", encoding="utf-8") as stream:
            json.dump(dict(value), stream, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def read_result(path: Path) -> dict[str, Any] | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(
            f"cannot read cleanup worker result {path}: {error}"
        ) from error
    if not isinstance(value, dict) or value.get("schema") != RESULT_SCHEMA:
        raise RuntimeError(f"cleanup worker result has an unsupported shape: {path}")
    return value


def emit(output: Mapping[str, Any], *, as_json: bool) -> None:
    if as_json:
        print(json.dumps(dict(output), sort_keys=True))
        return
    for key, value in output.items():
        if key == "removed" and isinstance(value, list):
            print(f"removed_checkouts={len(value)}")
            continue
        if key == "removed_cargo_homes" and isinstance(value, list):
            print(f"removed_cargo_homes={len(value)}")
            continue
        if key == "deferred" and isinstance(value, list):
            print(f"deferred_checkouts={len(value)}")
            continue
        if isinstance(value, (dict, list)):
            continue
        print(f"{key}={' '.join(str(value).split())}")


def summarize(
    report: Mapping[str, Any],
) -> tuple[
    str,
    str,
    list[Mapping[str, Any]],
    list[Mapping[str, Any]],
]:
    retained = list(report.get("retained", []))
    deferred = list(report.get("deferred", []))
    active = [row for row in retained if row.get("reason") == "unit-running"]
    refusals = [
        row
        for row in retained
        if row.get("reason") != "unit-running" and row.get("cleanup_failed") is not True
    ]
    failures = [row for row in retained if row.get("cleanup_failed") is True]
    failures.extend(report.get("bookkeeping_errors", []))
    state = "failed" if failures else "ok"
    summary = (
        f"removed {len(report.get('removed', []))} checkout(s) and "
        f"{len(report.get('removed_cargo_homes', []))} Cargo home(s); "
        f"retained {len(active)} running checkout(s)"
        f"; refused {len(refusals)} path(s)"
        f"; deferred {len(deferred)} checkout(s) to a later bounded batch"
    )

    def one_line(value: object) -> str:
        return " ".join(str(value).split())

    if refusals:
        details = "; ".join(
            one_line(
                f"{row.get('checkout') or row.get('record') or '(unknown)'}: "
                f"{row.get('reason', 'unknown reason')}"
            )
            for row in refusals[:3]
        )
        if len(refusals) > 3:
            details += f"; and {len(refusals) - 3} more"
        summary += f"; refusal details: {details}"
    if failures:
        details = "; ".join(
            one_line(
                f"{row.get('checkout') or row.get('record') or '(unknown)'}: "
                f"{row.get('reason', 'unknown reason')}"
            )
            for row in failures[:3]
        )
        if len(failures) > 3:
            details += f"; and {len(failures) - 3} more"
        summary += f"; could not clean {len(failures)} path(s): {details}"
    return state, summary, refusals, failures


def report_output(
    report: Mapping[str, Any],
) -> tuple[dict[str, Any], list[Mapping[str, Any]]]:
    state, summary, refusals, failures = summarize(report)
    discovery = report.get("discovery") or {}
    return (
        {
            "state": state,
            "summary": summary,
            # WHAT DISCOVERY LOOKED AT, not only what it acted on. "removed 0"
            # is unreadable without it: a sweep that considered no candidates
            # and one that considered a hundred and could clean none print the
            # same line.
            "json_files_seen": discovery.get("json_files_seen"),
            "framework_result_sidecars_skipped": discovery.get(
                "framework_result_sidecars_skipped"
            ),
            "run_handle_candidates": discovery.get("run_handle_candidates"),
            "removed": report.get("removed", []),
            "removed_cargo_homes": report.get("removed_cargo_homes", []),
            "retained": report.get("retained", []),
            "deferred": report.get("deferred", []),
            "bookkeeping_errors": report.get("bookkeeping_errors", []),
            "cleanup_refusals": len(refusals),
            "cleanup_failures": len(failures),
        },
        failures,
    )


def run_one_sweep(state_root: Path) -> tuple[int, dict[str, Any]]:
    try:
        report = start_unit.sweep_completed_checkouts(
            state_root.resolve(), run=start_unit.run_command
        )
    except (OSError, RuntimeError, ValueError) as error:
        summary = f"could not run validate checkout cleanup: {error}"
        return start_unit.EXIT_COULD_NOT_DETERMINE, {
            "state": "could-not-determine",
            "summary": summary,
        }
    output, failures = report_output(report)
    return (1 if failures else 0), output


def retirement_failure(
    reason: str, *, provider_returncode: int | None = None
) -> dict[str, Any]:
    detail = " ".join(reason.split())
    return {
        "schema": RETIREMENT_RESULT_SCHEMA,
        "limit": RETIREMENT_BATCH_LIMIT,
        "state": "could-not-determine",
        "summary": f"agent retirement could not be determined: {detail}",
        "provider_returncode": provider_returncode,
        "removed": [],
        "retained": [],
        "deferred": [],
        "could_not_determine": [
            {
                "outcome": "could-not-determine",
                "reason": detail,
                "recovery_required": True,
            }
        ],
        "recovery_required": True,
    }


def validate_retirement_result(value: object) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("retire-pending JSON root is not an object")
    expected_top_level_fields = {
        "schema",
        "limit",
        "removed",
        "retained",
        "deferred",
        "could_not_determine",
        "recovery_required",
    }
    if set(value) != expected_top_level_fields:
        unknown = sorted(set(value) - expected_top_level_fields)
        missing = sorted(expected_top_level_fields - set(value))
        raise ValueError(
            "retire-pending JSON fields disagree with the documented schema: "
            f"unknown={unknown}, missing={missing}"
        )

    if (
        type(value.get("schema")) is not int
        or value["schema"] != RETIREMENT_RESULT_SCHEMA
    ):
        raise ValueError("retire-pending JSON has an unsupported schema")
    if type(value.get("limit")) is not int or value["limit"] != RETIREMENT_BATCH_LIMIT:
        raise ValueError("retire-pending JSON does not report the requested limit")

    categories = {
        "removed": "removed",
        "retained": "retained",
        "deferred": "deferred",
        "could_not_determine": "could-not-determine",
    }
    common_fields = {"machine", "slot", "generation", "sha256", "outcome", "reason"}
    deferred_optional_fields = {
        "event_sequence",
        "enqueued_at",
        "last_attempt_sequence",
        "artifact_path",
        "artifact_source",
        "blocked_reason",
    }
    seen: set[tuple[str, str, int, str]] = set()
    for field, expected_outcome in categories.items():
        rows = value.get(field)
        if not isinstance(rows, list):
            raise ValueError(f"retire-pending JSON field {field} is not a list")
        for index, row in enumerate(rows):
            if not isinstance(row, dict):
                raise ValueError(
                    f"retire-pending JSON {field}[{index}] is not an object"
                )
            expected_fields = common_fields | (
                {"recovery_required"} if field == "could_not_determine" else set()
            )
            if field == "deferred":
                expected_fields |= deferred_optional_fields
            if not set(row) <= expected_fields:
                unknown = sorted(set(row) - expected_fields)
                raise ValueError(
                    f"retire-pending JSON {field}[{index}] has unknown fields: {unknown}"
                )
            machine = row.get("machine")
            slot = row.get("slot")
            generation = row.get("generation")
            digest = row.get("sha256")
            reason = row.get("reason")
            if not isinstance(machine, str) or not machine:
                raise ValueError(f"retire-pending JSON {field}[{index}] has no machine")
            if not isinstance(slot, str) or not slot:
                raise ValueError(f"retire-pending JSON {field}[{index}] has no slot")
            if type(generation) is not int or generation <= 0:
                raise ValueError(
                    f"retire-pending JSON {field}[{index}] has no generation"
                )
            if (
                not isinstance(digest, str)
                or re.fullmatch(r"[0-9a-f]{64}", digest) is None
            ):
                raise ValueError(f"retire-pending JSON {field}[{index}] has no SHA-256")
            if row.get("outcome") != expected_outcome:
                raise ValueError(
                    f"retire-pending JSON {field}[{index}] has the wrong outcome"
                )
            if not isinstance(reason, str) or not reason:
                raise ValueError(f"retire-pending JSON {field}[{index}] has no reason")
            if (
                field == "could_not_determine"
                and row.get("recovery_required") is not True
            ):
                raise ValueError(
                    f"retire-pending JSON {field}[{index}] does not require recovery"
                )
            identity = (machine, slot, generation, digest)
            if identity in seen:
                raise ValueError("retire-pending JSON repeats one target outcome")
            seen.add(identity)

    attempted = sum(
        len(value[field]) for field in ("removed", "retained", "could_not_determine")
    )
    if attempted > RETIREMENT_BATCH_LIMIT:
        raise ValueError(
            "retire-pending JSON reports more attempted outcomes than its limit"
        )
    recovery_required = value.get("recovery_required")
    if type(recovery_required) is not bool:
        raise ValueError("retire-pending JSON recovery_required is not boolean")
    if recovery_required != bool(value["could_not_determine"]):
        raise ValueError(
            "retire-pending JSON recovery_required disagrees with its outcomes"
        )
    return dict(value)


def run_retirement_batch(
    state_root: Path, *, run: Runner = subprocess.run
) -> tuple[int, dict[str, Any]]:
    command = [
        str(ROOT / "ci-hub" / "bin" / "wrkslots"),
        "--project-root",
        str(state_root.resolve()),
        "--allow-existing-unregistered-worktrees",
        "--wait-lock",
        str(RETIREMENT_WAIT_LOCK_SECONDS),
        "retire-pending",
        "--limit",
        str(RETIREMENT_BATCH_LIMIT),
        "--coordinator-pid",
        str(os.getpid()),
        "--coordinator-authorized",
        "--format",
        "json",
    ]
    try:
        completed = run(command, capture_output=True, text=True, check=False)
    except OSError as error:
        return start_unit.EXIT_COULD_NOT_DETERMINE, retirement_failure(
            f"could not start retire-pending: {error}"
        )
    try:
        parsed = json.loads(completed.stdout)
        result = validate_retirement_result(parsed)
    except (json.JSONDecodeError, ValueError) as error:
        diagnostic = completed.stderr.strip() or completed.stdout.strip() or str(error)
        return start_unit.EXIT_COULD_NOT_DETERMINE, retirement_failure(
            f"invalid retire-pending result: {error}; provider said: {diagnostic[:1000]}",
            provider_returncode=completed.returncode,
        )
    recovery_required = result["recovery_required"]
    if (
        completed.returncode not in {0, 3}
        or (completed.returncode == 3) != recovery_required
    ):
        return start_unit.EXIT_COULD_NOT_DETERMINE, retirement_failure(
            "retire-pending exit status disagrees with its structured recovery outcome",
            provider_returncode=completed.returncode,
        )
    result["provider_returncode"] = completed.returncode
    result["state"] = "could-not-determine" if recovery_required else "ok"
    result["summary"] = (
        f"agent retirement removed {len(result['removed'])} slot(s); "
        f"retained {len(result['retained'])}; deferred {len(result['deferred'])}; "
        f"could not determine {len(result['could_not_determine'])}"
    )
    return (
        start_unit.EXIT_COULD_NOT_DETERMINE if recovery_required else 0,
        result,
    )


def _extend_unique(target: list[str], values: object) -> None:
    if not isinstance(values, list):
        return
    for value in values:
        if isinstance(value, str) and value not in target:
            target.append(value)


def drain(
    state_root: Path,
    *,
    as_json: bool,
    run: Runner = subprocess.run,
) -> int:
    """Run bounded sweeps until this worker drains every deferred candidate."""

    latest_path, progress_path, _log_path = state_paths(state_root)
    run_id = uuid.uuid4().hex
    started_at = now()
    removed: list[str] = []
    removed_cargo_homes: list[str] = []
    last_output: dict[str, Any] = {}
    stalled_sweeps = 0
    for sweep in range(1, MAX_SWEEPS_PER_WORKER + 1):
        atomic_write_json(
            progress_path,
            {
                "schema": RESULT_SCHEMA,
                "status": "running",
                "run_id": run_id,
                "started_at": started_at,
                "updated_at": now(),
                "sweeps_started": sweep,
                "removed": removed,
                "removed_cargo_homes": removed_cargo_homes,
            },
        )
        code, output = run_one_sweep(state_root)
        last_output = output
        _extend_unique(removed, output.get("removed"))
        _extend_unique(removed_cargo_homes, output.get("removed_cargo_homes"))
        progress_made = bool(output.get("removed")) or bool(
            output.get("removed_cargo_homes")
        )
        stalled_sweeps = 0 if progress_made else stalled_sweeps + 1
        deferred = output.get("deferred")
        terminal = not isinstance(deferred, list) or not deferred or stalled_sweeps >= 3
        if code == start_unit.EXIT_COULD_NOT_DETERMINE:
            result_state = "could-not-determine"
        else:
            result_state = (
                "failed"
                if code != 0 or (isinstance(deferred, list) and deferred)
                else "ok"
            )
        progress = {
            "schema": RESULT_SCHEMA,
            "status": (
                "running"
                if not terminal
                else "completed" if result_state == "ok" else "failed"
            ),
            "state": result_state if terminal else "running",
            "run_id": run_id,
            "started_at": started_at,
            "updated_at": now(),
            "sweeps_completed": sweep,
            "removed": removed,
            "removed_cargo_homes": removed_cargo_homes,
            "last": output,
        }
        atomic_write_json(progress_path, progress)
        if terminal:
            stopped_without_progress = bool(deferred) and stalled_sweeps >= 3
            result = {
                **progress,
                "completed_at": now(),
                "summary": (
                    f"processed {sweep} bounded sweep(s); removed {len(removed)} "
                    f"checkout(s) and {len(removed_cargo_homes)} Cargo home(s); "
                    + (
                        "deferred paths made no progress in three consecutive sweeps"
                        if stopped_without_progress
                        else str(output.get("summary", "cleanup result unavailable"))
                    )
                ),
            }
            retirement_code, retirement = run_retirement_batch(state_root, run=run)
            result["validation_state"] = result_state
            result["retirement"] = retirement
            result["retirement_removed"] = len(retirement["removed"])
            result["retirement_retained"] = len(retirement["retained"])
            result["retirement_deferred"] = len(retirement["deferred"])
            result["retirement_could_not_determine"] = len(
                retirement["could_not_determine"]
            )
            result["summary"] += f"; {retirement['summary']}"
            if retirement_code != 0:
                result["status"] = "failed"
                result["state"] = "could-not-determine"
            atomic_write_json(progress_path, result)
            atomic_write_json(latest_path, result)
            emit(result, as_json=as_json)
            if retirement_code != 0:
                return retirement_code
            if result_state == "could-not-determine":
                return start_unit.EXIT_COULD_NOT_DETERMINE
            return 0 if result_state == "ok" else 1

    validation_state = (
        "could-not-determine"
        if code == start_unit.EXIT_COULD_NOT_DETERMINE
        else "failed"
    )
    result = {
        "schema": RESULT_SCHEMA,
        "status": "failed",
        "state": validation_state,
        "run_id": run_id,
        "started_at": started_at,
        "completed_at": now(),
        "sweeps_completed": MAX_SWEEPS_PER_WORKER,
        "removed": removed,
        "removed_cargo_homes": removed_cargo_homes,
        "last": last_output,
        "summary": (
            f"cleanup still had deferred paths after {MAX_SWEEPS_PER_WORKER} "
            "bounded sweeps"
        ),
    }
    retirement_code, retirement = run_retirement_batch(state_root, run=run)
    result["validation_state"] = validation_state
    result["retirement"] = retirement
    result["retirement_removed"] = len(retirement["removed"])
    result["retirement_retained"] = len(retirement["retained"])
    result["retirement_deferred"] = len(retirement["deferred"])
    result["retirement_could_not_determine"] = len(retirement["could_not_determine"])
    result["summary"] += f"; {retirement['summary']}"
    if retirement_code != 0:
        result["state"] = "could-not-determine"
    atomic_write_json(progress_path, result)
    atomic_write_json(latest_path, result)
    emit(result, as_json=as_json)
    if retirement_code != 0:
        return retirement_code
    if validation_state == "could-not-determine":
        return start_unit.EXIT_COULD_NOT_DETERMINE
    return 1


def worker_observation(
    run: Runner = subprocess.run,
) -> str:
    command = [
        "systemctl",
        "--user",
        "show",
        f"{WORKER_UNIT}.service",
        "--no-pager",
        "--property=LoadState",
        "--property=ActiveState",
        "--property=SubState",
        "--property=MainPID",
    ]
    try:
        completed = run(command, capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError(f"cannot observe cleanup worker: {error}") from error
    if completed.returncode != 0:
        detail = completed.stderr.strip() or f"exit {completed.returncode}"
        raise RuntimeError(f"cannot observe cleanup worker: {' '.join(detail.split())}")
    properties: dict[str, str] = {}
    for line in completed.stdout.splitlines():
        name, separator, value = line.partition("=")
        if not separator or name in properties:
            raise RuntimeError("systemctl returned malformed cleanup worker properties")
        properties[name] = value
    if set(properties) != {"LoadState", "ActiveState", "SubState", "MainPID"}:
        raise RuntimeError("systemctl omitted cleanup worker properties")
    if properties["LoadState"] == "not-found" and properties["MainPID"] == "0":
        return "absent"
    if properties["LoadState"] == "loaded" and properties["ActiveState"] in {
        "active",
        "activating",
        "deactivating",
        "reloading",
    }:
        return "active"
    if properties["LoadState"] == "loaded" and properties["ActiveState"] in {
        "inactive",
        "failed",
    }:
        return "terminal"
    raise RuntimeError(f"cleanup worker has uncertain state: {properties}")


def launch_worker(
    state_root: Path, log_path: Path, run: Runner = subprocess.run
) -> None:
    operational_tool = os.environ.get("DEV_HERMIT_OPERATIONAL_TOOL", "")
    parent_sha = os.environ.get("DEV_HERMIT_TOOL_PARENT_SHA", "")
    if (
        not operational_tool
        or not Path(operational_tool).is_file()
        or not os.access(operational_tool, os.X_OK)
        or re.fullmatch(r"[0-9a-f]{40}", parent_sha) is None
    ):
        raise RuntimeError(
            "verified installed operational-tool or exact parent tool SHA is unavailable"
        )
    log_path.parent.mkdir(parents=True, exist_ok=True)
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--collect",
        f"--unit={WORKER_UNIT}",
        "--description=bounded validation checkout cleanup",
        f"--property=StandardOutput=append:{log_path}",
        f"--property=StandardError=append:{log_path}",
        f"--working-directory={state_root}",
        f"--setenv=PATH={os.environ.get('PATH', '')}",
        f"--setenv=DEV_HERMIT_PARENT={state_root}",
        "--",
        operational_tool,
        "--run-current-tool",
        "--state-root",
        str(state_root),
        "--tool-parent-sha",
        parent_sha,
        "--tool-relative",
        "scripts/validate-checkout-cleanup-worker",
        "--",
        str(state_root),
    ]
    try:
        completed = run(command, capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError(f"cannot launch cleanup worker: {error}") from error
    if completed.returncode != 0:
        detail = (
            completed.stderr.strip()
            or completed.stdout.strip()
            or f"exit {completed.returncode}"
        )
        raise RuntimeError(
            f"cleanup worker launch was refused: {' '.join(detail.split())}"
        )


def controller(state_root: Path, *, as_json: bool, run: Runner = subprocess.run) -> int:
    latest_path, progress_path, log_path = state_paths(state_root)
    try:
        observation = worker_observation(run)
        latest = read_result(latest_path)
        progress = read_result(progress_path)
    except RuntimeError as error:
        output = {"state": "could-not-determine", "summary": str(error)}
        emit(output, as_json=as_json)
        return start_unit.EXIT_COULD_NOT_DETERMINE

    if observation == "active":
        if latest is None:
            output = {
                "state": "could-not-determine",
                "summary": "cleanup worker is active; no completed worker result is available yet",
            }
            emit(output, as_json=as_json)
            return start_unit.EXIT_COULD_NOT_DETERMINE
        output = {
            "state": "ok" if latest.get("status") == "completed" else "failed",
            "summary": f"cleanup worker is active; prior result: {latest.get('summary', 'unavailable')}",
            "worker": "active",
        }
        emit(output, as_json=as_json)
        return 0 if output["state"] == "ok" else 1

    interrupted = (
        progress is not None
        and progress.get("status") == "running"
        and (latest is None or progress.get("run_id") != latest.get("run_id"))
    )
    try:
        launch_worker(state_root, log_path, run)
    except RuntimeError as error:
        # A second launcher can win between observation and start. Re-observe
        # once so that harmless contention is not reported as a dead worker.
        try:
            if worker_observation(run) == "active":
                output = {
                    "state": "could-not-determine" if latest is None else "ok",
                    "summary": "cleanup worker is active after a concurrent launch",
                    "worker": "active",
                }
                emit(output, as_json=as_json)
                return start_unit.EXIT_COULD_NOT_DETERMINE if latest is None else 0
        except RuntimeError:
            pass
        output = {"state": "could-not-determine", "summary": str(error)}
        emit(output, as_json=as_json)
        return start_unit.EXIT_COULD_NOT_DETERMINE

    if interrupted:
        output = {
            "state": "failed",
            "summary": "the prior cleanup worker ended without a terminal result; started a replacement",
            "worker": "started",
        }
        emit(output, as_json=as_json)
        return 1
    if latest is None:
        output = {
            "state": "could-not-determine",
            "summary": "started cleanup worker; no completed worker result is available yet",
            "worker": "started",
        }
        emit(output, as_json=as_json)
        return start_unit.EXIT_COULD_NOT_DETERMINE
    output = {
        "state": "ok" if latest.get("status") == "completed" else "failed",
        "summary": f"started cleanup worker; prior result: {latest.get('summary', 'unavailable')}",
        "worker": "started",
    }
    emit(output, as_json=as_json)
    return 0 if output["state"] == "ok" else 1


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    result.add_argument("--state-root", type=Path, default=ROOT)
    result.add_argument("--json", action="store_true")
    mode = result.add_mutually_exclusive_group()
    mode.add_argument(
        "--sweep",
        action="store_true",
        help="run one synchronous bounded sweep (diagnostics and tests)",
    )
    mode.add_argument(
        "--drain",
        action="store_true",
        help="worker mode: repeat bounded sweeps until no candidate is deferred",
    )
    return result


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    state_root = args.state_root.resolve()
    if args.drain:
        return drain(state_root, as_json=args.json)
    if args.sweep:
        code, output = run_one_sweep(state_root)
        emit(output, as_json=args.json)
        return code
    return controller(state_root, as_json=args.json)


if __name__ == "__main__":
    raise SystemExit(main())
