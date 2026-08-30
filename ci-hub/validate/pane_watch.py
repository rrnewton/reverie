#!/usr/bin/env python3
"""Read-only Herdr observer for one ci-hub-owned validation service."""

from __future__ import annotations

import argparse
import os
import subprocess
import time
from collections.abc import Sequence
from datetime import datetime, timezone
from pathlib import Path

from run_registry import read_record, update_from_observer
import service_result
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402


TERMINAL_STATES = frozenset(("failed", "inactive"))


def service_properties(unit: str) -> dict[str, str] | None:
    result = bounded_capture.capture(
        [
            "systemctl",
            "--user",
            "show",
            unit,
            "--property=ActiveState",
            "--property=LoadState",
            "--property=SubState",
            "--property=ExecMainCode",
            "--property=ExecMainStatus",
            "--property=Result",
            "--property=InvocationID",
            "--property=MainPID",
            "--no-pager",
        ],
    )
    if result.returncode != 0:
        return None
    properties = dict(
        line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
    )
    if properties.get("LoadState") != "loaded" or not properties.get("InvocationID"):
        return None
    return properties


def proc_ppids(proc_root: Path = Path("/proc")) -> dict[int, int]:
    result: dict[int, int] = {}
    for entry in proc_root.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            raw = (entry / "stat").read_text()
            rest = raw[raw.rfind(")") + 2 :].split()
            result[int(entry.name)] = int(rest[1])
        except (FileNotFoundError, PermissionError, OSError, ValueError, IndexError):
            continue
    return result


def descendants(root_pid: int, relationships: dict[int, int]) -> set[int]:
    found = {root_pid}
    changed = True
    while changed:
        changed = False
        for pid, ppid in relationships.items():
            if ppid in found and pid not in found:
                found.add(pid)
                changed = True
    return found


def safe_ci_cgroups(root_pid: int, proc_root: Path = Path("/proc")) -> set[str]:
    groups: set[str] = set()
    for pid in descendants(root_pid, proc_ppids(proc_root)):
        try:
            for line in (proc_root / str(pid) / "cgroup").read_text().splitlines():
                path = line.rsplit(":", 1)[-1]
                if "safe-ci-" in path:
                    groups.add(path)
        except (FileNotFoundError, PermissionError, OSError):
            continue
    return groups


def emit_log(path: Path, offset: int) -> int:
    try:
        with path.open(errors="replace") as stream:
            stream.seek(offset)
            data = stream.read()
            offset = stream.tell()
    except (FileNotFoundError, OSError):
        return offset
    if data:
        print(data, end="", flush=True)
    return offset


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--unit", required=True)
    result.add_argument("--target", required=True)
    result.add_argument("--checkout", required=True, type=Path)
    result.add_argument("--log", required=True, type=Path)
    result.add_argument("--record", required=True, type=Path)
    result.add_argument("--pr", type=int)
    result.add_argument("--poll-seconds", type=float, default=1.0)
    result.add_argument("--appearance-seconds", type=float, default=30.0)
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    identity = f"PR #{args.pr}" if args.pr is not None else args.unit
    print(
        f"CI-HUB VALIDATE PANE {identity}\n"
        f"HEAD {args.target}\nUNIT {args.unit}\nCHECKOUT {args.checkout}\nLOG {args.log}",
        flush=True,
    )
    offset = 0
    seen = False
    observed_groups: set[str] = set()
    deadline = time.monotonic() + args.appearance_seconds
    final: dict[str, str] | None = None
    while True:
        offset = emit_log(args.log, offset)
        properties = service_properties(args.unit)
        if properties is None:
            if not seen and time.monotonic() < deadline:
                time.sleep(args.poll_seconds)
                continue
            break
        seen = True
        try:
            main_pid = int(properties.get("MainPID", "0"))
        except ValueError:
            main_pid = 0
        if main_pid > 0:
            new_groups = safe_ci_cgroups(main_pid) - observed_groups
            for group in sorted(new_groups):
                print(f"BOX-CGROUP {group}", flush=True)
            observed_groups.update(new_groups)
        if properties.get("ActiveState") in TERMINAL_STATES:
            final = properties
            break
        time.sleep(args.poll_seconds)

    offset = emit_log(args.log, offset)
    finished_at = datetime.now(timezone.utc).isoformat()
    try:
        durable = read_record(args.record)
    except RuntimeError:
        durable = {}
    if final is None:
        if durable.get("state") == "refused":
            classified = {
                "state": "refused",
                "result": str(durable.get("result", "launch-refused")),
                "exit_code": durable.get("exit_code"),
            }
        elif service_result.is_evidenced_terminal(durable):
            classified = {
                key: durable[key]
                for key in (
                    "state",
                    "result",
                    "exit_code",
                    "signal",
                    "executed_nodes",
                    "executed_tests",
                    "final_validate_status",
                    "scorecard_writeback",
                    "result_source",
                    "detail",
                )
                if key in durable
            }
        else:
            classified = service_result.from_run_record(args.record, durable, args.log)
    else:
        classified = service_result.from_run_record(
            args.record, durable, args.log, properties=final
        )
    durable = update_from_observer(
        args.record,
        **classified,
        finished_at=finished_at,
        observed_safe_ci_cgroups=sorted(observed_groups),
    )
    state = durable["state"]
    result = durable["result"]
    status = durable.get("exit_code")
    proof = ",".join(sorted(observed_groups)) or "UNOBSERVED"
    print(
        f"\nCI-HUB VALIDATE FINISHED state={state} result={result} "
        f"exit={status} box_cgroup={proof}",
        flush=True,
    )
    return status if isinstance(status, int) and 0 <= status <= 255 else 1


if __name__ == "__main__":
    raise SystemExit(main())
