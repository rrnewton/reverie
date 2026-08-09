#!/usr/bin/env python3
"""Derive PLATEAU and REGRESSION points from a safe-ci-dag-runner profile store.

Reads the runner's own ``step_profiles_<machine_id>_<container_class>.csv`` -- no external
stopwatch. Emits, per (step, width): n, median wall, min, max, IQR, effective_cores, and the
speedup with BOTH terms shown (baseline wall and parallel wall), never a bare ratio.

Definitions, stated because a plateau and a regression are different points on the curve:

* ``best``      -- the width with the lowest median wall.
* ``PLATEAU``   -- the SMALLEST width whose median wall is within ``--plateau-tol`` (default 5%)
                   of the best median. This is where you stop buying speed; going wider costs
                   cores for no return. It is the defensible max-threads setting.
* ``REGRESSION``-- the smallest width ABOVE ``best`` whose median wall exceeds the best median by
                   more than ``--regress-tol`` (default 5%) AND whose [min,max] sample range does
                   NOT overlap the best width's range. The overlap test is what stops a noisy draw
                   on a shared box from being reported as a regression; a width that fails only the
                   tolerance test is reported as ``suspected`` and explicitly NOT confirmed.

A step measured at fewer than two widths has no curve and is reported as such rather than being
given a plateau of 1 -- flat-by-construction is not a measured plateau.
"""
from __future__ import annotations

import argparse
import csv
import glob
import statistics
from collections import defaultdict
from pathlib import Path


def _iqr(xs: list[float]) -> float:
    if len(xs) < 4:
        return max(xs) - min(xs)
    q = statistics.quantiles(xs, n=4)
    return q[2] - q[0]


def load(store: Path, include_failed: bool = False):
    rows = []
    for f in glob.glob(str(store / "step_profiles_*.csv")):
        with open(f, newline="") as fh:
            for r in csv.DictReader(fh):
                if not include_failed and r.get("ok") != "True":
                    continue
                rows.append(r)
    return rows


