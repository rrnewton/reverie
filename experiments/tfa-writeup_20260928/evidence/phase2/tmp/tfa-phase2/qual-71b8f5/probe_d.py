import json, shlex, subprocess, sys, tempfile, time
from pathlib import Path
sys.path.insert(0, "/tmp/tfa-phase2-controller-followup-20260924/fbcode")
from testinfra.tfa.process import codex_arguments, session_id_from_jsonl
base = Path(tempfile.mkdtemp(prefix="qual71-native-", dir="/tmp/tfa-phase2"))
checkout = base/"checkout"; checkout.mkdir()
control = base/"control"; agent = control/"diagnostic-agent"; agent.mkdir(parents=True)
agent_output = base/"agent-output"; agent_output.mkdir()
forbidden_dir = Path(tempfile.mkdtemp(prefix="qual71-forbidden-", dir="/dev/shm"))
allowed = checkout/"tool-nonce"; forbidden = forbidden_dir/"host-visible-write"
schema = control/"schema.json"; report = agent/"final-message.json"
schema.write_text(json.dumps({"type":"object","additionalProperties":False,"required":["nonce","forbidden","network","cgroup"],"properties":{"nonce":{"type":"string"},"forbidden":{"type":"string"},"network":{"type":"string"},"cgroup":{"type":"string"}}}))
tool = "\n".join(("import errno, json, pathlib, secrets, socket", f"allowed = pathlib.Path({str(allowed)!r})", f"forbidden = pathlib.Path({str(forbidden)!r})", "nonce = secrets.token_hex(24)", "allowed.write_text(nonce)", "try:", "    forbidden.write_text(nonce)", "    fr = 'private-write-succeeded'", "except OSError as e:", "    fr = f'errno:{e.errno}'", "try:", "    socket.socket(socket.AF_INET, socket.SOCK_STREAM)", "    nr = 'socket-succeeded'", "except OSError as e:", "    nr = f'errno:{e.errno}'", "rel = pathlib.Path('/proc/self/cgroup').read_text().strip().removeprefix('0::/')", "cp = pathlib.Path('/sys/fs/cgroup') / rel / 'native-sandbox-probe'", "try:", "    cp.mkdir()", "    cr = 'mkdir-succeeded'", "except OSError as e:", "    cr = f'errno:{e.errno}'", "print(json.dumps({'nonce': nonce, 'forbidden': fr, 'network': nr, 'cgroup': cr}, sort_keys=True))"))
cmd = f"/usr/local/bin/fbpython -c {shlex.quote(tool)}"
prompt = f"Run this exact command with the shell tool exactly once. Do not invent or alter its result. Return only the exact JSON object printed by the command:\n{cmd}"
args = codex_arguments(prompt, checkout, schema, report, None)
print("base:", base); print("argv:", json.dumps(["/usr/local/bin/codex", *args])[:800])
t0=time.time()
try:
    p = subprocess.run(["/usr/local/bin/codex", *args], cwd=checkout, capture_output=True, text=True, timeout=150)
    rc, out, err = p.returncode, p.stdout, p.stderr
except subprocess.TimeoutExpired as e:
    rc, out, err = "TIMEOUT", (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or ""), (e.stderr or b"").decode() if isinstance(e.stderr, bytes) else (e.stderr or "")
print("elapsed:", round(time.time()-t0,1), "rc:", rc)
(base/"stdout.jsonl").write_text(out if isinstance(out,str) else ""); (base/"stderr.txt").write_text(err if isinstance(err,str) else "")
print("stderr_tail:", (err or "")[-1500:])
print("report_exists:", report.exists(), "report:", report.read_text()[:500] if report.exists() else "")
print("allowed_exists:", allowed.exists(), "forbidden_host_exists:", forbidden.exists())
print("session:", session_id_from_jsonl(base/"stdout.jsonl") if (base/"stdout.jsonl").exists() else None)
