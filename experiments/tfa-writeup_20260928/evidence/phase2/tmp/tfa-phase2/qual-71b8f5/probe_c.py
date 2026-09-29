import json, sys, tempfile
from pathlib import Path
sys.path.insert(0, "/tmp/tfa-phase2-controller-followup-20260924/fbcode")
from testinfra.tfa.process import run_process, diagnostic_environment
root = Path(tempfile.mkdtemp(prefix="qual71-bootstrap-", dir="/tmp/tfa-phase2"))
env = diagnostic_environment(root/"logs")
r = run_process("/usr/local/bin/codex", ("--version",), root, root/"run", 30, env)
md = json.loads((root/"run"/"metadata.json").read_text())
print("bootstrap_rc:", r.returncode, "stdout:", (root/"run"/"stdout.txt").read_text()[:80] if (root/"run"/"stdout.txt").exists() else r.stdout_path.read_text()[:80])
print("payload_isolation:", md.get("payload_isolation"), "containment_empty:", md.get("containment_empty"), "cgroup_exists_after:", Path(md["cgroup"]).exists(), "descendants_zero:", md.get("containment_empty"))
