#!/usr/bin/python3
"""Bind discovered files and inspect ELF metadata without invoking loaders."""
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat

from queries import ROOT, ENV, query

records = {}
absent = []
elf_records = []
pending = []
inspected = set()


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        before = os.fstat(stream.fileno())
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
        after = os.fstat(stream.fileno())
    assert (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    return value.hexdigest()


def link_chain(path):
    todo = list(Path(path).parts)[1:]
    current = Path("/")
    links = []
    while todo:
        component = todo.pop(0)
        if component == "..":
            current = current.parent
            continue
        if component == ".":
            continue
        candidate = current / component
        if candidate.is_symlink():
            target = os.readlink(candidate)
            s = candidate.lstat()
            links.append({"path": str(candidate), "target": target, "size": s.st_size,
                          "mode": stat.S_IMODE(s.st_mode), "device": s.st_dev, "inode": s.st_ino,
                          "sha256": hashlib.sha256(os.fsencode(target)).hexdigest()})
            assert len(links) < 40
            if target.startswith("/"):
                current = Path("/")
                todo = list(Path(target).parts)[1:] + todo
            else:
                todo = list(Path(target).parts) + todo
        else:
            current = candidate
    return links


def bind(path, role, required=True):
    path = os.fspath(path)
    if not os.path.isabs(path):
        raise ValueError(path)
    p = Path(path)
    if not p.exists():
        item = {"path": path, "role": role, "required": required, "state": "absent"}
        if item not in absent:
            absent.append(item)
        return None
    if path in records:
        if role not in records[path]["roles"]:
            records[path]["roles"].append(role)
        return records[path]
    resolved = p.resolve(strict=True)
    s = resolved.stat()
    if not stat.S_ISREG(s.st_mode):
        raise ValueError(f"not a regular input file: {path}")
    row = {"path": path, "resolved_path": str(resolved), "size": s.st_size,
           "mode": stat.S_IMODE(s.st_mode), "mode_octal": oct(stat.S_IMODE(s.st_mode)),
           "device": s.st_dev, "inode": s.st_ino, "mtime_ns": s.st_mtime_ns,
           "sha256": digest(resolved), "symlinks": link_chain(path), "roles": [role]}
    records[path] = row
    with resolved.open("rb") as stream:
        if stream.read(4) == b"\x7fELF":
            pending.append((str(resolved), role))
    return row


cache = {}
for line in (ROOT / "queries/ld-cache/stdout").read_text().splitlines():
    match = re.match(r"\s*(\S+) \((.*?)\) => (\S+)", line)
    if match and "x86-64" in match[2]:
        cache.setdefault(match[1], []).append({"path": match[3], "cache_flags": match[2]})


def process_elf_queue():
    while pending:
        path, role = pending.pop(0)
        if path in inspected:
            continue
        inspected.add(path)
        suffix = hashlib.sha256(path.encode()).hexdigest()[:16]
        stdout, stderr = query("elf-" + suffix, ["/usr/bin/readelf", "-W", "-l", "-d", path])
        needed = re.findall(r"\(NEEDED\).*Shared library: \[(.*?)\]", stdout)
        interpreters = re.findall(r"Requesting program interpreter: (.*?)\]", stdout)
        runpath = re.findall(r"\(RUNPATH\).*Library runpath: \[(.*?)\]", stdout)
        rpath = re.findall(r"\(RPATH\).*Library rpath: \[(.*?)\]", stdout)
        row = {"path": path, "role": role, "needed": needed, "interpreters": interpreters,
               "runpath": runpath, "rpath": rpath, "resolutions": [], "query": "elf-" + suffix}
        for interp in interpreters:
            bind(interp, "ELF interpreter for " + path)
        for name in needed:
            candidates = []
            for search in runpath or rpath:
                for directory in search.split(":"):
                    directory = directory.replace("${ORIGIN}", str(Path(path).parent)).replace("$ORIGIN", str(Path(path).parent))
                    directory = directory.replace("${LIB}", "lib64").replace("$LIB", "lib64")
                    if "$" in directory or not directory.startswith("/"):
                        raise ValueError(f"unresolved dynamic search directory: {directory}")
                    found = Path(directory) / name
                    if found.is_file():
                        candidates.append({"path": str(found), "from": "ELF search path"})
            if not candidates:
                candidates = cache.get(name, [])
            if not candidates:
                candidates = [{"path": str(Path(directory) / name), "from": "default directory"}
                              for directory in ["/lib64", "/usr/lib64"] if (Path(directory) / name).is_file()]
            if not candidates:
                absent.append({"name": name, "required_by": path, "required": True, "state": "unresolved DT_NEEDED"})
            else:
                # Retain every eligible cache candidate; selection is static discovery,
                # not a claim that a runtime loader was observed mapping it.
                for candidate in candidates:
                    bind(candidate["path"], "DT_NEEDED " + name + " for " + path)
            row["resolutions"].append({"name": name, "candidates": candidates})
        elf_records.append(row)


def main():
    for path in ["/usr/bin/python3", "/usr/bin/readelf", "/usr/sbin/ldconfig", "/usr/bin/cc", "/usr/bin/gcc", "/usr/bin/as", "/usr/bin/ld"]:
        bind(path, "discovery or compiler executable")
    for name in ["cc1", "collect2", "ld", "as", "lto-wrapper"]:
        path = (ROOT / ("queries/cc-program-" + name) / "stdout").read_text().strip()
        actual = path if path.startswith("/") else shutil.which(path, path=ENV["PATH"])
        if actual:
            bind(actual, "GCC program query " + name)
        else:
            absent.append({"name": path, "required": True, "state": "compiler program unavailable"})
    for directory in sorted((ROOT / "queries").glob("cc-file-*")):
        value = (directory / "stdout").read_text().strip()
        if value.startswith("/"):
            bind(value, "GCC file query " + directory.name[8:])
        else:
            absent.append({"name": value, "required": False, "state": "GCC print-file-name returned unresolved name", "query": directory.name})
    for path in ["/lib64/libc.so.6", "/usr/lib64/libc_nonshared.a", "/lib64/ld-linux-x86-64.so.2", "/lib64/libgcc_s.so.1"]:
        bind(path, "linker script input")
    for fixture in json.loads((ROOT / "fixtures.json").read_text()):
        bind(fixture["copy"], "exact copied fixture " + fixture["name"])
        bind(fixture["source"], "original fixture source " + fixture["name"])
        dependency = (ROOT / "queries" / (fixture["name"] + "-header-dependencies") / "stdout").read_text().replace("\\\n", " ")
        for path in shlex.split(dependency.split(":", 1)[1]):
            bind(path, "preprocessor input for " + fixture["name"])
    prerequisites = json.loads((ROOT / "guest-prerequisites.json").read_text())
    for path in prerequisites["required_paths"]:
        bind(path, "original required guest executable")
    for path in prerequisites["host_input_files"]:
        bind(path, "original host fixture")
    for path in ["/home/newton/.cargo/bin/rust-script", "/home/newton/.cargo/bin/cargo-nextest"]:
        bind(path, "production execution helper")
    for path in ["/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/ld.so.preload"]:
        bind(path, "dynamic loader configuration", required=path != "/etc/ld.so.preload")
    for path in sorted(Path("/etc/ld.so.conf.d").glob("*.conf")):
        bind(path, "ld.so.conf included configuration")
    for path in ["/etc/nsswitch.conf", "/etc/passwd", "/etc/group", "/etc/hosts", "/etc/resolv.conf", "/usr/share/zoneinfo/UTC"]:
        bind(path, "conditional runtime configuration", required=False)
    process_elf_queue()
    (ROOT / "inputs.json").write_text(json.dumps(sorted(records.values(), key=lambda r: r["path"]), indent=2) + "\n")
    (ROOT / "elf-dependencies.json").write_text(json.dumps(elf_records, indent=2) + "\n")
    (ROOT / "unavailable.json").write_text(json.dumps(absent, indent=2) + "\n")
    print(json.dumps({"files": len(records), "elf_objects": len(elf_records), "required_unavailable": [x for x in absent if x["required"]]}, indent=2))


if __name__ == "__main__":
    main()
