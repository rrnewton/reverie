#!/usr/bin/env python3
"""Incremental, idempotent ingestion of CI history into the ci-hub store.

This is the SINGLE local accumulator of commit/CI knowledge. Do not build a
parallel one: two independently-maintained stores drift (exactly how the
hand-maintained test-footprint map rotted). Consumers join by the documented
file-contract columns only (see JOIN KEYS below), never by importing this
module.

Two ingesters, both incremental and idempotent (re-running never duplicates a
row and resumes from the last cursor):

  (A) GitHub Actions runs -> ignored/ci-hub/gha-runs.csv
      Keyed by (repo, run_id, run_attempt); UPSERT by newest updated_at, so an
      in-progress row is promoted to its terminal conclusion on re-run with zero
      duplicate rows. Cursor ignored/ci-hub/gha-cursor.json tracks the newest
      created_at / max run_id per repo for incremental resume; an overlap window
      re-checks the tail, and every non-terminal stored row is re-fetched by id
      so queued/in_progress runs are promoted once they finish.
      TIMING IS SPLIT (the hosted pool is queue-starved, so a single wall figure
      lies): QUEUE_s = run_started_at - created_at is recorded SEPARATELY from
      RUN_s = updated_at - run_started_at.

      RUN_s is end-to-end wall time, not execution cost. RUNNER_s is the sum of
      attempt-matched job intervals. JOBS_SRC says whether that value came from
      job rows, a fetched attempt had no executable interval, or the attempt was
      never measured. Both latter states leave RUNNER_s and JOBS_n blank, never
      zero; only the fetched state is named `absent`.

  (B) Per-node CI profiling artifacts -> ignored/ci-hub/gha-profiles/<repo>/...
      Downloads GitHub artifacts named ^ci-perf-* (produced by the portable DAG
      runner; see hermit PR #1548) and unzips their safe-ci-dag-runner
      step_profiles CSVs onto local disk where query.py node-cpu-budgets reads
      them alongside local .safe-ci-dag-runner/profiles/ CSVs. Idempotent: an
      already-downloaded artifact id is skipped. Also refreshes the local
      validate-run history via validate/aggregate.py --write-global.

JOIN KEYS:
  gha-runs.csv:head_sha == local-runs.csv:git_sha
  repo is the same OWNER/REPO string in every store.

All GitHub access goes through `with-proxy gh` (override via CI_HUB_GH).

Usage:
  ci-hub/history/ingest.py                      # incremental, both repos
  ci-hub/history/ingest.py --full               # backfill all history (windowed)
  ci-hub/history/ingest.py --since 2026-07-01   # bound the backfill / incremental
  ci-hub/history/ingest.py --repo rrnewton/hermit
  ci-hub/history/ingest.py --no-profiles --no-local   # only refresh gha-runs.csv
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import zipfile

import gha_rows

DEFAULT_REPOS = ["rrnewton/hermit", "rrnewton/reverie"]

# GitHub caps flat workflow-run pagination at 1000 results per `created` query,
# so every mode uses one recursive time-window fetcher: fetch a [start,end]
# window, and if it saturates (1000 rows with a full final page) bisect it in
# time until it fits. This never silently drops runs from a busy window (a drain
# day can exceed 1000 runs) the way flat paging would.
PER_PAGE = 100
RESULT_CAP_PAGES = 10          # page*per_page must stay <= 1000
FULL_MAX_DAYS = 730            # do not window back further than this for --full
DEFAULT_OVERLAP_HOURS = 12     # incremental re-check window on the tail
DEFAULT_INCR_DAYS = 14         # first incremental run (no cursor) backfills this
MIN_WINDOW_S = 60              # smallest bisection window before we warn+accept
PROFILES_MAX_PAGES = 30        # newest artifact pages scanned for ci-perf-* per refresh

TERMINAL_STATUS = "completed"


class IngestUnavailable(RuntimeError):
    """A required history source could not be read; zero runs is not inferred."""

# Compatibility aliases for callers and tests. The shared row contract owns the
# field set and ordering; neither producer nor consumer restates it.
GHA_COLUMNS = list(gha_rows.RUN_COLUMNS)
JOBS_COLUMNS = list(gha_rows.JOB_COLUMNS)
JOBS_SRC_DERIVED = gha_rows.JOBS_SRC_DERIVED
JOBS_SRC_ABSENT = gha_rows.JOBS_SRC_ABSENT


def parent_root() -> str:
    env = os.environ.get("DEV_HERMIT_PARENT")
    if env and os.path.isdir(env):
        return os.path.abspath(env)
    return os.path.abspath(os.path.join(os.path.dirname(__file__), "../.."))


def store_dir(parent: str) -> str:
    d = os.path.join(parent, "ignored", "ci-hub")
    os.makedirs(d, exist_ok=True)
    return d


def gha_store_path(parent: str) -> str:
    return os.path.join(store_dir(parent), "gha-runs.csv")


def cursor_path(parent: str) -> str:
    return os.path.join(store_dir(parent), "gha-cursor.json")


def profiles_dir(parent: str) -> str:
    return os.path.join(store_dir(parent), "gha-profiles")


# ---------------------------------------------------------------------------
# GitHub access (always via with-proxy gh)
# ---------------------------------------------------------------------------

def gh_prefix() -> list[str]:
    return shlex.split(os.environ.get("CI_HUB_GH", "with-proxy gh"))


def gh_json(path: str, *, timeout: int = 120):
    """GET a GitHub REST path via gh or refuse the ingest."""
    cmd = gh_prefix() + ["api", "-H", "Accept: application/vnd.github+json", path]
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
        raise IngestUnavailable(f"gh api {path!r} failed: {exc}") from exc
    if out.returncode != 0:
        raise IngestUnavailable(
            f"gh api {path!r} rc={out.returncode}: {out.stderr.strip()[:200]}"
        )
    try:
        value = json.loads(out.stdout)
    except json.JSONDecodeError as exc:
        raise IngestUnavailable(f"gh api {path!r} returned malformed JSON") from exc
    if not isinstance(value, dict):
        raise IngestUnavailable(
            f"gh api {path!r} returned {type(value).__name__}, expected an object"
        )
    if not value:
        raise IngestUnavailable(f"gh api {path!r} returned an empty object")
    return value


def required_list(payload: dict, field: str, *, source: str) -> list[dict]:
    value = payload.get(field)
    if not isinstance(value, list) or not all(isinstance(row, dict) for row in value):
        raise IngestUnavailable(
            f"{source} field {field!r} is not a list of objects"
        )
    return value


# ---------------------------------------------------------------------------
# Time helpers
# ---------------------------------------------------------------------------

def _epoch(s: str | None) -> float | None:
    if not s:
        return None
    try:
        return dt.datetime.strptime(s[:19], "%Y-%m-%dT%H:%M:%S").replace(
            tzinfo=dt.timezone.utc).timestamp()
    except ValueError:
        return None


def _delta_s(later: str | None, earlier: str | None) -> str:
    a, b = _epoch(later), _epoch(earlier)
    if a is None or b is None:
        return ""
    d = a - b
    return "" if d < 0 else f"{d:.0f}"


# ---------------------------------------------------------------------------
# (A) GitHub Actions runs store
# ---------------------------------------------------------------------------

def load_store(path: str) -> dict[gha_rows.RunKey, gha_rows.GhaRunRow]:
    return gha_rows.load_run_store(path)


def write_store(
    path: str, rows: dict[gha_rows.RunKey, gha_rows.GhaRunRow]
) -> None:
    ordered = sorted(rows.values(),
                     key=lambda r: (r.get("repo", ""), r.get("created_at", ""),
                                    r.get("run_id", ""),
                                    r.get("run_attempt", "")))
    gha_rows.write_run_rows(path, ordered)


def load_cursor(path: str) -> dict:
    if not os.path.isfile(path):
        return {}
    try:
        with open(path) as fh:
            return json.load(fh)
    except (OSError, json.JSONDecodeError):
        return {}


def save_cursor(path: str, cursor: dict) -> None:
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(cursor, fh, indent=2, sort_keys=True)
    os.replace(tmp, path)


def run_to_row(repo: str, run: dict) -> gha_rows.GhaRunRow:
    prs = "|".join(str(pr.get("number")) for pr in (run.get("pull_requests") or [])
                   if pr.get("number") is not None)
    created = run.get("created_at")
    started = run.get("run_started_at")
    updated = run.get("updated_at")
    completed = run.get("status") == TERMINAL_STATUS
    return gha_rows.run_row({
        "repo": repo,
        "run_id": str(run.get("id", "")),
        "run_attempt": str(run.get("run_attempt", "")),
        "workflow_id": str(run.get("workflow_id", "")),
        "workflow_name": run.get("name") or "",
        "event": run.get("event") or "",
        "head_branch": run.get("head_branch") or "",
        "head_sha": run.get("head_sha") or "",
        "pull_requests": prs,
        "status": run.get("status") or "",
        "conclusion": run.get("conclusion") or "",
        "created_at": created or "",
        "run_started_at": started or "",
        "updated_at": updated or "",
        "queue_s": _delta_s(started, created),
        # RUN_s only meaningful once the run is terminal.
        "run_s": _delta_s(updated, started) if completed else "",
        "html_url": run.get("html_url") or "",
        "display_title": (run.get("display_title") or "")[:200],
        # GitHub's run object does not carry job execution time. Preserve a
        # previous derivation during upsert; ingest_jobs fills these from the
        # attempt-scoped job rows.
        "runner_s": "",
        "jobs_n": "",
        "jobs_src": "",
    })


def upsert(
    rows: dict[gha_rows.RunKey, gha_rows.GhaRunRow],
    row: gha_rows.GhaRunRow | dict,
) -> None:
    row = gha_rows.run_row(row)
    key = gha_rows.run_key(row)
    prev = rows.get(key)
    # Keep whichever observation is newest by updated_at (ISO UTC sorts lexically).
    if prev is None or row.get("updated_at", "") >= prev.get("updated_at", ""):
        # The run API has no job-duration fields. A tail refresh must not erase
        # a derivation already made from gha-jobs.csv, especially under
        # --no-jobs where no later pass would restore it.
        if prev is not None and not row["jobs_src"] and prev["jobs_src"]:
            row["runner_s"] = prev["runner_s"]
            row["jobs_n"] = prev["jobs_n"]
            row["jobs_src"] = prev["jobs_src"]
        rows[key] = row


def _iso(ts: float) -> str:
    return dt.datetime.fromtimestamp(ts, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _fetch_window_page(repo: str, start: float, end: float, page: int) -> list[dict]:
    window = f"{_iso(start)}..{_iso(end)}"
    q = f"per_page={PER_PAGE}&page={page}&created={window}"
    payload = gh_json(f"repos/{repo}/actions/runs?{q}")
    return required_list(payload, "workflow_runs", source="workflow-runs response")


def fetch_window(repo: str, start: float, end: float, rows: dict[tuple, dict],
                 stat: dict) -> None:
    """Fetch every run created in [start,end], bisecting on 1000-result saturation.

    GitHub caps a single `created` query at 1000 results, so a window that fills
    all pages is split in time and re-fetched. Bisection boundaries overlap by a
    second but UPSERT is idempotent, so nothing is double-counted.
    """
    if end < start:
        return
    page_runs: list[dict] = []
    saturated = False
    for page in range(1, RESULT_CAP_PAGES + 1):
        batch = _fetch_window_page(repo, start, end, page)
        stat["api_calls"] += 1
        if not batch:
            break
        page_runs.extend(batch)
        if len(batch) < PER_PAGE:
            break
        if page == RESULT_CAP_PAGES:
            saturated = True
    if saturated and (end - start) > MIN_WINDOW_S:
        mid = start + (end - start) / 2.0
        fetch_window(repo, mid, end, rows, stat)
        fetch_window(repo, start, mid, rows, stat)
        return
    if saturated:
        sys.stderr.write(
            f"ci-hub ingest [{repo}]: WARNING window {_iso(start)}..{_iso(end)} "
            f"still saturated at {MIN_WINDOW_S}s granularity; some runs may be "
            f"unreachable via the runs API (>1000 in <{MIN_WINDOW_S}s)\n")
    for run in page_runs:
        upsert(rows, run_to_row(repo, run))
        stat["seen"] += 1
        created = run.get("created_at") or ""
        if created > stat["newest_created"]:
            stat["newest_created"] = created
        stat["max_run_id"] = max(stat["max_run_id"], int(run.get("id", 0) or 0))


def recheck_open_runs(repo: str, rows: dict[tuple, dict],
                      before_iso: str | None = None) -> int:
    """Re-fetch stored non-terminal runs by id and upsert their outcome.

    The window fetch already refreshed every run created within [start, now], so
    only re-check open runs OLDER than the window start (`before_iso`); otherwise
    every queued/in_progress run in a busy window would cost one extra API call
    per run on each incremental pass.
    """
    open_ids = sorted({r["run_id"] for k, r in rows.items()
                       if k[0] == repo and r.get("status") != TERMINAL_STATUS
                       and r.get("run_id")
                       and (before_iso is None
                            or (r.get("created_at") or "") < before_iso)})
    promoted = 0
    for run_id in open_ids:
        run = gh_json(f"repos/{repo}/actions/runs/{run_id}")
        upsert(rows, run_to_row(repo, run))
        if run.get("status") == TERMINAL_STATUS:
            promoted += 1
    return promoted


def _window_start(mode: str, cursor: dict, repo: str, since: str | None,
                  overlap_hours: float, now: float) -> float:
    """Resolve the [start] of the fetch window for the chosen mode."""
    if since:
        s = _epoch(since if "T" in since else since[:10] + "T00:00:00Z")
        if s is not None:
            return s
    if mode == "full":
        return now - FULL_MAX_DAYS * 86400
    # incremental: resume from the cursor minus the overlap re-check window.
    anchor = _epoch(cursor.get(repo, {}).get("last_created_at"))
    if anchor is None:
        return now - DEFAULT_INCR_DAYS * 86400
    return anchor - overlap_hours * 3600


def ingest_runs(repo: str, parent: str, *, full: bool, since: str | None,
                overlap_hours: float) -> None:
    store = gha_store_path(parent)
    curp = cursor_path(parent)
    rows = load_store(store)
    cursor = load_cursor(curp)
    before = len(rows)

    now = dt.datetime.now(dt.timezone.utc).timestamp()
    mode = "full" if full else "incremental"
    start = _window_start(mode, cursor, repo, since, overlap_hours, now)
    stat = {"seen": 0, "api_calls": 0,
            "newest_created": cursor.get(repo, {}).get("last_created_at", ""),
            "max_run_id": int(cursor.get(repo, {}).get("max_run_id", 0) or 0)}
    fetch_window(repo, start, now, rows, stat)
    promoted = recheck_open_runs(repo, rows, before_iso=_iso(start))

    cursor[repo] = {"last_created_at": stat["newest_created"],
                    "max_run_id": stat["max_run_id"]}
    write_store(store, rows)
    save_cursor(curp, cursor)
    added = len(rows) - before
    print(f"ci-hub ingest [{repo}]: fetched {stat['seen']} runs ({mode}, "
          f"{stat['api_calls']} api calls from {_iso(start)}), +{added} new rows, "
          f"{promoted} promoted to terminal; store now {len(rows)} rows -> {store}")


# ---------------------------------------------------------------------------
# (B) Per-node CI profiling artifacts (ci-perf-*)
# ---------------------------------------------------------------------------

def _downloaded_manifest(parent: str) -> str:
    return os.path.join(profiles_dir(parent), "downloaded.json")


def ingest_profiles(repo: str, parent: str, max_pages: int = PROFILES_MAX_PAGES) -> None:
    dest_root = os.path.join(profiles_dir(parent), repo.replace("/", "__"))
    os.makedirs(dest_root, exist_ok=True)
    manifest_path = _downloaded_manifest(parent)
    done = set()
    if os.path.isfile(manifest_path):
        try:
            with open(manifest_path) as fh:
                done = set(json.load(fh))
        except (OSError, json.JSONDecodeError):
            done = set()

    # The artifacts API returns newest-first. ci-perf-* artifacts (producer:
    # hermit PR #1548) are recent, and already-downloaded ones are skipped via
    # the manifest, so we scan only the newest `max_pages` pages by default
    # rather than paging thousands of unrelated artifacts every refresh.
    artifacts: list[dict] = []
    for page in range(1, max_pages + 1):
        payload = gh_json(f"repos/{repo}/actions/artifacts?per_page=100&page={page}")
        batch = required_list(payload, "artifacts", source="artifacts response")
        if not batch:
            break
        artifacts.extend(batch)
        if len(batch) < 100:
            break

    perf = [a for a in artifacts
            if str(a.get("name", "")).startswith("ci-perf-")
            and not a.get("expired")]
    fetched = 0
    for art in perf:
        art_id = art.get("id")
        tag = f"{repo}:{art_id}"
        if art_id is None or tag in done:
            continue
        out_dir = os.path.join(dest_root, str(art_id))
        if _download_artifact(repo, art_id, out_dir):
            done.add(tag)
            fetched += 1

    with open(manifest_path, "w") as fh:
        json.dump(sorted(done), fh, indent=0)
    print(f"ci-hub ingest [{repo}]: ci-perf artifacts: {len(perf)} present, "
          f"{fetched} newly downloaded -> {dest_root}")


def _download_artifact(repo: str, art_id: int, out_dir: str) -> bool:
    """Download+unzip one artifact via `gh api ... > zip`; keep only its CSVs."""
    with tempfile.NamedTemporaryFile(suffix=".zip", delete=False) as tf:
        zip_path = tf.name
    try:
        cmd = gh_prefix() + ["api",
                             f"repos/{repo}/actions/artifacts/{art_id}/zip"]
        with open(zip_path, "wb") as zf:
            proc = subprocess.run(cmd, stdout=zf, stderr=subprocess.PIPE,
                                  timeout=300)
        if proc.returncode != 0:
            sys.stderr.write(f"ci-hub ingest: artifact {art_id} download "
                             f"rc={proc.returncode}: {proc.stderr[:200]!r}\n")
            return False
        os.makedirs(out_dir, exist_ok=True)
        with zipfile.ZipFile(zip_path) as zf:
            for name in zf.namelist():
                if name.endswith(".csv"):
                    target = os.path.join(out_dir, os.path.basename(name))
                    with zf.open(name) as src, open(target, "wb") as dst:
                        shutil.copyfileobj(src, dst)
        return True
    except (zipfile.BadZipFile, subprocess.TimeoutExpired, OSError) as exc:
        sys.stderr.write(f"ci-hub ingest: artifact {art_id} unzip failed: {exc}\n")
        return False
    finally:
        try:
            os.unlink(zip_path)
        except OSError:
            pass


# ---------------------------------------------------------------------------
# Local validate-run history refresh (build on aggregate.py, do not duplicate)
# ---------------------------------------------------------------------------
# (C) Per-JOB store for the seventh-case ordering discriminator
#
# query.py's green-time metric has a seventh case: a run whose RUN-LEVEL
# conclusion is `cancelled` but which contains a JOB that FAILED first. The
# discriminator is ORDERING (a red job that completed at/before the run's cancel
# moment failed on its own -> real red; one killed by the cancel stays
# no_result), and it is INERT until this per-job store exists. We only ever need
# job rows for the runs that can actually trigger the case: cancelled runs of an
# AUTHORITATIVE workflow on main. Fetching jobs for every run would be a large,
# needless multiplier on the API, so the default remains scoped to those runs.
# `--jobs-scope all` widens collection for runner_s measurement.
# ---------------------------------------------------------------------------

# Authoritative main-branch workflow per repo. MUST stay in lockstep with
# query.py AUTHORITATIVE (the green-time consumer); kept as a local copy rather
# than an import so the stores stay joinable by file-contract only.
AUTHORITATIVE_WORKFLOWS = {
    "rrnewton/hermit": ["CI (GitHub-managed portable)"],
    "rrnewton/reverie": ["Rust"],
}

JOBS_MAX_RUNS = 400  # per-pass cap on runs fetched (logged if hit)

def load_jobs_store(path: str) -> dict[gha_rows.JobKey, gha_rows.GhaJobRow]:
    return gha_rows.load_job_store(path)


def write_jobs_store(
    path: str, rows: dict[gha_rows.JobKey, gha_rows.GhaJobRow]
) -> None:
    ordered = sorted(rows.values(),
                     key=lambda r: (r.get("repo", ""), r.get("run_id", ""),
                                    r.get("run_attempt", ""),
                                    r.get("job_id", "")))
    gha_rows.write_job_rows(path, ordered)


def job_to_row(repo: str, job: dict) -> gha_rows.GhaJobRow:
    return gha_rows.job_row({
        "repo": repo,
        "run_id": str(job.get("run_id", "")),
        "run_attempt": str(job.get("run_attempt", "")),
        "job_id": str(job.get("id", "")),
        "name": job.get("name") or "",
        "status": job.get("status") or "",
        "conclusion": job.get("conclusion") or "",
        "started_at": job.get("started_at") or "",
        "completed_at": job.get("completed_at") or "",
        "html_url": job.get("html_url") or "",
    })


def augment_runs_with_jobs(
    run_rows: dict[gha_rows.RunKey, gha_rows.GhaRunRow],
    job_rows: dict[gha_rows.JobKey, gha_rows.GhaJobRow],
    fetched_attempts: set[gha_rows.RunAttemptKey] | None = None,
) -> int:
    """Derive aggregate runner seconds from jobs of the same run attempt.

    `run_s` remains end-to-end wall time. `runner_s` sums every executed job's
    own interval and can therefore exceed `run_s` when jobs run in parallel.
    A fetched attempt with no executable interval is absent with blank values;
    an attempt that was never fetched remains blank/never-measured. Neither is
    reported as zero work.
    """
    return gha_rows.apply_job_measurements(
        run_rows, job_rows, fetched_attempts or set()
    )


def cancelled_authoritative_runs(parent: str, repo: str,
                                 workflows: list[str],
                                 since: str | None) -> list[str]:
    """run_ids of cancelled authoritative-workflow runs on main (case-7 candidates)."""
    return list(dict.fromkeys(
        run_id for run_id, _ in job_candidate_attempts(
            parent, repo, workflows, since, scope="cancelled"
        )
    ))


JOBS_SCOPES = ("cancelled", "all")


def job_candidate_attempts(
    parent: str,
    repo: str,
    workflows: list[str],
    since: str | None,
    *,
    scope: str,
) -> list[tuple[str, str]]:
    """Return `(run_id, run_attempt)` candidates, newest first."""
    if scope not in JOBS_SCOPES:
        raise ValueError(f"unknown jobs scope {scope!r}; expected one of {JOBS_SCOPES}")
    path = gha_store_path(parent)
    want = set(workflows)
    since_iso = None
    if since:
        since_iso = since if "T" in since else since[:10] + "T00:00:00Z"
    selected: list[gha_rows.GhaRunRow] = []
    for row in gha_rows.read_run_rows(path):
        if row["repo"] != repo or row["workflow_name"] not in want:
            continue
        if scope == "cancelled":
            if row["head_branch"] != "main" or row["conclusion"].lower() != "cancelled":
                continue
        elif row["status"] != TERMINAL_STATUS:
            continue
        if since_iso and row["created_at"] < since_iso:
            continue
        selected.append(row)
    selected.sort(key=gha_rows.workflow_attempt_order, reverse=True)
    return [(row["run_id"], row["run_attempt"]) for row in selected]


def fetch_jobs(repo: str, run_id: str) -> list[dict]:
    """All attempts' jobs of a run via the paginated jobs endpoint.

    GitHub defaults this endpoint to the latest attempt. The stored run key and
    every job row both carry `run_attempt`, so accepting that default would make
    an earlier run row borrow a later attempt's jobs.
    """
    jobs: list[dict] = []
    for page in range(1, RESULT_CAP_PAGES + 1):
        payload = gh_json(
            f"repos/{repo}/actions/runs/{run_id}/jobs"
            f"?per_page={PER_PAGE}&page={page}&filter=all")
        batch = required_list(payload, "jobs", source="jobs response")
        jobs.extend(batch)
        if len(batch) < PER_PAGE:
            break
    return jobs


def jobs_fetched_path(parent: str) -> str:
    """Sidecar recording which run attempts have had jobs fetched.

    Job rows alone cannot mark an attempt "done": a run cancelled while still
    queued has zero jobs. Values therefore use `run_id:run_attempt`. A legacy
    bare run ID is migrated only when the run store has exactly one candidate
    attempt; multi-attempt runs are refreshed with `filter=all`.
    """
    return os.path.join(store_dir(parent), "gha-jobs-fetched.json")


def load_jobs_fetched(parent: str) -> dict:
    return load_cursor(jobs_fetched_path(parent))  # same {repo: [...]} json shape


def save_jobs_fetched(parent: str, data: dict) -> None:
    save_cursor(jobs_fetched_path(parent), data)


def _fetched_attempt(run_id: str, run_attempt: str) -> str:
    return f"{run_id}:{run_attempt}"


def ingest_jobs(repo: str, parent: str, *, workflows: list[str],
                since: str | None, refetch: bool, max_runs: int,
                scope: str = "cancelled") -> None:
    path = jobs_store_path(parent)
    rows = load_jobs_store(path)
    before = len(rows)
    run_path = gha_store_path(parent)
    run_rows = load_store(run_path)
    candidate_attempts = job_candidate_attempts(
        parent, repo, workflows, since, scope=scope
    )
    # The old sidecar recorded only run_id. It can be migrated without an API
    # call when the run store has exactly one candidate attempt: under the old
    # latest-attempt API default, that one is necessarily what was fetched. A
    # multi-attempt run is not inferable and is fetched again with filter=all.
    # Job rows themselves prove their exact attempts; the sidecar is needed for
    # attempts that returned zero jobs.
    fetched = load_jobs_fetched(parent)
    known = gha_rows.fetched_attempt_keys(fetched, run_rows, rows)
    have = {
        _fetched_attempt(run_id, run_attempt)
        for observed_repo, run_id, run_attempt in known
        if observed_repo == repo
    }
    attempts_by_run: dict[str, set[str]] = {}
    for run_id, run_attempt in candidate_attempts:
        attempts_by_run.setdefault(run_id, set()).add(run_attempt)
    todo_attempts = [
        pair for pair in candidate_attempts
        if refetch or _fetched_attempt(*pair) not in have
    ]
    todo = list(dict.fromkeys(run_id for run_id, _ in todo_attempts))
    truncated = 0
    if len(todo) > max_runs:
        truncated = len(todo) - max_runs
        todo = todo[:max_runs]
    api_calls = 0
    for run_id in todo:
        for job in fetch_jobs(repo, run_id):
            row = job_to_row(repo, job)
            rows[gha_rows.job_key(row)] = row
        for run_attempt in attempts_by_run.get(run_id, set()):
            have.add(_fetched_attempt(run_id, run_attempt))
        api_calls += 1
    fetched[repo] = sorted(have)
    save_jobs_fetched(parent, fetched)
    write_jobs_store(path, rows)
    known = gha_rows.fetched_attempt_keys(fetched, run_rows, rows)
    augment_runs_with_jobs(run_rows, rows, known)
    if run_rows:
        write_store(run_path, run_rows)
    added = len(rows) - before
    label = "cancelled authoritative-main" if scope == "cancelled" \
        else "terminal authoritative"
    msg = (f"ci-hub ingest [{repo}]: jobs for {len(todo)} {label} "
           f"runs ({api_calls} api calls), +{added} job rows; "
           f"store now {len(rows)} rows -> {path}")
    repo_rows = [row for row in run_rows.values() if row["repo"] == repo]
    derived = sum(row["jobs_src"] == JOBS_SRC_DERIVED for row in repo_rows)
    absent = sum(row["jobs_src"] == JOBS_SRC_ABSENT for row in repo_rows)
    unmeasured = len(repo_rows) - derived - absent
    msg += (f"; runner_s measured={derived}, absent={absent}, "
            f"never-measured={unmeasured}")
    if truncated:
        msg += (f"  [CAPPED: {truncated} more candidate runs not fetched this "
                f"pass; raise --jobs-max-runs or re-run to continue]")
    print(msg)


def jobs_store_path(parent: str) -> str:
    return os.path.join(store_dir(parent), "gha-jobs.csv")


# ---------------------------------------------------------------------------

def refresh_local(parent: str) -> None:
    agg = os.path.join(parent, "ci-hub", "validate", "aggregate.py")
    if not os.path.isfile(agg):
        return
    try:
        subprocess.run([sys.executable, agg, "--write-global"],
                       cwd=parent, timeout=300, check=False)
    except (subprocess.TimeoutExpired, OSError) as exc:
        sys.stderr.write(f"ci-hub ingest: local aggregate refresh failed: {exc}\n")


# ---------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", action="append",
                    help="OWNER/REPO to ingest (repeatable; default hermit+reverie)")
    ap.add_argument("--full", action="store_true",
                    help="backfill full history (recursive time-window fetch)")
    ap.add_argument("--since", metavar="YYYY-MM-DD",
                    help="bound ingestion to runs created on/after this UTC date")
    ap.add_argument("--overlap-hours", type=float, default=DEFAULT_OVERLAP_HOURS,
                    help="incremental tail re-check window (default 12h)")
    ap.add_argument("--profiles-max-pages", type=int, default=PROFILES_MAX_PAGES,
                    help="newest artifact pages scanned for ci-perf-* "
                         f"(default {PROFILES_MAX_PAGES}; raise for a full backfill)")
    ap.add_argument("--no-profiles", action="store_true",
                    help="skip ci-perf artifact download")
    ap.add_argument("--no-jobs", action="store_true",
                    help="skip per-job store (seventh-case discriminator source)")
    ap.add_argument("--refetch-jobs", action="store_true",
                    help="re-fetch jobs for selected runs already in the store")
    ap.add_argument("--jobs-max-runs", type=int, default=JOBS_MAX_RUNS,
                    help=f"per-pass cap on runs fetched for jobs "
                         f"(default {JOBS_MAX_RUNS})")
    ap.add_argument("--jobs-scope", choices=JOBS_SCOPES, default="cancelled",
                    help="'cancelled' keeps the case-7 set; 'all' fetches every "
                         "terminal authoritative run for runner_s measurement")
    ap.add_argument("--jobs-workflow", action="append",
                    help="authoritative workflow for job scoping (repeatable; "
                         "default per-repo AUTHORITATIVE_WORKFLOWS)")
    ap.add_argument("--no-local", action="store_true",
                    help="skip local validate-run aggregate refresh")
    args = ap.parse_args()

    parent = parent_root()
    repos = args.repo or DEFAULT_REPOS
    try:
        for repo in repos:
            ingest_runs(repo, parent, full=args.full, since=args.since,
                        overlap_hours=args.overlap_hours)
            if not args.no_profiles:
                ingest_profiles(repo, parent, max_pages=args.profiles_max_pages)
            if not args.no_jobs:
                wf = args.jobs_workflow or AUTHORITATIVE_WORKFLOWS.get(repo, [])
                if wf:
                    ingest_jobs(repo, parent, workflows=wf, since=args.since,
                                refetch=args.refetch_jobs, max_runs=args.jobs_max_runs,
                                scope=args.jobs_scope)
    except IngestUnavailable as exc:
        print("state=indeterminate")
        print(f"summary=ci-hub history ingest unavailable: {exc}")
        return 2
    if not args.no_local:
        refresh_local(parent)
    return 0


if __name__ == "__main__":
    sys.exit(main())
