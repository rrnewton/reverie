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
import subprocess
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


#: ⚠️ MUST FIT INSIDE THE GATE'S OWN BUDGET. tick-hub.yaml gives this gate
#: timeout_secs: 10, and up to four of these reads can run in one call, so a
#: 20-second bound was larger than the whole gate and could never have fired --
#: the gate would be killed first and the refusal would read as a timeout rather
#: than as "could not verify". Two seconds each keeps the worst case inside the
#: budget. These are local object reads; if one takes longer than that, treating
#: it as unverifiable is the right answer anyway.
GIT_TIMEOUT_SECONDS = 2.0


def _git(state_root: Path, *args: str) -> str | None:
    """One bounded read-only git command, or None when it cannot be answered."""
    try:
        done = subprocess.run(
            ["git", "-C", str(state_root), *args],
            capture_output=True, text=True,
            timeout=GIT_TIMEOUT_SECONDS, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def _gitlink(repo: Path, commit: str, path: str) -> str | None:
    line = _git(repo, "ls-tree", commit, "--", path)
    if not line:
        return None
    parts = line.split()
    return parts[2] if len(parts) >= 3 and SHA.fullmatch(parts[2]) else None


def provenance_objection(
    producer: dict[str, str], expected: dict[str, str], state_root: Path
) -> str | None:
    """Why this receipt's producer cannot be trusted, or None when it can.

    ⚠️ WHAT THIS USED TO REQUIRE, AND WHY THAT WENT DARK. The reader demanded the
    receipt's three shas EQUAL its own. The producer is
    hermit-wrkslots-audit.service on an hourly timer and the reader is the health
    tick every thirty minutes; each materialises its own tool root from
    origin/main at its own moment, so the two are equal only while origin/main has
    not moved between them. Measured 2026-09-05: six distinct tool-root revisions
    inside three hours, and the audit refused on every cycle where one had landed
    in between -- "producer provenance mismatch: parent_sha" for a dev-hermit-only
    landing, all three names when the landing also moved the submodule pins. Slot
    health had no signal for hours as a result.

    ⚠️ THE REFUSAL WAS THE RIGHT DIRECTION AND IS NOT BEING SOFTENED INTO "ANY
    WELL-FORMED SHA". The property actually worth asserting is that the receipt
    came from a real landed revision of THIS repository, not from the identical
    revision the reader happens to be running. So:

      - the producer's parent_sha must be an ancestor of, or equal to, the
        reader's expected parent_sha, in the state root's own history. A
        fabricated sha, a foreign repository's sha, or a commit that never landed
        is not, and is refused.
      - the producer's hermit_sha and agent_utils_sha must be exactly the gitlinks
        recorded AT that producer commit. This is a check the old code never made
        at all: it compared them to the reader's own and so could not tell whether
        a receipt's triple was internally coherent. A receipt claiming a real
        parent commit while carrying pins that commit never had is now refused.
      - anything unresolvable -- git missing, object absent, a timeout -- is an
        objection, not a pass. A provenance check that cannot run must not read
        as verified.

    The two-hour freshness bound is unchanged and still applies on top of this.
    """
    if producer == expected:
        return None
    parent = producer["parent_sha"]
    if _git(state_root, "cat-file", "-e", f"{parent}^{{commit}}") is None:
        return (
            f"producer parent_sha {parent[:12]} is not a commit in this "
            "repository, so its provenance cannot be established"
        )
    ancestry = subprocess.run(
        ["git", "-C", str(state_root), "merge-base", "--is-ancestor",
         parent, expected["parent_sha"]],
        capture_output=True, text=True, timeout=GIT_TIMEOUT_SECONDS, check=False,
    )
    if ancestry.returncode != 0:
        return (
            f"producer parent_sha {parent[:12]} is not an ancestor of the "
            f"current {expected['parent_sha'][:12]}, so the receipt was not "
            "produced by a landed revision on this history"
        )
    recorded_hermit = _gitlink(state_root, parent, "hermit")
    if recorded_hermit != producer["hermit_sha"]:
        return (
            f"producer hermit_sha {producer['hermit_sha'][:12]} is not the "
            f"gitlink recorded at {parent[:12]} ({(recorded_hermit or 'unresolvable')[:12]})"
        )
    recorded_agent_utils = _gitlink(
        state_root / "hermit", producer["hermit_sha"], "agent-utils"
    )
    if recorded_agent_utils != producer["agent_utils_sha"]:
        return (
            f"producer agent_utils_sha {producer['agent_utils_sha'][:12]} is not "
            f"the gitlink recorded at hermit {producer['hermit_sha'][:12]} "
            f"({(recorded_agent_utils or 'unresolvable')[:12]})"
        )
    return None


def evaluate(
    path: Path,
    *,
    max_age_seconds: int,
    state_root: Path,
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
        # Shape, types and timestamps first, with no provenance opinion: the
        # provenance decision needs the repository and is made below.
        validate_receipt(result)
    except ContractError as error:
        return "NO_RESULT", f"result violates its contract: {error}", {}
    producer = {name: result["producer"][name] for name in expected}
    objection = provenance_objection(producer, expected, state_root)
    if objection is not None:
        return "NO_RESULT", f"result violates its contract: {objection}", {}

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
        "producer_parent_sha": result["producer"]["parent_sha"],
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
        state_root=state_root,
        expected_parent_sha=expected_parent,
        expected_hermit_sha=expected_hermit,
        expected_agent_utils_sha=expected_agent_utils,
    )
    return _emit(state, summary, **fields)


if __name__ == "__main__":
    raise SystemExit(main())
