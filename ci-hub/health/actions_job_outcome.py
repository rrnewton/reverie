#!/usr/bin/env python3
"""Fail-closed authority for GitHub Actions no-result job failures.

The PR rollup is a useful index, not enough evidence to distinguish a product
failure from a runner that never started the workflow.  This module dereferences
exact Actions jobs and accepts two narrow shapes: a job that failed only while
setting up, and the registered merge-gate consequence of that exact prerequisite
having no result.  Every mismatch is an ordinary failed verification; callers
must retain the original red verdict.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime
import json
import re
import subprocess
import time
from urllib.parse import urlsplit


MAX_JOB_DEREFERENCES = 32
MAX_JOB_RESPONSE_BYTES = 256 * 1024
MAX_JOB_STEPS = 128
DEFAULT_JOB_CALL_TIMEOUT = 5.0
DEFAULT_JOB_TOTAL_TIMEOUT = 30.0

_ACTIONS_JOB_PATH = re.compile(
    r"^/(?P<owner>[A-Za-z0-9_.-]+)/(?P<repo>[A-Za-z0-9_.-]+)"
    r"/actions/runs/(?P<run>[1-9][0-9]*)/job/(?P<job>[1-9][0-9]*)$"
)
_FULL_SHA = re.compile(r"^[0-9a-f]{40}$")


@dataclass(frozen=True)
class JobReference:
    repo: str
    run_id: int
    job_id: int
    html_url: str


@dataclass(frozen=True)
class SetupOnlyVerification:
    """Result of dereferencing one exact failed check.

    ``accepted`` is the only value that changes a rollup classification.  The
    reason is retained for operator-visible diagnostics when evidence cannot be
    established.
    """

    accepted: bool
    reason: str
    run_id: int | None = None
    job_id: int | None = None
    kind: str = ""
    source_job_id: int | None = None
    completed_at: datetime | None = None


@dataclass(frozen=True)
class _VerifiedJob:
    """Identity-bound job payload shared by the two semantic predicates."""

    reference: JobReference
    steps: tuple[Mapping[str, object], ...]
    created_at: datetime
    started_at: datetime
    completed_at: datetime


_PREREQUISITE_SOURCE_NAME = "reverie-pin-ancestor-and-monotonic"
_PREREQUISITE_TRANSITION_SOURCE_NAME = "reverie-pin-ancestry-and-monotonicity"
_PREREQUISITE_LEGACY_SOURCE_NAME = "reverie-pin-is-latest-main"
_PREREQUISITE_GATE_NAME = "merge-gate-v4"
_PREREQUISITE_WORKFLOW_NAME = "Merge Gate"
_PREREQUISITE_WORKFLOW_PATH = ".github/workflows/merge-gate.yml"
_PREREQUISITE_LEGACY_GATE_STEPS: tuple[tuple[str, int, str, str], ...] = (
    ("Set up job", 1, "completed", "success"),
    ("Require the registered v4 gate definition", 2, "completed", "success"),
    ("Fetch the trusted receipt verifier", 3, "completed", "success"),
    ("Fetch the trusted check-outcome authority", 4, "completed", "success"),
    ("Require the latest-Reverie pin gate", 5, "completed", "failure"),
    ("Require successful CI or local validation", 6, "completed", "skipped"),
    ("Complete job", 7, "completed", "success"),
)
_PREREQUISITE_TRANSITION_GATE_STEPS: tuple[tuple[str, int, str, str], ...] = (
    *_PREREQUISITE_LEGACY_GATE_STEPS[:4],
    (
        "Require the Reverie-pin ancestry-and-monotonicity gate",
        5,
        "completed",
        "failure",
    ),
    *_PREREQUISITE_LEGACY_GATE_STEPS[5:],
)
_PREREQUISITE_GATE_STEPS: tuple[tuple[str, int, str, str], ...] = (
    *_PREREQUISITE_LEGACY_GATE_STEPS[:4],
    (
        "Require the Reverie-pin ancestor-and-monotonic gate",
        5,
        "completed",
        "failure",
    ),
    *_PREREQUISITE_LEGACY_GATE_STEPS[5:],
)


@dataclass(frozen=True)
class _PrerequisiteContract:
    """One reviewed, inseparable source-name/workflow/receipt identity."""

    source_name: str
    workflow_blob: str
    gate_steps: tuple[tuple[str, int, str, str], ...]


# These are paired contracts, not independent allowlists.  The first is the
# historical reviewed gate, the second records the later deployed old-name
# gate, the third records the ancestry+monotonicity transition, and the fourth
# is the owner-approved ancestor-and-monotonic identity.  A crossed name/blob or
# name/receipt combination must fail closed.
_PREREQUISITE_CONTRACTS = (
    _PrerequisiteContract(
        _PREREQUISITE_LEGACY_SOURCE_NAME,
        "579f5e7816c7e2844eadfd7018d95ee37c8d8640",
        _PREREQUISITE_LEGACY_GATE_STEPS,
    ),
    _PrerequisiteContract(
        _PREREQUISITE_LEGACY_SOURCE_NAME,
        "1502659be5e773f9bdfa9d6e6bb7346d77f03ad9",
        _PREREQUISITE_LEGACY_GATE_STEPS,
    ),
    _PrerequisiteContract(
        _PREREQUISITE_TRANSITION_SOURCE_NAME,
        "8aa4c71066d1575d6d3714827be02dd4f7c8b623",
        _PREREQUISITE_TRANSITION_GATE_STEPS,
    ),
    _PrerequisiteContract(
        _PREREQUISITE_SOURCE_NAME,
        "3ce527cb6dc7aede207d3cd07859d922a5241e38",
        _PREREQUISITE_GATE_STEPS,
    ),
)
_PREREQUISITE_SOURCE_NAMES = frozenset(
    contract.source_name for contract in _PREREQUISITE_CONTRACTS
)
_PREREQUISITE_WORKFLOW_BLOBS = frozenset(
    contract.workflow_blob for contract in _PREREQUISITE_CONTRACTS
)


@dataclass(frozen=True)
class RegisteredWorkflowBinding:
    """Dereferenced proof that an exact run used the reviewed v4 workflow."""

    repo: str
    run_id: int
    head_sha: str
    path: str
    blob_sha: str


def _reject(
    reason: str, reference: JobReference | None = None
) -> SetupOnlyVerification:
    return SetupOnlyVerification(
        accepted=False,
        reason=reason,
        run_id=reference.run_id if reference else None,
        job_id=reference.job_id if reference else None,
    )


def _canonical_reference(
    repo: str, check: Mapping[str, object]
) -> tuple[JobReference | None, str]:
    if check.get("__typename") != "CheckRun":
        return None, "rollup entry is not a CheckRun"
    url = check.get("detailsUrl")
    if not isinstance(url, str) or not url:
        return None, "CheckRun has no detailsUrl"
    parsed = urlsplit(url)
    if (
        parsed.scheme != "https"
        or parsed.netloc != "github.com"
        or parsed.query
        or parsed.fragment
    ):
        return None, "detailsUrl is not a canonical github.com Actions job URL"
    match = _ACTIONS_JOB_PATH.fullmatch(parsed.path)
    if match is None:
        return None, "detailsUrl is not an exact Actions run/job URL"
    observed_repo = f"{match.group('owner')}/{match.group('repo')}"
    if observed_repo != repo:
        return None, f"detailsUrl repository mismatch: {observed_repo!r} != {repo!r}"
    return (
        JobReference(
            repo=repo,
            run_id=int(match.group("run")),
            job_id=int(match.group("job")),
            html_url=url,
        ),
        "",
    )


def _timestamp(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed if parsed.tzinfo is not None else None


def _positive_int(value: object) -> int | None:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        return None
    return value


def _verify_exact_job_payload(
    *,
    repo: str,
    check: Mapping[str, object],
    head_sha: str,
    payload: object,
) -> tuple[_VerifiedJob | None, SetupOnlyVerification | None]:
    """Bind one payload to the exact rollup entry before inspecting semantics."""
    reference, reference_error = _canonical_reference(repo, check)
    if reference is None:
        return None, _reject(reference_error)
    if _FULL_SHA.fullmatch(head_sha) is None:
        return None, _reject("PR head is not an exact lowercase 40-hex SHA", reference)
    if not isinstance(payload, Mapping):
        return None, _reject("job API response is not an object", reference)
    if any(key in payload for key in ("jobs", "total_count", "next")):
        return None, _reject("job API response is a paginated collection", reference)

    expected_api_root = f"https://api.github.com/repos/{repo}"
    exact_fields: tuple[tuple[str, object], ...] = (
        ("id", reference.job_id),
        ("run_id", reference.run_id),
        ("head_sha", head_sha),
        ("url", f"{expected_api_root}/actions/jobs/{reference.job_id}"),
        ("html_url", reference.html_url),
        ("run_url", f"{expected_api_root}/actions/runs/{reference.run_id}"),
        ("check_run_url", f"{expected_api_root}/check-runs/{reference.job_id}"),
        ("status", "completed"),
        ("conclusion", "failure"),
        ("name", check.get("name")),
        ("workflow_name", check.get("workflowName")),
        ("started_at", check.get("startedAt")),
        ("completed_at", check.get("completedAt")),
    )
    if not isinstance(check.get("name"), str) or not check.get("name"):
        return None, _reject("CheckRun name is missing", reference)
    if not isinstance(check.get("workflowName"), str) or not check.get("workflowName"):
        return None, _reject("CheckRun workflowName is missing", reference)
    if check.get("status") != "COMPLETED" or check.get("conclusion") != "FAILURE":
        return None, _reject("CheckRun is not a completed failure", reference)
    if _positive_int(payload.get("id")) != reference.job_id:
        return None, _reject("job id is missing, non-integer, or mismatched", reference)
    if _positive_int(payload.get("run_id")) != reference.run_id:
        return None, _reject(
            "job run_id is missing, non-integer, or mismatched", reference
        )
    for field, expected in exact_fields:
        observed = payload.get(field)
        if observed != expected:
            return None, _reject(
                f"job {field} mismatch: {observed!r} != {expected!r}",
                reference,
            )

    created = _timestamp(payload.get("created_at"))
    started = _timestamp(payload.get("started_at"))
    completed = _timestamp(payload.get("completed_at"))
    if created is None or started is None or completed is None:
        return None, _reject("job timestamps are missing or malformed", reference)
    if not created <= started <= completed:
        return None, _reject("job timestamps are out of order", reference)

    steps = payload.get("steps")
    if not isinstance(steps, Sequence) or isinstance(steps, (str, bytes)):
        return None, _reject("job steps are missing or malformed", reference)
    if len(steps) > MAX_JOB_STEPS:
        return None, _reject(
            f"job step bound exceeded: {len(steps)} > {MAX_JOB_STEPS}",
            reference,
        )
    checked_steps: list[Mapping[str, object]] = []
    for index, step in enumerate(steps, start=1):
        if not isinstance(step, Mapping):
            return None, _reject(f"job step {index} is not an object", reference)
        checked_steps.append(step)
    return (
        _VerifiedJob(
            reference=reference,
            steps=tuple(checked_steps),
            created_at=created,
            started_at=started,
            completed_at=completed,
        ),
        None,
    )


def _step_interval_error(job: _VerifiedJob, step: Mapping[str, object]) -> str:
    started = _timestamp(step.get("started_at"))
    completed = _timestamp(step.get("completed_at"))
    if started is None or completed is None:
        return "step timestamps are missing or malformed"
    if not job.started_at <= started <= completed <= job.completed_at:
        return "step timestamps are outside the job interval"
    return ""


def verify_registered_workflow_payloads(
    *,
    repo: str,
    run_id: int,
    head_sha: str,
    run_payload: object,
    contents_payload: object,
) -> tuple[RegisteredWorkflowBinding | None, str]:
    """Bind an exact run to the reviewed workflow blob that declares ``needs``."""
    if _FULL_SHA.fullmatch(head_sha) is None:
        return None, "workflow binding head is not an exact lowercase 40-hex SHA"
    if not isinstance(run_payload, Mapping):
        return None, "workflow-run API response is not an object"
    if not isinstance(contents_payload, Mapping):
        return None, "workflow-contents API response is not an object"
    if any(key in run_payload for key in ("workflow_runs", "total_count", "next")):
        return None, "workflow-run API response is a paginated collection"
    if any(key in contents_payload for key in ("items", "total_count", "next")):
        return None, "workflow-contents API response is a paginated collection"

    api_root = f"https://api.github.com/repos/{repo}"
    exact_run_fields: tuple[tuple[str, object], ...] = (
        ("id", run_id),
        ("name", _PREREQUISITE_WORKFLOW_NAME),
        ("path", _PREREQUISITE_WORKFLOW_PATH),
        ("head_sha", head_sha),
        ("url", f"{api_root}/actions/runs/{run_id}"),
        ("jobs_url", f"{api_root}/actions/runs/{run_id}/jobs"),
    )
    if _positive_int(run_payload.get("id")) != run_id:
        return None, "workflow-run id is missing, non-integer, or mismatched"
    if _positive_int(run_payload.get("run_attempt")) is None:
        return None, "workflow-run attempt is missing or non-positive"
    for field, expected in exact_run_fields:
        observed = run_payload.get(field)
        if observed != expected:
            return None, (
                f"workflow-run {field} mismatch: {observed!r} != {expected!r}"
            )
    workflow_url = run_payload.get("workflow_url")
    workflow_prefix = f"{api_root}/actions/workflows/"
    if (
        not isinstance(workflow_url, str)
        or not workflow_url.startswith(workflow_prefix)
        or _positive_int(
            int(workflow_url.removeprefix(workflow_prefix))
            if workflow_url.removeprefix(workflow_prefix).isdigit()
            else None
        )
        is None
    ):
        return None, "workflow-run workflow_url is missing or non-canonical"

    exact_content_fields: tuple[tuple[str, object], ...] = (
        ("type", "file"),
        ("name", "merge-gate.yml"),
        ("path", _PREREQUISITE_WORKFLOW_PATH),
    )
    for field, expected in exact_content_fields:
        observed = contents_payload.get(field)
        if observed != expected:
            return None, (
                f"workflow contents {field} mismatch: {observed!r} != {expected!r}"
            )
    blob_sha = contents_payload.get("sha")
    if blob_sha not in _PREREQUISITE_WORKFLOW_BLOBS:
        return None, f"workflow contents sha mismatch: {blob_sha!r} is not reviewed"
    return (
        RegisteredWorkflowBinding(
            repo=repo,
            run_id=run_id,
            head_sha=head_sha,
            path=_PREREQUISITE_WORKFLOW_PATH,
            blob_sha=str(blob_sha),
        ),
        "",
    )


def verify_setup_only_job_payload(
    *,
    repo: str,
    check: Mapping[str, object],
    head_sha: str,
    payload: object,
) -> SetupOnlyVerification:
    """Verify the complete identity and sole-step shape of one Actions job."""
    job, rejection = _verify_exact_job_payload(
        repo=repo,
        check=check,
        head_sha=head_sha,
        payload=payload,
    )
    if rejection is not None:
        return rejection
    assert job is not None
    # This deliberately rejects even a later successful/skipped step with a
    # timestamp: the accepted authority says no workflow/product step started.
    if len(job.steps) != 1:
        return _reject(
            "setup-only job must contain exactly one step, observed "
            f"{len(job.steps)}",
            job.reference,
        )
    step = job.steps[0]
    expected_step: tuple[tuple[str, object], ...] = (
        ("name", "Set up job"),
        ("number", 1),
        ("status", "completed"),
        ("conclusion", "failure"),
    )
    for field, expected in expected_step:
        observed = step.get(field)
        if observed != expected or (
            field == "number" and _positive_int(observed) is None
        ):
            return _reject(
                f"setup step {field} mismatch: {observed!r} != {expected!r}",
                job.reference,
            )
    interval_error = _step_interval_error(job, step)
    if interval_error:
        return _reject(f"setup {interval_error}", job.reference)
    return SetupOnlyVerification(
        accepted=True,
        reason="exact job executed only failed Set up job; zero workflow steps ran",
        run_id=job.reference.run_id,
        job_id=job.reference.job_id,
        kind="setup-only",
        completed_at=job.completed_at,
    )


def verify_prerequisite_no_result_job_payload(
    *,
    repo: str,
    check: Mapping[str, object],
    head_sha: str,
    payload: object,
    source_check: Mapping[str, object],
    source: SetupOnlyVerification,
    workflow: RegisteredWorkflowBinding,
) -> SetupOnlyVerification:
    """Verify the registered v4 gate consequence of a setup-only prerequisite.

    This is intentionally not a generic "failed gate step" carve-out.  It
    recognizes one versioned contract only after the source job independently
    proved the direct setup-only predicate in the same selected rollup/run.
    """
    job, rejection = _verify_exact_job_payload(
        repo=repo,
        check=check,
        head_sha=head_sha,
        payload=payload,
    )
    if rejection is not None:
        return rejection
    assert job is not None
    if not source.accepted or source.kind != "setup-only":
        return _reject(
            "prerequisite source did not prove direct setup-only no-result",
            job.reference,
        )
    if (
        source_check.get("name") not in _PREREQUISITE_SOURCE_NAMES
        or source_check.get("workflowName") != _PREREQUISITE_WORKFLOW_NAME
    ):
        return _reject(
            "prerequisite source is not the registered Reverie-pin gate",
            job.reference,
        )
    if (
        check.get("name") != _PREREQUISITE_GATE_NAME
        or check.get("workflowName") != _PREREQUISITE_WORKFLOW_NAME
    ):
        return _reject(
            "failed job is not the registered merge-gate-v4 consequence",
            job.reference,
        )
    if source.run_id != job.reference.run_id:
        return _reject(
            "prerequisite source and consequence do not share an exact run",
            job.reference,
        )
    contract = next(
        (
            candidate
            for candidate in _PREREQUISITE_CONTRACTS
            if candidate.source_name == source_check.get("name")
            and candidate.workflow_blob == workflow.blob_sha
        ),
        None,
    )
    if contract is None:
        return _reject(
            "prerequisite source name and workflow blob are not one reviewed contract",
            job.reference,
        )
    expected_workflow = RegisteredWorkflowBinding(
        repo=repo,
        run_id=job.reference.run_id,
        head_sha=head_sha,
        path=_PREREQUISITE_WORKFLOW_PATH,
        blob_sha=contract.workflow_blob,
    )
    if workflow != expected_workflow:
        return _reject(
            "prerequisite chain is not bound to the reviewed v4 workflow blob",
            job.reference,
        )
    if source.job_id is None or source.job_id == job.reference.job_id:
        return _reject(
            "prerequisite source job identity is missing or self-referential",
            job.reference,
        )
    if source.completed_at is None or source.completed_at > job.created_at:
        return _reject(
            "prerequisite source did not complete before consequence creation",
            job.reference,
        )
    if len(job.steps) != len(contract.gate_steps):
        return _reject(
            "registered merge-gate-v4 must contain exactly "
            f"{len(contract.gate_steps)} steps, observed {len(job.steps)}",
            job.reference,
        )
    previous_completed: datetime | None = None
    for step, expected in zip(job.steps, contract.gate_steps, strict=True):
        expected_name, expected_number, expected_status, expected_conclusion = expected
        observed = (
            step.get("name"),
            step.get("number"),
            step.get("status"),
            step.get("conclusion"),
        )
        if observed != expected or _positive_int(step.get("number")) is None:
            return _reject(
                "registered merge-gate-v4 step mismatch: "
                f"{observed!r} != {expected!r}",
                job.reference,
            )
        interval_error = _step_interval_error(job, step)
        if interval_error:
            return _reject(f"registered merge-gate-v4 {interval_error}", job.reference)
        step_started = _timestamp(step.get("started_at"))
        step_completed = _timestamp(step.get("completed_at"))
        assert step_started is not None and step_completed is not None
        if previous_completed is not None and step_started < previous_completed:
            return _reject(
                "registered merge-gate-v4 steps overlap or run out of order",
                job.reference,
            )
        previous_completed = step_completed
    return SetupOnlyVerification(
        accepted=True,
        reason=(
            "registered merge-gate-v4 failed only because its exact "
            "Reverie-pin prerequisite had setup-only no-result"
        ),
        run_id=job.reference.run_id,
        job_id=job.reference.job_id,
        kind="prerequisite-no-result",
        source_job_id=source.job_id,
        completed_at=job.completed_at,
    )


@dataclass(frozen=True)
class _FetchResult:
    payload: object | None
    error: str


class GitHubActionsJobAuthority:
    """Bounded, cached exact-job dereferencer used by the gh PR-status engine."""

    def __init__(
        self,
        repo: str,
        *,
        net_wrapper: Sequence[str],
        gh_cmd: str = "gh",
        deadline: float | None = None,
        max_dereferences: int = MAX_JOB_DEREFERENCES,
        call_timeout: float = DEFAULT_JOB_CALL_TIMEOUT,
    ) -> None:
        self.repo = repo
        self.net_wrapper = tuple(net_wrapper)
        self.gh_cmd = gh_cmd
        self.deadline = (
            deadline
            if deadline is not None
            else time.monotonic() + DEFAULT_JOB_TOTAL_TIMEOUT
        )
        self.max_dereferences = max(0, max_dereferences)
        self.call_timeout = max(0.0, call_timeout)
        self._dereferences = 0
        self._cache: dict[int, _FetchResult] = {}
        self._run_cache: dict[int, _FetchResult] = {}
        self._workflow_cache: dict[str, _FetchResult] = {}

    def __call__(
        self, repo: str, check: Mapping[str, object], head_sha: str
    ) -> SetupOnlyVerification:
        if repo != self.repo:
            return _reject(f"authority repository mismatch: {repo!r} != {self.repo!r}")
        reference, reference_error = _canonical_reference(repo, check)
        if reference is None:
            return _reject(reference_error)
        fetched = self._cached_fetch(
            self._cache,
            reference.job_id,
            f"repos/{self.repo}/actions/jobs/{reference.job_id}",
            "exact-job",
        )
        if fetched.error:
            return _reject(fetched.error, reference)
        return verify_setup_only_job_payload(
            repo=repo,
            check=check,
            head_sha=head_sha,
            payload=fetched.payload,
        )

    def verify_failures(
        self,
        repo: str,
        checks: Sequence[Mapping[str, object]],
        head_sha: str,
    ) -> tuple[SetupOnlyVerification, ...]:
        """Classify a selected failed-check set with order-independent causality.

        The first pass independently dereferences every exact job and identifies
        direct setup-only sources.  Only the second pass can recognize the one
        registered downstream gate consequence, so input order cannot change
        the answer and a generic gate failure never inherits the carve-out.
        """
        direct = [self(repo, check, head_sha) for check in checks]
        sources_by_run: dict[
            int, list[tuple[Mapping[str, object], SetupOnlyVerification]]
        ] = {}
        for check, verification in zip(checks, direct, strict=True):
            if (
                verification.accepted
                and verification.kind == "setup-only"
                and verification.run_id is not None
                and check.get("name") in _PREREQUISITE_SOURCE_NAMES
                and check.get("workflowName") == _PREREQUISITE_WORKFLOW_NAME
            ):
                sources_by_run.setdefault(verification.run_id, []).append(
                    (check, verification)
                )

        classified = list(direct)
        for index, (check, verification) in enumerate(zip(checks, direct, strict=True)):
            if verification.accepted or (
                check.get("name") != _PREREQUISITE_GATE_NAME
                or check.get("workflowName") != _PREREQUISITE_WORKFLOW_NAME
            ):
                continue
            reference, reference_error = _canonical_reference(repo, check)
            if reference is None:
                classified[index] = _reject(reference_error)
                continue
            sources = sources_by_run.get(reference.run_id, [])
            if len(sources) != 1:
                classified[index] = _reject(
                    "registered merge-gate-v4 consequence requires exactly one "
                    "independently verified setup-only Reverie-pin source in the "
                    f"same selected run; observed {len(sources)}",
                    reference,
                )
                continue
            fetched = self._cache.get(reference.job_id)
            if fetched is None or fetched.error or fetched.payload is None:
                # The direct pass already carries the exact fetch error.  Keep
                # that conservative result rather than inventing new evidence.
                continue
            source_check, source = sources[0]
            workflow, workflow_error = self._registered_workflow_binding(
                reference.run_id, head_sha
            )
            if workflow is None:
                classified[index] = _reject(workflow_error, reference)
                continue
            classified[index] = verify_prerequisite_no_result_job_payload(
                repo=repo,
                check=check,
                head_sha=head_sha,
                payload=fetched.payload,
                source_check=source_check,
                source=source,
                workflow=workflow,
            )
        return tuple(classified)

    def _registered_workflow_binding(
        self, run_id: int, head_sha: str
    ) -> tuple[RegisteredWorkflowBinding | None, str]:
        run = self._cached_fetch(
            self._run_cache,
            run_id,
            f"repos/{self.repo}/actions/runs/{run_id}",
            "workflow-run",
        )
        if run.error:
            return None, run.error
        workflow = self._cached_fetch(
            self._workflow_cache,
            head_sha,
            "repos/"
            f"{self.repo}/contents/{_PREREQUISITE_WORKFLOW_PATH}?ref={head_sha}",
            "workflow-contents",
        )
        if workflow.error:
            return None, workflow.error
        return verify_registered_workflow_payloads(
            repo=self.repo,
            run_id=run_id,
            head_sha=head_sha,
            run_payload=run.payload,
            contents_payload=workflow.payload,
        )

    def _cached_fetch(
        self,
        cache: dict[object, _FetchResult],
        key: object,
        endpoint: str,
        label: str,
    ) -> _FetchResult:
        fetched = cache.get(key)
        if fetched is not None:
            return fetched
        if self._dereferences >= self.max_dereferences:
            fetched = _FetchResult(None, f"{label} dereference budget exhausted")
        else:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0 or self.call_timeout <= 0:
                fetched = _FetchResult(None, f"{label} deadline exhausted")
            else:
                self._dereferences += 1
                fetched = self._fetch_endpoint(
                    endpoint,
                    label=label,
                    timeout=min(self.call_timeout, remaining),
                )
        cache[key] = fetched
        return fetched

    def _fetch_endpoint(
        self, endpoint: str, *, label: str, timeout: float
    ) -> _FetchResult:
        command = [
            *self.net_wrapper,
            self.gh_cmd,
            "api",
            "--method",
            "GET",
            endpoint,
        ]
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
                check=False,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            return _FetchResult(None, f"{label} API exceeded {timeout:.1f}s")
        except OSError as error:
            return _FetchResult(None, f"{label} API could not start: {error}")
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "unknown error"
            return _FetchResult(None, f"{label} API failed: {detail[:300]}")
        if len(result.stdout.encode("utf-8")) > MAX_JOB_RESPONSE_BYTES:
            return _FetchResult(
                None,
                f"{label} API response exceeded "
                f"{MAX_JOB_RESPONSE_BYTES} byte bound",
            )
        if not result.stdout.strip():
            return _FetchResult(None, f"{label} API returned empty output")
        try:
            payload = json.loads(result.stdout)
        except json.JSONDecodeError:
            return _FetchResult(None, f"{label} API returned malformed JSON")
        if not isinstance(payload, Mapping):
            return _FetchResult(None, f"{label} API returned a non-object schema")
        return _FetchResult(payload, "")
