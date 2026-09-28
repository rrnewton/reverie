from pathlib import Path
import datetime
import hashlib
import json
import resource
import subprocess
import time

PACKET = Path(__file__).resolve().parent


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_inputs() -> int:
    data = json.loads((PACKET / "INPUTS.json").read_text())
    for record in data["records"]:
        path = PACKET / record["path"]
        assert path.stat().st_size == record["bytes"], record["path"]
        assert sha256(path) == record["sha256"], record["path"]
    return len(data["records"])


def write_new(name: str, data: object) -> None:
    with (PACKET / name).open("x") as output:
        json.dump(data, output, indent=2)
        output.write("\n")


input_count = check_inputs()
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
record = {
    "command": command,
    "cwd": str(PACKET),
    "review_target": "0598b5ffbeb737866372f89224d915efaeb29943",
    "inputs_sha256": sha256(PACKET / "INPUTS.json"),
    "prompt_sha256": sha256(PACKET / "prompt.txt"),
    "source_entries": input_count,
    "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "wall_seconds": 1200,
    "kill_after_seconds": 10,
    "per_output_file_bytes": 67108864,
}
write_new("launch.json", record)


def limits() -> None:
    resource.setrlimit(resource.RLIMIT_FSIZE, (67108864, 67108864))


started = time.monotonic()
with (
    (PACKET / "prompt.txt").open("rb") as prompt,
    (PACKET / "output.jsonl").open("xb") as output,
    (PACKET / "stderr.log").open("xb") as stderr,
):
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
    elapsed_seconds=time.monotonic() - started,
    completed_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
    inputs_unchanged=check_inputs() == input_count,
    prompt_unchanged=sha256(PACKET / "prompt.txt") == record["prompt_sha256"],
)
write_new("exit.json", record)
print(json.dumps(record))
assert record["inputs_unchanged"] and record["prompt_unchanged"]
raise SystemExit(result.returncode)
