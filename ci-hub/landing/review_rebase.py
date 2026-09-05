#!/usr/bin/env python3
"""Decide whether recorded reviews still apply after a mechanical rebase.

The approval-binding consumer remains the authority for which review families
approved which commits.  This helper adds one narrow alternative to an exact
current-head binding: every superseded approved commit must have an empty
``git range-diff`` against the current head after both series are expressed
relative to their respective bases.

Anything else refuses.  In particular, an absent binding, a merge commit, a
different commit count, a changed patch, or a semantic conflict resolution can
never turn a stale approval into landing authority.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402


SHA40 = re.compile(r"^[0-9a-f]{40}$")
UNCHANGED_RANGE_ROW = re.compile(
    r"^\s*\d+:\s+[0-9a-f]+\s+=\s+\d+:\s+[0-9a-f]+(?:\s+.*)?$"
)
BOUND_CURRENT = "BOUND_CURRENT"
SUPERSEDED = "SUPERSEDED"
REVIEW_LANES = frozenset({"claude", "codex"})


class Refused(RuntimeError):
    """The recorded review does not cover the candidate patch series."""


class ReviewCheckError(RuntimeError):
    """The review applicability check could not be completed."""


def git(repo: Path, *args: str, check: bool = True) -> bounded_capture.CapturedProcess:
    result = bounded_capture.git(args, cwd=repo)
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout).strip() or f"exit {result.returncode}"
        raise ReviewCheckError(f"git {' '.join(args)}: {detail}")
    return result


def exact_sha(value: Any, role: str) -> str:
    text = str(value or "")
    if not SHA40.fullmatch(text):
        raise Refused(f"{role} is not an exact lowercase 40-hex commit: {text!r}")
    return text


def range_count(repo: Path, base: str, tip: str) -> int:
    raw = git(repo, "rev-list", "--count", f"{base}..{tip}").stdout.strip()
    try:
        return int(raw)
    except ValueError as error:
        raise ReviewCheckError(f"git rev-list returned a non-integer count: {raw!r}") from error


def merge_commits(repo: Path, base: str, tip: str) -> list[str]:
    output = git(repo, "rev-list", "--min-parents=2", f"{base}..{tip}").stdout
    return [line for line in output.splitlines() if line]


def prove_mechanical_rebase(
    repo: Path, approved_head: str, current_base: str, current_head: str
) -> dict[str, Any]:
    old_base = git(repo, "merge-base", approved_head, current_base).stdout.strip()
    exact_sha(old_base, "approved series base")

    ancestry = git(
        repo,
        "merge-base",
        "--is-ancestor",
        current_base,
        current_head,
        check=False,
    )
    if ancestry.returncode != 0:
        raise Refused(
            f"current base {current_base} is not an ancestor of current head {current_head}"
        )

    old_merges = merge_commits(repo, old_base, approved_head)
    new_merges = merge_commits(repo, current_base, current_head)
    if old_merges or new_merges:
        raise Refused(
            "mechanical review carry requires linear patch series; "
            f"old merges={len(old_merges)} new merges={len(new_merges)}"
        )

    old_count = range_count(repo, old_base, approved_head)
    new_count = range_count(repo, current_base, current_head)
    if old_count == 0 or old_count != new_count:
        raise Refused(
            "patch-series commit counts differ: "
            f"approved={old_count} current={new_count}"
        )

    command = [
        "range-diff",
        "--no-color",
        "--no-patch",
        "--exit-code",
        f"{old_base}..{approved_head}",
        f"{current_base}..{current_head}",
    ]
    result = git(repo, *command, check=False)
    if result.returncode != 0:
        detail = (result.stdout or result.stderr).strip() or f"exit {result.returncode}"
        raise Refused(f"range-diff is non-empty or unavailable (rc={result.returncode}): {detail}")

    rows = [line for line in result.stdout.splitlines() if line.strip()]
    if len(rows) != old_count or any(
        UNCHANGED_RANGE_ROW.fullmatch(line) is None for line in rows
    ):
        raise Refused(
            "range-diff is non-empty: it did not emit one unchanged "
            "correspondence row per patch: "
            f"rows={len(rows)} patches={old_count}; output={' | '.join(rows)}"
        )
    return {
        "approved_head": approved_head,
        "approved_base": old_base,
        "current_base": current_base,
        "current_head": current_head,
        "commits": old_count,
        "range_diff": rows,
    }


def evaluate(
    report: dict[str, Any], repo: Path, current_base: str, current_head: str
) -> dict[str, Any]:
    exact_sha(current_base, "current base")
    exact_sha(current_head, "current head")
    if report.get("approval_set_missing") or report.get("approval_sets_missing"):
        raise Refused("the PR has no required approval set")
    if report.get("critical_label_missing"):
        raise Refused("the critical-change routing label is missing")
    missing_lanes = report.get("critical_lanes_missing") or []
    if missing_lanes:
        raise Refused("critical review families missing: " + ",".join(missing_lanes))

    rows = report.get("rows")
    if not isinstance(rows, list) or not rows:
        raise Refused("approval-binding report contains no approval rows")

    accepted: list[dict[str, Any]] = []
    for row in rows:
        if not isinstance(row, dict):
            raise Refused("approval-binding report contains a malformed row")
        report_head = exact_sha(row.get("head"), "approval report head")
        if report_head != current_head:
            raise Refused(
                f"approval report observes head {report_head}, not current head {current_head}"
            )
        lane = str(row.get("lane") or "")
        state = row.get("state")
        if lane not in REVIEW_LANES:
            continue
        if state == BOUND_CURRENT:
            accepted.append(
                {
                    "lane": lane,
                    "basis": BOUND_CURRENT,
                }
            )
            continue
        # approval_binding derives state and bound_to from canonical review
        # records. The mutable label and its source field are cache presentation
        # only, so neither participates in this mechanical carry decision or in
        # its authoritative result.
        if state != SUPERSEDED:
            raise Refused(
                f"{lane or 'approval'} is {state or 'missing-state'}, "
                "not bound or mechanically carried"
            )
        approved_head = exact_sha(row.get("bound_to"), f"{lane} approved head")
        proof = prove_mechanical_rebase(repo, approved_head, current_base, current_head)
        accepted.append(
            {
                "lane": lane,
                "basis": "MECHANICAL_REBASE",
                "proof": proof,
            }
        )

    if not accepted:
        raise Refused("approval-binding report contains no accepted review approval")

    return {
        "authorized": True,
        "state": "REVIEW_APPLIES",
        "head": current_head,
        "current_base": current_base,
        "approvals": accepted,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--approval-json", required=True)
    parser.add_argument("--repo-checkout", required=True)
    parser.add_argument("--current-base", required=True)
    parser.add_argument("--current-head", required=True)
    args = parser.parse_args()

    try:
        report = json.loads(Path(args.approval_json).read_text())
        if not isinstance(report, dict):
            raise ReviewCheckError("approval-binding JSON is not an object")
        verdict = evaluate(
            report,
            Path(args.repo_checkout).resolve(),
            args.current_base,
            args.current_head,
        )
    except Refused as error:
        print(f"review-rebase: REFUSED: {error}", file=sys.stderr)
        return 2
    except (OSError, json.JSONDecodeError, ReviewCheckError) as error:
        print(f"review-rebase: ERROR: {error}", file=sys.stderr)
        return 3

    print(json.dumps(verdict, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
