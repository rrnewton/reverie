#!/usr/bin/env python3
"""Per path: did the guest ATTEMPT the mutation, did IT see the change, and did
the SUPERVISOR's descriptor move? A path where the guest never attempts cannot
demonstrate blocking, and is labelled as such rather than counted as a pass."""
import fcntl, os, subprocess, sys
HERMIT, LABEL = sys.argv[1], sys.argv[2]
BASE = "/home/newton/work/dev-hermit/experiments/kvm-stderr-oappend-20260826"
GUEST = f"{BASE}/bin/stdio_flag_report"
PATHS = [
    ("run --verify ptrace",   ["run","--backend","ptrace","--strict","--verify","--"]),
    ("run --verify kvm",      ["run","--backend","kvm","--strict","--verify","--"]),
    ("run --verify dbt",      ["run","--backend","dbt","--strict","--verify","--"]),
    ("record start --verify", ["record","start","--verify","--"]),
]
print(f"===== {LABEL} =====")
for name, args in PATHS:
    slug = name.replace(" ","_").replace("-","")
    report = f"{BASE}/rep.{slug}.txt"
    errf   = f"{BASE}/err.{slug}"
    for p in (report, errf):
        if os.path.exists(p): os.unlink(p)
    fd = os.open(errf, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    before = fcntl.fcntl(fd, fcntl.F_GETFL)
    env = dict(os.environ, FLAG_REPORT=report)
    try:
        p = subprocess.run([HERMIT]+args+[GUEST], stdin=subprocess.DEVNULL,
                           stdout=subprocess.DEVNULL, stderr=fd, env=env, timeout=180)
        rc = str(p.returncode)
    except subprocess.TimeoutExpired:
        rc = "TIMEOUT"
    after = fcntl.fcntl(fd, fcntl.F_GETFL); os.close(fd)
    lines = open(report).read().strip().splitlines() if os.path.exists(report) else []
    attempts = sum(1 for l in lines if "attempted=1" in l)
    words = [l.split("guest_fd2_before=")[1].split()[0] for l in lines if "guest_fd2_before=" in l]
    handed = words[0] if words else "?"
    leaked = "LEAKED" if (~before & after & os.O_APPEND) else "clean "
    verdict = "vacuous - guest never attempted" if attempts == 0 else "MEANINGFUL"
    print(f"  {name:22s} rc={rc:8s} runs_reporting={len(lines)} attempts={attempts} "
          f"guest_was_handed={handed} supervisor 0x{before:x}->0x{after:x} {leaked}  [{verdict}]")
