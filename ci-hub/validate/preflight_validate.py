#!/usr/bin/env python3
"""Fail-closed fixed-floor admission for a receipt-producing validation.

Admission here proves that the head contains every immutable producer/merge-gate
floor in ``rebase-base-floors.json``. The enclosing ``validate-lock run`` owns
the separate moving boundary: it freshly fetches ``origin/main`` before queueing
and repeats the ancestry check immediately before acquiring the validation
lease. Schema-5 receipt authority checks that boundary again immediately before
the merge it authorizes.

Exit codes:
  0  OK       -- every immutable ancestry authority passes
  2  REFUSED  -- head is stale against a fixed floor
  3  ERROR    -- the authority could not be resolved

Historical differential debugging after a rebased head fails is not a
qualifying validation.  Run that focused comparison outside ``validate-run``;
do not use an old head to mint or reuse a landing receipt.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys

import preflight_anchor


DEFAULT_REPO = preflight_anchor.DEFAULT_REPO
DEFAULT_CHECKOUT = preflight_anchor.DEFAULT_CHECKOUT
DEFAULT_ANCHORS = preflight_anchor.DEFAULT_ANCHORS
DEFAULT_BASE_BRANCH = "main"

EXIT_OK = 0
EXIT_REFUSED = 2
EXIT_ERROR = 3
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
BRANCH_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/-]*$")


class AdmissionError(Exception):
    """A required identity or ancestry authority could not be resolved."""


def _short(sha: str) -> str:
    return sha[:12] if sha else "?"


def _validate_sha(value: str, *, field: str) -> str:
    value = value.strip()
    if not SHA_RE.fullmatch(value):
        raise AdmissionError(f"{field} is not an exact lowercase 40-hex commit: {value!r}")
    return value


def admission(
    head: str,
    *,
    checkout: str,
    repo: str,
    anchors_path: str,
    base_branch: str,
) -> dict:
    """Return one record carrying the immutable fixed-floor verdict."""
    head = _validate_sha(head, field="validation head")
    if not BRANCH_RE.fullmatch(base_branch) or ".." in base_branch or "@{" in base_branch:
        raise AdmissionError(f"unsafe base branch name {base_branch!r}")
    try:
        floors = preflight_anchor.preflight(
            head, checkout=checkout, repo=repo, anchors_path=anchors_path
        )
    except preflight_anchor.PreflightError as error:
        raise AdmissionError(str(error)) from error

    return {
        "schema_version": 1,
        "repo": repo,
        "head": head,
        "fixed_floors": floors,
        "moving_base": {
            "branch": base_branch,
            "status": "checked-by-validate-lock",
        },
        "ok": floors["ok"],
    }


def render(result: dict) -> str:
    lines: list[str] = []
    floors = result["fixed_floors"]
    if not floors["ok"]:
        lines.append(preflight_anchor.render(floors))

    if lines:
        return "\n".join(lines)
    return (
        f"OK: head {_short(result['head'])} contains all "
        f"{floors['n_anchors']} fixed validation floors; mutable origin/"
        f"{result['moving_base']['branch']} currency is checked by validate-lock."
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--head", help="exact lowercase 40-hex validation head")
    target.add_argument("--pr", help="resolve this PR's exact head through GitHub")
    parser.add_argument("--repo-checkout", default=DEFAULT_CHECKOUT)
    parser.add_argument("--repo", default=DEFAULT_REPO)
    parser.add_argument("--anchors", default=DEFAULT_ANCHORS)
    parser.add_argument("--base-branch", default=DEFAULT_BASE_BRANCH)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    try:
        if args.pr:
            head, _branch = preflight_anchor.resolve_pr_head(args.repo, args.pr)
        else:
            head = args.head
        result = admission(
            head,
            checkout=os.path.abspath(args.repo_checkout),
            repo=args.repo,
            anchors_path=args.anchors,
            base_branch=args.base_branch,
        )
    except (AdmissionError, preflight_anchor.PreflightError) as error:
        if args.json:
            print(json.dumps({"error": str(error)}, indent=2))
        else:
            print(f"ERROR: {error}", file=sys.stderr)
        return EXIT_ERROR

    if args.json:
        print(json.dumps(result, indent=2))
    else:
        print(render(result))
    return EXIT_OK if result["ok"] else EXIT_REFUSED


if __name__ == "__main__":
    raise SystemExit(main())
