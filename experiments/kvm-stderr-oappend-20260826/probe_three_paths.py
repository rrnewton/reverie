#!/usr/bin/env python3
"""Does the guest mutate the supervisor's stderr on EACH of the three two-run
verify paths, and what verdict does each return?"""
import fcntl, os, subprocess, sys

HERMIT, LABEL = sys.argv[1], sys.argv[2]
GUEST = ["/usr/bin/awk", "BEGIN { print 42 }"]

PATHS = [
    ("run --verify ptrace", ["run", "--backend", "ptrace", "--strict", "--verify", "--"]),
    ("run --verify kvm",    ["run", "--backend", "kvm",    "--strict", "--verify", "--"]),
    ("run --verify dbt",    ["run", "--backend", "dbt",    "--strict", "--verify", "--"]),
    ("record start --verify", ["record", "start", "--verify", "--"]),
]

print(f"===== {LABEL} =====")
for name, args in PATHS:
    path = f"/tmp/three-paths.{name.replace(' ','_').replace('-','')}.err"
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    before = fcntl.fcntl(fd, fcntl.F_GETFL)
    try:
        p = subprocess.run([HERMIT] + args + GUEST, stdin=subprocess.DEVNULL,
                           stdout=subprocess.PIPE, stderr=fd, timeout=180)
        rc = p.returncode
    except subprocess.TimeoutExpired:
        rc = "TIMEOUT"
    after = fcntl.fcntl(fd, fcntl.F_GETFL)
    os.close(fd)
    text = open(path, errors="replace").read()
    verdict = next((l.strip() for l in text.splitlines()
                    if l.startswith(":: ") and ("Success" in l or "Failure" in l)), "")
    leaked = "LEAKED" if (~before & after & os.O_APPEND) else "clean "
    print(f"  {name:22s} rc={str(rc):8s} supervisor 0x{before:x}->0x{after:x} {leaked}  {verdict[:52]}")
