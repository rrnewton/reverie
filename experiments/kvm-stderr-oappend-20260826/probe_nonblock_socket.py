import socket, subprocess, sys
H, B, G = sys.argv[1], sys.argv[2], sys.argv[3]
a, b = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
b.sendall(b"A")                      # data is available, so this is not a blocking question
p = subprocess.run([H, "run", "--backend", B, "--", G],
                   stdin=a.fileno(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
out = p.stdout.decode(errors="replace").strip().replace("\n", " | ")
err = [l for l in p.stderr.decode(errors="replace").splitlines()
       if "Invariant violation" in l or "panic" in l.lower()]
print(f"backend={B:6s} rc={p.returncode:4d}  guest: {out or '(no stdout)'}")
for l in err[:3]:
    print("    PANIC:", l.strip()[:170])
