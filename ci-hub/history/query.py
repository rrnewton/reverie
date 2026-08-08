#!/usr/bin/env python3
"""Read side of the ci-hub CI history store.

Reads only the file-contract stores that ingest.py maintains
(ignored/ci-hub/gha-runs.csv, downloaded ci-perf step_profiles CSVs, and local
.safe-ci-dag-runner/profiles/ CSVs). No GitHub access; no imports of ingest.py
internals.

Commands:
  (default)          summary (queue vs run-time shape) PLUS the K most-recent
                     individual runs, so a bare `ci-hub history` shows both the
                     distribution and the runs behind it. `--since` works here.
  node-cpu-budgets   per DAG-node CPU-second budgets for the cpu_timeout
                     derivation (round(max_cpu_s * 1.5), n>=5 else thin/UNSET).
  green-time         % of conclusive authoritative-run wall-clock whose CI
                     conclusion was success; NO-RESULT/gap excluded and shown.
  runs               same summary + recent-runs listing (alias of the default).

Usage:
  ci-hub/history/query.py [--repo R] [--since DATE] [--branch B] [--status S]
                          [--limit K] [--summary-only] [--json]
  ci-hub/history/query.py node-cpu-budgets [--repo R] [--since SHA|DATE] [--format csv|json]
  ci-hub/history/query.py green-time [--repo R] [--since DATE] [--workflow NAME] [--format text|json]
  ci-hub/history/query.py runs [--repo R] [--since DATE] [--branch B] [--status S] [--limit K]

The `--since`/`--repo`/`--branch`/`--limit`/`--json` flag names deliberately match
`ci-hub local-history` so the two history subcommands do not diverge. Every view
reads the SAME ignored/ci-hub/gha-runs.csv store, so this listing and any other
consumer of the store (e.g. queued-run analyses) report the same numbers.
"""
from __future__ import annotations

import argparse
import csv as csvmod
import datetime as dt
import json
import math
import os
import re
import sys
from pathlib import Path

CI_HUB = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(CI_HUB))

from check_outcome import (
    CheckOutcome,
    FAIL_CONCLUSIONS,
    classify_check,
    select_latest_workflow_attempts,
)
import qualifying_receipt

SHA_RE = re.compile(r"^[0-9a-f]{7,40}$")

# Authoritative main-branch workflow per repo for the green-time metric.
# Override with --workflow (repeatable). These are the current fork gates; the
# hermit portable CI and the reverie Rust suite.
AUTHORITATIVE = {
    "rrnewton/hermit": ["CI (GitHub-managed portable)"],
    "rrnewton/reverie": ["Rust"],
}

# ---------------------------------------------------------------------------
# GREEN-TIME DEFINITION (stated + dated so the metric's basis is challengeable).
#
# A metric whose basis is unstated becomes a number nobody can challenge. This is
# the definition the code implements, as of GREEN_TIME_DEFINITION_DATE. Change
# the date whenever the definition below changes.
#
# GREEN IS A POSITIVE SUCCESS RECORD, NOT THE ABSENCE OF RED. The wall-clock
# timeline of main is partitioned into FOUR mutually exclusive states so the
# whole observation window is accounted for and no non-green time is ever
# silently credited as green:
#
#   green      the latest attempt of EVERY authoritative workflow at the current
#              main commit completed with success.
#   red        any authoritative workflow at that commit produced a genuine BAD
#              answer (see the seven-case table for exactly which sub-cases).
#   no_result  the latest attempt was an answer that was destroyed, withheld, or
#              caused by our own harness rather than the product code under test.
#              Never remediates by revert, never counts as green.
#   gap        no terminal authoritative answer exists for the commit yet: the
#              run is still pending/queued/in_progress ("pending"), or the
#              authoritative workflow has no record at all for that commit
#              ("no-record"). We cannot claim health, so it is NOT green.
#
# SEVEN-CASE TAXONOMY. "Not red" is not one thing. When you look closely, an
# authoritative-workflow observation falls into one of seven cases; each maps to
# exactly one of the four states above. The DISCRIMINATOR column is what tells
# two look-alike cases apart, and DERIVABLE says whether the current RUN-LEVEL
# store (gha-runs.csv) can see that discriminator offline:
#
#  # observation                     -> state       discriminator          derivable now?
#  1 all authoritative wf success       green        run.conclusion         YES
#  2 genuine failing TEST verdict       red          run.conclusion         YES
#  3 cancelled BELOW its timeout cap    no_result    check annotation       NO (needs
#      (supersede / manual / queue         (signal destroyed;               annotations;
#       cancel-in-progress)                re-dispatch)                      duration is
#                                                                           NOT a proxy)
#  4 cancelled AT its timeout cap       red          check annotation       NO (needs
#      (self-timeout kill: a hang           ("exceeded the maximum          annotations)
#       hit our timeout-minutes box)        execution time")
#  5 environmental / harness-caused     no_result    local-leg signature    PARTIAL (only on
#      (sandbox EPERM, cold-build           (protocol.py); on the           the local-validate
#       link flake, inner-MemoryMax         GitHub leg it presents          leg; the GitHub
#       OOM, PID-sandbox timeout)           as bare `failure`)              leg cannot split it)
#  6 no run of the authoritative wf,    gap          run absent / status    YES
#      or run still pending/queued         !completed
#  7 run-level CANCELLED but a JOB      red          ORDERING: the job's    NO (needs
#      inside it FAILED first               red conclusion completed        job-level rows;
#      (run-level and job-level             at/before the run's cancel      run-level store
#       conclusions DISAGREE)               moment -> failed on its own     has no per-job
#                                           -> real red; killed BY the      conclusion)
#                                           cancel -> stays no_result
#
# HEADLINE DENOMINATOR. Green-time means green wall-hours divided by CONCLUSIVE
# AUTHORITATIVE-RUN wall-hours: green / (green + red). NO_RESULT and gap hours
# remain measured and reported, but are EXCLUDED from that denominator. A
# cancelled run therefore cannot wear a red badge merely because it produced no
# answer, and cannot depress the percentage as if it had disproved the code.
# Cases 4 and 7 enter the denominator only after their independent failure is
# positively established. The taxonomy therefore drives both the action and
# whether an interval is eligible for the headline denominator:
#    red       -> a genuine bad answer: fix-forward or revert.
#    no_result -> a destroyed/withheld/harness-caused answer: RE-DISPATCH, never
#                 revert a healthy tip.
#    gap       -> fill the hole (dispatch / wait for the pending run).
# The un-derivable splits (3-vs-4, 5, 7) are all CONSERVATIVE for green: an
# undiscriminated case sits in no_result, so the worst offline error is
# UNDER-counting the conclusive denominator (a hidden failure reads as "no
# answer"). Case 5 on the GitHub leg is the converse over-count: a harness cause
# reads as red and enters the denominator until richer evidence is ingested.
#
# COMBINE + REIGN. A commit's state combines its authoritative workflows with
# precedence red > gap > no_result > green: one real failure dominates; absent
# that, a missing/pending answer blocks a green claim. Each main commit "reigns"
# from its first observed run creation until the next commit's first run creation
# (the last commit reigns to now). Within a reign, [became-head,
# all-authoritative-terminal) is gap(pending) and the remainder takes the
# combined terminal state. green_pct = green wall-clock / (green + red)
# authoritative-run wall-clock. Separate *_window_pct fields account for the
# complete observation window; no_result and gap never enter green_pct.
#
# TRINARY / FLAKY: green_pct is the exact fraction of CONCLUSIVE authoritative
# wall-time in the unambiguous green state. Anything below 100% is genuine red
# time in that same denominator. NO_RESULT and gap are not healthy; they are
# excluded unknowns and are always printed beside the figure with the full
# window so 100% over a thin denominator cannot masquerade as full coverage.
#
# OFFLINE LIMITS (stated, not hidden — query.py never touches the network):
#  * cases 3/4: cancelled cannot be split supersede-vs-self-timeout offline
#    (needs check annotations; duration is NOT a discriminator — a supersede was
#    measured cancelled 4s under a 300s cap). ALL cancelled -> no_result here
#    UNLESS case 7 promotes it; the self-timeout -> red promotion lives only in
#    the live github_main_health dashboard until annotations are ingested.
#  * case 5: environmental failures are only separable on the local-validate leg
#    (protocol.py); on the GitHub authoritative leg they present as `failure` and
#    are counted as red here.
#  * case 7: the seventh-case ORDERING discriminator (see _resolve_cancelled_run)
#    is LIVE whenever the per-job store gha-jobs.csv is present (produced by
#    ingest.py, scoped to cancelled authoritative-main runs). With no job store,
#    or for a run cancelled while still queued (zero jobs), every cancelled run
#    stays no_result (conservative).
#  * a commit with NO run of ANY workflow in the store is invisible (no reign
#    boundary); the store cannot see it. Commits with some-but-not-authoritative
#    runs ARE counted as gap(no-record).
GREEN_TIME_DEFINITION_DATE = "2026-08-07"

