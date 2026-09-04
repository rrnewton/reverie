#!/usr/bin/env python3
"""Report live GitHub Actions health for the current main commit."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from dataclasses import asdict, dataclass, replace
from datetime import datetime, timezone
from pathlib import Path
from typing import Sequence

CI_HUB = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(CI_HUB))

from check_outcome import CheckOutcome, classify_check, select_latest_workflow_attempts

DEFAULT_REPOS = (
    "rrnewton/dev-hermit",
    "rrnewton/hermit",
    "rrnewton/reverie",
)
DEFAULT_RUN_LIMIT = 100
# Basis: the complete three-repository main-health query measured 7.28s on
# a devserver on 2026-08-03. Each repository makes two gh calls. A 15s call cap
# gives more than 6x the measured average call time; the 60s overall deadline
# gives more than 8x the measured complete-query time while remaining useful to
# an interactive health command. CI deliberately overrides these lower.
DEFAULT_CALL_TIMEOUT = float(os.environ.get("CI_HUB_MAIN_HEALTH_TIMEOUT", "15"))
DEFAULT_OVERALL_DEADLINE = float(os.environ.get("CI_HUB_MAIN_HEALTH_DEADLINE", "60"))
# `cancelled` is ambiguous and cannot be split by conclusion OR by duration. A
# self-inflicted `timeout-minutes` kill (our own box firing on a hang -- REAL
# signal about the code) and an externally-imposed cancel (a superseding push, a
# queue eviction, a manual stop -- the ABSENCE of a result) BOTH report
# conclusion=cancelled. Duration cannot tell them apart either: a concurrency
# supersede was observed cancelled 4s UNDER a 300s cap (task
# cancellation_taxonomy_distinguish_self), indistinguishable by wall-time from a
# 300s timeout. The reliable discriminator is the run's check annotations: a
# job killed by `timeout-minutes` carries GitHub's "exceeded the maximum
# execution time" annotation; a concurrency cancel carries "higher priority
# waiting request"; a manual/queue cancel carries neither. Only the
# self-timeout annotation promotes cancelled -> RED (a hang the box exists to
# surface). Its absence leaves cancelled as NO_RESULT, so a supersede/manual
# cancel can never manufacture a false red -- preserving cancelled-run-classified-as-red.
_SELF_TIMEOUT_ANNOTATION = "exceeded the maximum execution time"


def is_self_timeout(messages: Sequence[str]) -> bool:
    """Whether any check annotation is GitHub's `timeout-minutes` kill notice."""
    return any(_SELF_TIMEOUT_ANNOTATION in str(m).lower() for m in messages)


@dataclass(frozen=True)
class MainRun:
    workflow: str
    head_sha: str
    status: str
    conclusion: str
    url: str
    created_at: str
    run_id: str = ""
    # True only for a `cancelled` run whose annotations prove a self-inflicted
    # `timeout-minutes` kill (a hang); a supersede/manual/queue cancel is False.
    self_timeout: bool = False


@dataclass(frozen=True)
class RepoMainHealth:
    repo: str
    main_sha: str
    state: str
    runs: tuple[MainRun, ...]
    available: bool = True
    reason: str = ""


class RepoUnavailable(RuntimeError):
    """One live GitHub query exceeded its explicit wall-time budget."""


def _run_gh(command: Sequence[str], *, timeout: float) -> str:
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
    except FileNotFoundError as error:
        raise RuntimeError(
            "with-proxy was not found; GitHub queries must use the proxy wrapper"
        ) from error
    except subprocess.TimeoutExpired:
        raise RepoUnavailable(
            f"{' '.join(command)} exceeded the {timeout:.1f}s call timeout"
        ) from None

    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "unknown error"
        raise RuntimeError(f"{' '.join(command)} failed: {detail}")
    return result.stdout


def fetch_main_sha(repo: str, *, timeout: float = DEFAULT_CALL_TIMEOUT) -> str:
    output = _run_gh(
        (
            "with-proxy",
            "gh",
            "api",
            f"repos/{repo}/commits/main",
            "--jq",
            ".sha",
        ),
        timeout=timeout,
    )
    sha = output.strip()
    if len(sha) != 40:
        raise RuntimeError(f"{repo}: invalid main SHA from GitHub: {sha!r}")
    return sha


