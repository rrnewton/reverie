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
The gate is normally silent about EVALUATED_FAIL because a red gate is another
check's job. The exception is a failed run observed while the local flag says
Actions are disabled: that is a configuration mismatch, so this gate reports
both the real failure and the wrong flag. A FRESH pending remains non-actionable
because alarming on every push is an alarm no coordinator can satisfy.

A CAUSE IS ONLY EVER STATED AS FACT WHEN IT WAS OBSERVED. `SUPERSEDED` requires
a visible successor run; everything else cancelled is `CANCELLED_UNVERIFIED`
with the cause explicitly not established. An earlier version inferred
supersession from zero jobs OR from a FAILED jobs query, which turned missing
data into a causal claim -- the same absence-as-verdict defect one layer down,
in the CAUSE rather than the STATE.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import subprocess
import tempfile
import time
import sys
from datetime import datetime
from pathlib import Path
from typing import Any
from urllib.parse import quote

HEALTH_DIR = os.path.dirname(os.path.abspath(__file__))
if HEALTH_DIR not in sys.path:
    sys.path.insert(0, HEALTH_DIR)
LIB_DIR = str(Path(HEALTH_DIR).parent / "lib")
if LIB_DIR not in sys.path:
    sys.path.insert(0, LIB_DIR)

from manual_run import automatic_triggers, triggers_of  # noqa: E402
import ops_state  # noqa: E402

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

# Conclusions that are a genuine negative VERDICT. `cancelled` is deliberately
# NOT here: it is split by whether any job ran (see above).
FAILING = {"failure", "timed_out", "startup_failure", "action_required"}


class GitHubUnavailable(RuntimeError):
    """The live GitHub census did not complete; no domain verdict exists."""


_GH_TOKEN: str | None = None
_WORKFLOW_TRIGGERS_UNSET = object()


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
            # The EVENT is carried because existence alone cannot say whether a
            # run contradicts a "no automatic runs" directive. See
            # DELIBERATE_RUN_EVENTS.
            return EVALUATED_FAIL, False, {"event": (run.get("event") or "").strip()}

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
            return EVALUATED_FAIL, False, {
                "jobs": jobs, "event": (run.get("event") or "").strip()
            }

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


#: Events that mean somebody deliberately asked for this run rather than a
#: trigger firing on its own.
#:
#: ⚠️ WHY THIS EXISTS. The gate used to conclude, from the mere EXISTENCE of a run
#: while .ops-state.yaml says github_actions_enabled=false, that "the run proves
#: the local flag is wrong". Measured 2026-09-05 on the one EVALUATED_FAIL among
#: 40 inspected heads -- rrnewton/hermit PR 2904 at 226a6506daf9, run
#: 33758411902 of .github/workflows/merge-gate.yml:
#:
#:     event: workflow_dispatch, actor rrnewton, conclusion failure
#:
#: A human pressed the button, and merge-gate.yml's trigger block is
#: `workflow_dispatch` alone so it has no other way to start. That contradicts
#: nothing: hermit/scripts/check-github-actions-triggers.rs states the policy the
#: flag is shorthand for, and it says "every workflow may remain manually
#: dispatchable". GitHub-level Actions are in fact enabled=true; what is
#: deactivated is AUTOMATIC evaluation of pull-request heads.
#:
#: This gate fetches the whole run object, which carries `event`, and never read
#: it -- so a run a human asked for was indistinguishable from one a trigger
#: created, and only the second contradicts the directive.
#:
#: ⚠️ NARROW ON PURPOSE, AND IT FAILS TOWARDS STILL REPORTING. Only these two are
#: treated as deliberate. `workflow_run` is excluded because a chained run may
#: descend from an automatic push, and an unknown or absent event keeps the
#: original wording. Being wrong here costs a contradiction reported that is not
#: there; the opposite would cost a real one going unsaid.
DELIBERATE_RUN_EVENTS = frozenset({"workflow_dispatch", "repository_dispatch"})