PRUNE = {"target", ".git", "node_modules", ".cargo", "incremental", "deps",
         "build", ".venv", "__pycache__"}


def parent_root() -> str:
    env = os.environ.get("DEV_HERMIT_PARENT")
    if env and os.path.isdir(env):
        return os.path.abspath(env)
    return os.path.abspath(os.path.join(os.path.dirname(__file__), "../.."))


def store_dir(parent: str) -> str:
    return os.path.join(parent, "ignored", "ci-hub")


def gha_store_path(parent: str) -> str:
    return os.path.join(store_dir(parent), "gha-runs.csv")


def jobs_store_path(parent: str) -> str:
    """Per-job store for the seventh-case ordering discriminator.

    File contract (PRODUCED by ingest.py's job ingester, scoped to cancelled
    authoritative-main runs): one row per job of a run, columns at least
    repo, run_id, job_id, name, conclusion, started_at, completed_at
    (mirrors `gh api repos/{repo}/actions/runs/{id}/jobs`). Joined to gha-runs by
    (repo, run_id). When absent -> the discriminator is inert (see
    _resolve_cancelled_run) and every cancelled run stays no_result.
    """
    return os.path.join(store_dir(parent), "gha-jobs.csv")


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

def _epoch(s: str | None) -> float | None:
    if not s:
        return None
    try:
        return dt.datetime.strptime(s[:19], "%Y-%m-%dT%H:%M:%S").replace(
            tzinfo=dt.timezone.utc).timestamp()
    except ValueError:
        return None


def _float(v) -> float | None:
    if v is None or v == "":
        return None
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def percentile(sorted_vals: list[float], p: float) -> float | None:
    if not sorted_vals:
        return None
    rank = max(1, math.ceil(p / 100.0 * len(sorted_vals)))
    return sorted_vals[rank - 1]


def walk_pruned(root: str, want, max_depth: int = 8):
    root = os.path.abspath(root)
    base_depth = root.rstrip("/").count("/")
    for dirpath, dirnames, filenames in os.walk(root):
        depth = dirpath.rstrip("/").count("/") - base_depth
        if depth >= max_depth:
            dirnames[:] = []
        dirnames[:] = [d for d in dirnames
                       if d not in PRUNE and not d.startswith(".safe-ci-dag-runner.")]
        for fn in filenames:
            if want(fn):
                yield os.path.join(dirpath, fn)


def load_gha_rows(parent: str, repo: str | None, since: str | None,
                  branch: str | None) -> list[dict]:
    path = gha_store_path(parent)
    if not os.path.isfile(path):
        return []
    rows = []
    with open(path, newline="", errors="replace") as fh:
        for row in csvmod.DictReader(fh):
            if repo and row.get("repo") != repo:
                continue
            if branch and row.get("head_branch") != branch:
                continue
            if since and (row.get("created_at") or "") < since:
                continue
            rows.append(row)
    return rows


# ---------------------------------------------------------------------------
# node-cpu-budgets
# ---------------------------------------------------------------------------

def discover_step_profiles(parent: str, repo: str | None) -> list[dict]:
    """Every step_profiles_*.csv row from local checkouts + downloaded ci-perf."""
    rows: list[dict] = []

    def is_step_csv(fn: str) -> bool:
        return fn.startswith("step_profiles_") and fn.endswith(".csv")

    # Local safe-ci-dag-runner profiling under any checkout.
    for path in walk_pruned(parent, is_step_csv):
        if "/.safe-ci-dag-runner/profiles/" not in path:
            continue
        _read_step_csv(path, rows, origin="local")

    # Downloaded GitHub ci-perf artifacts.
    gha_prof = os.path.join(store_dir(parent), "gha-profiles")
    if os.path.isdir(gha_prof):
        sub = gha_prof
        if repo:
            cand = os.path.join(gha_prof, repo.replace("/", "__"))
            sub = cand if os.path.isdir(cand) else gha_prof
        for path in walk_pruned(sub, is_step_csv):
            _read_step_csv(path, rows, origin="github")
    return rows


def _read_step_csv(path: str, rows: list[dict], origin: str) -> None:
    try:
        with open(path, newline="", errors="replace") as fh:
            for row in csvmod.DictReader(fh):
                row["_origin"] = origin
                rows.append(row)
    except OSError:
        pass


def _is_kill_sample(row: dict) -> bool:
    """A step_profiles row whose resource numbers are CAP-TRUNCATED by a kill —
    a wall/cpu-timeout reap or an OOM — rather than a measurement of the work.

    Such a row must NEVER anchor a CPU budget, because its max is the DEFECT, not
    the worst LEGITIMATE run. Concretely: a livelocked ``test.detcore_misc`` hits
    the 600s wall gate burning ~one core, so it records ``cpu ~= wall ~= 600s``;
    fed into ``round(max_cpu * 1.5)`` that derives a ~912s budget — generous
    enough that the very livelock it was calibrated on could never trip it. A raw
    cpu/wall RATIO cannot separate these: a HEALTHY multi-threaded detcore_misc
    run has ratio ~2.3 (cpu across threads > wall) while the livelock sits at
    ~1.0, so ratio alone would keep the poison and could drop the healthy sample.
    The robust discriminator is simply *the sample was a kill* — the exact
    ``valid_sample`` rule the breach-table derivation already uses
    (``experiments/breach-table-portable-dag_20260803``): ``ok == True`` AND not
    wall/cpu-timed-out AND ``oom_kills == 0``.

    Fields absent (older rows without kill columns) => not classifiable as a kill
    => kept: fail-safe toward retaining data, never toward fabricating a budget
    from a defect (a node whose only samples ARE kills falls to n=0 -> thin ->
    UNSET, never a poisoned number).
    """
    if _kill_kind(row) is not None:
        return True
    return (row.get("ok") or "").strip() == "False"


# Headroom over the worst VALID (non-kill) CPU-second observation. Named, not a
# bare literal, so the one judgement lives in one place (the -j default diverged
# across two call sites precisely because the number was duplicated).
CPU_BUDGET_HEADROOM = 1.5

# Canonical per-platform multiplier for an UNDERPOWERED HOSTED runner. A CPU
# SECOND is load-immune (wall = cpu_busy + wait; contention inflates only wait),
# but it is NOT clock-immune: a slower hosted core retires the same instruction
# stream over MORE seconds of CPU occupancy, so the same work legitimately burns
# more CPU-seconds there. Hence a per-platform multiplier on the CPU budget.
#
# This is a CANONICAL CONSTANT, NOT a measured ratio, and it is labelled that way
# on purpose: hosted step_profiles carry WALL ONLY (user_s/sys_s empty — the
# GitHub producer emits no CPU-seconds), so there is no hosted CPU-second
# distribution to derive a per-node multiplier from. Deriving one from
# hosted-wall / local-cpu would pair two different quantities — the exact
# proxy-binding error this store exists to avoid. When hosted CPU-second emission
# lands, replace this constant with a measured per-node ratio. Owner's guidance
# was x1.5-x2 on underpowered hosted runners; default to the safer x2 (an
# unmeasured factor should err toward NOT killing a healthy-but-slow hosted run).
HOSTED_CPU_MULTIPLIER = 2.0


def node_cpu_budgets(parent: str, repo: str | None, since: str | None,
                     min_samples: int,
                     hosted_multiplier: float = HOSTED_CPU_MULTIPLIER) -> list[dict]:
    profiles = discover_step_profiles(parent, repo)

    since_sha = since if since and SHA_RE.match(since) else None
    since_date = since if since and not since_sha else None

    by_node: dict[str, dict] = {}
    for r in profiles:
        node = (r.get("step") or "").strip()
        if not node:
            continue
        if since_sha and not (r.get("git_sha") or "").startswith(since_sha):
            continue
        if since_date and (r.get("timestamp") or "") < since_date:
            continue
        agg = by_node.setdefault(
            node, {"cpu": [], "wall": [], "rows": 0, "excluded_kill": 0})
        agg["rows"] += 1
        # A killed run's cpu/wall are cap artifacts, not a measurement of the
        # work; excluding them is what makes the derived budget load-immune AND
        # defect-immune (see _is_kill_sample). Count the exclusions so the drop
        # is visible in every render, never silent.
        if _is_kill_sample(r):
            agg["excluded_kill"] += 1
            continue
        user = _float(r.get("user_s"))
        sys_ = _float(r.get("sys_s"))
        wall = _float(r.get("elapsed_s"))
        if user is not None and sys_ is not None:
            agg["cpu"].append(user + sys_)
        if wall is not None:
            agg["wall"].append(wall)

    out = []
    for node, agg in sorted(by_node.items()):
        cpu = sorted(agg["cpu"])
        wall = sorted(agg["wall"])
        n = len(cpu)
        max_cpu = cpu[-1] if cpu else None
        thin = n < min_samples
        base = None if (thin or max_cpu is None) else max_cpu * CPU_BUDGET_HEADROOM
        suggested = None if base is None else int(round(base))
        # Hosted budget = canonical local budget x the (unmeasured) platform
        # multiplier; None whenever the base is UNSET so a defect/thin node never
        # emits a hosted number either.
        suggested_hosted = None if base is None else int(round(base * hosted_multiplier))
        out.append({
            "node": node,
            "n_samples": n,
            "n_rows": agg["rows"],
            "n_excluded_kill": agg["excluded_kill"],
            "max_cpu_s": round(max_cpu, 2) if max_cpu is not None else None,
            "p95_cpu_s": round(percentile(cpu, 95), 2) if cpu else None,
            "p50_cpu_s": round(percentile(cpu, 50), 2) if cpu else None,
            "max_wall_s": round(wall[-1], 2) if wall else None,
            "suggested_cpu_timeout": suggested,
            "suggested_cpu_timeout_hosted": suggested_hosted,
            "hosted_multiplier": hosted_multiplier,
            "thin": thin,
        })
    return out


