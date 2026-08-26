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
# ⚠️ A COMMENT-SOURCED ROW IS NOT ADMISSION EVIDENCE, BUT IT IS STILL A REFUSAL.
# `approval_binding.py` reports these states; the loop below must not let one
# through merely because no `passed-review-*` label carries it. See the blocking
# branch in `evaluate` for the two heads this was measured on.
BLOCKING_STATES = ("REFUSED", "REFUSED_STALE", "UNDETERMINED")


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
        # Comment-discovered rows (label None) are REPORTED by approval_binding.py
        # so an unlabeled-but-approved PR stops reading as unreviewed. They are not
        # admission evidence here: treating one as authority would let a PR land
        # without a passed-review-* label, which is a policy change and not this
        # one. Skipping them keeps the landing boundary exactly where it was.
        #
        # ⚠️ BUT SKIPPING THEM WHOLESALE FAILED OPEN ON REFUSALS, AND THAT IS THE
        # OPPOSITE OF WHAT THE SKIP WAS FOR. Not admitting a comment APPROVAL is
        # conservative; not honouring a comment REFUSAL is permissive, and the
        # same `continue` did both. A codex lane that files
        # `CHANGES-REQUESTED-AT` and never gets a `passed-review-codex` label --
        # which is the normal state of a refusal, since nobody attests one --
        # was invisible to this gate while `approval_binding.py` reported it
        # plainly.
        #
        # MEASURED 2026-08-26 against main 9d4c998cfa5a, the only two open PRs
        # this gate authorized:
        #   hermit#2680  latest codex marker is CHANGES-REQUESTED-AT at the
        #                CURRENT head 642a2990, never withdrawn -> authorized
        #   hermit#2668  codex refusal withdrawn by hermit-001, who is the PR
        #                AUTHOR. `approval_binding` correctly refuses a
        #                self-withdrawal and still reports REFUSED_STALE; this
        #                gate authorized anyway, defeating that rule entirely
        # Both would have landed over a standing refusal. Neither is a label
        # problem, so no label change could have caught it.
        if row.get("source") == "comment":
            state = row.get("state")
            if state in BLOCKING_STATES:
                lane = str(row.get("lane") or "unknown")
                raise Refused(
                    f"the {lane} lane is {state} in a comment with no "
                    "passed-review label to carry it; a refusal blocks whether "
                    "or not anyone attested it"
                )
            continue
        if not isinstance(row, dict):
            raise Refused("approval-binding report contains a malformed row")
        report_head = exact_sha(row.get("head"), "approval report head")
        if report_head != current_head:
            raise Refused(
                f"approval report observes head {report_head}, not current head {current_head}"
            )
        lane = str(row.get("lane") or "")
        label = str(row.get("label") or "")
        state = row.get("state")
        if state == BOUND_CURRENT:
            accepted.append({"lane": lane, "label": label, "basis": BOUND_CURRENT})
            continue
        if state != SUPERSEDED:
            raise Refused(
                f"{label or lane or 'approval'} is {state or 'missing-state'}, "
                "not bound or mechanically carried"
            )
        approved_head = exact_sha(row.get("bound_to"), f"{label or lane} approved head")
        proof = prove_mechanical_rebase(repo, approved_head, current_base, current_head)
        accepted.append(
            {
                "lane": lane,
                "label": label,
                "basis": "MECHANICAL_REBASE",
                "proof": proof,
            }
        )

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