def likely_reason(pr: dict[str, Any], state: str, stale: bool,
                  ev: dict[str, Any] | None = None,
                  actions_enabled: bool = True,
                  actions_disabled_directive: str | None = None,
                  workflow_triggers: dict[str, Any] | None = None) -> str:
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
    if not actions_enabled and state == EVALUATED_FAIL:
        event = str(ev.get("event") or "")
        if event in DELIBERATE_RUN_EVENTS:
            return (
                f"a GitHub Actions run failed. It was started by {event}, so somebody "
                "asked for it: that is permitted while .ops-state.yaml says "
                "github_actions_enabled=false, which deactivates AUTOMATIC evaluation "
                "and not manual dispatch. The failure is real and stands; the flag is "
                "NOT contradicted by this run"
            )
        described = event or "an event this gate could not read"
        return (
            "a GitHub Actions run exists and failed while .ops-state.yaml says "
            f"github_actions_enabled=false, started by {described} rather than a "
            "deliberate dispatch. The observed failure is still real, and the run "
            "proves the local flag is wrong"
        )

    if state == NEVER_RAN:
        if (pr.get("mergeable") or "").upper() == "CONFLICTING":
            return ("no run was ever created; the PR conflicts with its base so GitHub cannot "
                    "build refs/pull/N/merge. REMEDY: resolve the conflict; a dispatch also works")
        if workflow_triggers and workflow_triggers.get("error"):
            return (
                "no run was ever created; whether the current workflow configuration "
                "provides an automatic trigger is NOT DETERMINABLE "
                f"({workflow_triggers['error']}). This remains a hole, not green. "
                "REMEDY: inspect the workflow configuration, then dispatch the gate at "
                "this head"
            )
        if workflow_triggers and workflow_triggers.get("manual_only") is True:
            count = workflow_triggers.get("active_workflows", 0)
            return (
                f"no run was ever created; the current workflow configuration is "
                f"manual-only: all {count} active workflow(s) declare workflow_dispatch "
                "and no automatic trigger. This remains a hole, not green. REMEDY: "
                "dispatch the gate at this head"
            )
        if workflow_triggers and workflow_triggers.get("no_active_workflows") is True:
            return (
                "no run was ever created; the repository currently has no active "
                "workflow, so no automatic run can be created. This remains a hole, "
                "not green. REMEDY: inspect the workflow configuration"
            )
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
    """Read this host's Actions policy; bind disabled state to its source."""
    del repo
    try:
        return ops_state.github_actions_policy()
    except ops_state.OpsStateError as error:
        raise GitHubUnavailable(str(error)) from error


def _workflow_trigger_evidence(
    registry: dict[str, Any], directory: list[dict[str, Any]],
    contents: dict[str, Any]
) -> dict[str, Any]:
    """Describe current active-workflow triggers without guessing through gaps."""
    workflows = registry.get("workflows")
    if not isinstance(workflows, list):
        raise ValueError("workflow registry did not return a workflows list")
    if any(not isinstance(workflow, dict) for workflow in workflows):
        raise ValueError("workflow registry contained a non-object row")
    total = registry.get("total_count")
    if isinstance(total, int) and total > len(workflows):
        raise ValueError(
            f"workflow registry was truncated ({len(workflows)} of {total})"
        )

    by_path = {workflow.get("path"): workflow for workflow in workflows}
    files = [
        item for item in directory
        if isinstance(item, dict)
        and item.get("type") == "file"
        and str(item.get("path") or "").endswith((".yml", ".yaml"))
    ]
    active = []
    for item in files:
        path = item.get("path")
        registered = by_path.get(path)
        if not isinstance(registered, dict):
            raise ValueError(f"workflow registry had no row for {path}")
        state = registered.get("state")
        if state == "active":
            active.append(item)
        elif not isinstance(state, str) or not state.startswith("disabled"):
            raise ValueError(f"workflow state was not classifiable for {path}: {state}")
    if not active:
        return {
            "active_workflows": 0,
            "manual_only": False,
            "automatic_triggers": [],
            "no_active_workflows": True,
        }

    automatic: set[str] = set()
    for workflow in active:
        path = workflow.get("path")
        if not isinstance(path, str) or not path:
            raise ValueError("active workflow file lacked a path")
        payload = contents.get(f"workflow:{path}")
        if not isinstance(payload, dict):
            raise ValueError(f"workflow configuration was unavailable for {path}")
        if (payload.get("encoding") != "base64"
                or not isinstance(payload.get("content"), str)):
            raise ValueError(f"workflow configuration was not base64 content for {path}")
        try:
            encoded = "".join(payload["content"].split())
            text = base64.b64decode(encoded, validate=True).decode("utf-8")
        except (ValueError, UnicodeDecodeError) as error:
            raise ValueError(f"workflow configuration was unreadable for {path}") from error
        triggers = triggers_of(text)
        if not triggers:
            raise ValueError(f"workflow trigger configuration was unreadable for {path}")
        named_automatic = automatic_triggers(triggers)
        unclassified = sorted(
            trigger for trigger in triggers
            if trigger != "workflow_dispatch" and trigger not in named_automatic
        )
        if unclassified:
            raise ValueError(
                f"workflow trigger configuration was not classifiable for {path}: "
                + ",".join(unclassified)
            )
        automatic.update(named_automatic)

    return {
        "active_workflows": len(active),
        "manual_only": not automatic,
        "automatic_triggers": sorted(automatic),
        "no_active_workflows": False,
    }


