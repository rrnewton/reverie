#!/usr/bin/env python3
"""Same escape probe, but under --strict --verify (two runs in one process)."""
import fcntl, os, subprocess, sys
HERMIT, BACKEND = sys.argv[1], sys.argv[2]
GUEST = ["/usr/bin/awk", "BEGIN { print 42 }"]
path = f"/tmp/kvm-oappend-verify.{BACKEND}.err"
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
before = fcntl.fcntl(fd, fcntl.F_GETFL)
cmd = [HERMIT, "run", "--backend", BACKEND, "--strict", "--verify", "--"] + GUEST
p = subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=fd)
after = fcntl.fcntl(fd, fcntl.F_GETFL)
os.close(fd)
tail = open(path, "rb").read()[-600:].decode(errors="replace")
print(f"backend={BACKEND} rc={p.returncode} stdout={p.stdout!r}")
print(f"  hermit's own stderr F_GETFL: before=0x{before:x} after=0x{after:x} "
      f"O_APPEND-leaked={bool(~before & after & os.O_APPEND)}")
print("  --- last 600 bytes of hermit stderr ---")
print(tail)
