#!/usr/bin/env python3
"""Produce one exact-head PR verdict through the centralized local runner.

This is deliberately just-in-time: callers name one PR already at the landing
frontier.  It never sweeps the open population.  The live GitHub head is read
before and after VALIDATE, and the canonical ledger is the only verdict source.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Sequence
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import bounded_capture  # noqa: E402


ROOT = Path(__file__).resolve().parents[2]
SUPPORTED_REPOS = frozenset(("rrnewton/hermit", "rrnewton/reverie"))
SHA_LEN = 40
Run = Callable[..., subprocess.CompletedProcess[str]]


@dataclass(frozen=True)
class PrIdentity:
    number: int
    head: str
    branch: str
    draft: bool
    state: str


def run_command(command: Sequence[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return bounded_capture.capture(command, **kwargs)


def full_sha(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == SHA_LEN
        and all(character in "0123456789abcdef" for character in value)
    )


def live_pr(repo: str, pr: int, *, run: Run) -> PrIdentity:
    result = run(
        [
            "with-proxy",
            "gh",
            "pr",
            "view",
            str(pr),
            "--repo",
            repo,
            "--json",
            "headRefOid,headRefName,isDraft,state",
        ],
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"live PR identity unavailable: {detail}")
    try:
        row = json.loads(result.stdout)
    except ValueError as error:
        raise RuntimeError(f"live PR identity is not JSON: {error}") from error
    head = row.get("headRefOid")
    if not full_sha(head):
        raise RuntimeError(f"live PR head is not an exact 40-hex SHA: {head!r}")
    return PrIdentity(
        number=pr,
        head=head.lower(),
        branch=str(row.get("headRefName") or ""),
        draft=bool(row.get("isDraft")),
        state=str(row.get("state") or "UNKNOWN").upper(),
    )


def canonical_status(repo: str, sha: str, *, run: Run) -> tuple[str, dict]:
    result = run(
        [
            str(ROOT / "ci-hub/ci-hub"),
            "validate-status",
            "--repo",
            repo,
            "--sha",
            sha,
            "--json",
        ],
        check=False,
    )
    try:
        report = json.loads(result.stdout)
    except ValueError as error:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise RuntimeError(f"canonical validate-status unavailable: {detail}: {error}") from error
    verdict = report.get("verdict")
    if not isinstance(verdict, str) or not verdict:
        raise RuntimeError("canonical validate-status JSON has no verdict")
    return verdict, report


def require_current_approval(repo: str, pr: int, *, run: Run) -> None:
    result = run(
        [
            "python3",
            str(ROOT / "ci-hub/health/approval_binding.py"),
            "--repo",
            repo,
            "--pr",
            str(pr),
            "--gate",
        ],
        check=False,
    )
    if result.returncode != 0:
        detail = result.stdout.strip() or result.stderr.strip() or f"exit {result.returncode}"
        raise PermissionError(f"exact-head approval authority refused: {detail}")


def validate_command(repo: str, pr: int, head: str, checkout: Path, agent: str) -> list[str]:
    command = [
        str(ROOT / "ci-hub/ci-hub"),
        "validate-run",
        "--checkout",
        str(checkout),
    ]
    if repo != "rrnewton/hermit":
        command += ["--repo", repo]
    command += ["--agent", agent, "--target", head, "--pr", str(pr)]
    if repo == "rrnewton/hermit":
        command += ["--", "full"]
    return command


def emit(state: str, *, repo: str, pr: int, head: str, detail: str, as_json: bool) -> None:
    report = {
        "schema_version": 1,
        "state": state,
        "repo": repo,
        "pr": pr,
        "head": head,
        "detail": detail,
        "acquirable": state == "VALIDATED",
    }
    if as_json:
        print(json.dumps(report, sort_keys=True))
    else:
        print(f"PR-HEAD-CI {state} {repo}#{pr} head={head} acquirable={str(report['acquirable']).lower()} -- {detail}")


def main(argv: Sequence[str] | None = None, *, run: Run = run_command) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, choices=sorted(SUPPORTED_REPOS))
    parser.add_argument("--pr", required=True, type=int)
    parser.add_argument("--expected-head", required=True)
    parser.add_argument("--checkout", required=True, type=Path)
    parser.add_argument("--agent", required=True)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    if args.pr <= 0 or not full_sha(args.expected_head):
        print("pr-head-ci: REFUSED: --pr must be positive and --expected-head exact 40-hex", file=sys.stderr)
        return 2
    expected = args.expected_head.lower()
    try:
        before = live_pr(args.repo, args.pr, run=run)
        if before.state != "OPEN":
            emit("UNKNOWN", repo=args.repo, pr=args.pr, head=expected,
                 detail=f"PR state is {before.state}, not OPEN", as_json=args.json)
            return 2
        if before.head != expected:
            emit("SUPERSEDED", repo=args.repo, pr=args.pr, head=before.head,
                 detail=f"expected head {expected} moved before admission", as_json=args.json)
            return 4

        verdict, _ = canonical_status(args.repo, expected, run=run)
        if verdict == "VALIDATED":
            emit("VALIDATED", repo=args.repo, pr=args.pr, head=expected,
                 detail="canonical exact-head receipt already qualifies", as_json=args.json)
            return 0
        if verdict == "FAILED":
            emit("FAILED", repo=args.repo, pr=args.pr, head=expected,
                 detail="canonical exact-head receipt is red", as_json=args.json)
            return 3
        if not args.execute:
            emit("NO_RESULT", repo=args.repo, pr=args.pr, head=expected,
                 detail=f"canonical verdict is {verdict}; pass --execute at the landing frontier", as_json=args.json)
            return 4
        if before.draft:
            emit("NO_RESULT", repo=args.repo, pr=args.pr, head=expected,
                 detail="draft PR is not at the landing frontier", as_json=args.json)
            return 4
        require_current_approval(args.repo, args.pr, run=run)

        run(validate_command(args.repo, args.pr, expected, args.checkout, args.agent), check=False)

        after = live_pr(args.repo, args.pr, run=run)
        if after.head != expected:
            emit("SUPERSEDED", repo=args.repo, pr=args.pr, head=after.head,
                 detail=f"validation was bound to {expected}; live head moved and has no inherited verdict", as_json=args.json)
            return 4
        verdict, _ = canonical_status(args.repo, expected, run=run)
        if verdict == "VALIDATED":
            emit("VALIDATED", repo=args.repo, pr=args.pr, head=expected,
                 detail="centralized local runner produced a qualifying exact-head receipt", as_json=args.json)
            return 0
        if verdict == "FAILED":
            emit("FAILED", repo=args.repo, pr=args.pr, head=expected,
                 detail="centralized local runner produced an exact-head red verdict", as_json=args.json)
            return 3
        emit("NO_RESULT", repo=args.repo, pr=args.pr, head=expected,
             detail=f"centralized local runner returned without a terminal receipt; canonical verdict={verdict}", as_json=args.json)
        return 4
    except PermissionError as error:
        emit("NO_RESULT", repo=args.repo, pr=args.pr, head=expected,
             detail=str(error), as_json=args.json)
        return 4
    except RuntimeError as error:
        emit("UNKNOWN", repo=args.repo, pr=args.pr, head=expected,
             detail=str(error), as_json=args.json)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