def render_node_budgets(rows: list[dict], fmt: str) -> str:
    if fmt == "json":
        return json.dumps(rows, indent=2)
    if fmt == "csv":
        import io
        buf = io.StringIO()
        cols = ["node", "n_samples", "n_excluded_kill", "max_cpu_s", "p95_cpu_s",
                "p50_cpu_s", "max_wall_s", "suggested_cpu_timeout",
                "suggested_cpu_timeout_hosted", "thin"]
        w = csvmod.DictWriter(buf, fieldnames=cols, extrasaction="ignore")
        w.writeheader()
        for r in rows:
            w.writerow({c: ("" if r.get(c) is None else r.get(c)) for c in cols})
        return buf.getvalue().rstrip("\n")
    # text table. CPU/WALL columns are seconds; the header says so, so a bare
    # number is never unit-less (matches the JSON/CSV `_s` field names).
    hdr = ("NODE", "N", "EXCL_KILL", "MAX_CPU(s)", "P95_CPU(s)", "P50_CPU(s)",
           "MAX_WALL(s)", "CPU_TIMEOUT(s)", "HOSTED_TIMEOUT(s)", "THIN")
    body = [(r["node"], str(r["n_samples"]), str(r.get("n_excluded_kill", 0)),
             _s(r["max_cpu_s"]), _s(r["p95_cpu_s"]), _s(r["p50_cpu_s"]),
             _s(r["max_wall_s"]),
             "-" if r["suggested_cpu_timeout"] is None else str(r["suggested_cpu_timeout"]),
             "-" if r.get("suggested_cpu_timeout_hosted") is None else str(r["suggested_cpu_timeout_hosted"]),
             "thin" if r["thin"] else "") for r in rows]
    return _table(hdr, body)


def _s(v):
    return "-" if v is None else f"{v:g}"


def _table(hdr, body) -> str:
    widths = [len(h) for h in hdr]
    for row in body:
        for i, c in enumerate(row):
            widths[i] = max(widths[i], len(str(c)))
    fmt = "  ".join("{:<%d}" % w for w in widths)
    lines = [fmt.format(*hdr), fmt.format(*["-" * w for w in widths])]
    for row in body:
        lines.append(fmt.format(*[str(c) for c in row]))
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# kill taxonomy  (splits the green-time no_result bucket)
#
# A wall/cpu kill is not one thing.  It is either a LIVELOCK — CPU burned at
# ~a full core for the whole budget, a product defect that retry can NEVER fix —
# or CONTENTION — low CPU against high wall, the step was waiting, environmental,
# and a re-dispatch works.  Opposite causes, opposite correct responses, and
# only the cpu/wall RATIO at the kill separates them.  The producer already
# records that ratio: the safe-ci-dag-runner writes cpu (user_s+sys_s and cgroup
# cpu.usage_usec) alongside wall (elapsed_s) and the kill flags (timed_out /
# cpu_timed_out / oom_kills) into step_profiles for EVERY step it runs, pass or
# kill — so this consumer needs no new producer emit for DAG-runner nodes.
# ---------------------------------------------------------------------------

# cpu/wall thresholds.  On a multi-core box a genuinely blocked/contended step
# spends most of its wall waiting, so cpu/wall stays low; a spinning step pegs
# ~one core, so cpu ~= wall (the measured livelock signature was 599.986 cpu /
# 600.013 wall -> ratio ~= 1.0).  Ratios are reported alongside the verdict, not
# in place of it, so a reader can audit every boundary call.
# The thresholds and the verdict function now live in ONE shared table,
# ci-hub/lib/kill_signature.py, imported by BOTH this ledger-side consumer and
# the run-bundle attributor (ci-hub/attribution/attribution.py).  They were
# previously implemented here only, leaving the attributor blind to the signal;
# a second copy would have been a standing drift risk.  Change a threshold once,
# there.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "lib"))
import kill_signature as _ks  # noqa: E402

LIVELOCK_RATIO = _ks.LIVELOCK_RATIO
CONTENTION_RATIO = _ks.CONTENTION_RATIO


def _cpu_wall(row: dict):
    """(cpu_s, wall_s, ratio) for one step_profiles row.

    cpu prefers user_s+sys_s and falls back to cgroup cpu.usage_usec so a row
    carrying only the cgroup counter still yields a ratio.  ratio is None when
    wall is absent/zero — never divide by a missing denominator.
    """
    wall = _float(row.get("elapsed_s"))
    user = _float(row.get("user_s"))
    sys_ = _float(row.get("sys_s"))
    cpu = None
    if user is not None or sys_ is not None:
        cpu = (user or 0.0) + (sys_ or 0.0)
    else:
        usec = _float(row.get("cpu.usage_usec"))
        if usec is not None:
            cpu = usec / 1e6
    ratio = (cpu / wall) if (cpu is not None and wall not in (None, 0.0)) else None
    return cpu, wall, ratio


def _kill_kind(row: dict):
    """Which budget/killer fired, or None if the row is not a kill."""
    if (row.get("cpu_timed_out") or "").strip() == "True":
        return "cpu_timeout"
    if (row.get("timed_out") or "").strip() == "True":
        return "wall_timeout"
    oom = _float(row.get("oom_kills"))
    if oom and oom > 0:
        return "oom"
    return None


def _kill_verdict(kind, ratio):
    """Classify a kill.  OOM is a MEMORY kill, orthogonal to the cpu/wall spin
    question, so it gets its own bucket rather than being forced into
    livelock/contention.  For a time kill (cpu/wall timeout) the ratio decides:
    ~a full core burned (>=0.8) is a livelock (retry-futile); mostly waiting
    (<0.3) is contention (retry-valid).  A high ratio (>>1) is legitimately
    parallel CPU-bound work that also cannot be fixed by retry, so it still
    lands in the livelock (retry-futile) bucket alongside single-core spin."""
    return _ks.classify_kill(kind, ratio)


def _source(row: dict) -> str:
    """Which PATH produced this record — PROVENANCE, recorded per record.

    Both values below are safe-ci-dag-runner step profiles and both carry cpu,
    so both are classifiable — but they run in DIFFERENT environments (this box
    vs GitHub-hosted/self-hosted runners) with different contention, so their
    cpu/wall ratios must never be pooled into one distribution.

    The wall-only GitHub-*jobs* population (gha-jobs.csv, what green-time's
    no_result is built from) is a THIRD path that carries NO cpu field and is
    NEVER ingested here — kill-taxonomy reads step_profiles only, so it
    structurally cannot mix a classifiable population with an unclassifiable one.
    Recording the source per record is the prerequisite that lets a future
    green-time no_result split REFUSE to attach a runner-native verdict to a
    GitHub-jobs run that has no cpu: a split without this tag would silently mix
    two populations and produce a ratio that looks precise and means nothing.
    Provenance must exist BEFORE the split is enabled, not be retrofitted after.
    """
    o = (row.get("_origin") or "").strip()
    if o == "local":
        return "runner-native"
    if o == "github":
        return "github-ciperf"
    return o or "unknown"


def _empty_verdicts() -> dict:
    return {"livelock": 0, "contention": 0, "ambiguous": 0, "oom": 0,
            "unknown": 0}


