#!/usr/bin/env python3
"""validate WALL TIME as a standing MAIN-BRANCH series, not a per-run gate.

`ci/wall-budget-600s` asks "was THIS run slow". That catches a slow run and is
blind to the thing the owner actually asked about: a change that made EVERYTHING
slower. A migration-shaped regression arrives with a commit and is invisible
without a series to compare against -- which is why "did the DAG migration make
validate slower" could not be answered.

This module builds that series from the validate ledger, and its first duty is to
say how thin the series is. Measured 2026-08-07 over 691 ledger rows:

    rows on main .................. 132/691  (19.1%)
    of those, full-profile PASS ...  26/132
    DISTINCT MAIN COMMITS with a full-pass datapoint ... 15   over 4.38 days
    main full-pass wall ........... median 686s, min 396s, max 995s (n=26)

Fifteen points across four days cannot separate a regression from noise, and the
median already sits ABOVE the owner's 600s budget. Both facts are reported rather
than smoothed away.

CONDITIONING IS NOT OPTIONAL. A wall time without its concurrency is
uninterpretable: median wall measured 490s at 0-3 concurrent validates and 852s
at 14+, so an unconditioned comparison can manufacture a 74% "regression" out of
scheduling alone. `concurrent_validates` is present on only 210/691 rows (30.4%),
so the UNKNOWN bucket is the MAJORITY, not a corner case. Unconditioned points are
kept but marked, and `compare` refuses to draw a verdict across them by default.

CPU/WALL SEPARATES THE TWO CAUSES. Derivable on 689/691 rows today (median 2.77x):
wall rising while CPU stays flat is CONTENTION; both rising is MORE WORK. Reporting
wall alone cannot tell a reader which happened, so the ratio travels with it.

WHAT IS ALREADY IN THE LEDGER (no new producer needed): real_seconds,
user_seconds, sys_seconds, commit, profile, result on 691/691, and -- contrary to
the task's assumption -- PER-GATE durations, as `gates[].real_seconds` on 690/691
rows. Per-gate attribution is a missing VIEW, not a missing producer.

WHAT IS GENUINELY MISSING: peak memory. No rss/mem/peak key exists on any of the
51 distinct keys observed. That one needs a producer change and is reported as
`null` here rather than silently omitted, so its absence stays visible.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
from datetime import datetime
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Sequence

VALIDATE_DIR = Path(__file__).resolve().parents[1] / "validate"
if str(VALIDATE_DIR) not in sys.path:
    sys.path.insert(0, str(VALIDATE_DIR))

from qualified_rows import DEFAULT_LEDGER  # noqa: E402
from qualified_rows import load_rows as load_ledger_rows  # noqa: E402
from qualified_rows import qualified_rows  # noqa: E402

# Concurrency buckets, from the measured knee: the curve is a STEP at ~6, not a
# slope, so these boundaries are the measurement's own and not round numbers.
BUCKETS = ((0, 3), (4, 6), (7, 9), (10, 13), (14, 10**6))
UNKNOWN = "unknown"

# The owner's budget, for reference in the report. This module does not GATE on
# it -- gating is ci/wall-budget-600s's job -- it reports where the series sits.
WALL_BUDGET_SECONDS = 600

HERMIT_REPOSITORY = "rrnewton/hermit"
REVERIE_REPOSITORY = "rrnewton/reverie"
REPOSITORY_ALIASES = {
    None: HERMIT_REPOSITORY,
    "": HERMIT_REPOSITORY,
    "hermit": HERMIT_REPOSITORY,
    HERMIT_REPOSITORY: HERMIT_REPOSITORY,
    "reverie": REVERIE_REPOSITORY,
    REVERIE_REPOSITORY: REVERIE_REPOSITORY,
}


def bucket_of(n: int | None) -> str:
    if n is None:
        return UNKNOWN
    for lo, hi in BUCKETS:
        if lo <= n <= hi:
            return f"{lo}-{hi}" if hi < 10**6 else f"{lo}+"
    return UNKNOWN


@dataclass
class Point:
    commit: str
    started_at: str
    wall: float
    cpu: float
    ratio: float | None
    concurrency: int | None
    bucket: str
    dag_jobs: int | None
    cache_state: str | None
    peak_memory_kb: int | None  # always None today; see module docstring
    gates: list[dict] = field(default_factory=list)

    @property
    def conditioned(self) -> bool:
        """A point is comparable only if it carries what it was measured under."""
        return self.concurrency is not None


def load_rows(path: Path) -> list[dict]:
    """Return the canonical complete, nonempty PASS population."""
    rows, _malformed = load_ledger_rows(path)
    return qualified_rows(rows)


def load_population(path: Path) -> tuple[list[dict], int, int]:
    """Return qualified rows plus transparent raw/malformed denominators."""
    rows, malformed = load_ledger_rows(path)
    return qualified_rows(rows), len(rows), malformed


def main_commits(repo: Path, commits: Iterable[str], repository: str,
                 ref: str = "origin/main") -> set[str]:
    """Which commits are ancestors of main.

    Ancestry, not a branch label on the row: a row can be produced from a branch
    that later landed, and a label recorded at run time cannot know that. Asking
    git at read time is the only thing that stays true as history moves.

    Read main once, then batch-check only requested IDs that are not in that
    history. The second query distinguishes valid off-main commits from missing
    objects; any missing or non-commit object refuses the whole population.

    A subprocess per ledger commit made this gate grow linearly with retained
    evidence until tick-hub's 30-second bound killed it before it emitted a
    verdict. Per repository, one ``rev-list`` plus one batch object check
    answers the ancestry question for every commit in a fixed two Git
    processes, independent of the ledger population size.
    """
    requested = {commit for commit in commits if commit}
    if not requested:
        return set()
    # --stdin accepts rev-list options as well as revisions. Ledger commit IDs
    # are full lowercase object IDs; refusing anything else prevents a row from
    # changing the query and also makes malformed IDs fail closed.
    if any(re.fullmatch(r"[0-9a-f]{40}", commit) is None for commit in requested):
        raise RuntimeError(
            f"main ancestry check refused malformed commit ID in {repository}")
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "rev-list", ref],
            capture_output=True, text=True, timeout=60,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError(
            f"main ancestry check refused for {repository}: {error}") from error
    if proc.returncode != 0:
        raise RuntimeError(
            f"main ancestry check refused for {repository}: "
            f"rev-list exited {proc.returncode}")
    on_main = requested.intersection(proc.stdout.splitlines())
    outside_main = requested.difference(on_main)
    if not outside_main:
        return on_main
    try:
        checked = subprocess.run(
            ["git", "-C", str(repo), "cat-file", "--batch-check=%(objecttype)"],
            input="\n".join(sorted(outside_main)) + "\n",
            capture_output=True, text=True, timeout=60,
            env=os.environ | {"GIT_NO_LAZY_FETCH": "1"},
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError(
            f"main ancestry check refused for {repository}: {error}") from error
    object_types = checked.stdout.splitlines()
    if checked.returncode != 0 or len(object_types) != len(outside_main):
        raise RuntimeError(
            f"main ancestry check refused for {repository}: "
            "batch object check was incomplete")
    unavailable = []
    non_commits = []
    for commit, object_type in zip(sorted(outside_main), object_types):
        if object_type == "commit":
            continue
        qualified = f"{repository}:{commit}"
        if object_type == "missing" or object_type.endswith(" missing"):
            unavailable.append(qualified)
        else:
            non_commits.append(qualified)
    if unavailable or non_commits:
        problems = []
        if unavailable:
            # NAME THE REMEDY, NOT JUST THE PROBLEM. Refusing here is correct and
            # stays correct -- an object this checkout cannot classify makes the
            # whole population untrustworthy. But on 2026-08-20 this gate refused
            # on EVERY tick for an entire session and produced no wall-time verdict
            # at all, because the message said which objects were missing and
            # nothing about what to do, so nobody acted on an alarm that was firing
            # correctly all day.
            #
            # What it turned out to be, and why a reader could not guess it: the
            # objects were on the server the whole time and this checkout had simply
            # never fetched them, because they are off-main branch states produced
            # on another host. `git cat-file` reports them as
            # "upload-pack: not our ref", which reads like the server does not have
            # them; it only means the SHA is not an advertised ref tip, and an
            # explicit fetch by SHA succeeds. Reading that message as an existence
            # claim is what makes the condition look unfixable.
            problems.append(
                f"{len(unavailable)} unavailable object(s): "
                f"{', '.join(unavailable[:4])}"
                f" -- absent from this checkout, which does NOT mean absent"
                f" upstream. Try `git -C <checkout> fetch --no-tags origin <sha>`"
                f" for each; that succeeds for an off-main commit that was pushed,"
                f" even though `git cat-file` calls it \"not our ref\". If the fetch"
                f" also fails, confirm with"
                f" `gh api repos/<owner>/<repo>/commits/<sha>`: a \"No commit found\""
                f" there means the commit never left the host that validated it, and"
                f" the row cannot be resolved from here")
        if non_commits:
            problems.append(
                f"{len(non_commits)} non-commit object(s): "
                f"{', '.join(non_commits[:4])}")
        raise RuntimeError("main ancestry check refused " + "; ".join(problems))
    return on_main


def repository_name(row: dict) -> str:
    """Return the canonical repository named by a ledger row.

    Historical Hermit rows predate the ``repo`` field, so absence retains its
    established Hermit meaning. Anything outside the two repositories that
    produce this ledger is refused rather than guessed.
    """
    raw = row.get("repo")
    try:
        return REPOSITORY_ALIASES[raw]
    except (KeyError, TypeError) as error:
        commit = row.get("commit") or "<missing-commit>"
        qualified_repository = raw if isinstance(raw, str) else repr(raw)
        raise RuntimeError(
            "main ancestry check refused unsupported repository "
            f"{raw!r} for {qualified_repository}:{commit}") from error


def rows_on_main(rows: Sequence[dict], repositories: dict[str, Path]) -> list[dict]:
    """Filter rows by ancestry in the repository that owns each commit."""
    commits_by_repository: dict[str, set[str]] = {}
    keys = []
    for row in rows:
        repository = repository_name(row)
        commit = row.get("commit") or ""
        keys.append((repository, commit))
        commits_by_repository.setdefault(repository, set()).add(commit)

    keep: set[tuple[str, str]] = set()
    failures = []
    for repository, commits in commits_by_repository.items():
        checkout = repositories.get(repository)
        if checkout is None:
            failures.append(
                f"main ancestry check refused: no checkout configured for {repository}")
            continue
        try:
            on_main = main_commits(checkout, commits, repository)
        except RuntimeError as error:
            failures.append(str(error))
            continue
        keep.update((repository, commit) for commit in on_main)
    if failures:
        raise RuntimeError(" | ".join(failures))
    return [row for row, key in zip(rows, keys) if key in keep]


def to_points(rows: Sequence[dict], keep: set[str] | None) -> list[Point]:
    pts: list[Point] = []
    for r in rows:
        commit = r.get("commit") or ""
        if keep is not None and commit not in keep:
            continue
        wall = r.get("real_seconds")
        usr, sysv = r.get("user_seconds"), r.get("sys_seconds")
        if not isinstance(wall, (int, float)) or not wall:
            continue
        cpu = (usr or 0) + (sysv or 0) if isinstance(usr, (int, float)) else 0.0
        conc = r.get("concurrent_validates")
        conc = conc if isinstance(conc, int) else None
        pts.append(Point(
            commit=commit, started_at=r.get("started_at") or "",
            wall=float(wall), cpu=float(cpu),
            ratio=round(cpu / wall, 3) if wall else None,
            concurrency=conc, bucket=bucket_of(conc),
            dag_jobs=r.get("dag_jobs") if isinstance(r.get("dag_jobs"), int) else None,
            cache_state=r.get("cache_state"),
            peak_memory_kb=None,
            gates=[g for g in (r.get("gates") or []) if isinstance(g, dict)],
        ))
    # Preserve qualified_rows() event-time order. Sorting by started_at here
    # would undo its finished_at ordering and recreate the positional defect
    # the canonical accessor exists to prevent.
    return pts


def gate_breakdown(points: Sequence[Point], top: int = 12) -> list[dict]:
    """Per-gate wall, so a regression is attributable to a NODE, not to 'validate'."""
    agg: dict[str, list[float]] = {}
    for p in points:
        for g in p.gates:
            secs = g.get("real_seconds")
            name = g.get("name")
            if isinstance(secs, (int, float)) and isinstance(name, str):
                agg.setdefault(name, []).append(float(secs))
    out = [{"gate": k, "n": len(v), "median_seconds": statistics.median(v),
            "max_seconds": max(v)} for k, v in agg.items()]
    out.sort(key=lambda d: d["median_seconds"], reverse=True)
    return out[:top]


def summarize(points: Sequence[Point]) -> dict:
    walls = [p.wall for p in points]
    ratios = [p.ratio for p in points if p.ratio is not None]
    conditioned = [p for p in points if p.conditioned]
    per_bucket: dict[str, list[float]] = {}
    for p in points:
        per_bucket.setdefault(p.bucket, []).append(p.wall)
    return {
        "n": len(points),
        "distinct_commits": len({p.commit for p in points}),
        "span": [points[0].started_at, points[-1].started_at] if points else [],
        "wall_median": statistics.median(walls) if walls else None,
        "wall_min": min(walls) if walls else None,
        "wall_max": max(walls) if walls else None,
        "over_budget": sum(1 for w in walls if w > WALL_BUDGET_SECONDS),
        "budget_seconds": WALL_BUDGET_SECONDS,
        "cpu_wall_ratio_median": statistics.median(ratios) if ratios else None,
        "conditioned": len(conditioned),
        "unconditioned": len(points) - len(conditioned),
        "by_concurrency_bucket": {
            k: {"n": len(v), "wall_median": statistics.median(v)}
            for k, v in sorted(per_bucket.items())
        },
        "peak_memory_available": False,
    }


def compare(baseline: Sequence[Point], candidate: Sequence[Point],
            threshold: float, require_conditioned: bool = True) -> dict:
    """Regression verdict, with an explicit refusal when the inputs cannot support one.

    The refusal is the point. Comparing an unconditioned candidate against an
    unconditioned baseline can manufacture a 74% regression out of concurrency
    alone (490s at 0-3 concurrent vs 852s at 14+). A verdict that cannot say what
    both sides were measured under is not a verdict, so this returns
    `INSUFFICIENT` rather than a number that reads as one.
    """
    b = [p for p in baseline if p.conditioned] if require_conditioned else list(baseline)
    c = [p for p in candidate if p.conditioned] if require_conditioned else list(candidate)
    if not b or not c:
        return {"verdict": "INSUFFICIENT", "reason":
                f"need conditioned points on both sides; baseline={len(b)} candidate={len(c)}"
                + (" (require_conditioned=True)" if require_conditioned else ""),
                "baseline_n": len(b), "candidate_n": len(c)}
    bm, cm = statistics.median([p.wall for p in b]), statistics.median([p.wall for p in c])
    delta = (cm - bm) / bm if bm else 0.0
    br = statistics.median([p.ratio for p in b if p.ratio is not None] or [0])
    cr = statistics.median([p.ratio for p in c if p.ratio is not None] or [0])
    if delta <= threshold:
        verdict, cause = "OK", "within threshold"
    else:
        # The ratio is what separates the two causes. Wall up with CPU flat means
        # we waited; wall up with CPU up means we did more.
        cause = "CONTENTION (wall up, CPU/wall ratio down)" if cr < br \
            else "MORE WORK (wall up, CPU/wall ratio held or rose)"
        verdict = "REGRESSION"
    return {"verdict": verdict, "baseline_median": bm, "candidate_median": cm,
            "delta_fraction": round(delta, 4), "threshold": threshold,
            "baseline_ratio": br, "candidate_ratio": cr, "cause": cause,
            "baseline_n": len(b), "candidate_n": len(c)}


def build(ledger: Path, repo: Path, only_main: bool, profile: str | None,
          result: str | None, reverie_repo: Path = Path("reverie")) -> dict:
    rows, total, malformed = load_population(ledger)
    sel = rows
    if profile:
        sel = [r for r in sel if r.get("profile") == profile]
    if result:
        sel = [r for r in sel if r.get("result") == result]
    rows_after_profile_result_filter = len(sel)
    if only_main:
        sel = rows_on_main(sel, {
            HERMIT_REPOSITORY: repo,
            REVERIE_REPOSITORY: reverie_repo,
        })
    pts = to_points(sel, None)
    rep = {
        "ledger_rows_total": total,
        "ledger_rows_malformed": malformed,
        "qualified_rows_total": len(rows),
        "rows_after_profile_result_filter": rows_after_profile_result_filter,
        "rows_on_main": len(pts) if only_main else None,
        "only_main": only_main,
        "summary": summarize(pts),
        "gates_top": gate_breakdown(pts),
        "points": [p.__dict__ | {"gates": len(p.gates)} for p in pts],
    }
    return rep


def render(rep: dict) -> str:
    s = rep["summary"]
    L = ["validate WALL on main -- standing series (not the per-run 600s gate)"]
    L.append(f"  ledger rows total                 : {rep['ledger_rows_total']}")
    L.append(f"  malformed rows skipped            : {rep['ledger_rows_malformed']}")
    L.append(f"  complete nonempty PASS rows       : {rep['qualified_rows_total']}")
    L.append(f"  after profile/result filter       : {rep['rows_after_profile_result_filter']}")
    if rep["only_main"]:
        L.append(f"  ON MAIN (ancestry-checked)        : {rep['rows_on_main']}")
    L.append(f"  distinct main commits with a point: {s['distinct_commits']}")
    L.append(f"  span                              : {' -> '.join(s['span']) if s['span'] else '-'}")
    L.append("")
    L.append(f"  wall median/min/max               : {s['wall_median']}s / {s['wall_min']}s / {s['wall_max']}s  (n={s['n']})")
    L.append(f"  over the {s['budget_seconds']}s budget            : {s['over_budget']}/{s['n']}")
    L.append(f"  CPU/wall ratio median             : {s['cpu_wall_ratio_median']}")
    L.append(f"  conditioned / unconditioned       : {s['conditioned']} / {s['unconditioned']}")
    L.append(f"  peak memory available             : {s['peak_memory_available']}  (no producer writes it)")
    L.append("")
    L.append("  wall median by concurrency bucket (a wall time without this is uninterpretable):")
    for k, v in s["by_concurrency_bucket"].items():
        L.append(f"    {k:<10} n={v['n']:<4} median={v['wall_median']}s")
    L.append("")
    L.append("  slowest gates by median wall (attribution to a NODE, not to 'validate'):")
    for g in rep["gates_top"]:
        L.append(f"    {g['median_seconds']:>7.1f}s  (max {g['max_seconds']:>6.1f}s, n={g['n']:<4}) {g['gate'][:70]}")
    return "\n".join(L)


def main(argv: Sequence[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--ledger", type=Path, default=DEFAULT_LEDGER,
                    help="canonical validation ledger root or JSONL path")
    ap.add_argument("--repo", default="hermit",
                    help="Hermit checkout for Hermit ancestry checks")
    ap.add_argument("--reverie-repo", default="reverie",
                    help="Reverie checkout for Reverie ancestry checks")
    ap.add_argument("--all-commits", action="store_true",
                    help="do NOT restrict to main (default is main-only)")
    ap.add_argument("--profile", default="full")
    ap.add_argument("--result", default="pass")
    ap.add_argument("--format", choices=("json", "text"), default="text")
    ap.add_argument("--fail-over-budget", action="store_true",
                    help="exit 1 if the median on main exceeds the wall budget "
                         "(one-shot audit; NOT wireable -- see --gate-regression)")
    ap.add_argument("--gate-regression", action="store_true",
                    help="tick-hub form: exit 1 ONLY on a supportable regression verdict")
    ap.add_argument("--baseline-frac", type=float, default=0.5,
                    help="fraction of the conditioned series used as baseline")
    ap.add_argument("--threshold", type=float, default=0.20)
    ap.add_argument("--readiness", action="store_true",
                    help="exit 1 if the series CANNOT yet support a verdict "
                         "(retention or conditioning shortfall)")
    ap.add_argument("--min-days", type=float, default=90.0)
    ap.add_argument("--min-conditioned-frac", type=float, default=0.9)
    args = ap.parse_args(argv)
    try:
        rep = build(args.ledger, Path(args.repo), not args.all_commits,
                    args.profile or None, args.result or None,
                    Path(args.reverie_repo))
    except RuntimeError as error:
        if args.gate_regression or args.readiness:
            _emit("refused", f"REFUSED: {error}")
            return 1
        print(f"wall-series: REFUSED: {error}", file=sys.stderr)
        return 2

    if args.gate_regression or args.readiness:
        return _gate(rep, args)

    print(json.dumps(rep, indent=2) if args.format == "json" else render(rep))
    if args.fail_over_budget:
        m = rep["summary"]["wall_median"]
        return 1 if (m is not None and m > WALL_BUDGET_SECONDS) else 0
    return 0


def series_readiness(rep: dict, min_days: float, min_conditioned_frac: float) -> dict:
    """Can this series support a regression verdict yet? Both conditions, ANDed.

    Measured and carried from the series' own design: raising EITHER alone fails.
    Ninety days of unqualified walls is still a refusal, because a wall time
    without its concurrency is not comparable (median 490s at 0-3 concurrent vs
    852s at 14+). So retention AND conditioning are both preconditions, and the
    shortfall names which one is missing rather than reporting a bare "not ready".
    """
    s = rep["summary"]
    n = s["n"] or 0
    frac = (s["conditioned"] / n) if n else 0.0
    days = 0.0
    if s["span"] and all(s["span"]):
        try:
            a, b = (datetime.fromisoformat(x.replace("Z", "+00:00")) for x in s["span"])
            days = (b - a).total_seconds() / 86400.0
        except ValueError:
            days = 0.0
    shortfalls = []
    if days < min_days:
        shortfalls.append(f"retention {days:.2f}d < {min_days}d")
    if frac < min_conditioned_frac:
        shortfalls.append(
            f"conditioning {s['conditioned']}/{n} = {frac:.0%} < {min_conditioned_frac:.0%}")
    return {"ready": not shortfalls, "span_days": round(days, 2),
            "conditioned": s["conditioned"], "n": n,
            "conditioned_frac": round(frac, 4), "shortfalls": shortfalls}


def _emit(state: str, summary: str, **fields: object) -> None:
    """Emit ONE `key=value` PAIR PER LINE, because that is what parses.

    These gates used to print a prose sentence and nothing else. tick-hub's
    `capture: true` runs stdout through `parse_kv_lines`, which is strictly
    line-oriented, so a bare sentence yielded no `summary` key and the emitted
    action rendered a LITERAL `{summary}` naming nothing.

    Worse than nothing, in fact: these particular sentences contain an equals
    sign ("conditioning 6/18 = 33% < 90%"), so the parser split on it and
    manufactured a key whose NAME was half a sentence. Measured 2026-08-08, the
    readiness gate had been firing DAILY since 0630284 emitting exactly that.

    The prose is kept, as the VALUE of `summary`. That is safe even when it
    contains "=", because the split is on the FIRST "=" of each line -- but only
    if each pair is on its own line, which is the whole point.
    """
    print(f"state={state}")
    print(f"summary={summary}")
    for key, value in fields.items():
        print(f"{key}={value}")


def _gate(rep: dict, args) -> int:
    ready = series_readiness(rep, args.min_days, args.min_conditioned_frac)
    shortfalls = "; ".join(ready["shortfalls"])
    common = {
        "span_days": ready["span_days"],
        "conditioned": ready["conditioned"],
        "n": ready["n"],
        "conditioned_frac": ready["conditioned_frac"],
    }
    if args.readiness:
        if ready["ready"]:
            _emit("ready",
                  f"wall series READY: {ready['span_days']}d, "
                  f"conditioned {ready['conditioned']}/{ready['n']}",
                  **common)
            return 0
        _emit("not-ready",
              f"wall series CANNOT support a regression verdict: {shortfalls}",
              shortfalls=shortfalls, **common)
        return 1
    # --gate-regression. A series that cannot support a verdict must NOT emit one,
    # and must NOT page either -- that is what the separate low-cadence readiness
    # reminder is for. Silence here is deliberate and is stated on stdout so a
    # reader can tell "no regression" from "could not look".
    if not ready["ready"]:
        _emit("holding",
              f"HOLDING: series cannot support a verdict ({shortfalls}). "
              "No regression claim made.",
              shortfalls=shortfalls, **common)
        return 0
    pts = [p for p in _points_from(rep) if p.conditioned]
    cut = max(1, int(len(pts) * args.baseline_frac))
    verdict = compare(pts[:cut], pts[cut:], args.threshold)
    if verdict["verdict"] == "REGRESSION":
        _emit("regression",
              f"WALL REGRESSION on main: {verdict['baseline_median']}s -> "
              f"{verdict['candidate_median']}s "
              f"(+{verdict['delta_fraction']:.1%}, threshold {args.threshold:.0%}); "
              f"{verdict['cause']}",
              baseline_median=verdict["baseline_median"],
              candidate_median=verdict["candidate_median"],
              delta_fraction=verdict["delta_fraction"],
              threshold=args.threshold,
              cause=verdict["cause"],
              baseline_n=verdict.get("baseline_n"),
              candidate_n=verdict.get("candidate_n"))
        return 1
    _emit("ok",
          f"wall OK: {verdict['verdict']} "
          f"(baseline n={verdict.get('baseline_n')}, candidate n={verdict.get('candidate_n')})",
          verdict=verdict["verdict"],
          baseline_n=verdict.get("baseline_n"),
          candidate_n=verdict.get("candidate_n"))
    return 0


def _points_from(rep: dict) -> list[Point]:
    out = []
    for d in rep["points"]:
        d = dict(d)
        d["gates"] = []
        out.append(Point(**d))
    return out


if __name__ == "__main__":
    sys.exit(main())
