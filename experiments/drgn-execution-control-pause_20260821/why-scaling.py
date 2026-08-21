#!/usr/bin/env python3
"""Does the residual scale with the pause, or is it fixed resume overhead?

Observable C only (kernel printk, 1 us).  If the residual is proportional to
the hold, some clock is still running at a reduced rate.  If it is constant,
it is the fixed cost of stopping and restarting the vCPUs.
"""
import sys, json
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from why_101ms_lib import trial, WORKLOAD_KMSG, KM

here = Path(__file__).resolve().parent
out = []
arms = sys.argv[1].split(",") if len(sys.argv) > 1 else ["kvm"]
holds = [float(h) for h in (sys.argv[2].split(",") if len(sys.argv) > 2
                            else "5,30,120,300".split(","))]
for arm in arms:
    for hold in holds:
        r = trial(arm, WORKLOAD_KMSG, KM,
                  here/"runs"/"scaling"/arm/("kmsg-%gs" % hold), hold)
        r["arm"] = arm
        out.append(r)
        print("%-7s hold=%6.1fs  guest_seconds_across_pause=%-12s "
              "sampling_period=%-10s  ratio_to_hold=%.2e" % (
                  arm, hold, r["guest_seconds_across_pause"],
                  r["sampling_period_median_seconds"],
                  r["guest_seconds_across_pause"]/hold), flush=True)
        Path("results/why-scaling.json").write_text(json.dumps(out, indent=2))
