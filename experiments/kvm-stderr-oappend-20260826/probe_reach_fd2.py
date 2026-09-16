#!/usr/bin/env python3
"""What ELSE can the guest still do to the supervisor's stderr after #2694?"""
import fcntl, os, struct, subprocess, sys
HERMIT, BACKEND, KIND = sys.argv[1], sys.argv[2], sys.argv[3]  # KIND: file|pipe
GUEST = sys.argv[4]

def snap(fd, kind):
    s = {"F_GETFL": hex(fcntl.fcntl(fd, fcntl.F_GETFL)),
         "F_GETOWN": fcntl.fcntl(fd, fcntl.F_GETOWN)}
    if kind == "file":
        st = os.fstat(fd)
        s["size"] = st.st_size
        s["mode"] = oct(st.st_mode & 0o7777)
        s["offset"] = os.lseek(fd, 0, os.SEEK_CUR)
    else:
        s["pipe_sz"] = fcntl.fcntl(fd, 1032)  # F_GETPIPE_SZ
    return s

if KIND == "file":
    path = f"/tmp/reach-fd2.{BACKEND}.err"
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
    os.write(fd, b"supervisor diagnostics line\n")
    keep = fd
else:
    r, w = os.pipe()
    fd, keep = w, w

before = snap(keep, KIND)
p = subprocess.run([HERMIT, "run", "--backend", BACKEND, "--", GUEST],
                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=fd)
after = snap(keep, KIND)
print(f"--- backend={BACKEND} stderr={KIND} rc={p.returncode} ---")
print("guest attempts:")
for line in p.stdout.decode(errors="replace").splitlines():
    print("   ", line)
print("supervisor's descriptor:")
for k in before:
    mark = "  CHANGED <<<" if before[k] != after[k] else ""
    print(f"    {k:9s} {before[k]!r} -> {after[k]!r}{mark}")
