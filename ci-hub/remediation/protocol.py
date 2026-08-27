#!/usr/bin/env python3
"""Verify landing ancestry and exact-head hosted CI status."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub"))
sys.path.insert(0, str(ROOT / "ci-hub/lib"))

import fetch_ref
import git_env
from check_outcome import CheckOutcome, classify_check, select_latest_workflow_attempts

DEFAULT_REPO = "rrnewton/hermit"
REVERIE_REPO = "rrnewton/reverie"
PARENT_REPO = "rrnewton/dev-hermit"
AGENT_UTILS_REPO = "rrnewton/agent-utils"

PARENT_TOOLING_WORKFLOW = "Dev-hermit operational tooling"
PARENT_TOOLING_WORKFLOW_FILE = ".github/workflows/dev-hermit-ci.yml"
PARENT_PORTABILITY_WORKFLOW = "Portability"
PARENT_PORTABILITY_WORKFLOW_FILE = ".github/workflows/portability.yml"
PARENT_DEMO_REVIEW_WORKFLOW = "Demo review gate"
PARENT_DEMO_REVIEW_WORKFLOW_FILE = ".github/workflows/demo-review-gate.yml"
DEFAULT_WORKFLOW = "CI (GitHub-managed portable)"
DEFAULT_WORKFLOW_FILE = ".github/workflows/ci-portable.yml"
REVERIE_WORKFLOW = "Rust"
REVERIE_WORKFLOW_FILE = ".github/workflows/ci.yml"

_CURRENT_VERIFICATION_POLICY_VERSION = {
    DEFAULT_REPO: 3,
    REVERIE_REPO: 2,
    PARENT_REPO: 1,
}
_VERSIONED_REPO_GITHUB_JOBS = {
    (DEFAULT_REPO, 3): (
        (DEFAULT_WORKFLOW_FILE, DEFAULT_WORKFLOW, "Regular tests (GitHub-managed portable)"),
    ),
    (REVERIE_REPO, 2): (
        (REVERIE_WORKFLOW_FILE, REVERIE_WORKFLOW, "Regular tests (GitHub-hosted)"),
        (REVERIE_WORKFLOW_FILE, REVERIE_WORKFLOW, "Host-dependent tests (self-hosted)"),
    ),
    (PARENT_REPO, 1): (
        (PARENT_TOOLING_WORKFLOW_FILE, PARENT_TOOLING_WORKFLOW, "Parent tooling shard"),
        (PARENT_TOOLING_WORKFLOW_FILE, PARENT_TOOLING_WORKFLOW, "ci-hub bounded operations shard"),
        (PARENT_PORTABILITY_WORKFLOW_FILE, PARENT_PORTABILITY_WORKFLOW, "Reject owner-specific build paths"),
        (PARENT_DEMO_REVIEW_WORKFLOW_FILE, PARENT_DEMO_REVIEW_WORKFLOW, "Demo-touching commits require a green-demo attestation"),
    ),
}
_DEFAULT_REPO_SOURCES = {
    DEFAULT_REPO: ROOT / "hermit",
    REVERIE_REPO: ROOT / "reverie",
    PARENT_REPO: ROOT,
    AGENT_UTILS_REPO: ROOT / "agent-utils",
}
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_GITHUB_PATH_COMPONENT = re.compile(r"[A-Za-z0-9_.-]+")
DEFAULT_NETWORK_TIMEOUT = float(os.environ.get("CI_HUB_NETWORK_TIMEOUT", "120"))
TERMINAL_VERIFICATION_STATES = frozenset(("green", "red"))


class ProtocolError(RuntimeError):
    """A requested verification could not be established."""


def _run(
    command: Sequence[str],
    *,
    cwd: Path | None = None,
    check: bool = False,
    capture_output: bool = True,
    env: Mapping[str, str] | None = None,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            list(command),
            cwd=cwd,
            check=check,
            capture_output=capture_output,
            text=True,
            env=git_env.sanitized_git_env(env),
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        raise ProtocolError(
            f"command timed out after {timeout:.1f}s: {' '.join(command)}"
        ) from error
    except (OSError, subprocess.CalledProcessError) as error:
        detail = (
            (error.stderr or error.stdout or "").strip()
            if isinstance(error, subprocess.CalledProcessError)
            else str(error)
        )
        raise ProtocolError(f"command failed: {' '.join(command)}: {detail}") from error


def verification_policy_for_repo(repo: str) -> dict[str, Any]:
    try:
        schema_version = _CURRENT_VERIFICATION_POLICY_VERSION[repo]
        required_jobs = _VERSIONED_REPO_GITHUB_JOBS[(repo, schema_version)]
    except KeyError as error:
        supported = ", ".join(sorted(_CURRENT_VERIFICATION_POLICY_VERSION))
        raise ProtocolError(
            f"unsupported hosted verification repository {repo!r}; "
            f"supported repositories: {supported}"
        ) from error
    return {
        "schema_version": schema_version,
        "repo": repo,
        "github": {
            "required_jobs": [
                {
                    "workflow_file": workflow_file,
                    "workflow_name": workflow_name,
                    "job_name": job_name,
                }
                for workflow_file, workflow_name, job_name in required_jobs
            ],
            "required_positive_count": len(required_jobs),
        },
    }


def validate_verification_policy(policy: Mapping[str, Any]) -> dict[str, Any]:
    repo = policy.get("repo")
    schema_version = policy.get("schema_version")
    if not isinstance(repo, str) or type(schema_version) is not int:
        raise ProtocolError("verification policy has no repository or schema identity")
    try:
        _VERSIONED_REPO_GITHUB_JOBS[(repo, schema_version)]
    except KeyError as error:
        raise ProtocolError(
            f"unsupported verification policy version {schema_version} for {repo!r}"
        ) from error
    expected = verification_policy_for_repo(repo)
    if expected["schema_version"] != schema_version or policy != expected:
        raise ProtocolError(f"invalid verification policy for {repo!r}")
    return expected


def _github_repo_from_remote(remote: str) -> str | None:
    value = remote.strip()
    scp = re.fullmatch(r"git@(?P<host>[^:/\s]+):(?P<path>[^?#\s]+)", value)
    if scp is not None:
        if scp.group("host").lower() != "github.com":
            return None
        repo_path = scp.group("path")
    else:
        try:
            parsed = urlsplit(value)
            port = parsed.port
        except ValueError:
            return None
        if parsed.scheme not in {"https", "ssh"}:
            return None
        if parsed.hostname is None or parsed.hostname.lower() != "github.com":
            return None
        if parsed.query or parsed.fragment or parsed.password is not None:
            return None
        if parsed.scheme == "https":
            if parsed.username is not None or port not in (None, 443):
                return None
        elif parsed.username != "git" or port not in (None, 22):
            return None
        repo_path = parsed.path.removeprefix("/")
    repo_path = repo_path.removesuffix("/").removesuffix(".git")
    parts = repo_path.split("/")
    if len(parts) != 2 or not all(_GITHUB_PATH_COMPONENT.fullmatch(part) for part in parts):
        return None
    return "/".join(parts)


def resolve_repo_source(repo: str, source: Path | None) -> Path:
    if repo not in _DEFAULT_REPO_SOURCES:
        supported = ", ".join(sorted(_DEFAULT_REPO_SOURCES))
        raise ProtocolError(
            f"unsupported landing verification repository {repo!r}; "
            f"supported repositories: {supported}"
        )
    candidate = source if source is not None else _DEFAULT_REPO_SOURCES[repo]
    candidate = candidate.expanduser().resolve()
    if not candidate.is_dir():
        raise ProtocolError(f"source checkout for {repo!r} is missing: {candidate}")
    result = _run(("git", "-C", str(candidate), "remote", "get-url", "origin"), check=True)
    origin = result.stdout.strip()
    if _github_repo_from_remote(origin) != repo:
        raise ProtocolError(
            f"source checkout {candidate} origin is {origin or '<missing>'!r}, "
            f"not required repository {repo!r}"
        )
    return candidate


def _fetch_target(source: Path, target: str) -> str:
    target_ref = f"origin/{target}"
    try:
        fetch_ref.fetch_remote_tracking_branch(
            source,
            remote="origin",
            branch=target,
            use_proxy=True,
            network_timeout_s=DEFAULT_NETWORK_TIMEOUT,
        )
    except fetch_ref.FetchRefError as error:
        raise ProtocolError(f"cannot safely fetch {target_ref}: {error}") from error
    return target_ref


def _is_target_ancestor(source: Path, sha: str, target_ref: str) -> bool:
    result = _run(
        ("git", "-C", str(source), "merge-base", "--is-ancestor", sha, target_ref),
        check=False,
    )
    if result.returncode not in (0, 1):
        detail = (result.stderr or result.stdout or "").strip()
        raise ProtocolError(f"cannot compare {sha} with fetched {target_ref}: {detail}")
    return result.returncode == 0


def _query_pr_landing(repo: str, pr: int) -> tuple[str, str, str]:
    result = _run(
        (
            "with-proxy", "gh", "pr", "view", str(pr), "-R", repo,
            "--json", "state,headRefOid,mergeCommit",
        ),
        check=True,
        timeout=DEFAULT_NETWORK_TIMEOUT,
    )
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ProtocolError("gh pr view returned invalid JSON") from error
    if not isinstance(payload, Mapping):
        raise ProtocolError("gh pr view returned a non-object")
    merge_commit = payload.get("mergeCommit")
    replay = (
        str(merge_commit.get("oid") or "").lower()
        if isinstance(merge_commit, Mapping)
        else ""
    )
    return (
        str(payload.get("state") or "").upper(),
        str(payload.get("headRefOid") or "").lower(),
        replay,
    )


def _print_landing_verdict(
    *, state: str, rc: int, reference: str, target_ref: str,
    json_output: bool, **details: object,
) -> int:
    payload = {"state": state, "rc": rc, "input": reference, "target": target_ref, **details}
    if json_output:
        print(json.dumps(payload, sort_keys=True))
    else:
        fields = " ".join(f"{key}={value}" for key, value in payload.items() if key != "state")
        print(f"{state.upper().replace('-', '_')} {fields}")
    return rc


def _resolve_raw_sha(source: Path, reference: str) -> str:
    command = ("git", "-C", str(source), "rev-parse", "--verify", f"{reference}^{{commit}}")
    result = _run(command, check=False)
    if result.returncode != 0:
        _run(
            ("with-proxy", "git", "-C", str(source), "fetch", "--no-tags", "origin", reference),
            check=True,
            timeout=DEFAULT_NETWORK_TIMEOUT,
        )
        result = _run(command, check=True)
    commit = result.stdout.strip().lower()
    if not SHA_RE.fullmatch(commit):
        raise ProtocolError(f"cannot resolve full commit SHA from {reference!r}")
    return commit


def _resolve_claimed_oid(
    source: Path, claimed_oid: str, *, pr: int, pr_head: str
) -> tuple[str, bool, bool]:
    abbreviated = claimed_oid.strip().lower()
    if not re.fullmatch(r"[0-9a-f]{7,40}", abbreviated):
        return "", False, False
    command = ("git", "-C", str(source), "rev-parse", "--verify", f"{abbreviated}^{{commit}}")
    resolved = _run(command, check=False)
    if resolved.returncode == 0:
        full_oid = resolved.stdout.strip().lower()
        if SHA_RE.fullmatch(full_oid):
            return full_oid, True, True
    if SHA_RE.fullmatch(pr_head) and pr_head.startswith(abbreviated):
        fetched = _run(
            ("with-proxy", "git", "-C", str(source), "fetch", "--no-tags", "origin", f"pull/{pr}/head"),
            check=False,
            timeout=DEFAULT_NETWORK_TIMEOUT,
        )
        available = fetched.returncode == 0 and _run(
            ("git", "-C", str(source), "cat-file", "-e", f"{pr_head}^{{commit}}"),
            check=False,
        ).returncode == 0
        return pr_head, True, available
    return "", False, False


def _claim_fields(
    *, item: str, claimed_oid: str, full_oid: str, resolves: bool,
    change_present: bool, claimed_ancestry_rc: int, target: str,
) -> dict[str, object]:
    fields: dict[str, object] = {
        "item": item, "claimed_oid": claimed_oid,
        "full_oid": full_oid or "unresolved", "resolves": resolves,
        "change_present_on_target": change_present,
        "claimed_ancestry_rc": claimed_ancestry_rc,
    }
    if target == "main":
        fields["change_present_on_main"] = change_present
    return fields


def verify_landing(args: argparse.Namespace) -> int:
    item = getattr(args, "item", None)
    claimed_oid = getattr(args, "claimed_oid", None)
    try:
        source = resolve_repo_source(args.repo, args.source)
        target_ref = _fetch_target(source, args.target)
        reference = args.reference.lower()
        if claimed_oid and not item:
            return _print_landing_verdict(
                state="unverifiable",
                rc=2,
                reference=args.reference,
                target_ref=target_ref,
                json_output=args.json,
                reason="--claimed-oid requires --item",
            )
        if re.fullmatch(r"[0-9a-f]{7,40}", reference):
            commit = _resolve_raw_sha(source, reference)
            is_ancestor = _is_target_ancestor(source, commit, target_ref)
            return _print_landing_verdict(
                state="landed" if is_ancestor else "not-landed",
                rc=0 if is_ancestor else 1,
                reference=args.reference, target_ref=target_ref,
                json_output=args.json, input_kind="sha", resolved_sha=commit,
                ancestry="ancestor" if is_ancestor else "not-ancestor",
                **_claim_fields(
                    item=item or args.reference, claimed_oid=args.reference,
                    full_oid=commit, resolves=True, change_present=is_ancestor,
                    claimed_ancestry_rc=0 if is_ancestor else 1, target=args.target,
                ),
            )
        if not args.reference.isdecimal() or int(args.reference) <= 0:
            return _print_landing_verdict(
                state="unverifiable", rc=2, reference=args.reference,
                target_ref=target_ref, json_output=args.json,
                reason="input must be a positive PR number or 7-40 character commit OID",
            )
        pr = int(args.reference)
        pr_state, head, replay = _query_pr_landing(args.repo, pr)
        if pr_state != "MERGED" or not SHA_RE.fullmatch(replay):
            return _print_landing_verdict(
                state="unverifiable", rc=2, reference=args.reference,
                target_ref=target_ref, json_output=args.json,
                input_kind="pr", repo=args.repo, pr=pr,
                pr_state=pr_state or "unknown", pr_head_sha=head or "unknown",
                reason="no mergeCommit.oid",
            )
        is_ancestor = _is_target_ancestor(source, replay, target_ref)
        if claimed_oid:
            full_oid, resolves, locally_available = _resolve_claimed_oid(
                source, claimed_oid, pr=pr, pr_head=head
            )
            claimed_ancestry_rc = 2
            if locally_available:
                claimed_ancestry_rc = 0 if _is_target_ancestor(source, full_oid, target_ref) else 1
            claim_details = _claim_fields(
                item=item, claimed_oid=claimed_oid, full_oid=full_oid,
                resolves=resolves, change_present=is_ancestor,
                claimed_ancestry_rc=claimed_ancestry_rc, target=args.target,
            )
        else:
            claim_details = _claim_fields(
                item=item or f"{args.repo}#{pr}", claimed_oid=replay, full_oid=replay,
                resolves=True, change_present=is_ancestor,
                claimed_ancestry_rc=0 if is_ancestor else 1, target=args.target,
            )
        return _print_landing_verdict(
            state="landed" if is_ancestor else "not-landed",
            rc=0 if is_ancestor else 1,
            reference=args.reference, target_ref=target_ref,
            json_output=args.json, input_kind="pr", repo=args.repo, pr=pr,
            pr_state=pr_state, pr_head_sha=head or "unknown", resolved_sha=replay,
            merge_commit_oid=replay,
            ancestry="ancestor" if is_ancestor else "not-ancestor",
            **claim_details,
        )
    except ProtocolError as error:
        return _print_landing_verdict(
            state="unverifiable", rc=2, reference=args.reference,
            target_ref=f"origin/{args.target}", json_output=args.json,
            reason=str(error),
        )


def _required_github_jobs(policy: Mapping[str, Any]) -> list[dict[str, str]]:
    return [dict(job) for job in validate_verification_policy(policy)["github"]["required_jobs"]]


def _required_workflows(policy: Mapping[str, Any]) -> list[tuple[str, str]]:
    workflows: list[tuple[str, str]] = []
    for job in _required_github_jobs(policy):
        identity = (job["workflow_file"], job["workflow_name"])
        if identity not in workflows:
            workflows.append(identity)
    return workflows


def _parse_github_runs(output: str, sha: str, policy: Mapping[str, Any]) -> list[dict[str, Any]]:
    policy = validate_verification_policy(policy)
    if not SHA_RE.fullmatch(sha):
        raise ProtocolError(f"invalid exact-SHA GitHub query {sha!r}")
    try:
        payload = json.loads(output)
    except json.JSONDecodeError as error:
        raise ProtocolError("GitHub workflow-runs API returned invalid JSON") from error
    if not isinstance(payload, Mapping) or not isinstance(payload.get("workflow_runs"), list):
        raise ProtocolError("GitHub workflow-runs API returned an invalid payload")
    raw_runs = payload["workflow_runs"]
    if type(payload.get("total_count")) is not int or payload["total_count"] != len(raw_runs):
        raise ProtocolError("GitHub workflow-runs response is truncated or has an invalid exact count")
    expected_by_file = dict(_required_workflows(policy))
    grouped: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for raw in raw_runs:
        if not isinstance(raw, Mapping):
            raise ProtocolError("GitHub workflow-runs response contains a non-object")
        run_sha = str(raw.get("head_sha") or "").lower()
        if run_sha != sha:
            raise ProtocolError(f"GitHub workflow run is for {run_sha or '<missing>'}, expected exact SHA {sha}")
        workflow_file = str(raw.get("path") or "")
        if workflow_file not in expected_by_file:
            continue
        workflow_name = str(raw.get("name") or "")
        if workflow_name != expected_by_file[workflow_file]:
            raise ProtocolError(
                f"GitHub workflow {workflow_file!r} is named {workflow_name or '<missing>'!r}, "
                f"expected {expected_by_file[workflow_file]!r}"
            )
        try:
            run_id = int(raw["id"])
        except (KeyError, TypeError, ValueError) as error:
            raise ProtocolError("GitHub workflow run has no integer id") from error
        normalized = {
            "databaseId": run_id, "headSha": run_sha,
            "workflowFile": workflow_file, "workflowName": workflow_name,
            "status": raw.get("status"), "conclusion": raw.get("conclusion"),
            "createdAt": raw.get("created_at"), "startedAt": raw.get("run_started_at"),
            "updatedAt": raw.get("updated_at"), "url": raw.get("html_url"),
            "event": raw.get("event"), "runAttempt": raw.get("run_attempt"),
        }
        grouped.setdefault((workflow_file, workflow_name), []).append(normalized)
    selected: list[dict[str, Any]] = []
    for identity in _required_workflows(policy):
        attempts = select_latest_workflow_attempts(
            grouped.get(identity, []), head_sha=sha, workflows=(identity[1],)
        )
        if attempts:
            selected.append(attempts[0])
    return selected


def _parse_github_jobs(output: str, *, run: Mapping[str, Any], sha: str) -> list[dict[str, Any]]:
    try:
        payload = json.loads(output)
    except json.JSONDecodeError as error:
        raise ProtocolError("GitHub jobs API returned invalid JSON") from error
    if not isinstance(payload, Mapping) or not isinstance(payload.get("jobs"), list):
        raise ProtocolError("GitHub jobs API returned an invalid payload")
    raw_jobs = payload["jobs"]
    if type(payload.get("total_count")) is not int or payload["total_count"] != len(raw_jobs):
        raise ProtocolError("GitHub jobs response is truncated or has an invalid exact count")
    run_id = int(run["databaseId"])
    jobs: list[dict[str, Any]] = []
    seen: set[int] = set()
    for raw in raw_jobs:
        if not isinstance(raw, Mapping):
            raise ProtocolError("GitHub jobs response contains a non-object")
        job_sha = str(raw.get("head_sha") or "").lower()
        if job_sha != sha:
            raise ProtocolError(f"GitHub job is for {job_sha or '<missing>'}, expected exact SHA {sha}")
        observed_run_id = raw.get("run_id")
        if type(observed_run_id) is not int or observed_run_id != run_id:
            raise ProtocolError(f"GitHub job belongs to run {observed_run_id!r}, expected {run_id}")
        raw_job_id = raw.get("id")
        job_id = raw_job_id if type(raw_job_id) is int and raw_job_id > 0 else None
        if job_id is not None:
            if job_id in seen:
                raise ProtocolError(f"GitHub jobs response repeats job identity ({run_id}, {job_id})")
            seen.add(job_id)
        jobs.append({
            "id": job_id, "run_id": observed_run_id, "head_sha": job_sha,
            "name": str(raw.get("name") or ""), "status": raw.get("status"),
            "conclusion": raw.get("conclusion"), "started_at": raw.get("started_at"),
            "completed_at": raw.get("completed_at"), "html_url": raw.get("html_url"),
        })
    return jobs


def github_runs(repo: str, sha: str, *, policy: Mapping[str, Any] | None = None) -> list[dict[str, Any]]:
    selected_policy = verification_policy_for_repo(repo) if policy is None else validate_verification_policy(policy)
    if selected_policy["repo"] != repo:
        raise ProtocolError(f"verification policy for {selected_policy['repo']!r} cannot query {repo!r}")
    if not SHA_RE.fullmatch(sha):
        raise ProtocolError(f"invalid exact-SHA GitHub query {sha!r}")
    result = _run(
        ("with-proxy", "gh", "api", f"repos/{repo}/actions/runs?head_sha={sha}&per_page=100"),
        check=True,
        timeout=DEFAULT_NETWORK_TIMEOUT,
    )
    runs = _parse_github_runs(result.stdout, sha, selected_policy)
    for run in runs:
        jobs_result = _run(
            ("with-proxy", "gh", "api", f"repos/{repo}/actions/runs/{run['databaseId']}/jobs?filter=latest&per_page=100"),
            check=True,
            timeout=DEFAULT_NETWORK_TIMEOUT,
        )
        run["jobs"] = _parse_github_jobs(jobs_result.stdout, run=run, sha=sha)
    return runs


def _github_state(run: Mapping[str, Any]) -> str:
    status = str(run.get("status") or "").lower()
    if status in {"requested", "waiting", "queued", "pending"}:
        return "pending"
    if status == "in_progress":
        return "running"
    if status != "completed":
        return "no_result"
    outcome = classify_check(run.get("status"), run.get("conclusion"))
    if outcome is CheckOutcome.PASSED:
        return "green"
    if outcome is CheckOutcome.FAILED:
        return "red"
    return "no_result"


def _github_patch(runs: Sequence[Mapping[str, Any]], sha: str, policy: Mapping[str, Any]) -> dict[str, Any]:
    policy = validate_verification_policy(policy)
    if not SHA_RE.fullmatch(sha):
        raise ProtocolError(f"invalid exact-SHA GitHub observation {sha!r}")
    expected_jobs = _required_github_jobs(policy)
    expected_count = policy["github"]["required_positive_count"]
    if expected_count != len(expected_jobs) or expected_count <= 0:
        raise ProtocolError("verification policy has a vacuous positive count")
    runs_by_workflow: dict[tuple[str, str], list[Mapping[str, Any]]] = {}
    run_workflows: dict[int, tuple[str, str]] = {}
    for run in runs:
        run_sha = str(run.get("headSha") or "").lower()
        if run_sha != sha:
            raise ProtocolError(f"GitHub run is for {run_sha or '<missing>'}, expected exact SHA {sha}")
        identity = (str(run.get("workflowFile") or ""), str(run.get("workflowName") or ""))
        if identity not in _required_workflows(policy):
            raise ProtocolError(f"unexpected GitHub workflow identity {identity!r}")
        run_id = run.get("databaseId")
        if type(run_id) is not int or run_id <= 0:
            raise ProtocolError("GitHub workflow evidence has no positive run id")
        previous = run_workflows.get(run_id)
        if previous is not None and previous != identity:
            raise ProtocolError(f"GitHub run {run_id} is reused for workflow identities {previous!r} and {identity!r}")
        run_workflows[run_id] = identity
        runs_by_workflow.setdefault(identity, []).append(run)

    observed_jobs: list[dict[str, Any]] = []
    run_ids: list[int] = []
    selected_job_ids: set[tuple[int, int]] = set()
    urls: list[str] = []
    reasons: list[str] = []
    for expected in expected_jobs:
        workflow_identity = (expected["workflow_file"], expected["workflow_name"])
        workflow_runs = runs_by_workflow.get(workflow_identity, [])
        if len(workflow_runs) > 1:
            raise ProtocolError(f"duplicate latest workflow evidence for {workflow_identity!r}")
        if not workflow_runs:
            reason = "no dereferenced workflow producer"
            observed_jobs.append({**expected, "state": "no_result", "reason": reason})
            reasons.append(f"{expected['job_name']}: {reason}")
            continue
        run = workflow_runs[0]
        run_id = int(run["databaseId"])
        if run_id not in run_ids:
            run_ids.append(run_id)
        run_url = str(run.get("url") or "")
        if run_url and run_url not in urls:
            urls.append(run_url)
        jobs = run.get("jobs")
        if not isinstance(jobs, list):
            raise ProtocolError(f"GitHub run {run_id} has no dereferenced jobs list")
        matches = [job for job in jobs if job.get("name") == expected["job_name"]]
        if len(matches) > 1:
            raise ProtocolError(f"duplicate required GitHub job {expected['job_name']!r} in run {run_id}")
        if not matches:
            reason = f"required job missing from {_github_state(run)} workflow run"
            observed_jobs.append({
                **expected, "run_id": run_id, "state": "no_result",
                "status": run.get("status"), "reason": reason,
            })
            reasons.append(f"{expected['job_name']}: {reason}")
            continue
        job = matches[0]
        if str(job.get("head_sha") or "").lower() != sha:
            raise ProtocolError(f"GitHub job is not for expected exact SHA {sha}")
        if type(job.get("run_id")) is not int or job["run_id"] != run_id:
            raise ProtocolError(f"GitHub job {job.get('id')} is not bound to selected run {run_id}")
        job_id = job.get("id")
        if type(job_id) is not int or job_id <= 0:
            reason = "required job has no positive dereferenced job identity"
            observed_jobs.append({
                **expected, "run_id": run_id, "state": "no_result",
                "status": job.get("status"), "conclusion": job.get("conclusion"),
                "url": str(job.get("html_url") or ""), "reason": reason,
            })
            reasons.append(f"{expected['job_name']}: {reason}")
            continue
        identity = (run_id, job_id)
        if identity in selected_job_ids:
            raise ProtocolError(f"required GitHub evidence repeats job identity {identity!r}")
        selected_job_ids.add(identity)
        state = _github_state(job)
        reason = None
        if state == "no_result":
            reason = f"required job completed without a verdict (conclusion={job.get('conclusion') or '<missing>'})"
            reasons.append(f"{expected['job_name']}: {reason}")
        job_url = str(job.get("html_url") or "")
        if job_url and job_url not in urls:
            urls.append(job_url)
        observed_jobs.append({
            **expected, "run_id": run_id, "job_id": job_id, "state": state,
            "status": job.get("status"), "conclusion": job.get("conclusion"),
            "url": job_url, "reason": reason,
        })

    states = [job["state"] for job in observed_jobs]
    positive_count = sum(state == "green" for state in states)
    if "red" in states:
        state = "red"
    elif positive_count == expected_count and len(observed_jobs) == expected_count:
        state = "green"
    elif "running" in states:
        state = "running"
    elif "pending" in states:
        state = "pending"
    else:
        state = "no_result"
    started = [str(run.get("startedAt") or run.get("createdAt")) for run in runs if run.get("startedAt") or run.get("createdAt")]
    finished = [str(run.get("updatedAt")) for run in runs if run.get("updatedAt")]
    return {"github": {
        "state": state,
        "started_at": min(started) if started else None,
        "finished_at": max(finished) if finished and (state in TERMINAL_VERIFICATION_STATES or state == "no_result") else None,
        "run_ids": run_ids,
        "urls": urls,
        "workflow_name": ", ".join(name for _, name in _required_workflows(policy)),
        "event": ", ".join(sorted({str(run.get("event")) for run in runs if run.get("event")})) or None,
        "required_positive_count": expected_count,
        "positive_count": positive_count,
        "jobs": observed_jobs,
        "last_poll_error": "; ".join(reasons) or None,
    }}


def hosted_status(args: argparse.Namespace) -> int:
    sha = args.sha.lower()
    if not SHA_RE.fullmatch(sha):
        raise ProtocolError("--sha must be a full 40-character commit SHA")
    policy = verification_policy_for_repo(args.repo)
    evidence = _github_patch(github_runs(args.repo, sha, policy=policy), sha, policy)["github"]
    report = {
        "schema_version": 1,
        "authority": "github-actions-exact-head-jobs",
        "repo": args.repo,
        "sha": sha,
        "policy_schema_version": policy["schema_version"],
        **evidence,
    }
    if args.json:
        print(json.dumps(report, sort_keys=True))
    else:
        print(
            f"HOSTED {str(evidence['state']).upper()} {args.repo}@{sha} "
            f"positive={evidence['positive_count']}/{evidence['required_positive_count']}"
        )
    return {"green": 0, "red": 3}.get(str(evidence["state"]), 4)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    verify = subparsers.add_parser(
        "verify-landing", aliases=["verify-landed-pr"],
        description="verify a PR replay SHA or commit SHA against a freshly fetched target",
    )
    verify.add_argument("reference", help="positive PR number or commit OID")
    verify.add_argument("--item")
    verify.add_argument("--claimed-oid", help="OID originally reported for a PR landing")
    verify.add_argument("--repo", default=DEFAULT_REPO)
    verify.add_argument("--source", type=Path)
    verify.add_argument("--target", default="main")
    verify.add_argument("--json", action="store_true")

    hosted = subparsers.add_parser(
        "hosted-status", description="verify the registered exact-head GitHub job authority"
    )
    hosted.add_argument("--repo", default=DEFAULT_REPO)
    hosted.add_argument("--sha", required=True)
    hosted.add_argument("--json", action="store_true")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.command in ("verify-landing", "verify-landed-pr"):
            return verify_landing(args)
        if args.command == "hosted-status":
            return hosted_status(args)
    except ProtocolError as error:
        print(f"ci-hub verification: {error}", file=sys.stderr)
        return 2
    raise AssertionError(f"unhandled command {args.command}")


if __name__ == "__main__":
    raise SystemExit(main())