def kill_taxonomy(parent: str, repo: str | None, since: str | None) -> dict:
    profiles = discover_step_profiles(parent, repo)
    since_sha = since if since and SHA_RE.match(since) else None
    since_date = since if since and not since_sha else None

    kills = []
    # keyed by (source, node): ratios from different producing paths are NOT
    # pooled — a node run both locally and on a GitHub runner is two populations.
    node_ratios: dict[tuple[str, str], list[float]] = {}
    for r in profiles:
        node = (r.get("step") or "").strip()
        if not node:
            continue
        if since_sha and not (r.get("git_sha") or "").startswith(since_sha):
            continue
        if since_date and (r.get("timestamp") or "") < since_date:
            continue
        src = _source(r)
        cpu, wall, ratio = _cpu_wall(r)
        if ratio is not None:
            node_ratios.setdefault((src, node), []).append(ratio)
        kind = _kill_kind(r)
        if kind is not None:
            kills.append({
                "node": node,
                "source": src,
                "git_sha": (r.get("git_sha") or "")[:12],
                "timestamp": r.get("timestamp") or "",
                "kill_kind": kind,
                "wall_s": round(wall, 3) if wall is not None else None,
                "cpu_s": round(cpu, 3) if cpu is not None else None,
                "cpu_wall_ratio": round(ratio, 3) if ratio is not None else None,
                "effective_cores": _float(r.get("effective_cores")),
                "verdict": _kill_verdict(kind, ratio),
            })

    summary = _empty_verdicts()
    # by_source makes the population mix VISIBLE: a reader (and any future split)
    # sees exactly how many kills came from each producing path, so a
    # mixed-population count can never masquerade as one clean number.
    by_source: dict[str, dict] = {}
    for k in kills:
        summary[k["verdict"]] += 1
        by_source.setdefault(k["source"], _empty_verdicts())[k["verdict"]] += 1

    node_stats = []
    for (src, node), ratios in sorted(node_ratios.items()):
        rs = sorted(ratios)
        node_stats.append({
            "node": node,
            "source": src,
            "n": len(rs),
            "p50_ratio": round(percentile(rs, 50), 3) if rs else None,
            "max_ratio": round(rs[-1], 3) if rs else None,
        })

    return {
        "repo": repo,
        "livelock_ratio": LIVELOCK_RATIO,
        "contention_ratio": CONTENTION_RATIO,
        "n_kills": len(kills),
        "summary": summary,
        "by_source": by_source,
        # highest ratio first: the most livelock-like kills lead.
        "kills": sorted(kills, key=lambda k: -(k["cpu_wall_ratio"] or -1.0)),
        "node_ratios": node_stats,
    }


def render_kill_taxonomy(res: dict, fmt: str) -> str:
    if fmt == "json":
        return json.dumps(res, indent=2)
    s = res["summary"]
    lines = [
        f"{res.get('repo') or 'all'} kill taxonomy "
        f"(cpu/wall >= {res['livelock_ratio']} = livelock [retry-futile], "
        f"< {res['contention_ratio']} = contention [retry-valid]): "
        f"{res['n_kills']} kills -> {s['livelock']} livelock, "
        f"{s['contention']} contention, {s['ambiguous']} ambiguous, "
        f"{s['oom']} oom, {s['unknown']} unknown"
    ]
    # population mix, so a ratio is never read as one clean number across paths.
    for src, sv in sorted(res.get("by_source", {}).items()):
        lines.append(
            f"  source={src}: {sv['livelock']} livelock, {sv['contention']} "
            f"contention, {sv['ambiguous']} ambiguous, {sv['oom']} oom, "
            f"{sv['unknown']} unknown"
        )
    if res["kills"]:
        hdr = ("NODE", "SOURCE", "SHA", "KILL", "WALL(s)", "CPU(s)", "CPU/WALL",
               "CORES", "VERDICT")
        body = [(k["node"], k.get("source", "unknown"), k["git_sha"],
                 k["kill_kind"], _s(k["wall_s"]), _s(k["cpu_s"]),
                 _s(k["cpu_wall_ratio"]), _s(k["effective_cores"]), k["verdict"])
                for k in res["kills"]]
        lines.append(_table(hdr, body))
    else:
        lines.append("  (no killed/timed-out/oom rows in the profile window)")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# green-time  (owner headline metric — derived, never estimated)
# ---------------------------------------------------------------------------

def _classify_terminal(conclusion: str) -> str:
    """A single completed run's terminal conclusion -> green/red/no_result."""
    outcome = classify_check("completed", conclusion)
    if outcome is CheckOutcome.PASSED:
        return "green"
    if outcome is CheckOutcome.FAILED:
        return "red"
    return "no_result"


def load_jobs_index(parent: str, repo: str | None) -> dict[str, list[dict]]:
    """run_id -> [job rows] from the optional gha-jobs.csv, or {} when absent."""
    path = jobs_store_path(parent)
    if not os.path.isfile(path):
        return {}
    idx: dict[str, list[dict]] = {}
    with open(path, newline="", errors="replace") as fh:
        for row in csvmod.DictReader(fh):
            if repo and row.get("repo") != repo:
                continue
            idx.setdefault(row.get("run_id") or "", []).append(row)
    return idx


def _resolve_cancelled_run(run: dict, jobs: list[dict] | None) -> str | None:
    """Seventh case: run-level CANCELLED, but a JOB inside it may have FAILED.

    Run-level and job-level conclusions disagree; the discriminator is ORDERING
    AND ROOT-CAUSE. A job whose conclusion is red (failure/timed_out/...) that
    completed at/before the cancel BEGAN, and that was not itself waiting on a
    cancelled dependency, failed on its own — the later run-level cancel only
    masked it — so the run is a real RED. A job killed BY the cancel, or one whose
    failure is PROPAGATED from a cancelled dependency, is not an independent
    verdict and stays no_result.

    Ordering reference: the CANCEL ONSET, not the run's terminal `updated_at`
    stamp. A cancel-in-progress kills the in-flight jobs, and a downstream
    aggregation gate (`needs:` all of them, "succeed or be deselected") then
    completes=failure BECAUSE a required dependency was cancelled — a PROPAGATED
    failure that finalizes at the run's cancel moment, not a product verdict.
    Ordering a red job against `updated_at` alone would flag that propagated gate
    RED (measured: it reproduces the run-30873193855 / hermit-238b false red —
    task cancellation_taxonomy_distinguish_self). So the reference is the earliest
    cancelled-sibling completion (when the cancel began killing jobs); a red job
    is INDEPENDENT only if it both COMPLETED and STARTED at/before that onset. A
    downstream gate STARTS only after its cancelled dependency resolves, so its
    start lands after the onset and it is correctly left as no_result even when
    second-granularity timestamps tie its completion with the onset. With no
    cancelled sibling the run was cancelled with nothing in flight, so the onset
    falls back to `updated_at`.

    Returns 'red' when a job failed independently of the cancel, else None (the
    caller keeps the run-level no_result classification). Inert when `jobs` is
    empty/None (no per-job store exists yet).
    """
    if not jobs:
        return None
    cancel_onsets = [
        _epoch(j.get("completed_at"))
        for j in jobs
        if (j.get("conclusion") or "").lower() == "cancelled"
    ]
    cancel_onsets = [c for c in cancel_onsets if c is not None]
    onset = min(cancel_onsets) if cancel_onsets else _epoch(run.get("updated_at"))
    for j in jobs:
        if (j.get("conclusion") or "").lower() not in FAIL_CONCLUSIONS:
            continue
        done = _epoch(j.get("completed_at"))
        if done is None:
            continue  # a red job with no completion time cannot be ordered
        if onset is None:
            return "red"
        # ORDERING + ROOT-CAUSE: the failure both finished and began at/before the
        # cancel onset -> it did not wait on a cancelled dependency -> independent.
        started = _epoch(j.get("started_at"))
        if done <= onset and (started is None or started <= onset):
            return "red"
    return None


def _combine_states(states: list[str]) -> str:
    """Combine per-workflow states with precedence red > gap > no_result > green.

    One genuine failure dominates; absent that, a missing/pending answer blocks a
    green claim; a destroyed answer (no_result) is preferred over green only when
    there is no gap. `states` is non-empty.
    """
    if "red" in states:
        return "red"
    if "gap" in states:
        return "gap"
    if "no_result" in states:
        return "no_result"
    return "green"


