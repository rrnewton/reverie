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
LINUX_INPUT = Path("/tmp/pr609-v2-linux-sources")
PR_BODY = Path("/tmp/degraded-unresolved-kvm-proc-backing-v9-pr-body.md")
BASE = "b13ad926a34f27bb39a349429a1d08e812d741b4"
HEAD = "33d8ee90c2ae81556e42345b3d05e37f088b9c04"
TREE = "4d9a7b3333dc791439449e3d6b9ee4aefc5572af"
BRANCH = "codex/kvm-syncfs-identity-20260920"
PR_REF = "refs/pull/609/head"
PR_URL = "https://github.com/rrnewton/reverie/pull/609"
CHANGED = [
    "reverie-kvm/src/elf.rs",
    "reverie-kvm/src/executor.rs",
    "reverie-kvm/tests/static_elf.rs",
]
SOURCE_HASHES = {
    "reverie-kvm/src/elf.rs": "3ef73519f6666c3108b746cbb6d21691cee1953b585a77b310cf85ee2567a3a6",
    "reverie-kvm/src/executor.rs": "df5ca5a039ca5656055e86f54f6051c4798548f4dee5e8fcf7794225d5d36b95",
    "reverie-kvm/tests/static_elf.rs": "62eac72d498dbf1afb6fffaa9a18767d0d44fa7b331f8ac3d8fae6e3520d6627",
}
PATCH_HASH = "ca2a95b12e6cdadfb3ac983c67aa21dd6fee6e9b7ac66271a7e7228732e5eb1e"
PR_BODY_HASH = "5bb3c86183d2ce7b3807cc87c6c37372cdce4146dccc66c15e738575192c4efa"
REPORT_HASH = "edbb21dcbdefa3e2c8c60cd310f00147ece0e8afaf53b85b790ffdfa5cd34eaa"
PROOF_MANIFEST_HASH = "65bb95c36e8d6af9389e51fa8af3fe62008121c604d7c5862fb75a365a1f3edc"
DISK_FLOOR = 429496729600
LINUX_URLS = {
    "43b450632676.patch": "https://github.com/torvalds/linux/commit/43b450632676fb60e9faeddff285d9fac94a4f58.patch",
    "v6.3-fs-fcntl.c": "https://raw.githubusercontent.com/torvalds/linux/v6.3/fs/fcntl.c",
    "v6.3-fs-namei.c": "https://raw.githubusercontent.com/torvalds/linux/v6.3/fs/namei.c",
    "v6.3-fs-open.c": "https://raw.githubusercontent.com/torvalds/linux/v6.3/fs/open.c",
    "v6.3-fs-proc-fd.c": "https://raw.githubusercontent.com/torvalds/linux/v6.3/fs/proc/fd.c",
    "v6.3-fs-read_write.c": "https://raw.githubusercontent.com/torvalds/linux/v6.3/fs/read_write.c",
    "v6.3-fs-seq_file.c": "https://raw.githubusercontent.com/torvalds/linux/v6.3/fs/seq_file.c",
    "v7.1-fs-fcntl.c": "https://raw.githubusercontent.com/torvalds/linux/v7.1/fs/fcntl.c",
    "v7.1-fs-namei.c": "https://raw.githubusercontent.com/torvalds/linux/v7.1/fs/namei.c",
    "v7.1-fs-open.c": "https://raw.githubusercontent.com/torvalds/linux/v7.1/fs/open.c",
    "v7.1-fs-proc-fd.c": "https://raw.githubusercontent.com/torvalds/linux/v7.1/fs/proc/fd.c",
    "v7.1-fs-read_write.c": "https://raw.githubusercontent.com/torvalds/linux/v7.1/fs/read_write.c",
    "v7.1-fs-seq_file.c": "https://raw.githubusercontent.com/torvalds/linux/v7.1/fs/seq_file.c",
}
LINUX_HASHES = {
    "43b450632676.patch": "3603300842739aebd53b5fb2e68e3e0053876672163eb48ba736a3d6294d6dd7",
    "v6.3-fs-fcntl.c": "acec724b66f190a7efbb517998dffd65397eb6af8de1a64e75f5279e6ad173b7",
    "v6.3-fs-namei.c": "b50726f5dc1ad79a9b62ea052fbcef35523491f8414f89324cdaa35203a184e9",
    "v6.3-fs-open.c": "263fe30fa92cb8a9692677b975a5af8d029f0b555887a1343af5b8ae299d1e92",
    "v6.3-fs-proc-fd.c": "58f368453635373e7f8e97ca16758bd105f58457586621b15d33df03aec6908e",
    "v6.3-fs-read_write.c": "65eb20119356ca0e0c774fc1e67bf71db0020e989c0cee086bc86f9911d1b6a6",
    "v6.3-fs-seq_file.c": "e7fa2d0553f06d145ac8dee0e63f867a0a505227d764d9b533f15e608741e680",
    "v7.1-fs-fcntl.c": "bd9cb6e3f2b2fa7aa547844eaffd22fd4385282f7f2c2f8c7108591d29c006af",
    "v7.1-fs-namei.c": "8a553b187d40f6c694c46e8d1b5932c5d2ccaaedfd323b7dd48ae9fc6ef865f6",
    "v7.1-fs-open.c": "340e10469b1fc0988a540d2d25fe8e006d4ede3b461be6a16432ec0b0ca6a45f",
    "v7.1-fs-proc-fd.c": "48fb7d0ecc45bce3a911dcbf6e0e189aaeba4650865d3dbd288c0c8e03666c68",
    "v7.1-fs-read_write.c": "58fe9b05f89c830accaf97a4f4b680c48e94f7c59ea22e2c2b6dbeb0b063c59f",
    "v7.1-fs-seq_file.c": "db28fa9bd5006c76035f5ac27d1d888024d31e37f553828c55451b94bcc8ac0d",
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
        ["with-proxy", "git", "ls-remote", "origin", "refs/heads/main", PR_REF],
        REPO,
    ).decode()
    refs = {}
    for line in value.splitlines():
        revision, name = line.split("\t", 1)
        refs[name] = revision
    expected = {"refs/heads/main": BASE, PR_REF: HEAD}
    assert refs == expected, refs
    return refs


