#!/usr/bin/env python3
"""Emit tick-hub gate fields for dev-hermit operational health checks."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from io import StringIO
from pathlib import Path
from typing import Any, NoReturn

ROOT = Path(__file__).resolve().parents[2]
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
MERGE_GATE_AUTHORITY_PROBE_TIMEOUT_SECS = float(
    os.environ.get("CI_HUB_MERGE_GATE_AUTHORITY_PROBE_TIMEOUT", "4")
)
MERGE_GATE_AUTHORITY_CHECKER = (
    ROOT / "hermit" / "scripts" / "configure-merge-gate-ruleset.sh"
)
MERGE_GATE_REPO = "rrnewton/hermit"
DEFAULT_AGENT_SNAPSHOT = ROOT / "ignored" / "ci-hub" / "agent-snapshot.json"
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
    last_activity: float | None = None

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


def pull_request_gate() -> int:
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
            status = pr_status.fetch_repo_status(repo, timeout=budget)
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
SNAPSHOT_DEFERRAL_STATE = ROOT / ".tick-hub" / "primary-snapshot-deferral.json"
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
    state_path: Path | None = None,
    now: float | None = None,
    budget_secs: int = SNAPSHOT_DEFERRAL_BUDGET_SECS,
) -> int:
    output, errors = StringIO(), StringIO()
    result = primary_checkout.checkout_fresh(
        primary_checkout.default_root(),
        publish_parent=True,
        strict=True,
        deadline_secs=PRIMARY_SNAPSHOT_INNER_BUDGET_SECS,
        out=output,
        err=errors,
    )
    path = state_path or SNAPSHOT_DEFERRAL_STATE
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
                last_activity=_last_activity_seconds(raw.get("last_activity")),
            )
        )
    return tuple(sorted(agents, key=lambda agent: agent.name))


def _persist_agent_snapshot(
    path: Path,
    payload: object,
    *,
    captured_at: float,
) -> None:
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
    pane-bound ORC agent carries a `%NN` pane identity. The full ORC array also
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
    pane_bound = sum(agent.tmux_pane_id is not None for agent in agents)
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
) -> tuple[tuple[AgentRecord, ...], float]:
    observed_at = time.time() if now is None else now
    text = (
        snapshot
        if snapshot is not None
        else os.environ.get("HERMIT_AGENT_SNAPSHOT_JSON")
    )
    if text is not None:
        try:
            payload: object = json.loads(text)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"invalid-agent-snapshot:{error.msg}") from error
        agents = _parse_agents(payload)
        if persist:
            _persist_agent_snapshot(snapshot_file, payload, captured_at=observed_at)
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
    return _parse_agents(agents_payload), float(captured_at)


def _emit_live_names_report(
    names: Sequence[str], *, reason: str, json_output: bool
) -> int:
    """Report claim EXPIRY from a name-only live fleet, and refuse the rest by name.

    ⚠️ ONE OF THREE QUESTIONS. `orphaned` is exactly `owner not in live_by_name`,
    which a name read settles. `claimed`, `actually_active` and `misrouted` each
    need an agent's `current_task`, which a name read does not carry -- computing
    them from names would mark EVERY LIVE agent's claim misrouted. Naming them in
    `not_determined` beats returning a confident wrong number for them.
    """
    owned = [
        task
        for task in _taskgraph_in_progress()
        if task.status.strip().upper() == "IN_PROGRESS"
        and not task.implemented
        and task.owner
    ]
    orphaned = sorted(task.id for task in owned if task.owner not in set(names))
    payload = {
        "schema_version": 1,
        "state": "degraded",
        "census_mode": "live-names-only",
        "reason": reason,
        "live_agents_examined": len(names),
        "live_agents": list(names),
        "orphaned": orphaned,
        "orphaned_count": len(orphaned),
        "not_determined": ["claimed", "actually_active", "misrouted"],
        "summary": (
            f"{reason}; read the fleet LIVE instead. {len(orphaned)} owned "
            f"IN_PROGRESS task(s) have an owner absent from a {len(names)}-agent "
            "live fleet and ARE reclaimable. claimed/actually_active/misrouted are "
            "NOT DETERMINED in this mode: they need each agent's current_task, "
            "which a name-only read does not carry."
        ),
    }
    if json_output:
        print(json.dumps(payload))
    else:
        print(payload["summary"])
        for task_id in orphaned:
            print(f"  ORPHANED (owner absent from live fleet): {task_id}")
    # Non-zero: a degraded answer to one of three questions. A caller that treats
    # 0 as "census complete" must not get it from here.
    return 1


def live_agent_names() -> tuple[str, ...] | None:
    """Read the fleet LIVE, by name, when the cached snapshot cannot be trusted.

    ⚠️ THIS EXISTS BECAUSE THE AUTHORITY COULD NOT ANSWER ITS OWN QUESTION.
    `CLAUDE.md` names `active-work` the machine-readable authority for claim
    expiry: a claim expires when a fresh ORC snapshot proves its owner is gone.
    Measured 2026-08-26, `ignored/ci-hub/agent-snapshot.json` was 356283s -- 4.12
    days -- old against a 600s window, so `load_agent_snapshot` raised and NO
    claim on the board could be expired by the documented mechanism, however dead
    its owner. The scheduled ORC workflow already carried the full snapshot, but
    its receiver had been deleted, leaving this fallback as the only answer.

    ⚠️ AND THE ANSWER WAS ALREADY IN THE TREE. `slot_agent_parity.py` hit the same
    file at 65.8 hours and responded by reading the census live through
    `residue_sweep.read_live_fleet()`. This calls that same function rather than
    inventing a second fleet source, so the two cannot disagree.
    """
    try:
        import importlib.util

        spec = importlib.util.spec_from_file_location(
            "_residue_sweep", ROOT / "ci-hub" / "health" / "residue_sweep.py"
        )
        if spec is None or spec.loader is None:
            return None
        module = importlib.util.module_from_spec(spec)
        # ⚠️ REGISTER BEFORE exec_module, AND THIS IS NOT STYLE. `dataclasses`
        # resolves a class's annotations through `sys.modules[cls.__module__]`,
        # so exec'ing a module that is not yet registered makes that lookup
        # return None and every `@dataclass` in it dies with
        # `'NoneType' object has no attribute '__dict__'`. Cost me a debugging
        # pass: the failure surfaced as "no live read available", which reads
        # like the fleet being unreachable rather than a loader bug here.
        sys.modules.setdefault("_residue_sweep", module)
        spec.loader.exec_module(module)
        fleet, _residues = module.read_live_fleet()
    except Exception as error:
        # A failed live read is NOT an empty fleet. Returning () here would make
        # every owned task look orphaned, which is the reclaim-a-live-agent's-work
        # direction; None keeps the caller on the existing hard refusal.
        #
        # ⚠️ SAY WHY. A bare `return None` reported a loader bug in this very
        # function as "the fleet is unreachable" -- indistinguishable from the
        # condition it exists to handle, and it hid a real defect for a whole
        # debugging pass.
        print(f"live-fleet-read-failed:{type(error).__name__}:{error}", file=sys.stderr)
        return None
    names = tuple(sorted(str(name) for name in fleet))
    return names or None


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


def _taskgraph_in_progress() -> tuple[TaskRecord, ...]:
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
    last_error = "unknown failure"
    for attempt in range(3):
        try:
            process = subprocess.run(
                ["tg", "sql", sql],
                cwd=ROOT,
                capture_output=True,
                text=True,
                timeout=60,
                env=taskgraph_db.child_env(taskgraph_db.resolve()),
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
    for line in process.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            raw = json.loads(line)
        except json.JSONDecodeError as error:
            raise RuntimeError(
                f"taskgraph-returned-invalid-json:{error.msg}"
            ) from error
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
    count_match = re.search(r"\((\d+) rows\)", process.stdout)
    if count_match is None or int(count_match.group(1)) != len(tasks):
        raise RuntimeError(
            f"taskgraph-row-count-mismatch:parsed={len(tasks)},reported="
            f"{count_match.group(1) if count_match else 'missing'}"
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
    busy_agents = tuple(agent for agent in live_agents if agent.busy)
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
    # The ORC production workflow supplies the full agent array to health-tick
    # every sixty minutes. health-tick persists that observation through the
    # explicit `cache-agents` command after validating its structure. Prefer the
    # fresh full snapshot because `claimed`, `actually_active`, and `misrouted`
    # require status and current_task; the live fallback has names only and can
    # determine `orphaned` but must refuse those other questions.
    try:
        agents, captured_at = load_agent_snapshot(
            snapshot,
            snapshot_file=snapshot_file,
            max_age_secs=max_age_secs,
            now=now,
            persist=persist,
        )
        report = reconcile_active_work(_taskgraph_in_progress(), agents)
    except RuntimeError as error:
        # ⚠️ EXPIRY IS ANSWERABLE FROM NAMES ALONE; THE REST IS NOT. `orphaned` is
        # exactly `owner not in live_by_name`, which a name-only live read
        # settles. `claimed`, `actually_active` and `misrouted` all need each
        # agent's `current_task`, which a name read does not carry -- computing
        # them from names would mark EVERY live agent's claim misrouted. So this
        # answers the expiry question, labels itself, and refuses the others by
        # name rather than by returning a wrong number for them.
        names = live_agent_names()
        if names is not None:
            return _emit_live_names_report(
                names, reason=str(error), json_output=json_output
            )
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
        help="read the ORC agent snapshot from this file",
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
    if parsed.agent_snapshot is not None:
        try:
            snapshot = parsed.agent_snapshot.read_text()
        except OSError as error:
            parser.error(f"cannot read --agent-snapshot: {error}")
    return active_work_gate(
        snapshot=snapshot,
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
        description="Reconcile TaskGraph state, task ownership, and live ORC agents.",
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


def _github_variable_surface_available() -> tuple[bool, str]:
    """Distinguish a genuinely absent variable from unreadable ambient auth.

    The authoritative shell checker intentionally maps `gh variable get`
    failures to an empty value so its reconciliation modes can create missing
    variables. In read-only health that shape is ambiguous: it could be a real
    half-transition or merely expired credentials. A strict list probe resolves
    availability only; the shell checker remains the sole semantic authority.
    """

    gh = ["gh"]
    if shutil.which("with-proxy") is not None:
        gh = ["with-proxy", "gh"]
    rc, stdout, stderr = _run_tool(
        [*gh, "variable", "list", "--repo", MERGE_GATE_REPO, "--json", "name"],
        timeout=MERGE_GATE_AUTHORITY_PROBE_TIMEOUT_SECS,
    )
    if rc != 0:
        return False, _single_line_detail(stdout, stderr)
    try:
        variables = json.loads(stdout)
    except json.JSONDecodeError as error:
        return False, f"variable-list returned invalid JSON: {error}"
    if not isinstance(variables, list):
        return False, "variable-list returned a non-list JSON value"
    return True, "GitHub variable surface readable"


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

    rc, stdout, stderr = _run_tool([str(checker), "--check"], timeout=timeout)
    detail = _single_line_detail(stdout, stderr)

    # Require the verifier's own positive statement, not merely an exit code
    # which could have come from a wrapper that never exercised the predicate.
    if rc == 0 and "PASS:" in stdout:
        _emit(
            {
                "state": "ok",
                "scope": "none",
                "summary": detail,
            }
        )
        return 0

    if rc in (124, 127) or rc != 1 or "FAIL:" not in stderr:
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

    # `read_variable` deliberately turns a failed variable read into `unset`.
    # Confirm the surface is readable before treating that word as evidence of
    # an actually missing binding rather than evidence of lost credentials.
    if "unset" in stderr:
        available, availability_detail = _github_variable_surface_available()
        if not available:
            _emit(
                {
                    "state": "unavailable",
                    "scope": "unknown",
                    "summary": (
                        "merge-authority variables could not be measured; "
                        "the checker's unset value is NOT transition evidence: "
                        f"{availability_detail}"
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


def _scan_fields(stdout: str) -> dict[str, str]:
    fields: dict[str, str] = {}
    for line in stdout.splitlines():
        if "=" in line and not line.startswith((" ", "\t")):
            key, _, value = line.partition("=")
            key = key.strip()
            if key in {"state", "summary", "contradictions", "drift"}:
                fields.setdefault(key, value.strip())
    return fields


def memory_skill_sync_gate() -> int:
    """Check authoritative repository skills; local memories are advisory mirrors.

    The structural linter validates versioned skills. The scanner gates only
    contradictions in those skills; local-memory absence or drift never makes
    machine-local state authoritative. Both tools are report-only.
    """
    lint_rc, lint_out, lint_err = _run_tool([str(MEMORY_SKILL_LINTER), "--quiet"])
    scan_rc, scan_out, scan_err = _run_tool([str(MEMORY_SKILL_SCANNER), "--gate"])

    # A checker that cannot even run means drift detection is DOWN -> alert.
    if lint_rc in (124, 127) or scan_rc in (124, 127):
        which = "linter" if lint_rc in (124, 127) else "scanner"
        detail = (lint_err if which == "linter" else scan_err).strip() or "unavailable"
        print(lint_out or scan_out, end="")
        _emit(
            {
                "state": "error",
                "problems": 0,
                "contradictions": 0,
                "summary": f"memory-skill-{which}-unrunnable: {detail} (is rust-script installed?)",
            }
        )
        return 1

    problems = 0
    match = re.search(r"problems:\s*(\d+)", lint_out)
    if match:
        problems = int(match.group(1))
    elif lint_rc != 0:
        problems = 1  # linter failed but count unparsed

    scan = _scan_fields(scan_out)
    contradictions = int(scan.get("contradictions", "0") or "0")

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
    if lint_rc != 0:
        for line in lint_out.splitlines():
            stripped = line.strip()
            if stripped and not stripped.startswith("OK "):
                detail_lines.append(f"[lint] {stripped}")
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
                "open_tasks": 0,
                "unlinked_tasks": 0,
                "new_violations": 0,
                "summary": f"task-reference-lint-unrunnable: {error}",
            }
        )
        return 1
    if process.returncode != 0:
        detail = (process.stderr or process.stdout).strip() or "no diagnostic"
        _emit(
            {
                "state": "error",
                "open_tasks": 0,
                "unlinked_tasks": 0,
                "new_violations": 0,
                "summary": f"task-reference-query-failed: {detail}",
            }
        )
        return 1

    total = 0
    grandfathered: list[str] = []
    violations: list[str] = []
    notes_only = 0
    for line in process.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        total += 1
        bad = unlinked_task_references(row.get("text", ""))
        if unlinked_task_references(row.get("notes", "")) and not bad:
            notes_only += 1
        if not bad:
            continue
        cited = ",".join("#" + n for n in sorted(bad, key=int)[:3])
        entry = f"{row.get('id', '?')}({cited})"
        if str(row.get("created", "")) >= TASK_REFERENCE_GRANDFATHER_DATE:
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

    commands = (
        ("cache-agents", "cache the current ORC agent snapshot"),
        ("github-main", "report hosted workflow state for current main"),
        ("pull-requests", "report pull-request validation and queue health"),
        ("primary-snapshot", "refresh the primary checkout snapshot when safe"),
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
    commands = {
        "cache-agents": cache_agent_snapshot,
        "github-main": github_main_gate,
        "pull-requests": pull_request_gate,
        "primary-snapshot": primary_snapshot_gate,
        "agents": agent_gate,
        "queue-health": queue_health_gate,
        "memory-skill-sync": memory_skill_sync_gate,
        "merge-gate-authority": merge_gate_authority_gate,
        "task-reference-hygiene": task_reference_hygiene_gate,
    }
    return commands[parsed.command]()


if __name__ == "__main__":
    raise SystemExit(main())
