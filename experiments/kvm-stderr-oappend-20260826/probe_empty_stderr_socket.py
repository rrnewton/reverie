import socket, subprocess, sys
H, B, G = sys.argv[1], sys.argv[2], sys.argv[3]
a, b = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)   # empty; stderr side
try:
    p = subprocess.run([H,"run","--backend",B,"--",G], stdin=subprocess.DEVNULL,
                       stdout=subprocess.PIPE, stderr=a.fileno(), timeout=25)
    print(f"backend={B:6s} rc={p.returncode:4d}  guest: {p.stdout.decode(errors='replace').strip() or '(none)'}")
except subprocess.TimeoutExpired:
    print(f"backend={B:6s} ⚠️ TIMED OUT after 25s — the nonblocking recv on stderr BLOCKED")