def fetch_main_runs(
    repo: str,
    limit: int = DEFAULT_RUN_LIMIT,
    *,
    head_sha: str,
    timeout: float = DEFAULT_CALL_TIMEOUT,
) -> list[MainRun]:
    output = _run_gh(
        (
            "with-proxy",
            "gh",
            "run",
            "list",
            "-R",
            repo,
            "--branch",
            "main",
            "--event",
            "push",
            "--limit",
            str(limit),
            "--json",
            "databaseId,workflowName,headSha,status,conclusion,url,createdAt",
        ),
        timeout=timeout,
    )
    try:
        payload = json.loads(output)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{repo}: gh run list returned invalid JSON") from error
    if not isinstance(payload, list):
        raise RuntimeError(f"{repo}: gh run list returned a non-list payload")

    runs: list[MainRun] = []
    for raw in select_latest_workflow_attempts(payload, head_sha=head_sha):
        if not isinstance(raw, dict):
            raise RuntimeError(f"{repo}: malformed workflow run: {raw!r}")
        runs.append(
            MainRun(
                workflow=str(raw.get("workflowName") or "unknown-workflow"),
                head_sha=str(raw.get("headSha") or ""),
                status=str(raw.get("status") or "").lower(),
                conclusion=str(raw.get("conclusion") or "").lower(),
                url=str(raw.get("url") or ""),
                created_at=str(raw.get("createdAt") or ""),
                run_id=str(raw.get("databaseId") or ""),
            )
        )
    return runs


def fetch_run_annotations(repo: str, run_id: str, *, timeout: float) -> list[str]:
    """Every check annotation message across a run's jobs, lowercased.

    Two `gh api` calls per run (job ids, then per-job annotations); used only to
    disambiguate a `cancelled` run, so it runs at most once per current-tip
    cancelled workflow. A fetch failure raises like any other gh call and is
    handled by the caller as "annotations unavailable" -> stay NO_RESULT (safe).
    """
    jobs = _run_gh(
        (
            "with-proxy",
            "gh",
            "api",
            f"repos/{repo}/actions/runs/{run_id}/jobs",
            "--jq",
            ".jobs[].id",
        ),
        timeout=timeout,
    )
    messages: list[str] = []
    for line in jobs.splitlines():
        job_id = line.strip()
        if not job_id:
            continue
        annotations = _run_gh(
            (
                "with-proxy",
                "gh",
                "api",
                f"repos/{repo}/check-runs/{job_id}/annotations",
                "--jq",
                ".[].message",
            ),
            timeout=timeout,
        )
        messages.extend(
            msg.strip().lower() for msg in annotations.splitlines() if msg.strip()
        )
    return messages


# How long an authority may sit unresolved before its silence is a HOLE rather
# than a normal post-push window.
#
# ARGUED FROM THE RUNS, NOT PICKED. Measured 2026-08-08 over the last 60 main
# runs on rrnewton/dev-hermit (55 completed):
#     all workflows        median   40s   p90 526s   max 758s
#     the ci-hub authority ("Dev-hermit operational tooling", n=17)
#                          median  393s              max 758s
#     Portability max 51s, Demo review gate max 22s
# So 758s is the slowest LEGITIMATE completion observed. 900s clears it by 142s
# (~19% margin, ~1.7x the p90). At the same moment three runs were sitting
# unresolved at 372s, 9198s and 15424s -- the stuck population starts at ~12x
# this threshold, so the separation is wide and the exact value is not delicate.
# A too-low threshold costs a wasted check; it authorises nothing.
DEFAULT_PENDING_STALE_SECS = float(
    os.environ.get("CI_HUB_MAIN_PENDING_STALE_SECS", "900")
)


def _parse_iso(value: str) -> datetime | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def pending_age_secs(runs: Sequence[MainRun], now: datetime | None = None) -> float | None:
    """Age of the OLDEST unresolved authority run, or None if none is unresolved.

    THE OLDEST, not the newest: with queue-depth-1 concurrency a fresh push can
    sit a new run in front of one that has been stuck for hours, and taking the
    newest would let each push reset the clock and hide the hole indefinitely.
    Returns None -- never 0.0 -- when nothing is pending, so "no pending run" and
    "pending for no time at all" stay distinguishable.
    """
    moment = now or datetime.now(timezone.utc)
    ages = []
    for run in runs:
        if classify_check(run.status, run.conclusion, self_timeout=run.self_timeout) \
                is not CheckOutcome.NO_RESULT:
            continue
        created = _parse_iso(run.created_at)
        if created is None:
            continue
        ages.append((moment - created).total_seconds())
    return max(ages) if ages else None


