#!/usr/bin/env python3
"""Re-measure "why 1.01s" with the backlog drain in place."""
import sys, json
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from why_101ms_lib import trial, WORKLOAD_1HZ, WORKLOAD_TIGHT, WORKLOAD_KMSG, HB, KM

here = Path(__file__).resolve().parent
arm = sys.argv[1] if len(sys.argv) > 1 else "kvm"
holds = [float(h) for h in (sys.argv[2].split(",") if len(sys.argv) > 2 else ["30"])]
obs = [("A_heartbeat_1hz", WORKLOAD_1HZ, HB),
       ("B_tight_loop", WORKLOAD_TIGHT, HB),
       ("C_kernel_printk", WORKLOAD_KMSG, KM)]
out = []
for hold in holds:
    for name, w, pat in obs:
        r = trial(arm, w, pat, here/"runs"/"why2"/arm/("%s-%gs" % (name, hold)), hold)
        r["observable"] = name; r["arm"] = arm
        out.append(r)
        print("%-16s hold=%6.1fs period=%-10s across_pause=%-12s ratio=%.2e" % (
            name, hold, r["sampling_period_median_seconds"],
            r["guest_seconds_across_pause"],
            r["guest_seconds_across_pause"]/hold), flush=True)
        Path("results/why-redo.json").write_text(json.dumps(out, indent=2))
