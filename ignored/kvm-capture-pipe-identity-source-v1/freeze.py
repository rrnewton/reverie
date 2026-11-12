"""Freeze authored source/evidence only. Never invoke Cargo, Git, or a guest."""
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys

D = Path(__file__).resolve().parent
V6 = D.parent / "rdtsc-recovery-source-v6"
V3 = D.parent / "rdtsc-recovery-source-v3"
S = Path("/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916")
W = S / "ignored/kvm-stdout-pipe-device-witness-v1"
NEW = {
    "reverie-kvm/src/capture_identity.rs",
    "reverie-kvm/src/capture_identity_tests.rs",
    "reverie-kvm/tests/support/capture_identity.rs",
}
CHANGED = NEW | {
    "reverie-kvm/src/executor.rs",
    "reverie-kvm/src/runtime.rs",
    "reverie-kvm/src/vm.rs",
    "reverie-kvm/tests/static_elf.rs",
}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write_json(name, data):
    path = D / name
    require(not path.exists(), f"refuse overwriting frozen artifact {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n")
    return path


def record(path):
    path = Path(path)
    info = path.lstat()
    row = {"path": str(path), "file_mode": stat.S_IMODE(info.st_mode)}
    if stat.S_ISREG(info.st_mode):
        data = path.read_bytes()
        row.update(kind="file", bytes=len(data), sha256=sha(data))
    elif stat.S_ISLNK(info.st_mode):
        target = os.readlink(path)
        row.update(kind="symlink", target=target, sha256=sha(os.fsencode(target)))
    elif stat.S_ISDIR(info.st_mode):
        require(not any(path.iterdir()), f"expected empty unexpanded gitlink: {path}")
        row.update(kind="unexpanded_gitlink")
    else:
        raise RuntimeError(f"unexpected input kind: {path}")
    return row


def validate(expected):
    actual = record(expected["path"])
    for key in ("kind", "bytes", "sha256", "target", "file_mode"):
        if key in expected:
            require(actual.get(key) == expected[key], f"input changed: {expected['path']} {key}")
    return actual


