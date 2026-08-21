#!/usr/bin/env python3
"""Does a 4-vCPU 2-NUMA guest boot, and does the KERNEL agree it has four?

`-smp 4` is a request to QEMU, not evidence about the guest.  The guest is
asked directly: how many CPUs are online, and how many NUMA nodes does it see.
"""
import sys, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from guest_smp import Guest, ARMS

PROBE = (
    "echo CFG online=$(cat /sys/devices/system/cpu/online)"
    " present=$(cat /sys/devices/system/cpu/present)"
    " nproc=$(nproc 2>/dev/null)"
    " numa=$(ls -d /sys/devices/system/node/node* 2>/dev/null | wc -l); "
    "echo CFG cpus_in_stat=$(grep -c '^cpu[0-9]' /proc/stat)"
    " model=$(grep -c '^processor' /proc/cpuinfo)"
    " clocksource=$(cat /sys/devices/system/clocksource/clocksource0/current_clocksource); "
    "echo CFG_DONE"
)

for arm in sys.argv[1:] or ["kvm", "tcg", "icount"]:
    g = Guest(arm, Path("runs/smoke-smp")/arm, PROBE, cpus=4, numa_nodes=2)
    t0 = time.monotonic()
    try:
        g.start(timeout=300)
        g.serial.wait_for(b"CFG_DONE", 120)
        print("=== %s (boot %.1fs) ===" % (arm, time.monotonic()-t0))
        for line in g.serial.text().splitlines():
            if line.startswith("CFG "):
                print("  " + line.strip())
        for line in g.serial.text().splitlines():
            if "smpboot" in line or "NUMA" in line or "Brought up" in line:
                print("  [kernel] " + line.strip()[:130])
    except Exception as e:
        print("=== %s FAILED: %s" % (arm, e))
        if g.serial: print("\n".join(g.serial.text().splitlines()[-12:]))
    finally:
        g.close()
