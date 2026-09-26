#!/usr/bin/env python3
"""wall_cpu_ratchet.py — a lint over validate wall + CPU, alarming AT THE COMMIT.

The owner's standing NEVER-BLIND requirement: a build-cost regression that
arrives with an infrastructure change (a DAG migration, a cap change, a new
dep on the critical path) is invisible, because the new numbers have nothing to
compare against and become the new normal. This tool makes the comparison
happen every run and attributes a crossing to the commit that first caused it.

It reads the same validate-run ledger that Hermit's Rust validation driver writes and
ci-hub/validate/aggregate.py aggregates (schema >= 1) — we do NOT invent a
parallel store (see ci-hub/history/README.md, "The ONE store").

WHAT IT RATCHETS, AND WHY TWO NUMBERS, NOT ONE RATIO
----------------------------------------------------
validate parallelism is TWO levels, and the whole-run cpu/wall RATIO conflates
them, so this tool ratchets wall and CPU-seconds SEPARATELY:

  * wall (real_seconds)  = A1, the OUTER makespan — what the owner feels, and
    what a critical-path/dep-serialization regression moves.
  * cpu-seconds (user+sys) = the numerator of A2 total parallelism — the box
    cost. A node getting more expensive moves this even if wall is hidden by
    slack in the DAG.

Splitting the alarm localizes the regression to a level:
  wall up,  cpu flat -> OUTER parallelism lost (a dep serialized the DAG, or a
                        resource cap tightened): the makespan grew without more
                        work.
  cpu up  (wall may or may not follow) -> a node's TOTAL WORK grew (a step got
                        more expensive); if it is on the critical path wall
                        follows, otherwise the DAG absorbs it.

CONTROLLING FOR THE TWO CONFOUNDS (or the ratchet cries wolf)
-------------------------------------------------------------
Measured on this box (n=101 full passes, 2026-08-04), two factors move wall
independently of any commit and MUST be held constant, or every alarm is a
false alarm:

  * cache_state: cold full pass wall ~732s vs warm ~499s; cpu/wall cold 8.0x
    vs warm 2.4x. Baselines are bucketed by cache_state so warm is never
    compared to cold.
  * between-validate CONCURRENCY: with many validates sharing the box, each
    run's WALL stretches (time-slicing) while its CPU-seconds stays ~flat.
    Warm budget-met rate: 100% at <=4 concurrent, ~90% through 7, 56% at >=12;
    median wall 465-484s through conc 7 then 748s at 8-11. So a WALL crossing
    measured while concurrency was elevated is a SCHEDULING event, not a commit
    regression — this tool marks it CONFOUNDED and does not blame the commit.
    CPU-seconds is not concurrency-sensitive, so a CPU crossing still fires.

Exit codes (lint convention):
  0  within band (or a wall crossing correctly classified CONFOUNDED)
  1  usage / IO error
  2  INSUFFICIENT BASELINE — cannot evaluate (a no-result, never a pass/fail)
  3  REGRESSION — a genuine crossing attributed to the target commit
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from dataclasses import dataclass, field
from datetime import datetime, timezone

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ledger"))
import validate_rows as canonical_ledger  # noqa: E402

# Robust upper-control-limit knobs. UCL = median + K * MAD_scaled, but never
# tighter than REL_FLOOR of the median (a low-variance baseline must not
# hair-trigger on ordinary noise).
DEFAULT_MIN_BASELINE = 8
DEFAULT_K = 4.0
DEFAULT_REL_FLOOR = 0.25
_MAD_TO_SIGMA = 1.4826

# Concurrency headroom: a wall crossing is CONFOUNDED (scheduling, not the
# commit) if the target ran at materially more concurrency than its baseline.
DEFAULT_CONC_MARGIN = 2

# Recommended retention (deliverable 2). A two-day window cannot see a
# migration; see the module doc and the companion experiment.
RECOMMENDED_RETENTION_DAYS = 90


def _parse_ts(s: str | None) -> float | None:
    if not s:
        return None
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()
    except (ValueError, AttributeError):
        return None


def _median(xs: list[float]) -> float:
    s = sorted(xs)
    n = len(s)
    if n == 0:
        raise ValueError("median of empty sequence")
    m = n // 2
    return s[m] if n % 2 else (s[m - 1] + s[m]) / 2.0


def _mad_scaled(xs: list[float], med: float) -> float:
    return _median([abs(x - med) for x in xs]) * _MAD_TO_SIGMA


@dataclass
class Run:
    raw: dict
    started: float | None
    finished: float | None

    @property
    def profile(self) -> str:
        return self.raw.get("profile", "")

    @property
    def cache_state(self) -> str:
        return self.raw.get("cache_state", "unknown")

    @property
    def host(self) -> str:
        return self.raw.get("host", "")

    @property
    def commit(self) -> str:
        return self.raw.get("commit", "")

    @property
    def result(self) -> str:
        return self.raw.get("result", "")

    @property
    def timed_pass(self) -> bool:
        # A Buck full run times the release E2E payload (no debug assertions, no
        # overflow checks), which is not the Cargo debug binary these baselines
        # measure. It still overlaps other validates, so it counts toward
        # concurrency, but never as a timing sample. An absent field predates it
        # and was a Cargo run.
        return self.result == "pass" and self.raw.get("release_builder", "cargo") == "cargo"

    @property
    def wall(self) -> float | None:
        w = self.raw.get("real_seconds")
        return float(w) if isinstance(w, (int, float)) and w > 0 else None

    @property
    def cpu(self) -> float | None:
        u = self.raw.get("user_seconds")
        s = self.raw.get("sys_seconds")
        if isinstance(u, (int, float)) and isinstance(s, (int, float)):
            return float(u) + float(s)
        return None

    concurrency: int = 0  # between-validate overlap, filled by load_runs


def load_runs(path: str | None = None) -> list[Run]:
    runs: list[Run] = []
    if path is None:
        raw_rows = canonical_ledger.rows()
    else:
        raw_rows = []
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    raw_rows.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    for raw in raw_rows:
        runs.append(
            Run(
                raw=raw,
                started=_parse_ts(raw.get("started_at")),
                finished=_parse_ts(raw.get("finished_at")),
            )
        )
    _annotate_concurrency(runs)
    return runs


def _annotate_concurrency(runs: list[Run]) -> None:
    """Count validates (ANY profile) whose window overlaps each run's own
    window, inclusive of self. This is the owner's measured proxy for box
    contention; it survives the cache_state confound and the wall effect is
    monotonic in it."""
    windowed = [r for r in runs if r.started is not None and r.finished is not None and r.finished >= r.started]
    for r in runs:
        if r.started is None or r.finished is None:
            r.concurrency = 0
            continue
        r.concurrency = 1 + sum(
            1 for o in windowed if o is not r and o.started < r.finished and o.finished > r.started
        )


@dataclass
class MetricVerdict:
    metric: str  # "wall" | "cpu"
    value: float
    baseline_median: float
    ucl: float
    n: int
    crossed: bool
    confounded: bool = False  # wall only: elevated concurrency explains it
    note: str = ""


@dataclass
class Verdict:
    status: str  # "clean" | "regression" | "insufficient" | "confounded"
    target: Run
    baseline_n: int
    target_concurrency: int
    baseline_concurrency_median: float | None
    metrics: list[MetricVerdict] = field(default_factory=list)
    message: str = ""

    def exit_code(self) -> int:
        return {"regression": 3, "insufficient": 2, "clean": 0, "confounded": 0}[self.status]


def _baseline(runs: list[Run], target: Run) -> list[Run]:
    """Passing runs in the same (profile, cache_state, host) bucket that
    FINISHED before the target STARTED — so a crossing names the first commit
    to exceed the band, never one that merely ran concurrently later."""
    if target.started is None:
        return []
    out = []
    for r in runs:
        if r is target or not r.timed_pass:
            continue
        if r.profile != target.profile or r.cache_state != target.cache_state or r.host != target.host:
            continue
        if r.finished is None or r.finished > target.started:
            continue
        out.append(r)
    return out


def evaluate(
    runs: list[Run],
    target: Run,
    *,
    min_baseline: int = DEFAULT_MIN_BASELINE,
    k: float = DEFAULT_K,
    rel_floor: float = DEFAULT_REL_FLOOR,
    conc_margin: int = DEFAULT_CONC_MARGIN,
) -> Verdict:
    base = _baseline(runs, target)
    conc_med = _median([float(r.concurrency) for r in base]) if base else None

    if len(base) < min_baseline:
        return Verdict(
            status="insufficient",
            target=target,
            baseline_n=len(base),
            target_concurrency=target.concurrency,
            baseline_concurrency_median=conc_med,
            message=(
                f"insufficient baseline: {len(base)} prior passing "
                f"{target.profile}/{target.cache_state}/{target.host} run(s), "
                f"need >={min_baseline}. Cannot certify a regression (no-result)."
            ),
        )

    metrics: list[MetricVerdict] = []
    conc_elevated = target.concurrency > (conc_med or 0) + conc_margin

    for name, getter in (("wall", Run.wall.fget), ("cpu", Run.cpu.fget)):
        tv = getter(target)
        vals = [getter(r) for r in base]
        vals = [v for v in vals if v is not None]
        if tv is None or len(vals) < min_baseline:
            continue
        med = _median(vals)
        ucl = med + max(k * _mad_scaled(vals, med), rel_floor * med)
        crossed = tv > ucl
        # Wall is concurrency-sensitive; cpu-seconds is not. A wall crossing
        # under elevated concurrency is a scheduling event, not the commit.
        confounded = crossed and name == "wall" and conc_elevated
        note = ""
        if confounded:
            note = (
                f"CONFOUNDED: ran at {target.concurrency} concurrent validates "
                f"vs baseline median {conc_med:.0f}; wall stretch is scheduling, "
                f"not this commit."
            )
        metrics.append(
            MetricVerdict(
                metric=name,
                value=tv,
                baseline_median=med,
                ucl=ucl,
                n=len(vals),
                crossed=crossed,
                confounded=confounded,
                note=note,
            )
        )

    real_regressions = [m for m in metrics if m.crossed and not m.confounded]
    confounded_only = [m for m in metrics if m.crossed and m.confounded]

    if real_regressions:
        status = "regression"
    elif confounded_only:
        status = "confounded"
    else:
        status = "clean"

    return Verdict(
        status=status,
        target=target,
        baseline_n=len(base),
        target_concurrency=target.concurrency,
        baseline_concurrency_median=conc_med,
        metrics=metrics,
        message="",
    )


def _pick_target(runs: list[Run], profile: str, commit: str | None) -> Run | None:
    candidates = [r for r in runs if r.profile == profile and r.timed_pass and r.started is not None]
    if commit:
        candidates = [r for r in candidates if r.commit.startswith(commit)]
    if not candidates:
        return None
    return max(candidates, key=lambda r: r.started)


def _render(v: Verdict, as_json: bool) -> str:
    if as_json:
        return json.dumps(
            {
                "status": v.status,
                "exit_code": v.exit_code(),
                "commit": v.target.commit,
                "profile": v.target.profile,
                "cache_state": v.target.cache_state,
                "host": v.target.host,
                "baseline_n": v.baseline_n,
                "target_concurrency": v.target_concurrency,
                "baseline_concurrency_median": v.baseline_concurrency_median,
                "message": v.message,
                "metrics": [
                    {
                        "metric": m.metric,
                        "value": round(m.value, 1),
                        "baseline_median": round(m.baseline_median, 1),
                        "ucl": round(m.ucl, 1),
                        "n": m.n,
                        "crossed": m.crossed,
                        "confounded": m.confounded,
                        "note": m.note,
                    }
                    for m in v.metrics
                ],
            },
            indent=2,
        )

    icon = {"clean": "✅", "regression": "🚨", "insufficient": "•", "confounded": "⚠️"}[v.status]
    lines = [
        f"{icon} wall/cpu ratchet: {v.status.upper()}  "
        f"commit {v.target.commit[:12] or '?'}  "
        f"[{v.target.profile}/{v.target.cache_state}/{v.target.host}]"
    ]
    if v.status == "insufficient":
        lines.append(f"    {v.message}")
        return "\n".join(lines)
    bc = f"{v.baseline_concurrency_median:.0f}" if v.baseline_concurrency_median is not None else "?"
    lines.append(f"    baseline n={v.baseline_n}  concurrency: this={v.target_concurrency} baseline_med={bc}")
    for m in v.metrics:
        flag = "OVER" if m.crossed else "ok"
        lines.append(
            f"    {m.metric:4s} {m.value:7.1f}s  vs median {m.baseline_median:7.1f}s "
            f"UCL {m.ucl:7.1f}s (n={m.n})  [{flag}]"
        )
        if m.note:
            lines.append(f"         {m.note}")
    if v.status == "regression":
        lines.append("    => a genuine cost crossing is attributed to this commit.")
    elif v.status == "confounded":
        lines.append("    => wall crossing explained by concurrency; NOT blamed on the commit.")
    return "\n".join(lines)


def cmd_check(args: argparse.Namespace) -> int:
    try:
        runs = load_runs()
    except OSError as exc:
        print(f"error: cannot read canonical ledger: {exc}", file=sys.stderr)
        return 1
    target = _pick_target(runs, args.profile, args.commit)
    if target is None:
        print(
            f"error: no passing {args.profile} run"
            + (f" for commit {args.commit}" if args.commit else "")
            + " in ledger",
            file=sys.stderr,
        )
        return 1
    v = evaluate(
        runs,
        target,
        min_baseline=args.min_baseline,
        k=args.k,
        rel_floor=args.rel_floor,
        conc_margin=args.conc_margin,
    )
    print(_render(v, args.json))
    return v.exit_code()


def cmd_report(args: argparse.Namespace) -> int:
    try:
        runs = load_runs()
    except OSError as exc:
        print(f"error: cannot read canonical ledger: {exc}", file=sys.stderr)
        return 1
    passing = [r for r in runs if r.timed_pass and r.wall is not None]
    buckets: dict[tuple[str, str, str], list[Run]] = {}
    for r in passing:
        buckets.setdefault((r.profile, r.cache_state, r.host), []).append(r)

    span_days = _ledger_span_days(runs)
    print(f"# ledger canonical-union: {len(runs)} rows, span {span_days:.1f} days")
    if span_days < RECOMMENDED_RETENTION_DAYS:
        print(
            f"#   RETENTION WARNING: span {span_days:.1f}d < recommended "
            f"{RECOMMENDED_RETENTION_DAYS}d. A window this short cannot see an "
            f"infrastructure migration (deliverable 2)."
        )
    print("# bucket (profile/cache/host): n  median_wall  median_cpu  median_conc  budget<600s%")
    for key in sorted(buckets):
        g = buckets[key]
        walls = [r.wall for r in g]
        cpus = [r.cpu for r in g if r.cpu is not None]
        concs = [float(r.concurrency) for r in g]
        met = 100.0 * sum(1 for w in walls if w < 600) / len(walls)
        cpu_med = f"{_median(cpus):7.0f}" if cpus else "      ?"
        print(
            f"  {'/'.join(key)}: n={len(g):3d}  "
            f"wall={_median(walls):6.0f}s  cpu={cpu_med}s  "
            f"conc={_median(concs):4.1f}  met={met:5.1f}%"
        )
    return 0


def _ledger_span_days(runs: list[Run]) -> float:
    ts = [r.started for r in runs if r.started is not None]
    if len(ts) < 2:
        return 0.0
    return (max(ts) - min(ts)) / 86400.0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    pc = sub.add_parser("check", description="ratchet a run against its trailing baseline", help="ratchet a run against its trailing baseline")
    pc.add_argument("--profile", default="full", help="validate profile to ratchet (default: full)")
    pc.add_argument("--commit", default=None, help="target commit (prefix ok); default = latest passing run")
    pc.add_argument("--min-baseline", type=int, default=DEFAULT_MIN_BASELINE)
    pc.add_argument("--k", type=float, default=DEFAULT_K, help="MAD multiplier for the upper control limit")
    pc.add_argument("--rel-floor", type=float, default=DEFAULT_REL_FLOOR, help="min UCL as fraction of median")
    pc.add_argument("--conc-margin", type=int, default=DEFAULT_CONC_MARGIN)
    pc.add_argument("--json", action="store_true")
    pc.set_defaults(func=cmd_check)

    pr = sub.add_parser("report", description="print current baselines + budget-met per bucket", help="print current baselines + budget-met per bucket")
    pr.set_defaults(func=cmd_report)
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
