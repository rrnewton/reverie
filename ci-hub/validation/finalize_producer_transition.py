#!/usr/bin/env python3
"""Prove and materialize one producer-definition transition.

This is deliberately a post-landing operation.  The canonical producer
verifier owns registry shape, lifecycle, exact-map membership, and the
mechanical finalized-registry transform.  The canonical landing verifier owns
the fresh fetch, GitHub PR dereference, and mergeCommit ancestry proof.  This
small coordinator refuses unless those two authorities agree on the same
rebase-merge replay and the freshly fetched main tip still resolves to the
candidate map.

Without ``--output`` the command is read-only and prints the proposed registry
and proof.  With ``--output`` it atomically writes those already-proven bytes;
it never edits the input registry in place while evidence is incomplete.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from typing import Any, Callable


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_VERIFIER = ROOT / "ci-hub/validation/verify_receipt.sh"
DEFAULT_CI_HUB = ROOT / "ci-hub/ci-hub"
SHA40 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


class Refused(RuntimeError):
    """Evidence is validly negative; retry only after external state changes."""


class AuthorityError(RuntimeError):
    """An authority was unavailable or emitted malformed evidence."""


def run(
    arguments: list[str],
    *,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        arguments,
        text=True,
        capture_output=True,
        env=env,
        timeout=600,
    )


def json_output(result: subprocess.CompletedProcess[str], authority: str) -> dict[str, Any]:
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise AuthorityError(f"{authority} emitted invalid JSON: {error}") from error
    if not isinstance(value, dict):
        raise AuthorityError(f"{authority} emitted a non-object")
    return value


def producer_call(
    verifier: Path,
    *arguments: str,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return run([str(verifier), *arguments], env=env)


def transition_evidence(verifier: Path) -> dict[str, Any]:
    result = producer_call(verifier, "--producer-definition-transition")
    if result.returncode == 1:
        raise Refused("no producer-definition transition is registered")
    if result.returncode != 0:
        raise AuthorityError(
            "producer-definition transition authority failed: "
            f"rc={result.returncode}: {result.stderr.strip()}"
        )
    evidence = json_output(result, "producer-definition transition authority")
    provenance = evidence.get("provenance")
    candidate = evidence.get("candidate_record")
    if (
        evidence.get("active") is not True
        or evidence.get("finalizable") is not True
        or not isinstance(provenance, dict)
        or not isinstance(candidate, dict)
        or candidate.get("coverage_status") != "complete"
        or not isinstance(candidate.get("definition"), dict)
        or candidate.get("paths") != sorted(candidate["definition"])
    ):
        raise Refused("transition is absent, expired, not yet finalizable, or incomplete")
    repository = provenance.get("repository")
    pull_request = provenance.get("pull_request")
    head = provenance.get("head")
    registry_sha256 = evidence.get("registry_sha256")
    if (
        repository != "rrnewton/hermit"
        or not isinstance(pull_request, int)
        or isinstance(pull_request, bool)
        or pull_request <= 0
        or not isinstance(head, str)
        or SHA40.fullmatch(head) is None
        or evidence.get("registered_at") != head
        or not isinstance(registry_sha256, str)
        or SHA256.fullmatch(registry_sha256) is None
    ):
        raise AuthorityError("transition authority emitted malformed provenance")
    return evidence


def landing_evidence(
    ci_hub: Path,
    checkout: Path,
    transition: dict[str, Any],
) -> dict[str, Any]:
    provenance = transition["provenance"]
    result = run(
        [
            str(ci_hub),
            "verify-landing",
            str(provenance["pull_request"]),
            "--repo",
            provenance["repository"],
            "--source",
            str(checkout),
            "--target",
            "main",
            "--json",
        ]
    )
    if result.returncode == 1:
        raise Refused("the PR replay is not on freshly fetched target main")
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise AuthorityError(
            f"landing authority failed: rc={result.returncode}: {detail}"
        )
    evidence = json_output(result, "landing authority")
    replay = evidence.get("merge_commit_oid")
    if (
        evidence.get("state") != "landed"
        or evidence.get("input_kind") != "pr"
        or evidence.get("repo") != provenance["repository"]
        or evidence.get("pr") != provenance["pull_request"]
        or evidence.get("pr_state") != "MERGED"
        or evidence.get("pr_head_sha") != provenance["head"]
        or not isinstance(replay, str)
        or SHA40.fullmatch(replay) is None
        or evidence.get("resolved_sha") != replay
        or evidence.get("ancestry") != "ancestor"
        or evidence.get("target") != "origin/main"
    ):
        raise Refused(
            "landing evidence does not bind the registered PR head to one "
            "mergeCommit.oid on freshly fetched main"
        )
    return evidence


def fetched_main_tip(checkout: Path) -> str:
    result = run(
        [
            "git",
            "-C",
            str(checkout),
            "rev-parse",
            "--verify",
            "refs/remotes/origin/main^{commit}",
        ]
    )
    tip = result.stdout.strip()
    if result.returncode != 0 or SHA40.fullmatch(tip) is None:
        raise AuthorityError(
            "cannot resolve origin/main after the landing authority's fresh fetch"
        )
    return tip


def resolved_map(verifier: Path, checkout: Path, sha: str) -> dict[str, Any]:
    result = producer_call(
        verifier,
        "--producer-definition-resolve",
        "--sha",
        sha,
        "--repo-checkout",
        str(checkout),
    )
    if result.returncode == 1:
        raise Refused(f"{sha} does not resolve to one currently allowed whole map")
    if result.returncode != 0:
        raise AuthorityError(
            "producer-definition resolver failed: "
            f"rc={result.returncode}: {result.stderr.strip()}"
        )
    return json_output(result, f"producer-definition resolver for {sha}")


def require_candidate(
    resolved: dict[str, Any], candidate: dict[str, Any], subject: str
) -> None:
    observed = {
        key: resolved.get(key)
        for key in ("definition", "coverage_status", "paths")
    }
    expected = {
        key: candidate.get(key)
        for key in ("definition", "coverage_status", "paths")
    }
    if observed != expected or "valid_commits" in resolved:
        raise Refused(f"{subject} does not resolve to the exact complete candidate map")


def finalized_registry(
    verifier: Path, replay: str, checkout: Path, expected_registry_sha256: str
) -> tuple[dict[str, Any], dict[str, Any]]:
    result = producer_call(
        verifier,
        "--producer-definition-finalize",
        "--landed-replay",
        replay,
        "--expected-registry-sha256",
        expected_registry_sha256,
    )
    if result.returncode == 1:
        raise Refused("producer-definition registry changed before finalization transform")
    if result.returncode != 0:
        raise AuthorityError(
            "producer-definition finalization transform failed: "
            f"rc={result.returncode}: {result.stderr.strip()}"
        )
    registry = json_output(result, "producer-definition finalization transform")

    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".json", encoding="utf-8", delete=False
    ) as temporary:
        temporary.write(json.dumps(registry, sort_keys=True))
        temporary.write("\n")
        registry_path = Path(temporary.name)
    try:
        environment = {**os.environ, "PRODUCER_DEFINITION_REGISTRY": str(registry_path)}
        check = producer_call(
            verifier,
            "--producer-definition-resolve",
            "--sha",
            replay,
            "--repo-checkout",
            str(checkout),
            env=environment,
        )
        if check.returncode != 0:
            raise AuthorityError(
                "the canonical verifier rejected its finalized registry: "
                f"rc={check.returncode}: {check.stderr.strip()}"
            )
        replay_evidence = json_output(check, "finalized producer-definition verifier")
    finally:
        registry_path.unlink(missing_ok=True)
    return registry, replay_evidence


def atomic_write(
    path: Path,
    value: dict[str, Any],
    before_replace: Callable[[], None] | None = None,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    body = json.dumps(value, indent=2, sort_keys=False) + "\n"
    fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(body)
            handle.flush()
            os.fsync(handle.fileno())
        if before_replace is not None:
            before_replace()
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def require_same_active_transition(
    verifier: Path, expected: dict[str, Any]
) -> None:
    """CAS the exact registry bytes and live lifecycle at the mutation boundary."""
    observed = transition_evidence(verifier)
    if observed != expected:
        raise Refused(
            "producer-definition registry or transition lifecycle changed during proof"
        )


def execute(args: argparse.Namespace) -> dict[str, Any]:
    checkout = args.repo_checkout.resolve()
    if not checkout.is_dir():
        raise AuthorityError(f"repository checkout is missing: {checkout}")
    transition = transition_evidence(args.verifier)
    landing = landing_evidence(args.ci_hub, checkout, transition)
    replay = landing["merge_commit_oid"]
    main_tip = fetched_main_tip(checkout)
    candidate = transition["candidate_record"]
    replay_map = resolved_map(args.verifier, checkout, replay)
    main_map = resolved_map(args.verifier, checkout, main_tip)
    require_candidate(replay_map, candidate, "dereferenced mergeCommit.oid")
    require_candidate(main_map, candidate, "freshly fetched main tip")
    registry, finalized_replay = finalized_registry(
        args.verifier, replay, checkout, transition["registry_sha256"]
    )
    require_candidate(finalized_replay, candidate, "finalized replay")
    if args.output is not None:
        atomic_write(
            args.output,
            registry,
            before_replace=lambda: require_same_active_transition(
                args.verifier, transition
            ),
        )
    return {
        "schema_version": 1,
        "action": "finalized" if args.output is not None else "would-finalize",
        "output": str(args.output) if args.output is not None else None,
        "transition_id": transition["id"],
        "repository": transition["provenance"]["repository"],
        "pull_request": transition["provenance"]["pull_request"],
        "registered_head": transition["provenance"]["head"],
        "source_registry_sha256": transition["registry_sha256"],
        "merge_commit_oid": replay,
        "fetched_main_tip": main_tip,
        "producer_condition": candidate,
        "landing_evidence": landing,
        "finalized_registry": registry,
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-checkout", type=Path, required=True)
    parser.add_argument("--verifier", type=Path, default=DEFAULT_VERIFIER)
    parser.add_argument("--ci-hub", type=Path, default=DEFAULT_CI_HUB)
    parser.add_argument("--output", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        report = execute(args)
    except Refused as error:
        print(json.dumps({"state": "refused", "reason": str(error)}, sort_keys=True))
        return 1
    except (AuthorityError, OSError, subprocess.SubprocessError) as error:
        print(
            json.dumps({"state": "unverifiable", "reason": str(error)}, sort_keys=True),
            file=sys.stderr,
        )
        return 2
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
