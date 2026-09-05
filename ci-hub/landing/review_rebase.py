#!/usr/bin/env python3
"""Decide whether recorded reviews still apply after a mechanical rebase.

The approval-binding consumer remains the authority for which review families
approved which commits.  This helper adds one narrow alternative to an exact
current-head binding: every superseded approved commit must have an empty
``git range-diff`` against the current head after both series are expressed
relative to their respective bases. A critical-class attestation moves across
a head change only through that same proof and its canonical GitHub record.

Anything else refuses.  In particular, an absent binding, a merge commit, a
different commit count, a changed patch, or a semantic conflict resolution can
never turn a stale approval into landing authority or remove a critical review
requirement.
"""

from __future__ import annotations

import argparse
import datetime
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "health"))
import bounded_capture  # noqa: E402
import approval_binding as approval_authority  # noqa: E402


SHA40 = re.compile(r"^[0-9a-f]{40}$")
UNCHANGED_RANGE_ROW = re.compile(
    r"^\s*\d+:\s+[0-9a-f]+\s+=\s+\d+:\s+[0-9a-f]+(?:\s+.*)?$"
)
BOUND_CURRENT = "BOUND_CURRENT"
SUPERSEDED = "SUPERSEDED"
UNDETERMINED = "UNDETERMINED"
REFUSED = "REFUSED"
REFUSED_STALE = "REFUSED_STALE"
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


def critical_requirement(
    report: dict[str, Any],
    repo: Path,
    current_base: str,
    current_head: str,
    proof_cache: dict[str, dict[str, Any]],
) -> tuple[bool, list[dict[str, Any]]]:
    """Determine criticality from canonical evidence.

    An old-head attestation is accepted only when the rebase is mechanical.
    """
    evidence = report.get("critical_attestations")
    if evidence is None:
        raise Refused(
            "approval-binding report has no canonical critical attestation evidence"
        )
    if not isinstance(evidence, list):
        raise Refused("critical attestation evidence is not a list")
    if not evidence:
        if (
            report.get("critical_required")
            or report.get("critical_lanes_missing")
            or report.get("critical_classes")
        ):
            raise Refused("critical review summary has no canonical attestation evidence")
        return False, []

    accepted: list[dict[str, Any]] = []
    refused: list[str] = []
    for position, item in enumerate(evidence):
        if not isinstance(item, dict):
            refused.append(f"record {position} is not an object")
            continue
        if item.get("error") is not None:
            refused.append(f"record {position}: {item['error']}")
            continue
        comment_id = str(item.get("comment_id") or "")
        actor = str(item.get("github_actor") or "")
        permission = str(item.get("permission") or "").lower()
        version_time = str(item.get("version_time") or "")
        critical_class = str(item.get("critical_class") or "")
        url = str(item.get("url") or "")
        try:
            parsed_version_time = datetime.datetime.fromisoformat(
                version_time.replace("Z", "+00:00")
            )
        except ValueError:
            parsed_version_time = None
        if (
            not comment_id.isdigit()
            or not actor
            or item.get("authorized") is not True
            or permission not in approval_authority.AUTHORIZED_REVIEW_PERMISSIONS
            or parsed_version_time is None
            or parsed_version_time.tzinfo is None
            or approval_authority.comment_database_id({"url": url}) != comment_id
            or critical_class not in approval_authority.CRITICAL_CLASSES
        ):
            refused.append(
                f"record {position} lacks immutable comment identity, current "
                "GitHub actor permission, version time, or a supported critical class"
            )
            continue
        try:
            attested_head = exact_sha(item.get("head"), "critical attestation head")
        except Refused as error:
            refused.append(f"record {position}: {error}")
            continue
        if attested_head == current_head:
            accepted.append(
                {
                    "critical_class": critical_class,
                    "basis": BOUND_CURRENT,
                    "comment_id": comment_id,
                }
            )
            continue
        try:
            proof = proof_cache.get(attested_head)
            if proof is None:
                proof = prove_mechanical_rebase(
                    repo, attested_head, current_base, current_head
                )
                proof_cache[attested_head] = proof
        except (Refused, ReviewCheckError) as error:
            refused.append(f"record {position}: {error}")
            continue
        accepted.append(
            {
                "critical_class": critical_class,
                "basis": "MECHANICAL_REBASE",
                "comment_id": comment_id,
                "proof": proof,
            }
        )
    if not accepted:
        detail = "; ".join(refused) or "no usable critical attestation"
        raise Refused(
            "critical classification is not bound to the current patch series; "
            f"publish a current-head attestation ({detail})"
        )
    return True, accepted


