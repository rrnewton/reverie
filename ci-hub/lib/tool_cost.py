#!/usr/bin/env python3
"""Shared wall/CPU estimate and completion reporting for command-line tools."""

from __future__ import annotations

import argparse
import json
import math
import os
import signal
import subprocess
import sys
import tempfile
import time
from collections.abc import Sequence
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1


def _seconds(value: float | None) -> str:
    return "unknown" if value is None else f"{value:.3f}s"


def print_estimate(
    *, tool: str, wall_seconds: float | None, cpu_seconds: float | None, basis: str
) -> None:
    """Print an estimate, or print NOTHING when there is no estimate to give.

    ⚠️ `wall=unknown cpu=unknown basis='not measured: ...'` IS A LINE THAT CARRIES NO
    INFORMATION, AND IT PRINTED ON EVERY TICK. It is the defect class this repository
    spent the night removing from its own gates -- output that reads as a measurement
    and is not one -- sitting in the layer that reports measurements.

    ⚠️ IT IS ALSO THE RESIDUE OF A CORRECT EARLIER FIX, WHICH IS WHY IT LOOKS LIKE
    SOMETHING THAT WAS ALREADY REMOVED. `04c785fb3` ("ci-hub: reject fabricated cost
    estimates", 2026-08-03) took out INVENTED numbers and made callers declare
    `--estimate-unknown` instead of guessing. That was right, and it left this line
    saying "unknown" rather than lying. The numbers were removed; the line was not.
    Nothing reverted it and no removal was partial -- this is the second half of that
    change, arriving three weeks later.

    ⚠️ THE FACT IS KEPT, ONLY THE NOISE GOES. The JSON receipt still records
    `estimate.kind = "unknown"` with null wall and cpu, so "we had no estimate" remains
    machine-readable and auditable. And `COST ACTUAL` is untouched: it carries real
    measurements and is the line worth reading.
    """
    if wall_seconds is None and cpu_seconds is None:
        return
    print(
        f"# {tool} tool COST ESTIMATE "
        f"wall={_seconds(wall_seconds)} cpu={_seconds(cpu_seconds)} "
        f"basis={basis!r}",
        file=sys.stderr,
        flush=True,
    )


def print_actual(
    *,
    tool: str,
    wall_seconds: float,
    cpu_user_seconds: float,
    cpu_system_seconds: float,
    exit_description: str,
) -> None:
    cpu_seconds = cpu_user_seconds + cpu_system_seconds
    print(
        f"# {tool} tool COST ACTUAL "
        f"wall={_seconds(wall_seconds)} cpu={_seconds(cpu_seconds)} "
        f"cpu_user={_seconds(cpu_user_seconds)} cpu_system={_seconds(cpu_system_seconds)} "
        f"exit={exit_description}",
        file=sys.stderr,
        flush=True,
    )


def _write_actual_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(
        dir=path.parent,
        prefix=f".{path.name}.",
        suffix=".tmp",
    )
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, sort_keys=True, separators=(",", ":"))
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _actual_payload(
    *,
    tool: str,
    estimate_wall_seconds: float | None,
    estimate_cpu_seconds: float | None,
    basis: str,
    wall_seconds: float,
    cpu_user_seconds: float,
    cpu_system_seconds: float,
    exit_description: str,
) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "tool": tool,
        "estimate": {
            "kind": "unknown" if estimate_wall_seconds is None else "derived",
            "wall_seconds": estimate_wall_seconds,
            "cpu_seconds": estimate_cpu_seconds,
            "basis": basis,
        },
        "actual": {
            "wall_seconds": wall_seconds,
            "cpu_seconds": cpu_user_seconds + cpu_system_seconds,
            "cpu_user_seconds": cpu_user_seconds,
            "cpu_system_seconds": cpu_system_seconds,
            "exit": exit_description,
        },
    }


def _exit_description(status: int) -> tuple[int, str]:
    if os.WIFEXITED(status):
        code = os.WEXITSTATUS(status)
        return code, str(code)
    if os.WIFSIGNALED(status):
        sig = os.WTERMSIG(status)
        return 128 + sig, f"signal:{sig}"
    return 1, "unknown"


