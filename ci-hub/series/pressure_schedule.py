#!/usr/bin/env python3
"""Launch one bounded recurring pressure-test sample without overlapping validate."""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import json
import os
import re
import secrets
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Callable, Mapping, Sequence


SOURCE_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SOURCE_ROOT / "ci-hub" / "series"))
sys.path.insert(0, str(SOURCE_ROOT / "ci-hub" / "validate"))
import series  # noqa: E402
import run_registry  # noqa: E402


STATE_ROOT = Path(
    os.environ.get("DEV_HERMIT_PARENT", series.shared_parent_root(SOURCE_ROOT))
).resolve()
UNIT = "hermit-pressure-test"
SAMPLE_COUNT = 10
CELL_TIMEOUT_SECONDS = 60
RUN_TIMEOUT_SECONDS = 1500
LOCK_WAIT_SECONDS = 3600
LOCK_CHILD_DEADLINE_SECONDS = 1800
UNIT_RUNTIME_SECONDS = LOCK_WAIT_SECONDS + LOCK_CHILD_DEADLINE_SECONDS + 300
SHA_RE = re.compile(r"[0-9a-f]{40}")


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def state_path(root: Path) -> Path:
    return root / "ignored" / "ci-hub" / "pressure-test-state.json"


def log_path(root: Path) -> Path:
    return root / "ignored" / "ci-hub" / "pressure-test.log"


def launch_lock_path(root: Path) -> Path:
    return root / "ignored" / "ci-hub" / "pressure-test-launch.lock"


def emit(state: str, summary: str) -> None:
    print(f"state={state}")
    print(f"summary={summary}")


def write_state(root: Path, value: dict) -> None:
    path = state_path(root)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f"{path.name}.tmp-{os.getpid()}")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, path)


