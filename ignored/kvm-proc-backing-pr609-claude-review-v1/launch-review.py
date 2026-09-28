from pathlib import Path
import datetime
import hashlib
import json
import os
import resource
import shutil
import subprocess
import time


PACKET = Path(__file__).resolve().parent
REPO = PACKET.parents[1]
DEV = Path("/home/newton/work/dev-hermit")
LINUX_INPUT = Path("/tmp/pr609-linux-sources")
PR_BODY = Path("/tmp/degraded-unresolved-kvm-proc-backing-v8-pr-body.md")
BASE = "b13ad926a34f27bb39a349429a1d08e812d741b4"
HEAD = "96d0b5848f3a61bc7c4c1806bd8e64d25c5f7bb7"
TREE = "e5c35ee9e04aa5935d27c2ac82b918c2c976ad9b"
BRANCH = "codex/kvm-syncfs-identity-20260920"
PR_REF = "refs/pull/609/head"
CHANGED = [
    "reverie-kvm/src/elf.rs",
    "reverie-kvm/src/executor.rs",
    "reverie-kvm/tests/static_elf.rs",
]
SOURCE_HASHES = {
    "reverie-kvm/src/elf.rs": "3ef73519f6666c3108b746cbb6d21691cee1953b585a77b310cf85ee2567a3a6",
    "reverie-kvm/src/executor.rs": "74b0e3732462aa83ef926dcd7e44a52802a63df1303bd62722e7fd7bb72df6a2",
    "reverie-kvm/tests/static_elf.rs": "b51557a4ce47c007d58a1257b787c2ff8f60bba6bc9d49d98e5860b775d5ea9f",
}
PATCH_HASH = "356462ca55635b56f572697db8d5e935a6915debb147350b7f587f4d148be0fb"
REPORT_HASH = "f93838b2be64850e82bd24b3ec69536e4e57600e88d5e1cd61ddf39e269457f7"
LINUX_HASHES = {
    "43b450632676.patch": "3603300842739aebd53b5fb2e68e3e0053876672163eb48ba736a3d6294d6dd7",
    "v6.3-fs-namei.c": "b50726f5dc1ad79a9b62ea052fbcef35523491f8414f89324cdaa35203a184e9",
    "v6.3-fs-open.c": "263fe30fa92cb8a9692677b975a5af8d029f0b555887a1343af5b8ae299d1e92",
    "v7.1-fs-fcntl.c": "bd9cb6e3f2b2fa7aa547844eaffd22fd4385282f7f2c2f8c7108591d29c006af",
    "v7.1-fs-namei.c": "8a553b187d40f6c694c46e8d1b5932c5d2ccaaedfd323b7dd48ae9fc6ef865f6",
    "v7.1-fs-open.c": "340e10469b1fc0988a540d2d25fe8e006d4ede3b461be6a16432ec0b0ca6a45f",
    "v7.1-fs-proc-fd.c": "48fb7d0ecc45bce3a911dcbf6e0e189aaeba4650865d3dbd288c0c8e03666c68",
}


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


def remote_refs():
    value = run_bytes(
        [
            "with-proxy",
            "git",
            "ls-remote",
            "origin",
            "refs/heads/main",
            PR_REF,
        ],
        REPO,
    ).decode()
    refs = {}
    for line in value.splitlines():
        revision, name = line.split("\t", 1)
        refs[name] = revision
    assert refs == {"refs/heads/main": BASE, PR_REF: HEAD}, refs
    return refs, value


def assert_source_state():
    assert run_bytes(["git", "rev-parse", "HEAD"], REPO).decode().strip() == HEAD
    assert run_bytes(["git", "rev-parse", "HEAD^{tree}"], REPO).decode().strip() == TREE
    assert run_bytes(["git", "branch", "--show-current"], REPO).decode().strip() == BRANCH
    assert run_bytes(
        ["git", "status", "--porcelain", "--untracked-files=no"], REPO
    ) == b""
    actual = run_bytes(
        ["git", "diff", "--name-only", f"{BASE}..{HEAD}"], REPO
    ).decode().splitlines()
    assert actual == CHANGED, actual
    for path, expected in SOURCE_HASHES.items():
        assert digest(REPO / path) == expected, path
    patch = run_bytes(["git", "diff", "--binary", f"{BASE}..{HEAD}"], REPO)
    assert digest_bytes(patch) == PATCH_HASH
    refs, raw_refs = remote_refs()
    return {
        "branch": BRANCH,
        "head": HEAD,
        "tree": TREE,
        "remote_refs": refs,
        "remote_refs_raw": raw_refs,
        "source_sha256": SOURCE_HASHES,
        "patch_sha256": PATCH_HASH,
    }


def copy_new(source, destination):
    value = source.read_bytes()
    write_new_bytes(destination, value)