def read_workflow_trigger_evidence(
    repo: str, deadline: float | None = None
) -> dict[str, Any]:
    """Read current workflow files; failure is evidence unavailable, not absence."""
    local_deadline = (
        time.monotonic() + 6.0 if deadline is None
        else min(deadline, time.monotonic() + 6.0)
    )
    try:
        inventory = gh_json_batch(
            {
                "workflow-registry": f"repos/{repo}/actions/workflows?per_page=100",
                "workflow-directory": f"repos/{repo}/contents/.github/workflows",
            },
            local_deadline,
        )
        registry_payload = inventory.get("workflow-registry")
        directory_payload = inventory.get("workflow-directory")
        if not isinstance(registry_payload, dict):
            raise ValueError("workflow registry response was not an object")
        if not isinstance(directory_payload, list):
            raise ValueError("workflow directory response was not a list")
        registry_by_path = {
            workflow.get("path"): workflow
            for workflow in registry_payload.get("workflows", [])
            if isinstance(workflow, dict)
        }
        active = [
            item for item in directory_payload
            if isinstance(item, dict)
            and item.get("type") == "file"
            and str(item.get("path") or "").endswith((".yml", ".yaml"))
            and (registry_by_path.get(item.get("path")) or {}).get("state") == "active"
        ]
        requests = {
            f"workflow:{workflow['path']}":
                f"repos/{repo}/contents/{quote(workflow['path'], safe='/')}"
            for workflow in active
            if isinstance(workflow.get("path"), str)
        }
        contents = gh_json_batch(requests, local_deadline) if requests else {}
        return _workflow_trigger_evidence(
            registry_payload, directory_payload, contents
        )
    except (GitHubUnavailable, ValueError, TypeError) as error:
        return {"error": " ".join(str(error).split())[:300]}


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
                 actions_disabled_directive: str | None = None,
                 workflow_triggers: dict[str, Any] | None | object =
                 _WORKFLOW_TRIGGERS_UNSET) -> list[dict[str, Any]]:
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
        actions_enabled, actions_disabled_directive = actions_policy(repo)
    initial = gh_json_batch(initial_requests, deadline)
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

    if workflow_triggers is _WORKFLOW_TRIGGERS_UNSET:
        workflow_triggers = (
            read_workflow_trigger_evidence(repo, deadline)
            if actions_enabled and no_evidence_prs else None
        )

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
            "workflow_triggers": workflow_triggers,
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
            pr, st, stale, ev, actions_enabled, actions_disabled_directive,
            workflow_triggers if isinstance(workflow_triggers, dict) else None,
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
        workflow_triggers = None
    elif isinstance(data, dict):
        prs = data.get("prs")
        actions_enabled = data.get("actions_enabled")
        actions_disabled_directive = data.get("actions_disabled_directive")
        workflow_triggers = data.get("workflow_triggers")
        if not isinstance(prs, list) or type(actions_enabled) is not bool:
            raise RuntimeError(
                "fixture object requires prs list and boolean actions_enabled"
            )
        if workflow_triggers is not None and not isinstance(workflow_triggers, dict):
            raise RuntimeError("fixture workflow_triggers must be an object")
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
            "workflow_triggers": workflow_triggers,
        }
        st, stale, ev = classify(runs, now=pr.get("now"), jobs_of=pr.get("jobs_of"),
                                 successor_of=pr.get("successor_of"))
        row["state"], row["stale"], row["evidence"] = st, stale, ev
        row["reason"] = likely_reason(
            pr, st, stale, ev, actions_enabled, actions_disabled_directive,
            workflow_triggers,
        )
        rows.append(row)
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
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
    # satisfy. EVALUATED_FAIL is deliberately not a NO VERDICT: it is a result.
    # A failed run while Actions are declared disabled is separately surfaced
    # below as a configuration mismatch rather than being hidden.
    no_verdict = [r for r in rows
                  if r["state"] in (NEVER_RAN, SUPERSEDED, CANCELLED_UNVERIFIED)
                  or (r["state"] == PENDING and r.get("stale"))]
    # ⚠️ SPLIT, BECAUSE A DISPATCHED RUN IS NOT A CONFIGURATION MISMATCH. Every
    # failing run under a false flag used to land in one bucket and be reported
    # as proof the flag was wrong. Measured 2026-09-05: the only such row in 40
    # inspected heads was PR 2904's run 33758411902, event workflow_dispatch --
    # a human pressing a button on a workflow whose trigger block is
    # `workflow_dispatch` alone. That is expressly permitted by the policy the
    # flag stands for, so it proved nothing. Both remain real failures and both
    # stay reported; only the contradiction claim is confined to the runs that
    # actually contradict something.
    _failing_while_disabled = [
        r for r in rows
        if r["state"] == EVALUATED_FAIL
        and r.get("actions_enabled") is False
    ]
    dispatched_failures = [
        r for r in _failing_while_disabled
        if str((r.get("evidence") or {}).get("event") or "") in DELIBERATE_RUN_EVENTS
    ]
    disabled_failures = [
        r for r in _failing_while_disabled if r not in dispatched_failures
    ]
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
    affected = holes + disabled_failures + dispatched_failures
    affected_prs = ",".join(f"#{r['number']}" for r in affected[:8]) or "none"
    if disabled_failures:
        additional_holes = (
            f"; plus {len(holes)} actionable no-verdict hole(s)"
            if holes else ""
        )
        tick_summary = (
            f"{len(disabled_failures)}/{len(rows)} inspected open PR heads have a "
            "failing GitHub Actions run while .ops-state.yaml says "
            "github_actions_enabled=false; affected="
            f"{affected_prs}; the observed run proves the flag is wrong, and the "
            f"failure remains a real failure{additional_holes}"
        )
    elif dispatched_failures:
        tick_summary = (
            f"{len(dispatched_failures)}/{len(rows)} inspected open PR heads have a "
            "failing GitHub Actions run that somebody dispatched by hand; affected="
            f"{affected_prs}; .ops-state.yaml says github_actions_enabled=false, which "
            "deactivates AUTOMATIC evaluation and not manual dispatch, so the flag is "
            "NOT contradicted; the failure remains a real failure"
        )
    elif holes:
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
        trigger_evidence = next(
            (r.get("workflow_triggers") for r in holes
             if isinstance(r.get("workflow_triggers"), dict)),
            None,
        )
        if trigger_evidence and trigger_evidence.get("error"):
            trigger_summary = (
                "workflow trigger configuration is NOT DETERMINABLE: "
                + trigger_evidence["error"]
            )
        elif trigger_evidence and trigger_evidence.get("manual_only") is True:
            trigger_summary = (
                f"current workflow configuration is manual-only across "
                f"{trigger_evidence.get('active_workflows', 0)} active workflow(s); "
                "no automatic trigger exists"
            )
        elif trigger_evidence and trigger_evidence.get("no_active_workflows") is True:
            trigger_summary = "the repository has no active workflow"
        elif trigger_evidence and trigger_evidence.get("automatic_triggers"):
            trigger_summary = (
                "current workflow configuration includes automatic trigger(s) "
                + ",".join(trigger_evidence["automatic_triggers"])
                + "; cause remains not established for mergeable rows"
            )
        else:
            trigger_summary = (
                "workflow trigger configuration was not inspected; cause remains "
                "not established for mergeable rows"
            )
        tick_summary = (
            f"{len(no_verdict)}/{len(rows)} inspected open PR heads carry NO VERDICT; "
            f"affected={affected_prs}; actionable holes={len(holes)}; "
            f"expected under owner directive={len(expected_no_verdict)}; "
            f"cause: {trigger_summary}; this is a "
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
                          "disabled_but_failed": len(disabled_failures),
                          "dispatched_but_failed": len(dispatched_failures),
                          "rows": rows}, indent=2))
    else:
        # Both terms, always: a bare count of unevaluated heads is not a
        # measurement without the population it came from.
        import collections as _c
        tally = _c.Counter(r["state"] for r in rows)
        print(f"inspected {len(rows)} open PR head(s); NO VERDICT: {len(no_verdict)}  "
              + "  ".join(f"{k}={v}" for k, v in sorted(tally.items())))
        for r in rows:
            if r in dispatched_failures:
                print(
                    f"  EVALUATED_FAIL(dispatched) PR {r['number']}  "
                    f"head {r['head'][:12]}  runs={r['run_count']}  "
                    f"checks={r.get('check_run_count', 0)}  mergeable={r['mergeable']}  "
                    f"-- {r['reason']}"
                )
            elif r in disabled_failures:
                print(
                    f"  EVALUATED_FAIL(config-mismatch) PR {r['number']}  "
                    f"head {r['head'][:12]}  runs={r['run_count']}  "
                    f"checks={r.get('check_run_count', 0)}  mergeable={r['mergeable']}  "
                    "-- a workflow ran and failed while .ops-state.yaml says "
                    "github_actions_enabled=false; the observed run proves the flag is wrong"
                )
            elif r in no_verdict:
                if r in expected_no_verdict:
                    label = r["state"] + "(expected)"
                else:
                    label = r["state"] + ("(stale)" if r.get("stale") else "")
                print(
                    f"  {label:<16} PR {r['number']}  head {r['head'][:12]}  "
                    f"runs={r['run_count']}  checks={r.get('check_run_count', 0)}  "
                    f"mergeable={r['mergeable']}  -- {r['reason']}"
                )
        if not no_verdict and not disabled_failures and not dispatched_failures:
            print("  every inspected head carries a verdict")

        if a.gate:
            state = (
                "configuration-mismatch"
                if disabled_failures
                else "hole" if holes
                else "expected" if expected_no_verdict
                else "ok"
            )
            print(f"state={state}")
            print(f"inspected={len(rows)}")
            print(f"not_evaluated={len(no_verdict)}")
            print(f"actionable_not_evaluated={len(holes)}")
            print(f"expected_not_evaluated={len(expected_no_verdict)}")
            print(f"disabled_but_failed={len(disabled_failures)}")
            print(f"dispatched_but_failed={len(dispatched_failures)}")
            print(f"mergeability_unavailable={mergeability_unavailable}")
            print(f"summary={tick_summary}")

    if a.gate and disabled_failures:
        print(
            f"GATE FAIL: {len(disabled_failures)} of {len(rows)} open PR head(s) have "
            "a failing GitHub Actions run while .ops-state.yaml says "
            "github_actions_enabled=false; the failure is real and the flag is wrong.",
            file=sys.stderr,
        )
        return 1
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
