#!/usr/bin/env python3
"""Refuse a Hermit producer change whose exact transition is not staged."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any


SHA40 = re.compile(r"^[0-9a-f]{40}$")
REPOSITORY = "rrnewton/hermit"


class GuardError(Exception):
    """A malformed input or unavailable authority."""


def run(
    command: list[str], *, environment: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
    )


def verifier_call(verifier: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    # A real landing reads the installed registry. Test fixtures install their
    # own complete tree instead of overriding this authority in-process.
    environment.pop("PRODUCER_DEFINITION_REGISTRY", None)
    environment.pop("script_dir", None)
    return run(["bash", str(verifier), *arguments], environment=environment)


def parse_object(raw: str, subject: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GuardError(f"{subject} returned malformed JSON: {error}") from error
    if not isinstance(value, dict):
        raise GuardError(f"{subject} did not return an object")
    return value


def git_blob(checkout: Path, head: str, relative: str) -> str | None:
    result = run(["git", "-C", str(checkout), "rev-parse", f"{head}:{relative}"])
    blob = result.stdout.strip()
    if result.returncode != 0 or not SHA40.fullmatch(blob):
        return None
    return blob


def changed_primary_paths(
    checkout: Path, head: str, primary: dict[str, Any]
) -> list[str]:
    changed: list[str] = []
    for relative, expected_blob in sorted(primary.items()):
        if not isinstance(relative, str) or not SHA40.fullmatch(str(expected_blob)):
            raise GuardError(
                "validated primary producer definition has an invalid entry"
            )
        if git_blob(checkout, head, relative) != expected_blob:
            changed.append(relative)
    return changed


def candidate_change_count(primary: dict[str, Any], candidate: dict[str, Any]) -> int:
    return sum(
        primary.get(relative) != candidate.get(relative)
        for relative in set(primary) | set(candidate)
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--pr", required=True, type=int)
    parser.add_argument("--head", required=True)
    parser.add_argument("--checkout", required=True, type=Path)
    args = parser.parse_args()

    if args.repo != REPOSITORY:
        parser.error(f"--repo must be {REPOSITORY}")
    if args.pr <= 0:
        parser.error("--pr must be positive")
    if not SHA40.fullmatch(args.head):
        parser.error("--head must be a 40-hex commit")
    if not args.checkout.is_dir():
        parser.error("--checkout must be a directory")
    commit = run(
        ["git", "-C", str(args.checkout), "cat-file", "-e", f"{args.head}^{{commit}}"]
    )
    if commit.returncode != 0:
        parser.error("--checkout does not contain --head")

    root = Path(__file__).resolve().parents[2]
    verifier = root / "ci-hub" / "validation" / "verify_receipt.sh"
    if not verifier.is_file():
        raise GuardError(f"producer-definition verifier is unavailable: {verifier}")

    primary_result = verifier_call(verifier, "--producer-definition-primary")
    if primary_result.returncode != 0:
        detail = primary_result.stderr.strip() or "no detail"
        raise GuardError(
            f"cannot read the validated primary producer definition: {detail}"
        )
    primary = parse_object(primary_result.stdout, "primary producer definition")
    changed = changed_primary_paths(args.checkout, args.head, primary)

    transition_result = verifier_call(verifier, "--producer-definition-transition")
    transition: dict[str, Any] | None = None
    if transition_result.returncode == 0:
        transition = parse_object(transition_result.stdout, "producer transition")
    elif transition_result.returncode != 1:
        detail = transition_result.stderr.strip() or "no detail"
        raise GuardError(f"cannot read the validated producer transition: {detail}")

    exact_transition = False
    if transition is not None:
        provenance = transition.get("provenance")
        exact_transition = (
            isinstance(provenance, dict)
            and provenance.get("repository") == args.repo
            and provenance.get("pull_request") == args.pr
            and provenance.get("head") == args.head
            and transition.get("registered_at") == args.head
        )

    if exact_transition:
        if transition.get("active") is not True:
            print(
                f"REFUSED: producer transition for {args.repo}#{args.pr} at {args.head} is not active",
                file=sys.stderr,
            )
            return 1
        candidate_record = transition.get("candidate_record")
        candidate = (
            candidate_record.get("definition")
            if isinstance(candidate_record, dict)
            else None
        )
        if not isinstance(candidate, dict):
            raise GuardError(
                "validated producer transition has no candidate definition"
            )
        resolved_result = verifier_call(
            verifier,
            "--producer-definition-resolve",
            "--sha",
            args.head,
            "--repo-checkout",
            str(args.checkout),
        )
        if resolved_result.returncode != 0:
            detail = resolved_result.stderr.strip() or "candidate map did not resolve"
            print(
                f"REFUSED: staged producer transition does not resolve at {args.head}: {detail}",
                file=sys.stderr,
            )
            return 1 if resolved_result.returncode == 1 else 2
        resolved = parse_object(resolved_result.stdout, "resolved producer definition")
        if resolved.get("definition") != candidate:
            print(
                f"REFUSED: {args.head} resolved to a producer definition other than the staged candidate",
                file=sys.stderr,
            )
            return 1
        count = candidate_change_count(primary, candidate)
        print(
            f"ACCEPTED: staged producer transition id={transition.get('id')} "
            f"head={args.head} changed_registered_paths={count}"
        )
        return 0

    if changed:
        paths = ",".join(changed)
        print(
            f"REFUSED: {args.repo}#{args.pr} changes registered producer paths without an active "
            f"transition bound to this exact head; changed_registered_paths={len(changed)} paths={paths}",
            file=sys.stderr,
        )
        return 1

    print(
        f"ACCEPTED: registered producer definition unchanged; changed_registered_paths=0 head={args.head}"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GuardError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(2)
