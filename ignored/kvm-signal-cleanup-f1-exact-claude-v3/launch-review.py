from pathlib import Path
import datetime
import hashlib
import json
import resource
import subprocess
import time


PACKET = Path(__file__).resolve().parent
INPUT_NAMES = ["prompt.txt", "BASE-TO-HEAD.patch", "QUALIFICATION.md"]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def snapshot():
    return {
        name: {"bytes": (PACKET / name).stat().st_size, "sha256": sha(PACKET / name)}
        for name in INPUT_NAMES
    }


def write_exclusive(name, value):
    with (PACKET / name).open("x") as output:
        json.dump(value, output, indent=2)
        output.write("\n")


command = [
    "timeout",
    "--signal=TERM",
    "--kill-after=10s",
    "1200s",
    "with-proxy",
    "claude",
    "-p",
    "--no-session-persistence",
    "--output-format",
    "stream-json",
    "--verbose",
    "--permission-mode",
    "plan",
    "--tools",
    "Read,Grep,Glob",
    "--allowedTools",
    "Read,Grep,Glob",
]
inputs = snapshot()
record = {
    "command": command,
    "cwd": str(PACKET),
    "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "wall_seconds": 1200,
    "kill_after_seconds": 10,
    "per_output_file_bytes": 67108864,
    "inputs": inputs,
}
write_exclusive("launch.json", record)


def limits():
    resource.setrlimit(resource.RLIMIT_FSIZE, (67108864, 67108864))


start = time.monotonic()
with (PACKET / "prompt.txt").open("rb") as prompt, (PACKET / "output.jsonl").open(
    "xb"
) as output, (PACKET / "stderr.log").open("xb") as stderr:
    result = subprocess.run(
        command,
        cwd=PACKET,
        stdin=prompt,
        stdout=output,
        stderr=stderr,
        preexec_fn=limits,
    )
record.update(
    exit_code=result.returncode,
    elapsed_seconds=time.monotonic() - start,
    completed_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
    inputs_unchanged=snapshot() == inputs,
)
write_exclusive("exit.json", record)
print(json.dumps(record))
if not record["inputs_unchanged"]:
    raise SystemExit("review inputs changed during execution")
raise SystemExit(result.returncode)