def remote_pr():
    value = run_bytes(
        [
            "with-proxy",
            "gh",
            "pr",
            "view",
            "609",
            "--repo",
            "rrnewton/reverie",
            "--json",
            "url,baseRefName,baseRefOid,headRefName,headRefOid,body",
        ],
        REPO,
    )
    data = json.loads(value)
    body = PR_BODY.read_text()
    assert digest_bytes(body.encode()) == PR_BODY_HASH
    assert data["url"] == PR_URL
    assert data["baseRefName"] == "main"
    assert data["baseRefOid"] == BASE
    assert data["headRefName"] == BRANCH
    assert data["headRefOid"] == HEAD
    assert data["body"] == body
    return {
        "url": data["url"],
        "base_ref_name": data["baseRefName"],
        "base_ref_oid": data["baseRefOid"],
        "head_ref_name": data["headRefName"],
        "head_ref_oid": data["headRefOid"],
        "body_sha256": digest_bytes(data["body"].encode()),
        "body_matches_packet": True,
    }


def assert_source_state():
    assert run_bytes(["git", "rev-parse", "HEAD"], REPO).decode().strip() == HEAD
    assert run_bytes(["git", "rev-parse", "HEAD^{tree}"], REPO).decode().strip() == TREE
    assert run_bytes(["git", "branch", "--show-current"], REPO).decode().strip() == BRANCH
    assert run_bytes(["git", "status", "--porcelain", "--untracked-files=no"], REPO) == b""
    actual = run_bytes(["git", "diff", "--name-only", f"{BASE}..{HEAD}"], REPO).decode().splitlines()
    assert actual == CHANGED, actual
    for path, expected in SOURCE_HASHES.items():
        assert digest(REPO / path) == expected, path
        committed = run_bytes(["git", "show", f"{HEAD}:{path}"], REPO)
        assert digest_bytes(committed) == expected, f"committed {path}"
    patch = run_bytes(["git", "diff", "--binary", f"{BASE}..{HEAD}"], REPO)
    assert digest_bytes(patch) == PATCH_HASH
    return {
        "branch": BRANCH,
        "head": HEAD,
        "tree": TREE,
        "remote_refs": remote_refs(),
        "remote_pr": remote_pr(),
        "source_sha256": SOURCE_HASHES,
        "patch_sha256": PATCH_HASH,
    }


def copy_new(source, destination):
    write_new_bytes(destination, source.read_bytes())


def validate_v9_evidence():
    evidence = REPO / "ignored/kvm-syncfs-identity-v9"
    assert digest(evidence / "REPORT.md") == REPORT_HASH
    manifest = evidence / "proof-artifacts.sha256"
    assert digest(manifest) == PROOF_MANIFEST_HASH
    pointer = (evidence / "proof-artifacts-manifest.sha256").read_text()
    assert pointer == (
        PROOF_MANIFEST_HASH
        + "  ignored/kvm-syncfs-identity-v9/proof-artifacts.sha256\n"
    )
    records = []
    for line in manifest.read_text().splitlines():
        expected, relative = line.split("  ", 1)
        assert relative.startswith("ignored/kvm-syncfs-identity-v9/")
        source = REPO / relative
        assert source.is_file(), relative
        assert digest(source) == expected, relative
        records.append(relative)
    assert len(records) == 78, len(records)
    return evidence, records


