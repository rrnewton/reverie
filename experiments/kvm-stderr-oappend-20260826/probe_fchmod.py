import os, subprocess, sys
HERMIT, BACKEND, GUEST = sys.argv[1], sys.argv[2], sys.argv[3]
path = f"/tmp/fchmod-div.{BACKEND}.err"
if os.path.exists(path): os.unlink(path)
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
before = oct(os.fstat(fd).st_mode & 0o7777)
p = subprocess.run([HERMIT, "run", "--backend", BACKEND, "--strict", "--verify", "--", GUEST],
                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=fd)
after = oct(os.fstat(fd).st_mode & 0o7777)
os.close(fd)
txt = open(path, errors="replace").read()
verdict = [l for l in txt.splitlines() if l.startswith(":: ") and ("Success" in l or "Failure" in l)]
print(f"backend={BACKEND} rc={p.returncode}  supervisor stderr mode {before} -> {after}")
print(f"  {verdict[0] if verdict else '(no verdict line)'}")
for l in txt.splitlines():
    if "Mismatch at" in l or "stderr_mode" in l:
        print("  ", l.strip()[:150])
