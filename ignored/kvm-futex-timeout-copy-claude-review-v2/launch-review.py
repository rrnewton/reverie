from pathlib import Path
import datetime
import hashlib
import json
import resource
import shutil
import subprocess
import time

PACKET = Path(__file__).resolve().parent
REPO = PACKET.parents[1]
V1 = REPO / "ignored/kvm-futex-timeout-copy-claude-review-v1"
DEV = Path("/home/newton/work/dev-hermit")
BASE = "123df7c4c0169006fbfe1f11a1553fe333eac937"
PRIOR = "8865eb263cb44530364cb90eda6cdb40826aba7d"
HEAD = "c4df9b6a646beaa506187058965876e56ebccc7a"
TREE = "635ec6b1e57f529e8bb77b8e02ef9b3180c4a724"
CHANGED = [
    "reverie-kvm/src/executor.rs",
    "reverie-kvm/src/executor/entry_host_wait_tests.rs",
    "reverie-kvm/tests/static_elf.rs",
]


def run_bytes(args, cwd):
    return subprocess.check_output(args, cwd=cwd)


def digest_bytes(value):
    return hashlib.sha256(value).hexdigest()


def digest(path):
    return digest_bytes(path.read_bytes())


def write_new_bytes(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as output:
        output.write(value)


def write_new_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")


def copy_new(source, destination):
    write_new_bytes(destination, source.read_bytes())


def assert_source_state():
    assert run_bytes(["git", "rev-parse", "HEAD"], REPO).decode().strip() == HEAD
    assert run_bytes(["git", "rev-parse", "HEAD^{tree}"], REPO).decode().strip() == TREE
    assert run_bytes(
        ["git", "status", "--porcelain", "--untracked-files=no"], REPO
    ) == b""
    actual = run_bytes(
        ["git", "diff", "--name-only", f"{BASE}..{HEAD}"], REPO
    ).decode().splitlines()
    assert actual == CHANGED, actual


def materialize_inputs():
    assert_source_state()
    write_new_bytes(
        PACKET / "BASE-TO-HEAD.patch",
        run_bytes(["git", "diff", "--binary", "--full-index", f"{BASE}..{HEAD}"], REPO),
    )
    write_new_bytes(
        PACKET / "ROUND2.patch",
        run_bytes(["git", "diff", "--binary", "--full-index", f"{PRIOR}..{HEAD}"], REPO),
    )
    for revision, destination in ((BASE, "base"), (PRIOR, "prior")):
        for path in CHANGED:
            value = run_bytes(["git", "show", f"{revision}:{path}"], REPO)
            write_new_bytes(PACKET / destination / path, value)
    shutil.copytree(
        REPO / "reverie-kvm/src",
        PACKET / "candidate/reverie-kvm/src",
        copy_function=lambda source, destination: shutil.copyfile(source, destination),
    )
    copy_new(
        REPO / "reverie-kvm/tests/static_elf.rs",
        PACKET / "candidate/reverie-kvm/tests/static_elf.rs",
    )
    for name in ("paper.txt", "scheduler.rs", "PROJECT_VISION.md", "hermit-v2-roadmap.md"):
        copy_new(V1 / "primary" / name, PACKET / "primary" / name)
    for name in ("code-review-SKILL.md", "deterministic-scheduling-review-SKILL.md"):
        copy_new(V1 / "context" / name, PACKET / "context" / name)
    prior_events = (V1 / "output.jsonl").read_text().splitlines()
    prior_result = None
    for event in prior_events:
        parsed = json.loads(event)
        if parsed.get("type") == "result":
            prior_result = parsed["result"]
    assert prior_result is not None
    write_new_bytes(
        PACKET / "context/PRIOR-CLAUDE-REVIEW.txt",
        (prior_result + "\n").encode(),
    )
    copy_new(
        V1 / "canonical-refusal-comment.md",
        PACKET / "context/canonical-refusal-comment.md",
    )
    copy_new(
        REPO / "ignored/kvm-futex-timeout-copy-v11/pr-body.md",
        PACKET / "PROPOSED-PR-BODY.md",
    )
    for generation in ("v10", "v11", "v12"):
        source_dir = REPO / f"ignored/kvm-futex-timeout-copy-{generation}"
        destination_dir = PACKET / f"qualification/{generation}"
        destination_dir.mkdir(parents=True)
        for source in sorted(source_dir.iterdir()):
            if source.is_file():
                copy_new(source, destination_dir / source.name)
    source = {
        "repo": str(REPO),
        "base": BASE,
        "prior_refused_head": PRIOR,
        "head": HEAD,
        "tree": TREE,
        "changed_paths": CHANGED,
        "base_patch_sha256": digest(PACKET / "BASE-TO-HEAD.patch"),
        "round2_patch_sha256": digest(PACKET / "ROUND2.patch"),
        "created_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    }
    write_new_json(PACKET / "SOURCE.json", source)
    roots = [
        PACKET / "prompt.txt",
        PACKET / "BASE-TO-HEAD.patch",
        PACKET / "ROUND2.patch",
        PACKET / "SOURCE.json",
        PACKET / "PROPOSED-PR-BODY.md",
        PACKET / "base",
        PACKET / "prior",
        PACKET / "candidate",
        PACKET / "primary",
        PACKET / "context",
        PACKET / "qualification",
    ]
    paths = []
    for root in roots:
        if root.is_file():
            paths.append(root)
        else:
            paths.extend(path for path in root.rglob("*") if path.is_file())
    records = []
    for path in sorted(paths):
        value = path.read_bytes()
        records.append(
            {
                "path": str(path.relative_to(PACKET)),
                "bytes": len(value),
                "sha256": digest_bytes(value),
            }
        )
    write_new_json(PACKET / "INPUTS.json", {"schema": 1, "records": records})


def check_inputs():
    data = json.loads((PACKET / "INPUTS.json").read_text())
    for record in data["records"]:
        path = PACKET / record["path"]
        assert path.stat().st_size == record["bytes"], record["path"]
        assert digest(path) == record["sha256"], record["path"]
    return len(data["records"])


materialize_inputs()
count = check_inputs()
command = [
    "timeout", "--signal=TERM", "--kill-after=10s", "2400s",
    "with-proxy", "claude", "-p", "--no-session-persistence",
    "--output-format", "stream-json", "--verbose",
    "--permission-mode", "plan", "--tools", "Read,Grep,Glob",
    "--allowedTools", "Read,Grep,Glob",
]
record = {
    "command": command,
    "cwd": str(PACKET),
    "review_target": HEAD,
    "tree": TREE,
    "inputs_sha256": digest(PACKET / "INPUTS.json"),
    "prompt_sha256": digest(PACKET / "prompt.txt"),
    "source_entries": count,
    "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "wall_seconds": 2400,
    "kill_after_seconds": 10,
    "per_output_file_bytes": 67108864,
}
write_new_json(PACKET / "launch.json", record)


def limits():
    resource.setrlimit(resource.RLIMIT_FSIZE, (67108864, 67108864))


started = time.monotonic()
with (
    (PACKET / "prompt.txt").open("rb") as prompt,
    (PACKET / "output.jsonl").open("xb") as output,
    (PACKET / "stderr.log").open("xb") as stderr,
):
    result = subprocess.run(
        command, cwd=PACKET, stdin=prompt, stdout=output, stderr=stderr,
        preexec_fn=limits,
    )
record.update(
    exit_code=result.returncode,
    elapsed_seconds=time.monotonic() - started,
    completed_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
    inputs_unchanged=digest(PACKET / "INPUTS.json") == record["inputs_sha256"]
    and check_inputs() == count,
)
try:
    assert_source_state()
    record["source_unchanged"] = True
except Exception as error:
    record["source_unchanged"] = False
    record["source_check_error"] = repr(error)
write_new_json(PACKET / "exit.json", record)
print(json.dumps(record))
assert record["inputs_unchanged"] and record["source_unchanged"]
raise SystemExit(result.returncode)
