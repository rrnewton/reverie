import os, sys
# Read-heavy: many small read() syscalls on a regular fd. This is the path the
# new sock_diag guard now tests a flag on, so it is the one to measure.
path, chunk = sys.argv[1], int(sys.argv[2])
total = 0
fd = os.open(path, os.O_RDONLY)
while True:
    b = os.read(fd, chunk)
    if not b:
        break
    total += len(b)
os.close(fd)
print("bytes", total)
