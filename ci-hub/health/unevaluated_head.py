#!/usr/bin/env python3
"""Detect PR heads that NO workflow ever evaluated.

WHY THIS EXISTS. Measured 2026-08-10 on rrnewton/hermit: 6 of the 25 most
recently updated open PRs (24%) had **zero** workflow runs at their current
head, and every one of them was CONFLICTING. GitHub cannot compute
``refs/pull/N/merge`` for a PR that conflicts with its base, and ``pull_request``
workflows check out that merge commit, so **no run is created at all**. The head
is silently never evaluated.

THE DANGER IS NOT THE ABSENCE, IT IS THAT THE ABSENCE LOOKS LIKE HEALTH. With no
runs there are no failing checks, so every surface reads clean: no red X, no
"checks pending", nothing. "Evaluated and clean" and "never evaluated" are
indistinguishable from the outside, and only the second is dangerous.

IT COMPOUNDS, AND NOT INDEPENDENTLY. ``merge-gate.yml``'s
``invalidate-local-validation`` job -- the thing that strips a ``passed-review-*``
label when the head moves -- is ITSELF a ``pull_request`` job. So the same
conflict that prevents evaluation also prevents invalidation. One cause disables
both guards, which is why a PR can end up APPROVED, UNEVALUATED, and green-looking
at the same time. Measured live on #1864.

WHAT THIS REPORTS. SIX states, and only ONE is good news:

  EVALUATED_PASS        every run at the exact head concluded successfully
  EVALUATED_FAIL        a real bad answer, including a cancel that killed
                        RUNNING jobs (a hang or an external kill IS a result)
  PENDING               a run exists and has not concluded; carries `stale`
  SUPERSEDED            cancelled AND a successor run is VISIBLE -- the only
                        case where supersession is asserted as fact
  CANCELLED_UNVERIFIED  cancelled with no verdict and NO successor evidence.
                        A hole whose CAUSE IS NOT ESTABLISHED.
  NEVER_RAN             no run object exists at all

A failing gate is a RESULT. No gate at all is not a result, and collapsing the
two is the defect. Compare `slot_disk_residue.py` / `holder_liveness.py`, which
draw the same distinction for slots.

Runs are matched on the EXACT head SHA. A run at an older head is evidence about
that older head and nothing else -- the same binding rule the approval labels
fail to apply.

Usage:
  unevaluated_head.py --repo rrnewton/hermit [--limit N] [--gate] [--json]
  unevaluated_head.py --from-json FIXTURE [--gate]   # offline, for the bracket

--gate exits 1 when any inspected head carries an actionable NO VERDICT --
NEVER_RAN, SUPERSEDED, CANCELLED_UNVERIFIED, or a PENDING past the stale window.
When GitHub Actions are deactivated by a standing owner directive, NEVER_RAN is
reported as expected rather than actionable. It is still not a pass or failure.
The gate is deliberately silent about EVALUATED_FAIL (a red gate is another
check's job) and about a FRESH pending (alarming on every push is an alarm no
coordinator can satisfy).

A CAUSE IS ONLY EVER STATED AS FACT WHEN IT WAS OBSERVED. `SUPERSEDED` requires
a visible successor run; everything else cancelled is `CANCELLED_UNVERIFIED`
with the cause explicitly not established. An earlier version inferred
supersession from zero jobs OR from a FAILED jobs query, which turned missing
data into a causal claim -- the same absence-as-verdict defect one layer down,
in the CAUSE rather than the STATE.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import tempfile
import time
import sys
from datetime import datetime
from typing import Any
from urllib.parse import quote

# FIVE STATES, AND ONLY ONE IS GOOD NEWS. Three was not enough, and the two it
# was missing were both being reported as the good one or as the wrong bad one:
#
#   NEVER_RAN       no run object exists at all -- the head was never evaluated
#   SUPERSEDED      a run was created and cancelled BEFORE ANY JOB STARTED,
#                   i.e. a newer push took the concurrency group. A HOLE, not a
#                   verdict. Superseding an obsolete run is CORRECT behaviour;
#                   only rendering it as an absent verdict was wrong.
#   PENDING         a run exists and has not concluded. Carries `stale` once it
#                   outlives the fresh window.
#   EVALUATED_FAIL  a real bad answer, including a cancel that killed RUNNING
#                   jobs (a hang or an external kill is a result, not a hole)
#   EVALUATED_PASS  every run at this head concluded successfully
#
# MEASURED DEFECT THIS REPLACES, in this very file: a run that was still QUEUED
# or IN PROGRESS has no failing conclusion, so the old three-state classifier
# fell through and returned EVALUATED_PASS. The detector built to stop "absence
# reads as success" was reporting an unfinished run as passed. And every
# `cancelled` was bucketed as EVALUATED_FAIL, which conflated supersession (a
# hole) with a genuine failure.
#
# THE cancelled SPLIT IS EVIDENCE-BASED, not a guess. Measured 2026-08-10:
#   dev-hermit 31349210392  cancelled, total_jobs=0   -> superseded; the newer
#                           push that took the group is visible one second
#                           before the cancellation
#   hermit     31352719284  cancelled, total_jobs=36 with jobs started and
#                           succeeded -> NOT supersession; its workflow is
#                           cancel-in-progress:false, so this was an external
#                           kill of an in-progress run
# So: cancelled with zero jobs is a hole; cancelled with jobs is a bad answer.
#
# The fresh/stale split follows github_main_health.classify_current_runs and is
# deliberate: alarming on every pending run fires after every push and is an
# alarm no coordinator can satisfy -- the anti-pattern already removed from
# other gates. A fresh pending is silent; only one that outlives the window is
# a finding.
NEVER_RAN = "NEVER_RAN"
NOT_EVALUATED = NEVER_RAN  # backward-compatible alias for existing consumers
SUPERSEDED = "SUPERSEDED"            # cancelled AND a successor is visible
CANCELLED_UNVERIFIED = "CANCELLED_UNVERIFIED"  # cancelled, cause NOT established
PENDING = "PENDING"
EVALUATED_PASS = "EVALUATED_PASS"
EVALUATED_FAIL = "EVALUATED_FAIL"

DEFAULT_PENDING_STALE_SECS = 900

OWNER_DISABLED_ACTIONS = {
    "rrnewton/hermit": "deactivate-gh-actions-on-hermit-main-and-dev-hermit-parent",
    "rrnewton/dev-hermit": "deactivate-gh-actions-on-hermit-main-and-dev-hermit-parent",
}

# Conclusions that are a genuine negative VERDICT. `cancelled` is deliberately
# NOT here: it is split by whether any job ran (see above).
FAILING = {"failure", "timed_out", "startup_failure", "action_required"}


class GitHubUnavailable(RuntimeError):
    """The live GitHub census did not complete; no domain verdict exists."""


_GH_TOKEN: str | None = None


def _resolve(fn, run):
    """Call an optional resolver, returning None on ANY failure.

    None means UNKNOWN and must never be promoted into a causal claim.
    """
    if fn is None:
        return None
    try:
        return fn(run)
    except Exception:
        return None


def _age_secs(run: dict[str, Any], now: float | None) -> float | None:
    created = (run.get("created_at") or "").replace("Z", "+00:00")
    if not created or now is None:
        return None
    try:
        return now - datetime.fromisoformat(created).timestamp()
    except ValueError:
        return None


def classify(
    runs: list[dict[str, Any]],
    now: float | None = None,
    jobs_of=None,
    successor_of=None,
    stale_after: int = DEFAULT_PENDING_STALE_SECS,
) -> tuple[str, bool, dict[str, Any]]:
    """Pure classifier: the seam the bracket drives. Returns (state, stale).

    `jobs_of(run) -> int` resolves how many jobs a run created; it is only
    consulted for `cancelled` runs, so the cost is bounded and rare. When it is
    unavailable the cancel is treated as SUPERSEDED-unknown rather than silently
    promoted to a verdict -- refusing to answer beats guessing a good answer.
    """
    if not runs:
        return NEVER_RAN, False, {}

    for run in runs:
        if (run.get("conclusion") or "").strip() in FAILING:
            return EVALUATED_FAIL, False, {}

    pending = [r for r in runs if not (r.get("conclusion") or "").strip()]
    if pending:
        oldest = max((_age_secs(r, now) or 0.0) for r in pending)
        return PENDING, oldest > stale_after, {}

    for run in runs:
        if (run.get("conclusion") or "").strip() != "cancelled":
            continue
        # A cancel that killed RUNNING jobs is a RESULT (a hang, or an external
        # kill), not a hole. Measured: hermit 31352719284, cancelled with 36 jobs
        # started, under a cancel-in-progress:false workflow.
        jobs = _resolve(jobs_of, run)
        if jobs is not None and jobs > 0:
            return EVALUATED_FAIL, False, {"jobs": jobs}

        # *** SUPERSESSION IS ASSERTED ONLY ON VISIBLE SUCCESSOR EVIDENCE. ***
        # The previous version returned SUPERSEDED whenever jobs==0 OR the jobs
        # query FAILED, while the reason text claimed "superseded by a newer
        # push". That reintroduced absence-as-verdict one layer down: it moved
        # the defect from the STATE to the CAUSE. Two concrete false positives it
        # admitted -- zero jobs also covers a MANUAL or PRE-START cancellation,
        # and a FAILED QUERY became a causal claim about the world.
        successor = _resolve(successor_of, run)
        if successor:
            return SUPERSEDED, False, {"successor": successor, "jobs": jobs}
        return CANCELLED_UNVERIFIED, False, {
            "jobs": jobs,
            "why_unverified": ("job data unavailable" if jobs is None
                               else "zero jobs, but no successor run is visible"),
        }

    return EVALUATED_PASS, False, {}


def likely_reason(pr: dict[str, Any], state: str, stale: bool,
                  ev: dict[str, Any] | None = None,
                  actions_enabled: bool = True,
                  actions_disabled_directive: str | None = None) -> str:
    """Named state -> cause -> remedy, with the CAUSE GRADED BY EVIDENCE.

    Shape copied from github_main_health's `stale-pending` emitter. The grading
    is the correction: a cause may only be stated as fact when it was observed.
    """
    ev = ev or {}
    if not actions_enabled and state == NEVER_RAN:
        if actions_disabled_directive:
            explanation = (
                "no run was created; GitHub Actions are deactivated by owner directive "
                f"{actions_disabled_directive}, so NEVER_RAN is EXPECTED and is neither "
                "a pass nor a failure"
            )
        else:
            explanation = (
                "no run was created and GitHub Actions are disabled, but no standing "
                "owner directive was found to establish that this absence is expected"
            )
        if (pr.get("mergeable") or "").upper() == "CONFLICTING":
            return (
                explanation
                + ". Independently, the PR conflicts with its base. "
                "REMEDY: resolve the conflict"
            )
        return explanation

    if not actions_enabled and state == SUPERSEDED:
        return (
            f"cancelled before any job started, and a successor run IS VISIBLE "
            f"({ev.get('successor')}), so supersession is OBSERVED here rather than "
            "inferred. GitHub Actions are currently deactivated; no Actions dispatch "
            "is permitted"
        )
    if not actions_enabled and state == CANCELLED_UNVERIFIED:
        return (
            "the run was cancelled and produced no verdict, and THE CAUSE IS NOT "
            f"ESTABLISHED ({ev.get('why_unverified')}). GitHub Actions are currently "
            "deactivated; no Actions dispatch is permitted"
        )
    if not actions_enabled and state == PENDING and stale:
        return (
            "a run has not resolved within the stale window. GitHub Actions are "
            "currently deactivated; inspect the exact run because no Actions dispatch "
            "is permitted"
        )

    if state == NEVER_RAN:
        if (pr.get("mergeable") or "").upper() == "CONFLICTING":
            return ("no run was ever created; the PR conflicts with its base so GitHub cannot "
                    "build refs/pull/N/merge. REMEDY: resolve the conflict; a dispatch also works")
        return ("no run was ever created and the cause is NOT established for a mergeable PR. "
                "REMEDY: dispatch the gate at this head -- proven to work on #1918 -- and report "
                "that triggering, not the workflow, is what failed")
    if state == SUPERSEDED:
        return (f"THIS IS A HOLE, NOT A GREEN: cancelled before any job started, and a successor "
                f"run IS VISIBLE ({ev.get('successor')}), so supersession is OBSERVED here rather "
                f"than inferred. Superseding an obsolete run is CORRECT. "
                f"REMEDY: re-dispatch the authority for the current head")
    if state == CANCELLED_UNVERIFIED:
        return ("THIS IS A HOLE, NOT A GREEN: the run was cancelled and produced no verdict, and "
                f"THE CAUSE IS NOT ESTABLISHED ({ev.get('why_unverified')}). It may have been "
                "superseded by a newer push, cancelled manually, or cancelled before start -- "
                "this tool has not seen which, and will not guess. "
                "REMEDY: re-dispatch the authority for the current head; if the cause matters, "
                "look for a successor run in the same concurrency group")
    if state == PENDING and stale:
        return ("THIS IS A HOLE, NOT A GREEN: a run has not resolved within the stale window. "
                "REMEDY: re-dispatch the authority for the current head")
    return ""


def gh_json(args: list[str]) -> Any:
    try:
        out = subprocess.run(
            ["with-proxy", "gh", *args], capture_output=True, text=True, timeout=300
        )
    except subprocess.TimeoutExpired as error:
        raise GitHubUnavailable(f"gh {' '.join(args)} timed out") from error
    if out.returncode != 0:
        raise GitHubUnavailable(
            f"gh {' '.join(args)} failed: {out.stderr.strip()[:300]}"
        )
    try:
        return json.loads(out.stdout or "null")
    except json.JSONDecodeError as error:
        raise GitHubUnavailable(
            f"gh {' '.join(args)} returned malformed JSON"
        ) from error


def _curl_config_value(value: str) -> str:
    """Quote one curl config value without placing credentials in argv."""
    return json.dumps(value)


def gh_json_batch(
    requests: dict[str, str], deadline: float | None = None
) -> dict[str, Any]:
    """Fetch independent GitHub REST paths in one parallel curl process.

    The old live reader launched one ``gh`` process per PR. Its wall time and
    process count therefore grew with ``--limit`` and eventually exceeded the
    tick's fixed 30-second bound. curl's parallel transfer mode preserves every
    exact REST request but multiplexes them through one process.
    """
    if not requests:
        return {}
    def remaining(cap: float) -> float:
        if deadline is None:
            return cap
        left = deadline - time.monotonic()
        if left <= 0.5:
            raise GitHubUnavailable(
                "live collection exhausted its 27-second internal budget"
            )
        return min(cap, left)

    global _GH_TOKEN
    if _GH_TOKEN is None:
        try:
            auth = subprocess.run(
                ["gh", "auth", "token"],
                capture_output=True,
                text=True,
                timeout=remaining(10),
            )
        except subprocess.TimeoutExpired as error:
            raise GitHubUnavailable("gh auth token timed out") from error
        token = auth.stdout.strip()
        if auth.returncode != 0 or not token:
            raise GitHubUnavailable(
                f"gh auth token failed: {auth.stderr.strip()[:300]}"
            )
        _GH_TOKEN = token
    token = _GH_TOKEN

    with tempfile.TemporaryDirectory(prefix="unevaluated-head-") as directory:
        paths: dict[str, str] = {}
        config = [
            "silent",
            "show-error",
            "fail-with-body",
            "fail-early",
            "compressed",
            "parallel",
            "parallel-immediate",
            "parallel-max = 16",
            "header = " + _curl_config_value("Accept: application/vnd.github+json"),
            "header = " + _curl_config_value(f"Authorization: Bearer {token}"),
            "header = " + _curl_config_value("X-GitHub-Api-Version: 2022-11-28"),
        ]
        for index, (key, api_path) in enumerate(requests.items()):
            if not api_path.startswith("repos/"):
                raise ValueError(f"GitHub batch path must start with repos/: {api_path}")
            output = os.path.join(directory, f"{index}.json")
            paths[key] = output
            config.append("url = " + _curl_config_value(f"https://api.github.com/{api_path}"))
            config.append("output = " + _curl_config_value(output))
        try:
            out = subprocess.run(
                ["with-proxy", "curl", "--config", "-"],
                input="\n".join(config) + "\n",
                capture_output=True,
                text=True,
                timeout=remaining(25),
            )
        except subprocess.TimeoutExpired as error:
            raise GitHubUnavailable(
                f"batched GitHub request timed out for {len(requests)} exact endpoint(s)"
            ) from error
        if out.returncode != 0:
            raise GitHubUnavailable(
                f"batched GitHub request failed for {len(requests)} exact endpoint(s): "
                f"{out.stderr.strip()[:300]}"
            )
        result: dict[str, Any] = {}
        for key, path in paths.items():
            try:
                with open(path, encoding="utf-8") as handle:
                    result[key] = json.load(handle)
            except (OSError, json.JSONDecodeError) as error:
                raise GitHubUnavailable(
                    f"batched GitHub response for {key} was absent or malformed"
                ) from error
        return result


def actions_policy(repo: str) -> tuple[bool, str | None]:
    """Read live Actions state; bind expected absence only to a known directive."""
    payload = gh_json(["api", f"repos/{repo}/actions/permissions"]) or {}
    enabled = payload.get("enabled")
    if type(enabled) is not bool:
        raise GitHubUnavailable(
            f"repos/{repo}/actions/permissions did not return a boolean enabled field"
        )
    directive = OWNER_DISABLED_ACTIONS.get(repo) if not enabled else None
    return enabled, directive


def canonical_head_sha(
    repo: str, pr: dict[str, Any], live: dict[str, Any] | None = None
) -> str:
    """Return a full exact PR-head SHA, resolving abbreviated reader output.

    The Actions runs endpoint treats ``head_sha`` as an exact 40-hex filter: a
    displayed 12-hex prefix returns an empty list rather than an error. Empty is
    therefore not trustworthy until the input has been canonicalised.
    """
    claimed = str(pr.get("headRefOid") or "").strip().lower()
    if re.fullmatch(r"[0-9a-f]{40}", claimed) and live is None:
        return claimed

    live = live or gh_json(["api", f"repos/{repo}/pulls/{pr['number']}"]) or {}
    resolved = str(((live.get("head") or {}).get("sha") or "")).strip().lower()
    if not re.fullmatch(r"[0-9a-f]{40}", resolved):
        raise GitHubUnavailable(
            f"PR #{pr['number']} head is not a full SHA: "
            f"listed={claimed or 'missing'} resolved={resolved or 'missing'}"
        )
    if claimed and not resolved.startswith(claimed):
        raise GitHubUnavailable(
            f"PR #{pr['number']} head changed while collecting: "
            f"listed={claimed} resolved={resolved}"
        )
    return resolved


def collect_live(repo: str, limit: int,
                 actions_enabled: bool | None = None,
                 actions_disabled_directive: str | None = None) -> list[dict[str, Any]]:
    deadline = time.monotonic() + 27.0
    if limit < 1:
        return []
    pages = (limit + 99) // 100
    initial_requests = {
        f"open-prs:{page}":
            f"repos/{repo}/pulls?state=open&sort=created&direction=desc&per_page=100&page={page}"
        for page in range(1, pages + 1)
    }
    if actions_enabled is None:
        initial_requests["actions-policy"] = f"repos/{repo}/actions/permissions"
    initial = gh_json_batch(initial_requests, deadline)
    if actions_enabled is None:
        enabled = (initial.get("actions-policy") or {}).get("enabled")
        if type(enabled) is not bool:
            raise GitHubUnavailable(
                f"repos/{repo}/actions/permissions did not return a boolean enabled field"
            )
        actions_enabled = enabled
        actions_disabled_directive = OWNER_DISABLED_ACTIONS.get(repo) if not enabled else None
    raw_prs = [
        item
        for page in range(1, pages + 1)
        for item in (initial.get(f"open-prs:{page}") or [])
    ][:limit]
    prs = [
        {
            "number": pr.get("number"),
            "headRefOid": ((pr.get("head") or {}).get("sha")),
            "mergeable": "UNKNOWN",
            "isDraft": pr.get("draft"),
            "title": pr.get("title"),
        }
        for pr in raw_prs
    ]
    if any(not isinstance(pr.get("number"), int) for pr in prs):
        raise GitHubUnavailable("open-PR REST census returned a row without a number")
    # ONE BATCHED REST PROCESS FOR ALL PR HEADS, NOT ONE `gh` PROCESS PER PR.
    #
    # Measured 2026-08-10: serially this was 18.11s wall for --limit 40 with only
    # 4.24s of user CPU and 57,446 context switches -- the signature of 41 short
    # subprocesses waiting on the network one at a time, not of heavy work. The
    # later 8-way pool reduced ordinary wall time but retained a variable number
    # of processes and still crossed the 30-second tick bound under contention.
    #
    # *** THIS SKIPS NOTHING. *** Same calls, same population, same per-head SHA
    # matching -- the requests are independent and are merely multiplexed. An
    # optimisation that decided some heads were "probably unchanged" and skipped
    # them would be the correct-but-unscheduled defect wearing a performance hat,
    # and is exactly what is NOT done here. If a query fails it raises, rather
    # than being treated as "no runs" -- absence of an answer must never read as
    # absence of runs.
    abbreviated = [
        pr for pr in prs
        if not re.fullmatch(r"[0-9a-f]{40}", str(pr.get("headRefOid") or ""))
    ]
    abbreviated_details = gh_json_batch({
        f"pull:{pr['number']}": f"repos/{repo}/pulls/{pr['number']}"
        for pr in abbreviated
    }, deadline)
    heads: dict[int, str] = {
        pr["number"]: canonical_head_sha(
            repo,
            pr,
            abbreviated_details.get(f"pull:{pr['number']}")
            if pr in abbreviated else None,
        )
        for pr in prs
    }
    run_payloads = gh_json_batch({
        f"runs:{number}":
            f"repos/{repo}/actions/runs?head_sha={head}&per_page=100"
        for number, head in heads.items()
    }, deadline)
    runs_by_pr = {
        pr["number"]: (run_payloads.get(f"runs:{pr['number']}") or {}).get(
            "workflow_runs", []
        )
        for pr in prs
    }

    # Two independent readers must agree on absence. Only heads whose exact
    # workflow-run query was empty need the exact-commit check-run reader.
    empty_prs = [pr for pr in prs if not runs_by_pr[pr["number"]]]
    check_payloads = gh_json_batch({
        f"checks:{pr['number']}":
            f"repos/{repo}/commits/{heads[pr['number']]}/check-runs?per_page=100&filter=all"
        for pr in empty_prs
    }, deadline)
    checks_by_pr: dict[int, list[dict[str, Any]]] = {}
    for pr in empty_prs:
        checks = [
            check
            for check in (check_payloads.get(f"checks:{pr['number']}") or {}).get(
                "check_runs", []
            )
            if ((check.get("app") or {}).get("slug") == "github-actions")
        ]
        for check in checks:
            check.setdefault("created_at", check.get("started_at"))
        checks_by_pr[pr["number"]] = checks

    # Mergeability is causal evidence only for a genuinely empty exact head.
    # Do not pay forty detail requests merely to decorate rows that already
    # carry a run verdict. Empty heads are re-read here so a moved head is never
    # described using stale conflict evidence.
    no_evidence_prs = [
        pr for pr in empty_prs if not checks_by_pr.get(pr["number"])
    ]
    mergeability_error = ""
    try:
        no_evidence_details = gh_json_batch({
            f"pull:{pr['number']}": f"repos/{repo}/pulls/{pr['number']}"
            for pr in no_evidence_prs
        }, min(deadline, time.monotonic() + 5.0))
    except GitHubUnavailable as error:
        # Mergeability explains some NEVER_RAN rows but does not decide whether
        # an exact-head run/check exists. Preserve the domain verdict, state the
        # cause as UNKNOWN, and do not let optional causal enrichment erase the
        # two exact readers' completed answer.
        no_evidence_details = {}
        mergeability_error = " ".join(str(error).split())
    for pr in no_evidence_prs:
        detail = no_evidence_details.get(f"pull:{pr['number']}") or {}
        if detail:
            canonical_head_sha(repo, pr, detail)
        mergeable = detail.get("mergeable")
        pr["mergeable"] = (
            "MERGEABLE" if mergeable is True
            else "CONFLICTING" if mergeable is False
            else "UNKNOWN"
        )
        if mergeability_error:
            pr["mergeability_error"] = mergeability_error

    cancelled = [
        run
        for runs in runs_by_pr.values()
        for run in runs
        if (run.get("conclusion") or "").strip() == "cancelled"
    ]
    cancelled_payloads = gh_json_batch({
        **{
            f"jobs:{run['id']}": f"repos/{repo}/actions/runs/{run['id']}/jobs"
            for run in cancelled
        },
        **{
            f"branch:{run['id']}":
                f"repos/{repo}/actions/runs?branch={quote(str(run.get('head_branch') or ''), safe='')}&per_page=20"
            for run in cancelled
        },
    }, deadline)

    rows: list[dict[str, Any]] = []
    for pr in prs:
        head = heads[pr["number"]]
        runs = runs_by_pr[pr["number"]]
        check_runs = checks_by_pr.get(pr["number"], [])

        def jobs_of(run: dict[str, Any]) -> int:
            return int((cancelled_payloads.get(f"jobs:{run['id']}") or {}).get(
                "total_count", 0
            ))

        row = {
            "number": pr["number"],
            "head": head,
            "mergeable": pr.get("mergeable"),
            "draft": pr.get("isDraft"),
            "run_count": len(runs),
            "check_run_count": len(check_runs),
            "actions_enabled": actions_enabled,
            "actions_disabled_directive": actions_disabled_directive,
            "mergeability_error": pr.get("mergeability_error"),
        }
        def successor_of(run: dict[str, Any]):
            """Visible successor evidence: a LATER run of the SAME workflow on the
            SAME branch. Absence of evidence is returned as None, never as a
            cause."""
            same = (cancelled_payloads.get(f"branch:{run['id']}") or {}).get(
                "workflow_runs", []
            )
            for other in same:
                if (other.get("workflow_id") == run.get("workflow_id")
                        and other.get("id") != run.get("id")
                        and (other.get("created_at") or "") > (run.get("created_at") or "")):
                    return f"run {other.get('id')} created {other.get('created_at')}"
            return None

        if runs:
            st, stale, ev = classify(runs, now=time.time(), jobs_of=jobs_of,
                                     successor_of=successor_of)
        else:
            # Check-runs expose the status/conclusion/created_at fields consumed
            # by the pure classifier. Cancellation remains UNVERIFIED because
            # this fallback has no workflow-successor evidence.
            st, stale, ev = classify(check_runs, now=time.time())
            if check_runs:
                ev = {
                    **ev,
                    "reader": "exact-commit-check-runs",
                    "workflow_runs_endpoint_count": 0,
                    "github_actions_check_runs": len(check_runs),
                }
        row["state"], row["stale"], row["evidence"] = st, stale, ev
        row["reason"] = likely_reason(
            pr, st, stale, ev, actions_enabled, actions_disabled_directive
        )
        rows.append(row)
    # Deterministic order regardless of completion order, so tick output is
    # stable and diffable across cycles.
    rows.sort(key=lambda r: r["number"], reverse=True)
    return rows


def collect_fixture(path: str) -> list[dict[str, Any]]:
    data = json.load(open(path))
    if isinstance(data, list):
        prs = data
        actions_enabled = True
        actions_disabled_directive = None
    elif isinstance(data, dict):
        prs = data.get("prs")
        actions_enabled = data.get("actions_enabled")
        actions_disabled_directive = data.get("actions_disabled_directive")
        if not isinstance(prs, list) or type(actions_enabled) is not bool:
            raise RuntimeError(
                "fixture object requires prs list and boolean actions_enabled"
            )
    else:
        raise RuntimeError("fixture must be a PR list or a repository-state object")
    rows = []
    for pr in prs:
        runs = pr.get("runs", [])
        row = {
            "number": pr["number"],
            "head": pr["head"],
            "mergeable": pr.get("mergeable"),
            "draft": pr.get("draft", False),
            "run_count": len(runs),
            "actions_enabled": actions_enabled,
            "actions_disabled_directive": actions_disabled_directive,
        }
        st, stale, ev = classify(runs, now=pr.get("now"), jobs_of=pr.get("jobs_of"),
                                 successor_of=pr.get("successor_of"))
        row["state"], row["stale"], row["evidence"] = st, stale, ev
        row["reason"] = likely_reason(
            pr, st, stale, ev, actions_enabled, actions_disabled_directive
        )
        rows.append(row)
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", default="rrnewton/hermit")
    ap.add_argument("--limit", type=int, default=40)
    ap.add_argument("--from-json", dest="fixture")
    ap.add_argument("--gate", action="store_true")
    ap.add_argument("--json", dest="as_json", action="store_true")
    a = ap.parse_args()

    try:
        rows = collect_fixture(a.fixture) if a.fixture else collect_live(a.repo, a.limit)
    except GitHubUnavailable as error:
        summary = (
            "open-PR exact-head evaluation could not be determined: "
            + " ".join(str(error).split()).replace("{", "(").replace("}", ")")
        )
        if a.gate and not a.as_json:
            print("state=unavailable")
            print(f"summary={summary}")
        elif a.as_json:
            print(json.dumps({"state": "unavailable", "summary": summary}))
        print(f"NO RESULT: {summary}", file=sys.stderr)
        return 75 if a.gate else 1
    # NO VERDICT: a fresh PENDING is deliberately excluded --
    # alarming on it fires after every push and is an alarm no coordinator can
    # satisfy. EVALUATED_FAIL is deliberately not here either: a red gate is a
    # RESULT and another check's job.
    no_verdict = [r for r in rows
                  if r["state"] in (NEVER_RAN, SUPERSEDED, CANCELLED_UNVERIFIED)
                  or (r["state"] == PENDING and r.get("stale"))]
    expected_no_verdict = [
        r for r in no_verdict
        if (r["state"] == NEVER_RAN
            and r.get("actions_enabled") is False
            and r.get("actions_disabled_directive"))
    ]
    holes = [r for r in no_verdict if r not in expected_no_verdict]
    unevaluated = no_verdict
    mergeability_unavailable = sum(bool(r.get("mergeability_error")) for r in rows)

    # tick-hub capture consumes key=value fields. Keep the human table below,
    # but also give the scheduled emitter a fully rendered one-line summary so
    # a finding can never degrade into a literal ``{summary}`` placeholder.
    affected_prs = ",".join(f"#{r['number']}" for r in no_verdict[:8]) or "none"
    if holes:
        remedies = []
        if any(r.get("actions_enabled") is True for r in holes):
            remedies.append("dispatch the authority only where Actions are enabled")
        if any((r.get("mergeable") or "").upper() == "CONFLICTING" for r in holes):
            remedies.append("resolve base conflicts where named")
        if any(r.get("actions_enabled") is False for r in holes):
            remedies.append(
                "inspect the unresolved run or verify why Actions are disabled; no "
                "Actions dispatch is permitted while disabled"
            )
        tick_summary = (
            f"{len(no_verdict)}/{len(rows)} inspected open PR heads carry NO VERDICT; "
            f"affected={affected_prs}; actionable holes={len(holes)}; "
            f"expected under owner directive={len(expected_no_verdict)}; this is a "
            f"hole, not green; remedy: {'; '.join(remedies)}"
        )
    elif expected_no_verdict:
        tick_summary = (
            f"{len(no_verdict)}/{len(rows)} inspected open PR heads carry NO VERDICT; "
            f"affected={affected_prs}; NEVER_RAN is expected because GitHub Actions "
            "are deactivated by owner directive; no Actions dispatch is permitted; "
            "absence is neither pass nor failure"
        )
    else:
        tick_summary = (
            f"0/{len(rows)} inspected open PR heads carry NO VERDICT; affected=none; "
            "every inspected head carries a verdict"
        )

    if a.as_json:
        print(json.dumps({"inspected": len(rows), "not_evaluated": len(unevaluated),
                          "actionable_not_evaluated": len(holes),
                          "expected_not_evaluated": len(expected_no_verdict),
                          "rows": rows}, indent=2))
    else:
        # Both terms, always: a bare count of unevaluated heads is not a
        # measurement without the population it came from.
        import collections as _c
        tally = _c.Counter(r["state"] for r in rows)
        print(f"inspected {len(rows)} open PR head(s); NO VERDICT: {len(no_verdict)}  "
              + "  ".join(f"{k}={v}" for k, v in sorted(tally.items())))
        for r in rows:
            if r in no_verdict:
                if r in expected_no_verdict:
                    label = r["state"] + "(expected)"
                else:
                    label = r["state"] + ("(stale)" if r.get("stale") else "")
                print(
                    f"  {label:<16} PR {r['number']}  head {r['head'][:12]}  "
                    f"runs={r['run_count']}  checks={r.get('check_run_count', 0)}  "
                    f"mergeable={r['mergeable']}  -- {r['reason']}"
                )
        if not no_verdict:
            print("  every inspected head carries a verdict")

        if a.gate:
            state = "hole" if holes else ("expected" if expected_no_verdict else "ok")
            print(f"state={state}")
            print(f"inspected={len(rows)}")
            print(f"not_evaluated={len(no_verdict)}")
            print(f"actionable_not_evaluated={len(holes)}")
            print(f"expected_not_evaluated={len(expected_no_verdict)}")
            print(f"mergeability_unavailable={mergeability_unavailable}")
            print(f"summary={tick_summary}")

    if a.gate and holes:
        print(
            f"GATE FAIL: {len(holes)} of {len(rows)} open PR head(s) carry NO VERDICT "
            "(never ran, superseded, or pending past the stale window). None of these is "
            "passing, and only one of the five states is good news.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