def curves(rows, plateau_tol: float, regress_tol: float):
    by = defaultdict(lambda: defaultdict(list))
    for r in rows:
        try:
            w = int(r["inner_jobs"])
            wall = float(r["elapsed_s"])
        except (KeyError, ValueError):
            continue
        eff = float(r["effective_cores"]) if r.get("effective_cores") else None
        amb = r.get("ambient_bucket", "")
        by[r["step"]][w].append((wall, eff, amb))

    out = []
    for step, widths in sorted(by.items()):
        levels = []
        for w in sorted(widths):
            walls = [x[0] for x in widths[w]]
            effs = [x[1] for x in widths[w] if x[1] is not None]
            ambs = {x[2] for x in widths[w]}
            levels.append(
                {
                    "width": w,
                    "n": len(walls),
                    "median": statistics.median(walls),
                    "min": min(walls),
                    "max": max(walls),
                    "iqr": _iqr(walls),
                    "eff_cores": statistics.median(effs) if effs else None,
                    "ambient": "/".join(sorted(ambs)),
                }
            )
        rec = {"step": step, "levels": levels}
        if len(levels) < 2:
            rec.update(curve=False, note="only one measured width - NO CURVE (not a plateau)")
            out.append(rec)
            continue
        rec["curve"] = True
        base = levels[0]
        best = min(levels, key=lambda L: L["median"])
        for L in levels:
            L["speedup_vs_base"] = base["median"] / L["median"] if L["median"] > 0 else float("nan")
        plateau = next(
            L for L in levels if L["median"] <= best["median"] * (1.0 + plateau_tol)
        )
        confirmed = suspected = None
        for L in levels:
            if L["width"] <= best["width"]:
                continue
            if L["median"] > best["median"] * (1.0 + regress_tol):
                overlaps = not (L["min"] > best["max"] or L["max"] < best["min"])
                if not overlaps and confirmed is None:
                    confirmed = L
                elif overlaps and suspected is None:
                    suspected = L
        rec.update(
            baseline_width=base["width"],
            baseline_wall=base["median"],
            best_width=best["width"],
            best_wall=best["median"],
            plateau_width=plateau["width"],
            plateau_wall=plateau["median"],
            regression_width=confirmed["width"] if confirmed else None,
            regression_wall=confirmed["median"] if confirmed else None,
            regression_suspected_width=suspected["width"] if suspected else None,
        )
        out.append(rec)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("store", type=Path)
    ap.add_argument("--plateau-tol", type=float, default=0.05)
    ap.add_argument("--regress-tol", type=float, default=0.05)
    ap.add_argument("--include-failed", action="store_true")
    ap.add_argument("--csv", type=Path, default=None)
    ns = ap.parse_args()

    rows = load(ns.store, ns.include_failed)
    cs = curves(rows, ns.plateau_tol, ns.regress_tol)
    for c in cs:
        print(f"\n=== {c['step']} ===")
        if not c["curve"]:
            print(f"  {c['note']}")
            continue
        print(
            f"  {'width':>6} {'n':>2} {'median_s':>9} {'min_s':>8} {'max_s':>8} "
            f"{'IQR_s':>7} {'eff_cores':>9} {'speedup':>9}  ambient"
        )
        for L in c["levels"]:
            tag = ""
            if L["width"] == c["best_width"]:
                tag += "  <-BEST"
            if L["width"] == c["plateau_width"]:
                tag += "  <-PLATEAU"
            if L["width"] == c["regression_width"]:
                tag += "  <-REGRESSION(confirmed)"
            if L["width"] == c["regression_suspected_width"]:
                tag += "  <-regression(suspected,overlapping)"
            ec = f"{L['eff_cores']:.1f}" if L["eff_cores"] is not None else "-"
            print(
                f"  {L['width']:>6} {L['n']:>2} {L['median']:>9.3f} {L['min']:>8.3f} "
                f"{L['max']:>8.3f} {L['iqr']:>7.3f} {ec:>9} "
                f"{L['speedup_vs_base']:>8.2f}x  {L['ambient']}{tag}"
            )
        print(
            f"  PLATEAU at width {c['plateau_width']}: "
            f"{c['baseline_wall']:.3f}s @ w={c['baseline_width']} -> {c['plateau_wall']:.3f}s "
            f"= {c['baseline_wall'] / c['plateau_wall']:.2f}x"
        )
        if c["regression_width"]:
            print(
                f"  REGRESSION CONFIRMED at width {c['regression_width']}: "
                f"{c['best_wall']:.3f}s @ w={c['best_width']} -> {c['regression_wall']:.3f}s "
                f"= {c['regression_wall'] / c['best_wall']:.2f}x SLOWER (sample ranges disjoint)"
            )
        elif c["regression_suspected_width"]:
            print(
                f"  regression SUSPECTED at width {c['regression_suspected_width']} but sample "
                f"ranges OVERLAP the best width - NOT confirmed, do not act on it"
            )
        else:
            print("  no regression observed within the measured width range")

    if ns.csv:
        with open(ns.csv, "w", newline="") as fh:
            w = csv.writer(fh, lineterminator="\n")
            w.writerow(
                ["step", "width", "n", "median_wall_s", "min_wall_s", "max_wall_s", "iqr_s",
                 "effective_cores", "speedup_vs_baseline", "baseline_width", "baseline_wall_s",
                 "ambient_buckets", "is_best", "is_plateau", "is_regression_confirmed"]
            )
            for c in cs:
                for L in c["levels"]:
                    w.writerow([
                        c["step"], L["width"], L["n"], f"{L['median']:.4f}", f"{L['min']:.4f}",
                        f"{L['max']:.4f}", f"{L['iqr']:.4f}",
                        f"{L['eff_cores']:.3f}" if L["eff_cores"] is not None else "",
                        f"{L.get('speedup_vs_base', float('nan')):.4f}" if c["curve"] else "",
                        c.get("baseline_width", ""),
                        f"{c.get('baseline_wall', 0):.4f}" if c["curve"] else "",
                        L["ambient"],
                        L["width"] == c.get("best_width"),
                        L["width"] == c.get("plateau_width"),
                        L["width"] == c.get("regression_width"),
                    ])
        print(f"\nwrote {ns.csv}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
