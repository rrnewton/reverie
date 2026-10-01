#!/usr/bin/env python3
# Parser and summarizer for the reverie PR 749 attribution runs
# (https://github.com/rrnewton/reverie/pull/749). Adapted from the part A
# summarize.py. Reads invocations.csv and the per-run stdout/stderr files that
# measure.py wrote; writes runs.csv and markers.csv and prints a Markdown
# summary. No interpretation; strict parsing: a run whose 96 marker lines are
# not complete, in order, and individually well formed is reported unparsed.
#
# In flight at marker i (0-based): children whose exit the guest has observed
# (i + 1 of them) minus children whose tracer-side after-hooks have finished.
#   A, B (head): line has finished_after_hooks_range=a..b  -> (i + 1) - b
#   C (main):    line has finished_after_hooks=[...]       -> (i + 1) - len(list)
# Baseline: the test's own definition, captured[0].1 = iteration 0's fds. At
# A and B the test prints it ("capacity fd bounds: baseline=..."), and the
# parser checks the printed value equals iteration 0's fds. At C it is not
# printed; iteration 0's fds and the run's modal fds value are reported.
import csv
import json
import os
import re
import statistics
import sys
from collections import Counter, defaultdict

SCRATCH = "/tmp/cml-attr749-1227"
OUTDIR = sys.argv[1] if len(sys.argv) > 1 else SCRATCH
TEST = ("tracer::tests::fatal_capacity_tests::"
        "retired_threads_release_descriptors_under_reduced_nofile")
TASKS = 96
VARIANTS = ["A", "B", "C"]

TUPLE = r"\(-?\d+, (?:true|false), (?:true|false), (?:true|false)\)"
TUPLE_RE = re.compile(TUPLE)
LIST_RE = re.compile(rf"^(?:{TUPLE}(?:, {TUPLE})*)?$")
HEAD_MARKER = re.compile(
    r"^capacity marker: iteration=(\d+), fds=(\d+), "
    r"finished_after_hooks_range=(\d+)\.\.(\d+), finished_after_hooks_new=\[(.*)\]$")
MAIN_MARKER = re.compile(
    r"^capacity marker: iteration=(\d+), fds=(\d+), finished_after_hooks=\[(.*)\]$")
BOUNDS = re.compile(
    r"^capacity fd bounds: baseline=(\d+), maximum_immediate=(\d+), steady_fds=(.*)$")
STEADY = re.compile(r"^capacity retirement sample: iteration=(\d+), fds=(\d+), root_alive=true$")
PREDICATE = re.compile(r"^capacity original predicate: .*samples=\[(.*)\], outcome=(.*)$")
SAMPLE = re.compile(r"\((\d+), (\d+)\)")


def parse_markers(variant, err_text):
    """Return (markers, reason). markers is a list of dicts or None."""
    lines = err_text.split("\n")
    # Any occurrence of the marker prefix that does not begin a line means two
    # writers' output shares a line.
    starts = sum(1 for l in lines if l.startswith("capacity marker:"))
    occurrences = err_text.count("capacity marker:")
    if occurrences != starts:
        return None, f"marker text not at line start ({occurrences} occurrences, {starts} line starts)"
    mlines = [l for l in lines if l.startswith("capacity marker:")]
    if len(mlines) != TASKS:
        return None, f"{len(mlines)} marker lines, expected {TASKS}"
    markers = []
    prev_end = 0
    prev_list = []
    for i, line in enumerate(mlines):
        if variant in ("A", "B"):
            m = HEAD_MARKER.match(line)
            if not m:
                return None, f"marker line {i} malformed: {line[:120]!r}"
            it, fds, a, b, new = (int(m.group(1)), int(m.group(2)), int(m.group(3)),
                                  int(m.group(4)), m.group(5))
            if not LIST_RE.match(new):
                return None, f"marker line {i} tuple list malformed"
            ntuples = len(TUPLE_RE.findall(new))
            if a != prev_end:
                return None, f"marker {i} range start {a} != previous end {prev_end}"
            if ntuples != b - a:
                return None, f"marker {i} has {ntuples} new tuples for range {a}..{b}"
            prev_end = b
            finished = b
        else:
            m = MAIN_MARKER.match(line)
            if not m:
                return None, f"marker line {i} malformed: {line[:120]!r}"
            it, fds, lst = int(m.group(1)), int(m.group(2)), m.group(3)
            if not LIST_RE.match(lst):
                return None, f"marker line {i} tuple list malformed"
            tuples = TUPLE_RE.findall(lst)
            if tuples[:len(prev_list)] != prev_list:
                return None, f"marker {i} cumulative list is not an extension of marker {i-1}'s"
            prev_list = tuples
            finished = len(tuples)
        if it != i:
            return None, f"marker line {i} has iteration={it}"
        markers.append({"iteration": it, "fds": fds, "finished": finished,
                        "inflight": (it + 1) - finished})
    return markers, ""


