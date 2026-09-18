#!/usr/bin/env python3
"""Remove terminal validation checkouts even when their launchers are gone."""

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
STATE_RELATIVE = Path("ignored/ci-hub/validate-checkout-cleanup")
Runner = Callable[..., subprocess.CompletedProcess[str]]


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def state_paths(state_root: Path) -> tuple[Path, Path, Path]:
    directory = state_root / STATE_RELATIVE
    return directory / "latest.json", directory / "progress.json", directory / "worker.log"


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
        raise RuntimeError(f"cannot read cleanup worker result {path}: {error}") from error
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


def summarize(report: Mapping[str, Any]) -> tuple[str, str, list[Mapping[str, str]]]:
    retained = list(report.get("retained", []))
    deferred = list(report.get("deferred", []))
    active = [row for row in retained if row.get("reason") == "unit-running"]
    failures = [row for row in retained if row.get("reason") != "unit-running"]
    failures.extend(report.get("bookkeeping_errors", []))
    state = "failed" if failures else "ok"
    summary = (
        f"removed {len(report.get('removed', []))} checkout(s) and "
        f"{len(report.get('removed_cargo_homes', []))} Cargo home(s); "
        f"retained {len(active)} running checkout(s)"
        f"; deferred {len(deferred)} checkout(s) to a later bounded batch"
    )
    if failures:
        def one_line(value: object) -> str:
            return " ".join(str(value).split())

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
    return state, summary, failures


def report_output(report: Mapping[str, Any]) -> tuple[dict[str, Any], list[Mapping[str, str]]]:
    state, summary, failures = summarize(report)
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


def _extend_unique(target: list[str], values: object) -> None:
    if not isinstance(values, list):
        return
    for value in values:
        if isinstance(value, str) and value not in target:
            target.append(value)


def drain(state_root: Path, *, as_json: bool) -> int:
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
        terminal = (
            not isinstance(deferred, list)
            or not deferred
            or stalled_sweeps >= 3
        )
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
                else "completed"
                if result_state == "ok"
                else "failed"
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
            atomic_write_json(latest_path, result)
            emit(result, as_json=as_json)
            return 0 if result_state == "ok" else 1

    result = {
        "schema": RESULT_SCHEMA,
        "status": "failed",
        "state": "failed",
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
    atomic_write_json(progress_path, result)
    atomic_write_json(latest_path, result)
    emit(result, as_json=as_json)
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


def launch_worker(state_root: Path, log_path: Path, run: Runner = subprocess.run) -> None:
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
        detail = completed.stderr.strip() or completed.stdout.strip() or f"exit {completed.returncode}"
        raise RuntimeError(f"cleanup worker launch was refused: {' '.join(detail.split())}")


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