def classify_current_runs(runs: Sequence[MainRun]) -> str:
    """Classify a repo's main authority.

    `pending` vs `stale-pending` is the whole point of the time argument. The
    gate used to alarm on explicit red and stay silent otherwise, so an authority
    that never resolved was indistinguishable from a green one -- and the ci-hub
    shard authority is absent or unreported on 9 of the last 25 main commits
    (36%), because queue-depth-1 concurrency lets newer pushes supersede pending
    runs. The fail-open was in the ALARM, not in the reader.

    The naive repair -- alarm on `pending` -- would fire after EVERY push and be
    an alarm no coordinator can satisfy, which is the anti-pattern removed from
    three other gates today. So a fresh pending window stays silent and only a
    pending that outlives `stale_after` is a finding.
    """
    if not runs:
        return "none"
    # A self-timeout cancel is a genuine BAD answer (a hang), not a hole: it
    # alarms like any other red so the box is never silent (task
    # cancellation_taxonomy_distinguish_self).
    outcomes = tuple(
        classify_check(
            run.status,
            run.conclusion,
            self_timeout=run.self_timeout,
        )
        for run in runs
    )
    if any(outcome is CheckOutcome.FAILED for outcome in outcomes):
        return "red"
    if any(outcome is CheckOutcome.NO_RESULT for outcome in outcomes):
        return "pending"
    return "green"


def _remaining_timeout(deadline: float, per_call_timeout: float, repo: str) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise RepoUnavailable(f"{repo}: overall GitHub-main deadline exhausted")
    return min(per_call_timeout, remaining)


def evaluate_repo(
    repo: str,
    limit: int = DEFAULT_RUN_LIMIT,
    *,
    per_call_timeout: float = DEFAULT_CALL_TIMEOUT,
    deadline: float | None = None,
) -> RepoMainHealth:
    deadline = (
        time.monotonic() + (2 * per_call_timeout) if deadline is None else deadline
    )
    main_sha = fetch_main_sha(
        repo,
        timeout=_remaining_timeout(deadline, per_call_timeout, repo),
    )
    candidates = [
        run
        for run in fetch_main_runs(
            repo,
            limit,
            head_sha=main_sha,
            timeout=_remaining_timeout(deadline, per_call_timeout, repo),
        )
    ]

    # Disambiguate cancelled current-tip runs: fetch annotations only for them
    # (rare) and promote a self-timeout kill to RED. A failed/absent annotation
    # fetch leaves the run NO_RESULT (safe): never invent a red we cannot prove.
    enriched: list[MainRun] = []
    for run in candidates:
        if run.conclusion == "cancelled" and run.run_id:
            try:
                messages = fetch_run_annotations(
                    repo,
                    run.run_id,
                    timeout=_remaining_timeout(deadline, per_call_timeout, repo),
                )
                if is_self_timeout(messages):
                    run = replace(run, self_timeout=True)
            except (RepoUnavailable, RuntimeError):
                pass
        enriched.append(run)
    current_runs = tuple(sorted(enriched, key=lambda run: run.workflow.lower()))
    return RepoMainHealth(
        repo=repo,
        main_sha=main_sha,
        state=classify_current_runs(current_runs),
        runs=current_runs,
    )


def collect_health(
    repos: Sequence[str],
    limit: int,
    *,
    per_call_timeout: float,
    overall_deadline: float,
) -> list[RepoMainHealth]:
    deadline = time.monotonic() + overall_deadline
    health: list[RepoMainHealth] = []
    for repo in repos:
        if deadline - time.monotonic() <= 0:
            health.append(
                RepoMainHealth(
                    repo=repo,
                    main_sha="",
                    state="unknown",
                    runs=(),
                    available=False,
                    reason=f"overall deadline {overall_deadline:.1f}s exhausted",
                )
            )
            continue
        try:
            health.append(
                evaluate_repo(
                    repo,
                    limit,
                    per_call_timeout=per_call_timeout,
                    deadline=deadline,
                )
            )
        except (RepoUnavailable, RuntimeError) as error:
            health.append(
                RepoMainHealth(
                    repo=repo,
                    main_sha="",
                    state="unknown",
                    runs=(),
                    available=False,
                    reason=str(error),
                )
            )
    return health


def stale_pending_repos(
    health: Sequence[RepoMainHealth],
    *,
    stale_after: float = DEFAULT_PENDING_STALE_SECS,
    now: datetime | None = None,
) -> dict[str, float]:
    """Repos whose oldest unresolved authority run has outlived `stale_after`.

    An unresolved run whose creation time cannot be parsed is NOT reported here.
    That is deliberate: calling it stale would assert it is "past the bound",
    which is a claim the data does not support -- absence of a timestamp is
    unmeasured, not old. Real GitHub payloads always carry createdAt, so this is
    a hypothetical; inventing a finding from it would be the same absence-reads-
    as-fact error this whole gate is being repaired for, pointed the other way.
    """
    stale: dict[str, float] = {}
    for repo in health:
        if repo.state != "pending":
            continue
        age = pending_age_secs(repo.runs, now)
        if age is not None and age > stale_after:
            stale[repo.repo] = age
    return stale