def failure_text(out_text, err_text):
    """Collect libtest's failure text: the JSON event's stdout field and any
    panic lines (with the message line after each) from stderr."""
    parts = []
    for line in out_text.split("\n"):
        if line.startswith("{") and '"event": "failed"' in line and TEST in line:
            try:
                ev = json.loads(line)
                if ev.get("stdout"):
                    parts.append("libtest stdout field: " + ev["stdout"].strip())
            except json.JSONDecodeError:
                parts.append("unparseable failed event: " + line[:300])
    elines = err_text.split("\n")
    for j, l in enumerate(elines):
        if "panicked at" in l or l.startswith("thread '"):
            parts.append("stderr: " + " | ".join(x for x in elines[j:j + 3] if x))
    return " || ".join(parts)[:2000]


def fmt_hist(counter):
    if not counter:
        return "(none)"
    return ", ".join(f"{k:+d}: {v}" for k, v in sorted(counter.items()))


def main():
    inv = list(csv.DictReader(open(os.path.join(SCRATCH, "invocations.csv"))))
    runs = []
    marker_rows = []
    for r in inv:
        v, k = r["variant"], int(r["k"])
        out_text = open(r["stdout"], "rb").read().decode("utf-8", "replace")
        err_text = open(r["stderr"], "rb").read().decode("utf-8", "replace")
        markers, reason = parse_markers(v, err_text)
        lines = err_text.split("\n")
        bounds = [BOUNDS.match(l) for l in lines]
        bounds = [b for b in bounds if b]
        steady = [STEADY.match(l) for l in lines]
        steady = [s for s in steady if s]
        preds = [PREDICATE.match(l) for l in lines]
        preds = [p for p in preds if p]
        row = {
            "variant": v, "k": k, "position": int(r["position"]),
            "start_utc": r["start_utc"], "rc": int(r["rc"]), "result": r["result"],
            "exec_time_s": r["exec_time_s"], "wall_s": r["wall_s"],
            "load1_before": r["loadavg_before"].split()[0],
            "load1_after": r["loadavg_after"].split()[0],
            "parsed": "yes" if markers is not None else "no",
            "unparsed_reason": reason,
            "baseline_printed": bounds[0].group(1) if bounds else "",
            "maximum_immediate_printed": bounds[0].group(2) if bounds else "",
            "steady_fds_printed": steady[0].group(2) if steady else "",
            "outcome": preds[0].group(2) if preds else "",
            "samples_match_markers": "",
            "fds_iter0": "", "baseline_used": "", "modal_fds": "", "modal_excess": "",
            "markers": "", "markers_inflight_1": "", "markers_inflight_ge2": "",
            "max_inflight": "", "max_excess": "", "min_excess": "",
            "marker0_inflight": "", "inflight_markers": "", "excess_at_inflight": "",
            "failure_text": failure_text(out_text, err_text) if r["result"] != "pass" else "",
            "stdout": r["stdout"], "stderr": r["stderr"],
        }
        if markers is not None:
            fds = [m["fds"] for m in markers]
            base = fds[0]
            if bounds and int(bounds[0].group(1)) != base:
                row["unparsed_reason"] = (f"printed baseline {bounds[0].group(1)} != "
                                          f"iteration 0 fds {base}")
            modal = Counter(fds).most_common()
            top = modal[0][1]
            modal_vals = sorted(val for val, c in modal if c == top)
            if preds:
                samples = [(int(a), int(b)) for a, b in SAMPLE.findall(preds[0].group(1))]
                row["samples_match_markers"] = (
                    "yes" if samples == [(i, f) for i, f in enumerate(fds)] else
                    f"no ({len(samples)} samples)")
            infl = [m["inflight"] for m in markers]
            exc = [f - base for f in fds]
            row.update({
                "fds_iter0": base, "baseline_used": base,
                "modal_fds": "/".join(map(str, modal_vals)),
                "modal_excess": "/".join(f"{x - base:+d}" for x in modal_vals),
                "markers": len(markers),
                "markers_inflight_1": sum(1 for x in infl if x == 1),
                "markers_inflight_ge2": sum(1 for x in infl if x >= 2),
                "max_inflight": max(infl), "max_excess": max(exc), "min_excess": min(exc),
                "marker0_inflight": infl[0],
                "inflight_markers": ";".join(f"{m['iteration']}:{m['inflight']}"
                                             for m in markers if m["inflight"] >= 1),
                "excess_at_inflight": ";".join(f"{m['iteration']}:{m['fds'] - base:+d}"
                                               for m in markers if m["inflight"] >= 1),
            })
            for m in markers:
                marker_rows.append({
                    "variant": v, "k": k, "iteration": m["iteration"], "fds": m["fds"],
                    "finished_after_hooks": m["finished"], "inflight": m["inflight"],
                    "baseline": base, "excess": m["fds"] - base,
                })
            row["_markers"] = markers
            row["_base"] = base
        runs.append(row)

    fields = [f for f in runs[0].keys() if not f.startswith("_")]
    with open(os.path.join(OUTDIR, "runs.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        w.writeheader()
        for row in sorted(runs, key=lambda x: (x["variant"], x["k"])):
            w.writerow(row)
    with open(os.path.join(OUTDIR, "markers.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(marker_rows[0].keys()))
        w.writeheader()
        for row in marker_rows:
            w.writerow(row)

    out = []
    out.append("### Per-variant summary")
    out.append("")
    out.append("| variant | runs | passes | failures | hangs | parsed runs | total markers | "
               "markers with 1 in flight | markers with >=2 in flight | markers with >=1 in flight "
               "| runs with >=1 in-flight marker | exec_time s min / median / max | "
               "invocation wall s min / median / max | 1-min loadavg range |")
    out.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    by_v = defaultdict(list)
    for row in runs:
        by_v[row["variant"]].append(row)
    for v in VARIANTS:
        rs = by_v[v]
        parsed = [r for r in rs if r["parsed"] == "yes"]
        n1 = sum(r["markers_inflight_1"] for r in parsed)
        n2 = sum(r["markers_inflight_ge2"] for r in parsed)
        nm = sum(r["markers"] for r in parsed)
        runs_any = sum(1 for r in parsed if r["max_inflight"] >= 1)
        ex = [float(r["exec_time_s"]) for r in rs if r["exec_time_s"]]
        wl = [float(r["wall_s"]) for r in rs]
        loads = [float(r["load1_before"]) for r in rs] + [float(r["load1_after"]) for r in rs]
        out.append(
            f"| {v} | {len(rs)} | {sum(1 for r in rs if r['result'] == 'pass')} | "
            f"{sum(1 for r in rs if r['result'] == 'fail')} | "
            f"{sum(1 for r in rs if r['result'] == 'hang')} | {len(parsed)} | {nm} | {n1} | {n2} | "
            f"{n1 + n2} | {runs_any} of {len(parsed)} | "
            f"{min(ex):.3f} / {statistics.median(ex):.3f} / {max(ex):.3f} | "
            f"{min(wl):.3f} / {statistics.median(wl):.3f} / {max(wl):.3f} | "
            f"{min(loads):.2f}-{max(loads):.2f} |")
    out.append("")
    for v in VARIANTS:
        rs = by_v[v]
        others = [r for r in rs if r["result"] != "pass"]
        unparsed = [r for r in rs if r["parsed"] != "yes"]
        mismatch = [r for r in rs if r["parsed"] == "yes" and r["unparsed_reason"]]
        sm = [r for r in rs if r["samples_match_markers"] not in ("yes", "")]
        out.append(f"- {v}: non-pass runs: " + (", ".join(
            f"k={r['k']} {r['result']} rc={r['rc']}: {r['failure_text']}" for r in others) or "none")
            + "; unparsed runs: " + (", ".join(
            f"k={r['k']} ({r['unparsed_reason']})" for r in unparsed) or "none")
            + "; baseline cross-check failures: " + (", ".join(
            f"k={r['k']} ({r['unparsed_reason']})" for r in mismatch) or "none")
            + "; RPC samples differing from marker fds: " + (", ".join(
            f"k={r['k']} ({r['samples_match_markers']})" for r in sm) or "none"))
    out.append("")
    out.append("### Baseline, excess and in-flight detail (parsed runs)")
    out.append("")
    out.append("Excess = marker fds minus the run's baseline (iteration 0's fds, the test's `captured[0].1`).")
    out.append("")
    out.append("| variant | baseline values (runs) | modal fds minus baseline (runs) | "
               "runs whose marker 0 had a child in flight | excess histogram at markers with >=1 in flight | "
               "excess histogram at markers with exactly 1 in flight | excess histogram at markers with >=2 in flight | "
               "excess histogram at markers with 0 in flight |")
    out.append("|---|---|---|---|---|---|---|---|")
    for v in VARIANTS:
        parsed = [r for r in by_v[v] if r["parsed"] == "yes"]
        bases = Counter(r["_base"] for r in parsed)
        modal = Counter(r["modal_excess"] for r in parsed)
        m0 = sum(1 for r in parsed if r["marker0_inflight"] >= 1)
        h_any, h1, h2, h0 = Counter(), Counter(), Counter(), Counter()
        for r in parsed:
            for m in r["_markers"]:
                e = m["fds"] - r["_base"]
                if m["inflight"] >= 1:
                    h_any[e] += 1
                if m["inflight"] == 1:
                    h1[e] += 1
                elif m["inflight"] >= 2:
                    h2[e] += 1
                else:
                    h0[e] += 1
        out.append(
            f"| {v} | " + ", ".join(f"{b} ({c})" for b, c in sorted(bases.items())) + " | "
            + ", ".join(f"{b} ({c})" for b, c in sorted(modal.items())) + f" | {m0} | "
            f"{fmt_hist(h_any)} | {fmt_hist(h1)} | {fmt_hist(h2)} | {fmt_hist(h0)} |")
    out.append("")
    out.append("| variant | largest excess per run: histogram (value: runs) | largest excess per run, by k |")
    out.append("|---|---|---|")
    for v in VARIANTS:
        parsed = sorted([r for r in by_v[v] if r["parsed"] == "yes"], key=lambda r: r["k"])
        h = Counter(r["max_excess"] for r in parsed)
        out.append(f"| {v} | {fmt_hist(h)} | " + " ".join(
            f"{r['k']}:{r['max_excess']:+d}" for r in parsed) + " |")
    out.append("")
    out.append("| variant | runs by number of in-flight markers (count: runs) | in-flight markers by iteration (iteration x count) |")
    out.append("|---|---|---|")
    for v in VARIANTS:
        parsed = [r for r in by_v[v] if r["parsed"] == "yes"]
        per_run = Counter(r["markers_inflight_1"] + r["markers_inflight_ge2"] for r in parsed)
        by_it = Counter()
        for r in parsed:
            for m in r["_markers"]:
                if m["inflight"] >= 1:
                    by_it[m["iteration"]] += 1
        out.append(f"| {v} | " + ", ".join(f"{a}: {b}" for a, b in sorted(per_run.items()))
                   + " | " + (", ".join(f"{a} x{b}" for a, b in sorted(by_it.items())) or "(none)")
                   + " |")
    out.append("")
    out.append("### Cross-check: one or more children in flight versus excess >= +5 (all markers of parsed runs)")
    out.append("")
    out.append("| variant | in flight >=1 and excess >=5 | in flight >=1 and excess <5 | "
               "in flight 0 and excess >=5 | in flight 0 and excess <5 | agreement |")
    out.append("|---|---|---|---|---|---|")
    for v in VARIANTS:
        parsed = [r for r in by_v[v] if r["parsed"] == "yes"]
        t = Counter()
        for r in parsed:
            for m in r["_markers"]:
                t[(m["inflight"] >= 1, m["fds"] - r["_base"] >= 5)] += 1
        total = sum(t.values())
        agree = t[(True, True)] + t[(False, False)]
        note = "" if v in ("A", "B") else " (supplementary)"
        out.append(f"| {v}{note} | {t[(True, True)]} | {t[(True, False)]} | {t[(False, True)]} | "
                   f"{t[(False, False)]} | {agree} of {total} |")
    out.append("")
    out.append("### Final steady sample (A and B print it; C has no such line)")
    out.append("")
    out.append("| variant | steady fds minus baseline (runs) |")
    out.append("|---|---|")
    for v in VARIANTS:
        parsed = [r for r in by_v[v] if r["parsed"] == "yes"]
        h = Counter(int(r["steady_fds_printed"]) - r["_base"] for r in parsed if r["steady_fds_printed"])
        out.append(f"| {v} | {fmt_hist(h)} |")
    print("\n".join(out))


if __name__ == "__main__":
    main()
