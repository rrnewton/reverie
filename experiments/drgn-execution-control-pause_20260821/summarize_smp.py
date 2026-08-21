#!/usr/bin/env python3
"""Derive every table in REPORT.md from the raw trial JSON.

Nothing in the report is transcribed by hand; this script is the only path
from measurement to table.
"""

import collections
import glob
import json
import re
from pathlib import Path

HPET_WRAP = 2 ** 32 / 100e6          # 32-bit mask at 100 MHz
ACPI_PM_WRAP = 2 ** 24 / 3.579545e6  # 24-bit at 3.579545 MHz


def load(pattern):
    out = []
    for f in sorted(glob.glob(pattern)):
        try:
            out.extend(json.load(open(f)))
        except Exception:
            pass
    return out


def rule(title):
    print("\n" + "=" * 78)
    print(title)
    print("=" * 78)


sweeps = [t for t in load("results/smp-*.json") + load("results/wrap-*.json")
          if t.get("ok")]
rule("1. PAUSE DURATION vs WHAT COMPLAINED  (4 vCPU, 2 NUMA nodes)")
print("guest_saw is the guest's own uptime step, sampled at 1 Hz, so any value")
print("at or below ~1.05 s is at the sampling floor and means 'nothing measurable'.")
groups = collections.defaultdict(lambda: collections.defaultdict(list))
for t in sweeps:
    key = (t["mode"], t.get("workload", "spin"), t.get("append", ""), t["arm"])
    groups[key][t["pause_seconds"]].append(t)
for key in sorted(groups):
    mode, workload, append, arm = key
    print("\n-- pause=%s  guest=%s  workload=%s%s" % (
        mode, arm, workload, ("  cmdline=" + append) if append else ""))
    for duration in sorted(groups[key]):
        rows = groups[key][duration]
        fired = [r for r in rows if r["mechanisms"]]
        mechanisms = sorted({m for r in rows for m in r["mechanisms"]})
        saw = [r["max_guest_uptime_step_seconds"] for r in rows
               if r["max_guest_uptime_step_seconds"] is not None]
        print("   %6.0fs  guest_saw=%-8s fired %d/%d  %s" % (
            duration, "%.1fs" % max(saw) if saw else "?",
            len(fired), len(rows), ", ".join(mechanisms) or "-"))

rule("2. EVERY TRIAL ACTUALLY STOPPED THE GUEST")
noisy = [t for t in sweeps if t.get("bytes_emitted_while_paused")]
print("trials emitting a console byte while paused: %d of %d" % (len(noisy), len(sweeps)))
loads = [t.get("runqueue_at_pause") for t in sweeps if t.get("runqueue_at_pause")]
print("trials recording the guest run-queue at the pause: %d" % len(loads))
if loads:
    runnable = [int(v.split("/")[0]) for v in loads]
    print("runnable tasks at the pause: min %d, median %d, max %d "
          "(4 spinners + the heartbeat shell is the intent)" % (
              min(runnable), sorted(runnable)[len(runnable) // 2], max(runnable)))

rule("3. FIRST-FIRE BOUNDARY, 3 repetitions per point (SIGSTOP, spin)")
for arm in ("kvm", "tcg"):
    counts = collections.defaultdict(list)
    for t in sweeps:
        if (t["arm"] == arm and t["mode"] == "sigstop"
                and t.get("workload") == "spin" and not t.get("append")):
            counts[t["pause_seconds"]].append(bool(t["mechanisms"]))
    cells = ["%.0fs=%d/%d" % (d, sum(v), len(v))
             for d, v in sorted(counts.items()) if len(v) >= 3]
    print("  %-4s %s" % (arm + ":", "  ".join(cells)))

rule("4. THE CLOCKSOURCE WRAP: predicted vs measured guest-visible time")
print("A pause longer than the clocksource's wrap period aliases to")
print("`pause mod wrap`, so a LONGER pause can look SHORTER to the guest.\n")
print("  %-9s %-9s %10s %10s   %s" % (
    "clocksrc", "wrap", "pause", "predicted", "measured (1 Hz floor ~1.05 s)"))
for t in sorted(sweeps, key=lambda r: (r.get("append", ""), r["pause_seconds"])):
    if t["mode"] != "sigstop" or t["arm"] != "tcg":
        continue
    source = "acpi_pm" if "acpi_pm" in t.get("append", "") else "hpet"
    wrap = ACPI_PM_WRAP if source == "acpi_pm" else HPET_WRAP
    saw = t["max_guest_uptime_step_seconds"]
    if saw is None:
        continue
    print("  %-9s %7.3fs %9.0fs %9.2fs   %.2fs%s" % (
        source, wrap, t["pause_seconds"], t["pause_seconds"] % wrap, saw,
        "   <- " + ",".join(t["mechanisms"]) if t["mechanisms"] else ""))

rule("5. CLOCK MODES BY BACKEND: guest-visible time across a 30 s stop/cont")
modes = load("results/clock-modes-*.json")
print("  %-22s %-10s %-11s %-12s %s" % (
    "mode", "requested", "in use", "across pause", "complained"))
for r in modes:
    if not r.get("ok"):
        note = r.get("error", "")
        short = re.sub(r"\s+", " ", note)[:88]
        print("  %-22s %-10s REFUSED: %s" % (
            r["mode"], r["requested_clocksource"][:10], short))
        continue
    print("  %-22s %-10s %-11s %-12s %s  [%s pause, sample %.4fs]" % (
        r["mode"], r["requested_clocksource"][:10], r.get("clocksource_in_use", "?"),
        r.get("guest_seconds_across_pause"), ",".join(r.get("mechanisms", [])) or "-",
        r["pause_mode"], r.get("sampling_period_median_seconds") or 0))

rule("6. WHY 1.01 s: the same pause seen at three sampling resolutions")
for f in ("results/why-redo.json", "results/why-scaling.json"):
    for r in load(f):
        print("  %-16s hold=%6.1fs  sampling_period=%-10s  across_pause=%-10s "
              "ratio_to_hold=%.2e" % (
                  r.get("observable", "C_kernel_printk"), r["hold_seconds"],
                  r["sampling_period_median_seconds"],
                  r["guest_seconds_across_pause"],
                  r["guest_seconds_across_pause"] / r["hold_seconds"]))

rule("7. PAUSE, READ WITH drgn, RESUME  (the capability)")
for f in sorted(glob.glob("results/pause-and-read-smp*.json")):
    s = json.load(open(f))
    if not s.get("ok"):
        print("  %s: %s" % (f, s.get("error")))
        continue
    print("  %s  arm=%s  kernel=%s" % (f, s["arm"], s["vmcoreinfo_osrelease"]))
    for p in s["pauses"]:
        cpus = collections.Counter(t.rsplit("cpu", 1)[1] for t in p["tasks"])
        print("    pause %d: held %.2fs  tasks=%d  reads=%d (%.1f MiB)  "
              "quiet=%d  last-ran-on cpu%s" % (
                  p["n"], p["held_wall_seconds"], p["task_count"],
                  p["physical_reads"], p["physical_bytes"] / (1 << 20),
                  p["bytes_emitted_while_paused"], dict(sorted(cpus.items()))))
    print("    max guest uptime step over the session: %s   complaints: %s" % (
        s.get("max_guest_uptime_step_seconds"), s.get("complaints") or "none"))
