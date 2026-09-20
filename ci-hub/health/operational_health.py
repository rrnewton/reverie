#!/usr/bin/env python3
"""Emit tick-hub gate fields for dev-hermit operational health checks."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import math
import os
import re
import signal
import subprocess
import sys
import time
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from io import StringIO
from pathlib import Path
from typing import Any, NoReturn, cast

ROOT = Path(__file__).resolve().parents[2]
STATE_ROOT = Path(os.environ.get("DEV_HERMIT_PARENT", ROOT)).resolve()
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "runners"))
# The TaskGraph binding is stated, not inherited.  `tg` with no database named
# reads an empty `tasks` default and returns 0 rows, so an unbound tick would
# have reported a clean, EMPTY fleet -- the failure mode that reads as health.
sys.path.insert(0, str(ROOT / "ci-hub" / "lib"))
import taskgraph_db  # noqa: E402
import agent_snapshot as agent_snapshot_contract  # noqa: E402

import github_main_health
import pr_status
import primary_checkout
import queue_health


DEFAULT_STUCK_AFTER_SECS = 60 * 60
DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECS = 10 * 60
DEFAULT_DISK_SNAPSHOT_MAX_AGE_SECS = 45 * 60
# Per-repo wall-clock budget for the auto-invoked PR-health gate, including its
# bounded exact-job dereferences. tick-hub's SubprocessGateRunner hard-kills any
# gate at 30s, so the gate MUST resolve well under that: two repos queried
# serially at 12s each is ~24s worst case. Staying under the guillotine lets
# pr_status emit its own structured
# degraded/unavailable result (distinguishing "GitHub was slow" from "ci-hub is
# broken") instead of the gate timing out into a bare, undifferentiated failure.
# WHY THIS IS A SHARED DEADLINE AND NOT A FLAT PER-REPO NUMBER.
#
# A flat budget is calibrated for the SMALLEST repo and starves the largest, and
# the starvation lands exactly where the answer matters most. Measured
# 2026-08-08: the `gh pr list` GH_FIELDS query costs ~80-90ms per open PR, so
# rrnewton/reverie (19 PRs) returns in 1.6-1.8s while rrnewton/hermit (143 PRs)
# takes 11.1-13.1s. Against the old flat 12s the large repo STRADDLED its budget
# -- sometimes under, sometimes over -- so the tick intermittently reported
# `open=3` (reverie alone, all green) and read as a nearly-drained queue.
#
# The failure is self-worsening at the wrong end: cost grows with the queue, so
# the gate loses the large repo precisely when there is most to see, and regains
# it only as the queue drains -- which is the thing the gate exists to measure.
#
# So the repos share ONE envelope and each takes what it needs, with a floor
# reserved for every repo not yet queried so a slow first repo cannot starve a
# later one. Serial querying (verified 2026-08-08: `pull_request_gate` is a plain
# `for repo in DEFAULT_REPOS` loop) is what makes a running deadline correct --
# a concurrent version would need a different allocation.
#
# The envelope stays under tick-hub's hard kill, which is real and is 30s
# (agent-utils `tick_hub/probes.py` DEFAULT_GATE_TIMEOUT_SECS). Worst case here
# is TOTAL, not TOTAL*repos: the last repo can only ever receive what is left.
PR_GATE_TOTAL_BUDGET_SECS = float(
    os.environ.get("CI_HUB_PR_GATE_TOTAL_BUDGET", "24")
)
# Floor held back for each repo still unqueried. Reverie needs ~2s, so 4s leaves
# margin for a small repo while still handing the large one ~20s of a 24s pool.
PR_GATE_MIN_REPO_BUDGET_SECS = float(
    os.environ.get("CI_HUB_PR_GATE_MIN_REPO_BUDGET", "4")
)
# Population discovery and its verdict use one evidence bound.  The cache may
# be configured shorter, but pr_status clamps it so it can never outlive this
# 15-minute authority window and silently omit newer PRs.
PR_CENSUS_VERDICT_FRESH_SECS = pr_status.CENSUS_FRESHNESS_BOUND_SECS


def pr_gate_repo_budget(
    elapsed: float, repos_remaining_after_this: int,
    total: float = PR_GATE_TOTAL_BUDGET_SECS,
    floor: float = PR_GATE_MIN_REPO_BUDGET_SECS,
) -> float:
    """Budget for the repo about to be queried, from the shared envelope.

    Take everything still unspent except a floor reserved for each repo that has
    not been queried yet. The sum over a serial pass can never exceed ``total``,
    because each call only ever offers what the clock has left.
    """

    left = total - elapsed
    # The reservation is meaningful only while budget remains.  Re-granting
    # ``floor`` after the shared envelope is exhausted lets a later repo escape
    # the inner deadline and reach tick-hub's otherwise-unlabelled 30s kill.
    return max(0.0, left - floor * max(0, repos_remaining_after_this))
# Per-gh-call bound for the auto-invoked CI queue-health gate, same rationale as
# the PR gate above. The queue gate now has three serial metadata phases
# (recent-run list, active-run list, runners API) plus one bounded concurrent
# active-jobs phase. At 6s each its worst-case wall bound is ~24s, under
# tick-hub's 30s guillotine. On expiry queue_health classifies an UPSTREAM
# timeout ("GitHub slow") rather than being hard-killed into a bare failure that
# reads as "ci-hub broken". Job calls are concurrent, so the bound does not grow
# with the number of active rollups.
DEFAULT_QUEUE_GATE_TIMEOUT_SECS = float(
    os.environ.get("CI_HUB_QUEUE_GATE_TIMEOUT", "6")
)
# The semantic check makes several GitHub calls. Keep the whole verdict inside
# tick-hub's 30s subprocess guillotine so a slow upstream is reported as
# unavailable instead of being killed into an unlabelled failure.
MERGE_GATE_AUTHORITY_TIMEOUT_SECS = float(
    os.environ.get("CI_HUB_MERGE_GATE_AUTHORITY_TIMEOUT", "20")
)
MERGE_GATE_AUTHORITY_CHECKER = (
    ROOT / "hermit" / "scripts" / "configure-merge-gate-ruleset.sh"
)
DEFAULT_AGENT_SNAPSHOT = STATE_ROOT / "ignored" / "ci-hub" / "agent-snapshot.json"
HERDR_COMPOSITE_SOURCE = "herdr-agentctl-snapshot-v2"
DEFAULT_DISK_STATE = STATE_ROOT / "ignored" / "ops-state" / "disk.json"
DEFAULT_DISK_FILESYSTEM_PATH = STATE_ROOT
DISK_FREE_INSTRUMENT = "btrfs filesystem usage -b"
DEFAULT_DISK_PATH_STATE = STATE_ROOT / "ignored" / "ops-state" / "disk-paths.json"
DEFAULT_DISK_PATH_PREVIOUS_STATE = (
    STATE_ROOT / "ignored" / "ops-state" / "disk-paths.previous.json"
)
DISK_PATH_INSTRUMENT = "compsize -b -x"
DEFAULT_DISK_PATH_TIMEOUT_SECS = 180.0
_BTRFS_FREE_ESTIMATED = re.compile(
    r"^\s*Free \(estimated\):\s*([0-9]+)(?:\s|$)", re.MULTILINE
)
_BTRFS_DEVICE_SIZE = re.compile(
    r"^\s*Device size:\s*([0-9]+)(?:\s|$)", re.MULTILINE
)
_COMPSIZE_TOTAL = re.compile(
    r"^TOTAL\s+\S+\s+([0-9]+)(?:\s|$)", re.MULTILINE
)


@dataclass(frozen=True)
class TaskRecord:
    id: str
    title: str
    owner: str
    tags: tuple[str, ...]
    # Defaulted so existing fixtures that only describe live work stay valid.
    status: str = "IN_PROGRESS"
    # True when the verified closure gateway recorded CLOSURE-VERIFIED for this
    # task, i.e. `ci-hub/bin/close-task` freshly proved the change is reachable
    # from the target `main`. Defaulted False so fixtures stay valid; an absent
    # proof means NOT-PROVEN-LANDED, never assumed-landed.
    landed: bool = False

    @property
    def implemented(self) -> bool:
        return "implemented" in self.tags

    @property
    def closed(self) -> bool:
        return self.status.strip().upper() == "CLOSED"

    @property
    def awaiting_landing(self) -> bool:
        """Finished work whose landing is still owed: implemented AND NOT landed.

        TAG FIRST, STATUS SECOND still holds — the same rule
        `scripts/status-log.rs` `classify_task` applies. Under the
        close-on-implemented lifecycle an implemented task is CLOSED
        immediately, so keying awaiting-landing off `IN_PROGRESS` reports ~zero
        while the real population sits in CLOSED. That reasoning is why the
        entry condition is the TAG and not the status, and it is unchanged.

        WHAT CHANGED, and why the previous version was wrong anyway. This used
        to be a bare `return self.implemented`, with no exit condition at all.
        Nothing ever removes the `implemented` tag and there is no `landed` tag
        anywhere in the corpus, so a task ENTERED this set on completion and
        could never leave it. The count therefore GREW MONOTONICALLY WITH
        SUCCESS: every landed task incremented it forever. Measured 2026-08-07
        it read 1718 (1665 CLOSED, 47 IN_PROGRESS, 6 BACKLOG) and was presented
        as a queue depth, when the live backlog was ~47. A number that rises
        when things go RIGHT and can never fall is not a health signal — it
        cries wolf until someone stops reading it, which is strictly worse than
        reporting nothing.

        The exit condition is the verified closure gateway's CLOSURE-VERIFIED
        record, because that is the only place landing is actually PROVEN:
        `close-task` re-derives ancestry against freshly fetched target `main`.
        A CLOSED status is NOT the exit — measured, only 291 of those 1718 carry
        a closure proof, so 1427 were closed with no landing evidence at all.
        Treating CLOSED as landed would discharge the debt by assertion, which
        is the phantom-closure failure this whole lifecycle exists to prevent
        (see `ci-hub/bin/close-task`, and `drain-implemented-to-landed`).

        Consequences worth stating so nobody "fixes" this back:
          * it can DECREASE — closing a task through the gateway removes it;
          * `lifecycle_violation` stays a DISTINCT signal. Scoping this to
            IN_PROGRESS instead would have made the two metrics near-duplicates
            (47 vs 53, differing only by the BACKLOG rows).
        """
        return self.implemented and not self.landed

    @property
    def lifecycle_violation(self) -> bool:
        """An implemented task that was NOT closed.

        The current lifecycle closes implemented work immediately and lets
        `drain-implemented-to-landed` enumerate the landing debt from
        CLOSED+implemented records. A nonterminal implemented row is therefore
        a deviation to fix, not a state to honour — it is invisible to the
        drain tracker while still occupying the live queue.
        """
        return self.implemented and not self.closed


@dataclass(frozen=True)
class AgentRecord:
    name: str
    status: str
    current_task: str | None
    tmux_pane_id: str | None = None
    herdr_pane_id: str | None = None
    last_activity: float | None = None
    role: str = "worker"

    @property
    def live(self) -> bool:
        return agent_snapshot_contract.status_semantics(self.status).live

    @property
    def busy(self) -> bool:
        return agent_snapshot_contract.status_semantics(self.status).busy

    @property
    def broken(self) -> bool:
        return agent_snapshot_contract.status_semantics(self.status).broken


@dataclass(frozen=True)
class Misroute:
    agent: str
    task: str
    reason: str


@dataclass(frozen=True)
class ActiveWorkReport:
    in_progress: tuple[TaskRecord, ...]
    awaiting_land: tuple[TaskRecord, ...]
    lifecycle_violations: tuple[TaskRecord, ...]
    stale: tuple[TaskRecord, ...]
    owned_active: tuple[TaskRecord, ...]
    # The canonical ownership view. Unlike `actually_active`, this includes a
    # live owner that is waiting/idle while still assigned to the exact task.
    # It excludes stale TaskGraph owners and therefore gives queue censuses a
    # bounded claim rather than a prose comment or an immortal owner string.
    claimed: tuple[TaskRecord, ...]
    actually_active: tuple[TaskRecord, ...]
    orphaned: tuple[TaskRecord, ...]
    off_book: tuple[AgentRecord, ...]
    misrouted: tuple[Misroute, ...]
    live_agents: tuple[AgentRecord, ...]
    busy_agents: tuple[AgentRecord, ...]

    @property
    def actionable_count(self) -> int:
        return (
            len(self.orphaned)
            + len(self.stale)
            + len(self.off_book)
            + len(self.misrouted)
        )

    def counts(self) -> dict[str, int]:
        return {
            "in_progress": len(self.in_progress),
            "awaiting_land": len(self.awaiting_land),
            "lifecycle_violations": len(self.lifecycle_violations),
            "stale": len(self.stale),
            "owned_active": len(self.owned_active),
            "claimed": len(self.claimed),
            "actually_active": len(self.actually_active),
            "orphaned": len(self.orphaned),
            "off_book": len(self.off_book),
            "misrouted": len(self.misrouted),
            "live_agents": len(self.live_agents),
            "busy_agents": len(self.busy_agents),
        }


def _field(value: object) -> str:
    return " ".join(str(value).split()) or "none"


def _emit(fields: Mapping[str, object]) -> None:
    for key, value in fields.items():
        print(f"{key}={_field(value)}")


def github_main_gate() -> int:
    try:
        health = github_main_health.collect_health(
            github_main_health.DEFAULT_REPOS,
            github_main_health.DEFAULT_RUN_LIMIT,
            per_call_timeout=github_main_health.DEFAULT_CALL_TIMEOUT,
            overall_deadline=github_main_health.DEFAULT_OVERALL_DEADLINE,
        )
        state = github_main_health.overall_state(health)
        per_repo = ",".join(
            f"{repo.repo}:{repo.state if repo.available else 'unavailable'}"
            for repo in health
        )
    except (RuntimeError, agent_snapshot_contract.AgentSnapshotSchemaError) as error:
        _emit({"state": "unknown", "summary": _field(error)})
        return 1

    # SILENCE IS NOT EVIDENCE OF GREEN. This gate alarmed on explicit red and
    # said nothing otherwise, so an authority that never resolved rendered
    # exactly like a passing one -- and the ci-hub shard authority is absent or
    # unreported on 9 of the last 25 main commits (36%), because queue-depth-1
    # concurrency lets a newer push supersede a pending run. Measured 2026-08-08
    # while this was being written: three main runs sat unresolved at 372s,
    # 9198s and 15424s, and the gate was silent about all three.
    #
    # `stale-pending` now alarms; a FRESH `pending` deliberately does not, because
    # alarming on pending fires after every push and is an alarm no coordinator
    # can satisfy -- the anti-pattern removed from three other gates today.
    stale = github_main_health.DEFAULT_PENDING_STALE_SECS
    ages = {
        repo.repo: github_main_health.pending_age_secs(getattr(repo, "runs", ()))
        for repo in health
        if repo.available
    }
    oldest = max((a for a in ages.values() if a is not None), default=None)
    fields: dict[str, object] = {"state": state, "per_repo": per_repo}
    if oldest is not None:
        fields["oldest_pending_secs"] = int(oldest)
    fields["pending_stale_after_secs"] = int(stale)

    if state == "stale-pending":
        named = ", ".join(
            f"{r} pending {int(a)}s" for r, a in sorted(ages.items())
            if a is not None and a > stale
        ) or "unresolved authority with an unreadable creation time"
        fields["summary"] = (
            f"main authority has NOT RESOLVED past the {int(stale)}s bound: {named}. "
            "This is a HOLE, not a green: the run was most likely superseded by a "
            "newer push under queue-depth-1 concurrency. Re-dispatch the authority "
            "for the current main head, or confirm a newer run has since reported. "
            f"Per repo: {per_repo}"
        )
    elif state in {"none", "degraded", "unknown"}:
        fields["summary"] = (
            f"main authority COULD NOT BE MEASURED ({state}) -- this is not a green "
            f"and not a red. Per repo: {per_repo}"
        )
    else:
        # red / green / fresh-pending keep the EXACT prior summary contract -- the
        # bare per-repo list. Prose is added only for the states that did not
        # previously exist, so no existing consumer of `{summary}` changes shape.
        fields["summary"] = per_repo
    _emit(fields)
    # `pending` (fresh) is the only non-green state that stays quiet.
    return 1 if state in {"red", "none", "degraded", "stale-pending"} else 0


def pull_request_gate(
    *, census_cache_dir: Path = pr_status.DEFAULT_CENSUS_CACHE_DIR
) -> int:
    # Query each repo under a bounded budget and keep whatever succeeds: one slow
    # or blocked repo must not discard the other's real reds, and the gate must
    # resolve before tick-hub's 30s guillotine so the failure is reported (with a
    # reason) rather than silently timed out. The budget is drawn from a shared
    # running deadline rather than being flat per repo -- see
    # PR_GATE_TOTAL_BUDGET_SECS for why a flat number starves the large repo.
    statuses: list[pr_status.RepoStatus] = []
    measured_repos: list[str] = []
    unavailable: list[tuple[str, str]] = []
    repos = list(pr_status.DEFAULT_REPOS)
    started = time.monotonic()
    for index, repo in enumerate(repos):
        budget = pr_gate_repo_budget(
            time.monotonic() - started, len(repos) - index - 1
        )
        try:
            status = pr_status.fetch_repo_status(
                repo,
                timeout=budget,
                census_cache_dir=census_cache_dir,
            )
            statuses.append(status)
            measured_repos.append(repo)
            if not getattr(status, "available", True):
                unavailable.append((repo, _field(status.reason)))
        except RuntimeError as error:
            unavailable.append((repo, _field(error)))

    if not statuses:
        # Every repo failed: ci-hub/GitHub is unavailable. This is NOT "no open
        # PRs" and NOT "all green" — report it loudly and fire the reminder.
        reasons = "; ".join(f"{repo}:{why}" for repo, why in unavailable)
        _emit(
            {
                "state": "unavailable",
                # UNKNOWN, not zero: no repo returned a population to count.
                "total": "unknown",
                "total_scope": f"all-open-prs-across-0/{len(repos)}-configured-repos",
                "ready_non_draft": "unknown",
                "ci_count_scope": "ready-non-draft-heads",
                "repo_coverage": f"0/{len(repos)}",
                "configured_repos": "|".join(repos),
                "red": "unknown",
                "pending": "unknown",
                "green": "unknown",
                "real_reds": "unknown",
                "setup_only_no_result_checks": 0,
                "prerequisite_no_result_checks": 0,
                "outage": "no",
                "degraded": "yes",
                "population_repo_coverage": f"0/{len(repos)}",
                "summary": (
                    f"PR population and CI health UNAVAILABLE across 0/{len(repos)} "
                    f"configured repos ({'|'.join(repos)}); this is not zero open PRs. "
                    f"Reasons: {reasons}"
                ),
            }
        )
        return 1

    available_statuses = [
        status for status in statuses if getattr(status, "available", True)
    ]
    health_fields = (
        "green",
        "red",
        "pending",
        "real_reds",
        "setup_only_no_result_checks",
        "prerequisite_no_result_checks",
    )
    # With zero health coverage, zero would assert a verdict that no authority
    # produced. Partial coverage retains the measured subtotal and is labelled
    # as partial below.
    counts: dict[str, int | str] = {
        state: (
            sum(getattr(status, state, 0) for status in available_statuses)
            if available_statuses
            else "unknown"
        )
        for state in health_fields
    }
    # The gh engine polls ready heads at normal cadence and carries the separate
    # cached all-open census when available. Never promote the ready count into
    # an all-open count when that rare census is unavailable.
    all_open_by_repo = {
        # Legacy/test adapters without the field still carry a complete raw
        # population. An explicit None from the new gh engine means the rare
        # census failed and must remain UNKNOWN.
        repo: getattr(status, "all_open", status.open)
        for repo, status in zip(measured_repos, statuses)
    }
    ready_by_repo = {
        repo: status.open for repo, status in zip(measured_repos, statuses)
    }
    census_age_by_repo = {
        repo: getattr(status, "population_age_secs", 0.0)
        for repo, status in zip(measured_repos, statuses)
    }
    population_unknown = [
        repo for repo, count in all_open_by_repo.items() if count is None
    ]
    population_coverage = (
        f"{len(statuses) - len(population_unknown)}/{len(repos)}"
    )
    all_open: int | str = (
        sum(count for count in all_open_by_repo.values() if count is not None)
        if not population_unknown
        else "unknown"
    )
    ready_non_draft = sum(status.open for status in statuses)
    drafts: int | str = (
        all_open - ready_non_draft if isinstance(all_open, int) else "unknown"
    )
    repo_scope = "|".join(
        f"{repo}:all_open={all_open_by_repo[repo] if all_open_by_repo[repo] is not None else 'unknown'},"
        f"ready_non_draft={ready_by_repo[repo]},"
        f"census_age_s={round(census_age_by_repo[repo], 1) if isinstance(census_age_by_repo[repo], (int, float)) else 'unknown'}"
        for repo in measured_repos
    )
    coverage = f"{len(available_statuses)}/{len(repos)}"
    known_census_ages = [
        age for age in census_age_by_repo.values() if isinstance(age, (int, float))
    ]
    census_max_age = round(max(known_census_ages), 1) if known_census_ages else "unknown"
    census_age_unknown = [
        repo
        for repo in measured_repos
        if all_open_by_repo[repo] is not None
        and not isinstance(census_age_by_repo[repo], (int, float))
    ]
    stale_census_repos = [
        repo
        for repo in measured_repos
        if isinstance(census_age_by_repo[repo], (int, float))
        and census_age_by_repo[repo] > PR_CENSUS_VERDICT_FRESH_SECS
    ]
    census_stale = bool(stale_census_repos)
    census_freshness_unknown = bool(census_age_unknown)
    if census_stale:
        all_open_clause = (
            f"all_open={all_open} AS-OF UP TO {census_max_age}s AGO "
            f"(STALE; freshness_bound_s={PR_CENSUS_VERDICT_FRESH_SECS})"
        )
    elif census_freshness_unknown:
        all_open_clause = (
            f"all_open={all_open} AS-OF UNKNOWN "
            f"(FRESHNESS UNKNOWN; freshness_bound_s={PR_CENSUS_VERDICT_FRESH_SECS})"
        )
    else:
        # A fresh count retains the compact present-tense form.  The bracket
        # proves this branch and the stale branch use the same consumer.
        all_open_clause = f"all_open={all_open}"
    outage = any(status.outage_suspected for status in available_statuses)
    unhealthy = any(status.real_reds > 0 for status in available_statuses) or outage
    population_complete = len(statuses) == len(repos) and not population_unknown
    health_coverage_degraded = len(available_statuses) != len(repos)
    coverage_degraded = bool(health_coverage_degraded or not population_complete)
    degraded = bool(
        coverage_degraded or census_stale or census_freshness_unknown
    )
    if unhealthy:
        state = "red"
    elif degraded:
        # Real reds and an untrustworthy census are different alarms:
        # "degraded" says the population answer is partial, stale, or freshness-
        # unknown, not that a PR's product checks failed.
        state = "degraded"
    else:
        state = "ok"
    summary = (
        f"{all_open_clause} across {coverage} configured repos ({repo_scope}); "
        f"all-open census max_age_s={census_max_age},"
        f"freshness_bound_s={PR_CENSUS_VERDICT_FRESH_SECS},"
        f"cache_ttl_s={pr_status.DEFAULT_CENSUS_TTL_SECS:.0f},"
        f"population_coverage={population_coverage}; "
        f"ready_non_draft={ready_non_draft}, verdict_coverage={coverage}, "
        "with ready-head CI "
        f"red={counts['red']},pending={counts['pending']},green={counts['green']},"
        f"real_reds={counts['real_reds']},"
        f"setup_only_no_result={counts['setup_only_no_result_checks']},"
        f"prerequisite_no_result={counts['prerequisite_no_result_checks']},"
        f"outage={'yes' if outage else 'no'}"
    )
    if coverage_degraded:
        if population_complete:
            summary += "; POPULATION COMPLETE; VERDICTS UNAVAILABLE OR PARTIAL"
        else:
            summary += "; PARTIAL SAMPLE, NOT A FLEET CENSUS"
        summary += (
            "; unavailable=" + ("|".join(repo for repo, _ in unavailable) or "none")
            + "; population_unknown=" + ("|".join(population_unknown) or "none")
        )
    if census_stale:
        summary += "; STALE CENSUS REPOS=" + "|".join(stale_census_repos)
    if census_freshness_unknown:
        summary += "; CENSUS FRESHNESS UNKNOWN REPOS=" + "|".join(census_age_unknown)
    _emit(
        {
            "state": state,
            "total": all_open,
            "total_scope": (
                f"cached-all-open-prs-across-{population_coverage}-configured-repos"
            ),
            "census_max_age_secs": census_max_age,
            "census_freshness_bound_secs": PR_CENSUS_VERDICT_FRESH_SECS,
            "census_stale": "yes" if census_stale else "no",
            "census_freshness_unknown": (
                "yes" if census_freshness_unknown else "no"
            ),
            "stale_census_repos": "|".join(stale_census_repos) or "none",
            "census_ttl_secs": round(pr_status.DEFAULT_CENSUS_TTL_SECS),
            "population_repo_coverage": population_coverage,
            "repo_coverage": coverage,
            "configured_repos": "|".join(repos),
            "measured_repos": "|".join(measured_repos),
            "repo_scope": repo_scope,
            "ready_non_draft": ready_non_draft,
            "draft": drafts,
            "ci_count_scope": "ready-non-draft-heads",
            "red": counts["red"],
            "pending": counts["pending"],
            "green": counts["green"],
            "real_reds": counts["real_reds"],
            "setup_only_no_result_checks": counts["setup_only_no_result_checks"],
            "prerequisite_no_result_checks": counts["prerequisite_no_result_checks"],
            "outage": "yes" if outage else "no",
            "degraded": "yes" if degraded else "no",
            "summary": summary,
        }
    )
    return 1 if (unhealthy or degraded) else 0


# How long the gitlink snapshot may keep losing races before it stops being a race
# and starts being a stuck snapshot.
#
# WHY 60 MINUTES. The gate runs every 300s, so this is twelve consecutive failed
# attempts. Parent main advanced five times in about twenty minutes on 2026-08-08,
# so any single attempt frequently loses -- but losing twelve in a row is not luck,
# it means something is holding the snapshot down. The other side of the choice is
# the cost of waiting: the gitlink snapshot is a reproducibility record, not a
# correctness gate, and nothing downstream reads it within the hour, so an hour of
# staleness costs nothing while an hour of quiet buys the fleet freedom from a
# permanently-firing alarm that would be muted within a day.
#
# The threshold is on TIME, not on a commit distance, because commit distance is
# exactly the moving quantity that made this gate unsatisfiable. Being 30 commits
# behind a main that thirteen agents are advancing is normal; being unable to
# publish for an hour is not.
SNAPSHOT_DEFERRAL_BUDGET_SECS = 3600
SNAPSHOT_DEFERRAL_STATE = STATE_ROOT / ".tick-hub" / "primary-snapshot-deferral.json"
PRIMARY_SNAPSHOT_INNER_BUDGET_SECS = float(
    os.environ.get("CI_HUB_PRIMARY_SNAPSHOT_BUDGET", "24")
)


def _read_deferral(path: Path) -> float | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    since = value.get("since") if isinstance(value, dict) else None
    return float(since) if isinstance(since, (int, float)) else None


def _write_deferral(path: Path, since: float | None) -> None:
    try:
        if since is None:
            path.unlink(missing_ok=True)
            return
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps({"since": since}) + "\n", encoding="utf-8")
        tmp.replace(path)
    except OSError:
        # Losing the deferral clock degrades this to the old "page on the first
        # lost race" behaviour at worst; it must never take the tick down.
        pass


def primary_snapshot_gate(
    *,
    root: Path | None = None,
    state_path: Path | None = None,
    now: float | None = None,
    budget_secs: int = SNAPSHOT_DEFERRAL_BUDGET_SECS,
) -> int:
    output, errors = StringIO(), StringIO()
    target_root = root or STATE_ROOT
    result = primary_checkout.checkout_fresh(
        target_root,
        publish_parent=True,
        strict=True,
        deadline_secs=PRIMARY_SNAPSHOT_INNER_BUDGET_SECS,
        out=output,
        err=errors,
    )
    path = state_path or target_root / ".tick-hub" / "primary-snapshot-deferral.json"
    moment = time.time() if now is None else now
    # A known-dirty exception is part of the snapshot fact, not incidental log
    # noise.  Preserve both streams so a later refusal cannot hide the earlier
    # disclosure (the old ``errors or output`` choice silently discarded it).
    report = "\n".join(
        part
        for part in (output.getvalue().strip(), errors.getvalue().strip())
        if part
    )

    if result == primary_checkout.SNAPSHOT_DEFERRED:
        since = _read_deferral(path)
        if since is None:
            since = moment
            _write_deferral(path, since)
        waited = max(0.0, moment - since)
        if waited >= budget_secs:
            _emit({
                "state": "blocked",
                "deferred_mins": round(waited / 60.0, 1),
                "summary": (
                    f"gitlink snapshot has been unpublishable for "
                    f"{waited / 60.0:.0f} min (budget {budget_secs // 60} min); "
                    "this is no longer a lost race. " + report
                ),
            })
            return 1
        _emit({
            "state": "deferred",
            "deferred_mins": round(waited / 60.0, 1),
            "summary": (
                f"snapshot due but not publishable this tick; deferred "
                f"{waited / 60.0:.0f} of {budget_secs // 60} min budget. " + report
            ),
        })
        return 0

    # Any non-deferred outcome ends the deferral, including a hard block: a block
    # is reported on its own terms and must not also inherit an aged clock.
    _write_deferral(path, None)
    _emit({
        "state": "ok" if result == 0 else "blocked",
        "deferred_mins": 0,
        "summary": report or "primary-snapshot-produced-no-output",
    })
    return 0 if result == 0 else 1


def queue_health_gate() -> int:
    """Fail-loud tick gate for CI queue depth, wait times, and staleness of the
    last green run. Delegates to the shared queue_health analysis so the tick and
    the human `runner-health` report never diverge."""
    import os

    gh_cmd = os.environ.get("GH", "with-proxy gh")
    limit = int(os.environ.get("CI_QUEUE_HEALTH_LIMIT", "100"))
    repos = [os.environ.get("CI_QUEUE_HEALTH_REPO", "rrnewton/hermit")]
    try:
        # Bound each gh call so the whole gate resolves under tick-hub's 30s
        # guillotine and reports a CLASSIFIED result (upstream-slow vs
        # ci-hub-broken) instead of being hard-killed into a bare timeout.
        return queue_health.gate(repos, gh_cmd, limit,
                                 per_call_timeout=DEFAULT_QUEUE_GATE_TIMEOUT_SECS)
    except Exception as error:  # never let a probe crash the tick silently
        _emit({"state": "unknown", "summary": _field(f"queue-health-error:{error}")})
        return 1


def _last_activity_seconds(raw: object) -> float | None:
    if isinstance(raw, bool) or not isinstance(raw, (int, float)):
        return None
    value = float(raw)
    if value > 10_000_000_000:
        value /= 1000.0
    return value


def classify_stuck_agents(
    agents: Sequence[object],
    *,
    now: float,
    stuck_after_secs: int = DEFAULT_STUCK_AFTER_SECS,
) -> list[tuple[str, str]]:
    stuck: list[tuple[str, str]] = []
    for agent in _parse_agents(agents):
        if agent.broken:
            stuck.append((agent.name, agent.status))
            continue
        if (
            agent.busy
            and agent.last_activity is not None
            and now - agent.last_activity >= stuck_after_secs
        ):
            age_minutes = int((now - agent.last_activity) // 60)
            stuck.append((agent.name, f"{agent.status}-silent-{age_minutes}m"))
    return sorted(stuck)


def agent_gate(
    snapshot: str | None = None,
    *,
    now: float | None = None,
    stuck_after_secs: int = DEFAULT_STUCK_AFTER_SECS,
) -> int:
    text = (
        snapshot
        if snapshot is not None
        else os.environ.get("HERMIT_AGENT_SNAPSHOT_JSON")
    )
    if text is None:
        _emit(
            {
                "state": "unknown",
                "count": 0,
                "names": "none",
                "summary": "ORC-agent-snapshot-missing",
            }
        )
        return 1
    try:
        payload: Any = json.loads(text)
    except json.JSONDecodeError as error:
        _emit(
            {
                "state": "unknown",
                "count": 0,
                "names": "none",
                "summary": f"invalid-agent-snapshot:{error.msg}",
            }
        )
        return 1
    try:
        stuck = classify_stuck_agents(
            payload,
            now=time.time() if now is None else now,
            stuck_after_secs=stuck_after_secs,
        )
    except (RuntimeError, agent_snapshot_contract.AgentSnapshotSchemaError) as error:
        _emit(
            {
                "state": "unknown",
                "count": 0,
                "names": "none",
                "summary": str(error),
            }
        )
        return 1
    _emit(
        {
            "state": "stuck" if stuck else "ok",
            "count": len(stuck),
            "names": ",".join(name for name, _reason in stuck) or "none",
            "summary": ",".join(f"{name}:{reason}" for name, reason in stuck)
            or "no-stuck-agents",
        }
    )
    return 1 if stuck else 0


def _parse_agents(payload: object) -> tuple[AgentRecord, ...]:
    try:
        validated = agent_snapshot_contract.parse_snapshot_payload(payload)
    except agent_snapshot_contract.AgentSnapshotSchemaError as error:
        raise RuntimeError(str(error)) from error
    agents: list[AgentRecord] = []
    for raw in validated:
        name = raw["name"]
        status = raw["status"]
        current_task_raw = raw.get("current_task")
        current_task = (
            current_task_raw.strip() if current_task_raw is not None else ""
        )
        agents.append(
            AgentRecord(
                name=name,
                status=status,
                current_task=current_task or None,
                tmux_pane_id=raw.get("tmux_pane_id"),
                herdr_pane_id=raw.get("herdr_pane_id"),
                last_activity=_last_activity_seconds(raw.get("last_activity")),
                role=str(raw.get("role", "worker")),
            )
        )
    return tuple(sorted(agents, key=lambda agent: agent.name))


def _active_herdr_authority(
    snapshot_file: Path,
) -> tuple[str, Path, str, dict[str, dict[str, object]]] | None:
    """Return schema-v2 binding expectations for the canonical state path."""
    path = snapshot_file.absolute()
    if tuple(path.parts[-3:]) != ("ignored", "ci-hub", "agent-snapshot.json"):
        return None
    binding_path = path.parents[2] / ".tick-hub" / "herdr-agent-bindings.json"
    try:
        raw = binding_path.read_bytes()
    except FileNotFoundError:
        return None
    except OSError as error:
        raise RuntimeError(f"cannot inspect Herdr authority binding: {error}") from error
    try:
        document = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RuntimeError(f"invalid Herdr authority binding: {error}") from error
    if not isinstance(document, dict):
        raise RuntimeError("invalid Herdr authority binding: root is not an object")
    if document.get("schema_version") != 2:
        raise RuntimeError(
            "unsupported Herdr authority binding schema; legacy writers remain disabled"
        )
    workspace = document.get("workspace")
    taskgraph_db_path = document.get("taskgraph_db")
    rows = document.get("agents")
    if (
        not isinstance(workspace, str)
        or not workspace
        or not isinstance(taskgraph_db_path, str)
        or not Path(taskgraph_db_path).is_absolute()
        or not isinstance(rows, list)
    ):
        raise RuntimeError("invalid schema-v2 Herdr authority binding")
    expected: dict[str, dict[str, object]] = {}
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            raise RuntimeError(
                f"invalid schema-v2 Herdr authority binding agent {index}"
            )
        name = row.get("snapshot_name")
        role = row.get("role")
        fields = {
            "name": name,
            "herdr_pane_id": row.get("pane_id"),
            "herdr_name": row.get("herdr_name"),
            "herdr_workspace": workspace,
            "harness": row.get("harness"),
            "role": role,
            "source": HERDR_COMPOSITE_SOURCE,
            "current_task": row.get("current_task"),
        }
        if (
            not isinstance(name, str)
            or not name
            or role not in {"coordinator", "worker"}
            or any(
                not isinstance(fields[key], str) or not fields[key]
                for key in ("herdr_pane_id", "herdr_name", "harness")
            )
        ):
            raise RuntimeError(
                f"invalid schema-v2 Herdr authority binding agent {index}"
            )
        if name in expected:
            raise RuntimeError(
                f"invalid schema-v2 Herdr authority binding duplicate {name!r}"
            )
        expected[name] = fields
    if not expected:
        raise RuntimeError("invalid schema-v2 Herdr authority binding: no agents")
    return (
        workspace,
        Path(taskgraph_db_path).resolve(),
        hashlib.sha256(raw).hexdigest(),
        expected,
    )


def _assert_herdr_composite_provenance(
    payload: object, authority_payload: object, snapshot_file: Path
) -> None:
    authority = _active_herdr_authority(snapshot_file)
    if authority is None:
        return
    _workspace, taskgraph_path, binding_sha256, expected = authority
    if not isinstance(authority_payload, dict):
        raise RuntimeError(
            "canonical Herdr snapshot lacks composite provenance authority"
        )
    if authority_payload.get("source") != HERDR_COMPOSITE_SOURCE:
        raise RuntimeError("canonical Herdr snapshot lacks composite authority source")
    if authority_payload.get("binding_sha256") != binding_sha256:
        raise RuntimeError(
            "canonical Herdr snapshot binding generation fingerprint changed"
        )
    recorded_db = authority_payload.get("taskgraph_db")
    if (
        not isinstance(recorded_db, str)
        or not Path(recorded_db).is_absolute()
        or Path(recorded_db).resolve() != taskgraph_path
    ):
        raise RuntimeError(
            "canonical Herdr snapshot TaskGraph path does not match its binding"
        )
    owned = authority_payload.get("owned_in_progress")
    if not isinstance(owned, list) or any(
        not isinstance(row, dict)
        or set(row) != {"local_id", "owner"}
        or not isinstance(row.get("local_id"), str)
        or not isinstance(row.get("owner"), str)
        for row in owned
    ):
        raise RuntimeError("canonical Herdr snapshot lacks owned TaskGraph proof")
    if not isinstance(payload, list) or any(not isinstance(row, dict) for row in payload):
        raise RuntimeError("canonical Herdr snapshot payload is not an agent list")
    rows = {str(row.get("name")): row for row in payload}
    if len(rows) != len(payload) or set(rows) != set(expected):
        raise RuntimeError(
            "canonical Herdr snapshot does not match the complete schema-v2 bound fleet"
        )
    for name, fields in expected.items():
        row = rows[name]
        for key, value in fields.items():
            if row.get(key) != value:
                raise RuntimeError(
                    f"canonical Herdr snapshot agent {name!r} lacks composite "
                    f"provenance: {key}={row.get(key)!r}, expected {value!r}"
                )
        if row.get("tmux_pane_id") is not None:
            raise RuntimeError(
                f"canonical Herdr snapshot agent {name!r} carries a tmux identity"
            )


def _persist_agent_snapshot(
    path: Path,
    payload: object,
    *,
    captured_at: float,
) -> None:
    if _active_herdr_authority(path) is not None:
        raise RuntimeError(
            "legacy agent snapshot writer disabled while schema-v2 Herdr authority is active"
        )
    document = {
        "schema_version": 1,
        "captured_at": captured_at,
        "agents": payload,
    }
    encoded = (json.dumps(document, sort_keys=True) + "\n").encode("utf-8")
    try:
        agent_snapshot_contract.parse_snapshot_bytes(encoded)
    except agent_snapshot_contract.AgentSnapshotSchemaError as error:
        raise RuntimeError(str(error)) from error
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_bytes(encoded)
    os.replace(temporary, path)


def _assert_plausible_agent_payload(payload: object, *, source: str) -> None:
    """Refuse a cached snapshot whose CONTENT cannot describe a real fleet.

    FRESHNESS IS NOT PLAUSIBILITY. `_persist_agent_snapshot` stamps
    `captured_at` on every write regardless of payload, so a bad write is
    also a MAXIMALLY FRESH one and slips past both existing guards (missing,
    stale). A one-entry `{"name": "worker"}` stub was observed in production
    52s old while 18 real tmux panes were running; every real agent then fails
    `owner not in live_by_name` and is reported ORPHANED. That is how a live
    agent got named dead while holding the freshest activity timestamp on the
    box, and under the autonomous-replacement mandate that alarm was wired to
    a respawn.

    This promotes the environment-independent half of the predicate that
    `scripts/allocate-worktree.rs::observe_owner_lease` already relies on: a
    pane-bound ORC agent carries a `%NN` identity and a Herdr agent carries an
    exact `wX:pY` identity. The full ORC array also
    contains managed, free, and unreachable entries with no pane, so requiring
    one from EVERY entry would refuse the real producer. Require at least one
    pane-bound entry and retain the complete array. Its OTHER half -- resolving
    that pane via `tmux list-panes -a` -- is DELIBERATELY NOT promoted.
    Measured 2026-08-08: from an agent shell (TMUX_TMPDIR inherited) that
    lists the 17-pane ORC server, but from `hermit-health-tick.service`, whose
    unit sets only PATH, it reaches a DIFFERENT 4-pane server under
    /tmp/tmux-$UID/default with ZERO overlap with the fleet. Promoting it
    would make the timer-driven tick refuse every REAL snapshot -- permanently
    blind, strictly worse than the bug it fixes. Structure is checkable from
    any environment; a server lookup is not. Note this is a plausibility
    check, NOT a second fleet authority: `tick-hub.yaml` reserves fleet
    liveness to ORC, and refusing an obviously broken copy does not claim it.

    Known limits, stated rather than implied: a fabricated but well-formed
    pane id still passes, and a snapshot that is merely INCOMPLETE (an ORC
    session cannot see a sibling session's agents) still passes -- completeness
    needs an independent source such as `ci-hub/health/agent_liveness_probe.py`.
    This catches the failure modes actually observed and is safe everywhere.
    """
    agents = _parse_agents(payload)
    if not agents:
        # Zero agents is indistinguishable from no observation, so it must read
        # as could-not-measure -- never as a healthy, empty fleet.
        raise RuntimeError(f"agent-snapshot-implausible:{source}:zero-agents")
    pane_bound = sum(
        agent.tmux_pane_id is not None or agent.herdr_pane_id is not None
        for agent in agents
    )
    if pane_bound == 0:
        raise RuntimeError(
            f"agent-snapshot-implausible:{source}:no-pane-bound-agents"
        )


def load_agent_snapshot(
    snapshot: str | None,
    *,
    snapshot_file: Path = DEFAULT_AGENT_SNAPSHOT,
    max_age_secs: int = DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECS,
    now: float | None = None,
    persist: bool = True,
    include_authority: bool = False,
) -> (
    tuple[tuple[AgentRecord, ...], float]
    | tuple[tuple[AgentRecord, ...], float, dict[str, object] | None]
):
    observed_at = time.time() if now is None else now
    # Ambient ORC input belongs only to `cache-agents`. Claim authority reads a
    # named envelope; an environment variable must never outrank that file.
    text = snapshot
    if text is not None:
        try:
            payload: object = json.loads(text)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"invalid-agent-snapshot:{error.msg}") from error
        agents = _parse_agents(payload)
        _assert_plausible_agent_payload(payload, source="explicit")
        if _active_herdr_authority(snapshot_file) is not None:
            raise RuntimeError(
                "bare explicit snapshot cannot replace schema-v2 composite authority"
            )
        if persist:
            _persist_agent_snapshot(snapshot_file, payload, captured_at=observed_at)
        if include_authority:
            return agents, observed_at, None
        return agents, observed_at

    try:
        raw_snapshot = snapshot_file.read_bytes()
    except FileNotFoundError as error:
        raise RuntimeError(
            "ORC-agent-snapshot-missing; wait for the operational tick or pass --agent-snapshot"
        ) from error
    except OSError as error:
        raise RuntimeError(f"cannot-read-agent-snapshot:{error}") from error
    try:
        envelope = agent_snapshot_contract.parse_snapshot_bytes(raw_snapshot)
    except agent_snapshot_contract.AgentSnapshotSchemaError as error:
        raise RuntimeError(str(error)) from error
    captured_at = float(envelope["captured_at"])
    age = observed_at - float(captured_at)
    if age < 0 or age > max_age_secs:
        raise RuntimeError(
            f"agent-snapshot-stale:age={max(0, int(age))}s,max={max_age_secs}s"
        )
    agents_payload = envelope["agents"]
    # AFTER the staleness check, so a stale snapshot still reports STALE and
    # keeps its existing, more specific diagnostic.
    _assert_plausible_agent_payload(agents_payload, source="cache")
    authority_payload = envelope.get("authority")
    _assert_herdr_composite_provenance(
        agents_payload, authority_payload, snapshot_file
    )
    parsed = _parse_agents(agents_payload)
    if include_authority:
        return parsed, float(captured_at), (
            authority_payload if isinstance(authority_payload, dict) else None
        )
    return parsed, float(captured_at)


def cache_agent_snapshot(
    snapshot: str | None = None,
    *,
    snapshot_file: Path = DEFAULT_AGENT_SNAPSHOT,
    now: float | None = None,
) -> int:
    """Persist the full ORC snapshot supplied to the scheduled health tick."""
    text = (
        snapshot
        if snapshot is not None
        else os.environ.get("HERMIT_AGENT_SNAPSHOT_JSON")
    )
    if text is None:
        _emit({"state": "unknown", "summary": "ORC-agent-snapshot-missing"})
        return 1
    try:
        payload: object = json.loads(text)
        agents = _parse_agents(payload)
        # A bad write is also a maximally fresh write. Refuse it before replacing
        # the last usable observation; freshness cannot establish plausibility.
        _assert_plausible_agent_payload(payload, source="ORC")
        _persist_agent_snapshot(
            snapshot_file,
            payload,
            captured_at=time.time() if now is None else now,
        )
    except (json.JSONDecodeError, OSError, RuntimeError) as error:
        _emit({"state": "unknown", "summary": f"cannot-cache-agent-snapshot:{error}"})
        return 1
    _emit({"state": "ok", "count": len(agents), "summary": "agent-snapshot-cached"})
    return 0


def _parse_btrfs_usage_bytes(output: str) -> tuple[int, int]:
    free_matches = _BTRFS_FREE_ESTIMATED.findall(output)
    size_matches = _BTRFS_DEVICE_SIZE.findall(output)
    if len(free_matches) != 1:
        raise RuntimeError(
            "btrfs output must contain exactly one Free (estimated) byte value"
        )
    if len(size_matches) != 1:
        raise RuntimeError("btrfs output must contain exactly one Device size byte value")
    free_bytes = int(free_matches[0])
    device_size_bytes = int(size_matches[0])
    if device_size_bytes <= 0 or free_bytes > device_size_bytes:
        raise RuntimeError(
            "btrfs Free (estimated) byte value is outside the device size"
        )
    return free_bytes, device_size_bytes


def _persist_disk_state(
    path: Path,
    *,
    free_bytes: int,
    device_size_bytes: int,
    filesystem_path: Path,
    captured_at: float,
) -> None:
    document = {
        "schema_version": 1,
        "free_bytes": free_bytes,
        "device_size_bytes": device_size_bytes,
        "instrument": DISK_FREE_INSTRUMENT,
        "filesystem_path": str(filesystem_path),
        "captured_at": captured_at,
    }
    encoded = (json.dumps(document, sort_keys=True) + "\n").encode("utf-8")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        temporary.write_bytes(encoded)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def _disk_path_populations(
    state_root: Path, *, home: Path | None = None
) -> tuple[tuple[str, tuple[Path, ...]], ...]:
    """Return the bounded top-level populations used for disk attribution."""
    home_root = Path.home() if home is None else home
    return (
        ("registered_slots", (state_root / "worktrees" / "slots",)),
        ("validation_checkouts", (state_root / "worktrees" / "validate",)),
        (
            "shared_checkouts",
            (
                state_root / "hermit",
                state_root / "reverie",
                state_root / "liteinst2",
            ),
        ),
        ("parent_git", (state_root / ".git",)),
        (
            "home_caches",
            tuple(
                home_root / name for name in (".cache", ".cargo", ".rustup", ".codex")
            ),
        ),
    )


def _measure_disk_population(
    name: str, paths: tuple[Path, ...], *, timeout: float
) -> dict[str, object]:
    started = time.monotonic()
    existing: list[Path] = []
    diagnostics: list[str] = []
    for path in paths:
        try:
            existing.append(path.resolve(strict=True))
        except OSError as error:
            diagnostics.append(f"{path}: {error}")

    command = [
        "nice",
        "-n",
        "19",
        "ionice",
        "-c3",
        "compsize",
        "-b",
        "-x",
        *(str(path) for path in existing),
    ]
    if not existing:
        return {
            "name": name,
            "paths": [str(path) for path in paths],
            "state": "unavailable",
            "physical_bytes": None,
            "elapsed_seconds": time.monotonic() - started,
            "diagnostics": diagnostics or ["no configured path exists"],
        }

    try:
        measured = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=timeout,
            env={**os.environ, "LC_ALL": "C"},
        )
    except subprocess.TimeoutExpired:
        diagnostics.append(f"timed out after {timeout:g}s")
        return {
            "name": name,
            "paths": [str(path) for path in paths],
            "state": "unavailable",
            "physical_bytes": None,
            "elapsed_seconds": time.monotonic() - started,
            "diagnostics": diagnostics,
        }
    except OSError as error:
        diagnostics.append(str(error))
        return {
            "name": name,
            "paths": [str(path) for path in paths],
            "state": "unavailable",
            "physical_bytes": None,
            "elapsed_seconds": time.monotonic() - started,
            "diagnostics": diagnostics,
        }

    diagnostics.extend(line for line in measured.stderr.splitlines() if line.strip())
    totals = _COMPSIZE_TOTAL.findall(measured.stdout)
    physical_bytes: int | None = int(totals[0]) if len(totals) == 1 else None
    if len(totals) != 1:
        diagnostics.append(
            f"compsize output contained {len(totals)} TOTAL physical-byte values"
        )
    if measured.returncode != 0:
        diagnostics.append(f"compsize exited {measured.returncode}")

    if physical_bytes is None:
        state = "unavailable"
    elif diagnostics:
        state = "partial"
    else:
        state = "complete"
    return {
        "name": name,
        "paths": [str(path) for path in paths],
        "state": state,
        "physical_bytes": physical_bytes,
        "elapsed_seconds": time.monotonic() - started,
        "diagnostics": diagnostics,
    }


def _fresh_btrfs_reading(filesystem_path: Path) -> tuple[int, int]:
    measured = subprocess.run(
        ["btrfs", "filesystem", "usage", "-b", str(filesystem_path)],
        capture_output=True,
        text=True,
        timeout=30,
        env={**os.environ, "LC_ALL": "C"},
    )
    if measured.returncode != 0:
        detail = measured.stderr.strip() or measured.stdout.strip() or "no output"
        raise RuntimeError(
            f"{DISK_FREE_INSTRUMENT} exited {measured.returncode}: {detail}"
        )
    return _parse_btrfs_usage_bytes(measured.stdout)


def _persist_rolling_state(path: Path, previous_path: Path, document: object) -> None:
    encoded = (json.dumps(document, sort_keys=True) + "\n").encode("utf-8")
    path.parent.mkdir(parents=True, exist_ok=True)
    previous_path.parent.mkdir(parents=True, exist_ok=True)
    current_temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    previous_temporary = previous_path.with_name(
        f".{previous_path.name}.{os.getpid()}.tmp"
    )
    try:
        current_temporary.write_bytes(encoded)
        if path.exists():
            previous_temporary.write_bytes(path.read_bytes())
            os.replace(previous_temporary, previous_path)
        os.replace(current_temporary, path)
    finally:
        current_temporary.unlink(missing_ok=True)
        previous_temporary.unlink(missing_ok=True)


def cache_disk_paths(
    *,
    state_root: Path = STATE_ROOT,
    state_file: Path = DEFAULT_DISK_PATH_STATE,
    previous_state_file: Path = DEFAULT_DISK_PATH_PREVIOUS_STATE,
    population_timeout: float = DEFAULT_DISK_PATH_TIMEOUT_SECS,
    now: float | None = None,
    populations: tuple[tuple[str, tuple[Path, ...]], ...] | None = None,
) -> int:
    """Publish bounded physical path measurements and retain the prior reading."""
    started = time.monotonic()
    try:
        resolved_root = state_root.resolve(strict=True)
        free_before, device_size_before = _fresh_btrfs_reading(resolved_root)
        selected = populations or _disk_path_populations(resolved_root)
        with concurrent.futures.ThreadPoolExecutor(
            max_workers=max(1, len(selected))
        ) as executor:
            readings = list(
                executor.map(
                    lambda item: _measure_disk_population(
                        item[0], item[1], timeout=population_timeout
                    ),
                    selected,
                )
            )
        free_after, device_size_after = _fresh_btrfs_reading(resolved_root)
        if device_size_before != device_size_after:
            raise RuntimeError("btrfs device size changed during path measurement")
        counts = {
            state: sum(reading["state"] == state for reading in readings)
            for state in ("complete", "partial", "unavailable")
        }
        document = {
            "schema_version": 1,
            "captured_at": time.time() if now is None else now,
            "elapsed_seconds": time.monotonic() - started,
            "instrument": DISK_PATH_INSTRUMENT,
            "values_are_additive": False,
            "filesystem": {
                "path": str(resolved_root),
                "instrument": DISK_FREE_INSTRUMENT,
                "device_size_bytes": device_size_before,
                "free_bytes_before": free_before,
                "free_bytes_after": free_after,
            },
            "counts": counts,
            "populations": readings,
        }
        _persist_rolling_state(state_file, previous_state_file, document)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        _emit({"state": "unknown", "summary": f"cannot-cache-disk-paths:{error}"})
        return 1

    usable = counts["complete"] + counts["partial"]
    state = "ok" if counts["partial"] == counts["unavailable"] == 0 else "partial"
    _emit(
        {
            "state": state,
            "populations": len(readings),
            "complete": counts["complete"],
            "partial": counts["partial"],
            "unavailable": counts["unavailable"],
            "summary": "disk-paths-cached",
        }
    )
    return 0 if usable else 1


def cache_disk_free(
    *,
    filesystem_path: Path = DEFAULT_DISK_FILESYSTEM_PATH,
    state_file: Path = DEFAULT_DISK_STATE,
    now: float | None = None,
) -> int:
    """Publish one fresh btrfs allocator reading for the disk watcher."""
    try:
        resolved_path = filesystem_path.resolve(strict=True)
        command = ["btrfs", "filesystem", "usage", "-b", str(resolved_path)]
        environment = dict(os.environ)
        environment["LC_ALL"] = "C"
        measured = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=30,
            env=environment,
        )
        if measured.returncode != 0:
            detail = measured.stderr.strip() or measured.stdout.strip() or "no output"
            raise RuntimeError(
                f"{DISK_FREE_INSTRUMENT} exited {measured.returncode}: {detail}"
            )
        free_bytes, device_size_bytes = _parse_btrfs_usage_bytes(measured.stdout)
        _persist_disk_state(
            state_file,
            free_bytes=free_bytes,
            device_size_bytes=device_size_bytes,
            filesystem_path=resolved_path,
            captured_at=time.time() if now is None else now,
        )
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        _emit({"state": "unknown", "summary": f"cannot-cache-disk-free:{error}"})
        return 1
    _emit(
        {
            "state": "ok",
            "free_bytes": free_bytes,
            "instrument": DISK_FREE_INSTRUMENT,
            "summary": "disk-free-cached",
        }
    )
    return 0


def _nonnegative_number(value: object, name: str) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise ValueError(f"{name} is not numeric")
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise ValueError(f"{name} is not a nonnegative finite number")
    return number


def _nonnegative_integer(value: object, name: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ValueError(f"{name} is not a nonnegative integer")
    return value


def _validate_disk_free_artifact(document: dict[str, object]) -> None:
    if document.get("instrument") != DISK_FREE_INSTRUMENT:
        raise ValueError("unexpected disk-free instrument")
    filesystem = document.get("filesystem_path")
    if not isinstance(filesystem, str) or not filesystem or not Path(filesystem).is_absolute():
        raise ValueError("filesystem_path is not an absolute path")
    free = _nonnegative_integer(document.get("free_bytes"), "free_bytes")
    size = _nonnegative_integer(
        document.get("device_size_bytes"), "device_size_bytes"
    )
    if size == 0 or free > size:
        raise ValueError("disk-free byte counts are outside the device size")


def _validate_disk_paths_artifact(document: dict[str, object]) -> None:
    if document.get("instrument") != DISK_PATH_INSTRUMENT:
        raise ValueError("unexpected disk-path instrument")
    if document.get("values_are_additive") is not False:
        raise ValueError("disk-path values_are_additive must be false")
    _nonnegative_number(document.get("elapsed_seconds"), "elapsed_seconds")
    filesystem = document.get("filesystem")
    if not isinstance(filesystem, dict):
        raise ValueError("filesystem is not an object")
    if filesystem.get("instrument") != DISK_FREE_INSTRUMENT:
        raise ValueError("unexpected filesystem instrument")
    path = filesystem.get("path")
    if not isinstance(path, str) or not path or not Path(path).is_absolute():
        raise ValueError("filesystem.path is not an absolute path")
    size = _nonnegative_integer(
        filesystem.get("device_size_bytes"), "filesystem.device_size_bytes"
    )
    before = _nonnegative_integer(
        filesystem.get("free_bytes_before"), "filesystem.free_bytes_before"
    )
    after = _nonnegative_integer(
        filesystem.get("free_bytes_after"), "filesystem.free_bytes_after"
    )
    if size == 0 or before > size or after > size:
        raise ValueError("filesystem free byte counts are outside the device size")

    states = ("complete", "partial", "unavailable")
    counts = document.get("counts")
    populations = document.get("populations")
    if not isinstance(counts, dict) or set(counts) != set(states):
        raise ValueError("counts does not name the complete disk-path state set")
    if not isinstance(populations, list) or not populations:
        raise ValueError("populations is not a nonempty list")
    typed_counts = {
        state: _nonnegative_integer(counts.get(state), f"counts.{state}")
        for state in states
    }
    observed = {state: 0 for state in states}
    names: set[str] = set()
    for index, population in enumerate(populations):
        if not isinstance(population, dict):
            raise ValueError(f"population {index} is not an object")
        name = population.get("name")
        if not isinstance(name, str) or not name or name in names:
            raise ValueError(f"population {index} has an empty or duplicate name")
        names.add(name)
        state = population.get("state")
        if state not in states:
            raise ValueError(f"population {index} has an unsupported state")
        observed[str(state)] += 1
        paths = population.get("paths")
        diagnostics = population.get("diagnostics")
        if not isinstance(paths, list) or not all(
            isinstance(item, str) and item for item in paths
        ):
            raise ValueError(f"population {index} paths are invalid")
        if not isinstance(diagnostics, list) or not all(
            isinstance(item, str) for item in diagnostics
        ):
            raise ValueError(f"population {index} diagnostics are invalid")
        _nonnegative_number(
            population.get("elapsed_seconds"),
            f"population {index} elapsed_seconds",
        )
        physical = population.get("physical_bytes")
        if physical is not None:
            _nonnegative_integer(physical, f"population {index} physical_bytes")
        if state == "complete" and physical is None:
            raise ValueError(f"complete population {index} has no physical_bytes")
    if typed_counts != observed:
        raise ValueError("counts does not match the population states")


def _artifact_freshness(
    path: Path, *, kind: str, now: float, max_age_secs: int
) -> tuple[str, int | None, str]:
    """Read a producer timestamp from one typed JSON artifact.

    Content time, not mtime, is authoritative: touching or copying an old
    snapshot must not make its observation current again.
    """
    try:
        raw = path.read_bytes()
        document = json.loads(raw)
        if not isinstance(document, dict) or document.get("schema_version") != 1:
            raise ValueError("expected a schema_version=1 object")
        if kind == "agent":
            document = agent_snapshot_contract.parse_snapshot_bytes(raw)
            try:
                _assert_plausible_agent_payload(
                    document["agents"], source="freshness"
                )
                _assert_herdr_composite_provenance(
                    document["agents"], document.get("authority"), path
                )
            except RuntimeError as error:
                raise ValueError(str(error)) from error
        elif kind == "disk-free":
            _validate_disk_free_artifact(document)
        elif kind == "disk-paths":
            _validate_disk_paths_artifact(document)
        else:
            raise ValueError(f"unsupported artifact kind {kind!r}")
        captured_at = document.get("captured_at")
        captured = _nonnegative_number(captured_at, "captured_at")
        age = now - captured
        if age < 0:
            return "invalid", None, f"captured_at is {-age:.0f}s in the future"
        rounded_age = int(age)
        if age > max_age_secs:
            return "stale", rounded_age, f"age={rounded_age}s max={max_age_secs}s"
        return "fresh", rounded_age, f"age={rounded_age}s max={max_age_secs}s"
    except FileNotFoundError:
        return "missing", None, "file does not exist"
    except (
        OSError,
        json.JSONDecodeError,
        ValueError,
        agent_snapshot_contract.AgentSnapshotSchemaError,
    ) as error:
        return "invalid", None, " ".join(str(error).split())


def operational_input_freshness_gate(
    *,
    state_root: Path = STATE_ROOT,
    agent_max_age_secs: int = DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECS,
    disk_max_age_secs: int = DEFAULT_DISK_SNAPSHOT_MAX_AGE_SECS,
    now: float | None = None,
) -> int:
    """Report stale producer inputs without refreshing or deleting them."""
    observed_at = time.time() if now is None else now
    inputs = (
        (
            "agent_snapshot",
            "agent",
            state_root / "ignored" / "ci-hub" / "agent-snapshot.json",
            agent_max_age_secs,
        ),
        (
            "disk_free_snapshot",
            "disk-free",
            state_root / "ignored" / "ops-state" / "disk.json",
            disk_max_age_secs,
        ),
        (
            "disk_paths_snapshot",
            "disk-paths",
            state_root / "ignored" / "ops-state" / "disk-paths.json",
            disk_max_age_secs,
        ),
    )
    fields: dict[str, object] = {}
    findings: list[str] = []
    for name, kind, path, max_age in inputs:
        state, age, detail = _artifact_freshness(
            path, kind=kind, now=observed_at, max_age_secs=max_age
        )
        fields[f"{name}_state"] = state
        fields[f"{name}_age_secs"] = "unavailable" if age is None else age
        if state != "fresh":
            findings.append(f"{name} {state} ({detail})")

    if findings:
        state = "actionable"
        summary = (
            f"{len(findings)} operational input snapshot(s) need attention: "
            + "; ".join(findings)
        )
        code = 1
    else:
        state = "ok"
        summary = "agent and disk producer snapshots are within their freshness bounds"
        code = 0
    # Captured values are tick templates; diagnostics may contain braces from a
    # path or parser and must not create recursive placeholders.
    summary = summary.replace("{", "(").replace("}", ")")
    _emit({"state": state, "summary": summary, **fields})
    return code


def _taskgraph_json_rows(output: str) -> tuple[dict[str, Any], ...]:
    """Read every JSON row and prove it matches TaskGraph's reported count.

    ``tg sql`` surrounds its JSON values with a human table header and a
    ``(N rows)`` footer.  Ignoring an undecodable row turns missing input into a
    smaller, apparently healthy population.  The footer is therefore part of
    the boundary check: a malformed, missing, or duplicated row is an error,
    never an absent task.
    """
    rows: list[dict[str, Any]] = []
    for line in output.splitlines():
        stripped = line.strip()
        if not stripped.startswith("{"):
            continue
        try:
            row = json.loads(stripped)
        except json.JSONDecodeError as error:
            raise RuntimeError(
                f"taskgraph-returned-invalid-json:{error.msg}"
            ) from error
        if not isinstance(row, dict):
            raise RuntimeError("taskgraph-returned-non-object-json-row")
        rows.append(row)

    counts = re.findall(r"^\((\d+) rows\)$", output, flags=re.MULTILINE)
    if len(counts) != 1 or int(counts[0]) != len(rows):
        reported = counts[0] if len(counts) == 1 else "missing-or-ambiguous"
        raise RuntimeError(
            f"taskgraph-row-count-mismatch:parsed={len(rows)},reported={reported}"
        )
    return tuple(rows)


def _taskgraph_in_progress(
    resolution: taskgraph_db.Resolution | None = None,
) -> tuple[TaskRecord, ...]:
    # Two populations, deliberately in one query:
    #   * IN_PROGRESS       -- the live queue (activity, staleness, routing)
    #   * implemented, ANY status -- the landing debt, which under the
    #     close-on-implemented lifecycle lives in CLOSED and was previously
    #     invisible here because the filter was `status = 'IN_PROGRESS'` alone.
    # Tag matching uses json_each on the JSON array so `implemented` cannot be
    # matched inside some longer tag by a LIKE '%implemented%'.
    sql = """
SELECT json_object(
  'id', local_id,
  'title', title,
  'owner', COALESCE(owner, ''),
  'status', status,
  'tags', json(tags),
  'landed', EXISTS (
      SELECT 1 FROM task_notes
      WHERE task_notes.task_id = tasks.local_id
        AND task_notes.content LIKE '%CLOSURE-VERIFIED%'
    )
) AS task_json
FROM tasks
WHERE status = 'IN_PROGRESS'
   OR EXISTS (
        SELECT 1 FROM json_each(tasks.tags)
        WHERE json_each.value = 'implemented'
      )
ORDER BY local_id
""".strip()
    try:
        bound_resolution = (
            taskgraph_db.resolve() if resolution is None else resolution
        )
    except taskgraph_db.TaskGraphUnavailable as error:
        raise RuntimeError(f"taskgraph-query-unavailable:{error}") from error
    if not bound_resolution.path.is_absolute():
        raise RuntimeError(
            "taskgraph-query-unavailable: TaskGraph binding must be an absolute path"
        )
    last_error = "unknown failure"
    for attempt in range(3):
        try:
            process = subprocess.run(
                ["tg", "sql", sql],
                cwd=ROOT,
                capture_output=True,
                text=True,
                timeout=60,
                env=taskgraph_db.child_env(bound_resolution),
            )
        except taskgraph_db.TaskGraphUnavailable as error:
            raise RuntimeError(f"taskgraph-query-unavailable:{error}") from error
        except (FileNotFoundError, OSError, subprocess.TimeoutExpired) as error:
            raise RuntimeError(f"taskgraph-query-unavailable:{error}") from error
        if process.returncode == 0:
            break
        last_error = (process.stderr or process.stdout).strip() or "no diagnostic"
        if not any(
            marker in last_error.lower()
            for marker in (
                "database is locked",
                "database schema changed",
                "error code 17",
            )
        ):
            raise RuntimeError(f"taskgraph-query-failed:{last_error}")
        time.sleep(0.1 * (attempt + 1))
    else:
        raise RuntimeError(f"taskgraph-query-failed-after-retry:{last_error}")

    tasks: list[TaskRecord] = []
    for raw in _taskgraph_json_rows(process.stdout):
        tags = raw.get("tags")
        if not isinstance(tags, list) or not all(isinstance(tag, str) for tag in tags):
            raise RuntimeError(f"taskgraph-task-has-invalid-tags:{raw.get('id')}")
        tasks.append(
            TaskRecord(
                id=str(raw.get("id") or ""),
                title=str(raw.get("title") or ""),
                owner=str(raw.get("owner") or "").strip(),
                tags=tuple(tags),
                status=str(raw.get("status") or "IN_PROGRESS").strip(),
                # SQLite EXISTS yields 0/1; anything unexpected is treated as
                # NOT proven landed, so a schema surprise over-reports debt
                # rather than silently discharging it.
                landed=raw.get("landed") == 1,
            )
        )
    return tuple(tasks)


def _resolve_current_task_ids(
    current_task: str,
    tasks: Sequence[TaskRecord],
) -> tuple[str, ...]:
    """Bind ORC's display value to zero, one, or many TaskGraph identities."""
    # Accept a stable identity if ORC starts carrying local_id directly. Today
    # listAgents() carries a title, truncated as "<prefix>..." at 40 chars.
    candidates = {
        task.id
        for task in tasks
        if task.id == current_task or task.title == current_task
    }
    # A value colliding between one task's local_id and another task's title is
    # ambiguous without a schema discriminator, so preserve both candidates.
    if candidates:
        return tuple(sorted(candidates))

    if current_task.endswith("..."):
        prefix = current_task[:-3]
        return tuple(
            task.id for task in tasks if task.title.startswith(prefix)
        )
    return ()


def reconcile_active_work(
    tasks: Sequence[TaskRecord],
    agents: Sequence[AgentRecord],
) -> ActiveWorkReport:
    everything = tuple(sorted(tasks, key=lambda task: task.id))
    # LANDING DEBT: implemented AND NOT proven landed. Entry is keyed off the
    # tag, not the status, so it survives the close-on-implemented lifecycle;
    # EXIT is the gateway's CLOSURE-VERIFIED ancestry proof. The previous
    # version had entry but no exit and so could only ever rise -- see
    # `TaskRecord.awaiting_landing`. Ancestry verification is still
    # `drain-implemented-to-landed`'s job to ACT on; this monitor only reads the
    # proof the gateway already recorded.
    awaiting_land = tuple(task for task in everything if task.awaiting_landing)
    # LIFECYCLE DEVIATION: implemented but not closed. Reported separately so
    # it is fixable rather than silently absorbed into the landing debt.
    lifecycle_violations = tuple(
        task for task in everything if task.lifecycle_violation
    )
    # THE LIVE QUEUE. Everything downstream — staleness, ownership, routing,
    # orphan detection — is derived from this and ONLY this, so a closed
    # implemented record can never be surfaced as ready work or dispatched.
    in_progress = tuple(
        task for task in everything if task.status.strip().upper() == "IN_PROGRESS"
    )
    active_candidates = tuple(task for task in in_progress if not task.implemented)
    stale = tuple(task for task in active_candidates if not task.owner)
    owned_active = tuple(task for task in active_candidates if task.owner)
    live_agents = tuple(
        sorted((agent for agent in agents if agent.live), key=lambda a: a.name)
    )
    # Coordinators are required census members but intentionally have no
    # current_task. Only workers participate in off-book/routing semantics.
    busy_agents = tuple(
        agent
        for agent in live_agents
        if agent.role != "coordinator" and agent.busy
    )
    live_by_name = {agent.name: agent for agent in live_agents}
    task_by_id = {task.id: task for task in in_progress}
    current_task_candidates = {
        agent.name: _resolve_current_task_ids(
            agent.current_task,
            in_progress,
        )
        for agent in live_agents
        if agent.current_task
    }

    orphaned = tuple(task for task in owned_active if task.owner not in live_by_name)
    claimed: list[TaskRecord] = []
    actually_active: list[TaskRecord] = []
    misrouted: set[Misroute] = set()
    for task in owned_active:
        agent = live_by_name.get(task.owner)
        if agent is None:
            continue
        candidates = current_task_candidates.get(agent.name, ())
        current_task_id = candidates[0] if len(candidates) == 1 else None
        if current_task_id == task.id:
            claimed.append(task)
        if agent.busy and current_task_id == task.id:
            actually_active.append(task)
            continue
        misrouted.add(
            Misroute(
                agent=agent.name,
                task=task.id,
                reason=(
                    f"owner-status={agent.status},"
                    "owner-current-task="
                    f"{current_task_id or agent.current_task or 'none'}"
                ),
            )
        )

    off_book = tuple(agent for agent in busy_agents if not agent.current_task)
    for agent in busy_agents:
        if not agent.current_task:
            continue
        candidates = current_task_candidates[agent.name]
        if len(candidates) != 1:
            misrouted.add(
                Misroute(
                    agent=agent.name,
                    task=agent.current_task,
                    reason=(
                        "current-task-title-is-ambiguous"
                        if candidates
                        else "current-task-is-not-in-progress"
                    ),
                )
            )
            continue
        task = task_by_id[candidates[0]]
        if task.implemented:
            misrouted.add(
                Misroute(
                    agent=agent.name,
                    task=task.id,
                    reason="current-task-is-tagged-implemented",
                )
            )
        elif task.owner != agent.name:
            misrouted.add(
                Misroute(
                    agent=agent.name,
                    task=task.id,
                    reason=f"task-owner={task.owner or 'none'}",
                )
            )

    return ActiveWorkReport(
        in_progress=in_progress,
        awaiting_land=awaiting_land,
        lifecycle_violations=lifecycle_violations,
        stale=stale,
        owned_active=owned_active,
        claimed=tuple(sorted(claimed, key=lambda task: task.id)),
        actually_active=tuple(sorted(actually_active, key=lambda task: task.id)),
        orphaned=tuple(sorted(orphaned, key=lambda task: task.id)),
        off_book=tuple(sorted(off_book, key=lambda agent: agent.name)),
        misrouted=tuple(
            sorted(misrouted, key=lambda item: (item.agent, item.task, item.reason))
        ),
        live_agents=live_agents,
        busy_agents=busy_agents,
    )


def _active_work_detail(report: ActiveWorkReport) -> list[str]:
    detail: list[str] = []
    detail.extend(f"ORPHANED {task.id} owner={task.owner}" for task in report.orphaned)
    detail.extend(f"STALE {task.id}" for task in report.stale)
    # The landing debt is now the whole CLOSED+implemented population, so
    # enumerating it here would bury every other line. Show a bounded sample
    # and state the residue explicitly -- a silent truncation would read as
    # "that is all of them".
    _AWAITING_SAMPLE = 5
    shown = report.awaiting_land[:_AWAITING_SAMPLE]
    detail.extend(
        f"AWAITING-LAND {task.id} owner={task.owner or 'none'}" for task in shown
    )
    residue = len(report.awaiting_land) - len(shown)
    if residue > 0:
        detail.append(
            f"AWAITING-LAND +{residue} more (full set via drain-implemented-to-landed)"
        )
    detail.extend(
        f"LIFECYCLE-VIOLATION {task.id} status={task.status} "
        "implemented-but-not-closed"
        for task in report.lifecycle_violations
    )
    detail.extend(
        f"OFF-BOOK {agent.name} status={agent.status}" for agent in report.off_book
    )
    detail.extend(
        f"MISROUTED {item.agent} task={item.task} {item.reason}"
        for item in report.misrouted
    )
    return detail


def _active_work_json(
    report: ActiveWorkReport, captured_at: float
) -> dict[str, object]:
    return {
        "schema_version": 1,
        "agent_snapshot_captured_at": captured_at,
        "state": "drift" if report.actionable_count else "ok",
        "counts": report.counts(),
        "orphaned": [asdict(task) for task in report.orphaned],
        "stale": [asdict(task) for task in report.stale],
        "awaiting_land": [asdict(task) for task in report.awaiting_land],
        "off_book": [asdict(agent) for agent in report.off_book],
        "misrouted": [asdict(item) for item in report.misrouted],
        "claimed": [asdict(task) for task in report.claimed],
        "actually_active": [asdict(task) for task in report.actually_active],
    }


def active_work_gate(
    *,
    snapshot: str | None = None,
    snapshot_file: Path = DEFAULT_AGENT_SNAPSHOT,
    max_age_secs: int = DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECS,
    json_output: bool = False,
    gate_output: bool = False,
    now: float | None = None,
    persist: bool = True,
) -> int:
    # Claim expiry requires a fresh, complete snapshot carrying identity,
    # status, and current_task. A name-only process census cannot prove that a
    # Herdr parent or one of its nested native workers is absent, so a missing or
    # stale snapshot makes every claim question unknown.
    try:
        agents, captured_at, authority = cast(
            tuple[
                tuple[AgentRecord, ...],
                float,
                dict[str, object] | None,
            ],
            load_agent_snapshot(
                snapshot,
                snapshot_file=snapshot_file,
                max_age_secs=max_age_secs,
                now=now,
                persist=persist,
                include_authority=True,
            ),
        )
        resolution: taskgraph_db.Resolution | None = None
        herdr_binding = _active_herdr_authority(snapshot_file)
        if herdr_binding is not None:
            _workspace, bound_db, binding_sha256, _expected = herdr_binding
            if (
                authority is None
                or authority.get("binding_sha256") != binding_sha256
                or authority.get("taskgraph_db") != str(bound_db)
            ):
                raise RuntimeError(
                    "agent snapshot authority changed after its binding was read"
                )
            # The schema-v2 binding and its fingerprint are the authority.  An
            # interactive caller must not need to inherit TG_DB_PATH from the
            # health service, and an unrelated ambient value must not redirect
            # this read after the snapshot has named and proven an exact DB.
            resolved = taskgraph_db.resolve(bound_db, env={})
            if resolved.path.resolve() != bound_db:
                raise RuntimeError(
                    "agent snapshot TaskGraph binding differs from resolved database"
                )
            resolution = taskgraph_db.Resolution(
                resolved.path, "herdr-snapshot-binding"
            )
        elif authority is not None:
            # Legacy/noncanonical envelopes do not acquire authority merely by
            # carrying an `authority` object.  They retain the original
            # fail-closed ambient resolver until a local binding activates the
            # composite contract.
            resolution = taskgraph_db.resolve()
        if resolution is not None and not resolution.path.is_absolute():
            raise RuntimeError(
                "active-work TaskGraph binding must be an absolute path"
            )
        tasks = _taskgraph_in_progress(resolution)
        if authority is not None:
            observed_owned = tuple(
                sorted(
                    (task.id, task.owner)
                    for task in tasks
                    if task.status.strip().upper() == "IN_PROGRESS" and task.owner
                )
            )
            owned_rows = authority.get("owned_in_progress")
            if not isinstance(owned_rows, list) or any(
                not isinstance(row, dict) for row in owned_rows
            ):
                raise RuntimeError("agent snapshot owned TaskGraph proof is malformed")
            recorded_owned = tuple(
                (str(row["local_id"]), str(row["owner"]))
                for row in owned_rows
                if isinstance(row, dict)
            )
            if recorded_owned != observed_owned:
                raise RuntimeError(
                    "TaskGraph owned IN_PROGRESS population changed after the "
                    f"agent snapshot proof: recorded={recorded_owned!r}, "
                    f"current={observed_owned!r}"
                )
        if herdr_binding is not None:
            final_binding = _active_herdr_authority(snapshot_file)
            if final_binding != herdr_binding:
                raise RuntimeError(
                    "Herdr authority binding changed during active-work reconciliation"
                )
        represented = {agent.name for agent in agents}
        unrepresented = sorted(
            (task.id, task.owner)
            for task in tasks
            if task.status.strip().upper() == "IN_PROGRESS"
            and task.owner
            and task.owner not in represented
        )
        if unrepresented:
            missing_detail = ", ".join(
                f"{task} owner={owner}" for task, owner in unrepresented
            )
            raise RuntimeError(
                "fresh agent snapshot no longer covers the current owned "
                f"TaskGraph population: {missing_detail}"
            )
        report = reconcile_active_work(tasks, agents)
    except RuntimeError as error:
        if json_output:
            print(
                json.dumps(
                    {"schema_version": 1, "state": "unknown", "error": str(error)}
                )
            )
        else:
            _emit({"state": "unknown", "summary": error})
        # 2, not 1: could-not-measure must be distinguishable from a measured
        # drift by exit code alone, the same way unowned_backlog.py separates
        # `unverifiable` (2) from `alert` (1).  A caller that only checks
        # nonzero still pages; one that reads the code can tell them apart.
        return 2

    counts = report.counts()
    state = "drift" if report.actionable_count else "ok"
    if json_output:
        print(json.dumps(_active_work_json(report, captured_at), sort_keys=True))
    elif gate_output:
        fields: dict[str, object] = {
            "state": state,
            **counts,
            "summary": (
                f"in-progress={counts['in_progress']},"
                f"actually-active={counts['actually_active']},"
                f"claimed={counts['claimed']},"
                f"awaiting-land={counts['awaiting_land']},"
                f"lifecycle-violations={counts['lifecycle_violations']},"
                f"stale={counts['stale']},orphaned={counts['orphaned']},"
                f"off-book={counts['off_book']},misrouted={counts['misrouted']}"
            ),
        }
        detail = _active_work_detail(report)
        if detail:
            fields["detail"] = " | ".join(detail)
        _emit(fields)
    else:
        print("ACTIVE-WORK RECONCILIATION")
        print(" ".join(f"{key}={value}" for key, value in counts.items()))
        detail = _active_work_detail(report)
        if detail:
            print("\n".join(detail))
        else:
            print("NO DIVERGENCES")
    return 1 if report.actionable_count else 0


def _add_active_work_options(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--agent-snapshot",
        type=Path,
        help=(
            "read a persisted schema-checked snapshot envelope, or a deliberate "
            "bare agent-array fixture, from this file"
        ),
    )
    parser.add_argument(
        "--max-snapshot-age",
        type=int,
        default=DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECS,
        help="maximum snapshot age in seconds (default: %(default)s)",
    )
    parser.add_argument("--json", action="store_true", help="emit JSON")
    parser.add_argument(
        "--gate", action="store_true", help="emit fields for the health gate"
    )


def _run_active_work(
    parsed: argparse.Namespace, parser: argparse.ArgumentParser
) -> int:
    if parsed.max_snapshot_age < 0:
        parser.error("--max-snapshot-age must be non-negative")
    snapshot = None
    snapshot_file = DEFAULT_AGENT_SNAPSHOT
    if parsed.agent_snapshot is not None:
        try:
            snapshot = parsed.agent_snapshot.read_text()
        except OSError as error:
            parser.error(f"cannot read --agent-snapshot: {error}")
        try:
            explicit_document = json.loads(snapshot)
        except json.JSONDecodeError:
            # Preserve the existing structured invalid-JSON result from
            # load_agent_snapshot rather than converting data errors to argparse.
            pass
        else:
            if isinstance(explicit_document, dict):
                # Real producers persist an envelope. Point the cache reader at
                # the named file so captured_at is honored; passing its text as
                # `snapshot` would incorrectly parse the root mapping as the
                # bare agent sequence used by deliberate fixtures.
                snapshot = None
                snapshot_file = parsed.agent_snapshot
    return active_work_gate(
        snapshot=snapshot,
        snapshot_file=snapshot_file,
        max_age_secs=parsed.max_snapshot_age,
        json_output=parsed.json,
        gate_output=parsed.gate,
        # `--agent-snapshot` NAMES A FILE TO READ.  It must never write one.
        # Before this, the explicit-snapshot branch of `load_agent_snapshot`
        # persisted the payload to `snapshot_file`, which this caller left at
        # its default -- the machine-wide `ignored/ci-hub/agent-snapshot.json`.
        # So `active-work --agent-snapshot /tmp/whatever.json` REPLACED the
        # fleet's snapshot with `/tmp/whatever.json`, freshly stamped, and the
        # flag's name gave no hint of it.  `ci-hub/tests/test_operational_bounds.py`
        # passes a one-entry `{"name": "worker"}` fixture through exactly this
        # path, so running that suite poisoned the live fleet snapshot; the stub
        # was observed in production on 2026-08-07 and again on 2026-08-08.
        # `DEV_HERMIT_PARENT` does not rescue a caller here, because
        # `DEFAULT_AGENT_SNAPSHOT` derives from `ROOT`, i.e. from `__file__`.
        # Seeding the default cache remains the separate `cache-agents` command's
        # job. An explicit file is read-only and cannot replace fleet evidence.
        persist=False,
    )


def active_work_command(args: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="operational_health.py active-work",
        description="Reconcile TaskGraph state, task ownership, and live coordinator agents.",
    )
    _add_active_work_options(parser)
    return _run_active_work(parser.parse_args(args), parser)


MEMORY_SKILL_LINTER = ROOT / "scripts" / "lint-memory-skill-sync.rs"
MEMORY_SKILL_SCANNER = ROOT / "scripts" / "memory-skill-contradiction-scan.rs"


def _run_tool(
    cmd: Sequence[str], *, timeout: float = 300
) -> tuple[int, str, str]:
    """Run a checker; return (returncode, stdout, stderr). Non-runnable -> rc 127.

    A TIMEOUT KILLS THE WHOLE PROCESS GROUP, not just the direct child.
    `subprocess.run(timeout=...)` kills only what it spawned, and a checker
    reached through a shebang is frequently not the process that keeps running:
    measured 2026-08-20, nine `slow-checker` spinners from
    ci-hub/health/tests/test_merge_gate_authority.py had accumulated as PPID=1
    orphans, each holding a whole core, the oldest for 18.9 hours -- 899% of a
    core in total on a box where other agents measure timings. Reproduced
    deterministically: one run of that test leaked exactly one more.

    The same path runs in production, so a real checker that hangs would leave
    the same residue. `start_new_session` puts the child in its own group so the
    group can be killed as a unit.
    """
    try:
        proc = subprocess.Popen(
            list(cmd),
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
    except FileNotFoundError as error:
        return 127, "", str(error)
    except OSError as error:  # e.g. not executable, rust-script missing
        return 127, "", str(error)
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        # `start_new_session` makes the child a group leader, so its pid is the
        # process-group id. Kill the group, then reap, so nothing is left behind
        # and no pipe stays open.
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            proc.kill()
        proc.communicate()
        return 124, "", f"timeout after {timeout:g}s"
    return proc.returncode, stdout, stderr


def _single_line_detail(stdout: str, stderr: str) -> str:
    lines = [line.strip() for line in (stdout + "\n" + stderr).splitlines()]
    return " | ".join(line for line in lines if line) or "checker emitted no detail"


def merge_gate_authority_gate(
    *,
    checker: Path = MERGE_GATE_AUTHORITY_CHECKER,
    timeout: float = MERGE_GATE_AUTHORITY_TIMEOUT_SECS,
) -> int:
    """Expose an interrupted repository-authority transition as one global fault.

    The absence of required hosted checks and the separate default-branch
    history protections live exclusively in
    configure-merge-gate-ruleset.sh. This adapter only bounds that verifier and
    gives its three observable outcomes distinct names: steady, measured
    half-transition, and unavailable evidence.
    """

    rc, stdout, stderr = _run_tool([str(checker), "--check", "--json"], timeout=timeout)
    detail = _single_line_detail(stdout, stderr)
    try:
        result = json.loads(stdout)
    except json.JSONDecodeError as error:
        result = None

    valid_result = (
        isinstance(result, dict)
        and set(result) == {
            "schema",
            "state",
            "gate_policy_matches",
            "history_policy_matches",
        }
        and result.get("schema") == 1
        and isinstance(result.get("gate_policy_matches"), bool)
        and isinstance(result.get("history_policy_matches"), bool)
    )
    if (
        rc == 0
        and valid_result
        and result["state"] == "pass"
        and result["gate_policy_matches"] is True
        and result["history_policy_matches"] is True
    ):
        _emit(
            {
                "state": "ok",
                "scope": "none",
                "summary": detail,
            }
        )
        return 0

    if rc in (124, 127) or rc != 1 or not valid_result or result["state"] != "fail":
        _emit(
            {
                "state": "unavailable",
                "scope": "unknown",
                "summary": (
                    "merge-authority evidence unavailable; no transition verdict: "
                    f"{detail}"
                ),
            }
        )
        return 2

    _emit(
        {
            "state": "interrupted-transition",
            "scope": "global-merge-admission",
            "summary": (
                "GLOBAL Hermit merge authority is half-applied: "
                f"{detail}. Complete the serialized transition or roll it back; "
                "do not diagnose or refire PRs individually."
            ),
        }
    )
    return 1


def _gate_fields(
    stdout: str, *, required: set[str], allowed: set[str]
) -> dict[str, str]:
    """Read one owned key=value report without inventing missing values."""
    fields: dict[str, str] = {}
    for line in stdout.splitlines():
        if "=" not in line or line.startswith((" ", "\t")):
            continue
        key, _, value = line.partition("=")
        key = key.strip()
        if key not in allowed:
            raise RuntimeError(f"gate-report-has-unknown-field:{key}")
        if key in fields:
            raise RuntimeError(f"gate-report-has-duplicate-field:{key}")
        fields[key] = value.strip()
    missing = sorted(required - fields.keys())
    if missing:
        raise RuntimeError("gate-report-is-missing-field:" + ",".join(missing))
    return fields


def _gate_count(fields: Mapping[str, str], field: str) -> int:
    value = fields[field]
    if not value.isdecimal():
        raise RuntimeError(f"gate-report-has-invalid-{field}:{value!r}")
    return int(value)


def memory_skill_sync_gate() -> int:
    """Check authoritative repository skills; local memories are advisory mirrors.

    The structural linter validates versioned skills. The scanner gates only
    contradictions in those skills; local-memory absence or drift never makes
    machine-local state authoritative. Both tools are report-only.
    """
    lint_rc, lint_out, lint_err = _run_tool([str(MEMORY_SKILL_LINTER), "--gate"])
    scan_rc, scan_out, scan_err = _run_tool([str(MEMORY_SKILL_SCANNER), "--gate"])

    # A checker that cannot even run means drift detection is DOWN -> alert.
    if lint_rc in (124, 127) or scan_rc in (124, 127):
        which = "linter" if lint_rc in (124, 127) else "scanner"
        detail = (lint_err if which == "linter" else scan_err).strip() or "unavailable"
        print(lint_out or scan_out, end="")
        _emit(
            {
                "state": "error",
                "problems": "unknown",
                "contradictions": "unknown",
                "summary": f"memory-skill-{which}-unrunnable: {detail} (is rust-script installed?)",
            }
        )
        return 1

    try:
        lint = _gate_fields(
            lint_out,
            required={
                "state",
                "summary",
                "active_skills",
                "mapped_memories",
                "in_sync",
                "problems",
                "warnings",
            },
            allowed={
                "state",
                "summary",
                "active_skills",
                "mapped_memories",
                "in_sync",
                "problems",
                "warnings",
                "detail",
            },
        )
        scan = _gate_fields(
            scan_out,
            required={"state", "summary", "contradictions", "drift"},
            allowed={"state", "summary", "contradictions", "drift"},
        )
        problems = _gate_count(lint, "problems")
        contradictions = _gate_count(scan, "contradictions")
        _gate_count(lint, "active_skills")
        _gate_count(lint, "mapped_memories")
        _gate_count(lint, "in_sync")
        _gate_count(lint, "warnings")
        _gate_count(scan, "drift")
        if lint["state"] not in {"ok", "problems"}:
            raise RuntimeError(f"gate-report-has-invalid-state:{lint['state']!r}")
        if scan["state"] not in {"ok", "contradiction"}:
            raise RuntimeError(f"gate-report-has-invalid-state:{scan['state']!r}")
        if (lint["state"] == "ok") != (problems == 0):
            raise RuntimeError("memory-skill-linter-state-disagrees-with-count")
        if (scan["state"] == "ok") != (contradictions == 0):
            raise RuntimeError("memory-skill-scanner-state-disagrees-with-count")
        if lint_rc not in {0, 1} or (lint_rc == 0) != (lint["state"] == "ok"):
            raise RuntimeError("memory-skill-linter-exit-disagrees-with-report")
        if scan_rc not in {0, 1} or (scan_rc == 0) != (scan["state"] == "ok"):
            raise RuntimeError("memory-skill-scanner-exit-disagrees-with-report")
    except RuntimeError as error:
        _emit(
            {
                "state": "error",
                "problems": "unknown",
                "contradictions": "unknown",
                "summary": str(error),
            }
        )
        return 1

    healthy = lint_rc == 0 and scan_rc == 0 and problems == 0 and contradictions == 0
    if healthy:
        state = "ok"
    elif contradictions and problems:
        state = "both"
    elif contradictions:
        state = "contradiction"
    else:
        state = "drift"

    summary = (
        f"repository structural problems={problems}, "
        f"repository contradictions={contradictions}; "
        "versioned skills are authoritative and optional local-memory drift is advisory"
    )

    # Build a single-line `detail` proposal from the tools' human output. It must
    # be ONE captured field: the tick-hub gate parser (parse_kv_lines) treats EVERY
    # stdout line containing '=' as a key=value pair, so emitting multi-line detail
    # would leak phantom fields and clobber state/summary. Packing it into one
    # collapsed value keeps embedded '=' safe (partition splits on the first only).
    detail_lines: list[str] = []
    if lint_rc != 0 and lint.get("detail"):
        detail_lines.append(f"[lint] {lint['detail']}")
    for line in scan_out.splitlines():
        stripped = line.strip()
        # keep the human report; drop the scanner's own key=value gate fields.
        if not stripped:
            continue
        if stripped.split("=", 1)[0] in {"state", "summary", "contradictions", "drift"}:
            continue
        detail_lines.append(stripped)

    fields: dict[str, object] = {
        "state": state,
        "problems": problems,
        "contradictions": contradictions,
        "summary": summary,
    }
    if not healthy and detail_lines:
        fields["detail"] = " | ".join(detail_lines)
    _emit(fields)
    return 0 if healthy else 1


# Tasks created BEFORE this date are counted and reported but NEVER fire.
#
# Measured 2026-08-25 across 57 open tasks: 57 unlinked references, and of the
# 10 whose own text cites a PR or issue at all, only 2 hyperlink every one --
# 20% compliance. A gate that hard-failed on that backlog would be switched off
# by the first person it stopped, which is strictly worse than 20%. So the
# backlog is grandfathered and only work created from here on is held to the
# rule. Raise this date only to grandfather a NEW backlog, which should never
# be necessary; lowering it retroactively gates history.
#
# It is the day AFTER this landed, not the day of, and that is load-bearing.
# Set to the landing date it fired on SIX tasks created earlier the same day --
# exactly the "hard-fails on pre-existing work" outcome this constant exists to
# prevent. Found by running the gate, not by reading it.
TASK_REFERENCE_GRANDFATHER_DATE = "2026-08-26"

# A reference is "linked" only if a full issue/pull URL for THAT NUMBER appears
# in the same text. Naming the repository is not enough: `hermit#2445` is
# resolvable by someone who knows the fleet and opaque to everyone else, and it
# is the single most common unlinked form (14 of 57 occurrences measured).
# It is deliberately matched here -- an earlier version of this detector missed
# it entirely, because in `hermit#2445` the `#` is preceded by a word character
# so neither a bare-`#` nor an `owner/repo#` pattern fires. That undercount read
# as 2 violations instead of 9.
_TASK_REF_URL = re.compile(r"github\.com/[\w.-]+/[\w.-]+/(?:issues|pull)/(\d{1,5})")
_TASK_REF_REPO = re.compile(r"(?<![\w/])[A-Za-z][\w.-]*(?:/[\w.-]+)?#(\d{1,5})\b")
_TASK_REF_BARE = re.compile(r"(?<![\w/#])#(\d{1,5})\b")
_TASK_REF_WORD = re.compile(r"\b(?:PR|pull request|issue)\s+#?(\d{2,5})\b", re.IGNORECASE)


def unlinked_task_references(text: str) -> set[str]:
    """Issue/PR numbers cited in `text` that carry no full hyperlink."""
    if not text:
        return set()
    linked = set(_TASK_REF_URL.findall(text))
    cited = (
        set(_TASK_REF_BARE.findall(text))
        | set(_TASK_REF_WORD.findall(text))
        | set(_TASK_REF_REPO.findall(text))
    )
    return cited - linked


def task_reference_hygiene_gate() -> int:
    """Report open tasks that proxy a GitHub issue/PR without a full hyperlink.

    ADVISORY BY CONSTRUCTION, in two separate ways, because the failure mode
    being avoided is the gate getting disabled rather than the rule going
    unmet:

    1. Only tasks created on or after TASK_REFERENCE_GRANDFATHER_DATE can fire.
       The pre-existing backlog is reported as a number and never fails.
    2. It fires on TASK TEXT (title + description) only -- the task itself
       proxying a PR -- never on notes. Notes are appended continuously during
       normal work and gating them would fire on every agent that mentions a
       number in passing.

    The notes population is still COUNTED and reported, because it is where the
    unresolvable citations actually accumulated: the three tasks written off as
    unmatchable (#19/#25/#50/#77/#80/#82/#114/#115) carry zero bare numbers in
    their titles or descriptions. Every one is in a note. A check that only ever
    looked at descriptions would have caught none of them, so the number is kept
    visible here rather than dropped for being unenforceable.

    Why a check at all, when the rule already exists in writing: it already
    exists in writing and sits at 20%. A directive at 20% is a preference.
    """
    sql = """
SELECT json_object(
  'id', local_id,
  'created', COALESCE(created_at, ''),
  'text', COALESCE(title, '') || '
' || COALESCE(description, ''),
  'notes', COALESCE((
      SELECT group_concat(content, '
') FROM task_notes WHERE task_notes.task_id = tasks.local_id
    ), '')
) AS task_json
FROM tasks
WHERE status IN ('OPEN', 'IN_PROGRESS')
ORDER BY local_id
""".strip()
    try:
        process = subprocess.run(
            ["tg", "sql", sql],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=60,
            env=taskgraph_db.child_env(taskgraph_db.resolve()),
        )
    except (
        taskgraph_db.TaskGraphUnavailable,
        FileNotFoundError,
        OSError,
        subprocess.TimeoutExpired,
    ) as error:
        # A checker that cannot run is DOWN, not clean. Say so rather than
        # emitting a reassuring zero.
        _emit(
            {
                "state": "error",
                "open_tasks": "unknown",
                "unlinked_tasks": "unknown",
                "new_violations": "unknown",
                "summary": f"task-reference-lint-unrunnable: {error}",
            }
        )
        return 1
    if process.returncode != 0:
        detail = (process.stderr or process.stdout).strip() or "no diagnostic"
        _emit(
            {
                "state": "error",
                "open_tasks": "unknown",
                "unlinked_tasks": "unknown",
                "new_violations": "unknown",
                "summary": f"task-reference-query-failed: {detail}",
            }
        )
        return 1

    try:
        rows = _taskgraph_json_rows(process.stdout)
    except RuntimeError as error:
        _emit(
            {
                "state": "error",
                "open_tasks": "unknown",
                "unlinked_tasks": "unknown",
                "new_violations": "unknown",
                "summary": str(error),
            }
        )
        return 1

    total = 0
    grandfathered: list[str] = []
    violations: list[str] = []
    notes_only = 0
    for row in rows:
        for field in ("id", "created", "text", "notes"):
            if not isinstance(row.get(field), str):
                _emit(
                    {
                        "state": "error",
                        "open_tasks": "unknown",
                        "unlinked_tasks": "unknown",
                        "new_violations": "unknown",
                        "summary": (
                            "task-reference-row-has-invalid-"
                            f"{field}:{row.get('id', '?')}"
                        ),
                    }
                )
                return 1
        if not row["id"]:
            _emit(
                {
                    "state": "error",
                    "open_tasks": "unknown",
                    "unlinked_tasks": "unknown",
                    "new_violations": "unknown",
                    "summary": "task-reference-row-has-empty-id",
                }
            )
            return 1
        total += 1
        bad = unlinked_task_references(row["text"])
        if unlinked_task_references(row["notes"]) and not bad:
            notes_only += 1
        if not bad:
            continue
        cited = ",".join("#" + n for n in sorted(bad, key=int)[:3])
        entry = f"{row.get('id', '?')}({cited})"
        if row["created"] >= TASK_REFERENCE_GRANDFATHER_DATE:
            violations.append(entry)
        else:
            grandfathered.append(entry)

    if violations:
        summary = (
            f"{len(violations)} task(s) created on/after "
            f"{TASK_REFERENCE_GRANDFATHER_DATE} cite an issue/PR with no full "
            f"hyperlink: {', '.join(sorted(violations)[:5])}"
            + ("..." if len(violations) > 5 else "")
            + ". Use the full https://github.com/<owner>/<repo>/pull/<n> URL; a "
            "bare number cannot be resolved from its own text and a repo name "
            "without a link still is not one."
        )
        state = "unlinked"
    else:
        summary = (
            f"all {total} open task(s) clean of new unlinked issue/PR references"
        )
        state = "clean"
    _emit(
        {
            "state": state,
            "open_tasks": total,
            "unlinked_tasks": len(violations) + len(grandfathered),
            "new_violations": len(violations),
            "grandfathered": len(grandfathered),
            "notes_only": notes_only,
            "summary": summary,
        }
    )
    return 1 if violations else 0


def _checkout_freshness_note() -> str:
    """Say whether THIS checkout is behind origin/main, or that we cannot tell.

    Deliberately measures rather than assumes, and reports all three outcomes
    distinctly. Swapping an unestablished "your command is invalid" for an
    unestablished "your checkout is stale" would be the same defect wearing a
    friendlier message.

    No fetch: this reads the last-fetched origin/main, so the distance is a
    LOWER BOUND and the text says so. An error path is the wrong place to make
    a network call.
    """
    try:
        head = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            cwd=ROOT, capture_output=True, text=True, timeout=10,
        )
        behind = subprocess.run(
            ["git", "rev-list", "--count", "HEAD..origin/main"],
            cwd=ROOT, capture_output=True, text=True, timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return (
            f"checkout freshness UNKNOWN ({error}); this may be a stale checkout "
            "rather than a bad command -- check before concluding the gate is broken"
        )
    if behind.returncode != 0 or not behind.stdout.strip().isdigit():
        detail = (behind.stderr or "no origin/main ref").strip().splitlines()[:1]
        return (
            f"checkout freshness UNKNOWN ({detail[0] if detail else 'unavailable'}); "
            "this may be a stale checkout rather than a bad command"
        )
    count = int(behind.stdout.strip())
    sha = head.stdout.strip() or "unknown"
    if count == 0:
        return (
            f"this checkout ({sha}) is level with the last-fetched origin/main, so "
            "the command genuinely does not exist here -- this is not staleness"
        )
    return (
        f"THIS CHECKOUT IS STALE: {sha} is at least {count} commit(s) behind the "
        "last-fetched origin/main (no fetch was made, so that is a lower bound). "
        "A subcommand added on main does not exist in an old tree, and argparse "
        "reports that as an invalid choice. Refresh the checkout, or run the tool "
        "from a fresh clone, before concluding the gate is broken."
    )


class _FreshnessAwareParser(argparse.ArgumentParser):
    """Argparse that will not blame the input for what may be a stale tree.

    Measured 2026-08-25: running `task-reference-hygiene` from a checkout 146
    commits behind main exited 2 with "invalid choice", which reads as a broken
    gate. The subcommand was landed and fine; the tree was old. That is the
    second staleness-sourced false signal of the night -- a stale test-harness
    binary rejecting current main's manifests as corrupt was the first -- and it
    is the same shape as a checker reporting "does not compile" for a submodule
    that was never populated: asserting a cause it did not establish.
    """

    def error(self, message: str) -> NoReturn:  # type: ignore[override]
        if "invalid choice" in message:
            message = f"{message}\n{_checkout_freshness_note()}"
        super().error(message)


def build_parser() -> argparse.ArgumentParser:
    parser = _FreshnessAwareParser(
        description="Run one dev-hermit operational health check."
    )
    subparsers = parser.add_subparsers(
        dest="command", required=True, metavar="COMMAND"
    )
    active_work = subparsers.add_parser(
        "active-work",
        help="reconcile TaskGraph state, task ownership, and live ORC agents",
        description="Reconcile TaskGraph state, task ownership, and live ORC agents.",
    )
    _add_active_work_options(active_work)
    active_work.set_defaults(_command_parser=active_work)

    cache_agents = subparsers.add_parser(
        "cache-agents",
        help="cache the current ORC agent snapshot",
        description="Cache the current ORC agent snapshot.",
    )
    cache_agents.add_argument(
        "--snapshot-file",
        type=Path,
        default=DEFAULT_AGENT_SNAPSHOT,
        help="destination for the validated snapshot (default: %(default)s)",
    )

    cache_disk = subparsers.add_parser(
        "cache-disk",
        help="publish a fresh btrfs free-space reading",
        description="Publish a fresh btrfs free-space reading.",
    )
    cache_disk.add_argument(
        "--filesystem-path",
        type=Path,
        default=DEFAULT_DISK_FILESYSTEM_PATH,
        help="path on the btrfs filesystem to measure (default: %(default)s)",
    )
    cache_disk.add_argument(
        "--state-file",
        type=Path,
        default=DEFAULT_DISK_STATE,
        help="destination for the disk reading (default: %(default)s)",
    )

    cache_disk_paths_parser = subparsers.add_parser(
        "cache-disk-paths",
        help="publish bounded physical usage for top-level disk populations",
        description="Publish bounded physical usage for top-level disk populations.",
    )
    cache_disk_paths_parser.add_argument(
        "--state-root",
        type=Path,
        default=STATE_ROOT,
        help="dev-hermit parent whose populations are measured (default: %(default)s)",
    )
    cache_disk_paths_parser.add_argument(
        "--state-file",
        type=Path,
        default=DEFAULT_DISK_PATH_STATE,
        help="destination for the current path reading (default: %(default)s)",
    )
    cache_disk_paths_parser.add_argument(
        "--previous-state-file",
        type=Path,
        default=DEFAULT_DISK_PATH_PREVIOUS_STATE,
        help="destination for the preceding path reading (default: %(default)s)",
    )
    cache_disk_paths_parser.add_argument(
        "--population-timeout",
        type=float,
        default=DEFAULT_DISK_PATH_TIMEOUT_SECS,
        help="per-population compsize timeout in seconds (default: %(default)s)",
    )
    cache_disk_paths_parser.set_defaults(_command_parser=cache_disk_paths_parser)

    input_freshness = subparsers.add_parser(
        "input-freshness",
        help="report stale agent and disk producer snapshots",
        description="Report stale agent and disk producer snapshots.",
    )
    input_freshness.add_argument(
        "--state-root",
        type=Path,
        default=STATE_ROOT,
        help="state root containing ignored producer snapshots (default: %(default)s)",
    )
    input_freshness.add_argument(
        "--agent-max-age-secs",
        type=int,
        default=DEFAULT_AGENT_SNAPSHOT_MAX_AGE_SECS,
        help="maximum agent snapshot age in seconds (default: %(default)s)",
    )
    input_freshness.add_argument(
        "--disk-max-age-secs",
        type=int,
        default=DEFAULT_DISK_SNAPSHOT_MAX_AGE_SECS,
        help="maximum disk snapshot age in seconds (default: %(default)s)",
    )
    input_freshness.set_defaults(_command_parser=input_freshness)

    pull_requests = subparsers.add_parser(
        "pull-requests",
        help="report pull-request validation and queue health",
        description="Report pull-request validation and queue health.",
    )
    pull_requests.add_argument(
        "--census-cache-dir",
        type=Path,
        default=pr_status.DEFAULT_CENSUS_CACHE_DIR,
        help="directory for the bounded all-open PR census cache",
    )

    primary_snapshot = subparsers.add_parser(
        "primary-snapshot",
        help="refresh the primary checkout snapshot when safe",
        description="Refresh the primary checkout snapshot when safe.",
    )
    primary_snapshot.add_argument(
        "--state-root",
        type=Path,
        default=STATE_ROOT,
        help="mutable primary checkout and deferral-state root",
    )

    commands = (
        ("github-main", "report hosted workflow state for current main"),
        ("agents", "report stuck or broken agents from the ORC snapshot"),
        ("queue-health", "report runner queue depth, waits, and stale runs"),
        ("memory-skill-sync", "check local memory skill mirrors for contradictions"),
        (
            "merge-gate-authority",
            "check whether merge-gate rules match the required authority",
        ),
        (
            "task-reference-hygiene",
            "report open tasks citing an issue/PR without a full hyperlink",
        ),
    )
    for name, description in commands:
        subparsers.add_parser(name, help=description, description=description)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parsed = build_parser().parse_args(argv)
    if parsed.command == "active-work":
        return _run_active_work(parsed, parsed._command_parser)
    if parsed.command == "cache-agents":
        return cache_agent_snapshot(snapshot_file=parsed.snapshot_file)
    if parsed.command == "cache-disk":
        return cache_disk_free(
            filesystem_path=parsed.filesystem_path,
            state_file=parsed.state_file,
        )
    if parsed.command == "cache-disk-paths":
        if parsed.population_timeout <= 0:
            parsed._command_parser.error("--population-timeout must be positive")
        return cache_disk_paths(
            state_root=parsed.state_root,
            state_file=parsed.state_file,
            previous_state_file=parsed.previous_state_file,
            population_timeout=parsed.population_timeout,
        )
    if parsed.command == "input-freshness":
        if parsed.agent_max_age_secs <= 0:
            parsed._command_parser.error("--agent-max-age-secs must be positive")
        if parsed.disk_max_age_secs <= 0:
            parsed._command_parser.error("--disk-max-age-secs must be positive")
        return operational_input_freshness_gate(
            state_root=parsed.state_root.resolve(),
            agent_max_age_secs=parsed.agent_max_age_secs,
            disk_max_age_secs=parsed.disk_max_age_secs,
        )
    if parsed.command == "pull-requests":
        return pull_request_gate(census_cache_dir=parsed.census_cache_dir)
    if parsed.command == "primary-snapshot":
        return primary_snapshot_gate(root=parsed.state_root.resolve())
    commands = {
        "github-main": github_main_gate,
        "agents": agent_gate,
        "queue-health": queue_health_gate,
        "memory-skill-sync": memory_skill_sync_gate,
        "merge-gate-authority": merge_gate_authority_gate,
        "task-reference-hygiene": task_reference_hygiene_gate,
    }
    return commands[parsed.command]()


if __name__ == "__main__":
    raise SystemExit(main())
