import fcntl, os, subprocess, sys
H, B, G = sys.argv[1], sys.argv[2], sys.argv[3]
path = f"/tmp/latch.{B}.err"
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
before = fcntl.fcntl(fd, fcntl.F_GETFL)
p = subprocess.run([H,"run","--backend",B,"--",G], stdin=subprocess.DEVNULL,
                   stdout=subprocess.PIPE, stderr=fd, timeout=120)
after = fcntl.fcntl(fd, fcntl.F_GETFL)
os.close(fd)
esc = []
if ~before & after & os.O_APPEND:   esc.append("O_APPEND")
if ~before & after & os.O_NONBLOCK: esc.append("O_NONBLOCK")
print(f"backend={B:6s} rc={p.returncode}  guest: {p.stdout.decode().strip()}")
print(f"   supervisor fd 0x{before:x} -> 0x{after:x}   escaped: {', '.join(esc) or 'nothing'}")
