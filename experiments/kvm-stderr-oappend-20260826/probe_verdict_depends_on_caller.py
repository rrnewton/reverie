#!/usr/bin/env python3
"""The --verify VERDICT under KVM is a function of the CALLER's stderr flags.

Run the identical command twice, differing only in whether the stderr hermit
inherits already has O_APPEND set.  If the verdict flips, the verification
result is not a property of the guest at all.
"""
import fcntl, os, subprocess, sys
HERMIT = sys.argv[1]
GUEST = ["/usr/bin/awk", "BEGIN { print 42 }"]
cmd = [HERMIT, "run", "--backend", "kvm", "--strict", "--verify", "--"] + GUEST

for label, preset in (("stderr WITHOUT O_APPEND", False), ("stderr WITH O_APPEND", True)):
    path = "/tmp/kvm-verdict-probe.err"
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    if preset:
        fcntl.fcntl(fd, fcntl.F_SETFL, fcntl.fcntl(fd, fcntl.F_GETFL) | os.O_APPEND)
    before = fcntl.fcntl(fd, fcntl.F_GETFL)
    p = subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=fd)
    after = fcntl.fcntl(fd, fcntl.F_GETFL)
    os.close(fd)
    text = open(path, "rb").read().decode(errors="replace")
    verdict = [l for l in text.splitlines() if l.startswith(":: ") and
               ("Success" in l or "Failure" in l)]
    print(f"{label:26s} caller fd 0x{before:x} -> 0x{after:x}  rc={p.returncode:3d}  "
          f"{verdict[0] if verdict else '(no verdict line)'}")
