"""Replay the Meta launcher's recorded payload argv with a different executable."""
import os, shlex, subprocess, sys, json, hashlib
record, exe, label, extra_path = sys.argv[1:5]
line = next(l for l in open(record) if l.startswith("ARGV "))
argv = shlex.split(line[5:])
argv[0] = exe
env = dict(os.environ)
if extra_path:
    env["PATH"] = extra_path + ":" + env["PATH"]
h = hashlib.sha256(open(exe, "rb").read()).hexdigest()
p = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=90)
out = {"label": label, "exe": exe, "sha256": h, "rc": p.returncode,
       "path_prefix": extra_path or None, "stdout": p.stdout[-2000:], "stderr": p.stderr[-2000:]}
print(json.dumps(out, indent=1))