def run_command(
    command: Sequence[str],
    *,
    tool: str,
    estimate_wall_seconds: float | None,
    estimate_cpu_seconds: float | None,
    basis: str,
    actual_json: Path | None = None,
) -> int:
    print_estimate(
        tool=tool,
        wall_seconds=estimate_wall_seconds,
        cpu_seconds=estimate_cpu_seconds,
        basis=basis,
    )
    started = time.monotonic()
    process: subprocess.Popen[bytes] | None = None
    old_handlers: dict[int, signal.Handlers] = {}

    def forward(signum: int, _frame: object) -> None:
        if process is None:
            return
        try:
            os.killpg(process.pid, signum)
        except ProcessLookupError:
            pass

    try:
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            old_handlers[signum] = signal.getsignal(signum)
            signal.signal(signum, forward)
        try:
            process = subprocess.Popen(list(command), start_new_session=True)
        except OSError as error:
            print(f"tool-cost: cannot launch {command[0]!r}: {error}", file=sys.stderr)
            wall_seconds = time.monotonic() - started
            print_actual(
                tool=tool,
                wall_seconds=wall_seconds,
                cpu_user_seconds=0.0,
                cpu_system_seconds=0.0,
                exit_description="127",
            )
            if actual_json is not None:
                _write_actual_json(
                    actual_json,
                    _actual_payload(
                        tool=tool,
                        estimate_wall_seconds=estimate_wall_seconds,
                        estimate_cpu_seconds=estimate_cpu_seconds,
                        basis=basis,
                        wall_seconds=wall_seconds,
                        cpu_user_seconds=0.0,
                        cpu_system_seconds=0.0,
                        exit_description="127",
                    ),
                )
            return 127

        while True:
            try:
                _pid, status, usage = os.wait4(process.pid, 0)
                break
            except InterruptedError:
                continue
        code, description = _exit_description(status)
        process.returncode = code
        wall_seconds = time.monotonic() - started
        print_actual(
            tool=tool,
            wall_seconds=wall_seconds,
            cpu_user_seconds=usage.ru_utime,
            cpu_system_seconds=usage.ru_stime,
            exit_description=description,
        )
        if actual_json is not None:
            _write_actual_json(
                actual_json,
                _actual_payload(
                    tool=tool,
                    estimate_wall_seconds=estimate_wall_seconds,
                    estimate_cpu_seconds=estimate_cpu_seconds,
                    basis=basis,
                    wall_seconds=wall_seconds,
                    cpu_user_seconds=usage.ru_utime,
                    cpu_system_seconds=usage.ru_stime,
                    exit_description=description,
                ),
            )
        return code
    finally:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Print an up-front cost estimate and final wall/CPU usage around a command."
    )
    parser.add_argument("--tool", required=True, help="stable tool/operation name")
    parser.add_argument("--estimate-wall-seconds", type=float)
    parser.add_argument("--estimate-cpu-seconds", type=float)
    parser.add_argument(
        "--estimate-unknown",
        action="store_true",
        help="report wall/CPU as unknown because no defensible estimate exists",
    )
    parser.add_argument("--basis", required=True, help="parameters/history behind the estimate")
    parser.add_argument(
        "--actual-json",
        type=Path,
        help="atomically persist the estimate and final wall/CPU result as JSON",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command and args.command[0] == "--":
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required after --")
    numeric = (args.estimate_wall_seconds, args.estimate_cpu_seconds)
    if args.estimate_unknown:
        if any(value is not None for value in numeric):
            parser.error("--estimate-unknown cannot be combined with numeric estimates")
        if not args.basis.lower().startswith(("not measured:", "unknown:")):
            parser.error("an unknown estimate basis must start with 'not measured:' or 'unknown:'")
    elif any(value is None for value in numeric):
        parser.error(
            "provide both numeric estimates or use --estimate-unknown; never invent a fallback"
        )
    elif any(
        value < 0 or not math.isfinite(value) for value in numeric if value is not None
    ):
        parser.error("estimates must be finite and non-negative")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    return run_command(
        args.command,
        tool=args.tool,
        estimate_wall_seconds=args.estimate_wall_seconds,
        estimate_cpu_seconds=args.estimate_cpu_seconds,
        basis=args.basis,
        actual_json=args.actual_json,
    )


if __name__ == "__main__":
    raise SystemExit(main())
