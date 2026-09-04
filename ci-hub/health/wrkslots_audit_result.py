#!/usr/bin/env python3
"""Read the independently scheduled wrkslots audit's typed result.

This gate performs no audit work.  Missing, stale, malformed, incomplete, or
wrong-provenance evidence is NO_RESULT and therefore cannot make the ordinary
operational tick green.
"""

from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import sys
import time
from typing import Any

from wrkslots_audit_contract import (
    ContractError,
    RECEIPT_SCHEMA,
    SHA,
    validate_receipt,
)


SCHEMA = RECEIPT_SCHEMA
DEFAULT_MAX_AGE_SECONDS = 2 * 60 * 60
DEFAULT_RESULT = Path("ignored/ci-hub/wrkslots-audit/latest.json")


def _emit(state: str, summary: str, **fields: object) -> int:
    print(f"state={state}")
    for key, value in fields.items():
        print(f"{key}={value}")
    print(f"summary={summary}")
    # tick-hub reserves EX_TEMPFAIL for a gate that could not establish a
    # verdict. Any other nonzero status is an ordinary failure and would be
    # rendered as ACTIONABLE under `when: failure`, consuming its cadence.
    return 0 if state == "ok" else 1 if state == "actionable" else 75


def _no_result(reason: str) -> int:
    return _emit("NO_RESULT", f"NO_RESULT/wrkslots-audit: {reason}")


def _read(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("top level is not an object")
    return value


def _expected(name: str, supplied: str | None) -> str:
    value = supplied or os.environ.get(name, "")
    if not SHA.fullmatch(value):
        raise ValueError(f"expected provenance {name} is absent or invalid")
    return value


def evaluate(
    path: Path,
    *,
    max_age_seconds: int,
    expected_parent_sha: str,
    expected_hermit_sha: str,
    expected_agent_utils_sha: str,
    now: float | None = None,
) -> tuple[str, str, dict[str, object]]:
    if not path.is_file():
        return "NO_RESULT", f"no result at {path}: audit has never completed", {}
    try:
        result = _read(path)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        return "NO_RESULT", f"result at {path} is unreadable: {error}", {}
    expected = {
        "parent_sha": expected_parent_sha,
        "hermit_sha": expected_hermit_sha,
        "agent_utils_sha": expected_agent_utils_sha,
    }
    try:
        validate_receipt(result, expected_producer=expected)
    except ContractError as error:
        return "NO_RESULT", f"result violates its contract: {error}", {}

    observed = result.get("observed_at_epoch_seconds")
    if not isinstance(observed, (int, float)) or not math.isfinite(observed):
        return "NO_RESULT", "result has no finite observation time", {}
    current = time.time() if now is None else now
    age = current - float(observed)
    if age < -60:
        return "NO_RESULT", f"result timestamp is {-age:.0f}s in the future", {}
    if age > max_age_seconds:
        return (
            "NO_RESULT",
            f"result is {age:.0f}s old, beyond {max_age_seconds}s freshness",
            {},
        )

    verdict = result.get("verdict")
    summary = str(result["summary"])
    fields: dict[str, object] = {
        "age_secs": max(0, int(age)),
        "producer_parent_sha": expected_parent_sha,
    }
    if verdict == "OK":
        return "ok", summary, fields
    if verdict == "ACTIONABLE":
        return "actionable", summary, fields
    if verdict == "NO_RESULT":
        return "NO_RESULT", summary, fields
    return "NO_RESULT", f"result has unknown verdict {verdict!r}", {}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--result", type=Path)
    parser.add_argument("--max-age-secs", type=int, default=DEFAULT_MAX_AGE_SECONDS)
    parser.add_argument("--expected-parent-sha")
    parser.add_argument("--expected-hermit-sha")
    parser.add_argument("--expected-agent-utils-sha")
    args = parser.parse_args(argv)
    if args.max_age_secs <= 0:
        parser.error("--max-age-secs must be positive")
    state_root = Path(os.environ.get("DEV_HERMIT_PARENT", Path.cwd()))
    path = args.result or state_root / DEFAULT_RESULT
    try:
        expected_parent = _expected(
            "DEV_HERMIT_TOOL_PARENT_SHA", args.expected_parent_sha
        )
        expected_hermit = _expected(
            "DEV_HERMIT_TOOL_HERMIT_SHA", args.expected_hermit_sha
        )
        expected_agent_utils = _expected(
            "DEV_HERMIT_TOOL_AGENT_UTILS_SHA", args.expected_agent_utils_sha
        )
    except ValueError as error:
        return _no_result(str(error))
    state, summary, fields = evaluate(
        path,
        max_age_seconds=args.max_age_secs,
        expected_parent_sha=expected_parent,
        expected_hermit_sha=expected_hermit,
        expected_agent_utils_sha=expected_agent_utils,
    )
    return _emit(state, summary, **fields)


if __name__ == "__main__":
    raise SystemExit(main())