def _iso_utc(epoch: float) -> str:
    return dt.datetime.fromtimestamp(epoch, dt.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ")


def state_timeline(parent: str, repo: str, since: str | None,
                   workflows: list[str] | None) -> dict:
    """Partition main's wall-clock into (start, end, state, reason) intervals.

    See the GREEN-TIME DEFINITION block above. Purely store-derived: reign
    boundaries come from run creation times in gha-runs.csv, verdicts from the
    authoritative-workflow runs at each commit. Returns intervals plus metadata;
    ``green_time`` and the trend view both build on it.
    """
    wanted = list(workflows) if workflows else list(AUTHORITATIVE.get(repo, []))
    rows = load_gha_rows(parent, repo, since, branch="main")
    jobs_index = load_jobs_index(parent, repo)  # {} until gha-jobs.csv exists
    job_promotions = 0  # case-7 cancelled->red reclassifications (auditable)

    # Commit universe + reign boundary: first observed run creation for the
    # commit, across ANY workflow (so a commit with only non-authoritative runs
    # still bounds a reign and shows as gap(no-record) for the metric).
    first_seen: dict[str, float] = {}
    by_concl: dict[str, int] = {}
    for r in rows:
        sha = r.get("head_sha") or ""
        created = _epoch(r.get("created_at"))
        if created is not None:
            prev = first_seen.get(sha)
            if prev is None or created < prev:
                first_seen[sha] = created
    latest = {
        (str(run.get("head_sha") or ""), str(run.get("workflow_name") or "")): run
        for run in select_latest_workflow_attempts(rows, workflows=wanted)
    }

    if not first_seen:
        return {"repo": repo, "workflows": wanted, "intervals": [], "samples": 0,
                "note": "no main-branch runs in store"}

    now = dt.datetime.now(dt.timezone.utc).timestamp()
    order = sorted(first_seen, key=lambda s: first_seen[s])

    # OBSERVATION HORIZON. The store only has evidence up to its newest row.
    # Past that we know NOTHING -- ingest may simply not have run (egress down,
    # scheduler stopped). Extending the last commit's reign to `now` would
    # ATTRIBUTE UNOBSERVED TIME TO THE LAST KNOWN STATE, and when that state is
    # success it reports hours nobody measured as GREEN. That is the precise
    # defect this metric exists to avoid: absence of a record is not a green
    # period. Measured on 2026-08-06 before this cap: the trailing interval was
    # 58.81h -- 53.3% of the whole denominator -- entirely extrapolated.
    # So the timeline STOPS at the horizon and the remainder is gap(no-record).
    horizon = 0.0
    for r in rows:
        for field in ("created_at", "updated_at", "run_started_at"):
            t = _epoch(r.get(field))
            if t is not None and t > horizon:
                horizon = t
    if horizon <= 0:
        horizon = now
    observed_end = min(now, horizon)
    intervals: list[dict] = []
    for i, sha in enumerate(order):
        t0 = first_seen[sha]
        t1 = first_seen[order[i + 1]] if i + 1 < len(order) else observed_end
        # t1 == t0 is legitimate for the TIP once the horizon cap applies (its
        # reign has zero observed duration). Classify it anyway: a zero-length
        # interval adds nothing to the duration buckets (_sum_by_state skips
        # span<=0) but preserves the tip's verdict for `current_state`.
        # Dropping it would report the PREVIOUS commit as the current state.
        if t1 < t0:
            continue
        wf_states: list[str] = []
        terminal_times: list[float] = []
        any_pending = False
        any_no_record = False
        for wf in (wanted or [""]):
            run = latest.get((sha, wf))
            if run is None:
                wf_states.append("gap")
                any_no_record = True
            elif (run.get("status") or "") != "completed":
                wf_states.append("gap")
                any_pending = True
            else:
                c = (run.get("conclusion") or "").lower()
                state = _classify_terminal(c)
                # Seventh case: a run-level cancelled that actually masks a job
                # that failed first is a real red (ORDERING discriminator).
                if state == "no_result" and c == "cancelled":
                    if _resolve_cancelled_run(
                            run, jobs_index.get(run.get("run_id") or "")) == "red":
                        state = "red"
                        job_promotions += 1
                wf_states.append(state)
                tt = _epoch(run.get("updated_at"))
                if tt is not None:
                    terminal_times.append(tt)
                by_concl[c] = by_concl.get(c, 0) + 1
        gap_present = any_pending or any_no_record
        if gap_present or not terminal_times:
            reason = "pending" if any_pending else "no-record"
            intervals.append({"start": t0, "end": t1, "state": "gap",
                              "reason": reason, "sha": sha})
            continue
        # All authoritative workflows terminal: gap(pending) until the last one
        # completes, then the combined verdict. A green claim needs every answer
        # in, so the split anchors on the max terminal time.
        split = min(max(max(terminal_times), t0), t1)
        combined = _combine_states(wf_states)
        if split > t0:
            intervals.append({"start": t0, "end": split, "state": "gap",
                              "reason": "pending", "sha": sha})
        intervals.append({"start": split, "end": t1, "state": combined,
                          "reason": None, "sha": sha})

    # The unobserved tail is GAP, explicitly, with the horizon named in the
    # reason so a reader can tell "CI told us nothing" from "ingest stopped".
    stale_s = max(0.0, now - observed_end)
    if stale_s > 0:
        intervals.append({
            "start": observed_end, "end": now, "state": "gap",
            "reason": f"no-record: store has no observation after "
                      f"{_iso_utc(observed_end)}",
            "sha": None,
        })
    return {
        "repo": repo,
        "workflows": wanted,
        "intervals": intervals,
        "samples": len(order),
        "window_start": _iso_utc(first_seen[order[0]]),
        "window_end_utc": _iso_utc(now),
        "observed_through_utc": _iso_utc(observed_end),
        "store_stale_hours": round(stale_s / 3600.0, 2),
        "runs_by_conclusion": by_concl,
        "job_level_red_promotions": job_promotions,
        "current_sha": order[-1],
    }


def _sum_by_state(intervals: list[dict], lo: float | None = None,
                  hi: float | None = None) -> tuple[dict[str, float], float]:
    """Seconds per state over [lo, hi) (unbounded when lo/hi are None)."""
    buckets = {"green": 0.0, "red": 0.0, "no_result": 0.0, "gap": 0.0}
    total = 0.0
    for iv in intervals:
        s = iv["start"] if lo is None else max(iv["start"], lo)
        e = iv["end"] if hi is None else min(iv["end"], hi)
        span = e - s
        if span <= 0:
            continue
        buckets[iv["state"]] = buckets.get(iv["state"], 0.0) + span
        total += span
    return buckets, total


# ---------------------------------------------------------------------------
# LEDGER-CORROBORATED GREEN. A GitHub `success` conclusion and a local
# full-pass validate receipt are TWO DIFFERENT claims; conflating them is what
# produced a misleading green figure. So the green wall-clock is split into two
# sub-buckets, reported SEPARATELY and never silently summed:
#   green_ledger           green-by-conclusion AND a validate-run-ledger receipt
#                          at that exact commit SHA satisfies the full-pass
#                          predicate below (the canonical shared predicate also
#                          used by ci-hub/lib/validate_status.rs).
#   green_conclusion_only  green-by-conclusion but NO corroborating receipt.
# A row missing both count fields does NOT corroborate, and schema-5+ rows must
# prove complete per-node coverage. `filtered_tests` is diagnostic: full runs
# legitimately filter tests outside their planned DAG nodes.
LEDGER_REL = os.path.join("ignored", "validate-run-ledger.jsonl")


def load_ledger_index(parent: str) -> dict[str, list[dict]]:
    """commit SHA -> [ledger rows] from ignored/validate-run-ledger.jsonl.

    Absent file -> empty index -> every green falls to conclusion-only. One
    malformed JSONL line is skipped, not fatal (the ledger has many writers).
    """
    path = os.path.join(parent, LEDGER_REL)
    idx: dict[str, list[dict]] = {}
    if not os.path.isfile(path):
        return idx
    with open(path, errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                continue
            sha = row.get("commit")
            if sha:
                idx.setdefault(sha, []).append(row)
    return idx


def _row_full_pass(row: dict) -> bool:
    """The full-pass corroboration predicate, delegated to the ONE shared
    qualifying-receipt predicate (`ci-hub/validate/qualifying-receipt.json`) that
    every consumer across Rust/Python/jq reads -- restating the clauses inline is
    exactly the drift this delegation removes (task
    `one-shared-qualifying-receipt-predicate-five-consumers-bypass-the-registry`).

    This previously had NO per-node coverage clause and so would have accepted a
    count-capable receipt whose coverage was incomplete (e.g. ee303899: 8 PASS
    rows at executed=427 but 15 absent nodes) -- the DRIFT-4 gap the shared
    predicate closes. The exact commit binding is handled by the index lookup, so
    the predicate is asked about the row's own recorded commit."""
    return qualifying_receipt.authoritative_row_qualifies(
        row, row.get("commit") or "", qualifying_receipt.active()
    )


def _ledger_corroborates(idx: dict[str, list[dict]], sha: str) -> bool:
    """True iff ANY ledger row for `sha` satisfies the full-pass predicate.

    Prefers an exact 40-hex commit match; defensively also accepts a ledger row
    whose (shorter) stored commit is a prefix of `sha`.
    """
    if not sha:
        return False
    candidates = list(idx.get(sha, []))
    if not candidates:  # defensive short-SHA prefix match only when no exact row
        for c, rows in idx.items():
            if c and len(c) < len(sha) and sha.startswith(c):
                candidates.extend(rows)
    return any(_row_full_pass(r) for r in candidates)


def _split_green_by_ledger(intervals: list[dict],
                           ledger_idx: dict[str, list[dict]]
                           ) -> tuple[float, float]:
    """Seconds of GREEN wall-clock split into (ledger-corroborated, conclusion-
    only). Reuses the existing timeline intervals — does not recompute states."""
    led = 0.0
    concl = 0.0
    for iv in intervals:
        if iv["state"] != "green":
            continue
        span = iv["end"] - iv["start"]
        if span <= 0:
            continue
        if _ledger_corroborates(ledger_idx, iv.get("sha") or ""):
            led += span
        else:
            concl += span
    return led, concl


def _last_observed(intervals: list[dict]) -> dict:
    """Last interval backed by a real commit, skipping the stale-gap tail.

    The tail appended past the store's observation horizon has ``sha=None``; it
    is real elapsed time (so it belongs in the duration buckets) but it is not
    a verdict about any commit, so it must not be reported as the current state.
    """
    for iv in reversed(intervals):
        if iv.get("sha"):
            return iv
    return intervals[-1]


def green_time(parent: str, repo: str, since: str | None,
               workflows: list[str] | None) -> dict:
    tl = state_timeline(parent, repo, since, workflows)
    wanted = tl["workflows"]
    intervals = tl.get("intervals", [])
    if not intervals:
        return {"repo": repo, "workflows": wanted, "samples": 0,
                "green_pct": None,
                "definition_date": GREEN_TIME_DEFINITION_DATE,
                "note": tl.get("note", "no authoritative main-branch runs")}
    sec, total = _sum_by_state(intervals)
    authoritative = sec["green"] + sec["red"]
    hrs = {k: round(v / 3600.0, 2) for k, v in sec.items()}
    window_pct = {
        k: (round(100.0 * v / total, 2) if total > 0 else None)
        for k, v in sec.items()
    }
    green_pct = (round(100.0 * sec["green"] / authoritative, 2)
                 if authoritative > 0 else None)
    red_pct = (round(100.0 * sec["red"] / authoritative, 2)
               if authoritative > 0 else None)
    # Split the GREEN bucket into ledger-corroborated vs conclusion-only. These
    # are reported SEPARATELY; green_pct stays the combined figure for back-compat.
    ledger_idx = load_ledger_index(parent)
    g_led_sec, g_concl_sec = _split_green_by_ledger(intervals, ledger_idx)
    green_ledger_hours = round(g_led_sec / 3600.0, 2)
    green_conclusion_only_hours = round(g_concl_sec / 3600.0, 2)
    green_ledger_pct = (round(100.0 * g_led_sec / authoritative, 2)
                        if authoritative > 0 else None)
    green_conclusion_only_pct = (
        round(100.0 * g_concl_sec / authoritative, 2)
        if authoritative > 0 else None
    )
    result = {
        "repo": repo,
        "workflows": wanted,
        "definition_date": GREEN_TIME_DEFINITION_DATE,
        "combine_rule": "green requires ALL authoritative workflows success; "
                        "precedence red>gap>no_result>green",
        "denominator_definition": "authoritative-run wall-hours = green_hours "
                                  "+ red_hours; NO-RESULT and gap excluded",
        "samples": tl["samples"],
        "window_start": tl["window_start"],
        "window_end_utc": tl["window_end_utc"],
        "observed_through_utc": tl.get("observed_through_utc"),
        "store_stale_hours": tl.get("store_stale_hours", 0.0),
        "green_pct": green_pct,
        "green_ledger_pct": green_ledger_pct,
        "green_conclusion_only_pct": green_conclusion_only_pct,
        "red_pct": red_pct,
        # Explicitly window-denominated diagnostics. Keep the legacy
        # no_result_pct/gap_pct aliases for readers that predate the named
        # denominator, but never mix them into the headline arithmetic.
        "green_window_pct": window_pct["green"],
        "red_window_pct": window_pct["red"],
        "no_result_window_pct": window_pct["no_result"],
        "gap_window_pct": window_pct["gap"],
        "no_result_pct": window_pct["no_result"],
        "gap_pct": window_pct["gap"],
        "green_hours": hrs["green"],
        "green_ledger_hours": green_ledger_hours,
        "green_conclusion_only_hours": green_conclusion_only_hours,
        "red_hours": hrs["red"],
        "no_result_hours": hrs["no_result"],
        "gap_hours": hrs["gap"],
        "authoritative_run_hours": round(authoritative / 3600.0, 2),
        # The share of the window the percentage actually describes. Carried in
        # the record so every consumer gets it, not just the human printer.
        "coverage_pct": round(100.0 * authoritative / total, 2) if total else 0.0,
        "excluded_hours": round((sec["no_result"] + sec["gap"]) / 3600.0,
                                2),
        "window_hours": round(total / 3600.0, 2),
        # Backward-compatible alias; its name is intentionally not used as the
        # green-time denominator anymore.
        "total_hours": round(total / 3600.0, 2),
        "runs_by_conclusion": tl["runs_by_conclusion"],
        "job_level_red_promotions": tl.get("job_level_red_promotions", 0),
        # STATE vs DURATION are different claims and must not be conflated.
        # "main is currently green" is a claim about the TIP's last known
        # verdict, and stays that. "main was green X% of the window" is a claim
        # about wall-clock, and must not bill unobserved hours to that verdict.
        # So current_state reports the last OBSERVED verdict (skipping the
        # synthetic stale-gap tail, which carries no sha), while the buckets
        # above count the unobserved tail as gap. store_stale_hours tells the
        # reader how old that "current" verdict is.
        "current_state": _last_observed(intervals)["state"],
        "current_reason": _last_observed(intervals).get("reason"),
        "current_sha": tl["current_sha"],
    }
    if authoritative <= 0:
        result["note"] = ("no conclusive authoritative-run wall-hours in "
                          "window; NO-RESULT and gap excluded")
    return result


_BUCKET_SECONDS = {"day": 86400.0, "week": 604800.0}


def green_time_trend(parent: str, repo: str, since: str | None,
                     workflows: list[str] | None, bucket: str) -> dict:
    """green_pct per fixed-width time bucket, so a trend (not just a snapshot) is
    visible — a single number cannot show whether we are improving."""
    tl = state_timeline(parent, repo, since, workflows)
    intervals = tl.get("intervals", [])
    width = _BUCKET_SECONDS[bucket]
    out = {"repo": repo, "workflows": tl["workflows"], "bucket": bucket,
           "definition_date": GREEN_TIME_DEFINITION_DATE, "buckets": []}
    if not intervals:
        out["note"] = tl.get("note", "no authoritative main-branch runs")
        return out
    lo = intervals[0]["start"]
    hi = intervals[-1]["end"]
    b0 = (lo // width) * width
    while b0 < hi:
        b1 = b0 + width
        sec, total = _sum_by_state(intervals, b0, b1)
        if total > 0:
            authoritative = sec["green"] + sec["red"]
            out["buckets"].append({
                "bucket_start": _iso_utc(b0),
                "bucket_end": _iso_utc(b1),
                "green_pct": (round(100.0 * sec["green"] / authoritative, 2)
                              if authoritative > 0 else None),
                "red_pct": (round(100.0 * sec["red"] / authoritative, 2)
                            if authoritative > 0 else None),
                "no_result_window_pct": round(
                    100.0 * sec["no_result"] / total, 2),
                "gap_window_pct": round(100.0 * sec["gap"] / total, 2),
                "authoritative_run_hours": round(authoritative / 3600.0, 2),
                "window_hours": round(total / 3600.0, 2),
            })
        b0 = b1
    return out


def append_green_time_log(parent: str, snapshot: dict, path: str | None) -> str:
    """Append a point-in-time green-time snapshot as one JSONL line, so hourly
    status updates build a DURABLE trend even if the store is later pruned. The
    file is ignored runtime data (like the CSV stores)."""
    if path is None:
        path = os.path.join(store_dir(parent), "green-time-log.jsonl")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    record = {
        "computed_at": _iso_utc(dt.datetime.now(dt.timezone.utc).timestamp()),
        "repo": snapshot.get("repo"),
        "workflows": snapshot.get("workflows"),
        "definition_date": snapshot.get("definition_date"),
        "denominator_definition": snapshot.get("denominator_definition"),
        "window_start": snapshot.get("window_start"),
        "window_end_utc": snapshot.get("window_end_utc"),
        "observed_through_utc": snapshot.get("observed_through_utc"),
        "green_pct": snapshot.get("green_pct"),
        "green_hours": snapshot.get("green_hours"),
        "red_hours": snapshot.get("red_hours"),
        "authoritative_run_hours": snapshot.get("authoritative_run_hours"),
        "window_hours": snapshot.get("window_hours"),
        "excluded_hours": snapshot.get("excluded_hours"),
        "no_result_hours": snapshot.get("no_result_hours"),
        "gap_hours": snapshot.get("gap_hours"),
        "green_ledger_pct": snapshot.get("green_ledger_pct"),
        "green_conclusion_only_pct": snapshot.get("green_conclusion_only_pct"),
        "red_pct": snapshot.get("red_pct"),
        "no_result_window_pct": snapshot.get("no_result_window_pct"),
        "gap_window_pct": snapshot.get("gap_window_pct"),
        "samples": snapshot.get("samples"),
        "runs_by_conclusion": snapshot.get("runs_by_conclusion"),
        "current_state": snapshot.get("current_state"),
        "current_sha": snapshot.get("current_sha"),
    }
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, sort_keys=True) + "\n")
    return path


# ---------------------------------------------------------------------------
# runs summary
# ---------------------------------------------------------------------------

def runs_summary(parent: str, repo: str | None, since: str | None,
                 branch: str | None) -> str:
    rows = load_gha_rows(parent, repo, since, branch)
    if not rows:
        return "ci-hub history: gha-runs.csv empty or missing (run refresh-history first)."
    by_repo: dict[str, dict] = {}
    for r in rows:
        rp = r.get("repo", "?")
        agg = by_repo.setdefault(rp, {"n": 0, "concl": {}, "queue": [], "run": []})
        agg["n"] += 1
        c = r.get("conclusion") or (r.get("status") or "?")
        agg["concl"][c] = agg["concl"].get(c, 0) + 1
        q = _float(r.get("queue_s"))
        rn = _float(r.get("run_s"))
        if q is not None:
            agg["queue"].append(q)
        if rn is not None:
            agg["run"].append(rn)
    out = ["=== ci-hub GHA run history ==="]
    for rp, agg in sorted(by_repo.items()):
        q = sorted(agg["queue"])
        rn = sorted(agg["run"])
        out.append(f"\n{rp}: {agg['n']} runs"
                   + (f" (branch={branch})" if branch else ""))
        out.append("  by conclusion: " +
                   ", ".join(f"{k}={v}" for k, v in sorted(agg["concl"].items(),
                                                           key=lambda x: -x[1])))
        if q:
            out.append(f"  queue_s : median={percentile(q,50):.0f} "
                       f"p95={percentile(q,95):.0f} max={q[-1]:.0f}")
        if rn:
            out.append(f"  run_s   : median={percentile(rn,50):.0f} "
                       f"p95={percentile(rn,95):.0f} max={rn[-1]:.0f}")
    return "\n".join(out)


# ---------------------------------------------------------------------------
# recent runs listing (individual runs behind the summary shape)
# ---------------------------------------------------------------------------

# A run is flagged as a queue outlier when it waited longer than this floor AND
# above the window's p95 queue time. The floor keeps a p95 of 0 (the common case
# — most runs start instantly) from flagging every run; only genuinely stuck
# runs (waiting on a runner) get marked.
QUEUE_OUTLIER_FLOOR_S = 300.0


def _effective_conclusion(r: dict) -> str:
    """What actually happened: the terminal conclusion, else the live status."""
    return (r.get("conclusion") or r.get("status") or "?")


def _branch_or_pr(r: dict) -> str:
    """Prefer the PR reference over the raw branch when the run has one."""
    prs = (r.get("pull_requests") or "").strip()
    if prs:
        first = prs.split()[0].split(",")[0].strip()
        if first:
            return f"#{first}"
    return r.get("head_branch") or "-"


def _short_utc(s: str | None) -> str:
    """2026-08-02T00:00:44Z -> '08-02 00:00' (compact, still UTC)."""
    if not s or len(s) < 16:
        return s or "-"
    return f"{s[5:10]} {s[11:16]}"


def _is_queue_outlier(q: float | None, thresh: float) -> bool:
    return q is not None and q > thresh and q > 0


def _queue_lower_bound_s(r: dict, snapshot_ts: float | None) -> float | None:
    """Lower bound on a still-queued run's wait, computed OFFLINE.

    A queued run has run_started_at == created_at (a GitHub placeholder), so the
    stored queue_s is 0 even after hours in the queue — a silent wrong reading.
    The honest floor is `snapshot_ts - created_at`: the run was still queued AS OF
    our last refresh, so it waited at least that long. Anchored to the snapshot
    (not to `now`), it can only understate; it never trusts a possibly-stale
    status the way `now - created_at` would. Terminal runs return None — their
    queue_s is the real measured value.
    """
    if snapshot_ts is None or _effective_conclusion(r) != "queued":
        return None
    created = _epoch(r.get("created_at"))
    if created is None:
        return None
    return max(0.0, snapshot_ts - created)


def recent_runs(parent: str, repo: str | None, since: str | None,
                branch: str | None, status: str | None, limit: int,
                slowest: bool = False) -> dict:
    """K runs plus the window's queue/run shape they came from.

    Ordered newest-first by default, or by descending *effective* wait with
    ``slowest=True`` (surfaces runs stuck for hours behind the p95=0 median).
    Effective wait = measured queue_s for terminal runs, else the offline
    lower bound for still-queued runs. The outlier flag is computed against the
    p95 of the measured (terminal) queue distribution, so it means "slow
    relative to the completed runs".
    """
    rows = load_gha_rows(parent, repo, since, branch)
    if status:
        rows = [r for r in rows if _effective_conclusion(r) == status]
    store = gha_store_path(parent)
    snapshot_ts = os.path.getmtime(store) if os.path.isfile(store) else None
    # p95/threshold from MEASURED terminal waits only — a lower bound is not a
    # measured wait and must not skew the completed-run distribution.
    queues = sorted(q for q in (_float(r.get("queue_s")) for r in rows)
                    if q is not None)
    p95_queue = percentile(queues, 95) or 0.0
    thresh = max(QUEUE_OUTLIER_FLOOR_S, p95_queue)

    def eff_wait(r: dict) -> float | None:
        q = _float(r.get("queue_s"))
        if q is not None and q > 0:
            return q
        lb = _queue_lower_bound_s(r, snapshot_ts)
        return lb if lb is not None else q

    # Count outliers across the WHOLE matched window (not just the shown K) using
    # the effective wait, so a queued-for-hours run counts even at queue_s=0.
    window_outliers = sum(1 for r in rows
                          if _is_queue_outlier(eff_wait(r), thresh))
    if slowest:
        rows.sort(key=lambda r: eff_wait(r) if eff_wait(r) is not None else -1.0,
                  reverse=True)
    else:
        # created_at is a fixed-width ISO string, so lexical == chronological.
        rows.sort(key=lambda r: r.get("created_at") or "", reverse=True)
    out = []
    for r in rows[:max(0, limit)]:
        q = _float(r.get("queue_s"))
        lb = _queue_lower_bound_s(r, snapshot_ts)
        out.append({
            "created_at": r.get("created_at") or "",
            "repo": r.get("repo") or "?",
            "run_id": r.get("run_id") or "",
            "workflow": r.get("workflow_name") or "?",
            "ref": _branch_or_pr(r),
            "conclusion": _effective_conclusion(r),
            "queue_s": q,
            "queue_lower_bound_s": round(lb) if lb is not None else None,
            "run_s": _float(r.get("run_s")),
            "url": r.get("html_url") or "",
            "queue_outlier": _is_queue_outlier(eff_wait(r), thresh),
        })
    return {
        "total_matched": len(rows),
        "shown": len(out),
        "order": "slowest-wait" if slowest else "newest",
        "snapshot_ts": (dt.datetime.fromtimestamp(snapshot_ts, dt.timezone.utc)
                        .strftime("%Y-%m-%dT%H:%M:%SZ")
                        if snapshot_ts is not None else None),
        "queue_p95_s": round(p95_queue, 1),
        "queue_outlier_threshold_s": round(thresh, 1),
        "window_outliers": window_outliers,
        "runs": out,
    }


def render_recent(res: dict, limit: int) -> str:
    runs = res["runs"]
    if not runs:
        return "  (no individual runs match this filter)"
    hdr = ("", "TIME(UTC)", "REPO", "RUN_ID", "WORKFLOW", "BRANCH/PR",
           "CONCL", "QUEUE(s)", "RUN(s)", "URL")
    body = []
    have_lb = False
    for r in runs:
        mark = "!" if r["queue_outlier"] else ""
        lb = r.get("queue_lower_bound_s")
        if lb is not None:            # still-queued: measured 0 is misleading,
            queue_cell = f">={lb:.0f}"  # show the offline lower bound instead.
            have_lb = True
        else:
            queue_cell = _s(r["queue_s"])
        body.append((
            mark,
            _short_utc(r["created_at"]),
            r["repo"],
            str(r["run_id"]),
            r["workflow"],
            r["ref"],
            r["conclusion"],
            queue_cell,
            _s(r["run_s"]),
            r["url"],
        ))
    n_out = sum(1 for r in runs if r["queue_outlier"])
    order = "slowest-wait first" if res.get("order") == "slowest-wait" \
        else "newest first"
    head = (f"--- {res['shown']} of {res['total_matched']} matched "
            f"runs ({order}) ---")
    lines = [head, _table(hdr, body)]
    if have_lb:
        lines.append(
            f">=N = still queued as of snapshot {res.get('snapshot_ts') or '?'}: "
            f"lower bound = snapshot - created_at (offline; not a live 'now', so "
            f"it can only understate the current wait)")
    lines.append(
        f"! = wait > {res['queue_outlier_threshold_s']:.0f}s "
        f"(floor {QUEUE_OUTLIER_FLOOR_S:.0f}s, terminal-run p95 "
        f"{res['queue_p95_s']:.0f}s) — waiting on a runner; "
        f"{n_out} of {res['shown']} shown flagged, "
        f"{res['window_outliers']} of {res['total_matched']} in window "
        f"(use --slowest to see them)")
    return "\n".join(lines)


# ---------------------------------------------------------------------------

def emit_history(parent: str, args) -> int:
    """Shared renderer for the default view and the `runs` alias."""
    repo = getattr(args, "repo", None)
    since = getattr(args, "since", None)
    branch = getattr(args, "branch", None)
    status = getattr(args, "status", None)
    limit = getattr(args, "limit", 20)
    slowest = getattr(args, "slowest", False)
    summary_only = getattr(args, "summary_only", False)
    as_json = getattr(args, "json", False)

    if as_json:
        res = recent_runs(parent, repo, since, branch, status, limit, slowest)
        print(json.dumps(res, indent=2))
        return 0
    print(runs_summary(parent, repo, since, branch))
    if not summary_only:
        res = recent_runs(parent, repo, since, branch, status, limit, slowest)
        print()
        print(render_recent(res, limit))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    # Top-level filters so a bare `ci-hub history --since ...` works (the flag
    # was previously only defined on subparsers, so `history --since` errored).
    # Names mirror `ci-hub local-history` for a consistent surface.
    ap.add_argument("--repo")
    ap.add_argument("--since", help="YYYY-MM-DD (or full ISO timestamp)")
    ap.add_argument("--branch")
    ap.add_argument("--status",
                    help="filter by conclusion/status, e.g. queued, failure")
    ap.add_argument("--limit", type=int, default=20,
                    help="show this many runs (default 20)")
    ap.add_argument("--slowest", action="store_true",
                    help="order by descending queue wait instead of recency")
    ap.add_argument("--summary-only", action="store_true",
                    help="print only the queue/run shape, omit the run listing")
    ap.add_argument("--json", action="store_true",
                    help="emit the recent-runs listing as JSON")
    sub = ap.add_subparsers(dest="cmd")

    p_nb = sub.add_parser("node-cpu-budgets",
                          help="per-DAG-node CPU budgets for cpu_timeout")
    p_nb.add_argument("--repo")
    p_nb.add_argument("--since", help="git SHA prefix or YYYY-MM-DD")
    p_nb.add_argument("--min-samples", type=int, default=5)
    p_nb.add_argument("--hosted-multiplier", type=float,
                      default=HOSTED_CPU_MULTIPLIER,
                      help="canonical CPU-budget multiplier for underpowered "
                           "hosted runners (default %(default)s; unmeasured — "
                           "hosted profiles carry wall only)")
    p_nb.add_argument("--format", choices=["text", "csv", "json"], default="text")

    p_gt = sub.add_parser("green-time",
                          help="%% conclusive authoritative-run wall-clock "
                               "green (NO-RESULT/gap excluded)")
    p_gt.add_argument("--repo", default="rrnewton/hermit")
    p_gt.add_argument("--since", help="YYYY-MM-DD")
    p_gt.add_argument("--workflow", action="append",
                      help="authoritative workflow name (repeatable)")
    p_gt.add_argument("--format", choices=["text", "json"], default="text")
    p_gt.add_argument("--trend", choices=["day", "week"],
                      help="green%% per fixed-width bucket (trend, not snapshot)")
    p_gt.add_argument("--append-log", nargs="?", const="", default=None,
                      metavar="PATH",
                      help="append this snapshot as JSONL (default store dir) so "
                           "hourly runs build a durable trend")

    p_kt = sub.add_parser("kill-taxonomy",
                          help="split wall/cpu kills into livelock vs "
                               "contention via the cpu/wall ratio")
    p_kt.add_argument("--repo")
    p_kt.add_argument("--since", help="git SHA prefix or YYYY-MM-DD")
    p_kt.add_argument("--format", choices=["text", "json"], default="text")

    p_ru = sub.add_parser("runs",
                          help="summary + recent-runs listing (alias of default)")
    p_ru.add_argument("--repo")
    p_ru.add_argument("--since", help="YYYY-MM-DD")
    p_ru.add_argument("--branch")
    p_ru.add_argument("--status")
    p_ru.add_argument("--limit", type=int, default=20)
    p_ru.add_argument("--slowest", action="store_true")
    p_ru.add_argument("--summary-only", action="store_true")
    p_ru.add_argument("--json", action="store_true")

    args = ap.parse_args()
    parent = parent_root()

    if args.cmd == "node-cpu-budgets":
        rows = node_cpu_budgets(parent, args.repo, args.since, args.min_samples,
                                args.hosted_multiplier)
        print(render_node_budgets(rows, args.format))
        return 0
    if args.cmd == "kill-taxonomy":
        res = kill_taxonomy(parent, args.repo, args.since)
        print(render_kill_taxonomy(res, args.format))
        return 0
    if args.cmd == "green-time":
        if getattr(args, "trend", None):
            tr = green_time_trend(parent, args.repo, args.since, args.workflow,
                                  args.trend)
            if args.format == "json":
                print(json.dumps(tr, indent=2))
            elif not tr["buckets"]:
                print(f"{tr['repo']}: no green-time trend — "
                      f"{tr.get('note', 'no data')}")
            else:
                print(f"{tr['repo']} green-time trend per {tr['bucket']} "
                      f"(authoritative {tr['workflows']}; "
                      f"def {tr['definition_date']}):")
                hdr = ("BUCKET_START", "BUCKET_END", "GREEN%", "RED%", "AUTH_H",
                       "NO_RESULT_WIN%", "GAP_WIN%", "WINDOW_H")
                body = [(
                    b["bucket_start"],
                    b["bucket_end"],
                    "n/a" if b["green_pct"] is None else f"{b['green_pct']:g}",
                    "n/a" if b["red_pct"] is None else f"{b['red_pct']:g}",
                    f"{b['authoritative_run_hours']:g}",
                    f"{b['no_result_window_pct']:g}",
                    f"{b['gap_window_pct']:g}",
                    f"{b['window_hours']:g}",
                ) for b in tr["buckets"]]
                print(_table(hdr, body))
            return 0
        res = green_time(parent, args.repo, args.since, args.workflow)
        if getattr(args, "append_log", None) is not None:
            log_path = append_green_time_log(
                parent, res, args.append_log or None)
            res["appended_log"] = log_path
        if args.format == "json":
            print(json.dumps(res, indent=2))
        else:
            if res.get("green_pct") is None:
                print(f"{res['repo']}: green-time UNAVAILABLE — "
                      f"{res.get('note', '')}; window "
                      f"{res.get('window_start', 'n/a')} to "
                      f"{res.get('window_end_utc', 'n/a')}; denominator "
                      f"{res.get('authoritative_run_hours', 0)} "
                      f"authoritative-run hours"
                      + (f"; logged -> {res['appended_log']}"
                         if res.get("appended_log") else ""))
            else:
                # THE HEADLINE CARRIES ITS COVERAGE, always. A bare "50% green"
                # reads as a statement about the week; it is a statement about the
                # fraction of the week that was actually measured. Measured
                # 2026-08-08: 50.28% described 61.77 authoritative hours of a
                # 148.32-hour window -- 42% coverage -- and the percentage was being
                # quoted without that. Coverage belongs in the same sentence as the
                # number, not on a line underneath it that gets dropped when quoted.
                print(f"{res['repo']} main green-time (authoritative "
                      f"{res['workflows']}; definition {res['definition_date']}): "
                      f"{res['green_pct']}% green over "
                      f"{res['authoritative_run_hours']}h authoritative of "
                      f"{res['window_hours']}h wall "
                      f"(coverage {res['authoritative_run_hours']}/"
                      f"{res['window_hours']} = {res['coverage_pct']}%)")
                print(f"  denominator: {res['green_hours']}h green / "
                      f"{res['authoritative_run_hours']}h authoritative-run "
                      f"(green + genuine red)")
                print(f"  window: {res['window_start']} to "
                      f"{res['window_end_utc']} = {res['window_hours']}h; "
                      f"excluded from denominator: {res['no_result_hours']}h "
                      f"NO-RESULT + {res['gap_hours']}h gap")
                # Split GREEN into ledger-corroborated vs conclusion-only. These
                # are DIFFERENT claims and are never silently summed.
                print(f"    ledger-corroborated: {res['green_ledger_pct']}% "
                      f"({res['green_ledger_hours']}h)")
                print(f"    conclusion-only    : "
                      f"{res['green_conclusion_only_pct']}% "
                      f"({res['green_conclusion_only_hours']}h)")
                print(f"  authoritative red={res['red_pct']}%; window shares: "
                      f"green={res['green_window_pct']}% "
                      f"red={res['red_window_pct']}% "
                      f"no_result={res['no_result_window_pct']}% "
                      f"gap={res['gap_window_pct']}% over {res['samples']} "
                      f"commits")
                stale = res.get("store_stale_hours") or 0.0
                if stale > 0:
                    print(f"  ⚠ store observed only through "
                          f"{res.get('observed_through_utc')}; the trailing "
                          f"{stale}h is counted as GAP, not carried forward "
                          f"from the last known state")
                cur = res['current_state']
                reason = res.get('current_reason')
                cur_disp = f"{cur}({reason})" if reason else cur
                print(f"  current state={cur_disp} @ {res['current_sha'][:8]}"
                      + (f"; logged -> {res['appended_log']}"
                         if res.get("appended_log") else ""))
                print(f"  runs by conclusion: {res['runs_by_conclusion']}")
                promo = res.get("job_level_red_promotions", 0)
                if promo:
                    print(f"  case-7 job-failed-under-cancel -> red: {promo} "
                          f"(run cancelled, a job failed first by ordering)")
        return 0
    if args.cmd == "runs" or args.cmd is None:
        return emit_history(parent, args)
    ap.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
