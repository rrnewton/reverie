from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, "/tmp/tfa-phase2-controller-followup-20260924/fbcode")

from testinfra.tfa.tests.test_production_containment import (  # noqa: E402
    _disposable_btrfs_verify_root,
)


manifest_path = Path(
    "/tmp/tfa-phase2-controller-followup-20260924/fbcode/testinfra/tfa/cases.v1.json"
)
case_id, outcome, binary, output_value, *options = sys.argv[1:]
output = Path(output_value)
output.mkdir(parents=True, exist_ok=False)
manifest = json.loads(manifest_path.read_text())
case = next(value for value in manifest["cases"] if value["case_id"] == case_id)
argv = list(case[f"{outcome}_argv"])
argv[argv.index("--sh-report") + 1] = str(output / "safehermit.txt")
argv[argv.index("--sh-disk-limit") + 2] = binary
argv.remove("--verify-strict")
argv.remove("--keep-logs")
verify_log = next(value for value in argv if value.startswith("--verify-log-dir="))
argv.remove(verify_log)
argv.remove("--chaos-per-thread-slowdown")
argv = [
    value
    for value in argv
    if not value.startswith("--chaos-slowdown-max-factor=")
    and not value.startswith("--chaos-epoch-length-ns=")
]
if "--target-timeslice" in argv:
    index = argv.index("--target-timeslice")
    del argv[index : index + 2]
if "--max-timeslice" in argv:
    argv[argv.index("--max-timeslice")] = "--preemption-timeout"
argv.insert(argv.index("--"), f"--workdir={case['working_directory']}")
environment_boundary = argv.index("--")
argv[environment_boundary:environment_boundary] = [
    "--env",
    "ASAN_OPTIONS=disable_coredump=1",
]
if options == ["single"]:
    argv.remove("--verify")
    verify_allow = argv.index("--verify-allow")
    del argv[verify_allow : verify_allow + 2]
started = time.monotonic()
with _disposable_btrfs_verify_root() as verify_root:
    environment = dict(os.environ)
    environment["SAFEHERMIT_LOG_ROOT"] = str(verify_root)
    completed = subprocess.run(
        argv,
        cwd=case["working_directory"],
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=150,
    )
(output / "stdout.txt").write_bytes(completed.stdout)
(output / "stderr.txt").write_bytes(completed.stderr)
(output / "result.json").write_text(
    json.dumps(
        {
            "argv": argv,
            "case_id": case_id,
            "duration_seconds": time.monotonic() - started,
            "outcome_label": outcome,
            "returncode": completed.returncode,
        },
        sort_keys=True,
    )
    + "\n"
)
print((output / "result.json").read_text(), end="")
