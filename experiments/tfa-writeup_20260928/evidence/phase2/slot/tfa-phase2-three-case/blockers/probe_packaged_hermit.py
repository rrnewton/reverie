import hashlib, json, os, sys, time
from pathlib import Path
from testinfra.tfa import hermit
from testinfra.tfa.manifest import load_manifest

out = Path(sys.argv[1])
manifest = load_manifest(Path(sys.argv[2]))
records = []
for case in manifest.cases:
    for outcome, argv in (("pass", case.pass_argv), ("fail", case.fail_argv)):
        destination = out / "probe-destinations" / case.case_id / outcome / "hermit"
        entry = {"case_id": case.case_id, "outcome": outcome, "destination": str(destination)}
        try:
            hermit.packaged_hermit_argv(argv, case.hermit_revision, case.hermit_sha256, destination)
            entry["result"] = "UNEXPECTED_SUCCESS"
        except Exception as error:
            entry["result"] = "REFUSED"
            entry["exception_type"] = type(error).__name__
            entry["exception"] = str(error)
        entry["destination_exists_after"] = os.path.lexists(destination)
        records.append(entry)
for case in manifest.cases:
    for seed in case.fresh_stress_seeds:
        records.append({"case_id": case.case_id, "stress_seed": seed,
                        "result": "NOT_RUN", "reason": "stress replays use the same packaged Hermit gate refused above"})
(out / "packaged-hermit-probe.json").write_text(json.dumps({
    "schema": "tfa.phase2.packaged-hermit-probe.v1",
    "hermit_module": hermit.__file__,
    "hermit_module_sha256": hashlib.sha256(Path(hermit.__file__).read_bytes()).hexdigest(),
    "wall_time_ns": time.time_ns(),
    "records": records}, indent=2, sort_keys=True) + "\n")
print(json.dumps([(r["case_id"], r.get("outcome", r.get("stress_seed")), r["result"]) for r in records]))