def verify_patch(old, new, patch):
    """Reconstruct every authored file from all unified hunks in memory."""
    lines = patch.splitlines(keepends=True)
    old_lines = old.splitlines(keepends=True)
    result = []
    cursor = 0
    i = 2  # --- / +++ headers
    while i < len(lines):
        match = re.fullmatch(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@\n", lines[i])
        require(match is not None, "invalid unified hunk")
        start = int(match[1])
        before_count = int(match[2] or "1")
        after_count = int(match[4] or "1")
        start = start - 1 if before_count else start
        require(start >= cursor, "overlapping hunk")
        result.extend(old_lines[cursor:start])
        cursor = start
        consumed = produced = 0
        i += 1
        while i < len(lines) and not lines[i].startswith("@@ "):
            line = lines[i]
            require(line[:1] in (" ", "-", "+"), "invalid hunk line")
            if line[:1] in (" ", "-"):
                require(cursor < len(old_lines) and old_lines[cursor] == line[1:], "hunk old content mismatch")
                cursor += 1
                consumed += 1
            if line[:1] in (" ", "+"):
                result.append(line[1:])
                produced += 1
            i += 1
        require(consumed == before_count and produced == after_count, "hunk count mismatch")
    result.extend(old_lines[cursor:])
    require("".join(result) == new, "complete patch reconstruction mismatch")


def main():
    require(not (D / "TARGET.json").exists(), "target already frozen")
    base_manifest = json.loads((V6 / "SOURCE-MANIFEST.json").read_text())
    base_by_name = {r["relative"]: r for r in base_manifest}
    require(len(base_by_name) == 2623, "unexpected V6 manifest denominator")
    copied = json.loads((D / "BASE-COPY.json").read_text())
    require(copied["manifest_sha256"] == sha((V6 / "SOURCE-MANIFEST.json").read_bytes()), "base copy manifest drift")
    external = {}
    current = []
    actual_changes = set()
    for old in base_manifest:
        checked = validate(old)
        external[checked["path"]] = checked
        now = record(D / "source" / old["relative"])
        now["relative"] = old["relative"]
        now["mode"] = old["mode"]
        if old["kind"] == "file":
            require(os.stat(old["path"]).st_ino != os.stat(now["path"]).st_ino, "base/source hardlink")
        for key in ("kind", "file_mode", "target"):
            if key in old:
                require(now.get(key) == old[key], f"unexpected mode/kind/alias change {old['relative']}")
        if now.get("sha256") != old.get("sha256"):
            actual_changes.add(old["relative"])
        current.append(now)
    for relative in sorted(NEW):
        require(relative not in base_by_name, "new path already existed")
        row = record(D / "source" / relative)
        require(row["kind"] == "file" and row["file_mode"] == 0o644, "new source kind/mode")
        row.update(relative=relative, mode="100644")
        current.append(row)
        actual_changes.add(relative)
    require(actual_changes == CHANGED, f"unexpected source diff: {actual_changes}")
    actual_leaves = set()
    for root, dirs, files in os.walk(D / "source", followlinks=False):
        for name in list(dirs):
            path = Path(root) / name
            if path.is_symlink():
                actual_leaves.add(str(path.relative_to(D / "source")))
                dirs.remove(name)
        for name in files:
            actual_leaves.add(str((Path(root) / name).relative_to(D / "source")))
    expected_leaves = {r["relative"] for r in current if r["kind"] != "unexpanded_gitlink"}
    require(actual_leaves == expected_leaves, "unindexed source path")
    current.sort(key=lambda row: row["relative"])
    write_json("SOURCE-MANIFEST.json", current)

    full_patch = []
    transformations = []
    for relative in sorted(CHANGED):
        before_path = V6 / "source" / relative
        old = before_path.read_text() if relative not in NEW else ""
        after_path = D / "source" / relative
        new = after_path.read_text()
        require((not old or old.endswith("\n")) and new.endswith("\n"), "unexpected no-newline source")
        raw = "".join(difflib.unified_diff(old.splitlines(keepends=True), new.splitlines(keepends=True), fromfile="/dev/null" if relative in NEW else "a/"+relative, tofile="b/"+relative))
        verify_patch(old, new, raw)
        full_patch.append(f"diff --git a/{relative} b/{relative}\n")
        if relative in NEW:
            full_patch.append("new file mode 100644\n")
        full_patch.append(raw)
        before = None
        if relative not in NEW:
            before = D / "before" / relative
            before.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(before_path, before)
        after = D / "after" / relative
        after.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(after_path, after)
        transformations.append({"relative":relative,"before":record(before) if before else None,"after":record(after),"source":record(after_path),"all_hunks_reconstructed":True})
    (D / "SOURCE.patch").write_text("".join(full_patch))
    write_json("CHANGES.json", transformations)
    write_json("PREPARATION-HISTORY.json", {"earlier_draft":record(D/"candidate.patch"),"status":"Retained earlier, incomplete authoring diff; only SOURCE.patch is the final candidate", "authoring_format":[record(D/name) for name in ["SOURCE-FORMAT.json","SOURCE-FORMAT-2.json","SOURCE-FORMAT-FINAL.json"]]})

    # Authenticate the full V3 source and compare every manifest entry, not only names.
    v3_manifest = json.loads((V3 / "SOURCE-MANIFEST.json").read_text())
    v3_by_name = {r["relative"]: r for r in v3_manifest}
    for row in v3_manifest:
        checked = validate(row)
        external[checked["path"]] = checked
    differences = []
    for relative in sorted(set(v3_by_name) | set(base_by_name)):
        left = v3_by_name.get(relative)
        right = base_by_name.get(relative)
        identity = lambda r: None if r is None else tuple(r.get(k) for k in ("kind","sha256","mode","target"))
        if identity(left) != identity(right):
            differences.append(relative)
    declared = json.loads((D / "V3-TO-V6.json").read_text())
    require(differences == [r["relative"] for r in declared], "V3/V6 changed-path audit mismatch")
    require(len(differences)==5 and len((D/"V3-TO-V6.patch").read_text().splitlines())==814, "unexpected V3/V6 delta")
    require(v3_by_name["reverie-kvm/src/executor.rs"]["sha256"] == base_by_name["reverie-kvm/src/executor.rs"]["sha256"], "executor not unchanged V3/V6")
    write_json("BASE-CONTINUITY.json", {"v3_manifest":record(V3/"SOURCE-MANIFEST.json"),"v6_manifest":record(V6/"SOURCE-MANIFEST.json"),"v3_entries_authenticated":len(v3_manifest),"v6_entries_authenticated":len(base_manifest),"all_five_changes":differences,"full_delta":record(D/"V3-TO-V6.patch"),"executor_unchanged_between_v3_v6":True,"candidate_changed_paths":sorted(CHANGED),"all_other_v6_entries_unchanged":len(base_manifest)-len(CHANGED-NEW)})

    # Source declarations are a preparation check, never a replacement for harness listing.
    selectors = json.loads((D / "SELECTORS.json").read_text())
    v6_selectors = json.loads((V6 / "qualification-v1/SELECTORS.json").read_text())
    old_names = {(g["artifact"], name) for g in v6_selectors["groups"].values() for name in g["names"]}
    selected = []
    names = set()
    for group, details in selectors["groups"].items():
        for name in details["names"]:
            key = (details["artifact"], name)
            require(key not in names, "duplicate selected declaration")
            names.add(key)
            relative = details["source"]
            source = (D/"source"/relative).read_text()
            matches = list(re.finditer(r"\bfn\s+"+re.escape(name.split("::")[-1])+r"\s*\(", source))
            require(len(matches)==1, f"source declaration count: {name}: {len(matches)}")
            line = source[:matches[0].start()].count("\n")+1
            base = (V6/"source"/relative).read_text() if relative not in NEW else None
            selected.append({"group":group,"artifact":details["artifact"],"name":name,"relative":relative,"line":line,"retained_v6_selector":key in old_names,"new_file":relative in NEW,"source_file_unchanged":base==source})
    require(len(names)==63 and len(old_names)==37 and old_names <= names, "selected denominator or V6 preservation")
    write_json("SELECTION-PREPARATION.json", {"status":"static source-name check only; no harness enumeration or execution", "unique_declarations":63,"retained_v6_declarations":37,"new_declarations":6,"existing_neighbors":20,"records":selected})

    # Explicit immutable history, not the predecessor's mutable build target.
    paths = [
        V6/"TARGET.json",V6/"SOURCE-MANIFEST.json",V6/"SOURCE.patch",V6/"final-v1/TARGET.json",V6/"final-v1/REPORT.md",V6/"final-v1/READBACK.json",V6/"final-v1/LEASE-RELEASE.json",
        V6/"qualification-result-v1/RESULTS.json",V6/"qualification-result-v1/RAW-EXECUTION-AUDIT.json",V6/"qualification-result-v1/READBACK.json",
        V6/"qualification-v1/retained-binaries/BINDING.json",V6/"qualification-v1/retained-binaries/ARTIFACTS.json",
        V6/"qualification-v1/SELECTORS.json",V6/"qualification-v1/observer/observer.py",V6/"qualification-v1/common.py",V6/"qualification-v1/phase.py",V6/"qualification-v1/cache_lease.py",
        V3/"TARGET.json",V3/"SOURCE-MANIFEST.json",
    ]
    paths += [W/"final-v1"/name for name in ["REPORT.md","RESULTS.json","INPUTS.json","READBACK.json"]]
    paths += [W/"correction-proposal-v1"/name for name in ["PROPOSAL.md","TARGET.json","INPUTS.json","READBACK.json"]]
    reference = json.loads((D/"CALLER-REFERENCE.json").read_text())
    paths += [Path(row["path"]) for row in reference["references"]]
    for path in paths:
        row = record(path)
        external[str(path)] = row
    write_json("HISTORY-REFERENCES.json", {"status":"Bound retained history; prior results are not new-candidate results. Runtime ELFs and entire historical transitive receipt closure are referenced through their immutable indexes, not requalified by this author freeze.","records":[external[str(path)] for path in paths]})

    # Held policies keep later review dispatch independent of mutable parent paths.
    policies = [
        Path("/home/newton/work/dev-hermit/AGENTS.md"),
        Path("/home/newton/work/dev-hermit/.skills/code-review/SKILL.md"),
        Path("/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/.claude/skills/post-facto-review/SKILL.md"),
    ]
    policy_records = []
    for number, original in enumerate(policies,1):
        destination = D/"instructions"/(f"{number:02d}-"+original.name)
        destination.parent.mkdir(parents=True,exist_ok=True)
        destination.write_bytes(original.read_bytes())
        policy_records.append({"origin":str(original),"held":record(destination),"original_resolved_path":str(original.resolve())})
    write_json("POLICIES.json", policy_records)

    packet = []
    for root, dirs, files in os.walk(D,followlinks=False):
        if root == str(D):
            dirs[:] = [name for name in dirs if name != "source"]
        for name in files:
            path = Path(root)/name
            packet.append(record(path))
    all_inputs = {row["path"]:row for row in [*external.values(),*current,*packet]}
    inputs = write_json("INPUTS.json", {"scope":"Complete current and V6/V3 source plus own preparation and explicitly listed immutable history. No mutable build-target/lease input, and no independent/runtime verdict.","records":sorted(all_inputs.values(),key=lambda row:row["path"])})
    target = write_json("TARGET.json", {
        "status":"SOURCE-ONLY CANDIDATE; UNCOMPILED, UNEXECUTED, NO INDEPENDENT APPROVAL",
        "source":str(D/"source"),"base_source":str(V6/"source"),
        "base_target":record(V6/"TARGET.json"),"base_final":record(V6/"final-v1/TARGET.json"),
        "source_manifest":record(D/"SOURCE-MANIFEST.json"),"source_entries":len(current),"changed_paths":7,
        "source_patch":record(D/"SOURCE.patch"),"changes":record(D/"CHANGES.json"),
        "report":record(D/"REPORT.md"),"ownership":record(D/"OWNERSHIP.md"),"qualification_plan":record(D/"QUALIFICATION-PLAN.md"),
        "selectors":record(D/"SELECTORS.json"),"proposed_declarations":63,
        "history":record(D/"HISTORY-REFERENCES.json"),"inputs":record(inputs),
        "base_continuity":record(D/"BASE-CONTINUITY.json"),"source_closure":record(D/"SOURCE-CLOSURE.json"),
        "no_product_scm_build_guest_cache_lease_or_reviewer_action":True,
        "no_previous_runtime_qualification_transferred":True,
    })
    for row in all_inputs.values():
        validate(row)
    readback = write_json("READBACK.json", {
        "target":record(target),"inputs":record(inputs),"source_patch":record(D/"SOURCE.patch"),"report":record(D/"REPORT.md"),
        "runtime":{"python":sys.executable,"argv":sys.argv,"optimization":sys.flags.optimize,"debug":__debug__},
        "checks_use_explicit_exceptions_not_python_assert":True,"input_records":len(all_inputs),
        "all_inputs_bytes_modes_aliases_unchanged":True,"source_entries":len(current),"source_changed_paths":7,
        "all_seven_diffs_reconstruct_exact_after_bytes":True,"other_v6_entries_unchanged":2619,
        "all_v3_and_v6_source_records_authenticated":True,"all_37_v6_selectors_retained":True,
        "actual_new_test_execution_count":0,"actual_candidate_build_count":0,"actual_candidate_vm_runs":0,
        "limitations":record(D/"READ-LIMITS.md"),
    })
    print(json.dumps({"target":record(target),"readback":record(readback),"source_patch":record(D/"SOURCE.patch"),"report":record(D/"REPORT.md"),"inputs":record(inputs),"source_entries":len(current),"records":len(all_inputs)},indent=2))


if __name__ == "__main__":
    main()