def read_state(root: Path) -> dict | None:
    try:
        value = json.loads(state_path(root).read_text())
    except (OSError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def launch_command(
    root: Path,
    systemd_run: str,
    with_proxy: str,
    target: str,
    record: Path,
    environment: Mapping[str, str] | None = None,
) -> list[str]:
    environment = os.environ if environment is None else environment
    log = log_path(root)
    operational_tool = environment.get("DEV_HERMIT_OPERATIONAL_TOOL", "")
    parent_sha = environment.get("DEV_HERMIT_TOOL_PARENT_SHA", "")
    cache_root = environment.get("DEV_HERMIT_TOOL_CACHE", "")
    has_handoff = bool(operational_tool or parent_sha or cache_root)
    if has_handoff:
        if (
            not operational_tool
            or not os.path.isabs(operational_tool)
            or not os.path.isfile(operational_tool)
            or not os.access(operational_tool, os.X_OK)
            or SHA_RE.fullmatch(parent_sha) is None
            or not cache_root
            or not os.path.isabs(cache_root)
            or not os.path.isdir(cache_root)
        ):
            raise ValueError(
                "incomplete operational-tool handoff; recurring pressure test "
                "not launched"
            )
        working_directory = root
        worker = [
            operational_tool,
            "--run-current-tool",
            "--state-root",
            str(root),
            "--cache-root",
            cache_root,
            "--tool-parent-sha",
            parent_sha,
            "--tool-relative",
            "ci-hub/series/pressure_schedule.py",
            "--",
        ]
    else:
        working_directory = SOURCE_ROOT
        worker = [
            sys.executable,
            str(SOURCE_ROOT / "ci-hub" / "series" / "pressure_schedule.py"),
        ]

    return [
        systemd_run,
        "--user",
        "--quiet",
        "--collect",
        f"--unit={UNIT}",
        "--description=run one recurring Hermit pressure-test sample",
        f"--property=RuntimeMaxSec={UNIT_RUNTIME_SECONDS}",
        f"--property=StandardOutput=append:{log}",
        f"--property=StandardError=append:{log}",
        f"--working-directory={working_directory}",
        f"--setenv=PATH={environment.get('PATH', '')}",
        "--setenv=PYTHONUNBUFFERED=1",
        f"--setenv=DEV_HERMIT_PARENT={root}",
        f"--setenv=DEV_HERMIT_TOOL_ROOT={SOURCE_ROOT}",
        "--",
        with_proxy,
        *worker,
        "_run",
        "--target",
        target,
        "--record",
        str(record),
        "--state-root",
        str(root),
    ]


def locked_command(
    root: Path,
    checkout: Path,
    target: str,
    results: Path,
    record: Path,
) -> list[str]:
    return [
        str(SOURCE_ROOT / "ci-hub" / "ci-hub"),
        "validate-lock",
        "run",
        "--agent",
        "tick-pressure-test",
        "--kind",
        "bench",
        "--target",
        target,
        "--run-record",
        str(record),
        "--wait",
        str(LOCK_WAIT_SECONDS),
        "--hold",
        "600",
        "--child-deadline",
        str(LOCK_CHILD_DEADLINE_SECONDS),
        "--",
        sys.executable,
        str(SOURCE_ROOT / "ci-hub" / "series" / "pressure_schedule.py"),
        "_locked",
        "--checkout",
        str(checkout),
        "--results",
        str(results),
        "--state-root",
        str(root),
    ]


def pressure_commands(checkout: Path, results: Path) -> list[list[str]]:
    pressure = str(checkout / "ci" / "compat-envelope" / "pressure-test.rs")
    return [
        [
            pressure,
            "run",
            "--results",
            str(results),
            "--sample",
            str(SAMPLE_COUNT),
            "--jobs",
            "4",
            "--cell-timeout",
            str(CELL_TIMEOUT_SECONDS),
            "--run-timeout",
            str(RUN_TIMEOUT_SECONDS),
        ],
        [pressure, "emit-series", "--results", str(results)],
    ]


def launch(
    root: Path = STATE_ROOT,
    run: Callable[..., subprocess.CompletedProcess] = subprocess.run,
) -> int:
    log = log_path(root)
    log.parent.mkdir(parents=True, exist_ok=True)
    systemctl = shutil.which("systemctl")
    systemd_run = shutil.which("systemd-run")
    with_proxy = shutil.which("with-proxy")
    missing = [
        name
        for name, value in (
            ("systemctl", systemctl),
            ("systemd-run", systemd_run),
            ("with-proxy", with_proxy),
        )
        if value is None
    ]
    if missing:
        emit(
            "unavailable",
            "recurring pressure test not launched; missing " + ",".join(missing),
        )
        return 1

    lock_path = launch_lock_path(root)
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+") as launch_lock:
        try:
            fcntl.flock(launch_lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            emit(
                "running",
                f"recurring pressure test launch already in progress as {UNIT}",
            )
            return 0

        active = run(
            [systemctl, "--user", "is-active", "--quiet", f"{UNIT}.service"],
            capture_output=True,
            text=True,
            check=False,
        )
        if active.returncode == 0:
            emit("running", f"recurring pressure test already active as {UNIT}")
            return 0
        if active.returncode not in (3, 4):
            detail = (
                active.stderr or active.stdout
            ).strip() or f"exit {active.returncode}"
            emit(
                "unavailable",
                "recurring pressure test unit state could not be read: " + detail,
            )
            return 1

        fetched = run(
            [
                with_proxy,
                "git",
                "-C",
                str(root / "hermit"),
                "fetch",
                "--no-tags",
                "origin",
                "main",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if fetched.returncode != 0:
            detail = (fetched.stderr or fetched.stdout).strip() or "no output"
            emit(
                "refused",
                f"recurring pressure test could not fetch Hermit main: {detail}",
            )
            return 1
        resolved = run(
            ["git", "-C", str(root / "hermit"), "rev-parse", "origin/main"],
            capture_output=True,
            text=True,
            check=False,
        )
        target = resolved.stdout.strip()
        if resolved.returncode != 0 or re.fullmatch(r"[0-9a-f]{40}", target) is None:
            detail = resolved.stderr.strip() or target or f"exit {resolved.returncode}"
            emit(
                "refused",
                f"recurring pressure test could not resolve Hermit main: {detail}",
            )
            return 1

        record = run_registry.record_path(root, UNIT)
        try:
            command = launch_command(root, systemd_run, with_proxy, target, record)
        except ValueError as error:
            emit("refused", str(error))
            return 1

        previous = read_state(root)
        started_at = utc_now()
        slot = (
            f"pressure-{target[:12]}-"
            f"{dt.datetime.now(dt.timezone.utc).strftime('%Y%m%d%H%M%S')}-"
            f"{secrets.token_hex(4)}"
        )
        run_registry.write_current_record(
            record,
            {
                "schema_version": run_registry.SCHEMA_VERSION,
                "kind": "bench",
                "state": "launching",
                "unit": f"{UNIT}.service",
                "target": target,
                "repo": "rrnewton/hermit",
                "checkout": str(root / "worktrees" / "validate" / slot),
                "log": str(log),
                "agent": "tick-pressure-test",
                "started_at": started_at,
                "producer": run_registry.PRODUCER,
                "admission": "ci-hub validate-lock",
            },
        )
        started = run(
            command,
            capture_output=True,
            text=True,
            check=False,
        )
        if started.returncode != 0:
            detail = (started.stderr or started.stdout).strip() or "no output"
            run_registry.update_record(
                record,
                state="refused",
                result="systemd-launch-refused",
                exit_code=started.returncode,
                detail=detail,
                finished_at=utc_now(),
            )
            emit("refused", f"recurring pressure test was not launched: {detail}")
            return 1
        run_registry.update_record(record, state="running")

        prior = ""
        if previous and previous.get("state") not in (None, "passed"):
            prior = (
                f"; previous state={previous.get('state')} "
                f"target={previous.get('target', '-')}"
            )
        emit(
            "launched",
            f"recurring pressure test launched as {UNIT}; "
            f"output appended to {log}{prior}",
        )
        return 1 if prior else 0


def remove_worktree(root: Path, slot: str) -> str | None:
    try:
        completed = subprocess.run(
            [
                str(SOURCE_ROOT / "ci-hub/bin/wrkslots"),
                # Executable authority is the immutable/current tool checkout;
                # --project-root below keeps all mutable worktree state in root.
                "--project-root",
                str(root),
                "--allow-existing-unregistered-worktrees",
                "remove",
                slot,
                "--validate-complete",
                "--coordinator-pid",
                str(os.getpid()),
                "--expected-generation",
                "1",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        return f"could not launch wrkslots remove: {error}"
    if completed.returncode == 0:
        return None
    return (
        completed.stderr.strip()
        or completed.stdout.strip()
        or f"exit {completed.returncode}"
    )


def run_campaign(target: str, record: Path, root: Path = STATE_ROOT) -> int:
    durable = run_registry.read_record(record)
    raw_checkout = durable.get("checkout")
    if not isinstance(raw_checkout, str):
        raise RuntimeError(
            "pressure-test run record lacks its wrkslots checkout; "
            "state: REFUSED -- no worktree was created and no pressure test ran. "
            "remedy: relaunch the recurring pressure test to write a complete run record"
        )
    checkout = Path(raw_checkout)
    slot = checkout.name
    if checkout.exists():
        raise RuntimeError(f"refusing existing pressure-test checkout {checkout}")

    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    results = root / "ignored" / "compat-envelope" / f"pressure-{target[:12]}-{stamp}"

    state = {
        "schema": 1,
        "state": "running",
        "started_at": utc_now(),
        "target": target,
        "results": str(results),
    }
    write_state(root, state)
    added = subprocess.run(
        [
            str(SOURCE_ROOT / "ci-hub/bin/wrkslots"),
            "--project-root",
            str(root),
            "--allow-existing-unregistered-worktrees",
            "create",
            slot,
            "--slot-type",
            "validate",
            "--coordinator-authorized",
            "--agent",
            "tick-pressure-test",
            "--task",
            "recurring-pressure-test",
            "--purpose",
            "run the scheduled compatibility pressure measurement",
            "--owner-pid",
            str(os.getpid()),
            "--coordinator-pid",
            str(os.getpid()),
            "--repo",
            "checkout=hermit",
            "--start",
            f"checkout={target}",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if added.returncode != 0:
        detail = (
            added.stderr.strip() or added.stdout.strip() or f"exit {added.returncode}"
        )
        state.update(
            state="failed", finished_at=utc_now(), detail=f"wrkslots create: {detail}"
        )
        write_state(root, state)
        raise RuntimeError(state["detail"])

    cleanup_error = None
    try:
        submodules = subprocess.run(
            [
                "git",
                "-C",
                str(checkout),
                "submodule",
                "update",
                "--init",
                "--recursive",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if submodules.returncode != 0:
            detail = submodules.stderr.strip() or submodules.stdout.strip()
            raise RuntimeError(
                f"cannot materialize Hermit submodules: {detail or submodules.returncode}"
            )
        completed = subprocess.run(
            locked_command(root, checkout, target, results, record), check=False
        )
        state.update(
            state="passed" if completed.returncode == 0 else "failed",
            finished_at=utc_now(),
            exit_code=completed.returncode,
        )
    except Exception as error:
        state.update(state="failed", finished_at=utc_now(), detail=str(error))
    finally:
        cleanup_error = remove_worktree(root, slot)
        if cleanup_error:
            state["cleanup_error"] = cleanup_error
            state["state"] = "failed"
            if state.get("exit_code") in (None, 0):
                state["exit_code"] = 1
        write_state(root, state)
        run_registry.update_record(
            record,
            state="completed" if state["state"] == "passed" else "failed",
            result=state["state"],
            exit_code=state.get("exit_code", 1),
            detail=state.get("detail"),
            finished_at=state["finished_at"],
            results=str(results),
            cleanup_error=cleanup_error,
        )

    emit(
        state["state"],
        f"target={target} results={results}"
        + (f" detail={state['detail']}" if state.get("detail") else "")
        + (f" cleanup_error={cleanup_error}" if cleanup_error else ""),
    )
    return 0 if state["state"] == "passed" else 1


def run_locked(checkout: Path, results: Path, root: Path = STATE_ROOT) -> int:
    environment = dict(os.environ)
    environment["DEV_HERMIT_PARENT"] = str(root)
    run_command, emit_command = pressure_commands(checkout, results)
    ran = subprocess.run(run_command, cwd=checkout, env=environment, check=False)
    emitted = None
    if (results / "run.json").is_file():
        emitted = subprocess.run(
            emit_command, cwd=checkout, env=environment, check=False
        )
    if ran.returncode != 0:
        return ran.returncode
    if emitted is None:
        print(
            "pressure_schedule: run produced no run.json; series was not emitted",
            file=sys.stderr,
        )
        return 2
    return emitted.returncode


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    if argv[:1] == ["_run"]:
        worker = argparse.ArgumentParser(add_help=False)
        worker.set_defaults(command="_run")
        worker.add_argument("_command")
        worker.add_argument("--target", required=True)
        worker.add_argument("--record", type=Path, required=True)
        worker.add_argument("--state-root", type=Path, default=STATE_ROOT)
        return worker.parse_args(argv)
    if argv[:1] == ["_locked"]:
        locked = argparse.ArgumentParser(add_help=False)
        locked.set_defaults(command="_locked")
        locked.add_argument("_command")
        locked.add_argument("--checkout", type=Path, required=True)
        locked.add_argument("--results", type=Path, required=True)
        locked.add_argument("--state-root", type=Path, default=STATE_ROOT)
        return locked.parse_args(argv)
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    launch_parser = subparsers.add_parser("launch", help="launch one detached sample")
    launch_parser.add_argument("--state-root", type=Path, default=STATE_ROOT)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.command == "launch":
        return launch(args.state_root)
    if args.command == "_run":
        try:
            return run_campaign(args.target, args.record, args.state_root)
        except RuntimeError as error:
            try:
                run_registry.update_record(
                    args.record,
                    state="failed",
                    result="failed",
                    exit_code=1,
                    detail=str(error),
                    finished_at=utc_now(),
                )
            except RuntimeError as record_error:
                emit(
                    "failed",
                    f"{error}; run record update also failed: {record_error}",
                )
                return 1
            emit("failed", str(error))
            return 1
    if args.command == "_locked":
        return run_locked(args.checkout, args.results, args.state_root)
    raise AssertionError(args.command)


if __name__ == "__main__":
    raise SystemExit(main())
