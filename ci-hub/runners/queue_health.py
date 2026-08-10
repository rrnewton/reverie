#!/usr/bin/env python3
"""GitHub Actions queue-depth, wait-time, and last-green health for the Hermit repos.

Non-mutating. Answers four operational questions the bare `runner-health`
histogram could not, keeping *time in queue* strictly separate from *run
duration* (the hosted/self-hosted pools are queue-starved, so conflating the two
poisons every downstream analysis):

  1. QUEUE DEPTH per workflow      — how many runs are queued vs already running.
  2. WAIT TIMES                    — time-in-queue distribution, reported apart
                                     from run duration. Two independent bases:
                                       * current queue age  = now - run.createdAt
                                         for still-queued runs (a *lower bound* on
                                         each run's final wait; free from the run
                                         list);
                                       * historical time-in-queue = per-job
                                         (started_at - created_at) from the jobs
                                         API over a bounded sample of recent
                                         COMPLETED runs (the true runner-pickup
                                         latency; run-level `startedAt` equals
                                         `createdAt` and cannot be used).
  3. TIME SINCE LAST GREEN         — per workflow, both the elapsed wall time and
                                     HOW MANY RUNS BACK the last success was, so a
                                     green that is three pages deep surfaces here.
  4. SELF-HOSTED RUNNER HEALTH     — CONFIGURED vs LIVE (a registered-but-offline
                                     runner is silently-dead capacity), busy/idle,
                                     labels, and an eligibility-honest capacity
                                     signal that never promotes an inventory
                                     label into a workflow constraint.
  5. NAMED IN-FLIGHT RUNS          — "run X on branch Y", not "something is
                                     running".
  6. LAST-WINDOW RUN AGGREGATES    — started/completed/success/failure/cancelled
                                     over a fixed window, with MERGE-GATE runs
                                     counted SEPARATELY so gate churn never swamps
                                     the real test-failure count.
  7. SELF-HOSTED UTILIZATION       — busy-runner-time as a PERCENTAGE OF CAPACITY,
                                     and PEAK observed concurrent jobs vs the
                                     runner count (the "can we delete a runner?"
                                     signal). Both are computed over a fixed
                                     window from the bounded jobs-API sample, so a
                                     truncated sample yields a strict LOWER BOUND;
                                     each figure states the DIRECTION of its own
                                     error and never a bare percentage.

Every number is derived with its basis stated, per the workspace honesty rule.

This module is importable (used by `ci-status.py` for the human report and by
`ci-hub/health/operational_health.py queue-health` for the ops tick). `gh` is
invoked through `$GH` (default "with-proxy gh") so it works behind the devserver
proxy without touching the machine-global gh account.

Design note: this is the dev-hermit equivalent of the self-hosted+GitHub CI
"stats" reporting a sibling project surfaced via a Makefile target; that target
was never ported here (see runners/README.md). This ports the *capability* into
ci-hub, wired to the tick, rather than the target.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone

# --- Status vocabulary (GitHub Actions run/job status strings) ----------------
QUEUED_STATUSES = frozenset({"queued", "pending", "waiting", "requested"})
RUNNING_STATUSES = frozenset({"in_progress"})
INFLIGHT_STATUSES = QUEUED_STATUSES | RUNNING_STATUSES

# --- Tick thresholds (env-overridable) ---------------------------------------
# Each is a warn boundary for the fail-loud tick gate; overriding lets the
# coordinator tune sensitivity without a code change.
QUEUE_DEPTH_WARN = int(os.environ.get("CI_QUEUE_DEPTH_WARN", "10"))
QUEUE_AGE_WARN_SECS = int(os.environ.get("CI_QUEUE_AGE_WARN_SECS", str(30 * 60)))
GREEN_RUNS_BACK_WARN = int(os.environ.get("CI_GREEN_RUNS_BACK_WARN", "15"))
GREEN_AGE_WARN_SECS = int(os.environ.get("CI_GREEN_AGE_WARN_SECS", str(6 * 3600)))
# Only gate "no recent green" for workflows with at least this many runs in the
# window, so a rarely-triggered workflow does not flap the tick.
GREEN_GATE_MIN_RUNS = int(os.environ.get("CI_GREEN_GATE_MIN_RUNS", "4"))
# Comma-separated substrings of workflow names to exclude from the "no recent
# green" gate (e.g. an if:always aggregator that is structurally never "success").
# Default empty: nothing is silenced, so the current signal stays fully honest.
GREEN_GATE_EXCLUDE = [s.strip() for s in
                      os.environ.get("CI_GREEN_GATE_EXCLUDE", "").split(",")
                      if s.strip()]


# --- Fetch-outcome classification (VISIBLE, classified failures) --------------
# A health check that runs automatically and fails silently is the same
# non-mechanism in a new costume. Every gh fetch that cannot complete is recorded
# as a FetchFailure and folded into the EXIT CODE, split into the two
# operator-actionable buckets the auto-invoker must tell apart:
#
#   CI-HUB BROKEN  — our token / config / tooling is wrong: auth (401/403), gh
#                    missing, unparseable output, 404. FIX US.  -> report exit 2.
#   UPSTREAM SLOW  — GitHub itself was slow or unavailable: subprocess timeout,
#                    5xx, rate-limit. RETRY, don't touch ci-hub. -> report exit 3.
#
# This split is exactly the "is ci-hub broken, or was GitHub just slow?" question.
FETCH_TIMEOUT = "timeout"
FETCH_AUTH = "auth"
FETCH_NOTFOUND = "notfound"
FETCH_RATELIMIT = "ratelimit"
FETCH_UPSTREAM = "upstream"
FETCH_TOOLING = "tooling"
FETCH_BADJSON = "badjson"
FETCH_ERROR = "error"

CI_HUB_BROKEN_CLASSES = frozenset({FETCH_AUTH, FETCH_NOTFOUND, FETCH_TOOLING,
                                   FETCH_BADJSON, FETCH_ERROR})
UPSTREAM_CLASSES = frozenset({FETCH_TIMEOUT, FETCH_RATELIMIT, FETCH_UPSTREAM})

# Exit codes for the HUMAN report path (`runner-health` / `ci-status.py`). The
# tick gate stays binary (0/1) but carries the same split in its emitted fields.
EXIT_OK = 0
EXIT_CI_HUB_BROKEN = 2      # actionable now: fix token / config / tooling
EXIT_UPSTREAM_DEGRADED = 3  # transient: GitHub slow / unavailable, retry

# Repos whose self-hosted runner inventory we actually administer. The repo-scope
# runners API needs admin, so querying it on any OTHER repo is STRUCTURALLY a 403
# — a false alarm we used to print to stderr while exiting 0. For a
# non-administered repo the runner inventory is reported N/A BY DESIGN and never
# fetched, so no spurious 403 is generated; run/queue/green signals still work.
SELF_HOSTED_REPOS = frozenset(
    s.strip() for s in os.environ.get(
        "CI_SELFHOSTED_REPOS", "rrnewton/hermit,rrnewton/reverie").split(",")
    if s.strip())


@dataclass
class FetchFailure:
    repo: str
    endpoint: str        # "run-list" | "runners-api" | "jobs-api"
    klass: str           # one of the FETCH_* classes
    detail: str          # short human string (already truncated)

    @property
    def is_ci_hub_broken(self) -> bool:
        return self.klass in CI_HUB_BROKEN_CLASSES

    def line(self) -> str:
        bucket = "CI-HUB-BROKEN" if self.is_ci_hub_broken else "UPSTREAM"
        return (f"[{bucket}] {self.repo} {self.endpoint}: "
                f"{self.klass} — {self.detail}")


def classify_gh_failure(returncode: int | None, stderr: str,
                        exc: Exception | None) -> tuple[str, str]:
    """Map a gh failure to (class, short_detail). Pure; unit-tested.

    Distinguishes our-side breakage (auth/tooling/badjson/404) from upstream
    slowness/unavailability (timeout/5xx/rate-limit) so the caller can pick the
    right response instead of collapsing everything to a silent None.
    """
    if isinstance(exc, subprocess.TimeoutExpired):
        return FETCH_TIMEOUT, "gh call timed out (GitHub slow / unreachable)"
    if isinstance(exc, FileNotFoundError):
        return FETCH_TOOLING, "gh executable not found (tooling misconfigured)"
    s = " ".join((stderr or "").split())
    low = s.lower()
    detail = s[:200] or f"gh exit {returncode}"
    if "rate limit" in low or "abuse" in low or "429" in s:
        return FETCH_RATELIMIT, detail
    if "403" in s or "401" in s or "permission" in low or "not accessible" in low:
        return FETCH_AUTH, detail
    if "404" in s or "not found" in low:
        return FETCH_NOTFOUND, detail
    if any(c in s for c in ("500", "502", "503", "504")) or "server error" in low:
        return FETCH_UPSTREAM, detail
    return FETCH_ERROR, detail


def fetch_verdict(failures: list[FetchFailure]) -> tuple[int, str, str]:
    """(exit_code, state, summary) for a set of fetch failures. Pure; tested.

    ci-hub-side breakage dominates upstream slowness because it is the one a human
    must act on now; a bare timeout is a retry, not a page.
    """
    if not failures:
        return EXIT_OK, "ok", "all fetches completed"
    broken = [f for f in failures if f.is_ci_hub_broken]
    upstream = [f for f in failures if not f.is_ci_hub_broken]
    if broken:
        return (EXIT_CI_HUB_BROKEN, "ci-hub-broken",
                f"{len(broken)} ci-hub-broken + {len(upstream)} upstream fetch "
                f"failure(s); ci-hub-side breakage dominates (fix token/config)")
    return (EXIT_UPSTREAM_DEGRADED, "upstream-degraded",
            f"{len(upstream)} upstream fetch failure(s): GitHub slow/unavailable "
            f"(retry; ci-hub itself is fine)")


# --- Green-time integral (owner headline metric) -----------------------------
# Instantaneous state answers "is main green right NOW?"; the owner is explicit
# that what we actually optimize is the INTEGRAL — "what fraction of conclusive
# authoritative-run wall-clock has been green?" A check that measures only the
# point sample cannot tell us whether we are improving. NO-RESULT and gap time
# travel beside the figure but do not enter its denominator. That integral is
# already derived (never estimated) by ci-hub/history/query.py from the LOCAL run
# store (no GitHub call, so it is cheap enough for every tick and immune to the
# gh-fetch timeout path above). We surface it here so the AUTO-INVOKED tick
# reports the scoreboard, not just the referee. It is a REPORTED metric, not a
# gate input: a lagging integral should not flap the fail-loud tick, and its
# freshness depends on ingest.py having refreshed the store.
def _load_history_query():
    """Import ci-hub/history/query.py by path (no sys.path pollution / name clash)."""
    import importlib.util
    path = os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "history", "query.py")
    spec = importlib.util.spec_from_file_location("ci_hub_history_query", path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def green_time_field(repo: str, since: str | None = None, *,
                     persist: bool = False) -> str:
    """Compact 'green-time' string for the tick / report. Never raises.

    Degrades to a STATED 'UNAVAILABLE (<why>)' — a thin/absent history store or an
    import failure must read as "we don't know yet", never as a crash and never
    as a fabricated number.
    """
    try:
        q = _load_history_query()
    except Exception as exc:  # history module unavailable
        return f"UNAVAILABLE (history query import failed: {exc})"
    try:
        parent = q.parent_root()
        res = q.green_time(parent, repo, since, None)
    except Exception as exc:  # store missing/corrupt/unreadable
        return f"UNAVAILABLE (green_time failed: {exc})"
    log_note = ""
    if persist:
        try:
            log_path = q.append_green_time_log(parent, res, None)
            log_note = f"; log={log_path}"
        except Exception as exc:
            # The metric remains usable, but a missing durable observation must
            # be visible in the captured tick field rather than silently lost.
            log_note = f"; log=ERROR({exc})"
    if res.get("green_pct") is None:
        return (f"UNAVAILABLE ({res.get('note', 'no data')}; window="
                f"{res.get('window_start', 'n/a')}.."
                f"{res.get('window_end_utc', 'n/a')}; denominator="
                f"{res.get('authoritative_run_hours', 0)} "
                f"authoritative-run h{log_note})")
    return (f"{res['green_pct']}% green = {res['green_hours']} green h / "
            f"{res['authoritative_run_hours']} authoritative-run h; window "
            f"{res['window_start']}..{res['window_end_utc']} "
            f"({res['window_hours']} wall h); excluded NO-RESULT "
            f"{res['no_result_hours']}h + gap {res['gap_hours']}h; "
            f"n={res['samples']} commits; current={res['current_state']}"
            f"{log_note}")


def _now() -> datetime:
    return datetime.now(timezone.utc)


def _parse_ts(s: str | None) -> datetime | None:
    if not s:
        return None
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00"))
    except ValueError:
        return None


def humanize_secs(secs: float | None) -> str:
    if secs is None:
        return "n/a"
    secs = int(secs)
    if secs < 0:
        secs = 0
    if secs < 60:
        return f"{secs}s"
    if secs < 3600:
        return f"{secs // 60}m{secs % 60:02d}s"
    if secs < 86400:
        return f"{secs // 3600}h{(secs % 3600) // 60:02d}m"
    return f"{secs // 86400}d{(secs % 86400) // 3600:02d}h"


def _pct(sorted_vals: list[float], q: float) -> float:
    """Nearest-rank percentile (q in [0,1]) of a non-empty sorted list."""
    if not sorted_vals:
        return 0.0
    if q <= 0:
        return sorted_vals[0]
    if q >= 1:
        return sorted_vals[-1]
    idx = int(round(q * (len(sorted_vals) - 1)))
    return sorted_vals[idx]


# --- gh plumbing --------------------------------------------------------------
# Default per-gh-call wall-clock bound. The human report path uses this generous
# value; the auto-invoked tick gate passes a much SMALLER bound (see
# compute_gate/gate `per_call_timeout`) so the whole gate resolves and reports a
# CLASSIFIED result before tick-hub's 30s SubprocessGateRunner guillotine — a
# hard-kill at 30s would surface a bare "timed out" indistinguishable from
# "ci-hub is broken", which is exactly the ambiguity this module exists to remove.
DEFAULT_GH_CALL_TIMEOUT = 120


def gh_json(args: list[str], gh_cmd: str,
            sink: list[FetchFailure] | None = None,
            repo: str = "", endpoint: str = "",
            timeout: float = DEFAULT_GH_CALL_TIMEOUT):
    """Run a gh subcommand and parse JSON stdout. Returns None on failure.

    When `sink` is provided, every failure is ALSO recorded as a classified
    FetchFailure so a caller can turn "returned None" into a visible, attributed
    exit code instead of silently swallowing it. `timeout` bounds the call; on
    expiry the failure is classified as an UPSTREAM timeout, not our-side breakage.
    """
    cmd = shlex.split(gh_cmd) + args
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
        print(f"  ! gh call failed ({exc.__class__.__name__}): {' '.join(args)}",
              file=sys.stderr)
        if sink is not None:
            klass, detail = classify_gh_failure(None, "", exc)
            sink.append(FetchFailure(repo, endpoint, klass, detail))
        return None
    if out.returncode != 0:
        print(f"  ! gh returned {out.returncode}: {out.stderr.strip()[:200]}",
              file=sys.stderr)
        if sink is not None:
            klass, detail = classify_gh_failure(out.returncode, out.stderr, None)
            sink.append(FetchFailure(repo, endpoint, klass, detail))
        return None
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        print(f"  ! gh returned unparseable JSON: {' '.join(args)}",
              file=sys.stderr)
        if sink is not None:
            sink.append(FetchFailure(repo, endpoint, FETCH_BADJSON,
                                     "gh returned unparseable JSON"))
        return None


def fetch_runs(repo: str, gh_cmd: str, limit: int,
               sink: list[FetchFailure] | None = None,
               timeout: float = DEFAULT_GH_CALL_TIMEOUT) -> list[dict] | None:
    fields = ("databaseId,workflowName,status,conclusion,createdAt,updatedAt,"
              "headBranch,event,displayTitle")
    return gh_json(
        ["run", "list", "--repo", repo, "--limit", str(limit), "--json", fields],
        gh_cmd, sink=sink, repo=repo, endpoint="run-list", timeout=timeout,
    )


def fetch_runners(repo: str, gh_cmd: str,
                  sink: list[FetchFailure] | None = None,
                  timeout: float = DEFAULT_GH_CALL_TIMEOUT) -> dict | None:
    return gh_json(["api", f"repos/{repo}/actions/runners"], gh_cmd,
                   sink=sink, repo=repo, endpoint="runners-api", timeout=timeout)


def fetch_job_timings(repo: str, gh_cmd: str, run_ids: list[int],
                      sink: list[FetchFailure] | None = None) -> list[dict]:
    """Per-job (wait, duration, runner, workflow) over the given runs.

    wait     = job.started_at - job.created_at  (runner-pickup latency)
    duration = job.completed_at - job.started_at
    Only jobs that actually ran on a runner are returned; skipped/never-dispatched
    jobs (no runner, no real start) are dropped so they cannot deflate the wait
    distribution.
    """
    jobs: list[dict] = []
    for rid in run_ids:
        data = gh_json(["api", f"repos/{repo}/actions/runs/{rid}/jobs"], gh_cmd,
                       sink=sink, repo=repo, endpoint="jobs-api")
        if not data:
            continue
        for j in data.get("jobs", []):
            created = _parse_ts(j.get("created_at"))
            started = _parse_ts(j.get("started_at"))
            completed = _parse_ts(j.get("completed_at"))
            runner = j.get("runner_name")
            # A job that never got a runner (skipped/queued-then-cancelled) has
            # no meaningful pickup latency; exclude it from the wait sample.
            if runner is None or started is None or created is None:
                continue
            wait = (started - created).total_seconds()
            dur = ((completed - started).total_seconds()
                   if completed is not None else None)
            if wait < 0:
                wait = 0.0
            jobs.append({
                "run_id": rid,
                "name": j.get("name"),
                "runner": runner,
                "conclusion": j.get("conclusion"),
                "wait": wait,
                "duration": dur,
                # Absolute timestamps for utilization / peak-concurrency sweeps.
                # `completed` is None for a still-running job (count it up to now).
                "created": created,
                "started": started,
                "completed": completed,
            })
    return jobs


# --- Analysis (pure functions over already-fetched data) ----------------------
@dataclass
class WorkflowQueue:
    workflow: str
    queued: int = 0
    running: int = 0
    queue_ages: list[float] = field(default_factory=list)  # secs, still-queued

    @property
    def inflight(self) -> int:
        return self.queued + self.running

    @property
    def max_age(self) -> float:
        return max(self.queue_ages) if self.queue_ages else 0.0

    @property
    def median_age(self) -> float:
        return _pct(sorted(self.queue_ages), 0.5) if self.queue_ages else 0.0


@dataclass
class LastGreen:
    workflow: str
    total_in_window: int
    runs_back: int | None          # 0 = most recent run is green; None = none found
    green_created: datetime | None
    green_id: int | None
    latest_status: str
    latest_conclusion: str | None


def analyze_queue(runs: list[dict], now: datetime | None = None
                  ) -> dict[str, WorkflowQueue]:
    now = now or _now()
    out: dict[str, WorkflowQueue] = {}
    for r in runs:
        wf = r.get("workflowName", "?")
        status = r.get("status", "")
        wq = out.setdefault(wf, WorkflowQueue(workflow=wf))
        if status in QUEUED_STATUSES:
            wq.queued += 1
            created = _parse_ts(r.get("createdAt"))
            if created is not None:
                wq.queue_ages.append((now - created).total_seconds())
        elif status in RUNNING_STATUSES:
            wq.running += 1
    return out


def analyze_last_green(runs: list[dict]) -> dict[str, LastGreen]:
    """runs must be newest-first (gh run list default)."""
    per_wf_runs: dict[str, list[dict]] = defaultdict(list)
    for r in runs:
        per_wf_runs[r.get("workflowName", "?")].append(r)
    out: dict[str, LastGreen] = {}
    for wf, wf_runs in per_wf_runs.items():
        runs_back = None
        green = None
        for i, r in enumerate(wf_runs):
            if r.get("conclusion") == "success":
                runs_back = i
                green = r
                break
        latest = wf_runs[0]
        out[wf] = LastGreen(
            workflow=wf,
            total_in_window=len(wf_runs),
            runs_back=runs_back,
            green_created=_parse_ts(green["createdAt"]) if green else None,
            green_id=green["databaseId"] if green else None,
            latest_status=latest.get("status", "?"),
            latest_conclusion=latest.get("conclusion"),
        )
    return out


@dataclass
class RunnerHealth:
    total: int                       # CONFIGURED runners registered at repo scope
    online: int                      # LIVE (status == online)
    idle: int
    busy: int
    runners: list[dict]              # raw runner objects
    pmu_total: int
    pmu_idle: int
    serial_runners: list[str]        # names carrying pmu-serial
    serial_busy: bool

    @property
    def offline(self) -> int:
        """CONFIGURED - LIVE: a registered-but-offline runner is silently dead
        capacity — invisible unless configured and live are compared."""
        return max(0, self.total - self.online)

    @property
    def names(self) -> set[str]:
        """Names of every configured runner (the repo runners API lists only
        self-hosted runners, so this set identifies self-hosted jobs)."""
        return {r.get("name") for r in self.runners if r.get("name")}


def analyze_runners(runner_api: dict | None) -> RunnerHealth | None:
    if not runner_api:
        return None
    runners = runner_api.get("runners", [])
    total = runner_api.get("total_count", len(runners))

    def labels_of(r):
        return {l["name"] for l in r.get("labels", [])}

    online = sum(1 for r in runners if r.get("status") == "online")
    busy = sum(1 for r in runners if r.get("busy"))
    idle = sum(1 for r in runners
               if r.get("status") == "online" and not r.get("busy"))
    pmu = [r for r in runners if "pmu" in labels_of(r)]
    pmu_idle = sum(1 for r in pmu
                   if r.get("status") == "online" and not r.get("busy"))
    serial = [r for r in runners if "pmu-serial" in labels_of(r)]
    serial_busy = any(r.get("busy") for r in serial)
    return RunnerHealth(
        total=total, online=online, idle=idle, busy=busy, runners=runners,
        pmu_total=len(pmu), pmu_idle=pmu_idle,
        serial_runners=[r.get("name") for r in serial], serial_busy=serial_busy,
    )


def binding_constraint(queues: dict[str, WorkflowQueue],
                       rh: RunnerHealth | None) -> str | None:
    """Return a one-line capacity signal, or None if no pressure is observed.

    The run-list snapshot says which *workflow runs* are queued, but not the
    labels requested by their queued jobs.  Runner labels are therefore
    inventory, not proof of eligibility.  Keep the legacy function name for
    callers, but never name PMU, gate, or pmu-serial as the binding constraint
    without job-level evidence this cheap tick path does not fetch.
    """
    if rh is None:
        return None
    total_queued = sum(q.queued for q in queues.values())
    if total_queued <= 0:
        return None

    offline = [r for r in rh.runners if r.get("status") != "online"]
    if offline:
        details = []
        for runner in offline:
            labels = [
                str(label.get("name"))
                for label in runner.get("labels", [])
                if label.get("name") not in {"self-hosted", "Linux", "X64"}
            ]
            details.append(
                f"{runner.get('name') or '?'}[{','.join(labels) or 'no-custom-label'}]"
            )
        return (
            f"configured self-hosted capacity missing: {','.join(details)} offline; "
            f"{total_queued} workflow run(s) queued; queued-job labels were not "
            "queried, so causal binding is not established"
        )
    if rh.idle == 0:
        return (
            f"all online self-hosted runners busy: 0 idle of {rh.online} online "
            f"({rh.total} configured) with {total_queued} workflow run(s) queued; "
            "queued-job labels were not queried, so PMU, gate, and pmu-serial "
            "cannot be named as binding"
        )
    return None


@dataclass
class WaitSample:
    """Historical time-in-queue vs run duration, per workflow, from the jobs API."""
    workflow: str
    n: int
    wait_median: float
    wait_p90: float
    wait_max: float
    dur_median: float
    dur_p90: float


def analyze_waits(jobs: list[dict], run_workflow: dict[int, str]
                  ) -> dict[str, WaitSample]:
    by_wf_wait: dict[str, list[float]] = defaultdict(list)
    by_wf_dur: dict[str, list[float]] = defaultdict(list)
    for j in jobs:
        wf = run_workflow.get(j["run_id"], "?")
        by_wf_wait[wf].append(j["wait"])
        if j["duration"] is not None:
            by_wf_dur[wf].append(j["duration"])
    out: dict[str, WaitSample] = {}
    for wf, waits in by_wf_wait.items():
        sw = sorted(waits)
        sd = sorted(by_wf_dur.get(wf, []))
        out[wf] = WaitSample(
            workflow=wf, n=len(sw),
            wait_median=_pct(sw, 0.5), wait_p90=_pct(sw, 0.9), wait_max=sw[-1],
            dur_median=_pct(sd, 0.5) if sd else 0.0,
            dur_p90=_pct(sd, 0.9) if sd else 0.0,
        )
    return out


# --- Utilization + peak concurrency (owner's top-priority capacity signal) -----
# Both derive from the SAME bounded jobs-API sample as the wait distribution, and
# both are computed over a FIXED window [now - window_hours, now]. Fixing the
# window is what makes truncation honest: the sample can only ever OMIT busy time
# / overlapping intervals, never invent them, so a truncated sample yields a
# strict LOWER BOUND on true utilization and true peak concurrency. Every result
# carries `lower_bound` and a `basis` string stating the direction of its error.

def _overlap_secs(start: datetime, end: datetime,
                  win_start: datetime, win_end: datetime) -> float:
    lo = max(start, win_start)
    hi = min(end, win_end)
    return (hi - lo).total_seconds() if hi > lo else 0.0


@dataclass
class Utilization:
    n_runners: int                   # self-hosted capacity denominator
    window_secs: float
    capacity_secs: float             # n_runners * window_secs
    busy_secs: float                 # self-hosted busy-runner-seconds in window
    util_pct: float
    n_jobs: int                      # self-hosted jobs contributing
    lower_bound: bool
    basis: str


def analyze_utilization(jobs: list[dict], selfhosted: set[str], n_runners: int,
                        window_start: datetime, now: datetime,
                        lower_bound: bool, basis: str) -> Utilization:
    """Self-hosted busy-runner-time as a percentage of capacity over the window.

    capacity = n_runners * window_secs. busy = sum of each self-hosted job's
    [started, completed] overlap with the window (a still-running job counts up to
    `now`). Only jobs whose runner is a known self-hosted runner contribute, so
    GitHub-hosted work never inflates self-hosted utilization.
    """
    window_secs = max(0.0, (now - window_start).total_seconds())
    busy = 0.0
    n = 0
    for j in jobs:
        if j.get("runner") not in selfhosted:
            continue
        started = j.get("started")
        if started is None:
            continue
        end = j.get("completed") or now
        ov = _overlap_secs(started, end, window_start, now)
        if ov > 0:
            busy += ov
            n += 1
    capacity = n_runners * window_secs if n_runners else 0.0
    util = (100.0 * busy / capacity) if capacity > 0 else 0.0
    return Utilization(
        n_runners=n_runners, window_secs=window_secs, capacity_secs=capacity,
        busy_secs=busy, util_pct=util, n_jobs=n,
        lower_bound=lower_bound, basis=basis,
    )


@dataclass
class PeakConcurrency:
    peak: int                        # max self-hosted jobs running at once
    at: datetime | None
    n_runners: int
    n_intervals: int
    lower_bound: bool
    basis: str


def analyze_peak_concurrency(jobs: list[dict], selfhosted: set[str],
                             n_runners: int, window_start: datetime,
                             now: datetime, lower_bound: bool,
                             basis: str) -> PeakConcurrency:
    """Peak observed concurrent self-hosted jobs via a sweep line over the
    sampled [started, completed] intervals clipped to the window.

    Directly answers "can we delete a runner?": a peak strictly below the runner
    count means capacity went unused even at the busiest observed instant — but
    only as a LOWER BOUND when the sample is truncated (unsampled jobs could add
    overlap), which the basis states.
    """
    events: list[tuple[datetime, int]] = []
    n_int = 0
    for j in jobs:
        if j.get("runner") not in selfhosted:
            continue
        started = j.get("started")
        if started is None:
            continue
        end = j.get("completed") or now
        lo = max(started, window_start)
        hi = min(end, now)
        if hi <= lo:
            continue
        events.append((lo, 1))
        events.append((hi, -1))
        n_int += 1
    # At an equal timestamp, close (-1) before open (+1) so back-to-back jobs on
    # one runner are not miscounted as concurrent.
    events.sort(key=lambda e: (e[0], e[1]))
    cur = peak = 0
    at: datetime | None = None
    for ts, delta in events:
        cur += delta
        if cur > peak:
            peak = cur
            at = ts
    return PeakConcurrency(
        peak=peak, at=at, n_runners=n_runners, n_intervals=n_int,
        lower_bound=lower_bound, basis=basis,
    )


# --- 24h window run aggregates (merge-gate kept SEPARATE from failures) --------
def _is_merge_gate(workflow: str | None, event: str | None) -> bool:
    """A merge-queue / merge-gate run. Kept out of the main failure tally so gate
    churn does not swamp real test failures (owner capability #7)."""
    if (event or "") == "merge_group":
        return True
    w = (workflow or "").lower()
    return "merge" in w and "gate" in w


@dataclass
class RunWindow:
    window_hours: float
    covers_window: bool              # run list reached back past window start
    oldest_created_iso: str | None
    # Non-gate main-branch/PR runs:
    started: int                     # created in window (any status)
    completed: int                   # reached terminal in window
    success: int
    failure: int
    cancelled: int
    # Merge-gate runs, counted SEPARATELY:
    gate_started: int
    gate_completed: int
    gate_failure: int


def analyze_run_window(runs: list[dict], now: datetime,
                       window_hours: float) -> RunWindow:
    win_start = now.timestamp() - window_hours * 3600
    started = completed = success = failure = cancelled = 0
    gate_started = gate_completed = gate_failure = 0
    oldest: datetime | None = None
    for r in runs:
        created = _parse_ts(r.get("createdAt"))
        if created is None:
            continue
        if oldest is None or created < oldest:
            oldest = created
        if created.timestamp() < win_start:
            continue
        gate = _is_merge_gate(r.get("workflowName"), r.get("event"))
        terminal = r.get("status") == "completed"
        concl = r.get("conclusion") or ""
        if gate:
            gate_started += 1
            if terminal:
                gate_completed += 1
                if concl == "failure":
                    gate_failure += 1
            continue
        started += 1
        if terminal:
            completed += 1
            if concl == "success":
                success += 1
            elif concl == "failure":
                failure += 1
            elif concl == "cancelled":
                cancelled += 1
    covers = oldest is not None and oldest.timestamp() <= win_start
    return RunWindow(
        window_hours=window_hours, covers_window=covers,
        oldest_created_iso=(oldest.strftime("%Y-%m-%dT%H:%M:%SZ")
                            if oldest else None),
        started=started, completed=completed, success=success,
        failure=failure, cancelled=cancelled,
        gate_started=gate_started, gate_completed=gate_completed,
        gate_failure=gate_failure,
    )


# --- Human report -------------------------------------------------------------
def report_repo(repo: str, gh_cmd: str, limit: int, sample: int,
                window_hours: float = 24.0,
                sink: list[FetchFailure] | None = None) -> None:
    print(f"\n================ {repo} ================")
    now = _now()
    runs = fetch_runs(repo, gh_cmd, limit, sink=sink)

    # (4)+(8) Runner health — CONFIGURED vs LIVE, so a silently-dead runner shows.
    # Only the repos we administer have a meaningful repo-scope runner inventory;
    # querying any other repo is a structural 403 (needs admin), so we report N/A
    # BY DESIGN there rather than fetch it and print a misleading auth error.
    if repo not in SELF_HOSTED_REPOS:
        rh = None
        print(f"  runners: N/A by design — no self-hosted runners administered "
              f"for {repo} (repo-scope runners API needs admin; not fetched, so "
              f"no spurious 403). Runner health is not a signal for this repo.")
    else:
        runner_api = fetch_runners(repo, gh_cmd, sink=sink)
        rh = analyze_runners(runner_api)
    if repo in SELF_HOSTED_REPOS and rh is None:
        print("  runners: COULD NOT FETCH self-hosted inventory (see DEGRADED "
              "summary below) — runner health UNKNOWN for this ADMINISTERED "
              "repo; this is a real failure, not an expected access limit.")
    elif rh is not None:
        dead = (f" | OFFLINE (silently-dead capacity) {rh.offline}"
                if rh.offline else "")
        print(f"  runners: CONFIGURED={rh.total} LIVE(online)={rh.online}"
              f"{dead} | idle={rh.idle} busy={rh.busy} | "
              f"pmu={rh.pmu_total} (idle {rh.pmu_idle}) | "
              f"pmu-serial={','.join(rh.serial_runners) or 'none'}")
        if rh.offline:
            print(f"    ! FINDING: {rh.offline} configured runner(s) are NOT "
                  f"online — registered but dead capacity (invisible without "
                  f"the configured-vs-live comparison).")
        for r in rh.runners:
            labels = ",".join(l["name"] for l in r.get("labels", []))
            job = "busy" if r.get("busy") else "idle"
            print(f"    - {r.get('name'):<22} {r.get('status'):<8} {job:<5} "
                  f"[{labels}]")

    if runs is None:
        print("  runs: (could not fetch) — basis: gh run list failed")
        return

    queues = analyze_queue(runs, now=now)
    greens = analyze_last_green(runs)

    # (1)+(2a) Queue depth + current queue age, per workflow.
    print(f"  queue depth (basis: gh run list, last {len(runs)} runs; "
          f"queue age = now - createdAt, a LOWER BOUND on final wait):")
    active = sorted((q for q in queues.values() if q.inflight),
                    key=lambda q: (-q.inflight, q.workflow))
    if not active:
        print("    (nothing in flight)")
    for q in active:
        age = (f" | queue age median {humanize_secs(q.median_age)} "
               f"max {humanize_secs(q.max_age)}" if q.queue_ages else "")
        print(f"    - {q.workflow:<32} queued={q.queued} running={q.running}"
              f"{age}")

    # Capacity signal.  The cheap run-list path does not expose queued-job labels,
    # so this deliberately refuses to name a label-specific binding constraint.
    bc = binding_constraint(queues, rh)
    if bc:
        print(f"  ^ CAPACITY SIGNAL (binding label not established): {bc}")

    # (5) NAMED in-flight runs — "run X on branch Y", not "something is running".
    inflight = [r for r in runs if r.get("status") in INFLIGHT_STATUSES]
    if inflight:
        inflight.sort(key=lambda r: (r.get("status") != "in_progress",
                                     r.get("createdAt") or ""))
        shown = inflight[:25]
        print(f"  in-flight runs, named ({len(inflight)} total):")
        for r in shown:
            created = _parse_ts(r.get("createdAt"))
            age = (humanize_secs((now - created).total_seconds())
                   if created else "n/a")
            title = (r.get("displayTitle") or r.get("workflowName") or "")[:48]
            print(f"    - #{r['databaseId']} {r.get('status'):<11} "
                  f"{(r.get('workflowName') or '?'):<28} "
                  f"[{r.get('headBranch') or '?'}] {title} (age {age})")
        if len(inflight) > len(shown):
            print(f"    (+{len(inflight) - len(shown)} more in-flight runs not "
                  f"listed)")

    # (6) 24h window aggregates — merge-gate counted SEPARATELY from failures.
    rw = analyze_run_window(runs, now, window_hours)
    coverage = ("" if rw.covers_window else
                f" — COVERAGE WARNING: the {len(runs)}-run window only reaches "
                f"back to {rw.oldest_created_iso}, NOT a full {window_hours:g}h; "
                f"counts are a LOWER BOUND (raise --limit)")
    print(f"  last {window_hours:g}h (basis: gh run list, {len(runs)} runs; "
          f"created-in-window){coverage}:")
    print(f"    - non-gate: started={rw.started} completed={rw.completed} "
          f"success={rw.success} failure={rw.failure} cancelled={rw.cancelled}")
    print(f"    - merge-gate (SEPARATE, excluded from the failure count above): "
          f"started={rw.gate_started} completed={rw.gate_completed} "
          f"failure={rw.gate_failure}")

    # (3) Time since last green + runs back.
    print("  last green per workflow (basis: newest-first scan of the window):")
    for wf in sorted(greens):
        g = greens[wf]
        if g.runs_back is None:
            print(f"    - {wf:<32} NO GREEN in last {g.total_in_window} runs "
                  f"(latest {g.latest_status}/{g.latest_conclusion or '-'})")
        else:
            elapsed = (humanize_secs((now - g.green_created).total_seconds())
                       if g.green_created else "n/a")
            back = ("most recent run" if g.runs_back == 0
                    else f"{g.runs_back} run(s) back")
            print(f"    - {wf:<32} GREEN {elapsed} ago, {back} "
                  f"(id {g.green_id})")

    # (8) GREEN-TIME INTEGRAL — the headline metric: not "is main green now?" but
    # "what fraction of conclusive authoritative-run wall-clock has it been
    # green?". Derived from the local history store (ci-hub/history), so it
    # reflects ingest freshness, not a live GitHub read.
    print(f"  main green-time (integral, authoritative workflow, from local "
          f"history store): {green_time_field(repo)}")

    # (2b)+(7) Jobs-API sample: wait/duration distribution, utilization, peak.
    if sample <= 0:
        print("  time-in-queue distribution / utilization / peak concurrency: "
              "skipped (--sample 0); current queue age above is the free "
              "lower-bound signal")
        return

    running_ids = [r["databaseId"] for r in runs
                   if r.get("status") in RUNNING_STATUSES]
    completed_all = [r["databaseId"] for r in runs
                     if r.get("status") == "completed"]
    completed_ids = completed_all[:sample]
    sampled_ids = running_ids + completed_ids
    jobs = fetch_job_timings(repo, gh_cmd, sampled_ids, sink=sink)
    run_wf = {r["databaseId"]: r.get("workflowName", "?") for r in runs}
    waits = analyze_waits(jobs, run_wf)

    print(f"  time-in-queue vs run duration (basis: jobs API over "
          f"{len(completed_ids)} completed + {len(running_ids)} running runs, "
          f"{len(jobs)} jobs; wait = job.started-created, SEPARATE from "
          f"duration):")
    if not waits:
        print("    (no jobs sampled)")
    for wf in sorted(waits):
        w = waits[wf]
        print(f"    - {wf:<32} n={w.n} wait med/p90/max "
              f"{humanize_secs(w.wait_median)}/{humanize_secs(w.wait_p90)}/"
              f"{humanize_secs(w.wait_max)} | dur med/p90 "
              f"{humanize_secs(w.dur_median)}/{humanize_secs(w.dur_p90)}")

    # Utilization + peak concurrency over a FIXED window, self-hosted only.
    if rh is None or rh.total == 0:
        print("  utilization / peak concurrency: n/a (no self-hosted runners "
              "visible at repo scope)")
        return
    truncated = len(completed_all) > len(completed_ids)
    if not rw.covers_window:
        truncated = True
    window_start = datetime.fromtimestamp(
        now.timestamp() - window_hours * 3600, tz=timezone.utc)
    trunc_note = (f"sample truncated ({len(completed_ids)} of "
                  f"{len(completed_all)} completed runs in the {len(runs)}-run "
                  f"list, NEWEST-FIRST — biased toward frequently-completing "
                  f"short workflows, so long runs are under-represented; run "
                  f"list itself "
                  f"{'does NOT' if not rw.covers_window else 'does'} span the "
                  f"full {window_hours:g}h)"
                  if truncated else
                  f"sample covers all {len(completed_all)} completed runs in the "
                  f"{len(runs)}-run list")
    selfhosted = rh.names
    util = analyze_utilization(jobs, selfhosted, rh.total, window_start, now,
                               lower_bound=truncated, basis=trunc_note)
    peak = analyze_peak_concurrency(jobs, selfhosted, rh.total, window_start,
                                    now, lower_bound=truncated, basis=trunc_note)
    bound = "LOWER BOUND" if truncated else "measured"
    print(f"  self-hosted utilization over last {window_hours:g}h "
          f"({bound}; basis: {util.basis}):")
    print(f"    - {util.util_pct:.1f}% = {humanize_secs(util.busy_secs)} busy / "
          f"{humanize_secs(util.capacity_secs)} capacity "
          f"({rh.total} runner(s) x {window_hours:g}h), from "
          f"{util.n_jobs} self-hosted job(s)")
    if truncated:
        print(f"      ^ direction of error: UNDER-counts busy time (omitted "
              f"jobs are never added), so TRUE utilization is >= this figure.")
    verdict = ("peak < runners: spare capacity existed even at the busiest "
               "SAMPLED instant" if peak.peak < rh.total else
               "peak >= runners: capacity was fully committed at the busiest "
               "sampled instant")
    print(f"  peak observed concurrent self-hosted jobs: {peak.peak} vs "
          f"{rh.total} configured runner(s) ({bound}) — {verdict}")
    if truncated:
        print(f"      ^ direction of error: unsampled overlapping jobs can only "
              f"RAISE the peak, so TRUE peak is >= {peak.peak}. Do NOT conclude "
              f"a runner is deletable from a truncated peak alone.")


# --- Tick gate ----------------------------------------------------------------
def _field(value: object) -> str:
    return " ".join(str(value).split()) or "none"


def compute_gate(repo: str, gh_cmd: str, limit: int, now: datetime | None = None,
                 sink: list[FetchFailure] | None = None,
                 per_call_timeout: float = DEFAULT_GH_CALL_TIMEOUT,
                 persist_green_time: bool = False,
                 ) -> tuple[int, dict[str, object]]:
    """Return (exit_code, fields) for the ops tick. Cheap: no jobs-API sampling.

    Unhealthy (exit 1) when any of: a workflow's queued depth >= QUEUE_DEPTH_WARN;
    max current queue age >= QUEUE_AGE_WARN_SECS; a workflow with >=
    GREEN_GATE_MIN_RUNS runs in the window has no green within GREEN_RUNS_BACK_WARN
    runs or GREEN_AGE_WARN_SECS; or the self-hosted lane is the active binding
    constraint. Every emitted number carries its basis in `summary`.
    """
    now = now or _now()
    # Derives from the local history store, so record it even when the live
    # GitHub queue query below is unavailable. Otherwise an egress outage would
    # create a hole in the very trend meant to expose health over time.
    green_time = green_time_field(repo, persist=persist_green_time)
    runs = fetch_runs(repo, gh_cmd, limit, sink=sink, timeout=per_call_timeout)
    # Same admin gate as the human report: only fetch the runner inventory where
    # we administer runners, so a non-administered repo never emits a false 403.
    runner_api = (fetch_runners(repo, gh_cmd, sink=sink, timeout=per_call_timeout)
                  if repo in SELF_HOSTED_REPOS else None)
    rh = analyze_runners(runner_api)
    if runs is None:
        return 1, {"state": "unknown", "summary": "gh-run-list-failed",
                   "repo": repo, "green_time": green_time}

    queues = analyze_queue(runs, now=now)
    greens = analyze_last_green(runs)

    max_depth = max((q.queued for q in queues.values()), default=0)
    worst_depth_wf = max(queues.values(), key=lambda q: q.queued, default=None)
    max_age = max((q.max_age for q in queues.values()), default=0.0)
    worst_age_wf = max(queues.values(), key=lambda q: q.max_age, default=None)

    stale_green: list[str] = []
    for wf, g in greens.items():
        if g.total_in_window < GREEN_GATE_MIN_RUNS:
            continue
        if any(sub in wf for sub in GREEN_GATE_EXCLUDE):
            continue
        no_recent = (g.runs_back is None
                     or g.runs_back >= GREEN_RUNS_BACK_WARN)
        old = (g.green_created is not None
               and (now - g.green_created).total_seconds() >= GREEN_AGE_WARN_SECS)
        if g.runs_back is None or no_recent or old:
            if g.runs_back is None:
                stale_green.append(f"{wf}:none/{g.total_in_window}")
            else:
                stale_green.append(f"{wf}:{g.runs_back}back")

    bc = binding_constraint(queues, rh)

    reasons: list[str] = []
    if max_depth >= QUEUE_DEPTH_WARN and worst_depth_wf:
        reasons.append(f"depth {worst_depth_wf.workflow}={max_depth}")
    if max_age >= QUEUE_AGE_WARN_SECS and worst_age_wf:
        reasons.append(f"age {worst_age_wf.workflow}={humanize_secs(max_age)}")
    if stale_green:
        reasons.append("stale-green " + ",".join(stale_green))
    if bc:
        reasons.append("capacity-signal")

    state = "red" if reasons else "ok"
    runners_txt = (f"total={rh.total},idle={rh.idle},pmu_idle={rh.pmu_idle}"
                   if rh else "unavailable")
    fields = {
        "state": state,
        "repo": repo,
        "max_queue_depth": max_depth,
        "max_queue_age": humanize_secs(max_age),
        "worst_workflow": worst_depth_wf.workflow if worst_depth_wf else "none",
        "stale_green": ",".join(stale_green) or "none",
        "binding_constraint": bc or "none",
        "runners": runners_txt,
        # Headline INTEGRAL, reported alongside the instantaneous queue state so a
        # tick shows both "green right now?" and "green over time?". Derived from
        # the local history store; does NOT affect the gate exit code.
        "green_time": green_time,
        "summary": (f"depth<= {max_depth}, age<= {humanize_secs(max_age)}, "
                    f"reasons=[{'; '.join(reasons) or 'none'}] "
                    f"(basis: last {len(runs)} runs; queue age=now-createdAt)"),
    }
    return (1 if reasons else 0), fields


def gate(repos: list[str], gh_cmd: str, limit: int,
         per_call_timeout: float = DEFAULT_GH_CALL_TIMEOUT) -> int:
    rc = 0
    agg: dict[str, object] = {}
    failures: list[FetchFailure] = []
    for repo in repos:
        code, fields = compute_gate(repo, gh_cmd, limit, sink=failures,
                                    per_call_timeout=per_call_timeout,
                                    persist_green_time=True)
        rc = rc or code
        if len(repos) > 1:
            agg[repo] = fields["state"]
        else:
            agg = fields
    # Any fetch failure trips the gate so the auto-invoker cannot mistake an
    # unreachable check for a healthy one; the fetch_* fields state WHICH side
    # ("ci-hub broken" vs "GitHub slow") so the tick message routes correctly.
    _fcode, fstate, fsummary = fetch_verdict(failures)
    if failures:
        rc = rc or 1
    if len(repos) > 1:
        # Multi-repo: emit a compact per-repo state map plus overall. A fetch
        # degradation becomes the overall state so the tick title is not "ok".
        overall = fstate if failures else ("red" if rc else "ok")
        per_repo = ",".join(f"{r}:{s}" for r, s in agg.items())
        summary = f"{fsummary}; per-repo: {per_repo}" if failures else per_repo
        print(f"state={_field(overall)}")
        print("summary=" + _field(summary))
    else:
        # Single repo: fold the fetch verdict into the emitted state/summary so
        # the tick TITLE ("{state}: {summary}") never reads "ok" while a wakeup
        # fires for a check that could not actually reach GitHub. A genuine queue
        # problem (state red/unknown) is more urgent and is kept as-is.
        if failures:
            agg = dict(agg)
            if str(agg.get("state", "ok")) == "ok":
                agg["state"] = fstate
            agg["summary"] = f"{fsummary} | queue: {agg.get('summary', '')}"
        for k, v in agg.items():
            print(f"{k}={_field(v)}")
    print(f"fetch_state={_field(fstate)}")
    print(f"fetch_detail={_field(fsummary)}")
    if failures:
        print("fetch_failures=" + _field("; ".join(f.line() for f in failures)))
    return rc


# --- CLI ----------------------------------------------------------------------
DEFAULT_REPO = "rrnewton/hermit"
ALL_REPOS = ["rrnewton/hermit", "rrnewton/reverie",
             "facebookexperimental/hermit"]


def main(argv=None) -> int:
    p = argparse.ArgumentParser(
        description="GitHub Actions queue-depth / wait-time / last-green health",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--repo", default=DEFAULT_REPO)
    p.add_argument("--all", action="store_true", help="all three Hermit repos")
    p.add_argument("--limit", type=int, default=100,
                   help="recent runs to summarize (default 100)")
    p.add_argument("--sample", type=int, default=15,
                   help="completed runs to sample via the jobs API for the "
                        "time-in-queue distribution, utilization, and peak "
                        "concurrency (0 disables; default 15)")
    p.add_argument("--window-hours", type=float, default=24.0,
                   help="window (hours) for the run aggregates, utilization, "
                        "and peak-concurrency figures (default 24)")
    p.add_argument("--gate", action="store_true",
                   help="emit key=value tick fields; exit 1 when unhealthy "
                        "(no jobs-API sampling)")
    p.add_argument("--gh", default=None,
                   help="gh command (default: $GH or 'with-proxy gh')")
    args = p.parse_args(argv)

    gh_cmd = args.gh or os.environ.get("GH", "with-proxy gh")
    repos = ALL_REPOS if args.all else [args.repo]

    if args.gate:
        return gate(repos, gh_cmd, args.limit)

    print(f"Hermit CI queue health — gh via: {gh_cmd!r}")
    failures: list[FetchFailure] = []
    for repo in repos:
        report_repo(repo, gh_cmd, args.limit, args.sample, args.window_hours,
                    sink=failures)
    print("\n(Remediation options: ci-hub/runners/README.md)")

    # VISIBILITY: a fetch we could not complete becomes a classified, non-zero
    # exit instead of a stderr line the auto-invoker would swallow while exiting 0.
    code, state, summary = fetch_verdict(failures)
    if code != EXIT_OK:
        print(f"\n!! DEGRADED ({state}, exit {code}): {summary}")
        for f in failures:
            print(f"   {f.line()}")
        print("   Interpretation: CI-HUB-BROKEN = our token/config/tooling — fix "
              "ci-hub; UPSTREAM = GitHub slow/unavailable — retry, ci-hub is fine.")
    return code


if __name__ == "__main__":
    raise SystemExit(main())