def materialize_inputs(source_state):
    write_new_bytes(
        PACKET / "BASE-TO-HEAD.patch",
        run_bytes(["git", "diff", "--binary", f"{BASE}..{HEAD}"], REPO),
    )
    for path in CHANGED:
        write_new_bytes(
            PACKET / "base" / path,
            run_bytes(["git", "show", f"{BASE}:{path}"], REPO),
        )
    for source_root in (REPO / "reverie-kvm/src", REPO / "reverie-kvm/tests"):
        relative = source_root.relative_to(REPO)
        shutil.copytree(
            source_root,
            PACKET / "candidate" / relative,
            copy_function=lambda source, destination: shutil.copyfile(source, destination),
        )
    copy_new(
        REPO / "reverie-kvm/Cargo.toml",
        PACKET / "candidate/reverie-kvm/Cargo.toml",
    )
    copy_new(REPO / "Cargo.toml", PACKET / "candidate/Cargo.toml")
    write_new_bytes(
        PACKET / "primary/COMMIT.txt",
        run_bytes(
            ["git", "show", "-s", "--format=fuller", "--no-show-signature", HEAD],
            REPO,
        ),
    )
    write_new_bytes(
        PACKET / "primary/HEAD-TREE.txt",
        run_bytes(["git", "ls-tree", "-r", "--full-tree", HEAD], REPO),
    )
    copy_new(PR_BODY, PACKET / "primary/PR-BODY.md")
    copy_new(DEV / "PROJECT_VISION.md", PACKET / "primary/PROJECT_VISION.md")
    copy_new(
        DEV / "ai_docs/hermit-v2-roadmap.md",
        PACKET / "primary/hermit-v2-roadmap.md",
    )
    copy_new(REPO / "BACKENDS.md", PACKET / "primary/REVERIE-BACKENDS.md")
    copy_new(REPO / "reverie-kvm/README.md", PACKET / "primary/reverie-kvm-README.md")
    copy_new(
        REPO / "docs/BACKEND_SOURCES.md",
        PACKET / "primary/REVERIE-BACKEND-SOURCES.md",
    )
    copy_new(
        DEV / ".skills/code-review/SKILL.md",
        PACKET / "context/code-review-SKILL.md",
    )
    evidence = REPO / "ignored/kvm-syncfs-identity-v8"
    assert digest(evidence / "REPORT.md") == REPORT_HASH
    shutil.copytree(
        evidence,
        PACKET / "qualification/v8",
        copy_function=lambda source, destination: shutil.copyfile(source, destination),
    )
    for name, expected in LINUX_HASHES.items():
        source = LINUX_INPUT / name
        assert digest(source) == expected, name
        copy_new(source, PACKET / "linux" / name)
    write_new_bytes(PACKET / "primary/REMOTE-REFS.txt", source_state["remote_refs_raw"].encode())
    source = {
        "repo": str(REPO),
        "pull_request": "https://github.com/rrnewton/reverie/pull/609",
        "base": BASE,
        "head": HEAD,
        "tree": TREE,
        "branch": BRANCH,
        "changed_paths": CHANGED,
        "source_sha256": SOURCE_HASHES,
        "patch_sha256": digest(PACKET / "BASE-TO-HEAD.patch"),
        "pr_body_sha256": digest(PACKET / "primary/PR-BODY.md"),
        "qualification_report_sha256": digest(PACKET / "qualification/v8/REPORT.md"),
        "remote_refs": source_state["remote_refs"],
        "linux_source_sha256": LINUX_HASHES,
        "created_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    }
    write_new_json(PACKET / "SOURCE.json", source)
    records = []
    roots = [
        PACKET / "prompt.txt",
        PACKET / "BASE-TO-HEAD.patch",
        PACKET / "SOURCE.json",
        PACKET / "base",
        PACKET / "candidate",
        PACKET / "primary",
        PACKET / "context",
        PACKET / "qualification",
        PACKET / "linux",
    ]
    paths = []
    for root in roots:
        if root.is_file():
            paths.append(root)
        else:
            paths.extend(path for path in root.rglob("*") if path.is_file())
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
    for path in paths:
        path.chmod(0o444)
    (PACKET / "INPUTS.json").chmod(0o444)
    for root in roots:
        if root.is_dir():
            for directory in sorted(
                (path for path in root.rglob("*") if path.is_dir()), reverse=True
            ):
                directory.chmod(0o555)
            root.chmod(0o555)


def check_inputs():
    data = json.loads((PACKET / "INPUTS.json").read_text())
    for record in data["records"]:
        path = PACKET / record["path"]
        assert path.stat().st_size == record["bytes"], record["path"]
        assert digest(path) == record["sha256"], record["path"]
    return len(data["records"]), sum(record["bytes"] for record in data["records"])


source_before = assert_source_state()
materialize_inputs(source_before)
count, input_bytes = check_inputs()
command = [
    "timeout",
    "--signal=TERM",
    "--kill-after=10s",
    "2400s",
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
    "review_target": HEAD,
    "tree": TREE,
    "inputs_sha256": digest(PACKET / "INPUTS.json"),
    "prompt_sha256": digest(PACKET / "prompt.txt"),
    "launcher_sha256": digest(Path(__file__).resolve()),
    "source_entries": count,
    "source_input_bytes": input_bytes,
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
    output_bytes=(PACKET / "output.jsonl").stat().st_size,
    output_sha256=digest(PACKET / "output.jsonl"),
    stderr_bytes=(PACKET / "stderr.log").stat().st_size,
    stderr_sha256=digest(PACKET / "stderr.log"),
    inputs_unchanged=digest(PACKET / "INPUTS.json") == record["inputs_sha256"]
    and check_inputs() == (count, input_bytes),
    launcher_unchanged=digest(Path(__file__).resolve()) == record["launcher_sha256"],
)
try:
    source_after = assert_source_state()
    record["source_unchanged"] = source_after == source_before
    record["source_after"] = source_after
except Exception as error:
    record["source_unchanged"] = False
    record["source_check_error"] = repr(error)
write_new_json(PACKET / "exit.json", record)
print(json.dumps(record))
assert record["inputs_unchanged"]
assert record["launcher_unchanged"]
assert record["source_unchanged"]
raise SystemExit(result.returncode)
