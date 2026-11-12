#!/usr/bin/env python3
"""Read-only source/SCM preparation; writes evidence only beside this script."""
import difflib
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess

D = Path(__file__).resolve().parent
R = D.parent.parent
F = R / "ignored/publication-fd-composition-qualification-v1/final-v1"
BASE = "000c15a1161ea2d58749431b5ddaaa97f7aa37d5"
PATHS = ["reverie-kvm/src/elf.rs", "reverie-kvm/src/executor.rs", "reverie-kvm/src/process_signal_publication.rs"]
ENV = dict(os.environ, GIT_OPTIONAL_LOCKS="0")
commands = []

def git(*args):
    proc = subprocess.run(["git", *args], cwd=R, env=ENV, capture_output=True, timeout=30)
    commands.append({"argv": ["git", *args], "returncode": proc.returncode,
                     "stdout_sha256": hashlib.sha256(proc.stdout).hexdigest(),
                     "stderr": proc.stderr.decode()})
    assert proc.returncode == 0, commands[-1]
    return proc.stdout

def digest(kind, data):
    return hashlib.sha1(kind.encode() + b" " + str(len(data)).encode() + b"\0" + data).hexdigest()

def record(path):
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "mode": stat.S_IMODE(path.stat().st_mode),
            "sha256": hashlib.sha256(data).hexdigest()}

def write(name, value):
    with (D / name).open("x") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")

head = git("rev-parse", "HEAD").decode().strip()
branch = git("branch", "--show-current").decode().strip()
index = Path(git("rev-parse", "--git-path", "index").decode().strip())
index_before = record(index)
assert head == BASE
assert branch == "codex/kvm-setitimer-20260918"
assert index_before["sha256"] == "979da5a207fb109ec448c915867c7681525a9783b8a11ba8a71037b627592683"
assert not git("diff", "--cached", "--name-only", BASE)
states = {}
for name in ["rebase-merge", "rebase-apply", "MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"]:
    path = Path(git("rev-parse", "--git-path", name).decode().strip())
    states[name] = {"path": str(path), "exists": path.exists()}
    assert not path.exists()
tracked_changes = git("diff", "--name-only", BASE).decode().splitlines()
assert tracked_changes == PATHS[:2]
status = git("status", "--porcelain=v1", "--untracked-files=normal").decode()
with (D / "STATUS.txt").open("x") as stream:
    stream.write(status)

base = {}
for row in git("ls-tree", "-r", "-z", BASE).split(b"\0"):
    if not row:
        continue
    header, name = row.split(b"\t", 1)
    mode, kind, obj = header.decode().split()
    base[name.decode()] = {"mode": mode, "kind": kind, "object": obj}
assert record(F / "TARGET.json")["sha256"] == "004482610ed2c1f5aa032669d9adc75c878003a90258b218d5eb479120c5a953"
assert record(F / "FULL-SOURCE-MANIFEST.json")["sha256"] == "a3bcb28bcfcb154875a56265c8a40c6f8e29067e94418bfcd2338e28cd29f6a3"
manifest = json.loads((F / "FULL-SOURCE-MANIFEST.json").read_text())
candidate = {}
for item in manifest:
    name = item["relative"]
    if item["kind"] == "unexpanded_gitlink":
        candidate[name] = {"mode": item["mode"], "kind": "commit", "object": item["publisher_git_object"]}
    else:
        path = F / "source" / name
        data = os.readlink(path).encode() if path.is_symlink() else path.read_bytes()
        assert hashlib.sha256(data).hexdigest() == item["sha256"], name
        if not path.is_symlink():
            assert stat.S_IMODE(path.stat().st_mode) == item["file_mode"], name
        candidate[name] = {"mode": item["mode"], "kind": "blob", "object": digest("blob", data)}
changes = sorted(k for k in set(base) | set(candidate) if base.get(k) != candidate.get(k))
assert changes == PATHS

def tree_id(entries):
    root = {}
    for path, value in entries.items():
        parts = path.split("/")
        parent = root
        for part in parts[:-1]:
            parent = parent.setdefault(part, {})
        parent[parts[-1]] = (value["mode"], value["object"])
    def encode(node):
        data = b""
        for name, value in sorted(node.items(), key=lambda pair: (pair[0] + ("/" if isinstance(pair[1], dict) else "")).encode()):
            mode, obj = ("40000", encode(value)) if isinstance(value, dict) else value
            data += mode.encode() + b" " + name.encode() + b"\0" + bytes.fromhex(obj)
        return digest("tree", data)
    return encode(root)

base_tree = tree_id(base)
assert base_tree == git("rev-parse", BASE + "^{tree}").decode().strip()
canonical = ""
paths = []
for name in PATHS:
    before = git("show", BASE + ":" + name) if name in base else b""
    after = (F / "source" / name).read_bytes()
    canonical += "".join(difflib.unified_diff(before.decode().splitlines(keepends=True), after.decode().splitlines(keepends=True),
                                            fromfile="a/" + name if name in base else "/dev/null", tofile="b/" + name))
    attributes = git("check-attr", "--all", "--", name).decode()
    assert not attributes
    paths.append({"relative": name, "status": "tracked modified" if name in base else "untracked product",
                  "qualified": record(F / "source" / name), "qualified_git": candidate[name],
                  "live": record(R / name), "base_git": base.get(name), "attributes": attributes})
assert canonical.encode() == (F / "SOURCE.patch").read_bytes()
assert record(index) == index_before
write("SOURCE-BINDING.json", {
    "schema": 1, "repository": str(R), "branch": branch, "base": BASE, "base_tree": base_tree,
    "expected_commit_tree": tree_id(candidate), "qualified_target": record(F / "TARGET.json"),
    "qualified_manifest": record(F / "FULL-SOURCE-MANIFEST.json"), "source_patch": record(F / "SOURCE.patch"),
    "canonical_patch_reconstructed_from_base_and_after_bytes": True,
    "base_leaf_count": len(base), "qualified_leaf_count": len(candidate), "exact_changed_paths": changes,
    "paths": paths, "index_unchanged": index_before, "in_progress_git_states": states,
    "no_active_local_hooks": not (R / ".githooks").exists(),
    "root_instruction": "Planning only; source reviews and SCM approval remain with root.",
})
write("EXPECTED-TREE.json", {"tree": tree_id(candidate), "entries": candidate})
write("COMMANDS.json", commands)
print(json.dumps({"expected_commit_tree": tree_id(candidate), "source_binding": record(D / "SOURCE-BINDING.json")}, indent=2))
