#!/usr/bin/env python3
"""Alarm when a validate run's CPU time regresses against a rolling baseline.

Owner's requirement: "We do not want to be in this place again where validate
becomes super slow and we wait on it in our inner loop blindly." This makes the
drift detectable instead of discovered months later.

WHY CPU AND NOT WALL. Measured on the live ledger (657 runs): CPU median 798s vs
wall median 193s, CPU p90 4621s vs wall p90 681s. The DAG is heavily parallel, so
wall is dominated by how contended the box was and CPU is not -- under 2x
oversubscription wall max inflated 2.80x while CPU max moved only 1.19x. So CPU
is the regression signal and wall is reported alongside it for human latency,
never gated on.

SCOPE, STATED UP FRONT: this ratchets TOTAL run time only. Per-node ratcheting is
deliberately absent because it cannot yet be done honestly -- `dag_jobs` is
populated on 190/657 runs (28.9%), and a p90 over a 29% sample is a biased
number, not a baseline. Gating on it would produce a threshold nobody trusts,
which gets muted, which is the failure this tool exists to prevent. Raise
per-node coverage to parity with total-time coverage (99.7%) first.

DEFAULT IS REPORT-ONLY. `--gate` is opt-in. A new blocking authority over landing
should be switched on deliberately once its baseline has been watched for a
while, not on the commit that introduces it.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

VALIDATE_DIR = Path(__file__).resolve().parents[1] / "validate"
sys.path.insert(0, str(VALIDATE_DIR))

import qualified_rows as qualified  # noqa: E402


DEFAULT_LEDGER = qualified.DEFAULT_LEDGER

# A baseline needs enough samples to have a meaningful p90. Below this the tool
# refuses to emit one rather than inventing a threshold from three runs.
MIN_SAMPLES = 30


def cpu_seconds(row: dict) -> float | None:
    u, s = row.get("user_seconds"), row.get("sys_seconds")
    if u in (None, "") or s in (None, ""):
        return None
    try:
        return float(u) + float(s)
    except (TypeError, ValueError):
        return None


def load(path: Path, profile: str | None) -> list[dict]:
    rows, _malformed = qualified.load_rows(path)
    ordered = qualified.qualified_rows(rows)
    return [row for row in ordered if not profile or row.get("profile") == profile]


def baseline(rows: list[dict]) -> dict | None:
    """p90 CPU over qualified runs carrying timing; refuse a small sample.

    Comparable runs only: a `quick` profile and a `full` profile are different
    workloads, so mixing them would make the p90 meaningless. Callers scope with
    --profile. ``load`` has already dropped incomplete, failed, zero-executed,
    and unordered rows through the shared qualified-row authority.
    """
    samples = sorted(c for c in (cpu_seconds(r) for r in rows) if c is not None)
    if len(samples) < MIN_SAMPLES:
        return None
    idx = max(0, int(0.9 * len(samples)) - 1)
    return {
        "n": len(samples),
        "median_cpu_s": statistics.median(samples),
        "p90_cpu_s": samples[idx],
        "max_cpu_s": samples[-1],
    }


def judge(cpu: float, base: dict, tolerance: float) -> dict:
    """Regression iff CPU exceeds p90 * tolerance.

    Keyed on p90 rather than the median so ordinary slow-but-normal runs do not
    alarm; the threshold is about drift in the tail, not variance.
    """
    limit = base["p90_cpu_s"] * tolerance
    over = cpu > limit
    return {
        "verdict": "REGRESSION" if over else "ok",
        "cpu_s": round(cpu, 1),
        "baseline_p90_cpu_s": round(base["p90_cpu_s"], 1),
        "limit_cpu_s": round(limit, 1),
        "ratio_to_p90": round(cpu / base["p90_cpu_s"], 2) if base["p90_cpu_s"] else None,
        "tolerance": tolerance,
        "baseline_n": base["n"],
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--profile", default="full", help="compare like with like; '' for all")
    ap.add_argument("--tolerance", type=float, default=1.25,
                    help="regression iff CPU > p90 * tolerance (default 1.25)")
    ap.add_argument("--cpu-seconds", type=float, default=None,
                    help="judge this run's CPU instead of the ledger's newest row")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--gate", action="store_true",
                    help="exit 1 on REGRESSION. Off by default: a new blocking "
                         "authority should be switched on deliberately, after its "
                         "baseline has been watched, not by the commit adding it")
    a = ap.parse_args(argv)

    rows = load(DEFAULT_LEDGER, a.profile or None)
    base = baseline(rows)
    if base is None:
        # Refusing is the point: a threshold derived from a handful of runs
        # would be a number with no authority behind it.
        print(f"UNVERIFIABLE: only {len(rows)} comparable run(s); need >= {MIN_SAMPLES} "
              f"for a p90 baseline", file=sys.stderr)
        return 2

    if a.cpu_seconds is not None:
        cpu = a.cpu_seconds
    else:
        timed = [r for r in rows if cpu_seconds(r) is not None]
        if not timed:
            print("UNVERIFIABLE: no run in the ledger carries timing", file=sys.stderr)
            return 2
        cpu = cpu_seconds(timed[-1])

    result = judge(cpu, base, a.tolerance)
    result["profile"] = a.profile
    if a.json:
        print(json.dumps({"baseline": base, "judgement": result}, indent=2))
    else:
        print(f"validate CPU ratchet [profile={a.profile or 'all'}]")
        print(f"  baseline  n={base['n']}  median={base['median_cpu_s']:.0f}s  "
              f"p90={base['p90_cpu_s']:.0f}s  max={base['max_cpu_s']:.0f}s")
        print(f"  this run  cpu={result['cpu_s']}s  "
              f"limit={result['limit_cpu_s']}s (p90 x {a.tolerance})  "
              f"ratio={result['ratio_to_p90']}")
        print(f"  verdict   {result['verdict']}")
        if result["verdict"] == "REGRESSION":
            print("  -> CPU exceeded the tail baseline. Attribute it to this commit now, "
                  "while the cause is one diff wide.")

    return 1 if (a.gate and result["verdict"] == "REGRESSION") else 0


if __name__ == "__main__":
    raise SystemExit(main())
