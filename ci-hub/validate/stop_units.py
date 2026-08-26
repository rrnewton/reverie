#!/usr/bin/env python3
"""Stop detached validation units instead of merely stopping their launchers."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections.abc import Callable, Sequence
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402


UNIT_RE = re.compile(r"^validate-[A-Za-z0-9_.@:-]+\.(?:service|scope)$")
Runner = Callable[..., subprocess.CompletedProcess[str]]


def run_command(command: Sequence[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return bounded_capture.capture(command, **kwargs)


def require_validate_unit(unit: str) -> str:
    if not UNIT_RE.fullmatch(unit):
        raise ValueError(
            f"refusing non-validation unit {unit!r}; expected validate-*.service or validate-*.scope"
        )
    return unit


def list_active_units(systemctl: str, *, run: Runner = run_command) -> list[str]:
    result = run(
        [
            systemctl,
            "--user",
            "list-units",
            "--type=service",
            "--type=scope",
            "--state=running",
            "--no-legend",
            "--no-pager",
            "validate-*",
        ],
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"cannot enumerate detached validate units: {detail}")

    units: list[str] = []
    for line in result.stdout.splitlines():
        fields = line.split()
        if not fields:
            continue
        units.append(require_validate_unit(fields[0]))
    return sorted(set(units))


def is_active(systemctl: str, unit: str, *, run: Runner = run_command) -> bool:
    result = run([systemctl, "--user", "is-active", "--quiet", unit], check=False)
    if result.returncode == 0:
        return True
    if result.returncode in (3, 4):
        return False
    detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
    raise RuntimeError(f"cannot determine state of {unit}: {detail}")


def stop_units(
    systemctl: str,
    units: Sequence[str],
    *,
    run: Runner = run_command,
    dry_run: bool = False,
) -> tuple[list[str], list[str]]:
    stopped: list[str] = []
    inactive: list[str] = []
    for raw_unit in units:
        unit = require_validate_unit(raw_unit)
        if not is_active(systemctl, unit, run=run):
            inactive.append(unit)
            continue
        if dry_run:
            stopped.append(unit)
            continue
        result = run([systemctl, "--user", "stop", unit], check=False)
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
            raise RuntimeError(f"failed to stop {unit}: {detail}")
        if is_active(systemctl, unit, run=run):
            raise RuntimeError(f"stop returned success but {unit} is still active")
        stopped.append(unit)
    return stopped, inactive


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description="Stop detached validate-* user units and verify they terminated."
    )
    selection = result.add_mutually_exclusive_group(required=True)
    selection.add_argument("--all", action="store_true", help="stop every active validate-* unit")
    selection.add_argument("--unit", action="append", help="stop one exact validate-* unit")
    result.add_argument("--dry-run", action="store_true")
    result.add_argument("--systemctl", default="systemctl", help=argparse.SUPPRESS)
    return result


def main(argv: Sequence[str] | None = None, *, run: Runner = run_command) -> int:
    args = parser().parse_args(argv)
    try:
        units = list_active_units(args.systemctl, run=run) if args.all else args.unit
        stopped, inactive = stop_units(
            args.systemctl, units, run=run, dry_run=args.dry_run
        )
    except (RuntimeError, ValueError) as error:
        print(f"validate-stop: REFUSED: {error}", file=sys.stderr)
        return 2

    action = "WOULD-STOP" if args.dry_run else "STOPPED"
    for unit in stopped:
        print(f"{action} {unit}")
    for unit in inactive:
        print(f"ALREADY-INACTIVE {unit}")
    print(f"validate-stop: {action.lower()}={len(stopped)} inactive={len(inactive)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
