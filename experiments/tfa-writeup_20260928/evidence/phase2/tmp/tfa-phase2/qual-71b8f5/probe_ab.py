import json, sys
from pathlib import Path
sys.path.insert(0, "/tmp/tfa-phase2-controller-followup-20260924/fbcode")
from testinfra.tfa.process import _codex_environment_preflight, ProcessError
from testinfra.tfa.state import regular_file_record
root = Path("/tmp/tfa-phase2/qual-71b8f5")
resolved = Path("/usr/local/bin/codex").resolve(strict=True)
print("resolved:", resolved)
print("record:", json.dumps(regular_file_record(resolved), sort_keys=True))
try:
    rec = _codex_environment_preflight(resolved.parent, root/"preflight-artifacts", None)
    print("PREFLIGHT_ELIGIBLE:", rec)
except ProcessError as e:
    print("PREFLIGHT_REFUSED:", e)
ident = list((root/"preflight-artifacts").glob("*/identity.json")) + list((root/"preflight-artifacts").glob("identity.json"))
for p in (root/"preflight-artifacts").rglob("identity.json"):
    print("identity.json:", p.read_text())