def evaluate(
    report: dict[str, Any], repo: Path, current_base: str, current_head: str
) -> dict[str, Any]:
    exact_sha(current_base, "current base")
    exact_sha(current_head, "current head")
    if report.get("approval_set_missing") or report.get("approval_sets_missing"):
        raise Refused("the PR has no required approval set")
    if report.get("critical_label_missing"):
        raise Refused("the critical-change routing label is missing")
    proof_cache: dict[str, dict[str, Any]] = {}
    critical_required, critical_evidence = critical_requirement(
        report, repo, current_base, current_head, proof_cache
    )

    rows = report.get("rows")
    if not isinstance(rows, list) or not rows:
        raise Refused("approval-binding report contains no approval rows")

    accepted: dict[str, dict[str, Any]] = {}
    superseded: list[tuple[str, dict[str, Any]]] = []
    unaccepted_states: dict[str, set[str]] = {}
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
            accepted[lane] = {"lane": lane, "basis": BOUND_CURRENT}
            continue
        # approval_binding derives state and bound_to from canonical review
        # records. The mutable label and its source field are cache presentation
        # only, so neither participates in this mechanical carry decision or in
        # its authoritative result.
        if state == SUPERSEDED:
            superseded.append((lane, row))
        else:
            unaccepted_states.setdefault(lane, set()).add(
                str(state or "missing-state")
            )

    blocking_states = {REFUSED, REFUSED_STALE}
    blocked = sorted(
        (lane, state)
        for lane, states in unaccepted_states.items()
        for state in states
        if state in blocking_states
    )
    if blocked:
        detail = ", ".join(f"{lane}={state}" for lane, state in blocked)
        raise Refused(f"unresolved review objection: {detail}")
    carry_errors: dict[str, Exception] = {}
    for lane, row in superseded:
        if lane in accepted:
            continue
        if not critical_required and accepted:
            break
        try:
            approved_head = exact_sha(row.get("bound_to"), f"{lane} approved head")
            proof = proof_cache.get(approved_head)
            if proof is None:
                proof = prove_mechanical_rebase(
                    repo, approved_head, current_base, current_head
                )
                proof_cache[approved_head] = proof
        except (Refused, ReviewCheckError) as error:
            carry_errors[lane] = error
            continue
        accepted[lane] = {
            "lane": lane,
            "basis": "MECHANICAL_REBASE",
            "proof": proof,
        }

    accepted_policy = approval_authority.review_lane_policy(
        [{"lane": lane, "state": BOUND_CURRENT} for lane in accepted],
        critical_required,
    )
    if critical_required:
        missing_lanes = list(accepted_policy["missing_lanes"])
        if missing_lanes:
            operational_error = next(
                (
                    carry_errors[lane]
                    for lane in missing_lanes
                    if isinstance(carry_errors.get(lane), ReviewCheckError)
                ),
                None,
            )
            if operational_error is not None:
                raise operational_error
            detail = "; ".join(
                str(carry_errors[lane])
                for lane in missing_lanes
                if lane in carry_errors
            )
            suffix = f"; mechanical carry refused: {detail}" if detail else ""
            raise Refused(
                "critical review families missing: "
                + ",".join(missing_lanes)
                + suffix
            )
    elif not accepted:
        if carry_errors:
            raise next(iter(carry_errors.values()))
        if unaccepted_states:
            lane = sorted(unaccepted_states)[0]
            states = "/".join(sorted(unaccepted_states[lane]))
            raise Refused(
                f"{lane} is {states}, not bound or mechanically carried"
            )
        raise Refused("approval-binding report contains no accepted review approval")

    return {
        "authorized": True,
        "state": "REVIEW_APPLIES",
        "head": current_head,
        "current_base": current_base,
        "approvals": [accepted[lane] for lane in sorted(accepted)],
        "critical_attestations": critical_evidence,
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