def overall_state(
    health: Sequence[RepoMainHealth],
    *,
    stale_after: float = DEFAULT_PENDING_STALE_SECS,
    now: datetime | None = None,
) -> str:
    available = [repo for repo in health if repo.available]
    states = {repo.state for repo in available}
    if "red" in states:
        return "red"
    if len(available) != len(health):
        return "degraded"
    if "pending" in states:
        # A fresh pending window is normal and stays quiet; one that outlives the
        # bound is a HOLE. Deciding this here rather than inside
        # classify_current_runs keeps that function a pure classifier of run
        # OUTCOMES and puts the time judgement where the runs are available.
        if stale_pending_repos(available, stale_after=stale_after, now=now):
            return "stale-pending"
        return "pending"
    if "none" in states:
        return "none"
    return "green" if available else "degraded"


def render_report(health: Sequence[RepoMainHealth]) -> str:
    state = overall_state(health)
    if state == "red":
        heading = "HARD WARNING: GITHUB MAIN IS RED"
    elif state == "green":
        heading = "GitHub main health: GREEN"
    elif state == "pending":
        heading = "GitHub main health: PENDING (do not claim green)"
    elif state == "degraded":
        unavailable = sum(not repo.available for repo in health)
        heading = (
            "GitHub main health: DEGRADED "
            f"(partial: {unavailable} of {len(health)} repos unavailable)"
        )
    else:
        heading = "GitHub main health: NO CURRENT-TIP RUNS (do not claim green)"

    lines = [heading, "Ground truth: gh run list --branch main --event push"]
    for repo in health:
        if not repo.available:
            lines.append(f"  {repo.repo}: UNAVAILABLE — {repo.reason}")
            continue
        lines.append(f"  {repo.repo} main={repo.main_sha[:12]} state={repo.state}")
        if not repo.runs:
            lines.append("    no push workflow runs found at the current main SHA")
            continue
        for run in repo.runs:
            outcome = classify_check(
                run.status,
                run.conclusion,
                self_timeout=run.self_timeout,
            )
            if run.self_timeout:
                # A self-inflicted timeout kill (a hang) surfaces as RED, not a
                # hole, so a human acts; the actuator still only re-dispatches it.
                marker = "SELF-TIMEOUT"
            elif outcome is CheckOutcome.FAILED:
                marker = "RED"
            elif outcome is CheckOutcome.NO_RESULT:
                # A hole in the record, not a failure: re-dispatch, do not alarm.
                marker = "NO-RESULT"
            else:
                marker = run.status.upper()
            conclusion = run.conclusion or "none"
            lines.append(
                f"    {marker:<9} {run.workflow}: {run.status}/{conclusion} {run.url}"
            )
    if any(not repo.available for repo in health):
        lines.append(
            "PARTIAL RESULT: unavailable repositories are UNKNOWN, not red; "
            "the command returned within its configured time budget."
        )
    return "\n".join(lines)


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--repo",
        dest="repos",
        action="append",
        help="GitHub OWNER/REPO to query; repeat to override the defaults",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=DEFAULT_RUN_LIMIT,
        help=f"main-branch push runs to inspect per repo (default: {DEFAULT_RUN_LIMIT})",
    )
    parser.add_argument(
        "--json", action="store_true", help="emit machine-readable JSON"
    )
    parser.add_argument(
        "--per-call-timeout",
        type=float,
        default=DEFAULT_CALL_TIMEOUT,
        help=(
            "seconds allowed for one gh call before that repo is unavailable "
            f"(default: {DEFAULT_CALL_TIMEOUT:g}; env CI_HUB_MAIN_HEALTH_TIMEOUT)"
        ),
    )
    parser.add_argument(
        "--overall-deadline",
        type=float,
        default=DEFAULT_OVERALL_DEADLINE,
        help=(
            "seconds allowed for all repositories before remaining results are unavailable "
            f"(default: {DEFAULT_OVERALL_DEADLINE:g}; env CI_HUB_MAIN_HEALTH_DEADLINE)"
        ),
    )
    args = parser.parse_args(argv)
    if args.limit <= 0:
        parser.error("--limit must be positive")
    if args.per_call_timeout <= 0:
        parser.error("--per-call-timeout must be positive")
    if args.overall_deadline <= 0:
        parser.error("--overall-deadline must be positive")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    repos = tuple(args.repos) if args.repos else DEFAULT_REPOS
    health = collect_health(
        repos,
        args.limit,
        per_call_timeout=args.per_call_timeout,
        overall_deadline=args.overall_deadline,
    )

    if args.json:
        print(
            json.dumps(
                {
                    "state": overall_state(health),
                    "repos": [asdict(repo) for repo in health],
                },
                sort_keys=True,
            )
        )
    else:
        print(render_report(health))
    state = overall_state(health)
    if state == "red":
        return 1
    if state in {"degraded", "none"}:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
