#!/usr/bin/env python3
"""Does a KVM guest mutate hermit's OWN stderr file description?

Opens a regular file, hands it to hermit as fd 2 (subprocess dups it onto the
child's fd 2 -- SAME open file description), runs a guest that sets O_APPEND on
its stderr, then re-reads the flags of OUR descriptor.  If the bit is set
afterwards, the guest reached out of the container and changed the parent's
file description.
"""
import fcntl
import os
import subprocess
import sys

HERMIT = sys.argv[1]
BACKEND = sys.argv[2]  # ptrace | kvm
GUEST = ["/usr/bin/awk", "BEGIN { print 42 }"]

path = f"/tmp/kvm-oappend-probe.{BACKEND}.err"
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
before = fcntl.fcntl(fd, fcntl.F_GETFL)
cmd = [HERMIT, "run", "--backend", BACKEND, "--"] + GUEST
proc = subprocess.run(cmd, stdin=subprocess.DEVNULL,
                      stdout=subprocess.PIPE, stderr=fd)
after = fcntl.fcntl(fd, fcntl.F_GETFL)
os.close(fd)
print(f"backend={BACKEND} rc={proc.returncode} stdout={proc.stdout!r}")
print(f"  our fd F_GETFL before = 0x{before:x}")
print(f"  our fd F_GETFL after  = 0x{after:x}")
print(f"  O_APPEND (0x400) leaked into the parent's description: "
      f"{bool(~before & after & os.O_APPEND)}")