def materialize_inputs(source_state):
    initial = {path.name for path in PACKET.iterdir()}
    assert initial == {"launch-review.py", "prompt.txt"}, initial
    assert shutil.disk_usage(REPO).free >= DISK_FLOOR
    evidence, proof_records = validate_v9_evidence()
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
    for relative in (
        "Cargo.toml",
        "reverie-kvm/Cargo.toml",
        "reverie-kvm/README.md",
        "reverie-kvm/RELATED_WORK.md",
    ):
        copy_new(REPO / relative, PACKET / "candidate" / relative)
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
    copy_new(DEV / "ai_docs/hermit-v2-roadmap.md", PACKET / "primary/hermit-v2-roadmap.md")
    copy_new(REPO / "BACKENDS.md", PACKET / "primary/REVERIE-BACKENDS.md")
    copy_new(REPO / "reverie-kvm/README.md", PACKET / "primary/reverie-kvm-README.md")
    copy_new(REPO / "reverie-kvm/RELATED_WORK.md", PACKET / "primary/reverie-kvm-RELATED-WORK.md")
    copy_new(REPO / "docs/BACKEND_SOURCES.md", PACKET / "primary/REVERIE-BACKEND-SOURCES.md")
    write_new_json(PACKET / "primary/REMOTE-PR.json", source_state["remote_pr"])
    refs_text = "".join(
        f"{revision}\t{name}\n"
        for name, revision in sorted(source_state["remote_refs"].items())
    )
    write_new_bytes(PACKET / "primary/REMOTE-REFS.txt", refs_text.encode())
    copy_new(
        DEV / ".llms/skills/code-review/SKILL.md",
        PACKET / "context/code-review-SKILL.md",
    )
    shutil.copytree(
        evidence,
        PACKET / "qualification/v9",
        copy_function=lambda source, destination: shutil.copyfile(source, destination),
    )
    for name, expected in LINUX_HASHES.items():
        source = LINUX_INPUT / name
        assert digest(source) == expected, name
        copy_new(source, PACKET / "linux" / name)
    write_new_json(
        PACKET / "linux/SOURCES.json",
        {
            "sha256": LINUX_HASHES,
            "source_urls": LINUX_URLS,
            "note": "Pinned tag files plus the pre-6.4 create-directory ordering commit patch.",
        },
    )
    source = {
        "repo": str(REPO),
        "pull_request": PR_URL,
        "base": BASE,
        "head": HEAD,
        "tree": TREE,
        "branch": BRANCH,
        "changed_paths": CHANGED,
        "source_sha256": SOURCE_HASHES,
        "patch_sha256": digest(PACKET / "BASE-TO-HEAD.patch"),
        "pr_body_sha256": digest(PACKET / "primary/PR-BODY.md"),
        "qualification_report_sha256": digest(PACKET / "qualification/v9/REPORT.md"),
        "qualification_proof_manifest_sha256": digest(
            PACKET / "qualification/v9/proof-artifacts.sha256"
        ),
        "qualification_proof_records": len(proof_records),
        "remote_refs": source_state["remote_refs"],
        "remote_pr": source_state["remote_pr"],
        "linux_source_sha256": LINUX_HASHES,
        "created_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    }
    write_new_json(PACKET / "SOURCE.json", source)
    roots = [
        PACKET / "launch-review.py",
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


def extract_final_response(path):
    results = []
    for line in path.read_text(errors="replace").splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "result" and isinstance(event.get("result"), str):
            results.append(event["result"])
    if not results:
        raise ValueError("stream-json output contains no result event")
    response = results[-1]
    final_line = next((line.strip() for line in reversed(response.splitlines()) if line.strip()), "")
    if final_line not in {"APPROVE", "CHANGES REQUESTED"}:
        raise ValueError(f"invalid final verdict line: {final_line!r}")
    return response, final_line


source_before = assert_source_state()
disk_before_materialization = shutil.disk_usage(REPO).free
materialize_inputs(source_before)
count, input_bytes = check_inputs()
disk_before_launch = shutil.disk_usage(REPO).free
assert disk_before_launch >= DISK_FLOOR, disk_before_launch
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
    "disk_floor_bytes": DISK_FLOOR,
    "disk_before_materialization_bytes": disk_before_materialization,
    "disk_before_launch_bytes": disk_before_launch,
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
    disk_after_review_bytes=shutil.disk_usage(REPO).free,
)
try:
    response, verdict = extract_final_response(PACKET / "output.jsonl")
    write_new_bytes(PACKET / "final-response.txt", response.encode())
    record["review_verdict"] = verdict
    record["final_response_bytes"] = (PACKET / "final-response.txt").stat().st_size
    record["final_response_sha256"] = digest(PACKET / "final-response.txt")
except Exception as error:
    record["review_verdict"] = None
    record["result_parse_error"] = repr(error)
try:
    source_after = assert_source_state()
    record["source_unchanged"] = source_after == source_before
    record["source_after"] = source_after
except Exception as error:
    record["source_unchanged"] = False
    record["source_check_error"] = repr(error)
write_new_json(PACKET / "exit.json", record)
print(json.dumps(record, sort_keys=True))
if result.returncode != 0:
    raise SystemExit(result.returncode)
if not record["inputs_unchanged"] or not record["launcher_unchanged"] or not record["source_unchanged"]:
    raise SystemExit(os.EX_IOERR)
if record["review_verdict"] is None:
    raise SystemExit(os.EX_DATAERR)

