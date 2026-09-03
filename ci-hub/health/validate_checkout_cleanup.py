#!/usr/bin/env python3
"""Remove terminal validation checkouts even when their launchers are gone."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Mapping


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "validate"))
import start_unit  # noqa: E402


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


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--state-root", type=Path, default=ROOT)
    result.add_argument("--json", action="store_true")
    return result


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        report = start_unit.sweep_completed_checkouts(
            args.state_root.resolve(), run=start_unit.run_command
        )
    except (OSError, RuntimeError, ValueError) as error:
        summary = f"could not run validate checkout cleanup: {error}"
        if args.json:
            print(json.dumps({"state": "could-not-determine", "summary": summary}))
        else:
            print("state=could-not-determine")
            print(f"summary={summary}")
        return start_unit.EXIT_COULD_NOT_DETERMINE

    state, summary, failures = summarize(report)
    output = {
        "state": state,
        "summary": summary,
        "removed": report.get("removed", []),
        "removed_cargo_homes": report.get("removed_cargo_homes", []),
        "retained": report.get("retained", []),
        "deferred": report.get("deferred", []),
        "bookkeeping_errors": report.get("bookkeeping_errors", []),
        "cleanup_failures": len(failures),
    }
    if args.json:
        print(json.dumps(output, sort_keys=True))
    else:
        print(f"state={state}")
        print(f"summary={summary}")
        print(f"removed_checkouts={len(output['removed'])}")
        print(f"removed_cargo_homes={len(output['removed_cargo_homes'])}")
        print(f"deferred_checkouts={len(output['deferred'])}")
        print(f"cleanup_failures={len(failures)}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
