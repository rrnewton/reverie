from pathlib import Path
import datetime
import hashlib
import json

PACKET = Path(__file__).resolve().parent
EXCLUDED = {
    "INPUTS.json",
    "launch.json",
    "output.jsonl",
    "stderr.log",
    "exit.json",
}

records = []
for path in sorted(PACKET.rglob("*")):
    if not path.is_file():
        continue
    relative = path.relative_to(PACKET).as_posix()
    if relative in EXCLUDED:
        continue
    data = path.read_bytes()
    records.append(
        {
            "path": relative,
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }
    )

manifest = {
    "schema": "kvm-claude-review-inputs-v1",
    "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "records": records,
}
with (PACKET / "INPUTS.json").open("x") as output:
    json.dump(manifest, output, indent=2)
    output.write("\n")
